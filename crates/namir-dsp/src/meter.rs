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

/// Level meter: fast-attack/slow-release peak, a short-term average, a latching peak-hold, and a
/// latching clip indicator (FR-IN-020/030, FR-OUT-020).
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
    /// Builds a meter at zero/silent readings, with the release time constant fixed to
    /// `sample_rate` for its lifetime.
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
    ///
    /// # Why the level updates are guarded (issue #129)
    ///
    /// A meter is a *follower*: every reading is a function of its own previous value, so a single
    /// value with no level to it does not produce one wrong frame, it produces wrong frames for
    /// ever. That is what a non-finite sample used to do here. `NaN > self.peak` is false, so the
    /// release branch computed `peak + coeff * (NaN - peak)` = NaN, and NaN compares false against
    /// everything after it, so no later sample — however loud — could ever move `peak` again.
    ///
    /// `namir-core`'s `linear_to_db` fix for the same issue does not mask this and was never meant
    /// to: it maps a NaN amplitude to the floor, which is the *readable* rendering of a poisoned
    /// meter and the reason the failure was invisible. A meter frozen at −600 dB reads as dead
    /// silence, which is precisely the condition a user consults a meter to rule out.
    ///
    /// So a sample that is not finite contributes to none of the three level readings; the follower
    /// simply keeps releasing, and the next real sample moves it again. Two things this guard
    /// deliberately does **not** do:
    ///
    /// - It does not touch the clip latch. That latch states "a sample reached or exceeded full
    ///   scale", and an infinite one did (`inf >= 1.0`); a NaN did not. Both keep the behaviour
    ///   they had, so the one visible trace a blown-up sample leaves in a meter survives the fix.
    /// - It does not report the fault. Containing a non-finite sample is FR-CHAIN-080's job —
    ///   `namir_engine::Chain` silences the whole block and increments a counter the UI can read —
    ///   and a DSP primitive with no error channel inventing a second one would be the worse
    ///   design. This is only about not being poisoned by what the chain is already reporting.
    ///
    /// The average is guarded on its *result* rather than on `x`, because the same poisoning is
    /// reachable from a perfectly finite sample: `x * x` overflows to infinity above a magnitude of
    /// ~1.8e19, and `inf + coeff * (x2 - inf)` is NaN on the next sample.
    ///
    /// **RT-safe:** the guards are branches on values already in registers — no allocation, no
    /// call, and the loop bound is still `buf.len()`.
    pub fn process(&mut self, buf: &[f32]) {
        for &x in buf {
            let abs_x = x.abs();

            if abs_x.is_finite() {
                // Fast attack (instantaneous jump to a new higher sample), slow exponential
                // release.
                if abs_x > self.peak {
                    self.peak = abs_x;
                } else {
                    self.peak += self.release_coeff * (abs_x - self.peak);
                }

                let avg_sq = self.avg_sq + self.release_coeff * (x * x - self.avg_sq);
                if avg_sq.is_finite() {
                    self.avg_sq = avg_sq;
                }

                if self.peak > self.peak_hold {
                    self.peak_hold = self.peak;
                }
            }

            if abs_x >= 1.0 {
                self.clipped = true;
            }
        }
    }

    /// Fast-attack/slow-release peak reading, in dB.
    pub fn peak_db(&self) -> f32 {
        linear_to_db(self.peak)
    }

    /// Short-term RMS-like average reading, in dB.
    pub fn average_db(&self) -> f32 {
        linear_to_db(self.avg_sq.sqrt())
    }

    /// Latched peak-hold reading, in dB; see this struct's `peak_hold` field doc for the
    /// latching contract.
    pub fn peak_hold_db(&self) -> f32 {
        linear_to_db(self.peak_hold)
    }

    /// Whether any sample since construction or the last `reset`/`reset_clip` reached or
    /// exceeded full scale.
    pub fn clipped(&self) -> bool {
        self.clipped
    }

    /// Clears the clip latch without touching peak, average, or peak-hold.
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

    // trace-partial: FR-IN-020
    // uncovered: FR-IN-020 — the "M for the display" half of the Verify line has no artifact:
    // uncovered: there is no docs/manual-tests/fr-in-020-*.md, and namir_ui::MeterReading carries
    // uncovered: only peak_db and rms_db, so the peak-hold value TrimStage publishes reaches no
    // uncovered: UI field for any script to observe; closes M8
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

    /// Issue #129's second half, the one `namir-core`'s `linear_to_db` fix does **not** mask: a
    /// single non-finite sample used to poison `peak` (and `avg_sq`) permanently. `NaN > peak` is
    /// false, so the release branch computed `peak + coeff * (NaN - peak)` = NaN, and every
    /// subsequent sample kept it NaN; `linear_to_db(NaN)` is the floor, so the meter read **dead
    /// silence forever** — the worst shape a wrong meter can take, since silence is exactly what a
    /// user checks a meter to rule out.
    ///
    /// Committed red-first: before the guard, `peak_db()` after the recovery tone is the -600 dB
    /// floor rather than a reading near the tone's own level.
    #[test]
    fn one_nan_sample_does_not_poison_the_meter_for_ever() {
        let floor = linear_to_db(0.0);
        for poison in [f32::NAN, -f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut meter = Meter::new(sr(48_000));
            meter.process(&[0.5f32; 64]);
            let before = meter.peak_db();
            assert!(before > floor + 100.0, "{poison}: setup did not register");

            meter.process(&[poison]);
            assert!(
                meter.peak_db().is_finite() && meter.average_db().is_finite(),
                "{poison}: reading went non-finite immediately"
            );

            // A full second of real signal after the bad sample: a meter that recovers reads the
            // tone, a poisoned one reads the floor no matter what it is fed.
            meter.process(&[0.5f32; 48_000]);
            assert!(
                (meter.peak_db() - before).abs() < 1.0,
                "{poison}: after one bad sample the meter reads {} dB against the {before} dB the \
                 same signal read before it",
                meter.peak_db()
            );
            assert!(
                meter.average_db() > floor + 100.0,
                "{poison}: the average stayed poisoned at {} dB",
                meter.average_db()
            );
            assert!(
                meter.peak_hold_db() > floor + 100.0,
                "{poison}: the peak-hold stayed poisoned at {} dB",
                meter.peak_hold_db()
            );
        }
    }

    /// The same poisoning reachable from a **finite** sample: `x * x` overflows to infinity for any
    /// magnitude above ~1.8e19, so `avg_sq` went infinite and the very next sample turned it into
    /// `inf + coeff * (x2 - inf)` = NaN. A guard that only reads `x.is_finite()` leaves this open,
    /// which is why the average commits its update only when the result is itself finite.
    #[test]
    fn a_huge_finite_sample_does_not_poison_the_average() {
        let mut meter = Meter::new(sr(48_000));
        meter.process(&[1e30f32]);
        meter.process(&[0.5f32; 48_000]);
        assert!(
            meter.average_db().is_finite() && meter.average_db() > linear_to_db(0.0) + 100.0,
            "the average reads {} dB after one 1e30 sample",
            meter.average_db()
        );
    }

    /// The clip latch is deliberately *not* part of the guard: it states "any sample reached or
    /// exceeded full scale", and an infinite one did. Pinned so the guard above cannot quietly
    /// take the one visible trace a blown-up sample leaves in a meter. (A NaN latches nothing —
    /// `NaN >= 1.0` is false — which is also unchanged, and is FR-CHAIN-080's fault to report,
    /// not this primitive's.)
    #[test]
    fn an_infinite_sample_still_latches_the_clip_indicator() {
        let mut meter = Meter::new(sr(48_000));
        meter.process(&[f32::INFINITY]);
        assert!(meter.clipped());

        let mut meter = Meter::new(sr(48_000));
        meter.process(&[f32::NEG_INFINITY]);
        assert!(meter.clipped());
    }

    #[test]
    fn process_does_not_allocate() {
        let mut meter = Meter::new(sr(48_000));
        let buf = [0.2f32; 128];
        audio_section(|| meter.process(&buf));
    }
}
