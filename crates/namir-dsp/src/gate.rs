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

/// The envelope detector's look-back window, in milliseconds. Not a user-facing control.
///
/// # Why a windowed peak and not a one-pole (issue #123)
///
/// The detector's job is to report *the level of the note*, so that FR-GATE-010's threshold means
/// what its dBFS unit says and FR-GATE-020's hysteresis gap is compared against something that
/// does not itself move by more than the gap. A symmetric one-pole on `|x|` — what this was until
/// M14 — does neither. At a 1 ms time constant its output on an 82.41 Hz carrier (a standard-tuned
/// guitar's low E) swings **9.5 dB peak to peak**, three times the shipped 3 dB hysteresis, so the
/// ripple alone re-crossed the band: a decaying low E produced **62** close events at `hold_ms = 0`
/// where FR-GATE-020 demands one. It also settled on mean-`|x|` rather than peak, which made the
/// threshold frequency-dependent — a −70 dBFS threshold first opened at −69.1 dBFS peak for 82.41
/// Hz but −66.5 dBFS for 1 kHz.
///
/// This is instead a sliding-window maximum of `|x|`: instantaneous attack (a transient is never
/// missed) and a release that is not a time constant at all but the age of the window. Two
/// properties follow, and they are the two the one-pole lacked.
///
/// *Calibration.* The reading is the true peak amplitude of the carrier, so a threshold in dBFS is
/// peak-referenced at every frequency, as FR-GATE-010's unit implies.
///
/// *Ripple.* For a sine whose rectified half-period is `T` and a retained window of `W`, the
/// window always contains the peak when `W >= T`, so the reading is exactly flat. Below that it
/// dips, to a worst case of `-20*log10(sin(pi*W/(2*T)))` dB — 0 dB down to 66.7 Hz on the figures
/// below, 1.7 dB at a four-string bass's 41.2 Hz low E, and still under the 3 dB default
/// hysteresis for anything above about **33 Hz**. That is the whole audible fundamental range of
/// both instruments this product names.
///
/// The window is also how long the gate takes to notice true silence, which is why it is short
/// rather than arbitrarily long: 8 ms here, of which at least
/// `DETECTOR_WINDOW_MS * (DETECTOR_BLOCKS - 1) / DETECTOR_BLOCKS` = 7.5 ms is retained at any
/// instant (see `DETECTOR_BLOCKS`). Attack, hold and release remain the user's own controls and
/// are unaffected; this constant only sets when the *detector* concedes that the note has stopped.
const DETECTOR_WINDOW_MS: f64 = 8.0;

/// The window is carried as this many rolling sub-block maxima rather than a per-sample ring
/// buffer: the maximum of a fixed, small array is O(1) per sample with no allocation and no
/// data-dependent loop, which a monotonic-deque sliding maximum would not be. The cost is that
/// the retained look-back is not exactly `DETECTOR_WINDOW_MS` but varies over
/// `[(K-1)/K, 1] * DETECTOR_WINDOW_MS` as the current sub-block fills; the ripple figures in
/// `DETECTOR_WINDOW_MS`'s doc are quoted against the retained minimum, the worst case.
const DETECTOR_BLOCKS: usize = 16;

/// Samples per detector sub-block, floored at 1 so a pathologically low sample rate cannot
/// produce a zero-length block (which would advance the window every sample and retain nothing).
fn detector_block_len(sample_rate_hz: f64) -> u32 {
    let window_samples = DETECTOR_WINDOW_MS / 1000.0 * sample_rate_hz;
    (window_samples / DETECTOR_BLOCKS as f64).round().max(1.0) as u32
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
    /// The detector's reading: the largest `|x|` anywhere in the retained window, i.e. the peak
    /// amplitude of the note. See `DETECTOR_WINDOW_MS` for why this is a windowed maximum and not
    /// a one-pole follower.
    envelope: f32,
    /// The window itself, as `DETECTOR_BLOCKS` rolling sub-block maxima; `envelope` is their
    /// maximum. `block_index` is the sub-block currently being filled.
    window_blocks: [f32; DETECTOR_BLOCKS],
    block_index: usize,
    /// Samples left before `block_index` advances and the oldest sub-block is discarded.
    block_remaining: u32,
    /// Samples per sub-block, fixed at construction with the sample rate.
    block_len: u32,
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
        let block_len = detector_block_len(sample_rate.hz_f64());
        let mut gate = Self {
            sample_rate,
            params: GateParams::default(),
            status: GateStatus::Closed,
            gain: 0.0,
            envelope: 0.0,
            window_blocks: [0.0; DETECTOR_BLOCKS],
            block_index: 0,
            block_remaining: block_len,
            block_len,
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

    /// Advances the sliding-window peak detector by one sample and returns its reading. Runs on
    /// the audio thread: fixed-size state, no allocation, and the only loop is the fold over
    /// `DETECTOR_BLOCKS` (a compile-time constant) once every `block_len` samples.
    fn detect(&mut self, input_abs: f32) -> f32 {
        if self.block_remaining == 0 {
            self.block_index = (self.block_index + 1) % DETECTOR_BLOCKS;
            self.window_blocks[self.block_index] = 0.0;
            self.block_remaining = self.block_len;
            // The oldest sub-block has just been discarded, so the running maximum has to be
            // rebuilt from what is left rather than merely relaxed.
            self.envelope = self.window_blocks.iter().copied().fold(0.0f32, f32::max);
        }
        self.block_remaining -= 1;

        let block = &mut self.window_blocks[self.block_index];
        if input_abs > *block {
            *block = input_abs;
        }
        if input_abs > self.envelope {
            self.envelope = input_abs;
        }
        self.envelope
    }

    /// Advances the state machine by one sample and returns the gain to apply to it.
    fn step(&mut self, input_abs: f32) -> f32 {
        let env_db = linear_to_db(self.detect(input_abs));
        let open = env_db >= self.params.threshold_db;
        let close = env_db < self.params.threshold_db - self.params.hysteresis_db;

        match self.status {
            GateStatus::Closed => {
                if open {
                    self.status = GateStatus::Opening;
                }
            }
            GateStatus::Opening => {
                if close {
                    // The signal fell back through the hysteresis band mid-attack: turn around
                    // from wherever the gain currently is, mirroring the `Closing -> Opening`
                    // resumption below. Without this the attack ramp ran to completion no matter
                    // what the detector did afterwards, so a single isolated sample opened the
                    // gate fully and held it open for attack + hold + release (issue #124).
                    self.status = GateStatus::Closing;
                } else {
                    self.gain += self.attack_step;
                    if self.gain >= 1.0 {
                        self.gain = 1.0;
                        self.status = GateStatus::Open;
                        self.hold_remaining = None;
                    }
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
        self.window_blocks = [0.0; DETECTOR_BLOCKS];
        self.block_index = 0;
        self.block_remaining = self.block_len;
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

    /// E2, the low-E fundamental of a standard-tuned guitar: the lowest note the instrument
    /// ahead of this gate actually produces, and the hardest case for the detector, whose
    /// window has to span the rectified half-period of the carrier to read its peak.
    const LOW_E_HZ: f64 = 82.41;

    /// FR-GATE-010's hold range (0..500 ms): both ends, the default, and the values between where
    /// hold and hysteresis trade off. Every entry is asserted; the range is spanned, not filtered.
    const FRS_HOLD_MS: [f32; 8] = [0.0, 1.0, 5.0, 10.0, 30.0, 100.0, 250.0, 500.0];

    /// The amplitude wobble applied to the hovering phase of [`hovering_then_decaying_low_e`], in
    /// dB either side of the threshold, and its rate. 2 dB peak to peak is narrower than the 3 dB
    /// default hysteresis and wider than a 0 or 1 dB gap, which is exactly what makes the gap the
    /// variable under test rather than a passenger.
    const WOBBLE_DB: f32 = 1.0;
    const WOBBLE_HZ: f64 = 8.0;

    /// FR-GATE-020's own two scenarios, back to back, as one stimulus: a low-E note that first
    /// **hovers at the threshold** (the requirement's sentence: "to prevent chatter on a signal
    /// hovering at the threshold") and then **decays through it** (its `Verify:` method: "a signal
    /// decaying through the threshold shall produce exactly one close event").
    ///
    /// Phase 1, 2 s: the carrier's peak amplitude sits at `threshold_db` with a ±`WOBBLE_DB`
    /// modulation at `WOBBLE_HZ` — a note held at the edge of the gate's threshold, which is the
    /// only place chatter can happen at all.
    /// Phase 2, 3 s: exponential decay from the threshold to about 30 dB below it, well clear of
    /// even the widest hysteresis gap swept below.
    ///
    /// **Both the carrier and the wobble are load-bearing.** A bare decaying envelope, which is
    /// what this test used before issue #125, is monotonic at any detector's output, so it crosses
    /// the close threshold once whatever the parameters are and cannot falsify the requirement. A
    /// bare *carrier* under a bare decay is monotonic too now that the detector reads its peak
    /// (issue #123) — it was only the old one-pole detector's own 9.5 dB of rectified ripple that
    /// made that stimulus chatter, i.e. the test would have been measuring a detector defect, not
    /// hysteresis. Real program material hovering at a gate threshold is not monotonic: it wobbles,
    /// and the wobble is what hysteresis exists to bridge.
    fn hovering_then_decaying_low_e(
        threshold_db: f32,
        samples: usize,
        sample_rate: u32,
    ) -> impl Iterator<Item = f32> {
        let hover_samples = sample_rate as usize * 2;
        let decay_tau = sample_rate as f64 * 0.8; // ~32 dB over the 3 s decay phase.
        let radians_per_sample = 2.0 * std::f64::consts::PI * LOW_E_HZ / sample_rate as f64;
        let wobble_per_sample = 2.0 * std::f64::consts::PI * WOBBLE_HZ / sample_rate as f64;
        (0..samples).map(move |n| {
            let wobble_db = WOBBLE_DB * (wobble_per_sample * n as f64).sin() as f32;
            let decay_db = if n < hover_samples {
                0.0
            } else {
                -8.685_889 * ((n - hover_samples) as f64 / decay_tau) as f32
            };
            let amplitude = namir_core::db_to_linear(threshold_db + wobble_db + decay_db);
            amplitude * (radians_per_sample * n as f64).sin() as f32
        })
    }

    /// Counts transitions into `Closing` — FR-GATE-020's "close event" — over
    /// [`hovering_then_decaying_low_e`].
    fn close_events(sample_rate: u32, hold_ms: f32, hysteresis_db: f32) -> u32 {
        let mut gate = NoiseGate::new(sr(sample_rate));
        let params = GateParams {
            hold_ms,
            hysteresis_db,
            ..GateParams::default()
        };
        gate.set_params(params);

        let mut closing_transitions = 0u32;
        let mut prev_status = gate.status();
        for x in
            hovering_then_decaying_low_e(params.threshold_db, sample_rate as usize * 5, sample_rate)
        {
            let mut sample = [x];
            gate.process(&mut sample);
            if gate.status() == GateStatus::Closing && prev_status != GateStatus::Closing {
                closing_transitions += 1;
            }
            prev_status = gate.status();
        }
        closing_transitions
    }

    /// The carrier's peak amplitude, in dBFS, at the sample where `gate` first leaves `Closed`
    /// (the open level) and where it first enters `Closing` afterwards (the close level).
    ///
    /// Driven by a slow symmetric ramp — 25 dB up over 2 s, then 25 dB back down — so that the
    /// level *is* the instantaneous amplitude to within the detector's own window (8 ms at
    /// 12.5 dB/s is 0.1 dB) and neither figure is an artefact of how fast the ramp moved.
    fn open_and_close_levels_dbfs(gate: &mut NoiseGate, freq_hz: f64) -> (f32, f32) {
        let sample_rate = 48_000u32;
        let leg = sample_rate as usize * 2;
        let radians_per_sample = 2.0 * std::f64::consts::PI * freq_hz / sample_rate as f64;
        let (mut open_at, mut close_at) = (None, None);
        for n in 0..leg * 2 {
            let db = if n < leg {
                -85.0 + 25.0 * (n as f32 / leg as f32)
            } else {
                -60.0 - 25.0 * ((n - leg) as f32 / leg as f32)
            };
            let mut sample =
                [namir_core::db_to_linear(db) * (radians_per_sample * n as f64).sin() as f32];
            gate.process(&mut sample);
            if open_at.is_none() && gate.status() != GateStatus::Closed {
                open_at = Some(db);
            }
            if open_at.is_some() && close_at.is_none() && gate.status() == GateStatus::Closing {
                close_at = Some(db);
            }
        }
        (
            open_at.expect("the gate never opened"),
            close_at.expect("the gate never closed"),
        )
    }

    // trace: FR-GATE-020
    #[test]
    fn hysteresis_prevents_chatter_on_a_low_e_note_hovering_at_the_threshold() {
        // (a) The requirement's own `Verify:` method — "exactly one close event" — over the whole
        //     of FR-GATE-010's hold range including its 0 ms end, at the shipped 3 dB gap. Run at
        //     three sample rates as well: the requirement does not quantify over them, but the
        //     detector's window is now the mechanism that carries it and `detector_block_len`
        //     rounds that window to whole samples per rate.
        for sample_rate in [44_100u32, 48_000, 96_000] {
            for hold_ms in FRS_HOLD_MS {
                let events = close_events(sample_rate, hold_ms, 3.0);
                assert_eq!(
                    events, 1,
                    "{sample_rate} Hz, hold {hold_ms} ms: expected exactly one transition into \
                     Closing, got {events}"
                );
            }
        }

        // (b) The falsifier. With the hysteresis gap removed and nothing else changed, the same
        //     stimulus chatters — so (a) at its 0 ms hold is resting on hysteresis and on nothing
        //     else. (At the long end of the hold range hold would carry it too; that is what (a)'s
        //     0 and 1 ms entries are for.)
        let without_hysteresis = close_events(48_000, 0.0, 0.0);
        assert!(
            without_hysteresis > 1,
            "hold 0 ms with no hysteresis should chatter, got {without_hysteresis} close events \
             — the assertion above is then resting on hold, not on hysteresis"
        );

        // (c) Widening the gap must reduce the chatter monotonically, and any gap wider than the
        //     wobble the note is hovering with must remove it entirely.
        let sweep: Vec<(f32, u32)> = [0.0f32, 1.0, 3.0, 6.0, 12.0, 24.0]
            .into_iter()
            .map(|db| (db, close_events(48_000, 0.0, db)))
            .collect();
        for pair in sweep.windows(2) {
            let ((narrow_db, narrow), (wide_db, wide)) = (pair[0], pair[1]);
            assert!(
                wide <= narrow,
                "widening hysteresis from {narrow_db} dB to {wide_db} dB raised the close-event \
                 count from {narrow} to {wide}"
            );
        }
        for &(db, events) in sweep.iter().filter(|(db, _)| *db >= 2.0 * WOBBLE_DB) {
            assert_eq!(
                events,
                1,
                "hold 0 ms at {db} dB of hysteresis — wider than the note's {} dB of wobble — \
                 should close exactly once, got {events}",
                2.0 * WOBBLE_DB
            );
        }

        // (d) The requirement's normative sentence, measured directly rather than inferred from a
        //     chatter count: "the level at which the gate closes shall be measurably below the
        //     level at which it opens". The gap is `hysteresis_db` by construction, so this also
        //     pins that the parameter is what sets it.
        for hysteresis_db in [3.0f32, 6.0, 12.0] {
            let mut gate = NoiseGate::new(sr(48_000));
            gate.set_params(GateParams {
                hold_ms: 0.0,
                hysteresis_db,
                ..GateParams::default()
            });
            let (open_db, close_db) = open_and_close_levels_dbfs(&mut gate, LOW_E_HZ);
            assert!(
                close_db < open_db,
                "close level {close_db:.2} dBFS is not below the open level {open_db:.2} dBFS"
            );
            let measured_gap = open_db - close_db;
            assert!(
                (measured_gap - hysteresis_db).abs() < 0.5,
                "hysteresis_db = {hysteresis_db}: opened at {open_db:.2} dBFS and closed at \
                 {close_db:.2} dBFS, a gap of {measured_gap:.2} dB"
            );
        }
    }

    /// **Issue #123's second symptom.** FR-GATE-010 specifies Threshold in dBFS, a peak-referenced
    /// unit, so the level at which a given threshold opens the gate must not depend on the
    /// frequency of the note. The one-pole detector this replaced settled on mean-`|x|`, so a −70
    /// dBFS threshold first opened at −69.1 dBFS peak for a low E but −66.5 dBFS for 1 kHz — a
    /// 2.6 dB spread across the instrument's range.
    #[test]
    fn the_threshold_is_peak_referenced_at_every_program_frequency() {
        let mut levels = Vec::new();
        for freq_hz in [LOW_E_HZ, 110.0, 196.0, 440.0, 1000.0, 5000.0] {
            let mut gate = NoiseGate::new(sr(48_000));
            let (open_db, _) = open_and_close_levels_dbfs(&mut gate, freq_hz);
            assert!(
                (open_db - GateParams::default().threshold_db).abs() < 0.5,
                "{freq_hz} Hz: a -70 dBFS threshold first opened at {open_db:.2} dBFS peak"
            );
            levels.push(open_db);
        }
        let spread = levels.iter().cloned().fold(f32::MIN, f32::max)
            - levels.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            spread < 0.5,
            "the open level varies by {spread:.2} dB across the instrument's range: {levels:?}"
        );
    }

    /// **Issue #123's first symptom, at the detector rather than through the state machine.** The
    /// reading on a steady carrier must be flat: any ripple is a level the hysteresis gap has to
    /// bridge before it can do the job FR-GATE-020 asks of it. The one-pole this replaced rippled
    /// 9.5 dB peak to peak on a low E, three times the default gap.
    ///
    /// The bass frequencies are below what `DETECTOR_WINDOW_MS`'s retained window spans, so they
    /// are asserted against that constant's own worst-case formula rather than at zero.
    #[test]
    fn the_detector_reads_a_steady_carrier_flat_across_the_instrument_range() {
        let sample_rate = 48_000u32;
        // The window's retained minimum — see `DETECTOR_BLOCKS`.
        let retained_s =
            DETECTOR_WINDOW_MS / 1000.0 * (DETECTOR_BLOCKS - 1) as f64 / DETECTOR_BLOCKS as f64;

        for freq_hz in [30.87f64, 41.2, LOW_E_HZ, 110.0, 440.0, 1000.0] {
            let mut gate = NoiseGate::new(sr(sample_rate));
            let amplitude = namir_core::db_to_linear(-20.0);
            let radians_per_sample = 2.0 * std::f64::consts::PI * freq_hz / sample_rate as f64;
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for n in 0..sample_rate as usize {
                gate.step((amplitude * (radians_per_sample * n as f64).sin() as f32).abs());
                // Skip the first half second, while the window is still filling.
                if n > sample_rate as usize / 2 {
                    let reading = linear_to_db(gate.envelope);
                    lo = lo.min(reading);
                    hi = hi.max(reading);
                }
            }

            let half_period_s = 0.5 / freq_hz;
            let predicted = if retained_s >= half_period_s {
                0.0
            } else {
                let ratio = std::f64::consts::PI * retained_s / (2.0 * half_period_s);
                -20.0 * ratio.sin().log10()
            };
            let ripple = (hi - lo) as f64;
            assert!(
                ripple <= predicted + 0.2,
                "{freq_hz} Hz: detector ripples {ripple:.2} dB peak to peak, above the \
                 {predicted:.2} dB DETECTOR_WINDOW_MS predicts"
            );
            // Every frequency at or above a four-string bass's low E must stay inside the default
            // hysteresis gap, or FR-GATE-020 cannot hold there.
            if freq_hz >= 41.2 {
                assert!(
                    ripple < GateParams::default().hysteresis_db as f64,
                    "{freq_hz} Hz: detector ripples {ripple:.2} dB, at or beyond the default \
                     {} dB hysteresis gap",
                    GateParams::default().hysteresis_db
                );
            }
        }
    }

    /// **Issue #124.** A single sample into otherwise silent input reads well above the threshold
    /// at the detector — a peak detector's whole point is that it does — and `Opening` used to ramp
    /// unconditionally to unity once entered, ignoring the detector for the rest of the attack. At
    /// the 50 ms maximum attack the gate reached a gain of exactly **1.000** and passed signal for
    /// **180 ms** (attack + hold + release) off one sample. It now turns the ramp around when the
    /// detector's reading falls back through the hysteresis band, so the gain only ever gets as far
    /// as the signal justified: `DETECTOR_WINDOW_MS / attack_ms`, measuring 0.155 at 50 ms.
    ///
    /// Attacks are swept from where that bound bites. Below about 8 ms the ramp finishes inside the
    /// detector's own window, so the gate does open — correctly: a −36 dBFS sample is 34 dB above
    /// the −70 dBFS threshold, a transient a 1 ms attack exists to catch, and how long it then
    /// stays open is Hold and Release, which are the user's own controls and not this defect.
    #[test]
    fn an_isolated_sample_does_not_run_the_attack_ramp_to_unity() {
        for attack_ms in [10.0f32, 25.0, 50.0] {
            let mut gate = NoiseGate::new(sr(48_000));
            gate.set_params(GateParams {
                attack_ms,
                ..GateParams::default()
            });

            let mut probe = vec![0.0f32; 48_000];
            probe[10] = namir_core::db_to_linear(-36.0);

            let mut max_gain = 0.0f32;
            let mut passing = 0usize;
            for x in &probe {
                let gain = gate.step(x.abs());
                max_gain = max_gain.max(gain);
                if gain > 0.0 {
                    passing += 1;
                }
            }

            let bound = (DETECTOR_WINDOW_MS as f32 / attack_ms) * 1.05;
            assert!(
                max_gain <= bound,
                "attack {attack_ms} ms: one isolated sample took the gate to a gain of \
                 {max_gain:.4}, past the {bound:.4} its own detector window justifies"
            );
            assert!(
                max_gain < 1.0,
                "attack {attack_ms} ms: one isolated sample opened the gate fully"
            );
            assert_eq!(
                gate.status(),
                GateStatus::Closed,
                "attack {attack_ms} ms: gate did not return to Closed"
            );
            // 180 ms is what the unconditional ramp cost at 50 ms of attack.
            let open_ms = passing as f64 / 48.0;
            assert!(
                open_ms < 90.0,
                "attack {attack_ms} ms: the gate passed signal for {open_ms:.1} ms off one sample"
            );
        }
    }

    /// Drives `input` through `gate` in `block`-sample calls and returns the gain that was
    /// actually applied to each sample, recovered as `output / input`. Exact, and the reason every
    /// caller below keeps its input non-zero everywhere: a gate driven with digital silence
    /// multiplies its own ramp away and reveals nothing about it.
    fn per_sample_gain(gate: &mut NoiseGate, input: &[f32], block: usize) -> Vec<f32> {
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(block) {
            gate.process(chunk);
        }
        output
            .iter()
            .zip(input)
            .map(|(o, i)| (*o as f64 / *i as f64) as f32)
            .collect()
    }

    /// The largest step between consecutive samples *inside* a block, and the largest step
    /// *across* a block boundary, over the half-open index range `range`.
    ///
    /// This pair is the whole of FR-GATE-030's "within the block, not stepped at block
    /// boundaries": a gate that recomputed its gain once per `process` call would show
    /// `interior == 0` with a large `boundary`, and one that interpolates per sample shows the two
    /// equal. Neither number is visible at all when `process` is called one sample at a time,
    /// which is why the older test above cannot falsify the distinction.
    fn interior_and_boundary_steps(
        gain: &[f32],
        block: usize,
        range: std::ops::Range<usize>,
    ) -> (f32, f32) {
        let mut interior = 0.0f32;
        let mut boundary = 0.0f32;
        for i in range.start..range.end.min(gain.len()) - 1 {
            let delta = (gain[i + 1] - gain[i]).abs();
            if (i + 1) % block == 0 {
                boundary = boundary.max(delta);
            } else {
                interior = interior.max(delta);
            }
        }
        (interior, boundary)
    }

    /// The most distinct gain values any single block in `range` carries. One, for a gate that
    /// steps at block boundaries; the block length, for one that ramps per sample.
    fn most_distinct_gains_in_any_block(
        gain: &[f32],
        block: usize,
        range: std::ops::Range<usize>,
    ) -> usize {
        gain[range]
            .chunks(block)
            .map(|c| {
                let mut bits: Vec<u32> = c.iter().map(|v| v.to_bits()).collect();
                bits.sort_unstable();
                bits.dedup();
                bits.len()
            })
            .max()
            .unwrap_or(0)
    }

    // trace: FR-GATE-030
    #[test]
    fn both_gain_ramps_are_sample_accurate_inside_the_block() {
        let sample_rate = 48_000u32;
        let block = 64usize;
        let mut gate = NoiseGate::new(sr(sample_rate));
        let params = GateParams::default();
        // FR-GATE-010's defaults at 48 kHz: 1 ms attack, 30 ms hold, 100 ms release.
        let attack_samples = (params.attack_ms / 1000.0 * sample_rate as f32) as usize;
        let release_samples = (params.release_ms / 1000.0 * sample_rate as f32) as usize;
        assert_eq!((attack_samples, release_samples), (48, 4800));

        let loud = namir_core::db_to_linear(-10.0);
        // Below the close threshold (-70 dBFS minus 3 dB of hysteresis) but not silent, so
        // `per_sample_gain` can still see the closing ramp. -90 dBFS at a gain of 1/4800 is
        // ~6.6e-9, far above f32's smallest normal.
        let quiet = namir_core::db_to_linear(-90.0);

        // 30 ms loud (opens, then fully open), then 300 ms quiet (hold expires, releases fully).
        let loud_samples = 1_440usize;
        let mut input = vec![loud; loud_samples];
        input.resize(loud_samples + 14_400, quiet);
        let gain = per_sample_gain(&mut gate, &input, block);
        assert_eq!(gate.status(), GateStatus::Closed, "gate never fully closed");

        // --- Opening ramp: the first stretch where 0 < gain < 1.
        let open_start = gain
            .iter()
            .position(|&g| g > 0.0)
            .expect("gate never opened");
        let open_end = gain[open_start..]
            .iter()
            .position(|&g| g >= 1.0)
            .expect("gate never reached unity")
            + open_start;
        let opening = open_start..open_end + 1;
        assert!(
            opening.len() >= attack_samples - 2,
            "opening ramp spans {} samples, expected about {attack_samples}",
            opening.len()
        );

        // --- Closing ramp: from the last sample at unity to the first back at zero.
        let close_start = gain
            .iter()
            .rposition(|&g| g >= 1.0)
            .expect("never at unity");
        let close_end = gain[close_start..]
            .iter()
            .position(|&g| g <= 0.0)
            .expect("gate never reached zero")
            + close_start;
        let closing = close_start..close_end + 1;
        assert!(
            closing.len() >= release_samples - 2,
            "closing ramp spans {} samples, expected about {release_samples}",
            closing.len()
        );

        for (label, range, ramp_samples) in [
            ("opening", opening, attack_samples),
            ("closing", closing, release_samples),
        ] {
            // The per-sample delta the requirement's method names, on *both* edges: an ideal
            // linear ramp over `ramp_samples` steps by 1/ramp_samples, and nothing may exceed it
            // by more than rounding.
            let (interior, boundary) = interior_and_boundary_steps(&gain, block, range.clone());
            let ideal = 1.0 / ramp_samples as f32;
            assert!(
                interior > 0.0,
                "{label}: gain never changed inside a block — that is a block-stepped ramp"
            );
            assert!(
                interior <= ideal * 1.01,
                "{label}: max in-block step {interior} exceeds the ideal {ideal}"
            );
            // The falsifiable half: a boundary step no larger than an interior one. A gate that
            // updated its gain once per `process` call would put the whole ramp here.
            assert!(
                boundary <= interior * 1.01,
                "{label}: step across a block boundary ({boundary}) exceeds the largest step \
                 inside one ({interior}) — the ramp is stepped at block boundaries"
            );
            let distinct = most_distinct_gains_in_any_block(&gain, block, range);
            assert!(
                distinct >= (block / 2).min(ramp_samples),
                "{label}: the busiest block carried only {distinct} distinct gain values"
            );
        }
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
