//! Output stage (FR-OUT-010/020): a click-free output-level gain ramp with FR-OUT-010's
//! exact-silence-at-or-below-`-60`dB floor, plus per-channel metering (FR-OUT-020, same
//! characteristics as FR-IN-020, including FR-IN-030's latching clip indicator).
//!
//! Out is not in FR-CHAIN-020's bypassable list (only Gate/Nam/Ir/Eq are), so unlike those four
//! stages this module has no dry/wet crossfade machinery — matches `trim.rs`'s identical note for
//! the chain's other non-bypassable stage.
//!
//! FR-OUT-030 (an optional brickwall limiter) is a Should, not a Must, and is out of scope for
//! this pass — see this module's own test coverage for what *is* built: the Must-level gain ramp
//! and metering.
//!
//! Unlike Gate/Nam (FR-CHAIN-050's mono-core-then-duplicate stages), Out processes every channel
//! independently — its own per-channel `GainRamp`/`Meter` state, one shared parameter target — so
//! there is no cross-channel mixing here and no `StageIo::channel` reborrow gotcha to work around.

use namir_dsp::{GainRamp, Meter};
use namir_params::ParamKind;
use namir_params::stages::out::{GAIN_DB, SILENCE_FLOOR_DB};

use crate::param::{ParamChange, ParamId};
use crate::prepare::{PrepareContext, PrepareError};
use crate::stage::{Stage, StagePrep};
use crate::stage_io::StageIo;
use crate::telemetry::{TelemetryEntry, TelemetrySink};

/// `GainRamp`'s one-pole time constant. Same figure, same reasoning as `trim.rs`'s identical
/// constant: `gain_ramp.rs`'s own doc comment derives 20 ms as the exact bound FR-PARAM-040
/// implies for a one-pole, with "very little margin" against `f32` rounding at exactly that
/// figure; 25 ms is that module's documented choice of comfortable margin, reproduced here since
/// its public API imposes no default of its own.
const GAIN_RAMP_TIME_CONSTANT_MS: f32 = 25.0;

/// FR-OUT-010's "settled" margin: once the ramp's current gain is within this many dB of
/// [`SILENCE_FLOOR_DB`], the transition is close enough to done that hard-zeroing the rest of the
/// way is inaudible — the smooth part of the fade (from wherever gain was down to this margin) is
/// still carried entirely by the ramp itself, so only the already-near-silent tail gets clamped to
/// literal zero.
const SILENCE_SETTLE_MARGIN_DB: f32 = 1.0;

/// This stage's RT-facing `namir_engine::ParamId`, converted once from `namir_params`'s own id for
/// the same key (see `trim.rs`'s identical convention and its doc comment for why the two crates
/// carry distinct `ParamId` types on purpose).
const GAIN_DB_ID: ParamId = ParamId(GAIN_DB.id.0);

/// One channel's telemetry signal ids, precomputed in `prepare` (D-6.1: hashing a `format!`-built
/// key allocates, so it happens once off the audio thread, not per `telemetry` call) rather than
/// derived from a single shared `const` the way single-instance stages like `trim.rs` do — Out has
/// more than one channel, and the channel index must be part of the key so per-channel readings
/// don't collide (this stage's own spec).
struct ChannelTelemetryIds {
    peak_db: u32,
    average_db: u32,
    peak_hold_db: u32,
    clipped: u32,
}

impl ChannelTelemetryIds {
    /// Builds the four signal ids for output channel `index`, each derived via
    /// `namir_params::ParamId::from_key` the same way every other stage's telemetry ids are (this
    /// crate's shared convention) — these are readouts, not automatable parameters, so they are
    /// never added to `namir_params::REGISTRY`.
    fn new(index: usize) -> Self {
        Self {
            peak_db: namir_params::ParamId::from_key(&format!("telemetry.out.ch{index}.peak_db")).0,
            average_db: namir_params::ParamId::from_key(&format!(
                "telemetry.out.ch{index}.average_db"
            ))
            .0,
            peak_hold_db: namir_params::ParamId::from_key(&format!(
                "telemetry.out.ch{index}.peak_hold_db"
            ))
            .0,
            clipped: namir_params::ParamId::from_key(&format!("telemetry.out.ch{index}.clipped")).0,
        }
    }
}

/// Builds [`OutStage`]. Holds no configuration of its own — Out's only parameter (`out.gain_db`)
/// seeds its initial value straight from its `namir-params` descriptor (see `prepare`'s body), so
/// there is nothing for a caller to pass in here.
pub struct OutPrep;

impl StagePrep for OutPrep {
    type Prepared = OutStage;

    /// Builds one `GainRamp` and one `Meter` per output channel (D-6.2: sized to
    /// `ctx.channel_config().output_channels()`, never resized in `process`), all ramps seeded to
    /// the same target so the level control reads as one shared value across channels even though
    /// each channel keeps its own smoothing state (this module's own doc comment).
    fn prepare(&self, ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError> {
        let sample_rate = ctx.sample_rate();
        let channel_count = ctx.channel_config().output_channels() as usize;

        // Seed the initial target from the descriptor rather than a second hardcoded copy of its
        // range/default (this crate's own convention, matching `trim.rs`/`gate.rs`). The
        // `unreachable!` arm is only a defensive check against a future edit to `namir-params`
        // changing this descriptor's `kind` out from under this file — it cannot be reached by
        // any input `prepare` is passed.
        let gain_default_db = match GAIN_DB.kind {
            ParamKind::Continuous { default, .. } => default,
            ParamKind::Stepped { .. } => unreachable!("out.gain_db is declared Continuous"),
        };

        let mut ramps = Vec::with_capacity(channel_count);
        let mut meters = Vec::with_capacity(channel_count);
        let mut telemetry_ids = Vec::with_capacity(channel_count);
        for index in 0..channel_count {
            let mut ramp = GainRamp::new(sample_rate, GAIN_RAMP_TIME_CONSTANT_MS);
            ramp.set_target_db(gain_default_db);
            ramps.push(ramp);
            meters.push(Meter::new(sample_rate));
            telemetry_ids.push(ChannelTelemetryIds::new(index));
        }

        Ok(OutStage {
            ramps,
            meters,
            telemetry_ids,
            gain_target_db: gain_default_db,
        })
    }
}

/// RT-safe output stage: per-channel `namir_dsp::GainRamp` (one shared target, independent
/// smoothing state per channel) with FR-OUT-010's exact-silence floor, plus per-channel
/// `namir_dsp::Meter` (FR-OUT-020/FR-IN-030).
pub struct OutStage {
    /// FR-OUT-010's level control, one ramp per output channel. All ramps share the same target
    /// (set together in `apply`) but each keeps its own `current` smoothing state — sharing the
    /// `GainRamp` *instance* across channels would mean one channel's processing order affected
    /// another's smoothed value, which is not what "stereo-linked level" means (this module's own
    /// doc comment).
    ramps: Vec<GainRamp>,
    /// FR-OUT-020/FR-IN-030's peak/average/peak-hold/clip readout, one per channel, measured
    /// post-gain (after the ramp and, when it fires, the exact-silence hard-zero — see
    /// `process`).
    meters: Vec<Meter>,
    /// Precomputed per-channel telemetry signal ids (`ChannelTelemetryIds`'s own doc comment for
    /// why this can't just be a handful of top-level `const`s the way single-instance stages'
    /// telemetry ids are).
    telemetry_ids: Vec<ChannelTelemetryIds>,
    /// The last target set via `apply` (or the descriptor default), in dB, kept alongside each
    /// ramp's own linear `target` so `process` can compare it against [`SILENCE_FLOOR_DB`] without
    /// re-deriving dB from a ramp's linear state every block.
    gain_target_db: f32,
}

impl Stage for OutStage {
    fn process(&mut self, io: &mut StageIo<'_>) {
        // FR-OUT-010: only once the *target* itself is at or below the floor does "exact silence"
        // apply at all — a target above the floor never hard-zeroes, no matter how quiet the
        // ramp's current value happens to be while passing through on its way to some other
        // target.
        let target_at_or_below_floor = self.gain_target_db <= SILENCE_FLOOR_DB;

        for ch in 0..io.channel_count() {
            let buf = io.channel(ch);
            let ramp = &mut self.ramps[ch];
            ramp.process(buf);

            // The smooth part of the transition into the floor is carried entirely by the ramp
            // above; this only clamps the already-near-floor tail to literal zero (FR-OUT-010's
            // "exact silence", not merely `db_to_linear`'s tiny-but-nonzero asymptote).
            if target_at_or_below_floor
                && ramp.current_db() <= SILENCE_FLOOR_DB + SILENCE_SETTLE_MARGIN_DB
            {
                for s in buf.iter_mut() {
                    *s = 0.0;
                }
            }

            // Post-gain (including the hard-zero clamp above, when it fires): reads what actually
            // leaves this stage, same ordering rationale as `trim.rs`'s meter placement.
            self.meters[ch].process(buf);
        }
    }

    fn reset(&mut self) {
        // Every channel's `GainRamp` deliberately keeps its current smoothed value: a reset is a
        // transport stop/reposition, not a parameter change, so the level should not jump back to
        // some default (same reasoning as `trim.rs`'s identical treatment of its own gain ramp).
        for meter in &mut self.meters {
            meter.reset();
        }
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail_samples(&self) -> u32 {
        0
    }

    fn apply(&mut self, change: ParamChange) {
        if change.id == GAIN_DB_ID {
            self.gain_target_db = change.value;
            // Share the *target value* across every channel's ramp, not the ramp instance itself
            // (this module's own doc comment on `ramps`).
            for ramp in &mut self.ramps {
                ramp.set_target_db(change.value);
            }
        }
    }

    fn telemetry(&self, out: &mut TelemetrySink<'_>) {
        for (meter, ids) in self.meters.iter().zip(self.telemetry_ids.iter()) {
            out.push(TelemetryEntry {
                id: ids.peak_db,
                value: meter.peak_db(),
            });
            out.push(TelemetryEntry {
                id: ids.average_db,
                value: meter.average_db(),
            });
            out.push(TelemetryEntry {
                id: ids.peak_hold_db,
                value: meter.peak_hold_db(),
            });
            out.push(TelemetryEntry {
                id: ids.clipped,
                value: if meter.clipped() { 1.0 } else { 0.0 },
            });
        }
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

    fn stage(channel_config: ChannelConfig) -> OutStage {
        OutPrep.prepare(&ctx(channel_config)).unwrap()
    }

    /// Drives every channel's ramp to convergence: 200 blocks of 64 samples at 48 kHz is ~267 ms,
    /// comfortably more than ten 25 ms time constants past any target change (mirrors
    /// `trim.rs`'s identical settle helper).
    fn settle(stage: &mut OutStage, channel_count: usize, value: f32) {
        for _ in 0..200 {
            let mut bufs: Vec<[f32; 64]> = (0..channel_count).map(|_| [value; 64]).collect();
            let mut channels: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
            let mut io = StageIo::new(&mut channels, 64);
            audio_section(|| stage.process(&mut io));
        }
    }

    #[test]
    fn gain_is_applied_once_settled() {
        let mut stage = stage(ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: GAIN_DB_ID,
            value: -6.0,
        });
        settle(&mut stage, 1, 1.0);

        let mut buf = [0.5f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        let expected = 0.5 * db_to_linear(-6.0);
        for s in io.channel(0) {
            assert!((*s - expected).abs() < 1e-3, "got {s}, expected {expected}");
        }
    }

    /// FR-OUT-010: at or below -60 dB the output is *exact* silence, not merely a very quiet
    /// asymptotic approach (`db_to_linear(-60.0)` is a tiny nonzero `f32` on its own).
    ///
    /// Partial rather than plain (D-23.1's second question), corrected at M9a's §14 re-audit.
    /// FR-OUT-010 states **three** literal parameters — the -60 dB to +12 dB range, a 0 dB default,
    /// and exact silence at or below -60 dB — and its `Verify: U` wants all three asserted. Only
    /// the silence clause is, here and in `namir-params`' own
    /// `silence_floor_matches_the_range_minimum`, which pins the range *minimum* to
    /// [`SILENCE_FLOOR_DB`] and says nothing about the maximum or the default. Both of those are
    /// declared in `namir_params::stages::out::GAIN_DB` and read back by nothing: `render_manifest`
    /// emits only key, id, kind tag and smoothing, and the kind tag is the bare word `continuous`
    /// carrying no bounds, so `params.lock` would not move if either literal changed.
    // trace-partial: FR-OUT-010
    // uncovered: FR-OUT-010 — of the requirement's three literal parameters only exact silence at
    // uncovered: or below -60 dB is asserted; the +12 dB maximum and the 0 dB default are declared
    // uncovered: in namir_params::stages::out::GAIN_DB and read back by no test, params.lock
    // uncovered: recording only key, id, kind and smoothing; closes M9b
    #[test]
    fn silence_floor_is_exact_not_asymptotic() {
        let mut at_floor = stage(ChannelConfig::Mono);
        at_floor.apply(ParamChange {
            id: GAIN_DB_ID,
            value: SILENCE_FLOOR_DB,
        });
        settle(&mut at_floor, 1, 1.0);

        let mut buf = [1.0f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| at_floor.process(&mut io));

        for s in io.channel(0) {
            assert_eq!(
                *s, 0.0,
                "expected literal zero at the settled silence floor"
            );
        }

        // A target *below* the descriptor's own minimum (still a valid `ParamChange.value`, since
        // `namir-engine`'s `apply` carries no range-clamping of its own) must floor exactly the
        // same way -- FR-OUT-010's "-60 dB or below".
        let mut stage_below = stage(ChannelConfig::Mono);
        stage_below.apply(ParamChange {
            id: GAIN_DB_ID,
            value: SILENCE_FLOOR_DB - 20.0,
        });
        settle(&mut stage_below, 1, 1.0);
        let mut buf = [1.0f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage_below.process(&mut io));
        for s in io.channel(0) {
            assert_eq!(*s, 0.0, "expected literal zero below the silence floor too");
        }
    }

    /// The fade *into* the floor must still be smooth (FR-PARAM-040-style click-freedom): the
    /// ramp itself carries the whole descent, and the hard-zero clamp only ever fires once the
    /// signal is already within `SILENCE_SETTLE_MARGIN_DB` of the floor, so no single sample
    /// should jump by anywhere near the signal's full range.
    #[test]
    fn transition_into_floor_is_click_free() {
        let sample_rate = 48_000u32;
        let mut stage = stage(ChannelConfig::Mono);
        // Settle at unity first so the descent to the floor is a full-range jump -- the case most
        // likely to expose a premature or oversized hard-zero.
        settle(&mut stage, 1, 1.0);

        stage.apply(ParamChange {
            id: GAIN_DB_ID,
            value: SILENCE_FLOOR_DB,
        });

        let total = 48_000usize; // 1 s: comfortably past settling at a 25 ms time constant.
        let mut out = Vec::with_capacity(total);
        let mut offset = 0usize;
        while offset < total {
            let n = 64usize.min(total - offset);
            let mut buf = [1.0f32; 64];
            let mut channels: [&mut [f32]; 1] = [&mut buf[..n]];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            out.extend_from_slice(io.channel(0));
            offset += n;
        }

        // One-pole first-step bound (`gain_ramp.rs`'s own derivation, reproduced here): the
        // steepest slope of a `time_constant_ms`-tau one-pole is `coeff * range`, i.e.
        // `range / (tau_seconds * sample_rate)`. A generous 5x safety factor comfortably covers
        // the extra (much smaller, per this module's own doc comment) discontinuity the hard-zero
        // clamp itself can introduce at the very end of the descent, while still catching a real
        // bug (e.g. a missing/miscomputed settle margin zeroing while still near unity).
        let range = 1.0 - db_to_linear(SILENCE_FLOOR_DB);
        let tau_seconds = GAIN_RAMP_TIME_CONSTANT_MS / 1000.0;
        let ideal_max_delta = range / (tau_seconds * sample_rate as f32);
        let bound = ideal_max_delta * 5.0;

        let mut prev = 1.0f32;
        let mut max_delta = 0.0f32;
        for &s in &out {
            max_delta = max_delta.max((s - prev).abs());
            prev = s;
        }
        assert!(
            max_delta <= bound,
            "max_delta={max_delta} exceeds the click-free bound {bound}"
        );
        // The transition must actually have happened (otherwise this would pass vacuously).
        assert_eq!(
            out.last().copied(),
            Some(0.0),
            "expected the descent to reach exact silence"
        );
    }

    // trace-partial: FR-OUT-020
    // uncovered: FR-OUT-020 — of the four characteristics this requirement imports from FR-IN-020
    // uncovered: and FR-IN-030, only the clip latch is asserted at the stage: no test reads
    // uncovered: OutStage's published peak_db, average_db or peak_hold_db telemetry entries, so a
    // uncovered: wiring error emitting one under another's id would pass, and the clip indicator
    // uncovered: is reachable by no user reset path; closes M9b
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
            .find(|e| e.id == stage.telemetry_ids[0].clipped)
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
            .find(|e| e.id == stage.telemetry_ids[0].clipped)
            .expect("clipped telemetry entry present");
        assert_eq!(clipped.value, 1.0, "clip indicator must stay latched");
    }

    /// Gain is stereo-linked (one shared target), but metering is per channel: a loud left signal
    /// and a quiet right signal must clip only the channel that actually clipped.
    #[test]
    fn multi_channel_metering_is_independent() {
        let mut stage = stage(ChannelConfig::Stereo);

        let mut left = [1.5f32; 64]; // over full scale.
        let mut right = [0.1f32; 64]; // well under.
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        let mut storage = [TelemetryEntry { id: 0, value: 0.0 }; 16];
        let mut sink = TelemetrySink::new(&mut storage);
        stage.telemetry(&mut sink);
        let left_clipped = sink
            .entries()
            .find(|e| e.id == stage.telemetry_ids[0].clipped)
            .unwrap()
            .value;
        let right_clipped = sink
            .entries()
            .find(|e| e.id == stage.telemetry_ids[1].clipped)
            .unwrap()
            .value;
        assert_eq!(left_clipped, 1.0, "left channel should be latched clipped");
        assert_eq!(right_clipped, 0.0, "right channel should not be clipped");
    }

    /// Covers both branches `process` can take: a normal (non-silent) block, and the hard-zero
    /// branch once the ramp has settled at the floor -- proving the `for s in buf { *s = 0.0 }`
    /// loop itself allocates nothing, per this crate's convention of proving RT-safety via the
    /// harness rather than by inspection alone.
    #[test]
    fn stereo_process_does_not_allocate_including_hard_zero_branch() {
        let mut stage = stage(ChannelConfig::Stereo);

        let mut left = [0.3f32; 64];
        let mut right = [0.4f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        stage.apply(ParamChange {
            id: GAIN_DB_ID,
            value: SILENCE_FLOOR_DB,
        });
        settle(&mut stage, 2, 1.0);

        let mut left = [1.0f32; 64];
        let mut right = [1.0f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));
        for s in io.channel(0) {
            assert_eq!(*s, 0.0);
        }
        for s in io.channel(1) {
            assert_eq!(*s, 0.0);
        }
    }
}
