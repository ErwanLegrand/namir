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

use namir_core::SampleRate;
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
            ramps.push(gain_ramp_at_default(sample_rate, gain_default_db));
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

/// Issue #127's follow-up: the one place this stage's per-channel gain ramp is constructed, so `prepare` and the
/// test that pins its start point cannot drift apart. `GainRamp::new_at_db` rather than
/// `GainRamp::new` followed by `set_target_db`: the latter leaves `current` at unity and `target`
/// at the default, so the first ~25 ms of audio after every prepare, sample-rate change or
/// re-prepare ramps from 0 dB to the parameter's real default. That is inaudible only because
/// `out.gain_db` happens to default to 0.0 dB today — see `GainRamp::new_at_db`'s own doc comment.
fn gain_ramp_at_default(sample_rate: SampleRate, default_db: f32) -> GainRamp {
    GainRamp::new_at_db(sample_rate, GAIN_RAMP_TIME_CONSTANT_MS, default_db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;
    use namir_core::{ChannelConfig, SampleRate, db_to_linear, linear_to_db};

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
        let stage = stage(ChannelConfig::Stereo);
        let default_db = match GAIN_DB.kind {
            ParamKind::Continuous { default, .. } => default,
            ParamKind::Stepped { .. } => unreachable!("out.gain_db is declared Continuous"),
        };
        for (index, ramp) in stage.ramps.iter().enumerate() {
            assert!(
                (ramp.current_db() - default_db).abs() < 1e-4,
                "channel {index}'s ramp sits at {} dB, not its {default_db} dB default",
                ramp.current_db()
            );
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

    /// **FR-OUT-010's other two literal parameters**, which no test read back until M14: the
    /// requirement states **three** — a −60 dB to +12 dB range, a 0 dB default, and exact silence
    /// at or below −60 dB — and only the third was asserted, here and in `namir-params`' own
    /// `silence_floor_matches_the_range_minimum`, which pins the range *minimum* to
    /// [`SILENCE_FLOOR_DB`] and says nothing about the maximum or the default. `params.lock` cannot
    /// stand in: `render_manifest` emits key, id, kind tag and smoothing, and the kind tag is the
    /// bare word `continuous` carrying no bounds, so it would not move if either literal changed.
    ///
    /// Both halves: the descriptor declares them, **and** the stage realises them — a stage built
    /// from the descriptor's default passes its input at unity without anyone setting anything, and
    /// one driven to the declared maximum applies exactly +12 dB.
    // trace: FR-OUT-010
    #[test]
    fn the_declared_range_and_default_are_the_ones_the_requirement_states() {
        let ParamKind::Continuous { min, max, default } = GAIN_DB.kind else {
            panic!("out.gain_db must be Continuous");
        };
        assert_eq!(min, -60.0, "out.gain_db: minimum");
        assert_eq!(max, 12.0, "out.gain_db: maximum");
        assert_eq!(default, 0.0, "out.gain_db: default");

        // The default, realised: an untouched stage is unity, not merely declared to be.
        let mut at_default = stage(ChannelConfig::Mono);
        settle(&mut at_default, 1, 0.5);
        let mut buf = [0.5f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| at_default.process(&mut io));
        for s in io.channel(0) {
            assert!(
                (*s - 0.5).abs() < 1e-6,
                "an untouched Out stage should be unity gain, got {s} for an input of 0.5"
            );
        }

        // The maximum, realised.
        let mut at_max = stage(ChannelConfig::Mono);
        at_max.apply(ParamChange {
            id: GAIN_DB_ID,
            value: max,
        });
        settle(&mut at_max, 1, 0.1);
        let mut buf = [0.1f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| at_max.process(&mut io));
        let expected = 0.1 * db_to_linear(max);
        for s in io.channel(0) {
            assert!(
                (*s - expected).abs() < 1e-4,
                "at the declared +{max} dB maximum, got {s}, expected {expected}"
            );
        }
    }

    /// FR-OUT-010: at or below -60 dB the output is *exact* silence, not merely a very quiet
    /// asymptotic approach (`db_to_linear(-60.0)` is a tiny nonzero `f32` on its own).
    // trace: FR-OUT-010
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

    /// Runs a sine of `amplitude` at `freq_hz` for `frames` samples through a mono stage in
    /// 64-sample blocks. Returns nothing: what these FR-OUT-020 tests read is the telemetry the
    /// stage publishes afterwards, not the audio.
    fn drive_sine(stage: &mut OutStage, frames: usize, freq_hz: f64, amplitude: f32) {
        let mut offset = 0usize;
        while offset < frames {
            let n = 64usize.min(frames - offset);
            let mut buf: Vec<f32> = (offset..offset + n)
                .map(|i| {
                    (f64::from(amplitude)
                        * (std::f64::consts::TAU * freq_hz * i as f64 / 48_000.0).sin())
                        as f32
                })
                .collect();
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            offset += n;
        }
    }

    /// This stage's published value for one telemetry id, in dB.
    fn telemetry_value(stage: &OutStage, id: u32) -> f32 {
        let mut storage = [TelemetryEntry { id: 0, value: 0.0 }; 16];
        let mut sink = TelemetrySink::new(&mut storage);
        stage.telemetry(&mut sink);
        sink.entries()
            .find(|e| e.id == id)
            .expect("telemetry entry present")
            .value
    }

    /// **FR-OUT-020's three unread readings.** The requirement imports FR-IN-020's characteristics
    /// — "peak and a short-term average, with a peak-hold indicator that latches for at least
    /// 1 second" — and until M14 no test read `OutStage`'s published `peak_db`, `average_db` or
    /// `peak_hold_db` at all, only the clip flag. A wiring error emitting any one of them under
    /// another's id would have passed every test in this file.
    ///
    /// The probe is chosen so **all three readings are different numbers**, which is what makes a
    /// swap detectable: a 0.8 burst latches the peak-hold, then a long 0.2 tone lets the
    /// fast-attack/slow-release peak fall to 0.2 while the short-term average settles on that
    /// tone's RMS, 0.2/√2. −1.94, −13.98 and −16.99 dB: no two within 3 dB of each other.
    ///
    /// The 1.5 s tail also executes FR-IN-020's "latches for at least 1 second" directly — the hold
    /// is still reading the burst a second and a half after the burst ended.
    // trace-partial: FR-OUT-020
    // uncovered: FR-OUT-020 — the engine-side half is closed here: peak, short-term average,
    // uncovered: peak-hold and the clip latch are each read back from OutStage's own telemetry
    // uncovered: under signals that make the three readings distinct, and measured post-gain. What
    // uncovered: is unspanned is FR-IN-020's "M for the display", which this requirement imports
    // uncovered: with the rest: namir_ui::MeterReading carries only peak_db and rms_db, so the
    // uncovered: peak-hold value this stage publishes reaches no UI field, no
    // uncovered: docs/manual-tests/fr-out-020-*.md exists, and the clip indicator is still
    // uncovered: reachable by no user reset path. That is roadmap section 21 Phase 1's unbuilt
    // uncovered: surface, deliberately deferred by this milestone; closes M8
    #[test]
    fn peak_average_and_peak_hold_each_report_their_own_quantity() {
        let mut stage = stage(ChannelConfig::Mono);

        // 0.25 s at 0.8: the peak follower jumps to 0.8 on the first crest and the hold latches it.
        drive_sine(&mut stage, 12_000, 1_000.0, 0.8);
        // 1.5 s at 0.2: five release time constants, so the peak has fallen onto the new tone while
        // the hold has not moved.
        drive_sine(&mut stage, 72_000, 1_000.0, 0.2);

        let ids = &stage.telemetry_ids[0];
        let peak = telemetry_value(&stage, ids.peak_db);
        let average = telemetry_value(&stage, ids.average_db);
        let peak_hold = telemetry_value(&stage, ids.peak_hold_db);

        let expected_hold = linear_to_db(0.8);
        let expected_peak = linear_to_db(0.2);
        let expected_average = linear_to_db(0.2 / std::f32::consts::SQRT_2);

        assert!(
            (peak_hold - expected_hold).abs() < 0.1,
            "peak-hold reads {peak_hold} dB, expected the 0.8 burst's {expected_hold} dB"
        );
        assert!(
            (peak - expected_peak).abs() < 0.3,
            "peak reads {peak} dB, expected the current 0.2 tone's {expected_peak} dB"
        );
        assert!(
            (average - expected_average).abs() < 0.3,
            "average reads {average} dB, expected the 0.2 tone's RMS {expected_average} dB"
        );

        // No two readings are close enough for a swapped id to have passed the three assertions
        // above by accident.
        assert!(
            peak_hold > peak + 3.0 && peak > average + 2.0,
            "the three readings are not well separated ({peak_hold} / {peak} / {average} dB), so \
             this probe cannot tell them apart"
        );

        // FR-IN-020's "latches for at least 1 second": the burst ended 1.5 s ago.
        assert!(
            peak_hold > peak + 3.0,
            "the peak-hold decayed within 1.5 s of the burst that set it"
        );
    }

    /// **FR-OUT-020's readings are of what leaves the stage.** An output meter that read its input
    /// would be reporting a level the user never hears; this stage's own doc comment says the
    /// meters run post-gain, and nothing checked it. The same tone through a stage settled at
    /// −6 dB must read 6 dB lower on all three.
    #[test]
    fn the_meter_reads_the_post_gain_signal() {
        let mut unity = stage(ChannelConfig::Mono);
        drive_sine(&mut unity, 48_000, 1_000.0, 0.5);

        let mut attenuated = stage(ChannelConfig::Mono);
        attenuated.apply(ParamChange {
            id: GAIN_DB_ID,
            value: -6.0,
        });
        // Settle the ramp, then clear the meter so only the settled part is measured (`reset` is
        // meters-only here — see this stage's own `reset`).
        drive_sine(&mut attenuated, 48_000, 1_000.0, 0.5);
        attenuated.reset();
        drive_sine(&mut attenuated, 48_000, 1_000.0, 0.5);

        let unity_ids = &unity.telemetry_ids[0];
        let attenuated_ids = &attenuated.telemetry_ids[0];
        for (what, id_of) in [("peak", 0usize), ("average", 1), ("peak-hold", 2)] {
            let pick = |ids: &ChannelTelemetryIds| match id_of {
                0 => ids.peak_db,
                1 => ids.average_db,
                _ => ids.peak_hold_db,
            };
            let loud = telemetry_value(&unity, pick(unity_ids));
            let quiet = telemetry_value(&attenuated, pick(attenuated_ids));
            assert!(
                (loud - quiet - 6.0).abs() < 0.2,
                "{what} reads {loud} dB at unity and {quiet} dB at -6 dB: a difference of {}, not \
                 the 6 dB the gain applies -- is the meter reading the stage's input?",
                loud - quiet
            );
        }
    }

    /// FR-OUT-020's clip half, which imports FR-IN-030. The requirement's tag sits on
    /// [`peak_average_and_peak_hold_each_report_their_own_quantity`] above, which names the one
    /// limb still unspanned; this test is the other half of the same evidence.
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
