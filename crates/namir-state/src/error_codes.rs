//! Local error catalogue for `namir-state` (D-16.1: `ErrorCode` is a shared *type*, not a closed
//! enum, so each crate owns its own consts rather than pushing them up into `namir-core`),
//! namespaced `state.*`.
//!
//! Every way a state/preset document can fail to load maps to exactly one of these — parsing
//! (`document`/`format`), a parameter that doesn't apply cleanly (`param`), or (once `resolve.rs`
//! lands, later in M5) a file reference that doesn't resolve (`reference`). Warnings (D-11.2's
//! tolerance rules) and errors (documents that cannot be loaded at all) share the same
//! `ErrorCode` type; `Severity` is what tells them apart, matching `namir_core::Severity`'s own
//! ordering.
//!
//! Grows incrementally with the rest of this crate rather than declaring every planned code up
//! front: an unused `pub const` in a private module is genuinely dead code (nothing outside this
//! crate can name it either, since `error_codes` itself is not `pub mod`), and this codebase
//! treats that as a defect to fix, not a lint to silence.

use namir_core::{ErrorCode, Severity};

/// The document is not valid JSON at all. The one rejection every other check in this crate
/// happens downstream of.
pub const MALFORMED_JSON: ErrorCode = ErrorCode::new(
    "state.document.malformed_json",
    Severity::Error,
    "This file is not a valid Namir state document ({detail}).",
);

/// NFR-SEC-020: the document (or an embedded resource inside it, FR-STATE-080) exceeded the
/// documented byte ceiling and was rejected before any allocation proportional to its declared
/// size was attempted.
pub const DOCUMENT_TOO_LARGE: ErrorCode = ErrorCode::new(
    "state.document.too_large",
    Severity::Error,
    "This state document is larger than the {limit_mb} MB limit.",
);

/// D-11.2's first tolerance rule: a key in the `parameters` block that no `ParamDescriptor` in
/// `namir_params::REGISTRY` claims. Carried in the document's carrier and written back verbatim
/// (a newer Namir's parameter surviving an older one's round trip), but never applied to the
/// chain — this build has nothing to apply it *as*.
pub const UNKNOWN_PARAMETER: ErrorCode = ErrorCode::new(
    "state.param.unknown_key",
    Severity::Warning,
    "The parameter \"{key}\" is not recognised by this build and was left \
                        unchanged.",
);

/// A recognised parameter whose stored value falls outside its descriptor's range — clamped,
/// not rejected, per D-11.2's tolerant-loading intent extended to values, not only keys.
pub const PARAMETER_OUT_OF_RANGE: ErrorCode = ErrorCode::new(
    "state.param.out_of_range",
    Severity::Warning,
    "The parameter \"{key}\" had a value outside its valid range and was \
                        clamped to {clamped}.",
);

/// A recognised parameter's stored value could not be read as a finite number at all (wrong JSON
/// type, `NaN`, `Infinity`) — the descriptor's own default is used instead of clamping, since
/// there is no meaningful nearby value to clamp *to*.
pub const PARAMETER_INVALID: ErrorCode = ErrorCode::new(
    "state.param.invalid",
    Severity::Warning,
    "The parameter \"{key}\" had an invalid value and was reset to its \
                        default.",
);

/// FR-STATE-070: none of a `FileRef`'s three resolution candidates (library-relative path,
/// absolute path, content-hash search) located the file. Produced by `resolve::MissingFile`'s
/// own `warning()` method, not by `resolve()` itself — it is a caller's choice whether a missing
/// reference is worth surfacing this way, not something forced on every `Resolution::Missing`.
pub const REFERENCE_NOT_FOUND: ErrorCode = ErrorCode::new(
    "state.reference.not_found",
    Severity::Warning,
    "The file \"{display_name}\" (hash {hash}) could not be found.",
);

/// D-11.1: `format_version` is the one field this format treats as non-negotiable — a document
/// missing it, or carrying a non-integer value for it, is not a Namir document at all rather
/// than an old or malformed one.
pub const MISSING_FORMAT_VERSION: ErrorCode = ErrorCode::new(
    "state.format.missing_version",
    Severity::Error,
    "This file has no format_version field and cannot be identified as a Namir \
                        state document.",
);

/// D-11.2: a document from a build newer than this one loads tolerantly rather than being
/// rejected — "a project saved by a newer Namir and opened by an older one does not silently
/// lose settings on the next save". A warning, not an error: the load proceeds.
pub const NEWER_FORMAT_VERSION: ErrorCode = ErrorCode::new(
    "state.format.newer",
    Severity::Warning,
    "This document was saved by a newer version of Namir (format_version \
                        {found}, this build understands up to {understood}). Some settings may \
                        not be applied.",
);

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[ErrorCode] = &[
        MALFORMED_JSON,
        DOCUMENT_TOO_LARGE,
        UNKNOWN_PARAMETER,
        PARAMETER_OUT_OF_RANGE,
        PARAMETER_INVALID,
        REFERENCE_NOT_FOUND,
        MISSING_FORMAT_VERSION,
        NEWER_FORMAT_VERSION,
    ];

    /// FR-ERR-020: every user-visible error maps to exactly one catalogue entry, verified
    /// statically. Same check every other crate's catalogue carries.
    #[test]
    fn catalogue_ids_are_unique_and_namespaced() {
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i + 1) {
                assert_ne!(a.id, b.id, "duplicate catalogue id {}", a.id);
            }
            assert!(a.id.starts_with("state."), "{} is not namespaced", a.id);
        }
    }
}
