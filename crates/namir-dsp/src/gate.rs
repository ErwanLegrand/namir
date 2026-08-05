//! Noise gate with hysteresis (FR-GATE-010..040).
//!
//! This primitive is intentionally trim-agnostic: it knows nothing about D-9.8's "gate before
//! input trim" ordering decision. That ordering is a chain-assembly concern for `namir-engine`
//! to apply when it wires this gate into a stage — not something this crate should encode or
//! re-litigate, since a primitive DSP block has no notion of "before" or "after" anything.

use namir_core::{SampleRate, linear_to_db};

/// FR-GATE-010's control set. `hysteresis_db` is not in that FRS table explicitly, but is
/// required by FR-GATE-020 ("the level at which the gate closes shall be measurably below the
/// level at which it opens"); `3.0` dB here is this crate's own engineering default pending a UI
/// control decision, not an FRS-specified figure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateParams {
    /// FR-GATE-010: -100..0 dBFS, default -70.
    pub threshold_db: f32,
    /// FR-GATE-010: 0.1..50 ms, default 1.
    pub attack_ms: f32,
    /// FR-GATE-010: 0..500 ms, default 30.
    pub hold_ms: f32,
    /// FR-GATE-010: 1..2000 ms, default 100.
    pub release_ms: f32,
    /// FR-GATE-020's hysteresis gap. Default 3.0 dB (this crate's own default; see struct doc).
    pub hysteresis_db: f32,
}

impl Default for GateParams {
    fn default() -> Self {
        Self {
            threshold_db: -70.0,
            attack_ms: 1.0,
            hold_ms: 30.0,
            release_ms: 100.0,
            hysteresis_db: 3.0,
        }
    }
}

/// The gate's state machine position. `Opening`/`Closing` are the sample-accurate ramp phases
/// FR-GATE-030 requires; the hold countdown between `Open` and `Closing` does not get its own
/// variant (FR-GATE-010 treats hold as part of being open, not a distinct user-visible state) and
/// is tracked internally instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    /// Fully attenuating; gain is `0.0`.
    Closed,
    /// Ramping gain upward on attack, having just crossed the open threshold.
    Opening,
    /// Fully passing (gain `1.0`), whether or not a hold countdown is in progress.
    Open,
    /// Ramping gain downward on release, having exhausted hold without re-opening.
    Closing,
}

/// A short, fixed envelope-detector time constant, independent of the attack/release *gain*
/// ramp times, so threshold/hysteresis comparisons are stable rather than jittering on raw
/// sample values. Not a user-facing control.
const DETECTOR_TIME_CONSTANT_MS: f64 = 1.0;

fn one_pole_coeff(time_constant_ms: f64, sample_rate_hz: f64) -> f32 {
    let tau_samples = (time_constant_ms / 1000.0) * sample_rate_hz;
    (1.0 - (-1.0 / tau_samples).exp()) as f32
}

/// Rounds `ms` of `sample_rate_hz` to whole samples, floored at 1 — for attack/release times,
/// whose FRS ranges (0.1..50 ms and 1..2000 ms) never legitimately reach zero samples, but a
/// pathological rate/ms combination could round down to 0 without this floor, which would divide
/// by zero below.
fn ms_to_samples_min1(ms: f32, sample_rate_hz: f64) -> u32 {
    let n = (ms as f64 / 1000.0 * sample_rate_hz).round();
    n.max(1.0) as u32
}

/// As `ms_to_samples_min1`, but for hold, whose FRS range (0..500 ms) legitimately includes
/// zero.
fn ms_to_samples(ms: f32, sample_rate_hz: f64) -> u32 {
    let n = (ms as f64 / 1000.0 * sample_rate_hz).round();
    n.max(0.0) as u32
}

/// A hysteretic, sample-accurate noise gate (FR-GATE-010..040); see this module's doc comment
/// for its position relative to input trim in the assembled chain.
pub struct NoiseGate {
    sample_rate: SampleRate,
    params: GateParams,
    status: GateStatus,
    /// Current linear gain applied to the signal; 1.0 = fully open, 0.0 = fully closed.
    gain: f32,
    /// Fast peak-follower envelope, see `DETECTOR_TIME_CONSTANT_MS`.
    envelope: f32,
    detector_coeff: f32,
    /// Per-sample gain increment while `Opening` (`1 / attack_samples`).
    attack_step: f32,
    /// Per-sample gain decrement while `Closing` (`1 / release_samples`).
    release_step: f32,
    hold_samples: u32,
    /// `Some(n)` while `Open` and counting down the hold period before `Closing`; `None` while
    /// fully open (not counting down) or in any other status.
    hold_remaining: Option<u32>,
}

impl NoiseGate {
    /// Builds a closed gate at `GateParams::default()`, fixed to `sample_rate` for its lifetime.
    pub fn new(sample_rate: SampleRate) -> Self {
        let detector_coeff = one_pole_coeff(DETECTOR_TIME_CONSTANT_MS, sample_rate.hz_f64());
        let mut gate = Self {
            sample_rate,
            params: GateParams::default(),
            status: GateStatus::Closed,
            gain: 0.0,
            envelope: 0.0,
            detector_coeff,
            attack_step: 1.0,
            release_step: 1.0,
            hold_samples: 0,
            hold_remaining: None,
        };
        gate.set_params(GateParams::default());
        gate
    }

    /// Recomputes the per-sample attack/release/hold coefficients from `params` and the sample
    /// rate fixed at construction.
    pub fn set_params(&mut self, params: GateParams) {
        let sr_hz = self.sample_rate.hz_f64();
        let attack_samples = ms_to_samples_min1(params.attack_ms, sr_hz);
        let release_samples = ms_to_samples_min1(params.release_ms, sr_hz);
        self.hold_samples = ms_to_samples(params.hold_ms, sr_hz);
        self.attack_step = 1.0 / attack_samples as f32;
        self.release_step = 1.0 / release_samples as f32;
        self.params = params;
    }

    /// Advances the state machine by one sample and returns the gain to apply to it.
    fn step(&mut self, input_abs: f32) -> f32 {
        self.envelope += self.detector_coeff * (input_abs - self.envelope);
        let env_db = linear_to_db(self.envelope);
        let open = env_db >= self.params.threshold_db;
        let close = env_db < self.params.threshold_db - self.params.hysteresis_db;

        match self.status {
            GateStatus::Closed => {
                if open {
                    self.status = GateStatus::Opening;
                }
            }
            GateStatus::Opening => {
                self.gain += self.attack_step;
                if self.gain >= 1.0 {
                    self.gain = 1.0;
                    self.status = GateStatus::Open;
                    self.hold_remaining = None;
                }
            }
            GateStatus::Open => match self.hold_remaining {
                None => {
                    if close {
                        self.hold_remaining = Some(self.hold_samples);
                    }
                }
                Some(0) => {
                    if open {
                        self.hold_remaining = None;
                    } else {
                        self.status = GateStatus::Closing;
                        self.hold_remaining = None;
                    }
                }
                Some(n) => {
                    if open {
                        self.hold_remaining = None;
                    } else {
                        self.hold_remaining = Some(n - 1);
                    }
                }
            },
            GateStatus::Closing => {
                if open {
                    // Resume opening from wherever the gain currently is — no discontinuity.
                    self.status = GateStatus::Opening;
                } else {
                    self.gain -= self.release_step;
                    if self.gain <= 0.0 {
                        self.gain = 0.0;
                        self.status = GateStatus::Closed;
                    }
                }
            }
        }

        self.gain
    }

    /// Applies the gate in place. Allocates nothing.
    pub fn process(&mut self, buf: &mut [f32]) {
        for x in buf.iter_mut() {
            let gain = self.step(x.abs());
            *x *= gain;
        }
    }

    /// Current attenuation in dB; `0.0` when fully open (FR-GATE-040).
    pub fn gain_reduction_db(&self) -> f32 {
        linear_to_db(self.gain)
    }

    /// The gate's current state-machine position; see `GateStatus`.
    pub fn status(&self) -> GateStatus {
        self.status
    }

    /// Resets detector and state machine to fully closed. Does not touch `params`.
    pub fn reset(&mut self) {
        self.status = GateStatus::Closed;
        self.gain = 0.0;
        self.envelope = 0.0;
        self.hold_remaining = None;
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
    fn defaults_match_the_frs_table() {
        let p = GateParams::default();
        assert_eq!(p.threshold_db, -70.0);
        assert_eq!(p.attack_ms, 1.0);
        assert_eq!(p.hold_ms, 30.0);
        assert_eq!(p.release_ms, 100.0);
    }

    #[test]
    fn burst_opens_and_silence_closes() {
        let mut gate = NoiseGate::new(sr(48_000));
        // Well above threshold (-70 dBFS): -10 dBFS.
        let loud = namir_core::db_to_linear(-10.0);
        let mut burst = vec![loud; 48_000]; // 1 s, comfortably longer than attack+hold+release.
        gate.process(&mut burst);
        assert!(
            gate.gain_reduction_db() > -0.1,
            "expected fully open, got {} dB",
            gate.gain_reduction_db()
        );
        assert_eq!(gate.status(), GateStatus::Open);

        let mut silence = vec![0.0f32; 48_000];
        gate.process(&mut silence);
        assert!(
            gate.gain_reduction_db() < -60.0,
            "expected fully closed, got {} dB",
            gate.gain_reduction_db()
        );
        assert_eq!(gate.status(), GateStatus::Closed);
    }

    #[test]
    fn hysteresis_prevents_chatter_on_a_slowly_decaying_signal() {
        let sample_rate = 48_000u32;
        let mut gate = NoiseGate::new(sr(sample_rate));
        // A smoothly, slowly decaying "envelope" (no carrier ripple, so the only reason for
        // multiple close events would be missing hysteresis, not envelope-follower artefacts).
        // Starts well above threshold, decays over several seconds down through the threshold
        // region and on to silence.
        let total = sample_rate as usize * 5;
        // Starts comfortably above the open threshold (-70 dBFS) and, over the 5 s window, decays
        // to well below the close threshold (threshold - hysteresis) — about -97 dBFS by the end
        // — so the envelope actually crosses the hysteresis band once, rather than asymptoting
        // to a level still above it.
        let start_linear = namir_core::db_to_linear(-10.0);
        let tau = (sample_rate as f64) * 0.5; // slow decay relative to detector/attack times.

        let mut closing_transitions = 0u32;
        let mut prev_status = gate.status();
        for n in 0..total {
            let amplitude = start_linear * (-(n as f64) / tau).exp() as f32;
            let mut sample = [amplitude];
            gate.process(&mut sample);
            if gate.status() == GateStatus::Closing && prev_status != GateStatus::Closing {
                closing_transitions += 1;
            }
            prev_status = gate.status();
        }

        assert_eq!(
            closing_transitions, 1,
            "expected exactly one transition into Closing, got {closing_transitions}"
        );
    }

    #[test]
    fn attack_ramps_sample_accurately_not_in_one_step() {
        let mut gate = NoiseGate::new(sr(48_000));
        let loud = namir_core::db_to_linear(-10.0);

        let mut max_delta = 0.0f32;
        let mut prev_gain = gate.gain;
        // Feed enough samples to cover the whole attack ramp (1 ms @ 48 kHz ~= 48 samples).
        for _ in 0..200 {
            let mut sample = [loud];
            gate.process(&mut sample);
            let delta = (gate.gain - prev_gain).abs();
            max_delta = max_delta.max(delta);
            prev_gain = gate.gain;
        }

        // A one-sample jump would show delta close to 1.0. Sample-accurate ramping over ~48
        // samples should show delta on the order of 1/48 ~= 0.02, not order 1.
        assert!(
            max_delta < 0.2,
            "max per-sample gain delta {max_delta} looks stepped, not ramped"
        );
        assert!(max_delta > 0.0, "gate never opened");
    }

    #[test]
    fn hold_delays_release_after_signal_drops() {
        let mut gate = NoiseGate::new(sr(48_000));
        let loud = namir_core::db_to_linear(-10.0);
        let mut burst = vec![loud; 4800]; // 100 ms, plenty to fully open.
        gate.process(&mut burst);
        assert_eq!(gate.status(), GateStatus::Open);

        // hold_ms defaults to 30 ms = 1440 samples @ 48 kHz. Feed silence for less than that and
        // the gate must not yet be releasing.
        let mut silence = vec![0.0f32; 1000];
        gate.process(&mut silence);
        assert_ne!(
            gate.status(),
            GateStatus::Closing,
            "gate started releasing before hold expired"
        );
        assert!(
            gate.gain_reduction_db() > -0.5,
            "gain should still be ~fully open during hold, got {}",
            gate.gain_reduction_db()
        );
    }

    #[test]
    fn process_does_not_allocate() {
        let mut gate = NoiseGate::new(sr(48_000));
        let mut buf = [0.1f32; 128];
        audio_section(|| gate.process(&mut buf));
    }
}
