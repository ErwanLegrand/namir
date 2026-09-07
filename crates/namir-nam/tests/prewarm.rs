//! Issue #173 / D-9.13: `PreparedNam::prewarm_samples` and `new_state_prewarmed`, the reference's
//! `DSP::prewarm` behaviour.
//!
//! `NeuralAmpModelerCore` runs every stateful model on a length of silence before its first real
//! sample (`NAM/dsp.cpp`'s `DSP::prewarm`), so every host running the reference — and every render
//! this project's goldens are compared against — presents a model that has already settled. Namir
//! did not, and the gap was measured on real trainer-produced exports at roughly -30 dB of error
//! over the first ~85 ms against -105 to -138 dB once settled
//! (`docs/manual-tests/fr-nam-030-real-a2-models.md`).
//!
//! What is asserted here is the arithmetic and the mechanism; that a prewarmed Namir matches a real
//! reference render *from sample 0* is asserted by `tests/golden_reference.rs`, which now drives
//! `new_state_prewarmed` rather than prewarming by hand.
//!
//! The counts are pinned to literals deliberately. A formula re-derived in the test is a test of
//! nothing — it would follow any edit to the implementation it is supposed to guard — so these are
//! the reference's own numbers, taken from the arithmetic already recorded in
//! `tests/a2_fixtures.rs` and `tests/golden_reference.rs`.

use namir_fixtures::nam::{A2Shape, LstmShape, WaveNetShape};

/// The real A2 shapes' receptive field, as recorded at `tests/a2_fixtures.rs`'s
/// `A2_PARITY_PROBE_SAMPLES` and `tests/golden_reference.rs`'s `assert_a2_golden`: 6 331 through
/// the 23 dilated layers (kernels 6/15, dilations to 239) plus 15 for the 16-tap head.
const A2_RECEPTIVE_FIELD: usize = 6_346;

/// The reference's own figure for an LSTM at 48 kHz — half a second, `NAM/lstm.cpp:127-134` — and
/// the value `tests/golden_reference.rs` prewarmed by hand before this existed.
const LSTM_PREWARM_AT_48K: usize = 24_000;

fn load(bytes: &[u8]) -> namir_nam::PreparedNam {
    namir_nam::load(bytes).expect("fixture should load")
}

/// The `+ 1` in each WaveNet expectation is the term issue #173 quotes the reference as adding for
/// an absent `condition_dsp`, unconditionally 1 here because this crate rejects `condition_dsp` at
/// load. The reference is not vendored in this repository, so that attribution rests on the issue's
/// quotation; what this test actually pins is the receptive-field arithmetic, against a figure two
/// other test files recorded independently. See `PreparedWaveNet::prewarm_samples`' doc comment for
/// the full statement of what is and is not corroborated in-tree.
//
// Deliberately carries no `trace:` tag of any form. FR-NAM-030's traced artifact is
// `tests/golden_reference.rs`, whose `Verify: G` comparison against the real reference is what the
// requirement's own method asks for; that artifact got stronger this pass by being moved onto the
// production prewarm path, which is a change to its evidence and not to its claim. This file pins
// the length and the mechanism — supporting evidence, and tagging it would either over-claim a
// `Verify: G` a unit test cannot perform or demote a requirement that is genuinely covered.
#[test]
fn prewarm_length_matches_the_reference_formula_for_every_architecture() {
    for (shape, name) in [(A2Shape::Full, "a2-full"), (A2Shape::Lite, "a2-lite")] {
        let model = load(
            &namir_fixtures::nam::generate_a2(shape, 30)
                .expect("A2 fixture")
                .to_json_bytes(),
        );
        assert_eq!(
            model.prewarm_samples(),
            A2_RECEPTIVE_FIELD + 1,
            "{name}: prewarm should be the receptive field plus the condition term"
        );
    }

    // A1's head is a kernel-1 `Conv1D`, contributing no history, so its whole prewarm is the
    // dilated stack plus the condition term — the same shape of arithmetic, a much smaller number.
    let a1 = load(
        &namir_fixtures::nam::generate(WaveNetShape::Nano, 30)
            .expect("A1 fixture")
            .to_json_bytes(),
    );
    let a1_prewarm = a1.prewarm_samples();
    assert!(
        a1_prewarm > 1 && a1_prewarm < A2_RECEPTIVE_FIELD,
        "A1-Nano's prewarm should be a real receptive field well under A2's, got {a1_prewarm}"
    );

    let lstm = load(
        &namir_fixtures::nam::generate_lstm(LstmShape::Tiny, 30)
            .expect("LSTM fixture")
            .to_json_bytes(),
    );
    assert_eq!(lstm.prewarm_samples(), LSTM_PREWARM_AT_48K);
}

/// The property that makes an over-estimate safe for WaveNet, and the one that would break if
/// `prewarm_samples` ever returned less than the true receptive field: past that many samples of
/// silence, the state is settled, so *more* silence changes nothing at all. Asserted bit-exactly,
/// because it is an exact claim about a finite-memory network rather than a numerical tolerance.
#[test]
fn a_wavenet_is_fully_settled_once_its_prewarm_length_of_silence_has_gone_through() {
    let model = load(
        &namir_fixtures::nam::generate_a2(A2Shape::Lite, 30)
            .expect("A2 fixture")
            .to_json_bytes(),
    );
    let probe: Vec<f32> = (0..512).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();

    let mut exact = model.new_state_prewarmed(probe.len());
    let from_exact = model.process(&mut exact, &probe);

    let mut over = model.new_state_prewarmed(probe.len());
    for _ in 0..4 {
        let _ = model.process(&mut over, &vec![0.0f32; probe.len()]);
    }
    let from_over = model.process(&mut over, &probe);

    assert_eq!(
        from_exact, from_over,
        "extra silence past the prewarm length must change nothing"
    );
}

/// A fresh state and a prewarmed one must actually differ for a model with trained (non-zero)
/// biases — otherwise every other assertion here would hold vacuously, which is exactly how the
/// A2 goldens' zero-bias fixtures behave (`tests/golden_reference.rs`'s `assert_a2_golden`).
#[test]
fn prewarming_an_lstm_changes_its_output() {
    let model = load(
        &namir_fixtures::nam::generate_lstm(LstmShape::Tiny, 30)
            .expect("LSTM fixture")
            .to_json_bytes(),
    );
    let probe = vec![0.25f32; 256];

    let mut cold = model.new_state(probe.len());
    let cold_out = model.process(&mut cold, &probe);

    let mut warm = model.new_state_prewarmed(probe.len());
    let warm_out = model.process(&mut warm, &probe);

    assert_ne!(
        cold_out, warm_out,
        "an LSTM starts from its declared h0/c0, so a prewarm must move it"
    );
}

/// NFR-SEC-020: the prewarm length is work performed at load time, so it needs a ceiling of its
/// own. A `.nam` file's `sample_rate` is validated only as nonzero, so a hostile file can ask an
/// LSTM for `u32::MAX / 2` samples of inference -- a load that never finishes, from a file that
/// parses cleanly. Exercised through a real file rather than by asserting the constant against
/// itself.
#[test]
fn an_absurd_declared_sample_rate_cannot_demand_an_unbounded_prewarm() {
    let mut fixture =
        namir_fixtures::nam::generate_lstm(LstmShape::Tiny, 30).expect("LSTM fixture");
    fixture.sample_rate = u32::MAX;
    let model = load(&fixture.to_json_bytes());

    let prewarm = model.prewarm_samples();
    assert!(
        prewarm <= 1 << 18,
        "a hostile sample rate must not demand {prewarm} samples of load-time inference"
    );

    // And the clamp must not have broken the ordinary path: this still has to be a usable model.
    let probe = vec![0.1f32; 64];
    let mut state = model.new_state_prewarmed(probe.len());
    assert_eq!(model.process(&mut state, &probe).len(), probe.len());
}
