//! Integration tests wiring `namir-nam` against `namir-fixtures`'s generated `.nam` corpus. This
//! is the actual point of this crate existing as more than a unit-tested parser: does it load
//! real generated shapes, survive mutated/corrupted variants without panicking (NFR-QUAL-040),
//! and — most importantly — does its blockwise stateful processing agree with itself across
//! different block splits and with an independently-written reference implementation.

use namir_fixtures::nam::{self, WaveNetShape};

const SHAPES: [(WaveNetShape, &str); 4] = [
    (WaveNetShape::Standard, "standard"),
    (WaveNetShape::Lite, "lite"),
    (WaveNetShape::Feather, "feather"),
    (WaveNetShape::Nano, "nano"),
];

/// A small, seeded, dependency-free pseudo-random signal generator (xorshift64*), used to build a
/// deterministic sine + noise probe signal for the tests below. `namir-fixtures`' own calibration
/// probe is private (`pub(super)`) to that crate, so this is a standalone equivalent rather than
/// a reuse of it.
fn deterministic_signal(seed: u64, n: usize) -> Vec<f32> {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let noise = ((x % 2_000_003) as f32 / 1_000_001.5) - 1.0; // roughly [-1, 1)
            let t = i as f32 / 48_000.0;
            0.2 * (2.0 * std::f32::consts::PI * 110.0 * t).sin() + 0.02 * noise
        })
        .collect()
}

// trace: FR-NAM-010, FR-NAM-020
#[test]
fn parses_all_generated_shapes() {
    for (shape, name) in SHAPES {
        let model = nam::generate(shape, 1).unwrap_or_else(|e| panic!("{name}: {e}"));
        let bytes = model.to_json_bytes();
        let prepared = namir_nam::load(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(
            prepared.sample_rate().hz(),
            48_000,
            "{name}: sample rate did not round-trip"
        );
        assert!(
            prepared.metadata().name.contains(name),
            "{name}: metadata name {:?} should contain {name:?}",
            prepared.metadata().name
        );
        assert_eq!(
            prepared.metadata().modeled_by,
            "namir-fixtures",
            "{name}: modeled_by"
        );
    }
}

#[test]
fn rejects_mutated_variants_without_panicking() {
    let model = nam::generate(WaveNetShape::Standard, 5).expect("standard fixture should generate");
    let bytes = model.to_json_bytes();

    // The only assertion here is implicit: this loop completes without panicking. `Ok` and `Err`
    // are both acceptable outcomes for any given mutated variant (a byte flip can easily land
    // somewhere harmless and still parse into a valid model) — NFR-QUAL-040 only asks that this
    // crate "not panic, hang, over-allocate ... on any input".
    for seed in 0..30u64 {
        for variant in namir_fixtures::mutate::seeded_corpus(&bytes, seed) {
            let _ = namir_nam::load(&variant);
        }
    }
}

#[test]
fn chunked_processing_matches_monolithic_processing() {
    let model =
        nam::generate(WaveNetShape::Standard, 123).expect("standard fixture should generate");
    let bytes = model.to_json_bytes();
    let prepared = namir_nam::load(&bytes).expect("generated fixture should load");

    let signal = deterministic_signal(42, 4_000);

    let mut mono_state = prepared.new_state(signal.len());
    let monolithic = prepared.process(&mut mono_state, &signal);

    // A mix of chunk sizes, including ones that don't evenly divide the signal length (so the
    // final chunk of a run is shorter than the rest) and a chunk size of 1 (the most demanding
    // case for the history/scratch carried in `NamState`).
    for &chunk_size in &[1usize, 3, 7, 64, 128, 500] {
        let mut chunk_state = prepared.new_state(chunk_size);
        let mut chunked = Vec::with_capacity(signal.len());
        for chunk in signal.chunks(chunk_size) {
            let out = prepared.process(&mut chunk_state, chunk);
            chunked.extend_from_slice(&out);
        }

        assert_eq!(
            chunked.len(),
            monolithic.len(),
            "chunk_size {chunk_size}: length mismatch"
        );
        for (i, (&whole, &parts)) in monolithic.iter().zip(chunked.iter()).enumerate() {
            assert!(
                (whole - parts).abs() < 1e-6,
                "chunk_size {chunk_size}: sample {i} differs: monolithic={whole}, chunked={parts}"
            );
        }
    }
}

/// M10 Phase 4: FR-NAM-030's tag moved off this test. It used to carry a `trace-partial:` here
/// (neither this Rust-vs-Rust comparison nor a real-reference one existed in-tree), naming two
/// unmet clauses: the comparison target (this crate's own from-scratch port, not the actual
/// reference NAM implementation) and the probe signal (~83 ms of sine+noise, not the specified
/// 10-second clean/transient/saturated one). Both are now closed by `tests/golden_reference.rs`,
/// which compares against a real `NeuralAmpModelerCore` render over that exact signal — see that
/// file's own doc comment. This test remains real, valuable evidence for **NFR-QUAL-030**
/// (a stated, numerical, reproducible correctness reference — D-9.11's resolution of that
/// requirement's wording) and for the A1/A2 non-regression baseline M10's weight-layout work was
/// measured against; it simply no longer claims FR-NAM-030 on its own.
#[test]
// trace: NFR-QUAL-030
fn numeric_parity_against_an_independent_reference_implementation() {
    // All four generated shapes, not just `Standard`. This is the regression baseline M10's A2
    // work (crates/namir-nam's `wavenet.rs` weight-layout and activation unification) is measured
    // against: every step of that refactor must leave these four figures unchanged, not merely
    // "still under -100 dB" — an unchanged figure is what makes a regression attributable to the
    // step that caused it, a merely-still-passing one is not.
    //
    // Baseline recorded before any A2 code touched this crate: standard -130.8 dB, lite -136.0 dB,
    // feather -135.4 dB, nano -136.0 dB.
    for (shape, name) in SHAPES {
        let model = nam::generate(shape, 7).unwrap_or_else(|e| panic!("{name}: {e}"));
        let bytes = model.to_json_bytes();
        let prepared = namir_nam::load(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));

        let probe = deterministic_signal(99, 4_000);

        let reference = nam::reference_infer(&model, &probe);
        let mut state = prepared.new_state(probe.len());
        let ours = prepared.process(&mut state, &probe);

        assert_eq!(reference.len(), ours.len(), "{name}: length mismatch");

        let mut sum_sq_err = 0.0f64;
        let mut sum_sq_ref = 0.0f64;
        for (&r, &o) in reference.iter().zip(ours.iter()) {
            let d = f64::from(r) - f64::from(o);
            sum_sq_err += d * d;
            sum_sq_ref += f64::from(r) * f64::from(r);
        }
        let rms_err = (sum_sq_err / reference.len() as f64).sqrt();
        let rms_ref = (sum_sq_ref / reference.len() as f64).sqrt();
        let db = 20.0 * (rms_err / rms_ref).log10();

        // This is the strongest correctness signal this crate has: two independently-written
        // from-scratch Rust ports of the same well-documented algorithm, both float32, agreeing to
        // this tightly is strong evidence neither has a subtle porting bug. -100 dB is generous
        // headroom below FR-NAM-030's -90 dB bar against the *real* C++ reference; if this is
        // nowhere close, that's a bug in one of the two Rust ports to chase down, not a tolerance
        // to loosen.
        println!("numeric parity vs. namir-fixtures reference ({name}): {db:.1} dB");
        assert!(
            db < -100.0,
            "{name}: numeric parity only {db:.1} dB (want <= -100 dB): a real bug, not noise"
        );
    }
}
