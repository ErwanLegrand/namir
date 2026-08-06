//! [`Scanner`]: D-12.1's incremental scan, expressed as a caller-pumped step machine rather than
//! a function that runs to completion on its own thread.
//!
//! # Why a step machine (seam 2, resolved)
//!
//! D-12.2 calls scanning "a cancellable worker job", but D-5.1 forbids this crate from depending
//! on `namir-worker` at all — so "cancellable worker job" has to split. [`Scanner::step`] does at
//! most one unit of work (expand one directory, or examine one file) and returns
//! [`ScanProgress`]; cancellation is simply the caller not calling it again. `namir-worker`
//! (M5's `library.rs`) owns the thread, the cancellation flag, and the progress cadence, driving
//! this step machine on its existing pool. This crate needs no concurrency primitives at all and
//! never learns threads exist — D-12.2's "cancellable worker job" becomes literally true because
//! the *job* lives in `namir-worker`, exactly where D-5.1 puts it.
//!
//! # Incremental rule (D-12.1)
//!
//! A file whose `(size, mtime)` match the previous index's entry is left alone — not reopened,
//! not rehashed, not re-probed, and not added to [`ScanDelta::upserts`] at all (the caller's
//! [`crate::index::Index::apply`] simply leaves the pre-existing entry as-is). This is what makes
//! an unchanged 10,000-file rescan cheap (NFR-PERF-060): the only per-file cost for an unchanged
//! file is one comparison against already-known values, not a read.
//!
//! # Cancellation and removals
//!
//! Every file already examined this scan is valid and kept — discarding correctly hashed work on
//! cancellation would make cancelling pure waste. But [`ScanDelta::removals`] is only trustworthy
//! when [`ScanDelta::complete`] is `true`: a scan that did not see the whole tree cannot conclude
//! that a path it never reached is gone. Treating "not seen this run" as "deleted" on a cancelled
//! scan would silently empty a user's library, violating both P8 and FR-LIB-070's intent — so a
//! caller must check `complete` before acting on `removals`.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::entry::{FileTime, LibraryEntry, Origin};
use crate::error::LibraryWarning;
use crate::error_codes;
use crate::fs::{DirEntryInfo, ScanFs};
use crate::index::Index;
use crate::probe;

/// What [`Scanner::step`] has learned so far in this run — counts only, never a percentage:
/// `files_seen` grows as directories are expanded, so a percentage of an unknown total would
/// move backwards as the walk discovers more of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScanProgress {
    /// Directories discovered but not yet expanded.
    pub dirs_pending: usize,
    /// Files discovered (queued or already examined) so far.
    pub files_seen: usize,
    /// Files actually examined (queue popped and compared against the prior index) so far.
    pub files_examined: usize,
    /// Of `files_examined`, how many actually needed reading (new or changed) rather than being
    /// skipped by the incremental rule — the number that matters for NFR-PERF-060.
    pub files_hashed: usize,
}

/// One [`Scanner::step`] call's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// One unit of work was done; `step` again to continue.
    Progressed(ScanProgress),
    /// Every directory reachable from the configured roots has been expanded.
    Finished,
}

/// Everything a scan run learned, ready for [`crate::index::Index::apply`].
#[derive(Debug, Clone, Default)]
pub struct ScanDelta {
    /// New or changed entries to write into the index.
    pub upserts: Vec<LibraryEntry>,
    /// Only meaningful when [`Self::complete`] is `true` — see this module's doc comment.
    pub removals: Vec<PathBuf>,
    /// Non-fatal conditions encountered along the way.
    pub warnings: Vec<LibraryWarning>,
    /// `false` if the scan was cancelled (the caller stopped calling [`Scanner::step`]) before
    /// every directory was expanded.
    pub complete: bool,
}

impl Index {
    /// Applies a scan's findings: every upsert is written in, and — only when `delta.complete` —
    /// every path not seen this run is removed. Kept as an inherent method on `Index` (rather
    /// than a free function) since it is the one place `ScanDelta`'s `complete` flag is actually
    /// consulted; every other piece of code just carries the flag through.
    pub fn apply(&mut self, delta: ScanDelta) {
        for entry in delta.upserts {
            self.upsert(entry);
        }
        if delta.complete {
            for path in delta.removals {
                self.remove(&path);
            }
            // D-12.1's mtime-settling protection: this scan's completion time becomes the
            // baseline the *next* scan's Scanner::new reads back, via last_scan_completed_at().
            self.set_last_scan_completed_at(FileTime::now());
        }
    }
}

/// D-12.1's mtime-settling window (`docs/02-architecture.md` §12's M5 consequence note): a file
/// whose mtime lands within this much of the *previous* scan's completion time is rehashed
/// unconditionally, even if its `(size, mtime)` otherwise matches what's on record. NTFS's
/// documented resolution is 100 ns, but observed real-world granularity (buffered writes,
/// FAT-formatted removable volumes, network shares) runs from roughly one to two seconds — this
/// is set to the conservative end of that range rather than the optimistic one, since the cost of
/// a false positive (one unnecessary rehash) is far smaller than the cost of a false negative (a
/// genuine edit silently invisible to every future scan, not just the next one, since the stored
/// `(size, mtime)` would then match the *new* content and the file would look unchanged forever).
const MTIME_SETTLING_WINDOW_NANOS: i128 = 2_000_000_000;

/// The caller-pumped scan step machine. See this module's doc comment.
pub struct Scanner {
    pending_dirs: VecDeque<PathBuf>,
    pending_files: VecDeque<DirEntryInfo>,
    /// The previous index's `(size, mtime)` per path, consulted for the incremental rule. Built
    /// once from the `prior` snapshot passed to [`Self::new`] — this scanner never mutates the
    /// caller's index directly, it only reads from this copy.
    prior: std::collections::HashMap<PathBuf, (u64, FileTime)>,
    /// When the scan that produced `prior` finished, if known — the baseline
    /// [`MTIME_SETTLING_WINDOW_NANOS`] is measured against. `None` for the very first scan of a
    /// fresh index, in which case every file is genuinely new and the settling window has nothing
    /// to protect against.
    prior_scan_completed_at: Option<FileTime>,
    seen: HashSet<PathBuf>,
    delta: ScanDelta,
    files_examined: usize,
    files_hashed: usize,
}

impl Scanner {
    /// Seeds a scan over `roots` (typically several configured library directories), comparing
    /// against `prior`'s already-known `(size, mtime)` per path and its recorded
    /// [`Index::last_scan_completed_at`] (D-12.1's settling-window baseline).
    pub fn new(roots: Vec<PathBuf>, prior: &Index) -> Scanner {
        let prior_map = prior
            .iter()
            .map(|e| (e.path.clone(), (e.size, e.mtime)))
            .collect();
        Scanner {
            pending_dirs: roots.into_iter().collect(),
            pending_files: VecDeque::new(),
            prior: prior_map,
            prior_scan_completed_at: prior.last_scan_completed_at(),
            seen: HashSet::new(),
            delta: ScanDelta::default(),
            files_examined: 0,
            files_hashed: 0,
        }
    }

    /// Whether `mtime` falls close enough to the previous scan's completion time that it might
    /// belong to an edit this scan's `(size, mtime)` comparison alone cannot distinguish from "no
    /// change" — see [`MTIME_SETTLING_WINDOW_NANOS`].
    fn within_settling_window(&self, mtime: FileTime) -> bool {
        let Some(completed_at) = self.prior_scan_completed_at else {
            return false;
        };
        let delta = mtime.as_nanos_since_epoch() - completed_at.as_nanos_since_epoch();
        delta.abs() <= MTIME_SETTLING_WINDOW_NANOS
    }

    fn progress(&self) -> ScanProgress {
        ScanProgress {
            dirs_pending: self.pending_dirs.len(),
            files_seen: self.seen.len() + self.pending_files.len(),
            files_examined: self.files_examined,
            files_hashed: self.files_hashed,
        }
    }

    /// Does at most one unit of work — examine one already-listed file, or expand one directory
    /// — and returns the progress made. [`Step::Finished`] once every directory reachable from
    /// the configured roots has been expanded and every file within them examined.
    pub fn step(&mut self, fs: &dyn ScanFs) -> Step {
        if let Some(file) = self.pending_files.pop_front() {
            self.examine_file(fs, file);
            return Step::Progressed(self.progress());
        }
        if let Some(dir) = self.pending_dirs.pop_front() {
            self.expand_dir(fs, &dir);
            return Step::Progressed(self.progress());
        }
        self.delta.complete = true;
        Step::Finished
    }

    fn expand_dir(&mut self, fs: &dyn ScanFs, dir: &Path) {
        let entries = match fs.read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                self.delta
                    .warnings
                    .push(LibraryWarning::new(e.code, e.detail));
                return;
            }
        };
        for entry in entries {
            // Non-UTF-8 paths are not indexed. serde_json can only serialise a PathBuf that is
            // valid UTF-8 (store.rs's on-disk format is JSON text), and reconstructing an
            // OsString from arbitrary bytes needs platform-specific APIs D-5.2's cfg lint
            // reserves for namir-platform, which this crate may not depend on (D-5.1) -- so
            // rather than fail the whole scan on one oddly-named file, or panic later at save
            // time, this is skipped here, with the reason recorded rather than silently dropped.
            if entry.path.to_str().is_none() {
                self.delta.warnings.push(LibraryWarning::new(
                    error_codes::NON_UTF8_PATH,
                    format!("{}", dir.display()),
                ));
                continue;
            }
            if entry.is_dir {
                self.pending_dirs.push_back(entry.path.clone());
                continue;
            }
            if probe::kind_from_extension(&entry.path).is_some() {
                self.pending_files.push_back(entry);
            }
            // Files with an unrecognised extension are neither an error nor indexed -- FR-LIB-010
            // scans "for .nam and IR files", not every file in a directory.
        }
    }

    fn examine_file(&mut self, fs: &dyn ScanFs, info: DirEntryInfo) {
        self.files_examined += 1;
        self.seen.insert(info.path.clone());

        if let Some(&(prior_size, prior_mtime)) = self.prior.get(&info.path)
            && prior_size == info.size
            && prior_mtime == info.mtime
            && !self.within_settling_window(info.mtime)
        {
            // D-12.1's incremental rule: unchanged, so not reopened, not rehashed, not upserted.
            return;
        }

        self.files_hashed += 1;
        let Some(kind) = probe::kind_from_extension(&info.path) else {
            return; // Shouldn't happen -- expand_dir already filtered -- but never panic on it.
        };

        let bytes = match fs.read_file(&info.path, crate::MAX_INDEXED_FILE_BYTES) {
            Ok(bytes) => bytes,
            Err(e) if e.code.id == error_codes::FILE_TOO_LARGE.id => {
                // NFR-SEC-020: still indexed and browsable, just without a hash or metadata.
                self.delta
                    .warnings
                    .push(LibraryWarning::new(e.code, e.detail));
                self.delta.upserts.push(LibraryEntry {
                    path: info.path,
                    kind,
                    size: info.size,
                    mtime: info.mtime,
                    hash: None,
                    metadata: crate::entry::ItemMetadata::None,
                    origin: Origin::Local,
                });
                return;
            }
            Err(e) => {
                // Vanished or became unreadable between listing and reading -- FR-LIB-070's own
                // race, tolerated: warn, skip, keep scanning.
                self.delta
                    .warnings
                    .push(LibraryWarning::new(e.code, e.detail));
                return;
            }
        };

        let hash = namir_core::ContentHash::of(&bytes);
        let metadata = probe::probe(&bytes, kind);
        self.delta.upserts.push(LibraryEntry {
            path: info.path,
            kind,
            size: info.size,
            mtime: info.mtime,
            hash: Some(hash),
            metadata,
            origin: Origin::Local,
        });
    }

    /// Consumes the scanner, returning everything learned. Callable after cancellation (some
    /// steps run, then the caller simply stops calling [`Self::step`]) — `delta.complete` will be
    /// `false` in that case, and `delta.removals` is empty regardless (removals are only
    /// computed once `Step::Finished` is reached, at which point `take_delta` fills them in from
    /// `prior`'s paths that were never `seen`).
    pub fn take_delta(mut self) -> ScanDelta {
        if self.delta.complete {
            self.delta.removals = self
                .prior
                .keys()
                .filter(|p| !self.seen.contains(*p))
                .cloned()
                .collect();
        }
        self.delta
    }

    /// Runs [`Self::step`] to completion in one call — a convenience for tests and any caller
    /// that genuinely doesn't need cancellability (the production path, `namir-worker`'s
    /// `library.rs`, always calls [`Self::step`] itself instead).
    pub fn run_to_completion(mut self, fs: &dyn ScanFs) -> ScanDelta {
        while let Step::Progressed(_) = self.step(fs) {}
        self.take_delta()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::StdFs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-library-scan-test-{name}-{}",
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

    fn write_wav(dir: &std::path::Path, name: &str, seed: u64) {
        let samples = namir_fixtures::ir::decaying_noise(64, seed, 20.0);
        let bytes = namir_fixtures::ir::to_mono_wav_bytes(&samples, 48_000);
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    /// Backdates `path`'s mtime, via `std::fs::File::set_modified` (stable, no extra dependency)
    /// — used to get a test's fixture files outside D-12.1's settling window without waiting for
    /// real wall-clock time to pass.
    fn age_mtime(path: &std::path::Path, seconds_ago: u64) {
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(seconds_ago);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(past)
            .unwrap();
    }

    /// FR-LIB-010: a recursive scan finds `.nam` and IR files under nested directories.
    #[test]
    fn a_full_scan_finds_every_file_under_nested_directories() {
        let root = temp_dir("full_scan");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        write_nam(&root, "a.nam");
        write_wav(&root, "b.wav", 1);
        write_wav(&root.join("sub"), "c.wav", 2);
        std::fs::write(root.join("ignored.txt"), b"not indexed").unwrap();

        let delta = Scanner::new(vec![root.clone()], &Index::empty()).run_to_completion(&StdFs);
        assert!(delta.complete);
        assert_eq!(delta.upserts.len(), 3);
        assert!(delta.warnings.is_empty());

        let mut index = Index::empty();
        index.apply(delta);
        assert_eq!(index.len(), 3);
        assert!(index.get(&root.join("ignored.txt")).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scanning_multiple_roots_covers_all_of_them() {
        let root_a = temp_dir("multi_root_a");
        let root_b = temp_dir("multi_root_b");
        write_nam(&root_a, "a.nam");
        write_wav(&root_b, "b.wav", 1);

        let delta = Scanner::new(vec![root_a.clone(), root_b.clone()], &Index::empty())
            .run_to_completion(&StdFs);
        assert_eq!(delta.upserts.len(), 2);

        let _ = std::fs::remove_dir_all(&root_a);
        let _ = std::fs::remove_dir_all(&root_b);
    }

    /// D-12.1's incremental rule, demonstrated directly: a second scan of an unchanged tree
    /// upserts nothing. Ages the fixture file's mtime well outside D-12.1's settling window
    /// (`MTIME_SETTLING_WINDOW_NANOS`) first — realistic (a real library's files almost never
    /// have an mtime coincidentally close to "whenever the last scan happened to finish"), and
    /// necessary: without ageing, `write_nam`'s freshly-written mtime and this test's own
    /// back-to-back scans would both land inside the settling window by construction, which is
    /// exactly the ambiguous case the window is *supposed* to treat as suspect (see the test
    /// below this one) — this test is about the *un*ambiguous case.
    #[test]
    fn an_unchanged_second_scan_upserts_nothing() {
        let root = temp_dir("unchanged");
        write_nam(&root, "a.nam");
        age_mtime(&root.join("a.nam"), 3600);

        let mut index = Index::empty();
        index.apply(Scanner::new(vec![root.clone()], &index.clone()).run_to_completion(&StdFs));
        assert_eq!(index.len(), 1);

        let second_delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&StdFs);
        assert!(
            second_delta.upserts.is_empty(),
            "unchanged file must not be re-upserted"
        );
        assert_eq!(second_delta.warnings.len(), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// FR-LIB-070: a file removed between scans is reflected as a removal, when the scan
    /// completed.
    #[test]
    fn a_deleted_file_is_reported_as_a_removal_on_a_complete_scan() {
        let root = temp_dir("deleted");
        write_nam(&root, "a.nam");
        write_nam(&root, "b.nam");

        let mut index = Index::empty();
        index.apply(Scanner::new(vec![root.clone()], &index.clone()).run_to_completion(&StdFs));
        assert_eq!(index.len(), 2);

        std::fs::remove_file(root.join("b.nam")).unwrap();
        let delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&StdFs);
        assert!(delta.complete);
        assert_eq!(delta.removals, vec![root.join("b.nam")]);

        index.apply(delta);
        assert_eq!(index.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The load-bearing safety property: a cancelled (incomplete) scan must never report
    /// removals, or applying it would silently empty entries the scan simply never got to.
    #[test]
    fn a_cancelled_scan_reports_no_removals_even_if_files_are_unseen() {
        let root = temp_dir("cancelled");
        write_nam(&root, "a.nam");
        write_nam(&root, "b.nam");

        let mut index = Index::empty();
        index.apply(Scanner::new(vec![root.clone()], &index.clone()).run_to_completion(&StdFs));

        // Simulate cancellation: build a scanner and take its delta without ever calling step(),
        // i.e. the caller stopped before Step::Finished.
        let scanner = Scanner::new(vec![root.clone()], &index);
        let delta = scanner.take_delta();
        assert!(!delta.complete);
        assert!(delta.removals.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn step_does_at_most_one_unit_of_work_per_call() {
        let root = temp_dir("stepwise");
        write_nam(&root, "a.nam");
        write_nam(&root, "b.nam");

        let mut scanner = Scanner::new(vec![root.clone()], &Index::empty());
        // Step 1: expands the root directory (one dir), queues two files -- no file examined yet.
        assert!(matches!(scanner.step(&StdFs), Step::Progressed(_)));
        assert_eq!(scanner.progress().files_examined, 0);
        // Step 2 and 3: one file examined each.
        assert!(matches!(scanner.step(&StdFs), Step::Progressed(_)));
        assert_eq!(scanner.progress().files_examined, 1);
        assert!(matches!(scanner.step(&StdFs), Step::Progressed(_)));
        assert_eq!(scanner.progress().files_examined, 2);
        // Step 4: nothing left.
        assert_eq!(scanner.step(&StdFs), Step::Finished);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// NFR-SEC-020: a file whose *claimed* size (from the directory listing, before any read
    /// happens) exceeds [`crate::MAX_INDEXED_FILE_BYTES`] is still indexed — browsable by path —
    /// but with no hash and no extracted metadata, and a warning is recorded. Uses [`FakeFs`] to
    /// claim an oversized size cheaply, without writing an actual multi-hundred-MB fixture.
    #[test]
    fn a_file_over_the_size_ceiling_is_indexed_without_a_hash() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");
        let mut fake = FakeFs::new();
        fake.add_file(
            &root,
            "huge.nam",
            crate::MAX_INDEXED_FILE_BYTES as u64 + 1,
            FileTime::now(),
            b"irrelevant, never read".to_vec(),
        );

        let delta = Scanner::new(vec![root], &Index::empty()).run_to_completion(&fake);
        assert_eq!(delta.upserts.len(), 1);
        assert_eq!(delta.upserts[0].hash, None);
        assert!(matches!(
            delta.upserts[0].metadata,
            crate::entry::ItemMetadata::None
        ));
        assert_eq!(delta.warnings.len(), 1);
        assert_eq!(delta.warnings[0].code.id, error_codes::FILE_TOO_LARGE.id);
    }

    #[test]
    fn an_unreadable_file_is_warned_about_and_skipped_not_fatal() {
        let root = temp_dir("vanishing");
        write_nam(&root, "a.nam");
        // Delete it out from under a freshly-built pending-file entry to simulate FR-LIB-070's
        // disappearing-file race.
        let mut scanner = Scanner::new(vec![root.clone()], &Index::empty());
        let _ = scanner.step(&StdFs); // expands the directory, queues a.nam
        std::fs::remove_file(root.join("a.nam")).unwrap();
        let _ = scanner.step(&StdFs); // attempts to read the now-missing file

        let delta = scanner.take_delta();
        assert!(delta.upserts.is_empty());
        assert_eq!(delta.warnings.len(), 1);
        assert_eq!(delta.warnings[0].code.id, error_codes::FILE_UNREADABLE.id);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// D-12.1's corrected change-detection rule (`docs/02-architecture.md` §12's M5 consequence
    /// note on D-12.1): a file edited in place to the *same length*, whose new mtime happens to
    /// coincide with the mtime already on record (a real risk on a filesystem with coarser mtime
    /// granularity than "however fast an editor can save after a scan finishes"), must still be
    /// reflected within one rescan (FR-LIB-070). The literal `size == prior_size && mtime ==
    /// prior_mtime` comparison cannot see this edit at all -- both fields agree with what was
    /// recorded before. Using `FakeFs` to construct the coincidence directly and deterministically,
    /// rather than trying to race real OS mtime granularity.
    #[test]
    fn a_same_length_edit_whose_mtime_collides_with_the_prior_scan_is_still_detected() {
        use crate::fs::FakeFs;

        let root = PathBuf::from("/fake/root");
        let path = root.join("a.nam");
        let shared_mtime = FileTime::now();

        // First scan: content C1, at shared_mtime.
        let mut fake = FakeFs::new();
        fake.add_file(&root, "a.nam", 100, shared_mtime, vec![1u8; 100]);
        let mut index = Index::empty();
        index.apply(Scanner::new(vec![root.clone()], &index.clone()).run_to_completion(&fake));
        let first_hash = index.get(&path).unwrap().hash;

        // Second scan: content C2 (same length, different bytes), but the filesystem reports the
        // *identical* mtime -- the coincidence D-12.1's literal rule cannot see through. Without
        // this test's fix, this file would be skipped entirely: size and mtime both "match".
        let mut fake2 = FakeFs::new();
        fake2.add_file(&root, "a.nam", 100, shared_mtime, vec![2u8; 100]);
        let delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&fake2);

        assert_eq!(
            delta.upserts.len(),
            1,
            "a same-length edit whose mtime collides with the prior scan must still be rehashed"
        );
        assert_ne!(
            delta.upserts[0].hash, first_hash,
            "the new content's hash must be recorded"
        );
    }
}
