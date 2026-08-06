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

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::entry::LibraryEntry;
use crate::error::{LibraryError, LibraryWarning};
use crate::error_codes;
use crate::index::Index;

const STORE_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct OnDisk {
    format_version: u32,
    entries: Vec<LibraryEntry>,
}

/// Owns the on-disk index file's path and knows how to (re)load and atomically replace it.
/// Deliberately holds no in-memory state of its own beyond the path — the [`Index`] it
/// loads/saves is the caller's to keep and mutate; this type is purely the persistence boundary.
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
            LoadOutcome::FirstRun => Index::empty(),
            LoadOutcome::Corrupt(warning) => {
                warnings.push(warning);
                Index::empty()
            }
        };
        (IndexStore { path }, index, warnings)
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
        LoadOutcome::Loaded(index)
    }

    /// Writes `index` to this store's path, atomically (see this module's doc comment). The
    /// temporary file lives in the same directory as the destination so the final `rename` is
    /// guaranteed to be within one filesystem (a cross-filesystem rename is not atomic on every
    /// platform, and some fail outright).
    pub fn save_atomic(&self, index: &Index) -> Result<(), LibraryError> {
        let on_disk = OnDisk {
            format_version: STORE_FORMAT_VERSION,
            entries: index.iter().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&on_disk)
            .expect("an Index built from LibraryEntry values always serialises");

        let tmp_path = self.path.with_extension("tmp");
        let mut file = File::create(&tmp_path).map_err(|e| {
            LibraryError::new(
                error_codes::INDEX_CORRUPT,
                format!("{}: {e}", tmp_path.display()),
            )
        })?;
        file.write_all(&bytes).map_err(|e| {
            LibraryError::new(
                error_codes::INDEX_CORRUPT,
                format!("{}: {e}", tmp_path.display()),
            )
        })?;
        file.sync_all().map_err(|e| {
            LibraryError::new(
                error_codes::INDEX_CORRUPT,
                format!("{}: {e}", tmp_path.display()),
            )
        })?;
        drop(file);

        std::fs::rename(&tmp_path, &self.path).map_err(|e| {
            LibraryError::new(
                error_codes::INDEX_CORRUPT,
                format!(
                    "renaming {} to {}: {e}",
                    tmp_path.display(),
                    self.path.display()
                ),
            )
        })
    }
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
        assert!(
            !path.with_extension("tmp").exists(),
            "temp file must not survive a successful save"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
