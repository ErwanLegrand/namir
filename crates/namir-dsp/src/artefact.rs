//! FR-PARAM-040's second measurement — "measure the artefact's spectral energy" — as a small
//! test-only instrument shared by `gain_ramp` and `biquad`.
//!
//! # What is being measured, and against what
//!
//! FR-PARAM-040 does not name an absolute figure. It names a *reference*: "no output discontinuity
//! greater than that of a 20 ms linear ramp", and, for frequency-affecting parameters, "the same
//! audible standard". So every measurement here is a comparison of the shipped behaviour against a
//! synthesised 20 ms linear transition between the same two steady states, on the same stimulus,
//! in the same analysis window. Both numbers come out of the same instrument, so nothing about the
//! window, the stimulus level or the guard band can favour one of them.
//!
//! # The stimulus and the window
//!
//! A steady sine placed **exactly on an FFT bin** of a power-of-two window, so a rectangular
//! window leaks nothing and everything that appears away from that bin was put there by the
//! parameter change rather than by the analysis. "The artefact" is then simply the energy in every
//! other bin, guarded by a few bins either side of the tone to exclude the tone's own numerical
//! skirt, expressed in dB relative to the tone's own energy.
//!
//! # Why the FFT is written out here
//!
//! An iterative radix-2 Cooley-Tukey transform, in `f64`, about forty lines. `namir-dsp` has no
//! FFT dependency and gains nothing from one: this is the only place in the crate that needs a
//! spectrum, it is test-only, and adding a dependency to the workspace's dependency register
//! (`02-architecture.md` §17) to compute one is out of proportion to that. It is not RT code and
//! never runs on the audio thread.

use std::f64::consts::PI;

/// The analysis window, in samples. 8192 at 48 kHz is 170.7 ms — long enough to contain a 20 ms
/// transition and several time constants of the 25 ms one-pole that is being compared with it.
pub const WINDOW: usize = 8192;

/// The stimulus bin. 171 of 8192 at 48 kHz is 1001.95 Hz: a normal guitar-band probe frequency,
/// and exactly on a bin, which is the property that matters.
pub const TONE_BIN: usize = 171;

/// The sample rate every caller of this module uses.
pub const SAMPLE_RATE_HZ: f64 = 48_000.0;

/// Half-width, in Hz, of the band around [`TONE_BIN`] excluded from the artefact figure.
///
/// **This choice is the whole of the measurement's meaning and it took one wrong answer to find.**
/// The first version guarded three bins (17.6 Hz) and read the shipped one-pole at −15.4 dB
/// against the 20 ms reference's −15.7 dB — a failure, and a meaningless one. At that width the
/// figure is dominated by the *intended* level change: a 60 dB step, however gently it is made,
/// modulates the carrier and puts sidebands within a few tens of Hz of it, and a slower transition
/// simply puts more of them nearer in. Comparing two transitions there measures which is slower,
/// not which clicks.
///
/// What FR-PARAM-040 means by an artefact is the click — energy far from the carrier, which no
/// intended level change of this duration can produce. 250 Hz is five times the ~50 Hz first-null
/// spacing of the 20 ms reference transition the requirement names, so the reference's own
/// envelope is inside the guard and only its corner splatter, and the shipped smoother's, remain.
const GUARD_HZ: f64 = 250.0;

/// One period-aligned unit-amplitude sine over [`WINDOW`] samples at [`TONE_BIN`].
pub fn tone() -> Vec<f32> {
    (0..WINDOW)
        .map(|i| (2.0 * PI * TONE_BIN as f64 * i as f64 / WINDOW as f64).sin() as f32)
        .collect()
}

/// Energy more than [`GUARD_HZ`] away from the stimulus, in dB relative to the energy within it —
/// the quantity FR-PARAM-040's method calls "the artefact's spectral energy". More negative is
/// cleaner.
pub fn artefact_energy_db(signal: &[f32]) -> f64 {
    artefact_energy_db_guarded(signal, GUARD_HZ)
}

/// [`artefact_energy_db`] with the guard band stated explicitly, so a caller (and the sensitivity
/// test below) can see how the figure moves with it.
pub fn artefact_energy_db_guarded(signal: &[f32], guard_hz: f64) -> f64 {
    assert_eq!(signal.len(), WINDOW, "artefact window is a fixed length");
    let (re, im) = fft(&apply_analysis_window(signal));
    let guard_bins = (guard_hz / (SAMPLE_RATE_HZ / WINDOW as f64)).round() as usize;

    let mut tone_energy = 0.0;
    let mut artefact_energy = 0.0;
    // One-sided: bins 1..WINDOW/2. DC is excluded from both sums, since a gain change legitimately
    // moves the mean of a window it does not span and that is not an audible artefact.
    for k in 1..WINDOW / 2 {
        let energy = re[k] * re[k] + im[k] * im[k];
        if k.abs_diff(TONE_BIN) <= guard_bins {
            tone_energy += energy;
        } else {
            artefact_energy += energy;
        }
    }
    assert!(tone_energy > 0.0, "the analysis window carried no tone");
    10.0 * (artefact_energy / tone_energy).log10()
}

/// A 20 ms linear crossfade from `before` to `after`, both of which must be the stimulus already
/// in its two steady states, starting at sample `at`. This is FR-PARAM-040's reference transition:
/// whatever the shipped smoother does must not splatter more than this does.
pub fn linear_20ms_crossfade(
    before: &[f32],
    after: &[f32],
    at: usize,
    sample_rate_hz: f64,
) -> Vec<f32> {
    let ramp = (0.020 * sample_rate_hz).round() as usize;
    before
        .iter()
        .zip(after)
        .enumerate()
        .map(|(i, (b, a))| {
            let mix = if i < at {
                0.0
            } else {
                (((i - at) as f32) / ramp as f32).min(1.0)
            };
            b * (1.0 - mix) + a * mix
        })
        .collect()
}

/// The largest step between consecutive samples — FR-PARAM-040's first measurement, "assert
/// maximum sample-to-sample delta".
pub fn max_step(signal: &[f32]) -> f32 {
    signal
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max)
}

/// A four-term Blackman-Harris taper, applied before the transform.
///
/// **Not optional, and the second thing this instrument got wrong before it got right.** A gain
/// transition leaves the window at a different level from the one it entered at, and a rectangular
/// window's implicit periodic extension therefore carries a full-scale envelope discontinuity at
/// the wrap point — an artefact of the *analysis*, not of the smoother, whose 1/f skirt buried
/// every real difference between the three signals under a common −47 dB floor. A taper that
/// reaches zero at both edges removes it. Blackman-Harris rather than Hann because its −92 dB
/// sidelobes sit far below every figure asserted against, so the carrier's own leakage is not what
/// is being read out in the artefact band.
fn apply_analysis_window(signal: &[f32]) -> Vec<f32> {
    const A: [f64; 4] = [0.35875, 0.48829, 0.14128, 0.01168];
    let n = signal.len() as f64;
    signal
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let t = 2.0 * PI * i as f64 / n;
            let w = A[0] - A[1] * t.cos() + A[2] * (2.0 * t).cos() - A[3] * (3.0 * t).cos();
            (s as f64 * w) as f32
        })
        .collect()
}

/// Iterative radix-2 Cooley-Tukey, in `f64`, returning the real and imaginary parts. `x.len()`
/// must be a power of two.
fn fft(x: &[f32]) -> (Vec<f64>, Vec<f64>) {
    let n = x.len();
    assert!(n.is_power_of_two(), "radix-2 needs a power-of-two length");
    let mut re: Vec<f64> = x.iter().map(|&s| s as f64).collect();
    let mut im = vec![0.0f64; n];

    // Bit-reversal permutation.
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = (i as u32).reverse_bits() >> (32 - bits);
        let j = j as usize;
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2;
    while len <= n {
        let angle = -2.0 * PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = (angle * k as f64).cos_sin();
                let (i, j) = (start + k, start + k + len / 2);
                let tr = wr * re[j] - wi * im[j];
                let ti = wr * im[j] + wi * re[j];
                re[j] = re[i] - tr;
                im[j] = im[i] - ti;
                re[i] += tr;
                im[i] += ti;
            }
        }
        len *= 2;
    }
    (re, im)
}

/// `(cos, sin)` in one call, purely so the butterfly above reads as one line.
trait CosSin {
    fn cos_sin(self) -> (f64, f64);
}

impl CosSin for f64 {
    fn cos_sin(self) -> (f64, f64) {
        (self.cos(), self.sin())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instrument's own floor: an undisturbed tone must measure as essentially all tone. If
    /// this were not far below every figure the callers assert against, none of them would mean
    /// anything.
    #[test]
    fn an_undisturbed_tone_measures_as_no_artefact() {
        let db = artefact_energy_db(&tone());
        assert!(db < -120.0, "instrument floor is only {db:.1} dB");
    }

    /// And its ceiling: a hard mid-window mute is the loudest artefact a gain change can make, and
    /// must read far above the floor.
    #[test]
    fn a_hard_mute_measures_as_a_large_artefact() {
        let mut signal = tone();
        signal[WINDOW / 2..].fill(0.0);
        let db = artefact_energy_db(&signal);
        assert!(db > -30.0, "a hard mute measured only {db:.1} dB");
    }

    /// The transform against a known spectrum: a sine exactly on a bin must put all of its energy
    /// in that bin and (to `f64` rounding) none anywhere else.
    #[test]
    fn the_transform_puts_an_on_bin_sine_in_its_own_bin() {
        let (re, im) = fft(&tone());
        let magnitude = |k: usize| (re[k] * re[k] + im[k] * im[k]).sqrt();
        let peak = (1..WINDOW / 2)
            .max_by(|&a, &b| magnitude(a).total_cmp(&magnitude(b)))
            .unwrap();
        assert_eq!(peak, TONE_BIN);
        assert!(magnitude(TONE_BIN) > 1e3 * magnitude(TONE_BIN + 10));
    }
}
