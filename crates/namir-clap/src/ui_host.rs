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
    telemetry: Option<TelemetryReader>,
    input_peak_db: f32,
    output_peak_db: f32,
}

impl ClapUiHost {
    pub(crate) fn new(inner: Arc<SharedInner>, telemetry: Option<TelemetryReader>) -> Self {
        Self {
            inner,
            telemetry,
            input_peak_db: f32::NEG_INFINITY,
            output_peak_db: f32::NEG_INFINITY,
        }
    }

    fn drain_meters(&mut self) {
        let Some(reader) = self.telemetry.as_mut() else {
            return;
        };
        let input_id = telemetry_input_peak_id();
        let output_ids = [telemetry_output_peak_id(0), telemetry_output_peak_id(1)];
        let mut buf = [TelemetryEntry { id: 0, value: 0.0 }; 64];
        for _ in 0..MAX_DRAIN_BATCHES {
            let drain = reader.drain(&mut buf);
            for entry in &buf[..drain.read] {
                if entry.id == input_id {
                    self.input_peak_db = entry.value;
                } else if output_ids.contains(&entry.id) {
                    self.output_peak_db = self.output_peak_db.max(entry.value);
                }
            }
            if drain.read < buf.len() {
                break;
            }
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
            UiIntent::LibraryQueryChanged(_query) => {
                // FR-UI-060's filtering is computed inside `namir-ui` itself from the raw index
                // this host already hands it every frame (`library_view`'s own module doc
                // comment); the query text has nothing for a `UiHost` to act on.
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
        self.inner.params.set_by_key(key, value);
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
        ClapUiHost::new(Arc::new(SharedInner::new()), None)
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
