//! Diagnostic: is the assembled chain's p99.9 tail *caused by this code*, or by the environment
//! it is measured in?
//!
//! # Why this binary exists
//!
//! M3's close-out repeatedly hit the same wall: interventions that provably reduce work did not
//! move `six_stage_chain.rs`'s p99.9. Vectorizing the IR head convolution, fixing `build_schedule`'s
//! cross-size phase alignment (worst-block modelled FFT load 11.9x -> 6.8x the mean), and sweeping
//! `max_partition` from 8192 down to 2048 each left the measured IR p99.9 essentially unchanged,
//! while `p50` moved exactly as predicted every time. Meanwhile repeated identical runs of the
//! chain benchmark drift (43% -> 45% -> 49% p99.9) while their p50 holds stable to three
//! significant figures.
//!
//! That pattern is consistent with a large share of the measured tail being **environmental**
//! (scheduler preemption, SMT-sibling occupancy, memory-system contention from other processes)
//! rather than a property of the DSP. If that is true, further optimisation aimed at p99.9 is
//! aimed at the wrong target, and NFR-PERF-010's gate needs to be measured differently rather than
//! engineered against. Before spending more effort either way, this binary settles which it is.
//!
//! # The three discriminators, and what each outcome means
//!
//! All three are computed from the *same* run, over the per-block durations recorded **in
//! acquisition order** (every other benchmark in this workspace sorts durations immediately and
//! so destroys exactly the temporal information needed here).
//!
//! 1. **Periodicity (the sharpest test, and specific to this codebase).** Every FFT partition in
//!    `namir-ir`'s schedule fires on a fixed cycle, so schedule-driven cost is *strictly periodic*
//!    in the host-block index with period `largest_partition / block_size` -- 128 blocks at this
//!    condition. If the tail is the FFT pileup, the slow blocks' indices must concentrate on a few
//!    residues mod 128. If it is environmental, those residues are uniform, because nothing in the
//!    OS knows or cares about the convolution schedule. This test cleanly separates the two
//!    hypotheses in a way neither autocorrelation nor run-length can, and it is the reason this
//!    binary reports residues at all.
//! 2. **Run length.** A contending thread or a thermal excursion lasts milliseconds -- tens to
//!    thousands of consecutive 64-sample blocks. Environmental slowness therefore arrives in
//!    *contiguous runs*. Code-driven slowness at a 1-in-100 rate arrives as isolated singletons
//!    (mean run length ~= 1.01, the geometric expectation).
//! 3. **Lag-1 autocorrelation.** The same distinction, as a single scalar: the driving signal is
//!    fresh xorshift noise with no block-to-block correlation, so any strong positive
//!    autocorrelation in *duration* comes from something with memory that is not the input --
//!    i.e. the environment.
//!
//! Reported together because any one alone is arguable and the three together are not: uniform
//! residues + long runs + high autocorrelation is environmental beyond reasonable dispute, and
//! concentrated residues + singleton runs + ~zero autocorrelation is the code's own cost just as
//! firmly.
//!
//! # Result of the first run (M3 close-out, reference machine, quiet, Best Performance)
//!
//! **The environmental hypothesis this binary was written to test is refuted.** All three
//! discriminators agreed, and all three point at the code:
//!
//! - **Run length: 1005 runs, mean 1.00, longest 1.** Not a single pair of consecutive slow
//!   blocks in 100,000. Sustained contention cannot produce that; it is the strongest single
//!   result here.
//! - **Lag-1 autocorrelation: 0.0945.** Effectively independent blocks.
//! - **Residues mod 128: chi2 = 1950** against ~128 for uniform -- strongly periodic, locked to
//!   the IR schedule's own period.
//!
//! The duration histogram independently confirms it by shape: the distribution is not a smooth
//! tail but a set of **discrete modes** (63,932 blocks at 120-140 us, then distinct populations of
//! 6,288 at 260-280 us and 1,642 at 400-420 us). Quantised cost levels are what a fixed partition
//! schedule produces and are not something scheduler noise imitates.
//!
//! **The open contradiction, recorded rather than smoothed over.** This result sits in unresolved
//! tension with two other measurements from the same session: dropping `max_partition` from 8192
//! to 2048 did not move the IR stage's p99.9 (23.42% -> 23.43%), and `build_schedule`'s
//! cross-size decorrelation fix (worst-block modelled FFT load 11.9x -> 6.8x the mean) did not
//! move it either. A tail that is demonstrably schedule-periodic ought to respond to both. It
//! does not, so the `2P*log2(2P)` cost model used to predict those effects (see
//! `namir-ir`'s `no_single_host_block_carries_a_disproportionate_share_of_fft_work`) is
//! incomplete -- most likely it mismodels either real FFT constant factors at small sizes or the
//! per-sample, per-partition bookkeeping that scales with partition *count* rather than size.
//! Resolving that is prerequisite to any further IR tail work: the next optimisation should not
//! be designed against a model already known to mispredict.
//!
//! Conditions, fixtures and chain assembly are identical to `six_stage_chain.rs` -- see that
//! file's doc comment for why each is what it is. This binary measures the same thing; it only
//! keeps more of the data.

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
use namir_params::stages::{eq, gate};

const BLOCK_SIZE: usize = 64;
const SAMPLE_RATE_HZ: u32 = 48_000;
const SAMPLE_RATE_F64: f64 = 48_000.0;
const WARMUP_BLOCKS: usize = 5_000;
const MEASURED_BLOCKS: usize = 100_000;

const NAM_SEED: u64 = 0xC0FF_EE01;
const IR_SEED_LEFT: u64 = 0xBEEF_0001;
const IR_SEED_RIGHT: u64 = 0xBEEF_0002;
const IR_LEN_SAMPLES: usize = 2 * SAMPLE_RATE_HZ as usize;
const IR_DECAY_TAU_SAMPLES: f64 = 8_000.0;
const GATE_THRESHOLD_DB: f32 = -40.0;
const EQ_LOW_SHELF_GAIN_DB: f32 = 6.0;

/// The IR schedule's own period in host blocks at this condition: `DEFAULT_MAX_PARTITION / 64`.
/// Hard-coded rather than derived from `namir_ir::build_schedule` so this diagnostic stays a
/// passive observer of the schedule rather than a second consumer of it; asserted against the
/// real schedule below so it cannot silently drift.
const IR_PERIOD_BLOCKS: usize = 8192 / BLOCK_SIZE;

fn percentile(sorted: &[u64], p: f64) -> u64 {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

fn gen_block(x: &mut u64, out: &mut [f32]) {
    for s in out.iter_mut() {
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        let noise = ((*x % 2_000_003) as f32 / 1_000_001.5) - 1.0;
        *s = 0.1 * noise;
    }
}

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

// trace-partial: NFR-RT-040
// uncovered: NFR-RT-040 — the binary asserts no invariance and varies neither of the
// uncovered: requirement's first two variables: all 105 000 blocks are driven with one xorshift
// uncovered: noise material at one amplitude, and the gate and EQ parameters are set once before
// uncovered: the measured loop and never varied within it, so "shall not depend on audio content
// uncovered: or parameter values" is untested and the residue analysis is reported rather than
// uncovered: asserted; closes M8
fn main() {
    pin_to_measurement_core();

    let sample_rate = SampleRate::new(SAMPLE_RATE_HZ).expect("48 kHz is valid");
    let ctx = PrepareContext::new(sample_rate, BLOCK_SIZE, ChannelConfig::Stereo)
        .expect("BLOCK_SIZE is nonzero");

    let mut gate_stage: GateStage = GatePrep.prepare(&ctx).expect("gate");
    let trim_stage = TrimPrep.prepare(&ctx).expect("trim");
    let mut nam_stage: NamStage = NamPrep.prepare(&ctx).expect("nam");
    let mut ir_stage: IrStage = IrPrep.prepare(&ctx).expect("ir");
    let mut eq_stage: EqStage = EqPrep.prepare(&ctx).expect("eq");
    let out_stage = OutPrep.prepare(&ctx).expect("out");

    let model = generate(WaveNetShape::Standard, NAM_SEED).expect("fixture");
    let nam_bytes = model.to_json_bytes();
    nam_stage.load_model(Arc::new(namir_nam::load(&nam_bytes).expect("load")));

    let left = decaying_noise(IR_LEN_SAMPLES, IR_SEED_LEFT, IR_DECAY_TAU_SAMPLES);
    let right = decaying_noise(IR_LEN_SAMPLES, IR_SEED_RIGHT, IR_DECAY_TAU_SAMPLES);
    let ir_bytes = write_stereo_wav(SAMPLE_RATE_HZ, &left, &right);
    let prepared_ir = Arc::new(
        namir_ir::PreparedIr::from_wav_bytes(&ir_bytes, sample_rate, BLOCK_SIZE).expect("ir load"),
    );
    ir_stage.load_ir(prepared_ir);

    // Confirm IR_PERIOD_BLOCKS against the real schedule rather than trusting the constant.
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
        "IR_PERIOD_BLOCKS is stale relative to the real schedule"
    );

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

    let mut l = vec![0f32; BLOCK_SIZE];
    let mut r = vec![0f32; BLOCK_SIZE];
    let mut rng = 0xC0DE_CAFEu64 ^ 0x9E37_79B9_7F4A_7C15;

    for _ in 0..WARMUP_BLOCKS {
        gen_block(&mut rng, &mut l);
        r.copy_from_slice(&l);
        let mut ch: [&mut [f32]; 2] = [&mut l, &mut r];
        let mut io = StageIo::new(&mut ch, BLOCK_SIZE);
        chain.process(&mut io);
        std::hint::black_box(io.channel(0));
    }

    // Durations in ACQUISITION ORDER -- deliberately not sorted until the copy below.
    let mut d = Vec::with_capacity(MEASURED_BLOCKS);
    for _ in 0..MEASURED_BLOCKS {
        gen_block(&mut rng, &mut l);
        r.copy_from_slice(&l);
        let mut ch: [&mut [f32]; 2] = [&mut l, &mut r];
        let mut io = StageIo::new(&mut ch, BLOCK_SIZE);
        let t0 = Instant::now();
        chain.process(&mut io);
        let e = t0.elapsed();
        std::hint::black_box(io.channel(0));
        d.push(e.as_nanos() as u64);
    }

    let block_period_ns = (BLOCK_SIZE as f64 / SAMPLE_RATE_F64 * 1e9) as u64;
    let pct = |v: u64| v as f64 / block_period_ns as f64 * 100.0;

    let mut sorted = d.clone();
    sorted.sort_unstable();
    let p50 = percentile(&sorted, 0.50);
    let p99 = percentile(&sorted, 0.99);
    let p999 = percentile(&sorted, 0.999);

    println!("=== Tail structure diagnostic: six-stage chain ===");
    println!(
        "p50 {:.2}%  p99 {:.2}%  p99.9 {:.2}%  (block period {block_period_ns} ns)",
        pct(p50),
        pct(p99),
        pct(p999)
    );

    // --- Discriminator 3: lag-1 autocorrelation over the whole series.
    let n = d.len() as f64;
    let mean = d.iter().map(|&v| v as f64).sum::<f64>() / n;
    let var = d.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / n;
    let cov = d
        .windows(2)
        .map(|w| (w[0] as f64 - mean) * (w[1] as f64 - mean))
        .sum::<f64>()
        / (n - 1.0);
    let lag1 = if var > 0.0 { cov / var } else { 0.0 };

    // --- "Slow" = at or above p99, i.e. the top ~1% of blocks.
    let slow: Vec<usize> = d
        .iter()
        .enumerate()
        .filter(|&(_, &v)| v >= p99)
        .map(|(i, _)| i)
        .collect();

    // --- Discriminator 2: contiguous run lengths among slow blocks.
    let mut runs: Vec<usize> = Vec::new();
    let mut cur = 1usize;
    for w in slow.windows(2) {
        if w[1] == w[0] + 1 {
            cur += 1;
        } else {
            runs.push(cur);
            cur = 1;
        }
    }
    if !slow.is_empty() {
        runs.push(cur);
    }
    let mean_run = runs.iter().sum::<usize>() as f64 / runs.len().max(1) as f64;
    let longest_run = runs.iter().copied().max().unwrap_or(0);

    // --- Discriminator 1: residues mod the IR schedule period.
    let mut residue = vec![0usize; IR_PERIOD_BLOCKS];
    for &i in &slow {
        residue[i % IR_PERIOD_BLOCKS] += 1;
    }
    let occupied = residue.iter().filter(|&&c| c > 0).count();
    let expected_per_residue = slow.len() as f64 / IR_PERIOD_BLOCKS as f64;
    let worst_residue = residue.iter().copied().max().unwrap_or(0);
    // Chi-square against a uniform distribution: large => concentrated (schedule-driven),
    // ~IR_PERIOD_BLOCKS => uniform (environmental).
    let chi2: f64 = residue
        .iter()
        .map(|&c| (c as f64 - expected_per_residue).powi(2) / expected_per_residue.max(1e-9))
        .sum();

    println!();
    println!("slow blocks (>= p99): {}", slow.len());
    println!(
        "  [1] residues mod {IR_PERIOD_BLOCKS}: {occupied}/{IR_PERIOD_BLOCKS} occupied, \
         busiest holds {worst_residue} (uniform would be {expected_per_residue:.1}), \
         chi2 = {chi2:.0}"
    );
    println!("      -> concentrated (chi2 >> {IR_PERIOD_BLOCKS}) = IR schedule drives the tail");
    println!("      -> uniform (chi2 ~ {IR_PERIOD_BLOCKS}) = environmental, not the schedule");
    println!(
        "  [2] contiguous runs: {} runs, mean length {mean_run:.2}, longest {longest_run}",
        runs.len()
    );
    println!(
        "      -> mean ~1.0 = isolated blocks (code); >> 1 = sustained episodes (environment)"
    );
    println!("  [3] lag-1 autocorrelation: {lag1:.4}");
    println!(
        "      -> ~0 = independent blocks (code); > 0.3 = something with memory (environment)"
    );

    // --- Contamination-immune estimate of the schedule's own worst-case block.
    //
    // The insight this milestone arrived at the hard way: interference is *additive and
    // aperiodic*, while the IR partition schedule is *periodic with period IR_PERIOD_BLOCKS*.
    // A given residue therefore recurs ~MEASURED_BLOCKS/period times, and its cheapest occurrence
    // is the one no interrupt, preemption or frequency excursion happened to land on. Since
    // nothing can make a block finish *faster* than its own arithmetic allows, the per-residue
    // MINIMUM across all periods is a lower bound on that block's cost that is also, in practice,
    // tight -- and the maximum of those minima is the schedule's true worst-case block.
    //
    // This is what makes a figure quotable on a general-purpose desktop that cannot be made
    // perfectly quiet: raw p99.9 mixes code cost with whatever the OS did during the run (and was
    // measured, on this machine, varying 17%-52% run to run with p50 pinned at ~7.8%), whereas
    // this estimator returns the same value whether the machine was quiet or busy, because the
    // busy samples are simply not the minima.
    let mut per_residue_min = vec![u64::MAX; IR_PERIOD_BLOCKS];
    let mut per_residue_n = vec![0usize; IR_PERIOD_BLOCKS];
    for (i, &v) in d.iter().enumerate() {
        let r = i % IR_PERIOD_BLOCKS;
        per_residue_min[r] = per_residue_min[r].min(v);
        per_residue_n[r] += 1;
    }
    let clean_worst = per_residue_min
        .iter()
        .copied()
        .filter(|&v| v != u64::MAX)
        .max();
    let clean_median = {
        let mut m: Vec<u64> = per_residue_min
            .iter()
            .copied()
            .filter(|&v| v != u64::MAX)
            .collect();
        m.sort_unstable();
        m.get(m.len() / 2).copied().unwrap_or(0)
    };
    println!();
    println!(
        "contamination-immune estimate (per-residue minimum over ~{} periods each):",
        per_residue_n.first().copied().unwrap_or(0)
    );
    if let Some(w) = clean_worst {
        println!(
            "  schedule's worst block:  {w} ns = {:.2}% of block period   <-- quotable figure",
            pct(w)
        );
        println!(
            "  schedule's median block: {clean_median} ns = {:.2}%",
            pct(clean_median)
        );
        println!(
            "  raw p99.9 for comparison: {:.2}% (the difference is contamination, not code)",
            pct(p999)
        );
    }

    // --- Coarse histogram, to show whether the distribution is bimodal (two tight modes) or a
    // smooth heavy tail -- the shape argument that motivated this binary.
    println!();
    println!("duration histogram (20 us bins, counts >= 10 shown):");
    let bin_ns = 20_000u64;
    let nbins = (sorted[sorted.len() - 1] / bin_ns + 1) as usize;
    let mut hist = vec![0usize; nbins.min(200)];
    for &v in &d {
        let b = (v / bin_ns) as usize;
        if b < hist.len() {
            hist[b] += 1;
        }
    }
    for (b, &c) in hist.iter().enumerate() {
        if c >= 10 {
            let lo = b as u64 * bin_ns;
            println!(
                "  {:>4}-{:>4} us ({:>5.1}% of period): {c:>6}",
                lo / 1000,
                (lo + bin_ns) / 1000,
                pct(lo)
            );
        }
    }
}
