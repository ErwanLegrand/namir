//! The NFR-PERF-010 IR-stage-share figure, measured per D-2.1/D-2.2: single-core-pinned,
//! reporting the 99.9th percentile and max, not the mean. Meant to be run only for the handful
//! of (schedule, block_size, ir_len) combinations `sweep.rs` flagged as worst-case, to produce
//! the number that actually goes in the architecture document.
//!
//! **Deviation from D-2.2's flat ">= 100,000 blocks" recorded here, not silently applied:** that
//! figure was set for S-1's NAM inference, where the cost distribution's rare tail needs a large
//! sample to land on. Here every same-size partition tier fires in lockstep (see `lib.rs`'s
//! module doc), so the worst case is *periodic*, not rare — it recurs every
//! `max_partition / block_size` blocks. For block=2048 against the worst-case 10 s@192 kHz IR,
//! that period is single-digit blocks and each block costs single-digit milliseconds, so
//! 100,000 blocks would mean 10+ minutes for one measurement. Instead this binary runs enough
//! blocks to cover >= 200 repetitions of that period (which, for a periodic rather than rare
//! event, gives a far more stable percentile estimate than 100,000 i.i.d.-ish samples would),
//! capped by a wall-clock budget, and only falls back below the raw D-2.2 count when the period
//! itself is long enough that 100,000 blocks would exceed that budget. Both the block count used
//! and the period it's justified against are printed below.
//!
//! Usage: `bench <ir_len_samples> <block_size> <growth_factor> <max_partition>`

use s2_ir_convolution::{PartitionedConvolver, build_schedule, fixtures};
use std::time::Instant;

const WARMUP_BLOCKS: usize = 2_000;
const D22_TARGET_BLOCKS: usize = 100_000;
const MIN_PERIOD_REPEATS: usize = 200;
const WALL_BUDGET_MS: f64 = 30_000.0;
const RATES: [(&str, f64); 4] = [
    ("44.1 kHz", 44_100.0),
    ("48 kHz", 48_000.0),
    ("96 kHz", 96_000.0),
    ("192 kHz", 192_000.0),
];

fn percentile(sorted_nanos: &[u64], p: f64) -> u64 {
    let idx = ((sorted_nanos.len() as f64 - 1.0) * p).round() as usize;
    sorted_nanos[idx]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: bench <ir_len_samples> <block_size> <growth_factor> <max_partition>");
        std::process::exit(2);
    }
    let ir_len: usize = args[1].parse().expect("ir_len_samples");
    let block_size: usize = args[2].parse().expect("block_size");
    let growth_factor: usize = args[3].parse().expect("growth_factor");
    let max_partition: usize = args[4].parse().expect("max_partition");

    if let Some(id) = core_affinity::get_core_ids().and_then(|ids| ids.into_iter().next()) {
        core_affinity::set_for_current(id);
    }

    let h = fixtures::decaying_noise(ir_len, 0xC0FF_EE00 ^ ir_len as u64, ir_len as f64 / 6.0);
    let mut conv = PartitionedConvolver::new(&h, block_size, growth_factor, max_partition.max(block_size));

    let mut rng_seed: u64 = 0xB16B_00B5;
    let mut gen_block = |buf: &mut [f32]| {
        for v in buf.iter_mut() {
            rng_seed ^= rng_seed << 13;
            rng_seed ^= rng_seed >> 7;
            rng_seed ^= rng_seed << 17;
            *v = ((rng_seed >> 40) as i32 as f32) / (1i64 << 23) as f32;
        }
    };

    let mut input = vec![0f32; block_size];
    let mut output = vec![0f32; block_size];

    for _ in 0..WARMUP_BLOCKS {
        gen_block(&mut input);
        conv.process_block(&input, &mut output);
        std::hint::black_box(&output);
    }

    // The largest partition tier's period, in blocks — every stage at that size fires together
    // (see the module doc), so that's the recurrence interval of the worst-case block.
    let schedule = build_schedule(ir_len, block_size, growth_factor, max_partition.max(block_size));
    let largest_size = schedule.iter().map(|s| s.size).max().unwrap_or(block_size);
    let period_blocks = (largest_size / block_size).max(1);

    let calib_blocks = 50usize;
    let calib_start = Instant::now();
    for _ in 0..calib_blocks {
        gen_block(&mut input);
        conv.process_block(&input, &mut output);
        std::hint::black_box(&output);
    }
    let per_block_ms = (calib_start.elapsed().as_secs_f64() * 1000.0 / calib_blocks as f64).max(1e-6);

    let stat_floor = period_blocks.saturating_mul(MIN_PERIOD_REPEATS).max(2_000);
    let affordable = ((WALL_BUDGET_MS / per_block_ms) as usize).max(stat_floor);
    let measured_blocks = D22_TARGET_BLOCKS.min(affordable);

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
    let p50 = percentile(&durations_ns, 0.50);
    let p99 = percentile(&durations_ns, 0.99);
    let p999 = percentile(&durations_ns, 0.999);
    let max = *durations_ns.last().unwrap();

    println!(
        "=== NFR-PERF-010 IR stage: ir_len={ir_len} samples, block={block_size}, schedule=g{growth_factor}/max{max_partition} ==="
    );
    println!(
        "worst-case tier period: {period_blocks} blocks (largest partition {largest_size} / block {block_size})"
    );
    println!(
        "blocks measured: {measured_blocks} = {:.0} period repetitions (warmup {WARMUP_BLOCKS} discarded){}",
        measured_blocks as f64 / period_blocks as f64,
        if measured_blocks < D22_TARGET_BLOCKS {
            " — below D-2.2's raw 100,000, see this binary's module doc for why that's justified here"
        } else {
            " — meets D-2.2's >= 100,000 directly"
        }
    );
    for (label, v) in [("p50", p50), ("p99", p99), ("p99.9 (D-2.2 gate)", p999), ("max", max)] {
        println!("  {label}: {v} ns ({:.4} ms)", v as f64 / 1e6);
    }
    println!();
    println!("per rate, as % of one core (D-2.1 — fraction of the block period):");
    for (rate_label, rate) in RATES {
        let block_period_ns = (block_size as f64 / rate * 1e9) as u64;
        for (label, v) in [("p99.9", p999), ("max", max)] {
            let pct = v as f64 / block_period_ns as f64 * 100.0;
            println!("  {rate_label:>9}  {label:>5}: {pct:6.2}% of one core");
        }
    }
}
