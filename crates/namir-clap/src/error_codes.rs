//! Local error catalogue for `namir-clap` (D-16.1) — failure modes that belong to the CLAP
//! adapter itself, distinct from a parse failure (already carries a precise `nam.load.*`/
//! `ir.load.*`/... id from the crate that detected it, passed through unchanged) or a
//! `namir-worker` orchestration failure (`worker.*`).

use namir_core::{ErrorCode, Severity};

/// The GUI asked to (re)scan the library, but this process has no library service: either
/// `namir_platform::config_dir()` returned `None` (an environment this crate has no per-user
/// directory convention for) or, more commonly today, no library roots have ever been
/// configured — `namir-ui`'s `UiIntent` set does not yet offer a way to add one (see
/// `crate::shared`'s module doc comment).
pub const LIBRARY_UNAVAILABLE: ErrorCode = ErrorCode {
    id: "clap.library.unavailable",
    severity: Severity::Warning,
    message_template: "The library scan could not start: {detail}.",
};

/// The host handed `set_parent` a window handle that could not be interpreted as valid — see
/// `crate::gui`'s written safety argument for the trust boundary this crosses.
pub const GUI_INVALID_PARENT: ErrorCode = ErrorCode {
    id: "clap.gui.invalid_parent",
    severity: Severity::Error,
    message_template: "The host-supplied editor window handle was not usable.",
};

/// The host presented a sample rate this crate could not represent (zero, or non-finite/negative
/// once rounded) — `namir_core::SampleRate` requires a nonzero `u32`.
pub const INVALID_SAMPLE_RATE: ErrorCode = ErrorCode {
    id: "clap.activate.invalid_sample_rate",
    severity: Severity::Error,
    message_template: "The host presented an unusable sample rate ({sample_rate_hz}).",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_has_no_duplicate_or_empty_ids() {
        namir_core::assert_unique_ids(&[
            LIBRARY_UNAVAILABLE,
            GUI_INVALID_PARENT,
            INVALID_SAMPLE_RATE,
        ]);
    }
}
