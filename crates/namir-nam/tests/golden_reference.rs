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
//! **It spans it now** (M14 Phase 4b, 2026-08-12, second note this session; the paragraph above is
//! left as written per this project's convention rather than edited into agreement). Two A2 golden
//! models and their reference renders are committed, so the file covers all three configurations
//! this crate runs -- WaveNet-A1, WaveNet-A2 (both configurations FR-NAM-150 names) and LSTM -- and
//! all four tags below are plain. **What that does and does not settle** is stated at
//! [`a2_full_matches_the_real_reference_implementation`], because two things Phase 4b found stay
//! open and are not closed by any render: no genuine trainer-produced A2 export has ever been
//! loaded, and upstream's default `NAM_ENABLE_A2_FAST=ON` path is not what these renders exercise.
//!
//! Per D-19.1, the fixtures here are
//! *generated*, not captured: `tests/golden/wavenet_nano.nam`, `tests/golden/lstm_tiny.nam`,
//! `tests/golden/a2_full.nam` and `tests/golden/a2_lite.nam` are
//! small, seeded `namir-fixtures` outputs (regenerate with the recipe below), and
//! `tests/golden/input_10s.wav` is FR-NAM-030's own "10-second test signal containing clean,
//! transient and saturated material," built from the same recipe
//! `spikes/s1-nam-inference/src/bin/generate_fixture.rs`'s `build_test_signal` uses (the roadmap's
//! own citation for this signal shape). `tests/golden/*_reference.wav` are that signal rendered
//! through the real reference implementation, pinned at commit `3cde95c`, built with
//! `-DNAM_USE_INLINE_GEMM -DNAM_ENABLE_A2_FAST=OFF` (D-9.12's PR #264 consequence note: the
//! default Eigen GEMM path is not bit-exact across Eigen version bumps, `NAM_USE_INLINE_GEMM`
//! bypasses it entirely and is the reproducible target to build against). None of these nine files
//! is large (~8.7 MB total) or licensed/captured audio -- every one is regenerable from the recipe
//! in this file plus a local NeuralAmpModelerCore checkout.
//!
//! **Why `-DNAM_ENABLE_A2_FAST=OFF`, recorded because M14 Phase 4b found the choice had no stated
//! rationale anywhere.** Upstream defaults that option **ON** (`CMakeLists.txt:58`), and
//! `a2_fast.cpp`'s `is_a2_shape` detector matches exactly the two shapes FR-NAM-150 names, so a
//! default-built host runs a real A2 model through `a2_fast.cpp` rather than through the general
//! `wavenet` code these renders exercise. Two reasons for excluding it, and one consequence stated
//! rather than measured. (1) `NAM_USE_INLINE_GEMM` is what makes this build reproducible across
//! Eigen version bumps, and the fast path is a hand-written kernel, not the GEMM the flag redirects
//! -- pinning one while leaving the other free would defeat the pin. (2) The fast path is upstream's
//! *optimisation* of the same declared model, so the general path is the definition and the fast
//! path is a claimed-equivalent implementation of it; a reference should be the definition.
//! **The consequence, unmeasured and not to be read as measured:** nothing here establishes that
//! `a2_fast.cpp` agrees with the general path, so nothing here establishes that Namir agrees with
//! what a default-built host actually runs. Closing that needs a second render pair from an
//! `NAM_ENABLE_A2_FAST=ON` build; it is recorded at **R-9** (`docs/02-architecture.md` §22) rather
//! than performed, because no such measurement was taken.
//!
//! # Regenerating these fixtures
//!
//! 1. `git clone --recurse-submodules https://github.com/sdatkinson/NeuralAmpModelerCore && cd
//!    NeuralAmpModelerCore && git checkout 3cde95c354d5ba6da01316cad90b05cfc4855053`
//! 2. Build `render` with `-DNAM_USE_INLINE_GEMM -DNAM_ENABLE_A2_FAST=OFF` (GCC/Clang:
//!    `cmake -S . -B build_inline -DCMAKE_BUILD_TYPE=Release
//!    -DCMAKE_CXX_FLAGS="-DNAM_USE_INLINE_GEMM" -DNAM_ENABLE_A2_FAST=OFF && cmake --build
//!    build_inline --target render -j4`. MSVC: the same with `/DNAM_USE_INLINE_GEMM` and
//!    `--config Release`). `spikes/s1-nam-inference/README.md` documents the MSVC-specific build
//!    corrections this needs (a GCC/Clang-only `-Wno-error` flag `tools/CMakeLists.txt` applies
//!    unconditionally, and Windows long-path limits).
//! 3. Build the same 10-second signal `build_test_signal` below generates, and the same
//!    `namir_fixtures::nam::generate(WaveNetShape::Nano, 30)` /
//!    `namir_fixtures::nam::generate_lstm(LstmShape::Tiny, 30)` fixtures, write each `.nam` to
//!    JSON and the signal to a mono 16-bit PCM WAV. For the two A2 models this step is executable
//!    in-tree: `cargo test -p namir-nam --test golden_reference regenerate_the_a2_golden_models --
//!    --ignored`, and [`the_a2_golden_models_match_their_generator`] is the standing guard that the
//!    committed bytes still match their generator.
//! 4. Run `render <model.nam> <input_10s.wav> <output.wav>` for each model and save the result
//!    over the corresponding `tests/golden/*_reference.wav`. Run it **outside** this repository and
//!    copy the results in; a NeuralAmpModelerCore checkout inside the worktree is not wanted.

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
///
/// **Changed from `-85.0` to `-90.0` at M14 Phase 4b, and the paragraph above is kept as the
/// record of why it was `-85.0`.** The change follows from the promotion below, not from any new
/// measurement: a plain `// trace: FR-NAM-030` asserts this file verifies the **whole**
/// requirement **by the requirement's own stated method**, and the requirement's own number is
/// "at least 90 dB below", not 85. A bar looser than the requirement cannot carry that claim, so
/// leaving it at `-85.0` while promoting would have been D-23.1's exact failure mode one level
/// down -- the tag saying "verified" over an assertion that would pass at -86 dB, where the
/// requirement fails.
///
/// It costs nothing *for these fixtures*: all four clear -90 dB by at least 36 dB (WaveNet Nano
/// -137, LSTM Tiny `-inf`, A2-Full -132.6, A2-Lite -126.5). What it does spend is exactly the
/// headroom the paragraph above reserved, and that is worth stating plainly rather than
/// discovering later: M10's cross-check against a `Standard`-shape WaveNet measured **-90.3 to
/// -90.9 dB** against this same reference, so a future golden of that size would sit on this bar
/// by tenths of a dB. That is a fact about FR-NAM-030's 90 dB floor at realistic model sizes --
/// the floor is nearly tight against non-associative float summation in a ten-times-larger model
/// -- and not a fact about this constant. Absorbing it into a looser bar would hide it; whoever
/// adds a `Standard`-sized golden should read it here and decide deliberately.
const GOLDEN_REFERENCE_DB_BAR: f64 = -90.0;

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
///
/// **Closed at M14 Phase 4b (2026-08-12); everything above is the record of the gap, kept.** The
/// A2 golden the last paragraph asked for is committed —
/// [`a2_full_matches_the_real_reference_implementation`] and its `Lite` sibling — rendered through
/// the same pinned build over this same `input_10s.wav`. The set FR-NAM-030 quantifies over is now
/// spanned between the four tests in this file, exactly as this comment's own reading of the
/// quantifier requires ("neither this test nor its LSTM sibling spans that set alone… between them
/// they cover two of the three"): with A2 present they cover three of three, so all four tags are
/// plain. The two A2 tests state what a render can and cannot settle, and it is less than "A2 is
/// verified" — read them before treating this promotion as more than it is.
// trace: FR-NAM-030
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

/// This half of the pair covers the LSTM architecture. Its `trace-partial:` and the
/// `// uncovered:` field naming WaveNet-A2 as the missing third configuration were retired at M14
/// Phase 4b, when that configuration's golden landed below; see the WaveNet test above for the
/// full record of what the gap was and what closing it did and did not settle.
// trace: FR-NAM-030
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

// ---------------------------------------------------------------------------------------------
// A2 (M14 Phase 4b, issue #37)
// ---------------------------------------------------------------------------------------------

/// The seed the committed A2 golden models were generated from. Any value works; this one is fixed
/// (and is the same `30` the two A1/LSTM goldens use) so the checked-in `.nam` bytes and the
/// `*_reference.wav` rendered from them stay reproducible together.
const A2_GOLDEN_SEED: u64 = 30;

/// The two configurations FR-NAM-150 names, with the basename each one's `.nam` and
/// `*_reference.wav` share under `tests/golden/`. `A2Shape::Full` is upstream's "A2 standard"
/// (channels = 8) and FR-NAM-150's "A2-Full"; `A2Shape::Lite` is upstream's "A2 nano"
/// (channels = 3) and FR-NAM-150's "A2-Lite" — the mapping
/// `crates/namir-fixtures/src/nam/mod.rs`'s `A2Shape` doc comment records.
/// `A2Shape::BottleneckProbe` is deliberately absent: it is `namir-fixtures`' own invention rather
/// than an upstream shape, `is_a2_shape` rejects it by construction, and FR-NAM-150 does not name
/// it. It keeps its in-house cross-check in `tests/a2_fixtures.rs`.
const A2_GOLDEN: [(namir_fixtures::nam::A2Shape, &str); 2] = [
    (namir_fixtures::nam::A2Shape::Full, "a2_full"),
    (namir_fixtures::nam::A2Shape::Lite, "a2_lite"),
];

/// Regenerates the two A2 golden `.nam` models, per step 3 of the recipe in this module's header.
/// `#[ignore]`d in the same shape as `params.lock`'s generator and `namir-clap`'s
/// `regenerate_the_golden_vector`: the checked-in files are the artifact, this is the recipe that
/// produced them, and [`the_a2_golden_models_match_their_generator`] is the standing guard that the
/// two have not drifted.
///
/// It deliberately does **not** regenerate `*_reference.wav` — that half needs the external
/// `NeuralAmpModelerCore` build the header documents and cannot run under `cargo test`.
#[test]
#[ignore = "regenerates the committed A2 golden .nam models; run it only when the generator changes"]
fn regenerate_the_a2_golden_models() {
    for (shape, name) in A2_GOLDEN {
        let model = namir_fixtures::nam::generate_a2(shape, A2_GOLDEN_SEED)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let path = golden_path(&format!("{name}.nam"));
        std::fs::write(&path, model.to_json_bytes()).expect("write A2 golden model");
        println!("wrote {}", path.display());
    }
}

/// The failure this guards against is silent and total: `namir-fixtures`' A2 generator changes,
/// someone re-runs [`regenerate_the_a2_golden_models`], and the committed `*_reference.wav` — which
/// can only be regenerated from an external C++ build — is now the render of a *different* model.
/// The two golden tests below would then compare Namir's output for one model against the
/// reference's output for another, and fail with a number that looks like an inference bug. This
/// test makes that failure name its real cause instead.
///
/// # Why this is not a byte comparison, and what is still exact
///
/// It was one until M14, and it failed on all three CI platforms while passing on the machine that
/// wrote the goldens. **Two bytes differed out of 205 986**, in one value appearing twice —
/// `config.head_scale` and the trailing weight that mirrors it:
///
/// ```text
/// this sandbox (1.94.1 and 1.98 alike):  "head_scale": 0.15790403
/// every CI runner (ubuntu, macOS, Windows):  "head_scale": 0.15790401
/// ```
///
/// Every one of the ~50 000 weights matched bit for bit, and that is the diagnosis rather than a
/// detail. Weights come straight from the seeded RNG, so they reproduce anywhere. `head_scale`
/// does not: `namir-fixtures`' generator calibrates it as `base * (target_rms / measured_rms)`,
/// and `measure_output_rms` runs the *whole* inference over a probe signal. One ULP of difference
/// anywhere in those thousands of `f32` operations — FMA contraction, autovectorisation, a libm
/// `tanh`/`exp` difference — lands in this one float. **D-19.1's premise is that a fixture is
/// reproducible from `(shape, seed)`; this value is reproducible only up to floating-point
/// inference, which nothing promises across execution environments.**
///
/// **The variable is the environment, not the compiler version, and the first reading of this said
/// otherwise.** M14 recorded the split as 1.94.1-against-1.98 because those were the two versions
/// to hand. Installing `cargo-llvm-cov` later in the same milestone made the sandbox upgrade to
/// 1.98 — CI's exact compiler — and `generate_a2` there still regenerates this file **byte for
/// byte**, `0.15790403` and all. Meanwhile all three CI runners agree with each other on
/// `0.15790401` across **two architectures** (x86-64 Linux and Windows, arm64 macOS), which also
/// rules out a target-feature explanation resting on one instruction set. What actually differs
/// has not been isolated; what is established is that a compiler-version story does not fit the
/// evidence, and that this value varies by machine — a stronger reason not to byte-compare it than
/// the version story ever was.
///
/// So the comparison is byte-exact everywhere *except* the head_scale value, which is compared
/// with a relative tolerance. This is deliberately not "ignore head_scale": a differing line is
/// accepted only if its committed value **is** the committed `head_scale`, so a drifting weight
/// still fails even though it is also a float.
///
/// **The committed `*_reference.wav` files stay valid, and that is arithmetic rather than
/// optimism.** One ULP of `head_scale` scales the rendered output by ~6e-8, about −144 dB —
/// below the −132.58 dB (Full) and −126.46 dB (Lite) margins the parity tests already assert. A
/// drift large enough to matter to them is orders of magnitude outside this tolerance.
#[test]
fn the_a2_golden_models_match_their_generator() {
    /// ~16 ULP at this magnitude: far wider than cross-environment inference noise, far tighter
    /// than any real change to the generator could be.
    const HEAD_SCALE_REL_TOL: f64 = 1e-6;

    /// The `f32` literal on a line of this generator's pretty-printed JSON, if the line carries
    /// exactly one. `None` for structural lines, which must then match byte for byte.
    fn sole_float(line: &str) -> Option<f64> {
        let body = line.rsplit(':').next()?.trim().trim_end_matches(',');
        body.parse::<f64>().ok()
    }

    for (shape, name) in A2_GOLDEN {
        let expected_bytes = namir_fixtures::nam::generate_a2(shape, A2_GOLDEN_SEED)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .to_json_bytes();
        let committed_bytes = std::fs::read(golden_path(&format!("{name}.nam"))).unwrap();

        if committed_bytes == expected_bytes {
            continue;
        }

        let committed = String::from_utf8(committed_bytes).expect("golden model is UTF-8 JSON");
        let expected = String::from_utf8(expected_bytes).expect("generated model is UTF-8 JSON");

        let drift_note = format!(
            "{name}.nam has drifted from generate_a2(.., {A2_GOLDEN_SEED}). Its committed \
             *_reference.wav was rendered from the old bytes and is now meaningless — re-run the \
             whole recipe in this module's header, not just `regenerate_the_a2_golden_models`. \
             (If the only difference is `head_scale`, see this test's doc comment: that value is \
             calibrated through a floating-point inference pass and drifts by ~1 ULP between \
             toolchains, which is tolerated below rather than regenerated.)"
        );

        let committed_lines: Vec<&str> = committed.lines().collect();
        let expected_lines: Vec<&str> = expected.lines().collect();
        assert_eq!(
            committed_lines.len(),
            expected_lines.len(),
            "{drift_note} The two differ in line count, so this is a structural change, not \
             floating-point drift."
        );

        // The committed `head_scale`, read from the line that names it. Any other line is allowed
        // to differ only if it carries this same value -- which is exactly the trailing weight
        // that mirrors it, and nothing else in the document.
        let head_scale = committed_lines
            .iter()
            .find(|line| line.contains("\"head_scale\""))
            .and_then(|line| sole_float(line))
            .unwrap_or_else(|| panic!("{name}.nam carries no readable `head_scale` line"));

        for (n, (got, want)) in committed_lines.iter().zip(&expected_lines).enumerate() {
            if got == want {
                continue;
            }
            let (Some(a), Some(b)) = (sole_float(got), sole_float(want)) else {
                panic!("{drift_note}\n  line {n} committed: {got}\n  line {n} generated: {want}");
            };
            assert!(
                (a - head_scale).abs() <= HEAD_SCALE_REL_TOL * head_scale.abs(),
                "{drift_note}\n  line {n} differs and its committed value {a} is not the \
                 committed head_scale {head_scale}, so this is a drifting *weight*, which no \
                 tolerance here excuses."
            );
            assert!(
                (a - b).abs() <= HEAD_SCALE_REL_TOL * a.abs().max(b.abs()),
                "{drift_note}\n  head_scale moved from {a} to {b}, further than the {} relative \
                 tolerance cross-environment inference noise can explain.",
                HEAD_SCALE_REL_TOL
            );
        }
    }
}

fn assert_a2_golden(name: &str) {
    let model_bytes = std::fs::read(golden_path(&format!("{name}.nam"))).unwrap();
    let input = read_mono_f32_wav(&golden_path("input_10s.wav"));
    let reference = read_mono_f32_wav(&golden_path(&format!("{name}_reference.wav")));

    // Longer than A2's receptive field by a factor of ~76: `input_10s.wav` is 480 000 samples and
    // the real A2 shapes' field is 6 346 (6 331 through `a2_core_layer_array`'s 23 dilated layers,
    // plus 15 for the 16-tap head), so ~98.7 % of the compared samples depend on no zero-padded
    // startup history at all. This is the probe length FR-NAM-150's own `uncovered:` field named as
    // its gap — `tests/a2_fixtures.rs`'s 4 000-sample probe was *shorter* than the field, so every
    // sample it compared sat inside the startup transient.
    assert!(
        input.len() > 70 * 6_346,
        "the probe must clear A2's receptive field by a wide margin"
    );

    let prepared = namir_nam::load(&model_bytes)
        .unwrap_or_else(|e| panic!("golden A2 fixture {name} should load: {e}"));
    // No prewarm, unlike the LSTM test above: `WaveNet` does not override `GetPrewarmSamples`, so
    // `NAM::DSP::Reset`'s prewarm is a no-op for it and for A2, and `render` starts from the same
    // zero history `namir-nam` does.
    let mut state = prepared.new_state(input.len());
    let ours = prepared.process(&mut state, &input);

    let db = rms_db(&reference, &ours);
    println!("{name} vs. real NeuralAmpModelerCore reference: {db:.2} dB");
    assert!(
        db < GOLDEN_REFERENCE_DB_BAR,
        "{name} parity against the real reference only {db:.2} dB (want <= {GOLDEN_REFERENCE_DB_BAR} dB)"
    );
}

/// The third configuration this crate runs, and the one no golden reached until M14 Phase 4b:
/// WaveNet-**A2**, upstream's "A2 standard" and FR-NAM-150's "A2-Full". Measured **-132.58 dB**
/// against the pinned reference build; its `Lite` sibling measures **-126.46 dB**. Both clear
/// FR-NAM-030's 90 dB floor by more than 36 dB, in the same band as the A1 golden's -137 dB — which
/// is the result a correct weight order produces, and emphatically not what a *wrong* one does
/// (see [`GOLDEN_REFERENCE_DB_BAR`]'s note: a weight-order error produces errors of a wholly
/// different order of magnitude, not a few dB of drift).
///
/// **This pair takes both tags, and neither is a formality.**
///
/// - **FR-NAM-030.** Its `Verify: G` asks for the reference NAM implementation, and this is it.
///   The A1 fixture could not stand in: it takes the A1 side of every A2 branch in `wavenet.rs`
///   (per-layer `kernel_sizes`, `bottleneck` distinct from `channels`, the nested convolutional
///   head, the per-layer activation array, the k-tap causal head conv), so the six parameterized
///   `Activation` variants A2 introduced were all unreachable by it — `LeakyReLU` among them, which
///   is what both fixtures here use, on all 23 layers.
/// - **FR-NAM-150.** Its own `Verify:` line elects "cross-implementation parity against an
///   independent reference implementation, per NFR-QUAL-030", and NFR-QUAL-030 says "golden
///   reference audio held in the repository, with tolerances stated numerically" — which is
///   literally this test, against the most independent reference available. The clause that kept
///   the tag partial was "**to the accuracy of FR-NAM-030**", and FR-NAM-030's accuracy is defined
///   over its own specified 10-second clean/transient/saturated signal at its own 90 dB floor. Both
///   are executed here: this is that signal, and [`GOLDEN_REFERENCE_DB_BAR`] is that floor.
///   `tests/a2_fixtures.rs` keeps the in-house `reference_infer_a2` cross-check — it is the
///   independent-*port* evidence, and it covers `BottleneckProbe`, which this pair does not — but
///   it is no longer where FR-NAM-150's tag lives, and its own doc comment says why.
///
/// **What this does not settle, stated because Phase 4b's finding was that a render can be read as
/// settling more than it does.** Bit-close agreement here proves Namir's A2 walk and
/// `NeuralAmpModelerCore`'s general `wavenet` walk derive the same numbers from the same declared
/// config. It does not prove:
///
/// 1. **That the declared config is what a real trainer emits.** No genuine trainer-produced A2
///    export has ever been loaded by this project. `namir-fixtures`' generator and `namir-nam`'s
///    parser were both derived from one reading of the same C++, and this test compares against
///    that same C++ — so a *shared misreading of the schema* is invisible to all three. `file.rs`'s
///    A2 fields are `#[serde(default)] Option<_>` with no `deny_unknown_fields`, so a real file
///    carrying a feature under a key nobody anticipated is silently ignored rather than rejected,
///    which is FR-NAM-140's concern for real files even though its own test is sound. This is the
///    class AGENTS.md warns about, citing the post-M6 `null`-vs-omitted bug, and it stays open: it
///    is closed by obtaining a real export, not by any render.
/// 2. **That Namir agrees with what a default-built host runs.** See this module's header on
///    `NAM_ENABLE_A2_FAST`, which upstream defaults **ON** and this build turns off.
///
/// Both are recorded at **R-9** (`docs/02-architecture.md` §22) rather than papered over, and R-9
/// is narrowed by this test rather than closed by it.
// trace: FR-NAM-030, FR-NAM-150
#[test]
fn a2_full_matches_the_real_reference_implementation() {
    assert_a2_golden("a2_full");
}

/// FR-NAM-150's second named configuration, upstream's "A2 nano" (channels = 3). Measured
/// **-126.46 dB**. See [`a2_full_matches_the_real_reference_implementation`] for what this pair
/// does and does not settle; everything there applies here unchanged.
// trace: FR-NAM-030, FR-NAM-150
#[test]
fn a2_lite_matches_the_real_reference_implementation() {
    assert_a2_golden("a2_lite");
}
