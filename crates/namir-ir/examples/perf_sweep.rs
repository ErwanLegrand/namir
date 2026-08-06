//! R-8 (`docs/03-implementation-roadmap.md` §7): the comparative pass across S-2's own grid (IR
//! lengths 0.1-10 s, block sizes 32-2048, rates 44.1-192 kHz), run against the *real*
//! `namir_ir::PreparedIr` (not the spike's standalone copy) via its public
//! [`PreparedIr::from_wav_bytes_with_schedule`] API. Mirrors
//! `spikes/s2-ir-convolution/src/bin/sweep.rs`'s methodology and constants exactly: a wall-
//! clock-budgeted (not fixed) block count per combo, because per-block cost spans sub-microsecond
//! to multi-millisecond across this grid and there are 56 combos. This pass's job is the same as
//! the spike's -- comparative, to flag worst-case combos -- not the certified figure; see
//! `perf_bench.rs` for that.
//!
//! **Sample rate decouples from raw cost** (ported reasoning from the spike, `docs/
//! 02-architecture.md` §19): the convolver has no notion of Hz, only samples, so this sweep
//! varies IR length directly in samples (the same 8 lengths the spike used, spanning what
//! 0.1-10 s at 44.1-192 kHz actually produces) and re-derives each rate's percentage from the
//! raw nanosecond figure afterwards. The WAV fixture itself is written at [`ENGINE_HZ`] and
//! loaded at the same engine rate, so `from_wav_bytes_with_schedule` never resamples -- the
//! measurement isolates convolution cost, not `resample_mono`'s cost.
//!
//! Usage: `cargo run -p namir-ir --release --example perf_sweep > sweep.csv 2> sweep_summary.txt`

use namir_core::SampleRate;
use namir_fixtures::ir::decaying_noise;
use namir_ir::{DEFAULT_GROWTH_FACTOR, DEFAULT_MAX_PARTITION, PreparedIr};
use std::time::Instant;

const MICRO_CALIBRATION_BLOCKS: usize = 16;
const TARGET_WARMUP_MS: f64 = 15.0;
const TARGET_MEASURED_MS: f64 = 120.0;
const MIN_WARMUP_BLOCKS: usize = 20;
const MAX_WARMUP_BLOCKS: usize = 2_000;
const MIN_BLOCKS: usize = 300;
const MAX_BLOCKS: usize = 100_000;
/// The rate the synthetic WAV fixture is written at, and the `engine_rate` it's loaded at -- kept
/// equal so `PreparedIr::from_wav_bytes_with_schedule` does zero resampling work, isolating the
/// convolution cost this sweep is measuring (see the module doc comment).
const ENGINE_HZ: u32 = 48_000;

fn percentile(sorted_nanos: &[u64], p: f64) -> u64 {
    let idx = ((sorted_nanos.len() as f64 - 1.0) * p).round() as usize;
    sorted_nanos[idx]
}

/// Writes `samples` as a 32-bit-float mono WAV, the smallest-ceremony format `wav::decode`
/// accepts that also round-trips `f32` taps exactly (no int quantization noise to reason about).
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

/// Runs one (IR, block_size) combination through the real `PreparedIr::process_block` and returns
/// `(p50_ns, p999_ns, max_ns, measured_blocks)`. Calibration/warmup/measurement structure ported
/// near-verbatim from the spike's `sweep.rs::run_one`.
fn run_one(h: &[f32], block_size: usize) -> (u64, u64, u64, usize) {
    let bytes = write_mono_wav(ENGINE_HZ, h);
    let engine_rate = SampleRate::new(ENGINE_HZ).unwrap();
    let prepared = PreparedIr::from_wav_bytes_with_schedule(
        &bytes,
        engine_rate,
        block_size,
        DEFAULT_GROWTH_FACTOR,
        DEFAULT_MAX_PARTITION,
    )
    .expect("synthetic fixture WAV always decodes");
    let mut state = prepared.new_state();

    let mut rng_seed = 0x5EED_0000u64 ^ (block_size as u64) ^ ((h.len() as u64) << 20);
    let mut input = vec![0f32; block_size];
    let mut output = vec![0f32; block_size];
    let mut gen_block = |input: &mut [f32]| {
        for v in input.iter_mut() {
            rng_seed ^= rng_seed << 13;
            rng_seed ^= rng_seed >> 7;
            rng_seed ^= rng_seed << 17;
            *v = ((rng_seed >> 40) as i32 as f32) / (1i64 << 23) as f32;
        }
    };
    let mut process = |input: &[f32], output: &mut [f32]| {
        let mut out_slice: &mut [f32] = output;
        prepared.process_block(&mut state, input, std::slice::from_mut(&mut out_slice));
    };

    let calib_start = Instant::now();
    for _ in 0..MICRO_CALIBRATION_BLOCKS {
        gen_block(&mut input);
        process(&input, &mut output);
        std::hint::black_box(&output);
    }
    let calib_elapsed_ms = calib_start.elapsed().as_secs_f64() * 1000.0;
    let per_block_ms = (calib_elapsed_ms / MICRO_CALIBRATION_BLOCKS as f64).max(1e-6);

    let warmup_blocks =
        ((TARGET_WARMUP_MS / per_block_ms) as usize).clamp(MIN_WARMUP_BLOCKS, MAX_WARMUP_BLOCKS);
    for _ in 0..warmup_blocks {
        gen_block(&mut input);
        process(&input, &mut output);
        std::hint::black_box(&output);
    }

    let measured_blocks =
        ((TARGET_MEASURED_MS / per_block_ms) as usize).clamp(MIN_BLOCKS, MAX_BLOCKS);
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
    let p999 = percentile(&durations_ns, 0.999);
    let max = *durations_ns.last().unwrap();
    let p50 = percentile(&durations_ns, 0.5);
    (p50, p999, max, measured_blocks)
}

fn main() {
    if let Some(id) = core_affinity::get_core_ids().and_then(|ids| ids.into_iter().next()) {
        core_affinity::set_for_current(id);
    }

    // Identical to the spike's own list: the two boundary extremes of 0.1-10 s at 44.1-192 kHz,
    // plus the NFR-PERF-010/FR-IR-050-relevant points in between.
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

    println!("ir_label,ir_len,block_size,measured_blocks,p50_ns,p999_ns,max_ns");

    let mut worst: (u64, String) = (0, String::new());
    for &(ir_label, ir_len) in &ir_lens {
        let h = decaying_noise(ir_len, 0xC0FF_EE00 ^ ir_len as u64, ir_len as f64 / 6.0);
        for &block_size in &block_sizes {
            let (p50, p999, max, measured_blocks) = run_one(&h, block_size);
            println!("{ir_label},{ir_len},{block_size},{measured_blocks},{p50},{p999},{max}");
            if max > worst.0 {
                worst = (max, format!("ir={ir_label} block={block_size}"));
            }
        }
    }

    eprintln!();
    eprintln!(
        "=== worst observed max per-block cost across the grid: {} ns ({}) ===",
        worst.0, worst.1
    );
}
