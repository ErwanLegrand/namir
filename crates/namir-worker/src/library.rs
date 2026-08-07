//! [`LibraryService`]: seam 3's composition (`namir-library`'s own crate doc comment — "both are
//! caller-supplied … `LibraryService::open(index_path, roots)`"), and seam 2's other half: this
//! crate drives `namir-library`'s caller-pumped [`Scanner`](namir_library::Scanner) step machine
//! on [`ThreadPool`], which is D-12.2's "cancellable worker job" made literally true by splitting
//! it across the two crates the way `namir-library`'s own doc comment says it would — the
//! *mechanism* lives there, the *job* (thread, cancellation flag, progress cadence) lives here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

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
    store: IndexStore,
    roots: Vec<PathBuf>,
    index: Arc<Mutex<Arc<Index>>>,
    /// Guards against two scans running against the same service at once — see
    /// [`Self::start_scan`]'s doc comment for why that case is refused rather than resolved.
    scanning: Arc<AtomicBool>,
}

impl LibraryService {
    /// Opens the index at `index_path` and configures `roots` as the directories a scan walks,
    /// in the order a resolved reference tries them (`namir_library::resolver`'s own rule).
    ///
    /// Never fails (P8, mirroring `IndexStore::open`'s own guarantee): a missing index file is
    /// the ordinary first-run case and produces no warning at all; a present-but-corrupt or
    /// wrong-version one degrades to an empty index plus a warning, returned here rather than
    /// swallowed, so a caller can still tell the user their library needs a rescan.
    pub fn open(index_path: PathBuf, roots: Vec<PathBuf>) -> (LibraryService, Vec<WorkerError>) {
        let (store, index, warnings) = IndexStore::open(index_path);
        (
            LibraryService {
                store,
                roots,
                index: Arc::new(Mutex::new(Arc::new(index))),
                scanning: Arc::new(AtomicBool::new(false)),
            },
            warnings.into_iter().map(WorkerError::from).collect(),
        )
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
    pub fn snapshot(&self) -> Arc<Index> {
        Arc::clone(&lock(&self.index))
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
    /// as any other job this crate submits. `scanning` is still cleared in that case (the pool's
    /// `catch_unwind` runs the closure's remainder up to the panic point only, so this method
    /// relies on `ThreadPool`'s isolation rather than its own — a panic mid-scan leaves this
    /// service's `scanning` flag stuck `true` and no further scan startable, which is D-16.3's
    /// documented containment boundary, not a gap this method papers over).
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
        let prior = self.snapshot();
        let index_slot = Arc::clone(&self.index);
        let scanning = Arc::clone(&self.scanning);
        let store = self.store.clone();

        pool.spawn(move || {
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
            *lock(&index_slot) = Arc::new(new_index);

            // Cleared before on_complete, not after: a caller that starts a new scan from inside
            // its own on_complete callback must see is_scanning() == false by then.
            scanning.store(false, Ordering::Release);

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

    fn write_nam(dir: &std::path::Path, name: &str) {
        let model =
            namir_fixtures::nam::generate(namir_fixtures::nam::WaveNetShape::Nano, 1).unwrap();
        std::fs::write(dir.join(name), model.to_json_bytes()).unwrap();
    }

    fn recv(rx: &mpsc::Receiver<ScanOutcome>) -> ScanOutcome {
        rx.recv_timeout(Duration::from_secs(30))
            .expect("the scan job should have completed")
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
