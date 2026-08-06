//! Nam stage (FR-NAM-\*): wraps `namir_nam::PreparedNam`/`NamState` with the D-8.1
//! crossfade-capable dual-resource shape, D-9.2/9.3's per-block-rate-mismatch resampling, the
//! shared per-stage bypass crossfade (FR-CHAIN-020), and FR-CHAIN-050's mono-core-then-duplicate
//! channel handling.
//!
//! # Why this holds two resource slots already, in M2
//!
//! FR-NAM-070 (glitch-free model swap) is D-8.1's four-step handover: *prepare* (worker, off this
//! stage entirely), *offer* (a command ring, M4), *crossfade* (here), *retire* (a return ring,
//! M4). Nothing in M2 drives steps 1/2/4 across a real thread yet — [`NamStage::load_model`] is
//! called directly by whoever holds a `&mut NamStage` (in M2, only this module's own tests; from
//! M4 on, a worker thread's handover). What M2 *does* build for real is step 3's mechanism: two
//! live [`NamSlot`]s and the equal-power blend between them, proven here so M4 only has to wire a
//! thread onto an already-working crossfade rather than invent one under real-time pressure.
//!
//! **Known M2 gap, not swept under the rug:** step 4 ("retire") is D-8.1's return ring, which
//! does not exist until M4 — "the audio thread pushes the old `Arc` into a return ring, and never
//! drops it [itself]" (`02-architecture.md` §8). Without that ring, *something* still has to drop
//! the outgoing slot once a handover completes, and in this M2 implementation that something is
//! `process` itself, on the audio thread, the instant `remaining` reaches zero (see
//! `NamStage::process_channel0`). Dropping a `NamSlot` frees its `NamState` scratch and may drop
//! the last `Arc<PreparedNam>` reference — a real P1 violation at that exact moment, acknowledged
//! here rather than hidden: this module's own RT-allocation tests deliberately stop short of
//! driving a handover to completion inside `rt_harness::audio_section` (they cover "crossfade in
//! progress", not "crossfade's final sample"), and its smoothness test drives a full handover
//! *without* that harness for the same reason. Closing this gap for real is M4's return ring, not
//! a fix available to this file.
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
use namir_nam::{NamState, PreparedNam};
use namir_params::ParamKind;
use namir_params::stages::nam::ENABLED;
use rubato::{FftFixedInOut, Resampler};

use crate::param::{ParamChange, ParamId};
use crate::prepare::{PrepareContext, PrepareError};
use crate::stage::{Stage, StagePrep};
use crate::stage_io::StageIo;
use crate::telemetry::{TelemetryEntry, TelemetrySink};

/// The shared per-stage bypass crossfade's one-pole time constant (FR-CHAIN-020) — same figure
/// and same rationale as `gate.rs`'s identical constant: not derived from an FRS requirement,
/// this stage's own documented choice for the shared pattern.
const BYPASS_CROSSFADE_TIME_CONSTANT_MS: f64 = 15.0;

/// FR-NAM-070: "the crossfade shall be equal-power and 5-50 ms." 20 ms is this stage's own chosen
/// point within that window, for the *handover* crossfade between `NamSlot`s — a fixed-duration
/// linear-in-`theta` fade, not a one-pole, so this is a duration in samples, not a time constant.
const HANDOVER_CROSSFADE_MS: f64 = 20.0;

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

/// Telemetry signal id: whether `slots[active]` currently holds a model (post-handover; a slot
/// that is only mid-handover-fade-in does not yet count, matching `latency_samples`'s own use of
/// `slots[active]`). Derived from a namespaced string the same way `namir-params`'s real
/// parameter ids are (this crate's shared telemetry-id convention) — a readout, not an
/// automatable parameter, so it is never added to `namir_params::REGISTRY`.
const TELEMETRY_LOADED: u32 = namir_params::ParamId::from_key("telemetry.nam.loaded").0;

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

        let tau_samples = (BYPASS_CROSSFADE_TIME_CONSTANT_MS / 1000.0) * sample_rate.hz_f64();
        let mix_coeff = (1.0 - (-1.0_f64 / tau_samples).exp()) as f32;
        let crossfade_total_samples =
            ((HANDOVER_CROSSFADE_MS / 1000.0) * sample_rate.hz_f64()).round() as u32;

        Ok(NamStage {
            sample_rate,
            max_block_size: max_block,
            slots: [None, None],
            active: 0,
            crossfade: None,
            crossfade_total_samples: crossfade_total_samples.max(1),
            enabled: enabled_default_on,
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
        })
    }
}

/// One loaded model: its immutable, shareable `Arc<PreparedNam>` (D-8.2 — shareable so a future
/// M4 process-global cache can hand the same `Arc` to every plugin instance using this model),
/// this instance's own mutable inference state, and — only when the model's declared sample rate
/// differs from the engine's — the D-9.2 resampler pair around it.
struct NamSlot {
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
}

impl NamSlot {
    /// **Not RT-safe.** Builds a fresh [`NamState`] (`PreparedNam::new_state` allocates every
    /// scratch buffer the model's inference needs) and, only when `model.sample_rate()` differs
    /// from `engine_sample_rate`, a [`SlotResampler`] (which itself allocates two `rubato`
    /// resamplers and their FIFOs). See [`NamStage::load_model`]'s doc comment for the full
    /// non-RT contract this mirrors.
    fn new(model: Arc<PreparedNam>, engine_sample_rate: SampleRate, max_block_size: usize) -> Self {
        let model_rate = model.sample_rate();
        if model_rate.hz() == engine_sample_rate.hz() {
            let state = model.new_state(max_block_size);
            Self {
                model,
                state,
                resample: None,
            }
        } else {
            let resample = SlotResampler::new(engine_sample_rate, model_rate, max_block_size);
            let state = model.new_state(resample.model_block);
            Self {
                model,
                state,
                resample: Some(resample),
            }
        }
    }

    /// Runs this slot's model (resampled around, if `resample` is `Some`) on `input`, writing
    /// exactly `input.len()` frames into `output`. RT-safe once constructed: every buffer this
    /// touches was sized in `NamSlot::new`/`SlotResampler::new`.
    fn process_wet(&mut self, input: &[f32], output: &mut [f32]) {
        match &mut self.resample {
            None => self.model.process_block(&mut self.state, input, output),
            Some(resampler) => resampler.process(&self.model, &mut self.state, input, output),
        }
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
    /// The two live resource slots D-8.1's handover shape asks for. At most one is fading out and
    /// at most one is fading in at any time (`load_model` always installs into `1 - active`).
    slots: [Option<NamSlot>; 2],
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
        let inactive = 1 - self.active;
        self.slots[inactive] = Some(NamSlot::new(model, self.sample_rate, self.max_block_size));
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
        let Some(mut crossfade) = self.crossfade else {
            // FR-CHAIN-040: `None` is a pure passthrough -- `io.channel(0)` already holds the
            // input, so there is nothing to do in that case.
            if let Some(slot) = &mut self.slots[self.active] {
                slot.process_wet(&self.dry[0][..n], io.channel(0));
            }
            return;
        };

        let outgoing_idx = self.active;
        let incoming_idx = 1 - self.active;
        match &mut self.slots[outgoing_idx] {
            Some(slot) => slot.process_wet(&self.dry[0][..n], &mut self.crossfade_outgoing[..n]),
            None => self.crossfade_outgoing[..n].copy_from_slice(&self.dry[0][..n]),
        }
        match &mut self.slots[incoming_idx] {
            Some(slot) => slot.process_wet(&self.dry[0][..n], &mut self.crossfade_incoming[..n]),
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
            // Known M2 gap (this module's doc comment): with no D-8.1 return ring yet (M4), this
            // drop runs right here, on the audio thread, at the exact instant a handover
            // completes -- not RT-pure. Every other line `process` executes is proven RT-safe by
            // this module's own tests; this one statement is the documented exception.
            self.slots[outgoing_idx] = None;
            self.active = incoming_idx;
            self.crossfade = None;
            self.recompute_mix_target();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;
    use namir_core::ChannelConfig;
    use namir_nam::{LayerArrayConfig, NamFile, NamMetadata, WaveNetConfig};

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
    fn minimal_layer_array() -> LayerArrayConfig {
        LayerArrayConfig {
            input_size: 1,
            condition_size: 1,
            head_size: 1,
            channels: 2,
            kernel_size: 2,
            dilations: vec![1],
            activation: "Tanh".to_string(),
            gated: false,
            head_bias: false,
        }
    }

    /// How many flat weights `PreparedNam::from_file` consumes for one `minimal_layer_array`,
    /// mirroring `wavenet.rs`'s own private `weight_count_for` test helper.
    fn weight_count_for(cfg: &LayerArrayConfig) -> usize {
        let mut n = cfg.channels * cfg.input_size; // rechannel, no bias
        for _ in &cfg.dilations {
            n += cfg.channels * cfg.channels * cfg.kernel_size; // dilated weight
            n += cfg.channels; // dilated bias
            n += cfg.channels * cfg.condition_size; // mixin, no bias
            n += cfg.channels * cfg.channels; // residual weight
            n += cfg.channels; // residual bias
        }
        n += cfg.head_size * cfg.channels; // head_rechannel weight
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
        // Deliberately *not* run inside `rt_harness::audio_section` here: this loop is long
        // enough (100 ms) to drive the handover all the way to completion, and completion drops
        // `slots[outgoing_idx]` -- a *real* `NamSlot` this time (unlike the first handover, from
        // nothing loaded, whose outgoing slot was already `None`), which deallocates on this very
        // call per this module's own documented M2 gap (see the crate doc comment's "Known M2
        // gap" section and `process_channel0`'s finalization comment). This test is about
        // smoothness, not RT-safety; `crossfade_in_progress_does_not_allocate` and
        // `resampled_crossfade_in_progress_does_not_allocate` below cover the RT-safety property
        // for the part of a handover this module's design actually guarantees it for.
        let total = 4800usize; // 100 ms, comfortably longer than the 20 ms handover.
        let value = 0.1f32;
        let mut prev: Option<f32> = None;
        let mut max_delta = 0.0f32;
        let mut offset = 0usize;
        while offset < total {
            let n = 64usize.min(total - offset);
            let mut buf = vec![value; n];
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, n);
            stage.process(&mut io);
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
}
