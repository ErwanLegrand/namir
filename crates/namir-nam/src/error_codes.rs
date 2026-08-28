//! Local error catalogue for `namir-nam`, following the pattern `namir_core::error`'s module doc
//! describes (D-16.1) and the same shape `namir-engine/src/error_codes.rs` uses: `ErrorCode` is a
//! shared *type*, not a closed enum, so each crate defines its own consts for its own failure
//! modes rather than pushing them up into `namir-core`.
//!
//! This catalogue exists to satisfy FR-NAM-040 ("A model file that is malformed, truncated, of an
//! unknown architecture, or whose declared configuration is inconsistent with its weight count,
//! shall be rejected with a message naming ... the specific reason") and P6 ("untrusted input is
//! parsed in one hardened place per format, and that place is fuzzed"): every way `.nam` bytes can
//! fail to become a `PreparedNam` maps to exactly one of these stable ids.

use namir_core::{ErrorCode, Severity};

/// The bytes handed to `NamFile::parse` are not valid JSON at all (FR-NAM-040: "malformed").
pub const MALFORMED_JSON: ErrorCode = ErrorCode::new(
    "nam.load.malformed_json",
    Severity::Error,
    "The model file is not valid JSON.",
    "Re-export the model from the Neural Amp Modeler trainer. A `.nam` file is JSON text, so a \
     download that was truncated -- or an error page saved under the wrong name -- fails exactly \
     here.",
);

/// `architecture` is neither `"WaveNet"` nor `"LSTM"` (FR-NAM-040: "of an unknown architecture")
/// — the two names `model::load`'s dispatch and each architecture module's own `from_file` check
/// recognize. Both `wavenet::PreparedWaveNet::from_file` and `lstm::PreparedLstm::from_file` also
/// return this same code if handed a file whose `architecture` field doesn't match their own
/// expected name (e.g. an `LstmFile` sniffed as `"LSTM"` but whose `config` was somehow parsed
/// with `architecture: "WaveNet"` still set) — one catalogue entry for "this isn't the
/// architecture I was asked to load," not one per architecture module.
pub const UNSUPPORTED_ARCHITECTURE: ErrorCode = ErrorCode::new(
    "nam.load.unsupported_architecture",
    Severity::Error,
    "This model's architecture is not supported by this build of Namir.",
    "Load a WaveNet or LSTM model; those are the two Namir plays. Re-export the profile as one of \
     them, or choose a different model in the library.",
);

/// `config.head` is present (non-null). Ordinary exported WaveNet models leave it null; a
/// populated post-stack head config is a real NAM feature this crate does not implement, ported
/// as a scope limit directly from the S-1 spike (see `spikes/s1-nam-inference/src/lib.rs`).
pub const UNSUPPORTED_HEAD_CONFIG: ErrorCode = ErrorCode::new(
    "nam.load.unsupported_head_config",
    Severity::Error,
    "This model uses a post-stack head configuration, which is not supported.",
    "Load a model exported without a post-stack head. Re-exporting from the trainer with its \
     default settings produces one.",
);

/// A layer array's `activation` string is not one of `Tanh`, `ReLU`, `Sigmoid`, `Identity`.
pub const UNSUPPORTED_ACTIVATION: ErrorCode = ErrorCode::new(
    "nam.load.unsupported_activation",
    Severity::Error,
    "This model uses an activation function that is not supported.",
    "Load a model whose layers use Tanh, ReLU, Sigmoid or Identity -- the four Namir implements. \
     Re-export from the trainer with a standard activation.",
);

/// `config.layers` is empty — there is no WaveNet stack to build at all.
pub const EMPTY_LAYER_ARRAYS: ErrorCode = ErrorCode::new(
    "nam.load.empty_layer_arrays",
    Severity::Error,
    "This model declares no WaveNet layer arrays.",
    "This file has no model in it to play. Re-export it from the trainer, or choose a different \
     model in the library.",
);

/// A layer array's `condition_size != 1`. This implementation always feeds the raw mono input as
/// the sole conditioning signal (matching every real WaveNet export); a different declared
/// condition size isn't representable by this code and must be rejected cleanly rather than
/// silently misinterpreted (e.g. by reading past the intended condition data).
pub const UNSUPPORTED_CONDITION_SIZE: ErrorCode = ErrorCode::new(
    "nam.load.unsupported_condition_size",
    Severity::Error,
    "This model's conditioning signal size is not supported.",
    "Load a plain, non-parametric model. Namir feeds one mono signal, so a model expecting several \
     conditioning inputs has nothing here to drive them with.",
);

/// The flat `weights` array's length doesn't match what the declared config implies (FR-NAM-040:
/// "whose declared configuration is inconsistent with its weight count"), accounting for the
/// trailing `head_scale` float that may or may not be present as an extra element.
pub const WEIGHT_COUNT_MISMATCH: ErrorCode = ErrorCode::new(
    "nam.load.weight_count_mismatch",
    Severity::Error,
    "This model's weight count does not match its declared configuration.",
    "The file is damaged or was edited by hand. Download or export it again rather than repairing \
     it in a text editor.",
);

/// Adjacent layer arrays' `head_size`/`channels`/`input_size` don't chain correctly (see the S-1
/// spike's confirmed reading of `NeuralAmpModelerCore`'s `WaveNet`/`LayerArray` construction).
pub const LAYER_ARRAY_CHAINING_MISMATCH: ErrorCode = ErrorCode::new(
    "nam.load.layer_array_chaining_mismatch",
    Severity::Error,
    "This model's layer arrays do not chain together correctly.",
    "The file is damaged or was assembled by something other than the trainer. Export it again \
     from the Neural Amp Modeler trainer.",
);

/// A declared dimension (channels, head_size, input_size, condition_size, kernel_size,
/// dilations-per-array, layer array count, or total weight count) exceeds this crate's documented
/// ceiling (NFR-SEC-020). Checked *before* any arithmetic or allocation is derived from the
/// dimension, so a hostile file that declares e.g. `channels: 4_000_000_000` is rejected instantly
/// instead of causing a multi-gigabyte or overflowing allocation attempt.
pub const DIMENSION_LIMIT_EXCEEDED: ErrorCode = ErrorCode::new(
    "nam.load.dimension_limit_exceeded",
    Severity::Error,
    "This model declares a dimension larger than Namir's supported limit.",
    "Load a model of an ordinary size. A file declaring dimensions this large is damaged or was \
     not produced by the trainer at all -- treat one from an untrusted source with suspicion.",
);

/// `sample_rate` is present but zero. Distinct from `WEIGHT_COUNT_MISMATCH`: this is a
/// self-contained field-level problem, not a cross-field consistency problem.
pub const INVALID_SAMPLE_RATE: ErrorCode = ErrorCode::new(
    "nam.load.invalid_sample_rate",
    Severity::Error,
    "This model declares a sample rate of 0 Hz, which is not valid.",
    "Re-export the model with its sample rate recorded (48 kHz is the usual choice); Namir cannot \
     resample from a declared 0 Hz to your device's rate.",
);

/// An LSTM model's `input_size`, `in_channels`, or `out_channels` is not `1` — this
/// implementation always feeds the raw mono signal as the sole input and produces a single mono
/// output (matching every real non-parametric LSTM export), mirroring `UNSUPPORTED_CONDITION_SIZE`
/// for WaveNet: a different declared width isn't representable by this code and must be rejected
/// cleanly rather than silently misinterpreted (e.g. by reading past the intended input width).
pub const UNSUPPORTED_LSTM_CHANNELS: ErrorCode = ErrorCode::new(
    "nam.load.unsupported_lstm_channels",
    Severity::Error,
    "This LSTM model's input/output channel configuration is not supported.",
    "Load a plain, non-parametric LSTM model -- one mono input, one mono output. Re-export from \
     the trainer without the extra input channels.",
);

/// FR-NAM-140: the file is well-formed and its `architecture` is supported, but its `config` uses
/// a feature this build does not implement (D-9.12's core-A2 scope boundary): `condition_dsp`,
/// FiLM conditioning at any of the eight `*_film` sites, an active `head1x1`, an inactive
/// `layer1x1`, gating (`gating_mode` other than `"none"` or the legacy `gated: true`), a `groups_*`
/// value other than 1, or a `slimmable` container. `detail` names the offending key — that naming
/// is FR-NAM-140's own requirement text, not a courtesy.
///
/// Also the model's *output* width, added with issue #46: the last layer array's head width (A1's
/// `head_size`, A2's `head.out_channels`) must be 1. That is the same scope limit
/// `config.in_channels != 1` above already carries at the input end — Namir is a mono-in,
/// mono-out amp simulator — and it belongs to this code for the same reason `in_channels` does:
/// the reference implementation derives its output channel count from exactly that field
/// (`wave_net_output_channels`), so a wider value is a real, supported multi-output model this
/// build does not implement, not a damaged file. Left unchecked it was one of the two files that
/// loaded cleanly and then panicked *on the audio thread* inside
/// `wavenet::PreparedWaveNet::process_block`; the other, `layers[0].input_size != 1`, is
/// `INCONSISTENT_CONFIGURATION` below rather than this code, and that entry says why. **Distinct from `MALFORMED_JSON` by
/// construction**: reaching this code means `serde` already accepted the document as a `NamFile`,
/// so "not valid JSON" was never a true statement about it. This is FR-NAM-140's *configuration*
/// clause; `UNSUPPORTED_ARCHITECTURE` above remains its *architecture* clause.
pub const UNSUPPORTED_CONFIGURATION: ErrorCode = ErrorCode::new(
    "nam.load.unsupported_configuration",
    Severity::Error,
    "This model uses a configuration option that this build of Namir does not support.",
    "Load a model exported without that option; the detail above names which one. Re-exporting \
     from the trainer with default settings avoids all of them.",
);

/// A weight, the `head_scale`, or an activation parameter is not a finite number (infinite or
/// NaN). `serde_json` accepts `1e40` — in `f64` range, out of `f32` range — and hands back
/// `f32::INFINITY` with no error at all, so nothing before this check distinguishes such a file
/// from a good one. Rejecting it at load is not cosmetic: a non-finite weight propagates through
/// inference to a non-finite output on the **audio thread**, where FR-CHAIN-080/090's non-finite
/// guard then mutes the block — permanent silence plus a fault counter, and no message naming the
/// cause, which is exactly what FR-NAM-040 requires the load to have produced instead.
pub const NON_FINITE_VALUE: ErrorCode = ErrorCode::new(
    "nam.load.non_finite_value",
    Severity::Error,
    "This model contains a weight or parameter that is not a finite number.",
    "The file is damaged, or was exported by a trainer run that diverged. Re-export or download \
     the model again; a model with an infinite or NaN weight can only ever produce silence.",
);

/// The file is well-formed and every feature it uses is one this build supports, but its declared
/// configuration contradicts itself: both or neither of `kernel_size`/`kernel_sizes` present; a
/// `kernel_sizes` or per-layer `activation` array whose length disagrees with `dilations`; both or
/// neither of the nested `head` object and the legacy `head_size`/`head_bias` pair. Kept separate
/// from `UNSUPPORTED_CONFIGURATION`: "we don't support that" would be a false statement about a
/// file that is simply self-contradictory, not one that names a real, unimplemented feature. Not
/// required by FR-NAM-140's own text — added for message truthfulness, recorded here rather than
/// left implicit so a reviewer doesn't have to rediscover the reasoning.
///
/// Issue #47 adds one more member: `layers[0].input_size != 1`. The first layer array's
/// `input_size` is the width feeding its rechannel, and the signal fed to it is the model's own
/// input, whose width is `config.in_channels` — already pinned to 1 by
/// `UNSUPPORTED_CONFIGURATION` above. So this is a file disagreeing with itself about how wide
/// its own input is, not a multi-input model Namir is declining to play: a real one declares
/// `in_channels` as well, and is rejected above, by name, as the unsupported feature it is.
pub const INCONSISTENT_CONFIGURATION: ErrorCode = ErrorCode::new(
    "nam.load.inconsistent_configuration",
    Severity::Error,
    "This model's declared configuration is internally inconsistent.",
    "The file contradicts itself, so no other way of loading it will help. Export it again from \
     the trainer.",
);

/// Carries a `namir_core::ErrorCode` (D-16.1) plus a `detail` string naming the specific reason
/// (FR-NAM-040 requires the rejection message to name "the specific reason"). This crate only
/// ever sees bytes, not a file path, so `detail` carries whatever numbers/names are relevant to
/// the failure (e.g. `"expected 1234 weights, found 1200"`); a caller that knows the file path
/// prepends it when presenting this to a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamLoadError {
    /// Which catalogue entry this failure maps to.
    pub code: ErrorCode,
    /// The specific reason, e.g. `"expected 1234 weights, found 1200"`; see this struct's doc
    /// comment for what it does and does not carry.
    pub detail: String,
}

impl std::fmt::Display for NamLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `render`, not `message_template`: since M14 a template may carry one `{detail}`
        // placeholder, and printing it raw is issue #15's defect at a second layer.
        write!(f, "{}: {}", self.code.id, self.code.render(&self.detail))
    }
}

impl std::error::Error for NamLoadError {}

#[cfg(test)]
const ALL: &[ErrorCode] = &[
    MALFORMED_JSON,
    UNSUPPORTED_ARCHITECTURE,
    UNSUPPORTED_HEAD_CONFIG,
    UNSUPPORTED_ACTIVATION,
    EMPTY_LAYER_ARRAYS,
    UNSUPPORTED_CONDITION_SIZE,
    WEIGHT_COUNT_MISMATCH,
    LAYER_ARRAY_CHAINING_MISMATCH,
    DIMENSION_LIMIT_EXCEEDED,
    INVALID_SAMPLE_RATE,
    UNSUPPORTED_LSTM_CHANNELS,
    UNSUPPORTED_CONFIGURATION,
    INCONSISTENT_CONFIGURATION,
    NON_FINITE_VALUE,
];

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::assert_unique_ids;

    #[test]
    fn catalogue_ids_are_unique() {
        assert_unique_ids(ALL);
    }

    // trace: FR-NAM-040
    #[test]
    fn display_includes_code_id_and_detail() {
        let err = NamLoadError {
            code: MALFORMED_JSON,
            detail: "unexpected end of input".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("nam.load.malformed_json"));
        assert!(s.contains("unexpected end of input"));
    }
}
