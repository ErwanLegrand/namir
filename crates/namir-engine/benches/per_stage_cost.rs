//! Per-stage cost isolation for NFR-PERF-010's own literal condition — the measurement
//! `benches/six_stage_chain.rs` cannot make.
//!
//! # Why this exists alongside `six_stage_chain.rs`
//!
//! `six_stage_chain.rs` is M3's *exit criterion*: it measures the assembled chain, which is the
//! only figure NFR-PERF-010 actually gates on. It deliberately cannot attribute that figure to
//! individual stages. As of this milestone's close-out, that left a real gap in the record: the
//! Nam stage had been measured alone (`namir-nam/benches/wavenet_inner_loops.rs`, R-4) and the Ir
//! stage had been measured alone (`namir-ir/examples/perf_bench.rs`, R-8), but **gate, trim, eq
//! and out never had been** — `03-implementation-roadmap.md` §7's own accounting had to describe
//! their contribution as "unmeasured-in-isolation, but nonzero", i.e. a guess, which is exactly
//! what D-2.1/D-2.2's methodology exists to refuse.
//!
//! This binary closes that gap: it measures each of the six stages' own `Stage::process` cost
//! separately, under the same conditions and the same D-2.1/D-2.2 methodology
//! `six_stage_chain.rs` uses, so the chain's figure can be attributed rather than speculated
//! about. Its output is diagnostic — it gates nothing, and NFR-PERF-010 continues to be judged
//! solely on `six_stage_chain.rs`'s assembled number.
//!
//! # What the per-stage numbers do and do not mean
//!
//! **Decision:** report each stage's cost measured in isolation, and do *not* expect the six
//! results to sum to the assembled chain's figure.
//!
//! **Rationale:** they cannot, for two independent reasons, both worth stating so a reader
//! doesn't treat a discrepancy as a defect. First, percentiles are not additive: the chain's
//! p99.9 block is the block whose *total* cost is worst, which is generally not the block where
//! any individual stage hit its own worst case, so summing six p99.9 figures systematically
//! overestimates. (Summing six *p50* figures is the better-behaved comparison, and is the one to
//! reach for when attributing the chain's typical-block cost.) Second, a stage measured alone
//! gets a warm cache and no competition for it; the same stage inside the chain runs after five
//! others have evicted much of what it wants, so its real in-chain cost is generally somewhat
//! higher than its isolated cost. Both effects push the same way for p50 (isolated sum <= chain)
//! and opposite ways for p99.9, which is why only the p50 comparison is interpreted below.
//!
//! **Consequence:** this binary prints the six stages' figures, their p50 sum, and the p50 sum as
//! a fraction of the block period — the last being the number worth comparing against
//! `six_stage_chain.rs`'s own p50. It does not print a pass/fail verdict for any stage, because
//! no requirement assigns any individual stage its own budget (NFR-PERF-010's 25% is for the
//! whole instance).
//!
//! # Conditions
//!
//! Identical to `six_stage_chain.rs`'s, deliberately, so the two are directly comparable: 48 kHz,
//! 64-sample blocks, stereo, a real generated "standard" WaveNet loaded through `namir_nam::load`,
//! a real generated 2 s stereo IR loaded through `PreparedIr::from_wav_bytes`, gate and EQ both
//! engaged with the same non-default values. Single-core-pinned, 5,000 warmup blocks discarded,
//! 100,000 measured blocks (D-2.2). See `six_stage_chain.rs`'s doc comment for why each of those
//! choices is what it is — this file follows it rather than re-deciding.
//!
//! Every stage is driven with the same freshly-generated input block each iteration, rather than
//! with whatever the previous stage in a chain would have produced. That is the point of an
//! isolation measurement (each stage's cost is measured against one fixed, known stimulus, not
//! against a signal whose character depends on five other stages' current parameter values), and
//! it is sound for every stage here because none of their per-block costs are signal-dependent:
//! the gate runs identical envelope/smoothing arithmetic whether open or closed, the biquads and
//! gain ramps are unconditional per-sample arithmetic, and both the WaveNet and the convolver do
//! a fixed amount of work per block determined entirely by their loaded resource's shape (see
//! `namir-ir/examples/perf_bench.rs`'s own note that convolution cost follows tap *count*, not
//! tap values). The one caveat this creates is recorded in `gen_block`'s own comment.

use std::sync::Arc;
use std::time::Instant;

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::stages::eq::{EqPrep, EqStage};
use namir_engine::stages::gate::{GatePrep, GateStage};
use namir_engine::stages::ir::{IrPrep, IrStage};
use namir_engine::stages::nam::{NamPrep, NamStage};
use namir_engine::stages::out::OutPrep;
use namir_engine::stages::trim::TrimPrep;
use namir_engine::{ParamChange, ParamId, PrepareContext, Stage, StageIo, StagePrep};
use namir_fixtures::ir::decaying_noise;
use namir_fixtures::nam::{WaveNetShape, generate};
use namir_params::stages::{eq, gate};

const BLOCK_SIZE: usize = 64;
const SAMPLE_RATE_HZ: u32 = 48_000;
const SAMPLE_RATE_F64: f64 = 48_000.0;
const WARMUP_BLOCKS: usize = 5_000;
const MEASURED_BLOCKS: usize = 100_000; // >= 100,000 per D-2.2

// Same fixture seeds and shapes as `six_stage_chain.rs`, so the two binaries measure the same
// resources rather than merely similar ones.
const NAM_SEED: u64 = 0xC0FF_EE01;
const IR_SEED_LEFT: u64 = 0xBEEF_0001;
const IR_SEED_RIGHT: u64 = 0xBEEF_0002;
const IR_LEN_SAMPLES: usize = 2 * SAMPLE_RATE_HZ as usize;
const IR_DECAY_TAU_SAMPLES: f64 = 8_000.0;
const GATE_THRESHOLD_DB: f32 = -40.0;
const EQ_LOW_SHELF_GAIN_DB: f32 = 6.0;

fn percentile(sorted_nanos: &[u64], p: f64) -> u64 {
    let idx = ((sorted_nanos.len() as f64 - 1.0) * p).round() as usize;
    sorted_nanos[idx]
}

/// Same seeded xorshift64* driving signal as `six_stage_chain.rs`'s, at the same ~-20 dBFS level
/// (comfortably above `GATE_THRESHOLD_DB`, so the gate measured here is the open gate the chain
/// benchmark also measures).
///
/// The one consequence of driving every stage with this same raw signal rather than its real
/// in-chain input: the Ir stage here convolves the raw noise rather than the Nam stage's output.
/// That changes the *values* flowing through the convolver, not the amount of arithmetic it does
/// (see the module doc comment), so it does not affect what is being measured — but it does mean
/// these numbers describe each stage's cost, not a simulation of the chain running.
fn gen_block(x: &mut u64, out: &mut [f32]) {
    for s in out.iter_mut() {
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        let noise = ((*x % 2_000_003) as f32 / 1_000_001.5) - 1.0; // roughly [-1, 1)
        *s = 0.1 * noise;
    }
}

/// Same helper as `six_stage_chain.rs`'s, duplicated for the same reason its own comment gives
/// (no shared path between two `benches/` binaries).
fn write_stereo_wav(sample_rate: u32, left: &[f32], right: &[f32]) -> Vec<u8> {
    assert_eq!(left.len(), right.len());
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut buf = Vec::new();
    {
        let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
        for (&l, &r) in left.iter().zip(right.iter()) {
            writer.write_sample(l).unwrap();
            writer.write_sample(r).unwrap();
        }
        writer.finalize().unwrap();
    }
    buf
}

fn stereo_ir_wav_bytes() -> Vec<u8> {
    let left = decaying_noise(IR_LEN_SAMPLES, IR_SEED_LEFT, IR_DECAY_TAU_SAMPLES);
    let right = decaying_noise(IR_LEN_SAMPLES, IR_SEED_RIGHT, IR_DECAY_TAU_SAMPLES);
    write_stereo_wav(SAMPLE_RATE_HZ, &left, &right)
}

/// One stage's measured cost distribution, in nanoseconds per block.
struct StageResult {
    name: &'static str,
    p50: u64,
    p99: u64,
    p999: u64,
    max: u64,
}

/// Runs the standard warmup-then-measure loop against one stage and returns its distribution.
/// Takes `&mut dyn Stage` rather than a generic parameter so the six concrete stage types can be
/// measured through one code path without monomorphizing six copies of the loop — the dynamic
/// dispatch this adds is the same `Chain::process` itself pays for every stage on every block
/// (`Chain` owns `Vec<Box<dyn Stage>>`, D-6.1), so it is representative rather than an artifact.
fn measure(name: &'static str, stage: &mut dyn Stage, rng_state: &mut u64) -> StageResult {
    let mut left = vec![0f32; BLOCK_SIZE];
    let mut right = vec![0f32; BLOCK_SIZE];

    for _ in 0..WARMUP_BLOCKS {
        gen_block(rng_state, &mut left);
        right.copy_from_slice(&left);
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, BLOCK_SIZE);
        stage.process(&mut io);
        std::hint::black_box(io.channel(0));
    }

    let mut durations_ns = Vec::with_capacity(MEASURED_BLOCKS);
    for _ in 0..MEASURED_BLOCKS {
        gen_block(rng_state, &mut left);
        right.copy_from_slice(&left);
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, BLOCK_SIZE);
        let start = Instant::now();
        stage.process(&mut io);
        let elapsed = start.elapsed();
        std::hint::black_box(io.channel(0));
        durations_ns.push(elapsed.as_nanos() as u64);
    }

    durations_ns.sort_unstable();
    StageResult {
        name,
        p50: percentile(&durations_ns, 0.50),
        p99: percentile(&durations_ns, 0.99),
        p999: percentile(&durations_ns, 0.999),
        max: *durations_ns.last().unwrap(),
    }
}

/// Pins this thread to one core (D-2.1), **deliberately not core 0**.
///
/// # Why not core 0 — measured, not assumed
///
/// Every benchmark in this workspace originally pinned to `get_core_ids().next()`, i.e. logical
/// CPU 0. An elevated `xperf -on Latency` trace on the `docs/02-architecture.md` §2 reference
/// machine showed why that was the worst possible choice: `dxgkrnl.sys` (the DirectX/GPU kernel
/// driver) issues **6,494 interrupts of 128-512 µs** over a 39.4 s trace — about 165 per second —
/// and its ISR time lands on **CPU 0 exclusively** (1,670,068 µs on CPU 0, exactly 0 µs on all 31
/// other logical CPUs; a steady ~4.2% of CPU 0's wall clock, every second).
///
/// ISRs execute at DIRQL, above every thread priority, which is why raising the process to
/// Windows `High` priority changed nothing when that was tried. Pinned to CPU 0, the IR stage
/// measured p99.9 = 258 µs; on any other core the same binary measures 55 µs — a 4.7x difference
/// entirely attributable to the GPU driver, and on a clean core p99 (51.6 µs) and p99.9 (55.0 µs)
/// converge, which is the tight schedule-bounded distribution the cost model predicted all along.
///
/// The same trace shows CPU 0 is not the only contaminated core: in the **DPC** table
/// `ntoskrnl.exe` accumulates 50,151 µs on **CPU 2** — the highest of any core, a steady
/// ~1,500-2,100 µs every second — because Windows routes ISRs and much of its DPC load to
/// *different* cores. Measured on the chain benchmark in one interleaved sequence: core 0 gives
/// p99.9 ~34.8%, core 2 gives 24.4-25.2%, cores 4/8/12 give ~17-24%. The *ordering* (0 worst,
/// 2 next, 4+ best) is reproducible and corroborated by the trace above. The *absolute* figure is
/// **not** yet stable: one session measured cores 4/8/12 at a tight 16.5-18.1% across twelve runs,
/// a later session measured the same cores at ~24% across nine runs -- machine quiet, no ETW
/// session active, same binary, in both. That ~40% session-to-session shift is unexplained, and is
/// why no NFR-PERF-010 pass/fail verdict should be read off these numbers yet: only same-session,
/// interleaved comparisons are currently trustworthy on this machine.
///
/// So core 0 measurements were not measuring Namir. This defaults to **index 4**, the first core
/// the trace shows clean of both the GPU ISR load and the kernel DPC load; override with
/// `NAMIR_PIN_CORE=<index>` to reproduce the contaminated figures or to probe a specific core.
/// Index is clamped into range, so this is safe on machines with few cores.
///
/// **This is a measurement fix, not a product fix.** A real audio callback that happens to be
/// scheduled onto CPU 0 on a machine like this would suffer the same 128-512 µs stalls. Giving the
/// audio thread sensible affinity/priority is `namir-platform`'s job and is tracked as M6 work.
fn pin_to_measurement_core() {
    let Some(ids) = core_affinity::get_core_ids() else {
        return;
    };
    if ids.is_empty() {
        return;
    }
    // Default index 4: clean of both CPU 0's GPU ISRs and CPU 2's kernel DPC load per the trace
    // described above.
    //
    // Even indices are NOT required, contrary to an earlier revision of this comment which
    // reasoned that on an SMT machine whose logical CPUs pair as (0,1), (2,3), ... an odd index
    // is a sibling sharing execution resources. Measured, that reasoning is inert while the
    // sibling is idle: the contamination-immune estimator returns 15.26 / 15.32 / 15.53 / 15.39 /
    // 15.30 / 15.23% on cores 4 / 5 / 8 / 9 / 12 / 13 respectively -- odd and even
    // indistinguishable. It would matter only if something were actually scheduled on the paired
    // thread, which is a reason to prefer an idle core, not specifically an even one.
    let idx = std::env::var("NAMIR_PIN_CORE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4)
        .min(ids.len() - 1);
    core_affinity::set_for_current(ids[idx]);
}

fn main() {
    // D-2.1: single-core-pinned, same as every other benchmark in this workspace.
    pin_to_measurement_core();

    let sample_rate = SampleRate::new(SAMPLE_RATE_HZ).expect("48 kHz is a valid SampleRate");
    let ctx = PrepareContext::new(sample_rate, BLOCK_SIZE, ChannelConfig::Stereo)
        .expect("BLOCK_SIZE is nonzero");

    let mut gate_stage: GateStage = GatePrep.prepare(&ctx).expect("GatePrep::prepare");
    let mut trim_stage = TrimPrep.prepare(&ctx).expect("TrimPrep::prepare");
    let mut nam_stage: NamStage = NamPrep.prepare(&ctx).expect("NamPrep::prepare");
    let mut ir_stage: IrStage = IrPrep.prepare(&ctx).expect("IrPrep::prepare");
    let mut eq_stage: EqStage = EqPrep.prepare(&ctx).expect("EqPrep::prepare");
    let mut out_stage = OutPrep.prepare(&ctx).expect("OutPrep::prepare");

    let nam_model = generate(WaveNetShape::Standard, NAM_SEED)
        .expect("standard WaveNet fixture should generate");
    let nam_bytes = nam_model.to_json_bytes();
    let prepared_nam =
        Arc::new(namir_nam::load(&nam_bytes).expect("generated WaveNet fixture should load"));
    nam_stage.load_model(prepared_nam);

    let ir_bytes = stereo_ir_wav_bytes();
    let prepared_ir = Arc::new(
        namir_ir::PreparedIr::from_wav_bytes(&ir_bytes, sample_rate, BLOCK_SIZE)
            .expect("generated stereo IR should load"),
    );
    assert_eq!(
        prepared_ir.channel_count(),
        2,
        "IR fixture must be genuinely stereo for NFR-PERF-010's own literal condition"
    );
    ir_stage.load_ir(prepared_ir);

    // Same activation as `six_stage_chain.rs`: explicit enable plus one real non-default value
    // each, so gate and EQ do real work rather than running at identity.
    gate_stage.apply(ParamChange {
        id: ParamId(gate::ENABLED.id.0),
        value: 1.0,
    });
    gate_stage.apply(ParamChange {
        id: ParamId(gate::THRESHOLD_DB.id.0),
        value: GATE_THRESHOLD_DB,
    });
    eq_stage.apply(ParamChange {
        id: ParamId(eq::ENABLED.id.0),
        value: 1.0,
    });
    eq_stage.apply(ParamChange {
        id: ParamId(eq::LOW_SHELF_GAIN_DB.id.0),
        value: EQ_LOW_SHELF_GAIN_DB,
    });

    let mut rng_state = 0xC0DE_CAFEu64 ^ 0x9E37_79B9_7F4A_7C15;

    // Measured in the chain's own runtime order (D-9.8: gate before trim), purely so the output
    // table reads in the same order as `six_stage_chain.rs`'s description of the chain.
    let results = vec![
        measure("gate", &mut gate_stage, &mut rng_state),
        measure("trim", &mut trim_stage, &mut rng_state),
        measure("nam", &mut nam_stage, &mut rng_state),
        measure("ir", &mut ir_stage, &mut rng_state),
        measure("eq", &mut eq_stage, &mut rng_state),
        measure("out", &mut out_stage, &mut rng_state),
    ];

    let block_period_ns = (BLOCK_SIZE as f64 / SAMPLE_RATE_F64 * 1e9) as u64;
    let pct = |v: u64| v as f64 / block_period_ns as f64 * 100.0;

    println!("=== NFR-PERF-010 attribution: each stage measured ALONE ===");
    println!(
        "48 kHz, {BLOCK_SIZE}-sample blocks, stereo, standard WaveNet, 2 s stereo IR, gate + EQ \
         active"
    );
    println!("blocks measured per stage: {MEASURED_BLOCKS} (warmup {WARMUP_BLOCKS} discarded)");
    println!(
        "block period (D-2.1): {block_period_ns} ns ({:.4} ms)",
        block_period_ns as f64 / 1e6
    );
    println!(
        "DIAGNOSTIC ONLY -- gates nothing; NFR-PERF-010 is judged solely on six_stage_chain.rs."
    );
    println!();
    println!("stage      p50            p99            p99.9          max");
    for r in &results {
        println!(
            "{:<10} {:>6.2}% {:>6}  {:>6.2}% {:>6}  {:>6.2}% {:>6}  {:>6.2}% {:>6}",
            r.name,
            pct(r.p50),
            r.p50,
            pct(r.p99),
            r.p99,
            pct(r.p999),
            r.p999,
            pct(r.max),
            r.max
        );
    }

    let p50_sum: u64 = results.iter().map(|r| r.p50).sum();
    let p999_sum: u64 = results.iter().map(|r| r.p999).sum();
    println!();
    println!(
        "p50 sum across the six stages: {p50_sum} ns = {:.2}% of the block period",
        pct(p50_sum)
    );
    println!(
        "  (compare against six_stage_chain.rs's own p50 -- this is the meaningful comparison; \
         see this file's doc comment)"
    );
    println!(
        "p99.9 sum across the six stages: {p999_sum} ns = {:.2}% -- NOT comparable to the chain's \
         p99.9 (percentiles are not additive; stated only to make that non-additivity visible)",
        pct(p999_sum)
    );

    // Rank by p50 contribution: the actionable output of this binary is "where does the typical
    // block's time actually go", which is what an optimization effort should be aimed at.
    let mut ranked: Vec<&StageResult> = results.iter().collect();
    ranked.sort_by_key(|r| std::cmp::Reverse(r.p50));
    println!();
    println!("p50 cost share (typical block), most expensive first:");
    for r in &ranked {
        let share = if p50_sum > 0 {
            r.p50 as f64 / p50_sum as f64 * 100.0
        } else {
            0.0
        };
        println!("  {:<6} {share:>5.1}% of the six stages' p50 sum", r.name);
    }
}
