//! [`StateError`]: a catalogue-backed failure that stops a document from loading at all.
//! [`StateWarning`]: the non-fatal counterpart D-11.2's tolerance rules produce — a document that
//! loads anyway, but with something worth telling the user about (an unknown parameter, a
//! clamped value, a file reference that didn't resolve). Kept as two distinct types rather than
//! one `Result<T, StateError>` with warnings folded into `Ok`, because a caller needs to
//! distinguish "loaded, here's what to show the user" from "did not load, here's why" at the type
//! level — `namir_worker::JobResult`'s three-way `Loaded{warning}/Failed/NotDelivered` split
//! (`crates/namir-worker/src/lib.rs`) is the precedent this mirrors.

use namir_core::ErrorCode;

/// A failure that stops a document from being read at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateError {
    /// Which catalogue entry this maps to.
    pub code: ErrorCode,
    /// Free-text context for the template's placeholders. Never the user-facing string itself —
    /// the template lives in the catalogue.
    pub detail: String,
}

impl StateError {
    /// Builds an error against a catalogue entry.
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} ({})",
            self.code.id, self.code.message_template, self.detail
        )
    }
}

impl std::error::Error for StateError {}

/// A non-fatal condition produced while loading a document that loaded anyway (D-11.2's
/// tolerance rules; FR-STATE-070's resolution failures). Structurally identical to
/// [`StateError`] — same `code`/`detail` shape — but kept as a separate type so a caller cannot
/// accidentally treat "the document loaded, with caveats" as "the document failed to load" by
/// forgetting to check which list an entry came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateWarning {
    /// Which catalogue entry this maps to.
    pub code: ErrorCode,
    /// Free-text context for the template's placeholders.
    pub detail: String,
}

impl StateWarning {
    /// Builds a warning against a catalogue entry.
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for StateWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} ({})",
            self.code.id, self.code.message_template, self.detail
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_codes;

    #[test]
    fn state_error_display_includes_code_id_and_detail() {
        let err = StateError::new(error_codes::MALFORMED_JSON, "unexpected token at line 3");
        let s = err.to_string();
        assert!(s.contains("state.document.malformed_json"));
        assert!(s.contains("unexpected token at line 3"));
    }

    #[test]
    fn state_warning_display_includes_code_id_and_detail() {
        let warn = StateWarning::new(error_codes::UNKNOWN_PARAMETER, "comp.ratio");
        let s = warn.to_string();
        assert!(s.contains("state.param.unknown_key"));
        assert!(s.contains("comp.ratio"));
    }
}
