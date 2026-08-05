//! Rust twin of `NeuralAmpModelerCore`'s `tools/render.cpp`: `render <model.nam> <input.wav>
//! <output.wav>`. Processes input in 64-sample blocks (NFR-PERF-010's block size) through
//! `PreparedWaveNet`, using a fresh `WaveNetState` per run — exercising exactly the immutable
//! weights / mutable per-instance state split D-9.1 requires.

use s1_nam_inference::{NamFile, PreparedWaveNet};
use std::env;
use std::path::Path;

const BLOCK_SIZE: usize = 64;

fn read_wav_mono_f32(path: &Path) -> (Vec<f32>, u32) {
    let mut reader = hound::WavReader::open(path).expect("open input wav");
    let spec = reader.spec();
    assert_eq!(spec.channels, 1, "input wav must be mono");
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.expect("sample") as f32 / max)
                .collect()
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.expect("sample"))
            .collect(),
    };
    (samples, spec.sample_rate)
}

fn write_wav_mono_f32(path: &Path, samples: &[f32], sample_rate: u32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create output wav");
    for &s in samples {
        writer.write_sample(s).expect("write sample");
    }
    writer.finalize().expect("finalize wav");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: render <model.nam> <input.wav> <output.wav>");
        std::process::exit(2);
    }
    let model_path = Path::new(&args[1]);
    let input_path = Path::new(&args[2]);
    let output_path = Path::new(&args[3]);

    eprintln!("Loading model [{}]", model_path.display());
    let nam_json = std::fs::read_to_string(model_path).expect("read .nam file");
    let nam: NamFile = serde_json::from_str(&nam_json).expect("parse .nam JSON");
    let prepared = PreparedWaveNet::from_nam_file(&nam).expect("build WaveNet from weights");
    eprintln!("Model loaded successfully");

    let (input, sample_rate) = read_wav_mono_f32(input_path);
    let mut state = prepared.new_state(BLOCK_SIZE);

    let mut output = Vec::with_capacity(input.len());
    for chunk in input.chunks(BLOCK_SIZE) {
        let out = prepared.process_block(&mut state, chunk);
        output.extend_from_slice(&out);
    }

    write_wav_mono_f32(output_path, &output, sample_rate);
    eprintln!(
        "Wrote {} samples to {}",
        output.len(),
        output_path.display()
    );
}
