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

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use namir_core::ContentHash;
use namir_library::LibraryResolver;
use namir_state::{FileRef, RelPath, State};
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
    /// FR-STATE-030's recall half needs a list to choose from: enumerate `<dir>` and report it
    /// back as [`AppEvent::PresetsListed`]. Off-thread because it reads a directory, which
    /// [`crate::host::AppHost::snapshot`] may not do.
    ListPresets(PathBuf),
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
    /// FR-STATE-030: the preset directory as last enumerated. Replaces whatever
    /// [`crate::host::AppHost`] was showing; an empty list is the ordinary first-run answer, not
    /// an error.
    PresetsListed(Vec<namir_ui::PresetSummary>),
    /// FR-IO-070: an audio stream failed (device lost, or another backend error).
    ///
    /// **Not sent by this thread, and since issue #88 not sent through this channel either.** The
    /// value is built by [`crate::host::AppHost`] on the UI thread, out of what
    /// [`crate::stream`]'s error callbacks pushed into [`crate::host::StreamFailureWatch`]'s
    /// bounded rings, and handed straight to `AppHost::handle_event`. It stays an [`AppEvent`]
    /// because the handling — issue #44's "the classification picks the catalogue entry" rule —
    /// should have exactly one implementation, not because a worker event is what crosses.
    ///
    /// It used to travel down this `mpsc` channel, sent from inside the `cpal` error callback:
    /// that meant a `format!` and a queue-node allocation on the stream's own thread, which
    /// NFR-RT-010 and FR-ERR-030 both forbid. D-16.2's audio-thread side of this event has
    /// already happened by the time this arrives — see `crate::stream`'s own module doc comment
    /// for the callback boundary it crosses.
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
    /// worker thread's own loop to report into the same stream
    /// [`AppHost`](crate::host::AppHost) already polls, rather than inventing a second queue.
    ///
    /// Its one caller was `crate::stream`'s error callback, and issue #88 took that away: an
    /// `mpsc` send allocates a queue node, which an audio-callback thread may not do. It is kept
    /// rather than deleted because the seam is still the right one for any *non*-RT producer, and
    /// because a `pub` method with no caller is a smaller thing to carry than a re-derived channel
    /// the next such producer would otherwise invent. **Not for a producer on an audio thread** —
    /// that is what `crate::host::StreamFailureWatch`'s rings are for.
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

/// FR-STATE-070: records which file a stage was just given, so a later `SaveState` writes a
/// preset that still knows what to reload.
///
/// The same three candidates `namir-clap`'s `worker_jobs::record_reference` records, in the same
/// order D-11.3 resolves them in — library-relative first (the one that makes a preset portable
/// between two machines whose library sits at different absolute paths), then the originating
/// absolute path, then the content hash, which is always present and is the identity (P7).
fn record_reference(
    ctx: &WorkerContext,
    target: Target,
    hash: ContentHash,
    display_name: String,
    path: &Path,
) {
    let reference = FileRef {
        hash,
        library_relative: library_relative_reference(&ctx.library_roots, path),
        absolute: Some(path.to_string_lossy().into_owned()),
        display_name,
        embedded: None,
    };
    let mut state = ctx.state.lock().unwrap_or_else(|e| e.into_inner());
    match target {
        Target::Nam => state.nam = Some(reference),
        Target::Ir => state.ir = Some(reference),
    }
}

/// `path` expressed relative to whichever configured library root contains it, or `None` if it
/// lies outside all of them (a file loaded from somewhere else entirely, for which there is no
/// library-relative form to record).
///
/// The first containing root wins, matching the order `namir_library::LibraryResolver` itself
/// tries them in, so a path recorded here resolves back to the same file it came from.
fn library_relative_reference(roots: &[PathBuf], path: &Path) -> Option<RelPath> {
    roots.iter().find_map(|root| {
        let relative = path.strip_prefix(root).ok()?;
        RelPath::from_relative_path(relative).ok()
    })
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
                // **Read here, hash here, load from bytes (FR-STATE-060/-070).** This used to be
                // `LoadSource::File(path)`, which reads the file inside `Instance::load` and hands
                // back nothing but a result — so this shell had no content hash, never built a
                // `FileRef`, and `AppCommand::SaveState` below wrote a preset that had silently
                // forgotten which model and IR were loaded. P7 makes the content hash the identity
                // of a resource, and a `FileRef` cannot be constructed without one.
                // `namir-clap`'s `worker_jobs::spawn_load_library_entry` already had exactly this
                // shape; this is the same three lines, so the two shells record the same reference
                // for the same file.
                //
                // **`read_file_bounded`, not `std::fs::read` (#145).** Reading the bytes here
                // rather than inside `Instance::load` took this path off `LoadSource::File`, which
                // was the only route through NFR-SEC-020's ceiling *and* through the `is_file()`
                // check issue #107 added — so the hash came at the price of both. It does not have
                // to: the bound belongs to the read, not to the `LoadSource`, and
                // `namir_worker::read_file_bounded` is `pub` for exactly this caller. Without it a
                // 4 GB `.wav` under a library root is read whole into memory before a parser
                // rejects it, and a named pipe at that path blocks this thread — the *only* worker
                // thread — leaving every later `SaveState`/`ListPresets`/`RescanLibrary` queued
                // behind it for good.
                let bytes = match namir_worker::read_file_bounded(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = events.send(AppEvent::LoadFinished {
                            target,
                            source: source_desc,
                            // Whole, with its own catalogue id (issue #39): `read_file_bounded`
                            // already distinguishes unreadable from too-large from not-a-regular-
                            // file, and flattening the three back into one would undo that.
                            outcome: LoadOutcomeSummary::Failed(e),
                        });
                        continue;
                    }
                };
                let hash = ContentHash::of(&bytes);
                let display_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let outcome = ctx.instance.with(|instance| {
                    instance.load(
                        &ctx.cache,
                        target,
                        LoadSource::Bytes(Arc::from(bytes.into_boxed_slice())),
                    )
                });
                let summary: LoadOutcomeSummary = outcome.result.into();
                if matches!(summary, LoadOutcomeSummary::Loaded { .. }) {
                    record_reference(&ctx, target, hash, display_name, &path);
                }
                let _ = events.send(AppEvent::LoadFinished {
                    target,
                    source: source_desc,
                    outcome: summary,
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
            AppCommand::ListPresets(dir) => {
                let _ = events.send(AppEvent::PresetsListed(crate::presets::list_presets(&dir)));
            }
            AppCommand::SaveState(path) => {
                let state = ctx.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
                // `try_write`, not `write`: NFR-SEC-020's document ceiling is enforced on the
                // write side too, and FR-STATE-080's embedded copy is the one thing in this
                // format that can realistically reach it. A refusal here is a reportable error,
                // not a truncated file on disk.
                let bytes = match state.try_write() {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        let _ = events.send(AppEvent::StateSaved {
                            path,
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                };
                // The preset directory is created on demand: a first save into a configuration
                // directory that has never held one must not fail for want of a `mkdir`.
                let error = path
                    .parent()
                    .map(std::fs::create_dir_all)
                    .transpose()
                    .and_then(|_| std::fs::write(&path, bytes))
                    .err()
                    .map(|e| e.to_string());
                let _ = events.send(AppEvent::StateSaved { path, error });
            }
            AppCommand::LoadState(path) => {
                // A user-chosen path, so exactly as untrusted as a library entry and read through
                // the same bounded reader (#145). `namir_state::Document::parse` does enforce
                // `MAX_DOCUMENT_BYTES`, but only once the whole file is already in memory — which
                // is the allocation NFR-SEC-020 exists to refuse — and it says nothing at all
                // about a path that is not a regular file.
                let bytes = match namir_worker::read_file_bounded(&path) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::{ChannelConfig, SampleRate};
    use namir_engine::{PrepareContext, RingCapacities, build_default_chain, split};
    use namir_worker::{EngineConfig, Instance, MAX_FILE_BYTES, ResourceCache};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-app-worker-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real worker thread wired to a real (no-hardware) engine — the same construction
    /// `crate::host`'s own tests use, minus the `AppHost` on top, because these tests drive
    /// [`AppCommand`]s directly.
    ///
    /// The `AudioEngine` is returned rather than dropped: dropping it retires the ring the
    /// worker's `Instance` submits into, and every load would then report `NotDelivered`.
    fn spawn_worker(dir: &Path) -> (WorkerHandle, namir_engine::AudioEngine) {
        let ctx = PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, ChannelConfig::Stereo)
            .unwrap();
        let chain = build_default_chain(&ctx).unwrap();
        let (engine, endpoint) = split(chain, RingCapacities::default());
        let instance = SharedInstance::new(Instance::new(EngineConfig { ctx }, endpoint));
        let (library, _warnings) = LibraryService::open_at(dir);
        let roots = library.roots().to_vec();
        let handle = WorkerHandle::spawn(WorkerContext {
            instance,
            cache: Arc::new(ResourceCache::new()),
            library: Arc::new(library),
            pool: ThreadPool::with_threads(1),
            library_roots: roots,
            state: Arc::new(Mutex::new(State::defaults())),
        });
        (handle, engine)
    }

    /// Waits for the first event the worker reports that `pick` accepts. The worker thread is
    /// asynchronous by construction, so a test has to wait for it rather than assume; five seconds
    /// is far longer than any of these commands takes and short enough to fail rather than hang CI.
    fn wait_for<T>(worker: &WorkerHandle, mut pick: impl FnMut(&AppEvent) -> Option<T>) -> T {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            for event in worker.drain_events() {
                if let Some(found) = pick(&event) {
                    return found;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the worker reported no matching event within the deadline");
    }

    /// A sparse file one byte past NFR-SEC-020's ceiling. `set_len` rather than writing 256 MiB:
    /// the bound is checked against the file's *length*, and every filesystem this project targets
    /// leaves the extension unallocated — the same construction `namir-worker`'s own
    /// `read_file_bounded` test uses.
    fn oversized_file(path: &Path) {
        let file = std::fs::File::create(path).unwrap();
        file.set_len(MAX_FILE_BYTES as u64 + 1).unwrap();
    }

    /// **Issue #145, finding 2.** `LoadLibraryEntry` reads the file itself (it needs the bytes'
    /// `ContentHash` for FR-STATE-060/-070's `FileRef`, which `LoadSource::File` never hands back)
    /// and that read has to be `namir_worker::read_file_bounded`, not a bare `std::fs::read`.
    /// With the bare read, a 4 GB `.wav` in a library root was pulled whole into memory and only
    /// then rejected by a parser; NFR-SEC-020's ceiling has to refuse it before a byte is read.
    //
    // Only the byte-ceiling half is asserted here: the non-regular-file half (a FIFO or character
    // device, which blocks this thread forever and with it every later command queued behind it)
    // has no portable construction, and D-5.1 confines `#[cfg(unix)]` to `namir-platform`. No
    // `trace:` tag either -- NFR-SEC-020's ledger entry is not this test's to move.
    #[test]
    fn an_oversized_library_entry_is_refused_before_it_is_read() {
        let dir = temp_dir("oversized_entry");
        let (worker, _engine) = spawn_worker(&dir);
        let path = dir.join("Library").join("huge.nam");
        oversized_file(&path);

        worker.send(AppCommand::LoadLibraryEntry(path));
        let error = wait_for(&worker, |event| match event {
            AppEvent::LoadFinished {
                outcome: LoadOutcomeSummary::Failed(e),
                ..
            } => Some(e.clone()),
            _ => None,
        });

        assert_eq!(
            error.code.id,
            namir_worker::error_codes::FILE_TOO_LARGE.id,
            "an oversized library entry must be refused by NFR-SEC-020's ceiling, not read \
             whole into memory and then refused by a parser: got {error}"
        );
        drop(worker);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Issue #145, finding 2**, the preset half: `LoadState` reads a user-chosen path, so it is
    /// exactly as untrusted as a library entry and goes through the same bounded reader.
    /// `namir_state::Document::parse` does check `MAX_DOCUMENT_BYTES`, but only *after* the whole
    /// file is already in memory — which is the allocation NFR-SEC-020 exists to prevent, and is
    /// no defence at all against a path that is not a regular file.
    //
    // Only the byte-ceiling half is asserted here: the non-regular-file half (a FIFO or character
    // device, which blocks this thread forever and with it every later command queued behind it)
    // has no portable construction, and D-5.1 confines `#[cfg(unix)]` to `namir-platform`. No
    // `trace:` tag either -- NFR-SEC-020's ledger entry is not this test's to move.
    #[test]
    fn an_oversized_preset_is_refused_before_it_is_read() {
        let dir = temp_dir("oversized_preset");
        let (worker, _engine) = spawn_worker(&dir);
        let path = dir.join("huge.namirpreset");
        oversized_file(&path);

        worker.send(AppCommand::LoadState(path));
        let error = wait_for(&worker, |event| match event {
            AppEvent::StateLoaded { error, .. } => error.clone(),
            _ => None,
        });

        assert!(
            error.contains(namir_worker::error_codes::FILE_TOO_LARGE.id),
            "an oversized preset must be refused by NFR-SEC-020's ceiling before the read, not \
             after `Document::parse` has already been handed 256 MiB: got {error}"
        );
        drop(worker);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
