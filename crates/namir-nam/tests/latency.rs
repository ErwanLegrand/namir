//! FR-NAM-110 (Must, `Verify: U`): "Namir shall report the model stage's processing latency in
//! samples, and shall report zero if the architecture is causal and introduces none." *Verify:* U
//! — **cross-correlate an impulse through the stage.**
//!
//! **Why this file exists** (M14, roadmap §21 Phase 4). Until it did, FR-NAM-110's entire evidence
//! was two unit tests — `wavenet.rs`'s and `lstm.rs`'s `latency_samples_is_zero` — each of which
//! read an accessor whose body is the literal `0` and asserted it equalled `0`. Both would have
//! passed **unchanged** if inference had introduced ten samples of delay, because neither ran any
//! audio: they compared a constant against itself. That is a test of the accessor, not of the
//! claim the accessor makes, and it is why both carried a `trace-partial:`.
//!
//! This file performs the requirement's own method instead: it drives a real impulse through a
//! real loaded model and cross-correlates the result against the impulse, so the reported figure is
//! checked against a *measured* one. It spans all four configurations this crate runs — WaveNet-A1,
//! WaveNet-A2 in both configurations FR-NAM-150 names, and LSTM — because "the architecture is
//! causal" is a per-architecture claim.
//!
//! # What is measured, and why it is sharp rather than approximate
//!
//! A NAM model is non-linear, so "the impulse response" is not defined the way it is for a filter,
//! and a raw cross-correlation of input against output would be dominated by the model's
//! *zero-input* response — the DC term its biases produce from silence, which has nothing to do
//! with the impulse. So each model is run twice from identical fresh state: once over silence, once
//! over the same silence with a single impulse in it. The difference of the two outputs is the
//! impulse's contribution and nothing else, and *that* is what the impulse is cross-correlated
//! against.
//!
//! Two properties are then asserted, and the first is exact rather than tolerance-based. Before the
//! impulse arrives, the two runs have performed bit-identical arithmetic on bit-identical inputs
//! from bit-identical state, so their outputs are equal to the bit and the correlation at every
//! negative lag is **exactly** `0.0`. Any non-zero there is anticipation — an output that moved
//! before its cause — which no amount of floating-point drift can produce. The onset lag is then
//! the first lag whose correlation is not exactly zero, and *that* is the measured latency the
//! model's reported [`namir_nam::PreparedNam::latency_samples`] is checked against. A model that
//! delayed its output by one sample would move the onset to lag 1 and fail.
//!
//! The peak lag is reported too — the argmax the phrase "cross-correlate an impulse" most directly
//! suggests — but it is deliberately *not* what the assertion turns on: for a model whose impulse
//! response peaks after its onset, the peak lag is a property of the response's shape, not of the
//! stage's latency. It is asserted only to be non-negative and is printed for the record.
//!
//! # What this does not cover
//!
//! FR-NAM-110 says "the model **stage**", and in Namir's architecture that is `namir-engine`'s
//! `NamStage`, which adds its own `SlotResampler` latency when the model rate differs from the
//! engine rate. This file verifies the model's own contribution — the zero this requirement's
//! second clause is about. The resampler's non-zero figure is asserted only as `> 0` in
//! `namir-engine` and its own doc comment says it is not verified sample-exactly
//! (`crates/namir-engine/src/stages/nam.rs:395`); that residue is what keeps FR-NAM-110's tag
//! `trace-partial:` here, narrowed to it.

use namir_fixtures::nam::{A2Shape, LstmShape, WaveNetShape};

/// Total probe length, in samples. Comfortably past the real A2 shapes' 6 346-sample receptive
/// field, so a delay introduced anywhere in the dilation stack has room to show up after the
/// impulse rather than falling off the end of the probe.
const PROBE_SAMPLES: usize = 8_192;

/// Where the impulse sits in the probe. Deliberately not at index 0: everything before it is the
/// negative-lag window this file's exact-zero anticipation check needs, and 2 048 samples of it is
/// far more than any plausible look-ahead a bug could introduce.
const IMPULSE_INDEX: usize = 2_048;

/// Impulse amplitude. Large enough that its effect clears no threshold-hunting (nothing here uses a
/// threshold), small enough to stay inside the range a generated fixture was RMS-calibrated for.
const IMPULSE_AMPLITUDE: f32 = 0.5;

/// The impulse's contribution to the output, isolated from the model's zero-input (bias-driven)
/// response by running the same model twice from fresh state and differencing.
fn impulse_contribution(model_bytes: &[u8]) -> Vec<f64> {
    let prepared = namir_nam::load(model_bytes).expect("fixture should load");

    let silence = vec![0.0f32; PROBE_SAMPLES];
    let mut with_impulse = silence.clone();
    with_impulse[IMPULSE_INDEX] = IMPULSE_AMPLITUDE;

    let mut baseline_state = prepared.new_state(PROBE_SAMPLES);
    let baseline = prepared.process(&mut baseline_state, &silence);

    let mut impulse_state = prepared.new_state(PROBE_SAMPLES);
    let driven = prepared.process(&mut impulse_state, &with_impulse);

    assert_eq!(baseline.len(), driven.len());
    driven
        .iter()
        .zip(baseline.iter())
        .map(|(&d, &b)| f64::from(d) - f64::from(b))
        .collect()
}

/// Cross-correlation of the unit impulse against `contribution`, one entry per lag in
/// `-IMPULSE_INDEX ..= max_lag`, in that order. The input is a single spike, so each term reduces
/// to one product rather than a sum — written as the general correlation anyway, because the
/// requirement's method names cross-correlation and a reader should be able to see that this is
/// what the code does.
fn cross_correlate(contribution: &[f64], max_lag: usize) -> Vec<f64> {
    let mut input = vec![0.0f64; PROBE_SAMPLES];
    input[IMPULSE_INDEX] = f64::from(IMPULSE_AMPLITUDE);

    let mut out = Vec::with_capacity(IMPULSE_INDEX + max_lag + 1);
    for lag in -(IMPULSE_INDEX as isize)..=(max_lag as isize) {
        let mut acc = 0.0f64;
        for (n, &x) in input.iter().enumerate() {
            if x == 0.0 {
                continue;
            }
            let shifted = n as isize + lag;
            if shifted >= 0 && (shifted as usize) < contribution.len() {
                acc += x * contribution[shifted as usize];
            }
        }
        out.push(acc);
    }
    out
}

/// Drives an impulse through `model_bytes` and asserts the model's reported latency equals the
/// latency the cross-correlation actually measures.
fn assert_reported_latency_is_the_measured_latency(name: &str, model_bytes: &[u8]) {
    let prepared = namir_nam::load(model_bytes).expect("fixture should load");
    let reported = prepared.latency_samples();

    let contribution = impulse_contribution(model_bytes);
    let max_lag = PROBE_SAMPLES - IMPULSE_INDEX - 1;
    let correlation = cross_correlate(&contribution, max_lag);

    // Lag 0 sits at index IMPULSE_INDEX of `correlation`, which starts at lag -IMPULSE_INDEX.
    let (negative, non_negative) = correlation.split_at(IMPULSE_INDEX);

    // Exact, not approximate: before the impulse, both runs did bit-identical arithmetic. See this
    // module's doc comment.
    for (k, &r) in negative.iter().enumerate() {
        assert_eq!(
            r,
            0.0,
            "{name}: non-zero cross-correlation at lag {} — the stage responded before its cause, \
             which is anticipation, not latency",
            k as isize - IMPULSE_INDEX as isize
        );
    }

    let onset_lag = non_negative
        .iter()
        .position(|&r| r != 0.0)
        .unwrap_or_else(|| {
            panic!(
                "{name}: the impulse produced no output deviation at any lag — the probe never \
                    reached the stage, so this test would have passed vacuously"
            )
        });

    let peak_lag = non_negative
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
        .map(|(k, _)| k)
        .expect("the non-negative lag window is never empty");

    println!(
        "{name}: reported {reported} samples; measured onset lag {onset_lag}, peak lag {peak_lag}"
    );

    assert_eq!(
        onset_lag as u32, reported,
        "{name}: reports {reported} samples of latency but the impulse cross-correlation measures \
         {onset_lag}. This is the assertion `latency_samples_is_zero` could not make: it compared \
         a constant against itself and never ran a sample of audio through the model."
    );
    assert!(
        peak_lag >= onset_lag,
        "{name}: impulse response peaks at lag {peak_lag}, before its own onset at {onset_lag}"
    );
}

/// FR-NAM-110's remaining gap after this file, narrowed from what M9a's field named. The method
/// *is* now performed, for every architecture, so the half that said "performed by nothing" is
/// false. What survives is the other half, and it is in a crate this test cannot reach: the stage's
/// resampler latency. It is not re-scoped away here because FR-NAM-110's subject really is the
/// stage, not the model.
// trace-partial: FR-NAM-110
// uncovered: FR-NAM-110 — the model's own latency is now measured by cross-correlating an impulse
// uncovered: through every architecture (this file), so the reported figure would fail if
// uncovered: inference introduced delay. What is still unverified is the other figure the
// uncovered: requirement's "model stage" covers: namir-engine's NamStage adds SlotResampler
// uncovered: latency when the model rate differs from the engine rate, and that value is asserted
// uncovered: only as > 0 (stages/nam.rs:1441-1459) with its own doc comment recording that it is
// uncovered: not verified sample-exactly (:395); no impulse is cross-correlated through the
// uncovered: assembled stage at a mismatched rate; closes M14
#[test]
fn reported_latency_matches_an_impulse_cross_correlation_for_every_architecture() {
    let wavenet_a1 = namir_fixtures::nam::generate(WaveNetShape::Nano, 30)
        .expect("A1 fixture")
        .to_json_bytes();
    assert_reported_latency_is_the_measured_latency("wavenet-a1-nano", &wavenet_a1);

    let lstm = namir_fixtures::nam::generate_lstm(LstmShape::Tiny, 30)
        .expect("LSTM fixture")
        .to_json_bytes();
    assert_reported_latency_is_the_measured_latency("lstm-tiny", &lstm);

    for (shape, name) in [(A2Shape::Full, "a2-full"), (A2Shape::Lite, "a2-lite")] {
        let a2 = namir_fixtures::nam::generate_a2(shape, 30)
            .expect("A2 fixture")
            .to_json_bytes();
        assert_reported_latency_is_the_measured_latency(name, &a2);
    }
}
