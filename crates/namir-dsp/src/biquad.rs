//! Transposed Direct Form II biquad filter (D-9.9): "EQ uses transposed-direct-form-II biquads
//! with coefficient interpolation across the block rather than coefficient recalculation per
//! sample." Coefficient *design* uses the RBJ "Audio EQ Cookbook" formulas, computed in `f64`
//! (D-9.10: "coefficient computation ... must be done in `f64` even though processing is `f32`,
//! because shelf and peak coefficients at low frequencies and high sample rates lose significance
//! in `f32`"); per-sample *processing* is `f32` throughout.

use namir_core::SampleRate;

/// The five FR-EQ-010 band shapes this crate can design a biquad for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    LowPass,
    HighPass,
    LowShelf,
    HighShelf,
    Peaking,
}

/// Normalized biquad coefficients (`a0` divided out, so it is implicitly `1` and not stored).
/// Stored as `f32` — D-9.10's per-sample precision — even though `design` computes them in
/// `f64`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

/// Frequencies at or above Nyquist, or non-positive, have no meaningful digital-filter design;
/// clamped rather than made fallible so `design` is infallible at the type level (P1: this runs
/// on a worker computing coefficients for the RT thread to consume, but the type must not force
/// a `Result` onto a caller for an out-of-range UI value).
const MIN_FREQ_HZ: f64 = 1.0;
const NYQUIST_HEADROOM: f64 = 0.999;

/// A `Q` of exactly zero would divide by zero in `alpha = sin(w0) / (2*Q)`; floored to a small
/// positive value so `design` never produces NaN/inf coefficients.
const MIN_Q: f64 = 1e-3;

impl BiquadCoeffs {
    /// Unity passthrough: `y = x`.
    pub fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    /// Designs a biquad per the RBJ Audio EQ Cookbook formulas. `freq_hz` is clamped to
    /// `(MIN_FREQ_HZ, nyquist * NYQUIST_HEADROOM)` and `q` to `>= MIN_Q` before computing, so a
    /// caller can never produce an unstable or degenerate design (house rule: clamp, don't fail,
    /// for anything that could eventually run on the audio thread).
    ///
    /// Shelf filters use a fixed shelf slope `S = 1`, per this crate's brief.
    pub fn design(
        kind: FilterKind,
        freq_hz: f64,
        q: f64,
        gain_db: f64,
        sample_rate: SampleRate,
    ) -> Self {
        let nyquist = sample_rate.hz_f64() / 2.0;
        let freq_hz = freq_hz.clamp(MIN_FREQ_HZ, nyquist * NYQUIST_HEADROOM);
        let q = q.max(MIN_Q);

        let w0 = 2.0 * std::f64::consts::PI * freq_hz / sample_rate.hz_f64();
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);
        let a = 10f64.powf(gain_db / 40.0);

        let (b0, b1, b2, a0, a1, a2) = match kind {
            FilterKind::LowPass => {
                let b0 = (1.0 - cos_w0) / 2.0;
                let b1 = 1.0 - cos_w0;
                let b2 = (1.0 - cos_w0) / 2.0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterKind::HighPass => {
                let b0 = (1.0 + cos_w0) / 2.0;
                let b1 = -(1.0 + cos_w0);
                let b2 = (1.0 + cos_w0) / 2.0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterKind::Peaking => {
                let b0 = 1.0 + alpha * a;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 - alpha * a;
                let a0 = 1.0 + alpha / a;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha / a;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterKind::LowShelf => {
                let sqrt_a = a.sqrt();
                // Shelf slope S = 1 fixed, per this crate's brief.
                let alpha_s = sin_w0 * std::f64::consts::SQRT_2 / 2.0;
                let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha_s);
                let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha_s);
                let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha_s;
                let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha_s;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterKind::HighShelf => {
                let sqrt_a = a.sqrt();
                let alpha_s = sin_w0 * std::f64::consts::SQRT_2 / 2.0;
                let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha_s);
                let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha_s);
                let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha_s;
                let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha_s;
                (b0, b1, b2, a0, a1, a2)
            }
        };

        Self {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        }
    }

    /// `H(z=1)`, the DC gain, computable exactly from the (already `a0`-normalized) coefficients
    /// without any complex arithmetic. Used by tests to check filter-type-defining behaviour
    /// (see the module test doc) rather than re-deriving `H(z)` from the same formula that
    /// produced the coefficients. Test-only: no production code needs this.
    #[cfg(test)]
    fn dc_gain(&self) -> f32 {
        (self.b0 + self.b1 + self.b2) / (1.0 + self.a1 + self.a2)
    }

    /// `H(z=-1)`, the Nyquist gain. See `dc_gain`.
    #[cfg(test)]
    fn nyquist_gain(&self) -> f32 {
        (self.b0 - self.b1 + self.b2) / (1.0 - self.a1 + self.a2)
    }
}

/// A stateful TDF-II biquad: current coefficients (which may be mid-interpolation towards a
/// target set by `set_coeffs`), and the two state registers `s1`/`s2`.
pub struct Biquad {
    current: BiquadCoeffs,
    target: BiquadCoeffs,
    delta_b0: f32,
    delta_b1: f32,
    delta_b2: f32,
    delta_a1: f32,
    delta_a2: f32,
    /// Samples remaining in the current coefficient ramp; `0` means "not interpolating".
    remaining: u32,
    s1: f32,
    s2: f32,
}

impl Biquad {
    /// Starts at `BiquadCoeffs::identity()` with zeroed state.
    pub fn new() -> Self {
        let identity = BiquadCoeffs::identity();
        Self {
            current: identity,
            target: identity,
            delta_b0: 0.0,
            delta_b1: 0.0,
            delta_b2: 0.0,
            delta_a1: 0.0,
            delta_a2: 0.0,
            remaining: 0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// D-9.9: interpolate coefficients across the block rather than recalculating per sample.
    ///
    /// The per-sample delta is `(target - current) / ramp_samples`, computed here from the
    /// *current* (possibly still mid-interpolation) coefficients — not from the previous target —
    /// so retargeting mid-ramp starts a fresh straight line from wherever the filter currently is,
    /// with no discontinuity. `process` clamps exactly to `target` on the final step (see there),
    /// so repeated `f32` addition cannot leave a residual drift away from the requested target.
    ///
    /// `ramp_samples == 0` jumps immediately (used by tests to contrast against the ramped path;
    /// a real stage should always pass a full block length per D-9.9).
    pub fn set_coeffs(&mut self, target: BiquadCoeffs, ramp_samples: u32) {
        if ramp_samples == 0 {
            self.current = target;
            self.target = target;
            self.remaining = 0;
            self.delta_b0 = 0.0;
            self.delta_b1 = 0.0;
            self.delta_b2 = 0.0;
            self.delta_a1 = 0.0;
            self.delta_a2 = 0.0;
            return;
        }
        let n = ramp_samples as f32;
        self.delta_b0 = (target.b0 - self.current.b0) / n;
        self.delta_b1 = (target.b1 - self.current.b1) / n;
        self.delta_b2 = (target.b2 - self.current.b2) / n;
        self.delta_a1 = (target.a1 - self.current.a1) / n;
        self.delta_a2 = (target.a2 - self.current.a2) / n;
        self.target = target;
        self.remaining = ramp_samples;
    }

    /// Applies the filter in place. Allocates nothing (see `rt_harness` tests below).
    pub fn process(&mut self, buf: &mut [f32]) {
        for x in buf.iter_mut() {
            if self.remaining > 0 {
                self.remaining -= 1;
                if self.remaining == 0 {
                    // Land exactly on the target rather than asymptotically approaching it
                    // through accumulated f32 addition error.
                    self.current = self.target;
                } else {
                    self.current.b0 += self.delta_b0;
                    self.current.b1 += self.delta_b1;
                    self.current.b2 += self.delta_b2;
                    self.current.a1 += self.delta_a1;
                    self.current.a2 += self.delta_a2;
                }
            }

            let xi = *x;
            let c = &self.current;
            let y = c.b0 * xi + self.s1;
            self.s1 = c.b1 * xi - c.a1 * y + self.s2;
            self.s2 = c.b2 * xi - c.a2 * y;
            *x = y;
        }
    }

    /// Zeroes the TDF-II state registers. Does not touch coefficients or any in-progress
    /// interpolation.
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

impl Default for Biquad {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;

    /// A small deterministic PRNG (xorshift32) so noise-based tests are reproducible without
    /// pulling in a `rand` dependency for one test module.
    struct Xorshift32(u32);
    impl Xorshift32 {
        fn next_f32(&mut self) -> f32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            // Map to [-1, 1).
            (x as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    fn sr(hz: u32) -> SampleRate {
        SampleRate::new(hz).unwrap()
    }

    // --- DC / Nyquist gain algebra: the defining, formula-independent property of each filter
    // type. NOT validated by evaluating the same H(z) formula the coefficients came from — see
    // the module-level rationale for why that would be circular.

    #[test]
    fn low_pass_passes_dc_and_blocks_nyquist() {
        let c = BiquadCoeffs::design(FilterKind::LowPass, 1000.0, 0.707, 0.0, sr(48_000));
        assert!((c.dc_gain() - 1.0).abs() < 1e-3, "dc_gain={}", c.dc_gain());
        assert!(
            c.nyquist_gain().abs() < 1e-3,
            "nyquist_gain={}",
            c.nyquist_gain()
        );
    }

    #[test]
    fn high_pass_blocks_dc_and_passes_nyquist() {
        let c = BiquadCoeffs::design(FilterKind::HighPass, 1000.0, 0.707, 0.0, sr(48_000));
        assert!(c.dc_gain().abs() < 1e-3, "dc_gain={}", c.dc_gain());
        assert!(
            (c.nyquist_gain() - 1.0).abs() < 1e-3,
            "nyquist_gain={}",
            c.nyquist_gain()
        );
    }

    #[test]
    fn low_shelf_applies_gain_at_dc_and_flat_at_nyquist() {
        let gain_db = 9.0f32;
        let c = BiquadCoeffs::design(
            FilterKind::LowShelf,
            200.0,
            0.707,
            gain_db as f64,
            sr(48_000),
        );
        let dc_db = namir_core::linear_to_db(c.dc_gain());
        assert!(
            (dc_db - gain_db).abs() < 0.1,
            "dc_db={dc_db}, expected {gain_db}"
        );
        let nyq_db = namir_core::linear_to_db(c.nyquist_gain());
        assert!(nyq_db.abs() < 0.1, "nyquist_db={nyq_db}, expected ~0");
    }

    #[test]
    fn high_shelf_applies_gain_at_nyquist_and_flat_at_dc() {
        let gain_db = -12.0f32;
        let c = BiquadCoeffs::design(
            FilterKind::HighShelf,
            4000.0,
            0.707,
            gain_db as f64,
            sr(48_000),
        );
        let dc_db = namir_core::linear_to_db(c.dc_gain());
        assert!(dc_db.abs() < 0.1, "dc_db={dc_db}, expected ~0");
        let nyq_db = namir_core::linear_to_db(c.nyquist_gain());
        assert!(
            (nyq_db - gain_db).abs() < 0.1,
            "nyquist_db={nyq_db}, expected {gain_db}"
        );
    }

    #[test]
    fn peaking_is_flat_at_dc_and_nyquist() {
        let c = BiquadCoeffs::design(FilterKind::Peaking, 1000.0, 1.0, 12.0, sr(48_000));
        let dc_db = namir_core::linear_to_db(c.dc_gain());
        let nyq_db = namir_core::linear_to_db(c.nyquist_gain());
        assert!(dc_db.abs() < 0.1, "dc_db={dc_db}, expected ~0");
        assert!(nyq_db.abs() < 0.1, "nyquist_db={nyq_db}, expected ~0");
    }

    // --- Real simulation: drive an actual `Biquad`, not the coefficient formula.

    /// Runs a steady-state sine through a fresh `Biquad` at `coeffs`, discards a settling
    /// prefix, and returns 20*log10(rms_out / rms_in).
    fn measure_gain_db(coeffs: BiquadCoeffs, freq_hz: f64, sample_rate_hz: f64) -> f32 {
        let mut biquad = Biquad::new();
        biquad.set_coeffs(coeffs, 0);

        let cycles = 200;
        let period_samples = (sample_rate_hz / freq_hz).round() as usize;
        let total = period_samples * cycles;
        let settle = total / 2;

        let mut input = vec![0.0f32; total];
        for (n, s) in input.iter_mut().enumerate() {
            *s = (2.0 * std::f64::consts::PI * freq_hz * n as f64 / sample_rate_hz).sin() as f32;
        }
        let mut output = input.clone();
        biquad.process(&mut output);

        let mut sum_in = 0.0f64;
        let mut sum_out = 0.0f64;
        let mut count = 0usize;
        for n in settle..total {
            sum_in += (input[n] as f64).powi(2);
            sum_out += (output[n] as f64).powi(2);
            count += 1;
        }
        let rms_in = (sum_in / count as f64).sqrt();
        let rms_out = (sum_out / count as f64).sqrt();
        (20.0 * (rms_out / rms_in).log10()) as f32
    }

    #[test]
    fn peaking_boosts_by_the_requested_gain_at_center_frequency() {
        let freq = 1000.0;
        let gain_db = 10.0;
        let sr_hz = 48_000.0;
        let coeffs = BiquadCoeffs::design(
            FilterKind::Peaking,
            freq,
            1.0,
            gain_db as f64,
            sr(sr_hz as u32),
        );
        let measured = measure_gain_db(coeffs, freq, sr_hz);
        assert!(
            (measured - gain_db).abs() < 0.3,
            "measured={measured}, expected~{gain_db}"
        );
    }

    #[test]
    fn peaking_cuts_by_the_requested_gain_at_center_frequency() {
        let freq = 2000.0;
        let gain_db = -8.0;
        let sr_hz = 44_100.0;
        let coeffs = BiquadCoeffs::design(
            FilterKind::Peaking,
            freq,
            2.0,
            gain_db as f64,
            sr(sr_hz as u32),
        );
        let measured = measure_gain_db(coeffs, freq, sr_hz);
        assert!(
            (measured - gain_db).abs() < 0.3,
            "measured={measured}, expected~{gain_db}"
        );
    }

    // --- Stability sweep (FR-EQ-020).

    #[test]
    fn stable_across_a_wide_parameter_and_sample_rate_sweep() {
        let kinds = [
            FilterKind::LowPass,
            FilterKind::HighPass,
            FilterKind::LowShelf,
            FilterKind::HighShelf,
            FilterKind::Peaking,
        ];
        let freqs = [20.0, 100.0, 1000.0, 5000.0, 20_000.0];
        let qs = [0.2, 0.707, 1.0, 2.5, 5.0];
        let gains = [-15.0, 0.0, 15.0];
        let sample_rates = [44_100u32, 48_000, 96_000, 192_000];

        for &sample_rate in &sample_rates {
            let sr_v = sr(sample_rate);
            for &kind in &kinds {
                for &freq in &freqs {
                    for &q in &qs {
                        for &gain in &gains {
                            let coeffs = BiquadCoeffs::design(kind, freq, q, gain, sr_v);
                            let mut biquad = Biquad::new();
                            biquad.set_coeffs(coeffs, 0);
                            let mut rng = Xorshift32(0x1234_5678 ^ (sample_rate));
                            let mut buf = vec![0.0f32; 4096];
                            for s in buf.iter_mut() {
                                *s = rng.next_f32();
                            }
                            biquad.process(&mut buf);
                            for (i, s) in buf.iter().enumerate() {
                                assert!(
                                    s.is_finite() && s.abs() < 1000.0,
                                    "unstable: kind={kind:?} freq={freq} q={q} gain={gain} \
                                     sr={sample_rate} sample[{i}]={s}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // --- Coefficient interpolation is smooth, not stepped (D-9.9 / FR-EQ-030).

    fn max_abs_delta(buf: &[f32]) -> f32 {
        buf.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    }

    #[test]
    fn coefficient_ramp_is_smoother_than_an_instant_jump() {
        let sr_v = sr(48_000);
        let low = BiquadCoeffs::design(FilterKind::LowPass, 200.0, 0.707, 0.0, sr_v);
        let peak = BiquadCoeffs::design(FilterKind::Peaking, 5000.0, 1.0, 15.0, sr_v);
        let block = 64u32;

        // Ramped retarget.
        let mut ramped = Biquad::new();
        ramped.set_coeffs(low, 0);
        // Settle on a constant input first so the discontinuity we measure is purely from the
        // coefficient change, not from filter startup transients.
        let mut warm = vec![0.7f32; 256];
        ramped.process(&mut warm);
        ramped.set_coeffs(peak, block);
        let mut ramped_buf = vec![0.7f32; block as usize];
        ramped.process(&mut ramped_buf);
        let ramped_delta = max_abs_delta(&ramped_buf);

        // Instant jump, same starting conditions.
        let mut jumped = Biquad::new();
        jumped.set_coeffs(low, 0);
        let mut warm2 = vec![0.7f32; 256];
        jumped.process(&mut warm2);
        jumped.set_coeffs(peak, 0);
        let mut jumped_buf = vec![0.7f32; block as usize];
        jumped.process(&mut jumped_buf);
        let jumped_delta = max_abs_delta(&jumped_buf);

        assert!(
            ramped_delta < jumped_delta,
            "ramped_delta={ramped_delta} should be well below jumped_delta={jumped_delta}"
        );
        // The jump path must actually show a much larger single-sample discontinuity, or this
        // test isn't proving the ramp path does anything.
        assert!(
            jumped_delta > ramped_delta * 4.0,
            "jumped_delta={jumped_delta} not clearly larger than ramped_delta={ramped_delta}"
        );
    }

    // --- Identity.

    #[test]
    fn identity_passes_signal_through_unchanged() {
        let mut biquad = Biquad::new();
        let input = [0.1f32, -0.4, 0.9, -1.0, 0.0, 0.3];
        let mut buf = input;
        biquad.process(&mut buf);
        for (a, b) in input.iter().zip(buf.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    // --- RT safety.

    #[test]
    fn process_does_not_allocate() {
        let mut biquad = Biquad::new();
        let coeffs = BiquadCoeffs::design(FilterKind::Peaking, 1000.0, 1.0, 6.0, sr(48_000));
        biquad.set_coeffs(coeffs, 0);
        let mut buf = [0.5f32; 128];
        audio_section(|| biquad.process(&mut buf));
    }
}
