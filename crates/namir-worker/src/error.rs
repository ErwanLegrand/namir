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
        // `render`, not `message_template`: since M14 a template may carry one `{detail}`
        // placeholder, and printing it raw is issue #15's defect at a second layer.
        write!(f, "{}: {}", self.code.id, self.code.render(&self.detail))
    }
}

impl std::error::Error for WorkerError {}

/// **The inner id is passed through unchanged, not re-wrapped.** A `.nam` file that failed
/// validation already has a precise `nam.load.*` id naming the reason; replacing it with a generic
/// `worker.*` id would destroy exactly the specificity FR-ERR-020 exists to preserve, and the user
/// would be told "loading failed" instead of "this file declares an unsupported architecture".
impl From<namir_nam::NamLoadError> for WorkerError {
    fn from(e: namir_nam::NamLoadError) -> Self {
        // `e.detail`, **not** `e.to_string()` (issue #39). `NamLoadError`'s own `Display` is
        // `{id}: {template} ({detail})`, so storing it here put a fully-rendered notice into the
        // field whose doc comment two dozen lines up says it must never hold one — and every
        // renderer downstream then wrapped that in the same shape again. A human reading step 1 of
        // `docs/manual-tests/fr-ui-070-non-modal-error-notices.md` on 2026-08-27 saw
        // `nam.load.malformed_json: The model file is not valid JSON.` twice in one line, and that
        // doubling is what pushed the plugin's `Dismiss` button off a 960x640 editor.
        Self {
            code: e.code,
            detail: e.detail,
        }
    }
}

/// See the `NamLoadError` impl above — same rule, `ir.load.*` ids preserved.
impl From<namir_ir::IrLoadError> for WorkerError {
    fn from(e: namir_ir::IrLoadError) -> Self {
        Self {
            code: e.code,
            detail: e.detail,
        }
    }
}

/// M5: `library.rs` drives `namir-library`'s scan and store on this crate's pool, and both can
/// report a `library.*` id (a directory that couldn't be read, an index file that failed to
/// parse). Same pass-through rule as the two impls above — the specific `library.*` id survives.
impl From<namir_library::LibraryError> for WorkerError {
    fn from(e: namir_library::LibraryError) -> Self {
        Self {
            code: e.code,
            detail: e.detail,
        }
    }
}

/// The warning-side mirror of the impl above. `namir-library` splits errors and warnings into two
/// types with identical shape (see that crate's `error.rs`); this crate does not make the same
/// split (`JobResult::Loaded`'s own `warning: Option<WorkerError>` field already uses `WorkerError`
/// for a non-fatal condition, distinguished by the catalogue entry's own `Severity`), so both
/// convert to the same target type here.
impl From<namir_library::LibraryWarning> for WorkerError {
    fn from(w: namir_library::LibraryWarning) -> Self {
        Self {
            code: w.code,
            detail: w.detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #39, as a standing regression: a converted error's `detail` must be the inner
    /// error's own `detail`, never its rendered `Display`. Asserted against the *shape* rather
    /// than the text -- a detail that contains its own code id is, by this field's own doc
    /// comment, a rendered string in a field that must not hold one.
    #[test]
    fn conversion_stores_the_bare_detail_not_a_rendered_string() {
        let Err(inner) = namir_nam::load(b"not a nam file") else {
            panic!("garbage bytes should not load");
        };
        let expected_detail = inner.detail.clone();
        let converted: WorkerError = inner.into();
        assert_eq!(converted.detail, expected_detail);
        assert!(
            !converted.detail.contains(converted.code.id),
            "detail carries a rendered notice, not a bare reason: {:?}",
            converted.detail
        );
        // ... and therefore the code id and template appear exactly once in the rendered line.
        let rendered = converted.to_string();
        assert_eq!(
            rendered.matches(converted.code.id).count(),
            1,
            "the code id is rendered twice: {rendered:?}"
        );
    }

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
