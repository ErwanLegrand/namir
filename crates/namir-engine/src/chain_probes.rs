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

use namir_core::{ChannelConfig, db_to_linear};
use namir_fixtures::nam::WaveNetShape;
use namir_params::stages::{eq, gate, ir, nam, out, trim};

use crate::chain::Chain;
use crate::param::ParamChange;
use crate::prepare::PrepareContext;
use crate::probe::{self, BLOCK, SR};
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
