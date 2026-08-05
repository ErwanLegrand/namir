//! Local error catalogue for `namir-engine`, following the pattern `namir_core::error`'s module
//! doc describes: `ErrorCode` is a shared *type*, not a closed enum, so this crate defines its
//! own consts rather than asking `namir-core` to know about engine-level failures.

use namir_core::{ErrorCode, Severity};

pub const MAX_BLOCK_SIZE_ZERO: ErrorCode = ErrorCode {
    id: "engine.prepare.max_block_size_zero",
    severity: Severity::Error,
    message_template: "Maximum block size must be greater than zero.",
};

// Only used by the uniqueness check below for now — will matter for real once a second code is
// added, but `assert_unique_ids` over a catalogue of one is still worth running (FR-ERR-020).
#[cfg(test)]
const ALL: &[ErrorCode] = &[MAX_BLOCK_SIZE_ZERO];

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::assert_unique_ids;

    #[test]
    fn catalogue_ids_are_unique() {
        assert_unique_ids(ALL);
    }
}
