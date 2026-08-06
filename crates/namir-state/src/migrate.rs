//! D-11.2's `format_version` gate, sitting between [`crate::Document::parse`] and
//! [`crate::State::from_document`] in [`crate::State::read`]'s pipeline (parse → migrate →
//! project).
//!
//! Three cases, per D-11.1's "the one field this format treats as non-negotiable" and D-11.2's
//! forward-compatibility intent:
//!
//! - absent, or present but not an integer → rejected outright
//!   ([`crate::error_codes::MISSING_FORMAT_VERSION`]) — a document with no identifiable version
//!   is not a Namir document at all, not an old or malformed one this crate should try to guess
//!   at.
//! - greater than [`crate::document::FORMAT_VERSION`] → loaded tolerantly, with a warning
//!   ([`crate::error_codes::NEWER_FORMAT_VERSION`]) rather than rejected — D-11.2's stated
//!   purpose is exactly this: "a project saved by a newer Namir and opened by an older one does
//!   not silently lose settings on the next save."
//! - less than or equal to [`crate::document::FORMAT_VERSION`] → loaded as-is. No migration
//!   exists yet, because `FORMAT_VERSION` is `1`, the format's own floor — there is nothing
//!   older to migrate *from*. This module is the seam a `format_version: 2` bump lands its first
//!   real migration step in, without redesigning [`crate::State::read`]'s pipeline to make room.

use crate::document::{Document, FORMAT_VERSION};
use crate::error::{StateError, StateWarning};
use crate::error_codes;

/// Checks `document`'s `format_version` per this module's doc comment and returns it unchanged
/// (no migration exists yet) alongside whatever warnings the check itself produced.
pub(crate) fn migrate(document: Document) -> Result<(Document, Vec<StateWarning>), StateError> {
    let version = document.format_version().ok_or_else(|| {
        StateError::new(
            error_codes::MISSING_FORMAT_VERSION,
            "no format_version field, or its value is not an unsigned integer",
        )
    })?;

    let mut warnings = Vec::new();
    if version > FORMAT_VERSION {
        warnings.push(StateWarning::new(
            error_codes::NEWER_FORMAT_VERSION,
            format!("found {version}, this build understands up to {FORMAT_VERSION}"),
        ));
    }

    Ok((document, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_at_the_current_format_version_produces_no_warning() {
        let (_, warnings) = migrate(Document::empty()).unwrap();
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_document_missing_format_version_is_rejected() {
        let doc = Document::parse(b"{}").unwrap();
        let err = migrate(doc).unwrap_err();
        assert_eq!(err.code.id, error_codes::MISSING_FORMAT_VERSION.id);
    }

    #[test]
    fn a_document_with_a_non_integer_format_version_is_rejected() {
        let doc = Document::parse(br#"{"format_version": "one"}"#).unwrap();
        let err = migrate(doc).unwrap_err();
        assert_eq!(err.code.id, error_codes::MISSING_FORMAT_VERSION.id);
    }

    /// D-11.2's stated purpose, proven directly: a newer document is not rejected.
    #[test]
    fn a_document_from_a_newer_build_loads_with_a_warning_rather_than_failing() {
        let doc = Document::parse(br#"{"format_version": 999}"#).unwrap();

        let (returned, warnings) = migrate(doc).unwrap();
        assert_eq!(returned.format_version(), Some(999));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.id, error_codes::NEWER_FORMAT_VERSION.id);
    }

    #[test]
    fn a_document_at_exactly_the_current_version_is_not_flagged_as_newer() {
        let doc = Document::parse(format!(r#"{{"format_version": {FORMAT_VERSION}}}"#).as_bytes())
            .unwrap();
        let (_, warnings) = migrate(doc).unwrap();
        assert!(warnings.is_empty());
    }
}
