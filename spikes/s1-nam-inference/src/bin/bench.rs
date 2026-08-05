//! NFR-PERF-010 measurement: 99.9th percentile of per-block processing time for the "standard"
//! WaveNet shape, single-core, 48 kHz / 64-sample blocks, reported per D-2.1 (as a percentage
//! of the 1.333 ms block period) and D-2.2 (99.9th percentile plus the maximum, over
//! >=100,000 blocks, not the mean).

use rand::Rng;
use rand::SeedableRng;
use s1_nam_inference::{NamFile, PreparedWaveNet};
use std::time::Instant;

const BLOCK_SIZE: usize = 64;
const SAMPLE_RATE: f64 = 48_000.0;
const WARMUP_BLOCKS: usize = 5_000;
const MEASURED_BLOCKS: usize = 200_000; // >= 100,000 per D-2.2

fn percentile(sorted_nanos: &[u64], p: f64) -> u64 {
    let idx = ((sorted_nanos.len() as f64 - 1.0) * p).round() as usize;
    sorted_nanos[idx]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: bench <model.nam>");
        std::process::exit(2);
    }

    // Pin to one core, per D-2.1: every figure is single-core, and cross-core migration would
    // pollute the tail with scheduler noise unrelated to the engine's own cost.
    if let Some(id) = core_affinity::get_core_ids().and_then(|ids| ids.into_iter().next()) {
        core_affinity::set_for_current(id);
    }

    let nam_json = std::fs::read_to_string(&args[1]).expect("read .nam file");
    let nam: NamFile = serde_json::from_str(&nam_json).expect("parse .nam JSON");
    let prepared = PreparedWaveNet::from_nam_file(&nam).expect("build WaveNet from weights");
    let mut state = prepared.new_state(BLOCK_SIZE);

    let mut rng = rand_pcg::Pcg64::seed_from_u64(0xBEEF_CAFE);
    let gen_block = |rng: &mut rand_pcg::Pcg64| -> Vec<f32> {
        (0..BLOCK_SIZE)
            .map(|_| rng.gen_range(-0.8f32..0.8))
            .collect()
    };

    let mut out = vec![0f32; BLOCK_SIZE]; // reused every block; process_block_into never allocates

    for _ in 0..WARMUP_BLOCKS {
        let block = gen_block(&mut rng);
        prepared.process_block_into(&mut state, &block, &mut out);
        std::hint::black_box(&out);
    }

    let mut durations_ns = Vec::with_capacity(MEASURED_BLOCKS);
    for _ in 0..MEASURED_BLOCKS {
        let block = gen_block(&mut rng);
        let start = Instant::now();
        prepared.process_block_into(&mut state, &block, &mut out);
        let elapsed = start.elapsed();
        std::hint::black_box(&out);
        durations_ns.push(elapsed.as_nanos() as u64);
    }

    durations_ns.sort_unstable();
    let p50 = percentile(&durations_ns, 0.50);
    let p99 = percentile(&durations_ns, 0.99);
    let p999 = percentile(&durations_ns, 0.999);
    let max = *durations_ns.last().unwrap();

    let block_period_ns = (BLOCK_SIZE as f64 / SAMPLE_RATE * 1e9) as u64;

    println!(
        "=== NFR-PERF-010: standard WaveNet, {BLOCK_SIZE}-sample blocks @ {SAMPLE_RATE} Hz ==="
    );
    println!("blocks measured: {MEASURED_BLOCKS} (warmup {WARMUP_BLOCKS} discarded)");
    println!(
        "block period (D-2.1): {block_period_ns} ns ({:.4} ms)",
        block_period_ns as f64 / 1e6
    );
    for (label, v) in [
        ("p50", p50),
        ("p99", p99),
        ("p99.9 (D-2.2 gate)", p999),
        ("max", max),
    ] {
        let pct = v as f64 / block_period_ns as f64 * 100.0;
        println!(
            "  {label}: {v} ns ({:.4} ms) = {pct:.2}% of block period",
            v as f64 / 1e6
        );
    }
    println!();
    println!(
        "NFR-PERF-010 figure (single core, 99.9th percentile): {:.2}% of one core",
        p999 as f64 / block_period_ns as f64 * 100.0
    );
}
