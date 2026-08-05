//! Computes the FR-NAM-030 accuracy figure: the RMS of (rust - reference), expressed in dB
//! below the RMS of the reference, over the whole signal and per FR-NAM-030 segment (clean /
//! transient / saturated, per `generate_fixture.rs`'s 3s/2s/5s layout at 48 kHz).
//!
//! Usage: `compare <reference.wav> <candidate.wav>`

use std::env;
use std::path::Path;

fn read_wav_mono_f32(path: &Path) -> (Vec<f32>, u32) {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    let spec = reader.spec();
    assert_eq!(spec.channels, 1, "wav must be mono");
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

fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&v| (v as f64) * (v as f64)).sum();
    (sum_sq / samples.len() as f64).sqrt()
}

/// Reports the FR-NAM-030 figure for one range: RMS(candidate - reference) in dB below
/// RMS(reference). Returns `None` (and prints a note) if the reference is silent in this range,
/// since dB-below-reference is undefined for zero reference RMS.
fn report_range(label: &str, reference: &[f32], candidate: &[f32]) -> Option<f64> {
    let diff: Vec<f32> = reference
        .iter()
        .zip(candidate)
        .map(|(&r, &c)| c - r)
        .collect();
    let ref_rms = rms(reference);
    let diff_rms = rms(&diff);
    if ref_rms <= 0.0 {
        println!("{label}: reference RMS is zero, skipping (undefined dB figure)");
        return None;
    }
    let db = 20.0 * (diff_rms / ref_rms).log10();
    println!(
        "{label}: reference RMS={ref_rms:.6e}, diff RMS={diff_rms:.6e}, error = {db:.2} dB below reference"
    );
    Some(db)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: compare <reference.wav> <candidate.wav>");
        std::process::exit(2);
    }
    let (reference, ref_sr) = read_wav_mono_f32(Path::new(&args[1]));
    let (candidate, cand_sr) = read_wav_mono_f32(Path::new(&args[2]));
    assert_eq!(
        ref_sr, cand_sr,
        "sample rate mismatch between reference and candidate"
    );

    let n = reference.len().min(candidate.len());
    if reference.len() != candidate.len() {
        eprintln!(
            "warning: length mismatch (reference {} vs candidate {}), comparing the first {n} samples",
            reference.len(),
            candidate.len()
        );
    }
    let reference = &reference[..n];
    let candidate = &candidate[..n];

    let sr = ref_sr as usize;
    let clean_end = (3 * sr).min(n);
    let transient_end = (5 * sr).min(n);

    println!("=== FR-NAM-030 accuracy: candidate vs reference ===");
    report_range(
        "clean [0-3s]",
        &reference[..clean_end],
        &candidate[..clean_end],
    );
    report_range(
        "transient [3-5s]",
        &reference[clean_end..transient_end],
        &candidate[clean_end..transient_end],
    );
    report_range(
        "saturated [5-10s]",
        &reference[transient_end..n],
        &candidate[transient_end..n],
    );
    let overall = report_range("overall", reference, candidate);

    if let Some(db) = overall {
        println!();
        println!("FR-NAM-030 figure: {:.2} dB", db);
    }
}
