//! R-8's D-2.2-rigor confirmatory benchmark, run against the real `namir_ir::PreparedIr` (not the
//! spike's copy). Meant to be run only for the handful of (IR length, block size) combinations
//! `perf_sweep.rs` flags as worst-case, plus NFR-PERF-010's own literal condition and FR-IR-050's
//! floor -- the numbers that actually go in `convolver.rs`'s module doc comment.
//!
//! **Same deviation from D-2.2's flat ">= 100,000 blocks", for the same reason, ported from
//! `spikes/s2-ir-convolution/src/bin/bench.rs`'s own module doc:** D-2.2's flat count was set for
//! S-1's NAM inference, where the cost distribution's rare tail needs a large sample to land on.
//! Here, every partition sharing a nominal FFT size fires on a fixed cycle (`size` samples), so
//! the worst-case block recurs *periodically*, not rarely -- with period `largest_size /
//! block_size` blocks (`largest_size` from the real schedule via [`namir_ir::build_schedule`],
//! not assumed). This binary runs enough blocks to cover >= 200 repetitions of that period (a
//! periodic event's percentile is far more stable per sample than an i.i.d.-ish rare-tail
//! event's), capped by a wall-clock budget, and only drops below D-2.2's raw 100,000 when the
//! period itself is long enough that 100,000 blocks would blow the budget. Both the block count
//! used and the period it's justified against are printed below, exactly as the spike's did.
//!
//! Usage: `bench <ir_len_samples> <block_size> [growth_factor] [max_partition]`
//! (growth_factor/max_partition default to this crate's shipped [`DEFAULT_GROWTH_FACTOR`] /
//! [`DEFAULT_MAX_PARTITION`] -- pass them explicitly only to check a non-default schedule.)

use namir_core::SampleRate;
use namir_fixtures::ir::decaying_noise;
use namir_ir::{DEFAULT_GROWTH_FACTOR, DEFAULT_MAX_PARTITION, PreparedIr, build_schedule};
use std::time::Instant;

const WARMUP_BLOCKS: usize = 2_000;
const D22_TARGET_BLOCKS: usize = 100_000;
const MIN_PERIOD_REPEATS: usize = 200;
const WALL_BUDGET_MS: f64 = 30_000.0;
const ENGINE_HZ: u32 = 48_000; // see perf_sweep.rs's module doc: kept equal to the WAV's own
// rate so from_wav_bytes_with_schedule does zero resampling work.
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

fn write_mono_wav(sample_rate: u32, samples: &[f32]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut buf = Vec::new();
    {
        let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
        for &s in samples {
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();
    }
    buf
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 && args.len() != 5 {
        eprintln!("usage: bench <ir_len_samples> <block_size> [growth_factor] [max_partition]");
        std::process::exit(2);
    }
    let ir_len: usize = args[1].parse().expect("ir_len_samples");
    let block_size: usize = args[2].parse().expect("block_size");
    let growth_factor: usize = if args.len() == 5 {
        args[3].parse().expect("growth_factor")
    } else {
        DEFAULT_GROWTH_FACTOR
    };
    let max_partition: usize = if args.len() == 5 {
        args[4].parse().expect("max_partition")
    } else {
        DEFAULT_MAX_PARTITION
    };

    if let Some(id) = core_affinity::get_core_ids().and_then(|ids| ids.into_iter().next()) {
        core_affinity::set_for_current(id);
    }

    let h = decaying_noise(ir_len, 0xC0FF_EE00 ^ ir_len as u64, ir_len as f64 / 6.0);
    let bytes = write_mono_wav(ENGINE_HZ, &h);
    let engine_rate = SampleRate::new(ENGINE_HZ).unwrap();
    let prepared = PreparedIr::from_wav_bytes_with_schedule(
        &bytes,
        engine_rate,
        block_size,
        growth_factor,
        max_partition,
    )
    .expect("synthetic fixture WAV always decodes");
    let mut state = prepared.new_state();

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
    let mut process = |input: &[f32], output: &mut [f32]| {
        let mut out_slice: &mut [f32] = output;
        prepared.process_block(&mut state, input, std::slice::from_mut(&mut out_slice));
    };

    for _ in 0..WARMUP_BLOCKS {
        gen_block(&mut input);
        process(&input, &mut output);
        std::hint::black_box(&output);
    }

    // The largest partition tier's period, in blocks -- see the module doc comment. Computed
    // from the real schedule the prepared IR was actually built with, not assumed.
    let schedule = build_schedule(ir_len, block_size, growth_factor, max_partition);
    let largest_size = schedule.iter().map(|s| s.size).max().unwrap_or(block_size);
    let period_blocks = (largest_size / block_size).max(1);

    let calib_blocks = 50usize;
    let calib_start = Instant::now();
    for _ in 0..calib_blocks {
        gen_block(&mut input);
        process(&input, &mut output);
        std::hint::black_box(&output);
    }
    let per_block_ms =
        (calib_start.elapsed().as_secs_f64() * 1000.0 / calib_blocks as f64).max(1e-6);

    let stat_floor = period_blocks.saturating_mul(MIN_PERIOD_REPEATS).max(2_000);
    let affordable = ((WALL_BUDGET_MS / per_block_ms) as usize).max(stat_floor);
    let measured_blocks = D22_TARGET_BLOCKS.min(affordable);

    let mut durations_ns = Vec::with_capacity(measured_blocks);
    for _ in 0..measured_blocks {
        gen_block(&mut input);
        let start = Instant::now();
        process(&input, &mut output);
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
            " -- below D-2.2's raw 100,000, see this binary's module doc for why that's justified here"
        } else {
            " -- meets D-2.2's >= 100,000 directly"
        }
    );
    for (label, v) in [
        ("p50", p50),
        ("p99", p99),
        ("p99.9 (D-2.2 gate)", p999),
        ("max", max),
    ] {
        println!("  {label}: {v} ns ({:.4} ms)", v as f64 / 1e6);
    }
    println!();
    println!("per rate, as % of one core (D-2.1 -- fraction of the block period):");
    for (rate_label, rate) in RATES {
        let block_period_ns = (block_size as f64 / rate * 1e9) as u64;
        for (label, v) in [("p99.9", p999), ("max", max)] {
            let pct = v as f64 / block_period_ns as f64 * 100.0;
            println!("  {rate_label:>9}  {label:>5}: {pct:6.2}% of one core");
        }
    }
}
