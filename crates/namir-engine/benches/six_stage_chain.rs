//! M3's exit criterion (`docs/03-implementation-roadmap.md` §7): "the full six-stage chain,
//! assembled for real, meets NFR-PERF-010's literal condition (48 kHz, 64-sample block, standard
//! WaveNet, 2 s stereo IR, gate + EQ active) at the 99.9th percentile per D-2.1/D-2.2's
//! methodology". Every prior number in this repo (S-1's NAM spike, S-2's IR spike, R-4's
//! `namir-nam` inner-loop bench, R-8's `namir-ir` `perf_bench.rs`) measured one stage in
//! isolation; this is the first benchmark that assembles the *real* product chain — every
//! `StagePrep::prepare` call the shipped `build_default_chain` makes, a real generated WaveNet
//! model and a real 2 s stereo IR actually loaded, gate and EQ actually engaged with non-default
//! values — and runs `Chain::process` end to end, gate → trim → nam → ir → eq → out, per block.
//!
//! # Why this duplicates `stages::build_default_chain`'s body instead of calling it
//!
//! **Decision:** hand-assemble the six stages here rather than call
//! `namir_engine::build_default_chain`.
//!
//! **Rationale:** `build_default_chain` returns `Chain`, which owns its stages only as
//! `Box<dyn Stage>` — the concrete `NamStage`/`IrStage` types (and their `load_model`/`load_ir`
//! methods, which are not part of the `Stage` trait at all) are gone the moment they're boxed.
//! There is no way to reach into an assembled `Chain` and load a resource into one of its stages
//! after the fact. The only way to load real resources *and* end up with the same chain
//! `build_default_chain` builds is to do what it does, here, keeping the concrete types around
//! just long enough to call `load_model`/`load_ir`, then boxing exactly as it does.
//!
//! **Consequence:** this file's stage-assembly block (the six `*Prep::prepare(&ctx)?` calls, the
//! `Vec<Box<dyn Stage>>`, `Chain::new` + `prepare_crosscutting`) must be kept in sync with
//! `stages/mod.rs::build_default_chain` by hand if that function's stage list or order ever
//! changes. Accepted: a `[[bench]]` binary cannot depend on `namir-engine`'s own private stage
//! fields any other way without changing `Chain`'s public API just for this one measurement, and
//! duplicating six lines of assembly is a smaller, more legible cost than either.
//!
//! # Methodology
//!
//! Same D-2.1/D-2.2 methodology as `spikes/s1-nam-inference/src/bin/bench.rs` and
//! `namir-nam/benches/wavenet_inner_loops.rs`, extended to the whole chain: single-core-pinned
//! (`core_affinity`), 5,000 warmup blocks discarded, >= 100,000 measured blocks of 64 samples at
//! 48 kHz, p50/p99/p99.9/max reported both as raw time and as a percentage of the 1.333 ms block
//! period. `chain.process(&mut io)` is called directly, without this crate's `rt_harness::
//! audio_section` wrapper — that harness exists to turn an accidental allocation into a test
//! failure (D-7.5), which is a correctness concern for `#[test]`s, not a timing concern for a
//! `[[bench]]` binary; wrapping every timed call in it here would only add the harness's own
//! bookkeeping overhead to every measured sample for no benefit this file needs.
//!
//! # Hardware caveat — read before trusting any number this binary prints
//!
//! This binary, run in this task's sandbox, measures on a **4-core Intel Xeon @ 2.10 GHz**, which
//! is **not** `docs/02-architecture.md` §2's pinned reference machine (AMD Ryzen 9 5950X,
//! 16c/32t, 3.4 GHz base, Windows 11). Every number this binary prints in that sandbox is
//! directional evidence only — valid for a same-machine before/after comparison, exactly like
//! `wavenet_inner_loops.rs`'s and `convolver.rs`'s own identical caveats — and is explicitly
//! **not** the certified NFR-PERF-010 sign-off figure, which `03-implementation-roadmap.md` §7
//! requires re-running this same binary on the actual reference machine to produce.

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
const MEASURED_BLOCKS: usize = 100_000; // >= 100,000 per D-2.2
const NFR_PERF_010_BUDGET_PCT: f64 = 25.0;

/// A fixed seed for the "standard" WaveNet fixture — D-19.1's generated-not-captured corpus,
/// same shape S-1/R-4 measured (`WaveNetShape::Standard`: the only shape confirmed against
/// `neural-amp-modeler`'s own `get_wavenet_config`, see `namir-fixtures`' own doc comment).
const NAM_SEED: u64 = 0xC0FF_EE01;
/// Seeds for the 2 s stereo IR's two independent channels (this module's own choice: a real
/// stereo IR's two channels are not the same signal, so two distinct seeds are used rather than
/// duplicating one mono decay onto both channels — see `stereo_ir_wav_bytes`).
const IR_SEED_LEFT: u64 = 0xBEEF_0001;
const IR_SEED_RIGHT: u64 = 0xBEEF_0002;
/// 2 seconds at 48 kHz, per NFR-PERF-010's own literal condition.
const IR_LEN_SAMPLES: usize = 2 * SAMPLE_RATE_HZ as usize;
/// Decay time constant for the `decaying_noise` fixture: short enough that the IR's energy is
/// concentrated well inside its own 2 s length (a flat, undecayed 2 s noise burst would not
/// resemble any real cabinet/room IR and is not what NFR-PERF-010's "2 s stereo IR" condition is
/// describing), long enough that essentially the whole 2 s length still carries convolution cost
/// (`namir-ir`'s own R-8 section: cost follows tap *count*, not tap values, so this only affects
/// how IR-shaped the fixture looks, not what it costs).
const IR_DECAY_TAU_SAMPLES: f64 = 8_000.0;

/// A real, non-default gate threshold (FR-GATE-010's -100..0 dBFS range; the descriptor default
/// is -70 dBFS) — chosen above the driving signal's own RMS level (see `gen_block`) so the gate
/// stays open throughout the measured run: this benchmark is timing steady-state per-sample DSP
/// cost (envelope detection + smoothing, which runs identically whether the gate is open or
/// closed), not attack/release transient behaviour, so an open gate keeps the measured condition
/// simple without changing what is actually being measured.
const GATE_THRESHOLD_DB: f32 = -40.0;
/// A real, non-default EQ low-shelf gain (FR-EQ-010's +-15 dB range; the descriptor default is
/// 0 dB) — proves the EQ cascade is doing real shaping work, not running at its identity
/// coefficients.
const EQ_LOW_SHELF_GAIN_DB: f32 = 6.0;

fn percentile(sorted_nanos: &[u64], p: f64) -> u64 {
    let idx = ((sorted_nanos.len() as f64 - 1.0) * p).round() as usize;
    sorted_nanos[idx]
}

/// A small, seeded, dependency-free xorshift64* generator for the per-block driving signal —
/// same approach `namir-nam/benches/wavenet_inner_loops.rs`'s own `gen_block` uses, duplicated
/// for the same reason that file's doc comment gives (no shared path between a `benches/` binary
/// here and one in another crate).
fn gen_block(x: &mut u64, out: &mut [f32]) {
    for s in out.iter_mut() {
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        let noise = ((*x % 2_000_003) as f32 / 1_000_001.5) - 1.0; // roughly [-1, 1)
        *s = 0.1 * noise; // ~-20 dBFS RMS: comfortably above GATE_THRESHOLD_DB, keeps the gate open.
    }
}

/// Writes a small in-memory stereo WAV via `hound::WavWriter`, the same pattern
/// `namir-ir/src/convolver.rs`'s own test module's `write_stereo_wav` uses (not reusable
/// directly — that helper is private to that crate's `#[cfg(test)]` module) and
/// `namir-engine/src/stages/ir.rs`'s own test module already duplicates for the identical
/// reason.
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

/// Builds the 2 s stereo IR's WAV bytes: `namir_fixtures::ir::decaying_noise` (D-9.5's
/// "realistic-shaped" fixture — a real cabinet/room IR's cost depends only on length, not exact
/// taps, per that module's own doc comment) on each channel independently, with different seeds
/// so the two channels are genuinely distinct signals rather than one mono decay duplicated —
/// FR-CHAIN-060's "stereo IR" case, not "dual mono".
fn stereo_ir_wav_bytes() -> Vec<u8> {
    let left = decaying_noise(IR_LEN_SAMPLES, IR_SEED_LEFT, IR_DECAY_TAU_SAMPLES);
    let right = decaying_noise(IR_LEN_SAMPLES, IR_SEED_RIGHT, IR_DECAY_TAU_SAMPLES);
    write_stereo_wav(SAMPLE_RATE_HZ, &left, &right)
}

fn main() {
    // Pin to one core, per D-2.1: every figure is single-core, and cross-core migration would
    // pollute the tail with scheduler noise unrelated to the chain's own cost.
    if let Some(id) = core_affinity::get_core_ids().and_then(|ids| ids.into_iter().next()) {
        core_affinity::set_for_current(id);
    }

    let sample_rate = SampleRate::new(SAMPLE_RATE_HZ).expect("48 kHz is a valid SampleRate");
    let ctx = PrepareContext::new(sample_rate, BLOCK_SIZE, ChannelConfig::Stereo)
        .expect("BLOCK_SIZE is nonzero");

    // --- Assemble the real six-stage chain (this file's own doc comment explains why this can't
    // just call `build_default_chain`), keeping the concrete `NamStage`/`IrStage` types long
    // enough to load real resources into them below.
    let mut gate_stage: GateStage = GatePrep.prepare(&ctx).expect("GatePrep::prepare");
    let trim_stage = TrimPrep.prepare(&ctx).expect("TrimPrep::prepare");
    let mut nam_stage: NamStage = NamPrep.prepare(&ctx).expect("NamPrep::prepare");
    let mut ir_stage: IrStage = IrPrep.prepare(&ctx).expect("IrPrep::prepare");
    let mut eq_stage: EqStage = EqPrep.prepare(&ctx).expect("EqPrep::prepare");
    let out_stage = OutPrep.prepare(&ctx).expect("OutPrep::prepare");

    // --- Load a real "standard" WaveNet model (D-19.1: generated, fixed seed, never captured)
    // through the real `namir_nam::load` parser, exactly as a future M4 worker would.
    let nam_model = generate(WaveNetShape::Standard, NAM_SEED)
        .expect("standard WaveNet fixture should generate");
    let nam_bytes = nam_model.to_json_bytes();
    let prepared_nam =
        Arc::new(namir_nam::load(&nam_bytes).expect("generated WaveNet fixture should load"));
    nam_stage.load_model(prepared_nam);

    // --- Load a real 2 s stereo IR through the real `PreparedIr::from_wav_bytes`, at this
    // chain's own block size (matching `IrPrep::prepare`'s sizing and every other caller's
    // convention, e.g. `stages/ir.rs`'s own tests).
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

    // --- Activate gate + EQ (NFR-PERF-010's literal condition names both "active"): explicit
    // ENABLED=on (already the descriptor default for both, per `gate.rs`/`eq.rs`'s own `prepare`,
    // but set here anyway so this doesn't silently depend on that default never changing) plus
    // one real non-default value each, so real per-sample DSP work happens rather than a
    // bypassed/identity passthrough. Same `ParamId`-wrapping pattern `stages/gate.rs`/`eq.rs`
    // themselves use: `namir_params`'s stable id, reinterpreted as this crate's own `ParamId`.
    let gate_enabled_id = ParamId(gate::ENABLED.id.0);
    let gate_threshold_id = ParamId(gate::THRESHOLD_DB.id.0);
    let eq_enabled_id = ParamId(eq::ENABLED.id.0);
    let eq_low_shelf_gain_id = ParamId(eq::LOW_SHELF_GAIN_DB.id.0);

    gate_stage.apply(ParamChange {
        id: gate_enabled_id,
        value: 1.0, // Stepped index 1 == "On" (ParamChange's own doc comment).
    });
    gate_stage.apply(ParamChange {
        id: gate_threshold_id,
        value: GATE_THRESHOLD_DB,
    });

    eq_stage.apply(ParamChange {
        id: eq_enabled_id,
        value: 1.0,
    });
    eq_stage.apply(ParamChange {
        id: eq_low_shelf_gain_id,
        value: EQ_LOW_SHELF_GAIN_DB,
    });

    // --- Box into the real chain, in the real runtime order (`stages/mod.rs`'s doc comment:
    // "gate before trim", D-9.8), and turn on the same cross-cutting features
    // `build_default_chain` does (FR-CHAIN-030/080/090).
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

    let mut left = vec![0f32; BLOCK_SIZE];
    let mut right = vec![0f32; BLOCK_SIZE];
    let mut rng_state = 0xC0DE_CAFEu64 ^ 0x9E37_79B9_7F4A_7C15;

    for _ in 0..WARMUP_BLOCKS {
        gen_block(&mut rng_state, &mut left);
        right.copy_from_slice(&left);
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, BLOCK_SIZE);
        chain.process(&mut io);
        std::hint::black_box(io.channel(0));
    }

    let mut durations_ns = Vec::with_capacity(MEASURED_BLOCKS);
    for _ in 0..MEASURED_BLOCKS {
        gen_block(&mut rng_state, &mut left);
        right.copy_from_slice(&left);
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, BLOCK_SIZE);
        let start = Instant::now();
        chain.process(&mut io);
        let elapsed = start.elapsed();
        std::hint::black_box(io.channel(0));
        durations_ns.push(elapsed.as_nanos() as u64);
    }

    assert_eq!(
        chain.fault_count(),
        0,
        "the measured run must not have hit FR-CHAIN-080's NaN/Inf fault path"
    );

    durations_ns.sort_unstable();
    let p50 = percentile(&durations_ns, 0.50);
    let p99 = percentile(&durations_ns, 0.99);
    let p999 = percentile(&durations_ns, 0.999);
    let max = *durations_ns.last().unwrap();

    let block_period_ns = (BLOCK_SIZE as f64 / SAMPLE_RATE_F64 * 1e9) as u64;

    println!("=== NFR-PERF-010: REAL six-stage chain (gate -> trim -> nam -> ir -> eq -> out) ===");
    println!(
        "48 kHz, {BLOCK_SIZE}-sample blocks, standard WaveNet, 2 s stereo IR, gate + EQ active"
    );
    println!("*** NOT the certified reference-machine figure -- see this file's doc comment ***");
    println!(
        "*** measured on this sandbox's 4-core Intel Xeon @ 2.10 GHz, NOT the pinned AMD Ryzen 9 \
         5950X reference machine (docs/02-architecture.md §2) -- directional evidence only ***"
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

    let p999_pct = p999 as f64 / block_period_ns as f64 * 100.0;
    println!();
    println!(
        "NFR-PERF-010 figure (single core, 99.9th percentile, THIS SANDBOX, not the reference \
         machine): {p999_pct:.2}% of one core (budget: {NFR_PERF_010_BUDGET_PCT:.0}%)"
    );
    if p999_pct <= NFR_PERF_010_BUDGET_PCT {
        println!(
            "PASS (this sandbox only) -- {p999_pct:.2}% <= {NFR_PERF_010_BUDGET_PCT:.0}% budget. \
             This is NOT a certified NFR-PERF-010 sign-off; §7 requires re-running this binary on \
             the pinned reference machine before that requirement can close."
        );
    } else {
        println!(
            "FAIL (this sandbox only) -- {p999_pct:.2}% > {NFR_PERF_010_BUDGET_PCT:.0}% budget. \
             Even accounting for this sandbox's weaker single-core performance relative to the \
             reference machine, this is evidence the real assembled chain has not yet closed \
             NFR-PERF-010 -- see docs/03-implementation-roadmap.md §7 and docs/02-architecture.md \
             §22's R-4/R-8 rows."
        );
    }
}
