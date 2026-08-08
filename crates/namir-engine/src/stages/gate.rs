//! Gate stage (FR-GATE-010..040): `namir_dsp::NoiseGate` wired into the `Stage` trait, the
//! shared per-stage bypass crossfade (FR-CHAIN-020), and FR-CHAIN-050's mono-core-then-duplicate
//! channel handling.
//!
//! Runtime order is `gate → trim → ...` (D-9.8; see `stages/mod.rs`'s doc comment) — this module
//! doesn't need to know that, it just implements Gate itself.
//!
//! # Why this is mono-core
//!
//! FR-CHAIN-050 treats Gate as conceptually mono: by the time this stage runs, every channel of
//! `io` already carries an identical signal (an invariant either Trim establishes downstream of
//! here, for `channel_count() > 1`, or that trivially holds for a true mono chain) since Gate
//! itself runs *before* Trim in this chain (D-9.8). Detecting and gating on channel 0 alone, then
//! duplicating the result, keeps that invariant intact for whatever comes next rather than
//! running (and potentially diverging) an independent detector per channel.

use namir_dsp::{GateParams, NoiseGate};
use namir_params::ParamKind;
use namir_params::stages::gate::{ATTACK_MS, ENABLED, HOLD_MS, RELEASE_MS, THRESHOLD_DB};

use crate::param::{ParamChange, ParamId};
use crate::prepare::{PrepareContext, PrepareError};
use crate::stage::{Stage, StagePrep};
use crate::stage_io::StageIo;
use crate::telemetry::{TelemetryEntry, TelemetrySink};

/// The dry/wet crossfade's one-pole time constant (FR-CHAIN-020's click-free bypass). 15 ms is
/// this stage's own documented figure for the shared bypass pattern (not derived from an FRS
/// requirement the way `GAIN_RAMP`'s 20 ms bound in `trim.rs` is) — see `process`'s use of
/// `mix_coeff` for where it's actually applied.
const BYPASS_CROSSFADE_TIME_CONSTANT_MS: f64 = 15.0;

/// This stage's RT-facing `namir_engine::ParamId`s, converted once from `namir_params`'s own ids
/// for the same keys (see `trim.rs`'s identical convention and its doc comment for why the two
/// crates carry distinct `ParamId` types on purpose).
const ENABLED_ID: ParamId = ParamId(ENABLED.id.0);
/// See [`ENABLED_ID`].
const THRESHOLD_DB_ID: ParamId = ParamId(THRESHOLD_DB.id.0);
/// See [`ENABLED_ID`].
const ATTACK_MS_ID: ParamId = ParamId(ATTACK_MS.id.0);
/// See [`ENABLED_ID`].
const HOLD_MS_ID: ParamId = ParamId(HOLD_MS.id.0);
/// See [`ENABLED_ID`].
const RELEASE_MS_ID: ParamId = ParamId(RELEASE_MS.id.0);

/// Telemetry signal id (FR-GATE-040), derived from a namespaced string the same way
/// `namir-params`'s real parameter ids are (this crate's shared telemetry-id convention) — this
/// is a readout, not an automatable parameter, so it is never added to `namir_params::REGISTRY`.
const TELEMETRY_GAIN_REDUCTION_DB: u32 =
    namir_params::ParamId::from_key("telemetry.gate.gain_reduction_db").0;

/// Reads a `Continuous` descriptor's default, panicking (defensively; unreachable from any input
/// `prepare` is passed) if a future edit to `namir-params` changes the descriptor's `kind` out
/// from under this file.
fn continuous_default(descriptor: namir_params::ParamDescriptor) -> f32 {
    match descriptor.kind {
        ParamKind::Continuous { default, .. } => default,
        ParamKind::Stepped { .. } => unreachable!("{} is declared Continuous", descriptor.key),
    }
}

/// Builds [`GateStage`]. Holds no configuration of its own — every one of Gate's five parameters
/// seeds its initial value straight from its `namir-params` descriptor (see `prepare`'s body), so
/// there is nothing for a caller to pass in here.
pub struct GatePrep;

impl StagePrep for GatePrep {
    type Prepared = GateStage;

    /// Sizes every buffer `GateStage::process` will ever touch: the per-channel dry scratch the
    /// bypass crossfade needs, and the channel-0-then-duplicate shuttle buffer FR-CHAIN-050's
    /// mono-core handling needs (`StageIo::channel`'s per-call reborrow of `&mut self` means two
    /// channels can never be borrowed at once — this crate's own cross-file gotcha, see
    /// `trim.rs`'s identical note).
    fn prepare(&self, ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError> {
        let sample_rate = ctx.sample_rate();
        let max_block = ctx.max_block_size();
        let channel_count = ctx.channel_config().output_channels() as usize;

        // Seed initial values from the descriptors rather than a second hardcoded copy of their
        // range/default (this crate's own convention, matching `trim.rs`). `hysteresis_db` has
        // no `namir-params` descriptor (`namir_dsp::gate`'s own doc comment: it's that crate's
        // engineering default pending a UI control decision), so it comes from
        // `GateParams::default()` rather than from a descriptor that doesn't exist.
        let enabled_default_on = match ENABLED.kind {
            ParamKind::Stepped { default_index, .. } => default_index.0 == 1,
            ParamKind::Continuous { .. } => unreachable!("gate.enabled is declared Stepped"),
        };
        let params = GateParams {
            threshold_db: continuous_default(THRESHOLD_DB),
            attack_ms: continuous_default(ATTACK_MS),
            hold_ms: continuous_default(HOLD_MS),
            release_ms: continuous_default(RELEASE_MS),
            ..GateParams::default()
        };

        let mut detector = NoiseGate::new(sample_rate);
        detector.set_params(params);

        let tau_samples = (BYPASS_CROSSFADE_TIME_CONSTANT_MS / 1000.0) * sample_rate.hz_f64();
        let mix_coeff = (1.0 - (-1.0_f64 / tau_samples).exp()) as f32;
        let mix_target = if enabled_default_on { 1.0 } else { 0.0 };

        Ok(GateStage {
            detector,
            params,
            enabled: enabled_default_on,
            mix: mix_target, // no prior audio exists yet at stage creation; start settled.
            mix_target,
            mix_coeff,
            dry: vec![vec![0.0; max_block]; channel_count],
            scratch: vec![0.0; max_block],
        })
    }
}

/// RT-safe noise gate: `namir_dsp::NoiseGate` run mono-core on channel 0 and duplicated
/// (FR-CHAIN-050), behind the shared click-free per-stage bypass crossfade (FR-CHAIN-020).
pub struct GateStage {
    /// FR-GATE-010..040's hysteresis/attack/hold/release state machine.
    detector: NoiseGate,
    /// This stage's own copy of `detector`'s current params. `NoiseGate` exposes no getter for
    /// them (`namir_dsp::gate`'s own scope), so `apply` mutates the relevant field here, then
    /// calls `detector.set_params` with the whole struct — recomputing every coefficient from
    /// scratch each time, which is fine at control rate (`set_params`'s own doc comment).
    params: GateParams,
    /// Whether this stage is enabled (FR-GATE-010's "Enabled" control, also FR-CHAIN-020's
    /// per-stage bypass for this stage). Tracked separately from `mix`/`mix_target` because it's
    /// the semantic on/off state `telemetry`/a future host query would want, not the crossfade's
    /// own in-flight progress.
    enabled: bool,
    /// Current dry/wet blend: `0.0` = fully dry/bypassed, `1.0` = fully wet/engaged. Advances
    /// toward `mix_target` by `mix_coeff` each sample (one-pole), never jumps.
    mix: f32,
    /// Where `mix` is heading: `1.0` when `enabled`, `0.0` otherwise (FR-CHAIN-040's "nothing
    /// loaded behaves as bypassed" doesn't apply to Gate — it has no loadable resource — so this
    /// only ever reflects `enabled`).
    mix_target: f32,
    /// One-pole coefficient for the `mix` crossfade, computed once in `prepare` from
    /// [`BYPASS_CROSSFADE_TIME_CONSTANT_MS`] and the sample rate.
    mix_coeff: f32,
    /// Per-channel pre-gate signal, captured at the top of every `process` call so the bypass
    /// crossfade has something to blend the gated ("wet") signal back against. One `Vec<f32>` per
    /// output channel, each sized to `ctx.max_block_size()` in `prepare`; never resized in
    /// `process`.
    dry: Vec<Vec<f32>>,
    /// Shuttle buffer for FR-CHAIN-050's channel-0-then-duplicate pattern: `StageIo::channel`'s
    /// per-call reborrow means channel 0's gated result must be copied out before a fresh
    /// mutable borrow of another channel can write it back in. Sized to `ctx.max_block_size()` in
    /// `prepare`; never resized in `process`.
    scratch: Vec<f32>,
}

impl Stage for GateStage {
    fn process(&mut self, io: &mut StageIo<'_>) {
        let n = io.frames();
        let channel_count = io.channel_count();

        for ch in 0..channel_count {
            self.dry[ch][..n].copy_from_slice(io.channel(ch));
        }

        // Wet: mono-core gate on channel 0 (this module's own doc comment), then duplicate its
        // gated result into every other channel via the scratch shuttle.
        self.detector.process(io.channel(0));
        if channel_count > 1 {
            self.scratch[..n].copy_from_slice(io.channel(0));
            let gated = &self.scratch[..n];
            for ch in 1..channel_count {
                io.channel(ch).copy_from_slice(gated);
            }
        }

        // Shared per-stage bypass crossfade (FR-CHAIN-020): every channel blends from the same
        // `start_mix` via the same per-sample recurrence, recomputed per channel rather than
        // carried over between channels, so every channel's fade stays in phase; only the last
        // channel's trajectory is committed back to `self.mix`, so `mix` advances `n` steps total
        // per block, not `n * channel_count`.
        let start_mix = self.mix;
        let last = channel_count - 1;
        for ch in 0..channel_count {
            let mut m = start_mix;
            let wet = io.channel(ch);
            let dry = &self.dry[ch][..n];
            for i in 0..n {
                m += self.mix_coeff * (self.mix_target - m);
                wet[i] = dry[i] * (1.0 - m) + wet[i] * m;
            }
            if ch == last {
                self.mix = m;
            }
        }
    }

    fn reset(&mut self) {
        // `namir_dsp::NoiseGate::reset`'s own scope is detector/state-machine only, not params —
        // and per that same boundary, not this stage's bypass-mix state either (a reset is a
        // transport stop/reposition, not a parameter or bypass-state change; matches `trim.rs`'s
        // identical treatment of its own gain ramp).
        self.detector.reset();
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail_samples(&self) -> u32 {
        0
    }

    fn apply(&mut self, change: ParamChange) {
        if change.id == ENABLED_ID {
            // Stepped param value is the index as f32 (`ParamChange`'s own doc comment); index 1
            // is "On" per `ENABLED`'s descriptor.
            self.enabled = change.value >= 0.5;
            self.mix_target = if self.enabled { 1.0 } else { 0.0 };
        } else if change.id == THRESHOLD_DB_ID {
            self.params.threshold_db = change.value;
            self.detector.set_params(self.params);
        } else if change.id == ATTACK_MS_ID {
            self.params.attack_ms = change.value;
            self.detector.set_params(self.params);
        } else if change.id == HOLD_MS_ID {
            self.params.hold_ms = change.value;
            self.detector.set_params(self.params);
        } else if change.id == RELEASE_MS_ID {
            self.params.release_ms = change.value;
            self.detector.set_params(self.params);
        }
    }

    fn telemetry(&self, out: &mut TelemetrySink<'_>) {
        out.push(TelemetryEntry {
            id: TELEMETRY_GAIN_REDUCTION_DB,
            value: self.detector.gain_reduction_db(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;
    use namir_core::{ChannelConfig, SampleRate, db_to_linear};

    fn ctx(channel_config: ChannelConfig) -> PrepareContext {
        PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, channel_config).unwrap()
    }

    fn stage(channel_config: ChannelConfig) -> GateStage {
        GatePrep.prepare(&ctx(channel_config)).unwrap()
    }

    /// Runs `total` samples of a constant `value` through a mono stage in 64-sample chunks
    /// (`PrepareContext`'s own `max_block_size`), returning the last output sample.
    fn process_constant_in_chunks(stage: &mut GateStage, total: usize, value: f32) -> f32 {
        let mut buf = vec![value; total];
        let mut offset = 0usize;
        while offset < buf.len() {
            let end = (offset + 64).min(buf.len());
            let n = end - offset;
            let mut channels: [&mut [f32]; 1] = [&mut buf[offset..end]];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            offset = end;
        }
        buf[buf.len() - 1]
    }

    // trace-partial: FR-GATE-010
    // uncovered: FR-GATE-010 — of the five controls the "U per control" method names, Attack and
    // uncovered: Release are exercised by no test: ATTACK_MS_ID is never written anywhere in the
    // uncovered: workspace, and RELEASE_MS_ID only as a helper inside a test whose assertion is
    // uncovered: about hold; closes M9b
    #[test]
    fn burst_opens_and_silence_closes_through_the_stage() {
        let mut stage = stage(ChannelConfig::Mono);
        // Well above threshold (-70 dBFS default): -10 dBFS, long enough (1 s) to fully open and
        // settle the bypass crossfade (enabled is the descriptor default, so mix starts at 1.0
        // already -- this just proves the detector itself opens through the Stage wiring).
        let loud = db_to_linear(-10.0);
        let tail = process_constant_in_chunks(&mut stage, 48_000, loud);
        assert!(
            (tail - loud).abs() < 1e-3,
            "expected the gate fully open (near-unity passthrough), got {tail} vs input {loud}"
        );

        let mut storage = [TelemetryEntry { id: 0, value: 0.0 }; 4];
        let mut sink = TelemetrySink::new(&mut storage);
        stage.telemetry(&mut sink);
        let reduction = sink
            .entries()
            .find(|e| e.id == TELEMETRY_GAIN_REDUCTION_DB)
            .expect("gain reduction telemetry present");
        assert!(
            reduction.value > -0.1,
            "expected ~0 dB reduction while open, got {}",
            reduction.value
        );

        let tail = process_constant_in_chunks(&mut stage, 48_000, 0.0);
        assert!(
            tail.abs() < 1e-4,
            "expected silence once closed, got {tail}"
        );

        let mut sink = TelemetrySink::new(&mut storage);
        stage.telemetry(&mut sink);
        let reduction = sink
            .entries()
            .find(|e| e.id == TELEMETRY_GAIN_REDUCTION_DB)
            .expect("gain reduction telemetry present");
        assert!(
            reduction.value < -60.0,
            "expected heavy reduction while closed, got {}",
            reduction.value
        );
    }

    #[test]
    fn threshold_apply_actually_changes_detector_behaviour() {
        // A signal fixed at -50 dBFS: above the default -70 dBFS threshold (opens), but below a
        // raised -20 dBFS threshold (stays closed once raised) -- unambiguous in both directions.
        let signal = db_to_linear(-50.0);

        let mut default_threshold = stage(ChannelConfig::Mono);
        let tail_default = process_constant_in_chunks(&mut default_threshold, 48_000, signal);
        assert!(
            (tail_default - signal).abs() < 1e-3,
            "expected open at the default -70 dBFS threshold for a -50 dBFS signal, got {tail_default}"
        );

        let mut raised_threshold = stage(ChannelConfig::Mono);
        raised_threshold.apply(ParamChange {
            id: THRESHOLD_DB_ID,
            value: -20.0,
        });
        let tail_raised = process_constant_in_chunks(&mut raised_threshold, 48_000, signal);
        assert!(
            tail_raised.abs() < 1e-4,
            "expected closed once threshold raised above the -50 dBFS signal, got {tail_raised}"
        );
    }

    #[test]
    fn hold_apply_actually_changes_detector_behaviour() {
        // Open the gate, then feed a short silence gap shorter than the default 30 ms hold: a
        // zero-hold stage (with a fast release, so the gap actually shows measurable attenuation)
        // must have released noticeably more than a default-hold stage over the same gap.
        let loud = db_to_linear(-10.0);
        let mut storage = [TelemetryEntry { id: 0, value: 0.0 }; 4];

        let mut default_hold = stage(ChannelConfig::Mono);
        process_constant_in_chunks(&mut default_hold, 4800, loud); // 100 ms: fully open.
        process_constant_in_chunks(&mut default_hold, 480, 0.0); // 10 ms gap.
        let mut sink = TelemetrySink::new(&mut storage);
        default_hold.telemetry(&mut sink);
        let default_reduction = sink
            .entries()
            .find(|e| e.id == TELEMETRY_GAIN_REDUCTION_DB)
            .unwrap()
            .value;
        assert!(
            default_reduction > -1.0,
            "expected default 30 ms hold to still be fully open at a 10 ms gap, got {default_reduction} dB"
        );

        let mut short_hold = stage(ChannelConfig::Mono);
        short_hold.apply(ParamChange {
            id: HOLD_MS_ID,
            value: 0.0,
        });
        short_hold.apply(ParamChange {
            id: RELEASE_MS_ID,
            value: 1.0, // fast release so 10 ms of silence clearly shows measurable attenuation.
        });
        process_constant_in_chunks(&mut short_hold, 4800, loud); // 100 ms: fully open.
        process_constant_in_chunks(&mut short_hold, 480, 0.0); // 10 ms gap, no hold to absorb it.
        let mut sink = TelemetrySink::new(&mut storage);
        short_hold.telemetry(&mut sink);
        let short_reduction = sink
            .entries()
            .find(|e| e.id == TELEMETRY_GAIN_REDUCTION_DB)
            .unwrap()
            .value;
        assert!(
            short_reduction < default_reduction - 1.0,
            "expected zero-hold stage to have released measurably more: {short_reduction} dB vs {default_reduction} dB"
        );
    }

    #[test]
    fn bypass_toggle_mid_signal_is_no_worse_than_a_15ms_linear_ramp() {
        let sample_rate = 48_000u32;
        let mut stage = stage(ChannelConfig::Mono);

        // Threshold pinned at the top of its range so a moderate, constant signal never opens
        // the gate: the detector stays closed (wet = 0.0) for the entire test, isolating the
        // bypass crossfade's own click-freedom from the gate's attack/release dynamics.
        stage.apply(ParamChange {
            id: THRESHOLD_DB_ID,
            value: 0.0,
        });
        let dry_value = 0.5f32;

        // Start disabled (mix settled at 0.0 = fully dry) and let it settle. 1 s is ~67 time
        // constants at 15 ms (mirrors `gain_ramp.rs`'s own 1 s settle for a 25 ms constant);
        // `settled_dry` is the actual last pre-toggle output sample, used below as `prev` rather
        // than the theoretical `dry_value`, so any microscopic residual from an imperfectly
        // converged one-pole doesn't get mistaken for part of the toggle's own transient.
        stage.apply(ParamChange {
            id: ENABLED_ID,
            value: 0.0,
        });
        let settled_dry = process_constant_in_chunks(&mut stage, 48_000, dry_value);

        // Toggle on mid-signal: mix_target jumps from 0.0 to 1.0, a full-range instantaneous
        // change, right as the next block starts.
        stage.apply(ParamChange {
            id: ENABLED_ID,
            value: 1.0,
        });

        // 100 ms, comfortably longer than the transient, processed in `max_block_size`-sized
        // (64-sample) chunks like every other block this test drives.
        let total = 4800usize;
        let mut out = Vec::with_capacity(total);
        let mut offset = 0usize;
        while offset < total {
            let n = 64usize.min(total - offset);
            let mut buf = vec![dry_value; n];
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            out.extend_from_slice(io.channel(0));
            offset += n;
        }

        // wet (gate closed throughout) = 0.0, dry = dry_value: the blend's full range is
        // dry_value itself. Include the transition from the settled pre-toggle sample into the
        // first post-toggle sample, since that's where the one-pole's steepest step occurs --
        // mirrors `gain_ramp.rs`'s own `full_range_jump_is_no_worse_than_a_20ms_linear_ramp` test.
        let mut prev = settled_dry;
        let mut max_delta = 0.0f32;
        for &s in &out {
            max_delta = max_delta.max((s - prev).abs());
            prev = s;
        }

        let range = dry_value; // |wet - dry| = |0.0 - dry_value|
        let ideal_max_delta = range / (0.015 * sample_rate as f32);
        assert!(
            max_delta <= ideal_max_delta * 1.01,
            "max_delta={max_delta} exceeds the 15 ms linear ramp bound {ideal_max_delta}"
        );
        // The blend must actually have moved (otherwise the test would pass vacuously).
        assert!(max_delta > 0.0, "bypass crossfade never advanced");
    }

    // trace-partial: FR-CHAIN-050
    // uncovered: FR-CHAIN-050 — the mono-core-then-duplicate behaviour is shown for GateStage
    // uncovered: only: NamStage has no multi-channel content assertion (every nam.rs test is
    // uncovered: ChannelConfig::Mono bar one that asserts nothing about channel content), and
    // uncovered: ChannelConfig::MonoToStereo appears in no gate.rs or nam.rs test at all;
    // uncovered: closes M9b
    #[test]
    fn stereo_duplicates_the_mono_core_gate_result_onto_every_channel() {
        let mut stage = stage(ChannelConfig::Stereo);
        let loud = db_to_linear(-10.0);

        // Settle fully open on both channels (identical input, per FR-CHAIN-050's invariant).
        for _ in 0..800 {
            let mut left = [loud; 64];
            let mut right = [loud; 64];
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut io = StageIo::new(&mut channels, 64);
            audio_section(|| stage.process(&mut io));
        }

        let mut left = [loud; 64];
        let mut right = [loud; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        let left_out = io.channel(0).to_vec();
        let right_out = io.channel(1).to_vec();
        for (l, r) in left_out.iter().zip(right_out.iter()) {
            assert!((l - r).abs() < 1e-6, "channels diverged: {l} vs {r}");
        }
        // And it's a real passthrough, not both channels silently zeroed.
        assert!(left_out.iter().any(|&s| s.abs() > 1e-3));
    }

    /// The path most likely to allocate if `dry`/`scratch` are undersized or absent: stereo, so
    /// both the dry capture and the channel-0-then-duplicate shuttle run in the same block.
    #[test]
    fn stereo_process_does_not_allocate() {
        let mut stage = stage(ChannelConfig::Stereo);
        let mut left = [0.1f32; 64];
        let mut right = [0.1f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));
    }
}
