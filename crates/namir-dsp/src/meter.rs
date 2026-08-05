//! Level metering (FR-IN-020/030, FR-OUT-020): peak, short-term average, latching peak-hold, and
//! a latching clip indicator.

use namir_core::{SampleRate, linear_to_db};

/// Time constant for both the peak follower's release and the short-term average's smoothing.
/// Not separately specified per-reading by the FRS; one shared, documented constant is simpler
/// than two unexplained numbers and both readings are meant to read "recent level", not "gate
/// timing" (which is `gate.rs`'s concern, not this one's).
const TIME_CONSTANT_MS: f64 = 300.0;

fn one_pole_coeff(time_constant_ms: f64, sample_rate_hz: f64) -> f32 {
    let tau_samples = (time_constant_ms / 1000.0) * sample_rate_hz;
    (1.0 - (-1.0 / tau_samples).exp()) as f32
}

pub struct Meter {
    release_coeff: f32,
    /// Fast-attack / slow-release peak follower, linear.
    peak: f32,
    /// Exponential moving average of `x^2`, linear power (its square root is an RMS-like
    /// reading).
    avg_sq: f32,
    /// Latched maximum of `peak`. FR-IN-020 requires this to latch for *at least* 1 second; the
    /// simplest implementation that trivially satisfies "at least" is to never decrease it at
    /// all except on an explicit `reset` — a real UI can still choose to visually decay its own
    /// display of this value after some elapsed time, but that's a presentation choice, not a
    /// measurement one, and out of scope for this primitive.
    peak_hold: f32,
    clipped: bool,
}

impl Meter {
    pub fn new(sample_rate: SampleRate) -> Self {
        Self {
            release_coeff: one_pole_coeff(TIME_CONSTANT_MS, sample_rate.hz_f64()),
            peak: 0.0,
            avg_sq: 0.0,
            peak_hold: 0.0,
            clipped: false,
        }
    }

    /// Updates peak, average, peak-hold and clip state from `buf`. Read-only over `buf` — a
    /// meter observes, it does not shape the signal.
    pub fn process(&mut self, buf: &[f32]) {
        for &x in buf {
            let abs_x = x.abs();

            // Fast attack (instantaneous jump to a new higher sample), slow exponential release.
            if abs_x > self.peak {
                self.peak = abs_x;
            } else {
                self.peak += self.release_coeff * (abs_x - self.peak);
            }

            self.avg_sq += self.release_coeff * (x * x - self.avg_sq);

            if self.peak > self.peak_hold {
                self.peak_hold = self.peak;
            }

            if abs_x >= 1.0 {
                self.clipped = true;
            }
        }
    }

    pub fn peak_db(&self) -> f32 {
        linear_to_db(self.peak)
    }

    pub fn average_db(&self) -> f32 {
        linear_to_db(self.avg_sq.sqrt())
    }

    pub fn peak_hold_db(&self) -> f32 {
        linear_to_db(self.peak_hold)
    }

    pub fn clipped(&self) -> bool {
        self.clipped
    }

    pub fn reset_clip(&mut self) {
        self.clipped = false;
    }

    /// Full reset: peak, average, peak-hold and the clip latch.
    pub fn reset(&mut self) {
        self.peak = 0.0;
        self.avg_sq = 0.0;
        self.peak_hold = 0.0;
        self.clipped = false;
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
    fn silence_reads_at_the_floor() {
        let mut meter = Meter::new(sr(48_000));
        let buf = vec![0.0f32; 1000];
        meter.process(&buf);
        let floor = linear_to_db(0.0);
        assert_eq!(meter.peak_db(), floor);
        assert_eq!(meter.average_db(), floor);
        assert_eq!(meter.peak_hold_db(), floor);
    }

    #[test]
    fn full_scale_sine_reads_near_0_dbfs_peak() {
        let mut meter = Meter::new(sr(48_000));
        let sr_hz = 48_000.0f64;
        let freq = 1000.0;
        let buf: Vec<f32> = (0..4800)
            .map(|n| (2.0 * std::f64::consts::PI * freq * n as f64 / sr_hz).sin() as f32)
            .collect();
        meter.process(&buf);
        assert!(
            meter.peak_db() > -0.5,
            "expected peak near 0 dBFS, got {}",
            meter.peak_db()
        );
    }

    #[test]
    fn clip_latches_and_resets() {
        let mut meter = Meter::new(sr(48_000));
        let mut buf = vec![0.1f32; 10];
        buf[5] = 1.5; // over 0 dBFS.
        meter.process(&buf);
        assert!(meter.clipped());

        meter.process(&[0.1f32; 10]);
        assert!(meter.clipped(), "clip must stay latched");

        meter.reset_clip();
        assert!(!meter.clipped());
    }

    #[test]
    fn peak_hold_latches_for_at_least_one_second() {
        let sample_rate = 48_000u32;
        let mut meter = Meter::new(sr(sample_rate));

        let mut transient = vec![0.0f32; 10];
        transient[0] = 0.9;
        meter.process(&transient);
        let held_db = meter.peak_hold_db();
        assert!(held_db > -1.0, "expected transient near 0 dBFS peak-hold");

        // Slightly less than 1 second of silence.
        let silence = vec![0.0f32; sample_rate as usize - 100];
        meter.process(&silence);
        assert!(
            (meter.peak_hold_db() - held_db).abs() < 1e-6,
            "peak-hold decreased before the 1 s floor: {} vs {}",
            meter.peak_hold_db(),
            held_db
        );

        // Well past the 1 s mark: still a well-defined, finite value (no contract on whether it
        // has changed by now).
        let more_silence = vec![0.0f32; sample_rate as usize * 3];
        meter.process(&more_silence);
        assert!(meter.peak_hold_db().is_finite());
    }

    #[test]
    fn average_responds_more_slowly_than_peak_to_a_step() {
        let mut meter = Meter::new(sr(48_000));
        let silence = vec![0.0f32; 1000];
        meter.process(&silence);

        // Sudden full-scale tone, a few samples in.
        let step = vec![1.0f32; 20];
        meter.process(&step);

        assert!(
            meter.peak_db() > -1.0,
            "peak should already be near 0 dBFS, got {}",
            meter.peak_db()
        );
        assert!(
            meter.average_db() < meter.peak_db() - 3.0,
            "average ({}) should still lag peak ({}) this soon after the step",
            meter.average_db(),
            meter.peak_db()
        );
    }

    #[test]
    fn process_does_not_allocate() {
        let mut meter = Meter::new(sr(48_000));
        let buf = [0.2f32; 128];
        audio_section(|| meter.process(&buf));
    }
}
