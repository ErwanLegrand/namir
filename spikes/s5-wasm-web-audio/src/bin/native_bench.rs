//! Native reference figures, for the wasm-vs-native ratio. Same harness, same
//! fixtures, same 128-frame block; the only difference is the clock.
//!
//! Also writes the fixture bytes and a reference render, which the wasm side
//! loads for the output-parity check.

use std::time::Instant;

use namir_fixtures::ir::{decaying_noise, to_stereo_wav_bytes};
use namir_fixtures::nam::{A2Shape, WaveNetShape, generate, generate_a2};
use s5_wasm_web_audio::harness::{
    BLOCK_SIZE, Harness, SAMPLE_RATE, Signal, Stats, set_flush_to_zero,
};

const SEED: u64 = 30;
const REPS: usize = 5;
const PARITY_SAMPLES: usize = BLOCK_SIZE * 256;
const WARMUP: u32 = 5_000;
const MEASURED: u32 = 100_000;
/// The census is a correctness pass, not a timing one; 20 000 blocks is 5x
/// `TAIL_PERIOD_BLOCKS` x 7.8 and plenty to characterise the cycle.
const CENSUS_BLOCKS: u32 = 20_000;
const SIGNALS: [Signal; 3] = [
    Signal::Steady,
    Signal::AmplitudeDecay,
    Signal::SubnormalTail,
];

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

fn ir_bytes(rate: u32) -> Vec<u8> {
    let len = (rate as usize) * 2; // 2 seconds
    let l = decaying_noise(len, SEED, rate as f64 * 0.25);
    let r = decaying_noise(len, SEED + 1, rate as f64 * 0.25);
    to_stereo_wav_bytes(&l, &r, rate)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pin_to_measurement_core();
    // Fails loudly if IR_PERIOD_BLOCKS no longer matches the real partition schedule.
    Harness::assert_ir_period(48_000 * 2);
    std::fs::create_dir_all("fixtures")?;

    let a1 = generate(WaveNetShape::Standard, SEED)?.to_json_bytes();
    let a2 = generate_a2(A2Shape::Lite, SEED)?.to_json_bytes();
    let ir48 = ir_bytes(48_000);
    let ir441 = ir_bytes(44_100);
    std::fs::write("fixtures/a1_standard.nam", &a1)?;
    std::fs::write("fixtures/a2_lite.nam", &a2)?;
    std::fs::write("fixtures/ir_48k.wav", &ir48)?;
    std::fs::write("fixtures/ir_44k1.wav", &ir441)?;

    // Reference render for the wasm parity check: deterministic input, left channel out.
    let mut h = Harness::new(SAMPLE_RATE, BLOCK_SIZE)?;
    h.load_nam(&a1)?;
    h.load_ir(&ir48)?;
    let input: Vec<f32> = (0..PARITY_SAMPLES)
        .map(|i| ((i as f32) * 0.01).sin() * 0.5)
        .collect();
    let mut reference = vec![0.0f32; PARITY_SAMPLES];
    h.render(&input, &mut reference);
    let mut raw = Vec::with_capacity(PARITY_SAMPLES * 4);
    for s in &reference {
        raw.extend_from_slice(&s.to_le_bytes());
    }
    // `--render-only <path>` writes just this render and stops. That is how the
    // native-vs-native *control* render is produced: build a second time into an
    // isolated CARGO_TARGET_DIR with different codegen flags and point it at a
    // different path. Without the flag a control build would also sit through the
    // 20-configuration bench for a file it writes in the first second.
    let render_only = std::env::args().skip_while(|a| a != "--render-only").nth(1);
    let render_path = render_only
        .clone()
        .unwrap_or_else(|| "fixtures/reference_render_f32le.bin".to_string());
    std::fs::write(&render_path, &raw)?;
    if render_only.is_some() {
        println!("wrote {render_path} ({} samples)", PARITY_SAMPLES);
        return Ok(());
    }

    // ---- Task 5: prove the premise before pricing it -------------------------------
    //
    // The denormal sub-experiment is only meaningful if subnormals actually occur. This
    // pass is untimed and reports what each signal numerically produced -- output-side
    // subnormal counts, and (x86-64 only) whether the CPU itself raised MXCSR's
    // Denormal-operand / Underflow status bits inside `process_block`, which is the only
    // witness that sees state the harness cannot read back.
    println!("-- census: FTZ/DAZ off --");
    for (name, model) in [("a1_standard", &a1), ("a2_lite", &a2)] {
        for signal in SIGNALS {
            let mut h = Harness::new(SAMPLE_RATE, BLOCK_SIZE)?;
            h.load_nam(model)?;
            h.load_ir(&ir48)?;
            let c = h.census(WARMUP, CENSUS_BLOCKS, signal);
            println!(
                "census {name} {:<9}: blocks {} | sub-out blocks {} ({:.1}%) | sub-out samples {} | DE blocks {} ({:.1}%) | UE blocks {} | min |x| {:e}",
                signal.label(),
                c.blocks,
                c.subnormal_output_blocks,
                100.0 * c.subnormal_output_blocks as f64 / c.blocks as f64,
                c.subnormal_output_samples,
                c.denormal_flag_blocks,
                100.0 * c.denormal_flag_blocks as f64 / c.blocks as f64,
                c.underflow_flag_blocks,
                c.min_abs_nonzero,
            );
        }
    }

    // Same census with FTZ/DAZ installed. Every count above that is a real subnormal must
    // collapse here; anything that does not was never a subnormal effect. This is the
    // control for the census itself, and it is also what prices the guard below.
    assert!(
        set_flush_to_zero(true),
        "FTZ/DAZ not settable on this target -- the guard-pricing half of Task 5 cannot run"
    );
    println!("-- census: FTZ/DAZ on --");
    for (name, model) in [("a1_standard", &a1), ("a2_lite", &a2)] {
        for signal in SIGNALS {
            let mut h = Harness::new(SAMPLE_RATE, BLOCK_SIZE)?;
            h.load_nam(model)?;
            h.load_ir(&ir48)?;
            let c = h.census(WARMUP, CENSUS_BLOCKS, signal);
            println!(
                "census-ftz {name} {:<9}: sub-out blocks {} | sub-out samples {} | DE blocks {} | UE blocks {} | min |x| {:e}",
                signal.label(),
                c.subnormal_output_blocks,
                c.subnormal_output_samples,
                c.denormal_flag_blocks,
                c.underflow_flag_blocks,
                c.min_abs_nonzero,
            );
        }
    }
    set_flush_to_zero(false);

    // ---- timing ---------------------------------------------------------------------
    for ftz in [false, true] {
        assert!(
            set_flush_to_zero(ftz),
            "FTZ/DAZ not settable on this target"
        );
        let tag = if ftz { "native-ftz" } else { "native" };
        // All three signals under both settings. The amplitude-decay mode gets a
        // guard-on figure too, deliberately: the census found it *does* produce
        // subnormals (contrary to what Task 2's correction assumed), so its ~40% cost
        // rise can only be split into "amplitude" and "denormal" halves by measuring it
        // with the guard installed.
        for (name, model) in [("a1_standard", &a1), ("a2_lite", &a2)] {
            for &signal in SIGNALS.iter() {
                for rep in 1..=REPS {
                    let mut h = Harness::new(SAMPLE_RATE, BLOCK_SIZE)?;
                    h.load_nam(model)?;
                    h.load_ir(&ir48)?;
                    let clock = || {
                        let d = Instant::now().duration_since(EPOCH.with(|e| *e));
                        d.as_nanos() as f64 / 1000.0
                    };
                    let s: Stats = h.run(WARMUP, MEASURED, signal, &clock);
                    println!(
                        "{tag} {name} {:<9} rep {rep}/{REPS}: p50 {:.2}% | p99 {:.2}% | p99.9 {:.2}% | max {:.2}% | estimator {:.2}% | {}",
                        signal.label(),
                        s.p50,
                        s.p99,
                        s.p999,
                        s.max,
                        s.estimator,
                        if s.is_quotable() {
                            "quotable"
                        } else {
                            "CONTAMINATED"
                        }
                    );
                }
            }
        }
    }
    set_flush_to_zero(false);
    Ok(())
}

thread_local! {
    static EPOCH: Instant = Instant::now();
}
