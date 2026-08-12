//! NFR-RT-040: "The engine's worst-case per-block processing time shall not depend on the audio
//! content, on parameter values, or on how long the engine has been running."
//!
//! # Why this is a separate binary from `tail_structure.rs`
//!
//! NFR-RT-040's annotation lived on `tail_structure.rs` until M14, and that file could not carry
//! it: it is a diagnostic that asks whether the chain's tail is code-driven or environmental, it
//! drives one xorshift material at one amplitude with the gate and EQ parameters set once before
//! the measured loop, and — the point its own `uncovered:` field made — it **asserts nothing**. A
//! `Verify: B` requirement needs a benchmark that asserts a numeric threshold in-process (D-23.1),
//! and the threshold NFR-RT-040 implies is not a budget but an *invariance*, which cannot be
//! measured at all without more than one condition to compare.
//!
//! So this binary drives the same chain assembly under **nine conditions** and asserts that the
//! worst-case per-block cost is the same under all of them. `tail_structure.rs` keeps its own job
//! and its own doc comment; nothing there changed except the annotation moving here.
//!
//! # The three variables, and how each is varied
//!
//! **Audio content** — six materials spanning what an instrument input can present: near-silence,
//! a small and a near-full-scale noise, a pure tone (perfectly correlated, unlike noise), sparse
//! transients (an envelope that swings the gate and the meters over the widest range available),
//! and a signal decaying continuously toward the denormal range. The last is the one with a
//! mechanism behind it: denormal arithmetic is the one thing in this chain that can genuinely make
//! identical code take longer, which is why NFR-RT-030 has a benchmark of its own.
//!
//! **Parameter values** — three settings: everything at its descriptor default, everything driven
//! to an extreme (gate pinned shut, EQ at ±15 dB with a Q of 5 and both defeatable filters on, IR
//! at +24 dB with both cuts engaged), and everything bypassed. Bypassed is the interesting one:
//! FR-CHAIN-020's bypass is a *blend*, not a branch — every stage still computes its wet path — so a
//! chain whose stages are all "off" must cost the same as one whose stages are all on. If it does
//! not, the bypass has become a branch, which is the failure mode this row exists to catch.
//!
//! **How long the engine has been running** — measured within each arm rather than across arms:
//! every arm's run is split in half and the same estimator computed over each half, so a cost that
//! crept upward with elapsed time (a growing buffer, an unbounded queue, a fragmenting allocator)
//! would separate the two halves.
//!
//! # The statistic, and why the assertion is on the estimator
//!
//! Both are computed and both are printed. The requirement names the **99.9th percentile**, so that
//! is reported for every arm, and it is what part 3 of the verdict asserts on — for the arms D-2.4
//! permits quoting. But raw `p99.9` on a general-purpose desktop mixes this chain's cost with
//! whatever else the machine did during that arm's run, and an *invariance* comparison is where
//! that is fatal: two arms differing by 20% of contamination would read as a violation of a
//! requirement about the code. So part 1 asserts on the **contamination-immune estimator** — the
//! largest per-residue minimum modulo the IR schedule's own period, `tail_structure.rs`'s
//! instrument, adopted by `six_stage_chain.rs` as permanent methodology and explained in full in
//! both. Background load can only push a per-residue minimum *up*, and only if it lands on every
//! one of that residue's occurrences, so the estimator is the same figure on a quiet machine and a
//! busy one.
//!
//! Every measured block is run inside a fresh `namir_platform::DenormalGuard`, exactly as
//! `denormal_guard.rs` does and for its reasons: D-7.4 puts one around the real audio callback, so
//! an unguarded measurement measures a configuration the product never runs in — and it would
//! report NFR-RT-030's territory (the subnormal penalty, which has a benchmark of its own) as an
//! NFR-RT-040 violation, since two of the arms here deliberately drive subnormals into every filter
//! state in the chain.
//!
//! Arms are measured **interleaved**, one repetition of each in turn rather than all repetitions of
//! one arm together, so a machine that gets busier over the run degrades every arm equally instead
//! of singling out whichever happened to be measured last. This is the same reasoning D-2.4 gives
//! for its own repetition rule, applied across conditions rather than across time.
//!
//! # What "the same" means numerically
//!
//! [`INVARIANCE_TOLERANCE_PCT`]: the spread between the cheapest and the dearest arm, as a
//! percentage of the cheapest. Its value and the measurement behind it are recorded at that
//! constant.

use std::sync::Arc;
use std::time::Instant;

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::stages::eq::{EqPrep, EqStage};
use namir_engine::stages::gate::{GatePrep, GateStage};
use namir_engine::stages::ir::{IrPrep, IrStage};
use namir_engine::stages::nam::{NamPrep, NamStage};
use namir_engine::stages::out::OutPrep;
use namir_engine::stages::trim::TrimPrep;
use namir_engine::{Chain, ParamChange, ParamId, PrepareContext, Stage, StageIo, StagePrep};
use namir_fixtures::ir::decaying_noise;
use namir_fixtures::nam::{WaveNetShape, generate};
use namir_params::stages::{eq, gate, ir, out, trim};
use namir_platform::DenormalGuard;

// Conditions, fixtures and chain assembly are `six_stage_chain.rs`'s -- see that file's doc comment
// for why each is what it is. This binary measures a different property of the same chain.
const BLOCK_SIZE: usize = 64;
const SAMPLE_RATE_HZ: u32 = 48_000;
const SAMPLE_RATE_F64: f64 = 48_000.0;

/// Discarded before every arm's measured run: long enough for that arm's parameter changes to have
/// finished ramping (the slowest is `trim.rs`/`out.rs`'s 25 ms gain ramp) and for its material to
/// have filled the gate's envelope, the meters and the convolution ring.
const WARMUP_BLOCKS: usize = 4_000;
/// Measured blocks per arm per repetition. 16 384 is 128 full IR schedule periods, so every residue
/// the estimator takes a minimum over is sampled 128 times — enough for the minimum to be the
/// uncontaminated occurrence rather than merely the least-contaminated one.
const MEASURED_BLOCKS: usize = 16_384;
/// Overrides [`MEASURED_BLOCKS`], rounded down to a whole number of `2 · IR_PERIOD_BLOCKS` so the
/// two halves part 2 compares stay aligned to the schedule, and floored at that same figure.
///
/// **Shortening the run makes this binary stricter, not laxer**: fewer occurrences per residue
/// means each residue's minimum is less likely to be the uncontaminated one, so the estimator sits
/// higher and noisier and the measured spread widens. A tolerance set from a short run is therefore
/// a conservative tolerance. This exists so a development machine can produce a real number in a
/// few minutes; the default is what the `docs/02-architecture.md` §2 reference machine should run.
const BLOCKS_ENV: &str = "NAMIR_RT_040_BLOCKS";
/// Interleaved repetitions of the whole arm set (D-2.4's "at least 5 repetitions, never a single
/// run", applied to each arm).
const DEFAULT_REPS: usize = 5;
/// The same floor, enforced: [`REPS_ENV`] is clamped up to it.
const MIN_REPS: usize = 5;
/// Overrides [`DEFAULT_REPS`] upward.
const REPS_ENV: &str = "NAMIR_RT_040_REPS";

const NAM_SEED: u64 = 0xC0FF_EE01;
const IR_SEED_LEFT: u64 = 0xBEEF_0001;
const IR_SEED_RIGHT: u64 = 0xBEEF_0002;
const IR_LEN_SAMPLES: usize = 2 * SAMPLE_RATE_HZ as usize;
const IR_DECAY_TAU_SAMPLES: f64 = 8_000.0;

/// The IR schedule's own period in host blocks at this condition: `DEFAULT_MAX_PARTITION /
/// BLOCK_SIZE`. Same constant, same reasoning and same staleness check as `tail_structure.rs`'s and
/// `six_stage_chain.rs`'s: hard-coded so this file stays a passive observer of the schedule rather
/// than a second consumer of it, and asserted against the real schedule in `main`.
const IR_PERIOD_BLOCKS: usize = 8192 / BLOCK_SIZE;

/// How far apart the arms' worst-case per-block costs may be — the dearest as a percentage above
/// the cheapest — before this binary calls NFR-RT-040 violated.
///
/// **Measured before it was chosen.** The chain has no branch on a sample value or on a parameter
/// value anywhere: every stage runs its whole signal path every block, and FR-CHAIN-020's bypass is
/// a blend rather than a branch, so the *expected* spread is zero and anything this bound admits is
/// noise in the estimator rather than real content- or parameter-dependence.
///
/// Measured on the development machine — **not** `docs/02-architecture.md` §2's reference machine,
/// so the absolute percentages that run printed are not quotable; a *ratio* between arms measured in
/// the same interleaved session is, which is the whole reason the arms are interleaved — at a
/// deliberately short 2 560 blocks per arm, which widens the spread (see [`BLOCKS_ENV`]). Over five
/// interleaved repetitions the nine arms' best estimators landed between **24.93% and 26.32%** of
/// the block period: a **part 1 spread of 5.6%**, and a **part 2 worst half-to-half drift of 6.0%**,
/// on a machine busy enough that D-2.4 discarded every single arm's raw `p99.9`. The cheapest arm
/// was the one decaying into subnormals, which is the guard doing its job.
///
/// Re-run at 1 280 blocks with the assertions live, the same nine arms measured **1.9%** apart with
/// a worst half-to-half drift of **10.1%** — the drift figure inflating as the run shortens exactly
/// as [`BLOCKS_ENV`] says it does, since ten schedule periods per half is a noisy estimator. That
/// run also left three arms quotable, so part 3 ran too: the requirement's own `p99.9` agreed to
/// **3.7%** across them.
///
/// 15% is those figures with about two and a half times their headroom, and deliberately no
/// tighter: a bound that tracks one machine's noise floor fails on the next machine for reasons
/// that have nothing to do with the requirement. A real content- or parameter-dependence would not
/// be a few percent — a branch taken on some inputs and not others, or a subnormal penalty escaping
/// D-7.4's guard, costs tens of percent, which is what `denormal_guard.rs`'s own measurements of
/// the latter show.
const INVARIANCE_TOLERANCE_PCT: f64 = 15.0;

/// D-2.4 condition 4, as a number, from `six_stage_chain.rs`: how far an arm's raw `p99.9` may
/// exceed its own contamination-immune estimator before that arm's `p99.9` is discarded rather than
/// asserted against. Part 2 below only compares arms that pass this.
const VALIDITY_MARGIN_PCT: f64 = 5.0;

/// Set (to anything) to print every figure and assert nothing. Unlike NFR-PERF-010's own
/// informational switch this is **not** for "the wrong machine" — NFR-RT-040 states a ratio, and a
/// ratio is meaningful on any hardware — it is for a machine too busy for the estimator itself to
/// be trusted, and for exploring a failure without the panic truncating the report.
const INFORMATIONAL_ENV: &str = "NAMIR_RT_040_INFORMATIONAL";

// ---------------------------------------------------------------------------------------------
// Materials: the audio-content axis.
// ---------------------------------------------------------------------------------------------

/// What an arm feeds the chain. Every variant is deterministic given its own state, so two
/// repetitions of one arm see byte-identical audio and any difference between them is the machine.
#[derive(Clone, Copy)]
enum Material {
    /// Xorshift noise at `amplitude`. `six_stage_chain.rs`'s and `tail_structure.rs`'s own
    /// material, at their own 0.1 and again near full scale.
    Noise { amplitude: f32 },
    /// A pure tone: perfectly correlated block to block, where noise is perfectly uncorrelated.
    Sine { hz: f64, amplitude: f32 },
    /// Digital black. The gate is shut, the meters read nothing, and every filter state decays
    /// toward zero — the cheapest input there is, if cost depended on the input at all.
    Silence,
    /// One full-scale impulse every `period` samples and silence between: the widest envelope
    /// swing available, so the gate opens and closes continuously throughout the measured run.
    Transients { period: usize, amplitude: f32 },
    /// A signal decaying continuously toward the denormal range and restarting. The one material
    /// with a mechanism by which identical code could genuinely take longer, and the reason
    /// NFR-RT-030 exists.
    Decay,
}

/// Per-arm generator state, reset at the start of every repetition so repetitions are comparable.
struct MaterialState {
    rng: u64,
    sample_index: u64,
}

impl MaterialState {
    fn new() -> Self {
        Self {
            rng: 0xC0DE_CAFE ^ 0x9E37_79B9_7F4A_7C15,
            sample_index: 0,
        }
    }
}

impl Material {
    fn fill(&self, state: &mut MaterialState, out: &mut [f32]) {
        for s in out.iter_mut() {
            let i = state.sample_index;
            state.sample_index += 1;
            *s = match *self {
                Material::Noise { amplitude } => {
                    state.rng ^= state.rng << 13;
                    state.rng ^= state.rng >> 7;
                    state.rng ^= state.rng << 17;
                    let noise = ((state.rng % 2_000_003) as f32 / 1_000_001.5) - 1.0;
                    amplitude * noise
                }
                Material::Sine { hz, amplitude } => {
                    (f64::from(amplitude)
                        * (std::f64::consts::TAU * hz * i as f64 / SAMPLE_RATE_F64).sin())
                        as f32
                }
                Material::Silence => 0.0,
                Material::Transients { period, amplitude } => {
                    if (i as usize).is_multiple_of(period) {
                        amplitude
                    } else {
                        0.0
                    }
                }
                // Halves every 1024 samples from near full scale and restarts once it is well
                // inside the subnormal range (`f32`'s smallest normal is ~1.18e-38, i.e. 2^-126;
                // 130 halvings from 0.9 is comfortably past it). ~2.8 s per cycle, so a measured
                // run covers several, and each cycle spends its last few thousand samples driving
                // genuinely subnormal values into every filter state in the chain.
                Material::Decay => {
                    let step = (i as usize / 1_024) % 130;
                    0.9 * 0.5f32.powi(step as i32)
                }
            };
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Parameter settings: the parameter-value axis.
// ---------------------------------------------------------------------------------------------

/// Which of the three parameter settings an arm runs under. Applied to the live chain before that
/// arm's warmup, through the same `Chain::apply` a host automation event travels.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Settings {
    /// Every parameter at its `namir-params` descriptor default.
    Default,
    /// Every parameter at an extreme: gate pinned shut with the fastest attack and the longest
    /// release, EQ at ±15 dB with a Q of 5 and both defeatable filters engaged, IR at +24 dB with
    /// both cuts engaged, trim and output at their loudest.
    Extreme,
    /// Every bypassable stage off. Costs the same as `Default` unless FR-CHAIN-020's blend has
    /// become a branch.
    Bypassed,
}

fn set(chain: &mut Chain, id: namir_params::ParamId, value: f32) {
    chain.apply(ParamChange {
        id: ParamId(id.0),
        value,
    });
}

impl Settings {
    fn apply_to(self, chain: &mut Chain) {
        // Always written explicitly rather than left over from the previous arm, so an arm's
        // configuration is its whole configuration and the order arms run in cannot matter.
        let (gate_enabled, nam_enabled, ir_enabled, eq_enabled) = match self {
            Settings::Bypassed => (0.0, 0.0, 0.0, 0.0),
            _ => (1.0, 1.0, 1.0, 1.0),
        };
        set(chain, gate::ENABLED.id, gate_enabled);
        set(chain, namir_params::stages::nam::ENABLED.id, nam_enabled);
        set(chain, ir::ENABLED.id, ir_enabled);
        set(chain, eq::ENABLED.id, eq_enabled);

        let extreme = self == Settings::Extreme;
        set(
            chain,
            gate::THRESHOLD_DB.id,
            if extreme { 0.0 } else { -70.0 },
        );
        set(chain, gate::ATTACK_MS.id, if extreme { 0.1 } else { 1.0 });
        set(chain, gate::HOLD_MS.id, if extreme { 500.0 } else { 30.0 });
        set(
            chain,
            gate::RELEASE_MS.id,
            if extreme { 2000.0 } else { 100.0 },
        );

        set(chain, trim::GAIN_DB.id, if extreme { 24.0 } else { 0.0 });

        set(chain, ir::LEVEL_DB.id, if extreme { 24.0 } else { 0.0 });
        set(chain, ir::LOW_CUT_ENABLED.id, f32::from(extreme));
        set(
            chain,
            ir::LOW_CUT_FREQ_HZ.id,
            if extreme { 500.0 } else { 80.0 },
        );
        set(chain, ir::HIGH_CUT_ENABLED.id, f32::from(extreme));
        set(
            chain,
            ir::HIGH_CUT_FREQ_HZ.id,
            if extreme { 1_000.0 } else { 8_000.0 },
        );

        set(
            chain,
            eq::LOW_SHELF_GAIN_DB.id,
            if extreme { 15.0 } else { 0.0 },
        );
        set(
            chain,
            eq::LOW_SHELF_FREQ_HZ.id,
            if extreme { 40.0 } else { 100.0 },
        );
        set(chain, eq::MID_GAIN_DB.id, if extreme { -15.0 } else { 0.0 });
        set(
            chain,
            eq::MID_FREQ_HZ.id,
            if extreme { 200.0 } else { 1_000.0 },
        );
        set(chain, eq::MID_Q.id, if extreme { 5.0 } else { 0.707 });
        set(
            chain,
            eq::HIGH_SHELF_GAIN_DB.id,
            if extreme { 15.0 } else { 0.0 },
        );
        set(
            chain,
            eq::HIGH_SHELF_FREQ_HZ.id,
            if extreme { 12_000.0 } else { 3_000.0 },
        );
        set(chain, eq::HIGH_PASS_ENABLED.id, f32::from(extreme));
        set(
            chain,
            eq::HIGH_PASS_FREQ_HZ.id,
            if extreme { 500.0 } else { 80.0 },
        );
        set(chain, eq::LOW_PASS_ENABLED.id, f32::from(extreme));
        set(
            chain,
            eq::LOW_PASS_FREQ_HZ.id,
            if extreme { 1_000.0 } else { 18_000.0 },
        );

        // Kept well below full scale in every arm so FR-CHAIN-090's ceiling is not what any arm is
        // measuring.
        set(chain, out::GAIN_DB.id, if extreme { -24.0 } else { -12.0 });
    }

    fn label(self) -> &'static str {
        match self {
            Settings::Default => "params=default",
            Settings::Extreme => "params=extreme",
            Settings::Bypassed => "params=bypassed",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// One arm.
// ---------------------------------------------------------------------------------------------

struct Arm {
    what: &'static str,
    material: Material,
    settings: Settings,
}

/// One arm's measurement, in percent of the block period.
#[derive(Clone, Copy)]
struct Measurement {
    p50: f64,
    p999: f64,
    /// The contamination-immune estimator over the whole run.
    estimator: f64,
    /// The same estimator over the first and second halves of the run — the "how long the engine
    /// has been running" axis.
    first_half: f64,
    second_half: f64,
}

impl Measurement {
    /// D-2.4's validity check: raw `p99.9` within [`VALIDITY_MARGIN_PCT`] points of this arm's own
    /// estimator. A `p99.9` that fails it is contamination, and D-2.4 says such a figure must be
    /// discarded rather than quoted — so it is not compared either.
    fn p999_is_quotable(&self) -> bool {
        self.p999 - self.estimator <= VALIDITY_MARGIN_PCT
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

/// The largest per-residue minimum of `durations` modulo [`IR_PERIOD_BLOCKS`] — the IR schedule's
/// own worst-case block, which background load cannot inflate. See this file's doc comment, and
/// `tail_structure.rs` for the full derivation.
fn estimator_ns(durations: &[u64]) -> u64 {
    let mut per_residue_min = vec![u64::MAX; IR_PERIOD_BLOCKS];
    for (i, &v) in durations.iter().enumerate() {
        let r = i % IR_PERIOD_BLOCKS;
        per_residue_min[r] = per_residue_min[r].min(v);
    }
    per_residue_min
        .iter()
        .copied()
        .filter(|&v| v != u64::MAX)
        .max()
        .unwrap_or(0)
}

fn analyse(durations: &[u64], block_period_ns: u64) -> Measurement {
    let pct = |v: u64| v as f64 / block_period_ns as f64 * 100.0;
    let mut sorted = durations.to_vec();
    sorted.sort_unstable();

    // Both halves are a whole number of IR periods, so each half's estimator is computed over the
    // same residue set as the other and the comparison is like for like.
    let half = durations.len() / 2;
    assert!(
        half.is_multiple_of(IR_PERIOD_BLOCKS),
        "the measured block count must be a multiple of 2 x {IR_PERIOD_BLOCKS}"
    );

    Measurement {
        p50: pct(percentile(&sorted, 0.50)),
        p999: pct(percentile(&sorted, 0.999)),
        estimator: pct(estimator_ns(durations)),
        first_half: pct(estimator_ns(&durations[..half])),
        second_half: pct(estimator_ns(&durations[half..])),
    }
}

// ---------------------------------------------------------------------------------------------
// Chain assembly and the measurement loop.
// ---------------------------------------------------------------------------------------------

fn write_stereo_wav(sample_rate: u32, left: &[f32], right: &[f32]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut buf = Vec::new();
    {
        let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
        for (&l, &r) in left.iter().zip(right.iter()) {
            w.write_sample(l).unwrap();
            w.write_sample(r).unwrap();
        }
        w.finalize().unwrap();
    }
    buf
}

/// Pins this thread to one core (D-2.1). Identical to `tail_structure.rs`'s, whose doc comment
/// carries the measured argument for why index 4 and not index 0.
fn pin_to_measurement_core() {
    let Some(ids) = core_affinity::get_core_ids() else {
        return;
    };
    if ids.is_empty() {
        return;
    }
    let idx = std::env::var("NAMIR_PIN_CORE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4)
        .min(ids.len() - 1);
    core_affinity::set_for_current(ids[idx]);
}

// trace-partial: NFR-RT-040
// uncovered: NFR-RT-040 — all three variables the requirement names are varied and asserted here:
// uncovered: nine content and parameter conditions (spread 1.9%) and every run's own two halves
// uncovered: (worst drift 10.1%), against the contamination-immune estimator. What is only
// uncovered: partly spanned is the statistic the Verify line actually names: raw p99.9 is computed
// uncovered: and printed for all nine arms but compared across only the 3 of 9 that D-2.4 left
// uncovered: quotable on the machine this has run on, the other six being contaminated. And that
// uncovered: machine is not 02-architecture.md section 2's reference machine — the ratios this
// uncovered: binary asserts are machine-independent in a way NFR-PERF-010's absolute budget is
// uncovered: not, but no run on the reference machine has been performed. Both are closed by one
// uncovered: quiet run there, not by more code; closes M8
fn main() {
    pin_to_measurement_core();

    let sample_rate = SampleRate::new(SAMPLE_RATE_HZ).expect("48 kHz is valid");
    let ctx = PrepareContext::new(sample_rate, BLOCK_SIZE, ChannelConfig::Stereo)
        .expect("BLOCK_SIZE is nonzero");

    let gate_stage: GateStage = GatePrep.prepare(&ctx).expect("gate");
    let trim_stage = TrimPrep.prepare(&ctx).expect("trim");
    let mut nam_stage: NamStage = NamPrep.prepare(&ctx).expect("nam");
    let mut ir_stage: IrStage = IrPrep.prepare(&ctx).expect("ir");
    let eq_stage: EqStage = EqPrep.prepare(&ctx).expect("eq");
    let out_stage = OutPrep.prepare(&ctx).expect("out");

    let model = generate(WaveNetShape::Standard, NAM_SEED).expect("fixture");
    nam_stage.load_model(Arc::new(
        namir_nam::load(&model.to_json_bytes()).expect("load"),
    ));

    let left = decaying_noise(IR_LEN_SAMPLES, IR_SEED_LEFT, IR_DECAY_TAU_SAMPLES);
    let right = decaying_noise(IR_LEN_SAMPLES, IR_SEED_RIGHT, IR_DECAY_TAU_SAMPLES);
    let ir_bytes = write_stereo_wav(SAMPLE_RATE_HZ, &left, &right);
    ir_stage.load_ir(Arc::new(
        namir_ir::PreparedIr::from_wav_bytes(&ir_bytes, sample_rate, BLOCK_SIZE).expect("ir load"),
    ));

    let schedule = namir_ir::build_schedule(
        IR_LEN_SAMPLES,
        BLOCK_SIZE,
        namir_ir::DEFAULT_GROWTH_FACTOR,
        namir_ir::DEFAULT_MAX_PARTITION,
    );
    let largest = schedule.iter().map(|s| s.size).max().unwrap_or(BLOCK_SIZE);
    assert_eq!(
        largest / BLOCK_SIZE,
        IR_PERIOD_BLOCKS,
        "IR_PERIOD_BLOCKS is stale relative to the real schedule, so the estimator this binary \
         asserts on would be computed modulo the wrong period"
    );

    let stages: Vec<Box<dyn Stage>> = vec![
        Box::new(gate_stage),
        Box::new(trim_stage),
        Box::new(nam_stage),
        Box::new(ir_stage),
        Box::new(eq_stage),
        Box::new(out_stage),
    ];
    let mut chain = Chain::new(stages);
    chain.prepare_crosscutting(&ctx);

    let arms = [
        // The content axis, at the default settings.
        Arm {
            what: "silence",
            material: Material::Silence,
            settings: Settings::Default,
        },
        Arm {
            what: "noise -20 dBFS",
            material: Material::Noise { amplitude: 0.1 },
            settings: Settings::Default,
        },
        Arm {
            what: "noise -1 dBFS",
            material: Material::Noise { amplitude: 0.9 },
            settings: Settings::Default,
        },
        Arm {
            what: "220 Hz tone",
            material: Material::Sine {
                hz: 220.0,
                amplitude: 0.5,
            },
            settings: Settings::Default,
        },
        Arm {
            what: "sparse transients",
            material: Material::Transients {
                period: 2_400,
                amplitude: 0.9,
            },
            settings: Settings::Default,
        },
        Arm {
            what: "decay into denormals",
            material: Material::Decay,
            settings: Settings::Default,
        },
        // The parameter axis, on the material the other benchmarks in this crate use.
        Arm {
            what: "noise -20 dBFS",
            material: Material::Noise { amplitude: 0.1 },
            settings: Settings::Extreme,
        },
        Arm {
            what: "noise -20 dBFS",
            material: Material::Noise { amplitude: 0.1 },
            settings: Settings::Bypassed,
        },
        Arm {
            what: "220 Hz tone",
            material: Material::Sine {
                hz: 220.0,
                amplitude: 0.5,
            },
            settings: Settings::Extreme,
        },
    ];

    let reps = std::env::var(REPS_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_REPS)
        .max(MIN_REPS);
    let aligned = 2 * IR_PERIOD_BLOCKS;
    let measured_blocks = std::env::var(BLOCKS_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(MEASURED_BLOCKS, |v| (v / aligned).max(1) * aligned);
    let block_period_ns = (BLOCK_SIZE as f64 / SAMPLE_RATE_F64 * 1e9) as u64;

    println!("=== NFR-RT-040: per-block cost must not depend on content, parameters or uptime ===");
    println!(
        "48 kHz, {BLOCK_SIZE}-sample blocks, standard WaveNet, 2 s stereo IR, real six-stage chain"
    );
    println!(
        "{} arms x {reps} interleaved repetitions x {measured_blocks} measured blocks, \
         {WARMUP_BLOCKS} warmup blocks discarded per arm per repetition",
        arms.len()
    );
    println!(
        "block period (D-2.1): {block_period_ns} ns; estimator = largest per-residue minimum mod \
         {IR_PERIOD_BLOCKS} blocks\n"
    );

    let mut l = vec![0f32; BLOCK_SIZE];
    let mut r = vec![0f32; BLOCK_SIZE];
    let mut durations = Vec::with_capacity(measured_blocks);
    // One row per arm, holding that arm's repetitions.
    let mut results: Vec<Vec<Measurement>> = vec![Vec::with_capacity(reps); arms.len()];

    for rep in 1..=reps {
        for (index, arm) in arms.iter().enumerate() {
            arm.settings.apply_to(&mut chain);
            let mut state = MaterialState::new();

            for _ in 0..WARMUP_BLOCKS {
                arm.material.fill(&mut state, &mut l);
                r.copy_from_slice(&l);
                let mut ch: [&mut [f32]; 2] = [&mut l, &mut r];
                let mut io = StageIo::new(&mut ch, BLOCK_SIZE);
                chain.process(&mut io);
                std::hint::black_box(io.channel(0));
            }

            durations.clear();
            for _ in 0..measured_blocks {
                arm.material.fill(&mut state, &mut l);
                r.copy_from_slice(&l);
                let mut ch: [&mut [f32]; 2] = [&mut l, &mut r];
                let mut io = StageIo::new(&mut ch, BLOCK_SIZE);
                // Guarded, per block, exactly as `denormal_guard.rs` does it and for its reasons:
                // D-7.4 puts a `DenormalGuard` around the real audio callback, so a measurement
                // taken without one is a measurement of a configuration the product never runs in.
                // It matters most for the arms this binary exists to compare — the decaying
                // material and the gate-pinned-shut settings both drive subnormals into every
                // filter state in the chain, and an unguarded run would report NFR-RT-030's
                // territory as an NFR-RT-040 violation. Acquisition and drop are inside the timed
                // window because a real callback's budget includes them.
                let t0 = Instant::now();
                let guard = DenormalGuard::new();
                chain.process(&mut io);
                drop(guard);
                let e = t0.elapsed();
                std::hint::black_box(io.channel(0));
                durations.push(e.as_nanos() as u64);
            }

            assert_eq!(
                chain.fault_count(),
                0,
                "arm '{}' ({}) hit FR-CHAIN-080's NaN/Inf fault path, so its blocks were silenced \
                 rather than processed and its timings measure nothing",
                arm.what,
                arm.settings.label()
            );

            let m = analyse(&durations, block_period_ns);
            println!(
                "rep {rep}/{reps}  {:<22} {:<16} p50 {:>6.2}%  p99.9 {:>6.2}%  estimator \
                 {:>6.2}%  (halves {:>6.2}% / {:>6.2}%){}",
                arm.what,
                arm.settings.label(),
                m.p50,
                m.p999,
                m.estimator,
                m.first_half,
                m.second_half,
                if m.p999_is_quotable() {
                    ""
                } else {
                    "  p99.9 DISCARDED (D-2.4)"
                }
            );
            results[index].push(m);
        }
        println!();
    }

    verdict(&arms, &results);
}

/// Every assertion this binary makes, in one place — the same separation `six_stage_chain.rs` uses,
/// so there is exactly one place to read for what is enforced.
fn verdict(arms: &[Arm], results: &[Vec<Measurement>]) {
    // The estimator can only be pushed *up* by interference, so the smallest of an arm's
    // repetitions is its least-contaminated view of its own worst-case block. Comparing each arm's
    // best is comparing like with like.
    let best: Vec<f64> = results
        .iter()
        .map(|reps| {
            reps.iter()
                .map(|m| m.estimator)
                .fold(f64::INFINITY, f64::min)
        })
        .collect();

    let cheapest = best.iter().copied().fold(f64::INFINITY, f64::min);
    let dearest = best.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let spread_pct = (dearest / cheapest - 1.0) * 100.0;

    println!("--- per arm, best (least-contaminated) estimator of its repetitions ---");
    for (arm, &value) in arms.iter().zip(best.iter()) {
        println!(
            "  {:<22} {:<16} {value:>6.2}%  ({:+.1}% vs the cheapest arm)",
            arm.what,
            arm.settings.label(),
            (value / cheapest - 1.0) * 100.0
        );
    }
    println!(
        "\npart 1 -- content and parameter invariance: spread {spread_pct:.1}% \
         (tolerance {INVARIANCE_TOLERANCE_PCT:.0}%)"
    );

    // The uptime axis: within each arm, each repetition's own two halves.
    let mut worst_drift = 0.0f64;
    let mut worst_drift_arm = "";
    for (arm, reps) in arms.iter().zip(results.iter()) {
        for m in reps {
            let drift = (m.second_half / m.first_half - 1.0) * 100.0;
            if drift > worst_drift {
                worst_drift = drift;
                worst_drift_arm = arm.what;
            }
        }
    }
    println!(
        "part 2 -- uptime invariance: worst second-half-vs-first-half drift {worst_drift:+.1}% \
         (arm '{worst_drift_arm}', tolerance {INVARIANCE_TOLERANCE_PCT:.0}%)"
    );

    // The requirement names p99.9, so it is compared too — over the arms D-2.4 permits quoting.
    let quotable: Vec<(usize, f64)> = results
        .iter()
        .enumerate()
        .filter_map(|(i, reps)| {
            reps.iter()
                .filter(|m| m.p999_is_quotable())
                .map(|m| m.p999)
                .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a| a.min(v))))
                .map(|v| (i, v))
        })
        .collect();
    let p999_spread = if quotable.len() >= 2 {
        let lo = quotable
            .iter()
            .map(|&(_, v)| v)
            .fold(f64::INFINITY, f64::min);
        let hi = quotable
            .iter()
            .map(|&(_, v)| v)
            .fold(f64::NEG_INFINITY, f64::max);
        Some((hi / lo - 1.0) * 100.0)
    } else {
        None
    };
    match p999_spread {
        Some(s) => println!(
            "part 3 -- the requirement's own p99.9, across the {} of {} arms D-2.4 permits \
             quoting: spread {s:.1}% (tolerance {INVARIANCE_TOLERANCE_PCT:.0}%)",
            quotable.len(),
            arms.len()
        ),
        None => println!(
            "part 3 -- the requirement's own p99.9: NOT ASSERTED. Fewer than two arms passed \
             D-2.4's validity check (raw p99.9 within {VALIDITY_MARGIN_PCT:.0} points of its own \
             estimator), and a figure D-2.4 says must be discarded is not one to compare. The \
             machine was not quiet; parts 1 and 2 still apply."
        ),
    }

    if std::env::var(INFORMATIONAL_ENV).is_ok() {
        println!("\nINFORMATIONAL ({INFORMATIONAL_ENV} is set) -- NOTHING ASSERTED.");
        return;
    }

    assert!(
        spread_pct <= INVARIANCE_TOLERANCE_PCT,
        "NFR-RT-040 (part 1): the chain's worst-case per-block cost spans {spread_pct:.1}% across \
         the arms, over the {INVARIANCE_TOLERANCE_PCT:.0}% tolerance. This statistic is a \
         per-residue MINIMUM, so background load cannot explain it away: the chain's cost depends \
         on its audio content or on its parameter values, which is exactly what this requirement \
         forbids. The per-arm table above says which condition is the expensive one."
    );
    assert!(
        worst_drift <= INVARIANCE_TOLERANCE_PCT,
        "NFR-RT-040 (part 2): arm '{worst_drift_arm}' cost {worst_drift:+.1}% more in the second \
         half of its run than in the first, over the {INVARIANCE_TOLERANCE_PCT:.0}% tolerance -- \
         the chain gets dearer the longer it runs"
    );
    if let Some(s) = p999_spread {
        assert!(
            s <= INVARIANCE_TOLERANCE_PCT,
            "NFR-RT-040 (part 3): the requirement's own 99.9th percentile spans {s:.1}% across the \
             arms D-2.4 permits quoting, over the {INVARIANCE_TOLERANCE_PCT:.0}% tolerance"
        );
    }

    println!("\nPASS: NFR-RT-040 holds across every arm measured.");
}
