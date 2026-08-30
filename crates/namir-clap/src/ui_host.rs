//! [`ClapUiHost`]: this crate's `namir_ui::UiHost` implementation — the bridge FR-CLAP-100's
//! embedded GUI needs between `namir-ui`'s pure view layer (which may not depend on
//! `namir-engine`/`namir-worker`, per D-5.1) and this instance's real, live state
//! ([`crate::shared::SharedInner`]).
//!
//! # `snapshot`: three sources, one read-only picture
//!
//! - **Parameters** — [`crate::param_mirror::ParamMirror::snapshot`], the same lock-free mirror
//!   host automation and GUI dispatch both write through, so the GUI is never stale by more than
//!   the mirror's own atomic-store granularity.
//! - **Meters** — drained here, from the [`namir_engine::TelemetryReader`] this crate keeps a
//!   clone of once an engine exists (`crate::audio`'s `activate`). D-7.3's own reader is
//!   `Clone`, specifically so a UI-side consumer can hold one independent of whatever
//!   `namir_worker::Instance` does with the `WorkerEndpoint` it was built from. Only two ids are
//!   read: `telemetry.trim.peak_db` (post-trim input level) and `telemetry.out.ch{0,1}.peak_db`
//!   (post-output level, both channels). **Known gap, stated rather than glossed:** the engine
//!   publishes peak only, not RMS — `MeterReading::rms_db` is set equal to `peak_db` rather than
//!   left at a stale or fabricated value, and this is called out explicitly rather than presented
//!   as a real RMS reading.
//! - **Everything else** (loaded model/IR names, library index, notices, dirty flag) — read
//!   straight off [`SharedInner`]'s own fields.
//!
//! # `dispatch`: every intent converges on the same two channels a preset recall uses
//!
//! A [`namir_ui::UiIntent::SetParam`]/`ResetParamToDefault` writes the mirror *and* pushes a
//! `Command::Param` onto the live engine's ring directly with `RingProducer`'s own
//! `try_push`-under-a-producer-mutex path (`namir_worker::CommandSubmitter::try_submit` — "what
//! the UI thread uses" per that method's own doc comment) — never through
//! `AudioEngine::apply_param_direct`, which is reserved for the audio thread itself (see
//! `crate::audio`'s module doc comment for why mixing the two producers here would be unsound).
//! A `LoadLibraryEntry`/library rescan intent hands off to [`crate::worker_jobs`], which run on
//! [`namir_worker::pool::ThreadPool`] so this method itself never blocks the GUI thread (FR-UI-070:
//! "shall never interrupt audio" — and, by the same construction, never interrupt the GUI either).

use std::sync::Arc;

use namir_engine::{ParamChange, ParamId, TelemetryEntry, TelemetryReader};
use namir_params::REGISTRY;
use namir_ui::{MeterReading, UiHost, UiIntent, UiSnapshot};

use crate::shared::SharedInner;
use crate::worker_jobs;

/// Post-trim input level (`namir-engine/src/stages/trim.rs`'s own telemetry id). Computed with
/// `namir_params::ParamId::from_key` (the same derivation those stages use to build their own
/// telemetry ids) — `namir_engine::ParamId` is a bare RT-path wrapper with no such constructor
/// (see that type's own doc comment).
fn telemetry_input_peak_id() -> u32 {
    namir_params::ParamId::from_key("telemetry.trim.peak_db").0
}

/// Post-output level, per channel (`namir-engine/src/stages/out.rs`'s own telemetry id shape).
fn telemetry_output_peak_id(channel: usize) -> u32 {
    namir_params::ParamId::from_key(&format!("telemetry.out.ch{channel}.peak_db")).0
}

/// Bounded: a GUI frame drains at most this many batches of the telemetry ring before giving up
/// and showing whatever it caught up to. Not RT-constrained (this runs on the GUI thread), but
/// bounded anyway so a telemetry flood cannot turn one frame into unbounded work.
const MAX_DRAIN_BATCHES: usize = 8;

pub(crate) struct ClapUiHost {
    inner: Arc<SharedInner>,
    /// A clone of the live engine's telemetry reader, or `None` before the first `activate()`.
    ///
    /// **Re-fetched, not captured** (issue #95). This used to be handed in at editor-open and
    /// never looked at again, which broke the meters in two ordinary situations: a host that opens
    /// the editor before the first `activate()` — common, and the plugin's own `get_size`/
    /// `set_parent` sequence happens on the main thread with no audio configured yet — got `None`
    /// and read -inf for the editor's whole life; and every deactivate/reactivate cycle
    /// (including the one the plugin itself asks for when its latency changes) installs a *fresh*
    /// ring, leaving a captured clone draining a retired one for ever.
    telemetry: Option<TelemetryReader>,
    /// The [`SharedInner::telemetry_generation`] the clone above came from. When the shared
    /// counter has moved past it, the clone is stale — see [`ClapUiHost::rebind_telemetry_if_stale`].
    telemetry_generation: u64,
    /// The last reading [`ClapUiHost::drain_meters`] actually saw, held so that a GUI frame which
    /// drains no telemetry shows the previous value rather than dropping to silence.
    ///
    /// **These are held, not accumulated**, and the distinction is the whole of what went wrong
    /// here once: `output_peak_db` used to be updated as `self.output_peak_db.max(entry.value)`,
    /// which reads correctly as "the louder of the two output channels" until you notice that the
    /// left-hand side is a field that outlives the drain. That made it a maximum over the
    /// *instance's whole lifetime*, so the output meter climbed to the loudest peak the plugin had
    /// ever seen and then never moved again — full and stuck, from the first transient that
    /// reached 0 dBFS onwards. Any cross-channel maximum belongs to one drain and must be local to
    /// it; see [`ClapUiHost::drain_meters`], and `namir-app`'s `read_meters`, which has always had
    /// this shape.
    input_peak_db: f32,
    output_peak_db: f32,
}

impl ClapUiHost {
    /// Builds the bridge for one editor window. Takes no telemetry reader: whichever one is live
    /// is fetched on demand, including the case where none exists yet (issue #95).
    pub(crate) fn new(inner: Arc<SharedInner>) -> Self {
        Self {
            inner,
            telemetry: None,
            // `SharedInner`'s counter starts at 0 too and is bumped by every
            // `set_telemetry_reader`, so "0 and no reader" is exactly the state of an instance
            // that has never been activated -- and any activation, past or future, moves it.
            telemetry_generation: 0,
            input_peak_db: f32::NEG_INFINITY,
            output_peak_db: f32::NEG_INFINITY,
        }
    }

    /// Re-clones the telemetry reader when the engine behind it has been replaced.
    ///
    /// The held peaks are reset with it: they describe a ring that no longer exists, and holding
    /// them would leave the meters frozen at whatever the retired engine's last block happened to
    /// be until the new one publishes — reading -inf for a moment after a restart is the truth.
    fn rebind_telemetry_if_stale(&mut self) {
        let generation = self.inner.telemetry_generation();
        if generation == self.telemetry_generation {
            return;
        }
        self.telemetry_generation = generation;
        self.telemetry = self.inner.telemetry_reader();
        self.input_peak_db = f32::NEG_INFINITY;
        self.output_peak_db = f32::NEG_INFINITY;
    }

    /// Reads whatever the engine has published since the last GUI frame and replaces the held
    /// readings with it.
    ///
    /// Both maxima are **local to this call**, deliberately: they combine what arrived within one
    /// drain — the two output channels, and any repeat entries from several blocks — and then
    /// *replace* the stored reading. Accumulating into the field instead is the ratchet described
    /// on [`ClapUiHost::output_peak_db`]. The assignment is guarded by `Option` rather than
    /// unconditional so that a frame which drained nothing holds the previous reading instead of
    /// flashing to silence; that is also why the fields cannot simply be reset at the top.
    fn drain_meters(&mut self) {
        self.rebind_telemetry_if_stale();
        let Some(reader) = self.telemetry.as_mut() else {
            return;
        };
        let input_id = telemetry_input_peak_id();
        let output_ids = [telemetry_output_peak_id(0), telemetry_output_peak_id(1)];
        let mut buf = [TelemetryEntry { id: 0, value: 0.0 }; 64];
        let mut input_peak: Option<f32> = None;
        let mut output_peak: Option<f32> = None;
        for _ in 0..MAX_DRAIN_BATCHES {
            let drain = reader.drain(&mut buf);
            for entry in &buf[..drain.read] {
                if entry.id == input_id {
                    input_peak = Some(entry.value);
                } else if output_ids.contains(&entry.id) {
                    output_peak =
                        Some(output_peak.map_or(entry.value, |held: f32| held.max(entry.value)));
                }
            }
            if drain.read < buf.len() {
                break;
            }
        }
        if let Some(peak) = input_peak {
            self.input_peak_db = peak;
        }
        if let Some(peak) = output_peak {
            self.output_peak_db = peak;
        }
    }
}

impl UiHost for ClapUiHost {
    fn snapshot(&mut self) -> UiSnapshot {
        self.drain_meters();
        UiSnapshot {
            params: self.inner.params.snapshot(),
            input_meter: MeterReading {
                peak_db: self.input_peak_db,
                rms_db: self.input_peak_db,
            },
            output_meter: MeterReading {
                peak_db: self.output_peak_db,
                rms_db: self.output_peak_db,
            },
            loaded_model_name: self.inner.nam_ref().map(|r| r.display_name),
            loaded_ir_name: self.inner.ir_ref().map(|r| r.display_name),
            library: self.inner.library_snapshot(),
            // FR-STATE-030's preset list. Enumerated off-thread and cached — see
            // `SharedInner::presets_snapshot`; a GUI frame never reads a directory.
            presets: self.inner.presets_snapshot(),
            // FR-IO-020's indicator is `None` here, and permanently: a CLAP plugin never opens an
            // audio device — the host owns it, and hands this plugin buffers it has already
            // captured — so there is no share mode this crate could report without inventing one.
            audio_mode: None,
            unsaved_changes: self.inner.is_dirty(),
            notices: self.inner.notices(),
        }
    }

    fn dispatch(&mut self, intent: UiIntent) {
        match intent {
            UiIntent::SetParam { key, value } => self.set_param(key, value),
            UiIntent::ResetParamToDefault { key } => {
                if let Some(descriptor) = REGISTRY.iter().find(|d| d.key == key) {
                    let default = default_value_of(descriptor);
                    self.set_param(key, default);
                }
            }
            UiIntent::SavePreset { name } => {
                worker_jobs::spawn_save_preset(Arc::clone(&self.inner), name);
                // Left out of the `mark_dirty` below with `RecallPreset`, and for the mirror-image
                // reason: a save is what *clears* the dirty flag, and the job that writes the file
                // is the one that clears it once the bytes are actually on disk.
                return;
            }
            UiIntent::RecallPreset { path } => {
                worker_jobs::spawn_recall_preset(Arc::clone(&self.inner), path);
                // **The carve-out**: a recall makes this instance *match* what was last
                // recalled, which is the definition of not-dirty (`UiSnapshot::unsaved_changes`).
                // Marking it dirty here would also race the job that marks it clean, so the
                // flag is left entirely to `adopt_document_bytes`, exactly as it is for a
                // host-driven `state` load. `SavePreset` is left out for the same reason from
                // the other direction.
                return;
            }
            UiIntent::LoadLibraryEntry(path) => {
                worker_jobs::spawn_load_library_entry(Arc::clone(&self.inner), path);
            }
            UiIntent::RescanLibraryRequested => self.inner.start_library_scan(),
            UiIntent::CancelScanRequested => self.inner.cancel_library_scan(),
            UiIntent::DismissNotice { id } => self.inner.dismiss_notice(id),
        }
        self.inner.mark_dirty();
    }
}

impl ClapUiHost {
    fn set_param(&self, key: &'static str, value: f32) {
        // `set_by_key_from_gui`, not `set_by_key`: this is the one path in this crate a user
        // gesture *inside the plugin's own editor* takes, so the change is also queued for
        // delivery to the host as automation (issue #94 — see
        // `crate::params_ext::emit_gui_param_changes`). Host-originated writes deliberately use
        // the unmarked setter, or the plugin would echo the host's automation back at it.
        self.inner.params.set_by_key_from_gui(key, value);
        let Some(descriptor) = REGISTRY.iter().find(|d| d.key == key) else {
            return;
        };
        self.inner.with_instance(|instance| {
            // "What the UI thread uses" — see `namir_worker::Instance::try_submit_param`'s own
            // doc comment. One attempt, never blocks; a param change that misses one block is not
            // worth stalling a GUI frame for (D-15.3).
            let _ = instance.try_submit_param(ParamChange {
                id: ParamId(descriptor.id.0),
                value,
            });
        });
    }
}

fn default_value_of(descriptor: &namir_params::ParamDescriptor) -> f32 {
    match descriptor.kind {
        namir_params::ParamKind::Continuous { default, .. } => default,
        namir_params::ParamKind::Stepped { default_index, .. } => default_index.0 as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_params::stages::trim;

    fn host() -> ClapUiHost {
        ClapUiHost::new(Arc::new(SharedInner::new()))
    }

    /// A host wired to a real telemetry ring, plus the producer end to publish into it.
    ///
    /// Every test above this point had no reader at all, which is exactly why the ratchet below
    /// survived from M6 to M13: `drain_meters` returned at its first line in every test that
    /// existed, so the only meter code in this crate was never executed by the suite at all.
    ///
    /// The reader is installed **through `SharedInner`**, the way `crate::audio`'s `activate` does
    /// it, rather than handed to the constructor — since issue #95 that is the only way it can be
    /// installed, and it is what lets the two tests below drive the transitions the old
    /// captured-once reader could not survive.
    fn host_with_telemetry() -> (ClapUiHost, namir_engine::TelemetryProducer) {
        let inner = Arc::new(SharedInner::new());
        let producer = install_ring(&inner);
        (ClapUiHost::new(inner), producer)
    }

    /// Installs a fresh telemetry ring on `inner`, exactly as an `activate()` would, and returns
    /// the producer end.
    fn install_ring(inner: &Arc<SharedInner>) -> namir_engine::TelemetryProducer {
        let (producer, reader) = namir_engine::telemetry_ring(256);
        inner.set_telemetry_reader(Some(reader));
        producer
    }

    fn publish(producer: &mut namir_engine::TelemetryProducer, id: u32, value: f32) {
        producer.push(TelemetryEntry { id, value });
    }

    #[test]
    fn the_output_meter_follows_the_signal_down_as_well_as_up() {
        // The reported bug, as its symptom: a loud transient followed by a quiet passage. Before
        // the fix the meter kept the transient's value for the life of the plugin instance, so it
        // read full and never moved again.
        let (mut h, mut p) = host_with_telemetry();
        let out0 = telemetry_output_peak_id(0);

        publish(&mut p, out0, -0.5);
        assert_eq!(h.snapshot().output_meter.peak_db, -0.5);

        publish(&mut p, out0, -42.0);
        assert_eq!(
            h.snapshot().output_meter.peak_db,
            -42.0,
            "the output meter must fall when the signal does, not hold its loudest ever reading"
        );
    }

    #[test]
    fn the_output_meter_takes_the_louder_channel_within_one_drain() {
        // The behaviour the ratchet was reaching for, and which must survive the fix: two channels
        // reported in the same drain collapse to the louder one.
        let (mut h, mut p) = host_with_telemetry();
        publish(&mut p, telemetry_output_peak_id(0), -30.0);
        publish(&mut p, telemetry_output_peak_id(1), -12.0);
        assert_eq!(h.snapshot().output_meter.peak_db, -12.0);

        // ...and that maximum does not leak into the next frame.
        publish(&mut p, telemetry_output_peak_id(0), -30.0);
        publish(&mut p, telemetry_output_peak_id(1), -31.0);
        assert_eq!(h.snapshot().output_meter.peak_db, -30.0);
    }

    #[test]
    fn the_input_meter_follows_the_signal_down_too() {
        // The input path was assigned rather than accumulated and so never had the bug; asserted
        // anyway, because the fix rewrote both branches and a regression here would be silent.
        let (mut h, mut p) = host_with_telemetry();
        let input = telemetry_input_peak_id();
        publish(&mut p, input, -3.0);
        assert_eq!(h.snapshot().input_meter.peak_db, -3.0);
        publish(&mut p, input, -55.0);
        assert_eq!(h.snapshot().input_meter.peak_db, -55.0);
    }

    #[test]
    fn a_frame_that_drains_nothing_holds_the_previous_reading() {
        // Why the assignment is guarded by `Option` rather than the fields being reset at the top
        // of the drain: a GUI frame can easily run between two engine blocks, and a meter that
        // flashed to silence on every such frame would be worse than one that lags by a frame.
        let (mut h, mut p) = host_with_telemetry();
        publish(&mut p, telemetry_output_peak_id(0), -9.0);
        publish(&mut p, telemetry_input_peak_id(), -6.0);
        assert_eq!(h.snapshot().output_meter.peak_db, -9.0);

        let snap = h.snapshot();
        assert_eq!(snap.output_meter.peak_db, -9.0);
        assert_eq!(snap.input_meter.peak_db, -6.0);
    }

    #[test]
    fn a_host_with_no_telemetry_reads_silent_forever() {
        let mut h = host();
        assert_eq!(h.snapshot().output_meter.peak_db, f32::NEG_INFINITY);
        assert_eq!(h.snapshot().input_meter.peak_db, f32::NEG_INFINITY);
    }

    /// **Issue #95, first half.** A host is entitled to open the editor before it ever activates
    /// the plugin — and the plugin's own `gui` sequence (`create`, `get_size`, `set_parent`) is
    /// all `[main-thread]` work with no audio configuration in sight, so this is the ordinary
    /// order, not an exotic one. The reader captured at editor-open was `None` then, and the
    /// meters read -inf for the editor's entire life.
    #[test]
    fn an_editor_opened_before_the_first_activation_still_gets_meters() {
        let inner = Arc::new(SharedInner::new());
        let mut h = ClapUiHost::new(Arc::clone(&inner));

        // The editor is already rendering frames, and there is nothing to show yet.
        assert_eq!(h.snapshot().output_meter.peak_db, f32::NEG_INFINITY);

        // ...and now the host activates the plugin, which installs a ring.
        let mut producer = install_ring(&inner);
        publish(&mut producer, telemetry_output_peak_id(0), -7.5);

        assert_eq!(
            h.snapshot().output_meter.peak_db,
            -7.5,
            "an editor opened before the first activate() must pick up the ring that activation \
             installs, not stay bound to the absence it saw at set_parent time"
        );
    }

    /// **Issue #95, second half.** Every deactivate/reactivate cycle installs a *fresh* ring
    /// (`crate::audio`'s `activate` builds a whole new engine), including the cycle the plugin
    /// itself asks for when its latency changes. A reader cloned once at editor-open goes on
    /// draining the retired ring, so the meters freeze at whatever the old engine last published.
    #[test]
    fn a_reactivation_rebinds_the_meters_to_the_new_ring() {
        let inner = Arc::new(SharedInner::new());
        let mut h = ClapUiHost::new(Arc::clone(&inner));

        let mut first = install_ring(&inner);
        publish(&mut first, telemetry_output_peak_id(0), -6.0);
        assert_eq!(h.snapshot().output_meter.peak_db, -6.0);

        // deactivate(): the engine, and its ring, are gone.
        inner.set_telemetry_reader(None);
        assert_eq!(
            h.snapshot().output_meter.peak_db,
            f32::NEG_INFINITY,
            "with no engine there is no signal, and holding the retired ring's last reading would \
             show a meter that is simply wrong"
        );

        // activate(): a new engine, a new ring.
        let mut second = install_ring(&inner);
        publish(&mut second, telemetry_output_peak_id(0), -21.0);
        assert_eq!(
            h.snapshot().output_meter.peak_db,
            -21.0,
            "the meters must follow the live engine across a restart -- publishing into the ring \
             the new activation installed must move them"
        );

        // The retired ring is genuinely no longer read.
        publish(&mut first, telemetry_output_peak_id(0), -1.0);
        assert_eq!(
            h.snapshot().output_meter.peak_db,
            -21.0,
            "a publish into the retired ring must not move the meters"
        );
    }

    /// **Issue #94.** A knob turned in the plugin's own editor is queued for delivery to the host
    /// as automation; nothing else that writes the mirror is.
    #[test]
    fn a_gui_param_change_is_queued_for_the_host_but_host_automation_is_not() {
        let mut h = host();
        assert!(!h.inner.params.has_gui_pending());

        h.dispatch(UiIntent::SetParam {
            key: trim::GAIN_DB.key,
            value: 3.0,
        });
        assert!(
            h.inner.params.has_gui_pending(),
            "a GUI-originated change must be queued for the host, or automation written from the \
             editor is silently lost"
        );
        h.inner.params.take_gui_pending();

        // The path host automation takes (`crate::audio`'s `apply_direct_and_mirror`) writes the
        // same mirror and must *not* queue anything, or the plugin echoes the host back at itself.
        h.inner.params.set_by_id(trim::GAIN_DB.id.0, 4.0);
        assert!(
            !h.inner.params.has_gui_pending(),
            "a host-originated change must not be reported back to the host"
        );
    }

    /// A reset gesture is a user gesture too, and reaches the host the same way.
    #[test]
    fn a_reset_to_default_is_queued_for_the_host_as_well() {
        let mut h = host();
        h.dispatch(UiIntent::ResetParamToDefault {
            key: trim::GAIN_DB.key,
        });
        assert!(h.inner.params.has_gui_pending());
    }

    /// FR-STATE-030: recalling a preset makes this instance *match* what was recalled, so it must
    /// not be left looking like it has unsaved changes.
    #[test]
    fn recalling_a_preset_does_not_mark_the_instance_dirty() {
        let mut h = host();
        assert!(!h.inner.is_dirty());
        h.dispatch(UiIntent::RecallPreset {
            path: std::path::PathBuf::from("no-such-preset.namirpreset"),
        });
        assert!(
            !h.inner.is_dirty(),
            "a recall is the definition of not-dirty; the job that adopts the document owns the \
             flag from here"
        );
    }

    #[test]
    fn snapshot_reflects_the_mirror() {
        let mut h = host();
        h.inner.params.set_by_key(trim::GAIN_DB.key, 2.5);
        let snap = h.snapshot();
        assert_eq!(snap.params.get(trim::GAIN_DB.key), Some(2.5));
    }

    #[test]
    fn dispatching_set_param_updates_the_mirror_and_marks_dirty() {
        let mut h = host();
        assert!(!h.inner.is_dirty());
        h.dispatch(UiIntent::SetParam {
            key: trim::GAIN_DB.key,
            value: 4.0,
        });
        assert_eq!(h.inner.params.snapshot().get(trim::GAIN_DB.key), Some(4.0));
        assert!(h.inner.is_dirty());
    }

    #[test]
    fn dispatching_reset_to_default_restores_the_registry_default() {
        let mut h = host();
        h.inner.params.set_by_key(trim::GAIN_DB.key, 9.0);
        h.dispatch(UiIntent::ResetParamToDefault {
            key: trim::GAIN_DB.key,
        });
        let namir_params::ParamKind::Continuous { default, .. } = trim::GAIN_DB.kind else {
            panic!("expected Continuous");
        };
        assert_eq!(
            h.inner.params.snapshot().get(trim::GAIN_DB.key),
            Some(default)
        );
    }

    #[test]
    fn dismiss_notice_removes_it_from_the_next_snapshot() {
        let mut h = host();
        h.inner
            .push_notice(crate::error_codes::LIBRARY_UNAVAILABLE, "detail");
        let id = h.snapshot().notices[0].id;
        h.dispatch(UiIntent::DismissNotice { id });
        assert!(h.snapshot().notices.is_empty());
    }

    #[test]
    fn an_unknown_param_key_from_reset_is_ignored_rather_than_panicking() {
        let mut h = host();
        h.dispatch(UiIntent::ResetParamToDefault {
            key: "not.a.real.key",
        });
        // No panic is the assertion.
    }
}
