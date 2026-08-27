//! Local error catalogue for `namir-library` (D-16.1), namespaced `library.*`.

use namir_core::{ErrorCode, Severity};

/// A directory could not be read (permissions, race with deletion, I/O error). FR-LIB-070:
/// "a missing file shall never crash Namir or the host" — the scan skips it and continues.
pub const DIR_UNREADABLE: ErrorCode = ErrorCode::new(
    "library.scan.dir_unreadable",
    Severity::Warning,
    "A directory could not be read and was skipped ({detail}).",
    "Anything inside it is missing from the library. Check the directory still exists and that you \
     have permission to list it, then rescan.",
);

/// A file could not be read while probing it — vanished between being listed and being opened
/// (a real race, not hypothetical, since FR-LIB-070 requires tolerating exactly this), or a
/// permissions error.
pub const FILE_UNREADABLE: ErrorCode = ErrorCode::new(
    "library.scan.file_unreadable",
    Severity::Warning,
    "A file could not be read and was skipped ({detail}).",
    "It is missing from the library list. If it was being written or moved while the scan ran, \
     rescan; otherwise check you have permission to read it.",
);

/// NFR-SEC-020: a file exceeded [`crate::MAX_INDEXED_FILE_BYTES`]. Still indexed (browsable) with
/// no extracted metadata — see [`crate::entry::LibraryEntry::hash`]'s doc comment.
pub const FILE_TOO_LARGE: ErrorCode = ErrorCode::new(
    "library.scan.file_too_large",
    Severity::Warning,
    "A file is larger than the scanning limit, so it is listed without its details ({detail}).",
    "It can still be browsed and loaded; only its metadata and content hash are missing, so it \
     will not be found by a hash search. Replace it with a smaller export if you need those.",
);

/// D-12.3/P8: the on-disk index file was missing a `format_version`, carried an unsupported one,
/// or failed to parse as the expected shape at all. Never fatal — [`crate::store::IndexStore::open`]
/// degrades to an empty index and the next scan repopulates it.
pub const INDEX_CORRUPT: ErrorCode = ErrorCode::new(
    "library.index.corrupt",
    Severity::Warning,
    "The library index could not be read and will be rebuilt by the next scan ({detail}).",
    "Rescan the library to rebuild it. Nothing on disk was lost -- the index is only a cache of \
     what a scan already found.",
);

/// The in-memory index could not be **written back** to the index file: the temporary file could
/// not be created or flushed, or the final rename failed.
///
/// Added M14 (issue #40). Every one of `store::IndexStore::save_atomic`'s four error paths reported
/// [`INDEX_CORRUPT`] until then, and every word of that entry is false on this path: nothing was
/// read, the previous index is intact, and what was actually lost is the new scan's results. The
/// same entry is still right on the *open* path — the 2026-08-27 manual run induced both and
/// recorded that contrast, which is the argument for splitting them.
pub const INDEX_SAVE_FAILED: ErrorCode = ErrorCode::new(
    "library.index.save_failed",
    Severity::Warning,
    "The library index could not be saved, so this scan's results are only kept until Namir \
     closes ({detail}).",
    "The list on screen is current; only the file on disk is stale. Check the configuration \
     directory is writable and has free space, then rescan.",
);

/// A path's file name is not valid UTF-8. Recorded rather than discovered later: reconstructing
/// an `OsString` from arbitrary bytes needs platform-specific APIs (`std::os::unix::ffi::OsStrExt`
/// and its Windows equivalent), which D-5.2's textual cfg lint reserves for `namir-platform` —
/// which this crate may not depend on (D-5.1). Decision: such a path is skipped, not indexed.
pub const NON_UTF8_PATH: ErrorCode = ErrorCode::new(
    "library.path.non_utf8",
    Severity::Warning,
    "A file name is not valid UTF-8 and was skipped ({detail}).",
    "Rename the file using ordinary text characters and rescan; Namir cannot index a name it \
     cannot spell.",
);

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[ErrorCode] = &[
        DIR_UNREADABLE,
        FILE_UNREADABLE,
        FILE_TOO_LARGE,
        INDEX_CORRUPT,
        INDEX_SAVE_FAILED,
        NON_UTF8_PATH,
    ];

    #[test]
    fn catalogue_ids_are_unique_and_namespaced() {
        namir_core::assert_unique_ids(ALL);
        for a in ALL {
            assert!(a.id.starts_with("library."), "{} is not namespaced", a.id);
        }
    }

    /// Issue #40: reading the index and writing it back fail differently and must not share an
    /// entry. `INDEX_CORRUPT` promises a rebuild by the next scan, which is true of a file that
    /// failed to parse and false of a scan whose results never reached disk.
    #[test]
    fn the_open_and_save_paths_have_separate_entries() {
        assert_ne!(INDEX_CORRUPT.id, INDEX_SAVE_FAILED.id);
        assert!(
            !INDEX_SAVE_FAILED
                .message_template
                .contains("could not be read"),
            "the save-path entry must not claim anything was read"
        );
    }
}
