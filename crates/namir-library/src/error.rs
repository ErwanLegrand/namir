//! [`LibraryError`]/[`LibraryWarning`]: catalogue-backed failures, mirroring
//! `namir_state::{StateError, StateWarning}`'s shape exactly (see that crate's `error.rs` for the
//! rationale — the two-type split so a caller can't accidentally treat "loaded with caveats" as
//! "failed to load" by forgetting which list an entry came from).

use namir_core::ErrorCode;

/// A failure that stops an operation outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryError {
    /// Which catalogue entry this maps to.
    pub code: ErrorCode,
    /// Free-text context for the template's placeholders.
    pub detail: String,
}

impl LibraryError {
    /// Builds an error against a catalogue entry.
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for LibraryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `render`, not `message_template`: since M14 a template may carry one `{detail}`
        // placeholder, and printing it raw is issue #15's defect at a second layer.
        write!(f, "{}: {}", self.code.id, self.code.render(&self.detail))
    }
}

impl std::error::Error for LibraryError {}

/// A non-fatal condition an operation produced but proceeded anyway (P8: "failure degrades; it
/// does not propagate").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryWarning {
    /// Which catalogue entry this maps to.
    pub code: ErrorCode,
    /// Free-text context for the template's placeholders.
    pub detail: String,
}

impl LibraryWarning {
    /// Builds a warning against a catalogue entry.
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for LibraryWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `render`, not `message_template`: since M14 a template may carry one `{detail}`
        // placeholder, and printing it raw is issue #15's defect at a second layer.
        write!(f, "{}: {}", self.code.id, self.code.render(&self.detail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_codes;

    #[test]
    fn error_display_includes_code_id_and_detail() {
        let err = LibraryError::new(error_codes::FILE_UNREADABLE, "/some/path.nam");
        let s = err.to_string();
        assert!(s.contains("library.scan.file_unreadable"));
        assert!(s.contains("/some/path.nam"));
    }

    #[test]
    fn warning_display_includes_code_id_and_detail() {
        let warn = LibraryWarning::new(error_codes::INDEX_CORRUPT, "bad json");
        let s = warn.to_string();
        assert!(s.contains("library.index.corrupt"));
        assert!(s.contains("bad json"));
    }
}
