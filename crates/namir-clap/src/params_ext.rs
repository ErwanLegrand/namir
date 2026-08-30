//! CLAP's `params` extension (`clack_extensions::params`) — required infrastructure for
//! FR-PARAM-030 ("parameter changes shall be accepted from the UI, CLAP automation, and preset
//! loading") and the concrete mechanism FR-CLAP-060 (host-driven bypass) is built on: see this
//! module's `global.bypass` handling in [`param_info`], which marks that one `REGISTRY` entry
//! with CLAP's own [`ParamInfoFlags::IS_BYPASS`] — "used to merge the plugin and host bypass
//! button" per `clack_extensions::params`'s own module doc comment — rather than inventing a
//! separate ad hoc flag. Once marked, a host's own bypass button sends an ordinary
//! `ParamValueEvent` on `global.bypass`'s id, which reaches `Chain::apply`/`set_global_bypass`
//! through exactly the same path every other automated parameter does
//! (`crate::audio`'s `apply_param_direct`) — D-10.4 (this session's own prerequisite decision)
//! is what made this possible at all: before it, global bypass had no `ParamDescriptor`, so there
//! was nothing here to flag.

use std::ffi::CStr;
use std::sync::atomic::{AtomicU64, Ordering};

use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams,
};
use clack_plugin::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::events::io::{InputEvents, OutputEvents};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::events::{Match, Pckn};
use clack_plugin::utils::{ClapId, Cookie};
use namir_engine::{ParamChange, ParamId as EngineParamId};
use namir_params::global::GLOBAL_BYPASS;
use namir_params::{ParamDescriptor, ParamKind, REGISTRY};

use crate::audio::NamirAudioProcessor;
use crate::main_thread::NamirMainThread;

fn descriptor_by_id(id: ClapId) -> Option<&'static ParamDescriptor> {
    REGISTRY.iter().find(|d| d.id.0 == id.get())
}

fn param_info(descriptor: &'static ParamDescriptor) -> ParamInfo<'static> {
    let mut flags = ParamInfoFlags::IS_AUTOMATABLE;
    let (min_value, max_value, default_value) = match descriptor.kind {
        ParamKind::Continuous { min, max, default } => (min as f64, max as f64, default as f64),
        ParamKind::Stepped {
            values,
            default_index,
        } => {
            flags |= ParamInfoFlags::IS_STEPPED;
            (
                0.0,
                (values.len().saturating_sub(1)) as f64,
                default_index.0 as f64,
            )
        }
    };
    // FR-CLAP-060: the one descriptor CLAP's own bypass convention applies to.
    if descriptor.key == GLOBAL_BYPASS.key {
        flags |= ParamInfoFlags::IS_BYPASS;
    }
    ParamInfo {
        id: ClapId::new(descriptor.id.0),
        flags,
        cookie: Cookie::default(),
        name: descriptor.name.as_bytes(),
        module: b"",
        min_value,
        max_value,
        default_value,
    }
}

/// FR-UI-040's parse direction, reimplemented here rather than depending on `namir-ui::format`
/// (a private module of that crate, not re-exported — see its own `lib.rs`) — small enough
/// (`ParamKind`'s own two shapes) not to warrant changing that crate's public surface for one
/// caller.
fn parse_text_to_value(descriptor: &ParamDescriptor, text: &str) -> Option<f64> {
    let trimmed = text.trim();
    match descriptor.kind {
        ParamKind::Continuous { min, max, .. } => trimmed
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(min as f64, max as f64)),
        ParamKind::Stepped { values, .. } => {
            if let Some(index) = values.iter().position(|v| v.eq_ignore_ascii_case(trimmed)) {
                return Some(index as f64);
            }
            let max_index = values.len().saturating_sub(1) as f64;
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .map(|v| v.round().clamp(0.0, max_index))
        }
    }
}

/// Decodes every `ParamValue` event in `input` and hands each to `apply` — the one event-decode
/// loop **both** `flush` implementations below run.
///
/// Issue #97: the audio-processor impl used to hand-roll an identical copy of this loop, while
/// this helper's own doc comment claimed to be shared by both. Two copies of one decode is exactly
/// the drift a shared helper exists to prevent, and `crate::audio`'s `apply_direct_and_mirror`
/// already avoids it for the *apply* half.
///
/// Mirroring is the caller's, deliberately: the two sides mirror through different paths (the
/// audio side through `apply_direct_and_mirror`, which is itself the "one direct-applied change
/// plus its mirror update" both audio-thread entry points share), so folding it in here would put
/// a second mirror write on that path rather than removing one.
fn apply_flush_events(input: &InputEvents, mut apply: impl FnMut(EngineParamId, f32)) {
    for event in input.iter() {
        if let Some(CoreEventSpace::ParamValue(ev)) = event.as_core_event()
            && let Some(id) = ev.param_id()
        {
            apply(EngineParamId(id.get()), ev.value() as f32);
        }
    }
}

/// Whether a [`ParamGestureBeginEvent`] this crate emitted is still owed its matching
/// [`ParamGestureEndEvent`], per `REGISTRY` index — the state [`emit_gui_param_changes`] needs
/// between two calls, and the whole of it.
///
/// **Why this exists (issue #145's review, below the cut).** The three pushes that make up one
/// gesture are three separate `OutputEvents::try_push` calls, and a host is entitled to refuse any
/// of them: `try_push`'s own documentation says so ("usually a sign that the implementer ran out of
/// buffer space"). A refusal partway through leaves the *begin* already delivered, and there is no
/// un-push — so the only way to keep the host's view well-formed is to remember what is still open
/// and close it on the next call, before anything else is emitted. A host that tracks gesture
/// nesting (which is what the begin/end pair is *for*) otherwise keeps recording automation for a
/// knob the user let go of, until the next accident happens to balance the books.
///
/// One `u64`, one bit per `REGISTRY` entry, exactly like [`crate::param_mirror::ParamMirror`]'s own
/// pending set — so this holds at most 64 parameters, which is the same ceiling that type already
/// documents and asserts.
///
/// Lives in [`crate::shared::SharedInner`] rather than in either `flush` implementation because the
/// two audio-thread entry points (`crate::audio`'s `process` and this module's
/// `PluginAudioProcessorParams::flush`) and the main-thread one all emit through the same function,
/// and a gesture opened by one of them must be closed by whichever runs next.
#[derive(Default)]
pub(crate) struct GestureState {
    open: AtomicU64,
}

impl GestureState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Takes the set of parameters whose gesture is still open, leaving it empty — the caller then
    /// owns closing them, and stores back whatever it could not close ([`Self::store_open`]).
    fn take_open(&self) -> u64 {
        self.open.swap(0, Ordering::Relaxed)
    }

    fn store_open(&self, open: u64) {
        self.open.store(open, Ordering::Relaxed);
    }

    /// Whether any gesture is still waiting for its end event — read by
    /// [`crate::main_thread::NamirMainThread::request_param_flush_if_pending`], so an inactive
    /// plugin still gets a call in which to close one.
    pub(crate) fn has_open(&self) -> bool {
        self.open.load(Ordering::Relaxed) != 0
    }
}

/// Reports every parameter the user moved in **this plugin's own editor** to the host, as a
/// gesture-wrapped automation point — issue #94, and `clack_extensions::params`' own "Turning a
/// knob on the Plugin interface" scenario ("send an automation event and don't forget to wrap the
/// parameter change(s) with `ParamGestureBeginEvent` and `ParamGestureEndEvent`").
///
/// Without this a knob turned in the editor reached the engine and the mirror and stopped there:
/// the host could not record the move as automation, and its own generic parameter UI stayed stale
/// until it independently re-polled `get_value`.
///
/// # Why a begin/end pair around every single change
///
/// `namir-ui` emits one [`namir_ui::UiIntent::SetParam`] per changed value and has no notion of a
/// drag beginning or ending, so this crate cannot honestly report a *long* gesture; it reports each
/// change as its own complete one, which is what makes a host record it rather than treat it as an
/// unterminated drag. Widening `UiIntent` to carry drag boundaries is a `namir-ui` change, not a
/// `namir-clap` one.
///
/// # A refused push must not leave a gesture hanging (issue #145)
///
/// The three pushes were `&&`-chained, which reads as all-or-nothing and is not: `&&` short-circuits
/// *after* the begin has already been handed to the host, so a buffer that filled up between the
/// begin and the end left an unmatched `ParamGestureBegin` out there — and the retry path then
/// emitted a *second* begin on the next block, because the change was correctly put back in the
/// pending set. Two begins, one end.
///
/// There is no un-push, and `OutputEvents` exposes no remaining capacity to check first, so the
/// emission is made whole across calls rather than within one:
///
/// 1. any gesture a previous call left open is closed first, before anything new is emitted;
/// 2. a begin is only pushed once nothing is still open, since a host that is refusing events is
///    refusing the end that would balance a new begin too;
/// 3. once a begin *is* delivered, this owes an end for that parameter — so the end is attempted
///    even when the value push was refused, and recorded as still-open if it too is refused.
///
/// The value and the gesture are tracked separately on purpose: a value that did not reach the host
/// goes back into the mirror's pending set and is reported again next call (never lost, never
/// duplicated), while the *gesture* is a debt to the host that is settled where it was incurred.
///
/// # Real-time safety
///
/// Called from `process()` (the audio thread) as well as from both `flush` implementations.
/// Allocation-free and bounded: two `swap`s, at most 64 iterations each (one per `REGISTRY` entry,
/// and only for entries actually marked), at most three stack-built `#[repr(C)]` events per entry,
/// and no branch that can loop. `OutputEvents::try_push` calls the host's own callback, which the
/// host is required to keep real-time safe for exactly this reason.
pub(crate) fn emit_gui_param_changes(
    mirror: &crate::param_mirror::ParamMirror,
    gestures: &GestureState,
    out: &mut OutputEvents,
) {
    // 1. Settle what a previous call could not: a begin is out there with no end after it, and
    //    nothing new may be emitted in front of it.
    let mut unclosed = gestures.take_open();
    let mut still_open = 0u64;
    while unclosed != 0 {
        let index = unclosed.trailing_zeros() as usize;
        let bit = 1u64 << index;
        unclosed &= !bit;
        let Some(descriptor) = REGISTRY.get(index) else {
            continue;
        };
        let id = ClapId::new(descriptor.id.0);
        if out.try_push(ParamGestureEndEvent::new(0, id)).is_err() {
            still_open |= bit;
        }
    }

    // 2. This call's own changes.
    let mut pending = mirror.take_gui_pending();
    let mut undelivered = 0u64;
    while pending != 0 {
        let index = pending.trailing_zeros() as usize;
        let bit = 1u64 << index;
        pending &= !bit;

        let (Some(descriptor), Some(value)) = (REGISTRY.get(index), mirror.value_at(index)) else {
            continue;
        };
        if still_open != 0 {
            // The host is refusing events -- it just refused an end. Opening another gesture now
            // would only add a second one that cannot be closed either.
            undelivered |= bit;
            continue;
        }
        let id = ClapId::new(descriptor.id.0);
        if out.try_push(ParamGestureBeginEvent::new(0, id)).is_err() {
            // Nothing was delivered for this parameter, so nothing is owed: report it next call.
            undelivered |= bit;
            continue;
        }
        // From here the host has the begin, and is owed the end whatever else happens.
        let value_delivered = out
            .try_push(ParamValueEvent::new(
                0,
                id,
                Pckn::new(Match::All, Match::All, Match::All, Match::All),
                value as f64,
                Cookie::empty(),
            ))
            .is_ok();
        if out.try_push(ParamGestureEndEvent::new(0, id)).is_err() {
            still_open |= bit;
        }
        if !value_delivered {
            undelivered |= bit;
        }
    }

    if undelivered != 0 {
        mirror.restore_gui_pending(undelivered);
    }
    gestures.store_open(still_open);
}

impl<'a> PluginMainThreadParams for NamirMainThread<'a> {
    fn count(&mut self) -> u32 {
        REGISTRY.len() as u32
    }

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        if let Some(descriptor) = REGISTRY.get(param_index as usize) {
            info.set(&param_info(descriptor));
        }
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        self.shared
            .inner
            .params
            .get_by_id(param_id.get())
            .map(|v| v as f64)
    }

    fn value_to_text(
        &mut self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        use std::fmt::Write;
        if let Some(descriptor) = descriptor_by_id(param_id) {
            write!(writer, "{}", descriptor.format_value(value as f32))
        } else {
            Err(std::fmt::Error)
        }
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let descriptor = descriptor_by_id(param_id)?;
        let text = text.to_str().ok()?;
        parse_text_to_value(descriptor, text)
    }

    fn flush(
        &mut self,
        input_parameter_changes: &InputEvents,
        output_parameter_changes: &mut OutputEvents,
    ) {
        // Inactive (no live engine): update only the mirror, which the *next* `activate()`'s
        // replay (`crate::audio`) will push onto a fresh engine. See `crate::shared`'s
        // `SharedInner::with_instance` — a `None` instance here is not an error, just "not yet
        // activated", handled the same way `try_submit_param` degrades when abandoned.
        // A preset recalled from this plugin's editor may be waiting to be announced; a flush is
        // a `[main-thread]` call, so it is one of the two places that can do it.
        self.rescan_params_if_pending();

        // The `&'a NamirShared` is copied out first so the closure borrows only it, never `self`.
        let shared = self.shared;
        let mirror = &shared.inner.params;
        apply_flush_events(input_parameter_changes, |id, value| {
            mirror.set_by_id(id.0, value);
            shared.inner.with_instance(|instance| {
                let _ = instance.try_submit_param(ParamChange { id, value });
            });
        });
        // The outbound half (issue #94). This is the *only* channel a GUI-originated change has
        // while the plugin is inactive, which is why `crate::main_thread`'s
        // `request_param_flush_if_pending` exists to ask for this call at all -- including when
        // there is no change left to report and only a gesture to close (issue #145).
        emit_gui_param_changes(mirror, &shared.inner.gestures, output_parameter_changes);
    }
}

impl<'a> PluginAudioProcessorParams for NamirAudioProcessor<'a> {
    fn flush(
        &mut self,
        input_parameter_changes: &InputEvents,
        output_parameter_changes: &mut OutputEvents,
    ) {
        // Active, but `process()` was not called this cycle -- still the audio thread (per
        // `clack_plugin`'s own thread-model doc comment on `PluginAudioProcessorParams`), so this
        // uses the same direct-apply path `process()` itself uses, not the ring.
        //
        // Through `apply_flush_events`, which is what that helper's own doc comment has always
        // said it was for: this impl used to hand-roll the identical decode loop (issue #97), two
        // copies of one event decode with nothing keeping them in step. The `&'a NamirShared` is
        // copied out first so the mirror borrow does not hold a borrow of `self` across the
        // closure's `&mut self.engine`.
        let shared = self.shared();
        apply_flush_events(input_parameter_changes, |id, value| {
            self.apply_direct_and_mirror(id, value)
        });
        emit_gui_param_changes(
            &shared.inner.params,
            &shared.inner.gestures,
            output_parameter_changes,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **FR-CLAP-060's tag moved out of this file at M14**, to
    /// `tests/clap_host_automation.rs`. What this test asserts — that the bypass descriptor
    /// carries CLAP's `IS_BYPASS` flag — is a real and necessary part of "host-driven bypass",
    /// since a host that cannot recognise the parameter never sends the automation at all; it is
    /// not, on its own, evidence about either "sample-accurate" or "click-free", which is what the
    /// old tag's own `uncovered:` field said. Those are now driven through the real vtable with
    /// real event offsets, and the tag lives beside them.
    #[test]
    fn global_bypass_param_info_carries_the_is_bypass_flag() {
        let info = param_info(&GLOBAL_BYPASS);
        assert!(info.flags.contains(ParamInfoFlags::IS_BYPASS));
        assert!(info.flags.contains(ParamInfoFlags::IS_STEPPED));
        assert_eq!(info.min_value, 0.0);
        assert_eq!(info.max_value, 1.0);
    }

    #[test]
    fn a_continuous_descriptor_carries_no_bypass_or_stepped_flag() {
        let info = param_info(&namir_params::stages::trim::GAIN_DB);
        assert!(!info.flags.contains(ParamInfoFlags::IS_BYPASS));
        assert!(!info.flags.contains(ParamInfoFlags::IS_STEPPED));
    }

    #[test]
    fn parse_text_to_value_matches_namir_ui_formats_semantics() {
        let d = &namir_params::stages::trim::GAIN_DB;
        assert_eq!(parse_text_to_value(d, "6.0"), Some(6.0));
        assert_eq!(parse_text_to_value(d, "loud"), None);
    }

    #[test]
    fn parse_text_to_value_accepts_a_named_stepped_value_case_insensitively() {
        let d = &namir_params::stages::gate::ENABLED;
        assert_eq!(parse_text_to_value(d, "on"), Some(1.0));
    }

    #[test]
    fn descriptor_by_id_finds_a_known_registry_entry() {
        let id = ClapId::new(namir_params::stages::trim::GAIN_DB.id.0);
        assert_eq!(
            descriptor_by_id(id).map(|d| d.key),
            Some(namir_params::stages::trim::GAIN_DB.key)
        );
    }

    #[test]
    fn descriptor_by_id_returns_none_for_an_unknown_id() {
        assert!(descriptor_by_id(ClapId::new(0xFFFF_FFFE)).is_none());
    }

    /// **Issue #94, at the seam that talks to the host.** A knob moved in the plugin's own editor
    /// comes out as a complete, gesture-wrapped automation point, on the parameter's own id and
    /// carrying its plain value.
    ///
    /// Driven through a real `clack_common::events::io::EventBuffer` — the same type
    /// `tests/support`'s host harness collects a block's output events into — so what is asserted
    /// is the actual CLAP event stream, not an intermediate of this module's own.
    ///
    /// **Asserted through `UnknownEvent::as_event`, not `as_core_event`, and that is not a
    /// stylistic choice.** `clack-common` 0.1.1's `CoreEventSpace::from_unknown`
    /// (`src/events/spaces/core.rs:66-84`) has arms for eleven of its thirteen variants and omits
    /// exactly the two gesture ones, so `as_core_event()` answers `None` for a
    /// `ParamGestureBeginEvent` that is perfectly well-formed — the enum carries
    /// `ParamGestureBegin`/`ParamGestureEnd` variants that its own decoder can never produce.
    /// A host reads the raw `clap_event_header`, so this is a defect in clack's convenience
    /// decoder rather than in what this plugin emits; checking the header's own `type_id` is both
    /// the accurate assertion and the one that will not silently start passing for the wrong
    /// reason if that decoder is ever fixed.
    ///
    /// **Upstream status (issue #144, as of 2026-08-30): reported as
    /// [prokopyl/clack#97](https://github.com/prokopyl/clack/issues/97)** (opened 2026-08-16,
    /// open — do not refile). A fix exists in flight as
    /// [prokopyl/clack#99](https://github.com/prokopyl/clack/pull/99), adding exactly the two
    /// missing arms, but it is unmerged, unreviewed and targets the 0.2 line, so it reaches no
    /// 0.1.1 dependent; crates.io publishes only 0.1.0 and 0.1.1.
    ///
    /// **This test would not notice a fixed clack** — `as_event` reads the header and keeps
    /// working either way, which is exactly why it was chosen. Noticing is
    /// [`clack_0_1_1_cannot_decode_a_gesture_event_through_core_event_space`]'s job, below.
    #[test]
    fn a_gui_originated_change_comes_out_as_a_gesture_wrapped_automation_point() {
        use clack_plugin::events::event_types::{
            ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
        };
        use clack_plugin::events::io::EventBuffer;

        let mirror = crate::param_mirror::ParamMirror::new();
        let descriptor = &namir_params::stages::trim::GAIN_DB;
        mirror.set_by_key_from_gui(descriptor.key, 4.5);

        let mut buffer = EventBuffer::with_capacity(8);
        emit_gui_param_changes(&mirror, &GestureState::new(), &mut buffer.as_output());

        let events: Vec<&clack_plugin::events::UnknownEvent> = buffer.iter().collect();
        assert_eq!(
            events.len(),
            3,
            "a user gesture is begin + value + end, or a host has no complete gesture to record"
        );
        let expected_id = Some(ClapId::new(descriptor.id.0));

        let begin = events[0]
            .as_event::<ParamGestureBeginEvent>()
            .expect("the first event must be a gesture begin");
        assert_eq!(begin.param_id(), expected_id);

        let value = events[1]
            .as_event::<ParamValueEvent>()
            .expect("the second event must be the value");
        assert_eq!(value.param_id(), expected_id);
        assert_eq!(value.value(), 4.5);

        let end = events[2]
            .as_event::<ParamGestureEndEvent>()
            .expect("the third event must be a gesture end");
        assert_eq!(end.param_id(), expected_id);

        // Reported once: a second drain with nothing new emits nothing at all.
        let mut again = EventBuffer::with_capacity(8);
        emit_gui_param_changes(&mirror, &GestureState::new(), &mut again.as_output());
        assert!(
            again.is_empty(),
            "a change already reported must not be reported again every block"
        );
    }

    /// **The tripwire for [prokopyl/clack#97](https://github.com/prokopyl/clack/issues/97), and
    /// the reason the test above can afford to be quiet about it.**
    ///
    /// `clack-common` 0.1.1's `CoreEventSpace::from_unknown` (`src/events/spaces/core.rs:66-84`)
    /// has no arm for `ParamGestureBeginEvent::TYPE_ID` or `ParamGestureEndEvent::TYPE_ID`, so a
    /// gesture event this crate emits — well-formed, as the first assertion in the loop below
    /// re-establishes through `UnknownEvent::as_event` — decodes to `None` through
    /// `as_core_event()`.
    /// That is why `emit_gui_param_changes`' own test asserts through `as_event` and why
    /// `crate::audio`'s input path can only ever match `CoreEventSpace::ParamValue`.
    ///
    /// Asserting the *gap itself* is what makes it retire on its own. The `as_event` assertions
    /// keep passing once the decoder is fixed (issue #144: a `cargo update` to a fixed 0.1.2 is
    /// all it takes, since `Cargo.toml`'s `"0.1.1"` is `^0.1.1` and the lockfile is what holds
    /// the version), so without this test nothing anywhere would report that the workaround had
    /// become unnecessary. When this fails, delete it, and prefer `as_core_event()` at both
    /// sites.
    #[test]
    fn clack_0_1_1_cannot_decode_a_gesture_event_through_core_event_space() {
        use clack_plugin::events::io::EventBuffer;

        let mirror = crate::param_mirror::ParamMirror::new();
        mirror.set_by_key_from_gui(namir_params::stages::trim::GAIN_DB.key, 4.5);

        let mut buffer = EventBuffer::with_capacity(8);
        emit_gui_param_changes(&mirror, &GestureState::new(), &mut buffer.as_output());
        let events: Vec<&clack_plugin::events::UnknownEvent> = buffer.iter().collect();
        assert_eq!(events.len(), 3, "begin + value + end");

        // The one event in the trio 0.1.1 does decode, so a failure below is the decoder's two
        // missing arms and not this test having lost its way to the event stream.
        assert!(
            matches!(
                events[1].as_core_event(),
                Some(CoreEventSpace::ParamValue(_))
            ),
            "the value event decodes through CoreEventSpace in every version"
        );

        for (name, event) in [("begin", events[0]), ("end", events[2])] {
            assert!(
                event.as_event::<ParamGestureBeginEvent>().is_some()
                    || event.as_event::<ParamGestureEndEvent>().is_some(),
                "the gesture {name} event is well-formed: the header says what it is"
            );
            assert!(
                event.as_core_event().is_none(),
                "clack 0.1.1's CoreEventSpace::from_unknown drops gesture events \
                 (prokopyl/clack#97; fix in flight as prokopyl/clack#99, targeting 0.2). If this \
                 now decodes, the lockfile has moved to a version that fixes it: delete this test \
                 and read the gesture {name} event through as_core_event() instead of as_event"
            );
        }
    }

    /// The other half of the same rule: a change that came *from* the host is not sent back to it.
    #[test]
    fn a_host_originated_change_produces_no_output_events() {
        use clack_plugin::events::io::EventBuffer;

        let mirror = crate::param_mirror::ParamMirror::new();
        mirror.set_by_id(namir_params::stages::trim::GAIN_DB.id.0, 9.0);

        let mut buffer = EventBuffer::with_capacity(8);
        emit_gui_param_changes(&mirror, &GestureState::new(), &mut buffer.as_output());
        assert!(
            buffer.is_empty(),
            "echoing the host's own automation back at it is a feedback loop, not a report"
        );
    }
}

#[cfg(test)]
mod gesture_tests {
    use super::*;
    use clack_plugin::events::UnknownEvent;
    use clack_plugin::events::io::{EventBuffer, OutputEventBuffer, TryPushError};

    /// An `OutputEvents` backing buffer that accepts `cap` events and refuses everything after
    /// them.
    struct CappedOutput {
        buffer: EventBuffer,
        cap: u32,
    }

    impl CappedOutput {
        fn new(cap: u32) -> Self {
            Self {
                buffer: EventBuffer::with_capacity(8),
                cap,
            }
        }
    }

    impl OutputEventBuffer for CappedOutput {
        fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
            if self.buffer.len() >= self.cap {
                return Err(TryPushError::new());
            }
            self.buffer.try_push(event)
        }
    }

    /// `(kind, param_id)` for every event in `buffer`, where kind is `1` for a gesture begin, `-1`
    /// for a gesture end and `0` for a value.
    fn shape(buffer: &EventBuffer) -> Vec<(i32, Option<ClapId>)> {
        buffer
            .iter()
            .map(|e| {
                if let Some(b) = e.as_event::<ParamGestureBeginEvent>() {
                    (1, b.param_id())
                } else if let Some(end) = e.as_event::<ParamGestureEndEvent>() {
                    (-1, end.param_id())
                } else if let Some(v) = e.as_event::<ParamValueEvent>() {
                    (0, v.param_id())
                } else {
                    panic!("unexpected event kind")
                }
            })
            .collect()
    }

    /// **Issue #145's below-the-cut finding.** A host that refuses a push partway through a
    /// gesture must not be left holding an unmatched [`ParamGestureBeginEvent`].
    ///
    /// Driven at every interesting capacity, because the three pushes fail in three materially
    /// different places: `0` refuses the begin (nothing is owed), `1` refuses the value and the end
    /// (the begin is out there alone), `2` refuses only the end, and `3` refuses nothing. In every
    /// case the two blocks together must read as well-formed gestures — never two begins in a row,
    /// never an end with nothing open, and nothing left open at the finish — and the value the user
    /// dialled must reach the host exactly once, neither lost nor duplicated.
    ///
    /// Before the fix, capacities 1 and 2 produced `[Begin, (Value,) Begin, Value, End]`: the
    /// `&&`-chain short-circuited after the begin had already been handed over, and the retry path
    /// — correct in itself, since the change must not be dropped — opened a second gesture on the
    /// next block.
    #[test]
    fn a_refused_push_never_leaves_the_host_with_an_unmatched_gesture_begin() {
        for cap in 0..=3u32 {
            let mirror = crate::param_mirror::ParamMirror::new();
            let gestures = GestureState::new();
            let descriptor = &namir_params::stages::trim::GAIN_DB;
            let expected_id = Some(ClapId::new(descriptor.id.0));
            mirror.set_by_key_from_gui(descriptor.key, 4.5);

            // One block into a host buffer with room for `cap` events...
            let mut capped = CappedOutput::new(cap);
            emit_gui_param_changes(
                &mirror,
                &gestures,
                &mut clack_plugin::events::io::OutputEvents::from_buffer(&mut capped),
            );

            // ...and the next one into a host that refuses nothing.
            let mut second = EventBuffer::with_capacity(8);
            emit_gui_param_changes(&mirror, &gestures, &mut second.as_output());

            let mut stream = shape(&capped.buffer);
            stream.extend(shape(&second));

            let mut depth = 0i32;
            for (kind, id) in &stream {
                assert_eq!(
                    *id, expected_id,
                    "cap {cap}: an event on the wrong parameter"
                );
                depth += kind;
                assert!(
                    (0..=1).contains(&depth),
                    "cap {cap}: the host sees a malformed gesture stream {stream:?} (depth \
                     {depth}) -- a second ParamGestureBegin arrived before the first was closed, \
                     so a host tracking gesture nesting keeps writing automation after the user \
                     let go"
                );
            }
            assert_eq!(
                depth, 0,
                "cap {cap}: every gesture the host was told about must eventually be closed: \
                 {stream:?}"
            );
            assert_eq!(
                stream.iter().filter(|(kind, _)| *kind == 0).count(),
                1,
                "cap {cap}: the value the user dialled must reach the host exactly once: \
                 {stream:?}"
            );
            assert!(
                !gestures.has_open(),
                "cap {cap}: a gesture is still recorded as open after a block that refused nothing"
            );
        }
    }

    /// The one case the retry path cannot settle on its own: the second block refuses the closing
    /// end too, so the debt is still recorded and `has_open` is what tells
    /// `crate::main_thread`'s `request_param_flush_if_pending` to ask the host for another call.
    #[test]
    fn a_gesture_that_could_not_be_closed_is_still_owed_after_the_retry() {
        let mirror = crate::param_mirror::ParamMirror::new();
        let gestures = GestureState::new();
        mirror.set_by_key_from_gui(namir_params::stages::trim::GAIN_DB.key, 4.5);

        let mut first = CappedOutput::new(2); // begin + value, no room for the end
        emit_gui_param_changes(
            &mirror,
            &gestures,
            &mut clack_plugin::events::io::OutputEvents::from_buffer(&mut first),
        );
        assert!(gestures.has_open());

        let mut second = CappedOutput::new(0); // refuses the end as well
        emit_gui_param_changes(
            &mirror,
            &gestures,
            &mut clack_plugin::events::io::OutputEvents::from_buffer(&mut second),
        );
        assert!(
            gestures.has_open(),
            "the end is still owed, and nothing but a later call can deliver it"
        );

        let mut third = EventBuffer::with_capacity(8);
        emit_gui_param_changes(&mirror, &gestures, &mut third.as_output());
        assert_eq!(
            shape(&third),
            vec![(
                -1,
                Some(ClapId::new(namir_params::stages::trim::GAIN_DB.id.0))
            )],
            "the first call that can take an event closes the gesture, and emits nothing else"
        );
        assert!(!gestures.has_open());
    }

    /// A begin that was itself refused owes nothing: the change goes back into the mirror's
    /// pending set and is reported whole next time, with no phantom end in front of it.
    #[test]
    fn a_refused_begin_leaves_no_debt_and_loses_no_change() {
        let mirror = crate::param_mirror::ParamMirror::new();
        let gestures = GestureState::new();
        let descriptor = &namir_params::stages::trim::GAIN_DB;
        mirror.set_by_key_from_gui(descriptor.key, 4.5);

        let mut refused = CappedOutput::new(0);
        emit_gui_param_changes(
            &mirror,
            &gestures,
            &mut clack_plugin::events::io::OutputEvents::from_buffer(&mut refused),
        );
        assert!(refused.buffer.is_empty());
        assert!(!gestures.has_open());

        let mut next = EventBuffer::with_capacity(8);
        emit_gui_param_changes(&mirror, &gestures, &mut next.as_output());
        let id = Some(ClapId::new(descriptor.id.0));
        assert_eq!(shape(&next), vec![(1, id), (0, id), (-1, id)]);
    }
}
