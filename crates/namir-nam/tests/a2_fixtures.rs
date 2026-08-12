//! Integration tests wiring `namir-nam`'s core-A2 implementation against `namir-fixtures`'s
//! independently-written A2 generator and reference inference (M10, D-9.12). The A2 counterpart
//! of `fixtures.rs` — see that file's doc comment, which applies here unchanged in spirit: does
//! this crate load real generated A2 shapes, survive mutated/corrupted variants without panicking,
//! process consistently across block splits, and agree with an independently-written reference.
//!
//! **Why this lives in a separate file from `fixtures.rs` rather than extending its `SHAPES`
//! array:** `namir-fixtures` exposes A2 fixtures through a distinct type (`A2Model`, not
//! `NamModel`) and a distinct reference function (`reference_infer_a2`, not `reference_infer`) —
//! see that crate's own module doc comment for why A2's structural differences from A1 (a
//! `bottleneck` width, per-layer `kernel_sizes`, a real convolutional head) made a sibling type the
//! natural fit there, not a widened one. This file mirrors that split.
//!
//! **Shape-name mapping** (recorded once, at the one other place this repository needs it,
//! `crates/namir-fixtures/src/nam/mod.rs`'s `A2Shape` doc comment): `A2Shape::Full` is upstream's
//! "A2 standard" (channels=8) and FR-NAM-150's "A2-Full"; `A2Shape::Lite` is upstream's "A2 nano"
//! (channels=3) and FR-NAM-150's "A2-Lite". `A2Shape::BottleneckProbe` is `namir-fixtures`'s own
//! invention (not a real upstream or FRS-named configuration) covering `bottleneck != channels`,
//! which neither real A2 shape exercises — included in every test below for the coverage, but not
//! part of what FR-NAM-150 itself names.

use namir_fixtures::nam::{self, A2Shape};

const A2_SHAPES: [(A2Shape, &str); 3] = [
    (A2Shape::Full, "a2-full"),
    (A2Shape::Lite, "a2-lite"),
    (A2Shape::BottleneckProbe, "a2-bottleneck-probe"),
];

/// Mirrors `fixtures.rs`'s own `deterministic_signal` exactly (a standalone copy, not a shared
/// helper — see that function's doc comment for why: `namir-fixtures`' calibration probe is
/// `pub(super)` to that crate).
fn deterministic_signal(seed: u64, n: usize) -> Vec<f32> {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let noise = ((x % 2_000_003) as f32 / 1_000_001.5) - 1.0;
            let t = i as f32 / 48_000.0;
            0.2 * (2.0 * std::f32::consts::PI * 110.0 * t).sin() + 0.02 * noise
        })
        .collect()
}

#[test]
fn parses_all_generated_a2_shapes() {
    for (shape, name) in A2_SHAPES {
        let model = nam::generate_a2(shape, 1).unwrap_or_else(|e| panic!("{name}: {e}"));
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
    }
}

#[test]
fn chunked_processing_matches_monolithic_processing_for_a2() {
    for (shape, name) in A2_SHAPES {
        let model = nam::generate_a2(shape, 123).unwrap_or_else(|e| panic!("{name}: {e}"));
        let bytes = model.to_json_bytes();
        let prepared = namir_nam::load(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));

        let signal = deterministic_signal(42, 2_000);

        let mut mono_state = prepared.new_state(signal.len());
        let monolithic = prepared.process(&mut mono_state, &signal);

        // Chunk size 1 in particular is the strongest detector for a mis-sized or missing
        // head-rechannel history buffer (A2's nested `head` is a real k-tap causal `Conv1D`,
        // unlike A1's implicit 1x1) — see `wavenet.rs`'s own unit-test coverage of the same
        // property against a hand-built fixture; this exercises it against a generated one too.
        for &chunk_size in &[1usize, 3, 7, 64] {
            let mut chunk_state = prepared.new_state(chunk_size);
            let mut chunked = Vec::with_capacity(signal.len());
            for chunk in signal.chunks(chunk_size) {
                let out = prepared.process(&mut chunk_state, chunk);
                chunked.extend_from_slice(&out);
            }

            assert_eq!(
                chunked.len(),
                monolithic.len(),
                "{name} chunk_size {chunk_size}: length mismatch"
            );
            for (i, (&whole, &parts)) in monolithic.iter().zip(chunked.iter()).enumerate() {
                assert!(
                    (whole - parts).abs() < 1e-4,
                    "{name} chunk_size {chunk_size}: sample {i} differs: monolithic={whole}, chunked={parts}"
                );
            }
        }
    }
}

/// FR-NAM-150 (Must): "Namir shall load and run NAM Architecture 2 (A2) models in the A2-Full and
/// A2-Lite configurations, to the accuracy of FR-NAM-030." `Verify: U — cross-implementation
/// parity against an independent reference implementation, per NFR-QUAL-030.`
///
/// Both quantified configurations are covered (`A2Shape::Full`/`A2Shape::Lite`, mapped to
/// "A2-Full"/"A2-Lite" per this file's own doc comment), against `namir-fixtures`'s independently
/// derived `reference_infer_a2` — an implementation written, per that crate's own task record,
/// without reading this crate's A2 code at any point (R-9's mitigation, `docs/02-architecture.md`
/// §22). `-100.0 dB` is the same in-tree bar `fixtures.rs`'s own A1 parity test uses: generous
/// headroom below FR-NAM-030's `-90 dB` floor against the *real* NAM implementation (which this
/// Rust-vs-Rust comparison is not — see that test's own doc comment on what this evidence is and
/// is not). `BottleneckProbe` is checked too, at the same bar, even though it is outside what
/// FR-NAM-150 itself names — it is the only generated fixture exercising `bottleneck != channels`
/// at all.
///
/// **Why this is a `trace-partial:` even so.** The set FR-NAM-150 quantifies over is spanned —
/// D-23.1's first question passes. Its second question is what fails, and on exactly one clause:
/// "**to the accuracy of FR-NAM-030**". *Not* on the comparison target — FR-NAM-150's own
/// `Verify:` line elects "an independent reference implementation, per NFR-QUAL-030", so
/// `reference_infer_a2` is the artifact this requirement asks for, and the in-house-port objection
/// that demotes FR-NAM-030's own tags is not a gap here. What is unmet is the probe: FR-NAM-030's
/// accuracy is stated "over a specified 10-second test signal containing clean, transient and
/// saturated material", and `deterministic_signal(99, 4_000)` is ~83 ms of 110 Hz sine plus low
/// noise at 48 kHz — the same probe M9a demoted the A1 and LSTM parity tests for, with no
/// transient and nothing saturated. Worse for A2 specifically: the real A2 shapes' receptive field
/// is **~6 346 samples** (6 331 through the 23 dilated layers of `a2_core_layer_array`'s
/// `KERNEL_SIZES`/`DILATIONS`, kernels 6/15 and dilations to 239, plus 15 for the 16-tap head),
/// which is *longer than the 4 000-sample probe*. Every compared sample therefore still depends on
/// zero-padded startup history on both sides, so the figure this test asserts is measured entirely
/// inside the startup transient and never over settled output. Closing it needs a longer probe of
/// the specified material — roadmap §21 Phase 4b, issue #37.
// trace-partial: FR-NAM-150
// uncovered: FR-NAM-150 — the "to the accuracy of FR-NAM-030" clause. Both named configurations
// uncovered: are spanned and the in-house reference is what this requirement's own Verify line
// uncovered: elects, so neither is a gap, but the probe is deterministic_signal(99, 4_000):
// uncovered: ~83 ms of 110 Hz sine plus low noise, not FR-NAM-030's specified 10-second
// uncovered: clean/transient/saturated signal, and shorter than the real A2 shapes' ~6 346-sample
// uncovered: receptive field, so every compared sample still depends on zero-padded startup
// uncovered: history and the asserted figure never leaves the startup transient; closes M14
#[test]
fn numeric_parity_against_an_independent_reference_implementation_for_a2() {
    for (shape, name) in A2_SHAPES {
        let model = nam::generate_a2(shape, 7).unwrap_or_else(|e| panic!("{name}: {e}"));
        let bytes = model.to_json_bytes();
        let prepared = namir_nam::load(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));

        let probe = deterministic_signal(99, 4_000);

        let reference = nam::reference_infer_a2(&model, &probe);
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

        println!("numeric parity vs. namir-fixtures A2 reference ({name}): {db:.1} dB");
        assert!(
            db < -100.0,
            "{name}: numeric parity only {db:.1} dB (want <= -100 dB): a real bug, not noise"
        );
    }
}

#[test]
fn rejects_mutated_a2_variants_without_panicking() {
    let model = nam::generate_a2(A2Shape::Lite, 5).expect("a2-lite fixture should generate");
    let bytes = model.to_json_bytes();

    for seed in 0..30u64 {
        for variant in namir_fixtures::mutate::seeded_corpus(&bytes, seed) {
            let _ = namir_nam::load(&variant);
        }
    }
}
