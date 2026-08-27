//! The dedicated background thread every blocking operation — a model/IR load, a preset save or
//! recall, a library scan start/cancel — runs on, so neither the GUI thread (FR-UI-060: "shall
//! not block the user interface") nor the audio thread is ever the one waiting on disk I/O or a
//! handover's R-7 serialisation window.
//!
//! [`WorkerHandle`] is [`crate::host::AppUiHost`]'s only way to reach this thread: an
//! [`AppCommand`] in, a stream of [`AppEvent`]s out, both over plain `mpsc` channels — matching
//! D-15.3's "the UI never blocks on the worker" by construction, since sending to an unbounded
//! `mpsc::Sender` never blocks and polling `try_recv` never blocks either.
//!
//! Ordinary parameter changes (`UiIntent::SetParam`/`ResetParamToDefault`) do **not** go through
//! this thread — they reach [`crate::instance::SharedInstance`] directly from the GUI thread via
//! `namir_worker::Instance::try_submit_param`, per that method's own "this is what the UI thread
//! uses" (non-blocking). Routing them through this thread instead would add a full channel round
//! trip to the single highest-frequency interaction in the whole application for no benefit.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use namir_library::LibraryResolver;
use namir_state::State;
use namir_worker::library::{LibraryService, ScanHandle, ScanOutcome};
use namir_worker::pool::ThreadPool;
use namir_worker::recall::{RecallOutcome, ResourceRecall};
use namir_worker::{JobResult, LoadSource, ResourceCache, Target};

use crate::instance::SharedInstance;

/// One request from the UI thread.
pub enum AppCommand {
    /// FR-UI-050-adjacent: load a library entry, inferring Nam vs. Ir from its extension.
    LoadLibraryEntry(PathBuf),
    /// FR-LIB-020: (re)start a library scan.
    RescanLibrary,
    /// Cancel a running scan.
    CancelScan,
    /// FR-STATE-010: serialise the current parameter/global/reference state to `path`.
    SaveState(PathBuf),
    /// FR-STATE-030: load and recall a saved state from `path`.
    LoadState(PathBuf),
    /// Stops the worker thread. Sent automatically by [`WorkerHandle`]'s `Drop`.
    Shutdown,
}

/// One report from the worker thread back to the UI.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// A load or unload finished (successfully or not).
    LoadFinished {
        /// Which stage.
        target: Target,
        /// What was requested, for a notice's message.
        source: String,
        /// How it ended.
        outcome: LoadOutcomeSummary,
    },
    /// FR-LIB-020's progress cadence.
    ScanProgress(namir_library::ScanProgress),
    /// A scan finished.
    ScanFinished(ScanOutcomeSummary),
    /// A save finished.
    StateSaved {
        /// The file it was saved to.
        path: PathBuf,
        /// The failure reason, if it did not succeed.
        error: Option<String>,
    },
    /// A recall finished.
    StateLoaded {
        /// The file it was loaded from.
        path: PathBuf,
        /// The recall's per-resource outcome, if the file was readable and parsed.
        outcome: Option<RecallOutcomeSummary>,
        /// Why nothing was recalled, if the file could not be read or parsed at all.
        error: Option<String>,
    },
    /// FR-IO-070: an audio stream failed (device lost, or another backend error) — sent by
    /// [`crate::stream`]'s own error callback, not by this thread's own loop, via the cloneable
    /// sender [`WorkerHandle::event_sender`] hands out. D-16.2's audio-thread-side of this event
    /// has already happened by the time this arrives — see `crate::stream`'s own module doc
    /// comment for the callback boundary this crosses.
    ///
    /// **Carries the classification as well as a message since M14 (issue #44).** It used to carry
    /// only `crate::audio_io::StreamFailure`'s **`Debug`** rendering, which had two consequences a
    /// human found on 2026-08-27: `Other("OS Error ...")` reached the screen with the Rust variant
    /// name in it, and [`crate::host`] had nothing left to choose a catalogue entry *from*, so it
    /// chose from `direction` and called every stream error a device loss.
    StreamFailure {
        /// Which side (input/output) failed.
        direction: crate::stream::Direction,
        /// How the backend classified it — what picks the catalogue entry.
        failure: crate::audio_io::StreamFailure,
        /// A human-readable description, naming the direction and the device.
        detail: String,
    },
}

/// A UI-facing, `Clone`-friendly summary of [`JobResult`] — `JobResult` itself carries a
/// `namir_worker::WorkerError`, which is already `Clone`, but summarising here keeps
/// [`AppEvent`]'s shape stable even if that changes, and keeps [`crate::host`] from needing to
/// match on `namir_worker` internals directly.
#[derive(Debug, Clone)]
pub enum LoadOutcomeSummary {
    /// Loaded successfully.
    Loaded {
        /// A non-fatal condition worth surfacing (D-9.7's IR truncation), with its own catalogue
        /// entry -- never a rendered string (issue #39).
        warning: Option<namir_worker::WorkerError>,
    },
    /// Failed; carries the failure with its own catalogue entry.
    Failed(namir_worker::WorkerError),
    /// Not delivered to the audio thread in time.
    NotDelivered,
    /// Unloaded successfully.
    Unloaded,
}

/// **The `WorkerError`s travel whole (issue #39).** This impl used to call `to_string()` on both,
/// storing a fully-rendered `{id}: {template} ({detail})` line where a bare detail belongs — the
/// same defect `namir_worker::error`'s `From` impls had, one layer further up, and the reason
/// step 1 of `docs/manual-tests/fr-ui-070-non-modal-error-notices.md` showed its notice twice in
/// one line. Keeping the error whole also keeps its *specific* catalogue id (`nam.load.*`,
/// `ir.load.*`), which `namir-worker` goes out of its way to preserve and this crate then threw
/// away in favour of a generic `app.host.load_failed`.
impl From<JobResult> for LoadOutcomeSummary {
    fn from(result: JobResult) -> Self {
        match result {
            JobResult::Loaded { warning, .. } => Self::Loaded { warning },
            JobResult::Failed(e) => Self::Failed(e),
            JobResult::NotDelivered(_) => Self::NotDelivered,
            JobResult::Unloaded { .. } => Self::Unloaded,
        }
    }
}

/// A `Clone`-friendly summary of one [`namir_worker::library::ScanOutcome`].
#[derive(Debug, Clone)]
pub struct ScanOutcomeSummary {
    /// Whether the scan ran to completion.
    pub complete: bool,
    /// How many entries were new or changed.
    pub upserted: usize,
    /// How many were removed.
    pub removed: usize,
    /// Any non-fatal warnings, each with its own catalogue entry (issue #39: these used to be
    /// pre-rendered strings, which `crate::host` then re-wrapped in the same shape).
    pub warnings: Vec<namir_worker::WorkerError>,
    /// Whether the scan's findings could not be persisted to disk.
    pub save_error: Option<namir_worker::WorkerError>,
}

impl From<ScanOutcome> for ScanOutcomeSummary {
    fn from(outcome: ScanOutcome) -> Self {
        Self {
            complete: outcome.complete,
            upserted: outcome.upserted,
            removed: outcome.removed,
            warnings: outcome.warnings,
            save_error: outcome.save_error,
        }
    }
}

/// A `Clone`-friendly summary of one [`RecallOutcome`].
#[derive(Debug, Clone)]
pub struct RecallOutcomeSummary {
    /// Nam's outcome.
    pub nam: LoadOutcomeSummary,
    /// Ir's outcome.
    pub ir: LoadOutcomeSummary,
    /// A missing Nam reference's display name, if any.
    pub nam_missing: Option<String>,
    /// A missing Ir reference's display name, if any.
    pub ir_missing: Option<String>,
}

fn summarise_resource(recall: ResourceRecall) -> (LoadOutcomeSummary, Option<String>) {
    match recall {
        ResourceRecall::Unloaded(o) => (o.result.into(), None),
        ResourceRecall::Loaded(o) => (o.result.into(), None),
        ResourceRecall::Missing { unload, missing } => {
            (unload.result.into(), Some(missing.display_name))
        }
    }
}

impl From<RecallOutcome> for RecallOutcomeSummary {
    fn from(outcome: RecallOutcome) -> Self {
        let (nam, nam_missing) = summarise_resource(outcome.nam);
        let (ir, ir_missing) = summarise_resource(outcome.ir);
        Self {
            nam,
            ir,
            nam_missing,
            ir_missing,
        }
    }
}

/// Everything the worker thread needs at start-up: the shared engine instance, the resource cache,
/// the library service and the pool it scans on, and the current in-memory state (kept here so
/// `SaveState` has something to serialise, and `LoadState` has somewhere to recall into).
pub struct WorkerContext {
    /// The shared engine instance (load/unload/recall) — see [`crate::instance`]'s module doc
    /// comment for why this thread and [`crate::host::AppHost`] each hold their own clone of one
    /// `Mutex`-guarded `namir_worker::Instance` rather than this thread owning it outright the way
    /// the old `LiveEngine` substitute did.
    pub instance: SharedInstance,
    /// D-8.2's process-global resource cache (`namir_worker::ResourceCache::shared()`, wired by
    /// [`crate::app::run`]) — `Instance::load`/`Instance::recall` both take this explicitly on
    /// every call rather than owning a copy the way `LiveEngine` used to.
    pub cache: Arc<ResourceCache>,
    /// FR-LIB-020's service. `Arc`-shared with [`crate::host::AppHost`], which also needs
    /// `snapshot()`/`is_scanning()` every UI frame -- `LibraryService`'s own methods all take
    /// `&self` and are internally synchronised (an `Arc<Mutex<Arc<Index>>>` plus an atomic scan
    /// flag), so sharing one instance by reference across the worker and UI threads is exactly
    /// what it is built for.
    pub library: Arc<LibraryService>,
    /// The pool the library scan runs on.
    pub pool: ThreadPool,
    /// The library roots a saved reference's `library_relative` candidate is computed against.
    pub library_roots: Vec<PathBuf>,
    /// The state this session currently reflects — updated by [`AppCommand::LoadState`], read by
    /// [`AppCommand::SaveState`]. [`crate::host::AppUiHost`] owns the authoritative live copy for
    /// per-parameter UI mirroring; this one only needs to be right at save/recall boundaries.
    pub state: Arc<Mutex<State>>,
}

/// The UI thread's handle onto the worker thread.
pub struct WorkerHandle {
    commands: mpsc::Sender<AppCommand>,
    events: mpsc::Receiver<AppEvent>,
    event_tx: mpsc::Sender<AppEvent>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WorkerHandle {
    /// Spawns the worker thread, taking ownership of `context`.
    pub fn spawn(context: WorkerContext) -> Self {
        let (command_tx, command_rx) = mpsc::channel::<AppCommand>();
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>();

        let thread = std::thread::spawn({
            let event_tx = event_tx.clone();
            move || run(context, command_rx, event_tx)
        });

        Self {
            commands: command_tx,
            events: event_rx,
            event_tx,
            thread: Some(thread),
        }
    }

    /// Enqueues a command. Never blocks (`mpsc::Sender::send` on an unbounded channel).
    pub fn send(&self, command: AppCommand) {
        // The worker thread only ever exits via `Shutdown`, sent by `Drop` below, so a send
        // failing here would mean the thread already panicked -- nothing this call can recover
        // from, and dropping the command (rather than propagating an error nobody would act on
        // differently) matches P8's degrade-not-propagate framing at this boundary.
        let _ = self.commands.send(command);
    }

    /// Drains every event currently available, in order. Never blocks.
    pub fn drain_events(&self) -> Vec<AppEvent> {
        self.events.try_iter().collect()
    }

    /// A cloneable sender onto this handle's own event queue — for a producer other than the
    /// worker thread's own loop (`crate::stream`'s error callback, running on an audio callback
    /// thread) to report into the same stream [`AppHost`](crate::host::AppHost) already polls,
    /// rather than inventing a second queue.
    pub fn event_sender(&self) -> mpsc::Sender<AppEvent> {
        self.event_tx.clone()
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.send(AppCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(ctx: WorkerContext, commands: mpsc::Receiver<AppCommand>, events: mpsc::Sender<AppEvent>) {
    let mut scan_handle: Option<ScanHandle> = None;
    for command in commands {
        match command {
            AppCommand::Shutdown => break,
            AppCommand::LoadLibraryEntry(path) => {
                let Some(kind) = namir_library::kind_from_extension(&path) else {
                    let _ = events.send(AppEvent::LoadFinished {
                        target: Target::Nam,
                        source: path.display().to_string(),
                        // The one failure on this path with no more specific catalogue entry
                        // behind it: nothing parsed, so no `nam.load.*`/`ir.load.*` id exists yet.
                        // The path is deliberately *not* in the detail -- `crate::host` prepends
                        // `source`, which is this same path, and naming it twice in one line is the
                        // shape issue #39 exists to keep out of a notice.
                        outcome: LoadOutcomeSummary::Failed(namir_worker::WorkerError::new(
                            crate::host::local_error_codes::LOAD_FAILED,
                            "the file extension is neither .nam nor .wav",
                        )),
                    });
                    continue;
                };
                let target = match kind {
                    namir_library::ItemKind::Nam => Target::Nam,
                    namir_library::ItemKind::Ir => Target::Ir,
                };
                let source_desc = path.display().to_string();
                let outcome = ctx
                    .instance
                    .with(|instance| instance.load(&ctx.cache, target, LoadSource::File(path)));
                let _ = events.send(AppEvent::LoadFinished {
                    target,
                    source: source_desc,
                    outcome: outcome.result.into(),
                });
            }
            AppCommand::RescanLibrary => {
                let progress_tx = events.clone();
                let complete_tx = events.clone();
                scan_handle = ctx.library.start_scan(
                    &ctx.pool,
                    move |progress| {
                        let _ = progress_tx.send(AppEvent::ScanProgress(progress));
                    },
                    move |outcome| {
                        let _ = complete_tx.send(AppEvent::ScanFinished(outcome.into()));
                    },
                );
            }
            AppCommand::CancelScan => {
                if let Some(handle) = scan_handle.take() {
                    handle.cancel();
                }
            }
            AppCommand::SaveState(path) => {
                let state = ctx.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let bytes = state.write();
                let error = std::fs::write(&path, bytes).err().map(|e| e.to_string());
                let _ = events.send(AppEvent::StateSaved { path, error });
            }
            AppCommand::LoadState(path) => {
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = events.send(AppEvent::StateLoaded {
                            path,
                            outcome: None,
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                };
                let (state, _warnings) = match State::read(&bytes) {
                    Ok(ok) => ok,
                    Err(e) => {
                        let _ = events.send(AppEvent::StateLoaded {
                            path,
                            outcome: None,
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                };
                let snapshot = ctx.library.snapshot();
                let resolver = LibraryResolver::new(&snapshot, &ctx.library_roots);
                let outcome = ctx
                    .instance
                    .with(|instance| instance.recall(&ctx.cache, &state, &resolver));
                *ctx.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
                let _ = events.send(AppEvent::StateLoaded {
                    path,
                    outcome: Some(outcome.into()),
                    error: None,
                });
            }
        }
    }
}
