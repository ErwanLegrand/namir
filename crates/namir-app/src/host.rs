//! [`AppHost`]: this crate's [`namir_ui::UiHost`] implementation — the bridge between
//! `namir-ui`'s pure view layer and this crate's real [`crate::instance::SharedInstance`],
//! [`crate::worker::WorkerHandle`] and [`namir_worker::library::LibraryService`].
//!
//! # The bridge's shape, one frame at a time
//!
//! [`namir_ui::host`]'s own module doc comment fixes the contract: `snapshot` once, render, then
//! `dispatch` every intent that frame produced. [`AppHost::snapshot`] does four things, all cheap
//! (no blocking, no filesystem I/O — every blocking operation already ran, or is running, on
//! [`crate::worker::WorkerHandle`]'s own thread):
//!
//! 1. Drains [`crate::worker::WorkerHandle`]'s event queue and folds every [`crate::worker::AppEvent`]
//!    into this host's own state (`loaded_model_name`/`loaded_ir_name`, `notices`, the library
//!    snapshot/scan progress, `unsaved_changes`).
//! 2. Drains the telemetry ring and converts the two readings FR-UI-020 needs into
//!    [`namir_ui::MeterReading`]s — `telemetry.trim.peak_db`/`average_db` for the input meter (Trim
//!    is the first real stage after Gate, so its own peak/average readings are the closest thing
//!    this chain has to "the signal as it enters the amp/cab chain"), `telemetry.out.ch*.peak_db`/
//!    `average_db` for the output meter (the maximum across whichever channels are active, since a
//!    stereo chain's two channels can differ and FR-UI-020 wants one number).
//! 3. Reads the shared `State` mirror for current parameter values.
//! 4. Reads the shared `LibraryService` for the current index/scan-progress snapshot.
//!
//! [`AppHost::dispatch`] turns each [`namir_ui::UiIntent`] into either an immediate, non-blocking
//! submission through [`crate::instance::SharedInstance`] (`SetParam`/`ResetParamToDefault`, via
//! `namir_worker::Instance::try_submit_param`) or an [`crate::worker::AppCommand`] handed to the
//! worker thread (`LoadLibraryEntry`/`RescanLibraryRequested`/`CancelScanRequested`) — see
//! [`crate::worker`]'s module doc comment for why load/scan don't also go through the direct path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use namir_core::ErrorCode;
use namir_engine::{ParamChange, ParamId as EngineParamId, TelemetryEntry, TelemetryReader};
use namir_params::REGISTRY;
use namir_state::State;
use namir_ui::{
    AudioModeStatus, AudioShareMode, LibrarySnapshot, MeterReading, UiHost, UiIntent, UiNotice,
    UiSnapshot,
};
use namir_worker::Target;
use namir_worker::library::LibraryService;

use crate::instance::SharedInstance;
use crate::worker::{AppCommand, AppEvent, LoadOutcomeSummary, WorkerHandle};

/// This crate's own catalogue entries for the notices [`AppHost`] itself synthesises (as opposed
/// to ones that already carry a `namir_core::ErrorCode`, like a load failure).
mod local_error_codes {
    use namir_core::{ErrorCode, Severity};

    pub const LOAD_FAILED: ErrorCode = ErrorCode {
        id: "app.host.load_failed",
        severity: Severity::Error,
        message_template: "Could not load {source}: {reason}.",
    };
    pub const LOAD_NOT_DELIVERED: ErrorCode = ErrorCode {
        id: "app.host.load_not_delivered",
        severity: Severity::Error,
        message_template: "{source} was prepared but could not be handed to the audio engine in \
                            time.",
    };
    pub const SCAN_SAVE_FAILED: ErrorCode = ErrorCode {
        id: "app.host.scan_save_failed",
        severity: Severity::Warning,
        message_template: "The library scan finished but its results could not be saved: {reason}.",
    };
    pub const STATE_SAVE_FAILED: ErrorCode = ErrorCode {
        id: "app.host.state_save_failed",
        severity: Severity::Error,
        message_template: "Could not save {path}: {reason}.",
    };
    pub const STATE_LOAD_FAILED: ErrorCode = ErrorCode {
        id: "app.host.state_load_failed",
        severity: Severity::Error,
        message_template: "Could not load {path}: {reason}.",
    };
    pub const REFERENCE_MISSING: ErrorCode = ErrorCode {
        id: "app.host.reference_missing",
        severity: Severity::Warning,
        message_template: "{name} could not be found and was left unloaded.",
    };

    /// FR-IO-070: which catalogue entry a [`crate::stream::Direction`]-tagged stream failure maps
    /// to. Both directions use `crate::error_codes::DEVICE_LOST` today (the input/output
    /// distinction is carried in the notice's `detail` text instead) since this crate's own
    /// catalogue does not yet distinguish "input device lost" from "output device lost" as
    /// separate ids — nothing in FR-IO-070 requires it to.
    pub fn stream_failure_code(_direction: crate::stream::Direction) -> ErrorCode {
        crate::error_codes::DEVICE_LOST
    }
}

const TELEMETRY_TRIM_PEAK_DB: u32 = namir_params::ParamId::from_key("telemetry.trim.peak_db").0;
const TELEMETRY_TRIM_AVERAGE_DB: u32 =
    namir_params::ParamId::from_key("telemetry.trim.average_db").0;

fn out_channel_peak_id(index: usize) -> u32 {
    namir_params::ParamId::from_key(&format!("telemetry.out.ch{index}.peak_db")).0
}
fn out_channel_average_id(index: usize) -> u32 {
    namir_params::ParamId::from_key(&format!("telemetry.out.ch{index}.average_db")).0
}

/// How many output channels [`AppHost::snapshot`] scans for telemetry — comfortably above any
/// channel count this build's `ChannelConfig` ever produces (at most 2).
const MAX_OUTPUT_CHANNELS_SCANNED: usize = 2;

/// Telemetry entries drained per frame. `namir-engine`'s own `TELEMETRY_SCRATCH_ENTRIES` (64) is
/// the whole real chain's per-block count; this is sized the same so a frame never sees "missed"
/// entries from its own drain being too small.
const TELEMETRY_DRAIN_BATCH: usize = 64;

/// The one-line conversion FR-IO-020's mode indicator needs across D-5.1's seam: `namir-ui` cannot
/// depend on this crate, so it declares its own [`AudioShareMode`] and this crate maps onto it here
/// — in the bridge module, alongside every other snapshot field's translation, rather than in
/// [`crate::audio_io`], which is deliberately kept to the `cpal` boundary and knows nothing about a
/// UI.
impl From<crate::audio_io::ShareMode> for AudioShareMode {
    fn from(mode: crate::audio_io::ShareMode) -> Self {
        match mode {
            crate::audio_io::ShareMode::Shared => Self::Shared,
            crate::audio_io::ShareMode::Exclusive => Self::Exclusive,
        }
    }
}

fn basename(path_or_desc: &str) -> String {
    std::path::Path::new(path_or_desc)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_or_desc.to_string())
}

/// This crate's [`UiHost`] implementation. See this module's doc comment for the full bridge
/// design.
pub struct AppHost {
    instance: SharedInstance,
    worker: WorkerHandle,
    telemetry: TelemetryReader,
    library: Arc<LibraryService>,
    state: Arc<Mutex<State>>,
    last_saved: State,

    loaded_model_name: Option<String>,
    loaded_ir_name: Option<String>,
    /// FR-IO-020's mode indicator, settled once by [`crate::app::run`] before this host exists and
    /// never changed afterwards — a share mode is fixed for the lifetime of the streams, so unlike
    /// every other snapshot field this one is a constant handed in at construction rather than
    /// something folded in from a worker event. `None` when there is no audio device at all
    /// (`crate::app`'s `open_window_without_audio` path).
    audio_mode: Option<AudioModeStatus>,
    input_meter: MeterReading,
    output_meter: MeterReading,
    scan_progress: Option<namir_library::ScanProgress>,
    notices: Vec<UiNotice>,
    next_notice_id: AtomicU64,
}

impl AppHost {
    /// Builds a host from its already-constructed parts. Construction of the engine, worker
    /// thread, library service etc. is [`crate::app`]'s job — this constructor just assembles
    /// what it is handed.
    pub fn new(
        instance: SharedInstance,
        worker: WorkerHandle,
        telemetry: TelemetryReader,
        library: Arc<LibraryService>,
        state: Arc<Mutex<State>>,
        audio_mode: Option<AudioModeStatus>,
    ) -> Self {
        let last_saved = state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Self {
            instance,
            worker,
            telemetry,
            library,
            state,
            last_saved,
            loaded_model_name: None,
            loaded_ir_name: None,
            audio_mode,
            input_meter: MeterReading::default(),
            output_meter: MeterReading::default(),
            scan_progress: None,
            notices: Vec::new(),
            next_notice_id: AtomicU64::new(1),
        }
    }

    fn push_notice(&mut self, code: ErrorCode, detail: impl Into<String>) {
        let id = self.next_notice_id.fetch_add(1, Ordering::Relaxed);
        self.notices.push(UiNotice {
            id,
            code,
            detail: detail.into(),
        });
    }

    /// Public wrapper over [`Self::push_notice`] for [`crate::app`]'s own startup-time warnings
    /// (a remembered device unavailable, a settings file that failed to load, ...) — anything
    /// this crate wants surfaced through the same FR-UI-070 notice list, from before the host is
    /// otherwise driven by worker events.
    pub fn report(&mut self, code: ErrorCode, detail: impl Into<String>) {
        self.push_notice(code, detail);
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::LoadFinished {
                target,
                source,
                outcome,
            } => self.handle_load_finished(target, source, outcome),
            AppEvent::ScanProgress(progress) => self.scan_progress = Some(progress),
            AppEvent::ScanFinished(outcome) => {
                self.scan_progress = None;
                for warning in outcome.warnings {
                    self.push_notice(
                        namir_core::ErrorCode {
                            id: "app.host.scan_warning",
                            severity: namir_core::Severity::Warning,
                            message_template: "{detail}",
                        },
                        warning,
                    );
                }
                if let Some(reason) = outcome.save_error {
                    self.push_notice(local_error_codes::SCAN_SAVE_FAILED, reason);
                }
            }
            AppEvent::StateSaved { path, error } => {
                if let Some(reason) = error {
                    self.push_notice(
                        local_error_codes::STATE_SAVE_FAILED,
                        format!("{}: {reason}", path.display()),
                    );
                } else {
                    self.last_saved = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
                }
            }
            AppEvent::StateLoaded {
                path,
                outcome,
                error,
            } => {
                if let Some(reason) = error {
                    self.push_notice(
                        local_error_codes::STATE_LOAD_FAILED,
                        format!("{}: {reason}", path.display()),
                    );
                    return;
                }
                if let Some(outcome) = outcome {
                    self.apply_recall_summary(outcome);
                }
                self.last_saved = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
            }
            AppEvent::StreamFailure { direction, detail } => {
                let code = local_error_codes::stream_failure_code(direction);
                self.push_notice(code, detail);
            }
        }
    }

    fn handle_load_finished(
        &mut self,
        target: Target,
        source: String,
        outcome: LoadOutcomeSummary,
    ) {
        match outcome {
            LoadOutcomeSummary::Loaded { warning } => {
                let name = Some(basename(&source));
                match target {
                    Target::Nam => self.loaded_model_name = name,
                    Target::Ir => self.loaded_ir_name = name,
                }
                if let Some(detail) = warning {
                    self.push_notice(namir_worker::error_codes::IR_TRUNCATED, detail);
                }
            }
            LoadOutcomeSummary::Unloaded => match target {
                Target::Nam => self.loaded_model_name = None,
                Target::Ir => self.loaded_ir_name = None,
            },
            LoadOutcomeSummary::Failed(reason) => {
                self.push_notice(
                    local_error_codes::LOAD_FAILED,
                    format!("{source}: {reason}"),
                );
            }
            LoadOutcomeSummary::NotDelivered => {
                self.push_notice(local_error_codes::LOAD_NOT_DELIVERED, source);
            }
        }
    }

    fn apply_recall_summary(&mut self, outcome: crate::worker::RecallOutcomeSummary) {
        match outcome.nam {
            LoadOutcomeSummary::Loaded { .. } => {}
            LoadOutcomeSummary::Unloaded => self.loaded_model_name = None,
            _ => {}
        }
        match outcome.ir {
            LoadOutcomeSummary::Loaded { .. } => {}
            LoadOutcomeSummary::Unloaded => self.loaded_ir_name = None,
            _ => {}
        }
        if let Some(name) = outcome.nam_missing {
            self.push_notice(local_error_codes::REFERENCE_MISSING, name);
        }
        if let Some(name) = outcome.ir_missing {
            self.push_notice(local_error_codes::REFERENCE_MISSING, name);
        }
    }

    fn read_meters(&mut self) {
        let mut buf = [TelemetryEntry { id: 0, value: 0.0 }; TELEMETRY_DRAIN_BATCH];
        let drain = self.telemetry.drain(&mut buf);
        let mut trim_peak = None;
        let mut trim_average = None;
        let mut out_peak: Option<f32> = None;
        let mut out_average: Option<f32> = None;

        for entry in &buf[..drain.read] {
            if entry.id == TELEMETRY_TRIM_PEAK_DB {
                trim_peak = Some(entry.value);
            } else if entry.id == TELEMETRY_TRIM_AVERAGE_DB {
                trim_average = Some(entry.value);
            } else {
                for ch in 0..MAX_OUTPUT_CHANNELS_SCANNED {
                    if entry.id == out_channel_peak_id(ch) {
                        out_peak = Some(out_peak.map_or(entry.value, |v: f32| v.max(entry.value)));
                    } else if entry.id == out_channel_average_id(ch) {
                        out_average =
                            Some(out_average.map_or(entry.value, |v: f32| v.max(entry.value)));
                    }
                }
            }
        }

        if let Some(peak) = trim_peak {
            self.input_meter.peak_db = peak;
        }
        if let Some(avg) = trim_average {
            self.input_meter.rms_db = avg;
        }
        if let Some(peak) = out_peak {
            self.output_meter.peak_db = peak;
        }
        if let Some(avg) = out_average {
            self.output_meter.rms_db = avg;
        }
    }
}

impl UiHost for AppHost {
    fn snapshot(&mut self) -> UiSnapshot {
        for event in self.worker.drain_events() {
            self.handle_event(event);
        }
        self.read_meters();

        let params = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .params
            .clone();
        let unsaved_changes =
            *self.state.lock().unwrap_or_else(|e| e.into_inner()) != self.last_saved;
        let index = self.library.snapshot();

        UiSnapshot {
            params,
            input_meter: self.input_meter,
            output_meter: self.output_meter,
            loaded_model_name: self.loaded_model_name.clone(),
            loaded_ir_name: self.loaded_ir_name.clone(),
            library: LibrarySnapshot {
                index,
                scan: self.scan_progress,
            },
            audio_mode: self.audio_mode.clone(),
            unsaved_changes,
            notices: self.notices.clone(),
        }
    }

    fn dispatch(&mut self, intent: UiIntent) {
        match intent {
            UiIntent::SetParam { key, value } => {
                let Some(descriptor) = REGISTRY.iter().find(|d| d.key == key) else {
                    return;
                };
                {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = state.params.set(key, value);
                }
                self.instance.with(|i| {
                    // "What the UI thread uses" -- see `Instance::try_submit_param`'s own doc
                    // comment. One attempt, never blocks; D-15.3 doesn't think a knob turn is worth
                    // stalling a GUI frame for.
                    let _ = i.try_submit_param(ParamChange {
                        id: EngineParamId(descriptor.id.0),
                        value,
                    });
                });
            }
            UiIntent::ResetParamToDefault { key } => {
                let Some(descriptor) = REGISTRY.iter().find(|d| d.key == key) else {
                    return;
                };
                let default = default_value_of(descriptor);
                {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = state.params.set(key, default);
                }
                self.instance.with(|i| {
                    let _ = i.try_submit_param(ParamChange {
                        id: EngineParamId(descriptor.id.0),
                        value: default,
                    });
                });
            }
            UiIntent::LibraryQueryChanged(_) => {
                // Pure view-side filtering state (`namir_ui::library_view::LibraryViewState`);
                // this host has nothing to do -- the query never touches engine/library state.
            }
            UiIntent::LoadLibraryEntry(path) => {
                self.worker.send(AppCommand::LoadLibraryEntry(path));
            }
            UiIntent::RescanLibraryRequested => {
                self.worker.send(AppCommand::RescanLibrary);
            }
            UiIntent::CancelScanRequested => {
                self.worker.send(AppCommand::CancelScan);
            }
            UiIntent::DismissNotice { id } => {
                self.notices.retain(|n| n.id != id);
            }
        }
    }
}

fn default_value_of(descriptor: &namir_params::ParamDescriptor) -> f32 {
    match descriptor.kind {
        namir_params::ParamKind::Continuous { default, .. } => default,
        namir_params::ParamKind::Stepped { default_index, .. } => default_index.0 as f32,
    }
}

/// Requests a preset save (FR-STATE-010) — not a [`UiIntent`] today (`namir-ui`'s FR-UI-020 screen
/// has no save/load control yet; that is FR-UI's own scope, not this crate's), but exposed here so
/// [`crate::app`] can wire a future menu/shortcut to it without reaching into [`AppHost`]'s private
/// fields.
impl AppHost {
    /// Requests a save to `path`.
    pub fn save_state(&self, path: PathBuf) {
        self.worker.send(AppCommand::SaveState(path));
    }

    /// Requests a load-and-recall from `path`.
    pub fn load_state(&self, path: PathBuf) {
        self.worker.send(AppCommand::LoadState(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::{ChannelConfig, SampleRate};
    use namir_engine::{PrepareContext, RingCapacities, build_default_chain, split};
    use namir_worker::pool::ThreadPool;
    use namir_worker::{EngineConfig, Instance, ResourceCache};

    const SR: u32 = 48_000;
    const BLOCK: usize = 64;

    fn ctx() -> PrepareContext {
        PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Stereo).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("namir-app-host-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Builds a real (no-hardware) `AppHost` wired to a real engine, exactly the way
    /// `namir-worker`'s own tests build a real `Instance` against `build_default_engine` -- no
    /// mock `UiHost` needed, since this host's whole job is bridging to real, already-tested
    /// crates.
    fn build_host(dir: &std::path::Path) -> (AppHost, namir_engine::AudioEngine) {
        build_host_with_audio_mode(dir, None)
    }

    fn build_host_with_audio_mode(
        dir: &std::path::Path,
        audio_mode: Option<AudioModeStatus>,
    ) -> (AppHost, namir_engine::AudioEngine) {
        let c = ctx();
        let chain = build_default_chain(&c).unwrap();
        let (engine, endpoint) = split(chain, RingCapacities::default());
        // `TelemetryReader` is `Clone` (D-7.3), cloned before `endpoint` is consumed by
        // `Instance::new` -- see `crate::instance`'s module doc comment.
        let telemetry = endpoint.telemetry.clone();
        let instance =
            crate::instance::SharedInstance::new(Instance::new(EngineConfig { ctx: c }, endpoint));
        let cache = Arc::new(ResourceCache::new());

        let (library, _warnings) = namir_worker::library::LibraryService::open_at(dir);
        let roots = library.roots().to_vec();
        let library = Arc::new(library);
        let pool = ThreadPool::with_threads(1);

        let state = Arc::new(Mutex::new(State::defaults()));
        let worker_ctx = crate::worker::WorkerContext {
            instance: instance.clone(),
            cache,
            library: Arc::clone(&library),
            pool,
            library_roots: roots,
            state: Arc::clone(&state),
        };
        let worker = WorkerHandle::spawn(worker_ctx);

        let host = AppHost::new(instance, worker, telemetry, library, state, audio_mode);
        (host, engine)
    }

    /// A default snapshot matches `UiSnapshot::default`'s own shape in every field this host
    /// controls directly.
    #[test]
    fn a_fresh_host_produces_a_default_looking_snapshot() {
        let dir = temp_dir("fresh_snapshot");
        let (mut host, _engine) = build_host(&dir);
        let snapshot = host.snapshot();
        assert!(snapshot.loaded_model_name.is_none());
        assert!(snapshot.loaded_ir_name.is_none());
        assert!(snapshot.audio_mode.is_none());
        assert!(!snapshot.unsaved_changes);
        assert!(snapshot.notices.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FR-IO-020: the granted share mode reaches the screen. It is handed in at construction (a
    /// share mode cannot change while streams are open) and must survive every subsequent snapshot,
    /// not just the first -- the meters and library fields around it are rebuilt every frame.
    #[test]
    fn the_granted_share_mode_reaches_every_snapshot_not_only_the_first() {
        let dir = temp_dir("audio_mode");
        let granted = AudioModeStatus {
            share_mode: AudioShareMode::Exclusive,
            device_name: "Scarlett 2i2".to_string(),
        };
        let (mut host, _engine) = build_host_with_audio_mode(&dir, Some(granted.clone()));
        assert_eq!(host.snapshot().audio_mode.as_ref(), Some(&granted));
        assert_eq!(host.snapshot().audio_mode.as_ref(), Some(&granted));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `namir-ui` mirror of `crate::audio_io::ShareMode` maps value for value -- a swap here
    /// would make the indicator report the opposite of what was granted, which §18 of the roadmap
    /// rules out explicitly.
    #[test]
    fn the_share_mode_conversion_across_the_ui_seam_preserves_which_mode_it_is() {
        assert_eq!(
            AudioShareMode::from(crate::audio_io::ShareMode::Exclusive),
            AudioShareMode::Exclusive
        );
        assert_eq!(
            AudioShareMode::from(crate::audio_io::ShareMode::Shared),
            AudioShareMode::Shared
        );
    }

    /// **The end-to-end proof of `crate::instance::SharedInstance`'s whole reason for existing:** a
    /// `SetParam` intent dispatched through `AppHost` -- via `Instance::try_submit_param`, not a
    /// bespoke submitter -- reaches the real audio thread.
    #[test]
    fn set_param_intent_reaches_the_audio_thread() {
        let dir = temp_dir("set_param");
        let (mut host, mut engine) = build_host(&dir);
        let key = namir_params::stages::trim::GAIN_DB.key;
        // -24.0 is trim.gain_db's own minimum (namir_params::stages::trim::GAIN_DB's descriptor);
        // deliberately at the floor rather than an out-of-range value, since `ParamValues::set`
        // clamps to the descriptor's range and this test also checks the exact stored value.
        host.dispatch(UiIntent::SetParam { key, value: -24.0 });

        // The gain ramp needs several blocks to settle -- see `namir-worker`'s own
        // `try_submit_param_delivers_a_plain_parameter_change` test for why one block is not
        // enough, and why `left`/`right`/`channels` are rebuilt fresh each iteration rather than
        // reused across the loop.
        let mut left = [0.0f32; BLOCK];
        let mut right = [0.0f32; BLOCK];
        for _ in 0..100 {
            left.fill(0.5);
            right.fill(0.5);
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
            engine.process(&mut io);
        }
        assert!(left.last().unwrap().abs() < 0.05);

        let snapshot = host.snapshot();
        assert_eq!(snapshot.params.get(key), Some(-24.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FR-UI-050's reset gesture: dispatching `ResetParamToDefault` restores the descriptor's own
    /// default.
    #[test]
    fn reset_param_to_default_restores_the_descriptor_default() {
        let dir = temp_dir("reset_param");
        let (mut host, _engine) = build_host(&dir);
        let key = namir_params::stages::trim::GAIN_DB.key;
        host.dispatch(UiIntent::SetParam { key, value: -30.0 });
        host.dispatch(UiIntent::ResetParamToDefault { key });
        let snapshot = host.snapshot();
        assert_eq!(
            snapshot.params.get(key),
            Some(namir_state::ParamValues::defaults().get(key).unwrap())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `SetParam` marks the state unsaved; saving clears it again once the worker reports
    /// success.
    #[test]
    fn saving_clears_unsaved_changes() {
        let dir = temp_dir("save_clears");
        let (mut host, _engine) = build_host(&dir);
        let key = namir_params::stages::trim::GAIN_DB.key;
        host.dispatch(UiIntent::SetParam { key, value: -12.0 });
        assert!(host.snapshot().unsaved_changes);

        let save_path = dir.join("preset.namirpreset");
        host.save_state(save_path);

        // Poll until the worker thread's StateSaved event lands -- deterministic in that the
        // event is guaranteed to arrive eventually, bounded here only by test-timeout hygiene.
        let mut snapshot = host.snapshot();
        for _ in 0..200 {
            if !snapshot.unsaved_changes {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            snapshot = host.snapshot();
        }
        assert!(
            !snapshot.unsaved_changes,
            "save should have cleared unsaved_changes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A load failure (garbage bytes) surfaces as a notice rather than being silently dropped.
    #[test]
    fn a_load_failure_surfaces_as_a_notice() {
        let dir = temp_dir("load_failure");
        let (mut host, _engine) = build_host(&dir);
        let bad_file = dir.join("bad.nam");
        std::fs::write(&bad_file, b"not a nam file").unwrap();
        host.dispatch(UiIntent::LoadLibraryEntry(bad_file));

        let mut snapshot = host.snapshot();
        for _ in 0..200 {
            if !snapshot.notices.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            snapshot = host.snapshot();
        }
        assert!(
            !snapshot.notices.is_empty(),
            "a load failure should produce a notice"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FR-UI-070: dismissing a notice removes exactly that one.
    #[test]
    fn dismiss_notice_removes_only_the_named_notice() {
        let dir = temp_dir("dismiss");
        let (mut host, _engine) = build_host(&dir);
        host.push_notice(local_error_codes::LOAD_FAILED, "a");
        host.push_notice(local_error_codes::LOAD_FAILED, "b");
        let first_id = host.notices[0].id;
        host.dispatch(UiIntent::DismissNotice { id: first_id });
        assert_eq!(host.notices.len(), 1);
        assert_ne!(host.notices[0].id, first_id);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
