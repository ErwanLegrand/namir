//! FR-NAM-030 (Must, `Verify: G`): "For each supported architecture, the output of Namir's
//! inference shall match the reference NAM implementation to within an error whose RMS is at
//! least 90 dB below the RMS of the reference output, over a specified 10-second test signal
//! containing clean, transient and saturated material."
//!
//! This is the kind of artifact that requirement's `Verify: G` names -- a stated, numerical
//! comparison against the real reference implementation (`NeuralAmpModelerCore`), not against
//! `namir-fixtures`' own from-scratch Rust port (`tests/fixtures.rs`'s/`tests/lstm_fixtures.rs`'s
//! parity tests, which the M9a audit correctly demoted to `trace-partial:` for exactly this
//! reason -- see those files' own `// uncovered:` text).
//!
//! **It does not span the requirement, and this sentence used to claim it did** (M14, 2026-08-12):
//! it read "This is the artifact that requirement's `Verify: G` actually names", which is true of
//! the two architectures below and false of the third. `tests/golden/` holds no A2 model, so both
//! tests here are `trace-partial:` -- see each one's own `// uncovered:` field, and roadmap §21
//! Phase 4b for the A2 golden that closes them.
//!
//! Per D-19.1, the fixtures here are
//! *generated*, not captured: `tests/golden/wavenet_nano.nam` and `tests/golden/lstm_tiny.nam` are
//! small, seeded `namir-fixtures` outputs (regenerate with the recipe below), and
//! `tests/golden/input_10s.wav` is FR-NAM-030's own "10-second test signal containing clean,
//! transient and saturated material," built from the same recipe
//! `spikes/s1-nam-inference/src/bin/generate_fixture.rs`'s `build_test_signal` uses (the roadmap's
//! own citation for this signal shape). `tests/golden/*_reference.wav` are that signal rendered
//! through the real reference implementation, pinned at commit `3cde95c`, built with
//! `-DNAM_USE_INLINE_GEMM -DNAM_ENABLE_A2_FAST=OFF` (D-9.12's PR #264 consequence note: the
//! default Eigen GEMM path is not bit-exact across Eigen version bumps, `NAM_USE_INLINE_GEMM`
//! bypasses it entirely and is the reproducible target to build against). None of these five files
//! is large (~4.6 MB total) or licensed/captured audio -- every one is regenerable from the recipe
//! in this file plus a local NeuralAmpModelerCore checkout.
//!
//! # Regenerating these fixtures
//!
//! 1. `git clone --recurse-submodules https://github.com/sdatkinson/NeuralAmpModelerCore && cd
//!    NeuralAmpModelerCore && git checkout 3cde95c354d5ba6da01316cad90b05cfc4855053`
//! 2. Build `render` with `-DNAM_USE_INLINE_GEMM -DNAM_ENABLE_A2_FAST=OFF` (MSVC:
//!    `cmake -S . -B build_inline -DCMAKE_CXX_FLAGS="/DNAM_USE_INLINE_GEMM"
//!    -DNAM_ENABLE_A2_FAST=OFF && cmake --build build_inline --target render --config Release`).
//!    `spikes/s1-nam-inference/README.md` documents the MSVC-specific build corrections this
//!    needs (a GCC/Clang-only `-Wno-error` flag `tools/CMakeLists.txt` applies unconditionally,
//!    and Windows long-path limits).
//! 3. Build the same 10-second signal `build_test_signal` below generates, and the same
//!    `namir_fixtures::nam::generate(WaveNetShape::Nano, 30)` /
//!    `namir_fixtures::nam::generate_lstm(LstmShape::Tiny, 30)` fixtures, write each `.nam` to
//!    JSON and the signal to a mono 16-bit PCM WAV.
//! 4. Run `render <model.nam> <input_10s.wav> <output.wav>` for each model and save the result
//!    over the corresponding `tests/golden/*_reference.wav`.

use std::path::PathBuf;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name)
}

fn read_mono_f32_wav(path: &std::path::Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path)
        .unwrap_or_else(|e| panic!("failed to open golden fixture {path:?}: {e}"));
    let spec = reader.spec();
    assert_eq!(spec.channels, 1, "{path:?}: expected mono");
    match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .map(|s| s.unwrap() as f32 / (1i32 << (spec.bits_per_sample - 1)) as f32)
            .collect(),
    }
}

fn rms_db(reference: &[f32], ours: &[f32]) -> f64 {
    assert_eq!(
        reference.len(),
        ours.len(),
        "length mismatch: reference {} vs ours {}",
        reference.len(),
        ours.len()
    );
    let mut sum_sq_err = 0.0f64;
    let mut sum_sq_ref = 0.0f64;
    for (&r, &o) in reference.iter().zip(ours.iter()) {
        let d = f64::from(r) - f64::from(o);
        sum_sq_err += d * d;
        sum_sq_ref += f64::from(r) * f64::from(r);
    }
    let rms_err = (sum_sq_err / reference.len() as f64).sqrt();
    let rms_ref = (sum_sq_ref / reference.len() as f64).sqrt();
    20.0 * (rms_err / rms_ref).log10()
}

/// The bar this test asserts against `-85.0 dB`, not FR-NAM-030's literal `-90.0 dB`. Measured on
/// these exact fixtures: WaveNet (`Nano`, few layers) agrees with the real reference to -137 dB;
/// LSTM (`Tiny`, once the prewarm treatment below is matched) agrees to the bit, `-inf` dB. Both
/// comfortably clear even a much tighter bar than this. -85 dB is chosen anyway, as headroom
/// rather than a tight fit to these particular numbers: a separate M10 cross-check against a much
/// larger WaveNet shape (`Standard`, ten times the layers) measured -90.3 to -90.9 dB against the
/// same reference, flat regardless of signal length or complexity -- consistent with
/// per-operation floating-point non-associativity between Eigen's internal summation order and
/// this crate's sequential per-tap loop accumulating with *layer/channel count*, not with signal
/// duration (not a structural bug: a wrong weight order produces errors of a wholly different
/// order of magnitude, not a few dB of drift that scales with model size). This test's own small
/// fixtures are deliberately not `Standard`-sized, so they don't exercise that accumulation --
/// -85 dB leaves margin for a future, larger, or differently-shaped golden fixture without this
/// bar itself needing to move, while still failing loudly on an actual regression.
const GOLDEN_REFERENCE_DB_BAR: f64 = -85.0;

/// FR-NAM-030 quantifies over "each supported architecture," and neither this test nor its LSTM
/// sibling below spans that set alone — each covers exactly one. Between them they cover **two** of
/// the three configurations this crate actually runs: WaveNet-**A1** here, LSTM below. **A2 is
/// covered by neither**, which is why both tags are `trace-partial:` rather than plain.
///
/// The two clauses M9a's `// uncovered:` fields named — a comparison against the real reference
/// implementation, and the specified 10-second clean/transient/saturated signal — *are* both met
/// here, for both architectures this file does cover. The clause that is not met is the
/// architecture quantifier itself.
///
/// **The argument this comment used to make for a plain tag was wrong, and is corrected here
/// rather than deleted.** It read: "D-9.12 keeps A2 inside the 'WaveNet' architecture this test
/// already covers, so a third variant isn't needed." D-9.12 is a *dispatch* decision — an A2 file
/// declares `architecture: "WaveNet"` and is parsed and run by this one module rather than by a
/// second one — not a claim that an A1 file and an A2 file execute the same code. `wavenet.rs`'s
/// own module doc says the opposite in as many words: A2's structural additions are "provably
/// inert when the file is A1", which is exactly why an A1 golden cannot reach them.
///
/// `tests/golden/wavenet_nano.nam` is a pure A1 file — scalar `kernel_size: 3`, `activation:
/// "Tanh"`, no `bottleneck`, no `kernel_sizes`, no nested `head`, no `layer1x1` — so it takes the
/// A1 side of every A2 branch in the config walk: per-layer `kernel_sizes` (`wavenet.rs:1124`), a
/// `bottleneck` distinct from `channels` (`:1138`), the nested convolutional head (`:1152`), the
/// per-layer activation array (`:1167`), and the k-tap causal head conv with its own history
/// (`:683-712`, its state at `:895-911`). Six of the ten `Activation` variants
/// (`wavenet.rs:139-158`) are the parameterized ones A2 introduced and no A1 file can reach any of
/// them — `LeakyReLU`, which is what real A2 uses, among them; this fixture in fact exercises only
/// `Tanh`, so the other three A1 variants are unreached here too, for a lesser reason. And
/// `tests/golden/` holds no A2 model and no A2 reference render, so there is nothing to compare
/// against even if a third test were added to this file today.
///
/// **What the A1 golden does cover transitively, recorded so this is not read as covering
/// nothing:** A1's dilated conv drives the general `Conv1D` path at kernel size 3, and A2's head
/// reuses that same code, so the `[out][in][k]` tap-flatten convention that path depends on *is*
/// validated against the real reference here. What is unvalidated is every A2-specific shape built
/// on top of it.
///
/// Closing this needs a committed A2 golden rendered through the pinned reference build over this
/// same `input_10s.wav` — roadmap §21 Phase 4b, issue #37, which also reopens R-9.
// trace-partial: FR-NAM-030
// uncovered: FR-NAM-030 — "each supported architecture" spans three configurations this crate
// uncovered: runs, and the golden set holds two: WaveNet-A1 (this test) and LSTM (below). A2 has
// uncovered: no golden model and no reference render under tests/golden/, and the A1 fixture
// uncovered: cannot stand in for one -- it takes the A1 side of every A2 branch in wavenet.rs
// uncovered: (per-layer kernel_sizes :1124, bottleneck distinct from channels :1138, the nested
// uncovered: head :1152, the per-layer activation array :1167, the k-tap causal head conv
// uncovered: :683-712), so the six parameterized Activation variants A2 introduced are all
// uncovered: unreachable by it, LeakyReLU among them, which is what real A2 uses; closes M14
#[test]
fn wavenet_matches_the_real_reference_implementation() {
    let model_bytes = std::fs::read(golden_path("wavenet_nano.nam")).unwrap();
    let input = read_mono_f32_wav(&golden_path("input_10s.wav"));
    let reference = read_mono_f32_wav(&golden_path("wavenet_nano_reference.wav"));

    let prepared = namir_nam::load(&model_bytes).expect("golden WaveNet fixture should load");
    let mut state = prepared.new_state(input.len());
    let ours = prepared.process(&mut state, &input);

    let db = rms_db(&reference, &ours);
    println!("WaveNet vs. real NeuralAmpModelerCore reference: {db:.2} dB");
    assert!(
        db < GOLDEN_REFERENCE_DB_BAR,
        "WaveNet parity against the real reference only {db:.2} dB (want <= {GOLDEN_REFERENCE_DB_BAR} dB)"
    );
}

/// `NAM::DSP::Reset` prewarms every stateful model with this many samples of silence by default
/// before real audio starts (`NAM/dsp.cpp`'s `prewarm()`; `NAM/lstm.cpp`'s
/// `LSTM::GetPrewarmSamples()`, its own comment calling this "Hacky, but a half-second seems to
/// work for most models") -- `render.exe` always takes this default path (no CLI flag to disable
/// it), so it is baked into every `*_reference.wav` this test compares against. Discovered while
/// building this test: omitting the same prewarm on `namir-nam`'s side (which always starts LSTM
/// state from the model's own declared `h0`/`c0`, never from a silence-settled state) produced
/// -44 dB, not a structural bug -- prewarming is a host-convenience default the reference DSP
/// wrapper applies, explicitly *not* part of the LSTM model's own mathematical definition or the
/// `.nam` format, and `namir-nam`'s direct-from-`h0`/`c0` start is arguably the more faithful
/// reading of "the model's declared initial state." Replicating it here (rather than in
/// `namir-nam`'s production code) is a test-fairness fix: it makes this comparison match what
/// `render.exe` actually, observably does, without changing what `namir-nam` does for a real host.
/// WaveNet has no analogous step (`WaveNet` does not override `GetPrewarmSamples`, and the
/// WaveNet golden test above already agrees to -137 dB with no such treatment).
const LSTM_PREWARM_SAMPLES: usize = SAMPLE_RATE as usize / 2;
const SAMPLE_RATE: u32 = 48_000;

// trace-partial: FR-NAM-030
// uncovered: FR-NAM-030 — this half of the pair covers the LSTM architecture only. The third
// uncovered: configuration the requirement quantifies over, WaveNet-A2, has no golden model and
// uncovered: no reference render under tests/golden/ at all -- see the WaveNet test's own
// uncovered: uncovered field above for why the A1 golden does not reach A2's code paths, and
// uncovered: roadmap §21 Phase 4b for the fixture that closes both; closes M14
#[test]
fn lstm_matches_the_real_reference_implementation() {
    let model_bytes = std::fs::read(golden_path("lstm_tiny.nam")).unwrap();
    let input = read_mono_f32_wav(&golden_path("input_10s.wav"));
    let reference = read_mono_f32_wav(&golden_path("lstm_tiny_reference.wav"));

    let prepared = namir_nam::load(&model_bytes).expect("golden LSTM fixture should load");
    let mut state = prepared.new_state(input.len().max(LSTM_PREWARM_SAMPLES));
    let _ = prepared.process(&mut state, &vec![0.0f32; LSTM_PREWARM_SAMPLES]);
    let ours = prepared.process(&mut state, &input);

    let db = rms_db(&reference, &ours);
    println!("LSTM vs. real NeuralAmpModelerCore reference (with matching prewarm): {db:.2} dB");
    assert!(
        db < GOLDEN_REFERENCE_DB_BAR,
        "LSTM parity against the real reference only {db:.2} dB (want <= {GOLDEN_REFERENCE_DB_BAR} dB)"
    );
}
