//! Local error catalogue for `namir-params`, following the pattern `namir_core::error`'s module
//! doc describes (D-16.1) and the same shape `namir-nam`/`namir-engine` use: `ErrorCode` is a
//! shared *type*, not a closed enum, so each crate defines its own consts for its own failure
//! modes rather than pushing them up into `namir-core`.
//!
//! Every way [`crate::check_manifest`] can reject a `params.lock` diff (FR-PARAM-020: "a checked-
//! in parameter manifest is diffed in CI; a changed or reused identifier fails the build") maps to
//! exactly one of these stable ids.

use namir_core::{ErrorCode, Severity};

/// A key that was live in the old manifest now derives a different id (D-10.2's derivation must
/// be pure; if this fires against a real diff, either the key text changed under a fixed id
/// expectation, or the derivation itself changed — both forbidden by FR-PARAM-020).
pub const ID_CHANGED: ErrorCode = ErrorCode::new(
    "params.manifest.id_changed",
    Severity::Error,
    "A parameter's identifier changed from its manifest entry.",
    "Restore the parameter's key text, or retire the old key with a tombstone and add the new one \
     beside it. Never edit a live line in params.lock by hand.",
);

/// An id that the old manifest already tombstoned (whether under the same key or, via an FNV
/// collision, a different one) appears live in the new descriptor set. FR-PARAM-020: "removed
/// ... shall have its identifier retired permanently, never reassigned."
pub const TOMBSTONE_REUSED: ErrorCode = ErrorCode::new(
    "params.manifest.tombstone_reused",
    Severity::Error,
    "A parameter reuses an identifier that was already tombstoned.",
    "Choose a different key. A tombstoned identifier is retired permanently, so that presets saved \
     by an older build cannot be read back as some other parameter.",
);

/// A key stayed live across old and new manifests but its kind shape (continuous vs. stepped)
/// changed. D-10.1: changing an existing entry's type fails the build; retiring it needs a
/// tombstone plus a new key instead.
pub const KIND_CHANGED: ErrorCode = ErrorCode::new(
    "params.manifest.kind_changed",
    Severity::Error,
    "A live parameter changed kind (continuous/stepped) in place.",
    "Retire the existing key with a tombstone and add a new key for the new kind, rather than \
     changing the existing entry in place.",
);

/// Two descriptors in the same new descriptor set derive the same id (an FNV-1a collision between
/// two distinct keys, or the same key declared twice under different constants).
pub const DUPLICATE_ID: ErrorCode = ErrorCode::new(
    "params.manifest.duplicate_id",
    Severity::Error,
    "Two parameters in the new descriptor set derive the same identifier.",
    "Rename one of the two keys. Two distinct keys deriving one identifier is an FNV collision, \
     and any spelling change to either key resolves it.",
);

/// The same key string appears more than once in the same new descriptor set.
pub const DUPLICATE_KEY: ErrorCode = ErrorCode::new(
    "params.manifest.duplicate_key",
    Severity::Error,
    "A key is declared more than once in the new descriptor set.",
    "Remove the duplicate descriptor from the registry; a key names one parameter.",
);

/// A key was live in the old manifest and is absent from the new descriptor set without ever
/// being tombstoned — a silent drop, which FR-PARAM-020 forbids ("never reassigned" presumes the
/// old identifier is still accounted for, not simply gone).
pub const DROPPED: ErrorCode = ErrorCode::new(
    "params.manifest.dropped",
    Severity::Error,
    "A parameter live in the old manifest is missing from the new descriptor set.",
    "Retire it with a tombstone entry instead of deleting its line, so its identifier stays \
     accounted for and can never be handed to a different parameter.",
);

/// A line in the old manifest text didn't parse as either a comment, a well-formed
/// `format_version <n>` line, or a well-formed `key id kind live|tombstoned <shape...>` data line,
/// every shape column being a `name=value` pair.
pub const MALFORMED_LINE: ErrorCode = ErrorCode::new(
    "params.manifest.malformed_line",
    Severity::Error,
    "A line in the manifest text could not be parsed.",
    "Restore params.lock from version control and regenerate it with `cargo run -p xtask -- \
     params-lock --write` rather than editing it by hand.",
);

/// The manifest declares a `format_version` this build cannot read: either newer than
/// [`crate::FORMAT_VERSION`], or not a number at all. Reported alone, because every other finding
/// under an unknown line grammar would be a guess — before this code existed, a future file was
/// reported as a pile of `MALFORMED_LINE`s instead.
pub const FORMAT_VERSION_UNSUPPORTED: ErrorCode = ErrorCode::new(
    "params.manifest.format_version_unsupported",
    Severity::Error,
    "params.lock declares a format version this build cannot read: {detail}.",
    "Build from a revision whose namir-params writes that format version. Do not regenerate the \
     file with an older build: that would overwrite a manifest written by newer tooling, tombstones \
     included. An *older* format version needs none of this -- it is migrated by `cargo run -p \
     xtask -- params-lock --write`.",
);

/// A descriptor in the new set contradicts itself: a default outside its own range, a stepped
/// default index past the end of its values, a non-finite bound. `ParamDescriptor::new` is a
/// `const fn` that accepts all of these, and since format version 2 the manifest records the range,
/// the default and the stepped-value fingerprint — so an inconsistent descriptor would be written
/// into the checked-in file as fact.
pub const INVALID_DESCRIPTOR: ErrorCode = ErrorCode::new(
    "params.manifest.invalid_descriptor",
    Severity::Error,
    "A parameter descriptor contradicts its own declared value space: {detail}.",
    "Correct the descriptor in its stage's module under crates/namir-params/src. A default has to \
     lie inside the range it belongs to, and a stepped default index has to index its own values \
     list.",
);

/// One diagnosed manifest problem: a catalogued [`ErrorCode`] plus a `detail` string naming the
/// specific key/id/line involved (mirrors `namir_nam::NamLoadError`'s shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestViolation {
    /// Which catalogue entry this violation maps to.
    pub code: ErrorCode,
    /// The specific key/id/line involved, e.g. `"key=gate.threshold, old_id=..., new_id=..."`.
    pub detail: String,
}

impl std::fmt::Display for ManifestViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `render`, not `message_template`: since M14 a template may carry one `{detail}`
        // placeholder, and printing it raw is issue #15's defect at a second layer.
        write!(f, "{}: {}", self.code.id, self.code.render(&self.detail))
    }
}

impl std::error::Error for ManifestViolation {}

#[cfg(test)]
const ALL: &[ErrorCode] = &[
    ID_CHANGED,
    TOMBSTONE_REUSED,
    KIND_CHANGED,
    DUPLICATE_ID,
    DUPLICATE_KEY,
    DROPPED,
    MALFORMED_LINE,
    FORMAT_VERSION_UNSUPPORTED,
    INVALID_DESCRIPTOR,
];

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::assert_unique_ids;

    #[test]
    fn catalogue_ids_are_unique() {
        assert_unique_ids(ALL);
    }
}
