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
//!
//! **`complete` is not the whole of that rule (issue #65).** It says the queue drained, which is
//! not the same as "the tree was walked": a directory the scan was *refused* — an offline volume,
//! an ACL-restricted folder, a listing that ended early — leaves the walk able to reach
//! `Step::Finished` with a whole subtree never looked at. Concluding removals from that erases
//! every entry under it, on every scan, permanently; with a single root that is the entire index,
//! which is the same silent-erasure failure `LibraryService::open_default`'s zero-roots bug
//! caused through a different door. So a directory that could not be listed, or could not be
//! listed to the end, records its path in [`ScanDelta::unreadable_prefixes`], and
//! [`Scanner::take_delta`] excludes everything beneath those prefixes from `removals`. The scan
//! is still `complete` — the rest of the tree *was* walked, and a file genuinely deleted
//! elsewhere is still reported — but the part nobody could see degrades to "keep what we had and
//! warn" rather than "assume it is gone".

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
    /// Directories this scan could not see inside (issue #65): unlistable, listed only in part,
    /// or a symlink whose target could not be resolved. Nothing under one of these paths appears
    /// in [`Self::removals`], however `complete` this run was — see this module's doc comment.
    pub unreadable_prefixes: Vec<PathBuf>,
    /// When this scan *started*, for D-12.1's mtime-settling baseline (issue #67). `None` only on
    /// a hand-built delta; [`Scanner::take_delta`] always fills it in.
    pub scan_started_at: Option<FileTime>,
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
            // D-12.1's mtime-settling protection: this scan's *start* time becomes the baseline
            // the next scan's Scanner::new reads back, via last_scan_started_at() — issue #67,
            // and see MTIME_SETTLING_WINDOW_NANOS for why the start and not the finish.
            self.set_last_scan_started_at(delta.scan_started_at.unwrap_or_else(FileTime::now));
        }
    }
}

/// D-12.1's mtime-settling window (`docs/02-architecture.md` §12's M5 consequence note): a file
/// whose mtime is no older than this much before the *previous* scan began is rehashed
/// unconditionally, even if its `(size, mtime)` otherwise matches what's on record. NTFS's
/// documented resolution is 100 ns, but observed real-world granularity (buffered writes,
/// FAT-formatted removable volumes, network shares) runs from roughly one to two seconds — this
/// is set to the conservative end of that range rather than the optimistic one, since the cost of
/// a false positive (one unnecessary rehash) is far smaller than the cost of a false negative (a
/// genuine edit silently invisible to every future scan, not just the next one, since the stored
/// `(size, mtime)` would then match the *new* content and the file would look unchanged forever).
///
/// # Why the previous scan's *start*, and why one-sided (issue #67)
///
/// The ambiguity this window exists to cover is around each file's own **examination**, not
/// around the moment the scan happened to finish. A file examined at `t` and edited moments later
/// can be reported with the same mtime it already had — that is the whole false negative — and
/// `t` is anywhere inside the previous scan, which on a cold library is far longer than two
/// seconds. Anchored to completion, only the handful of files examined in the last two seconds of
/// a scan were ever protected; every file examined earlier fell outside the window by exactly the
/// amount of scan that came after it.
///
/// Recording the previous scan's start time and asking `mtime >= start - WINDOW` covers every
/// examination time in that scan, since `start <= t` for all of them. It is one-sided for the
/// same reason: an mtime *after* the previous scan is a file written during or since that scan
/// and is exactly as suspect, whereas an mtime comfortably older than the scan's start cannot
/// have been overwritten inside its own granularity by an edit that came after it was examined.
/// The set of files this rehashes shrinks to nothing as the baseline advances — a file untouched
/// since a scan or two ago falls out of it and stays out.
const MTIME_SETTLING_WINDOW_NANOS: i128 = 2_000_000_000;

/// The caller-pumped scan step machine. See this module's doc comment.
pub struct Scanner {
    pending_dirs: VecDeque<PathBuf>,
    /// Directory symlinks discovered but not yet expanded, held apart from [`Self::pending_dirs`]
    /// and drained only once that queue is empty — issue #73's second half. Both spellings of one
    /// directory cannot be walked (see [`Self::visited_dirs`]), so *which* one survives is a real
    /// choice, and making it here rather than leaving it to `read_dir`'s unspecified order is
    /// what makes it the user's actual folder every time rather than whichever name the
    /// filesystem happened to hand over first.
    pending_links: VecDeque<PathBuf>,
    pending_files: VecDeque<DirEntryInfo>,
    /// The previous index's `(size, mtime)` per path, consulted for the incremental rule. Built
    /// once from the `prior` snapshot passed to [`Self::new`] — this scanner never mutates the
    /// caller's index directly, it only reads from this copy.
    prior: std::collections::HashMap<PathBuf, (u64, FileTime)>,
    /// When the scan that produced `prior` **started**, if known — the baseline
    /// [`MTIME_SETTLING_WINDOW_NANOS`] is measured against. `None` for the very first scan of a
    /// fresh index, in which case every file is genuinely new and the settling window has nothing
    /// to protect against.
    prior_scan_started_at: Option<FileTime>,
    /// When *this* scan started, recorded at construction so it survives into the delta and
    /// becomes the next scan's baseline (issue #67).
    started_at: FileTime,
    seen: HashSet<PathBuf>,
    /// The canonical path of every directory this scan has expanded — issue #73's cycle *and*
    /// duplicate guard. Consulted at the moment a queued entry is expanded, whatever queued it, so
    /// one directory is walked at most once however many spellings of it the tree contains: a link
    /// that leads back into the tree terminates instead of recursing forever, and a link that
    /// merely names a directory the walk already covered (`Library/Favourites` -> `Library/Amps`,
    /// an entirely ordinary setup) contributes no second copy of every file underneath it.
    ///
    /// Checking here rather than at the point a link is *resolved* is what makes the second half
    /// true. A link is resolved while its parent's listing is being read; a plain directory is
    /// only queued then, and canonicalised later — so a sibling link was always resolved before
    /// its own target had been expanded, found the target unvisited, and was followed, leaving
    /// both spellings walked in either listing order.
    visited_dirs: HashSet<PathBuf>,
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
            pending_links: VecDeque::new(),
            pending_files: VecDeque::new(),
            prior: prior_map,
            prior_scan_started_at: prior.last_scan_started_at(),
            started_at: FileTime::now(),
            seen: HashSet::new(),
            visited_dirs: HashSet::new(),
            delta: ScanDelta::default(),
            files_examined: 0,
            files_hashed: 0,
        }
    }

    /// Whether `mtime` is recent enough, relative to when the previous scan *began*, that it
    /// might belong to an edit this scan's `(size, mtime)` comparison alone cannot distinguish
    /// from "no change" — see [`MTIME_SETTLING_WINDOW_NANOS`].
    fn within_settling_window(&self, mtime: FileTime) -> bool {
        let Some(started_at) = self.prior_scan_started_at else {
            return false;
        };
        mtime.as_nanos_since_epoch()
            >= started_at.as_nanos_since_epoch() - MTIME_SETTLING_WINDOW_NANOS
    }

    fn progress(&self) -> ScanProgress {
        ScanProgress {
            dirs_pending: self.pending_dirs.len() + self.pending_links.len(),
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
        // Only once no plain directory is left anywhere: see `pending_links`.
        if let Some(link) = self.pending_links.pop_front() {
            self.expand_link(fs, &link);
            return Step::Progressed(self.progress());
        }
        self.delta.complete = true;
        Step::Finished
    }

    /// Expands one plain directory: the visited-set claim first, then the listing.
    ///
    /// The claim is made here, at expansion, rather than where a link is resolved — see
    /// [`Self::visited_dirs`] for why that distinction is the whole of issue #73's duplicate bug.
    /// A directory that cannot be canonicalised claims nothing and is listed anyway: the guard
    /// degrades to "walk it", which still terminates, rather than to "skip it".
    ///
    /// A plain directory losing the claim means the scan reached one directory by two names
    /// without a symlink of its own in the way — overlapping roots, essentially — so nothing is
    /// warned about; it is the same tree, already walked. The prefix is still recorded, because
    /// the files under it were `seen` under the *other* spelling and their absence under this one
    /// is not evidence that anything was deleted.
    fn expand_dir(&mut self, fs: &dyn ScanFs, dir: &Path) {
        if let Ok(canonical) = fs.canonical_dir(dir)
            && !self.visited_dirs.insert(canonical)
        {
            self.delta.unreadable_prefixes.push(dir.to_path_buf());
            return;
        }
        self.list_children(fs, dir);
    }

    /// Expands one directory symlink, popped from [`Self::pending_links`] after every plain
    /// directory has been expanded.
    ///
    /// Issue #73: that a symlink is followed at all was never a decision, only a side effect of
    /// asking `file_type()` (which does not follow links) and nothing else — and its cost was
    /// never recorded: a user who symlinks a model collection into the library root saw an empty
    /// library and no diagnostic at all, which is an entirely ordinary setup on Linux and macOS.
    /// Following it makes that setup work; [`Self::visited_dirs`] is what replaces the
    /// loop-safety the old shape got for free, so a link that points at an ancestor, at a sibling
    /// that points back, or at itself is recognised and skipped rather than recursed into.
    ///
    /// Same claim as [`Self::expand_dir`] makes, then, with the two
    /// outcomes a link has that a directory does not: a target that cannot be resolved at all,
    /// and a target some other spelling already covered — which, links being expanded last, is
    /// always genuinely this link being the redundant name for a directory the user has, so
    /// `SYMLINK_NOT_FOLLOWED` is true of it.
    fn expand_link(&mut self, fs: &dyn ScanFs, link: &Path) {
        let canonical = match fs.canonical_dir(link) {
            Ok(canonical) => canonical,
            Err(e) => {
                self.delta
                    .warnings
                    .push(LibraryWarning::new(e.code, e.detail));
                self.delta.unreadable_prefixes.push(link.to_path_buf());
                return;
            }
        };
        if !self.visited_dirs.insert(canonical) {
            self.delta.warnings.push(LibraryWarning::new(
                error_codes::SYMLINK_NOT_FOLLOWED,
                format!("{}", link.display()),
            ));
            // Whatever was indexed under this spelling on an earlier scan is still on disk; this
            // scan simply reached it by another name. Not a removal.
            self.delta.unreadable_prefixes.push(link.to_path_buf());
            return;
        }
        self.list_children(fs, link);
    }

    /// The listing itself, shared by [`Self::expand_dir`] and [`Self::expand_link`] — by this
    /// point the directory's claim on [`Self::visited_dirs`] has been made and won, so this is
    /// only ever "read one directory and queue what is in it".
    fn list_children(&mut self, fs: &dyn ScanFs, dir: &Path) {
        let listing = match fs.read_dir(dir) {
            Ok(listing) => listing,
            Err(e) => {
                // Issue #65: nobody looked inside, so nothing inside may be inferred to be gone.
                self.delta
                    .warnings
                    .push(LibraryWarning::new(e.code, e.detail));
                self.delta.unreadable_prefixes.push(dir.to_path_buf());
                return;
            }
        };
        self.delta.warnings.extend(listing.warnings);
        // Issue #66: a child that could not be described is skipped, not fatal -- but it was
        // *seen*, so whatever the index already knows about it stays. Only a path nobody
        // encountered is a removal.
        for path in listing.unreadable_entries {
            self.seen.insert(path);
        }
        if !listing.fully_enumerated {
            // A listing that ended early is not evidence about the children it never reached.
            self.delta.unreadable_prefixes.push(dir.to_path_buf());
        }
        for entry in listing.entries {
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
            if entry.is_dir_symlink {
                // Queued, not resolved: the visited-set claim belongs at expansion time, and
                // deferring it behind every plain directory is what makes the real folder rather
                // than the link the spelling that survives (issue #73).
                self.pending_links.push_back(entry.path.clone());
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
        self.delta.scan_started_at = Some(self.started_at);
        if self.delta.complete {
            let unreadable = std::mem::take(&mut self.delta.unreadable_prefixes);
            self.delta.removals = self
                .prior
                .keys()
                .filter(|p| !self.seen.contains(*p))
                // Issue #65: a path under a directory this scan could not see inside was never
                // looked for, so its absence from `seen` is not evidence of anything.
                .filter(|p| !unreadable.iter().any(|prefix| p.starts_with(prefix)))
                .cloned()
                .collect();
            self.delta.unreadable_prefixes = unreadable;
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
        write_nam_seeded(dir, name, 1);
    }

    /// `write_nam` with the fixture seed chosen by the caller — two different seeds give two
    /// different models, hence two different content hashes, which is what FR-LIB-070's "files
    /// that change" needs in order to be distinguishable from "files that were re-listed".
    fn write_nam_seeded(dir: &std::path::Path, name: &str, seed: u64) {
        let model =
            namir_fixtures::nam::generate(namir_fixtures::nam::WaveNetShape::Nano, seed).unwrap();
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
    // trace-partial: FR-LIB-010
    // uncovered: FR-LIB-010 — the "the user shall be able to nominate one or more directories as
    // uncovered: library roots" clause has no mechanism to exercise: both shells open through
    // uncovered: LibraryService::open_at/open_default, which hard-code the single root
    // uncovered: <config_dir>/Library, AppSettings has no roots field and UiIntent has no add-root
    // uncovered: or remove-root variant; closes M8
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

    /// FR-LIB-070's "files that disappear", in isolation: a file removed between scans is
    /// reflected as a removal, when the scan completed. The requirement's whole set is spanned
    /// by `one_rescan_reflects_disappeared_changed_and_added_files_and_survives_a_vanishing_one`
    /// below, which carries the tag.
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
    // trace: NFR-SEC-020
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

    /// FR-LIB-070's "a missing file shall never crash Namir", in isolation; the requirement's
    /// whole set is spanned by the tagged rescan test below.
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

    /// FR-LIB-070 in full, in a single rescan. The requirement's own sentence enumerates three
    /// mutations — files that **disappear**, **change** or **are added** while Namir is running —
    /// and adds that a missing file shall never crash Namir or the host. The three tests around
    /// this one each pin one member in isolation (deletion above, the same-length edit below, the
    /// vanish-mid-read race in between); none of them spans the set, and "are added" was spanned
    /// by nothing at all before this test existed. Here all four conditions are applied at once,
    /// between one completed scan and the next, and the *single* following rescan must reflect
    /// every one of them.
    ///
    /// The vanishing file is made deterministic rather than raced: the scanner's first `step`
    /// expands the root and queues every file, so deleting one between that step and the
    /// file-examining steps that follow guarantees the read finds it missing — inside this same
    /// rescan, so the no-crash clause is exercised on a scan that must still get the other three
    /// members right.
    // trace: FR-LIB-070
    #[test]
    fn one_rescan_reflects_disappeared_changed_and_added_files_and_survives_a_vanishing_one() {
        let root = temp_dir("mutations");
        write_nam(&root, "gone.nam");
        write_nam_seeded(&root, "changed.nam", 2);
        write_nam_seeded(&root, "stable.nam", 3);
        // Outside D-12.1's settling window, so an unchanged file is genuinely recognised as
        // unchanged rather than rehashed for safety (which would make the "stable" control
        // prove nothing).
        for name in ["gone.nam", "changed.nam", "stable.nam"] {
            age_mtime(&root.join(name), 3600);
        }

        let mut index = Index::empty();
        index.apply(Scanner::new(vec![root.clone()], &index.clone()).run_to_completion(&StdFs));
        assert_eq!(index.len(), 3);
        let changed_path = root.join("changed.nam");
        let stable_path = root.join("stable.nam");
        let changed_hash_before = index.get(&changed_path).unwrap().hash;
        let stable_hash_before = index.get(&stable_path).unwrap().hash;

        // Disappears; changes (different model, so a different content hash); is added.
        std::fs::remove_file(root.join("gone.nam")).unwrap();
        write_nam_seeded(&root, "changed.nam", 4);
        write_nam_seeded(&root, "added.nam", 5);
        // And one more that exists when the directory is listed and is gone by the time the
        // scanner tries to read it.
        write_nam_seeded(&root, "racing.nam", 6);

        let mut scanner = Scanner::new(vec![root.clone()], &index);
        assert!(matches!(scanner.step(&StdFs), Step::Progressed(_))); // expands the root
        std::fs::remove_file(root.join("racing.nam")).unwrap();
        while let Step::Progressed(_) = scanner.step(&StdFs) {}
        let delta = scanner.take_delta();

        assert!(delta.complete);
        // Disappeared.
        assert_eq!(delta.removals, vec![root.join("gone.nam")]);
        // Changed and added -- and only those two: the untouched file is not re-upserted, and
        // the file that vanished before it could be read contributes no entry.
        let mut upserted: Vec<PathBuf> = delta.upserts.iter().map(|e| e.path.clone()).collect();
        upserted.sort();
        assert_eq!(upserted, vec![root.join("added.nam"), changed_path.clone()]);
        // Never crashed on the missing file: the scan ran to completion and recorded the miss as
        // one warning rather than a panic or an aborted scan.
        assert_eq!(delta.warnings.len(), 1);
        assert_eq!(delta.warnings[0].code.id, error_codes::FILE_UNREADABLE.id);

        index.apply(delta);
        let mut paths: Vec<PathBuf> = index.iter().map(|e| e.path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                root.join("added.nam"),
                changed_path.clone(),
                stable_path.clone()
            ]
        );
        assert_ne!(
            index.get(&changed_path).unwrap().hash,
            changed_hash_before,
            "a changed file must be reflected as new content, not just re-listed"
        );
        assert_eq!(index.get(&stable_path).unwrap().hash, stable_hash_before);

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
    /// **Issue #65:** a directory that cannot be listed must not let the scan conclude that
    /// everything under it is gone.
    ///
    /// `complete` used to mean only that the queue drained. An ACL-restricted folder was warned
    /// about and skipped, the walk still reached `Step::Finished`, and every path under it became
    /// a removal — on every scan, so those entries were dropped and never came back.
    #[test]
    fn an_unreadable_directory_does_not_erase_its_subtree() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");
        let locked = root.join("locked");

        // A prior index holding one entry inside the directory that will fail to list.
        let mut index = Index::empty();
        index.upsert(LibraryEntry {
            path: locked.join("a.nam"),
            kind: crate::entry::ItemKind::Nam,
            size: 10,
            mtime: FileTime::now(),
            hash: None,
            metadata: crate::entry::ItemMetadata::None,
            origin: Origin::Local,
        });

        // The root lists `locked` as a directory, but `locked` itself is not registered in the
        // fake -- read_dir on it fails, exactly as an ACL-restricted folder does.
        let mut fake = FakeFs::new();
        fake.add_unlistable_dir(&root, "locked");

        let delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&fake);
        assert_eq!(
            delta.warnings.len(),
            1,
            "the unreadable directory must be reported"
        );
        assert!(
            delta.removals.is_empty(),
            "a directory that could not be listed must not make its contents look deleted: {:?}",
            delta.removals
        );

        index.apply(delta);
        assert_eq!(index.len(), 1, "the subtree must survive");
    }
    /// **Issue #65, the whole-index form.** The same defect with a single root — the shape both
    /// product shells actually run, since `LibraryService::open_at` hard-codes one — is the
    /// historical zero-roots erasure through another door: an offline volume or a permissions
    /// change makes `read_dir` on the sole root fail, and a `complete` scan then reports the
    /// *entire* index as removals, which is saved over the shared index file.
    #[test]
    fn an_unreadable_sole_root_never_reports_the_whole_index_as_removals() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");

        let mut index = Index::empty();
        for name in ["a.nam", "b.nam", "c.wav"] {
            index.upsert(LibraryEntry {
                path: root.join(name),
                kind: crate::entry::ItemKind::Nam,
                size: 10,
                mtime: FileTime::now(),
                hash: None,
                metadata: crate::entry::ItemMetadata::None,
                origin: Origin::Local,
            });
        }

        // Nothing registered at all: read_dir on the root itself fails.
        let delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&FakeFs::new());
        assert_eq!(delta.warnings.len(), 1);
        assert_eq!(delta.unreadable_prefixes, vec![root.clone()]);
        assert!(
            delta.removals.is_empty(),
            "a root that could not be listed must not empty the index: {:?}",
            delta.removals
        );

        index.apply(delta);
        assert_eq!(index.len(), 3, "the index must survive an unreadable root");
    }

    /// **Issue #66:** one child that cannot be described is not a reason to lose the directory.
    ///
    /// The port used to propagate a per-entry failure out of `read_dir` with `?`, so one locked
    /// file, reparse point or cloud placeholder made the whole directory unlistable — and, via
    /// issue #65's `complete`, turned every indexed sibling into a removal. The child itself must
    /// not be inferred away either: it was seen, it just could not be described.
    ///
    /// A genuinely deleted third file is still reported, so this is a precise degradation rather
    /// than "warn once and stop concluding anything".
    #[test]
    fn one_undescribable_child_costs_neither_its_siblings_nor_itself() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");

        let mut index = Index::empty();
        for name in ["kept.nam", "locked.nam", "gone.nam"] {
            index.upsert(LibraryEntry {
                path: root.join(name),
                kind: crate::entry::ItemKind::Nam,
                size: 1,
                mtime: FileTime::from_system_time(std::time::UNIX_EPOCH),
                hash: None,
                metadata: crate::entry::ItemMetadata::None,
                origin: Origin::Local,
            });
        }

        let mut fake = FakeFs::new();
        fake.add_file(
            &root,
            "kept.nam",
            1,
            FileTime::from_system_time(std::time::UNIX_EPOCH),
            b"x".to_vec(),
        );
        fake.add_unreadable_entry(&root, "locked.nam");

        let delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&fake);
        assert!(delta.complete);
        assert_eq!(delta.warnings.len(), 1, "the locked child is reported");
        assert_eq!(
            delta.warnings[0].code.id,
            error_codes::FILE_UNREADABLE.id,
            "a per-entry failure is a file's failure, not the directory's"
        );
        assert_eq!(
            delta.removals,
            vec![root.join("gone.nam")],
            "only the file that really is gone -- the sibling survives and so does the locked one"
        );

        index.apply(delta);
        let mut paths: Vec<PathBuf> = index.iter().map(|e| e.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec![root.join("kept.nam"), root.join("locked.nam")]);
    }

    /// A listing that could not be enumerated to the end concludes nothing about the children it
    /// never reached — the iterator-level half of issue #66, where the skipped child has no path
    /// to record and so the whole directory has to be treated as unseen.
    #[test]
    fn a_listing_that_ended_early_suppresses_removals_under_it() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");

        let mut index = Index::empty();
        index.upsert(LibraryEntry {
            path: root.join("unseen.nam"),
            kind: crate::entry::ItemKind::Nam,
            size: 1,
            mtime: FileTime::from_system_time(std::time::UNIX_EPOCH),
            hash: None,
            metadata: crate::entry::ItemMetadata::None,
            origin: Origin::Local,
        });

        let mut fake = FakeFs::new();
        fake.mark_partial(&root);

        let delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&fake);
        assert!(delta.complete);
        assert_eq!(delta.unreadable_prefixes, vec![root.clone()]);
        assert!(delta.removals.is_empty());
    }

    /// **Issue #67:** D-12.1's settling window has to cover every file's own examination time, and
    /// on any scan longer than the window those are nowhere near the moment the scan finished.
    ///
    /// The scenario is an ordinary cold scan of a real library: it begins at `S` and runs for two
    /// minutes. A file examined a minute in is edited moments later, in place, to the same length,
    /// and the filesystem reports the mtime it already had. Anchored to *completion*, the recorded
    /// timestamp is `S + 120 s` and the file's mtime `S + 60 s` sits a minute outside a two-second
    /// window, so the edit is skipped -- and, its stored `(size, mtime)` now matching the new
    /// content, it is invisible to every future scan as well. Anchored to the scan's *start*, an
    /// mtime at or after `S - 2 s` is suspect, which every examination time in that scan is.
    #[test]
    fn a_file_edited_during_a_long_scan_is_not_invisible_forever() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");
        let path = root.join("a.nam");

        let scan_started_at = FileTime::now();
        let examined_at = FileTime::from_nanos_since_epoch(
            scan_started_at.as_nanos_since_epoch() + 60 * 1_000_000_000,
        );

        let mut fake = FakeFs::new();
        fake.add_file(&root, "a.nam", 100, examined_at, vec![1u8; 100]);
        let mut index = Index::empty();
        index.apply(Scanner::new(vec![root.clone()], &index.clone()).run_to_completion(&fake));
        // The long scan's own baseline, as Index::apply would have recorded it.
        index.set_last_scan_started_at(scan_started_at);
        let first_hash = index.get(&path).unwrap().hash;

        // The same-length edit, reported with the mtime it already had.
        let mut edited = FakeFs::new();
        edited.add_file(&root, "a.nam", 100, examined_at, vec![2u8; 100]);
        let delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&edited);

        assert_eq!(
            delta.upserts.len(),
            1,
            "a file whose mtime falls inside the previous scan must be rehashed, wherever in that \
             scan it happened to be examined"
        );
        assert_ne!(delta.upserts[0].hash, first_hash);
    }

    /// The other side of issue #67's rule: widening the window must not turn the incremental scan
    /// into a full one. A file untouched since well before the previous scan began is still
    /// skipped without being read.
    #[test]
    fn a_file_older_than_the_previous_scan_is_still_skipped() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");

        let scan_started_at = FileTime::now();
        let long_before = FileTime::from_nanos_since_epoch(
            scan_started_at.as_nanos_since_epoch() - 3_600 * 1_000_000_000,
        );

        let mut fake = FakeFs::new();
        fake.add_file(&root, "a.nam", 100, long_before, vec![1u8; 100]);
        let mut index = Index::empty();
        index.apply(Scanner::new(vec![root.clone()], &index.clone()).run_to_completion(&fake));
        index.set_last_scan_started_at(scan_started_at);

        let delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&fake);
        assert!(
            delta.upserts.is_empty(),
            "an unchanged, old file must not be rehashed"
        );
    }

    /// **Issue #73, the decision:** a directory symlink *is* followed.
    ///
    /// Symlinking a model collection into the library root is an ordinary setup, and not
    /// traversing it was never chosen — it fell out of asking `file_type()` (which does not follow
    /// links) and nothing else, leaving that user with an empty library and no diagnostic at all.
    /// The loop-safety that shape got for free is replaced by an explicit visited set of canonical
    /// directories; the two tests below pin both halves.
    #[test]
    fn a_symlinked_library_folder_is_traversed() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");
        let collection = PathBuf::from("/fake/elsewhere/collection");

        let mut fake = FakeFs::new();
        fake.add_file(
            &collection,
            "amp.nam",
            4,
            FileTime::from_system_time(std::time::UNIX_EPOCH),
            b"junk".to_vec(),
        );
        fake.add_dir_symlink(&root, "models", &collection);

        let delta = Scanner::new(vec![root.clone()], &Index::empty()).run_to_completion(&fake);
        assert!(delta.warnings.is_empty(), "{:?}", delta.warnings);
        assert_eq!(
            delta
                .upserts
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>(),
            vec![root.join("models").join("amp.nam")],
            "the linked collection is indexed, under the path the user actually browses"
        );
    }

    /// Issue #73's other half: following links means loops are no longer impossible by
    /// construction, so they are made impossible by the visited set instead. A link pointing at a
    /// directory this scan has already walked is reported and skipped — the walk terminates, the
    /// files are not indexed twice, and the user is told why the second spelling is not listed.
    #[test]
    fn a_symlink_loop_terminates_and_is_reported() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");

        let mut fake = FakeFs::new();
        fake.add_file(
            &root,
            "amp.nam",
            4,
            FileTime::from_system_time(std::time::UNIX_EPOCH),
            b"junk".to_vec(),
        );
        fake.add_dir_symlink(&root, "self", &root);

        let delta = Scanner::new(vec![root.clone()], &Index::empty()).run_to_completion(&fake);
        assert_eq!(
            delta.upserts.len(),
            1,
            "indexed once, not once per spelling"
        );
        assert_eq!(delta.warnings.len(), 1);
        assert_eq!(
            delta.warnings[0].code.id,
            error_codes::SYMLINK_NOT_FOLLOWED.id
        );
    }

    /// **The ordinary case issue #73's guard did not actually cover:** `Library/Favourites` ->
    /// `Library/Amps`, both spellings inside the scanned tree.
    ///
    /// The visited set was consulted only when deciding whether to *follow a link*, and a link is
    /// resolved the moment its parent's listing is read, while a plain directory is only *queued*
    /// then and canonicalised later. So a sibling link always reached `expand_dir_symlink` before
    /// its target had been expanded, found the target's canonical path unvisited, and both
    /// spellings were walked — in either listing order, `read_dir`'s order not being guaranteed.
    /// Every file underneath got two `LibraryEntry` rows under two paths: each model listed twice
    /// in the library, and `paths_for_hash` reporting a duplicate that is not one.
    ///
    /// The surviving spelling is the real directory, not whichever the listing happened to name
    /// first: links are expanded only once every plain directory has been, so the path the user
    /// actually has on disk is the one indexed and the skipped one is always genuinely a symlink
    /// — which is what makes the `SYMLINK_NOT_FOLLOWED` warning true of it.
    #[test]
    fn a_symlink_beside_its_own_target_indexes_each_file_once() {
        use crate::fs::FakeFs;
        for link_listed_first in [true, false] {
            let root = PathBuf::from("/fake/root");
            let amps = root.join("amps");

            let mut fake = FakeFs::new();
            if link_listed_first {
                fake.add_dir_symlink(&root, "favourites", &amps);
                fake.add_dir(&root, "amps");
            } else {
                fake.add_dir(&root, "amps");
                fake.add_dir_symlink(&root, "favourites", &amps);
            }
            fake.add_file(
                &amps,
                "amp.nam",
                4,
                FileTime::from_system_time(std::time::UNIX_EPOCH),
                b"junk".to_vec(),
            );

            let delta = Scanner::new(vec![root.clone()], &Index::empty()).run_to_completion(&fake);
            let mut paths: Vec<PathBuf> = delta.upserts.iter().map(|e| e.path.clone()).collect();
            paths.sort();
            assert_eq!(
                paths,
                vec![amps.join("amp.nam")],
                "link listed first: {link_listed_first} -- each file indexed once, under the real \
                 directory rather than once per spelling"
            );
            assert_eq!(
                delta.warnings.len(),
                1,
                "link listed first: {link_listed_first} -- the second spelling is reported, not \
                 silently dropped: {:?}",
                delta.warnings
            );
            assert_eq!(
                delta.warnings[0].code.id,
                error_codes::SYMLINK_NOT_FOLLOWED.id
            );

            // And the duplicate is not one the rest of the library has to live with either.
            let mut index = Index::empty();
            index.apply(delta);
            let hash = namir_core::ContentHash::of(b"junk");
            assert_eq!(
                index.paths_for_hash(hash),
                [amps.join("amp.nam")],
                "link listed first: {link_listed_first} -- one file on disk is one path"
            );
        }
    }

    /// A skipped symlink is not a deletion either: whatever an earlier scan indexed under that
    /// spelling is still on disk, reached this time under another name.
    #[test]
    fn a_skipped_symlink_does_not_remove_what_was_indexed_under_it() {
        use crate::fs::FakeFs;
        let root = PathBuf::from("/fake/root");

        let mut index = Index::empty();
        index.upsert(LibraryEntry {
            path: root.join("self").join("amp.nam"),
            kind: crate::entry::ItemKind::Nam,
            size: 4,
            mtime: FileTime::from_system_time(std::time::UNIX_EPOCH),
            hash: None,
            metadata: crate::entry::ItemMetadata::None,
            origin: Origin::Local,
        });

        let mut fake = FakeFs::new();
        fake.add_file(
            &root,
            "amp.nam",
            4,
            FileTime::from_system_time(std::time::UNIX_EPOCH),
            b"junk".to_vec(),
        );
        fake.add_dir_symlink(&root, "self", &root);

        let delta = Scanner::new(vec![root.clone()], &index).run_to_completion(&fake);
        assert!(delta.removals.is_empty(), "{:?}", delta.removals);
    }
}
