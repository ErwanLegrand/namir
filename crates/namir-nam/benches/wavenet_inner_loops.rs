//! NFR-PERF-010 measurement for R-4's vectorized WaveNet inner loops (`wavenet.rs`'s `axpy`).
//! S-1's spike measured the *scalar* implementation at 41% of one core (p99.9) against
//! NFR-PERF-010's 25% budget, on the project's pinned reference machine (docs/02-architecture.md
//! §2: AMD Ryzen 9 5950X, Windows 11) — see `spikes/s1-nam-inference/src/bin/bench.rs`, whose
//! D-2.1/D-2.2 methodology this benchmark follows as closely as this crate's public API allows:
//! single-core-pinned, warmup discarded, >= 100,000 measured blocks, p50/p99/p99.9/max reported
//! both as raw time and as a percentage of the 1.333 ms block period (48 kHz, 64-sample block).
//!
//! Numbers this binary prints when run in *this* sandbox are **not** the certified NFR-PERF-010
//! figure — this sandbox's CPU is a 4-core Intel Xeon @ 2.10 GHz, not the pinned reference
//! machine — but are valid for a relative before/after comparison on the same hardware.
//!
//! `[[bench]] harness = false` (this crate's `Cargo.toml`) rather than `#[bench]`/criterion: a
//! plain `fn main` gives the same full control over pinning and percentile reporting the spike's
//! own `bin/bench.rs` uses, and this workspace has no existing criterion convention to match —
//! this crate's other non-test binary (`examples/generate_fuzz_corpus.rs`) is likewise a plain
//! `fn main`. `benches/` rather than `examples/` since this specifically is a `cargo bench`
//! target (`cargo bench -p namir-nam`), not a general-purpose tool.

use namir_fixtures::nam::{WaveNetShape, generate};

const BLOCK_SIZE: usize = 64;
const SAMPLE_RATE: f64 = 48_000.0;
const WARMUP_BLOCKS: usize = 5_000;
const MEASURED_BLOCKS: usize = 100_000; // >= 100,000 per D-2.2

fn percentile(sorted_nanos: &[u64], p: f64) -> u64 {
    let idx = ((sorted_nanos.len() as f64 - 1.0) * p).round() as usize;
    sorted_nanos[idx]
}

/// A small, seeded, dependency-free xorshift64* generator for the per-block probe signal —
/// deliberately its own copy rather than shared with `tests/fixtures.rs`'s near-identical
/// `deterministic_signal`, same reasoning that function's own doc comment gives for not sharing
/// with `namir-fixtures`' private calibration probe: no `pub` path between a `tests/` binary and
/// a `benches/` binary to share it through without adding a module surface just for this.
fn gen_block(x: &mut u64) -> Vec<f32> {
    (0..BLOCK_SIZE)
        .map(|_| {
            *x ^= *x << 13;
            *x ^= *x >> 7;
            *x ^= *x << 17;
            ((*x % 2_000_003) as f32 / 1_000_001.5) - 1.0 // roughly [-1, 1)
        })
        .collect()
}

fn main() {
    // Pin to one core, per D-2.1: every figure is single-core, and cross-core migration would
    // pollute the tail with scheduler noise unrelated to the engine's own cost.
    if let Some(id) = core_affinity::get_core_ids().and_then(|ids| ids.into_iter().next()) {
        core_affinity::set_for_current(id);
    }

    let model = generate(WaveNetShape::Standard, 1).expect("standard fixture should generate");
    let bytes = model.to_json_bytes();
    let prepared = namir_nam::load(&bytes).expect("generated fixture should load");

    // `PreparedNam` has no public `head_size()` accessor (this crate's public surface is
    // intentionally minimal, see `lib.rs`), so the output buffer's length is learned once, up
    // front, via the allocating `process` convenience wrapper on a throwaway state — this does
    // not touch the `state`/`out` used by the timed loop below.
    let out_len = {
        let mut probe_state = prepared.new_state(BLOCK_SIZE);
        prepared
            .process(&mut probe_state, &vec![0f32; BLOCK_SIZE])
            .len()
    };

    let mut state = prepared.new_state(BLOCK_SIZE);
    let mut out = vec![0f32; out_len];
    let mut rng_state = 0xBEEF_CAFEu64 ^ 0x9E37_79B9_7F4A_7C15;

    for _ in 0..WARMUP_BLOCKS {
        let block = gen_block(&mut rng_state);
        prepared.process_block(&mut state, &block, &mut out);
        std::hint::black_box(&out);
    }

    let mut durations_ns = Vec::with_capacity(MEASURED_BLOCKS);
    for _ in 0..MEASURED_BLOCKS {
        let block = gen_block(&mut rng_state);
        let start = std::time::Instant::now();
        prepared.process_block(&mut state, &block, &mut out);
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
    println!("*** NOT the certified reference-machine figure -- see this file's doc comment ***");
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
        "figure (single core, 99.9th percentile, THIS SANDBOX, not the reference machine): {:.2}% of one core",
        p999 as f64 / block_period_ns as f64 * 100.0
    );
}
