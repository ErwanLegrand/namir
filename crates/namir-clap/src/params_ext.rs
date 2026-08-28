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
/// # Real-time safety
///
/// Called from `process()` (the audio thread) as well as from both `flush` implementations.
/// Allocation-free and bounded: one `swap`, at most 64 iterations (one per `REGISTRY` entry, and
/// only for entries actually marked), three stack-built `#[repr(C)]` events each, and no branch
/// that can loop. `OutputEvents::try_push` calls the host's own callback, which the host is
/// required to keep real-time safe for exactly this reason.
///
/// A `try_push` the host refuses (a full buffer) puts the change back in the pending set rather
/// than dropping it, so it is reported on the next block instead of silently lost.
pub(crate) fn emit_gui_param_changes(
    mirror: &crate::param_mirror::ParamMirror,
    out: &mut OutputEvents,
) {
    let mut pending = mirror.take_gui_pending();
    let mut undelivered = 0u64;
    while pending != 0 {
        let index = pending.trailing_zeros() as usize;
        let bit = 1u64 << index;
        pending &= !bit;

        let (Some(descriptor), Some(value)) = (REGISTRY.get(index), mirror.value_at(index)) else {
            continue;
        };
        let id = ClapId::new(descriptor.id.0);
        let delivered = out.try_push(ParamGestureBeginEvent::new(0, id)).is_ok()
            && out
                .try_push(ParamValueEvent::new(
                    0,
                    id,
                    Pckn::new(Match::All, Match::All, Match::All, Match::All),
                    value as f64,
                    Cookie::empty(),
                ))
                .is_ok()
            && out.try_push(ParamGestureEndEvent::new(0, id)).is_ok();
        if !delivered {
            undelivered |= bit;
        }
    }
    if undelivered != 0 {
        mirror.restore_gui_pending(undelivered);
    }
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
        // `request_param_flush_if_pending` exists to ask for this call at all.
        emit_gui_param_changes(mirror, output_parameter_changes);
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
        emit_gui_param_changes(&shared.inner.params, output_parameter_changes);
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
        emit_gui_param_changes(&mirror, &mut buffer.as_output());

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
        emit_gui_param_changes(&mirror, &mut again.as_output());
        assert!(
            again.is_empty(),
            "a change already reported must not be reported again every block"
        );
    }

    /// The other half of the same rule: a change that came *from* the host is not sent back to it.
    #[test]
    fn a_host_originated_change_produces_no_output_events() {
        use clack_plugin::events::io::EventBuffer;

        let mirror = crate::param_mirror::ParamMirror::new();
        mirror.set_by_id(namir_params::stages::trim::GAIN_DB.id.0, 9.0);

        let mut buffer = EventBuffer::with_capacity(8);
        emit_gui_param_changes(&mirror, &mut buffer.as_output());
        assert!(
            buffer.is_empty(),
            "echoing the host's own automation back at it is a feedback loop, not a report"
        );
    }
}
