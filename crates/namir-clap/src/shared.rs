//! [`NamirShared`]: the CLAP `[thread-safe]` half of a plugin instance (`clack_plugin::plugin::
//! PluginShared`) — the one place data genuinely needs to be reachable from the audio thread, the
//! main thread, the GUI thread, and this crate's own worker-pool jobs all at once.
//!
//! # Why the data lives behind an `Arc<SharedInner>`, not directly in `NamirShared<'a>`
//!
//! [`namir_worker::pool::ThreadPool::spawn`] requires `impl FnOnce() + Send + 'static` — a
//! background job cannot capture anything tied to the plugin's own `'a` lifetime, only `'static`
//! data. `NamirShared<'a>` itself is only ever reachable through a `&'a` reference (clack hands it
//! out that way to the main thread and the audio processor), so it cannot be the thing a spawned
//! closure captures. [`SharedInner`] carries every field that has no reason to know about `'a` —
//! everything except the [`clack_plugin::host::HostSharedHandle`] a caller needs to notify the
//! host of a latency change (`namir-clap`'s only genuine `'a`-tied need) — behind an `Arc`, so a
//! worker job captures a cheap `Arc::clone` and is `'static` by construction. [`ClapUiHost`]
//! (`crate::ui_host`) holds only an `Arc<SharedInner>` clone for the same reason: the GUI's own
//! window/thread lifetime is unrelated to `'a` too.
//!
//! # What this holds, and why each piece is here rather than somewhere per-thread
//!
//! - [`crate::param_mirror::ParamMirror`] — see that module's doc comment.
//! - `cache: Arc<ResourceCache>` — **`ResourceCache::shared()`**, not `ResourceCache::new()`. This
//!   is FR-CLAP-090's entire mechanism: every `namir-clap` instance in one host process calls
//!   `new_shared` once, and every one of those calls resolves the same process-global `Arc` (a
//!   `OnceLock` behind `ResourceCache::shared()`), so two instances loading the same file converge
//!   on one set of weights rather than each parsing and holding its own copy.
//! - `instance: Mutex<Option<Instance>>` — **not present until the first `activate()`**, and
//!   replaced (not mutated) by every subsequent one, because a `namir_worker::Instance` owns one
//!   engine's `WorkerEndpoint`, and CLAP rebuilds the whole engine on every activation (mid-session
//!   sample-rate/block-size changes go through a deactivate-then-reactivate cycle — FR-CLAP-080).
//!   A `Mutex` here is sound specifically because **the audio thread never touches this field** —
//!   only worker-pool jobs and the GUI/main thread reach into it (NFR-RT-010's "no lock the audio
//!   thread can contend on" is about the audio thread's *own* path, which is
//!   `AudioEngine::process`/`apply_param_direct`, neither of which is behind this lock).
//! - `nam_ref`/`ir_ref: Mutex<Option<FileRef>>` — the "what the user asked to have loaded" half of
//!   a [`namir_state::State`], kept independently of whatever the worker has actually finished
//!   loading (which can lag behind by however long a file read/parse takes) so that a save
//!   started immediately after a load request still records the right reference.
//! - `doc: Mutex<Document>` — the last state document this instance loaded from (or
//!   `Document::empty()` if none yet), so a save can go through `State::write_onto` and preserve
//!   whatever a host-specific or future-version section this build doesn't understand (D-11.2).
//! - `library: Mutex<Option<LibraryService>>` — opened via
//!   [`namir_worker::library::LibraryService::open_default`], `None` only under the same
//!   conditions that itself degrades to `None` for (no per-user config directory on this system).
//!   **An earlier version of this crate opened with an empty root list instead**, on the theory
//!   that "no UI to configure one yet" meant "leave it empty until there is one." That reasoning
//!   was wrong in a way that isn't merely inert: `namir_library::scan`'s own rule is that a
//!   *complete* walk concludes every path it didn't see is removed, and a walk over zero roots
//!   completes trivially — so clicking "Rescan library" inside the plugin didn't just fail to
//!   find new files, it wiped every entry `namir-app` had already indexed, since both products
//!   read and write the identical `library-index.json` under the same config directory.
//!   `open_default` is now the one function both product shells call, specifically so this
//!   default can't drift between them a second time. No `namir-ui::UiIntent` yet lets a user add
//!   a *second* root — that gap is real and still open — but a single, correct default needed no
//!   new UI to fix, only for this crate to stop assuming "unconfigured" was a harmless state to
//!   leave a destructive scan operation pointed at.
//! - `latency_samples`/`latency_announced`/`latency_basis_rate`/`latency_dirty` — see
//!   `crate::audio`'s module doc comment for the full FR-CLAP-040 story, and
//!   [`SharedInner::carried_latency`] for the part of it (issue #93) that has to survive an
//!   activation; these are the audio-thread-writable, main-thread-readable channel between the two
//!   halves of it.
//! - `telemetry`/`telemetry_generation` — the live engine's meter feed, and a counter that lets a
//!   [`ClapUiHost`] holding a *clone* of the reader notice that the clone it holds has been
//!   retired (issue #95). See [`SharedInner::set_telemetry_reader`].
//! - `thread_priority_kind`/`thread_priority_os_error` — D-13.2's elevation outcome, parked here by
//!   the audio thread and reported from the main one. See
//!   [`SharedInner::record_thread_priority_outcome`] for why it cannot be reported where it is
//!   produced.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use clack_plugin::plugin::PluginShared;
use namir_core::ErrorCode;
use namir_engine::TelemetryReader;
use namir_library::ScanProgress;
use namir_platform::ThreadPriorityOutcome;
use namir_state::{Document, FileRef, State};
use namir_ui::{PresetSummary, UiNotice};
use namir_worker::library::{LibraryService, ScanHandle};
use namir_worker::pool::ThreadPool;
use namir_worker::{Instance, ResourceCache};

use crate::param_mirror::ParamMirror;

/// Everything this instance needs that has no reason to be tied to the plugin's `'a` lifetime.
/// See this module's doc comment.
pub(crate) struct SharedInner {
    pub(crate) params: ParamMirror,
    pub(crate) cache: Arc<ResourceCache>,
    pub(crate) pool: ThreadPool,
    pub(crate) instance: Mutex<Option<Instance>>,
    nam_ref: Mutex<Option<FileRef>>,
    ir_ref: Mutex<Option<FileRef>>,
    doc: Mutex<Document>,
    notices: Mutex<Vec<UiNotice>>,
    next_notice_id: AtomicU64,
    unsaved_changes: AtomicBool,
    /// Set in `activate()`, cleared in `deactivate()` — see `crate::latency_ext`'s use of it.
    pub(crate) active: AtomicBool,
    /// The chain's own last-measured `latency_samples()`, published by the audio thread every
    /// block (see `crate::audio`).
    pub(crate) latency_samples: AtomicU32,
    /// The value of `latency_samples` at the moment the host was last *told* to re-read it
    /// (`crate::main_thread`'s `notify_latency_changed`, which is every `activate()` plus
    /// `on_main_thread`'s inactive branch).
    ///
    /// **This is what closes issue #93's restart loop.** A restart is only worth asking a host for
    /// when the figure it currently believes is wrong; comparing against what was actually
    /// announced — rather than against whatever a freshly rebuilt engine happens to report before
    /// its replay has landed — is what makes "the model I already restarted for came back" a
    /// non-event. See `crate::main_thread`'s `on_main_thread`.
    pub(crate) latency_announced: AtomicU32,
    /// The sample rate `latency_samples` was measured at, or 0 if it has never been measured.
    /// [`Self::carried_latency`] is the only reader; see its doc comment.
    pub(crate) latency_basis_rate: AtomicU32,
    /// Set by the audio thread when `latency_samples` changed since it was last reported to the
    /// host; cleared once `on_main_thread` has acted on it. See `crate::audio`'s module doc
    /// comment for the full FR-CLAP-040 sequencing.
    pub(crate) latency_dirty: AtomicBool,
    /// D-13.2's elevation outcome as an atomic pair — the discriminant, and `OsError`'s payload.
    /// Written from the audio thread, read and reported from the main one. See
    /// [`Self::record_thread_priority_outcome`].
    thread_priority_kind: AtomicU8,
    thread_priority_os_error: AtomicI64,
    /// Whether an outcome worth reporting has already been recorded for this instance. See
    /// [`SharedInner::record_thread_priority_outcome`]'s return value.
    thread_priority_seen: AtomicBool,
    library: Mutex<Option<LibraryService>>,
    scan_progress: Mutex<Option<ScanProgress>>,
    scan_handle: Mutex<Option<ScanHandle>>,
    /// A clone of the live engine's `namir_engine::TelemetryReader`, set fresh by every
    /// `activate()` and cleared by `deactivate()` — see `crate::ui_host`'s module doc comment for
    /// why the GUI keeps its own clone rather than reading through here directly (each clone
    /// tracks an independent cursor, so the GUI's drain cadence never affects anyone else's).
    telemetry: Mutex<Option<TelemetryReader>>,
    /// Incremented by every [`Self::set_telemetry_reader`] call. A GUI-side holder of a clone
    /// compares this against the generation its own clone came from and re-clones when it is
    /// behind — issue #95, where a clone taken once at editor-open outlived the ring it read from.
    telemetry_generation: AtomicU64,
    /// FR-STATE-030's preset list, as last enumerated off-thread, and when that was — see
    /// [`SharedInner::presets_snapshot`], which is modelled on [`SharedInner::library_snapshot`]'s
    /// "never block the GUI thread, fill in a moment later" contract.
    presets: Mutex<Vec<PresetSummary>>,
    presets_listed_at: Mutex<Option<Instant>>,
    /// Set when a preset recall has replaced every parameter value behind the host's back, so the
    /// next main-thread callback can tell it to re-read them (`HostParams::rescan`) — see
    /// `crate::main_thread`'s `notify_params_changed`, and its `on_main_thread` for why this is a
    /// flag rather than a direct call.
    pub(crate) params_rescan_pending: AtomicBool,
}

/// How stale [`SharedInner::presets_snapshot`] lets its cached listing get before enumerating the
/// preset directory again.
///
/// A GUI frame must not do a `read_dir`, so the listing is refreshed by a pool job and the GUI
/// renders whatever the last one produced. One second is short enough that a preset saved from the
/// standalone application (or from another instance of this plugin) appears while the user is
/// still looking for it, and long enough that a 60 Hz editor is not listing a directory 60 times a
/// second.
const PRESET_LISTING_MAX_AGE: Duration = Duration::from_secs(1);

/// `thread_priority_kind`'s discriminants. Nothing outside this module reads them: the pair of
/// atomics is a private transport for one `ThreadPriorityOutcome` between two threads, and both
/// ends of it are `SharedInner` methods.
const THREAD_PRIORITY_UNREPORTED: u8 = 0;
const THREAD_PRIORITY_ELEVATED: u8 = 1;
const THREAD_PRIORITY_DENIED: u8 = 2;
const THREAD_PRIORITY_OS_ERROR: u8 = 3;
const THREAD_PRIORITY_UNSUPPORTED: u8 = 4;

impl SharedInner {
    pub(crate) fn new() -> Self {
        // M14 (§22 R-18, issue #22): `open_default` no longer reads `library-index.json` — the
        // parse was the whole of NFR-PERF-040's missing margin, once per instance with no sharing
        // between instances — so there are no warnings to report *here* any more, and this
        // constructor must not ask for any: `ensure_loaded()` on this path would put the parse
        // straight back where it was. The load's warnings are drained in `library_snapshot`
        // instead, which the GUI calls every frame and which is off the instantiation path.
        let library = LibraryService::open_default().map(|(service, _)| service);
        Self::with_library(library)
    }

    /// [`Self::new`] against an explicitly supplied per-user configuration directory, for tests
    /// that need a library whose roots they control — the same injectable-path seam
    /// [`LibraryService::open_at`] exists for, and the only way a test can assert anything about
    /// [`Self::library_roots`] without depending on what this developer's machine happens to hold.
    #[cfg(test)]
    pub(crate) fn new_at(config_dir: &std::path::Path) -> Self {
        let (service, _warnings) = LibraryService::open_at(config_dir);
        Self::with_library(Some(service))
    }

    fn with_library(library: Option<LibraryService>) -> Self {
        Self {
            params: ParamMirror::new(),
            cache: ResourceCache::shared(),
            pool: ThreadPool::new(),
            instance: Mutex::new(None),
            nam_ref: Mutex::new(None),
            ir_ref: Mutex::new(None),
            doc: Mutex::new(Document::empty()),
            notices: Mutex::new(Vec::new()),
            next_notice_id: AtomicU64::new(0),
            unsaved_changes: AtomicBool::new(false),
            active: AtomicBool::new(false),
            latency_samples: AtomicU32::new(0),
            latency_announced: AtomicU32::new(0),
            latency_basis_rate: AtomicU32::new(0),
            latency_dirty: AtomicBool::new(false),
            thread_priority_kind: AtomicU8::new(THREAD_PRIORITY_UNREPORTED),
            thread_priority_os_error: AtomicI64::new(0),
            thread_priority_seen: AtomicBool::new(false),
            library: Mutex::new(library),
            scan_progress: Mutex::new(None),
            scan_handle: Mutex::new(None),
            telemetry: Mutex::new(None),
            telemetry_generation: AtomicU64::new(0),
            presets: Mutex::new(Vec::new()),
            presets_listed_at: Mutex::new(None),
            params_rescan_pending: AtomicBool::new(false),
        }
    }

    pub(crate) fn telemetry_reader(&self) -> Option<TelemetryReader> {
        lock(&self.telemetry).clone()
    }

    /// Installs (or clears) the reader every GUI-side meter drain works from, and bumps
    /// [`Self::telemetry_generation`] so a holder of a stale clone notices.
    ///
    /// The generation is bumped *after* the new reader is in place, so a reader that observes the
    /// new generation is guaranteed to fetch the new reader (or a later one) and never the retired
    /// one.
    pub(crate) fn set_telemetry_reader(&self, reader: Option<TelemetryReader>) {
        *lock(&self.telemetry) = reader;
        self.telemetry_generation.fetch_add(1, Ordering::Release);
    }

    /// How many times [`Self::set_telemetry_reader`] has been called. See `crate::ui_host`'s
    /// `rebind_telemetry_if_stale` — issue #95.
    pub(crate) fn telemetry_generation(&self) -> u64 {
        self.telemetry_generation.load(Ordering::Acquire)
    }

    /// The library roots a resolver built for this instance must search — FR-STATE-070's first
    /// resolution candidate, `library_relative`, resolves against exactly these.
    ///
    /// Read off the held [`LibraryService`] rather than restated, for the same reason
    /// `LibraryService::open_default` is the one function both shells bootstrap through: issue
    /// #96 was `crate::worker_jobs::spawn_recall` building its resolver with a hardcoded empty
    /// list, so every `library_relative` reference in a preset resolved in the standalone
    /// application and missed in the plugin.
    pub(crate) fn library_roots(&self) -> Vec<std::path::PathBuf> {
        lock(&self.library)
            .as_ref()
            .map(|service| service.roots().to_vec())
            .unwrap_or_default()
    }

    pub(crate) fn nam_ref(&self) -> Option<FileRef> {
        lock(&self.nam_ref).clone()
    }

    pub(crate) fn ir_ref(&self) -> Option<FileRef> {
        lock(&self.ir_ref).clone()
    }

    pub(crate) fn set_nam_ref(&self, r: Option<FileRef>) {
        *lock(&self.nam_ref) = r;
    }

    pub(crate) fn set_ir_ref(&self, r: Option<FileRef>) {
        *lock(&self.ir_ref) = r;
    }

    /// The `namir_state::State` this instance currently stands for — the payload every save, and
    /// every replay onto a freshly (re)activated engine, is built from.
    pub(crate) fn snapshot_state(&self) -> State {
        State {
            params: self.params.snapshot(),
            nam: self.nam_ref(),
            ir: self.ir_ref(),
        }
    }

    /// Applies a freshly loaded/restored state's non-resource half to the mirror, and records the
    /// resource references — **does not** itself touch a live engine; the caller (host `state`
    /// load, or `activate()`'s replay) is responsible for that via `Instance::recall`, which is
    /// not RT-safe and belongs on a worker thread.
    pub(crate) fn adopt_state(&self, state: &State) {
        self.params.load(&state.params);
        self.set_nam_ref(state.nam.clone());
        self.set_ir_ref(state.ir.clone());
    }

    pub(crate) fn last_document(&self) -> Document {
        lock(&self.doc).clone()
    }

    pub(crate) fn set_last_document(&self, doc: Document) {
        *lock(&self.doc) = doc;
    }

    pub(crate) fn mark_dirty(&self) {
        self.unsaved_changes.store(true, Ordering::Relaxed);
    }

    pub(crate) fn mark_clean(&self) {
        self.unsaved_changes.store(false, Ordering::Relaxed);
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.unsaved_changes.load(Ordering::Relaxed)
    }

    /// Queues one FR-UI-070 notice **and writes the matching FR-ERR-010 log record**.
    ///
    /// Both from one function on purpose: a notice the user dismissed and a log record a bug report
    /// is built from must describe the same event, and the only way they cannot drift apart is for
    /// there to be no second call site to forget. Every caller of this — `crate::audio`'s
    /// `activate`, `crate::gui`'s `set_parent`, `crate::state_ext`'s host `state` load,
    /// `crate::worker_jobs`' three jobs, and [`Self::start_library_scan`] — is on the main, GUI or
    /// pool thread; none is the audio thread, which is what keeps this side of D-16.2 true.
    ///
    /// The record is written *before* the notices lock is taken, so this never holds two locks at
    /// once (the log has a process-global mutex of its own).
    pub(crate) fn push_notice(&self, code: ErrorCode, detail: impl Into<String>) {
        let id = self.next_notice_id.fetch_add(1, Ordering::Relaxed);
        let detail = detail.into();
        // Written before the deduplication and cap in `push_deduplicated` discard anything -- see
        // `namir_ui::push_deduplicated`'s own doc comment, and `crate::worker_jobs::spawn_recall`,
        // whose two deliberate triggers are what made deduplication necessary (issue #43).
        namir_platform::logging::record(code, &detail);
        namir_ui::push_deduplicated(&mut lock(&self.notices), UiNotice { id, code, detail });
    }

    pub(crate) fn dismiss_notice(&self, id: u64) {
        lock(&self.notices).retain(|n| n.id != id);
    }

    pub(crate) fn notices(&self) -> Vec<UiNotice> {
        lock(&self.notices).clone()
    }

    /// The library as the GUI sees it this frame — and the point at which the deferred load's
    /// warnings are reported (M14, see [`SharedInner::new`]).
    ///
    /// `snapshot()` never blocks: until the loader thread has finished parsing the index this
    /// returns an empty one, so an instance whose window opens in the first fraction of a second
    /// shows a library that fills in rather than a frame that stalls for the parse. Draining the
    /// warnings here rather than at construction is what keeps a corrupt index reported at all,
    /// and each is reported once — `take_load_warnings` empties the list it reads.
    pub(crate) fn library_snapshot(&self) -> namir_ui::LibrarySnapshot {
        let guard = lock(&self.library);
        let (index, warnings) = match guard.as_ref() {
            Some(service) => (service.snapshot(), service.take_load_warnings()),
            None => (Arc::new(namir_library::Index::empty()), Vec::new()),
        };
        drop(guard);
        for w in &warnings {
            log_worker_warning(w);
        }
        let scan = *lock(&self.scan_progress);
        namir_ui::LibrarySnapshot { index, scan }
    }

    /// FR-STATE-030's preset list as the GUI sees it this frame, and the point at which a stale
    /// listing is refreshed — off-thread, exactly as `library_snapshot` defers its own parse.
    ///
    /// Never blocks and never touches the filesystem on the calling thread: until the first
    /// enumeration lands this returns an empty list, which `namir_ui::UiSnapshot::presets`
    /// documents as "the host knows of none (or has not looked yet)" and which the UI renders as a
    /// disabled recall control rather than as an error.
    pub(crate) fn presets_snapshot(self: &Arc<Self>) -> Vec<PresetSummary> {
        self.refresh_presets_if_stale();
        lock(&self.presets).clone()
    }

    /// Forces the next [`Self::presets_snapshot`] to re-enumerate — called after this instance
    /// writes a preset, so the list it just added to is not up to a second out of date.
    pub(crate) fn mark_presets_stale(&self) {
        *lock(&self.presets_listed_at) = None;
    }

    fn refresh_presets_if_stale(self: &Arc<Self>) {
        {
            let mut listed_at = lock(&self.presets_listed_at);
            if listed_at.is_some_and(|at| at.elapsed() < PRESET_LISTING_MAX_AGE) {
                return;
            }
            // Stamped before the job runs, not after: two frames in the same millisecond must not
            // both queue an enumeration.
            *listed_at = Some(Instant::now());
        }
        let this = Arc::clone(self);
        self.pool.spawn(move || {
            let listed = crate::presets::preset_dir()
                .map(|dir| crate::presets::list_presets(&dir))
                .unwrap_or_default();
            *lock(&this.presets) = listed;
        });
    }

    pub(crate) fn start_library_scan(self: &Arc<Self>) {
        let library_guard = lock(&self.library);
        let Some(service) = library_guard.as_ref() else {
            drop(library_guard);
            self.push_notice(
                crate::error_codes::LIBRARY_UNAVAILABLE,
                "no per-user configuration directory is available on this system",
            );
            return;
        };
        let this_progress = Arc::clone(self);
        let this_complete = Arc::clone(self);
        let handle = service.start_scan(
            &self.pool,
            move |progress| {
                *lock(&this_progress.scan_progress) = Some(progress);
            },
            move |outcome| {
                *lock(&this_complete.scan_progress) = None;
                for w in &outcome.warnings {
                    log_worker_warning(w);
                }
                if let Some(e) = &outcome.save_error {
                    log_worker_warning(e);
                }
            },
        );
        drop(library_guard);
        *lock(&self.scan_handle) = handle;
    }

    pub(crate) fn cancel_library_scan(&self) {
        if let Some(handle) = lock(&self.scan_handle).take() {
            handle.cancel();
        }
    }

    /// The latency figure a fresh `activate()` should keep reporting rather than replacing with
    /// the zero its freshly built engine reports — **issue #93's other half**.
    ///
    /// Every activation builds a default engine (latency 0) and dispatches
    /// `crate::worker_jobs::spawn_recall` to reload whatever this instance stands for. Publishing
    /// the transient zero and then the replayed model's real figure is what made the plugin
    /// observe a latency *change* on every single activation, and ask for a restart for it — a
    /// cycle with no exit for as long as a rate-mismatched model stayed loaded.
    ///
    /// So when a replay is pending, the figure the host already has is carried across the
    /// activation instead: it is the value that same replay converged on last time, and the
    /// activation is the plugin's own restart, not a configuration change.
    ///
    /// **Two conditions, both necessary.** There must be something to replay (`nam_ref`/`ir_ref`),
    /// or the engine's zero is simply the truth; and the activation's sample rate must match the
    /// rate the carried figure was measured at, because D-9.2's resampler — the chain's only
    /// source of latency in 1.0 — exists precisely when the model's rate differs from the
    /// session's, so a rate change can legitimately move the converged figure to something else.
    /// When either fails the caller adopts the engine's own reading and the ordinary
    /// change-detection path does the rest, at the cost of the one restart it was always going to
    /// cost.
    pub(crate) fn carried_latency(&self, sample_rate_hz: u32) -> Option<u32> {
        let replay_pending = self.nam_ref().is_some() || self.ir_ref().is_some();
        let basis = self.latency_basis_rate.load(Ordering::Relaxed);
        (replay_pending && basis != 0 && basis == sample_rate_hz)
            .then(|| self.latency_samples.load(Ordering::Relaxed))
    }

    /// Records the figure `clap_plugin_latency.get` reports, and the sample rate it was measured
    /// at (which [`Self::carried_latency`] later checks against).
    pub(crate) fn publish_latency(&self, latency: u32, sample_rate_hz: u32) {
        self.latency_samples.store(latency, Ordering::Relaxed);
        self.latency_basis_rate
            .store(sample_rate_hz, Ordering::Relaxed);
    }

    /// D-13.2's outcome, parked for the main thread to report — **the audio thread cannot report
    /// it itself** (issue #76's follow-up).
    ///
    /// `namir_platform::elevate_current_thread_priority` can only raise the priority of the thread
    /// that calls it, so its one caller here is `crate::audio`'s `process()`. FR-ERR-030 forbids
    /// logging, allocation and logging-formatting on that thread, and `xtask rt-logging` fails the
    /// build if `crates/namir-clap/src/audio.rs` so much as names the logger — so the outcome
    /// crosses to the main thread as two plain atomic stores and becomes a notice (and an
    /// FR-ERR-010 record) in [`Self::report_thread_priority_outcome`], exactly the way `activate`'s
    /// unusable-sample-rate condition already goes through `push_notice` here rather than there.
    ///
    /// Wait-free and allocation-free: two relaxed-and-release stores of a discriminant and an
    /// `i64`. The payload is written before the discriminant is released, so a main thread that
    /// acquires a non-zero discriminant sees the payload that belongs to it.
    ///
    /// **Returns whether the main thread is worth waking for this** — `false` for an elevation
    /// that succeeded (nothing to report) and for the second and later activations of one
    /// instance, since the answer cannot change within a process and one notice about it is the
    /// right number. That makes the extra `request_callback` a once-per-instance event rather than
    /// a once-per-activation one, which is also what keeps `tests/clap_host_latency.rs`'s waits on
    /// "the plugin asked for a callback" unambiguous.
    #[must_use]
    pub(crate) fn record_thread_priority_outcome(&self, outcome: ThreadPriorityOutcome) -> bool {
        if outcome.diagnostic().is_none() || self.thread_priority_seen.swap(true, Ordering::AcqRel)
        {
            return false;
        }
        let kind = match outcome {
            ThreadPriorityOutcome::Elevated => THREAD_PRIORITY_ELEVATED,
            ThreadPriorityOutcome::PermissionDenied => THREAD_PRIORITY_DENIED,
            ThreadPriorityOutcome::OsError(code) => {
                self.thread_priority_os_error.store(code, Ordering::Relaxed);
                THREAD_PRIORITY_OS_ERROR
            }
            ThreadPriorityOutcome::Unsupported => THREAD_PRIORITY_UNSUPPORTED,
        };
        self.thread_priority_kind.store(kind, Ordering::Release);
        true
    }

    /// Turns whatever [`Self::record_thread_priority_outcome`] parked into an FR-UI-070 notice and
    /// an FR-ERR-010 record, once. Called from `crate::main_thread`'s `on_main_thread`.
    ///
    /// Reported once per outcome, not once per block: the discriminant is taken (swapped back to
    /// "nothing to report") by whoever reads it, and `process()` only writes it once per audio
    /// processor anyway. A successful elevation reports nothing at all —
    /// `ThreadPriorityOutcome::diagnostic()` is `None` for it, which is the whole reason that
    /// method exists rather than a bare `bool`.
    pub(crate) fn report_thread_priority_outcome(&self) {
        let kind = self
            .thread_priority_kind
            .swap(THREAD_PRIORITY_UNREPORTED, Ordering::Acquire);
        let outcome = match kind {
            THREAD_PRIORITY_ELEVATED => ThreadPriorityOutcome::Elevated,
            THREAD_PRIORITY_DENIED => ThreadPriorityOutcome::PermissionDenied,
            THREAD_PRIORITY_OS_ERROR => ThreadPriorityOutcome::OsError(
                self.thread_priority_os_error.load(Ordering::Relaxed),
            ),
            THREAD_PRIORITY_UNSUPPORTED => ThreadPriorityOutcome::Unsupported,
            _ => return,
        };
        let Some(code) = outcome.diagnostic() else {
            return;
        };
        // Formatted here, on the main thread, from a value the audio thread only ever stored --
        // FR-ERR-030's "no formatting for logging on the audio thread" is why the payload crosses
        // as an `i64` rather than as a message.
        match outcome {
            ThreadPriorityOutcome::OsError(os) => {
                self.push_notice(code, format!("the OS reported error {os}"))
            }
            _ => self.push_notice(code, "the audio thread runs at its default priority"),
        }
    }

    fn lock_instance(&self) -> MutexGuard<'_, Option<Instance>> {
        lock(&self.instance)
    }

    pub(crate) fn with_instance<R>(&self, f: impl FnOnce(&mut Instance) -> R) -> Option<R> {
        self.lock_instance().as_mut().map(f)
    }

    /// Installs a freshly built `Instance`, replacing whatever was there — every `activate()`
    /// calls this exactly once (see `crate::audio`'s module doc comment for why the whole engine
    /// is rebuilt on every activation rather than mutated in place).
    pub(crate) fn install_instance(&self, instance: Instance) {
        *self.lock_instance() = Some(instance);
    }

    /// Drops the current `Instance` — called by `deactivate()`. The `WorkerEndpoint` (and thus
    /// the SPSC rings) it owned goes with it; any worker-pool job still holding an `Arc` to this
    /// `SharedInner` and racing a submit against it degrades exactly as D-8.1 describes for an
    /// abandoned ring (`SubmitError::Abandoned`), not a panic.
    pub(crate) fn clear_instance(&self) {
        *self.lock_instance() = None;
    }

    /// Ends this instance's off-thread work and returns only once every worker thread it started
    /// has been joined. Called from [`NamirShared`]'s [`Drop`] — that is, from
    /// `clap_plugin.destroy`, on the main thread — and from nowhere else. See that impl's doc
    /// comment for why it is not optional.
    ///
    /// A library scan is the one job that can run for seconds rather than microseconds, so it is
    /// cancelled first: [`namir_worker::library::ScanHandle::cancel`] is cooperative and
    /// `namir-library`'s step machine notices between steps, which bounds the join below by one
    /// `Scanner::step` instead of by a whole library walk.
    pub(crate) fn shutdown_workers(&self) {
        self.cancel_library_scan();
        self.pool.shutdown();
    }
}

/// A worker/library warning this crate has nowhere richer to send — it reaches no FR-UI-070 notice
/// list, because every caller is a callback that receives it with no `Arc<SharedInner>` in scope.
///
/// **No longer a no-op (M9b).** It was one until FR-ERR-010's writer existed; the comment it
/// carried promised that wiring a real sink would be "a one-function change, not a
/// grep-and-patch", and this is that one function. Every record is catalogue-backed (D-16.5), and
/// the warning already carries its own `ErrorCode` — passed through unchanged, per
/// `namir_worker::WorkerError`'s own pass-through rule, so a `library.*` id stays a `library.*` id
/// in the log.
///
/// Never reachable from the audio thread: the two call sites are `SharedInner::new` (main thread,
/// via `clap_plugin.create`) and `start_library_scan`'s progress/completion callbacks, which run
/// on `namir-worker`'s pool.
fn log_worker_warning(w: &namir_worker::WorkerError) {
    namir_platform::logging::record(w.code, &w.detail);
}

/// This instance's CLAP `[thread-safe]` half (`clack_plugin::plugin::PluginShared`). See this
/// module's doc comment for why the real data lives in `Arc<SharedInner>` rather than here.
///
/// Must be `pub`, not `pub(crate)`: `clack_plugin::plugin::Plugin::Shared` is an associated type
/// of a trait implemented on the crate's `pub struct NamirClapPlugin` (`lib.rs`), and Rust
/// requires an associated type reachable from a public trait impl to be at least as visible as
/// that impl — nothing outside this crate can actually construct or name a useful value of this
/// type, since every field and every constructor stays `pub(crate)`.
///
/// No `HostSharedHandle` field: every site that needs one already has its own (`crate::audio`'s
/// `NamirAudioProcessor::host`, `crate::main_thread`'s `NamirMainThread::host`), obtained
/// straight from `activate`/`new_main_thread`'s own parameters — so `'a` is carried only via
/// `PhantomData` here, not by storing a redundant third handle nothing reads.
pub struct NamirShared<'a> {
    pub(crate) inner: Arc<SharedInner>,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> PluginShared<'a> for NamirShared<'a> {}

impl<'a> NamirShared<'a> {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(SharedInner::new()),
            _lifetime: std::marker::PhantomData,
        }
    }
}

/// **`clap_plugin.destroy` must not return while this instance's worker threads are still
/// running**, and this is the only thing that guarantees it.
///
/// # The bug this exists to prevent (found M9a, fixed M9b)
///
/// `SharedInner` owns its [`ThreadPool`], and every worker job holds an `Arc<SharedInner>` to
/// reach the rest of it (`crate::worker_jobs::spawn_recall`, `spawn_load_library_entry`, and
/// [`SharedInner::start_library_scan`]'s two callbacks). That is an ownership cycle with exactly
/// one exit: the pool is joined by `Drop for ThreadPool`, which cannot run until the last
/// `Arc<SharedInner>` dies — and the last one belongs to whichever *job* finishes last, not to the
/// host thread calling destroy.
///
/// So without this impl, `destroy` merely decremented a refcount and returned, with the plugin's
/// worker threads still executing code inside the plugin's own shared library. A host is entitled
/// to unload that library the instant destroy returns (`clap-validator` does exactly this — it
/// calls `clap_entry->deinit()` and drops its `libloading::Library` at the end of every test), and
/// unmapping code out from under a running thread is `STATUS_ACCESS_VIOLATION`. It presented as
/// `ERROR Test state-reproducibility-basic crashed: exit code: 0xc0000005` on a contended CI
/// runner and as nothing at all on an idle developer machine, because whether the job drained
/// before destroy was purely a scheduling race — which is why it survived M6's manual 32-of-32
/// validator run and M9a's local one.
///
/// The trigger was `crate::state_ext`'s `load`, whose last act is `spawn_recall`; the same cycle
/// was live at `crate::audio`'s `activate` and at every GUI-driven job too.
///
/// # Why the shutdown is explicit rather than "make the job hold a `Weak`"
///
/// A `Weak` does not close it. A job that upgrades still holds a strong reference while it runs,
/// so it can still be the last holder — and then `SharedInner`, and therefore `ThreadPool`, drops
/// *on a pool thread*, where `ThreadPool::drop` joins its own `JoinHandle`. On Windows that is a
/// permanent block: the crash would become a hang. The pool has to be shut down from a non-pool
/// thread, before destroy returns, which is what this does. (`ThreadPool::shutdown` also carries a
/// self-join guard now, so that shape degrades rather than wedges — but it is a backstop, not the
/// fix.)
///
/// # Ordering
///
/// `clack_plugin`'s `PluginWrapper` declares `audio_processor`, `main_thread`, `shared` in that
/// order, so by the time this runs the audio processor is gone (destroy deactivates first if the
/// host did not) and `NamirMainThread` — which owns the embedded editor window, and with it the
/// `crate::ui_host::ClapUiHost` that can dispatch new jobs — has already dropped. Nothing is left
/// that could spawn; and `ThreadPool::spawn` drops rather than queues after a shutdown anyway.
impl Drop for NamirShared<'_> {
    fn drop(&mut self) {
        self.inner.shutdown_workers();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // P8, matching `namir-worker::cache::lock`'s identical recovery: a panic elsewhere in this
    // instance must not permanently disable its own state.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_params::stages::trim;

    /// **FR-CLAP-090's mechanism, proven at the point this crate actually calls it**: two
    /// independently constructed `SharedInner`s (standing in for two plugin instances in one host
    /// process) resolve to the *same* `Arc<ResourceCache>` because both go through
    /// `ResourceCache::shared()`, not `ResourceCache::new()`.
    // trace-partial: FR-CLAP-090
    // uncovered: FR-CLAP-090 — the B half of "I plus B", that N instances of one model use
    // uncovered: materially less memory than N separate copies, is measured by nothing: this
    // uncovered: crate's one bench (benches/plugin_instantiation.rs) times NFR-PERF-040's
    // uncovered: instantiation window, and no benchmark anywhere in the workspace measures memory
    // uncovered: at all; closes M8
    #[test]
    fn two_shared_inners_resolve_to_the_same_process_global_cache() {
        let a = SharedInner::new();
        let b = SharedInner::new();
        assert!(
            Arc::ptr_eq(&a.cache, &b.cache),
            "two instances must share one process-global ResourceCache (FR-CLAP-090)"
        );
    }

    #[test]
    fn snapshot_state_round_trips_through_adopt_state() {
        let inner = SharedInner::new();
        inner.params.set_by_key(trim::GAIN_DB.key, 5.0);
        inner.set_nam_ref(Some(FileRef {
            hash: namir_core::ContentHash::of(b"x"),
            library_relative: None,
            absolute: None,
            display_name: "x.nam".to_string(),
            embedded: None,
        }));
        let state = inner.snapshot_state();

        let other = SharedInner::new();
        other.adopt_state(&state);
        assert_eq!(other.params.snapshot().get(trim::GAIN_DB.key), Some(5.0));
        assert_eq!(
            other.nam_ref().map(|r| r.display_name),
            Some("x.nam".to_string())
        );
    }

    #[test]
    fn dirty_flag_starts_clean_and_tracks_mark_calls() {
        let inner = SharedInner::new();
        assert!(!inner.is_dirty());
        inner.mark_dirty();
        assert!(inner.is_dirty());
        inner.mark_clean();
        assert!(!inner.is_dirty());
    }

    /// **The M9a `clap-validator` crash, as a test.** `clap_plugin.destroy` — which is
    /// `NamirShared`'s drop — must not return while a worker job this instance spawned is still in
    /// flight, because the host may unload the plugin's shared library the moment it does. See
    /// `impl Drop for NamirShared`'s doc comment for the full mechanism.
    ///
    /// The job below stands in for `crate::worker_jobs::spawn_recall`'s: what made teardown racy
    /// was not what a recall job *does* (with no resources loaded it returns immediately) but that
    /// it captures an `Arc<SharedInner>`, so the pool's only join was owned by the job rather than
    /// by the thread calling destroy. Against the pre-fix code this test fails: the drop returned
    /// at once and `finished` was still false.
    #[test]
    fn destroy_does_not_return_while_a_worker_job_is_still_in_flight() {
        let shared = NamirShared::new();
        let inner = Arc::clone(&shared.inner);
        let finished = Arc::new(AtomicBool::new(false));

        let job_inner = Arc::clone(&inner);
        let job_finished = Arc::clone(&finished);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        inner.pool.spawn(move || {
            let _holds_shared_inner_alive = job_inner;
            started_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
            job_finished.store(true, Ordering::Release);
        });

        started_rx.recv().expect("the job must start");
        // 200 ms of margin against a thread that has only just announced itself — the point is
        // that the job is genuinely still running when destroy happens.
        assert!(!finished.load(Ordering::Acquire));

        drop(shared); // `clap_plugin.destroy`

        assert!(
            finished.load(Ordering::Acquire),
            "destroy returned with a worker job still running: a host that unloads the library \
             here faults the thread that is executing it (0xc0000005)"
        );
        assert_eq!(
            inner.pool.threads(),
            0,
            "destroy must have joined every worker thread"
        );
    }

    #[test]
    fn notices_can_be_pushed_and_dismissed() {
        let inner = SharedInner::new();
        inner.push_notice(crate::error_codes::LIBRARY_UNAVAILABLE, "detail");
        assert_eq!(inner.notices().len(), 1);
        let id = inner.notices()[0].id;
        inner.dismiss_notice(id);
        assert!(inner.notices().is_empty());
    }

    fn temp_config_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-clap-shared-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// **Issue #96.** The roots a resolver is built from come off the `LibraryService` this
    /// instance holds. Built against an injected configuration directory, so what is asserted is
    /// the wiring rather than whatever this developer's machine happens to have configured.
    #[test]
    fn the_library_roots_are_the_services_own_and_not_an_empty_list() {
        let config = temp_config_dir("roots");
        let inner = SharedInner::new_at(&config);

        let roots = inner.library_roots();
        assert_eq!(
            roots,
            vec![config.join("Library")],
            "an empty root list here is issue #96: LibraryResolver::resolve_library_relative \
             cannot succeed against one, so FR-STATE-070's first resolution candidate is dead in \
             the plugin"
        );

        // And it is a set a real resolver can actually resolve against.
        std::fs::create_dir_all(config.join("Library").join("marshall")).unwrap();
        std::fs::write(config.join("Library/marshall/jcm800.nam"), b"{}").unwrap();
        let index = namir_library::Index::empty();
        let resolver = namir_library::LibraryResolver::new(&index, &roots);
        let rel = namir_state::RelPath::parse("marshall/jcm800.nam").unwrap();
        assert_eq!(
            namir_state::FileResolver::resolve_library_relative(&resolver, &rel),
            Some(config.join("Library/marshall/jcm800.nam"))
        );

        let _ = std::fs::remove_dir_all(&config);
    }

    /// Issue #76's follow-up: D-13.2's elevation outcome is produced on the audio thread and
    /// becomes a notice on the main one. A refusal is reported...
    #[test]
    fn a_refused_priority_elevation_becomes_a_notice_on_the_main_thread() {
        let inner = SharedInner::new();
        assert!(
            inner.record_thread_priority_outcome(ThreadPriorityOutcome::PermissionDenied),
            "a refusal is worth waking the main thread for"
        );
        assert!(
            inner.notices().is_empty(),
            "the audio thread must not push the notice itself -- FR-ERR-030"
        );

        inner.report_thread_priority_outcome();
        let notices = inner.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].code.id,
            ThreadPriorityOutcome::PermissionDenied
                .diagnostic()
                .expect("a refusal has a diagnostic")
                .id
        );

        // ...once. A second callback with nothing new parked reports nothing.
        inner.report_thread_priority_outcome();
        assert_eq!(inner.notices().len(), 1);
    }

    /// ...and a success is not. `ThreadPriorityOutcome::diagnostic()` returning `None` for
    /// `Elevated` is the whole reason that method exists rather than a bare `bool`.
    #[test]
    fn a_successful_priority_elevation_reports_nothing() {
        let inner = SharedInner::new();
        assert!(
            !inner.record_thread_priority_outcome(ThreadPriorityOutcome::Elevated),
            "a successful elevation is not worth a main-thread callback"
        );
        inner.report_thread_priority_outcome();
        assert!(inner.notices().is_empty());
    }

    /// An `OsError`'s payload survives the crossing, so FR-ERR-010's record names the number the
    /// platform's own documentation uses — the formatting happens on the main thread, which is
    /// exactly why the payload crosses as an `i64` and not as a message.
    #[test]
    fn an_os_error_code_survives_the_crossing_to_the_main_thread() {
        let inner = SharedInner::new();
        assert!(inner.record_thread_priority_outcome(ThreadPriorityOutcome::OsError(-2147024882)));
        inner.report_thread_priority_outcome();
        let notices = inner.notices();
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].detail.contains("-2147024882"),
            "the OS's own error code must reach the record: {:?}",
            notices[0].detail
        );
    }

    /// **Issue #93's carrying rule**, at the field that implements it. A figure measured at one
    /// sample rate is only carried across an activation at the *same* rate, and only while there
    /// is something for that activation's replay to restore.
    #[test]
    fn a_latency_figure_is_carried_only_for_a_pending_replay_at_the_same_rate() {
        let inner = SharedInner::new();
        inner.publish_latency(512, 48_000);

        assert_eq!(
            inner.carried_latency(48_000),
            None,
            "with nothing loaded there is no replay coming, so the fresh engine's own reading is \
             the truth"
        );

        inner.set_nam_ref(Some(FileRef {
            hash: namir_core::ContentHash::of(b"m"),
            library_relative: None,
            absolute: None,
            display_name: "m.nam".to_string(),
            embedded: None,
        }));
        assert_eq!(
            inner.carried_latency(48_000),
            Some(512),
            "the replay will put this model, and its latency, back -- republishing zero in the \
             meantime is what made the restart unbounded (issue #93)"
        );
        assert_eq!(
            inner.carried_latency(44_100),
            None,
            "at another rate D-9.2's resampler may not be needed at all, so the old figure is not \
             evidence about the new configuration"
        );
    }
}
