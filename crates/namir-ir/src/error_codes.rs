//! Local error catalogue for `namir-ir`, following the pattern `namir_core::error`'s module doc
//! describes (D-16.1) and the same shape `namir-nam/src/error_codes.rs` uses: `ErrorCode` is a
//! shared *type*, not a closed enum, so each crate defines its own consts for its own failure
//! modes rather than pushing them up into `namir-core`.
//!
//! This catalogue exists to satisfy FR-IR-010's declared WAV support matrix and P6 ("untrusted
//! input is parsed in one hardened place per format, and that place is fuzzed"): every way WAV
//! bytes can fail to become a `PreparedIr` maps to exactly one of these stable ids, namespaced
//! `ir.load.*`.

use namir_core::{ErrorCode, Severity};

/// The bytes handed to `wav::decode` are not a well-formed WAV file at all: a bad RIFF/WAVE
/// header, a truncated or inconsistent chunk, or an I/O error while reading declared data that
/// isn't actually present (including a `data` chunk whose declared size lies about what the byte
/// slice actually contains).
pub const MALFORMED_WAV: ErrorCode = ErrorCode::new(
    "ir.load.malformed_wav",
    Severity::Error,
    "The impulse response file is not a well-formed WAV file.",
    "Open the file in an audio editor and export it again as a WAV. A file that will not open \
     there either is damaged and should be downloaded again.",
);

/// The file's channel count is outside `1..=2` (mono/stereo only, FR-IR-010), or its
/// bit-depth/sample-format combination is not one of the four FR-IR-010 supports: 16-bit int,
/// 24-bit int, 32-bit int, 32-bit float.
pub const UNSUPPORTED_FORMAT: ErrorCode = ErrorCode::new(
    "ir.load.unsupported_format",
    Severity::Error,
    "This impulse response file's channel count or sample format is not supported.",
    "Export the impulse response as mono or stereo WAV at 16-, 24- or 32-bit integer, or 32-bit \
     float -- the four formats Namir reads.",
);

/// The file's sample rate is outside FR-IR-010's supported matrix, `8_000..=192_000` Hz.
pub const INVALID_SAMPLE_RATE: ErrorCode = ErrorCode::new(
    "ir.load.invalid_sample_rate",
    Severity::Error,
    "This impulse response file declares a sample rate outside the supported range.",
    "Export the impulse response at a sample rate between 8 kHz and 192 kHz; Namir resamples \
     anything in that range to your device's rate.",
);

/// The file declares zero audio frames — there is no impulse response to load at all.
pub const EMPTY_IR: ErrorCode = ErrorCode::new(
    "ir.load.empty_ir",
    Severity::Error,
    "This impulse response file contains no audio frames.",
    "The file has no audio in it. Export the impulse response again, or choose a different one in \
     the library.",
);

/// Carries a `namir_core::ErrorCode` (D-16.1) plus a `detail` string naming the specific reason.
/// This crate only ever sees bytes, not a file path, so `detail` carries whatever numbers/names
/// are relevant to the failure; a caller that knows the file path prepends it when presenting
/// this to a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrLoadError {
    /// Which catalogue entry this failure maps to.
    pub code: ErrorCode,
    /// The specific reason, e.g. `"channels = 5, supported range is 1..=2"`.
    pub detail: String,
}

impl std::fmt::Display for IrLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `render`, not `message_template`: since M14 a template may carry one `{detail}`
        // placeholder, and printing it raw is issue #15's defect at a second layer.
        write!(f, "{}: {}", self.code.id, self.code.render(&self.detail))
    }
}

impl std::error::Error for IrLoadError {}

#[cfg(test)]
const ALL: &[ErrorCode] = &[
    MALFORMED_WAV,
    UNSUPPORTED_FORMAT,
    INVALID_SAMPLE_RATE,
    EMPTY_IR,
];

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::assert_unique_ids;

    #[test]
    fn catalogue_ids_are_unique() {
        assert_unique_ids(ALL);
    }

    #[test]
    fn display_includes_code_id_and_detail() {
        let err = IrLoadError {
            code: MALFORMED_WAV,
            detail: "no RIFF tag found".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("ir.load.malformed_wav"));
        assert!(s.contains("no RIFF tag found"));
    }
}
