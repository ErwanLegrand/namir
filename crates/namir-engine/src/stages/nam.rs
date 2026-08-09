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
//! `mix_target` is recomputed from `slots[active]` — deliberately the *pre-handover* active slot,
//! not whichever slot is fading in — every time `enabled` changes or `active` itself changes
//! (i.e. when a handover completes, never mid-handover). One consequence worth stating plainly:
//! loading the very first model (nothing previously active) does not make the bypass blend start
//! moving until the handover crossfade itself finishes and `active` flips — the two fades compose
//! in sequence for that specific case, not in parallel. Both fades are individually smooth
//! one-pole/equal-power curves, so the composition is still click-free throughout, just not the
//! single ~20 ms fade a naive reading might expect. Loading a *replacement* model into an already
//! fully-engaged stage (`slots[active]` already `Some`, bypass blend already settled at 1.0) does
//! not have this composition effect: `mix_target` is already 1.0 and stays there, so the handover
//! crossfade's own equal-power blend is heard in full, which is the FR-NAM-070 case that matters.

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

/// D-9.2's "fixed internal block" for a resampled slot, as a desired *input* (engine-rate) chunk
/// length handed to `rubato::FftFixedInOut::new` — the constructor may round this to a different
/// exact value (see `SlotResampler::new`'s doc comment); this is a hint, not a guarantee. Chosen
/// independent of `ctx.max_block_size()` on purpose: D-9.2 wants the model's own internal block
/// size, and therefore the resampler's latency, to be a property of the *stage*, not of whatever
/// block size the host happens to be calling with this session.
const RESAMPLE_CHUNK_FRAMES: usize = 256;

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
/// # Known limitation — best-effort, not verified to D-9.3's quality bar
///
/// FR-NAM-060's stopband/ripple requirement is explicitly **out of scope for M2**
/// (`03-implementation-roadmap.md` §6: "most of §5.4 (NAM, minus... resampling-quality...)"), and
/// this implementation has not been measured against it — `FftFixedInOut`'s antialiasing filter
/// is used as configured by `rubato` itself, not tuned or verified the way D-9.3 asks for. Nor is
/// [`SlotResampler`]'s `latency_samples` field proven sample-exact: it sums both resamplers'
/// `output_delay()` (converting the first one's model-rate figure to engine-rate samples) plus
/// one `engine_block` for FIFO buffering granularity, which is the right *order of magnitude* but
/// not a value derived from a per-sample trace of the actual pipeline. What *is* verified here
/// (see this module's tests): the mismatched-rate path runs without allocating or panicking
/// across many blocks of varying size, and its FIFOs' capacities are generous relative to the
/// bound the design keeps them under (see the `engine_in_fifo`/`engine_out_fifo` field docs) —
/// not a formally proven bound, an empirical one the RT harness would catch a violation of.
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
            RESAMPLE_CHUNK_FRAMES,
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
            engine_out_fifo: VecDeque::with_capacity(fifo_capacity),
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
    /// Where `mix` is heading: `1.0` when `enabled && slots[active].is_some()`, `0.0` otherwise
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
        self.crossfade = Some(Crossfade {
            remaining: self.crossfade_total_samples,
            total: self.crossfade_total_samples,
        });
        self.recompute_mix_target();
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
        self.crossfade = Some(Crossfade {
            remaining: self.crossfade_total_samples,
            total: self.crossfade_total_samples,
        });
        self.recompute_mix_target();
    }

    /// `mix_target` is a function of exactly two inputs (`enabled`, `slots[active]`'s presence) —
    /// see the field's own doc comment for the FR-CHAIN-040 rationale and for why it is
    /// deliberately `slots[active]`, not whichever slot a handover is fading into.
    fn recompute_mix_target(&mut self) {
        self.mix_target = if self.enabled && self.slots[self.active].is_some() {
            1.0
        } else {
            0.0
        };
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
            // is mathematically complete but the retire pen is still occupied, so `active` has
            // not flipped yet. Run only the incoming slot rather than blending in an outgoing one
            // scaled by `cos(FRAC_PI_2)` — which is -4.4e-8 in f32, not exactly zero, and would
            // otherwise leave a faint copy of the old model in the output for as long as the
            // deferral lasts. Skipping it also avoids paying the 2x inference cost in a state
            // that can persist across many blocks.
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
            if self.retired.is_none() {
                // **The M2 P1 violation, closed.** This used to be `self.slots[outgoing_idx] =
                // None`, i.e. a *drop* — freeing the outgoing `NamState`'s scratch and possibly
                // the last `Arc<PreparedNam>` reference, on the audio thread, at the exact
                // instant a handover completed. `take()` *moves*: nothing is dropped here, and
                // the return ring carries the slot to a worker that can afford to free it
                // (D-8.1 step 4). Do not "simplify" this back to an assignment.
                self.retired = self.slots[outgoing_idx]
                    .take()
                    .map(|slot| Resource::nam(slot, self.prepared_for));
                self.active = incoming_idx;
                self.crossfade = None;
                self.recompute_mix_target();
            } else {
                // The pen is still occupied because the return ring was full when
                // `collect_retired` last ran — i.e. the worker is not draining (D-8.1: "If the
                // worker dies, the ring fills and memory is retained but audio continues.
                // Degradation, not failure (P8)").
                //
                // Only the *bookkeeping* is deferred; the audio is already correct. `theta` has
                // saturated at FRAC_PI_2, so the outgoing slot is multiplied by cos(pi/2) and
                // contributes nothing audible, and `process_channel0`'s own fast path above skips
                // running it at all. The stage simply stays in this state until a later block's
                // `collect_retired` empties the pen, then finalizes.
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
                resampler.engine_out_fifo.clear();
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

    // trace-partial: FR-NAM-050
    // uncovered: FR-NAM-050 — the comparison the Verify method specifies, a 48 kHz model driven
    // uncovered: at 44.1 kHz against the same model driven at 48 kHz with the input and output
    // uncovered: resampled offline to the FR-NAM-030 tolerance, is computed nowhere; the tagged
    // uncovered: test asserts only finiteness and non-allocation over 200 blocks of varying size;
    // uncovered: closes M9b
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
    // uncovered: method is not executed as stated in the general case; closes M9b
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
}
