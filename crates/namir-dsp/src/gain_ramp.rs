//! One-pole gain smoother (D-10.3: "gain-like parameters get a one-pole ramp"). Smooths in the
//! linear domain, not dB — `set_target_db` converts once so `process`'s per-sample hot path is a
//! multiply-add, never a `powf`/`log10` call.

use namir_core::{SampleRate, db_to_linear, linear_to_db};

/// FR-PARAM-040: "a full-range instantaneous change [shall] produce no output discontinuity
/// greater than that of a 20 ms linear ramp." A one-pole's steepest slope is its very first
/// sample after a target change, of size `coeff * range`; for a jump of `range` this must not
/// exceed an ideal 20 ms linear ramp's per-sample step of `range / (0.020 * sample_rate)`, i.e.
/// `coeff <= 1 / (0.020 * sample_rate)`. Since `coeff = 1 - exp(-1/tau_samples) < 1/tau_samples`
/// for any `tau_samples > 0`, a time constant of `tau_samples = 0.020 * sample_rate` — i.e.
/// `time_constant_ms = 20` — already clears the bound, but with very little margin (the two
/// sides differ by a second-order term). 25 ms is used here as this crate's documented default
/// to keep comfortable margin against `f32` rounding; see the `gain_ramp` tests for the
/// comparison this constant is chosen to satisfy. Test-only: a real caller (a future
/// `namir-engine` stage) picks its own time constant per parameter; this crate does not impose
/// a default via its public API, per the "no speculative abstraction" house rule.
#[cfg(test)]
const RECOMMENDED_TIME_CONSTANT_MS: f32 = 25.0;

/// A minimum floor for `time_constant_ms`, so a pathological (zero or negative) caller value
/// cannot produce a negative or exploding one-pole coefficient. Not itself an FRS figure — just
/// a safety net, per this crate's "clamp, don't fail" rule.
const MIN_TIME_CONSTANT_MS: f32 = 1e-3;

/// A one-pole gain smoother; see this module's doc comment for why the smoothing runs in the
/// linear rather than dB domain.
pub struct GainRamp {
    /// Current linear gain.
    current: f32,
    /// Target linear gain, set (not recomputed per sample) by `set_target_db`.
    target: f32,
    /// One-pole coefficient derived from `time_constant_ms` and the sample rate.
    coeff: f32,
}

impl GainRamp {
    /// Starts at unity gain (1.0 linear, 0 dB). `time_constant_ms` is clamped to
    /// `MIN_TIME_CONSTANT_MS`; see `RECOMMENDED_TIME_CONSTANT_MS`'s doc for the FR-PARAM-040
    /// constraint that should drive the caller's choice of value.
    pub fn new(sample_rate: SampleRate, time_constant_ms: f32) -> Self {
        let time_constant_ms = time_constant_ms.max(MIN_TIME_CONSTANT_MS);
        let tau_samples = (time_constant_ms as f64 / 1000.0) * sample_rate.hz_f64();
        let coeff = (1.0 - (-1.0 / tau_samples).exp()) as f32;
        Self {
            current: 1.0,
            target: 1.0,
            coeff,
        }
    }

    /// Converts `db` to linear once and stores it as the one-pole target — the whole point of
    /// doing this here rather than in `process` is that the per-sample path never calls
    /// `db_to_linear`/`powf`.
    pub fn set_target_db(&mut self, db: f32) {
        self.target = db_to_linear(db);
    }

    /// Smooths towards `target` in the linear domain and applies the running gain to `buf`.
    /// Allocates nothing.
    pub fn process(&mut self, buf: &mut [f32]) {
        for x in buf.iter_mut() {
            self.current += self.coeff * (self.target - self.current);
            *x *= self.current;
        }
    }

    /// The current linear gain, converted to dB for display/metering.
    pub fn current_db(&self) -> f32 {
        linear_to_db(self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;

    fn sr(hz: u32) -> SampleRate {
        SampleRate::new(hz).unwrap()
    }

    #[test]
    fn converges_to_target_after_several_time_constants() {
        let mut ramp = GainRamp::new(sr(48_000), RECOMMENDED_TIME_CONSTANT_MS);
        ramp.set_target_db(-12.0);
        let mut buf = vec![1.0f32; 48_000]; // 1 s, many time constants at 25 ms.
        ramp.process(&mut buf);
        assert!(
            (ramp.current_db() - (-12.0)).abs() < 0.1,
            "current_db={}",
            ramp.current_db()
        );
    }

    #[test]
    fn full_range_jump_is_no_worse_than_a_20ms_linear_ramp() {
        let sample_rate = 48_000u32;
        let mut ramp = GainRamp::new(sr(sample_rate), RECOMMENDED_TIME_CONSTANT_MS);

        // Settle at -60 dB first.
        ramp.set_target_db(-60.0);
        let mut settle = vec![1.0f32; 48_000];
        ramp.process(&mut settle);

        // Full-range instantaneous jump to 0 dB.
        ramp.set_target_db(0.0);
        let mut buf = vec![1.0f32; 4800]; // 100 ms, comfortably longer than the transient.
        ramp.process(&mut buf);

        // Include the transition from the settled pre-jump level into the first post-jump
        // sample, since that is where the one-pole's steepest step occurs.
        let mut prev = db_to_linear(-60.0);
        let mut max_delta = 0.0f32;
        for &s in &buf {
            max_delta = max_delta.max((s - prev).abs());
            prev = s;
        }

        let range = db_to_linear(0.0) - db_to_linear(-60.0);
        let ideal_max_delta = range / (0.020 * sample_rate as f32);

        assert!(
            max_delta <= ideal_max_delta * 1.01,
            "max_delta={max_delta} exceeds the 20 ms linear ramp bound {ideal_max_delta}"
        );
    }

    #[test]
    fn process_does_not_allocate() {
        let mut ramp = GainRamp::new(sr(48_000), RECOMMENDED_TIME_CONSTANT_MS);
        ramp.set_target_db(-6.0);
        let mut buf = [1.0f32; 128];
        audio_section(|| ramp.process(&mut buf));
    }
}
