//! Generates the S-1 spike's test fixtures: a "standard"-shaped WaveNet `.nam` file with
//! seeded, constrained-init weights (D-19.1's accepted fallback to training — see the spike
//! README), and a 10-second 48 kHz mono test signal with clean, transient and saturated
//! material (FR-NAM-030). Deterministic from `--seed`; nothing here is captured audio.
//!
//! Usage: `generate-fixture <out-dir> [--seed N]`

use rand::Rng;
use rand::SeedableRng;
use s1_nam_inference::{NamFile, PreparedWaveNet};
use std::env;
use std::f32::consts::PI;
use std::path::Path;

const SAMPLE_RATE: u32 = 48_000;

struct ArraySpec {
    input_size: usize,
    condition_size: usize,
    head_size: usize,
    channels: usize,
    kernel_size: usize,
    dilations: Vec<usize>,
    head_bias: bool,
}

/// The "standard" NAM WaveNet shape: 2 layer arrays, channels 16/8, kernel_size 3, dilations
/// 1..512 (10 layers) each, ungated, Tanh, head_scale ~0.02. Confirmed against
/// `neural-amp-modeler`'s `nam/train/core.py` (v0.10.0) `get_wavenet_config` — see README.
fn standard_shape() -> Vec<ArraySpec> {
    let dilations: Vec<usize> = (0..10).map(|i| 1usize << i).collect(); // 1,2,4,...,512
    vec![
        ArraySpec {
            input_size: 1,
            condition_size: 1,
            head_size: 8,
            channels: 16,
            kernel_size: 3,
            dilations: dilations.clone(),
            head_bias: false,
        },
        ArraySpec {
            // Chains from array 1's *channels* (16), not its head_size (8) — the residual
            // "trunk" output is what feeds the next array's rechannel, confirmed by reading
            // NeuralAmpModelerCore's WaveNet::process/LayerArray::ProcessInner directly (see
            // README). head_size(prev)=8 separately seeds this array's head accumulator and
            // must match this array's `channels`, which it does (8).
            input_size: 16,
            condition_size: 1,
            head_size: 1,
            channels: 8,
            kernel_size: 3,
            dilations,
            head_bias: true,
        },
    ]
}

fn push_uniform(rng: &mut impl Rng, out: &mut Vec<f32>, count: usize, scale: f32) {
    for _ in 0..count {
        out.push(rng.gen_range(-scale..scale));
    }
}

fn push_zeros(out: &mut Vec<f32>, count: usize) {
    out.resize(out.len() + count, 0.0);
}

/// Builds the flat weight array in exactly the order `PreparedWaveNet::from_nam_file` consumes
/// it (confirmed against `NeuralAmpModelerCore`'s `WaveNet::set_weights_` — see README):
/// per array [rechannel(w, no bias), per-layer[dilated(w,b), mixin(w), residual(w,b)], head_rechannel],
/// then a trailing head_scale float.
fn build_weights(specs: &[ArraySpec], rng: &mut impl Rng, head_scale: f32) -> Vec<f32> {
    let mut w = Vec::new();
    for spec in specs {
        // Rechannel: input_size -> channels, NO bias (confirmed: NeuralAmpModelerCore
        // constructs `_rechannel` as `Conv1x1(input_size, channels, /*bias=*/false)`).
        let s = 1.0 / (spec.input_size as f32).sqrt();
        push_uniform(rng, &mut w, spec.channels * spec.input_size, s);

        for _ in &spec.dilations {
            // Dilated conv: channels -> channels, kernel_size taps, with bias.
            let s = 1.0 / ((spec.channels * spec.kernel_size) as f32).sqrt();
            push_uniform(
                rng,
                &mut w,
                spec.channels * spec.channels * spec.kernel_size,
                s,
            );
            push_zeros(&mut w, spec.channels);

            // Input-mixin: condition_size -> channels, no bias.
            let s = 1.0 / (spec.condition_size as f32).sqrt();
            push_uniform(rng, &mut w, spec.channels * spec.condition_size, s);

            // Residual (layer1x1): channels -> channels, with bias.
            let s = 1.0 / (spec.channels as f32).sqrt();
            push_uniform(rng, &mut w, spec.channels * spec.channels, s);
            push_zeros(&mut w, spec.channels);
        }

        // Head-rechannel: channels -> head_size, bias iff head_bias.
        let s = 1.0 / (spec.channels as f32).sqrt();
        push_uniform(rng, &mut w, spec.head_size * spec.channels, s);
        if spec.head_bias {
            push_zeros(&mut w, spec.head_size);
        }
    }
    w.push(head_scale);
    w
}

fn build_nam_json(specs: &[ArraySpec], weights: &[f32], head_scale: f32) -> serde_json::Value {
    let layers: Vec<_> = specs
        .iter()
        .map(|s| {
            serde_json::json!({
                "input_size": s.input_size,
                "condition_size": s.condition_size,
                "head_size": s.head_size,
                "channels": s.channels,
                "kernel_size": s.kernel_size,
                "dilations": s.dilations,
                "activation": "Tanh",
                "gated": false,
                "head_bias": s.head_bias,
            })
        })
        .collect();

    serde_json::json!({
        "version": "0.5.5",
        "architecture": "WaveNet",
        "config": {
            "layers": layers,
            "head_scale": head_scale,
            "head": null,
        },
        "weights": weights,
        "sample_rate": SAMPLE_RATE,
        "metadata": {
            "name": "S-1 spike fixture (generated, not captured)",
            "modeled_by": "namir S-1 spike generator",
            "gear_type": "amp",
            "tone_type": "clean",
            "description": "Seeded, constrained-init WaveNet. Not a trained model: parity is a \
                property of architecture and weights, tonal realism is irrelevant (D-19.1)."
        }
    })
}

/// Runs the model over a calibration probe and returns the output RMS, to check the fixture
/// isn't near-silent or divergent (D-19.1's explicit hazard) before trusting it.
fn measure_output_rms(nam_value: &serde_json::Value, probe: &[f32]) -> f32 {
    let nam: NamFile = serde_json::from_value(nam_value.clone()).expect("valid NamFile JSON");
    let prepared = PreparedWaveNet::from_nam_file(&nam).expect("valid WaveNet weights");
    let mut state = prepared.new_state(64);
    let mut sum_sq = 0f64;
    for chunk in probe.chunks(64) {
        let out = prepared.process_block(&mut state, chunk);
        for &v in &out {
            sum_sq += (v as f64) * (v as f64);
        }
    }
    ((sum_sq / probe.len() as f64).sqrt()) as f32
}

fn calibration_probe(seed: u64) -> Vec<f32> {
    let mut rng = rand_pcg::Pcg64::seed_from_u64(seed ^ 0xC411_B4A7);
    let n = SAMPLE_RATE as usize; // 1s
    (0..n)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            0.3 * (2.0 * PI * 220.0 * t).sin() + 0.05 * rng.gen_range(-1.0f32..1.0)
        })
        .collect()
}

/// Builds the FR-NAM-030 10-second test signal: clean, transient, then saturated material.
fn build_test_signal(seed: u64) -> Vec<f32> {
    let mut rng = rand_pcg::Pcg64::seed_from_u64(seed);
    let mut sig = Vec::with_capacity(10 * SAMPLE_RATE as usize);

    // 0-3s: clean, low level. A few decaying plucked-string-like tones (guitar-relevant
    // frequencies) summed, kept well below saturation.
    let clean_freqs = [82.41, 110.0, 220.0, 440.0, 880.0]; // low E .. two octaves up
    for i in 0..(3 * SAMPLE_RATE) {
        let t = i as f32 / SAMPLE_RATE as f32;
        let mut s = 0.0f32;
        for (k, &f) in clean_freqs.iter().enumerate() {
            let onset = k as f32 * 0.5;
            let local_t = t - onset;
            if local_t >= 0.0 {
                let env = (-local_t * 1.5).exp();
                s += 0.08 * env * (2.0 * PI * f * local_t).sin();
            }
        }
        sig.push(s.clamp(-1.0, 1.0));
    }

    // 3-5s: transients. Sharp clicks plus fast-attack/slower-decay "plucks" at moderate level.
    for i in 0..(2 * SAMPLE_RATE) {
        let t = i as f32 / SAMPLE_RATE as f32;
        let mut s = 0.0f32;
        // A click every 250ms.
        let click_period = 0.25f32;
        let phase = t % click_period;
        if phase < 1.0 / SAMPLE_RATE as f32 {
            s += 0.6;
        }
        // A decaying pluck every 500ms at 330 Hz.
        let pluck_period = 0.5f32;
        let pluck_phase = t % pluck_period;
        let env = (-pluck_phase * 8.0).exp();
        s += 0.5 * env * (2.0 * PI * 330.0 * pluck_phase).sin();
        sig.push(s.clamp(-1.0, 1.0));
    }

    // 5-10s: saturated, high level. Band-limited (simple leaky integrator smoothed) noise at
    // high amplitude plus a loud tone, deliberately driving the nonlinearity hard.
    let mut lp_state = 0f32;
    for i in 0..(5 * SAMPLE_RATE) {
        let t = i as f32 / SAMPLE_RATE as f32;
        let white: f32 = rng.gen_range(-1.0..1.0);
        lp_state += 0.15 * (white - lp_state); // crude low-pass to avoid pure hiss
        let tone = (2.0 * PI * 196.0 * t).sin(); // low G, a heavy-riff-relevant note
        let s = 0.9 * lp_state + 0.5 * tone;
        sig.push(s.clamp(-1.0, 1.0));
    }

    sig
}

/// Writes plain 16-bit PCM (canonical 16-byte fmt chunk, format tag 1). Both 32-bit float and
/// 24-bit int make hound emit a `WAVE_FORMAT_EXTENSIBLE` fmt chunk, which
/// `NeuralAmpModelerCore`'s bundled WAV reader (`AudioDSPTools/dsp/wav.cpp`) rejects (it wants
/// a `fact` chunk before `data` for extensible files; hound doesn't write one there). 16-bit
/// quantization of the *shared input* doesn't cap the Rust-vs-C++ comparison's precision: both
/// renderers parse the same input samples to the same float value, so it's common-mode, not an
/// error source between the two implementations.
fn write_wav_mono_f32(path: &Path, samples: &[f32], sample_rate: u32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create wav");
    let scale = (1i32 << 15) as f32;
    for &s in samples {
        let v = (s * scale).round().clamp(-(scale), scale - 1.0) as i32;
        writer.write_sample(v as i16).expect("write sample");
    }
    writer.finalize().expect("finalize wav");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: generate-fixture <out-dir> [--seed N]");
        std::process::exit(2);
    }
    let out_dir = Path::new(&args[1]);
    std::fs::create_dir_all(out_dir).expect("create out dir");

    let mut seed: u64 = 0xA1_5E1_5E1_u64; // fixed default seed
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--seed" {
            seed = args[i + 1].parse().expect("--seed takes an integer");
            i += 2;
        } else {
            i += 1;
        }
    }

    let specs = standard_shape();
    let base_head_scale = 0.02f32;

    let mut rng = rand_pcg::Pcg64::seed_from_u64(seed);
    let weights = build_weights(&specs, &mut rng, base_head_scale);
    let nam_value = build_nam_json(&specs, &weights, base_head_scale);

    let probe = calibration_probe(seed);
    let measured_rms = measure_output_rms(&nam_value, &probe);
    println!("seed {seed}: uncalibrated output RMS = {measured_rms:.6e} (probe RMS ~0.22)");

    let target_rms = 0.15f32; // comparable order of magnitude to the probe's input level
    let calibrated_head_scale = if measured_rms.is_finite() && measured_rms > 1e-6 {
        base_head_scale * (target_rms / measured_rms)
    } else {
        panic!(
            "fixture is degenerate (RMS={measured_rms}) — near-silent or divergent, per D-19.1's \
             explicit hazard. Try a different --seed."
        );
    };

    let weights = {
        let mut w = weights;
        *w.last_mut().unwrap() = calibrated_head_scale;
        w
    };
    let nam_value = build_nam_json(&specs, &weights, calibrated_head_scale);
    let calibrated_rms = measure_output_rms(&nam_value, &probe);
    println!(
        "seed {seed}: calibrated head_scale = {calibrated_head_scale:.6e}, output RMS = {calibrated_rms:.6e}"
    );
    assert!(
        calibrated_rms.is_finite() && calibrated_rms > 1e-4 && calibrated_rms < 10.0,
        "calibration failed to produce a sane RMS ({calibrated_rms})"
    );

    let nam_path = out_dir.join("model.nam");
    std::fs::write(&nam_path, serde_json::to_string_pretty(&nam_value).unwrap())
        .expect("write .nam");
    println!("wrote {}", nam_path.display());

    let test_signal = build_test_signal(seed);
    let wav_path = out_dir.join("test_signal.wav");
    write_wav_mono_f32(&wav_path, &test_signal, SAMPLE_RATE);
    println!(
        "wrote {} ({} samples, {:.1}s @ {} Hz)",
        wav_path.display(),
        test_signal.len(),
        test_signal.len() as f32 / SAMPLE_RATE as f32,
        SAMPLE_RATE
    );
}
