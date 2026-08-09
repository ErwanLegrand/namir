//! `cargo run -p xtask -- nam-parity --model <path> --input <path> --reference <path>`: M10
//! Phase 4(a)'s tool for FR-NAM-030's other half (`docs/03-implementation-roadmap.md` §17,
//! "Phase 4"). Loads a real, locally-held `.nam` model, runs `namir_nam`'s inference over a WAV
//! probe, and compares the result against a WAV rendered by the real `NeuralAmpModelerCore`
//! reference build — printing the same RMS-in-f64, `20 * log10(rms_err / rms_ref)` figure
//! `crates/namir-nam/tests/fixtures.rs`'s
//! `numeric_parity_against_an_independent_reference_implementation` already uses, against
//! FR-NAM-030's own -90 dB floor.
//!
//! **Why this is an `xtask` subcommand and not a `#[test]`.** The models this closes the gap for —
//! the 67 real LSTM `.nam` files `docs/manual-tests/fr-nam-020-real-lstm-models.md` records — carry
//! no stated licence and cannot enter this repository; D-19.1's generated-never-captured rule
//! forbids a captured/licensed test asset independently of that licence question anyway. So this
//! cannot be a committed test with committed inputs: it is a tool a human runs locally against
//! files that never enter the repository, with the executed result recorded by hand in that
//! manual-test document, the same pattern every other human-verified finding in this project uses.
//!
//! # Argument parsing
//!
//! Strict, mirroring `traceability`'s own `parse_traceability_args` (see that function's comment
//! for why: an unrecognised flag here is a typo pointing at the wrong file, not a coverage
//! decision, and should be loud rather than silently ignored).
//!
//! # The skip-clean-when-absent outcome
//!
//! When any of `--model`/`--input`/`--reference` does not exist, this prints an explanatory line
//! and returns success — a clean skip, not a failure. This is a genuinely new outcome shape: every
//! other `xtask` subcommand's inputs are the repository itself, always present in any checkout, so
//! none of them has ever needed to distinguish "input missing" from "input wrong". This
//! subcommand's inputs are, by construction, files that must never be committed — a contributor's
//! clone with no local model library, or a CI runner, hitting an absent path is the *expected*
//! case, not a build break, so it must not fail the way a real parity mismatch does.

use std::path::{Path, PathBuf};

pub struct NamParityArgs {
    model: PathBuf,
    input: PathBuf,
    reference: PathBuf,
}

/// Parses `nam-parity`'s own argument list (everything after the `nam-parity` token). All three
/// flags are required and each takes exactly one value; any other flag, a flag with no value, or a
/// bare positional argument is refused outright — see the module doc comment for why this parses
/// strictly rather than the loose `any(|a| a == "--write")` pattern most other subcommands use.
pub fn parse_args(args: &[String]) -> Result<NamParityArgs, String> {
    let mut model = None;
    let mut input = None;
    let mut reference = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let slot = match arg.as_str() {
            "--model" => &mut model,
            "--input" => &mut input,
            "--reference" => &mut reference,
            other => {
                return Err(format!(
                    "nam-parity: unrecognised argument `{other}` (expected --model, --input and \
                     --reference, each followed by a path)"
                ));
            }
        };
        let Some(value) = iter.next() else {
            return Err(format!("nam-parity: `{arg}` needs a path argument"));
        };
        *slot = Some(PathBuf::from(value));
    }

    let (Some(model), Some(input), Some(reference)) = (model, input, reference) else {
        return Err(
            "nam-parity: missing required argument(s) -- usage: nam-parity --model <path> \
             --input <path> --reference <path>"
                .to_string(),
        );
    };

    Ok(NamParityArgs {
        model,
        input,
        reference,
    })
}

/// Reads a mono WAV file into `f32` samples normalised to `[-1.0, 1.0]`, accepting either 16-bit
/// PCM or 32-bit float (the two formats this tool's own doc / the roadmap's Phase 4 text names:
/// `render.exe`'s own WAV reader has a format-detection bug on 32-bit float input, so 16-bit PCM is
/// the reliable choice for the *input* side, but its own *output* is always 32-bit float).
fn read_mono_wav(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1 {
        return Err(format!(
            "{}: expected a mono WAV, found {} channel(s)",
            path.display(),
            spec.channels
        ));
    }

    let samples: Result<Vec<f32>, hound::Error> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let full_scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / full_scale))
                .collect()
        }
        hound::SampleFormat::Float => reader.samples::<f32>().collect(),
    };
    let samples = samples.map_err(|e| format!("{}: {e}", path.display()))?;

    Ok((samples, spec.sample_rate))
}

/// Runs the subcommand. Returns `true` for both a clean pass and a clean skip (files absent);
/// `false` only when the files exist but something about them is actually wrong -- a load failure,
/// a shape mismatch, or (implicitly, since it is not asserted here at all -- the figure is printed
/// for a human to read against FR-NAM-030's floor, this tool does not itself pass/fail the -90 dB
/// bar) a malformed WAV.
pub fn run(args: &NamParityArgs) -> bool {
    for (label, path) in [
        ("--model", &args.model),
        ("--input", &args.input),
        ("--reference", &args.reference),
    ] {
        if !path.is_file() {
            println!(
                "nam-parity: {label} path {} does not exist -- skipping cleanly (no local model \
                 library on this machine/CI runner is the expected case, not a failure; D-19.1 \
                 forbids committing the files this tool compares)",
                path.display()
            );
            return true;
        }
    }

    let model_bytes = match std::fs::read(&args.model) {
        Ok(b) => b,
        Err(e) => {
            println!("nam-parity: could not read {}: {e}", args.model.display());
            return false;
        }
    };
    let prepared = match namir_nam::load(&model_bytes) {
        Ok(p) => p,
        Err(e) => {
            println!("nam-parity: {} failed to load: {e}", args.model.display());
            return false;
        }
    };

    let (input, input_rate) = match read_mono_wav(&args.input) {
        Ok(v) => v,
        Err(e) => {
            println!("nam-parity: {e}");
            return false;
        }
    };
    let (reference, reference_rate) = match read_mono_wav(&args.reference) {
        Ok(v) => v,
        Err(e) => {
            println!("nam-parity: {e}");
            return false;
        }
    };

    let model_rate = prepared.sample_rate().hz();
    if input_rate != model_rate {
        println!(
            "nam-parity: warning -- input WAV sample rate ({input_rate} Hz) does not match \
             {}'s declared rate ({model_rate} Hz); FR-NAM-030's comparison assumes they agree",
            args.model.display()
        );
    }
    if reference_rate != input_rate {
        println!(
            "nam-parity: warning -- reference WAV sample rate ({reference_rate} Hz) does not \
             match the input WAV's ({input_rate} Hz)"
        );
    }

    let mut state = prepared.new_state(input.len().max(1));
    let ours = prepared.process(&mut state, &input);

    let len = ours.len().min(reference.len());
    if len == 0 {
        println!("nam-parity: empty output or reference -- nothing to compare");
        return false;
    }
    if ours.len() != reference.len() {
        println!(
            "nam-parity: warning -- our output is {} sample(s), the reference is {} sample(s); \
             comparing over the shared {len} sample(s)",
            ours.len(),
            reference.len()
        );
    }

    // Same formula as `crates/namir-nam/tests/fixtures.rs`'s
    // `numeric_parity_against_an_independent_reference_implementation`: RMS computed in f64 over
    // both signals, error expressed relative to the reference's own RMS in dB.
    let mut sum_sq_err = 0.0f64;
    let mut sum_sq_ref = 0.0f64;
    for i in 0..len {
        let d = f64::from(ours[i]) - f64::from(reference[i]);
        sum_sq_err += d * d;
        sum_sq_ref += f64::from(reference[i]) * f64::from(reference[i]);
    }
    let rms_err = (sum_sq_err / len as f64).sqrt();
    let rms_ref = (sum_sq_ref / len as f64).sqrt();

    if rms_ref <= 0.0 {
        println!(
            "nam-parity: reference signal has zero RMS -- cannot express error relative to it"
        );
        return false;
    }

    let db = 20.0 * (rms_err / rms_ref).log10();
    println!(
        "nam-parity: {} vs. {} ({len} samples compared): {db:.2} dB (FR-NAM-030 floor: -90 dB)",
        args.model.display(),
        args.reference.display()
    );
    if db < -90.0 {
        println!("nam-parity: within FR-NAM-030's -90 dB floor");
    } else {
        println!("nam-parity: does NOT meet FR-NAM-030's -90 dB floor");
    }

    true
}
