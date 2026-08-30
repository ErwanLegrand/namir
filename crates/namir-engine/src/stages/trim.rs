//! Trim stage (FR-IN-010/020/030/040): input gain with a click-free ramp, an optional DC-blocking
//! high-pass, and metering — plus, uniquely among the six stages
//! (`03-implementation-roadmap.md` §6), the chain's *only* real cross-channel mixing.
//!
//! Runtime order is `gate → trim → ...` (D-9.8; see `stages/mod.rs`'s doc comment), not
//! FR-CHAIN-010's literal prose order — this module doesn't need to know that, it just
//! implements Trim itself.
//!
//! # Why Trim owns the downmix
//!
//! `StageIo`'s channel count is fixed for the whole chain to
//! `ctx.channel_config().output_channels()` (`stage_io.rs`'s own doc comment). Every stage after
//! this one relies on the invariant "every channel carries an identical signal whenever
//! `channel_count() > 1`" (FR-CHAIN-050: Gate/Nam are mono-core, Eq/Out process channels
//! independently off a shared parameter target — neither shape needs or wants channels to
//! diverge). Establishing that invariant from whatever the host handed in is Trim's job: for
//! `MonoToStereo` both channels already carry the same duplicated signal, so the -6 dB-both-terms
//! sum below is a no-op by construction (it re-derives the same shared signal, just attenuated,
//! not a blend of two different signals); for `Stereo` the channels genuinely differ, and the
//! same rule performs FR-CHAIN-060's default "2 ch summed to the mono core at -6 dB". One rule,
//! two configurations, no per-configuration branching needed.
//!
//! Trim is not in FR-CHAIN-020's bypassable list, so unlike Gate/Nam/Ir/Eq this stage has no
//! dry/wet crossfade machinery.

use namir_core::{SampleRate, db_to_linear};
use namir_dsp::{DcBlocker, GainRamp, Meter};
use namir_params::ParamKind;
use namir_params::stages::trim::{DC_BLOCKER_ENABLED, GAIN_DB};

use crate::param::{ParamChange, ParamId};
use crate::prepare::{PrepareContext, PrepareError};
use crate::stage::{Stage, StagePrep};
use crate::stage_io::StageIo;
use crate::telemetry::{TelemetryEntry, TelemetrySink};

/// `GainRamp`'s one-pole time constant. `gain_ramp.rs`'s own doc comment derives 20 ms as the
/// exact bound FR-PARAM-040 ("no worse than a 20 ms linear ramp") implies for a one-pole, with
/// "very little margin" against `f32` rounding at exactly that figure; 25 ms is that module's own
/// documented choice of comfortable margin, reproduced here since its public API imposes no
/// default and leaves the choice to each caller.
const GAIN_RAMP_TIME_CONSTANT_MS: f32 = 25.0;

/// FR-IN-040: "corner no higher than 20 Hz."
const DC_BLOCKER_CORNER_HZ: f32 = 20.0;

/// FR-CHAIN-060's default stereo-to-mono-core mix: both terms attenuated by -6 dB before summing,
/// rather than a plain 0.5/0.5 average, per that requirement's own wording.
const DOWNMIX_EACH_TERM_DB: f32 = -6.0;

/// This stage's RT-facing `namir_engine::ParamId`, converted once from `namir_params`'s own id
/// for the same key. The two crates carry distinct `ParamId` types on purpose (see
/// `namir_params`'s crate doc: `namir-engine`'s is "a separate, deliberately bare RT-path type"),
/// so matching in `apply` goes through this converted constant rather than comparing across
/// types.
const GAIN_DB_ID: ParamId = ParamId(GAIN_DB.id.0);
/// See [`GAIN_DB_ID`].
const DC_BLOCKER_ENABLED_ID: ParamId = ParamId(DC_BLOCKER_ENABLED.id.0);

/// Telemetry signal ids, derived from a namespaced string the same way `namir-params`'s real
/// parameter ids are (this crate's shared telemetry-id convention) — these are readouts, not
/// automatable parameters, so they are never added to `namir_params::REGISTRY`.
const TELEMETRY_PEAK_DB: u32 = namir_params::ParamId::from_key("telemetry.trim.peak_db").0;
/// See [`TELEMETRY_PEAK_DB`].
const TELEMETRY_AVERAGE_DB: u32 = namir_params::ParamId::from_key("telemetry.trim.average_db").0;
/// See [`TELEMETRY_PEAK_DB`].
const TELEMETRY_PEAK_HOLD_DB: u32 =
    namir_params::ParamId::from_key("telemetry.trim.peak_hold_db").0;
/// See [`TELEMETRY_PEAK_DB`].
const TELEMETRY_CLIPPED: u32 = namir_params::ParamId::from_key("telemetry.trim.clipped").0;

/// Builds [`TrimStage`]. Holds no configuration of its own — Trim's only two parameters
/// (`trim.gain_db`, `trim.dc_blocker_enabled`) both seed their initial value straight from their
/// `namir-params` descriptor (see `prepare`'s body), so there is nothing for a caller to pass in
/// here.
pub struct TrimPrep;

impl StagePrep for TrimPrep {
    type Prepared = TrimStage;

    /// Sizes every buffer `TrimStage::process` will ever touch: the scratch channel used to
    /// shuttle a value across `StageIo`'s per-call reborrow (this module's own doc comment), and
    /// the DSP primitives' own per-sample-rate state.
    fn prepare(&self, ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError> {
        let sample_rate = ctx.sample_rate();

        // Seed initial values from the descriptors rather than a second hardcoded copy of their
        // range/default (this crate's own convention). The `unreachable!` arm is only a
        // defensive check against a future edit to `namir-params` changing a descriptor's `kind`
        // out from under this file -- it cannot be reached by any input `prepare` is passed.
        let gain_default_db = match GAIN_DB.kind {
            ParamKind::Continuous { default, .. } => default,
            ParamKind::Stepped { .. } => unreachable!("trim.gain_db is declared Continuous"),
        };
        let dc_blocker_default_on = match DC_BLOCKER_ENABLED.kind {
            ParamKind::Stepped { default_index, .. } => default_index.0 == 1,
            ParamKind::Continuous { .. } => {
                unreachable!("trim.dc_blocker_enabled is declared Stepped")
            }
        };

        Ok(TrimStage {
            gain_ramp: gain_ramp_at_default(sample_rate, gain_default_db),
            dc_blocker: DcBlocker::new(sample_rate, DC_BLOCKER_CORNER_HZ),
            dc_blocker_enabled: dc_blocker_default_on,
            meter: Meter::new(sample_rate),
            downmix_gain: db_to_linear(DOWNMIX_EACH_TERM_DB),
            scratch: vec![0.0; ctx.max_block_size()],
        })
    }
}

/// RT-safe input trim: gain ramp, optional DC blocker, metering, and (uniquely among the six
/// stages) the chain's stereo-to-mono-core downmix. See this module's doc comment for the
/// channel-handling rationale.
pub struct TrimStage {
    /// FR-IN-010's gain control, smoothed per [`GAIN_RAMP_TIME_CONSTANT_MS`].
    gain_ramp: GainRamp,
    /// FR-IN-040's optional DC-blocking high-pass.
    dc_blocker: DcBlocker,
    /// Whether `dc_blocker` runs this block; toggled by `apply`, defaulted from
    /// `DC_BLOCKER_ENABLED`'s descriptor.
    dc_blocker_enabled: bool,
    /// FR-IN-020/030's peak/average/peak-hold/clip readout, measured on the post-trim signal
    /// (after gain and the DC blocker, so it reflects what actually leaves this stage).
    meter: Meter,
    /// `db_to_linear(DOWNMIX_EACH_TERM_DB)`, computed once here rather than re-deriving a `powf`
    /// every block (the same house pattern `GainRamp::set_target_db`'s doc comment names).
    downmix_gain: f32,
    /// Per-channel shuttle buffer (D-6.2/P1): `StageIo::channel`'s per-call reborrow of `&mut
    /// self` means two channels can never be borrowed at once (this module's own doc comment on
    /// the crate-level gotcha), so reading one channel while writing another goes through this
    /// buffer instead. Sized to `ctx.max_block_size()` in `prepare`; never resized in `process`.
    scratch: Vec<f32>,
}

impl Stage for TrimStage {
    fn process(&mut self, io: &mut StageIo<'_>) {
        let n = io.frames();
        let channel_count = io.channel_count();

        if channel_count >= 2 {
            // Copy channel 1 out before touching channel 0: holding both `channel()` borrows at
            // once does not compile (this module's own doc comment). -6 dB on *both* terms, per
            // FR-CHAIN-060, not a plain 0.5/0.5 average.
            self.scratch[..n].copy_from_slice(io.channel(1));
            let downmix_gain = self.downmix_gain;
            let right = &self.scratch[..n];
            let left = io.channel(0);
            for (l, &r) in left.iter_mut().zip(right.iter()) {
                *l = *l * downmix_gain + r * downmix_gain;
            }
        }

        // From here on there is exactly one signal (channel 0, already carrying the full mix
        // when channel_count >= 2): ramp, then DC-block, then meter -- metering last so it reads
        // the actual post-trim signal, not a pre-filter one.
        self.gain_ramp.process(io.channel(0));
        if self.dc_blocker_enabled {
            self.dc_blocker.process(io.channel(0));
        }
        self.meter.process(io.channel(0));

        if channel_count >= 2 {
            // Re-establish "every channel identical" for whatever comes next in the chain.
            self.scratch[..n].copy_from_slice(io.channel(0));
            let result = &self.scratch[..n];
            for ch in 1..channel_count {
                io.channel(ch).copy_from_slice(result);
            }
        }
    }

    fn reset(&mut self) {
        // `gain_ramp` deliberately keeps its current smoothed value: a reset is a transport
        // stop/reposition, not a parameter change (this stage's own spec).
        self.dc_blocker.reset();
        self.meter.reset();
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail_samples(&self) -> u32 {
        0
    }

    fn apply(&mut self, change: ParamChange) {
        if change.id == GAIN_DB_ID {
            self.gain_ramp.set_target_db(change.value);
        } else if change.id == DC_BLOCKER_ENABLED_ID {
            // Stepped param value is the index as f32 (`ParamChange`'s own doc comment); index 1
            // is "On" per `DC_BLOCKER_ENABLED`'s descriptor.
            self.dc_blocker_enabled = change.value >= 0.5;
        }
    }

    fn telemetry(&self, out: &mut TelemetrySink<'_>) {
        out.push(TelemetryEntry {
            id: TELEMETRY_PEAK_DB,
            value: self.meter.peak_db(),
        });
        out.push(TelemetryEntry {
            id: TELEMETRY_AVERAGE_DB,
            value: self.meter.average_db(),
        });
        out.push(TelemetryEntry {
            id: TELEMETRY_PEAK_HOLD_DB,
            value: self.meter.peak_hold_db(),
        });
        out.push(TelemetryEntry {
            id: TELEMETRY_CLIPPED,
            value: if self.meter.clipped() { 1.0 } else { 0.0 },
        });
    }
}

/// Issue #127's follow-up: the one place this stage's gain ramp is constructed, so `prepare` and the
/// test that pins its start point cannot drift apart. `GainRamp::new_at_db` rather than
/// `GainRamp::new` followed by `set_target_db`: the latter leaves `current` at unity and `target`
/// at the default, so the first ~25 ms of audio after every prepare, sample-rate change or
/// re-prepare ramps from 0 dB to the parameter's real default. That is inaudible only because
/// `trim.gain_db` happens to default to 0.0 dB today — see `GainRamp::new_at_db`'s own doc comment.
fn gain_ramp_at_default(sample_rate: SampleRate, default_db: f32) -> GainRamp {
    GainRamp::new_at_db(sample_rate, GAIN_RAMP_TIME_CONSTANT_MS, default_db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;
    use namir_core::{ChannelConfig, SampleRate};

    fn ctx(channel_config: ChannelConfig) -> PrepareContext {
        PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, channel_config).unwrap()
    }

    fn stage(channel_config: ChannelConfig) -> TrimStage {
        TrimPrep.prepare(&ctx(channel_config)).unwrap()
    }

    /// Drives the (mono) gain ramp to convergence: 200 blocks of 64 samples at 48 kHz is ~267 ms,
    /// comfortably more than ten 25 ms time constants past any target change.
    fn settle_mono_gain_ramp(stage: &mut TrimStage) {
        for _ in 0..200 {
            let mut buf = [1.0f32; 64];
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, 64);
            audio_section(|| stage.process(&mut io));
        }
    }

    /// Processes `total` samples of a constant `value` through a mono stage in
    /// `PrepareContext`-respecting 64-sample chunks, returning the last output sample. Used by
    /// the DC-blocker test, which needs far more than one block's worth of settling time.
    fn process_constant_in_chunks(stage: &mut TrimStage, total: usize, value: f32) -> f32 {
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

    /// Issue #127's follow-up. The defect is invisible at the shipped 0.0 dB default — a ramp from
    /// unity to unity has nowhere to travel — so this drives the same constructor `prepare` uses
    /// with a default that is *not* unity, which is the shape the trap is set for. Red against
    /// `GainRamp::new` + `set_target_db`: that starts `current` at 1.0 and the first block fades.
    #[test]
    fn the_gain_ramp_is_built_settled_at_a_non_unity_default() {
        let sample_rate = SampleRate::new(48_000).unwrap();
        let mut ramp = gain_ramp_at_default(sample_rate, -12.0);
        assert!(
            (ramp.current_db() - (-12.0)).abs() < 1e-4,
            "the ramp starts at {} dB, not the -12.0 dB default it was built with",
            ramp.current_db()
        );

        let expected = db_to_linear(-12.0);
        let mut buf = [1.0f32; 64];
        ramp.process(&mut buf);
        for (i, x) in buf.iter().enumerate() {
            assert!(
                (x - expected).abs() < 1e-6,
                "sample {i} of the first block is {x}, not {expected} -- the ramp is \
                 travelling to its default instead of starting there"
            );
        }
    }

    /// The other half: `prepare` really does route through gain_ramp_at_default, so the assertion above is
    /// about this stage and not just about `namir-dsp`. At the shipped default this is a
    /// tripwire rather than a live check -- it starts failing the day the default moves and the
    /// construction has drifted back.
    #[test]
    fn a_prepared_stage_starts_settled_at_the_gain_default() {
        let stage = stage(ChannelConfig::Mono);
        let default_db = match GAIN_DB.kind {
            ParamKind::Continuous { default, .. } => default,
            ParamKind::Stepped { .. } => unreachable!("trim.gain_db is declared Continuous"),
        };
        assert!(
            (stage.gain_ramp.current_db() - default_db).abs() < 1e-4,
            "a freshly prepared stage's ramp sits at {} dB, not its {default_db} dB default",
            stage.gain_ramp.current_db()
        );
    }

    // trace: FR-IN-010
    #[test]
    fn pure_gain_is_applied_once_settled() {
        let mut stage = stage(ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: GAIN_DB_ID,
            value: -6.0,
        });
        // Isolate the gain path from the DC blocker.
        stage.apply(ParamChange {
            id: DC_BLOCKER_ENABLED_ID,
            value: 0.0,
        });
        settle_mono_gain_ramp(&mut stage);

        let mut buf = [0.5f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        let expected = 0.5 * db_to_linear(-6.0);
        for s in io.channel(0) {
            assert!((*s - expected).abs() < 1e-3, "got {s}, expected {expected}");
        }
    }

    #[test]
    fn dc_blocker_enabled_removes_dc_disabled_passes_it() {
        // Enabled is the descriptor default: a long run of DC settles towards zero.
        let mut enabled = stage(ChannelConfig::Mono);
        let tail_enabled = process_constant_in_chunks(&mut enabled, 48_000, 1.0);
        assert!(
            tail_enabled.abs() < 1e-2,
            "expected DC heavily attenuated when enabled, got {tail_enabled}"
        );

        // Disabled: DC passes through essentially unattenuated (unity gain, no target change).
        let mut disabled = stage(ChannelConfig::Mono);
        disabled.apply(ParamChange {
            id: DC_BLOCKER_ENABLED_ID,
            value: 0.0,
        });
        let tail_disabled = process_constant_in_chunks(&mut disabled, 6_400, 1.0);
        assert!(
            tail_disabled > 0.99,
            "expected DC to pass through when disabled, got {tail_disabled}"
        );
    }

    #[test]
    fn stereo_downmix_sums_both_channels_at_minus_six_db_identically() {
        let mut stage = stage(ChannelConfig::Stereo);
        // Isolate the downmix arithmetic from the DC blocker.
        stage.apply(ParamChange {
            id: DC_BLOCKER_ENABLED_ID,
            value: 0.0,
        });

        let mut left = [0.2f32; 64];
        let mut right = [0.8f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        let g = db_to_linear(DOWNMIX_EACH_TERM_DB);
        let expected = 0.2 * g + 0.8 * g;
        for s in io.channel(0) {
            assert!(
                (*s - expected).abs() < 1e-4,
                "left: got {s}, expected {expected}"
            );
        }
        for s in io.channel(1) {
            assert!(
                (*s - expected).abs() < 1e-4,
                "right: got {s}, expected {expected}"
            );
        }
    }

    #[test]
    fn mono_to_stereo_duplicated_input_downmixes_identically_on_both_channels() {
        // Both channels already carry the same signal in MonoToStereo, so the downmix is a
        // no-op on *content* -- it still applies FR-CHAIN-060's -6 dB-per-term attenuation
        // uniformly, since that rule doesn't distinguish "genuinely stereo" from "duplicated
        // mono" input.
        let mut stage = stage(ChannelConfig::MonoToStereo);
        stage.apply(ParamChange {
            id: DC_BLOCKER_ENABLED_ID,
            value: 0.0,
        });

        let mut left = [0.3f32; 64];
        let mut right = [0.3f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        let g = db_to_linear(DOWNMIX_EACH_TERM_DB);
        let expected = 0.3 * g * 2.0;
        for s in io.channel(0) {
            assert!((*s - expected).abs() < 1e-4, "got {s}, expected {expected}");
        }

        // Copy out (allocates, but we're past `audio_section` by now) to compare both channels
        // without holding two `channel()` borrows of `io` at once (this module's own gotcha).
        let left_out = io.channel(0).to_vec();
        let right_out = io.channel(1).to_vec();
        for (l, r) in left_out.iter().zip(right_out.iter()) {
            assert!((l - r).abs() < 1e-6, "channels diverged: {l} vs {r}");
        }
    }

    #[test]
    fn mono_passthrough_skips_downmix_entirely() {
        let mut stage = stage(ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: DC_BLOCKER_ENABLED_ID,
            value: 0.0,
        });

        let mut buf = [0.4f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        // Unity gain (default), DC blocker disabled, single channel: output equals input.
        for s in io.channel(0) {
            assert!((*s - 0.4).abs() < 1e-4, "got {s}");
        }
    }

    // trace-partial: FR-IN-030
    // uncovered: FR-IN-030 — the "resettable by the user" clause is unbuilt as well as
    // uncovered: unverified: Meter::reset_clip has no caller outside its own unit test, TrimStage
    // uncovered: exposes no clip-reset parameter and its Stage::reset is the transport-stop path,
    // uncovered: and UiIntent carries no reset-clip variant; closes M8
    #[test]
    fn clip_latches_and_is_reported_via_telemetry() {
        let mut stage = stage(ChannelConfig::Mono);

        let mut buf = [1.5f32; 64]; // over full scale.
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        let mut storage = [TelemetryEntry { id: 0, value: 0.0 }; 8];
        let mut sink = TelemetrySink::new(&mut storage);
        stage.telemetry(&mut sink);
        let clipped = sink
            .entries()
            .find(|e| e.id == TELEMETRY_CLIPPED)
            .expect("clipped telemetry entry present");
        assert_eq!(clipped.value, 1.0);

        // Stays latched across a subsequent quiet block.
        let mut quiet = [0.1f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut quiet];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        let mut sink = TelemetrySink::new(&mut storage);
        stage.telemetry(&mut sink);
        let clipped = sink
            .entries()
            .find(|e| e.id == TELEMETRY_CLIPPED)
            .expect("clipped telemetry entry present");
        assert_eq!(clipped.value, 1.0, "clip indicator must stay latched");
    }

    /// The path most likely to allocate if `scratch` is undersized or absent: stereo, so both
    /// the downmix and the duplicate-back shuttle run in the same block.
    #[test]
    fn stereo_process_does_not_allocate() {
        let mut stage = stage(ChannelConfig::Stereo);
        let mut left = [0.1f32; 64];
        let mut right = [0.2f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));
    }
}
