//! FR-NAM-120 / NFR-PERF-010 cost curve for LSTM (M10 Phase 4(b),
//! `docs/03-implementation-roadmap.md` §17). `namir-nam` has had a WaveNet cost benchmark
//! (`wavenet_inner_loops.rs`) since M3 and **no LSTM benchmark at all** until this file: every
//! performance figure this project holds -- S-1, R-4, D-2.3's `x86-64-v3` finding,
//! `wavenet_inner_loops.rs`, NFR-PERF-010's certification -- is WaveNet. Where LSTM sits against
//! NFR-PERF-010's 25%-of-one-core budget has been simply unknown, not known-good.
//!
//! Sweeps the exact shape grid the real 67-model set characterises
//! (`docs/manual-tests/fr-nam-020-real-lstm-models.md`): `num_layers` in {1, 2, 3, 4} x
//! `hidden_size` in {1..12, 16, 20, 24, 28, 32}, all at `input_size` 1, 48 kHz -- the same grid
//! that set's source post ("Towards a (good) CPU-efficient NAM") published specifically to
//! characterise the low-compute regime. Reported **as a curve** (one row per shape), not a single
//! figure, per Phase 4's acceptance text: if any shape breaches the budget, that is printed
//! plainly rather than averaged away.
//!
//! # Why the weights are locally generated rather than real
//!
//! D-19.1 (generated-never-captured) applies to every fixture this crate's test/bench suite uses,
//! independent of the separate licence question the real 67-model set also carries (no stated
//! licence -- see the manual-test doc above) -- so a real `.nam` file has no business being a bench
//! input regardless. That is not a loss here specifically: **cost follows topology, not weight
//! values** (`namir-fixtures`' own `WaveNetShape` doc comment makes the identical point), so a
//! constrained-init fixture at the real shape is exactly as informative for a cost curve as the
//! real model would be. Numerical parity against the real models -- where weight values *do*
//! matter -- is `xtask nam-parity`'s job (`xtask/src/nam_parity.rs`), not this file's.
//!
//! `namir_fixtures::nam::LstmShape` only names three shapes (`Standard`/`Small`/`Tiny`), not the
//! 68-shape grid this sweep needs, so the weight builder below is a bench-local generalisation of
//! that crate's own private `build_lstm_weights` (constrained, fan-in-scaled uniform init; no RMS
//! calibration pass, since calibration only matters when the *value* of the output is compared
//! against something, not when only its cost is measured). `namir-nam` carries no `rand`
//! dependency, so this uses the same home-grown, seeded xorshift64* generator `tests/fixtures.rs`,
//! `tests/lstm_fixtures.rs` and `wavenet_inner_loops.rs` already each carry their own copy of,
//! rather than adding one just for this. The container types (`LstmModel`/`LstmConfig`/
//! `NamMetadata`) are reused from `namir_fixtures::nam` as-is -- they are already public, and
//! re-deriving `.nam`'s LSTM JSON schema a third time would be pure risk for no benefit.
//!
//! # Methodology
//!
//! Same shape as `wavenet_inner_loops.rs`: `[[bench]] harness = false`, plain `fn main`,
//! `pin_to_measurement_core` (D-2.1), warmup discarded, p50/p99/p99.9/max reported both as raw
//! time and as a percentage of the 1.333 ms block period (48 kHz, 64-sample block).
//!
//! **Informational, not certified (D-2.4).** This machine is not a quiet benchmarking rig (see
//! `docs/02-architecture.md` §2 and `AGENTS.md`'s "Benchmark methodology" section), and a
//! certified NFR-PERF-010 figure needs >= 5 repetitions on the pinned reference machine under full
//! rigor -- a routine 68-shape sweep is explicitly not that. Per-shape block counts are also much
//! smaller than `wavenet_inner_loops.rs`'s single-shape 100,000 measured blocks: running that many
//! per shape across the whole grid would cost far more wall-clock time than the extra statistical
//! depth is worth for a curve whose purpose is to find where costs move, not to certify one figure.
//! p50 is stable at these counts; p99.9/max are noisier than a dedicated single-shape run's and
//! should be read as indicative, not as a tight bound -- re-run and compare before trusting any one
//! shape's tail figure in isolation.

use namir_fixtures::nam::{LstmConfig, LstmModel, NamMetadata};

const BLOCK_SIZE: usize = 64;
const SAMPLE_RATE: f64 = 48_000.0;
const WARMUP_BLOCKS: usize = 500;
const MEASURED_BLOCKS: usize = 5_000;
/// NFR-PERF-010's budget: 25% of one core, single-threaded, per block.
const NFR_PERF_010_BUDGET_PCT: f64 = 25.0;

/// `num_layers` 1-4, matching the real 67-model set's grid exactly.
const NUM_LAYERS: [usize; 4] = [1, 2, 3, 4];
/// `hidden_size` 1-12, then 16/20/24/28/32 -- again the real set's own grid, not a guess.
const HIDDEN_SIZES: [usize; 17] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 16, 20, 24, 28, 32];

fn percentile(sorted_nanos: &[u64], p: f64) -> u64 {
    let idx = ((sorted_nanos.len() as f64 - 1.0) * p).round() as usize;
    sorted_nanos[idx]
}

/// A small, seeded, dependency-free xorshift64* generator -- the same construction
/// `tests/fixtures.rs`'s/`tests/lstm_fixtures.rs`'s `deterministic_signal` and
/// `wavenet_inner_loops.rs`'s `gen_block` each already carry their own copy of, generalised here
/// into a reusable struct since this file needs it for two different purposes (weight init and
/// per-block probe signals) rather than one.
struct Xorshift64Star(u64);

impl Xorshift64Star {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    /// Roughly uniform over `[-1, 1)`, same construction every copy of this generator in this
    /// crate uses.
    fn next_unit(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        ((x % 2_000_003) as f32 / 1_000_001.5) - 1.0
    }

    fn uniform(&mut self, scale: f32) -> f32 {
        self.next_unit() * scale
    }
}

fn push_uniform(rng: &mut Xorshift64Star, out: &mut Vec<f32>, count: usize, scale: f32) {
    for _ in 0..count {
        out.push(rng.uniform(scale));
    }
}

fn push_zeros(out: &mut Vec<f32>, count: usize) {
    out.resize(out.len() + count, 0.0);
}

/// Bench-local generalisation of `namir_fixtures::nam`'s private `build_lstm_weights`, to any
/// `(num_layers, hidden_size)` rather than just the three named `LstmShape` constants. Same flat
/// layout `namir-nam`'s `lstm.rs` consumes: per layer `[W, b, h0, c0]` (layer 0's `W` has
/// `cell_input_size == input_size`, every later layer's has `cell_input_size == hidden_size`),
/// then `head_weight`, `head_bias`.
fn build_lstm_weights(
    num_layers: usize,
    input_size: usize,
    hidden_size: usize,
    out_channels: usize,
    rng: &mut Xorshift64Star,
) -> Vec<f32> {
    let mut w = Vec::new();
    for i in 0..num_layers {
        let cell_input_size = if i == 0 { input_size } else { hidden_size };
        let rows = 4 * hidden_size;
        let cols = cell_input_size + hidden_size;
        let s = 1.0 / (cols as f32).sqrt();
        push_uniform(rng, &mut w, rows * cols, s); // W
        push_zeros(&mut w, rows); // b
        push_uniform(rng, &mut w, hidden_size, 0.1); // h0
        push_uniform(rng, &mut w, hidden_size, 0.1); // c0
    }

    let s = 1.0 / (hidden_size as f32).sqrt();
    push_uniform(rng, &mut w, out_channels * hidden_size, s); // head_weight
    push_zeros(&mut w, out_channels); // head_bias
    w
}

/// Builds one shape's fixture. `input_size`/`in_channels`/`out_channels` are pinned to 1, matching
/// both the real 67-model set and `namir-nam`'s own scope restriction for LSTM.
fn build_model(num_layers: usize, hidden_size: usize, seed: u64) -> LstmModel {
    let mut rng = Xorshift64Star::new(seed);
    let weights = build_lstm_weights(num_layers, 1, hidden_size, 1, &mut rng);
    LstmModel {
        version: "0.5.5".to_string(),
        architecture: "LSTM".to_string(),
        config: LstmConfig {
            num_layers,
            input_size: 1,
            hidden_size,
            in_channels: 1,
            out_channels: 1,
        },
        weights,
        sample_rate: SAMPLE_RATE as u32,
        metadata: NamMetadata {
            name: format!("namir-nam bench fixture LSTM-{num_layers}-{hidden_size:03}"),
            modeled_by: "namir-nam lstm_inner_loops bench".to_string(),
            gear_type: "amp".to_string(),
            tone_type: "clean".to_string(),
            description: "Locally generated, constrained-init LSTM used only to measure \
                process_block cost across the real shape grid (D-19.1); not a numeric-parity \
                input, and no calibration pass -- cost does not depend on weight values."
                .to_string(),
        },
    }
}

fn gen_block(rng: &mut Xorshift64Star) -> Vec<f32> {
    (0..BLOCK_SIZE).map(|_| rng.uniform(1.0)).collect()
}

/// See `wavenet_inner_loops.rs`'s own copy of this function for the full measured rationale
/// (GPU-driver ISR contamination on CPU 0, kernel DPC load on CPU 2) -- duplicated rather than
/// shared for the same reason every other per-bench-binary helper in this crate is: no `pub` path
/// between two separate `benches/` binaries to share it through without adding module surface just
/// for this.
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

struct ShapeResult {
    num_layers: usize,
    hidden_size: usize,
    p50_ns: u64,
    p99_ns: u64,
    p999_ns: u64,
    max_ns: u64,
}

fn main() {
    // Pin to one core, per D-2.1: every figure is single-core, and cross-core migration would
    // pollute the tail with scheduler noise unrelated to the engine's own cost.
    pin_to_measurement_core();

    let block_period_ns = (BLOCK_SIZE as f64 / SAMPLE_RATE * 1e9) as u64;

    println!(
        "=== FR-NAM-120 / NFR-PERF-010: LSTM cost curve, {BLOCK_SIZE}-sample blocks @ {SAMPLE_RATE} Hz ==="
    );
    println!(
        "*** INFORMATIONAL -- not the certified reference-machine figure; see this file's doc comment (D-2.4) ***"
    );
    println!(
        "grid: num_layers {:?} x hidden_size {:?} ({} shapes)",
        NUM_LAYERS,
        HIDDEN_SIZES,
        NUM_LAYERS.len() * HIDDEN_SIZES.len()
    );
    println!(
        "block period (D-2.1): {block_period_ns} ns ({:.4} ms); warmup {WARMUP_BLOCKS}, measured {MEASURED_BLOCKS} blocks per shape",
        block_period_ns as f64 / 1e6
    );
    println!();

    let mut results = Vec::with_capacity(NUM_LAYERS.len() * HIDDEN_SIZES.len());

    for &num_layers in &NUM_LAYERS {
        for &hidden_size in &HIDDEN_SIZES {
            let seed = 0xA5F0_0000_u64 ^ ((num_layers as u64) << 16) ^ hidden_size as u64;
            let model = build_model(num_layers, hidden_size, seed);
            let bytes = model.to_json_bytes();
            let prepared = namir_nam::load(&bytes)
                .unwrap_or_else(|e| panic!("LSTM-{num_layers}-{hidden_size:03}: {e}"));

            // Learn the output buffer length once, on a throwaway state, same approach
            // `wavenet_inner_loops.rs` uses (`PreparedNam` has no public `head_size` accessor).
            let out_len = {
                let mut probe_state = prepared.new_state(BLOCK_SIZE);
                prepared
                    .process(&mut probe_state, &vec![0f32; BLOCK_SIZE])
                    .len()
            };

            let mut state = prepared.new_state(BLOCK_SIZE);
            let mut out = vec![0f32; out_len];
            let mut rng = Xorshift64Star::new(seed ^ 0xBEEF_CAFE);

            for _ in 0..WARMUP_BLOCKS {
                let block = gen_block(&mut rng);
                prepared.process_block(&mut state, &block, &mut out);
                std::hint::black_box(&out);
            }

            let mut durations_ns = Vec::with_capacity(MEASURED_BLOCKS);
            for _ in 0..MEASURED_BLOCKS {
                let block = gen_block(&mut rng);
                let start = std::time::Instant::now();
                prepared.process_block(&mut state, &block, &mut out);
                let elapsed = start.elapsed();
                std::hint::black_box(&out);
                durations_ns.push(elapsed.as_nanos() as u64);
            }

            durations_ns.sort_unstable();
            results.push(ShapeResult {
                num_layers,
                hidden_size,
                p50_ns: percentile(&durations_ns, 0.50),
                p99_ns: percentile(&durations_ns, 0.99),
                p999_ns: percentile(&durations_ns, 0.999),
                max_ns: *durations_ns.last().unwrap(),
            });
        }
    }

    println!(
        "{:>7}  {:>6}  {:>10}  {:>7}  {:>10}  {:>7}  {:>10}  {:>7}  {:>10}",
        "layers", "hidden", "p50 ns", "p50 %", "p99 ns", "p99 %", "p99.9 ns", "p99.9 %", "max ns"
    );
    let mut breaches: Vec<&ShapeResult> = Vec::new();
    for r in &results {
        let pct = |ns: u64| ns as f64 / block_period_ns as f64 * 100.0;
        println!(
            "{:>7}  {:>6}  {:>10}  {:>6.2}%  {:>10}  {:>6.2}%  {:>10}  {:>6.2}%  {:>10}",
            r.num_layers,
            r.hidden_size,
            r.p50_ns,
            pct(r.p50_ns),
            r.p99_ns,
            pct(r.p99_ns),
            r.p999_ns,
            pct(r.p999_ns),
            r.max_ns,
        );
        if pct(r.p999_ns) >= NFR_PERF_010_BUDGET_PCT {
            breaches.push(r);
        }
    }

    println!();
    if breaches.is_empty() {
        println!(
            "no shape in this grid breaches NFR-PERF-010's {NFR_PERF_010_BUDGET_PCT}% budget at p99.9 (informational figure -- D-2.4)"
        );
    } else {
        println!(
            "*** {} shape(s) BREACH NFR-PERF-010's {NFR_PERF_010_BUDGET_PCT}% budget at p99.9 (informational figure -- re-measure under D-2.4 rigor before treating this as certified): ***",
            breaches.len()
        );
        for r in &breaches {
            let pct999 = r.p999_ns as f64 / block_period_ns as f64 * 100.0;
            println!(
                "  - LSTM-{}-{:03}: p99.9 = {pct999:.2}%",
                r.num_layers, r.hidden_size
            );
        }
    }
}
