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
    // Pin to one core, per D-2.1: every figure is single-core, and cross-core migration would
    // pollute the tail with scheduler noise unrelated to the engine's own cost.
    pin_to_measurement_core();

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
