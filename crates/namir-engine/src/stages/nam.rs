//! Nam stage (FR-NAM-\*): wraps `namir_nam::PreparedNam`/`NamState` with the D-8.1
//! crossfade-capable dual-resource shape, D-9.2/9.3's per-block-rate-mismatch resampling, the
//! shared per-stage bypass crossfade (FR-CHAIN-020), and FR-CHAIN-050's mono-core-then-duplicate
//! channel handling.
//!
//! # The four-step handover, and where each step now lives
//!
//! FR-NAM-070 (glitch-free model swap) is D-8.1's four-step protocol: *prepare* (a worker builds
//! the whole [`NamSlot`] — see `crate::resource`'s module doc for why the slot, not just the
//! `Arc`), *offer* (through the command ring, arriving here as [`Stage::accept_resource`]),
//! *crossfade* (this file's [`Crossfade`] and the equal-power blend in `process_channel0`), and
//! *retire* (parked in `self.retired`, moved to the return ring by [`Stage::collect_retired`]).
//!
//! M2 built step 3 alone and proved it before a thread had to drive it; M4 wired the other three
//! around it, which is why the crossfade itself needed no rework.
//!
//! **The M2 gap this module used to document is closed.** Between M2 and M4 a completing handover
//! dropped the outgoing slot right here, on the audio thread — freeing its `NamState` scratch and
//! possibly the last `Arc<PreparedNam>` reference — and this comment recorded that as a real P1
//! violation rather than hiding it. It is gone: the finalization in `process_channel0` now `take()`s
//! the slot (a move) into `self.retired`, and the return ring carries it to a worker that can
//! afford to free it. The evidence is that this module's RT-allocation tests no longer stop short
//! of completion, and `handover_crossfade_has_no_large_single_sample_jump` now drives a full
//! real-to-real handover *inside* `rt_harness::audio_section` — which it could not do before.
//! There is a second, subtler drop site closed at the same time: an install that displaces a slot
//! still fading in used to drop the displaced slot. See [`NamStage::install`].
//!
//! # Resampling (D-9.2/9.3)
//!
//! Each slot independently resamples around the model it holds: engine rate → model rate before
//! inference, model rate → engine rate after. When a slot's model declares the same rate as the
//! engine, `NamSlot::resample` is `None` and inference runs directly on the engine's own blocks —
//! D-9.2's "bypassed entirely... zero cost and zero added latency" for the overwhelmingly common
//! 48 kHz case. When rates differ, [`SlotResampler`] runs the model on a **fixed internal block**
//! (D-9.2's own phrase) via `rubato::FftFixedInOut`, with an input FIFO accumulating engine-rate
//! samples until a full fixed block is ready and an output FIFO holding resampled-back results
//! until this stage's caller asks for them — see that struct's doc comment for the exact shape
//! and for this module's honest account of what is and isn't proven about it.
//!
//! # Why this is mono-core
//!
//! Same invariant `gate.rs` relies on (FR-CHAIN-050): by the time this stage runs, every channel
//! of `io` already carries an identical signal. Both slots (and any in-flight crossfade between
//! them) run on channel 0 only; the result is duplicated onto every other channel via the
//! scratch-shuttle pattern before the shared bypass blend runs.
//!
//! # FR-CHAIN-040 vs the handover crossfade — two independent fades, composed
//!
//! This stage carries *two* separate fade mechanisms that must not be confused:
//! 1. The **handover crossfade** ([`Crossfade`]), internal to this stage: an equal-power blend
//!    between `slots[active]`'s output and `slots[1 - active]`'s output, driven by
//!    [`NamStage::load_model`], independent of whether the stage is enabled at all.
//! 2. The **shared per-stage bypass blend** (`mix`/`mix_target`/`mix_coeff`, the same pattern
//!    `gate.rs`/`trim.rs` use), which blends the handover crossfade's *result* against this
//!    stage's dry input, based on `enabled && slots[active].is_some()`.
//!
//! `mix_target` is recomputed every time `enabled` changes, every time `active` changes (i.e. when
//! a handover completes) and at the start of every handover. Loading a *replacement* model into an
//! already fully-engaged stage never moves it: it is already 1.0 and stays there, so the handover
//! crossfade's own equal-power blend is heard in full.
//!
//! **Loading the very first model used to be the exception, and issue #141 is that it was the
//! wrong one.** `mix_target` was a function of `slots[active]` alone — deliberately the
//! *pre-handover* active slot, which on a first load is `None` — so the bypass blend stayed pinned
//! at 0.0 for the whole handover and multiplied the equal-power fade's result out of existence.
//! Measured on this stage: bit-exactly the dry input for all 960 samples of the fade, with the
//! model first becoming audible at the *start of the block* the fade completed in (frame 512, 768,
//! 896 and 959 for block sizes 512, 256, 64 and 1). FR-NAM-070's fade was inaudible and what
//! replaced it was the 15 ms bypass blend at a block-quantised instant. Since #141 the target
//! counts the slot a fade is fading *into* as well, and [`NamStage::begin_crossfade`] moves `mix`
//! there at once rather than ramping — read that method's doc comment for why an instant move is
//! the click-free choice here and a ramp is not.

use std::collections::VecDeque;
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

use namir_core::SampleRate;
use namir_dsp::GainRamp;
use namir_nam::{NamState, PreparedNam};
use namir_params::ParamKind;
use namir_params::stages::nam::{
    ENABLED, NORMALIZE_ENABLED, NORMALIZE_OFFSET_DB, TARGET_LOUDNESS_LUFS,
};
use rubato::{FftFixedInOut, Resampler};

use crate::command::RetireSink;
use crate::param::{ParamChange, ParamId};
use crate::prepare::{PrepareContext, PrepareError};
use crate::resource::{Resource, ResourceKind};
use crate::stage::{Stage, StagePrep};
use crate::stage_io::StageIo;
use crate::stages::HANDOVER_CROSSFADE_MS;
use crate::telemetry::{TelemetryEntry, TelemetrySink};

/// The shared per-stage bypass crossfade's one-pole time constant (FR-CHAIN-020) — same figure
/// and same rationale as `gate.rs`'s identical constant: not derived from an FRS requirement,
/// this stage's own documented choice for the shared pattern.
const BYPASS_CROSSFADE_TIME_CONSTANT_MS: f64 = 15.0;

/// D-9.2's "fixed internal block" for a resampled slot, expressed as the minimum FFT length
/// `rubato::FftFixedInOut` must end up using **in the lower of the two rates' domain** —
/// `resample_chunk_frames` turns this into the `chunk_size_in` the constructor actually takes.
/// Chosen independent of `ctx.max_block_size()` on purpose: D-9.2 wants the model's own internal
/// block size, and therefore the resampler's latency, to be a property of the *stage*, not of
/// whatever block size the host happens to be calling with this session.
///
/// **256 is a measured figure, not a round number** (M9b, FR-NAM-060 — see
/// `resampler_frequency_response_meets_the_stopband_and_ripple_bar`). `rubato`'s FFT resampler
/// builds its antialiasing filter as a `BlackmanHarris2`-windowed sinc whose length *is* the FFT
/// size, with its cutoff placed by `rubato::calculate_cutoff` at
/// `1/(1 + 13.745/n + 121.7/n² + 5964/n³)` of the lower Nyquist for a length-`n` filter — so a
/// short filter does not merely have a wide transition band, it has its passband edge pulled *in*.
/// At n = 64 (which is what a flat 256-frame chunk yielded at a 192 kHz engine rate against a
/// 48 kHz model) the cutoff lands at 0.789 × 24 kHz ≈ 18.9 kHz and the response is **−15 dB at
/// 20 kHz**; at n = 147 (a 96 kHz engine against a 44.1 kHz model) it is −5.6 dB. At n = 256 the
/// cutoff is 0.947 × Nyquist and every rate pair in that test measures flat to 20 kHz well inside
/// FR-NAM-060's 0.1 dB.
const MIN_RESAMPLE_FFT_FRAMES: usize = 256;

/// Greatest common divisor, iterative Euclid — used only by [`resample_chunk_frames`], to
/// reproduce the same `gcd(rate_in, rate_out)` arithmetic `rubato::FftFixedInOut::new` does
/// internally so this module can *choose* its FFT size rather than discover it.
fn gcd(a: usize, b: usize) -> usize {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The `chunk_size_in` (engine-rate frames) to hand `rubato::FftFixedInOut::new` so that its
/// resulting FFT length in the **lower** of `engine_hz`/`model_hz`'s domain is at least
/// [`MIN_RESAMPLE_FFT_FRAMES`] — which is what FR-NAM-060's passband bar actually constrains, the
/// antialiasing filter's cutoff being placed relative to the *lower* Nyquist (see
/// [`MIN_RESAMPLE_FFT_FRAMES`]).
///
/// `FftFixedInOut::new` derives `fft_size_in = k · rate_in/gcd` and `fft_size_out = k ·
/// rate_out/gcd` with `k = ceil(chunk_size_in / (rate_in/gcd))`; it inverts that, picking the `k`
/// the bar needs and returning the exact `chunk_size_in` that produces it. Returning an exact
/// multiple (rather than a hint the constructor rounds up) is also what makes
/// `SlotResampler::new`'s round-trip symmetry assertions hold.
///
/// Never smaller than what a flat 256-frame request would have produced: when the engine rate is
/// the lower of the two, the two rules coincide exactly (`engine_hz.min(model_hz) == engine_hz`),
/// so the 48 kHz engine cases — including 48 kHz against a 44.1 kHz model — are unchanged by M9b.
fn resample_chunk_frames(engine_hz: usize, model_hz: usize) -> usize {
    let gcd = gcd(engine_hz, model_hz);
    let fft_unit_low = engine_hz.min(model_hz) / gcd;
    let fft_unit_engine = engine_hz / gcd;
    MIN_RESAMPLE_FFT_FRAMES.div_ceil(fft_unit_low) * fft_unit_engine
}

/// This stage's RT-facing `namir_engine::ParamId`, converted once from `namir_params`'s own id
/// for the same key — see `trim.rs`'s identical convention and its doc comment for why the two
/// crates carry distinct `ParamId` types on purpose.
const ENABLED_ID: ParamId = ParamId(ENABLED.id.0);
/// See [`ENABLED_ID`].
const NORMALIZE_ENABLED_ID: ParamId = ParamId(NORMALIZE_ENABLED.id.0);
/// See [`ENABLED_ID`].
const NORMALIZE_OFFSET_DB_ID: ParamId = ParamId(NORMALIZE_OFFSET_DB.id.0);

/// FR-NAM-090's normalisation-gain smoothing time constant. Same figure and same rationale as
/// `trim.rs`/`out.rs`'s identical constant — `gain_ramp.rs`'s own doc comment derives 20 ms as
/// the exact bound FR-PARAM-040 implies for a one-pole, with "very little margin" against `f32`
/// rounding at exactly that figure; 25 ms is that module's documented choice of comfortable
/// margin, reproduced here since its public API imposes no default of its own.
const NORMALIZE_GAIN_RAMP_TIME_CONSTANT_MS: f32 = 25.0;

/// Telemetry signal id: whether `slots[active]` currently holds a model (post-handover; a slot
/// that is only mid-handover-fade-in does not yet count, matching `latency_samples`'s own use of
/// `slots[active]`). Derived from a namespaced string the same way `namir-params`'s real
/// parameter ids are (this crate's shared telemetry-id convention) — a readout, not an
/// automatable parameter, so it is never added to `namir_params::REGISTRY`.
const TELEMETRY_LOADED: u32 = namir_params::ParamId::from_key("telemetry.nam.loaded").0;

/// Telemetry signal id: whether a handover crossfade is currently in flight. Two independent
/// readers need this — the UI (to show that an audition is still settling) and M4's own
/// `handover_crossfade` benchmark (to assert the fraction of blocks it actually measured with a
/// fade in flight, rather than trusting its own parameterisation). Same readout-not-parameter
/// convention as `TELEMETRY_LOADED`, so it is never added to `namir_params::REGISTRY` and
/// `params.lock` is unaffected.
const TELEMETRY_HANDOVER_ACTIVE: u32 =
    namir_params::ParamId::from_key("telemetry.nam.handover_active").0;

/// Builds [`NamStage`]. Holds no configuration of its own — this stage's one parameter
/// (`nam.enabled`) seeds its initial value straight from its `namir-params` descriptor (see
/// `prepare`'s body), and no model is loaded at construction (`slots` starts `[None, None]`,
/// FR-CHAIN-040's "nothing loaded behaves as bypassed").
pub struct NamPrep;

impl StagePrep for NamPrep {
    type Prepared = NamStage;

    /// Sizes every buffer `NamStage::process` will ever touch: the per-channel dry scratch the
    /// bypass crossfade needs, the channel-0-then-duplicate shuttle, and the two handover-fade
    /// scratch buffers (`crossfade_outgoing`/`crossfade_incoming`) — all sized to
    /// `ctx.max_block_size()`, never resized in `process`. Does **not** allocate anything
    /// slot-shaped: no model is loaded yet, and everything a loaded slot needs (inference state,
    /// resampler, FIFOs) is [`NamStage::load_model`]'s job, deliberately deferred out of the
    /// worker-thread-but-still-non-RT `prepare` path to the *even later*, explicitly-non-RT path
    /// a future M4 handover drives.
    fn prepare(&self, ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError> {
        let sample_rate = ctx.sample_rate();
        let max_block = ctx.max_block_size();
        let channel_count = ctx.channel_config().output_channels() as usize;

        let enabled_default_on = match ENABLED.kind {
            ParamKind::Stepped { default_index, .. } => default_index.0 == 1,
            ParamKind::Continuous { .. } => unreachable!("nam.enabled is declared Stepped"),
        };
        let normalize_enabled_default_on = match NORMALIZE_ENABLED.kind {
            ParamKind::Stepped { default_index, .. } => default_index.0 == 1,
            ParamKind::Continuous { .. } => {
                unreachable!("nam.normalize_enabled is declared Stepped")
            }
        };
        let normalize_offset_default_db = match NORMALIZE_OFFSET_DB.kind {
            ParamKind::Continuous { default, .. } => default,
            ParamKind::Stepped { .. } => {
                unreachable!("nam.normalize_offset_db is declared Continuous")
            }
        };

        let tau_samples = (BYPASS_CROSSFADE_TIME_CONSTANT_MS / 1000.0) * sample_rate.hz_f64();
        let mix_coeff = (1.0 - (-1.0_f64 / tau_samples).exp()) as f32;
        let crossfade_total_samples =
            ((HANDOVER_CROSSFADE_MS / 1000.0) * sample_rate.hz_f64()).round() as u32;

        Ok(NamStage {
            sample_rate,
            max_block_size: max_block,
            prepared_for: *ctx,
            slots: [None, None],
            active: 0,
            crossfade: None,
            crossfade_total_samples: crossfade_total_samples.max(1),
            enabled: enabled_default_on,
            normalize_enabled: normalize_enabled_default_on,
            normalize_offset_db: normalize_offset_default_db,
            // FR-CHAIN-040: nothing loaded behaves as bypassed, regardless of `enabled` — no
            // prior audio exists yet at stage creation either, so `mix` starts already settled at
            // its target rather than needing to ramp there.
            mix: 0.0,
            mix_target: 0.0,
            mix_coeff,
            dry: vec![vec![0.0; max_block]; channel_count],
            scratch: vec![0.0; max_block],
            crossfade_outgoing: vec![0.0; max_block],
            crossfade_incoming: vec![0.0; max_block],
            retired: None,
        })
    }
}

/// One loaded model: its immutable, shareable `Arc<PreparedNam>` (D-8.2 — shareable so a future
/// M4 process-global cache can hand the same `Arc` to every plugin instance using this model),
/// this instance's own mutable inference state, and — only when the model's declared sample rate
/// differs from the engine's — the D-9.2 resampler pair around it.
pub(crate) struct NamSlot {
    /// Immutable weights/config (D-9.1); cheap to clone (`Arc`) into a future cache or a
    /// crossfaded-out slot's replacement.
    model: Arc<PreparedNam>,
    /// This instance's own causal-conv history and reusable inference scratch. Sized (via
    /// `PreparedNam::new_state`) to `resample`'s fixed model-rate block when resampling is
    /// active, or to the stage's own `max_block_size` when it isn't.
    state: NamState,
    /// `None` exactly when `model.sample_rate() == engine sample rate` (D-9.2: "bypassed
    /// entirely... zero cost and zero added latency"); `Some` otherwise.
    resample: Option<SlotResampler>,
    /// FR-NAM-090: this slot's model's declared-loudness normalisation gain relative to
    /// [`TARGET_LOUDNESS_LUFS`], in dB — `TARGET_LOUDNESS_LUFS - model.loudness_lufs()` when the
    /// model declares a loudness, or `0.0` (no correction) when it doesn't (A1 files, or any A2
    /// file omitting the key — "there's nothing to normalise against" per FR-NAM-090's own scope,
    /// not a value worth guessing). Computed once here, at construction, since it depends only on
    /// the model itself, never on a live parameter; `process_wet` combines it with the stage's
    /// current `normalize_enabled`/`normalize_offset_db` every block.
    base_normalize_gain_db: f32,
    /// FR-NAM-090's applied gain, smoothed with the same one-pole `namir_dsp::GainRamp` pattern
    /// every other continuous gain-shaped parameter in this crate uses (D-10.3). Its *target* is
    /// recomputed every block in `process_wet` from `base_normalize_gain_db` plus the stage's own
    /// `normalize_enabled`/`normalize_offset_db`, so a live toggle or offset change ramps smoothly
    /// rather than stepping — and a freshly loaded slot itself ramps in from unity gain (`GainRamp`
    /// always starts there), composing with the handover crossfade the same way the shared bypass
    /// blend already does (this module's doc comment, "two independent fades, composed").
    normalize_gain: GainRamp,
}

impl NamSlot {
    /// **Not RT-safe. This is D-8.1 step 1, and from M4 on it runs on a worker thread.** Builds a
    /// fresh [`NamState`] (`PreparedNam::new_state` allocates every scratch buffer the model's
    /// inference needs) and, only when `model.sample_rate()` differs from `engine_sample_rate`, a
    /// [`SlotResampler`] (which itself allocates two `rubato` resamplers and their FIFOs).
    ///
    /// `pub(crate)` so [`crate::Command::load_nam`] can do this work off the audio thread. That is
    /// the whole reason a command carries a built slot rather than a bare `Arc<PreparedNam>`: an
    /// `Arc` alone would leave exactly these allocations to be made at install time, on the audio
    /// thread, which is a P1 violation — see `crate::resource`'s module doc comment.
    pub(crate) fn new(
        model: Arc<PreparedNam>,
        engine_sample_rate: SampleRate,
        max_block_size: usize,
    ) -> Self {
        let model_rate = model.sample_rate();
        // FR-NAM-090: fixed for this model's whole lifetime as a slot -- see this field's own doc
        // comment for why `None` (no declared loudness) becomes `0.0` rather than a guess.
        let base_normalize_gain_db = model
            .loudness_lufs()
            .map(|declared| TARGET_LOUDNESS_LUFS - declared)
            .unwrap_or(0.0);
        let normalize_gain =
            GainRamp::new(engine_sample_rate, NORMALIZE_GAIN_RAMP_TIME_CONSTANT_MS);
        if model_rate.hz() == engine_sample_rate.hz() {
            let state = model.new_state(max_block_size);
            Self {
                model,
                state,
                resample: None,
                base_normalize_gain_db,
                normalize_gain,
            }
        } else {
            let resample = SlotResampler::new(engine_sample_rate, model_rate, max_block_size);
            let state = model.new_state(resample.model_block);
            Self {
                model,
                state,
                resample: Some(resample),
                base_normalize_gain_db,
                normalize_gain,
            }
        }
    }

    /// Runs this slot's model (resampled around, if `resample` is `Some`) on `input`, writing
    /// exactly `input.len()` frames into `output`, then applies FR-NAM-090's normalisation gain to
    /// `output` in place. `normalize_enabled`/`normalize_offset_db` are the stage's current values
    /// (read once per call by `NamStage::process_channel0`, not stored here, since they're shared
    /// across both slots and can change independently of this slot's own model). RT-safe once
    /// constructed: every buffer this touches was sized in `NamSlot::new`/`SlotResampler::new`,
    /// and `GainRamp::set_target_db`/`process` allocate nothing (`gain_ramp.rs`'s own contract).
    fn process_wet(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        normalize_enabled: bool,
        normalize_offset_db: f32,
    ) {
        match &mut self.resample {
            None => self.model.process_block(&mut self.state, input, output),
            Some(resampler) => resampler.process(&self.model, &mut self.state, input, output),
        }
        let target_db = if normalize_enabled {
            self.base_normalize_gain_db + normalize_offset_db
        } else {
            0.0
        };
        self.normalize_gain.set_target_db(target_db);
        self.normalize_gain.process(output);
    }

    /// This slot's own added latency: its resampler's, or `0` if it runs at the engine rate.
    fn latency_samples(&self) -> u32 {
        self.resample.as_ref().map_or(0, |r| r.latency_samples)
    }
}

/// D-9.2/9.3: resamples engine-rate audio to a loaded model's declared rate, runs the model on a
/// **fixed internal block** at that rate (D-9.2's own phrase — deterministic latency and constant
/// per-call work, rather than a block size that depends on whatever the host happens to be
/// calling with), and resamples the result back. Built once, in [`NamSlot::new`]; every buffer
/// `process` touches is preallocated here so `process` itself never allocates.
///
/// Implemented with `rubato::FftFixedInOut`, which — unlike every other resampler shape this
/// crate offers — takes a **fixed** number of input frames and returns a **fixed** number of
/// output frames per call, which is exactly D-9.2's "fixed internal block" requirement without
/// this struct having to build that determinism itself. Constructing the engine→model and
/// model→engine resamplers from the *same* chosen chunk size makes the round trip exactly
/// symmetric: `into_model`'s declared input length equals `out_of_model`'s declared output
/// length, and `into_model`'s declared output length equals `out_of_model`'s declared input
/// length (both derive their internal FFT sizes from `gcd(engine_hz, model_hz)`-based
/// arithmetic, and feeding `out_of_model` a chunk size that is already an exact multiple of its
/// own minimum chunk — which `into_model`'s *output* length always is, by that same arithmetic —
/// makes its rounding a no-op). `SlotResampler::new` asserts this symmetry with `debug_assert`
/// rather than silently trusting it.
///
/// # Verified against D-9.3's quality bar since M9b, and `latency_samples` since M14
///
/// FR-NAM-060's stopband/ripple requirement was out of scope for M2
/// (`03-implementation-roadmap.md` §6: "most of §5.4 (NAM, minus... resampling-quality...)"), and
/// until M9b this implementation had never been measured against it — the note that stood here
/// said so. It is measured now, by
/// `resampler_frequency_response_meets_the_stopband_and_ripple_bar` below, and the measurement
/// changed the configuration: `rubato`'s antialiasing filter is no longer "used as configured by
/// `rubato` itself" but sized by [`MIN_RESAMPLE_FFT_FRAMES`], because as configured it failed the
/// bar badly at engine rates above the model rate (−15 dB at 20 kHz for a 48 kHz model at a
/// 192 kHz engine rate, against a 0.1 dB allowance). Stopband attenuation was never the problem
/// and measures ≥ 140 dB throughout.
///
/// [`SlotResampler`]'s `latency_samples` field is **derived** rather than traced: it sums both
/// resamplers' `output_delay()` (converting the first one's model-rate figure to engine-rate
/// samples) plus one `engine_block` for FIFO buffering granularity. Until M14 that derivation had
/// never been checked against the pipeline's actual behaviour, and this note said so. It is checked
/// now, and it is exact — `the_resampled_stages_reported_latency_is_the_delay_the_signal_actually_sees`
/// cross-correlates a chirp through the resampled stage against the *same model at the engine's own
/// rate*, so the model's own filtering group delay cancels and what is left is this field: **640
/// reported, 640 measured** for a 44.1 kHz Nano model at a 48 kHz engine.
///
/// Still not formally proven, and empirical rather than derived: the FIFOs' capacities are generous
/// relative to the bound the design keeps them under (see the `engine_in_fifo`/`engine_out_fifo`
/// field docs) — a bound the RT harness would catch a violation of rather than one anything
/// establishes ahead of time.
struct SlotResampler {
    /// Engine rate → model rate.
    into_model: FftFixedInOut<f32>,
    /// Model rate → engine rate.
    out_of_model: FftFixedInOut<f32>,
    /// `into_model`'s fixed input length = `out_of_model`'s fixed output length, in engine-rate
    /// frames (this struct's doc comment explains why these are exactly equal).
    engine_block: usize,
    /// `into_model`'s fixed output length = `out_of_model`'s fixed input length, in model-rate
    /// frames. `NamState` is sized to exactly this (`NamSlot::new`), since every model tick this
    /// struct ever runs processes exactly this many frames.
    model_block: usize,
    /// Accumulates incoming engine-rate samples (pushed whole in `process`) until at least
    /// `engine_block` are queued, at which point one full internal tick drains exactly
    /// `engine_block` of them. Capacity is chosen generously relative to the (unproven, see this
    /// struct's doc comment) bound the design keeps this under, not sized to a tight worst case.
    engine_in_fifo: VecDeque<f32>,
    /// Scratch: one `engine_block`-long chunk popped from `engine_in_fifo`, `into_model`'s input.
    engine_in_chunk: Vec<f32>,
    /// Scratch: `into_model`'s output / the model's input, `model_block` long.
    model_in_chunk: Vec<f32>,
    /// Scratch: the model's output / `out_of_model`'s input, `model_block` long.
    model_out_chunk: Vec<f32>,
    /// Scratch: `out_of_model`'s output, `engine_block` long, pushed whole into `engine_out_fifo`.
    engine_out_chunk: Vec<f32>,
    /// Resampled-back output, produced `engine_block` frames at a time, drained by `process`
    /// whatever number of frames its caller asked for (which need not be a multiple of
    /// `engine_block` — that mismatch is exactly why this FIFO exists). See `engine_in_fifo`'s
    /// doc comment for the same capacity caveat.
    engine_out_fifo: VecDeque<f32>,
    /// This slot's added latency in engine-rate samples — see this struct's doc comment for the
    /// precision caveat.
    latency_samples: u32,
}

impl SlotResampler {
    /// **Not RT-safe** — constructs two `rubato::FftFixedInOut` resamplers (each allocates an FFT
    /// plan and working buffers) and reserves both FIFOs' capacity up front. Called only from
    /// [`NamSlot::new`], itself only ever called from [`NamStage::load_model`]'s explicitly-non-RT
    /// path.
    fn new(engine_rate: SampleRate, model_rate: SampleRate, max_block_size: usize) -> Self {
        let into_model = FftFixedInOut::<f32>::new(
            engine_rate.hz() as usize,
            model_rate.hz() as usize,
            resample_chunk_frames(engine_rate.hz() as usize, model_rate.hz() as usize),
            1,
        )
        .expect("SampleRate's own invariant guarantees both rates are nonzero");
        let engine_block = into_model.input_frames_next();
        let model_block = into_model.output_frames_next();

        let out_of_model = FftFixedInOut::<f32>::new(
            model_rate.hz() as usize,
            engine_rate.hz() as usize,
            model_block,
            1,
        )
        .expect("SampleRate's own invariant guarantees both rates are nonzero");
        // This struct's doc comment derives why this round trip is exactly symmetric; assert it
        // rather than silently relying on it, since a violation would desync the fixed-block
        // pipeline this whole struct exists to guarantee.
        debug_assert_eq!(out_of_model.input_frames_next(), model_block);
        debug_assert_eq!(out_of_model.output_frames_next(), engine_block);

        let in_delay_model_domain = into_model.output_delay() as f64;
        let in_delay_engine_domain =
            (in_delay_model_domain * engine_rate.hz_f64() / model_rate.hz_f64()).round() as u32;
        let out_delay_engine_domain = out_of_model.output_delay() as u32;
        let latency_samples = in_delay_engine_domain
            .saturating_add(out_delay_engine_domain)
            .saturating_add(engine_block as u32);

        // Generous, not tight (this struct's doc comment's "known limitation" section): large
        // enough that ordinary use is nowhere near it, so a capacity violation the RT harness
        // catches means a real design error, not a plausible legitimate block-size pattern.
        let fifo_capacity = 16 * (engine_block + max_block_size) + 4096;

        // **M9b: the output FIFO is primed with one engine block of silence, and that priming is
        // load-bearing rather than a convenience.** Without it `process`'s produce loop primes the
        // pipeline only to *the current call's* output length, and its drain tail substitutes a
        // zero for anything missing. Each starved frame therefore splices in one sample of silence
        // that is never taken back, so the stage's delay becomes a function of the block-size
        // *history*: measured at 48 kHz engine / 44.1 kHz model, uniform blocks of 512 down to 64
        // ran at the nominal delay while 32/16/8/4/2/1-frame blocks accumulated 32/48/56/60/62/63
        // extra samples, and the shift arrived *mid-stream* when a host changed block size — 63
        // samples of silence spliced into a live signal, which is FR-CLAP-070's "without
        // artefacts" clause failing, not merely its parity clause.
        //
        // Priming establishes `engine_in_fifo.len() + engine_out_fifo.len() == engine_block` as an
        // invariant, under which the loop cannot starve: whenever `out < n` the identity gives
        // `in == engine_block + n - out > engine_block`, so a full internal tick is always
        // available to run. It also makes the *actual* delay equal the `latency_samples` this
        // stage already reports; before priming that figure was an upper bound the truth met only
        // by accident (576 under 512-frame blocks, 639 under 1-frame blocks, reported as 640).
        //
        // Found by FR-CLAP-070's resource-loaded parity test at M9b, which fails without this and
        // passes bit-exactly with it. D-6.2's consequence asserted the design already handled
        // arbitrary block sizes; it did not, and §15 item 13 fixed the branch policy for that
        // discovery — fix the engine — before the test was written.
        let mut engine_out_fifo = VecDeque::with_capacity(fifo_capacity);
        engine_out_fifo.resize(engine_block, 0.0);

        Self {
            into_model,
            out_of_model,
            engine_block,
            model_block,
            engine_in_fifo: VecDeque::with_capacity(fifo_capacity),
            engine_in_chunk: vec![0.0; engine_block],
            model_in_chunk: vec![0.0; model_block],
            model_out_chunk: vec![0.0; model_block],
            engine_out_chunk: vec![0.0; engine_block],
            engine_out_fifo,
            latency_samples,
        }
    }

    /// RT-safe: every buffer this touches (the two FIFOs included, so long as their capacity
    /// headroom holds — see this struct's doc comment) was sized in `new`.
    fn process(
        &mut self,
        model: &PreparedNam,
        state: &mut NamState,
        input: &[f32],
        output: &mut [f32],
    ) {
        self.engine_in_fifo.extend(input.iter().copied());

        // Run only as many fixed internal ticks as needed to satisfy *this* call's output, not
        // every full chunk `engine_in_fifo` happens to hold — this is what keeps both FIFOs'
        // occupancy tracking this call's own input/output size rather than drifting unboundedly
        // across calls (see this struct's doc comment for why that bound isn't formally proven).
        while self.engine_out_fifo.len() < output.len()
            && self.engine_in_fifo.len() >= self.engine_block
        {
            for sample in self.engine_in_chunk.iter_mut() {
                *sample = self
                    .engine_in_fifo
                    .pop_front()
                    .expect("loop condition just checked at least engine_block are queued");
            }

            let wave_in: [&[f32]; 1] = [&self.engine_in_chunk[..]];
            let mut wave_out: [&mut [f32]; 1] = [&mut self.model_in_chunk[..]];
            self.into_model
                .process_into_buffer(&wave_in, &mut wave_out, None)
                .expect("buffers are exactly this resampler's own declared chunk sizes");

            model.process_block(state, &self.model_in_chunk, &mut self.model_out_chunk);

            let wave_in: [&[f32]; 1] = [&self.model_out_chunk[..]];
            let mut wave_out: [&mut [f32]; 1] = [&mut self.engine_out_chunk[..]];
            self.out_of_model
                .process_into_buffer(&wave_in, &mut wave_out, None)
                .expect("buffers are exactly this resampler's own declared chunk sizes");

            self.engine_out_fifo
                .extend(self.engine_out_chunk.iter().copied());
        }

        // Any frames not yet available (start-of-stream buffering delay, accounted for in
        // `latency_samples`) come out as silence rather than reading uninitialized/stale data.
        for sample in output.iter_mut() {
            *sample = self.engine_out_fifo.pop_front().unwrap_or(0.0);
        }
    }

    /// FR-NAM-060's "in isolation": [`process`](Self::process)'s internal tick with the model
    /// taken out of the middle, so what comes back is the two resamplers and nothing else. Runs
    /// whole `engine_block`-sized chunks straight out of `input` and returns everything the pair
    /// produced, rather than going through the FIFOs — those exist to reconcile the fixed internal
    /// block with a caller's arbitrary block size, which is not a property of the resampling.
    ///
    /// Test-only, and not RT-safe (it allocates its own output). Lives here beside `process`
    /// rather than in the test module so the two cannot drift: if the tick ever grows a step, this
    /// is the code that has to grow it too.
    #[cfg(test)]
    fn resample_only(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len() * 2);
        for chunk in input.chunks_exact(self.engine_block) {
            let wave_in: [&[f32]; 1] = [chunk];
            let mut wave_out: [&mut [f32]; 1] = [&mut self.model_in_chunk[..]];
            self.into_model
                .process_into_buffer(&wave_in, &mut wave_out, None)
                .expect("buffers are exactly this resampler's own declared chunk sizes");

            let wave_in: [&[f32]; 1] = [&self.model_in_chunk[..]];
            let mut wave_out: [&mut [f32]; 1] = [&mut self.engine_out_chunk[..]];
            self.out_of_model
                .process_into_buffer(&wave_in, &mut wave_out, None)
                .expect("buffers are exactly this resampler's own declared chunk sizes");

            output.extend_from_slice(&self.engine_out_chunk);
        }
        output
    }
}

/// The handover crossfade's progress (FR-NAM-070): a fixed-duration equal-power fade between
/// `slots[active]` (fading out) and `slots[1 - active]` (fading in). `Copy` so `NamStage::process`
/// can read it out of `self.crossfade`, mutate a local copy across a whole block, and write it
/// back (or clear it) once, without holding a borrow of `self.crossfade` across the same
/// statements that also need to borrow `self.slots`/`self.crossfade_outgoing`/etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Crossfade {
    /// Samples left until this handover completes.
    remaining: u32,
    /// The fade's total duration in samples, fixed at construction
    /// (`NamStage::crossfade_total_samples`, from [`HANDOVER_CROSSFADE_MS`]).
    total: u32,
}

/// RT-safe NAM stage: up to two [`NamSlot`]s, equal-power-crossfaded between per FR-NAM-070's
/// handover protocol (D-8.1's shape), run mono-core on channel 0 and duplicated
/// (FR-CHAIN-050), behind the shared click-free per-stage bypass crossfade (FR-CHAIN-020).
pub struct NamStage {
    /// The engine sample rate every slot's resampler (if any) is built against.
    sample_rate: SampleRate,
    /// The block-size ceiling every slot's non-resampled inference state is sized to
    /// (`ctx.max_block_size()`, recorded here so `load_model` — which runs long after `prepare`
    /// returns — doesn't need it passed in separately each time).
    max_block_size: usize,
    /// The whole `PrepareContext` this stage was built against, kept so an incoming offer's own
    /// context can be checked against it rather than trusted. A resource prepared for a different
    /// sample rate or block size would otherwise install silently-wrong-sized buffers — and in the
    /// Ir stage's case, `PreparedIr::process_block` asserts on an over-long block, so a mismatch
    /// there is a panic on the audio thread rather than merely a wrong sound.
    prepared_for: PrepareContext,
    /// The two live resource slots D-8.1's handover shape asks for. At most one is fading out and
    /// at most one is fading in at any time (an install always goes into `1 - active`).
    ///
    /// Boxed since M4: the slot travels to and from a worker through a preallocated ring, and
    /// boxing is what makes that ring's element a pointer rather than the whole slot — see
    /// `crate::resource`'s module doc comment. Deref coercion means every use site below is
    /// unaffected.
    slots: [Option<Box<NamSlot>>; 2],
    /// Index into `slots` of the slot that is live outside of an in-flight handover — and, during
    /// one, the slot that is fading *out* (see this module's doc comment for why `active` itself
    /// only updates once the handover completes, never mid-fade).
    active: usize,
    /// `Some` while a handover between `slots[active]` and `slots[1 - active]` is in progress.
    crossfade: Option<Crossfade>,
    /// [`HANDOVER_CROSSFADE_MS`] converted to samples once, in `prepare`, for every handover this
    /// stage ever starts (`load_model` reads this rather than recomputing it, which would need
    /// `sample_rate.hz_f64()` — fine on paper, but this keeps `load_model` itself trivial).
    crossfade_total_samples: u32,
    /// FR-CHAIN-020's per-stage enable/disable for this stage, independent of whether any model is
    /// loaded (`mix_target`'s own doc comment covers how the two combine).
    enabled: bool,
    /// FR-NAM-090: whether declared-loudness normalisation gain is applied at all. Independent of
    /// `enabled` above — see [`NORMALIZE_ENABLED`]'s own doc comment.
    normalize_enabled: bool,
    /// FR-NAM-090: user-controlled trim added to the computed normalisation gain, in dB. See
    /// [`NORMALIZE_OFFSET_DB`]'s own doc comment.
    normalize_offset_db: f32,
    /// Current dry/wet blend for the *shared* bypass crossfade: `0.0` = fully dry/bypassed,
    /// `1.0` = fully wet/engaged. See this module's doc comment for how this composes with the
    /// separate handover crossfade above.
    mix: f32,
    /// Where `mix` is heading: `1.0` when `enabled` and some slot is contributing wet signal to
    /// the output right now, `0.0` otherwise
    /// (FR-CHAIN-040: nothing loaded behaves as bypassed). Recomputed by `apply`, `load_model`,
    /// and by `process` itself right after a handover completes and `active` changes — every
    /// place this stage's doc comment lists as changing one of the two inputs to this formula.
    mix_target: f32,
    /// One-pole coefficient for the `mix` crossfade, computed once in `prepare` from
    /// [`BYPASS_CROSSFADE_TIME_CONSTANT_MS`] and the sample rate.
    mix_coeff: f32,
    /// Per-channel pre-stage signal, captured at the top of every `process` call — both the
    /// shared bypass blend's dry reference *and*, for channel 0, the wet path's own input (reused
    /// rather than copied a second time).
    dry: Vec<Vec<f32>>,
    /// Shuttle buffer for FR-CHAIN-050's channel-0-then-duplicate pattern.
    scratch: Vec<f32>,
    /// Handover-fade scratch: `slots[active]`'s (fading-out) wet output for the current block,
    /// or a dry passthrough copy of the input when that slot is `None` (this module's doc
    /// comment: "a slot that is `None` inside a crossfade contributes its input directly").
    crossfade_outgoing: Vec<f32>,
    /// Handover-fade scratch: `slots[1 - active]`'s (fading-in) wet output for the current block,
    /// or a dry passthrough copy, symmetric to `crossfade_outgoing`.
    crossfade_incoming: Vec<f32>,
    /// D-8.1 step 4's holding pen: a slot this stage has finished with, waiting to be moved into
    /// the return ring by [`Stage::collect_retired`].
    ///
    /// **Capacity one, deliberately.** Two things can retire a slot — a completing crossfade, and
    /// an install that displaces a slot still fading in — and the engine's drain gate guarantees
    /// an offer is only delivered when this is empty, while a completing crossfade *defers its own
    /// finalization* rather than overwrite an occupied pen. "At most one thing is parked here at
    /// any instant" is a far easier invariant to state and test than any capacity above one, and
    /// the engine collects twice per block so the pen is empty again almost immediately.
    retired: Option<Resource>,
}

impl NamStage {
    /// Installs `model` into the currently-inactive slot and begins a
    /// [`HANDOVER_CROSSFADE_MS`]-long equal-power fade into it (FR-NAM-070; D-8.1 step 3 — the
    /// real cross-thread offer/retire wiring around this call is M4's job, per
    /// `03-implementation-roadmap.md` §3/§6; in M2 this method exists so that mechanism can be
    /// proven before a thread has to drive it, and this crate's own tests are its only caller).
    ///
    /// **Not RT-safe.** Builds a new [`NamSlot`] (`NamState`, and — only if `model`'s declared
    /// rate differs from the engine's — a resampler pair and its FIFOs; all of which allocate).
    /// Must never be called from `Stage::process`; mirrors `StagePrep::prepare`'s own "may
    /// allocate, may fail" contract, though this method cannot itself fail (constructing a slot
    /// for an already-validated `PreparedNam` has no failure mode this crate's types allow).
    ///
    /// If a handover is already in progress when this is called, the slot currently fading *in*
    /// (`slots[1 - active]`) is replaced outright and the fade restarts at full duration —
    /// `active` (and therefore which slot is fading *out*) is unaffected, since `active` only
    /// ever changes when a handover completes.
    pub fn load_model(&mut self, model: Arc<PreparedNam>) {
        let slot = NamSlot::new(model, self.sample_rate, self.max_block_size);
        let ctx = self.prepared_for;
        self.install(Box::new(slot), ctx);
    }

    /// **RT-safe.** Installs an already-built slot into the inactive position and starts the
    /// handover fade. This is D-8.1 step 3's entry point, and everything it does is a move or a
    /// scalar assignment — the allocations were all made by whoever built `slot` (from M4 on, a
    /// worker thread, via [`crate::Command::load_nam`]).
    ///
    /// **The displaced slot is parked, never dropped.** An install that lands while a handover is
    /// already in flight replaces the slot currently fading *in*. Before M4 that replacement was
    /// `self.slots[inactive] = Some(..)`, which *drops* the displaced slot; that was tolerable
    /// only because `load_model` was documented non-RT and never ran on the audio thread. Once
    /// offers arrive through the command ring, installs happen on the audio thread, and the drop
    /// would be a second P1 violation alongside the one at completion. Both are closed the same
    /// way: move the slot into `retired` and let the return ring carry it away.
    ///
    /// The pen being occupied here would mean the engine's drain gate let an offer through with a
    /// retirement still outstanding, which it does not — hence the `debug_assert`. Even if it
    /// somehow did, the fallback still never drops: the incoming slot is refused and handed back.
    pub(crate) fn install(&mut self, slot: Box<NamSlot>, ctx: PrepareContext) -> Option<Resource> {
        if self.retired.is_some() {
            debug_assert!(
                false,
                "install with a retirement still parked: the engine's drain gate should \
                 have held this offer back"
            );
            return Some(Resource::nam(slot, ctx));
        }
        let inactive = 1 - self.active;
        if let Some(displaced) = self.slots[inactive].take() {
            // A move, not a drop. See this method's doc comment.
            self.retired = Some(Resource::nam(displaced, self.prepared_for));
        }
        self.slots[inactive] = Some(slot);
        self.begin_crossfade();
        None
    }

    /// **RT-safe.** FR-STATE-070's "the state shall load with that stage empty": the mirror
    /// image of [`Self::install`]. Displaces the inactive slot into `self.retired` exactly as
    /// `install` does (never dropped — see that method's doc comment), but leaves the inactive
    /// position `None` instead of putting a new slot there, and starts the same
    /// [`HANDOVER_CROSSFADE_MS`]-long fade. `process_channel0` already treats a `None` slot as a
    /// dry passthrough on either side of a fade (this module's own doc comment), so fading
    /// *into* `None` needs no new DSP — it is an entry point onto the existing state machine,
    /// not a new one. Once the fade completes, the ordinary finalization block moves the
    /// (formerly active) outgoing slot into `self.retired` and flips `active` onto the now-empty
    /// slot, exactly as it does after any other handover.
    pub(crate) fn unload(&mut self) {
        if self.retired.is_some() {
            debug_assert!(
                false,
                "unload with a retirement still parked: the engine's drain gate should \
                 have held this command back"
            );
            return;
        }
        let inactive = 1 - self.active;
        if let Some(displaced) = self.slots[inactive].take() {
            // A move, not a drop. See `install`'s doc comment.
            self.retired = Some(Resource::nam(displaced, self.prepared_for));
        }
        self.begin_crossfade();
    }

    /// **RT-safe.** Starts a [`HANDOVER_CROSSFADE_MS`]-long fade (D-8.1 step 3) and puts the
    /// shared bypass blend where that fade can actually be heard. Shared by [`Self::install`] and
    /// [`Self::unload`], which differ only in what they leave in the inactive slot first.
    ///
    /// # Issue #141: why `mix` is *snapped* here rather than left to its one-pole
    ///
    /// See this module's own doc comment for the measurement. In short: on a first load the fade's
    /// outgoing side is `None`, so a `mix_target` derived from `slots[active]` alone held the
    /// bypass blend at 0.0 for the fade's whole duration and the equal-power blend was multiplied
    /// out of existence — the stage emitted bit-exactly its dry input until the fade *completed*,
    /// and then engaged over 15 ms starting at whatever block boundary that landed on.
    ///
    /// [`Self::recompute_mix_target`] fixes the first half by counting the slot being faded *into*.
    /// That alone would leave the second: a one-pole ramp composed on top of the equal-power curve
    /// is not the equal-power curve FR-NAM-070 asks for, and would still spread the onset over
    /// 15 ms. So `mix` is moved to its target in one step.
    ///
    /// **The step is click-free by construction, not by tolerance.** It is taken only when
    /// [`Self::wet_path_is_transparent`] holds — this stage's wet path is currently a bit-exact
    /// copy of its dry input — and in that state `mix` is unobservable: the stage's output is the
    /// dry signal for *every* value of `mix`, on the sample before the step and on the sample
    /// after it. The fade then picks up from exactly there, because its first sample has
    /// `theta == 0`, `cos(0) == 1`, `sin(0) == 0` and a `None` outgoing slot contributes a
    /// `copy_from_slice` of the dry input. When the wet path is *not* transparent (a replacement
    /// model faded into an already-engaged stage) nothing is snapped and `mix` is already 1.0
    /// anyway, which is the case that always worked.
    ///
    /// Costs two `Option` inspections and three scalar assignments; allocates nothing, and every
    /// branch is straight-line.
    fn begin_crossfade(&mut self) {
        // Sampled *before* `self.crossfade` is overwritten: an in-flight fade is itself one of the
        // things that makes the wet path non-transparent.
        let mix_is_unobservable = self.wet_path_is_transparent();
        self.crossfade = Some(Crossfade {
            remaining: self.crossfade_total_samples,
            total: self.crossfade_total_samples,
        });
        self.recompute_mix_target();
        if mix_is_unobservable {
            self.mix = self.mix_target;
        }
    }

    /// Whether this stage's wet path currently reproduces its dry input **bit-exactly**, which is
    /// what makes `mix` unobservable and [`Self::begin_crossfade`]'s snap inaudible.
    ///
    /// True exactly when nothing is active and no fade is in flight: `process_channel0` returns
    /// without touching `io` in that state (FR-CHAIN-040's passthrough), so the bypass blend below
    /// it is blending the dry signal against itself. Unlike `ir.rs`'s counterpart there is nothing
    /// else on this stage's wet path to be transparent about — no filters, no level ramp.
    fn wet_path_is_transparent(&self) -> bool {
        self.slots[self.active].is_none() && self.crossfade.is_none()
    }

    /// `mix_target` is a function of `enabled` and of whether *any* slot is contributing wet
    /// signal to the output right now — see the field's own doc comment for the FR-CHAIN-040
    /// rationale.
    ///
    /// **Issue #141 widened the second input.** It used to be `slots[active]`'s presence alone,
    /// deliberately the pre-handover active slot; but a fade *into* the inactive slot is audible
    /// from its own first sample, so a target that ignores it holds the bypass blend closed over
    /// exactly the interval FR-NAM-070 specifies a fade for. `slots[active]` still governs
    /// everywhere outside a handover, and still governs `latency_samples`/`telemetry`, which are
    /// statements about the settled stage rather than about what is audible this block.
    fn recompute_mix_target(&mut self) {
        let engaged = self.slots[self.active].is_some()
            || (self.crossfade.is_some() && self.slots[1 - self.active].is_some());
        self.mix_target = if self.enabled && engaged { 1.0 } else { 0.0 };
    }

    /// The mono-core wet path (FR-CHAIN-050): writes this block's processed result into
    /// `io.channel(0)`, reading input from `self.dry[0]` (already captured by `process` before
    /// this is called, so this needn't take a second copy). Handles all three shapes the
    /// handover protocol requires: no handover in progress (single slot, or pure passthrough if
    /// `slots[active]` is `None`); a handover in progress (equal-power blend of both slots' wet
    /// output, either side substituting a dry passthrough for a `None` slot); and a handover that
    /// completes partway through this very block (the per-sample `theta` below saturates at
    /// `total`, so the tail of the block after completion is already pure incoming-slot output,
    /// consistent with the finalization performed once after the loop).
    fn process_channel0(&mut self, io: &mut StageIo<'_>, n: usize) {
        // FR-NAM-090: read once per block, before any of `self.slots` is borrowed below -- shared
        // across both slots (each applies its own `base_normalize_gain_db` against these same two
        // values), and can change independently of either slot's own model.
        let normalize_enabled = self.normalize_enabled;
        let normalize_offset_db = self.normalize_offset_db;

        let Some(mut crossfade) = self.crossfade else {
            // FR-CHAIN-040: `None` is a pure passthrough -- `io.channel(0)` already holds the
            // input, so there is nothing to do in that case.
            if let Some(slot) = &mut self.slots[self.active] {
                slot.process_wet(
                    &self.dry[0][..n],
                    io.channel(0),
                    normalize_enabled,
                    normalize_offset_db,
                );
            }
            return;
        };

        let outgoing_idx = self.active;
        let incoming_idx = 1 - self.active;

        if crossfade.remaining == 0 {
            // Deferred-finalization state (see this method's finalization block below): the fade
            // is mathematically complete but the retire pen was still occupied when it ended, so
            // `active` has not flipped yet.
            //
            // **Try to finalize first, every block (issue #56).** This used to fall straight
            // through to the incoming-only render, which made the state a dead end: nothing else
            // in `process_channel0` re-tests `self.retired`, so once entered, `active` never
            // flipped, `crossfade` never cleared and the outgoing slot never reached the pen — for
            // the rest of the session. The consequences were permanent and all silent:
            // `latency_samples()` kept reporting the outgoing slot (FR-CLAP-040 wrong),
            // `telemetry.nam.handover_active` stayed pinned at 1.0, `recompute_mix_target` never
            // re-ran (so a *first* load left the stage bypassed forever), and a later install
            // displaced the audible slot and re-faded from the stale outgoing one. The block
            // comment below promised exactly this recovery; it simply did not exist.
            //
            // The retry is one `Option::is_none()` check per block. `collect_retired` empties the
            // pen as soon as the worker drains, so the deferral is normally over within a block or
            // two — but nothing bounds it, which is precisely why it must be retried rather than
            // entered once.
            self.try_finalize_handover();

            // Run only the incoming slot rather than blending in an outgoing one scaled by
            // `cos(FRAC_PI_2)` — which is -4.4e-8 in f32, not exactly zero, and would otherwise
            // leave a faint copy of the old model in the output for as long as the deferral lasts.
            // Skipping it also avoids paying the 2x inference cost in a state that can persist
            // across many blocks. `incoming_idx` names the same slot either way: a successful
            // finalization sets `self.active` *to* it.
            if let Some(slot) = &mut self.slots[incoming_idx] {
                slot.process_wet(
                    &self.dry[0][..n],
                    io.channel(0),
                    normalize_enabled,
                    normalize_offset_db,
                );
            }
            return;
        }

        match &mut self.slots[outgoing_idx] {
            Some(slot) => slot.process_wet(
                &self.dry[0][..n],
                &mut self.crossfade_outgoing[..n],
                normalize_enabled,
                normalize_offset_db,
            ),
            None => self.crossfade_outgoing[..n].copy_from_slice(&self.dry[0][..n]),
        }
        match &mut self.slots[incoming_idx] {
            Some(slot) => slot.process_wet(
                &self.dry[0][..n],
                &mut self.crossfade_incoming[..n],
                normalize_enabled,
                normalize_offset_db,
            ),
            None => self.crossfade_incoming[..n].copy_from_slice(&self.dry[0][..n]),
        }

        let total = crossfade.total.max(1);
        let out = io.channel(0);
        for ((o, &outgoing), &incoming) in out
            .iter_mut()
            .zip(self.crossfade_outgoing[..n].iter())
            .zip(self.crossfade_incoming[..n].iter())
        {
            let progress = (total - crossfade.remaining).min(total);
            let theta = (progress as f32 / total as f32) * FRAC_PI_2;
            *o = outgoing * theta.cos() + incoming * theta.sin();
            if crossfade.remaining > 0 {
                crossfade.remaining -= 1;
            }
        }

        if crossfade.remaining == 0 {
            if !self.try_finalize_handover() {
                // The pen is still occupied because the return ring was full when
                // `collect_retired` last ran — i.e. the worker is not draining (D-8.1: "If the
                // worker dies, the ring fills and memory is retained but audio continues.
                // Degradation, not failure (P8)").
                //
                // Only the *bookkeeping* is deferred; the audio is already correct. `theta` has
                // saturated at FRAC_PI_2, so the outgoing slot is multiplied by cos(pi/2) and
                // contributes nothing audible, and `process_channel0`'s own fast path above skips
                // running it at all. The stage stays in this state until a later block's
                // `collect_retired` empties the pen — and that same fast path retries the
                // finalization on every block until it does (issue #56).
                //
                // The wrong "fix" here is to drop the outgoing slot to make progress. That is
                // exactly the bug this milestone removes; deferring costs bounded memory, and
                // dropping costs a dropout.
                self.crossfade = Some(crossfade);
            }
        } else {
            self.crossfade = Some(crossfade);
        }
    }

    /// D-8.1's step-4 bookkeeping for a fade that has reached zero: move the outgoing slot into
    /// the retire pen, flip `active` onto the incoming one, clear the crossfade and recompute the
    /// bypass blend's target. Returns `false` — changing nothing at all — when the pen is still
    /// occupied, which is the deferred-finalization state both callers document.
    ///
    /// **RT-safe:** one `Option::is_none()`, one `Option::take()` (a move, never a drop — see the
    /// note in `install`), and three scalar assignments.
    ///
    /// Called from two places, and that is the fix for issue #56: once at the end of the fade in
    /// `process_channel0`, and again on every subsequent block from that method's `remaining == 0`
    /// fast path, so a deferral entered because the worker was not draining is left as soon as it
    /// is.
    fn try_finalize_handover(&mut self) -> bool {
        if self.retired.is_some() {
            return false;
        }
        let outgoing_idx = self.active;
        // **The M2 P1 violation, closed.** This used to be `self.slots[outgoing_idx] = None`, i.e.
        // a *drop* — freeing the outgoing `NamState`'s scratch and possibly the last
        // `Arc<PreparedNam>` reference, on the audio thread, at the exact instant a handover
        // completed. `take()` *moves*: nothing is dropped here, and the return ring carries the
        // slot to a worker that can afford to free it (D-8.1 step 4). Do not "simplify" this back
        // to an assignment.
        self.retired = self.slots[outgoing_idx]
            .take()
            .map(|slot| Resource::nam(slot, self.prepared_for));
        self.active = 1 - outgoing_idx;
        self.crossfade = None;
        self.recompute_mix_target();
        true
    }
}

impl Stage for NamStage {
    fn process(&mut self, io: &mut StageIo<'_>) {
        let n = io.frames();
        let channel_count = io.channel_count();

        // Capture dry input for every channel — for the shared bypass blend below, and (channel
        // 0 only) as the wet path's own input, per `process_channel0`'s doc comment.
        for ch in 0..channel_count {
            self.dry[ch][..n].copy_from_slice(io.channel(ch));
        }

        self.process_channel0(io, n);

        // FR-CHAIN-050: duplicate channel 0's wet result onto every other channel.
        if channel_count > 1 {
            self.scratch[..n].copy_from_slice(io.channel(0));
            let wet = &self.scratch[..n];
            for ch in 1..channel_count {
                io.channel(ch).copy_from_slice(wet);
            }
        }

        // Shared per-stage bypass crossfade (FR-CHAIN-020) — identical pattern to `gate.rs`: same
        // `start_mix` and per-sample recurrence for every channel, recomputed per channel rather
        // than carried over between channels, so every channel's fade stays in phase; only the
        // last channel's trajectory is committed back to `self.mix`.
        let start_mix = self.mix;
        let last = channel_count - 1;
        for ch in 0..channel_count {
            let mut m = start_mix;
            let wet = io.channel(ch);
            let dry = &self.dry[ch][..n];
            for i in 0..n {
                m += self.mix_coeff * (self.mix_target - m);
                wet[i] = dry[i] * (1.0 - m) + wet[i] * m;
            }
            if ch == last {
                self.mix = m;
            }
        }
    }

    fn reset(&mut self) {
        // RT-safe-resettable state only (mirrors `gate.rs`'s identical scoping decision): each
        // resampler's own `reset()` clears its filter overlap history without allocating, and
        // clearing a `VecDeque` (`.clear()`) drops its len to zero without releasing capacity.
        // Known gap, not silently worked around: `NamState`'s own causal-conv history has no
        // public reset (`namir-nam`'s own scope — see `namir_nam::NamState`'s doc comment), and
        // the only way to clear it is a fresh `new_state`, which allocates and is therefore not
        // callable from here. A `reset()` on a loaded NAM stage currently leaves that history
        // intact; closing this gap needs either a `namir-nam` API addition or accepting the
        // history as part of what a reset does not clear, and is left to whoever wires transport
        // reset semantics for real (out of M2's scope here).
        for slot in self.slots.iter_mut().flatten() {
            if let Some(resampler) = &mut slot.resample {
                resampler.into_model.reset();
                resampler.out_of_model.reset();
                resampler.engine_in_fifo.clear();
                // Re-prime rather than merely clear: `SlotResampler::new`'s comment explains why
                // one engine block of silence has to be present for the loop not to starve.
                // Clearing alone would leave a reset stage in exactly the pre-M9b state whose
                // block-size-dependent delay FR-CLAP-070 caught, so the two sites must agree.
                // RT-safe: `resize` only ever shrinks back to a length this FIFO's capacity,
                // reserved in `new`, already covers.
                let engine_block = resampler.engine_block;
                resampler.engine_out_fifo.clear();
                resampler.engine_out_fifo.resize(engine_block, 0.0);
            }
        }
    }

    fn latency_samples(&self) -> u32 {
        self.slots[self.active]
            .as_ref()
            .map_or(0, |s| s.latency_samples())
    }

    fn tail_samples(&self) -> u32 {
        // WaveNet is causal (`PreparedNam::latency_samples`'s own doc comment); this stage adds
        // no reverb/convolution tail of its own.
        0
    }

    fn apply(&mut self, change: ParamChange) {
        if change.id == ENABLED_ID {
            // Stepped param value is the index as f32 (`ParamChange`'s own doc comment); index 1
            // is "On" per `ENABLED`'s descriptor.
            self.enabled = change.value >= 0.5;
            self.recompute_mix_target();
        } else if change.id == NORMALIZE_ENABLED_ID {
            self.normalize_enabled = change.value >= 0.5;
        } else if change.id == NORMALIZE_OFFSET_DB_ID {
            self.normalize_offset_db = change.value;
        }
    }

    fn telemetry(&self, out: &mut TelemetrySink<'_>) {
        out.push(TelemetryEntry {
            id: TELEMETRY_LOADED,
            value: if self.slots[self.active].is_some() {
                1.0
            } else {
                0.0
            },
        });
        out.push(TelemetryEntry {
            id: TELEMETRY_HANDOVER_ACTIVE,
            value: if self.crossfade.is_some() { 1.0 } else { 0.0 },
        });
    }

    /// D-8.1 step 2. Takes the offer only if it is a Nam resource prepared for this stage's own
    /// context; anything else is left untouched for another stage (or for the engine to return).
    ///
    /// A context mismatch is *retired rather than installed*: the resource's buffers were sized
    /// for a different sample rate or block size, so installing it would be silently wrong. It
    /// is taken (so the engine's "nobody wanted it" path doesn't fire) and parked for the return
    /// ring, which is P8 degradation — no panic, no wrong-sized buffer, and the resource still
    /// reaches a thread that can free it.
    fn accept_resource(&mut self, offer: &mut Option<Resource>) {
        let Some((slot, ctx)) = Resource::take_nam(offer) else {
            return;
        };
        if ctx != self.prepared_for {
            debug_assert!(
                self.retired.is_none(),
                "the engine's drain gate should have held this offer back"
            );
            self.retired = Some(Resource::nam(slot, ctx));
            return;
        }
        let ctx = self.prepared_for;
        if let Some(refused) = self.install(slot, ctx) {
            // `install` only refuses when the pen is already occupied, which the drain gate
            // prevents. Hand it back rather than drop it if that invariant ever breaks.
            *offer = Some(refused);
        }
    }

    /// D-8.1 step 4. Moves a parked slot into the return ring, or keeps holding it if the ring is
    /// full — never drops it. See [`RetireSink::push`].
    fn collect_retired(&mut self, out: &mut RetireSink<'_>) {
        if let Some(resource) = self.retired.take()
            && let Err(back) = out.push(resource)
        {
            self.retired = Some(back);
        }
    }

    /// M5: FR-STATE-070's "the state shall load with that stage empty", entry point. Ignores a
    /// `kind` that isn't ours, exactly as `apply` ignores a `ParamId` it doesn't own.
    fn unload_resource(&mut self, kind: ResourceKind) {
        if kind == ResourceKind::Nam {
            self.unload();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;
    use namir_core::ChannelConfig;
    use namir_fixtures::resample_response::{ResampleResponse, measure};
    use namir_nam::{
        ActivationEntry, ActivationSpec, LayerArrayConfig, NamFile, NamMetadata, WaveNetConfig,
    };

    fn ctx(sample_rate_hz: u32, channel_config: ChannelConfig) -> PrepareContext {
        PrepareContext::new(SampleRate::new(sample_rate_hz).unwrap(), 64, channel_config).unwrap()
    }

    fn stage(sample_rate_hz: u32, channel_config: ChannelConfig) -> NamStage {
        NamPrep
            .prepare(&ctx(sample_rate_hz, channel_config))
            .unwrap()
    }

    /// A tiny, deterministic WaveNet layer array — same shape `namir-nam`'s own private test
    /// fixture uses (`wavenet.rs`'s `minimal_layer_array`, not reusable from here since it's not
    /// `pub`), rebuilt against the fully public `NamFile`/`LayerArrayConfig` surface instead.
    /// M10: `LayerArrayConfig` widened to make room for A2's fields (see `namir_nam::file`'s
    /// module doc comment) — every field beyond the A1 five is now `Option`, so this helper states
    /// all of them explicitly (`None` where A1 has no opinion) rather than relying on a `Default`
    /// impl, mirroring `wavenet.rs`'s own private `minimal_layer_array` test helper.
    fn minimal_layer_array() -> LayerArrayConfig {
        LayerArrayConfig {
            input_size: 1,
            condition_size: 1,
            channels: 2,
            dilations: vec![1],
            activation: ActivationSpec::One(ActivationEntry::Name("Tanh".to_string())),
            kernel_size: Some(2),
            kernel_sizes: None,
            bottleneck: None,
            head_size: Some(1),
            head_bias: Some(false),
            head: None,
            gated: Some(false),
            gating_mode: None,
            secondary_activation: None,
            groups_input: None,
            groups_input_mixin: None,
            layer1x1: None,
            head1x1: None,
            slimmable: None,
            conv_pre_film: None,
            conv_post_film: None,
            input_mixin_pre_film: None,
            input_mixin_post_film: None,
            activation_pre_film: None,
            activation_post_film: None,
            layer1x1_post_film: None,
            head1x1_post_film: None,
        }
    }

    /// How many flat weights `PreparedNam::from_file` consumes for one `minimal_layer_array`,
    /// mirroring `wavenet.rs`'s own private `weight_count_for` test helper. Assumes `cfg` is a
    /// plain A1 shape, as `minimal_layer_array` always produces.
    fn weight_count_for(cfg: &LayerArrayConfig) -> usize {
        let kernel_size = cfg.kernel_size.expect("A1 fixture: kernel_size is set");
        let head_size = cfg.head_size.expect("A1 fixture: head_size is set");
        let mut n = cfg.channels * cfg.input_size; // rechannel, no bias
        for _ in &cfg.dilations {
            n += cfg.channels * cfg.channels * kernel_size; // dilated weight
            n += cfg.channels; // dilated bias
            n += cfg.channels * cfg.condition_size; // mixin, no bias
            n += cfg.channels * cfg.channels; // residual weight
            n += cfg.channels; // residual bias
        }
        n += head_size * cfg.channels; // head_rechannel weight
        n
    }

    /// A minimal but real `PreparedNam`, deterministic (fixed weight values, not random — this
    /// module only needs *some* nontrivial model to exercise the stage's wiring, not a realistic
    /// one), at `sample_rate_hz`.
    fn tiny_model(sample_rate_hz: u32) -> Arc<PreparedNam> {
        let cfg = minimal_layer_array();
        let n = weight_count_for(&cfg);
        let mut weights: Vec<f32> = (0..n).map(|i| 0.01 * ((i % 7) as f32 - 3.0)).collect();
        weights.push(0.5); // trailing head_scale
        let file = NamFile {
            version: None,
            architecture: "WaveNet".to_string(),
            config: WaveNetConfig {
                layers: vec![cfg],
                head_scale: 0.5,
                head: None,
                in_channels: None,
                condition_dsp: None,
            },
            weights,
            sample_rate: Some(sample_rate_hz),
            metadata: NamMetadata::default(),
        };
        Arc::new(PreparedNam::from_file(&file).expect("minimal fixture should load"))
    }

    /// Runs `total` samples of a constant `value` through a mono stage in 64-sample chunks
    /// (`ctx`'s own `max_block_size`), returning every output sample in order.
    fn process_constant_in_chunks(stage: &mut NamStage, total: usize, value: f32) -> Vec<f32> {
        let mut buf = vec![value; total];
        let mut out = Vec::with_capacity(total);
        let mut offset = 0usize;
        while offset < buf.len() {
            let end = (offset + 64).min(buf.len());
            let n = end - offset;
            let mut channels: [&mut [f32]; 1] = [&mut buf[offset..end]];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            out.extend_from_slice(io.channel(0));
            offset = end;
        }
        out
    }

    /// Runs the whole of `input` through a mono stage in 64-sample chunks, returning every output
    /// sample in order — the non-constant-input analogue of `process_constant_in_chunks`, needed
    /// by the FR-NAM-090 tests below since a constant input can't distinguish "converged to the
    /// same level" from "both settled to the same silence".
    fn process_signal_in_chunks(stage: &mut NamStage, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(input.len());
        let mut offset = 0usize;
        while offset < input.len() {
            let end = (offset + 64).min(input.len());
            let n = end - offset;
            let mut buf = input[offset..end].to_vec();
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            out.extend_from_slice(io.channel(0));
            offset = end;
        }
        out
    }

    /// Same shape as `tiny_model`, but with a caller-chosen `head_scale` and declared `loudness`
    /// metadata (FR-NAM-090). `head_scale` linearly rescales the model's raw output as the very
    /// last step of the pipeline (`wavenet.rs`'s own doc comment: "output scaling applied after
    /// the head"), *after* every nonlinearity in the network -- so for a fixed input, doubling it
    /// is an exact +6.02 dB gain regardless of the internal `Tanh` activations, which makes it a
    /// clean, deterministic way to give two otherwise-identical fixture models a known real
    /// amplitude difference to normalise away.
    fn tiny_model_with_loudness(
        sample_rate_hz: u32,
        head_scale: f32,
        loudness: Option<f32>,
    ) -> Arc<PreparedNam> {
        let cfg = minimal_layer_array();
        let n = weight_count_for(&cfg);
        let mut weights: Vec<f32> = (0..n).map(|i| 0.01 * ((i % 7) as f32 - 3.0)).collect();
        weights.push(head_scale); // trailing weight is the authoritative head_scale.
        let file = NamFile {
            version: None,
            architecture: "WaveNet".to_string(),
            config: WaveNetConfig {
                layers: vec![cfg],
                head_scale,
                head: None,
                in_channels: None,
                condition_dsp: None,
            },
            weights,
            sample_rate: Some(sample_rate_hz),
            metadata: NamMetadata {
                loudness,
                ..NamMetadata::default()
            },
        };
        Arc::new(PreparedNam::from_file(&file).expect("minimal fixture should load"))
    }

    /// A deterministic, non-constant test signal (a slow sine, same shape
    /// `load_model_settles_to_match_direct_process_block` already drives its own comparison
    /// with) -- a constant input can't distinguish "two outputs converged to the same level" from
    /// "both settled to the same near-silent DC value" the way a genuinely time-varying signal
    /// can.
    fn sine_signal(total: usize) -> Vec<f32> {
        (0..total)
            .map(|i| 0.2 * ((i as f32) * 0.01).sin())
            .collect()
    }

    /// Plain RMS-to-dB, standing in for a true ITU-R BS.1770 integrated-loudness (LUFS)
    /// measurement, which does not exist anywhere in this codebase yet and would be a large
    /// undertaking outside this task's scope (no K-weighting, no gating, no channel weighting --
    /// just `20 * log10(rms)`). For the comparisons the tests below actually make -- two signals
    /// that are otherwise identical (or near enough) differing only in the gain FR-NAM-090's
    /// normalisation applies -- a plain RMS-in-dB figure moves in lockstep with what a true LUFS
    /// meter would report, so it is a fair proxy for *this* purpose even though it is not a
    /// conformant loudness measurement in general.
    fn rms_db(samples: &[f32]) -> f32 {
        let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
        namir_core::linear_to_db(rms)
    }

    /// Comfortably past the ~20 ms handover crossfade, the separate ~15 ms shared bypass blend,
    /// and FR-NAM-090's own 25 ms normalisation-gain ramp (all one-pole; several time constants
    /// each) -- 400 ms at 48 kHz, the same margin `load_model_settles_to_match_direct_process_block`
    /// already uses for the first two.
    const NORMALIZE_SETTLE_SAMPLES: usize = 19_200;

    // trace: FR-NAM-130
    #[test]
    fn nothing_loaded_is_exact_passthrough() {
        // FR-CHAIN-040/FR-NAM-130: usable with no model loaded, behaving as bypassed.
        let mut stage = stage(48_000, ChannelConfig::Mono);
        let input = 0.37f32;
        let out = process_constant_in_chunks(&mut stage, 48_000, input);
        let tail = *out.last().unwrap();
        assert!(
            (tail - input).abs() < 1e-6,
            "expected exact passthrough with nothing loaded, got {tail} vs input {input}"
        );
    }

    #[test]
    fn load_model_settles_to_match_direct_process_block() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        let model = tiny_model(sample_rate);

        // Long enough to clear the ~20 ms handover crossfade and the ~15 ms bypass blend many
        // times over (1 s at 48 kHz), driven with the same deterministic (not constant, so the
        // model's causal-conv history actually gets exercised) input the reference run sees.
        let total = 48_000usize;

        // A reference run of the *same* model via `PreparedNam::process` directly (its own
        // non-RT convenience wrapper, `wavenet.rs`'s own doc comment), on the whole input in one
        // shot, for comparison once the stage's handover has fully settled. Sized to `total` up
        // front since `process`/`process_block` require the state's own `max_n` to cover the
        // block passed to it (`PreparedNam::process_block`'s own panic contract) and `process`
        // passes its whole input as a single block.
        let mut reference_state = model.new_state(total);

        stage.load_model(Arc::clone(&model));

        let mut input = vec![0.0f32; total];
        for (i, s) in input.iter_mut().enumerate() {
            *s = 0.2 * ((i as f32) * 0.01).sin();
        }

        let mut stage_out = Vec::with_capacity(total);
        let mut offset = 0usize;
        while offset < total {
            let end = (offset + 64).min(total);
            let n = end - offset;
            let mut buf = input[offset..end].to_vec();
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            stage_out.extend_from_slice(io.channel(0));
            offset = end;
        }

        let reference_out = model.process(&mut reference_state, &input);

        // Only the tail should match the reference bit-for-bit-ish: same model, same sample rate
        // (no resampling in play), same causal history by then. The ~20 ms handover crossfade
        // itself finishes quickly, but the *separate* shared bypass blend only starts its own
        // ~15 ms one-pole convergence once the handover completes and `active` flips (this
        // module's doc comment's "two independent fades, composed" section) and a one-pole needs
        // several time constants to become numerically negligible -- 400 ms (many tau past both)
        // is a comfortable margin, not a tight one.
        let settle = 19_200usize; // 400 ms.
        for i in settle..total {
            assert!(
                (stage_out[i] - reference_out[i]).abs() < 1e-5,
                "sample {i}: stage {} vs reference {}",
                stage_out[i],
                reference_out[i]
            );
        }
    }

    #[test]
    fn handover_crossfade_has_no_large_single_sample_jump() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        let model_a = tiny_model(sample_rate);
        let model_b = tiny_model(sample_rate);

        stage.load_model(model_a);
        // Settle the first handover and the bypass blend fully (well past both).
        process_constant_in_chunks(&mut stage, 48_000, 0.1);

        stage.load_model(model_b);

        // A steady input through the second handover: track the largest single-sample jump.
        //
        // This runs **inside** `rt_harness::audio_section`, and that is the point of the test as
        // much as the smoothness assertion is. Before M4 it could not: the loop is long enough
        // (100 ms) to drive the handover all the way to completion, and completion used to *drop*
        // `slots[outgoing_idx]` -- a real `NamSlot` this time (unlike the first handover, from
        // nothing loaded, whose outgoing slot was already `None`) -- deallocating on the audio
        // thread. D-8.1 step 4's return ring is what closes that, so this test now covers
        // smoothness and RT-safety across a *complete* real-to-real handover at once. Do not
        // relax it back out of the harness: that would silently un-verify the one defect this
        // milestone exists to fix.
        let total = 4800usize; // 100 ms, comfortably longer than the 20 ms handover.
        let value = 0.1f32;
        let mut prev: Option<f32> = None;
        let mut max_delta = 0.0f32;
        let mut offset = 0usize;
        // Allocated up front, outside the harness, and reused: the buffer is the test's own
        // scaffolding, not part of what `process` is allowed to do.
        let mut buf = vec![value; 64];
        while offset < total {
            let n = 64usize.min(total - offset);
            buf[..n].fill(value);
            let mut channels: [&mut [f32]; 1] = [&mut buf[..n]];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            for &s in io.channel(0).iter() {
                if let Some(p) = prev {
                    max_delta = max_delta.max((s - p).abs());
                }
                prev = Some(s);
            }
            offset += n;
        }

        // Two structurally different (random-ish weight) WaveNet models processing the same
        // small constant input each stay within a small bounded range; a smooth equal-power
        // crossfade between two bounded signals cannot itself introduce a jump anywhere near a
        // full-range discontinuity. 0.5 is a generous bound well above ordinary sample-to-sample
        // movement for this fixture, tight enough to catch an actual discontinuity (a dropped
        // handover step would show as a jump on the order of the *entire* outgoing-vs-incoming
        // gap in a single sample).
        assert!(
            max_delta < 0.5,
            "handover crossfade produced a jump of {max_delta}, expected a smooth fade"
        );
    }

    /// **FR-CHAIN-020's "U per stage" limb for this stage**, which nothing executed until M14:
    /// `disabled_stage_is_passthrough_even_with_a_model_loaded` below applies `ENABLED = 0` *before
    /// any audio is processed* and then asserts a steady state, so it cannot see a bypass toggle at
    /// all — the requirement's "without an audible click or discontinuity" was untested for this
    /// stage.
    ///
    /// A constant input, exactly as `gate.rs`'s counterpart uses, so the signal contributes no slew
    /// of its own and every sample-to-sample step measured belongs to the crossfade. The bound is
    /// this stage's own [`BYPASS_CROSSFADE_TIME_CONSTANT_MS`]: a one-pole of time constant τ steps
    /// by at most `range · (1 − e^(−1/τ))`, which for a 15 ms τ at 48 kHz is `range / 720`.
    // trace: FR-CHAIN-020
    #[test]
    fn bypass_toggle_mid_signal_is_click_free() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        stage.load_model(tiny_model(sample_rate));

        // 1 s: many times over both the ~20 ms handover crossfade and the ~15 ms bypass blend, and
        // long enough for the model's own causal history to settle on a constant input.
        let value = 0.2f32;
        let settled_wet = *process_constant_in_chunks(&mut stage, 48_000, value)
            .last()
            .expect("the settle run produced output");
        // Non-vacuity: bypassing has to change something, or the smoothness assertion is empty.
        let range = (settled_wet - value).abs();
        assert!(
            range > 1e-3,
            "the model's output is indistinguishable from its input ({settled_wet} vs {value}), \
             so bypassing it changes nothing and this test would pass vacuously"
        );

        stage.apply(ParamChange {
            id: ENABLED_ID,
            value: 0.0,
        });

        // 200 ms, comfortably past the 15 ms blend.
        let out = process_constant_in_chunks(&mut stage, 9_600, value);

        // Include the step from the last pre-toggle sample into the first post-toggle one, which is
        // where an unramped switch would show.
        let mut prev = settled_wet;
        let mut max_delta = 0.0f32;
        for &s in &out {
            max_delta = max_delta.max((s - prev).abs());
            prev = s;
        }

        let tau_samples = (BYPASS_CROSSFADE_TIME_CONSTANT_MS / 1000.0) * f64::from(sample_rate);
        let ideal_max_delta = range * (1.0 - (-1.0 / tau_samples).exp()) as f32;
        assert!(
            max_delta <= ideal_max_delta * 1.01,
            "max_delta={max_delta} exceeds the {BYPASS_CROSSFADE_TIME_CONSTANT_MS} ms one-pole's \
             own steepest step {ideal_max_delta} across a range of {range}"
        );
        assert!(max_delta > 0.0, "the bypass blend never advanced");
        // And it actually arrived: the stage is passing its input through by the end.
        let tail = *out.last().unwrap();
        assert!(
            (tail - value).abs() < 1e-3,
            "expected the bypassed stage to settle onto its input, got {tail} vs {value}"
        );
    }

    /// **FR-CHAIN-050's mono core, for `NamStage` itself, in both multi-channel configurations.**
    /// Until M14 every test in this file was `ChannelConfig::Mono` bar one that asserted nothing
    /// about channel content, and `MonoToStereo` appeared in none of them — so the
    /// channel-0-then-duplicate shuttle this stage performs was verified only through the assembled
    /// chain, never at the stage.
    ///
    /// The two input channels carry *different* signals, so "both outputs agree" is a statement
    /// about the shuttle and not an accident of the input; and the shared result is compared
    /// against a `Mono` stage fed channel 0 alone, which is what "the engine core shall process a
    /// single channel" means operationally.
    // trace: FR-CHAIN-050
    #[test]
    fn every_multi_channel_configuration_duplicates_one_mono_core_result() {
        const FRAMES: usize = 24_000;
        const SETTLE: usize = 19_200;
        let sample_rate = 48_000;

        let left = sine_signal(FRAMES);
        let right: Vec<f32> = (0..FRAMES)
            .map(|i| 0.15 * ((i as f32) * 0.037).sin())
            .collect();

        let mut mono = stage(sample_rate, ChannelConfig::Mono);
        mono.load_model(tiny_model(sample_rate));
        let mono_out = process_signal_in_chunks(&mut mono, &left);

        for channel_config in [ChannelConfig::Stereo, ChannelConfig::MonoToStereo] {
            let mut stage = stage(sample_rate, channel_config);
            stage.load_model(tiny_model(sample_rate));

            let mut out_left = Vec::with_capacity(FRAMES);
            let mut out_right = Vec::with_capacity(FRAMES);
            let mut offset = 0usize;
            while offset < FRAMES {
                let end = (offset + 64).min(FRAMES);
                let n = end - offset;
                let mut l = left[offset..end].to_vec();
                let mut r = right[offset..end].to_vec();
                let mut channels: [&mut [f32]; 2] = [&mut l, &mut r];
                let mut io = StageIo::new(&mut channels, n);
                audio_section(|| stage.process(&mut io));
                out_left.extend_from_slice(io.channel(0));
                out_right.extend_from_slice(io.channel(1));
                offset = end;
            }

            // **Measured after the fade-in, and that is not a weakening.** Until the handover
            // completes and the shared bypass blend settles, this stage is partly passing its
            // *dry* input through, and the dry path is per channel by construction (`gate.rs` and
            // this module both capture it that way) — so two channels carrying different signals
            // legitimately produce different output while `mix < 1`. That is FR-CHAIN-040's
            // passthrough, not a second core. The mono-core claim is about the wet path, which is
            // everything this stage emits once settled.
            //
            // **To within a residue, and the residue has a name.** `mix` approaches 1.0 through
            // `m += mix_coeff · (1 − m)` in `f32`, and once `1 − m` falls below about
            // `ulp(1.0) / 2 / mix_coeff ≈ 2·10⁻⁵` the increment rounds away and the recurrence
            // stalls there rather than landing on 1.0. So a fully-engaged stage keeps mixing in
            // ~0.002% of its dry input — about −94 dB, inaudible, and the reason this compares
            // within a tolerance instead of `assert_eq!`. A core that really ran per channel would
            // differ by order 0.1 here, four orders of magnitude above the bound.
            for i in SETTLE..FRAMES {
                let spread = (out_left[i] - out_right[i]).abs();
                assert!(
                    spread < 1e-4,
                    "{channel_config:?}: sample {i} differs between channels by {spread}, so the \
                     core did not process a single channel"
                );
            }
            for i in SETTLE..FRAMES {
                assert!(
                    (out_left[i] - mono_out[i]).abs() < 1e-5,
                    "{channel_config:?}: sample {i} is {} where a Mono stage fed channel 0 alone \
                     produces {} -- the widened configuration is not the same core",
                    out_left[i],
                    mono_out[i]
                );
            }
            // Non-vacuous: channel 1 really did carry something else, and that something else
            // really was discarded rather than passed through. A stage that ran a core per channel,
            // or that simply passed channel 1 along, would leave channel 1's own 0.15-amplitude
            // tone in the output; what is there instead is channel 0's core result.
            assert!(
                left.iter()
                    .zip(right.iter())
                    .any(|(l, r)| (l - r).abs() > 0.05),
                "the two probe channels are too similar for this comparison to mean anything"
            );
            let discarded = right[SETTLE..]
                .iter()
                .zip(out_right[SETTLE..].iter())
                .map(|(dry, out)| (dry - out).abs())
                .fold(0.0f32, f32::max);
            assert!(
                discarded > 0.05,
                "{channel_config:?}: channel 1's output is within {discarded} of its own input, so \
                 nothing shows it was replaced by the mono core's result"
            );
        }
    }

    #[test]
    fn disabled_stage_is_passthrough_even_with_a_model_loaded() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: ENABLED_ID,
            value: 0.0,
        });
        stage.load_model(tiny_model(sample_rate));

        let input = 0.42f32;
        let out = process_constant_in_chunks(&mut stage, 48_000, input);
        let tail = *out.last().unwrap();
        assert!(
            (tail - input).abs() < 1e-4,
            "expected disabled stage to pass through even with a model loaded, got {tail} vs {input}"
        );
    }

    /// **FR-NAM-110's `Verify: U` method — "cross-correlate an impulse through the stage" — for the
    /// figure `namir-nam` cannot see.** W3 closed the model's own half in
    /// `crates/namir-nam/tests/latency.rs`, which cross-correlates through every architecture and
    /// would fail if inference introduced delay. What survived was this stage's *other* figure: when
    /// a model's declared rate differs from the engine's, [`SlotResampler`] adds latency, and that
    /// value was asserted only as `> 0` by
    /// [`latency_reports_the_active_slots_resampler_latency`] below, with this module's own doc
    /// comment recording that it "is not proven sample-exact".
    ///
    /// It is proven here, and it is exact: **640 samples reported, 640 measured** for a 44.1 kHz
    /// Nano model in a 48 kHz engine at a 64-frame block, stable across every correlation window
    /// tried.
    ///
    /// # What is correlated against what, and why not against the input
    ///
    /// The obvious construction — correlate the stage's output against the stage's *input* — does
    /// not measure this. A NAM model is a filter as well as a nonlinearity, and an arbitrary filter
    /// has a **group delay of its own** that is neither latency nor constant with frequency: driving
    /// this same chirp through a 1:1-rate stage, where D-9.2 bypasses the resampler entirely and the
    /// stage correctly reports zero, an input-referenced correlation reads 11, 9, 8 or 7 samples
    /// depending only on where the measurement window starts, because the chirp is at a different
    /// frequency in each. That is the model's filtering, not a latency the stage failed to declare —
    /// `namir-nam`'s own `tests/latency.rs` is what establishes that inference itself is causal.
    ///
    /// So the reference is **the same model at the engine's own rate**. Both runs carry the model's
    /// filtering; the only thing between them is the resampler, so the lag between the two outputs
    /// is exactly what [`SlotResampler`] adds — which is the figure FR-NAM-110 makes this stage
    /// report and the one nothing had ever checked from outside the stage.
    ///
    /// A chirp rather than a literal impulse: this stage's bypass blend needs a settling period
    /// before the measurement and an impulse would be long over by then, while a sustained broadband
    /// probe measures the same delay and conditions the correlation far better.
    // trace: FR-NAM-110
    #[test]
    fn the_resampled_stages_reported_latency_is_the_delay_the_signal_actually_sees() {
        const FRAMES: usize = 32_768;
        /// Discarded: past the handover crossfade and the bypass blend, so what is correlated is
        /// the settled wet path rather than a fade between it and an undelayed dry signal.
        const WARMUP: usize = 8_192;
        /// Correlation window: long enough for the chirp to be well conditioned, short enough to
        /// stay cheap in a debug build.
        const WINDOW: usize = 8_192;

        let signal = crate::probe::chirp(FRAMES, 200.0, 6_000.0, 48_000, 0.25);

        let run = |declared_rate_hz: u32| -> (u32, Vec<f32>) {
            let mut stage = stage(48_000, ChannelConfig::Mono);
            stage.load_model(crate::probe::nam_model(
                namir_fixtures::nam::WaveNetShape::Nano,
                53,
                declared_rate_hz,
            ));
            let out = process_signal_in_chunks(&mut stage, &signal);
            (stage.latency_samples(), out)
        };

        let (reported_1_to_1, at_engine_rate) = run(48_000);
        assert_eq!(
            reported_1_to_1, 0,
            "a model at the engine's own rate engages no conversion, so nothing may be reported"
        );

        let (reported, resampled) = run(44_100);
        assert!(
            reported > 0,
            "a model at a different rate must engage the resampler and report its latency"
        );

        let measured = crate::probe::estimate_delay_samples(
            &at_engine_rate[WARMUP..WARMUP + WINDOW],
            &resampled[WARMUP..],
            reported as usize * 2 + 64,
        );
        assert_eq!(
            measured, reported as usize,
            "the stage reports {reported} samples of latency and delays the signal by {measured}: \
             FR-NAM-110 asks for the figure it reports, not one of the right order of magnitude"
        );
    }

    /// **Issue #56: the deferred-finalization state was entered and never left.**
    ///
    /// When a fade reached `remaining == 0` while the retire pen was occupied, `process_channel0`
    /// set `crossfade = Some(remaining: 0)` and skipped finalization. On every later block the
    /// `remaining == 0` fast path returned *before* the finalization block, so `self.retired` was
    /// never re-tested: `active` never flipped, `crossfade` never cleared, and the outgoing slot
    /// never reached the pen — permanently, for the rest of the session, even after the worker
    /// resumed draining. `latency_samples()` kept reporting the outgoing slot (FR-CLAP-040), the
    /// `handover_active` reading stayed pinned at 1.0, and a later install would displace the
    /// audible slot and re-fade from the stale outgoing one.
    ///
    /// The state is reached the way the engine reaches it: an install that displaces a slot still
    /// fading in parks that slot in the pen, and the fade then completes with the pen occupied.
    /// This test simply does not collect in between, which is what a stalled worker looks like
    /// from inside the stage.
    ///
    /// Committed red-first: before the fix, the final three assertions all fail — `crossfade` is
    /// still `Some`, `active` is still 1, and the pen is empty because the outgoing slot is stuck
    /// in `slots[1]` forever.
    /// **Issue #141 at the stage, where the mechanism is visible.** The chain-level probe
    /// (`chain_probes.rs`'s `a_first_load_is_audible_inside_its_own_fade_at_every_block_size`)
    /// asserts the audible consequence; this pins the two pieces of state that produced it, so a
    /// regression names its own cause rather than only its symptom.
    ///
    /// The defect was **not** that the wet signal went unproduced during a first load's fade — it
    /// was produced all along. Measured before the fix, on the first 64-frame block after a first
    /// `load_model`: `crossfade_incoming` diverged from the dry input by 2.0e-1 (the whole wet
    /// signal), `crossfade_outgoing` by exactly 0e0 (the `None` slot's dry passthrough), the fade
    /// advanced 960 -> 896 — and `io` came out **bit-exactly the dry input**, because the shared
    /// bypass blend sat at `mix == mix_target == 0.0` for the fade's entire duration and multiplied
    /// the equal-power blend away. So the fade was applied and then discarded.
    ///
    /// Asserted here: the blend is engaged from the first block of a first load, the fade's own
    /// first sample is still bit-exactly dry (which is what makes `begin_crossfade`'s snap
    /// click-free rather than merely quiet), and the block as a whole is no longer dry.
    #[test]
    fn a_first_load_engages_the_bypass_blend_for_the_whole_fade() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);

        assert_eq!(stage.mix, 0.0, "nothing loaded: bypassed");
        assert_eq!(stage.mix_target, 0.0);

        stage.load_model(tiny_model(sample_rate));
        assert_eq!(
            stage.mix_target, 1.0,
            "a first load's fade must be heard, so the bypass blend's target is engaged when the \
             fade starts -- not when it completes"
        );
        assert_eq!(
            stage.mix, 1.0,
            "and `mix` is snapped there, because a 15 ms one-pole composed on top of the \
             equal-power curve is not the equal-power curve FR-NAM-070 specifies"
        );

        let input: Vec<f32> = (0..64).map(|i| 0.2 * ((i as f32) * 0.05).sin()).collect();
        let out = process_signal_in_chunks(&mut stage, &input);

        assert_eq!(
            out[0].to_bits(),
            input[0].to_bits(),
            "the fade's first sample has theta = 0, so it must be the dry input bit-for-bit: that \
             is what makes snapping `mix` a continuation rather than a step"
        );
        let divergence = out
            .iter()
            .zip(input.iter())
            .position(|(a, b)| a.to_bits() != b.to_bits());
        assert_eq!(
            divergence,
            Some(1),
            "the wet signal must appear on the fade's second sample, not at the block boundary \
             after it completes (issue #141)"
        );
        assert!(
            stage.crossfade.is_some(),
            "64 samples is well inside a 960-sample fade"
        );
    }

    /// The other half of issue #141's fix: the snap is taken **only** where it cannot be heard.
    /// A replacement model faded into an already-engaged stage has a non-transparent wet path
    /// (`slots[active]` is `Some`), so nothing is snapped and `mix` is left exactly where it was —
    /// which is 1.0 there anyway, the case that always worked.
    #[test]
    fn a_replacement_load_snaps_nothing() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        stage.load_model(tiny_model(sample_rate));
        process_constant_in_chunks(&mut stage, 48_000, 0.1);
        assert!(
            !stage.wet_path_is_transparent(),
            "an engaged stage's wet path is not a passthrough, which is what withholds the snap"
        );

        // Disable the stage and catch `mix` part-way down its 15 ms ramp, so a snap would be
        // visible as a jump rather than hidden by an already-settled value.
        stage.apply(ParamChange {
            id: ENABLED_ID,
            value: 0.0,
        });
        process_constant_in_chunks(&mut stage, 64, 0.1);
        let mid_ramp = stage.mix;
        assert!(
            mid_ramp > 0.0 && mid_ramp < 1.0,
            "the bypass ramp should be mid-flight, got {mid_ramp}"
        );

        stage.load_model(tiny_model(sample_rate));
        assert_eq!(
            stage.mix, mid_ramp,
            "an install into a stage whose wet path is audible must not move `mix` at all"
        );
    }

    #[test]
    fn a_handover_deferred_by_a_full_retire_pen_finalizes_once_the_pen_clears() {
        const SR: u32 = 48_000;
        // 20 ms at 48 kHz = 960 samples; 2048 is comfortably past a whole fade.
        const PAST_A_FADE: usize = 2_048;
        // Well inside one, so the next install displaces a slot that is still fading in.
        const MID_FADE: usize = 128;

        let mut stage = stage(SR, ChannelConfig::Mono);

        // First model: settles with nothing displaced, so the pen stays empty.
        stage.load_model(tiny_model(SR));
        process_constant_in_chunks(&mut stage, PAST_A_FADE, 0.1);
        assert_eq!(stage.active, 1);
        assert!(stage.crossfade.is_none());
        assert!(stage.retired.is_none());

        // Second model, then a third *while the second is still fading in*: the third install
        // displaces the second into the pen (`install`'s "a move, not a drop").
        stage.load_model(tiny_model(SR));
        process_constant_in_chunks(&mut stage, MID_FADE, 0.1);
        stage.load_model(tiny_model(SR));
        assert!(
            stage.retired.is_some(),
            "the displaced slot should be parked in the pen"
        );

        // Let the third model's fade run to completion with the pen still occupied -- nothing
        // collects, which is exactly D-8.1's "the worker is not draining" case.
        process_constant_in_chunks(&mut stage, PAST_A_FADE, 0.1);
        assert_eq!(
            stage.crossfade,
            Some(Crossfade {
                remaining: 0,
                total: stage.crossfade_total_samples
            }),
            "the fade should have reached zero and deferred its finalization"
        );
        assert_eq!(
            stage.active, 1,
            "`active` may not flip while the pen is full"
        );

        // The worker drains: the pen empties.
        let (mut producer, mut consumer) = crate::ring::ring::<Resource>(4);
        {
            let mut sink = RetireSink::new(&mut producer);
            stage.collect_retired(&mut sink);
        }
        assert!(stage.retired.is_none());
        assert!(
            consumer.try_pop().is_some(),
            "the displaced slot reached the ring"
        );

        // One more block is all it should take. Before the fix, no number of blocks was enough.
        process_constant_in_chunks(&mut stage, 64, 0.1);
        assert!(
            stage.crossfade.is_none(),
            "the deferred handover must finalize once the pen clears, not stay in it forever"
        );
        assert_eq!(
            stage.active, 0,
            "finalization flips `active` onto the slot that faded in"
        );
        assert!(
            stage.retired.is_some(),
            "the outgoing slot must reach the pen, not stay stuck in `slots`"
        );
        assert_eq!(
            stage.mix_target, 1.0,
            "`recompute_mix_target` must have re-run against the newly-active slot"
        );
    }

    /// The consequence of issue #56 that is audible rather than merely wrong on paper: a
    /// **first** load deferred by a full pen left `mix_target` at 0.0, so the stage stayed
    /// bypassed — silent as far as the model is concerned — for the rest of the session.
    ///
    /// Reached the same way as the test above, but with the pen filled by an unload rather than by
    /// a prior model, so the deferred handover is the one that first makes a model audible.
    ///
    /// **Issue #141 changed what this can observe, and the change is recorded rather than papered
    /// over.** A first load now engages the bypass blend when its fade *starts*, so the deferral
    /// this test constructs no longer has a bypassed interval for the mid-run assertion to catch;
    /// what it pins instead is that the deferral is entered without losing audibility and left
    /// completely once the pen clears (`crossfade` cleared, `active` flipped, `mix_target` still
    /// engaged), which is the state machine #56 was about.
    #[test]
    fn a_deferred_first_handover_does_not_leave_the_stage_bypassed_forever() {
        const SR: u32 = 48_000;
        const PAST_A_FADE: usize = 2_048;
        const MID_FADE: usize = 128;

        let mut stage = stage(SR, ChannelConfig::Mono);

        // Get one model settled and audible, then unload it so the stage is back to nothing
        // active -- and immediately load a replacement while that unload fade is still running,
        // which parks the unload's own slot in the pen.
        stage.load_model(tiny_model(SR));
        process_constant_in_chunks(&mut stage, PAST_A_FADE, 0.1);
        {
            let (mut producer, _consumer) = crate::ring::ring::<Resource>(4);
            let mut sink = RetireSink::new(&mut producer);
            stage.collect_retired(&mut sink);
        }
        stage.unload();
        process_constant_in_chunks(&mut stage, PAST_A_FADE, 0.1);
        assert_eq!(stage.mix_target, 0.0, "an unloaded stage fades to dry");
        {
            // The unload's own finalization parks the formerly-active slot; clear it so the next
            // install is the ordinary pen-empty case and the deferral this test is about is
            // caused by the mid-fade displacement below, not by leftovers.
            let (mut producer, _consumer) = crate::ring::ring::<Resource>(4);
            let mut sink = RetireSink::new(&mut producer);
            stage.collect_retired(&mut sink);
        }

        stage.load_model(tiny_model(SR));
        process_constant_in_chunks(&mut stage, MID_FADE, 0.1);
        stage.load_model(tiny_model(SR));
        assert!(stage.retired.is_some());
        process_constant_in_chunks(&mut stage, PAST_A_FADE, 0.1);
        // **Issue #141 moved this assertion, and moved it in the direction #56 wanted.** It used
        // to read `mix_target == 0.0` — "still deferred, still bypassed" — because a first load's
        // target was derived from the (empty) outgoing slot alone. `recompute_mix_target` now
        // counts the slot being faded *into*, so the deferral no longer costs audibility at all:
        // it is the bookkeeping that is outstanding, never the audio. What #56 is about — that the
        // deferral is left rather than entered forever — is the `crossfade`/`active`/`retired`
        // assertions below, which are unchanged.
        assert_eq!(
            stage.mix_target, 1.0,
            "a deferred first handover must still be audible: the pen being full delays the \
             retirement, not the fade"
        );

        let (mut producer, _consumer) = crate::ring::ring::<Resource>(4);
        {
            let mut sink = RetireSink::new(&mut producer);
            stage.collect_retired(&mut sink);
        }
        process_constant_in_chunks(&mut stage, 64, 0.1);
        assert!(
            stage.crossfade.is_none(),
            "the deferred handover must finalize once the pen clears, not stay in it forever"
        );
        assert_eq!(
            stage.active, 1,
            "finalization flips `active` onto the slot that faded in"
        );
        assert_eq!(
            stage.mix_target, 1.0,
            "once the pen clears the stage must be audible on its own account, with the fade over \
             and `slots[active]` holding the model"
        );
    }

    #[test]
    fn latency_reports_the_active_slots_resampler_latency() {
        // 1:1 rate: bypassed entirely, zero added latency (D-9.2).
        let mut stage_1_to_1 = stage(48_000, ChannelConfig::Mono);
        assert_eq!(stage_1_to_1.latency_samples(), 0);
        stage_1_to_1.load_model(tiny_model(48_000));
        process_constant_in_chunks(&mut stage_1_to_1, 48_000, 0.0); // settle the handover.
        assert_eq!(
            stage_1_to_1.latency_samples(),
            0,
            "a same-rate model must report zero added latency"
        );

        // Mismatched rate: the resampler pair introduces real, nonzero latency.
        let mut stage_resampled = stage(48_000, ChannelConfig::Mono);
        stage_resampled.load_model(tiny_model(44_100));
        process_constant_in_chunks(&mut stage_resampled, 48_000, 0.0);
        assert!(
            stage_resampled.latency_samples() > 0,
            "a resampled model must report nonzero added latency"
        );
    }

    /// The path most likely to allocate if a `NamSlot`'s scratch/FIFOs are undersized or absent:
    /// stereo, mid-handover-crossfade, so the dry capture, both slots' wet processing, the
    /// equal-power blend and the channel-0-then-duplicate shuttle all run in the same block.
    #[test]
    fn crossfade_in_progress_does_not_allocate() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Stereo);
        stage.load_model(tiny_model(sample_rate));
        // Still mid-handover (20 ms = 960 samples at 48 kHz; 64 samples in is well inside it).
        let mut left = [0.1f32; 64];
        let mut right = [0.1f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        stage.load_model(tiny_model(sample_rate)); // start a second handover, still mid-first.
        let mut left = [0.1f32; 64];
        let mut right = [0.1f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));
    }

    /// Same shape as `crossfade_in_progress_does_not_allocate`, but the active slot is resampled
    /// (D-9.2's mismatched-rate path) — the highest-risk path for an accidental allocation, since
    /// it is the only one that touches `SlotResampler`'s FIFOs during `process`.
    #[test]
    fn resampled_crossfade_in_progress_does_not_allocate() {
        let mut stage = stage(48_000, ChannelConfig::Mono);
        stage.load_model(tiny_model(44_100));
        let mut buf = [0.1f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        stage.load_model(tiny_model(44_100)); // start a second handover, still mid-first.
        let mut buf = [0.1f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));
    }

    /// Offline, non-real-time sample-rate conversion by direct windowed-sinc interpolation, written
    /// from scratch here so that FR-NAM-050's "resampled offline" reference does not share an
    /// implementation with the thing under test. [`SlotResampler`] is `rubato::FftFixedInOut`, an
    /// overlap-add FFT resampler running on a fixed internal block; this is a time-domain
    /// convolution against a Blackman-windowed sinc, evaluated at each output sample's own
    /// fractional position with no blocking at all. Same operation, no shared code and no shared
    /// method — which is the whole point of an offline reference.
    ///
    /// The kernel is normalised per output sample so DC gain is exactly 1 at every fractional
    /// phase; the cutoff sits at 0.45 × the lower of the two rates, matching the region FR-NAM-060's
    /// M14 note calls the satisfiable one.
    fn resample_offline(input: &[f32], from_hz: f64, to_hz: f64) -> Vec<f32> {
        /// Half the kernel length. 96 taps either side is far longer than anything real-time would
        /// use, which is exactly what an offline reference is for.
        const HALF_TAPS: isize = 96;

        let ratio = from_hz / to_hz; // input samples per output sample
        let cutoff = 0.45 * from_hz.min(to_hz);
        let out_len = (input.len() as f64 / ratio).floor() as usize;
        let half_width = HALF_TAPS as f64 + 1.0;

        let sinc = |x: f64| {
            if x.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
            }
        };
        // Blackman window over [-1, 1], zero outside.
        let window = |t: f64| {
            if t.abs() >= 1.0 {
                0.0
            } else {
                let a = std::f64::consts::PI * (t + 1.0);
                0.42 - 0.5 * a.cos() + 0.08 * (2.0 * a).cos()
            }
        };

        let mut out = Vec::with_capacity(out_len);
        for m in 0..out_len {
            let center = m as f64 * ratio;
            let first = center.floor() as isize;
            let (mut acc, mut norm) = (0.0f64, 0.0f64);
            for k in (first - HALF_TAPS)..=(first + HALF_TAPS) {
                let t = center - k as f64;
                let w = window(t / half_width);
                if w == 0.0 {
                    continue;
                }
                let h = 2.0 * cutoff / from_hz * sinc(2.0 * cutoff * t / from_hz) * w;
                norm += h;
                if k >= 0 && (k as usize) < input.len() {
                    acc += h * f64::from(input[k as usize]);
                }
            }
            out.push((acc / norm) as f32);
        }
        out
    }

    /// `20·log10(rms(error) / rms(reference))` — FR-NAM-030's figure of merit, which FR-NAM-050
    /// imports by reference.
    fn error_rms_db(reference: &[f32], measured: &[f32]) -> f64 {
        assert_eq!(reference.len(), measured.len());
        let mut err_sq = 0.0f64;
        let mut ref_sq = 0.0f64;
        for (r, m) in reference.iter().zip(measured.iter()) {
            let (r, m) = (f64::from(*r), f64::from(*m));
            err_sq += (r - m) * (r - m);
            ref_sq += r * r;
        }
        20.0 * ((err_sq / ref_sq).sqrt()).log10()
    }

    /// **FR-NAM-050's `Verify: I` method, computed for the first time.** "A 48 kHz model driven at
    /// 44.1 kHz shall match, within the FR-NAM-030 tolerance, the same model driven at 48 kHz with
    /// the input and output resampled offline."
    ///
    /// Both paths, on the same probe, through the same model:
    ///
    /// - **the stage's path** — a 48 kHz model in a 44.1 kHz engine, so [`SlotResampler`] runs
    ///   44.1 → 48 kHz into the model and 48 → 44.1 kHz out of it, block by block through
    ///   `Stage::process`, aligned afterwards by the latency the stage itself reports;
    /// - **the offline path** — the same probe resampled to 48 kHz by [`resample_offline`], the
    ///   model run directly on it in one shot by `PreparedNam::process`, and the result resampled
    ///   back to 44.1 kHz the same way.
    ///
    /// # The measured figures, and why this stays a partial
    ///
    /// **The tolerance is met — up to a probe bandwidth, and then it is not.** FR-NAM-030's figure
    /// is an error RMS 90 dB below the reference's; measured over four probe bands, latency-aligned:
    ///
    /// | Probe | Error RMS | FR-NAM-030's −90 dB |
    /// |---|---|---|
    /// | 100 Hz – 1 kHz | −91.5 dB | met |
    /// | 100 Hz – 2 kHz | −92.9 dB | met |
    /// | 100 Hz – 5 kHz | −86.3 dB | 3.7 dB short |
    /// | 100 Hz – 8 kHz | −78.2 dB | 11.8 dB short |
    ///
    /// The trend is the finding, and it is not a resampler passband problem: both conversions are
    /// flat far above 8 kHz. **It is the nonlinearity between them.** A NAM model is a distortion,
    /// so an 8 kHz probe puts harmonics at 16, 24 and 32 kHz — above the 22.05 kHz Nyquist the
    /// return conversion has to fold or reject — and that is precisely where a 193-tap windowed sinc
    /// and a 256-point overlap-add FFT resampler stop agreeing. Two *different* resamplers cannot
    /// agree to −90 dB on content sitting in their transition bands, and an offline reference that
    /// shared `SlotResampler`'s own implementation would be checking nothing.
    ///
    /// So the requirement's own tolerance is executed and met for a probe whose harmonics stay
    /// inside both passbands, and the residue is a question the FRS has to answer rather than a test
    /// can: FR-NAM-050's method names no probe signal and no reference resampler, and FR-NAM-030's
    /// tolerance was written for a comparison against `NeuralAmpModelerCore` on a *fixed* 10-second
    /// signal, not for a resampler-versus-resampler difference.
    ///
    /// What every band establishes, and what nothing established before M14: the stage's round trip
    /// really does carry the signal through the model at the model's declared rate, sample-aligned
    /// by the latency it reports. A regression that broke the conversion moves these figures by tens
    /// of dB — as the one-sample-misalignment control below demonstrates.
    // trace-partial: FR-NAM-050
    // uncovered: FR-NAM-050 — the comparison the Verify method specifies is computed here for the
    // uncovered: first time (a 48 kHz model in a 44.1 kHz engine against the same model driven at
    // uncovered: 48 kHz with the probe resampled offline by an independent from-scratch
    // uncovered: windowed-sinc reference, latency-aligned) and FR-NAM-030's -90 dB tolerance is
    // uncovered: met for probes up to 2 kHz: -91.5 dB to 1 kHz, -92.9 dB to 2 kHz. It is not met
    // uncovered: for wider probes -- -86.3 dB to 5 kHz, -78.2 dB to 8 kHz -- because the model's
    // uncovered: own harmonics land above the 22.05 kHz Nyquist where two different resamplers
    // uncovered: cannot agree to -90 dB, and a reference sharing SlotResampler's implementation
    // uncovered: would check nothing. Which probe decides is an FRS question: the method names no
    // uncovered: signal and no reference resampler; closes M8
    #[test]
    fn a_48_khz_model_driven_at_44_1_khz_matches_the_offline_resampled_path() {
        /// 1 s at 44.1 kHz. Long enough for both paths to settle far from their edges, short enough
        /// that a 193-tap offline convolution stays cheap in a debug build.
        const FRAMES: usize = 44_100;
        /// Discarded from both ends before comparing: the stage's own handover and bypass blends at
        /// the start, and the offline kernel's edge taper at both ends.
        const EDGE: usize = 8_192;
        /// FR-NAM-030's tolerance, which FR-NAM-050 imports by reference.
        const FR_NAM_030_DB: f64 = -90.0;

        let engine_rate = 44_100u32;
        let model_rate = 48_000u32;
        let model =
            crate::probe::nam_model(namir_fixtures::nam::WaveNetShape::Nano, 59, model_rate);

        // (probe's top frequency, the ceiling this band is asserted against). The first two are
        // FR-NAM-030's own tolerance; the last two are the measured shortfall, recorded with a
        // couple of dB of headroom so a regression still fails. See this test's doc comment.
        let bands: [(f32, f64); 4] = [
            (1_000.0, FR_NAM_030_DB),
            (2_000.0, FR_NAM_030_DB),
            (5_000.0, -84.0),
            (8_000.0, -76.0),
        ];

        for (top_hz, ceiling_db) in bands {
            let probe = crate::probe::chirp(FRAMES, 100.0, top_hz, engine_rate, 0.25);

            // --- The stage's path.
            let mut stage = stage(engine_rate, ChannelConfig::Mono);
            stage.load_model(Arc::clone(&model));
            let through_stage = process_signal_in_chunks(&mut stage, &probe);
            let latency = stage.latency_samples() as usize;
            assert!(
                latency > 0,
                "a 48 kHz model in a 44.1 kHz engine must engage the resampler"
            );

            // --- The offline path.
            let at_model_rate =
                resample_offline(&probe, f64::from(engine_rate), f64::from(model_rate));
            let mut state = model.new_state(at_model_rate.len());
            let model_out = model.process(&mut state, &at_model_rate);
            let offline =
                resample_offline(&model_out, f64::from(model_rate), f64::from(engine_rate));

            // --- Aligned by the figure the stage reports, over the settled middle.
            let end = (FRAMES - EDGE)
                .min(offline.len())
                .min(through_stage.len() - latency - 1);
            assert!(end > EDGE + 8_000, "the comparison window is too short");
            let reference = &offline[EDGE..end];
            let figure = error_rms_db(reference, &through_stage[EDGE + latency..end + latency]);

            assert!(
                figure < ceiling_db,
                "a 100 Hz-{top_hz} Hz probe: the stage's resampled path differs from the \
                 offline-resampled path by {figure:.2} dB RMS, past this band's recorded \
                 {ceiling_db} dB"
            );
            // Non-vacuous twice over: the reference really carries signal, and the comparison is
            // really this sensitive -- a one-sample misalignment costs tens of dB.
            assert!(
                reference.iter().any(|s| s.abs() > 1e-3),
                "a 100 Hz-{top_hz} Hz probe: the offline path produced no signal to compare against"
            );
            let misaligned = error_rms_db(
                reference,
                &through_stage[EDGE + latency + 1..end + latency + 1],
            );
            assert!(
                misaligned > figure + 6.0,
                "a 100 Hz-{top_hz} Hz probe: shifting the comparison by one sample moves the \
                 figure only from {figure:.2} to {misaligned:.2} dB, so this test cannot tell an \
                 aligned path from a misaligned one"
            );
        }
    }

    #[test]
    fn resampled_path_runs_many_varying_blocks_without_allocating_or_panicking() {
        // Best-effort coverage for the mismatched-rate path (this module's doc comment: not
        // verified to D-9.3's quality bar) -- proves it survives sustained, irregularly-sized
        // real use, not that its numerical output meets any particular fidelity bound.
        let mut stage = stage(48_000, ChannelConfig::Mono);
        stage.load_model(tiny_model(44_100));

        let block_sizes = [64usize, 1, 37, 64, 3, 64, 64, 17];
        for (i, &n) in block_sizes.iter().cycle().take(200).enumerate() {
            let mut buf = vec![0.1 * ((i as f32) * 0.05).sin(); n];
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            for &s in io.channel(0).iter() {
                assert!(s.is_finite(), "non-finite output at iteration {i}");
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // FR-NAM-090: loudness normalisation.
    // -----------------------------------------------------------------------------------------

    /// FR-NAM-090's own `Verify: U` method, executed close to verbatim: "measure integrated
    /// loudness of two models with differing declared loudness driven by the same input; the
    /// difference shall be within 1 LU with normalisation enabled." The two fixture models share
    /// every weight except a doubled `head_scale` on the louder one (an exact +6.02 dB real
    /// amplitude difference — see `tiny_model_with_loudness`'s doc comment for why that's clean to
    /// reason about), and declare loudness values 6 dB apart to match (`-20`/`-14` LUFS) — so
    /// normalisation should very nearly cancel the real gap. `rms_db` is a stated proxy for a true
    /// LUFS meter; see its own doc comment for why that's a fair substitution here.
    // trace-partial: FR-NAM-090
    // uncovered: FR-NAM-090 — the Verify: U method names "integrated loudness," ITU-R BS.1770's
    // uncovered: specific K-weighted, gated measurement; this test substitutes plain RMS-in-dB
    // uncovered: (rms_db's own doc comment states the substitution). The two agree for this
    // uncovered: test's own otherwise-matched signal pairs, which is why the behavior under test
    // uncovered: is real, but a true LUFS meter does not exist anywhere in this codebase, so the
    // uncovered: method is not executed as stated in the general case; closes M8
    #[test]
    fn normalization_enabled_brings_differently_declared_models_within_one_lu() {
        let sample_rate = 48_000;
        let mut quiet = stage(sample_rate, ChannelConfig::Mono);
        let mut loud = stage(sample_rate, ChannelConfig::Mono);
        quiet.load_model(tiny_model_with_loudness(sample_rate, 0.5, Some(-20.0)));
        loud.load_model(tiny_model_with_loudness(sample_rate, 1.0, Some(-14.0)));

        let input = sine_signal(48_000);
        let quiet_out = process_signal_in_chunks(&mut quiet, &input);
        let loud_out = process_signal_in_chunks(&mut loud, &input);

        let quiet_level = rms_db(&quiet_out[NORMALIZE_SETTLE_SAMPLES..]);
        let loud_level = rms_db(&loud_out[NORMALIZE_SETTLE_SAMPLES..]);
        assert!(
            (quiet_level - loud_level).abs() <= 1.0,
            "quiet_level={quiet_level} loud_level={loud_level}, expected within 1 LU"
        );
    }

    /// FR-NAM-090: "The user shall be able to disable this normalisation" — with it off, the two
    /// models' real ~6.02 dB amplitude gap (from the doubled `head_scale`, see
    /// `tiny_model_with_loudness`'s doc comment) must survive essentially untouched, not collapse
    /// the way the enabled case above does.
    #[test]
    fn normalization_disabled_leaves_models_at_their_original_relative_levels() {
        let sample_rate = 48_000;
        let mut quiet = stage(sample_rate, ChannelConfig::Mono);
        let mut loud = stage(sample_rate, ChannelConfig::Mono);
        quiet.apply(ParamChange {
            id: NORMALIZE_ENABLED_ID,
            value: 0.0,
        });
        loud.apply(ParamChange {
            id: NORMALIZE_ENABLED_ID,
            value: 0.0,
        });
        quiet.load_model(tiny_model_with_loudness(sample_rate, 0.5, Some(-20.0)));
        loud.load_model(tiny_model_with_loudness(sample_rate, 1.0, Some(-14.0)));

        let input = sine_signal(48_000);
        let quiet_out = process_signal_in_chunks(&mut quiet, &input);
        let loud_out = process_signal_in_chunks(&mut loud, &input);

        let quiet_level = rms_db(&quiet_out[NORMALIZE_SETTLE_SAMPLES..]);
        let loud_level = rms_db(&loud_out[NORMALIZE_SETTLE_SAMPLES..]);
        let gap = loud_level - quiet_level;
        assert!(
            (gap - 6.02).abs() < 0.5,
            "expected the raw ~6.02 dB gap to survive with normalisation disabled, got {gap}"
        );
    }

    /// FR-NAM-090: "The user shall be able to... offset it" — applying a +6 dB offset to a single
    /// loaded model's normalisation gain must raise its output level by ~6 dB relative to no
    /// offset.
    #[test]
    fn offset_parameter_shifts_the_applied_gain_by_the_expected_amount() {
        let sample_rate = 48_000;
        let input = sine_signal(48_000);

        let mut baseline_stage = stage(sample_rate, ChannelConfig::Mono);
        baseline_stage.load_model(tiny_model_with_loudness(sample_rate, 0.5, Some(-20.0)));
        let baseline_out = process_signal_in_chunks(&mut baseline_stage, &input);
        let baseline_level = rms_db(&baseline_out[NORMALIZE_SETTLE_SAMPLES..]);

        let mut offset_stage = stage(sample_rate, ChannelConfig::Mono);
        offset_stage.apply(ParamChange {
            id: NORMALIZE_OFFSET_DB_ID,
            value: 6.0,
        });
        offset_stage.load_model(tiny_model_with_loudness(sample_rate, 0.5, Some(-20.0)));
        let offset_out = process_signal_in_chunks(&mut offset_stage, &input);
        let offset_level = rms_db(&offset_out[NORMALIZE_SETTLE_SAMPLES..]);

        let delta = offset_level - baseline_level;
        assert!(
            (delta - 6.0).abs() < 0.5,
            "expected a +6 dB shift from the offset parameter, got {delta}"
        );
    }

    /// FR-NAM-090: "When a model has no declared loudness at all... apply no normalisation gain"
    /// — a model whose `loudness` is `None` (A1 files, or any file omitting the key) must sound
    /// identical whether normalisation is on or off, since there is nothing declared to normalise
    /// against.
    #[test]
    fn model_with_no_declared_loudness_gets_zero_normalization_gain() {
        let sample_rate = 48_000;
        let input = sine_signal(48_000);

        let mut normalize_on = stage(sample_rate, ChannelConfig::Mono);
        normalize_on.load_model(tiny_model_with_loudness(sample_rate, 0.5, None));
        let on_out = process_signal_in_chunks(&mut normalize_on, &input);

        let mut normalize_off = stage(sample_rate, ChannelConfig::Mono);
        normalize_off.apply(ParamChange {
            id: NORMALIZE_ENABLED_ID,
            value: 0.0,
        });
        normalize_off.load_model(tiny_model_with_loudness(sample_rate, 0.5, None));
        let off_out = process_signal_in_chunks(&mut normalize_off, &input);

        let on_level = rms_db(&on_out[NORMALIZE_SETTLE_SAMPLES..]);
        let off_level = rms_db(&off_out[NORMALIZE_SETTLE_SAMPLES..]);
        assert!(
            (on_level - off_level).abs() < 0.05,
            "expected a model with no declared loudness to receive zero normalisation gain \
             regardless of the enable flag, got on={on_level} off={off_level}"
        );
    }

    /// NFR-RT-010: FR-NAM-090's normalisation gain (the per-slot `GainRamp::set_target_db`/
    /// `process` calls `process_wet` now makes every block) must not allocate on the audio
    /// thread. Stereo, with a nonzero offset applied, so both the duplicate-channel shuttle and a
    /// genuinely non-unity ramp target are exercised inside the harness — mirrors
    /// `crossfade_in_progress_does_not_allocate`'s shape.
    #[test]
    fn normalization_gain_application_does_not_allocate() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Stereo);
        stage.apply(ParamChange {
            id: NORMALIZE_OFFSET_DB_ID,
            value: 3.0,
        });
        stage.load_model(tiny_model_with_loudness(sample_rate, 0.5, Some(-20.0)));

        let mut left = [0.1f32; 64];
        let mut right = [0.1f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        // A second block, still inside the ramp's settling window, so the harness also covers a
        // genuinely-moving (not-yet-converged) gain target, not just a settled one.
        let mut left = [0.1f32; 64];
        let mut right = [0.1f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));
    }

    // -------------------------------------------------------------------------------------
    // FR-NAM-060 (M9b) — the resampler's frequency response, measured in isolation.
    //
    // Nothing had ever measured it: `SlotResampler`'s own doc comment said as much, and the
    // first measurement failed. `rubato`'s FFT resampler places its antialiasing filter's
    // cutoff at a fraction of the lower Nyquist that *depends on the filter's length*, and the
    // filter's length is the FFT size, which the pre-M9b configuration let shrink to 64 frames
    // at a 192 kHz engine rate — a response 15 dB down at 20 kHz against a 0.1 dB allowance.
    // `MIN_RESAMPLE_FFT_FRAMES` (and D-9.3's "configured to meet FR-NAM-060... not by trusting
    // the library's defaults") is the answer; these tests are what makes it a measured claim.
    //
    // The instrument is `namir_fixtures::resample_response`, whose module doc comment derives
    // the coherent-sampling method and whose own tests check it against an identity converter
    // and an analytically-known one-pole low-pass. It is shared with `namir-ir`, which owes the
    // same numbers for FR-IR-030.
    // -------------------------------------------------------------------------------------

    /// The engine/model rate pairs the measurement below covers: every standard session rate
    /// from 44.1 to 192 kHz against the three rates a `.nam` file realistically declares. Both
    /// directions of each are measured, plus the round trip, so a 44.1 kHz-model row and a
    /// 44.1 kHz-engine row are not the same measurement seen twice.
    const MEASURED_RATE_PAIRS: [(u32, u32); 10] = [
        (48_000, 44_100),
        (44_100, 48_000),
        (88_200, 48_000),
        (96_000, 44_100),
        (96_000, 48_000),
        (176_400, 48_000),
        (192_000, 44_100),
        (192_000, 48_000),
        (48_000, 96_000),
        (44_100, 96_000),
    ];

    /// Streams `input` through one `rubato` resampler in exactly the fixed chunks
    /// `SlotResampler::process` feeds it, returning everything it produced. Whole chunks only:
    /// a `FftFixedInOut` accepts nothing else, which is the property D-9.2 chose it for.
    fn stream_fixed(resampler: &mut FftFixedInOut<f32>, input: &[f32]) -> Vec<f32> {
        let in_frames = resampler.input_frames_next();
        let out_frames = resampler.output_frames_next();
        let mut output = Vec::with_capacity(input.len() * 2);
        let mut chunk_out = vec![0f32; out_frames];
        for chunk in input.chunks_exact(in_frames) {
            let wave_in: [&[f32]; 1] = [chunk];
            let mut wave_out: [&mut [f32]; 1] = [&mut chunk_out[..]];
            resampler
                .process_into_buffer(&wave_in, &mut wave_out, None)
                .expect("buffers are exactly this resampler's own declared chunk sizes");
            output.extend_from_slice(&chunk_out);
        }
        output
    }

    /// Both halves of FR-NAM-060's bar, with the measured figures in the failure message so a
    /// reader sees the margin rather than a bare pass/fail. `label` names the conversion.
    fn assert_meets_fr_nam_060(label: &str, response: &ResampleResponse) {
        println!("FR-NAM-060 {label}: {}", response.summary());
        assert!(
            response.ripple_db <= 0.1,
            "FR-NAM-060 allows 0.1 dB of passband ripple; {label} measured {}",
            response.summary()
        );
        if let Some(stopband_db) = response.stopband_db {
            assert!(
                stopband_db <= -100.0,
                "FR-NAM-060 requires 100 dB of stopband attenuation; {label} measured {}",
                response.summary()
            );
        }
    }

    // trace-partial: FR-NAM-060
    // uncovered: FR-NAM-060 — measured across ten engine/model pairs drawn from the standard
    // uncovered: rates (44.1/48/88.2/96/176.4/192 kHz engine against 44.1/48/96 kHz models), not
    // uncovered: every pair `SampleRate::new` admits: it takes any nonzero u32, and where the
    // uncovered: lower of the two rates falls below about 40 kHz the requirement's own "or the
    // uncovered: Nyquist frequency, whichever is lower" clause is unsatisfiable by construction,
    // uncovered: an antialiasing filter flat to Nyquist not being able to be 100 dB down just
    // uncovered: above it, so that region needs an FRS decision rather than a test; closes M8
    #[test]
    fn resampler_frequency_response_meets_the_stopband_and_ripple_bar() {
        for (engine_hz, model_hz) in MEASURED_RATE_PAIRS {
            let engine = SampleRate::new(engine_hz).unwrap();
            let model = SampleRate::new(model_hz).unwrap();
            // Everything above the *lower* of the two Nyquists is what the pair must remove —
            // for the round trip that is the model's, which neither of its own two rates names.
            let band_edge_hz = engine_hz.min(model_hz) as f64 / 2.0;

            let into_model = measure(engine_hz, model_hz, band_edge_hz, |input| {
                let mut resampler = SlotResampler::new(engine, model, 64);
                stream_fixed(&mut resampler.into_model, input)
            });
            assert_meets_fr_nam_060(&format!("{engine_hz} Hz -> {model_hz} Hz"), &into_model);

            let out_of_model = measure(model_hz, engine_hz, band_edge_hz, |input| {
                let mut resampler = SlotResampler::new(engine, model, 64);
                stream_fixed(&mut resampler.out_of_model, input)
            });
            assert_meets_fr_nam_060(&format!("{model_hz} Hz -> {engine_hz} Hz"), &out_of_model);

            // The whole of FR-NAM-050's "resample ... and resample the result back", which is
            // what a listener actually hears and where the two halves' errors compound.
            let round_trip = measure(engine_hz, engine_hz, band_edge_hz, |input| {
                let mut resampler = SlotResampler::new(engine, model, 64);
                resampler.resample_only(input)
            });
            assert_meets_fr_nam_060(
                &format!("{engine_hz} Hz -> {model_hz} Hz -> {engine_hz} Hz"),
                &round_trip,
            );
        }
    }

    /// The measurement's calibration against a *failing* resampler, and the reason the numbers
    /// above can be believed: a measurement that has never been seen to fail proves nothing.
    ///
    /// The resampler here is the pre-M9b configuration — a flat 256-frame chunk regardless of
    /// rate — at the rate pair where it was worst, 192 kHz engine against a 48 kHz model. Its
    /// FFT is 64 frames long in the 48 kHz domain, so `rubato::calculate_cutoff` puts the
    /// passband edge at 0.789 × 24 kHz, and the harness must report the resulting hole. Note
    /// which half fails: the stopband is fine even here, and always was. Nothing about
    /// FR-NAM-060's 100 dB was ever in doubt; its 0.1 dB was.
    #[test]
    fn frequency_response_measurement_catches_an_undersized_antialias_filter() {
        let undersized = || {
            FftFixedInOut::<f32>::new(192_000, 48_000, 256, 1)
                .expect("both rates are nonzero constants")
        };
        assert_eq!(
            undersized().output_frames_next(),
            64,
            "the configuration this test exists to reject should still be the one described"
        );

        let response = measure(192_000, 48_000, 24_000.0, |input| {
            stream_fixed(&mut undersized(), input)
        });
        assert!(
            response.ripple_db > 10.0,
            "the harness should see this filter's hole at 20 kHz, but reported {}",
            response.summary()
        );
        assert!(
            response
                .stopband_db
                .expect("a down-conversion has a stopband")
                <= -100.0,
            "even undersized, this resampler's stopband was never the problem: {}",
            response.summary()
        );
    }

    /// [`resample_chunk_frames`] claims to compute the chunk length that makes `rubato` pick a
    /// particular FFT size, rather than hinting at one and letting it round — `SlotResampler`'s
    /// round-trip symmetry `debug_assert`s depend on landing exactly. Checks the claim on both
    /// counts, for every pair the measurement above covers.
    #[test]
    fn resample_chunk_frames_lands_exactly_on_rubatos_own_fft_size() {
        for (engine_hz, model_hz) in MEASURED_RATE_PAIRS {
            let resampler = SlotResampler::new(
                SampleRate::new(engine_hz).unwrap(),
                SampleRate::new(model_hz).unwrap(),
                64,
            );
            assert_eq!(
                resampler.engine_block,
                resample_chunk_frames(engine_hz as usize, model_hz as usize),
                "{engine_hz}/{model_hz}: rubato rounded the requested chunk"
            );
            assert!(
                resampler.engine_block.min(resampler.model_block) >= MIN_RESAMPLE_FFT_FRAMES,
                "{engine_hz}/{model_hz}: FFT length in the lower rate's domain is {}, below the \
                 {MIN_RESAMPLE_FFT_FRAMES} frames FR-NAM-060's passband bar needs",
                resampler.engine_block.min(resampler.model_block)
            );
        }
    }
}
