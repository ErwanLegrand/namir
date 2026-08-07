//! [`WorkerError`]: a catalogue-backed failure, carrying whatever detail the UI needs to fill the
//! entry's message template (FR-ERR-020).
//!
//! Formatting and allocating here is correct, not a violation: D-16.2 confines that away from the
//! *audio* thread, and its own wording puts it on "the UI or worker side" explicitly.

use namir_core::ErrorCode;

/// One failure, with its catalogue entry and the context needed to render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerError {
    /// Which catalogue entry this maps to. May belong to *another* crate's catalogue — see the
    /// `From` impls below.
    pub code: ErrorCode,
    /// Free-text context (a path, an underlying message) for the template's placeholders. Never
    /// the user-facing string itself: the template lives in the catalogue.
    pub detail: String,
}

impl WorkerError {
    /// Builds an error against a catalogue entry.
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} ({})",
            self.code.id, self.code.message_template, self.detail
        )
    }
}

impl std::error::Error for WorkerError {}

/// **The inner id is passed through unchanged, not re-wrapped.** A `.nam` file that failed
/// validation already has a precise `nam.load.*` id naming the reason; replacing it with a generic
/// `worker.*` id would destroy exactly the specificity FR-ERR-020 exists to preserve, and the user
/// would be told "loading failed" instead of "this file declares an unsupported architecture".
impl From<namir_nam::NamLoadError> for WorkerError {
    fn from(e: namir_nam::NamLoadError) -> Self {
        Self {
            code: e.code,
            detail: e.to_string(),
        }
    }
}

/// See the `NamLoadError` impl above — same rule, `ir.load.*` ids preserved.
impl From<namir_ir::IrLoadError> for WorkerError {
    fn from(e: namir_ir::IrLoadError) -> Self {
        Self {
            code: e.code,
            detail: e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-ERR-020: conversion must preserve the specific reason, not flatten it.
    #[test]
    fn a_nam_load_failure_keeps_its_own_catalogue_id() {
        // `unwrap_err` needs `Debug` on the Ok side, which `PreparedNam` deliberately lacks.
        let Err(inner) = namir_nam::load(b"not a nam file") else {
            panic!("garbage bytes should not load");
        };
        let expected = inner.code.id;
        let converted: WorkerError = inner.into();
        assert_eq!(
            converted.code.id, expected,
            "conversion must not replace the specific nam.load.* id"
        );
        assert!(converted.code.id.starts_with("nam.load."));
    }
}
