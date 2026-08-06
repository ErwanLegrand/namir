//! Local error catalogue for `namir-library` (D-16.1), namespaced `library.*`.

use namir_core::{ErrorCode, Severity};

/// A directory could not be read (permissions, race with deletion, I/O error). FR-LIB-070:
/// "a missing file shall never crash Namir or the host" — the scan skips it and continues.
pub const DIR_UNREADABLE: ErrorCode = ErrorCode {
    id: "library.scan.dir_unreadable",
    severity: Severity::Warning,
    message_template: "The directory {path} could not be read and was skipped.",
};

/// A file could not be read while probing it — vanished between being listed and being opened
/// (a real race, not hypothetical, since FR-LIB-070 requires tolerating exactly this), or a
/// permissions error.
pub const FILE_UNREADABLE: ErrorCode = ErrorCode {
    id: "library.scan.file_unreadable",
    severity: Severity::Warning,
    message_template: "The file {path} could not be read and was skipped.",
};

/// NFR-SEC-020: a file exceeded [`crate::MAX_INDEXED_FILE_BYTES`]. Still indexed (browsable) with
/// no extracted metadata — see [`crate::entry::LibraryEntry::hash`]'s doc comment.
pub const FILE_TOO_LARGE: ErrorCode = ErrorCode {
    id: "library.scan.file_too_large",
    severity: Severity::Warning,
    message_template: "The file {path} is larger than the {limit_mb} MB limit and was indexed \
                        without its content hash or metadata.",
};

/// D-12.3/P8: the on-disk index file was missing a `format_version`, carried an unsupported one,
/// or failed to parse as the expected shape at all. Never fatal — [`crate::store::IndexStore::open`]
/// degrades to an empty index and the next scan repopulates it.
pub const INDEX_CORRUPT: ErrorCode = ErrorCode {
    id: "library.index.corrupt",
    severity: Severity::Warning,
    message_template: "The library index at {path} could not be read ({detail}) and will be \
                        rebuilt by the next scan.",
};

/// A path's file name is not valid UTF-8. Recorded rather than discovered later: reconstructing
/// an `OsString` from arbitrary bytes needs platform-specific APIs (`std::os::unix::ffi::OsStrExt`
/// and its Windows equivalent), which D-5.2's textual cfg lint reserves for `namir-platform` —
/// which this crate may not depend on (D-5.1). Decision: such a path is skipped, not indexed.
pub const NON_UTF8_PATH: ErrorCode = ErrorCode {
    id: "library.path.non_utf8",
    severity: Severity::Warning,
    message_template: "A path under {parent} is not valid UTF-8 and was skipped.",
};

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[ErrorCode] = &[
        DIR_UNREADABLE,
        FILE_UNREADABLE,
        FILE_TOO_LARGE,
        INDEX_CORRUPT,
        NON_UTF8_PATH,
    ];

    #[test]
    fn catalogue_ids_are_unique_and_namespaced() {
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i + 1) {
                assert_ne!(a.id, b.id, "duplicate catalogue id {}", a.id);
            }
            assert!(a.id.starts_with("library."), "{} is not namespaced", a.id);
        }
    }
}
