//! Native reference figures, for the wasm-vs-native ratio. Same harness, same
//! fixtures, same 128-frame block; the only difference is the clock.
//!
//! Also writes the fixture bytes and a reference render, which the wasm side
//! loads for the output-parity check.

use std::time::Instant;

use namir_fixtures::ir::{decaying_noise, to_stereo_wav_bytes};
use namir_fixtures::nam::{A2Shape, WaveNetShape, generate, generate_a2};
use s5_wasm_web_audio::harness::{BLOCK_SIZE, Harness, SAMPLE_RATE, Signal, Stats};

const SEED: u64 = 30;
const REPS: usize = 5;
const PARITY_SAMPLES: usize = BLOCK_SIZE * 256;

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

    for (name, model) in [("a1_standard", &a1), ("a2_lite", &a2)] {
        for signal in [Signal::Steady, Signal::Decaying] {
            let label = if signal == Signal::Steady { "steady" } else { "decaying" };
            for rep in 1..=REPS {
                let mut h = Harness::new(SAMPLE_RATE, BLOCK_SIZE)?;
                h.load_nam(model)?;
                h.load_ir(&ir48)?;
                let clock = || {
                    let d = Instant::now().duration_since(EPOCH.with(|e| *e));
                    d.as_nanos() as f64 / 1000.0
                };
                let s: Stats = h.run(5_000, 100_000, signal, &clock);
                println!(
                    "native {name} {label} rep {rep}/{REPS}: p50 {:.2}% | p99 {:.2}% | \
                     p99.9 {:.2}% | max {:.2}% | estimator {:.2}% | {}",
                    s.p50,
                    s.p99,
                    s.p999,
                    s.max,
                    s.estimator,
                    if s.is_quotable() { "quotable" } else { "CONTAMINATED" }
                );
            }
        }
    }
    Ok(())
}

thread_local! {
    static EPOCH: Instant = Instant::now();
}
