use std::num::NonZeroU32;

/// An engine sample rate in Hz. Never zero — `namir-core` has "no logic" per D-5.1, but a rate
/// of 0 Hz is a type-level impossibility everywhere it appears, not a validation rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SampleRate(NonZeroU32);

impl SampleRate {
    pub fn new(hz: u32) -> Option<Self> {
        NonZeroU32::new(hz).map(Self)
    }

    pub fn hz(self) -> u32 {
        self.0.get()
    }

    pub fn hz_f64(self) -> f64 {
        f64::from(self.0.get())
    }
}

impl std::fmt::Display for SampleRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} Hz", self.hz())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_rejected() {
        assert_eq!(SampleRate::new(0), None);
    }

    #[test]
    fn round_trips_hz() {
        let sr = SampleRate::new(48_000).unwrap();
        assert_eq!(sr.hz(), 48_000);
        assert_eq!(sr.hz_f64(), 48_000.0);
    }

    #[test]
    fn displays_with_unit() {
        let sr = SampleRate::new(44_100).unwrap();
        assert_eq!(sr.to_string(), "44100 Hz");
    }
}
