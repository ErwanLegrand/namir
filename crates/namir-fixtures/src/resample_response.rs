//! FR-NAM-060's measuring instrument: the magnitude frequency response of an arbitrary
//! sample-rate converter, treated as a black box behind a `&[f32] -> Vec<f32>` closure.
//!
//! Two crates own resamplers that FR-NAM-060's numbers apply to — `namir-engine`'s
//! `SlotResampler` around the NAM model (FR-NAM-050) and `namir-ir`'s `resample_mono` for IRs
//! loaded at a foreign rate (FR-IR-030, which imports FR-NAM-060's bar by reference). They are
//! different resampler *types* from `rubato` with different configuration knobs, so the one thing
//! that must not differ between them is the yardstick. Hence one instrument here rather than a
//! copy in each crate's test module.
//!
//! # Method
//!
//! The awkward part of measuring a resampler is that input and output live at different rates, so
//! "the same frequency" is a different FFT bin on each side — and a 100 dB stopband cannot be seen
//! at all through a window whose spectral leakage is at −60 dB. Both problems go away with
//! **coherent sampling**, which is what this module is built around:
//!
//! 1. Let `g = gcd(from_hz, to_hz)`. One period of `1/g` seconds is exactly `from_hz/g` input
//!    frames and `to_hz/g` output frames — both whole numbers. Take `P` such periods: the analysis
//!    window is `n_in = P·from_hz/g` input frames and `n_out = P·to_hz/g` output frames, and both
//!    windows have **the same bin spacing** `g/P` Hz. Bin `k` therefore means the same physical
//!    frequency on both sides, and a signal built from those bins is exactly periodic in both
//!    windows — so a plain rectangular window has *zero* leakage and the usable dynamic range is
//!    set by `f32` arithmetic (≈ −150 dB in practice), not by a window function. Every alias and
//!    image of an excited bin also lands exactly on a bin, since both rates are whole multiples of
//!    the bin spacing.
//! 2. The stimulus is a **periodic multitone**: every bin in the band of interest excited at equal
//!    magnitude with [Schroeder-style quadratic phases][schroeder] (deterministic, and a low crest
//!    factor so the resampler is driven well inside `f32`'s range), synthesized by inverse FFT and
//!    repeated so the resampler reaches steady state. The response is then read off *every* bin at
//!    once — `|Y[k]|/|X[k]|` measured against the input's own spectrum, not against the intended
//!    amplitudes — rather than at a handful of probe frequencies a smooth-looking response could
//!    hide a ripple between.
//! 3. **Passband ripple** is the worst `|20·log10 |Y[k]|/|X[k]||` over the excited band.
//!    **Stopband attenuation** comes from two runs: with the passband excited, whatever appears
//!    *above* the band edge is an image; with only the region between the band edge and the input
//!    Nyquist excited, whatever appears anywhere in the output is an alias. The worse of the two is
//!    reported.
//!
//! Both stopband figures are conservative rather than exact: with many bins excited at once,
//! several input bins can fold onto the same output bin, so a reported level can overstate the
//! contribution of any single one by the number of folds (a few dB at these ratios). It errs
//! toward failing a resampler that passes, never the reverse.
//!
//! The instrument is calibrated in the sense that matters: `namir-engine`'s own
//! `frequency_response_measurement_catches_an_undersized_antialias_filter` feeds it a deliberately
//! under-configured resampler and asserts it reports the failure.
//!
//! [schroeder]: M. R. Schroeder, "Synthesis of low-peak-factor signals and binary sequences with
//!     low autocorrelation", IEEE Trans. Inf. Theory, 1970.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// Target length of the analysis window in output frames; the real window is the nearest whole
/// number of `1/gcd`-second periods at or below it (never fewer than one). 16 384 puts the bin
/// spacing at a few Hz for every rate pair Namir can hit, which is finer than any feature of a
/// resampler's passband, and keeps one measurement in the low tens of milliseconds.
const WINDOW_TARGET_FRAMES: usize = 16_384;

/// How many copies of the periodic stimulus to feed. The measurement window is taken from the
/// middle of the output, so this only has to be enough for the resampler's start-up transient to
/// be over well before the window starts and for the window to end well before the last block.
const STIMULUS_PERIODS: usize = 4;

/// The response of one sample-rate conversion, in the two quantities FR-NAM-060 states. All
/// frequencies in Hz, all levels in dB.
#[derive(Debug, Clone, Copy)]
pub struct ResampleResponse {
    /// Worst absolute deviation from unity gain across the measured passband, in dB — the quantity
    /// FR-NAM-060 caps at 0.1. Always ≥ 0.
    pub ripple_db: f64,
    /// Frequency at which `ripple_db` was measured.
    pub ripple_at_hz: f64,
    /// Top of the measured passband: `min(20 kHz, band_edge)`, FR-NAM-060's "20 kHz or the Nyquist
    /// frequency, whichever is lower".
    pub passband_top_hz: f64,
    /// Number of bins the passband figure was measured at — the density of the sweep.
    pub passband_points: usize,
    /// Worst level surviving outside the passband, in dB relative to the stimulus (so a *negative*
    /// number, and FR-NAM-060's "at least 100 dB of stopband attenuation" is `<= -100.0`). `None`
    /// when the conversion has no out-of-band region at all — an up-conversion measured with its
    /// own band edge already at the input Nyquist has nothing that could alias or image.
    pub stopband_db: Option<f64>,
    /// Frequency at which `stopband_db` was measured (output-domain, i.e. where the alias landed).
    pub stopband_at_hz: Option<f64>,
    /// The analysis window's bin spacing, in Hz — the resolution of both figures above.
    pub bin_hz: f64,
}

impl ResampleResponse {
    /// One line naming both figures and where each was measured, for an assertion message: the
    /// point of measuring is that a reader sees the margin, not just pass/fail.
    pub fn summary(&self) -> String {
        let stop = match (self.stopband_db, self.stopband_at_hz) {
            (Some(db), Some(hz)) => format!("{db:.1} dB @ {hz:.0} Hz"),
            _ => "n/a (no out-of-band region)".to_string(),
        };
        format!(
            "ripple {:.6} dB @ {:.0} Hz (passband to {:.0} Hz, {} points, {:.2} Hz apart), \
             worst out-of-band {stop}",
            self.ripple_db,
            self.ripple_at_hz,
            self.passband_top_hz,
            self.passband_points,
            self.bin_hz,
        )
    }
}

/// Measures the magnitude response of the conversion `run` performs from `from_hz` to `to_hz`.
///
/// `band_edge_hz` is the Nyquist frequency of the **lowest** rate anywhere in the path being
/// measured — `min(from_hz, to_hz)/2` for a single conversion, but the *model* rate's Nyquist when
/// `run` is a round trip out to a model rate and back (where `from_hz == to_hz`, and the band edge
/// is not visible in either of them). Everything below it is passband; everything above it is what
/// the antialiasing filter must remove.
///
/// `run` is called two or three times with a stimulus signal at `from_hz` and must return the whole
/// converted signal at `to_hz`; it may buffer, delay or truncate freely, since the analysis window
/// is taken from the middle of what comes back. It must return at least twice the analysis window
/// (about `4 · WINDOW_TARGET_FRAMES` frames for a 1:1 ratio), which feeding it everything it is
/// given always achieves.
///
/// # Panics
///
/// If `run` returns too little to analyse, or returns a silent window — both of which are bugs in
/// the adapter rather than measurable responses, and both of which would otherwise be reported as
/// a spectacular (and spurious) frequency response.
pub fn measure<F>(from_hz: u32, to_hz: u32, band_edge_hz: f64, mut run: F) -> ResampleResponse
where
    F: FnMut(&[f32]) -> Vec<f32>,
{
    let window = CoherentWindow::new(from_hz, to_hz);
    let passband_top_hz = 20_000.0_f64.min(band_edge_hz);

    // --- Run 1: the passband excited, everything above the band edge silent. ------------------
    let pass_bins = window.bins_between(20.0, passband_top_hz, from_hz);
    assert!(
        !pass_bins.is_empty(),
        "{from_hz} -> {to_hz}: no passband bins below {passband_top_hz} Hz to measure"
    );
    let (x, y) = window.run_stimulus(&pass_bins, &mut run);

    let mut ripple_db = 0.0;
    let mut ripple_at_hz = 0.0;
    for &k in &pass_bins {
        let gain_db = 20.0 * (y[k] / x[k]).log10();
        if gain_db.abs() > ripple_db {
            ripple_db = gain_db.abs();
            ripple_at_hz = window.hz(k);
        }
    }

    let stimulus_level = mean(&pass_bins.iter().map(|&k| x[k]).collect::<Vec<_>>());
    let mut worst_out_of_band: Option<(f64, f64)> = None;
    let first_image_bin = window.bin_at_or_above(band_edge_hz * 1.001);
    for (k, &level) in y.iter().enumerate().skip(first_image_bin) {
        let db = 20.0 * (level / stimulus_level).log10();
        if worst_out_of_band.is_none_or(|(worst, _)| db > worst) {
            worst_out_of_band = Some((db, window.hz(k)));
        }
    }

    // --- Run 2: only the region the antialiasing filter must remove, excited. ------------------
    // Empty exactly when the input has no frequencies above the band edge to begin with, i.e. for
    // the up-conversion half of a pair, whose only out-of-band residue is run 1's images.
    let stop_bins =
        window.bins_between(band_edge_hz * 1.001, from_hz as f64 / 2.0 * 0.999, from_hz);
    if !stop_bins.is_empty() {
        let (x, y) = window.run_stimulus(&stop_bins, &mut run);
        let stimulus_level = mean(&stop_bins.iter().map(|&k| x[k]).collect::<Vec<_>>());
        for (k, &level) in y.iter().enumerate() {
            let db = 20.0 * (level / stimulus_level).log10();
            if worst_out_of_band.is_none_or(|(worst, _)| db > worst) {
                worst_out_of_band = Some((db, window.hz(k)));
            }
        }
    }

    ResampleResponse {
        ripple_db,
        ripple_at_hz,
        passband_top_hz,
        passband_points: pass_bins.len(),
        stopband_db: worst_out_of_band.map(|(db, _)| db),
        stopband_at_hz: worst_out_of_band.map(|(_, hz)| hz),
        bin_hz: window.bin_hz,
    }
}

/// The coherent analysis window this module's doc comment derives: `n_in` input frames and
/// `n_out` output frames spanning the same whole number of `1/gcd(from, to)`-second periods, and
/// therefore sharing one bin spacing.
struct CoherentWindow {
    n_in: usize,
    n_out: usize,
    bin_hz: f64,
}

impl CoherentWindow {
    fn new(from_hz: u32, to_hz: u32) -> Self {
        let gcd = gcd(from_hz as usize, to_hz as usize);
        let unit_in = from_hz as usize / gcd;
        let unit_out = to_hz as usize / gcd;
        let periods = (WINDOW_TARGET_FRAMES / unit_out).max(1);
        let n_out = periods * unit_out;
        Self {
            n_in: periods * unit_in,
            n_out,
            bin_hz: to_hz as f64 / n_out as f64,
        }
    }

    fn hz(&self, bin: usize) -> f64 {
        bin as f64 * self.bin_hz
    }

    fn bin_at_or_above(&self, hz: f64) -> usize {
        (hz / self.bin_hz).ceil().max(1.0) as usize
    }

    /// The bins from `low_hz` up to `high_hz`, clamped to what the **input** window can represent
    /// (nothing at or above the input Nyquist, nothing at DC). Deliberately not clamped to the
    /// output Nyquist: exciting input bins that lie above it is the whole of the stopband
    /// measurement, since they are exactly the frequencies that must not survive the conversion.
    fn bins_between(&self, low_hz: f64, high_hz: f64, from_hz: u32) -> Vec<usize> {
        let nyquist_bin = (from_hz as f64 / 2.0 / self.bin_hz).floor() as usize;
        let first = self.bin_at_or_above(low_hz);
        let last = ((high_hz / self.bin_hz).floor() as usize)
            .min(nyquist_bin.saturating_sub(1))
            .min(self.n_in / 2 - 1);
        if first > last {
            Vec::new()
        } else {
            (first..=last).collect()
        }
    }

    /// Feeds `run` a multitone exciting `bins`, returning the input and output magnitude spectra
    /// (both one-sided, both normalized so a bin's value is that partial's amplitude, so they are
    /// directly comparable across the two different window lengths).
    fn run_stimulus<F>(&self, bins: &[usize], run: &mut F) -> (Vec<f64>, Vec<f64>)
    where
        F: FnMut(&[f32]) -> Vec<f32>,
    {
        let period = multitone_period(self.n_in, bins);
        let mut stimulus = Vec::with_capacity(self.n_in * STIMULUS_PERIODS);
        for _ in 0..STIMULUS_PERIODS {
            stimulus.extend_from_slice(&period);
        }
        let out = run(&stimulus);
        assert!(
            out.len() >= 2 * self.n_out,
            "resampler under test returned {} frames, too few for a {}-frame analysis window",
            out.len(),
            self.n_out
        );
        // From the middle: past any start-up transient, and clear of whatever the adapter does at
        // the end (`namir-ir`'s `resample_mono`, for one, zero-pads its output to an exact length).
        let start = (out.len() - self.n_out) / 2;
        let window = &out[start..start + self.n_out];
        assert!(
            window.iter().any(|s| s.abs() > 1e-12),
            "resampler under test returned a silent analysis window"
        );
        (magnitude_spectrum(&period), magnitude_spectrum(window))
    }
}

/// One period of a multitone exciting exactly `bins`, each at unit magnitude, scaled to a peak of
/// 0.5. Phases are Schroeder's quadratic sequence: deterministic, and with a crest factor low
/// enough that a stimulus of thousands of partials still drives the resampler at a sane level
/// rather than as a giant impulse.
fn multitone_period(n: usize, bins: &[usize]) -> Vec<f32> {
    let mut spectrum = vec![Complex::new(0.0_f64, 0.0); n];
    let count = bins.len() as f64;
    for (i, &k) in bins.iter().enumerate() {
        let phase = std::f64::consts::PI * (i as f64) * (i as f64) / count;
        let value = Complex::from_polar(1.0, phase);
        spectrum[k] = value;
        spectrum[n - k] = value.conj();
    }
    FftPlanner::new().plan_fft_inverse(n).process(&mut spectrum);
    let peak = spectrum
        .iter()
        .fold(0.0_f64, |acc, c| acc.max(c.re.abs()))
        .max(f64::MIN_POSITIVE);
    spectrum
        .iter()
        .map(|c| (0.5 * c.re / peak) as f32)
        .collect()
}

/// One-sided magnitude spectrum, normalized so bin `k`'s value is the amplitude of the sinusoid at
/// that bin (the factor of two for the negative-frequency half included), which makes spectra of
/// two different window lengths directly comparable.
fn magnitude_spectrum(x: &[f32]) -> Vec<f64> {
    let n = x.len();
    let mut buffer: Vec<Complex<f64>> = x.iter().map(|&s| Complex::new(s as f64, 0.0)).collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut buffer);
    buffer[..=n / 2]
        .iter()
        .map(|c| 2.0 * c.norm() / n as f64)
        .collect()
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn gcd(a: usize, b: usize) -> usize {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instrument against a perfect converter: `from_hz == to_hz` and the identity function.
    /// Ripple must read as zero (to within `f32` round-trip noise), which is the floor everything
    /// else in this module is measured against.
    #[test]
    fn the_identity_conversion_measures_as_flat() {
        let response = measure(48_000, 48_000, 24_000.0, |input| input.to_vec());
        assert!(
            response.ripple_db < 1e-6,
            "identity should measure flat, got {}",
            response.summary()
        );
    }

    /// The instrument against a converter with a known, analytically-computable response: a
    /// one-pole low-pass `y[n] = y[n-1] + a·(x[n] - y[n-1])` at 48 kHz, whose magnitude response
    /// is `a / |1 - (1-a)·e^{-jω}|`. Measured ripple must equal that formula's own worst deviation
    /// over the same band, which no accident of windowing or normalization would reproduce.
    #[test]
    fn a_one_pole_low_pass_measures_as_its_analytic_response() {
        let a = 0.25_f64;
        let response = measure(48_000, 48_000, 24_000.0, |input| {
            let mut y = 0.0_f64;
            input
                .iter()
                .map(|&x| {
                    y += a * (x as f64 - y);
                    y as f32
                })
                .collect()
        });

        // The same filter's analytic worst |gain| in dB over the measured band.
        let mut analytic_worst_db: f64 = 0.0;
        let mut hz = 20.0;
        while hz <= response.passband_top_hz {
            let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
            let denominator = Complex::new(1.0, 0.0) - Complex::from_polar(1.0 - a, -w);
            let db = 20.0 * (a / denominator.norm()).log10();
            analytic_worst_db = analytic_worst_db.max(db.abs());
            hz += 1.0;
        }

        assert!(
            (response.ripple_db - analytic_worst_db).abs() < 0.05,
            "measured {:.4} dB against an analytic {analytic_worst_db:.4} dB — {}",
            response.ripple_db,
            response.summary()
        );
    }
}
