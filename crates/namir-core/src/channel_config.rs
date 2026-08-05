/// The three channel configurations FR-CHAIN-060 requires the engine to support. NAM models are
/// inherently monophonic (FR-CHAIN-050's rationale), so `core_channels` is always 1 — that's a
/// property of every configuration, not a fourth thing to track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelConfig {
    Mono,
    MonoToStereo,
    Stereo,
}

impl ChannelConfig {
    pub const fn input_channels(self) -> u16 {
        match self {
            ChannelConfig::Mono => 1,
            ChannelConfig::MonoToStereo => 1,
            ChannelConfig::Stereo => 2,
        }
    }

    /// Always 1: the NAM core runs mono in every configuration (FR-CHAIN-060).
    pub const fn core_channels(self) -> u16 {
        1
    }

    pub const fn output_channels(self) -> u16 {
        match self {
            ChannelConfig::Mono => 1,
            ChannelConfig::MonoToStereo => 2,
            ChannelConfig::Stereo => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_is_one_in_one_out() {
        assert_eq!(ChannelConfig::Mono.input_channels(), 1);
        assert_eq!(ChannelConfig::Mono.output_channels(), 1);
    }

    #[test]
    fn mono_to_stereo_widens_on_output_only() {
        assert_eq!(ChannelConfig::MonoToStereo.input_channels(), 1);
        assert_eq!(ChannelConfig::MonoToStereo.output_channels(), 2);
    }

    #[test]
    fn stereo_is_two_in_two_out() {
        assert_eq!(ChannelConfig::Stereo.input_channels(), 2);
        assert_eq!(ChannelConfig::Stereo.output_channels(), 2);
    }

    #[test]
    fn core_is_always_mono() {
        for cfg in [
            ChannelConfig::Mono,
            ChannelConfig::MonoToStereo,
            ChannelConfig::Stereo,
        ] {
            assert_eq!(cfg.core_channels(), 1);
        }
    }
}
