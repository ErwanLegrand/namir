//! DC-blocking high-pass filter (FR-IN-040): "A DC-blocking high-pass filter, corner no higher
//! than 20 Hz, shall be applicable at the input." Standard 1-pole DC blocker:
//! `y[n] = x[n] - x[n-1] + r*y[n-1]`.

use namir_core::SampleRate;

/// `r` is clamped to this range as a safety net against a pathological `corner_hz` (e.g. zero,
/// negative, or far above Nyquist) — FR-IN-040's "no higher than 20 Hz" at typical sample rates
/// already gives an `r` very close to 1, well inside this range.
const MIN_R: f64 = 0.9;
const MAX_R: f64 = 0.9999;

pub struct DcBlocker {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    pub fn new(sample_rate: SampleRate, corner_hz: f32) -> Self {
        let r = 1.0 - (2.0 * std::f64::consts::PI * corner_hz as f64 / sample_rate.hz_f64());
        let r = r.clamp(MIN_R, MAX_R);
        Self {
            r: r as f32,
            x1: 0.0,
            y1: 0.0,
        }
    }

    /// Applies the filter in place. Allocates nothing.
    pub fn process(&mut self, buf: &mut [f32]) {
        for x in buf.iter_mut() {
            let xi = *x;
            let y = xi - self.x1 + self.r * self.y1;
            self.x1 = xi;
            self.y1 = y;
            *x = y;
        }
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
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
    fn dc_input_settles_towards_zero() {
        let mut blocker = DcBlocker::new(sr(48_000), 20.0);
        let mut buf = vec![1.0f32; 48_000]; // 1 s of DC.
        blocker.process(&mut buf);
        // Many time constants in, DC should be attenuated by tens of dB.
        let tail_level = buf[47_999].abs();
        assert!(
            tail_level < 1e-3,
            "expected DC heavily attenuated, tail level = {tail_level}"
        );
    }

    #[test]
    fn one_hundred_hz_passes_with_little_attenuation() {
        let sample_rate = 48_000u32;
        let corner_hz = 10.0; // "no higher than 20 Hz".
        let mut blocker = DcBlocker::new(sr(sample_rate), corner_hz);

        let sr_hz = sample_rate as f64;
        let freq = 100.0;
        let cycles = 200;
        let period_samples = (sr_hz / freq).round() as usize;
        let total = period_samples * cycles;
        let settle = total / 2;

        let input: Vec<f32> = (0..total)
            .map(|n| (2.0 * std::f64::consts::PI * freq * n as f64 / sr_hz).sin() as f32)
            .collect();
        let mut output = input.clone();
        blocker.process(&mut output);

        let mut sum_in = 0.0f64;
        let mut sum_out = 0.0f64;
        for n in settle..total {
            sum_in += (input[n] as f64).powi(2);
            sum_out += (output[n] as f64).powi(2);
        }
        let rms_in = (sum_in / (total - settle) as f64).sqrt();
        let rms_out = (sum_out / (total - settle) as f64).sqrt();
        let attenuation_db = 20.0 * (rms_out / rms_in).log10();

        assert!(
            attenuation_db > -1.0,
            "expected only modest attenuation at 100 Hz, got {attenuation_db} dB"
        );
    }

    #[test]
    fn process_does_not_allocate() {
        let mut blocker = DcBlocker::new(sr(48_000), 20.0);
        let mut buf = [0.3f32; 128];
        audio_section(|| blocker.process(&mut buf));
    }
}
