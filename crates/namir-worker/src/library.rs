//! [`LibraryService`]: seam 3's composition (`namir-library`'s own crate doc comment — "both are
//! caller-supplied … `LibraryService::open(index_path, roots)`"), and seam 2's other half: this
//! crate drives `namir-library`'s caller-pumped [`Scanner`](namir_library::Scanner) step machine
//! on [`ThreadPool`], which is D-12.2's "cancellable worker job" made literally true by splitting
//! it across the two crates the way `namir-library`'s own doc comment says it would — the
//! *mechanism* lives there, the *job* (thread, cancellation flag, progress cadence) lives here.
//!
//! # M14: opening a library does not read the index file (§22 R-18, issue #22)
//!
//! [`LibraryService::open`] used to be `IndexStore::open` — a `read` plus a `serde_json` parse of
//! the whole index — on the calling thread, before it returned. That put one JSON parse **on the
//! plugin instantiation path, per instance, with no sharing between instances**, which is where
//! NFR-PERF-040's margin went: 187.5 ms of a 200 ms Must at FR-LIB-020's stated 10 000-file scale,
//! of which chain construction is 147 µs and essentially all the rest is that parse
//! (`crates/namir-clap/benches/plugin_instantiation.rs`'s own measured table). Asserting on `max`,
//! the breach arrived near 10 700 entries — so a user with a large library could fail a Must on a
//! machine where nothing was wrong, and no regression test could ever catch it because nothing
//! regresses: the cost is there today and stays constant.
//!
//! Two changes take it off that path, both inside this module so that neither product shell needs
//! its own copy of anything (the duplication [`LibraryService::open_default`]'s doc comment
//! records a real bug for):
//!
//! 1. **The load is deferred and happens off the caller's thread.** `open` registers the path and
//!    returns; a short-lived loader thread does the read and parse and publishes the result. Until
//!    it lands, [`LibraryService::snapshot`] returns an empty index rather than blocking — a
//!    library that fills in a moment after the window appears, instead of a window that does not
//!    appear until the library is read.
//! 2. **One parsed index per path per process, shared by every service.** A second instance opening
//!    the same index file gets the same live [`Index`] the first one has, so ten plugin instances
//!    in a host cost one parse rather than ten. The shared slot is what a completing scan writes
//!    into, so instances also stop disagreeing about what the library contains.
//!
//! **What every caller must know**, because it is a real change of contract and not an
//! optimisation behind an unchanged interface:
//!
//! * `open`'s returned warning list is now always empty — the load has not happened yet when it
//!   returns. A corrupt index still degrades to an empty one with a warning; that warning is
//!   drained with [`LibraryService::take_load_warnings`] once the load has landed.
//! * Anything that needs the index *now* — a scan, a count, a test — calls
//!   [`LibraryService::ensure_loaded`] first, which blocks until it is there. [`Self::start_scan`]
//!   does this **inside its pool job**, because a scan whose `prior` snapshot were the
//!   not-yet-loaded empty index would conclude that every entry it does not re-find was removed,
//!   which is the erase-the-index failure this module already has one scar from.
//! * Freshness across processes is kept by a stat, not a parse: `open` compares the file's
//!   modification time and length against the ones the cached parse was taken from, and reloads if
//!   they differ. The standalone and the plugin running side by side therefore still see each
//!   other's rescans, at the cost of one `metadata` call per open.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant, SystemTime};

use namir_library::{Index, IndexStore, ScanProgress, Scanner, StdFs, Step};

use crate::error::WorkerError;
use crate::pool::ThreadPool;

/// The progress callback's cadence while a scan runs. FR-UI-060 (M6) wants a UI that stays
/// responsive during a 10,000-file scan; this crate has no notion of a frame, so rather than
/// guess at the UI's own refresh rate, the callback fires at a fraction of a plausible frame
/// budget a UI could always keep up with — half the 16 ms/60 Hz frame `docs/02-architecture.md`
/// already uses as a reference figure elsewhere, so a progress callback landing on the wrong
/// side of a frame boundary can never itself be what causes a UI to miss one.
const SCAN_PROGRESS_CADENCE: Duration = Duration::from_millis(50);

/// A running scan's cancel switch, returned by [`LibraryService::start_scan`].
///
/// Dropping this without calling [`Self::cancel`] lets the scan run to completion — cancellation
/// is opt-in, never implied by the handle going out of scope, so a caller that only wants
/// fire-and-forget behaviour can discard it safely.
pub struct ScanHandle {
    cancel: Arc<AtomicBool>,
}

impl ScanHandle {
    /// Requests cancellation. Not immediate: `namir-library`'s step machine notices between
    /// steps (`Scanner::step` does at most one directory-expand or one file-examine), so at most
    /// one more unit of work completes first. Everything examined before that point is still
    /// committed — see [`ScanOutcome::complete`]'s doc comment for why that is safe.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// How one [`LibraryService::start_scan`] run ended.
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    /// Whether every directory reachable from the configured roots was expanded before the scan
    /// stopped. `false` after [`ScanHandle::cancel`] — and, when it is, [`Self::removed`] is
    /// always `0`: `namir_library::scan`'s own rule is that an incomplete walk cannot conclude a
    /// path it never reached is gone, so a cancelled scan reports upserts (what it *did* learn)
    /// but never removals (what it did not have the chance to rule out).
    pub complete: bool,
    /// How many entries were new or changed this run.
    pub upserted: usize,
    /// How many were removed. Always `0` unless `complete` — see this struct's own doc comment.
    pub removed: usize,
    /// Non-fatal conditions encountered along the way (an unreadable file, a non-UTF-8 path, a
    /// directory that could not be listed).
    pub warnings: Vec<WorkerError>,
    /// `Some` if the scan's findings could not be persisted to the index file. The in-memory
    /// index — visible through [`LibraryService::snapshot`] from the moment this callback fires —
    /// is updated regardless of this field: a save failure degrades to "this session's view is
    /// current, the file on disk is stale" rather than losing the scan's work outright (P8).
    pub save_error: Option<WorkerError>,
}

/// The worker-side half of `namir-library`: owns the index file's path and the configured
/// library roots — the two things `namir-library` deliberately never learns on its own (seam 3)
/// — and drives a scan on a [`ThreadPool`].
///
/// [`Self::snapshot`] is cheap enough to call every frame: the index behind it is swapped
/// wholesale by a completing scan (an `Arc` store, never a mutation in place), so a reader is
/// never blocked by one and never observes a half-updated index.
pub struct LibraryService {
    shared: Arc<SharedIndex>,
    roots: Vec<PathBuf>,
    /// Guards against two scans running against the same service at once — see
    /// [`Self::start_scan`]'s doc comment for why that case is refused rather than resolved.
    scanning: Arc<AtomicBool>,
}

/// What the file's contents were read from, cheap enough to take on every open: a parse is 161 ms
/// at 10 000 entries and a `metadata` call is microseconds, so freshness costs a stat and only a
/// genuine change costs a parse.
///
/// `None` when the file does not exist (the first-run case) or its metadata cannot be read, and two
/// `None`s compare equal — an index that is absent now and was absent when the cached parse was
/// taken has not changed.
type Stamp = Option<(SystemTime, u64)>;

/// One index file's parsed contents, shared by every [`LibraryService`] in this process that names
/// the same path.
///
/// The `Mutex<Option<Loaded>>` is the load's own mutual exclusion: the loader thread holds it for
/// the duration of the read and parse, so a second caller either finds the work done or waits for
/// it, and nothing parses the same file twice concurrently. `index` is a separate lock because it
/// is read every frame and must never be held behind a parse.
struct SharedIndex {
    path: PathBuf,
    loaded: Mutex<Option<Loaded>>,
    index: Mutex<Arc<Index>>,
    warnings: Mutex<Vec<WorkerError>>,
}

struct Loaded {
    store: IndexStore,
    stamp: Stamp,
}

/// Every `SharedIndex` alive in this process, by index path.
///
/// `Weak`, so an entry dies with the last service holding it: a process that opens a throwaway
/// index (every test in this module does) must not keep it parsed and cached forever, and a stale
/// entry outliving its file is exactly the kind of hidden state that makes one test's result depend
/// on another's.
fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<SharedIndex>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<SharedIndex>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn stamp_of(path: &Path) -> Stamp {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

impl SharedIndex {
    /// The shared entry for `path`, created if this process has none.
    fn for_path(path: PathBuf) -> Arc<SharedIndex> {
        let mut registry = lock(registry());
        if let Some(existing) = registry.get(&path).and_then(Weak::upgrade) {
            return existing;
        }
        let shared = Arc::new(SharedIndex {
            path: path.clone(),
            loaded: Mutex::new(None),
            index: Mutex::new(Arc::new(Index::empty())),
            warnings: Mutex::new(Vec::new()),
        });
        registry.insert(path, Arc::downgrade(&shared));
        // Entries whose service has been dropped are cleared here rather than by a background
        // sweep: this map is only ever touched on an open, which is rare, and a `Weak` that cannot
        // upgrade costs nothing to remove.
        registry.retain(|_, weak| weak.strong_count() > 0);
        shared
    }

    /// Reads and parses the index if this process has not already done so for the current contents
    /// of the file, and returns the store either way. **Blocks**; never call it on an audio thread
    /// or on a plugin's instantiation path.
    fn ensure_loaded(&self) -> IndexStore {
        let mut loaded = lock(&self.loaded);
        let stamp = stamp_of(&self.path);
        if let Some(current) = loaded.as_ref()
            && current.stamp == stamp
        {
            return current.store.clone();
        }
        let (store, index, warnings) = IndexStore::open(self.path.clone());
        *lock(&self.index) = Arc::new(index);
        lock(&self.warnings).extend(warnings.into_iter().map(WorkerError::from));
        *loaded = Some(Loaded {
            store: store.clone(),
            stamp,
        });
        store
    }

    /// Publishes a scan's result into the shared slot and records the stamp of the file it was just
    /// saved to, so the next open does not re-parse a file this process wrote itself.
    fn publish(&self, index: Index, saved: bool) {
        *lock(&self.index) = Arc::new(index);
        if saved && let Some(loaded) = lock(&self.loaded).as_mut() {
            loaded.stamp = stamp_of(&self.path);
        }
    }
}

/// Starts the deferred load on a thread of its own, unless this process is already doing it or has
/// already done it for the file as it stands.
///
/// One short-lived thread per *stale or first* open, not per open: the `try_lock` fails while a
/// loader is running, and a fresh cached parse returns without spawning anything, so the ordinary
/// case of a host adding a tenth plugin instance spawns nothing at all. A failed spawn is not an
/// error — [`LibraryService::ensure_loaded`] will do the work on whatever thread next needs the
/// index, which is the same fallback as a machine that refuses threads under NFR-PORT-030.
fn schedule_load(shared: &Arc<SharedIndex>) {
    let up_to_date = match shared.loaded.try_lock() {
        Ok(guard) => guard
            .as_ref()
            .is_some_and(|loaded| loaded.stamp == stamp_of(&shared.path)),
        Err(TryLockError::Poisoned(poisoned)) => poisoned
            .into_inner()
            .as_ref()
            .is_some_and(|loaded| loaded.stamp == stamp_of(&shared.path)),
        // Held: a loader is running. It is doing exactly what this function would ask for.
        Err(TryLockError::WouldBlock) => true,
    };
    if up_to_date {
        return;
    }
    let shared = Arc::clone(shared);
    let _ = std::thread::Builder::new()
        .name("namir-library-index".to_string())
        .spawn(move || {
            shared.ensure_loaded();
        });
}

impl LibraryService {
    /// Registers the index at `index_path` and configures `roots` as the directories a scan walks,
    /// in the order a resolved reference tries them (`namir_library::resolver`'s own rule).
    ///
    /// Never fails (P8, mirroring `IndexStore::open`'s own guarantee): a missing index file is
    /// the ordinary first-run case and produces no warning at all; a present-but-corrupt or
    /// wrong-version one degrades to an empty index plus a warning.
    ///
    /// **This does not read the index file** (M14, §22 R-18 — see this module's header). It costs
    /// one `metadata` call and, when this process has not already parsed the file as it currently
    /// stands, the spawn of a loader thread. The returned warning list is therefore **always
    /// empty**: nothing has been read yet, so nothing can have been found wrong with it. Drain the
    /// load's warnings with [`Self::take_load_warnings`] after the index has landed, and block for
    /// it with [`Self::ensure_loaded`] where a caller genuinely cannot proceed without the index.
    ///
    /// The tuple return is kept rather than narrowed to `Self` so that both product shells and
    /// every test keep compiling against one shape while the warning delivery point moves; a
    /// caller that ignored the second element before is correct in continuing to.
    pub fn open(index_path: PathBuf, roots: Vec<PathBuf>) -> (LibraryService, Vec<WorkerError>) {
        let shared = SharedIndex::for_path(index_path);
        schedule_load(&shared);
        (
            LibraryService {
                shared,
                roots,
                scanning: Arc::new(AtomicBool::new(false)),
            },
            Vec::new(),
        )
    }

    /// Blocks until this process has the index file's contents, parsing it here if the loader
    /// thread has not got to it (or could not be spawned).
    ///
    /// **Never call this on an audio thread or on a plugin's instantiation path** — parsing a
    /// 10 000-entry index is ~160 ms on the reference machine, which is the whole of what M14 took
    /// off that path. It is here for the callers that cannot proceed without the index: a scan
    /// (which does it inside its own pool job), a shell that wants to report the entry count, and
    /// tests.
    pub fn ensure_loaded(&self) {
        self.shared.ensure_loaded();
    }

    /// The warnings the deferred load produced — a corrupt or wrong-version index file — removing
    /// them from this service so they are delivered once.
    ///
    /// Empty while the load has not landed; that is not the same as "the index is fine", so a
    /// caller wanting a definite answer calls [`Self::ensure_loaded`] first. Shared across every
    /// service naming the same path, like the index itself: the first caller to drain them is the
    /// one that reports them.
    pub fn take_load_warnings(&self) -> Vec<WorkerError> {
        std::mem::take(&mut lock(&self.shared.warnings))
    }

    /// The library roots this service scans, in configured order.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The one per-user default location every product shell shares, at an explicitly-supplied
    /// config directory: an index at `<config_dir>/library-index.json` and one root,
    /// `<config_dir>/Library`, created if it doesn't exist yet (a scan over a directory that
    /// doesn't exist would otherwise report every already-known entry as removed relative to
    /// nothing — P8: degrade gracefully rather than surface a spurious warning on a first launch).
    ///
    /// Takes `config_dir` as a parameter, rather than calling `namir_platform::config_dir()`
    /// itself, for the same reason `namir_platform::paths`' own `config_dir_from` takes an
    /// injectable environment lookup instead of reading `std::env` directly: a test can supply a
    /// throwaway directory without touching this machine's real per-user config location.
    /// [`Self::open_default`] is the real-environment caller.
    pub fn open_at(config_dir: &std::path::Path) -> (LibraryService, Vec<WorkerError>) {
        let index_path = config_dir.join("library-index.json");
        let default_root = config_dir.join("Library");
        let _ = std::fs::create_dir_all(&default_root);
        Self::open(index_path, vec![default_root])
    }

    /// [`Self::open_at`], resolved against this machine's real `namir_platform::config_dir()`.
    /// `None` under the same conditions that itself degrades to `None` for.
    ///
    /// **This is the only correct way for a product shell to open its default library.** See this
    /// module's own history: `namir-clap` once opened with an empty root list instead of this
    /// default, on the theory that "no UI to configure a root yet" meant "leave it unconfigured"
    /// was harmless. It wasn't: `namir-library`'s scan rule is that a *complete* walk (which a
    /// walk over zero roots trivially is) concludes every path it didn't see is removed, so a
    /// rescan against an empty root list didn't just fail to find new files, it erased every
    /// entry `namir-app` had already indexed at the *same* index path both products share. Both
    /// product shells now call this one function instead of each computing the default
    /// independently, specifically so this can't happen a second time by two crates' bootstrap
    /// logic drifting apart.
    pub fn open_default() -> Option<(LibraryService, Vec<WorkerError>)> {
        Some(Self::open_at(&namir_platform::config_dir()?))
    }

    /// A cheap, point-in-time view of the index — safe from any thread, at any time, including
    /// concurrently with a running scan (see this struct's own doc comment).
    ///
    /// **Never blocks, and is empty until the deferred load lands** (M14): a UI polling this every
    /// frame gets an empty library for the moment the parse takes and then the real one, rather
    /// than a frame that takes as long as the parse. A caller that needs the loaded index rather
    /// than the current one calls [`Self::ensure_loaded`] first.
    pub fn snapshot(&self) -> Arc<Index> {
        Arc::clone(&lock(&self.shared.index))
    }

    /// Whether a scan started by this service is currently running.
    pub fn is_scanning(&self) -> bool {
        self.scanning.load(Ordering::Acquire)
    }

    /// Starts a scan on `pool`. Returns `None`, starting nothing, if a scan against this service
    /// is already running: `Scanner::new` takes a single `prior` snapshot at the moment it is
    /// built, so two scans running concurrently against the same roots would each learn from a
    /// stale view of what the other is finding and race on whose findings get committed last —
    /// simpler and safer to refuse the second than to define an ordering nobody needs.
    ///
    /// `on_progress` runs on the pool thread roughly every [`SCAN_PROGRESS_CADENCE`] while the
    /// scan is in flight, and exactly once more with the final state before `on_complete` runs —
    /// so a caller always sees a terminal progress report even when the whole scan finished
    /// inside one cadence window (a small or already-up-to-date library, the common case).
    /// `on_complete` runs exactly once, after the scan's findings — whatever was learned before
    /// completion or cancellation — have been committed to the in-memory index and a save to
    /// disk has been attempted.
    ///
    /// **Isolated per D-16.3**, inherited rather than reimplemented: the scan closure runs
    /// through [`ThreadPool::spawn`], so a panic inside it is caught at the job boundary exactly
    /// as any other job this crate submits. `scanning` is cleared in that case too, by
    /// [`ScanFlag`]'s `Drop` rather than by the `store` at the end of the job body — the pool's
    /// `catch_unwind` runs the closure only up to the panic point, so a flag cleared by a
    /// statement past that point would stay `true` and refuse every later scan for the life of
    /// the process (issue #109). Containment is D-16.3's boundary; losing the library's
    /// rescannability to it was not, and this method no longer does.
    pub fn start_scan(
        &self,
        pool: &ThreadPool,
        mut on_progress: impl FnMut(ScanProgress) + Send + 'static,
        on_complete: impl FnOnce(ScanOutcome) + Send + 'static,
    ) -> Option<ScanHandle> {
        if self.scanning.swap(true, Ordering::AcqRel) {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = ScanHandle {
            cancel: Arc::clone(&cancel),
        };

        let roots = self.roots.clone();
        let shared = Arc::clone(&self.shared);
        // Moved into the job and held for its whole duration, so that an unwind from anywhere
        // inside still clears the flag; released explicitly before `on_complete` below.
        let scanning = ScanFlag(Arc::clone(&self.scanning));

        pool.spawn(move || {
            // **Inside the job, and the `prior` snapshot is taken after it** (M14). The load is
            // deferred, so a `prior` read on the calling thread could be the empty
            // not-yet-loaded index -- and a *complete* walk against an empty prior concludes that
            // every entry it does not re-find was removed, which is the erase-the-shared-index
            // failure `open_default`'s doc comment records. Blocking here costs a pool thread the
            // parse, once per process, on a job that is about to read the whole library anyway.
            let store = shared.ensure_loaded();
            let prior = Arc::clone(&lock(&shared.index));

            let mut scanner = Scanner::new(roots, &prior);
            let mut last_progress = ScanProgress::default();
            let mut last_reported_at = Instant::now();
            loop {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                match scanner.step(&StdFs) {
                    Step::Progressed(progress) => {
                        last_progress = progress;
                        if last_reported_at.elapsed() >= SCAN_PROGRESS_CADENCE {
                            on_progress(last_progress);
                            last_reported_at = Instant::now();
                        }
                    }
                    Step::Finished => break,
                }
            }
            // The terminal report, unconditionally -- see this method's doc comment.
            on_progress(last_progress);

            let delta = scanner.take_delta();
            let complete = delta.complete;
            let upserted = delta.upserts.len();
            let removed = delta.removals.len();
            let warnings = delta
                .warnings
                .iter()
                .cloned()
                .map(WorkerError::from)
                .collect();

            // Cancellation commits what it learned (namir_library::scan's own doc comment: the
            // whole reason a cancelled scan is not pure waste) -- so this runs unconditionally,
            // not only when complete.
            let mut new_index = (*prior).clone();
            new_index.apply(delta);
            let save_error = store.save_atomic(&new_index).err().map(WorkerError::from);
            // Into the *shared* slot, so every service in this process naming this index file sees
            // the scan's result -- and carrying the saved file's new stamp with it, so the next
            // open does not re-parse a file this process just wrote.
            shared.publish(new_index, save_error.is_none());

            // Cleared before on_complete, not after: a caller that starts a new scan from inside
            // its own on_complete callback must see is_scanning() == false by then. Dropping the
            // guard early is what does it; the guard itself is only the unwinding path's backstop.
            drop(scanning);

            on_complete(ScanOutcome {
                complete,
                upserted,
                removed,
                warnings,
                save_error,
            });
        });

        Some(handle)
    }
}

/// Owns the "a scan is running" bit for the duration of one scan job and clears it on drop.
///
/// A plain `store(false)` at the end of the job body is not equivalent: the pool contains a
/// panicking job at its own boundary (D-16.3), so a statement past the panic point never runs and
/// the flag would stay `true` — [`LibraryService::start_scan`] would then refuse every later scan
/// for the life of the process, silently, since the only trace of the panic is a log record
/// (issue #109). `Drop` runs during the unwind, so containment no longer costs the flag.
struct ScanFlag(Arc<AtomicBool>);

impl Drop for ScanFlag {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // P8, mirroring pool.rs's and cache.rs's identical recovery: a panic elsewhere must not
    // permanently disable this service's index access.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-worker-library-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Moves a file's modification time `seconds` into the past. `namir-library`'s incremental rule
    /// treats a file whose mtime is within two seconds of the previous scan's completion as
    /// suspect and rehashes it, so a test about the *unchanged* path has to leave that window.
    fn age_mtime(path: &std::path::Path, seconds: u64) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        let aged = std::time::SystemTime::now() - Duration::from_secs(seconds);
        file.set_modified(aged).unwrap();
    }

    fn write_nam(dir: &std::path::Path, name: &str) {
        let model =
            namir_fixtures::nam::generate(namir_fixtures::nam::WaveNetShape::Nano, 1).unwrap();
        std::fs::write(dir.join(name), model.to_json_bytes()).unwrap();
    }

    /// How long every test here but one waits for its scan's terminal `ScanOutcome`. Ample: each
    /// of those fixtures is two files or fewer, and
    /// `cancelling_a_large_scan_stops_it_before_completion` uses the 10,000-file corpus but
    /// cancels immediately rather than reading it to the end.
    const SCAN_BUDGET: Duration = Duration::from_secs(30);

    /// The exception, for `a_full_scan_of_the_shared_corpus_reports_progress_more_than_once`: the
    /// only test in this module that reads *and content-hashes* all 10,000 files of
    /// `namir-fixtures`' shared corpus through to completion, which is a different order of work
    /// from every neighbour above.
    ///
    /// **Why it differs, recorded rather than left as an unexplained number.** That test failed
    /// once at [`SCAN_BUDGET`] and then passed eight consecutive re-runs unchanged on the same
    /// machine — the signature of a budget sized for the median run rather than for one sharing a
    /// disk with the rest of `cargo test --workspace`, not of a broken assertion. It runs in CI's
    /// `build-test` job, which already blocks a merge, so an intermittent timeout there is
    /// expensive and this is the cheap fix. Deliberately **not** paired with any change to that
    /// test's `>= 2` progress-call threshold: the threshold is the property under test and was
    /// never what was marginal.
    const FULL_CORPUS_SCAN_BUDGET: Duration = Duration::from_secs(180);

    fn recv(rx: &mpsc::Receiver<ScanOutcome>) -> ScanOutcome {
        recv_within(rx, SCAN_BUDGET)
    }

    fn recv_within(rx: &mpsc::Receiver<ScanOutcome>, budget: Duration) -> ScanOutcome {
        rx.recv_timeout(budget)
            .unwrap_or_else(|e| panic!("the scan job should have completed within {budget:?}: {e}"))
    }

    /// FR-LIB-010, driven end to end through the worker: a scan started on the pool finds files
    /// under the configured roots and commits them into the in-memory index *and* the on-disk
    /// store, and reports a terminal progress call even though this fixture is far smaller than
    /// one [`SCAN_PROGRESS_CADENCE`] window.
    #[test]
    fn a_scan_commits_found_files_to_the_snapshot_and_the_store() {
        let root = temp_dir("commit");
        write_nam(&root, "a.nam");
        write_nam(&root, "b.nam");
        let index_path = root.join("index.json");

        let (service, warnings) = LibraryService::open(index_path.clone(), vec![root.clone()]);
        assert!(warnings.is_empty());
        assert!(service.snapshot().is_empty());

        let pool = ThreadPool::with_threads(1);
        let (tx, rx) = mpsc::channel();
        let progress_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = Arc::clone(&progress_calls);
        service
            .start_scan(
                &pool,
                move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                },
                move |outcome| tx.send(outcome).unwrap(),
            )
            .expect("no scan was already running");

        let outcome = recv(&rx);
        assert!(outcome.complete);
        assert_eq!(outcome.upserted, 2);
        assert_eq!(outcome.removed, 0);
        assert!(outcome.warnings.is_empty());
        assert!(outcome.save_error.is_none());
        assert!(
            progress_calls.load(Ordering::SeqCst) >= 1,
            "the terminal progress report must fire even for a scan shorter than the cadence"
        );

        assert_eq!(service.snapshot().len(), 2);
        assert!(!service.is_scanning());

        // The commit reached disk too, not only the in-memory snapshot.
        let (_store, reloaded, reload_warnings) = namir_library::IndexStore::open(index_path);
        assert!(reload_warnings.is_empty());
        assert_eq!(reloaded.len(), 2);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_at_places_the_index_and_default_root_under_config_dir() {
        let dir = std::path::PathBuf::from("/config/dir");
        let (service, warnings) = LibraryService::open_at(&dir);
        assert!(warnings.is_empty());
        assert_eq!(service.roots(), [dir.join("Library")].as_slice());
    }

    /// A first launch (no config directory yet at all) opens cleanly with an empty index and no
    /// warnings, and the default root now exists on disk for a future scan to walk.
    #[test]
    fn open_at_creates_the_default_root_on_first_launch() {
        let dir = temp_dir("open_at_first_launch");
        let _ = std::fs::remove_dir_all(&dir); // temp_dir() itself creates it; start from absent.
        let (service, warnings) = LibraryService::open_at(&dir);
        assert!(warnings.is_empty());
        assert!(service.snapshot().is_empty());
        assert!(dir.join("Library").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The regression this function exists to prevent: two independent product shells (or one
    /// shell across two runs) each calling `open_at` against the *same* config directory must
    /// resolve to the *same* root, so a scan run by either one finds -- and never erases -- what
    /// the other already indexed. Before `open_at`/`open_default` existed, `namir-clap` opened its
    /// own `LibraryService` with an empty root list; a scan against zero roots completes
    /// trivially and, per `namir_library::scan`'s own "a complete walk removes every path it
    /// didn't see" rule, would have wiped every entry a prior `namir-app` scan had already
    /// committed to this same index file. This test proves that can't happen through this
    /// function: opening twice and scanning both times retains the file the first scan found.
    #[test]
    fn two_opens_of_the_same_config_dir_share_a_root_and_a_second_scan_does_not_erase_the_first() {
        let dir = temp_dir("shared_config_dir");
        let pool = ThreadPool::with_threads(1);

        let (first, _) = LibraryService::open_at(&dir);
        write_nam(&dir.join("Library"), "a.nam");
        let (tx, rx) = mpsc::channel();
        first
            .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
            .unwrap();
        let outcome = recv(&rx);
        assert!(outcome.complete);
        assert_eq!(outcome.upserted, 1);
        assert_eq!(first.snapshot().len(), 1);

        // A second, independently-constructed service against the identical config_dir -- the
        // shape of a second product shell opening the same per-user location.
        let (second, _) = LibraryService::open_at(&dir);
        assert_eq!(second.roots(), first.roots());
        let (tx2, rx2) = mpsc::channel();
        second
            .start_scan(&pool, |_| {}, move |outcome| tx2.send(outcome).unwrap())
            .unwrap();
        let outcome2 = recv(&rx2);
        assert!(outcome2.complete);
        assert_eq!(
            outcome2.removed, 0,
            "a second open against the same config dir must not lose the first scan's file"
        );
        assert_eq!(second.snapshot().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The load-then-read-`prior` ordering inside `start_scan`'s job, discriminated.** Its
    /// neighbour above shares an already-loaded `SharedIndex` between two services, so the
    /// deferred load has always finished before the scan job runs: that test passes with the two
    /// lines transposed and verifies sharing, not sequencing.
    ///
    /// Three things are needed to reach the not-yet-loaded state deterministically, and a fourth
    /// to make it observable.
    ///
    /// The first service is **dropped**, so its `Weak` registry entry dies and the next open
    /// builds a `SharedIndex` that has parsed nothing. The test then takes `for_path`'s `Arc`
    /// *before* opening the service and holds its `loaded` mutex -- exactly the condition
    /// `schedule_load` reads as "a loader is already running" (`TryLockError::WouldBlock`) and
    /// returns on, so no loader thread is spawned and the state cannot resolve behind the test's
    /// back. Releasing the guard afterwards lets both orderings finish; only one read the index
    /// first.
    ///
    /// The observable is `upserted`, and it needs the sleep. `removed` cannot serve: an empty
    /// `prior` has no entries to conclude removals about, so it is `0` either way. `upserted`
    /// distinguishes them only once the file is outside `namir_library`'s two-second mtime
    /// settling window relative to the first scan's completion -- inside it the file is rehashed
    /// unconditionally and upserted whatever `prior` said, which is correct behaviour and is why
    /// the sleep is before the first scan rather than after it.
    #[test]
    fn a_scan_that_starts_before_the_deferred_load_still_sees_the_saved_index() {
        let dir = temp_dir("scan_before_load");
        let index_path = dir.join("library-index.json");

        // A first service scans and saves, then goes away entirely. The file is written well
        // before that scan completes, so the settling window does not cover it next time.
        //
        // **The first phase gets a pool of its own, and dropping it is load-bearing.**
        // `start_scan` hands its job an `Arc::clone` of the `SharedIndex`, and the outcome
        // callback fires *inside* that closure -- so `recv` returning does not mean the job has
        // been dropped, and the pool thread can still hold the `Arc` seconds later. Dropping
        // `first` then leaves the registry's `Weak` upgradable, `for_path` below returns the
        // already-loaded entry instead of a fresh one, and the `held.is_none()` assertion fires.
        // That is not hypothetical: it passed here and in the pull-request runs and failed on
        // trunk under `cargo llvm-cov`, whose instrumentation shifts the timing (M14).
        // `ThreadPool`'s `Drop` calls `shutdown`, which joins every thread, so the scope end is
        // the barrier -- deterministic, rather than a sleep long enough to usually work.
        {
            let pool = ThreadPool::with_threads(1);
            let (first, _) = LibraryService::open_at(&dir);
            write_nam(&dir.join("Library"), "a.nam");
            std::thread::sleep(Duration::from_millis(2_100));
            let (tx, rx) = mpsc::channel();
            first
                .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
                .unwrap();
            let outcome = recv(&rx);
            assert!(outcome.complete && outcome.save_error.is_none());
            assert_eq!(outcome.upserted, 1, "the first scan discovers the file");
        }

        // Pin the fresh shared entry and hold its load, so `open_at` below spawns no loader.
        let shared = SharedIndex::for_path(index_path);
        let held: MutexGuard<'_, Option<Loaded>> = lock(&shared.loaded);
        assert!(
            held.is_none(),
            "a dropped service must not leave a parse behind"
        );

        let (second, warnings) = LibraryService::open_at(&dir);
        assert!(
            warnings.is_empty(),
            "open reads nothing, so it can warn about nothing"
        );
        assert_eq!(second.snapshot().len(), 0, "nothing is loaded yet");

        let pool = ThreadPool::with_threads(1);
        let (tx, rx) = mpsc::channel();
        second
            .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
            .unwrap();

        // The job is now blocked inside `ensure_loaded` on the guard this thread holds -- or, if
        // the two lines were transposed, has already taken an empty `prior` and blocks after the
        // fact.
        drop(held);

        let outcome = recv(&rx);
        assert!(outcome.complete);
        assert_eq!(
            outcome.upserted, 0,
            "the file is unchanged and already in the saved index, so a job that read the index \
             before taking `prior` has nothing to upsert -- a non-zero count here means it took \
             `prior` from the not-yet-loaded empty index"
        );
        assert_eq!(second.snapshot().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second `start_scan` while one is already running is refused rather than started, and
    /// `is_scanning()` reflects the running job.
    #[test]
    fn a_second_concurrent_scan_is_refused() {
        let root = temp_dir("concurrent");
        write_nam(&root, "a.nam");
        let (service, _) = LibraryService::open(root.join("index.json"), vec![root.clone()]);

        let pool = ThreadPool::with_threads(1);
        let (tx, rx) = mpsc::channel();
        let handle = service
            .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
            .expect("first scan should start");

        // The pool has one thread and the job has been queued; whether or not it has begun
        // running yet, `scanning` was set synchronously inside `start_scan`, before this method
        // ever returned -- so this check is deterministic, not a timing race.
        assert!(service.is_scanning());
        assert!(
            service.start_scan(&pool, |_| {}, |_| {}).is_none(),
            "a scan already in flight must refuse a second one"
        );

        recv(&rx);
        assert!(!service.is_scanning());
        drop(handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **Issue #109.** A panic inside the scan job -- injected here through the caller's own
    /// `on_progress`, which `start_scan` calls from the pool thread and which is the one part of
    /// the job body a test can make fail without a fault-injection seam -- must not leave
    /// `scanning` stuck `true`. D-16.3 contains the panic at the job boundary; containment must
    /// not also cost this service every later scan for the life of the process, which is what a
    /// clear-at-the-end `store(false)` past the panic point did.
    ///
    /// The second job is the synchronisation: the pool has one thread and runs its queue in
    /// order, so its arrival proves the panicked job has already unwound (and therefore that the
    /// flag's guard has already dropped) without polling the flag this test is asserting on.
    #[test]
    fn a_panicked_scan_releases_the_scanning_flag_and_a_later_scan_still_starts() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let root = temp_dir("panicked-scan");
        write_nam(&root, "a.nam");
        let (service, _) = LibraryService::open(root.join("index.json"), vec![root.clone()]);

        let pool = ThreadPool::with_threads(1);
        service
            .start_scan(
                &pool,
                |_| panic!("a scan progress callback failing on purpose"),
                |_| {},
            )
            .expect("the first scan should start");

        let (drained_tx, drained_rx) = mpsc::channel();
        pool.spawn(move || drained_tx.send(()).unwrap());
        drained_rx
            .recv_timeout(SCAN_BUDGET)
            .expect("the pool must keep serving after a scan job panics");
        std::panic::set_hook(previous);

        assert!(
            !service.is_scanning(),
            "a panicked scan must not leave the scanning flag set"
        );

        let (tx, rx) = mpsc::channel();
        service
            .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
            .expect("a scan must still be startable after an earlier one panicked");
        recv(&rx);
        assert!(!service.is_scanning());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// D-12.2's cancellable job, exercised against `namir-fixtures`'s 10,000-file shared corpus —
    /// large enough that cancelling immediately after `start_scan` returns reliably lands before
    /// a full scan (which reads and hashes every file) could finish, unlike a two-file fixture
    /// where the race could go either way. `ScanOutcome::complete` must be `false`.
    #[test]
    fn cancelling_a_large_scan_stops_it_before_completion() {
        let corpus =
            namir_fixtures::library::generate_shared_corpus(1).expect("corpus should generate");
        let index_path = temp_dir("cancel").join("index.json");

        let (service, _) = LibraryService::open(index_path, vec![corpus.root.clone()]);
        let pool = ThreadPool::with_threads(1);
        let (tx, rx) = mpsc::channel();
        let handle = service
            .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
            .expect("no scan already running");

        handle.cancel();
        let outcome = recv(&rx);

        assert!(
            !outcome.complete,
            "cancelling right after start should stop a 10,000-file scan before it finishes"
        );
        assert_eq!(
            outcome.removed, 0,
            "an incomplete scan must never report removals"
        );
        assert!(!service.is_scanning());
    }

    /// FR-LIB-020's **"progress shall be visible"** clause, at the scale that requirement's own
    /// `*Verify:*` method names ("I with a synthetic library of at least 10 000 files"): a full,
    /// uncancelled scan of `namir-fixtures`' shared corpus must call `on_progress` **more than
    /// once**, so at least one of those calls came from [`LibraryService::start_scan`]'s cadence
    /// branch (`last_reported_at.elapsed() >= SCAN_PROGRESS_CADENCE`) rather than from the
    /// unconditional terminal report alone.
    ///
    /// **What this closes.** Before it, the cadence branch was asserted by no test at any scale.
    /// `a_scan_commits_found_files_to_the_snapshot_and_the_store` counts calls but asserts `>= 1`
    /// against a two-file fixture, and its own message says what that measures: the terminal report
    /// firing "even for a scan shorter than the cadence". The only >= 10 000-file test,
    /// `cancelling_a_large_scan_stops_it_before_completion`, passes `|_| {}`.
    ///
    /// **Why a new test rather than an assertion added to the cancel test.** That test cancels
    /// immediately after `start_scan` returns, so its loop can break before a single
    /// [`SCAN_PROGRESS_CADENCE`] window has elapsed; a `>= 2` assertion there would be flaky by
    /// construction rather than a check of anything.
    ///
    /// # Where FR-LIB-020's tag went (M9b)
    ///
    /// This test carried `// trace-partial: FR-LIB-020` from M9a until M9b, its `uncovered:` field
    /// naming the one clause it could not reach: *"shall occur off the audio thread"* was evidenced
    /// only by `tests/rt_stress.rs`'s axis C, whose corpus is six files. That axis was deliberately
    /// left at six (see `write_small_scan_corpus`'s own doc comment: it wants many fast scan cycles
    /// inside its run window, not one slow one), and the gap was closed instead by a new harness —
    /// `tests/library_scan_scale.rs`, which runs a live `AudioEngine` and a simulated 60 Hz UI
    /// thread across a full scan of this same corpus and spans all four of the requirement's
    /// clauses at the scale its `*Verify:*` method names. **That** test is now FR-LIB-020's traced
    /// artifact and carries the plain tag; this one keeps its own assertion, no longer tagged,
    /// because a `trace:` is a per-site claim about the whole requirement and this site only ever
    /// covered the progress clause.
    #[test]
    fn a_full_scan_of_the_shared_corpus_reports_progress_more_than_once() {
        let corpus =
            namir_fixtures::library::generate_shared_corpus(1).expect("corpus should generate");
        let index_path = temp_dir("progress").join("index.json");

        let (service, _) = LibraryService::open(index_path, vec![corpus.root.clone()]);
        let pool = ThreadPool::with_threads(1);
        let (tx, rx) = mpsc::channel();
        let progress_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = Arc::clone(&progress_calls);
        let handle = service
            .start_scan(
                &pool,
                move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                },
                move |outcome| tx.send(outcome).unwrap(),
            )
            .expect("no scan already running");

        // `FULL_CORPUS_SCAN_BUDGET`, not the module's ordinary `recv` -- see that constant's own
        // doc comment for why this one test needs a longer budget than every neighbour here.
        let outcome = recv_within(&rx, FULL_CORPUS_SCAN_BUDGET);
        drop(handle);

        assert!(
            outcome.complete,
            "this scan is never cancelled, so it must run every root to completion"
        );
        assert_eq!(
            outcome.upserted,
            namir_fixtures::library::TOTAL_COUNT,
            "a first scan of the shared corpus must upsert every file in it"
        );
        assert!(
            progress_calls.load(Ordering::SeqCst) >= 2,
            "a full {}-file scan reported progress {} time(s); at least one call must come from \
             the {SCAN_PROGRESS_CADENCE:?} cadence branch on top of the unconditional terminal \
             report, or \"progress shall be visible\" is true of the end of the scan only",
            namir_fixtures::library::TOTAL_COUNT,
            progress_calls.load(Ordering::SeqCst)
        );
    }

    // ---- M14 (§22 R-18): the index is off the instantiation path -------------------------------

    /// The new contract in one test: `open` returns no warnings because it has read nothing, and a
    /// corrupt index is still reported -- through [`LibraryService::take_load_warnings`], once the
    /// load it belongs to has actually happened.
    ///
    /// The old contract returned that warning from `open` itself, which is exactly what made `open`
    /// a parse.
    #[test]
    fn a_corrupt_index_is_reported_by_the_deferred_load_rather_than_by_open() {
        let dir = temp_dir("corrupt_deferred");
        let index_path = dir.join("library-index.json");
        std::fs::write(&index_path, b"{ this is not a library index").unwrap();

        let (service, warnings) = LibraryService::open(index_path, vec![dir.clone()]);
        assert!(
            warnings.is_empty(),
            "open reads nothing, so it can report nothing: {warnings:#?}"
        );

        service.ensure_loaded();
        let warnings = service.take_load_warnings();
        assert!(
            !warnings.is_empty(),
            "a corrupt index must still degrade with a warning rather than silently"
        );
        assert!(
            service.snapshot().is_empty(),
            "a corrupt index degrades to an empty one, not to whatever parsed"
        );
        assert!(
            service.take_load_warnings().is_empty(),
            "draining removes them, so a shell polling every frame reports each once"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cross-instance sharing, which is the half of R-18's fix that makes a *host* with ten plugin
    /// instances pay one parse rather than ten: a second service naming the same index file sees
    /// the first one's index, and a scan run through either is visible through both.
    ///
    /// Deterministic rather than timing-dependent in both directions: the second `open` returns the
    /// already-parsed shared entry synchronously (nothing is spawned when this process's parse is
    /// current), and the scan's result is published into that same shared slot before its
    /// `on_complete` callback fires.
    #[test]
    fn two_services_on_one_index_file_share_one_parsed_index() {
        let dir = temp_dir("shared_parse");
        let index_path = dir.join("library-index.json");
        let pool = ThreadPool::with_threads(1);

        let (first, _) = LibraryService::open(index_path.clone(), vec![dir.clone()]);
        first.ensure_loaded();
        write_nam(&dir, "a.nam");

        let (tx, rx) = mpsc::channel();
        first
            .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
            .unwrap();
        assert_eq!(recv(&rx).upserted, 1);

        // Opened *after* the scan and never loaded by anything: its index is the one the first
        // service's scan published, not a second parse and not an empty placeholder.
        let (second, _) = LibraryService::open(index_path, vec![dir.clone()]);
        assert_eq!(
            second.snapshot().len(),
            1,
            "a second instance must see the index this process already has"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The failure the deferral could have introduced, guarded at the seam that prevents it: a scan
    /// started before the load has landed must still take the *persisted* index as its `prior`.
    ///
    /// `start_scan`'s job blocks on `ensure_loaded` before reading `prior` precisely so that a
    /// complete walk cannot conclude, from a not-yet-loaded empty index, that everything is gone --
    /// the erase-the-shared-index failure `open_default`'s doc comment records from M6. The
    /// discriminator is `upserted`: against the real prior the unchanged file is not re-upserted,
    /// against an empty one it is.
    #[test]
    fn a_scan_started_before_the_load_lands_still_reads_the_persisted_index_as_its_prior() {
        let dir = temp_dir("prior_after_load");
        let index_path = dir.join("library-index.json");
        let pool = ThreadPool::with_threads(1);
        write_nam(&dir, "a.nam");
        // Aged well outside `namir-library`'s mtime settling window, for the same reason that
        // crate's own `an_unchanged_second_scan_upserts_nothing` ages its fixture: a file written
        // moments before a scan completes is re-hashed by design, which would mask the property
        // under test here.
        age_mtime(&dir.join("a.nam"), 3600);

        let (first, _) = LibraryService::open(index_path.clone(), vec![dir.clone()]);
        let (tx, rx) = mpsc::channel();
        first
            .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
            .unwrap();
        assert_eq!(recv(&rx).upserted, 1, "the first scan indexes the file");
        // Dropped so the process no longer holds this path's parsed index: the next open is a
        // genuine cold one, as a freshly-launched plugin instance would be.
        drop(first);

        let (second, _) = LibraryService::open(index_path, vec![dir.clone()]);
        let (tx, rx) = mpsc::channel();
        second
            .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
            .unwrap();
        let outcome = recv(&rx);
        assert!(outcome.complete);
        assert_eq!(
            outcome.upserted, 0,
            "an unchanged file must not be re-upserted -- the scan's prior was the empty \
             not-yet-loaded index rather than the persisted one"
        );
        assert_eq!(outcome.removed, 0);
        assert_eq!(second.snapshot().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Another process's rescan is still picked up: the cache is keyed on the file's modification
    /// time and length, so a changed file is re-parsed on the next open rather than served stale.
    /// Written here by re-saving through a second store, which is what another process's scan does.
    #[test]
    fn an_index_file_changed_underneath_this_process_is_re_read_on_the_next_open() {
        let dir = temp_dir("stale_cache");
        let index_path = dir.join("library-index.json");
        write_nam(&dir, "a.nam");

        let (first, _) = LibraryService::open(index_path.clone(), vec![dir.clone()]);
        first.ensure_loaded();
        assert!(first.snapshot().is_empty(), "nothing has been scanned yet");
        drop(first);

        // Stand in for another process: index the file through a service of its own, save, drop.
        {
            let pool = ThreadPool::with_threads(1);
            let (other, _) = LibraryService::open(index_path.clone(), vec![dir.clone()]);
            let (tx, rx) = mpsc::channel();
            other
                .start_scan(&pool, |_| {}, move |outcome| tx.send(outcome).unwrap())
                .unwrap();
            assert_eq!(recv(&rx).upserted, 1);
        }

        let (fresh, _) = LibraryService::open(index_path, vec![dir.clone()]);
        fresh.ensure_loaded();
        assert_eq!(
            fresh.snapshot().len(),
            1,
            "an index written since the last parse must be re-read, not served from the cache"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing index file (the ordinary first-run case) opens cleanly with no warnings and an
    /// empty snapshot -- `IndexStore::open`'s own P8 guarantee, carried through unchanged.
    #[test]
    fn opening_with_no_existing_index_file_yields_an_empty_snapshot() {
        let root = temp_dir("first_run");
        let (service, warnings) = LibraryService::open(root.join("index.json"), vec![root.clone()]);
        assert!(warnings.is_empty());
        assert!(service.snapshot().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
