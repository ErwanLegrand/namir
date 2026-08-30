//! M14 Phase 4's chain-level probes: the requirements whose annotated tests ran against an empty
//! chain (FR-CHAIN-010/-020/-050/-060/-080 and NFR-PERF-020), re-verified against a real
//! `build_default_chain` with real resources loaded and a real signal through it.
//!
//! # Why these live here rather than beside the stages
//!
//! Every one of them is a statement about the *assembly*, not about a stage: an ordering, a
//! duplication invariant, one stage's bypass leaving the others alone, a fault containment only
//! the chain performs, a latency figure only the chain reports. `stages/mod.rs`'s own tests are
//! the assembly's smoke tests (it builds, it runs, it does not allocate); this module is where the
//! assembly is actually measured. The per-stage halves of FR-CHAIN-020's and FR-CHAIN-050's
//! `Verify:` methods stay in the stage modules, which is why those two keep a `trace-partial:`
//! here naming what this file does not do.
//!
//! Everything shared with the stage-level probes — the signals, the runner, the fixture loaders,
//! the estimators — lives in [`crate::probe`] and is not duplicated here.

use std::sync::Arc;

use namir_core::{ChannelConfig, db_to_linear};
use namir_fixtures::nam::WaveNetShape;
use namir_params::stages::{eq, gate, ir, nam, out, trim};

use crate::chain::Chain;
use crate::param::ParamChange;
use crate::prepare::PrepareContext;
use crate::probe::{self, BLOCK, SR};
use crate::rt_harness::audio_section;
use crate::stage::{Stage, StagePrep};
use crate::stage_io::StageIo;
use crate::stages::{self, build_default_chain};
use crate::telemetry::TelemetrySink;

// ---------------------------------------------------------------------------------------------
// Chain assembly under test, and the orderings a probe is measured against.
// ---------------------------------------------------------------------------------------------

/// One position in the fixed 1.0 chain, so a probe can assemble the six product stages in an order
/// of its choosing and measure what changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    Gate,
    Trim,
    Nam,
    Ir,
    Eq,
    Out,
}

/// **FR-CHAIN-010's order, as amended by its own `*Consequence (added M9a, 2026-08-09)*` note**:
/// `input → noise gate → input trim → NAM → IR → EQ → output level → output`. Transcribed from the
/// requirement, not from `build_default_chain` — the point of
/// [`fr_chain_010_a_probe_signal_pins_every_position_in_the_shipped_chain`] is that the two are
/// compared rather than assumed equal.
const FRS_ORDER: [Slot; 6] = [
    Slot::Gate,
    Slot::Trim,
    Slot::Nam,
    Slot::Ir,
    Slot::Eq,
    Slot::Out,
];

/// Prepares the stages `order` names, in that order.
fn build_stage_list(order: &[Slot], ctx: &PrepareContext) -> Vec<Box<dyn Stage>> {
    order
        .iter()
        .map(|slot| match slot {
            Slot::Gate => Box::new(stages::gate::GatePrep.prepare(ctx).unwrap()) as Box<dyn Stage>,
            Slot::Trim => Box::new(stages::trim::TrimPrep.prepare(ctx).unwrap()) as Box<dyn Stage>,
            Slot::Nam => Box::new(stages::nam::NamPrep.prepare(ctx).unwrap()) as Box<dyn Stage>,
            Slot::Ir => Box::new(stages::ir::IrPrep.prepare(ctx).unwrap()) as Box<dyn Stage>,
            Slot::Eq => Box::new(stages::eq::EqPrep.prepare(ctx).unwrap()) as Box<dyn Stage>,
            Slot::Out => Box::new(stages::out::OutPrep.prepare(ctx).unwrap()) as Box<dyn Stage>,
        })
        .collect()
}

/// Assembles the six product stages in `order` with the same cross-cutting preparation
/// `build_default_chain` performs — so the only difference between this and the product path is
/// the order itself.
fn build_in_order(order: &[Slot], ctx: &PrepareContext) -> Chain {
    let mut chain = Chain::new(build_stage_list(order, ctx));
    chain.prepare_crosscutting(ctx);
    chain
}

// ---------------------------------------------------------------------------------------------
// FR-CHAIN-010 — "measure stage interaction against a specified probe signal".
// ---------------------------------------------------------------------------------------------

/// **FR-CHAIN-010's own `Verify: I` method, executed.** Two things are measured, because the
/// method needs both to mean anything.
///
/// **(1) The shipped chain is the chain the requirement names.** `build_default_chain`'s output is
/// compared, sample for sample, against a hand-assembly of the same six prepared stages in
/// [`FRS_ORDER`] — the amended requirement's order, transcribed from the document. Bit-exact, not
/// approximate: both run identical arithmetic in identical sequence, so any inequality at all
/// means the product's order moved.
///
/// **(2) That comparison has teeth at every position.** A comparison against a reference is only
/// as good as the probe's sensitivity to the thing compared, and the old FR-CHAIN-010 test's
/// failure was exactly this: zeros in, zeros out, which cannot distinguish any ordering from an
/// empty chain. So every adjacent transposition of [`FRS_ORDER`] is run through a probe chosen to
/// make *that* interaction observable, and the difference is measured:
///
/// - **Gate↔Trim** — the pair D-9.8 is about. Probe: a −10 dBFS chirp with the gate threshold at
///   −30 dBFS and the trim at −40 dB. Shipped (gate first) the gate sees −10 dBFS and stays open;
///   swapped, it sees −50 dBFS and shuts. The difference is the whole signal.
/// - **Trim↔Nam** and **Nam↔Ir** — the NAM stage is the chain's only nonlinearity, so it commutes
///   with neither a gain nor a convolution. Probe: the same chirp driven hot (+12 dB trim) so that
///   nonlinearity is actually engaged, with the output level pulled down 30 dB to keep the run
///   clear of FR-CHAIN-090's ceiling — a clamped output would hide the very differences being
///   measured.
/// - **Ir↔Eq** and **Eq↔Out** — measured, and asserted to be **indistinguishable**, which is the
///   honest result rather than a gap: convolution, biquads and a gain are all LTI, so those two
///   transpositions describe the same system and no probe signal can separate them. What pins
///   their order is assertion (1) alone, whose bit-exactness does resolve them.
// trace: FR-CHAIN-010
#[test]
fn fr_chain_010_a_probe_signal_pins_every_position_in_the_shipped_chain() {
    /// Discarded before measuring, so every gain ramp, both handover crossfades and each bypass
    /// mix have settled and what is measured is the assembled chain rather than its start-up.
    const WARMUP: usize = 8_000;
    const FRAMES: usize = 24_000;

    let ctx = probe::ctx(ChannelConfig::Mono);
    let model = probe::nam_model(WaveNetShape::Nano, 7, SR);
    let cabinet = probe::mono_ir(3, 512, SR, BLOCK);
    let signal = probe::chirp(FRAMES, 100.0, 8_000.0, SR, 0.3);
    let input = probe::duplicated(&signal, 1);

    // Two probe configurations, because no single one makes all five transpositions observable:
    // the gate/trim interaction needs the signal to straddle the gate threshold, and the two NAM
    // interactions need the model driven into its nonlinearity.
    let gate_probe = |chain: &mut Chain| {
        probe::set_param(chain, gate::THRESHOLD_DB.id, -30.0);
        probe::set_param(chain, trim::GAIN_DB.id, -40.0);
    };
    let hot_probe = |chain: &mut Chain| {
        probe::set_param(chain, gate::THRESHOLD_DB.id, -80.0);
        probe::set_param(chain, trim::GAIN_DB.id, 12.0);
        probe::set_param(chain, eq::LOW_SHELF_GAIN_DB.id, 6.0);
        probe::set_param(chain, eq::MID_GAIN_DB.id, -6.0);
        probe::set_param(chain, eq::HIGH_SHELF_GAIN_DB.id, 4.0);
        probe::set_param(chain, out::GAIN_DB.id, -30.0);
    };

    let run = |order: Option<&[Slot]>, configure: &dyn Fn(&mut Chain)| -> Vec<f32> {
        let mut chain = match order {
            Some(order) => build_in_order(order, &ctx),
            None => build_default_chain(&ctx).unwrap(),
        };
        configure(&mut chain);
        probe::load_nam(&mut chain, model.clone(), &ctx);
        probe::load_ir(&mut chain, cabinet.clone(), &ctx);
        probe::run(&mut chain, &input, BLOCK)[0][WARMUP..].to_vec()
    };

    let gated_reference = run(Some(&FRS_ORDER), &gate_probe);
    let hot_reference = run(Some(&FRS_ORDER), &hot_probe);

    // (1) The product path and the requirement's order are the same chain, under both probes.
    for (configure, mandated) in [
        (&gate_probe as &dyn Fn(&mut Chain), &gated_reference),
        (&hot_probe, &hot_reference),
    ] {
        let shipped = run(None, configure);
        assert_eq!(
            probe::max_abs_difference(&shipped, mandated),
            0.0,
            "build_default_chain no longer assembles FR-CHAIN-010's amended order \
             (gate -> trim -> nam -> ir -> eq -> out)"
        );
    }

    // (2a) The three adjacent transpositions a probe signal can see.
    for (position, configure, reference) in [
        (0usize, &gate_probe as &dyn Fn(&mut Chain), &gated_reference), // Gate <-> Trim
        (1, &hot_probe, &hot_reference),                                // Trim <-> Nam
        (2, &hot_probe, &hot_reference),                                // Nam  <-> Ir
    ] {
        let mut swapped_order = FRS_ORDER;
        swapped_order.swap(position, position + 1);
        let swapped = run(Some(&swapped_order), configure);

        let difference = probe::max_abs_difference(reference, &swapped);
        let scale = probe::peak(reference);
        assert!(
            scale > 1e-3,
            "the probe produced no signal to measure against ({:?}/{:?})",
            FRS_ORDER[position],
            FRS_ORDER[position + 1]
        );
        assert!(
            difference > 0.1 * scale,
            "swapping {:?} and {:?} moved the output by only {difference} against a probe peak \
             of {scale}: this probe cannot see that interaction, so assertion (1) is unguarded \
             at that position",
            FRS_ORDER[position],
            FRS_ORDER[position + 1],
        );
    }

    // (2b) Ir <-> Eq and Eq <-> Out: LTI, therefore commuting. Asserted as a *bound*, not a
    // difference, so that a future change making one of them nonlinear (FR-OUT-030's optional
    // brickwall limiter is the obvious candidate — a Should this pass does not build) fails here
    // and gets a real probe of its own, rather than silently leaving assertion (1) as the only
    // guard at those two positions.
    for position in [3usize, 4] {
        let mut swapped_order = FRS_ORDER;
        swapped_order.swap(position, position + 1);
        let swapped = run(Some(&swapped_order), &hot_probe);

        let difference = probe::max_abs_difference(&hot_reference, &swapped);
        let scale = probe::peak(&hot_reference);
        assert!(
            difference < 1e-3 * scale,
            "swapping {:?} and {:?} moved the output by {difference} against a probe peak of \
             {scale} -- these two stages are supposed to be LTI and therefore commuting; if one \
             of them is not any more, this transposition needs a probe of its own",
            FRS_ORDER[position],
            FRS_ORDER[position + 1],
        );
    }
}

// ---------------------------------------------------------------------------------------------
// FR-CHAIN-020 — one stage's bypass, toggled inside an assembled chain.
// ---------------------------------------------------------------------------------------------

/// **FR-CHAIN-020's "without disturbing the other stages", at chain level.** For each of the four
/// stages the requirement names, three runs of the same probe through the same assembled chain:
/// one with everything on, one with the target stage bypassed from the start, and one where the
/// target's bypass is flipped mid-signal.
///
/// The three together say what "without disturbing the other stages" means operationally:
///
/// - before the flip, the toggled run is **bit-identical** to the all-on run — nothing about being
///   able to bypass a stage changes anything until it is asked for;
/// - once settled, the toggled run has **converged onto** the bypassed-from-the-start run, so what
///   the flip removed is exactly that stage's contribution and every other stage is left doing
///   what it does in a chain that never had it on;
/// - the two ends are **materially different** from each other, so neither of the above passes
///   vacuously;
/// - and the flip is click-free, measured as a first-difference against the untoggled run's own
///   steady-state roughness (FR-CHAIN-020's `I for click-freedom` limb, the self-calibrating shape
///   `engine.rs`'s handover tests use).
// trace: FR-CHAIN-020
#[test]
fn fr_chain_020_toggling_one_stages_bypass_mid_signal_leaves_the_others_undisturbed() {
    const TOGGLE_BLOCK: usize = 300; // ~400 ms in, long after every ramp has settled.
    const TOTAL_BLOCKS: usize = 900; // ~1.2 s, leaving ~0.8 s to converge.
    const FRAMES: usize = TOTAL_BLOCKS * BLOCK;
    /// The window the convergence and effect assertions are measured over: the last ~0.25 s.
    const TAIL: usize = FRAMES - 12_000;

    let ctx = probe::ctx(ChannelConfig::Mono);
    let model = probe::nam_model(WaveNetShape::Nano, 11, SR);
    let cabinet = probe::mono_ir(5, 512, SR, BLOCK);
    // A steady tone rather than a sweep: the convergence assertion compares two runs whose filter
    // states differ, and a stationary excitation is what makes "they have converged" measurable
    // at a fixed tolerance.
    let input = probe::duplicated(&probe::sine(FRAMES, 220.0, SR, 0.3), 1);

    for (stage_name, enabled, gate_threshold_db) in [
        // The gate's own row needs a threshold *above* the probe, or bypassing the gate would
        // change nothing and the "did it actually do anything" assertion below would be the one
        // that fails: an open gate and a bypassed gate are the same passthrough. Every other row
        // wants the opposite — a threshold well below the probe, so the gate is open on merit and
        // stays out of the measurement.
        ("gate", gate::ENABLED.id, -6.0),
        ("nam", nam::ENABLED.id, -60.0),
        ("ir", ir::ENABLED.id, -60.0),
        ("eq", eq::ENABLED.id, -60.0),
    ] {
        let run = |toggle_at: Option<usize>, initially_on: bool| -> Vec<f32> {
            let mut chain = build_default_chain(&ctx).unwrap();
            probe::set_param(&mut chain, gate::THRESHOLD_DB.id, gate_threshold_db);
            probe::set_param(&mut chain, eq::LOW_SHELF_GAIN_DB.id, 5.0);
            probe::set_param(&mut chain, eq::MID_GAIN_DB.id, -9.0);
            probe::set_param(&mut chain, out::GAIN_DB.id, -12.0);
            if !initially_on {
                probe::set_param(&mut chain, enabled, 0.0);
            }
            probe::load_nam(&mut chain, model.clone(), &ctx);
            probe::load_ir(&mut chain, cabinet.clone(), &ctx);
            probe::run_with(&mut chain, &input, BLOCK, |block, chain| {
                if Some(block) == toggle_at {
                    probe::set_param(chain, enabled, 0.0);
                }
            })
            .remove(0)
        };

        let all_on = run(None, true);
        let off_from_the_start = run(None, false);
        let toggled = run(Some(TOGGLE_BLOCK), true);

        let before = ..TOGGLE_BLOCK * BLOCK;
        assert_eq!(
            probe::max_abs_difference(&toggled[before], &all_on[before]),
            0.0,
            "{stage_name}: the output changed before its bypass was ever toggled"
        );

        // The louder of the two ends, since for the gate's own row the all-on end is the quiet one.
        let scale = probe::peak(&all_on[TAIL..]).max(probe::peak(&off_from_the_start[TAIL..]));
        assert!(scale > 1e-3, "{stage_name}: the probe produced no signal");

        let residue = probe::max_abs_difference(&toggled[TAIL..], &off_from_the_start[TAIL..]);
        assert!(
            residue < 0.02 * scale,
            "{stage_name}: a chain whose bypass was flipped mid-signal has not converged onto one \
             bypassed from the start (residue {residue} against a peak of {scale}) -- something \
             other than the {stage_name} stage was disturbed by the toggle"
        );

        let effect = probe::max_abs_difference(&all_on[TAIL..], &off_from_the_start[TAIL..]);
        assert!(
            effect > 0.05 * scale,
            "{stage_name}: bypassing it changes the output by only {effect} against a peak of \
             {scale}, so the convergence assertion above is vacuous"
        );

        // Click-freedom across the flip, self-calibrated against the untoggled runs' own
        // steady-state roughness over the same window.
        let window = TOGGLE_BLOCK * BLOCK..TOGGLE_BLOCK * BLOCK + 8_000;
        let baseline = probe::max_abs_first_difference(&all_on[window.clone()]).max(
            probe::max_abs_first_difference(&off_from_the_start[window.clone()]),
        );
        let jump = probe::max_abs_first_difference(&toggled[window]);
        assert!(
            jump <= 3.0 * baseline,
            "{stage_name}: bypassing it mid-signal produced a step of {jump} against a no-toggle \
             baseline of {baseline} (allowed 3x)"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// FR-CHAIN-050 / FR-CHAIN-060 — the mono core, and the three channel configurations.
// ---------------------------------------------------------------------------------------------

/// Trim's downmix applies −6 dB to **both** terms rather than averaging (`stages/trim.rs`'s own
/// note), so a multi-channel configuration whose channels already carry the identical signal —
/// which is every configuration by the time Trim runs, since Gate is upstream of it and mono-core
/// — feeds the core `2 · db_to_linear(−6)` ≈ 1.0024 times that signal, where `Mono` feeds it
/// unscaled. A probe comparing a widened configuration against a mono one has to account for that
/// +0.02 dB or it is measuring the downmix rather than the core.
fn downmix_scale() -> f32 {
    2.0 * db_to_linear(-6.0)
}

/// **FR-CHAIN-050's `Verify: I`, against a loaded chain.** "The engine core shall process a single
/// channel. Channel configurations shall be realised by the placement of the mono core within the
/// surrounding routing."
///
/// Both halves, for `MonoToStereo` and `Stereo`, with a NAM model and a **mono** IR loaded (a
/// stereo IR is the one thing in the chain that legitimately makes channels differ — that is
/// FR-CHAIN-060's row, tested separately below):
///
/// 1. **One channel of work.** Every output channel is sample-identical, so what left the chain is
///    one core result duplicated, not two independent ones.
/// 2. **Placement, not a different core.** That shared result matches a `Mono` chain's output on
///    the same core input to within a tight tolerance, so the surrounding routing is the only
///    thing the configuration changed.
/// 3. **Non-vacuous**, twice over: the output is not silence, and a `Stereo` run whose right
///    channel carries a tone the left does not is *not* the run that tone alone would produce —
///    which is what makes assertion 2 a statement about the core's input rather than an accident.
// trace: FR-CHAIN-050
#[test]
fn fr_chain_050_every_configuration_duplicates_one_mono_core_result() {
    const FRAMES: usize = 24_000;
    const WARMUP: usize = 8_000;

    let model = probe::nam_model(WaveNetShape::Nano, 13, SR);
    let cabinet = probe::mono_ir(7, 512, SR, BLOCK);

    let left = probe::sine(FRAMES, 220.0, SR, 0.3);
    // A tone the left channel does not contain, so its presence or absence in the output is
    // decisive about what the mono core was fed.
    let right = probe::sine(FRAMES, 3_100.0, SR, 0.3);

    let run = |channel_config: ChannelConfig, input: Vec<Vec<f32>>| -> Vec<Vec<f32>> {
        let ctx = probe::ctx(channel_config);
        let mut chain = build_default_chain(&ctx).unwrap();
        probe::set_param(&mut chain, gate::THRESHOLD_DB.id, -60.0);
        probe::set_param(&mut chain, out::GAIN_DB.id, -12.0);
        probe::load_nam(&mut chain, model.clone(), &ctx);
        probe::load_ir(&mut chain, cabinet.clone(), &ctx);
        probe::run(&mut chain, &input, BLOCK)
    };

    // The reference: one channel in, one channel out, fed exactly what Trim's downmix hands the
    // core in a two-channel configuration.
    let scaled: Vec<f32> = left.iter().map(|s| s * downmix_scale()).collect();
    let mono = run(ChannelConfig::Mono, vec![scaled]);
    assert_eq!(mono.len(), 1);
    let scale = probe::peak(&mono[0][WARMUP..]);
    assert!(scale > 1e-3, "the probe produced no signal");

    for (channel_config, input) in [
        // A mono source arrives already duplicated across the chain's channels (`stage_io.rs`:
        // the channel count is fixed to `output_channels()` for the whole chain).
        (ChannelConfig::MonoToStereo, probe::duplicated(&left, 2)),
        (ChannelConfig::Stereo, vec![left.clone(), right.clone()]),
    ] {
        let widened = run(channel_config, input);
        assert_eq!(
            widened.len(),
            2,
            "{channel_config:?} must produce 2 channels"
        );

        // (1) One core result, duplicated.
        assert_eq!(
            probe::max_abs_difference(&widened[0], &widened[1]),
            0.0,
            "{channel_config:?}: the two output channels are not the same signal, so the core did \
             not process a single channel"
        );

        // (2) The same core, differently placed.
        let residue = probe::max_abs_difference(&widened[0][WARMUP..], &mono[0][WARMUP..]);
        assert!(
            residue < 0.01 * scale,
            "{channel_config:?}: the widened configuration's core result differs from the Mono \
             configuration's by {residue} against a peak of {scale}"
        );
    }

    // (3) The right channel's 3.1 kHz tone never reaches the core in `Stereo`: Gate is upstream of
    // Trim (D-9.8) and copies channel 0 over channel 1 before Trim can sum them, so the shipped
    // Stereo routing is FR-CHAIN-060's "L-only" input — which is what assertion (2) just measured.
    // Stated here as the discriminator rather than assumed: a routing that carried the right
    // channel into the core at all would put that tone in the output, and (2) would have failed.
    let stereo = run(ChannelConfig::Stereo, vec![left, right.clone()]);
    let right_only = run(
        ChannelConfig::Mono,
        vec![right.iter().map(|s| s * downmix_scale()).collect()],
    );
    assert!(
        probe::max_abs_difference(&stereo[0][WARMUP..], &right_only[0][WARMUP..]) > 0.1 * scale,
        "the Stereo configuration's output is indistinguishable from one fed the right channel \
         alone, so this probe cannot tell which channel reached the core"
    );
}

/// **FR-CHAIN-060's `Verify: I per configuration`, with an IR actually loaded into every row.**
///
/// The table's three rows differ in what reaches the IR stage and what leaves it, and the
/// `Mono→stereo` row's IR cell — "stereo IR, or dual mono IR" — was exercised by nothing: no test
/// had ever loaded any IR into an `IrStage` prepared with `ChannelConfig::MonoToStereo`. Both of
/// that cell's alternatives run here, and they are distinguished from each other rather than
/// merely both surviving:
///
/// - **dual mono IR** (a one-channel IR widened by `ir.rs`'s `wet_channel_index`): the two outputs
///   carry the same signal;
/// - **stereo IR**: the two outputs genuinely differ, because a stereo IR's two channels reach two
///   distinct physical outputs — the one thing in the chain allowed to break FR-CHAIN-050's
///   identical-channel invariant, and the reason that cell is worded as it is.
// trace: FR-CHAIN-060
#[test]
fn fr_chain_060_every_configuration_runs_with_a_real_ir_in_its_ir_stage() {
    const FRAMES: usize = 16_000;
    const WARMUP: usize = 4_000;

    let model = probe::nam_model(WaveNetShape::Nano, 17, SR);
    let signal = probe::chirp(FRAMES, 120.0, 6_000.0, SR, 0.3);

    let run = |channel_config: ChannelConfig, stereo_cabinet: bool| -> Vec<Vec<f32>> {
        let ctx = probe::ctx(channel_config);
        let cabinet = if stereo_cabinet {
            probe::stereo_ir(23, 512, SR, BLOCK)
        } else {
            probe::mono_ir(29, 512, SR, BLOCK)
        };
        let mut chain = build_default_chain(&ctx).unwrap();
        probe::set_param(&mut chain, gate::THRESHOLD_DB.id, -60.0);
        probe::set_param(&mut chain, out::GAIN_DB.id, -12.0);
        probe::load_nam(&mut chain, model.clone(), &ctx);
        probe::load_ir(&mut chain, cabinet, &ctx);
        let input = probe::duplicated(&signal, channel_config.output_channels() as usize);
        probe::run(&mut chain, &input, BLOCK)
    };

    for channel_config in [
        ChannelConfig::Mono,
        ChannelConfig::MonoToStereo,
        ChannelConfig::Stereo,
    ] {
        let channels = channel_config.output_channels() as usize;

        for stereo_cabinet in [false, true] {
            let out = run(channel_config, stereo_cabinet);
            assert_eq!(
                out.len(),
                channels,
                "{channel_config:?}: wrong channel count"
            );
            for (index, channel) in out.iter().enumerate() {
                assert!(
                    channel.iter().all(|s| s.is_finite()),
                    "{channel_config:?}: channel {index} produced a non-finite sample"
                );
                assert!(
                    probe::peak(&channel[WARMUP..]) > 1e-3,
                    "{channel_config:?}: channel {index} produced no signal with an IR loaded"
                );
            }

            if channels < 2 {
                continue;
            }
            let spread = probe::max_abs_difference(&out[0][WARMUP..], &out[1][WARMUP..]);
            let scale = probe::peak(&out[0][WARMUP..]);
            if stereo_cabinet {
                assert!(
                    spread > 0.05 * scale,
                    "{channel_config:?}: a stereo IR produced two identical output channels \
                     (spread {spread} against a peak of {scale}) -- the IR's second channel never \
                     reached the second output"
                );
            } else {
                assert_eq!(
                    spread, 0.0,
                    "{channel_config:?}: a dual-mono IR must widen to two identical channels"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// FR-CHAIN-080 — a NaN in each stage's own state.
// ---------------------------------------------------------------------------------------------

/// Writes a NaN into every channel's first sample on one nominated `process` call and never
/// again. Spliced in *before* a product stage in an otherwise-product chain, it puts a NaN into
/// that stage's own state — its filter registers, its envelope follower, its gain ramp, its
/// convolution ring, its model history — which is what FR-CHAIN-080's method asks for and what no
/// test in this crate had ever done. `chain.rs`'s own `NanOnce` writes into an *output* buffer at
/// the end of a chain of one, reaching no product stage's state at all.
struct NanOnBlock {
    block: usize,
    seen: usize,
}

impl Stage for NanOnBlock {
    fn process(&mut self, io: &mut StageIo<'_>) {
        if self.seen == self.block {
            for channel in io.channels_mut() {
                if let Some(first) = channel.first_mut() {
                    *first = f32::NAN;
                }
            }
        }
        self.seen += 1;
    }
    fn reset(&mut self) {}
    fn latency_samples(&self) -> u32 {
        0
    }
    fn tail_samples(&self) -> u32 {
        0
    }
    fn apply(&mut self, _change: ParamChange) {}
    fn telemetry(&self, _out: &mut TelemetrySink<'_>) {}
}

/// **FR-CHAIN-080's `Verify: U` method, executed against all six product stages.** "Inject a NaN
/// into each stage's state and assert output finiteness."
///
/// Seven runs. In run *i* the six product stages are prepared exactly as `build_default_chain`
/// prepares them, with a [`NanOnBlock`] spliced in immediately before stage *i*, so the NaN enters
/// that stage's input and from there its state. The seventh feeds the NaN straight into
/// `build_default_chain`'s own input buffer, which is the literal "through build_default_chain"
/// case the requirement's `uncovered:` field named.
///
/// Each run asserts all three of the requirement's clauses: the affected block is replaced with
/// silence, the fault indicator is set, and no sample the chain ever emits is non-finite.
///
/// **One thing this measures and deliberately does not assert.** Once a NaN is in an IIR stage's
/// state — the EQ's biquads, Trim's DC blocker, the gate's envelope follower — it is *sticky*: the
/// stage keeps producing NaN and the chain keeps silencing the block, block after block. Output
/// finiteness, which is the property this requirement's `Verify:` method names and the property
/// its Rationale is about ("a NaN escaping into a DAW's mix bus"), holds throughout — which is why
/// this test passes as written. Whether "continue processing subsequent blocks" should additionally
/// mean "recover audio" is a question about the requirement's text, not about this test, and
/// `Stage::reset` could not deliver it today in any case: neither `NamState`'s history nor
/// `IrState`'s convolution ring has an allocation-free reset (`stages/nam.rs`'s and
/// `stages/ir.rs`'s own `reset` doc comments each record that gap). Written down here so the
/// behaviour is on the record rather than rediscovered.
// trace: FR-CHAIN-080
#[test]
fn fr_chain_080_a_nan_in_any_stages_state_is_contained_and_flagged() {
    /// Index of the block the NaN lands in: well after both handover crossfades have settled.
    const NAN_BLOCK: usize = 12;
    const BLOCKS: usize = 40;
    const FRAMES: usize = BLOCKS * BLOCK;

    let ctx = probe::ctx(ChannelConfig::Mono);
    let model = probe::nam_model(WaveNetShape::Nano, 31, SR);
    let cabinet = probe::mono_ir(37, 512, SR, BLOCK);
    let clean = probe::duplicated(&probe::sine(FRAMES, 220.0, SR, 0.3), 1);

    // `None` is the no-injector run: the NaN arrives in `build_default_chain`'s own input buffer.
    for position in [
        None,
        Some(0usize),
        Some(1),
        Some(2),
        Some(3),
        Some(4),
        Some(5),
    ] {
        let mut chain = match position {
            None => build_default_chain(&ctx).unwrap(),
            Some(position) => {
                let mut built = build_stage_list(&FRS_ORDER[..position], &ctx);
                built.push(Box::new(NanOnBlock {
                    block: NAN_BLOCK,
                    seen: 0,
                }));
                built.extend(build_stage_list(&FRS_ORDER[position..], &ctx));
                let mut chain = Chain::new(built);
                chain.prepare_crosscutting(&ctx);
                chain
            }
        };

        probe::set_param(&mut chain, gate::THRESHOLD_DB.id, -60.0);
        probe::set_param(&mut chain, out::GAIN_DB.id, -12.0);
        probe::load_nam(&mut chain, model.clone(), &ctx);
        probe::load_ir(&mut chain, cabinet.clone(), &ctx);

        let mut input = clean.clone();
        if position.is_none() {
            input[0][NAN_BLOCK * BLOCK] = f32::NAN;
        }
        let out = probe::run(&mut chain, &input, BLOCK);

        let label = match position {
            None => "build_default_chain's own input".to_string(),
            Some(position) => format!("{:?}'s state", FRS_ORDER[position]),
        };

        assert!(
            out[0].iter().all(|s| s.is_finite()),
            "a NaN injected into {label} reached the chain's output"
        );
        let affected = &out[0][NAN_BLOCK * BLOCK..(NAN_BLOCK + 1) * BLOCK];
        assert!(
            affected.iter().all(|s| *s == 0.0),
            "a NaN injected into {label} did not replace its whole block with silence"
        );
        assert!(
            chain.fault_count() > 0,
            "a NaN injected into {label} did not set the fault indicator"
        );
        // Non-vacuous: the run really was carrying signal before the injection.
        assert!(
            probe::peak(&out[0][..NAN_BLOCK * BLOCK]) > 1e-3,
            "the {label} run produced no signal to be faulted out of"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// NFR-PERF-020 — measured group delay against the reported latency.
// ---------------------------------------------------------------------------------------------

/// **NFR-PERF-020's `Verify: U`, at a configuration whose reported latency is not zero.** "The
/// engine shall add no latency beyond that reported per FR-CLAP-040, and that reported latency
/// shall be zero when no sample-rate conversion is active and no limiter look-ahead is engaged."
///
/// Both conjuncts, and the first one where it can actually fail. `nam.rs`'s `SlotResampler` is the
/// chain's only source of latency — every other stage's `latency_samples()` is a literal `0`, and
/// FR-OUT-030's look-ahead limiter is a Should that does not exist — so the only configuration in
/// which the first conjunct says anything is a model whose declared rate differs from the engine's,
/// which is exactly what nothing had ever measured a group delay against.
///
/// The measurement is a normalised cross-correlation of the chain's output against its input
/// ([`probe::estimate_delay_samples`], itself checked against constructed delays by `probe.rs`'s
/// own tests). The assertion is one-sided, matching the requirement's wording: the measured delay
/// must not *exceed* what `latency_samples()` reports. A second, looser assertion keeps the first
/// from being satisfiable by reporting a wildly inflated figure — a reported latency far above the
/// truth is a host-visible misalignment in the other direction.
///
/// **Measured, first time of asking: 640 samples reported, 640 measured**, for a 44.1 kHz Nano
/// model in a 48 kHz engine at a 64-frame block. That is `SlotResampler`'s own note made good —
/// M9b's output-FIFO priming claims it "makes the *actual* delay equal the `latency_samples` this
/// stage already reports", against a figure that was an upper bound met only by accident before —
/// and it is the first time anything has checked it from outside the stage.
// trace: NFR-PERF-020
#[test]
fn nfr_perf_020_measured_group_delay_never_exceeds_the_reported_latency() {
    const FRAMES: usize = 32_768;
    const WARMUP: usize = 8_192;
    /// Correlation window. Long enough that the chirp is well conditioned, short enough that the
    /// scan stays cheap in a debug build.
    const WINDOW: usize = 8_192;

    let signal = probe::chirp(FRAMES, 200.0, 6_000.0, SR, 0.25);
    let input = probe::duplicated(&signal, 1);
    let ctx = probe::ctx(ChannelConfig::Mono);

    // **No IR is loaded, deliberately.** A cabinet impulse response has a group delay of its own —
    // it is a filter, not a delta — and it is not latency: `PreparedIr::latency_samples()` is 0 by
    // construction (D-9.4) and the requirement is about what the *engine* adds on top of what it
    // reports. Loading one measurably moves the correlation peak (2 samples for this file's own
    // fixture, at a configuration reporting zero), which would make this test fail on the IR's
    // response rather than on any latency the engine failed to declare. The Ir stage still runs;
    // with nothing loaded it is FR-CHAIN-040's passthrough.
    let measure = |declared_rate_hz: u32| -> (u32, usize) {
        let mut chain = build_default_chain(&ctx).unwrap();
        probe::set_param(&mut chain, gate::THRESHOLD_DB.id, -70.0);
        probe::set_param(&mut chain, out::GAIN_DB.id, -12.0);
        probe::load_nam(
            &mut chain,
            probe::nam_model(WaveNetShape::Nano, 43, declared_rate_hz),
            &ctx,
        );
        let out = probe::run(&mut chain, &input, BLOCK);
        let reported = chain.latency_samples();
        let measured = probe::estimate_delay_samples(
            &signal[WARMUP..WARMUP + WINDOW],
            &out[0][WARMUP..],
            reported as usize * 2 + 64,
        );
        (reported, measured)
    };

    // Second conjunct: no conversion active, so nothing may be reported -- and nothing may be
    // added either, which is the half the old test could not see.
    let (reported, measured) = measure(SR);
    assert_eq!(
        reported, 0,
        "a model at the engine's own rate engages no conversion, so the chain must report zero"
    );
    assert_eq!(
        measured, 0,
        "the chain reports zero latency but delays the signal by {measured} samples"
    );

    // First conjunct, at the one configuration where it can fail: a 44.1 kHz model in a 48 kHz
    // engine.
    let (reported, measured) = measure(44_100);
    assert!(
        reported > 0,
        "a model at a different rate must engage the resampler and report its latency"
    );
    assert!(
        measured <= reported as usize,
        "the chain delays the signal by {measured} samples while reporting only {reported}: \
         latency the host is never told about"
    );
    assert!(
        measured * 4 >= reported as usize,
        "the chain reports {reported} samples of latency but only {measured} are measurable -- a \
         reported figure this far above the truth is a host-visible misalignment in the other \
         direction"
    );
}

// ---------------------------------------------------------------------------------------------
// Issue #58 — bypass latency compensation against a latency that changes at runtime.
// ---------------------------------------------------------------------------------------------

/// **Issue #58, at the configuration that actually produces it.** `chain.rs`'s own
/// `bypass_compensation_follows_a_latency_change_made_after_prepare` pins the mechanism with a
/// fake stage; this drives the real one. `build_default_chain` calls `prepare_crosscutting` once,
/// with nothing loaded, so the chain reports zero latency at that moment — and installing a NAM
/// model whose declared rate differs from the engine's engages `stages/nam.rs`'s `SlotResampler`
/// and makes it 640, which is precisely the runtime latency change FR-CLAP-040 names ("including
/// as a result of a model change under FR-NAM-050").
///
/// The compensation used to be frozen at that prepare-time zero, so from the model change onward
/// the chain told the host it added 640 samples while its own bypass path added none: bypassed
/// audio misaligned against the rest of the session by exactly the resampler's delay.
///
/// **No tag.** FR-CHAIN-030's own null test is in `chain.rs` and this does not replace it;
/// FR-CLAP-040 is a statement about the *plugin* notifying the host, which lives in `namir-clap`
/// and which nothing here executes.
///
/// The signal runs continuously across the bypass switch, and the comparison starts at the switch
/// itself rather than after a settling window — with the delay line fed on both paths (issue #59)
/// the very first bypassed sample is already correctly aligned, and a test that skipped past the
/// transition would not notice if it were not.
#[test]
fn bypass_compensation_tracks_the_latency_a_resampled_model_adds_at_runtime() {
    const FRAMES: usize = 16_384;
    /// Past the 20 ms handover crossfade (960 samples) and the 15 ms bypass blend many times
    /// over, so `NamStage::latency_samples()` has settled on the installed slot before the switch.
    const BYPASS_AT_BLOCK: usize = 128;

    let ctx = probe::ctx(ChannelConfig::Mono);
    let signal = probe::sine(FRAMES, 220.0, SR, 0.25);
    let input = probe::duplicated(&signal, 1);

    let mut chain = build_default_chain(&ctx).unwrap();
    probe::set_param(&mut chain, gate::THRESHOLD_DB.id, -70.0);
    // A model declaring 44.1 kHz in a 48 kHz engine: the chain's only source of latency.
    probe::load_nam(
        &mut chain,
        probe::nam_model(WaveNetShape::Nano, 43, 44_100),
        &ctx,
    );

    let out = probe::run_with(&mut chain, &input, BLOCK, |i, chain| {
        if i == BYPASS_AT_BLOCK {
            probe::set_param(chain, namir_params::global::GLOBAL_BYPASS.id, 1.0);
        }
    });

    let reported = chain.latency_samples() as usize;
    assert!(
        reported > 0,
        "a model at a different declared rate must engage the resampler and report its latency"
    );

    let switch = BYPASS_AT_BLOCK * BLOCK;
    let null_floor = db_to_linear(-120.0);
    /// Frames of issue #142's bypass crossfade to let pass before comparing. `Chain`'s blend
    /// settles at about 4 800 frames at 48 kHz; this leaves margin and still leaves 3 072 frames
    /// to null over. Skipping the transition does not weaken the #58 property this guards: a
    /// compensation that failed to track the latency change fails the post-settle null too.
    const SETTLE_FRAMES: usize = 5_120;
    let peak_residual = (switch + SETTLE_FRAMES..FRAMES)
        .map(|n| (out[0][n] - signal[n - reported]).abs())
        .fold(0.0f32, f32::max);
    assert!(
        peak_residual <= null_floor,
        "bypassed output minus input delayed by the {reported} samples the chain reports peaked \
         at {peak_residual:e}, above the -120 dBFS null floor {null_floor:e}: the compensation is \
         not tracking a latency that changed after `prepare_crosscutting` ran"
    );
}

// ---------------------------------------------------------------------------------------------
// Issue #30 — the sub-block split the CLAP processor performs at every automation offset.
// ---------------------------------------------------------------------------------------------

/// The block size both runs of the split probe declare to [`PrepareContext`] — so every stage's
/// scratch, and the IR convolver's whole partition schedule, are sized identically in both. Only
/// the *division* of those frames into `Chain::process` calls differs, which is exactly what the
/// split under test is.
const SPLIT_BLOCK: usize = 512;

/// The offsets a block is cut at, cycled one per block. Deliberately the set
/// `namir-clap/tests/clap_host_automation.rs` places its automation event at, including both
/// boundaries a splitter can get wrong — `1`, one frame before the rest, and `SPLIT_BLOCK - 1`,
/// one frame after it — plus a mid-block value and offsets that are multiples of nothing in
/// particular.
///
/// `0` is not among them: `namir-clap/src/audio.rs` skips a zero-length leading segment, so a
/// block cut at 0 is not cut at all and would contribute a block identical to the reference run's.
const SPLIT_OFFSETS: [usize; 5] = [1, 37, 256, 300, SPLIT_BLOCK - 1];

/// Runs `input` through `chain` in [`SPLIT_BLOCK`]-frame blocks, each divided into **two**
/// `Chain::process` calls at `SPLIT_OFFSETS[i % 5]` — the shape `namir-clap/src/audio.rs`'s event
/// split produces for a block carrying one automation point.
///
/// Deliberately not a [`probe`] helper: that module's own doc comment asks a generator with one
/// caller to stay next to that caller until it has two. Like [`probe::run`], only `Chain::process`
/// is inside [`audio_section`], so every run doubles as NFR-RT-010 evidence that the split path
/// allocates nothing.
fn run_split(chain: &mut Chain, input: &[Vec<f32>], frames: usize) -> Vec<Vec<f32>> {
    let mut out: Vec<Vec<f32>> = input.iter().map(|_| Vec::with_capacity(frames)).collect();
    let mut scratch: Vec<Vec<f32>> = input.iter().map(|_| vec![0.0f32; SPLIT_BLOCK]).collect();

    let mut offset = 0;
    let mut index = 0;
    while offset < frames {
        let whole = SPLIT_BLOCK.min(frames - offset);
        let cut = SPLIT_OFFSETS[index % SPLIT_OFFSETS.len()].min(whole);
        for n in [cut, whole - cut] {
            if n == 0 {
                continue;
            }
            for (channel, buf) in input.iter().zip(scratch.iter_mut()) {
                buf[..n].copy_from_slice(&channel[offset..offset + n]);
            }
            {
                let mut refs: Vec<&mut [f32]> = scratch.iter_mut().map(|b| &mut b[..n]).collect();
                let mut io = StageIo::new(&mut refs, n);
                audio_section(|| chain.process(&mut io));
            }
            for (buf, channel) in scratch.iter().zip(out.iter_mut()) {
                channel.extend_from_slice(&buf[..n]);
            }
            offset += n;
        }
        index += 1;
    }
    out
}

/// Frames of settling run through both chains, in whole [`SPLIT_BLOCK`] blocks, before either is
/// measured. It was **load-bearing, and not a way of avoiding an inconvenient result**, until
/// issue #141 removed the transient it excluded — see
/// [`splitting_a_block_the_way_host_automation_does_changes_nothing`]'s own doc comment for what
/// that transient was, and for the post-fix re-measurement that now reads 0e0 with no settling.
/// 8 192 frames is 171 ms at 48 kHz, an order of magnitude past the 20 ms handover crossfade and
/// the 15 ms per-stage bypass blend that make it up.
const SPLIT_SETTLE_FRAMES: usize = 8_192;

/// The loaded, settled chain both runs of the split probe drive, built the product way.
fn split_probe_chain(ctx: &PrepareContext) -> Chain {
    let mut chain = build_default_chain(ctx).unwrap();
    // Well below the probe's own level, so the gate is open throughout and its envelope never
    // becomes the thing being compared.
    probe::set_param(&mut chain, gate::THRESHOLD_DB.id, -70.0);
    // A model *declaring* 44.1 kHz in a 48 kHz engine: the one configuration that engages
    // `stages/nam.rs`'s `SlotResampler`, which is the machinery issue #30 named.
    probe::load_nam(
        &mut chain,
        probe::nam_model(WaveNetShape::Nano, 43, 44_100),
        ctx,
    );
    // A real IR, so `namir-ir`'s partitioned convolver — whose partition schedule is staggered in
    // multiples of the *declared* block size — is inside the comparison rather than passed through.
    probe::load_ir(&mut chain, probe::mono_ir(7, 2_048, SR, SPLIT_BLOCK), ctx);
    let settle = probe::duplicated(
        &probe::sine(SPLIT_SETTLE_FRAMES, 440.0, SR, 0.05),
        ctx.channel_config().output_channels() as usize,
    );
    let _ = probe::run(&mut chain, &settle, SPLIT_BLOCK);
    chain
}

/// **Issue #30's own caveat, asserted.** `namir-clap/src/audio.rs` now splits every block at each
/// automation event's `header().time()` (M14), so from the engine's side a host that automates
/// anything is a host that hands the same frames over in a *different division*. The issue named
/// the risk that carries — sub-blocks "must not reintroduce the starvation M9b just fixed",
/// `SlotResampler`'s output-FIFO priming being what makes its delay a property of the stream
/// rather than of the block-size history — and nothing checked it.
///
/// Nothing could have. `namir-clap/tests/clap_host_block_sizes.rs` drives FR-CLAP-070's randomised
/// schedule through the real vtable but with **nothing loaded**, so neither the resampler nor the
/// convolver is in that comparison at all; `stages/nam.rs`'s own
/// `resampled_path_runs_many_varying_blocks_without_allocating_or_panicking` asserts finiteness,
/// which is what its own name says. This is the gap between the two, at the configuration where
/// both pieces of machinery are live.
///
/// The assertion is that the division is *invisible*: one run in whole [`SPLIT_BLOCK`] blocks, one
/// with every block cut in two at a [`SPLIT_OFFSETS`] offset, **no parameter changed in either**,
/// compared sample for sample. Both chains are built from the same seeds and settled identically,
/// so the comparison isolates the division and nothing else.
///
/// **Measured: 0 on both channels** — the two runs agree bitwise. The bound is nonetheless stated
/// as [`SPLIT_TOLERANCE`] rather than `==`, for the reason `clap_host_block_sizes.rs` gives for
/// the same choice: the first stage whose summation order legitimately depends on the block length
/// would fail an equality assertion for something that is not a defect. A starved resampler
/// splices whole samples of silence and lands three orders above the bound; the observed maximum
/// is carried into the failure message so a drift from 0 is legible rather than absorbed.
///
/// # The transient this deliberately does not measure — issue #141, since fixed
///
/// [`SPLIT_SETTLE_FRAMES`] was not padding. Inside the ~20 ms after a resource was installed, this
/// chain's output *did* depend on the block division: measured at 1.3e-2 (Nam) and 7.2e-2 (Ir)
/// against settled peaks of ~1.2e-1 and ~5.1e-1, decaying to 1.9e-4 and 9.3e-4 over the following
/// 4 000 frames. That was **not** the split's doing — it reproduced exactly under [`probe::run`]
/// alone at 512 against 256, 128 and 64 frames, with no sub-block anywhere — and its mechanism was
/// upstream of this file, in the handover path both stages share.
///
/// It was reported from here as issue #141 and fixed there. On a first load both stages' output
/// stayed **bit-exactly the dry input** for the whole 960-sample equal-power handover crossfade,
/// and the wet signal first appeared at the start of the block the fade completed in — frame 512,
/// 768, 896 and 959 for block sizes 512, 256, 64 and 1, identically for Nam and for Ir — because
/// the shared bypass blend derived its target from the (empty) outgoing slot and so stayed shut for
/// exactly the interval the fade occupied. What a first load sounded like was therefore the 15 ms
/// bypass blend starting at a block-quantised instant, with FR-NAM-070's and FR-IR-060's fade
/// masked behind it. `stages/nam.rs`'s `begin_crossfade` carries the account;
/// [`a_first_load_is_audible_inside_its_own_fade_at_every_block_size`] is the assertion.
///
/// **Re-measured after that fix, with this probe's own chain and signal: the transient's
/// block-division dependence is 0e0** — at 512 against 256, 128 and 64, for a Nam-only and an
/// Ir-only load alike, and for this probe's own whole-versus-split comparison run from frame 0 with
/// no settling at all (8.6e-3 before the fix, exactly zero after). [`SPLIT_SETTLE_FRAMES`] is
/// therefore no longer load-bearing for the comparison below; it is kept because a probe that
/// asserts a settled property should still settle, and because dropping it would silently widen
/// what this test is claiming.
#[test]
fn splitting_a_block_the_way_host_automation_does_changes_nothing() {
    const FRAMES: usize = 16_384;
    /// Two orders above f32 accumulation over this signal and three below any real
    /// block-dependency defect — see this test's own doc comment.
    const SPLIT_TOLERANCE: f32 = 1e-6;

    let ctx = probe::ctx_at(SR, SPLIT_BLOCK, ChannelConfig::Stereo);
    let signal = probe::chirp(FRAMES, 200.0, 6_000.0, SR, 0.05);
    let input = probe::duplicated(&signal, 2);

    let mut whole_chain = split_probe_chain(&ctx);
    let whole = probe::run(&mut whole_chain, &input, SPLIT_BLOCK);
    let reported = whole_chain.latency_samples();

    let mut split_chain = split_probe_chain(&ctx);
    let split = run_split(&mut split_chain, &input, FRAMES);

    assert!(
        reported > 0,
        "the resampler never engaged, so this probe drove the one path it was written for as a \
         plain passthrough"
    );
    assert_eq!(
        split_chain.latency_samples(),
        reported,
        "the two runs report different latencies, so they are not the same chain"
    );

    for (channel, (whole, split)) in whole.iter().zip(split.iter()).enumerate() {
        assert_eq!(
            whole.len(),
            FRAMES,
            "channel {channel}: short reference run"
        );
        assert_eq!(split.len(), FRAMES, "channel {channel}: short split run");

        // Non-vacuous: two silent buffers would compare equal and prove nothing.
        let level = probe::peak(whole);
        assert!(
            level > 1e-3,
            "channel {channel}: the reference run produced no signal ({level:e}) to compare \
             against"
        );

        let (at, difference) = whole
            .iter()
            .zip(split.iter())
            .map(|(a, b)| (a - b).abs())
            .enumerate()
            .fold(
                (0usize, 0.0f32),
                |acc, (i, d)| {
                    if d > acc.1 { (i, d) } else { acc }
                },
            );
        assert!(
            difference <= SPLIT_TOLERANCE,
            "channel {channel}: cutting each block in two moved the output by {difference:e} at \
             frame {at}, against a peak of {level:e}. The division of a block into `process` \
             calls is host-driven — every automation event splits one — so a stage sensitive to \
             it renders differently depending on what the user automates"
        );
    }
}

/// The negative control for the probe above, which would otherwise be satisfiable by a comparison
/// too blunt to see anything.
///
/// The same two runs, with the split run compared against the reference **shifted by one sample** —
/// the smallest displacement a starved resampler produces, and well inside the 32 to 63 spliced
/// samples M9b actually measured. If the comparison could not tell an aligned signal from a
/// one-sample-shifted one, the bound above would hold on a chain that had lost samples.
#[test]
fn the_split_probe_would_notice_a_single_spliced_sample() {
    const FRAMES: usize = 4_096;

    let ctx = probe::ctx_at(SR, SPLIT_BLOCK, ChannelConfig::Stereo);
    let signal = probe::chirp(FRAMES, 200.0, 6_000.0, SR, 0.05);
    let input = probe::duplicated(&signal, 2);

    let mut whole_chain = split_probe_chain(&ctx);
    let whole = probe::run(&mut whole_chain, &input, SPLIT_BLOCK);

    let mut split_chain = split_probe_chain(&ctx);
    let split = run_split(&mut split_chain, &input, FRAMES);

    let shifted = whole[0]
        .iter()
        .zip(split[0].iter().skip(1))
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    let level = probe::peak(&whole[0]);
    assert!(
        shifted > level * 0.01,
        "shifting the comparison by one sample moves it only {shifted:e} against a peak of \
         {level:e}, so the equality the probe above asserts cannot tell an aligned run from one \
         that spliced a sample of silence into the stream"
    );
}

// ---------------------------------------------------------------------------------------------
// Issue #141 — a first load's equal-power fade, and where its onset actually lands.
// ---------------------------------------------------------------------------------------------

/// The handover crossfade's length in frames at [`SR`]: [`stages::HANDOVER_CROSSFADE_MS`] (20 ms)
/// at 48 kHz. Written as the same conversion the stages perform rather than as `960`, so a change
/// to that constant moves this probe with it.
const FADE_FRAMES: usize = (stages::HANDOVER_CROSSFADE_MS as usize) * (SR as usize) / 1000;

/// The block sizes issue #141's own table was measured at — the point of the test being that the
/// answer must not depend on which one a host picks. `1` is not a realistic host block size; it is
/// the limit case that makes a block-quantised onset unmistakable (959 before the fix, against 512
/// at a 512-frame block).
const ONSET_BLOCKS: [usize; 4] = [512, 256, 64, 1];

/// The declared block size every run of the onset probe is prepared at, so the IR's partition
/// schedule and every stage's scratch are identical across [`ONSET_BLOCKS`] and only the division
/// into `process` calls varies — the same isolation [`SPLIT_BLOCK`] performs for the split probe.
const ONSET_PREPARED_BLOCK: usize = 512;

/// Which resource a run of [`first_load_onset`] loads. Both stages carry the same handover
/// machinery and issue #141 measured the same numbers through both, so both are driven — one at a
/// time, so the frame each becomes audible at is attributable to that stage.
#[derive(Clone, Copy, Debug)]
enum FirstLoad {
    Nam,
    Ir,
}

/// Runs `frames` of a probe sine through a default chain in `block`-frame blocks, once with
/// nothing loaded and once with `what` loaded immediately before the first block, and returns
/// `(baseline, loaded)`.
///
/// The two chains are built identically from the same seeds and driven with the same input, so the
/// only difference between the two outputs is the resource — which makes "the first frame at which
/// they differ" exactly "the first frame at which the newly-loaded resource became audible".
fn first_load_pair(what: FirstLoad, block: usize, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let ctx = probe::ctx_at(SR, ONSET_PREPARED_BLOCK, ChannelConfig::Mono);
    let signal = probe::sine(frames, 220.0, SR, 0.25);
    let input = probe::duplicated(&signal, 1);

    let build = || {
        let mut chain = build_default_chain(&ctx).unwrap();
        // Well below the probe's level, so the gate is open from the first block and its envelope
        // is identical in both runs rather than being the thing that differs.
        probe::set_param(&mut chain, gate::THRESHOLD_DB.id, -70.0);
        chain
    };

    let mut baseline_chain = build();
    let baseline = probe::run(&mut baseline_chain, &input, block);

    let mut loaded_chain = build();
    match what {
        // A model declaring the engine's own rate: D-9.2 bypasses `SlotResampler` entirely, so the
        // stage adds no latency and the two runs stay sample-aligned. Issue #141 is about *when*
        // the wet signal appears, and a latency difference between the two runs would confound it.
        FirstLoad::Nam => probe::load_nam(
            &mut loaded_chain,
            probe::nam_model(WaveNetShape::Nano, 11, SR),
            &ctx,
        ),
        FirstLoad::Ir => probe::load_ir(
            &mut loaded_chain,
            probe::mono_ir(5, 1_024, SR, ONSET_PREPARED_BLOCK),
            &ctx,
        ),
    }
    let loaded = probe::run(&mut loaded_chain, &input, block);

    (
        baseline.into_iter().next().unwrap(),
        loaded.into_iter().next().unwrap(),
    )
}

/// The first frame at which `loaded` differs from `baseline` at all — bit inequality rather than a
/// threshold, because the question this probe asks is when the wet signal *appears*, and the
/// equal-power fade's own first samples are legitimately tiny (`sin(pi/2 / 960)` is 1.6e-3 of the
/// wet signal one sample in). A threshold would answer a different, blurrier question.
fn first_divergence(baseline: &[f32], loaded: &[f32]) -> Option<usize> {
    baseline
        .iter()
        .zip(loaded.iter())
        .position(|(a, b)| a.to_bits() != b.to_bits())
}

/// **Issue #141, asserted.** A first load must become audible inside its own equal-power fade, at
/// the same frame whatever block size the host happens to be using.
///
/// # What was wrong, and what this would have caught
///
/// On a first load `slots[active]` is `None`, and both stages derived the shared bypass blend's
/// target from that slot alone — so the blend stayed shut for the fade's whole duration and
/// multiplied FR-NAM-070's/FR-IR-060's equal-power crossfade out of existence. Both stages emitted
/// **bit-exactly the dry input** for all [`FADE_FRAMES`] of the fade, and the model or IR first
/// became audible at the start of the *block* in which the fade completed. Measured before the fix,
/// identically for [`FirstLoad::Nam`] and [`FirstLoad::Ir`]:
///
/// | block | onset before | onset after |
/// |---|---|---|
/// | 512 | 512 | 1 |
/// | 256 | 768 | 1 |
/// | 64 | 896 | 1 |
/// | 1 | 959 | 1 |
///
/// So what a user heard was not the specified fade at all but the 15 ms per-stage bypass blend,
/// starting at a block-quantised instant — up to ~85 ms of jitter at a 4096-frame block, on a path
/// [`splitting_a_block_the_way_host_automation_does_changes_nothing`] found by accident.
///
/// Frame 1 rather than frame 0 is not slack: the fade's first sample has `theta == 0`, so its
/// `sin` term is exactly zero and its `cos` term is exactly the dry passthrough a `None` outgoing
/// slot contributes. The two runs are *required* to agree bit-for-bit there, and that is the same
/// fact that makes the bypass blend's snap click-free.
///
/// # The three things asserted, and why each is needed
///
/// 1. **The onset is inside the fade, immediately** — `<= ONSET_TOLERANCE_FRAMES`, two orders
///    inside [`FADE_FRAMES`], for every block size.
/// 2. **The onset does not depend on the block size** — the defect's whole signature was that it
///    did. Asserted as equality across [`ONSET_BLOCKS`], not just as a bound each satisfies.
/// 3. **It is a fade, not a step** — the largest single-sample movement anywhere in the fade is
///    bounded by what the same signal produces with nothing loaded plus what it produces once the
///    fade has settled, which is the most an equal-power blend of the two can slew. Without this
///    the first two assertions would be satisfied by snapping straight to the wet signal, which is
///    the click FR-CHAIN-020 forbids.
#[test]
fn a_first_load_is_audible_inside_its_own_fade_at_every_block_size() {
    /// How far into the fade the wet signal is allowed to first appear. One sample is what the
    /// fix produces; the bound is loose enough not to pin an implementation detail and two orders
    /// tighter than the block-quantised onsets the defect produced.
    const ONSET_TOLERANCE_FRAMES: usize = 8;
    /// Long enough to leave a settled window well past the fade to measure the wet signal's own
    /// slew in.
    const FRAMES: usize = 8_192;
    /// Where "settled" starts: several times the 20 ms fade and the 15 ms bypass blend.
    const SETTLED: usize = 4_096;

    for what in [FirstLoad::Nam, FirstLoad::Ir] {
        let mut onsets: Vec<(usize, usize)> = Vec::new();

        for block in ONSET_BLOCKS {
            let (baseline, loaded) = first_load_pair(what, block, FRAMES);

            // Non-vacuity: if loading changed nothing at all, every assertion below is empty.
            let settled_difference =
                probe::max_abs_difference(&baseline[SETTLED..], &loaded[SETTLED..]);
            assert!(
                settled_difference > 1e-3,
                "{what:?} at block {block}: loading changed the settled output by only \
                 {settled_difference:e}, so this probe cannot see a resource become audible at all"
            );

            let onset = first_divergence(&baseline, &loaded).unwrap_or_else(|| {
                panic!("{what:?} at block {block}: the loaded run never diverged from the baseline")
            });
            assert!(
                onset <= ONSET_TOLERANCE_FRAMES,
                "{what:?} at block {block}: the wet signal first appears at frame {onset}, not \
                 inside the {FADE_FRAMES}-frame equal-power fade that started at frame 0. That is \
                 issue #141: the fade is masked by a bypass blend that stays shut until it \
                 completes, so what is heard is a 15 ms blend beginning at a block boundary"
            );
            assert_eq!(
                baseline[0].to_bits(),
                loaded[0].to_bits(),
                "{what:?} at block {block}: the fade's first sample must be the dry signal \
                 bit-for-bit (theta = 0), or the bypass blend's snap is a step"
            );

            // 3: a fade, not a step.
            let fade_slew = probe::max_abs_first_difference(&loaded[..FADE_FRAMES]);
            let dry_slew = probe::max_abs_first_difference(&baseline[..FADE_FRAMES]);
            let settled_slew = probe::max_abs_first_difference(&loaded[SETTLED..]);
            let bound = (dry_slew + settled_slew) * 1.05;
            assert!(
                fade_slew <= bound,
                "{what:?} at block {block}: the largest single-sample step inside the fade is \
                 {fade_slew:e}, above the {bound:e} an equal-power blend of a dry signal slewing \
                 {dry_slew:e} and a wet one slewing {settled_slew:e} can produce — the onset is a \
                 step, not a fade"
            );

            onsets.push((block, onset));
        }

        let (_, first_onset) = onsets[0];
        assert!(
            onsets.iter().all(|&(_, onset)| onset == first_onset),
            "{what:?}: the frame the wet signal appears at depends on the block size: {onsets:?}. \
             That dependence is issue #141's audible consequence — the instant a model becomes \
             audible jitters by up to a whole block, ~85 ms at 4096 frames"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Issue #145 finding 7 — a handover into a rate-mismatched model, and the silence its resampler
// is primed with.
// ---------------------------------------------------------------------------------------------

/// The declared rate that engages D-9.2's `SlotResampler` in a 48 kHz engine, and therefore the
/// only configuration in which a slot's own pipeline latency is nonzero.
const MISMATCHED_MODEL_RATE: u32 = 44_100;

/// Frames run to settle a handover before anything is measured: past the 960-frame equal-power
/// fade, its 640-frame priming hold, and every gain ramp in the chain.
const HANDOVER_SETTLE_FRAMES: usize = 4_096;

/// Builds the probe chain both limbs below use: a real default chain with the gate held open, so
/// the only thing that moves the output level is the handover under test.
fn handover_probe_chain(ctx: &PrepareContext) -> Chain {
    let mut chain = build_default_chain(ctx).unwrap();
    probe::set_param(&mut chain, gate::THRESHOLD_DB.id, -70.0);
    chain
}

/// **Issue #145 finding 7, and the defect behind PR #145's red `clap_host_sample_rates` job.**
/// A handover *into* a rate-mismatched model must not crossfade against the silence that model's
/// resampler is primed with.
///
/// # The mechanism
///
/// `SlotResampler::new` puts one engine block of silence in the incoming slot's output FIFO —
/// deliberately, and load-bearing since M9b: it is what makes the slot's actual delay equal the
/// 640 samples it reports. The consequence nothing had measured is that the slot's first 640
/// outputs *are* that silence, so an equal-power fade started at the install spends its first two
/// thirds blending against nothing: `outgoing * cos(theta)` alone, reaching `cos(60 deg)` = 0.5
/// at frame 640. A −6 dB, 13 ms sag in the middle of every handover into a resampled model,
/// which is FR-NAM-070's "shall not ... glitch" clause failing on precisely the path D-9.2's
/// resampler exists for.
///
/// # Why the same model on both sides
///
/// Because it makes the correct answer exact rather than approximate. FR-NAM-070 specifies an
/// **equal-power** crossfade; between two identical, sample-aligned signals that law gives
/// `level * (cos(theta) + sin(theta))`, which is `>= level` everywhere on `[0, pi/2]` and peaks
/// at `sqrt(2)`. So the envelope of a reload of the same model may rise, and may not fall — no
/// tolerance-chasing, and no dependence on how two different models happen to compare in level.
/// It is also exactly what `crates/namir-clap/tests/clap_host_sample_rates.rs`'s loaded sweep
/// does (its activation replay and its own `state_ext::load` both recall the same document), which
/// is how this reached CI: with the sag inside that test's 960-frame measurement window its RMS
/// came out 1.76 dB low.
///
/// Committed red-first. Before the fix the envelope of a same-model reload runs
/// `0.1743 0.1743 ... 0.0993 → 0.1454` — a monotone sag to 0.5x over 640 frames ended by a 47%
/// single-block jump when the incoming slot's real output finally arrives. Measured as the worst
/// 64-frame window peak against the settled level: **−4.89 dB before, +0.00 dB after** (the
/// instantaneous minimum is the `cos(60 deg)` the mechanism predicts, −6.0 dB at frame 640; the
/// window this metric quantises to straddles it).
/// **Carries no trace tag**, deliberately: FR-NAM-070 already resolves through
/// `engine.rs`'s `fr_nam_070_swapping_models_under_a_sine_has_no_discontinuity_or_dropout`, and
/// this is regression evidence for one defect on that path rather than a second reading of the
/// requirement — a tag here would add a resolution site and move the generated plan without
/// changing what is actually verified.
#[test]
fn a_handover_into_a_resampled_model_never_fades_against_its_priming_silence() {
    const FRAMES: usize = 8_192;
    const BLOCK_N: usize = 256;
    /// The window the fade and its priming hold occupy, generously: 640 + 960 frames plus a
    /// block of margin, rounded up.
    const FADE_WINDOW: usize = 2_048;
    /// Envelope resolution. 64 frames is 1.3 cycles of the 1 kHz probe, so a window's peak is its
    /// envelope, and 30 windows span the fade.
    const ENVELOPE_WINDOW: usize = 64;
    /// How far under the settled level the envelope may sit. An equal-power blend of a signal
    /// with itself cannot go under it at all; this is float and gain-ramp slack, two orders
    /// tighter than the 6 dB the defect produces.
    const SAG_TOLERANCE_DB: f32 = 0.2;

    let ctx = probe::ctx_at(SR, BLOCK_N, ChannelConfig::Mono);
    let signal = probe::sine(FRAMES, 1_000.0, SR, 0.25);
    let input = probe::duplicated(&signal, 1);
    let model = probe::nam_model(WaveNetShape::Nano, 11, MISMATCHED_MODEL_RATE);

    let mut chain = handover_probe_chain(&ctx);
    probe::load_nam(&mut chain, Arc::clone(&model), &ctx);
    let settling = probe::duplicated(&signal[..HANDOVER_SETTLE_FRAMES], 1);
    probe::run(&mut chain, &settling, BLOCK_N);
    assert!(
        chain.latency_samples() > 0,
        "the first model never engaged D-9.2's resampler, so this probe is measuring a handover \
         with no priming silence in it and proves nothing"
    );

    // The same model again, installed on the first block of the measured run: a replacement
    // handover, both of whose sides carry the identical 640-sample delay.
    let out = probe::run_with(&mut chain, &input, BLOCK_N, |i, chain| {
        if i == 0 {
            probe::load_nam(chain, Arc::clone(&model), &ctx);
        }
    });
    let rendered = &out[0];

    let settled = probe::peak(&rendered[FRAMES - HANDOVER_SETTLE_FRAMES..]);
    assert!(
        settled > 1e-3,
        "the settled level is {settled:e}, so there is no signal here to detect a sag in"
    );
    let worst = probe::min_window_peak(&rendered[..FADE_WINDOW], ENVELOPE_WINDOW);
    let sag_db = 20.0 * (worst / settled).log10();
    assert!(
        sag_db >= -SAG_TOLERANCE_DB,
        "reloading the same rate-mismatched model dipped the output envelope to {worst:e} \
         against a settled {settled:e} ({sag_db:+.2} dB) inside its own handover. An equal-power \
         fade between a signal and itself cannot go below that level at all, so what the fade is \
         blending against for the first 640 frames is the incoming SlotResampler's priming \
         silence, not its output"
    );
}

/// Finding 7's other half, stated as an equality rather than a level: until the incoming slot has
/// produced a real sample there is nothing to fade *to*, so the stage's output must be its dry
/// input **bit for bit** — the outgoing side alone, which is what `theta == 0` already evaluates
/// to.
///
/// A first load, so the outgoing side is a pure dry passthrough and "the outgoing side alone" is
/// something a baseline chain with nothing loaded reproduces exactly. The two runs are built from
/// the same seeds and driven with the same input, so the first frame at which they differ is the
/// first frame at which the model contributed anything.
///
/// **Not in tension with [`a_first_load_is_audible_inside_its_own_fade_at_every_block_size`]**,
/// which asserts the opposite bound — divergence within 8 frames — because that probe loads a
/// model declaring the *engine's own* rate and says so: with no `SlotResampler` there is no
/// pipeline to prime, this hold is zero frames long, and issue #141's onset is unchanged. The two
/// together say the fade starts as early as it can and no earlier.
///
/// Committed red-first: before the fix the two runs diverge at **frame 2**, 638 frames before the
/// incoming slot can produce anything (frames 0 and 1 agree only because `cos(theta)` is still 1.0
/// to the bit that early). The model's contribution there is a scaling of the dry signal by
/// `cos(theta)` — an attenuation dressed up as a fade.
#[test]
fn a_resampled_first_load_is_the_dry_signal_until_its_pipeline_has_primed() {
    const FRAMES: usize = 8_192;
    const BLOCK_N: usize = 256;
    /// How soon after the hold the wet signal must appear. The same bound, and the same
    /// reasoning, as [`ONSET_TOLERANCE_FRAMES`]: one frame is what the fix produces.
    const ONSET_TOLERANCE_FRAMES: usize = 8;

    let ctx = probe::ctx_at(SR, BLOCK_N, ChannelConfig::Mono);
    let signal = probe::sine(FRAMES, 220.0, SR, 0.25);
    let input = probe::duplicated(&signal, 1);
    let model = probe::nam_model(WaveNetShape::Nano, 11, MISMATCHED_MODEL_RATE);

    let mut baseline_chain = handover_probe_chain(&ctx);
    let baseline = probe::run(&mut baseline_chain, &input, BLOCK_N);

    let mut loaded_chain = handover_probe_chain(&ctx);
    let loaded = probe::run_with(&mut loaded_chain, &input, BLOCK_N, |i, chain| {
        if i == 0 {
            probe::load_nam(chain, Arc::clone(&model), &ctx);
        }
    });

    // Read once the handover has settled: `NamStage::latency_samples` reports the *outgoing* slot
    // for the whole fade, so this is the figure only after `active` has flipped onto the model.
    let hold = loaded_chain.latency_samples() as usize;
    assert!(
        hold > 0,
        "the model never engaged D-9.2's resampler, so there is no priming hold to measure"
    );

    let onset = first_divergence(&baseline[0], &loaded[0])
        .expect("the loaded run never diverged from the baseline at all");
    assert!(
        onset >= hold,
        "a model whose pipeline cannot produce a real sample for {hold} frames changed the \
         output at frame {onset}. What it contributed there was the incoming SlotResampler's \
         priming silence, faded in at cos(theta) against the dry signal"
    );
    assert!(
        onset <= hold + ONSET_TOLERANCE_FRAMES,
        "the wet signal first appears at frame {onset}, {} frames after the {hold}-frame hold \
         its own pipeline needs — the fade is starting later than it can",
        onset - hold
    );
}
