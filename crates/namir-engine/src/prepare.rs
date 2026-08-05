use namir_core::{ChannelConfig, ErrorCode, SampleRate};

use crate::error_codes;

/// Inputs to `StagePrep::prepare` (D-6.1): everything a stage needs to size its own allocations
/// once, up front, so `Stage::process` never needs to ask for more (P1).
///
/// Only these three fields have a concrete use yet — engine sample rate, the block-size ceiling
/// buffers get sized to (D-6.2), and the channel layout (FR-CHAIN-060). More may be added once a
/// real stage needs them; this is deliberately not padded out speculatively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrepareContext {
    sample_rate: SampleRate,
    max_block_size: usize,
    channel_config: ChannelConfig,
}

impl PrepareContext {
    /// Rejects a zero `max_block_size`. Unlike `SampleRate`, nothing at the type level rules
    /// this out, but every stage that sizes a scratch buffer to it needs it nonzero — checked
    /// once here rather than redundantly in every stage's own `prepare`.
    pub fn new(
        sample_rate: SampleRate,
        max_block_size: usize,
        channel_config: ChannelConfig,
    ) -> Result<Self, PrepareError> {
        if max_block_size == 0 {
            return Err(PrepareError {
                code: error_codes::MAX_BLOCK_SIZE_ZERO,
            });
        }
        Ok(Self {
            sample_rate,
            max_block_size,
            channel_config,
        })
    }

    /// The engine sample rate a stage's `prepare` should size itself for.
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// The block-size ceiling a stage's scratch buffers (D-6.2) should be sized to.
    pub fn max_block_size(&self) -> usize {
        self.max_block_size
    }

    /// The channel layout (FR-CHAIN-060) a stage's `prepare` should size itself for.
    pub fn channel_config(&self) -> ChannelConfig {
        self.channel_config
    }
}

/// Carries a `namir_core::ErrorCode` (D-16.1) rather than an ad hoc message. `prepare` runs on a
/// worker (D-6.1), off the audio thread, so formatting this is not a P1 concern — but it should
/// still resolve to the same catalogue-driven diagnostics as everything else (FR-ERR-020).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrepareError {
    /// Which catalogue entry this failure maps to.
    pub code: ErrorCode,
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.id, self.code.message_template)
    }
}

impl std::error::Error for PrepareError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_max_block_size_is_rejected() {
        let err = PrepareContext::new(SampleRate::new(48_000).unwrap(), 0, ChannelConfig::Stereo)
            .unwrap_err();
        assert_eq!(err.code.id, error_codes::MAX_BLOCK_SIZE_ZERO.id);
    }

    #[test]
    fn nonzero_max_block_size_is_accepted() {
        let ctx = PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, ChannelConfig::Stereo)
            .unwrap();
        assert_eq!(ctx.max_block_size(), 64);
        assert_eq!(ctx.channel_config(), ChannelConfig::Stereo);
        assert_eq!(ctx.sample_rate().hz(), 48_000);
    }

    #[test]
    fn display_includes_code_id() {
        let err = PrepareError {
            code: error_codes::MAX_BLOCK_SIZE_ZERO,
        };
        assert!(
            err.to_string()
                .contains("engine.prepare.max_block_size_zero")
        );
    }
}
