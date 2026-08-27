//! M14 Phase 4's shared probe-signal harness: the pieces every chain-level and stage-level probe
//! needs — a deterministic probe signal, a way to drive a real [`Chain`] with it block by block
//! inside the D-7.5 allocation harness, the two fixture loaders, and the handful of estimators the
//! assertions are written against.
//!
//! # Why this module exists
//!
//! Roadmap §21 Phase 4's argument, in one sentence: *both DSP defects M9b found were found by
//! running tests that had never run against a loaded chain.* Six Must requirements
//! (FR-CHAIN-010/-020/-050/-060/-080 and NFR-PERF-020) were annotated against tests that feed
//! silence into an empty chain, which cannot distinguish any ordering of any stages from an empty
//! chain — and every one of them needs the same four things: a signal, a chain built the way the
//! product builds it, resources actually loaded into it, and a way to say what came out. Written
//! once here rather than six times, so a probe that is wrong is wrong in one place.
//!
//! # Test-only, and RT-safe where it has to be
//!
//! `#[cfg(test)]` in `lib.rs`, exactly like [`crate::rt_harness`] and [`crate::test_support`]:
//! nothing here is linked into a product build. Everything expensive — building fixtures,
//! allocating the output vectors, constructing a [`Resource`] — happens *outside*
//! [`crate::rt_harness::audio_section`]; the only thing inside it is [`Chain::process`] itself, so
//! every probe run doubles as NFR-RT-010 evidence for whatever configuration it drove.
//!
//! # What a caller is expected to supply
//!
//! Probe signals are plain `Vec<f32>` per channel ([`duplicated`] widens a mono signal to the
//! channel count a configuration needs). [`run`] and [`run_with`] return the same shape back. The
//! deliberate absence of a `Probe` struct is so a caller can build its own signal (an impulse, a
//! step, a burst, a NaN) without this module having to anticipate it — only the two generators
//! more than one probe already needs, [`sine`] and [`chirp`], are here, and a generator with one
//! caller belongs next to that caller until it has two.

use std::sync::Arc;

use namir_core::{ChannelConfig, SampleRate};
use namir_fixtures::ir::{decaying_noise, to_mono_wav_bytes, to_stereo_wav_bytes};
use namir_fixtures::nam::{WaveNetShape, generate};
use namir_ir::PreparedIr;
use namir_nam::PreparedNam;

use crate::chain::Chain;
use crate::command::Command;
use crate::param::{ParamChange, ParamId};
use crate::prepare::PrepareContext;
use crate::rt_harness::audio_section;
use crate::stage_io::StageIo;

/// The engine rate every probe uses unless it is specifically testing rate conversion.
pub const SR: u32 = 48_000;
/// The block size every probe uses unless it is specifically testing block-size variation.
pub const BLOCK: usize = 64;

/// A [`PrepareContext`] at [`SR`]/[`BLOCK`] for `channel_config`.
pub fn ctx(channel_config: ChannelConfig) -> PrepareContext {
    ctx_at(SR, BLOCK, channel_config)
}

/// A [`PrepareContext`] at an explicit rate and block size.
pub fn ctx_at(sample_rate_hz: u32, block: usize, channel_config: ChannelConfig) -> PrepareContext {
    PrepareContext::new(
        SampleRate::new(sample_rate_hz).expect("probe sample rates are valid"),
        block,
        channel_config,
    )
    .expect("probe prepare contexts are valid")
}

// ---------------------------------------------------------------------------------------------
// Fixtures (D-19.1: generated, never captured).
// ---------------------------------------------------------------------------------------------

/// A loaded WaveNet model that *declares* `declared_rate_hz`.
///
/// The declared rate is a parameter rather than a constant because it is the only thing in the
/// whole chain that makes [`Chain::latency_samples`] nonzero: a model whose rate differs from the
/// engine's engages `nam.rs`'s `SlotResampler`, which is what NFR-PERF-020's probe needs and what
/// nothing had ever measured a group delay against.
pub fn nam_model(shape: WaveNetShape, seed: u64, declared_rate_hz: u32) -> Arc<PreparedNam> {
    let mut model = generate(shape, seed).expect("fixture should generate");
    model.sample_rate = declared_rate_hz;
    Arc::new(namir_nam::load(&model.to_json_bytes()).expect("generated fixture should load"))
}

/// A one-channel impulse response of `taps` decaying-noise taps, prepared for `engine_rate`.
pub fn mono_ir(seed: u64, taps: usize, engine_rate: u32, block: usize) -> Arc<PreparedIr> {
    let bytes = to_mono_wav_bytes(&decaying_noise(taps, seed, 128.0), engine_rate);
    prepare_ir(&bytes, engine_rate, block)
}

/// A genuinely two-channel impulse response: two *different* decaying-noise tap sets, so a probe
/// can tell a real stereo IR apart from the same mono IR duplicated (FR-CHAIN-060's "stereo IR"
/// cell, which is exactly the distinction a dual-mono IR does not make).
pub fn stereo_ir(seed: u64, taps: usize, engine_rate: u32, block: usize) -> Arc<PreparedIr> {
    let left = decaying_noise(taps, seed, 128.0);
    let right = decaying_noise(taps, seed ^ 0x5eed_5eed, 96.0);
    let bytes = to_stereo_wav_bytes(&left, &right, engine_rate);
    prepare_ir(&bytes, engine_rate, block)
}

fn prepare_ir(wav_bytes: &[u8], engine_rate: u32, block: usize) -> Arc<PreparedIr> {
    Arc::new(
        PreparedIr::from_wav_bytes(
            wav_bytes,
            SampleRate::new(engine_rate).expect("probe sample rates are valid"),
            block,
        )
        .expect("generated IR should load"),
    )
}

// ---------------------------------------------------------------------------------------------
// Driving a chain.
// ---------------------------------------------------------------------------------------------

/// Offers `model` to `chain` through the same [`Resource`](crate::resource::Resource) a worker
/// would build (D-8.1 step 1 + 2), so a probe exercises the product's install path rather than
/// `NamStage::load_model`'s test-only shortcut.
///
/// Not RT-safe (it builds the slot); call it before or between runs, never inside a block.
pub fn load_nam(chain: &mut Chain, model: Arc<PreparedNam>, ctx: &PrepareContext) {
    let Command::Load(resource) = Command::load_nam(model, ctx) else {
        unreachable!("Command::load_nam builds a Load by construction");
    };
    let mut offer = Some(resource);
    chain.offer(&mut offer);
    assert!(
        offer.is_none(),
        "no stage accepted the NAM resource — is this chain missing its Nam stage?"
    );
}

/// [`load_nam`]'s Ir counterpart.
pub fn load_ir(chain: &mut Chain, ir: Arc<PreparedIr>, ctx: &PrepareContext) {
    let Command::Load(resource) = Command::load_ir(ir, ctx) else {
        unreachable!("Command::load_ir builds a Load by construction");
    };
    let mut offer = Some(resource);
    chain.offer(&mut offer);
    assert!(
        offer.is_none(),
        "no stage accepted the IR resource — is this chain missing its Ir stage?"
    );
}

/// Applies one parameter change by its `namir-params` descriptor id, converting to this crate's
/// RT-facing [`ParamId`] the same way [`Chain`] itself does (D-10.4).
pub fn set_param(chain: &mut Chain, descriptor: namir_params::ParamId, value: f32) {
    chain.apply(ParamChange {
        id: ParamId(descriptor.0),
        value,
    });
}

/// Runs `input` (one `Vec<f32>` per channel, all the same length) through `chain` in `block`-frame
/// blocks, returning the output in the same shape.
pub fn run(chain: &mut Chain, input: &[Vec<f32>], block: usize) -> Vec<Vec<f32>> {
    run_with(chain, input, block, |_, _| {})
}

/// [`run`] with a hook that fires immediately *before* each block, so a probe can flip a parameter
/// or offer a resource mid-signal without leaving the run.
///
/// The hook runs outside [`audio_section`] and may allocate; only [`Chain::process`] is inside it.
pub fn run_with(
    chain: &mut Chain,
    input: &[Vec<f32>],
    block: usize,
    mut at_block: impl FnMut(usize, &mut Chain),
) -> Vec<Vec<f32>> {
    assert!(!input.is_empty(), "a probe needs at least one channel");
    let frames = input[0].len();
    assert!(
        input.iter().all(|c| c.len() == frames),
        "every probe channel must be the same length"
    );

    let mut out: Vec<Vec<f32>> = input.iter().map(|_| Vec::with_capacity(frames)).collect();
    let mut scratch: Vec<Vec<f32>> = input.iter().map(|_| vec![0.0f32; block]).collect();

    let mut offset = 0;
    let mut index = 0;
    while offset < frames {
        at_block(index, chain);
        let n = block.min(frames - offset);
        for (channel, buf) in input.iter().zip(scratch.iter_mut()) {
            buf[..n].copy_from_slice(&channel[offset..offset + n]);
        }
        {
            let mut refs: Vec<&mut [f32]> = scratch.iter_mut().map(|b| &mut b[..n]).collect();
            let mut io = StageIo::new(&mut refs, n);
            audio_section(|| chain.process(&mut io));
        }
        for (buf, channel) in scratch.iter().zip(out.iter_mut()) {
            channel.extend_from_slice(&buf[..n]);
        }
        offset += n;
        index += 1;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Probe signals.
// ---------------------------------------------------------------------------------------------

/// A steady sine. The workhorse probe: loud enough to hold the noise gate open, quiet enough that
/// FR-CHAIN-090's default 0 dBFS ceiling never clips it.
pub fn sine(len: usize, freq_hz: f32, sample_rate: u32, amplitude: f32) -> Vec<f32> {
    let step = std::f32::consts::TAU * freq_hz / sample_rate as f32;
    (0..len)
        .map(|i| amplitude * (step * i as f32).sin())
        .collect()
}

/// A linear-frequency sweep. Broadband, so every stage that shapes the spectrum (EQ, IR, the NAM
/// resampler) leaves a mark on it — which is what makes a permuted chain distinguishable from the
/// shipped one, and what makes a cross-correlated delay estimate well conditioned.
pub fn chirp(len: usize, from_hz: f32, to_hz: f32, sample_rate: u32, amplitude: f32) -> Vec<f32> {
    let sr = sample_rate as f32;
    let mut phase = 0.0f32;
    (0..len)
        .map(|i| {
            let t = i as f32 / len.max(1) as f32;
            let f = from_hz + (to_hz - from_hz) * t;
            let s = amplitude * phase.sin();
            phase += std::f32::consts::TAU * f / sr;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
            s
        })
        .collect()
}

/// `mono` repeated into `channels` identical channels — the shape both `MonoToStereo` and any
/// correlated-stereo probe needs (`stage_io.rs`: a chain's channel count is fixed to
/// `output_channels()`, so a mono source arrives already duplicated).
pub fn duplicated(mono: &[f32], channels: usize) -> Vec<Vec<f32>> {
    (0..channels).map(|_| mono.to_vec()).collect()
}

// ---------------------------------------------------------------------------------------------
// Estimators the assertions are written against.
// ---------------------------------------------------------------------------------------------

/// Largest absolute sample.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Largest absolute sample-to-sample step — the click detector every handover and bypass test in
/// this crate is built on (`engine.rs`'s `max_abs_first_difference`, defined once here now that
/// more than one module needs it).
pub fn max_abs_first_difference(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max)
}

/// Largest absolute difference between two equal-length signals.
pub fn max_abs_difference(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "signals must be the same length");
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max)
}

/// The quietest `window`-long window's peak — the dropout detector FR-NAM-070's method asks for
/// ("no dropout"), stated as a number so a caller can assert a floor on it.
pub fn min_window_peak(samples: &[f32], window: usize) -> f32 {
    samples
        .chunks(window.max(1))
        .map(peak)
        .fold(f32::INFINITY, f32::min)
}

/// The lag, in samples, at which `output` best correlates with `input` — the chain's measured
/// group delay.
///
/// Normalised cross-correlation over `0..=max_lag`, so an overall gain change (every stage in the
/// chain applies one) cannot move the peak. Deliberately one-sided: a causal chain cannot have a
/// negative delay, and allowing negative lags would let a periodic probe alias onto one.
pub fn estimate_delay_samples(input: &[f32], output: &[f32], max_lag: usize) -> usize {
    let mut best_lag = 0;
    let mut best_score = f32::NEG_INFINITY;
    for lag in 0..=max_lag {
        if lag >= output.len() {
            break;
        }
        let n = (output.len() - lag).min(input.len());
        if n == 0 {
            break;
        }
        let x = &input[..n];
        let y = &output[lag..lag + n];
        let mut num = 0.0f64;
        let mut den_x = 0.0f64;
        let mut den_y = 0.0f64;
        for (&a, &b) in x.iter().zip(y.iter()) {
            num += a as f64 * b as f64;
            den_x += a as f64 * a as f64;
            den_y += b as f64 * b as f64;
        }
        let den = (den_x * den_y).sqrt();
        if den <= 0.0 {
            continue;
        }
        let score = (num / den) as f32;
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }
    best_lag
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The delay estimator is the only piece of this module carrying real arithmetic, and every
    /// NFR-PERF-020 assertion is written against it — so it is checked against a signal whose
    /// delay is known by construction before it is trusted with one that is not.
    #[test]
    fn the_delay_estimator_recovers_a_known_delay() {
        let input = chirp(4096, 200.0, 6000.0, SR, 0.5);
        for delay in [0usize, 1, 37, 512] {
            let mut output = vec![0.0f32; delay];
            output.extend(input.iter().map(|s| s * 0.37));
            assert_eq!(
                estimate_delay_samples(&input, &output, 1024),
                delay,
                "estimator missed a constructed delay of {delay}"
            );
        }
    }

    #[test]
    fn min_window_peak_finds_a_dropout() {
        let mut signal = sine(1024, 220.0, SR, 0.5);
        assert!(min_window_peak(&signal, 32) > 0.1);
        signal[256..320].fill(0.0);
        assert!(min_window_peak(&signal, 32) < 1e-6);
    }
}
