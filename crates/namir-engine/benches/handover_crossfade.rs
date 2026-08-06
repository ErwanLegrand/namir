//! R-7: "Crossfade doubles NAM cost transiently, eating the NFR-PERF-010 budget." The risk
//! register's own mitigation is "the benchmark measures the crossfade, not just steady state
//! (D-8.1)" — and D-8.1's own consequence clause says the same thing: "NFR-PERF-010's budget must
//! therefore be met with headroom for a 2x transient, or the crossfade must be measured as part of
//! the benchmark. **The benchmark measures it.**" This is that benchmark; until M4 there was
//! nothing to measure it with, because no handover could be driven across a real ring.
//!
//! # What it measures, and why five arms
//!
//! Every arm runs NFR-PERF-010's literal condition — 48 kHz, 64-sample blocks, a standard WaveNet,
//! a 2 s stereo IR, gate and EQ engaged — identically to `six_stage_chain.rs`, with the same seeds
//! and fixtures. They differ only in what handovers are in flight:
//!
//! | Arm | Content |
//! |---|---|
//! | **A** | Steady state, no handover. Reproduces `six_stage_chain`'s condition *in this run*, so the delta is measured rather than inferred across binaries and sessions. |
//! | **B** | A NAM handover every `period` blocks. |
//! | **C** | An IR handover every `period` blocks. |
//! | **D** | Both at once — four live resources. |
//! | **E** | Both, but **serialised**: `namir-worker`'s R-7 rule applied, so the two stages' fades never coincide. |
//!
//! Arm D exists because R-7's row names only NAM, but FR-IR-060 permits an IR swap concurrently and
//! M3's close-out established the IR stage as the chain's dominant tail contributor. Measuring only
//! B would understate the worst case the risk is actually about. Arm E was added after arms A-D
//! measured D as the only over-budget condition: it is the measurement of what
//! `namir-worker`'s serialisation rule buys, and the `overlap` column is how you check the rule is
//! actually in force rather than assumed.
//!
//! # Why the handover period must divide 128
//!
//! `tail_structure.rs` established that this chain's cost is periodic with period
//! `DEFAULT_MAX_PARTITION / block_size` = 128 blocks, and the per-residue-minimum estimator D-2.4
//! mandates as a validity check depends on each residue recurring many times. A handover period
//! coprime with 128 would force the combined period to `lcm(128, p)` and collapse the estimator's
//! sample count per residue. So the swept periods are all divisors of 128, keeping the combined
//! period at 128 and ~781 occurrences per residue — the same statistical strength `tail_structure`
//! itself has.
//!
//! # Why nothing is allocated inside the measured window
//!
//! **Decision:** handover resources are *recycled* — the bench drains the return ring and re-offers
//! the very same `Resource` it just got back, rather than preparing a fresh one per swap.
//!
//! **Rationale:** at the shortest period this run performs thousands of handovers. Preparing a slot
//! per handover would allocate (that is the whole reason D-8.1 step 1 lives on a worker), and
//! preparing them all up front would need thousands of live `NamState`s. Recycling keeps the timed
//! region free of any allocation while still exercising the real drain-and-install path, and the
//! recycling itself happens outside `Instant::now()..elapsed()`.
//!
//! **Consequence:** a recycled slot carries stale internal state from its previous life — old
//! convolution ring contents, old causal-conv history. That is immaterial *here* because
//! convolution and inference cost follows tap and layer counts, not tap values (`namir-ir`'s own
//! R-8 note makes the same point), and this binary measures cost, not correctness. It would be
//! wrong for a correctness test, which is why the FR-NAM-070/FR-IR-060 tests in `engine.rs` build
//! genuinely distinct resources instead.
//!
//! # The estimator is valid for arms A and B, and **not** for arms C and D
//!
//! Recorded because the first run of this binary made it obvious and it would otherwise be quoted
//! as if it meant something. D-2.4's per-residue-minimum estimator assumes cost is periodic *in
//! block index* with period [`IR_PERIOD_BLOCKS`]. That holds when one `IrState` runs continuously,
//! because its stream-time counter advances with the block index. It stops holding as soon as IR
//! slots are swapped: each recycled slot carries its own stream position, sits out while another is
//! installed, and resumes from where it left off, so the expensive large-partition FFT triggers
//! land on *varying* residues instead of fixed ones. Every residue then has some cheap occurrences,
//! and the per-residue minimum collapses toward the cheap baseline.
//!
//! The symptom is unmistakable and is why this was caught: arm C's estimator reads **below** arm
//! A's, which is impossible for a statistic that is supposed to be a lower bound on the same
//! schedule's worst block while doing strictly more work. So:
//!
//! - **Arms A and B**: the estimator is meaningful and D-2.4's contamination check applies as
//!   written (arm B swaps only NAM slots; the IR runs continuously, so the schedule keeps its
//!   phase).
//! - **Arms C, D and E**: the estimator is **not** a valid validity check. Use arm A's, measured in the
//!   same run, to decide whether the run as a whole was contaminated, and read arms C and D's raw
//!   percentiles on that basis. Do not quote their `est` column as if it bounded anything.
//!
//! This is a limitation of applying the estimator to a deliberately aperiodic workload, not a
//! defect in the estimator — which is exactly what D-2.4 promotes it for: telling you when a
//! reading means something.
//!
//! # Read this before quoting any number from this binary
//!
//! D-2.4 governs. Every figure here must be taken with the benchmark pinned away from device-ISR
//! cores (this defaults to core 4; **never** CPU 0, which absorbs `dxgkrnl.sys`'s 128-512 us GPU
//! interrupts, nor CPU 2, which carries the heaviest kernel DPC load), on a machine *verified*
//! quiet rather than assumed so, across **at least five repetitions with the spread reported** —
//! and cross-checked against the per-residue-minimum estimator this binary prints alongside each
//! arm. **If an arm's raw p99.9 substantially exceeds its own estimator, that run was contaminated:
//! discard it, do not quote it.**
//!
//! Also: `RUSTFLAGS` *replaces* `.cargo/config.toml`'s `-C target-cpu=x86-64-v3` rather than
//! appending to it, so a shell with `RUSTFLAGS` set silently measures without AVX2 and reports a
//! NAM cost roughly 3x too high. Check it is unset before believing anything below.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::{Command, PrepareContext, RingCapacities, StageIo, build_default_chain, split};
use namir_fixtures::ir::decaying_noise;
use namir_fixtures::nam::{WaveNetShape, generate};
use namir_ir::PreparedIr;

const SAMPLE_RATE_HZ: u32 = 48_000;
const BLOCK_SIZE: usize = 64;
/// D-2.2: ">= 100 000 blocks" for a meaningful 99.9th percentile.
const MEASURED_BLOCKS: usize = 100_000;
const WARMUP_BLOCKS: usize = 5_000;
/// 1.333 ms at NFR-PERF-010's own condition (D-2.1: budgets are a fraction of the block period).
const BLOCK_PERIOD_NS: f64 = (BLOCK_SIZE as f64 / SAMPLE_RATE_HZ as f64) * 1e9;

/// The IR schedule's own period in host blocks — `DEFAULT_MAX_PARTITION / BLOCK_SIZE`, the same
/// figure `tail_structure.rs` derives and asserts against the live schedule.
const IR_PERIOD_BLOCKS: usize = 8192 / BLOCK_SIZE;

/// Divisors of [`IR_PERIOD_BLOCKS`], so the combined period stays 128 — see the module doc.
/// 16 blocks is the effective always-in-flight worst case (a 20 ms fade is 15 blocks).
const PERIODS: [usize; 4] = [16, 32, 64, 128];

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

/// See `six_stage_chain.rs`'s identical function for the full measured argument against CPU 0 and
/// CPU 2. Defaults to index 4; override with `NAMIR_PIN_CORE`.
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

/// Which resources this arm keeps swapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    Steady,
    Nam,
    Ir,
    Both,
    /// Arm D with `namir-worker`'s R-7 serialisation rule applied: the same two handover streams at
    /// the same rates, but offset by half a period so the two stages' fades never coincide.
    ///
    /// The rule itself lives in `namir-worker`, which `namir-engine` may not depend on (D-5.1), so
    /// this arm reproduces its *effect* rather than calling it: "a NAM and an IR handover are never
    /// in flight simultaneously" is exactly what a half-period offset produces, provided half a
    /// period exceeds the fade. The `overlap` column reports the measured overlap, so the claim is
    /// checked rather than assumed -- and at `period 16` it is *not* achievable (half of 16 is 8
    /// blocks against a 15-block fade), which the measurement shows rather than hides.
    BothSerialised,
}

impl Arm {
    fn label(self) -> &'static str {
        match self {
            Arm::Steady => "A steady (no handover)",
            Arm::Nam => "B NAM handover",
            Arm::Ir => "C IR handover",
            Arm::Both => "D NAM + IR handover",
            Arm::BothSerialised => "E NAM + IR serialised",
        }
    }
}

struct ArmResult {
    durations: Vec<u64>,
    /// Blocks in which a crossfade was actually in flight, as a fraction. Reported so a
    /// mis-parameterised run is caught rather than quoted.
    fade_fraction: f64,
    /// Blocks in which **both** stages were crossfading at once -- the condition M4 measured as the
    /// only one that exceeds NFR-PERF-010's budget, and the thing arm E exists to drive to zero.
    overlap_fraction: f64,
}

fn run_arm(arm: Arm, period: usize, model_bytes: &[u8], ir_bytes: &[u8]) -> ArmResult {
    let ctx = PrepareContext::new(
        SampleRate::new(SAMPLE_RATE_HZ).unwrap(),
        BLOCK_SIZE,
        ChannelConfig::Stereo,
    )
    .unwrap();

    let mut chain = build_default_chain(&ctx).unwrap();
    chain.apply(namir_engine::ParamChange {
        id: namir_engine::ParamId(namir_params::stages::gate::THRESHOLD_DB.id.0),
        value: GATE_THRESHOLD_DB,
    });
    chain.apply(namir_engine::ParamChange {
        id: namir_engine::ParamId(namir_params::stages::eq::LOW_SHELF_GAIN_DB.id.0),
        value: EQ_LOW_SHELF_GAIN_DB,
    });

    let (mut engine, mut worker) = split(
        chain,
        RingCapacities {
            commands: 64,
            retire: 64,
            telemetry: 256,
        },
    );

    let model = Arc::new(namir_nam::load(model_bytes).unwrap());
    let ir = Arc::new(
        PreparedIr::from_wav_bytes(
            ir_bytes,
            SampleRate::new(SAMPLE_RATE_HZ).unwrap(),
            BLOCK_SIZE,
        )
        .unwrap(),
    );

    // Load once so every arm — including the steady one — measures a fully-engaged chain.
    worker
        .commands
        .try_push(Command::load_nam(Arc::clone(&model), &ctx))
        .ok()
        .expect("command ring should accept the initial model");
    worker
        .commands
        .try_push(Command::load_ir(Arc::clone(&ir), &ctx))
        .ok()
        .expect("command ring should accept the initial IR");

    // A small pool of spare resources to rotate through, prepared here, outside every timed
    // region. Two of each is enough: at any instant one is installed, one is in flight or parked,
    // and the rest cycle through the return ring.
    let mut spare_nam: Vec<namir_engine::Resource> = Vec::new();
    let mut spare_ir: Vec<namir_engine::Resource> = Vec::new();
    if matches!(arm, Arm::Nam | Arm::Both | Arm::BothSerialised) {
        for _ in 0..2 {
            if let Command::Load(r) = Command::load_nam(Arc::clone(&model), &ctx) {
                spare_nam.push(r);
            }
        }
    }
    if matches!(arm, Arm::Ir | Arm::Both | Arm::BothSerialised) {
        for _ in 0..2 {
            if let Command::Load(r) = Command::load_ir(Arc::clone(&ir), &ctx) {
                spare_ir.push(r);
            }
        }
    }

    let mut left = vec![0.0f32; BLOCK_SIZE];
    let mut right = vec![0.0f32; BLOCK_SIZE];
    let mut rng = 0x1234_5678_9ABC_DEF0u64;
    let mut durations = Vec::with_capacity(MEASURED_BLOCKS);
    let mut fade_blocks = 0usize;

    let mut telemetry_out = [namir_engine::TelemetryEntry { id: 0, value: 0.0 }; 256];
    let nam_fade_id = namir_params::ParamId::from_key("telemetry.nam.handover_active").0;
    let ir_fade_id = namir_params::ParamId::from_key("telemetry.ir.handover_active").0;
    let mut overlap_blocks = 0usize;

    for b in 0..(WARMUP_BLOCKS + MEASURED_BLOCKS) {
        // --- outside the timed window: recycle retirements and queue the next handover.
        while let Some(resource) = worker.retire.try_pop() {
            match resource.kind() {
                namir_engine::ResourceKind::Nam => spare_nam.push(resource),
                namir_engine::ResourceKind::Ir => spare_ir.push(resource),
            }
        }
        let nam_due = matches!(arm, Arm::Nam | Arm::Both | Arm::BothSerialised) && b % period == 0;
        let ir_due = match arm {
            Arm::Ir | Arm::Both => b % period == 0,
            // Half a period out of phase with the NAM stream -- the serialisation rule's effect.
            Arm::BothSerialised => b % period == period / 2,
            _ => false,
        };
        if nam_due && let Some(resource) = spare_nam.pop() {
            let _ = worker.commands.try_push(Command::Load(resource));
        }
        if ir_due && let Some(resource) = spare_ir.pop() {
            let _ = worker.commands.try_push(Command::Load(resource));
        }

        gen_block(&mut rng, &mut left);
        right.copy_from_slice(&left);

        let elapsed = {
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut io = StageIo::new(&mut channels, BLOCK_SIZE);
            let start = Instant::now();
            engine.process(&mut io);
            let elapsed = start.elapsed().as_nanos() as u64;
            black_box(io.channel(0)[0]);
            elapsed
        };

        if b >= WARMUP_BLOCKS {
            durations.push(elapsed);
            let drain = worker.telemetry.drain(&mut telemetry_out);
            let seen = &telemetry_out[..drain.read];
            let nam_fading = seen.iter().any(|e| e.id == nam_fade_id && e.value > 0.5);
            let ir_fading = seen.iter().any(|e| e.id == ir_fade_id && e.value > 0.5);
            if nam_fading || ir_fading {
                fade_blocks += 1;
            }
            if nam_fading && ir_fading {
                overlap_blocks += 1;
            }
        } else {
            let _ = worker.telemetry.drain(&mut telemetry_out);
        }
    }

    let fade_fraction = fade_blocks as f64 / MEASURED_BLOCKS as f64;
    let overlap_fraction = overlap_blocks as f64 / MEASURED_BLOCKS as f64;
    ArmResult {
        durations,
        fade_fraction,
        overlap_fraction,
    }
}

/// The contamination-immune estimator D-2.4 mandates as a validity check, computed exactly as
/// `tail_structure.rs` computes it: the maximum over schedule residues of the minimum duration
/// observed at that residue. Interference is additive and aperiodic while the schedule is
/// periodic, so a residue's cheapest occurrence is the one nothing landed on.
fn per_residue_minimum_worst(durations: &[u64]) -> u64 {
    let mut per_residue_min = vec![u64::MAX; IR_PERIOD_BLOCKS];
    for (i, &d) in durations.iter().enumerate() {
        let r = i % IR_PERIOD_BLOCKS;
        per_residue_min[r] = per_residue_min[r].min(d);
    }
    per_residue_min
        .into_iter()
        .filter(|&v| v != u64::MAX)
        .max()
        .unwrap_or(0)
}

fn pct(nanos: u64) -> f64 {
    nanos as f64 / BLOCK_PERIOD_NS * 100.0
}

fn main() {
    pin_to_measurement_core();

    let model_bytes = generate(WaveNetShape::Standard, NAM_SEED)
        .expect("standard fixture should generate")
        .to_json_bytes();
    let ir_bytes = stereo_ir_wav_bytes();

    println!("R-7: handover crossfade cost against NFR-PERF-010's 25% budget");
    println!(
        "condition: {} Hz, {}-sample blocks, standard WaveNet, 2 s stereo IR, gate + EQ active",
        SAMPLE_RATE_HZ, BLOCK_SIZE
    );
    println!(
        "{} warmup + {} measured blocks per arm; block period {:.3} ms",
        WARMUP_BLOCKS,
        MEASURED_BLOCKS,
        BLOCK_PERIOD_NS / 1e6
    );
    println!(
        "D-2.4: pin away from CPU 0/2 (this run used NAMIR_PIN_CORE={}), verify the machine is \n\
         quiet, take >= 5 repetitions and report the spread, and DISCARD any run whose raw p99.9 \n\
         substantially exceeds its own estimator.\n",
        std::env::var("NAMIR_PIN_CORE").unwrap_or_else(|_| "4 (default)".into())
    );

    // Arm A once: it has no period.
    let steady = run_arm(Arm::Steady, usize::MAX, &model_bytes, &ir_bytes);
    let mut sorted = steady.durations.clone();
    sorted.sort_unstable();
    let steady_p999 = percentile(&sorted, 0.999);
    println!(
        "{:<24} period    - | p50 {:>6.2}% | p99 {:>6.2}% | p99.9 {:>6.2}% | max {:>7.2}% | est {:>6.2}% | fade {:>5.1}%",
        Arm::Steady.label(),
        pct(percentile(&sorted, 0.5)),
        pct(percentile(&sorted, 0.99)),
        pct(steady_p999),
        pct(*sorted.last().unwrap()),
        pct(per_residue_minimum_worst(&steady.durations)),
        steady.fade_fraction * 100.0,
    );

    for arm in [Arm::Nam, Arm::Ir, Arm::Both, Arm::BothSerialised] {
        for period in PERIODS {
            let result = run_arm(arm, period, &model_bytes, &ir_bytes);
            let mut sorted = result.durations.clone();
            sorted.sort_unstable();
            let p999 = percentile(&sorted, 0.999);
            println!(
                "{:<24} period {:>4} | p50 {:>6.2}% | p99 {:>6.2}% | p99.9 {:>6.2}% | max {:>7.2}% | est {:>6.2}% | fade {:>5.1}% | overlap {:>5.1}% | dp99.9 {:+6.2} pp",
                arm.label(),
                period,
                pct(percentile(&sorted, 0.5)),
                pct(percentile(&sorted, 0.99)),
                pct(p999),
                pct(*sorted.last().unwrap()),
                pct(per_residue_minimum_worst(&result.durations)),
                result.fade_fraction * 100.0,
                result.overlap_fraction * 100.0,
                pct(p999) - pct(steady_p999),
            );
        }
    }

    println!(
        "\nR-7 retires only if the worst arm's p99.9 stays within the 25% budget across >= 5 \n\
         repetitions under D-2.4's conditions, with the estimator agreeing to within ~2 points."
    );
}
