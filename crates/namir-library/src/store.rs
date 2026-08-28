//! AQ-3 resolved (D-12.3, `docs/02-architecture.md`): the library index is stored as a single
//! pretty-printed JSON document, replaced atomically. No new dependency, no copyleft, no C/C++
//! (NFR-PORT-040), and corruption degrades to a full rescan rather than a crash or wrong results
//! (P8) — see below for how.
//!
//! **Rationale.** FR-LIB-040's free-text search has no key by which it could be an indexed
//! lookup — it filters over every record's name and every metadata field — so the whole index
//! must be resident in memory regardless of how it is stored on disk (at 10,000 records of a few
//! hundred bytes each, a few MB). An embedded key-value store's entire value proposition —
//! random access to one record without its neighbours — is a property this workload has no use
//! for: the index is a rebuildable cache, not a database. A single JSON document reuses
//! `serde_json`, already the one hardened parser D-11.1 chose specifically so there would be only
//! one to fuzz (P6) — a second, third-party binary format would be a second attack surface owned
//! by someone else.
//!
//! **Atomicity.** Written to a temporary file in the same directory, `fsync`ed, then
//! `std::fs::rename`d over the destination — which replaces an existing file on both Unix and
//! Windows, so no platform-conditional code is needed (D-5.2's cfg lint would reject one anyway).
//! A reader therefore always sees either the complete previous file or the complete new one,
//! **never a partial write** — a torn write is impossible by construction, which satisfies
//! D-12.3's corruption clause by construction rather than by recovery logic.
//!
//! **Corruption.** Any other read failure — the file is missing, the JSON is malformed, or its
//! `format_version` is one this build doesn't understand — yields an *empty* index and a
//! [`LibraryWarning`], never a hard error: the next scan repopulates it from scratch. A missing
//! file (the ordinary first-run case) produces no warning at all; that is not corruption.
//!
//! **Favourites are exempt from that policy (issue #68).** "Discard everything and rescan" is
//! only harmless for what a rescan can rebuild. FR-LIB-050's favourite marks are hand-curated and
//! a scan cannot reproduce a single one of them, so a malformed byte used to destroy the user's
//! whole favourites list permanently — under a warning that said the index "will be rebuilt by
//! the next scan", which was true of the entries and false of the marks. `index.rs`'s own note
//! had the trade-off backwards: co-locating them avoided "a second thing that can go missing
//! independently" at the price of making them go missing *together*.
//!
//! They are therefore mirrored to a small sidecar document beside the index
//! ([`IndexStore::favourites_path`]), written by the same atomic discipline, and recovered on a
//! corrupt open — first by re-reading the damaged index leniently (a `format_version` this build
//! rejects is still perfectly good JSON, and so is a document with one bad entry in it), then
//! from the sidecar. The index document keeps carrying them too and stays the authority whenever
//! it loads, so the sidecar can never resurrect a mark the user has since removed: it is
//! consulted only when the document it mirrors could not be read at all.
//!
//! **Rejected:** an append-only log with compaction (D-12.3's other named option) — it can tear
//! on a crash mid-append, which atomic whole-file replacement cannot, and needs its own
//! compaction policy, for an incremental-write saving (avoiding rewriting a few MB) that does not
//! justify the added failure mode against a workload NFR-PERF-060 already budgets two seconds
//! for. `redb` 4.1.0 (MIT OR Apache-2.0; one transitive dependency, `libc`) — it clears the
//! licence bar but carries a build script, as does `libc`, and this workspace's own adoption bar
//! (the criteria that admitted `rtrb`, §17: "zero transitive dependencies, no build script,
//! `no_std`-capable pure Rust, MSRV far below this workspace's own") is met on one of four
//! criteria, not all. D-17.1 rejected `symphonia` over a licence nuance on a **Should**
//! requirement; taking on an embedded B-tree store's build-script and cross-compilation risk
//! (both new crates must build for `aarch64-linux-android`/`aarch64-apple-ios`, NFR-PORT-030) for
//! a few-MB rebuildable cache on a **Must** is a weaker case than that one already was.

use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::entry::{FileTime, LibraryEntry};
use crate::error::{LibraryError, LibraryWarning};
use crate::error_codes;
use crate::favourites::Favourites;
use crate::index::Index;

const STORE_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct OnDisk {
    format_version: u32,
    entries: Vec<LibraryEntry>,
    /// D-12.1's mtime-settling protection (see `index.rs`'s field of the same name) — persisted
    /// so it survives a restart. `#[serde(default)]` so an index written before this field
    /// existed still loads (D-11.2's tolerant-reading spirit, applied to this crate's own format
    /// rather than namir-state's).
    ///
    /// Issue #67 changed what this records — the scan's start rather than its completion — so the
    /// key changed with it. The `alias` reads an index written by a build that stored the
    /// completion time: using it as a start time is a *narrower* window than it should be, and
    /// therefore no worse than the `None` the rename would otherwise produce, for the single
    /// scan it takes to be rewritten.
    #[serde(default, alias = "last_scan_completed_at")]
    last_scan_started_at: Option<FileTime>,
    /// FR-LIB-050's favourite marks. `#[serde(default)]` for the same forward-compatibility
    /// reason as `last_scan_started_at`.
    #[serde(default)]
    favourites: Favourites,
}

/// Owns the on-disk index file's path and knows how to (re)load and atomically replace it.
/// Deliberately holds no in-memory state of its own beyond the path — the [`Index`] it
/// loads/saves is the caller's to keep and mutate; this type is purely the persistence boundary.
///
/// `Clone` (a single `PathBuf`, so this is cheap): M5's `namir-worker::library::LibraryService`
/// hands a copy into the closure a pool job runs so the job can save on completion without a
/// second, path-only type existing solely to carry that one field across the thread boundary.
#[derive(Clone)]
pub struct IndexStore {
    path: PathBuf,
}

impl IndexStore {
    /// Opens the index at `path`, returning a usable [`Index`] regardless of what it finds there
    /// (P8) — never a hard error. A missing file (first run) yields an empty index silently; a
    /// present-but-corrupt or wrong-version file yields an empty index plus one
    /// [`LibraryWarning`].
    pub fn open(path: PathBuf) -> (IndexStore, Index, Vec<LibraryWarning>) {
        let mut warnings = Vec::new();
        let index = match Self::try_load(&path) {
            LoadOutcome::Loaded(index) => index,
            // Issue #68: the entries are a cache and may be dropped; the favourite marks are not
            // and may not. Recovered from whatever of the two documents can still be read.
            LoadOutcome::FirstRun => Self::empty_with_recovered_favourites(&path),
            LoadOutcome::Corrupt(warning) => {
                warnings.push(warning);
                Self::empty_with_recovered_favourites(&path)
            }
        };
        (IndexStore { path }, index, warnings)
    }

    /// The sidecar document FR-LIB-050's marks are mirrored into — see this module's doc comment.
    /// `<index-stem>.favourites.json`, beside the index so the two move together.
    pub fn favourites_path(path: &Path) -> PathBuf {
        path.with_extension("favourites.json")
    }

    /// An empty index carrying whatever favourites survived (issue #68). Tried in order of
    /// freshness: the index document itself, read leniently — a `format_version` this build
    /// refuses, or one bad entry among ten thousand, still leaves a perfectly readable
    /// `favourites` array — and then the sidecar, which is what survives a document too damaged
    /// to parse as JSON at all, or one that was deleted outright.
    fn empty_with_recovered_favourites(path: &Path) -> Index {
        let mut index = Index::empty();
        let recovered = Self::salvage_favourites(path)
            .or_else(|| Self::read_favourites_sidecar(path))
            .unwrap_or_default();
        *index.favourites_mut() = recovered;
        index
    }

    fn salvage_favourites(path: &Path) -> Option<Favourites> {
        let bytes = std::fs::read(path).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let favourites: Favourites =
            serde_json::from_value(value.get("favourites")?.clone()).ok()?;
        (!favourites.is_empty()).then_some(favourites)
    }

    fn read_favourites_sidecar(path: &Path) -> Option<Favourites> {
        let bytes = std::fs::read(Self::favourites_path(path)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn try_load(path: &Path) -> LoadOutcome {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LoadOutcome::FirstRun,
            Err(e) => {
                return LoadOutcome::Corrupt(LibraryWarning::new(
                    error_codes::INDEX_CORRUPT,
                    format!("{}: {e}", path.display()),
                ));
            }
        };
        let on_disk: OnDisk = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                return LoadOutcome::Corrupt(LibraryWarning::new(
                    error_codes::INDEX_CORRUPT,
                    format!("{}: {e}", path.display()),
                ));
            }
        };
        if on_disk.format_version != STORE_FORMAT_VERSION {
            return LoadOutcome::Corrupt(LibraryWarning::new(
                error_codes::INDEX_CORRUPT,
                format!(
                    "{}: format_version {} is not the {STORE_FORMAT_VERSION} this build \
                     understands",
                    path.display(),
                    on_disk.format_version
                ),
            ));
        }
        let mut index = Index::empty();
        for entry in on_disk.entries {
            index.upsert(entry);
        }
        if let Some(at) = on_disk.last_scan_started_at {
            index.set_last_scan_started_at(at);
        }
        *index.favourites_mut() = on_disk.favourites;
        LoadOutcome::Loaded(index)
    }

    /// Writes `index` to this store's path, atomically (see this module's doc comment). The
    /// temporary file lives in the same directory as the destination so the final `rename` is
    /// guaranteed to be within one filesystem (a cross-filesystem rename is not atomic on every
    /// platform, and some fail outright).
    ///
    /// FR-LIB-050's favourites are mirrored to [`Self::favourites_path`] in the same call. That
    /// write's failure is deliberately **not** this call's failure (issue #68): the marks are also
    /// inside the index document that just landed, so a sidecar that could not be written costs
    /// redundancy, not data — and reporting a save failure for an index that saved correctly would
    /// be the opposite of P8's "failure degrades".
    pub fn save_atomic(&self, index: &Index) -> Result<(), LibraryError> {
        let on_disk = OnDisk {
            format_version: STORE_FORMAT_VERSION,
            entries: index.iter().cloned().collect(),
            last_scan_started_at: index.last_scan_started_at(),
            favourites: index.favourites().clone(),
        };
        let bytes = serde_json::to_vec_pretty(&on_disk)
            .expect("an Index built from LibraryEntry values always serialises");
        let favourites = serde_json::to_vec_pretty(index.favourites())
            .expect("a Favourites is a list of hex strings and always serialises");

        let index_result = self.write_atomic(&self.path, &bytes);
        // Attempted whichever way the index write went: if that one failed, the sidecar is the
        // only place these marks now exist.
        let _ = self.write_atomic(&Self::favourites_path(&self.path), &favourites);
        index_result
    }

    /// One staged-then-renamed write. `dest`'s directory holds the staging file, so the rename
    /// never crosses a filesystem.
    fn write_atomic(&self, dest: &Path, bytes: &[u8]) -> Result<(), LibraryError> {
        let tmp_path = stage_path(dest);
        let result = (|| {
            let mut file = File::create(&tmp_path).map_err(|e| save_failed(&tmp_path, e))?;
            file.write_all(bytes)
                .map_err(|e| save_failed(&tmp_path, e))?;
            file.sync_all().map_err(|e| save_failed(&tmp_path, e))?;
            drop(file);
            std::fs::rename(&tmp_path, dest).map_err(|e| {
                LibraryError::new(
                    error_codes::INDEX_SAVE_FAILED,
                    format!("renaming {} to {}: {e}", tmp_path.display(), dest.display()),
                )
            })
        })();
        if result.is_err() {
            // A staging name is unique per write (below), so a failed write that left its file
            // behind would leave a new one behind every time.
            let _ = std::fs::remove_file(&tmp_path);
        }
        result
    }
}

fn save_failed(tmp_path: &Path, e: std::io::Error) -> LibraryError {
    LibraryError::new(
        error_codes::INDEX_SAVE_FAILED,
        format!("{}: {e}", tmp_path.display()),
    )
}

/// Issue #69: the staging file's name, unique to this process and to this write.
///
/// It used to be `dest.with_extension("tmp")` — one fixed name, unowned and unlocked. Both product
/// shells resolve the same index path through `LibraryService::open_default`, so the standalone app
/// running while a DAW loads the CLAP plugin — the ordinary case, not a contrived one — had two
/// processes `File::create`ing (that is, truncating) and writing into the same staging file at
/// once, after which whichever `rename`d published a blend of both. `rename`'s atomicity says
/// nothing about that: it guarantees the *destination* is never seen half-written, not that the
/// source was ever whole.
///
/// The process id separates processes; the counter separates concurrent writes inside one process
/// (both shells can be hosted in a single process, and nothing here is otherwise serialised).
fn stage_path(dest: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name: OsString = dest.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{serial}.tmp", std::process::id()));
    dest.with_file_name(name)
}

enum LoadOutcome {
    Loaded(Index),
    FirstRun,
    Corrupt(LibraryWarning),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{FileTime, ItemKind, ItemMetadata, Origin};
    use namir_core::ContentHash;

    fn temp_index_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-library-store-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("index.json")
    }

    fn sample_entry() -> LibraryEntry {
        LibraryEntry {
            path: PathBuf::from("marshall/plexi.nam"),
            kind: ItemKind::Nam,
            size: 1234,
            mtime: FileTime::now(),
            hash: Some(ContentHash::of(b"store test")),
            metadata: ItemMetadata::None,
            origin: Origin::Local,
        }
    }

    #[test]
    fn opening_a_missing_file_yields_an_empty_index_and_no_warnings() {
        let path = temp_index_path("missing");
        let (_, index, warnings) = IndexStore::open(path);
        assert!(index.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn save_then_open_round_trips_the_index() {
        let path = temp_index_path("round_trip");
        let entry = sample_entry(); // built once: FileTime::now() advances between calls
        let (store, mut index, _) = IndexStore::open(path.clone());
        index.upsert(entry.clone());
        store.save_atomic(&index).unwrap();

        let (_, reloaded, warnings) = IndexStore::open(path.clone());
        assert!(warnings.is_empty());
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.get(Path::new("marshall/plexi.nam")), Some(&entry));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// D-12.1's mtime-settling protection must survive a restart, or a process that reopens the
    /// index right after a scan loses the very window it's supposed to guard.
    #[test]
    fn last_scan_started_at_survives_a_save_and_reload() {
        let path = temp_index_path("scan_completed_at");
        let (store, mut index, _) = IndexStore::open(path.clone());
        let stamp = FileTime::now();
        index.set_last_scan_started_at(stamp);
        store.save_atomic(&index).unwrap();

        let (_, reloaded, _) = IndexStore::open(path.clone());
        assert_eq!(reloaded.last_scan_started_at(), Some(stamp));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// D-11.2's tolerant-reading spirit, applied to this crate's own on-disk format: an index
    /// written before `last_scan_started_at` existed (or by a future build that omits it for
    /// some other reason) must still load.
    #[test]
    fn a_missing_last_scan_started_at_field_defaults_to_none() {
        let path = temp_index_path("no_scan_completed_at_field");
        std::fs::write(&path, br#"{"format_version": 1, "entries": []}"#).unwrap();
        let (_, index, warnings) = IndexStore::open(path.clone());
        assert!(warnings.is_empty());
        assert_eq!(index.last_scan_started_at(), None);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// D-12.3's literal requirement: corruption degrades to an empty index (forcing a full
    /// rescan), never a crash.
    #[test]
    fn a_malformed_index_file_degrades_to_empty_with_a_warning() {
        let path = temp_index_path("malformed");
        std::fs::write(&path, b"{ this is not valid json").unwrap();

        let (_, index, warnings) = IndexStore::open(path.clone());
        assert!(index.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.id, error_codes::INDEX_CORRUPT.id);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_unsupported_format_version_degrades_to_empty_with_a_warning() {
        let path = temp_index_path("bad_version");
        std::fs::write(&path, br#"{"format_version": 999, "entries": []}"#).unwrap();

        let (_, index, warnings) = IndexStore::open(path.clone());
        assert!(index.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.id, error_codes::INDEX_CORRUPT.id);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// **Issue #40: the save path reports a save failure, not a corrupt index.** Every one of
    /// `save_atomic`'s four error paths reported `library.index.corrupt` until M14, whose text
    /// says the index "could not be read ... and will be rebuilt by the next scan" -- on this path
    /// nothing was read, the previous index is intact, and what was lost is the *new* scan's
    /// results. The 2026-08-27 FR-UI-070 run induced both cases (steps 7 and 10) and recorded that
    /// every word of that text was true on the open path and false on this one.
    ///
    /// Induced by making the destination a *directory*, so the final rename cannot succeed on any
    /// platform, while the earlier temp-file steps do.
    #[test]
    fn a_failed_save_reports_the_save_entry_and_not_the_corrupt_one() {
        let path = temp_index_path("save_failure");
        let (store, mut index, _) = IndexStore::open(path.clone());
        index.upsert(sample_entry());
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir_all(&path).unwrap();

        let err = store
            .save_atomic(&index)
            .expect_err("renaming onto a directory cannot succeed");
        assert_eq!(err.code.id, error_codes::INDEX_SAVE_FAILED.id);
        assert_ne!(err.code.id, error_codes::INDEX_CORRUPT.id);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Atomicity's whole point: a `save_atomic` call must never leave a reader observing a
    /// half-written file. This is demonstrated by construction (the write goes to a temp file,
    /// visible under `.tmp`) rather than by trying to inject a mid-write crash, which no
    /// unprivileged test can do deterministically.
    #[test]
    fn save_writes_to_a_temp_path_before_the_final_rename() {
        let path = temp_index_path("atomic");
        let (store, mut index, _) = IndexStore::open(path.clone());
        index.upsert(sample_entry());
        store.save_atomic(&index).unwrap();

        assert!(path.exists());
        assert_eq!(
            leftover_temp_files(path.parent().unwrap()),
            Vec::<PathBuf>::new(),
            "no staging file may survive a successful save -- and since issue #69 gave each write \
             its own name, one left behind would be a new one every time"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
    /// **Issue #68:** a corrupt index file must not take the user's hand-curated favourites
    /// with it. The entries are a rebuildable cache; the favourite marks are not.
    #[test]
    fn favourites_survive_a_corrupt_index_file() {
        let path = temp_index_path("favourites_vs_corruption");
        let (store, mut index, _) = IndexStore::open(path.clone());
        index.upsert(sample_entry());
        let favourite = ContentHash::of(b"a treasured model");
        index.favourites_mut().mark(favourite);
        store.save_atomic(&index).unwrap();

        // The index document is damaged -- a truncated write, a bad byte, a future format_version.
        std::fs::write(&path, b"{ not json at all").unwrap();

        let (_, reloaded, warnings) = IndexStore::open(path.clone());
        assert!(
            reloaded.is_empty(),
            "the entries are a cache and may be dropped"
        );
        assert_eq!(warnings.len(), 1);
        assert!(
            reloaded.favourites().is_favourite(favourite),
            "favourites are not rebuildable by a rescan and must survive"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// **Issue #69:** the staging file's name must not be a fixed name every writer in every
    /// process picks. Both product shells open the same index path through
    /// `LibraryService::open_default`, so the standalone app and a plugin instance staging their
    /// saves through one `library-index.tmp` — truncating and writing into it simultaneously — is
    /// the ordinary case, and `rename`'s atomicity does not help when two writers share one
    /// staging file.
    ///
    /// Demonstrated deterministically rather than by racing two threads and hoping the window is
    /// hit: anything already sitting on the deterministic name breaks this writer's save outright,
    /// which is only possible because the name is predictable and unowned.
    #[test]
    fn a_save_is_not_broken_by_something_occupying_a_predictable_temp_name() {
        let path = temp_index_path("temp_name_collision");
        let (store, mut index, _) = IndexStore::open(path.clone());
        index.upsert(sample_entry());

        // Whatever a second writer would stage through, this writer must not depend on it being
        // free. A directory stands in for "occupied by someone else" in a way no platform lets
        // File::create silently take over.
        std::fs::create_dir_all(path.with_extension("tmp")).unwrap();

        store
            .save_atomic(&index)
            .expect("a save must not depend on a shared, predictable staging name being free");

        let (_, reloaded, warnings) = IndexStore::open(path.clone());
        assert!(warnings.is_empty());
        assert_eq!(reloaded.len(), 1);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
    /// Every staging file in this test's directory, whatever it is called — the point of issue
    /// #69's fix is that the name is no longer predictable, so a test cannot name it either.
    fn leftover_temp_files(dir: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        found.sort();
        found
    }

    /// **Issue #69:** two writers must not stage through one file. The names differ per process
    /// and per write, and both still sit beside the destination so the final rename stays within
    /// one filesystem.
    #[test]
    fn each_write_stages_through_its_own_file_beside_the_destination() {
        let dest = PathBuf::from("/some/dir/library-index.json");
        let first = stage_path(&dest);
        let second = stage_path(&dest);

        assert_ne!(first, second, "two concurrent writes must not share a file");
        assert_eq!(first.parent(), dest.parent());
        assert_eq!(second.parent(), dest.parent());
        let name = first.file_name().unwrap().to_str().unwrap();
        assert!(
            name.contains(&std::process::id().to_string()),
            "another process must not pick this name: {name}"
        );
        assert!(name.ends_with(".tmp"));
    }

    /// A failed save takes its own staging file with it, rather than leaving one behind per
    /// attempt now that the names are unique.
    #[test]
    fn a_failed_save_leaves_no_staging_file_behind() {
        let path = temp_index_path("failed_save_cleanup");
        let (store, mut index, _) = IndexStore::open(path.clone());
        index.upsert(sample_entry());
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir_all(&path).unwrap();

        store.save_atomic(&index).unwrap_err();
        assert_eq!(
            leftover_temp_files(path.parent().unwrap()),
            Vec::<PathBuf>::new()
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// **Issue #68**, the version-bump form: a `format_version` this build refuses is still
    /// perfectly readable JSON, so the marks come back out of the very document that was rejected.
    #[test]
    fn favourites_survive_an_index_written_by_a_future_build() {
        let path = temp_index_path("favourites_vs_future_version");
        let favourite = ContentHash::of(b"a treasured model");
        std::fs::write(
            &path,
            format!(r#"{{"format_version": 99, "entries": [], "favourites": ["{favourite}"]}}"#),
        )
        .unwrap();

        let (_, index, warnings) = IndexStore::open(path.clone());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.id, error_codes::INDEX_CORRUPT.id);
        assert!(index.is_empty());
        assert!(index.favourites().is_favourite(favourite));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// **Issue #68**, the deleted-index form: the marks are mirrored beside the index, so even
    /// deleting the index outright to force a rescan keeps them.
    #[test]
    fn favourites_survive_the_index_file_being_deleted() {
        let path = temp_index_path("favourites_vs_deletion");
        let (store, mut index, _) = IndexStore::open(path.clone());
        index.upsert(sample_entry());
        let favourite = ContentHash::of(b"a treasured model");
        index.favourites_mut().mark(favourite);
        store.save_atomic(&index).unwrap();
        std::fs::remove_file(&path).unwrap();

        let (_, reloaded, warnings) = IndexStore::open(path.clone());
        assert!(warnings.is_empty(), "a missing index is not corruption");
        assert!(reloaded.is_empty());
        assert!(reloaded.favourites().is_favourite(favourite));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The sidecar must never resurrect a mark the user has removed: the index document is the
    /// authority whenever it loads, and the sidecar is consulted only when it does not.
    #[test]
    fn the_index_document_outranks_the_sidecar_whenever_it_loads() {
        let path = temp_index_path("favourites_precedence");
        let (store, mut index, _) = IndexStore::open(path.clone());
        let favourite = ContentHash::of(b"a treasured model");
        index.favourites_mut().mark(favourite);
        store.save_atomic(&index).unwrap();

        // Unmarked, and saved again -- both documents are rewritten.
        index.favourites_mut().unmark(favourite);
        store.save_atomic(&index).unwrap();

        let (_, reloaded, _) = IndexStore::open(path.clone());
        assert!(!reloaded.favourites().is_favourite(favourite));

        // Even with a stale sidecar (a save whose sidecar write failed, say), an index that loads
        // is believed.
        std::fs::write(
            IndexStore::favourites_path(&path),
            format!(r#"["{favourite}"]"#),
        )
        .unwrap();
        let (_, reloaded, warnings) = IndexStore::open(path.clone());
        assert!(warnings.is_empty());
        assert!(
            !reloaded.favourites().is_favourite(favourite),
            "a readable index document is the authority on its own favourites"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
