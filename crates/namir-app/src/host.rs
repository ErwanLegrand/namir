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
use std::time::{Duration, Instant};

use namir_core::ErrorCode;
use namir_engine::{ParamChange, ParamId as EngineParamId, TelemetryEntry, TelemetryReader};
use namir_params::REGISTRY;
use namir_state::State;
use namir_ui::{
    AudioModeStatus, AudioShareMode, LibrarySnapshot, MeterReading, PresetSummary, UiHost,
    UiIntent, UiNotice, UiSnapshot,
};
use namir_worker::Target;
use namir_worker::library::LibraryService;

use crate::audio_io::StreamFailure;
use crate::instance::SharedInstance;
use crate::stream::{Direction, RunningStreams, ThreadPriorityReport};
use crate::worker::{AppCommand, AppEvent, LoadOutcomeSummary, WorkerHandle};

/// This crate's own catalogue entries for the notices [`AppHost`] itself synthesises (as opposed
/// to ones that already carry a `namir_core::ErrorCode`, like a load failure).
pub(crate) mod local_error_codes {
    use namir_core::{ErrorCode, Severity};

    use crate::audio_io::StreamFailure;

    pub const LOAD_FAILED: ErrorCode = ErrorCode::new(
        "app.host.load_failed",
        Severity::Error,
        "The file could not be loaded ({detail}).",
        "The detail names the specific reason. Pick another file in the library, or fix this one \
         and load it again -- audio keeps running on whatever was loaded before.",
    );
    pub const LOAD_NOT_DELIVERED: ErrorCode = ErrorCode::new(
        "app.host.load_not_delivered",
        Severity::Error,
        "The file was prepared but could not be handed to the audio engine in time ({detail}).",
        "Load it again. Nothing was lost and whatever was loaded before is still playing.",
    );
    pub const SCAN_SAVE_FAILED: ErrorCode = ErrorCode::new(
        "app.host.scan_save_failed",
        Severity::Warning,
        "The library scan finished but its results could not be saved ({detail}).",
        "The library list on screen is current; only the index file on disk is stale. Check \
         Namir's configuration directory is writable, then rescan.",
    );
    pub const STATE_SAVE_FAILED: ErrorCode = ErrorCode::new(
        "app.host.state_save_failed",
        Severity::Error,
        "The preset could not be saved ({detail}).",
        "Save it somewhere you can write to. Your settings are unchanged and still on screen, so \
         nothing is lost by trying again.",
    );
    pub const STATE_LOAD_FAILED: ErrorCode = ErrorCode::new(
        "app.host.state_load_failed",
        Severity::Error,
        "The preset could not be loaded ({detail}).",
        "Choose another preset file. The current settings were left untouched, so nothing was \
         half-applied.",
    );
    pub const REFERENCE_MISSING: ErrorCode = ErrorCode::new(
        "app.host.reference_missing",
        Severity::Warning,
        "A file this preset refers to could not be found and was left unloaded ({detail}).",
        "Put the file back where it was, or load a replacement from the library and save the \
         preset again. A library rescan also helps -- Namir can find a moved file by its content \
         hash once the library has seen it.",
    );
    // **Retired at M14 and kept as a tombstone, not deleted.** `app.host.scan_warning` was the id
    // `AppHost::handle` built inline for each warning a finished library scan reports -- a real,
    // user-visible error path whose code belonged to no catalogue, which is what M14's first pass
    // moved here. This pass removed its last use: every scan warning already arrives as a
    // `namir_worker::WorkerError` carrying its own `library.scan.*` entry, and wrapping that in a
    // generic `{detail}` template was one of the layers issue #39 found re-rendering the same
    // text. Recorded here rather than dropped so the id is not silently reassigned to something
    // else later; `namir-params`' tombstones are the same discipline for parameter identifiers.
    // A plain comment, not a doc comment: a `///` block with no item under it would attach itself
    // to whatever comes next, which is the function below.
    //
    // Retired id: `app.host.scan_warning`.

    pub const PRESET_NAME_REFUSED: ErrorCode = ErrorCode::new(
        "app.host.preset_name_refused",
        Severity::Warning,
        "That preset name cannot be used ({detail}).",
        "Choose a name without a slash, backslash, colon, asterisk, question mark, quote, angle \
         bracket or vertical bar. Nothing was written, and your settings are unchanged.",
    );
    pub const PRESET_LOCATION_UNKNOWN: ErrorCode = ErrorCode::new(
        "app.host.preset_location_unknown",
        Severity::Warning,
        "Presets cannot be saved or listed: this environment has no per-user configuration \
         directory ({detail}).",
        "Audio and every other feature still work; only named presets are unavailable. This is \
         the same degradation that stops Namir remembering your audio device between launches.",
    );

    /// FR-IO-070: which catalogue entry a stream failure maps to.
    ///
    /// **It takes the classification, not the direction (issue #44).** Until M14 it took a
    /// [`crate::stream::Direction`] and ignored it, returning `DEVICE_LOST` unconditionally — so
    /// *every* stream error was reported to the user as an unplugged device, whatever the backend
    /// actually said. The 2026-08-27 manual run is why that is more than a tidiness point: the
    /// physical unplug it induced arrived as [`StreamFailure::Other`] carrying an unmapped OS
    /// error rather than as `DeviceLost`, and Namir named it correctly *by luck*, because the one
    /// answer it could give happened to be the right one. `crate::audio_io` now recovers the
    /// device-loss cases that reach `Other`; anything still unclassified is reported as
    /// [`crate::error_codes::STREAM_FAILED`], which does not claim a device went away.
    ///
    /// The input/output distinction stays in the notice's `detail`, as before — nothing in
    /// FR-IO-070 asks for separate ids per direction, and issue #43's duplicate-notice fix
    /// depends on the two directions' details differing, which they now do.
    pub fn stream_failure_code(failure: &StreamFailure) -> ErrorCode {
        match failure {
            StreamFailure::DeviceLost => crate::error_codes::DEVICE_LOST,
            // Not reported as a notice today (`crate::app`'s callback counts xruns instead), but
            // matched rather than folded into the catch-all so adding that report later cannot
            // silently pick up the wrong entry.
            StreamFailure::Xrun => crate::error_codes::STREAM_FAILED,
            StreamFailure::Other(message) => {
                if crate::audio_io::classifies_as_device_loss(message.as_str()) {
                    crate::error_codes::DEVICE_LOST
                } else {
                    crate::error_codes::STREAM_FAILED
                }
            }
        }
    }
}

const TELEMETRY_TRIM_PEAK_DB: u32 = namir_params::ParamId::from_key("telemetry.trim.peak_db").0;
const TELEMETRY_TRIM_AVERAGE_DB: u32 =
    namir_params::ParamId::from_key("telemetry.trim.average_db").0;

/// How many output channels [`AppHost::snapshot`] scans for telemetry — comfortably above any
/// channel count this build's `ChannelConfig` ever produces (at most 2).
const MAX_OUTPUT_CHANNELS_SCANNED: usize = 2;

/// The per-output-channel telemetry ids [`AppHost::read_meters`] matches each drained entry
/// against, resolved **once, at compile time** (issue #90).
///
/// These used to be two functions calling `ParamId::from_key(&format!("telemetry.out.ch{index}.
/// peak_db"))`, invoked from *inside* the per-entry loop — so a frame draining a full
/// [`TELEMETRY_DRAIN_BATCH`] rebuilt the same four constant strings up to 256 times, each one a
/// heap allocation plus a key hash, at frame rate. `ParamId::from_key` is a `const fn` (that is
/// how [`TELEMETRY_TRIM_PEAK_DB`] above is already written), so the whole cost is removable
/// rather than merely reducible: the arrays below are the same four numbers, computed by the
/// compiler.
///
/// Written out entry by entry rather than generated in a loop because a `const` initialiser
/// cannot `format!` a key at all — which is the point. Both are declared with
/// [`MAX_OUTPUT_CHANNELS_SCANNED`] as their length, so raising the scan width without writing the
/// matching keys here fails to compile rather than silently ignoring the new channel.
const TELEMETRY_OUT_PEAK_DB: [u32; MAX_OUTPUT_CHANNELS_SCANNED] = [
    namir_params::ParamId::from_key("telemetry.out.ch0.peak_db").0,
    namir_params::ParamId::from_key("telemetry.out.ch1.peak_db").0,
];
const TELEMETRY_OUT_AVERAGE_DB: [u32; MAX_OUTPUT_CHANNELS_SCANNED] = [
    namir_params::ParamId::from_key("telemetry.out.ch0.average_db").0,
    namir_params::ParamId::from_key("telemetry.out.ch1.average_db").0,
];

/// How many stream failures [`AppHost::snapshot`] turns into notices in one frame. Bounds the
/// per-frame work regardless of how fast a failing backend reports; anything left waits for the
/// next frame, and a ring that overflows in the meantime drops the excess at the producer end
/// (see [`crate::app`]'s `stream_failure_sink`).
const STREAM_FAILURE_DRAIN_BATCH: usize = 8;

/// How stale [`AppHost`]'s cached preset listing may get before another enumeration is requested.
///
/// A GUI frame must not `read_dir` ([`UiHost::snapshot`]'s own contract), so the listing is
/// refreshed by [`crate::worker`] and each frame renders whatever the last one produced. One
/// second is short enough that a preset saved from the *plugin* (or from another copy of this
/// application) appears while the user is still looking for it, and long enough that a 60 Hz
/// window is not listing a directory 60 times a second. The same cadence `namir-clap`'s
/// `SharedInner::presets_snapshot` uses, for the same reason.
const PRESET_LISTING_MAX_AGE: Duration = Duration::from_secs(1);

/// The UI-thread end of [`crate::stream`]'s two error callbacks: one bounded ring per direction,
/// plus the device names a notice has to name (issue #44).
///
/// Two rings rather than one because `rtrb` is single-producer and the two `cpal` error callbacks
/// run on two different, unsynchronised threads — see [`crate::app`]'s `stream_failure_sink` for
/// why the report crosses on a ring at all rather than down the `mpsc` channel every *other*
/// [`AppEvent`] uses.
pub struct StreamFailureWatch {
    input: rtrb::Consumer<StreamFailure>,
    output: rtrb::Consumer<StreamFailure>,
    input_device_name: String,
    output_device_name: String,
}

impl StreamFailureWatch {
    /// Assembles the watch from the consumer end of each direction's ring and the device name that
    /// direction was opened on.
    #[must_use]
    pub fn new(
        input: rtrb::Consumer<StreamFailure>,
        output: rtrb::Consumer<StreamFailure>,
        input_device_name: String,
        output_device_name: String,
    ) -> Self {
        Self {
            input,
            output,
            input_device_name,
            output_device_name,
        }
    }

    /// The next failure from either direction, input first, or `None` when both rings are empty.
    fn pop(&mut self) -> Option<(Direction, StreamFailure)> {
        if let Ok(failure) = self.input.pop() {
            return Some((Direction::Input, failure));
        }
        self.output.pop().ok().map(|f| (Direction::Output, f))
    }

    /// FR-IO-070's notice text: which side failed, on which device, and what the backend said.
    /// Built here, on the UI thread — the callback that detected the failure may not format a
    /// string (FR-ERR-030).
    fn detail(&self, direction: Direction, failure: StreamFailure) -> String {
        let (side, device) = match direction {
            Direction::Input => ("input", &self.input_device_name),
            Direction::Output => ("output", &self.output_device_name),
        };
        // `{failure}`, not `{failure:?}` -- `StreamFailure`'s `Display` was added at M14 precisely
        // so no `Debug` rendering reaches a user-facing string (issue #44).
        format!("{side} device \"{device}\": {failure}")
    }
}

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
    /// FR-STATE-030's preset directory, or `None` where [`namir_platform::config_dir`] resolved
    /// nothing — an environment with no per-user configuration convention, where the session runs
    /// but remembers nothing across launches (P8).
    preset_dir: Option<PathBuf>,
    /// The preset directory as last enumerated by [`crate::worker`], and when the enumeration was
    /// *asked for* — stamped on request, not on arrival, so two frames in the same millisecond do
    /// not both queue one.
    presets: Vec<PresetSummary>,
    presets_listed_at: Option<Instant>,
    /// FR-IO-070's stream-failure reports, when this host is driving a real duplex path. `None`
    /// on `crate::app`'s `open_window_without_audio` path, where there is no stream to fail.
    stream_failures: Option<StreamFailureWatch>,
    /// The running duplex path itself, so FR-IO-070's "stop the stream cleanly" has something to
    /// stop (issue #24). `None` before [`AppHost::hold_streams`] is called, on the
    /// `open_window_without_audio` path, and again after a device loss has stopped it.
    streams: Option<RunningStreams>,
    /// D-13.2's thread-elevation outcome, posted by the output callback and reported from here
    /// (issue #76). Cleared once reported, so the notice is written once per session rather than
    /// once per frame.
    thread_priority: Option<Arc<ThreadPriorityReport>>,
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
            preset_dir: None,
            presets: Vec::new(),
            presets_listed_at: None,
            stream_failures: None,
            streams: None,
            thread_priority: None,
            notices: Vec::new(),
            next_notice_id: AtomicU64::new(1),
        }
    }

    /// Wires FR-IO-070's stream-failure reports in. Called by [`crate::app::run`] once, after
    /// [`crate::stream::open`] has handed back the consumer end of each direction's ring; a host
    /// with no streams behind it (`open_window_without_audio`) simply never calls it.
    pub fn watch_stream_failures(&mut self, watch: StreamFailureWatch) {
        self.stream_failures = Some(watch);
    }

    /// Takes ownership of the running duplex path, so FR-IO-070's "stop the stream cleanly" is
    /// something this host can actually do (issue #24).
    ///
    /// **Why the host and not [`crate::app::run`].** The failure is *detected* on a `cpal` error
    /// thread, which may not stop anything (it is inside the stream it would be closing, and
    /// NFR-RT-010 forbids the blocking work either way), and it is *reported* here, on the UI
    /// thread, one frame later. `run` is meanwhile blocked inside `namir_ui::open_blocking` for the
    /// whole life of the window and cannot react to anything. So the only thread that both learns
    /// of the loss and is allowed to act on it is this one — which is why the streams live here
    /// rather than in a `run` local, as they did until this change.
    ///
    /// Called once, after `RunningStreams::play`, so the elevation watch and the first callback
    /// are already in place; a session with no audio device never calls it.
    pub fn hold_streams(&mut self, streams: RunningStreams) {
        self.streams = Some(streams);
    }

    /// Points this host at FR-STATE-030's preset directory (`<config_dir>/Presets`, see
    /// [`crate::presets`]). Called by [`crate::app::run`] once, with the configuration directory
    /// that launch actually resolved. A host never given one still runs: `SavePreset` reports
    /// [`local_error_codes::PRESET_LOCATION_UNKNOWN`] and the recall list stays empty, which
    /// [`namir_ui::UiSnapshot::presets`] documents as a disabled control rather than an error.
    pub fn watch_presets(&mut self, preset_dir: PathBuf) {
        self.preset_dir = Some(preset_dir);
        self.presets_listed_at = None;
    }

    /// Asks [`crate::worker`] for a fresh preset listing if the last one is stale. Never reads a
    /// directory itself — see [`PRESET_LISTING_MAX_AGE`].
    fn refresh_presets_if_stale(&mut self) {
        let Some(dir) = self.preset_dir.clone() else {
            return;
        };
        if self
            .presets_listed_at
            .is_some_and(|at| at.elapsed() < PRESET_LISTING_MAX_AGE)
        {
            return;
        }
        // Stamped before the request, not after it lands.
        self.presets_listed_at = Some(Instant::now());
        self.worker.send(AppCommand::ListPresets(dir));
    }

    /// Wires D-13.2's thread-elevation outcome in (issue #76). Called by [`crate::app::run`] once,
    /// with [`crate::stream::RunningStreams::thread_priority`]'s report; the outcome does not exist
    /// yet at that point, because it is produced by the output callback's *first* invocation, so
    /// this host polls for it and reports it on whichever frame it appears.
    pub fn watch_thread_priority(&mut self, report: Arc<ThreadPriorityReport>) {
        self.thread_priority = Some(report);
    }

    /// Reports D-13.2's elevation outcome, once, as an FR-ERR-010 record and an FR-UI-070 notice.
    ///
    /// `ThreadPriorityOutcome::diagnostic` supplies the catalogue entry — `None` for `Elevated`,
    /// which has nothing to report — and this side supplies the `{detail}`. Both halves of that
    /// split are deliberate: `namir-platform` returns an `ErrorCode` and formats nothing, because
    /// obtaining one allocates nothing and is therefore safe from the audio callback, while
    /// *emitting* the record is not. This function is the UI-thread end that emitting was deferred
    /// to.
    ///
    /// The watch is dropped as soon as an outcome arrives: the elevation happens once per stream,
    /// so there is nothing further to poll for.
    fn report_thread_priority(&mut self) {
        let outcome = match &self.thread_priority {
            Some(report) => report.take(),
            None => return,
        };
        let Some(outcome) = outcome else {
            return;
        };
        self.thread_priority = None;
        if let Some(code) = outcome.diagnostic() {
            self.push_notice(code, thread_priority_detail(outcome));
        }
    }

    /// Turns whatever the two error callbacks reported since the last frame into notices, through
    /// the same [`AppEvent::StreamFailure`] arm the `mpsc` path used before issue #88 — so the
    /// classification-picks-the-catalogue-entry rule (issue #44) has exactly one implementation.
    ///
    /// The watch is taken out of `self` for the duration so the loop can call `&mut self` methods;
    /// nothing else touches the field, and it is put straight back.
    fn drain_stream_failures(&mut self) {
        let Some(mut watch) = self.stream_failures.take() else {
            return;
        };
        for _ in 0..STREAM_FAILURE_DRAIN_BATCH {
            let Some((direction, failure)) = watch.pop() else {
                break;
            };
            let detail = watch.detail(direction, failure);
            self.handle_event(AppEvent::StreamFailure {
                direction,
                failure,
                detail,
            });
        }
        self.stream_failures = Some(watch);
    }

    /// FR-IO-070's "stop the stream cleanly", on a device loss and on nothing else (issue #24).
    ///
    /// Dropping is the stop: [`crate::audio_io::AudioStream`]'s own contract is that dropping
    /// stops the stream, [`RunningStreams`] is built on it, and unlike `pause()` it cannot fail —
    /// which matters here, because the device this is stopping has just gone away, so a `pause`
    /// against it is as likely to error as to succeed and there would be nothing useful to do with
    /// that error. Taking the field also makes the stop idempotent: a second report from the other
    /// direction's ring, or a second frame, finds `None` and does nothing.
    ///
    /// **Only on `DEVICE_LOST`.** [`crate::error_codes::STREAM_FAILED`] covers everything a
    /// backend reported that was *not* classified as a removal, and some of those are survivable —
    /// `cpal`'s own `ErrorKind` includes `RealtimeDenied` ("audio will still play") and
    /// `DeviceChanged` ("the stream remains active and no rebuild is required"). Stopping on those
    /// would turn a warning into a silent session. A loss is different in kind: the endpoint is
    /// gone, the callbacks are running against nothing, and `DEVICE_LOST`'s own remedy already
    /// tells the user that audio does not resume by itself.
    fn stop_streams(&mut self) {
        drop(self.streams.take());
    }

    /// Queues one FR-UI-070 notice **and writes the matching FR-ERR-010 log record**.
    ///
    /// Wired here rather than at each of the ten call sites (`SCAN_SAVE_FAILED`,
    /// `STATE_SAVE_FAILED`/`STATE_LOAD_FAILED`, `DEVICE_LOST`, `IR_TRUNCATED`, `LOAD_FAILED`,
    /// `LOAD_NOT_DELIVERED`, `REFERENCE_MISSING`, the scan warnings, and everything
    /// [`Self::report`] carries in from [`crate::app`]) for one reason: a notice the user dismissed
    /// and the log line a bug report is reconstructed from must describe the same event, and one
    /// function is the only shape in which they cannot drift apart. A record is a no-op until
    /// [`crate::app::run`] has called `namir_platform::logging::init`, which it does first thing —
    /// so this crate's own tests, which build an `AppHost` directly, write nothing.
    ///
    /// **Not an audio-thread path** (D-16.2/FR-ERR-030). Every caller is on the UI thread: either
    /// [`crate::app::run`] before the window opens, or [`UiHost::snapshot`]'s drain of
    /// [`crate::worker::WorkerHandle`]'s event queue. An engine-detected fault reaches the log by
    /// exactly that route — the audio thread pushes a number through the telemetry ring and this
    /// side maps it — never by the audio callback calling a logger itself.
    fn push_notice(&mut self, code: ErrorCode, detail: impl Into<String>) {
        let id = self.next_notice_id.fetch_add(1, Ordering::Relaxed);
        let detail = detail.into();
        // The log record is written unconditionally, *before* the deduplication and the cap below
        // discard anything: a notice the user never sees must still be reconstructible from
        // namir.log, which is what makes both of those safe to do at all.
        namir_platform::logging::record(code, &detail);
        namir_ui::push_deduplicated(&mut self.notices, UiNotice { id, code, detail });
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
                // Each warning's *own* `library.scan.*` entry, not this crate's generic
                // `app.host.scan_warning` wrapped around a pre-rendered string (issue #39). That
                // generic entry has no remaining caller and is tombstoned above.
                for warning in outcome.warnings {
                    self.push_notice(warning.code, warning.detail);
                }
                if let Some(error) = outcome.save_error {
                    self.push_notice(local_error_codes::SCAN_SAVE_FAILED, error.detail);
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
            AppEvent::PresetsListed(presets) => self.presets = presets,
            AppEvent::StreamFailure {
                direction: _,
                failure,
                detail,
            } => {
                // The *classification* picks the entry (issue #44); the direction is carried in
                // `detail`, which `crate::app`'s callback builds naming both it and the device.
                let code = local_error_codes::stream_failure_code(&failure);
                if code.id == crate::error_codes::DEVICE_LOST.id {
                    self.stop_streams();
                }
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
                if let Some(warning) = warning {
                    // The warning's own entry -- `IR_TRUNCATED` was hard-coded here, which was
                    // right only because it is the one warning a load can currently produce.
                    self.push_notice(warning.code, warning.detail);
                }
            }
            LoadOutcomeSummary::Unloaded => match target {
                Target::Nam => self.loaded_model_name = None,
                Target::Ir => self.loaded_ir_name = None,
            },
            LoadOutcomeSummary::Failed(error) => {
                // The specific `nam.load.*`/`ir.load.*`/`worker.*` entry the failure already
                // carries, with the file name prepended to its detail -- `app.host.load_failed`
                // would say "the file could not be loaded" and drop the reason (issue #39).
                //
                // Prepended only when the detail does not already name the file: `namir-worker`'s
                // own I/O errors are built as `"<path>: <reason>"`, while a parser's are just the
                // reason, and printing the path twice in one line is the same defect at a smaller
                // scale.
                let detail = if error.detail.contains(&source) {
                    error.detail
                } else {
                    format!("{source}: {}", error.detail)
                };
                self.push_notice(error.code, detail);
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
            } else if TELEMETRY_OUT_PEAK_DB.contains(&entry.id) {
                out_peak = Some(out_peak.map_or(entry.value, |v: f32| v.max(entry.value)));
            } else if TELEMETRY_OUT_AVERAGE_DB.contains(&entry.id) {
                out_average = Some(out_average.map_or(entry.value, |v: f32| v.max(entry.value)));
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
        self.drain_stream_failures();
        self.report_thread_priority();
        self.refresh_presets_if_stale();
        self.read_meters();

        // **One guard, both readings (issue #91).** These used to be two separate `lock()` calls,
        // and `crate::worker`'s `LoadState` replaces the whole `State` behind this mutex from the
        // worker thread — so a recall landing between them produced a frame whose parameter values
        // came from the state before the recall and whose `unsaved_changes` flag was computed
        // against the state after it. The visible symptom is a one-frame unsaved marker that is
        // either spurious or missing; the underlying defect is that the two fields of one snapshot
        // were not read from one state at all. Taking the guard once makes that unrepresentable.
        let (params, unsaved_changes) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.params.clone(), *state != self.last_saved)
        };
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
            // Whatever the last off-thread enumeration produced -- a GUI frame never reads a
            // directory (`refresh_presets_if_stale` only ever *asks* for one).
            presets: self.presets.clone(),
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
            UiIntent::SavePreset { name } => {
                // FR-STATE-030's save half. The name is resolved to a path *here*, because
                // `namir-ui` may not name a file (D-5.1 puts `namir-platform` out of its reach)
                // and says so in `UiIntent::SavePreset`'s own doc comment; a name this shell will
                // not write is refused with a notice rather than written somewhere else.
                let Some(dir) = self.preset_dir.clone() else {
                    self.push_notice(
                        local_error_codes::PRESET_LOCATION_UNKNOWN,
                        "namir_platform::config_dir resolved nothing on this system",
                    );
                    return;
                };
                let Some(path) = crate::presets::preset_path(&dir, &name) else {
                    self.push_notice(local_error_codes::PRESET_NAME_REFUSED, name);
                    return;
                };
                self.worker.send(AppCommand::SaveState(path));
                // The list the user is about to look at must contain what they just saved.
                self.presets_listed_at = None;
            }
            UiIntent::RecallPreset { path } => {
                // FR-STATE-030's recall half. The path came from `UiSnapshot::presets`, i.e. from
                // this host's own listing -- `namir-ui` never constructs one.
                self.worker.send(AppCommand::LoadState(path));
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

/// The `{detail}` for `platform.thread_priority.*` (issue #76): what the OS actually answered,
/// since the catalogue entry already carries the sentence and the remedy. `Elevated` has no entry
/// and so never reaches here, but is matched rather than folded into a catch-all so a future
/// caller that does reach here with it gets something truthful.
fn thread_priority_detail(outcome: namir_platform::ThreadPriorityOutcome) -> String {
    match outcome {
        namir_platform::ThreadPriorityOutcome::Elevated => {
            "the audio callback thread was elevated".to_string()
        }
        namir_platform::ThreadPriorityOutcome::PermissionDenied => {
            "the operating system refused the request for want of a privilege this process does \
             not hold"
                .to_string()
        }
        // The raw code, un-prettified: FR-ERR-050's diagnostic bundle wants the number the
        // platform's own documentation uses, which is why `OsError` widened it to `i64`.
        namir_platform::ThreadPriorityOutcome::OsError(code) => {
            format!("the operating system call failed with code {code}")
        }
        namir_platform::ThreadPriorityOutcome::Unsupported => {
            "namir-platform has no thread-priority implementation for this target".to_string()
        }
    }
}

fn default_value_of(descriptor: &namir_params::ParamDescriptor) -> f32 {
    match descriptor.kind {
        namir_params::ParamKind::Continuous { default, .. } => default,
        namir_params::ParamKind::Stepped { default_index, .. } => default_index.0 as f32,
    }
}

/// FR-STATE-010's save/recall by explicit *path*, as opposed to FR-STATE-030's save/recall by
/// *name* — which is what [`UiIntent::SavePreset`]/[`UiIntent::RecallPreset`] now carry and what
/// [`AppHost::dispatch`] resolves through [`crate::presets`].
///
/// These two used to be documented as "not a `UiIntent` today", which stopped being true when
/// `namir-ui` grew the preset controls. They are kept, with that claim corrected, because the two
/// requirements are genuinely different gestures: FR-STATE-010 is "save this state to a file the
/// user chose", which needs a file dialog this window does not have yet, and its path is not
/// required to be inside the preset directory at all.
impl AppHost {
    /// Requests a save to `path`, wherever that is. FR-STATE-030's named-preset save goes through
    /// [`UiIntent::SavePreset`] instead.
    pub fn save_state(&self, path: PathBuf) {
        self.worker.send(AppCommand::SaveState(path));
    }

    /// Requests a load-and-recall from `path`, wherever that is. FR-STATE-030's named-preset
    /// recall goes through [`UiIntent::RecallPreset`] instead.
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

    /// **Issue #90: the per-entry telemetry drain allocates nothing.** The four output-channel
    /// telemetry ids used to be rebuilt by `format!` *inside* the loop over drained entries, so a
    /// frame carrying a full [`TELEMETRY_DRAIN_BATCH`] paid up to four heap allocations and four
    /// key hashes per entry — ~256 per frame, at frame rate — to recompute four compile-time
    /// constants.
    ///
    /// Asserted with D-7.5's `assert_no_alloc` harness rather than by counting allocations
    /// indirectly: `read_meters` is not audio-thread code, but "this loop must allocate nothing"
    /// is exactly what that harness answers, and it is the only mechanism in this crate that can
    /// fail if the `format!` comes back. Real blocks are processed first so the drain has real
    /// entries — a drain of zero entries never enters the loop at all and would pass whatever the
    /// loop body did.
    #[test]
    fn draining_telemetry_entries_allocates_nothing_per_entry() {
        let dir = temp_dir("telemetry_drain_alloc");
        let (mut host, mut engine) = build_host(&dir);

        let mut left = [0.0f32; BLOCK];
        let mut right = [0.0f32; BLOCK];
        for _ in 0..8 {
            left.fill(0.5);
            right.fill(0.5);
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
            engine.process(&mut io);
        }

        let before = host.output_meter.peak_db;
        crate::rt_harness::audio_section(|| host.read_meters());
        // The drain really did carry output-channel entries, or the loop above was never entered
        // and the assertion inside the harness held over nothing.
        assert_ne!(
            host.output_meter.peak_db, before,
            "no output telemetry was drained -- this test asserted nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hoisted constants are the same ids the removed `format!`-per-entry helpers produced.
    /// Cheap, and the one thing a compile-time array cannot get wrong loudly: a typo in a key
    /// would silently stop matching the meter it names.
    #[test]
    fn the_hoisted_output_telemetry_ids_match_their_keys() {
        for ch in 0..MAX_OUTPUT_CHANNELS_SCANNED {
            assert_eq!(
                TELEMETRY_OUT_PEAK_DB[ch],
                namir_params::ParamId::from_key(&format!("telemetry.out.ch{ch}.peak_db")).0
            );
            assert_eq!(
                TELEMETRY_OUT_AVERAGE_DB[ch],
                namir_params::ParamId::from_key(&format!("telemetry.out.ch{ch}.average_db")).0
            );
        }
    }

    /// **Issue #91: one snapshot must describe one state.** `snapshot` took the state lock twice —
    /// once to clone `params`, once to compare the whole state against `last_saved` — and
    /// `crate::worker`'s `LoadState` arm replaces the entire `State` behind that mutex from the
    /// worker thread. A recall landing between the two acquisitions therefore produced a frame
    /// whose parameter values and whose unsaved marker disagreed.
    ///
    /// Driven as a race rather than by injecting a delay, because the defect *is* a race and this
    /// crate has no seam to pause `snapshot` halfway through: a writer thread swaps the shared
    /// `State` between exactly `last_saved` and a modified copy as fast as it can, while this
    /// thread snapshots repeatedly and asserts the two fields agree. With one guard the assertion
    /// cannot fail; with two it fails within a few thousand iterations on this machine.
    #[test]
    fn a_snapshot_reads_its_params_and_its_unsaved_flag_from_one_state() {
        let dir = temp_dir("snapshot_atomicity");
        let (mut host, _engine) = build_host(&dir);
        let key = namir_params::stages::trim::GAIN_DB.key;

        let saved = host.last_saved.clone();
        let mut modified = saved.clone();
        modified.params.set(key, -12.0).unwrap();
        let saved_params = saved.params.clone();

        let state = Arc::clone(&host.state);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_writer = Arc::clone(&stop);
        let writer = std::thread::spawn(move || {
            let mut recalled = false;
            while !stop_writer.load(Ordering::Relaxed) {
                // Whole-`State` replacement, which is exactly what `crate::worker`'s `LoadState`
                // arm does when a preset is recalled.
                *state.lock().unwrap_or_else(|e| e.into_inner()) = if recalled {
                    modified.clone()
                } else {
                    saved.clone()
                };
                recalled = !recalled;
            }
        });

        for _ in 0..20_000 {
            let snapshot = host.snapshot();
            assert_eq!(
                snapshot.unsaved_changes,
                snapshot.params != saved_params,
                "the snapshot's params and its unsaved marker came from different states"
            );
        }

        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **FR-IO-070 end to end on the UI side (issue #88).** A failure pushed onto the direction's
    /// ring — which is what `crate::stream`'s error callback does — becomes a notice on the next
    /// snapshot, carrying the catalogue entry its *classification* chose (issue #44) and a detail
    /// naming the side and the device. All the formatting happens here, on the UI thread; nothing
    /// on the callback side built a string at all.
    #[test]
    fn a_failure_pushed_onto_the_ring_becomes_a_notice_naming_its_side_and_device() {
        let dir = temp_dir("stream_failure_notice");
        let (mut host, _engine) = build_host(&dir);

        let (mut input_tx, input_rx) = rtrb::RingBuffer::new(4);
        let (mut output_tx, output_rx) = rtrb::RingBuffer::new(4);
        host.watch_stream_failures(StreamFailureWatch::new(
            input_rx,
            output_rx,
            "Line (AudioBox 22VSL)".to_string(),
            "Speakers (AudioBox 22VSL)".to_string(),
        ));

        input_tx.push(StreamFailure::DeviceLost).unwrap();
        output_tx
            .push(StreamFailure::Other(crate::audio_io::InlineDetail::from(
                "the requested buffer size is not supported",
            )))
            .unwrap();

        let notices = host.snapshot().notices;
        assert_eq!(notices.len(), 2, "{notices:?}");

        let lost = &notices[0];
        assert_eq!(lost.code.id, crate::error_codes::DEVICE_LOST.id);
        assert!(lost.detail.contains("input"), "{}", lost.detail);
        assert!(
            lost.detail.contains("Line (AudioBox 22VSL)"),
            "{}",
            lost.detail
        );

        // Unclassified, so it must *not* be promoted to a device loss -- the safe direction.
        let other = &notices[1];
        assert_eq!(other.code.id, crate::error_codes::STREAM_FAILED.id);
        assert!(other.detail.contains("output"), "{}", other.detail);
        assert!(
            other.detail.contains("buffer size is not supported"),
            "{}",
            other.detail
        );
        // No `Debug` rendering anywhere in what a user reads (issue #44).
        assert!(!other.detail.contains("Other("), "{}", other.detail);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Opens the whole duplex path over [`crate::stream::FakeBackend`] and hands both ends to
    /// `host`: the real [`crate::app::stream_failure_sink`] closures on the callback side, the
    /// [`StreamFailureWatch`] and the [`RunningStreams`] themselves on this side. Everything below
    /// the backend is the production code path; only the device is virtual.
    fn open_fake_duplex(host: &mut AppHost, backend: &crate::stream::FakeBackend) {
        let xruns = Arc::new(crate::xrun::XrunCounter::new());
        let (input_tx, input_rx) = rtrb::RingBuffer::new(8);
        let (output_tx, output_rx) = rtrb::RingBuffer::new(8);
        let running = crate::stream::open(
            crate::stream::fake_duplex_setup(backend, BLOCK),
            crate::stream::default_test_engine(BLOCK),
            Arc::clone(&xruns),
            crate::app::stream_failure_sink(Arc::clone(&xruns), input_tx),
            crate::app::stream_failure_sink(xruns, output_tx),
        )
        .expect("the fake backend opens unless it was told to fail");
        running.play().unwrap();
        host.watch_stream_failures(StreamFailureWatch::new(
            input_rx,
            output_rx,
            "Line (AudioBox 22VSL)".to_string(),
            "Speakers (AudioBox 22VSL)".to_string(),
        ));
        host.hold_streams(running);
    }

    /// **FR-IO-070 through its own stated apparatus (issue #24, §22 R-5).** The requirement's
    /// method is *"I with a virtual device that can be made to fail on demand"*, and until this
    /// test no such device existed: the tagged artifact asserted that selecting from an empty
    /// slice is `None`, and the only evidence of a real removal was a manual document.
    ///
    /// What runs here is the production path end to end, with nothing but the device faked. A
    /// [`crate::stream::FakeBackend`] stream is opened by `crate::stream::open`, played, and driven
    /// for a few blocks so the failure is genuinely *mid-stream*; then the error callback `cpal`
    /// itself would invoke — captured by the fake since issue #88 — is fired on the output
    /// direction with the 2026-08-27 transcript verbatim.
    ///
    /// It is fired as [`StreamFailure::Other`], not `DeviceLost`, deliberately: that is the shape
    /// the real unplug arrived in, and it is what makes this exercise
    /// `crate::audio_io::classifies_as_device_loss` rather than a pre-classified value a test
    /// handed itself.
    ///
    /// Three of the requirement's four clauses are asserted: no crash or hang (the test completes),
    /// the condition is reported (one `DEVICE_LOST` notice naming the side and the device), and the
    /// stream is stopped cleanly (both directions' streams dropped exactly once, from the UI
    /// thread, and not before the report). The fourth is `select_device`'s re-selection, in the
    /// test below.
    // trace-partial: FR-IO-070
    // uncovered: FR-IO-070 — "allow the user to select another device" is spanned only by the
    // uncovered: restart-mediated substitute below (`device_state::select_device` picking a
    // uncovered: replacement on the next launch); no in-session device chooser exists in either
    // uncovered: shell, so the clause as written is unimplemented (issue #26, roadmap §15 item 16)
    // uncovered: and no test can reach it. The failable device is also virtual, so what a real
    // uncovered: removal makes the OS and cpal do stays evidenced only by
    // uncovered: docs/manual-tests/fr-io-070-device-removal.md, whose steps 1 and 3 are still
    // uncovered: NOT EXECUTED; closes M8
    #[test]
    fn a_device_lost_mid_stream_is_reported_and_stops_both_streams_cleanly() {
        let dir = temp_dir("device_lost_mid_stream");
        let (mut host, _engine) = build_host(&dir);
        let backend = crate::stream::FakeBackend::new();
        open_fake_duplex(&mut host, &backend);

        assert_eq!(backend.input_stream.plays(), 1);
        assert_eq!(backend.output_stream.plays(), 1);

        // Mid-stream, not at open: audio is flowing before anything fails, which is the condition
        // FR-IO-070 names ("device removal **while in use**").
        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();
        let mut out = [0.0f32; BLOCK * 2];
        for _ in 0..4 {
            output_cb(&mut out);
        }
        assert_eq!(
            backend.output_stream.stops(),
            0,
            "nothing has failed yet, so nothing may have been stopped"
        );

        // The transcript from the 2026-08-27 unplug, verbatim, arriving the way it really did --
        // as an `Other` carrying an OS error whose own message formatting had failed.
        let mut output_err = backend.output_error.lock().unwrap().take().unwrap();
        output_err(StreamFailure::Other(crate::audio_io::InlineDetail::from(
            "OS Error -2004287450 (FormatMessageW() returned error 317) (os error -2004287450)",
        )));

        let notices = host.snapshot().notices;
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].code.id, crate::error_codes::DEVICE_LOST.id);
        assert!(
            notices[0].detail.contains("output"),
            "{}",
            notices[0].detail
        );
        assert!(
            notices[0].detail.contains("Speakers (AudioBox 22VSL)"),
            "{}",
            notices[0].detail
        );

        // "stop the stream cleanly": both directions, exactly once each. `DEVICE_LOST`'s own
        // catalogue text has claimed "the stream was stopped" since M14; until issue #24 nothing
        // stopped it, and the notice was telling the user something untrue.
        assert_eq!(
            backend.output_stream.stops(),
            1,
            "the failing direction's stream must be stopped"
        );
        assert_eq!(
            backend.input_stream.stops(),
            1,
            "the other direction goes with it: half a duplex path is not a working session"
        );
        assert_eq!(
            backend.output_stream.pauses(),
            0,
            "the stop is a drop, not a pause: pausing an endpoint that has just gone away is as \
             likely to error as to succeed, and a paused stream is still an open device"
        );

        // Idempotent: a second frame, or a second report from the direction still holding a full
        // ring, must not double-stop or re-report a path that is already gone.
        assert!(host.snapshot().notices.len() <= 1);
        assert_eq!(backend.output_stream.stops(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same apparatus for FR-IO-070's *other* first-sentence half — "a device failing to open"
    /// — which `docs/manual-tests/fr-io-070-device-removal.md` records as step 1, **NOT EXECUTED**,
    /// because inducing it needs a device that can be made to refuse an open.
    ///
    /// The output direction is told to fail, so the input stream has already been built when the
    /// open gives up. What is asserted is the teardown: `crate::stream::open` must stop the half it
    /// did open rather than leaking a live capture stream on a session that has no audio, and the
    /// caller gets a reportable error rather than a panic.
    #[test]
    fn a_device_that_fails_to_open_reports_and_leaves_no_half_open_stream() {
        let dir = temp_dir("device_open_failure");
        let (mut host, _engine) = build_host(&dir);
        let backend = crate::stream::FakeBackend::new().failing_to_open(Direction::Output);

        let xruns = Arc::new(crate::xrun::XrunCounter::new());
        let (input_tx, _input_rx) = rtrb::RingBuffer::new(8);
        let (output_tx, _output_rx) = rtrb::RingBuffer::new(8);
        let opened = crate::stream::open(
            crate::stream::fake_duplex_setup(&backend, BLOCK),
            crate::stream::default_test_engine(BLOCK),
            Arc::clone(&xruns),
            crate::app::stream_failure_sink(Arc::clone(&xruns), input_tx),
            crate::app::stream_failure_sink(xruns, output_tx),
        );
        let error = opened.err().expect("the output open was told to fail");

        assert_eq!(
            backend.stream_log(Direction::Input).stops(),
            1,
            "the input stream built before the failure must be stopped, not leaked"
        );
        assert_eq!(backend.stream_log(Direction::Output).stops(), 0);
        assert!(
            backend.output_data.lock().unwrap().is_none(),
            "a refused open must not have kept the callbacks it was handed"
        );

        // What `crate::app::run` does with that error, and the notice a user actually sees.
        host.report(crate::error_codes::DEVICE_OPEN_FAILED, error.to_string());
        let notices = host.snapshot().notices;
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(
            notices[0].code.id,
            crate::error_codes::DEVICE_OPEN_FAILED.id
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FR-IO-070's third clause, as far as anything in this build can reach it: after the loss,
    /// picking up another device.
    ///
    /// **This is the restart-mediated substitute, not the clause as written**, and the tag above
    /// says so. No device-selection surface exists in either shell (issue #26), so what a user can
    /// actually do after the notice is close Namir and launch it again — at which point
    /// `device_state::select_device` finds the remembered device gone and degrades to another one,
    /// reporting `REMEMBERED_DEVICE_UNAVAILABLE` (FR-IO-080). Asserted here rather than assumed,
    /// because it is the only continuation path the product has and nothing else tests it against
    /// a device that was lost *while in use*.
    #[test]
    fn after_a_loss_the_next_launch_selects_another_device() {
        let lost = "Speakers (AudioBox 22VSL)";
        let remaining = [
            crate::audio_io::DeviceInfo {
                name: "Speakers (Realtek)".to_string(),
                is_default: true,
            },
            crate::audio_io::DeviceInfo {
                name: "Headphones".to_string(),
                is_default: false,
            },
        ];

        let selection = crate::device_state::select_device(&remaining, Some(lost))
            .expect("another device is present, so the session has somewhere to go");
        assert_eq!(selection.device.name, "Speakers (Realtek)");
        assert_eq!(
            selection.fell_back_from.as_deref(),
            Some(lost),
            "the substitution has to be reportable, not silent"
        );
    }

    /// A host with no streams behind it (`crate::app`'s `open_window_without_audio`) never calls
    /// [`AppHost::watch_stream_failures`], and snapshotting must not care.
    #[test]
    fn a_host_with_no_stream_watch_snapshots_normally() {
        let dir = temp_dir("no_stream_watch");
        let (mut host, _engine) = build_host(&dir);
        assert!(host.snapshot().notices.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Issue #76's UI-thread end.** A non-`Elevated` outcome posted by the audio callback
    /// becomes exactly one notice, carrying `ThreadPriorityOutcome::diagnostic`'s own catalogue
    /// entry and a detail naming what the OS answered — and an `Elevated` one becomes none, since
    /// there is nothing to tell anybody about a request that succeeded.
    #[test]
    fn a_refused_thread_elevation_becomes_exactly_one_notice() {
        let dir = temp_dir("thread_priority_notice");
        let (mut host, _engine) = build_host(&dir);
        let report = Arc::new(ThreadPriorityReport::new());
        host.watch_thread_priority(Arc::clone(&report));

        // Nothing posted yet: the audio callback has not run.
        assert!(host.snapshot().notices.is_empty());

        report.post(namir_platform::ThreadPriorityOutcome::OsError(
            -2_147_024_882,
        ));
        let notices = host.snapshot().notices;
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(
            notices[0].code.id,
            namir_platform::error_codes::THREAD_PRIORITY_NOT_ELEVATED.id
        );
        assert!(
            notices[0].detail.contains("-2147024882"),
            "the raw OS code is what FR-ERR-050's bundle wants: {}",
            notices[0].detail
        );

        // Polled every frame, reported once.
        assert_eq!(host.snapshot().notices.len(), 1);
        assert_eq!(host.snapshot().notices.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The successful case says nothing, which is the point of `diagnostic()` returning `None` for
    /// it: a notice per launch reading "your audio thread is fine" is noise.
    #[test]
    fn a_successful_thread_elevation_produces_no_notice() {
        let dir = temp_dir("thread_priority_ok");
        let (mut host, _engine) = build_host(&dir);
        let report = Arc::new(ThreadPriorityReport::new());
        host.watch_thread_priority(Arc::clone(&report));
        report.post(namir_platform::ThreadPriorityOutcome::Elevated);
        assert!(host.snapshot().notices.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The detail is written per outcome rather than shared, and each names the thing a reader
    /// would look for -- the privilege for a denial, the raw code for an OS error, the target for
    /// an unsupported platform.
    #[test]
    fn each_elevation_outcome_gets_a_detail_that_says_what_happened() {
        use namir_platform::ThreadPriorityOutcome as Outcome;
        assert!(thread_priority_detail(Outcome::PermissionDenied).contains("privilege"));
        assert!(thread_priority_detail(Outcome::OsError(-5)).contains("-5"));
        assert!(thread_priority_detail(Outcome::Unsupported).contains("target"));
    }

    /// Polls `host` until `ready` holds, or gives up. The worker thread is a real thread, so every
    /// test that dispatches an intent and then looks at the result has to wait for one; bounded
    /// only by test-timeout hygiene, since the event is guaranteed to arrive eventually.
    fn snapshot_until(
        host: &mut AppHost,
        mut ready: impl FnMut(&UiSnapshot) -> bool,
    ) -> UiSnapshot {
        let mut snapshot = host.snapshot();
        for _ in 0..400 {
            if ready(&snapshot) {
                return snapshot;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            snapshot = host.snapshot();
        }
        snapshot
    }

    /// **FR-STATE-030's save half, end to end.** `UiIntent::SavePreset` carries a *name*; this
    /// host resolves it to `<preset dir>/<name>.namirpreset` (`crate::presets`), the worker writes
    /// it, and the next listing contains it — which is the list `UiSnapshot::presets` hands the
    /// recall control.
    #[test]
    fn saving_a_named_preset_writes_it_and_it_appears_in_the_next_listing() {
        let dir = temp_dir("preset_save");
        let (mut host, _engine) = build_host(&dir);
        let preset_dir = crate::presets::preset_dir_under(&dir);
        host.watch_presets(preset_dir.clone());

        host.dispatch(UiIntent::SavePreset {
            name: "  Crunch Rhythm  ".to_string(),
        });

        let snapshot = snapshot_until(&mut host, |s| !s.presets.is_empty());
        assert_eq!(
            snapshot
                .presets
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Crunch Rhythm"],
            "the name is trimmed, and the listing names presets by file stem"
        );
        assert_eq!(
            snapshot.presets[0].path,
            preset_dir.join("Crunch Rhythm.namirpreset")
        );
        assert!(snapshot.presets[0].path.is_file());
        assert!(
            snapshot.notices.is_empty(),
            "a successful save reports nothing: {:?}",
            snapshot.notices
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The preset directory is created on demand -- a first save into a configuration directory
    /// that has never held one must not fail for want of a `mkdir`.
    #[test]
    fn the_first_save_creates_the_preset_directory() {
        let dir = temp_dir("preset_mkdir");
        let preset_dir = crate::presets::preset_dir_under(&dir);
        assert!(!preset_dir.exists());
        let (mut host, _engine) = build_host(&dir);
        host.watch_presets(preset_dir.clone());
        host.dispatch(UiIntent::SavePreset {
            name: "First".to_string(),
        });
        snapshot_until(&mut host, |s| !s.presets.is_empty());
        assert!(preset_dir.join("First.namirpreset").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `UiIntent::SavePreset`'s own doc comment: a name illegal as a filename is *the host's* to
    /// reject, and to report. Nothing may be written anywhere, least of all outside the preset
    /// directory.
    #[test]
    fn a_preset_name_that_could_escape_the_directory_is_refused_with_a_notice() {
        let dir = temp_dir("preset_hostile_name");
        let (mut host, _engine) = build_host(&dir);
        let preset_dir = crate::presets::preset_dir_under(&dir);
        host.watch_presets(preset_dir.clone());

        host.dispatch(UiIntent::SavePreset {
            name: "../escaped".to_string(),
        });
        let snapshot = host.snapshot();
        assert_eq!(snapshot.notices.len(), 1, "{:?}", snapshot.notices);
        assert_eq!(
            snapshot.notices[0].code.id,
            local_error_codes::PRESET_NAME_REFUSED.id
        );
        assert!(!dir.join("escaped.namirpreset").exists());
        assert!(!preset_dir.exists(), "nothing was written at all");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P8: a session with no per-user configuration directory still runs; only named presets are
    /// unavailable, and asking for one says so rather than failing silently.
    #[test]
    fn a_host_with_no_preset_directory_reports_rather_than_writing() {
        let dir = temp_dir("preset_no_dir");
        let (mut host, _engine) = build_host(&dir);
        // Deliberately never calls `watch_presets`.
        host.dispatch(UiIntent::SavePreset {
            name: "Anything".to_string(),
        });
        let snapshot = host.snapshot();
        assert_eq!(snapshot.notices.len(), 1, "{:?}", snapshot.notices);
        assert_eq!(
            snapshot.notices[0].code.id,
            local_error_codes::PRESET_LOCATION_UNKNOWN.id
        );
        assert!(snapshot.presets.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **FR-STATE-030's recall half.** A preset saved at one parameter value, recalled after the
    /// value changed, puts it back — and clears the unsaved marker with it, since `last_saved` is
    /// updated on `StateLoaded`.
    #[test]
    fn recalling_a_preset_restores_the_parameter_values_it_was_saved_with() {
        let dir = temp_dir("preset_recall");
        let (mut host, _engine) = build_host(&dir);
        host.watch_presets(crate::presets::preset_dir_under(&dir));
        let key = namir_params::stages::trim::GAIN_DB.key;

        host.dispatch(UiIntent::SetParam { key, value: -18.0 });
        host.dispatch(UiIntent::SavePreset {
            name: "Quiet".to_string(),
        });
        let snapshot = snapshot_until(&mut host, |s| !s.presets.is_empty());
        let path = snapshot.presets[0].path.clone();

        host.dispatch(UiIntent::SetParam { key, value: -3.0 });
        assert_eq!(host.snapshot().params.get(key), Some(-3.0));

        host.dispatch(UiIntent::RecallPreset { path });
        let snapshot = snapshot_until(&mut host, |s| s.params.get(key) == Some(-18.0));
        assert_eq!(snapshot.params.get(key), Some(-18.0));
        assert!(
            !snapshot.unsaved_changes,
            "a recall is the new baseline, so nothing is unsaved"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **FR-STATE-060/-070, and the prerequisite the save control could not ship without.** Until
    /// this pass `grep FileRef crates/namir-app/src/` was empty: nothing in this shell ever built
    /// a reference, so `AppCommand::SaveState` serialised a `State` whose `nam`/`ir` were always
    /// `None` and every preset silently forgot which model and IR were loaded. A save button that
    /// quietly loses the user's setup is worse than no save button, so this asserts the reference
    /// actually reaches the file: its content hash (P7's identity), its display name (FR-STATE-070's
    /// "the user shall be shown the missing file's name") and its originating absolute path.
    ///
    /// Driven with a generated IR rather than a `.nam` only because the fixture is one line
    /// (D-19.1: every fixture is generated, never captured); `crate::worker`'s recording step is
    /// the same code for both targets.
    #[test]
    fn a_saved_preset_remembers_which_resource_was_loaded() {
        let dir = temp_dir("preset_references");
        let (mut host, _engine) = build_host(&dir);
        host.watch_presets(crate::presets::preset_dir_under(&dir));

        let ir_path = dir.join("cab.wav");
        let ir_bytes =
            namir_fixtures::ir::to_mono_wav_bytes(&namir_fixtures::ir::delta(64), 48_000);
        std::fs::write(&ir_path, &ir_bytes).unwrap();

        host.dispatch(UiIntent::LoadLibraryEntry(ir_path.clone()));
        let snapshot = snapshot_until(&mut host, |s| s.loaded_ir_name.is_some());
        assert_eq!(
            snapshot.loaded_ir_name.as_deref(),
            Some("cab.wav"),
            "{:?}",
            snapshot.notices
        );

        host.dispatch(UiIntent::SavePreset {
            name: "WithCab".to_string(),
        });
        let snapshot = snapshot_until(&mut host, |s| !s.presets.is_empty());
        let written = std::fs::read(&snapshot.presets[0].path).unwrap();

        let (recalled, _warnings) = namir_state::State::read(&written).unwrap();
        let reference = recalled
            .ir
            .expect("the preset must remember the IR that was loaded (FR-STATE-070)");
        assert_eq!(reference.hash, namir_core::ContentHash::of(&ir_bytes));
        assert_eq!(reference.display_name, "cab.wav");
        assert_eq!(
            reference.absolute.as_deref(),
            Some(ir_path.to_string_lossy().as_ref())
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
