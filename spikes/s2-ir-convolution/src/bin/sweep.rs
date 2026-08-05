//! S-2's actual question: "What partition schedule minimises worst-case per-block cost for IRs
//! of 0.1-10 s at block sizes 32-2048 and rates 44.1-192 kHz?"
//!
//! Cost in raw time depends only on IR length in *samples* and block size in *samples* — the
//! engine has no notion of Hz. Sample rate only rescales the block *period* (block_size/rate),
//! which is what turns a raw nanosecond figure into the D-2.1 percentage. So this sweep varies
//! IR length directly in samples (chosen to span what 0.1-10 s at 44.1-192 kHz actually
//! produces, including both boundary extremes) and block size in samples; `bench.rs` re-derives
//! the percentage at all four rates from the same raw measurement afterwards. Documented here so
//! the missing rate axis doesn't read as an oversight.
//!
//! Per-combo block count is calibrated to a wall-clock time budget rather than fixed at D-2.2's
//! >=100,000, because a naive direct head partition is O(block_size^2) per block and the sweep
//! has ~280 combos — running all of them at >=100,000 blocks would take hours. This sweep's job
//! is comparative (which schedule wins), not the final certified figure; the winning schedule is
//! re-measured at full D-2.2 rigor by `bench.rs`.

use s2_ir_convolution::{PartitionedConvolver, fixtures};
use std::time::Instant;

const MICRO_CALIBRATION_BLOCKS: usize = 16;
const TARGET_WARMUP_MS: f64 = 15.0;
const TARGET_MEASURED_MS: f64 = 120.0;
const MIN_WARMUP_BLOCKS: usize = 20;
const MAX_WARMUP_BLOCKS: usize = 2_000;
const MIN_BLOCKS: usize = 300;
const MAX_BLOCKS: usize = 100_000;

#[derive(Clone, Copy)]
struct Schedule {
    label: &'static str,
    growth_factor: usize,
    max_partition: usize,
}

fn percentile(sorted_nanos: &[u64], p: f64) -> u64 {
    let idx = ((sorted_nanos.len() as f64 - 1.0) * p).round() as usize;
    sorted_nanos[idx]
}

fn run_one(h: &[f32], block_size: usize, sched: Schedule) -> (u64, u64, u64, usize) {
    let mut conv = PartitionedConvolver::new(h, block_size, sched.growth_factor, sched.max_partition);
    let mut rng_seed = 0x5EED_0000u64 ^ (block_size as u64) ^ ((h.len() as u64) << 20);
    let mut input = vec![0f32; block_size];
    let mut output = vec![0f32; block_size];

    let mut gen_block = |input: &mut [f32]| {
        // cheap xorshift, avoids pulling rand into the hot loop's dependency surface here
        for v in input.iter_mut() {
            rng_seed ^= rng_seed << 13;
            rng_seed ^= rng_seed >> 7;
            rng_seed ^= rng_seed << 17;
            *v = ((rng_seed >> 40) as i32 as f32) / (1i64 << 23) as f32;
        }
    };

    // Cost per block ranges from sub-microsecond (small block, small IR) to several
    // milliseconds (block=2048's O(block_size^2) direct head alone is ~4M mults) — a >1000x
    // spread across the grid. A tiny, cheap probe first, then warmup/measured block counts are
    // derived from a wall-time budget so no combo (particularly the expensive large-block ones)
    // blows the sweep's total run time.
    let calib_start = Instant::now();
    for _ in 0..MICRO_CALIBRATION_BLOCKS {
        gen_block(&mut input);
        conv.process_block(&input, &mut output);
        std::hint::black_box(&output);
    }
    let calib_elapsed_ms = calib_start.elapsed().as_secs_f64() * 1000.0;
    let per_block_ms = (calib_elapsed_ms / MICRO_CALIBRATION_BLOCKS as f64).max(1e-6);

    let warmup_blocks =
        ((TARGET_WARMUP_MS / per_block_ms) as usize).clamp(MIN_WARMUP_BLOCKS, MAX_WARMUP_BLOCKS);
    for _ in 0..warmup_blocks {
        gen_block(&mut input);
        conv.process_block(&input, &mut output);
        std::hint::black_box(&output);
    }

    let measured_blocks =
        ((TARGET_MEASURED_MS / per_block_ms) as usize).clamp(MIN_BLOCKS, MAX_BLOCKS);

    let mut durations_ns = Vec::with_capacity(measured_blocks);
    for _ in 0..measured_blocks {
        gen_block(&mut input);
        let start = Instant::now();
        conv.process_block(&input, &mut output);
        let elapsed = start.elapsed();
        std::hint::black_box(&output);
        durations_ns.push(elapsed.as_nanos() as u64);
    }
    durations_ns.sort_unstable();
    let p999 = percentile(&durations_ns, 0.999);
    let max = *durations_ns.last().unwrap();
    let p50 = percentile(&durations_ns, 0.5);
    (p50, p999, max, measured_blocks)
}

fn main() {
    if let Some(id) = core_affinity::get_core_ids().and_then(|ids| ids.into_iter().next()) {
        core_affinity::set_for_current(id);
    }

    let ir_lens: [(&str, usize); 8] = [
        ("0.1s@44.1k (min)", 4_410),
        ("0.5s@48k", 24_000),
        ("1s@48k", 48_000),
        ("2s@48k (FR-IR-050 floor)", 96_000),
        ("5s@48k", 240_000),
        ("10s@48k", 480_000),
        ("10s@96k", 960_000),
        ("10s@192k (max)", 1_920_000),
    ];
    let block_sizes = [32usize, 64, 128, 256, 512, 1024, 2048];
    let schedules = [
        Schedule { label: "uniform", growth_factor: 1, max_partition: 1 },
        Schedule { label: "g2/max8192", growth_factor: 2, max_partition: 8192 },
        Schedule { label: "g2/max16384", growth_factor: 2, max_partition: 16384 },
        Schedule { label: "g2/max32768", growth_factor: 2, max_partition: 32768 },
        Schedule { label: "g4/max8192", growth_factor: 4, max_partition: 8192 },
        Schedule { label: "g4/max16384", growth_factor: 4, max_partition: 16384 },
        Schedule { label: "g8/max8192", growth_factor: 8, max_partition: 8192 },
    ];

    println!("ir_label,ir_len,block_size,schedule,growth_factor,max_partition,measured_blocks,p50_ns,p999_ns,max_ns");

    // (schedule label) -> worst max_ns observed anywhere in the grid, and where.
    let mut worst_per_schedule: Vec<(u64, String)> = vec![(0, String::new()); schedules.len()];

    for &(ir_label, ir_len) in &ir_lens {
        let h = fixtures::decaying_noise(ir_len, 0xC0FF_EE00 ^ ir_len as u64, ir_len as f64 / 6.0);
        for &block_size in &block_sizes {
            for (si, &sched) in schedules.iter().enumerate() {
                let max_partition = sched.max_partition.max(block_size);
                let (p50, p999, max, measured_blocks) = run_one(
                    &h,
                    block_size,
                    Schedule { max_partition, ..sched },
                );
                println!(
                    "{ir_label},{ir_len},{block_size},{},{},{},{measured_blocks},{p50},{p999},{max}",
                    sched.label, sched.growth_factor, max_partition
                );
                if max > worst_per_schedule[si].0 {
                    worst_per_schedule[si] =
                        (max, format!("ir={ir_label} block={block_size}"));
                }
            }
        }
    }

    eprintln!();
    eprintln!("=== worst-case max per-block cost per schedule, across the whole grid ===");
    for (sched, (worst_ns, where_)) in schedules.iter().zip(worst_per_schedule.iter()) {
        eprintln!(
            "{:14} worst max = {:>10} ns  ({where_})",
            sched.label, worst_ns
        );
    }
    let best = worst_per_schedule
        .iter()
        .enumerate()
        .min_by_key(|(_, (ns, _))| *ns)
        .unwrap();
    eprintln!();
    eprintln!(
        "=== recommended default: {} (lowest worst-case max across the grid) ===",
        schedules[best.0].label
    );
}
