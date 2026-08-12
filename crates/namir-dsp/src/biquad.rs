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
    /// Passes frequencies below the cutoff, attenuates above it.
    LowPass,
    /// Attenuates frequencies below the cutoff, passes above it.
    HighPass,
    /// Flat above the corner, applies `gain_db` below it.
    LowShelf,
    /// Flat below the corner, applies `gain_db` above it.
    HighShelf,
    /// Flat away from the center frequency, applies `gain_db` at it (bell curve).
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

    /// The two TDF-II state registers plus the coefficients currently applied, all of which
    /// FR-EQ-020's method asks be asserted finite ("asserting bounded output **and finite
    /// state**"). A filter can hand back one finite block while its own registers have already
    /// reached infinity; the next block is then entirely NaN, which is the failure a bounded-output
    /// check alone would not see until a block later. Test-only: no production code reads the
    /// registers.
    #[cfg(test)]
    fn state_is_finite(&self) -> bool {
        self.s1.is_finite()
            && self.s2.is_finite()
            && self.current.b0.is_finite()
            && self.current.b1.is_finite()
            && self.current.b2.is_finite()
            && self.current.a1.is_finite()
            && self.current.a2.is_finite()
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
    use crate::artefact;
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

    /// The six rates FR-EQ-020's method names by number: "44.1/48/88.2/96/176.4/192 kHz". Held as
    /// a named const rather than a literal inside the loop so the correspondence to the FRS line is
    /// checkable by eye; M14 added 88 200 and 176 400, which had simply never been in it.
    const FR_EQ_020_SAMPLE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

    // trace: FR-EQ-020
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

        for &sample_rate in &FR_EQ_020_SAMPLE_RATES {
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
                            // "and finite state": the registers the *next* block would be
                            // computed from, which a bounded-output check cannot see.
                            assert!(
                                biquad.state_is_finite(),
                                "non-finite state: kind={kind:?} freq={freq} q={q} gain={gain} \
                                 sr={sample_rate}"
                            );
                        }
                    }
                }
            }
        }
    }

    // --- FR-PARAM-040's second sentence: "Frequency-affecting parameters shall be smoothed or
    // their coefficients interpolated to the same audible standard."
    //
    // The standard the first sentence sets is a 20 ms linear ramp, so that is what the shipped
    // coefficient interpolation is measured against here: for each full-range change one of the
    // EQ's frequency-like parameters can make, the artefact energy of the real interpolation must
    // not exceed the artefact energy of a 20 ms linear transition between the same two steady
    // states. `namir-engine`'s `EqStage::retarget` ramps over `max_block_size` samples, so 64 —
    // the shortest block a host realistically asks for, and therefore the least favourable ramp
    // the shipped code ever runs.

    /// One full-range change of a frequency-affecting EQ parameter: the two designs and the
    /// FR-EQ-010/FR-IR-070 range endpoints they come from.
    struct FrequencyChange {
        label: &'static str,
        from: (FilterKind, f64, f64, f64),
        to: (FilterKind, f64, f64, f64),
    }

    /// Every frequency-like parameter `namir-params` declares for the EQ, driven end to end of
    /// its own FRS range. `eq.mid_q` is included because Q is `SmoothingCategory::FrequencyLike`
    /// too and travels the same `set_coeffs` path.
    const FREQUENCY_CHANGES: &[FrequencyChange] = &[
        FrequencyChange {
            label: "eq.mid_freq_hz 200 -> 5000 at Q 5.0, +15 dB",
            from: (FilterKind::Peaking, 200.0, 5.0, 15.0),
            to: (FilterKind::Peaking, 5_000.0, 5.0, 15.0),
        },
        FrequencyChange {
            label: "eq.mid_q 0.2 -> 5.0 at 1 kHz, +15 dB",
            from: (FilterKind::Peaking, 1_000.0, 0.2, 15.0),
            to: (FilterKind::Peaking, 1_000.0, 5.0, 15.0),
        },
        FrequencyChange {
            label: "eq.low_shelf_freq_hz 40 -> 500 at +15 dB",
            from: (FilterKind::LowShelf, 40.0, 0.707, 15.0),
            to: (FilterKind::LowShelf, 500.0, 0.707, 15.0),
        },
        FrequencyChange {
            label: "eq.high_shelf_freq_hz 1000 -> 12000 at -15 dB",
            from: (FilterKind::HighShelf, 1_000.0, 0.707, -15.0),
            to: (FilterKind::HighShelf, 12_000.0, 0.707, -15.0),
        },
        FrequencyChange {
            label: "eq.high_pass_freq_hz 20 -> 500",
            from: (FilterKind::HighPass, 20.0, 0.707, 0.0),
            to: (FilterKind::HighPass, 500.0, 0.707, 0.0),
        },
        FrequencyChange {
            label: "eq.low_pass_freq_hz 20000 -> 1000",
            from: (FilterKind::LowPass, 20_000.0, 0.707, 0.0),
            to: (FilterKind::LowPass, 1_000.0, 0.707, 0.0),
        },
    ];

    fn design(spec: (FilterKind, f64, f64, f64), sample_rate: SampleRate) -> BiquadCoeffs {
        BiquadCoeffs::design(spec.0, spec.1, spec.2, spec.3, sample_rate)
    }

    /// The steady-state response of `coeffs` to `artefact::tone()`, phase-aligned with it: the
    /// tone is periodic in the analysis window, so feeding three copies and keeping the last
    /// leaves the filter settled and the output aligned sample-for-sample with the input.
    fn steady_state(coeffs: BiquadCoeffs, tone: &[f32]) -> Vec<f32> {
        let mut biquad = Biquad::new();
        biquad.set_coeffs(coeffs, 0);
        let mut last = Vec::new();
        for _ in 0..3 {
            last = tone.to_vec();
            biquad.process(&mut last);
        }
        last
    }

    /// The response of a filter settled at `from` that is retargeted to `to` over `ramp_samples`
    /// at sample `at` of the analysis window. `ramp_samples == 0` is the unsmoothed control.
    fn transition(
        from: BiquadCoeffs,
        to: BiquadCoeffs,
        tone: &[f32],
        at: usize,
        ramp_samples: u32,
    ) -> Vec<f32> {
        let mut biquad = Biquad::new();
        biquad.set_coeffs(from, 0);
        for _ in 0..2 {
            let mut warm = tone.to_vec();
            biquad.process(&mut warm);
        }
        let mut out = tone.to_vec();
        biquad.process(&mut out[..at]);
        biquad.set_coeffs(to, ramp_samples);
        biquad.process(&mut out[at..]);
        out
    }

    /// The largest pole radius reached anywhere along the straight line `set_coeffs` walks from
    /// `from` to `to`. Sampled rather than solved: the interpolation is linear in the
    /// coefficients, so a dense sample of it is the interpolation.
    fn worst_pole_radius(from: BiquadCoeffs, to: BiquadCoeffs) -> f64 {
        let mut worst = 0.0f64;
        for i in 0..=1000 {
            let t = i as f64 / 1000.0;
            let a1 = from.a1 as f64 + t * (to.a1 as f64 - from.a1 as f64);
            let a2 = from.a2 as f64 + t * (to.a2 as f64 - from.a2 as f64);
            let discriminant = a1 * a1 - 4.0 * a2;
            let radius = if discriminant >= 0.0 {
                let root = discriminant.sqrt();
                ((-a1 + root) / 2.0).abs().max(((-a1 - root) / 2.0).abs())
            } else {
                a2.max(0.0).sqrt()
            };
            worst = worst.max(radius);
        }
        worst
    }

    /// FR-PARAM-040's two measurements, applied to the frequency-affecting parameters for the
    /// first time.
    ///
    /// **What this test asserts, and why it is not the 20 ms bound the gain half asserts.** Run,
    /// the measurements say the shipped one-block coefficient interpolation does *not* meet a
    /// 20 ms linear transition's standard on either quantity for the widest changes: at a
    /// 64-sample ramp the mid band swept 200 Hz → 5 kHz at Q 5 measures −31.8 dB of artefact
    /// energy against the reference's −71.9 dB, and 0.177 of peak sample-to-sample delta against
    /// the reference's 0.132. The mechanism is understood and is not a click: linear interpolation
    /// of `(a1, a2)` cannot leave the stability triangle, that region being convex, so the filter
    /// never rings away — instead its resonance *sweeps* across the band and releases its stored
    /// energy as a chirp. A longer ramp makes that worse, not better (−14.4 dB and 0.278 at 4096
    /// samples, the widest gap this test drives), which is the signature of a glide rather than of
    /// a discontinuity.
    ///
    /// So the numbers do not show a defect and they also do not show compliance: FR-PARAM-040's
    /// "the same audible standard" is not a figure, and a swept resonance is not the same
    /// phenomenon as the level step the first sentence bounds. Asserting the 20 ms figure here
    /// would be inventing a requirement and failing the product against it; asserting nothing
    /// would leave the clause where M14 found it. What is asserted is what the measurements
    /// establish without invention — the change is bounded, finite and stable throughout, and
    /// every figure is printed — and FR-PARAM-040's `uncovered:` field names the rest.
    // trace-partial: FR-PARAM-040
    // uncovered: FR-PARAM-040 — the second sentence's "frequency-affecting parameters ... to the
    // uncovered: same audible standard" is measured but not asserted, because the FRS states no
    // uncovered: figure for it and the measurement says the shipped one-block coefficient
    // uncovered: interpolation exceeds a 20 ms linear transition between the same two steady
    // uncovered: states by up to 57 dB of artefact energy and 2.1x of peak sample-to-sample delta
    // uncovered: on the widest EQ sweeps, a swept resonance not being the level step the first
    // uncovered: sentence bounds; closes M8
    #[test]
    fn full_range_frequency_changes_are_measured_against_the_20ms_linear_standard() {
        let sample_rate = sr(48_000);
        let tone = artefact::tone();
        let at = artefact::WINDOW / 2;
        // `EqStage::retarget` ramps over `max_block_size`; 64 is the shortest block a host
        // realistically asks for, and 4096 the longest, so both ends are driven.
        for &ramp_samples in &[64u32, 4096] {
            for change in FREQUENCY_CHANGES {
                let from = design(change.from, sample_rate);
                let to = design(change.to, sample_rate);

                let interpolated = transition(from, to, &tone, at, ramp_samples);
                let jumped = transition(from, to, &tone, at, 0);
                let reference = artefact::linear_20ms_crossfade(
                    &steady_state(from, &tone),
                    &steady_state(to, &tone),
                    at,
                    artefact::SAMPLE_RATE_HZ,
                );

                let after = at..artefact::WINDOW;
                println!(
                    "FR-PARAM-040 ramp={ramp_samples} {}: artefact interpolated {:.1} dB / \
                     20 ms reference {:.1} dB / unsmoothed jump {:.1} dB; max sample delta \
                     {:.5} / {:.5} / {:.5}",
                    change.label,
                    artefact::artefact_energy_db(&interpolated),
                    artefact::artefact_energy_db(&reference),
                    artefact::artefact_energy_db(&jumped),
                    artefact::max_step(&interpolated[after.clone()]),
                    artefact::max_step(&reference[after.clone()]),
                    artefact::max_step(&jumped[after]),
                );

                // The interpolation is walked in a straight line through coefficient space, and
                // the biquad stability region — |a2| < 1 and |a1| < 1 + a2 — is a convex triangle,
                // so a line between two stable designs cannot leave it. Asserted rather than
                // argued, because it is the property that makes everything above a sweep rather
                // than a divergence, and nothing else in the tree checks it.
                let radius = worst_pole_radius(from, to);
                assert!(
                    radius < 1.0,
                    "{}: the coefficient interpolation reaches a pole radius of {radius}",
                    change.label
                );

                // And the audible consequence of that: the transition stays bounded. The peak
                // overshoot measured is 1.44x the steady-state amplitude at the widest sweep, so
                // 4x is a generous ceiling that a divergence would still blow straight through.
                let peak = interpolated.iter().fold(0.0f32, |a, s| a.max(s.abs()));
                let steady_peak = steady_state(to, &tone)
                    .iter()
                    .fold(0.0f32, |a, s| a.max(s.abs()));
                assert!(
                    interpolated.iter().all(|s| s.is_finite()),
                    "{}: non-finite output during the change",
                    change.label
                );
                assert!(
                    peak <= steady_peak.max(1.0) * 4.0,
                    "{}: peaked at {peak} against a steady-state {steady_peak}",
                    change.label
                );
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
