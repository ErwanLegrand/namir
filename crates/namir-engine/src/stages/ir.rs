//! Ir stage (FR-IR-040..100): wraps `namir_ir::PreparedIr`/`IrState` with the D-8.1
//! crossfade-capable dual-resource shape (FR-IR-060/FR-IR-080), the shared per-stage bypass
//! crossfade (FR-CHAIN-020), FR-IR-070's level/low-cut/high-cut controls on the wet path, and
//! this stage's own true-stereo-width channel handling.
//!
//! # Structural mirror of `nam.rs`, three differences
//!
//! This stage is `nam.rs`'s D-8.1 dual-resource-slot shape (two live [`IrSlot`]s, an equal-power
//! [`Crossfade`] between them driven by [`IrStage::load_ir`], the same shared bypass
//! `mix`/`mix_target`/`mix_coeff` pattern) with three changes:
//!
//! 1. **No per-block resampling wrapper.** `namir_ir::PreparedIr::from_wav_bytes` resamples once
//!    at load time (FR-IR-030), so an [`IrSlot`] is just `{ ir: Arc<PreparedIr>, state: IrState }`
//!    — no `nam.rs`-style `SlotResampler`/FIFO machinery.
//! 2. **True stereo width.** Gate/Nam are mono-core (FR-CHAIN-050): they run channel 0 only and
//!    duplicate the result onto every other channel. Ir cannot do that and still honor
//!    FR-CHAIN-060's "stereo IR, or dual mono IR" — a stereo IR's two channels must reach two
//!    distinct physical output channels. So instead of a channel-0-then-duplicate shuttle, this
//!    stage always calls `PreparedIr::process_block` with exactly `ir.channel_count()` outputs
//!    (that API's own contract — see [`IrSlot::process_wet`]) and then *reads back* from those
//!    produced channels per physical output channel via [`wet_channel_index`], duplicating
//!    channel 0 when the IR has fewer channels than the chain (dual mono, FR-CHAIN-060), or
//!    dropping the IR's extra channel(s) when the chain has fewer than the IR (a stereo IR loaded
//!    into a `Mono` chain — an edge case FR-CHAIN-060 doesn't explicitly ask for, but one this
//!    stage handles rather than panics on: "just use the IR's channel 0").
//! 3. **`tail_samples()` is nonzero.** Ir is the one stage in the fixed six-stage chain whose own
//!    processing can still produce non-negligible output after its input goes silent
//!    (`chain.rs`'s own doc comment: "at most one stage with a nonzero tail"). See
//!    [`Stage::tail_samples`]'s impl below.
//!
//! Everything else — the two-fades-composed reasoning, the `mix_target` recomputation triggers,
//! and D-8.1's four-step handover including the two audio-thread drop sites M4 closed (a completing
//! handover's outgoing slot, and a displaced still-fading-in slot) — is identical in spirit to
//! `nam.rs`'s own module doc comment; read that first, this doc comment only covers what differs.
//!
//! # The handover crossfade is per physical channel, not mono-core
//!
//! Because point 2 above means this stage's wet signal is not a single channel duplicated,
//! [`IrStage::process_wet`]'s handover blend runs its equal-power `theta` recurrence
//! independently for *every* physical output channel (not once, on channel 0, the way `nam.rs`'s
//! `process_channel0` does) — but from the exact same starting `remaining` and the exact same
//! per-sample recurrence for every channel, only committing the last channel's trajectory back to
//! `self.crossfade`, for the identical in-phase reason the shared bypass blend's own per-channel
//! loop (below, and in every other bypassable stage) does the same thing with `mix`.
//!
//! # FR-IR-070 order: convolve → HP/LP → level, all on the wet path before the outer blend
//!
//! So that toggling `ir.enabled` bypasses the whole IR+filter+level chain cleanly (not just the
//! convolution), the low-cut/high-cut `Biquad`s and the level `GainRamp`s run unconditionally on
//! whatever `process_wet` produced (a real convolution, or the dry passthrough FR-CHAIN-040 uses
//! when nothing is loaded) — their own `*_enabled` toggles are independent of `ir.enabled` and
//! (like `eq.rs`'s HP/LP) are expressed as smooth coefficient interpolation to
//! `BiquadCoeffs::identity()`, not as a second dry/wet blend. The *shared* bypass blend runs last,
//! exactly as `nam.rs`'s doc comment describes for its own two composed fades.

use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

use namir_core::SampleRate;
use namir_dsp::{Biquad, BiquadCoeffs, FilterKind, GainRamp};
use namir_ir::{IrState, PreparedIr};
use namir_params::ParamKind;
use namir_params::stages::ir::{
    ENABLED, HIGH_CUT_ENABLED, HIGH_CUT_FREQ_HZ, LEVEL_DB, LOW_CUT_ENABLED, LOW_CUT_FREQ_HZ,
};

use crate::command::RetireSink;
use crate::param::{ParamChange, ParamId};
use crate::prepare::{PrepareContext, PrepareError};
use crate::resource::{Resource, ResourceKind};
use crate::stage::{Stage, StagePrep};
use crate::stage_io::StageIo;
use crate::stages::HANDOVER_CROSSFADE_MS;
use crate::telemetry::{TelemetryEntry, TelemetrySink};

/// The shared per-stage bypass crossfade's one-pole time constant (FR-CHAIN-020) — same figure
/// and rationale as `nam.rs`'s/`gate.rs`'s identical constant.
const BYPASS_CROSSFADE_TIME_CONSTANT_MS: f64 = 15.0;

/// `namir_dsp::GainRamp`'s one-pole time constant for FR-IR-070's level control. Same figure as
/// `out.rs`'s identical constant, reproduced here since `GainRamp`'s public API imposes no
/// default of its own — see `gain_ramp.rs`'s own doc comment for the FR-PARAM-040 derivation.
const LEVEL_RAMP_TIME_CONSTANT_MS: f32 = 25.0;

/// Butterworth-flat (maximally-flat magnitude) `Q` for the defeatable low-cut/high-cut corners:
/// neither has a dedicated `Q` parameter, same reasoning and same value as `eq.rs`'s identical
/// `HIGH_PASS_LOW_PASS_Q` constant for its own HP/LP pair.
const LOW_CUT_HIGH_CUT_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// This stage's RT-facing `namir_engine::ParamId`s, converted once from `namir_params`'s own ids
/// for the same keys — see `trim.rs`'s identical convention and its doc comment for why the two
/// crates carry distinct `ParamId` types on purpose.
const ENABLED_ID: ParamId = ParamId(ENABLED.id.0);
/// See [`ENABLED_ID`].
const LEVEL_DB_ID: ParamId = ParamId(LEVEL_DB.id.0);
/// See [`ENABLED_ID`].
const LOW_CUT_ENABLED_ID: ParamId = ParamId(LOW_CUT_ENABLED.id.0);
/// See [`ENABLED_ID`].
const LOW_CUT_FREQ_HZ_ID: ParamId = ParamId(LOW_CUT_FREQ_HZ.id.0);
/// See [`ENABLED_ID`].
const HIGH_CUT_ENABLED_ID: ParamId = ParamId(HIGH_CUT_ENABLED.id.0);
/// See [`ENABLED_ID`].
const HIGH_CUT_FREQ_HZ_ID: ParamId = ParamId(HIGH_CUT_FREQ_HZ.id.0);

/// Telemetry signal id: whether `slots[active]` currently holds an IR (post-handover; see
/// `nam.rs`'s identical `TELEMETRY_LOADED` for why this is deliberately `slots[active]`, not
/// whichever slot a handover is fading into). Derived the same namespaced-string way every other
/// stage's telemetry ids are — a readout, never added to `namir_params::REGISTRY`.
const TELEMETRY_LOADED: u32 = namir_params::ParamId::from_key("telemetry.ir.loaded").0;

/// Telemetry signal id: whether a handover crossfade is currently in flight. Same purpose and
/// readout-not-parameter convention as `nam.rs`'s identical id, so `params.lock` is unaffected.
const TELEMETRY_HANDOVER_ACTIVE: u32 =
    namir_params::ParamId::from_key("telemetry.ir.handover_active").0;

/// Reads a `Continuous` descriptor's default; panicking arm is defensive-only (unreachable from
/// any input `prepare` is passed) — matches `eq.rs`'s/`gate.rs`'s identical helper.
fn continuous_default(descriptor: namir_params::ParamDescriptor) -> f32 {
    match descriptor.kind {
        ParamKind::Continuous { default, .. } => default,
        ParamKind::Stepped { .. } => unreachable!("{} is declared Continuous", descriptor.key),
    }
}

/// Reads a `Stepped` descriptor's default as "index 1 (On) selected" — matches `eq.rs`'s/
/// `nam.rs`'s identical helper and `ParamChange`'s own stepped-value-is-the-index convention.
fn stepped_default_on(descriptor: namir_params::ParamDescriptor) -> bool {
    match descriptor.kind {
        ParamKind::Stepped { default_index, .. } => default_index.0 == 1,
        ParamKind::Continuous { .. } => unreachable!("{} is declared Stepped", descriptor.key),
    }
}

/// Which produced-channel index (into an [`IrSlot`]'s up-to-two wet output channels) physical
/// output channel `ch` should read from, given that slot's IR produced exactly `produced_channels`
/// channels this block. Duplicates channel 0 when the chain has more physical channels than the
/// IR provides (dual mono, FR-CHAIN-060); drops the IR's extra channel(s) when the IR provides
/// more than the chain has (a stereo IR into a `Mono` chain — "just use the IR's channel 0", per
/// this module's doc comment). `produced_channels` is always `>= 1` (an `IrSlot`'s IR reports 1
/// or 2 channels; a `None` slot's dry passthrough is treated as `1`), so this never underflows.
fn wet_channel_index(ch: usize, produced_channels: usize) -> usize {
    ch.min(produced_channels - 1)
}

/// Builds [`IrStage`]. Holds no configuration of its own — every one of Ir's six parameters seeds
/// its initial value straight from its `namir-params` descriptor (see `prepare`'s body), and no
/// IR is loaded at construction (`slots` starts `[None, None]`, FR-CHAIN-040's "nothing loaded
/// behaves as bypassed").
pub struct IrPrep;

impl StagePrep for IrPrep {
    type Prepared = IrStage;

    /// Sizes every buffer `IrStage::process` will ever touch: the per-channel dry scratch the
    /// bypass crossfade needs, the two (fixed-capacity-2, since an IR is mono or stereo only)
    /// handover-fade wet scratch buffers, and one `Biquad` HP/LP pair plus one `GainRamp` per
    /// physical output channel for FR-IR-070 — all sized to `ctx.max_block_size()`/
    /// `ctx.channel_config().output_channels()`, never resized in `process`. Does **not**
    /// allocate anything slot-shaped: no IR is loaded yet, and everything a loaded slot needs
    /// (`IrState`'s ring buffers/accumulators) is [`IrStage::load_ir`]'s job, deliberately
    /// deferred to that explicitly-non-RT path — same split `nam.rs`'s `NamPrep`/`load_model`
    /// makes for the identical reason.
    fn prepare(&self, ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError> {
        let sample_rate = ctx.sample_rate();
        let max_block = ctx.max_block_size();
        let channel_count = ctx.channel_config().output_channels() as usize;

        let enabled_default_on = stepped_default_on(ENABLED);
        let level_db_default = continuous_default(LEVEL_DB);
        let low_cut_enabled_default = stepped_default_on(LOW_CUT_ENABLED);
        let low_cut_freq_hz_default = continuous_default(LOW_CUT_FREQ_HZ);
        let high_cut_enabled_default = stepped_default_on(HIGH_CUT_ENABLED);
        let high_cut_freq_hz_default = continuous_default(HIGH_CUT_FREQ_HZ);

        let tau_samples = (BYPASS_CROSSFADE_TIME_CONSTANT_MS / 1000.0) * sample_rate.hz_f64();
        let mix_coeff = (1.0 - (-1.0_f64 / tau_samples).exp()) as f32;
        let crossfade_total_samples =
            ((HANDOVER_CROSSFADE_MS / 1000.0) * sample_rate.hz_f64()).round() as u32;

        let mut level = Vec::with_capacity(channel_count);
        for _ in 0..channel_count {
            let mut ramp = GainRamp::new(sample_rate, LEVEL_RAMP_TIME_CONSTANT_MS);
            ramp.set_target_db(level_db_default);
            level.push(ramp);
        }

        let mut stage = IrStage {
            sample_rate,
            max_block_size: max_block,
            slots: [None, None],
            active: 0,
            crossfade: None,
            crossfade_total_samples: crossfade_total_samples.max(1),
            enabled: enabled_default_on,
            // FR-CHAIN-040: nothing loaded behaves as bypassed, regardless of `enabled` -- no
            // prior audio exists yet at stage creation either, so `mix` starts already settled at
            // its target rather than needing to ramp there.
            mix: 0.0,
            mix_target: 0.0,
            mix_coeff,
            dry: vec![vec![0.0; max_block]; channel_count],
            crossfade_outgoing: [vec![0.0; max_block], vec![0.0; max_block]],
            crossfade_incoming: [vec![0.0; max_block], vec![0.0; max_block]],
            prepared_for: *ctx,
            retired: None,
            level_db: level_db_default,
            low_cut_enabled: low_cut_enabled_default,
            low_cut_freq_hz: low_cut_freq_hz_default,
            high_cut_enabled: high_cut_enabled_default,
            high_cut_freq_hz: high_cut_freq_hz_default,
            low_cut: (0..channel_count).map(|_| Biquad::new()).collect(),
            high_cut: (0..channel_count).map(|_| Biquad::new()).collect(),
            level,
        };

        // Jump (ramp_samples = 0) both filters straight to their descriptor-default target on
        // every channel: no prior audio exists yet at stage construction, so there is nothing to
        // click against.
        let low_cut_target = stage.low_cut_target();
        let high_cut_target = stage.high_cut_target();
        for f in &mut stage.low_cut {
            f.set_coeffs(low_cut_target, 0);
        }
        for f in &mut stage.high_cut {
            f.set_coeffs(high_cut_target, 0);
        }

        Ok(stage)
    }
}

/// One loaded IR: its immutable, shareable `Arc<PreparedIr>` (D-8.2 — shareable so a future M4
/// process-global resource cache can hand the same `Arc` to every plugin instance loading the
/// same file, FR-CLAP-090) and this instance's own mutable convolution state. Unlike `nam.rs`'s
/// `NamSlot`, there is no resampler here: `PreparedIr::from_wav_bytes` already resampled to the
/// engine rate once, at load time (see this module's doc comment).
pub(crate) struct IrSlot {
    /// Immutable per-partition FFT machinery (D-9.1); cheap to clone (`Arc`) into a future cache
    /// or a crossfaded-out slot's replacement.
    ir: Arc<PreparedIr>,
    /// This instance's own ring buffers/input accumulators/stream-time counter. Sized (via
    /// `PreparedIr::new_state`) to exactly what `ir` needs.
    state: IrState,
}

impl IrSlot {
    /// **Not RT-safe. This is D-8.1 step 1, and from M4 on it runs on a worker thread** —
    /// `PreparedIr::new_state` allocates every per-channel ring buffer/accumulator the convolution
    /// needs. `pub(crate)` so [`crate::Command::load_ir`] can do this work off the audio thread,
    /// mirroring `NamSlot::new`'s identical contract and rationale.
    pub(crate) fn new(ir: Arc<PreparedIr>) -> Self {
        let state = ir.new_state();
        Self { ir, state }
    }

    /// `1` for a mono IR, `2` for a stereo IR — see [`PreparedIr::channel_count`].
    fn channel_count(&self) -> usize {
        self.ir.channel_count()
    }

    /// This slot's `tail_samples()` contribution: the loaded IR's own tap count at the engine
    /// rate, post-truncation. `PreparedIr::len_samples` is bounded by D-9.7's 10-second-at-
    /// engine-rate ceiling (at most `10 * 192_000 = 1_920_000` taps at the widest supported
    /// engine rate), far below `u32::MAX`, so this cast never truncates in practice.
    fn tail_samples(&self) -> u32 {
        self.ir.len_samples() as u32
    }

    /// Runs this slot's convolution on `mono_input`, writing exactly `ir.channel_count()`
    /// channels of `mono_input.len()` frames into the front of `wet` (`wet[..channel_count()]`,
    /// each sliced to `n`). `wet` is fixed-capacity-2 scratch owned by [`IrStage`] (an IR is mono
    /// or stereo only, so 2 channels of scratch always suffices) — see this struct's doc comment
    /// for why there is no resampling step here, unlike `nam.rs`'s `NamSlot::process_wet`.
    ///
    /// RT-safe once constructed: every buffer this touches was sized in `IrSlot::new`/`wet`'s own
    /// allocation in `IrPrep::prepare`. Building the `[&mut [f32]; 2]` array below is a stack
    /// construction, not a heap one (unlike a `Vec<&mut [f32]>` would be) — required so this
    /// stays allocation-free despite `PreparedIr::process_block`'s API wanting a slice of
    /// dynamically-many output channels.
    fn process_wet(&mut self, mono_input: &[f32], wet: &mut [Vec<f32>; 2], n: usize) {
        let ir_channels = self.ir.channel_count();
        let (w0, w1) = wet.split_at_mut(1);
        let mut outs: [&mut [f32]; 2] = [&mut w0[0][..n], &mut w1[0][..n]];
        self.ir
            .process_block(&mut self.state, mono_input, &mut outs[..ir_channels]);
    }
}

/// The handover crossfade's progress (FR-IR-060/FR-NAM-070): a fixed-duration equal-power fade
/// between `slots[active]` (fading out) and `slots[1 - active]` (fading in). `Copy` for the same
/// reason as `nam.rs`'s identical type: `IrStage::process_wet` reads this out of `self.crossfade`,
/// mutates a local copy across a whole block (here, across every physical channel — see this
/// module's doc comment), and writes it back (or clears it) once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Crossfade {
    /// Samples left until this handover completes.
    remaining: u32,
    /// The fade's total duration in samples, fixed at construction
    /// (`IrStage::crossfade_total_samples`, from [`HANDOVER_CROSSFADE_MS`]).
    total: u32,
}

/// RT-safe Ir stage: up to two [`IrSlot`]s, equal-power-crossfaded between per FR-IR-060/
/// FR-IR-080's handover protocol (D-8.1's shape, mirroring `nam.rs`), each physical channel run
/// independently rather than mono-core-then-duplicate (this module's doc comment), behind the
/// shared click-free per-stage bypass crossfade (FR-CHAIN-020) and FR-IR-070's low-cut/high-cut/
/// level controls.
pub struct IrStage {
    /// Needed by `low_cut_target`/`high_cut_target` to redesign a filter's coefficients from its
    /// (possibly just-changed) parameter fields.
    sample_rate: SampleRate,
    /// D-9.9's ramp length for any low-cut/high-cut coefficient retarget `apply` triggers, and the
    /// block-size ceiling every buffer was sized to in `prepare`. `apply` has no `io` to read an
    /// actual block's frame count from (it runs off `Chain::apply`, not `process`), so this is
    /// used as a safe upper bound — same reasoning as `eq.rs`'s identical field.
    max_block_size: usize,
    /// The two live resource slots D-8.1's handover shape asks for. At most one is fading out and
    /// at most one is fading in at any time (an install always goes into `1 - active`).
    ///
    /// Boxed since M4, for the reason `nam.rs`'s identical field documents: the slot travels to
    /// and from a worker through a preallocated ring, and boxing keeps that ring's element a
    /// pointer rather than the whole slot.
    slots: [Option<Box<IrSlot>>; 2],
    /// Index into `slots` of the slot that is live outside of an in-flight handover — and, during
    /// one, the slot that is fading *out* (see `nam.rs`'s doc comment for why `active` itself only
    /// updates once the handover completes, never mid-fade).
    active: usize,
    /// `Some` while a handover between `slots[active]` and `slots[1 - active]` is in progress.
    crossfade: Option<Crossfade>,
    /// [`HANDOVER_CROSSFADE_MS`] converted to samples once, in `prepare`.
    crossfade_total_samples: u32,
    /// FR-CHAIN-020's per-stage enable/disable for this stage, independent of whether any IR is
    /// loaded (`mix_target`'s own doc comment covers how the two combine, identically to `nam.rs`).
    enabled: bool,
    /// Current dry/wet blend for the *shared* bypass crossfade: `0.0` = fully dry/bypassed,
    /// `1.0` = fully wet/engaged.
    mix: f32,
    /// Where `mix` is heading: `1.0` when `enabled && slots[active].is_some()`, `0.0` otherwise
    /// (FR-CHAIN-040). Recomputed by `apply`, `load_ir`, and by `process_wet` itself right after a
    /// handover completes and `active` changes.
    mix_target: f32,
    /// One-pole coefficient for the `mix` crossfade, computed once in `prepare` from
    /// [`BYPASS_CROSSFADE_TIME_CONSTANT_MS`] and the sample rate.
    mix_coeff: f32,
    /// Per-physical-channel pre-stage signal, captured at the top of every `process` call — both
    /// the shared bypass blend's dry reference *and*, `dry[0]`, the wet path's own mono input
    /// (every channel is identical entering this stage, per the chain's own invariant, so reusing
    /// channel 0 rather than a second copy is correct, not merely convenient).
    dry: Vec<Vec<f32>>,
    /// Handover-fade scratch: `slots[active]`'s (fading-out) wet output for the current block, one
    /// `Vec` per potential IR channel (fixed capacity 2 — an IR is mono or stereo only, this
    /// module's doc comment) — or a dry passthrough copy of the mono input in `[0]` when that
    /// slot is `None` (`nam.rs`'s doc comment: "a slot that is `None` inside a crossfade
    /// contributes its input directly").
    crossfade_outgoing: [Vec<f32>; 2],
    /// Handover-fade scratch: `slots[1 - active]`'s (fading-in) wet output for the current block,
    /// symmetric to `crossfade_outgoing`.
    crossfade_incoming: [Vec<f32>; 2],
    /// The whole `PrepareContext` this stage was built against, so an incoming offer's own context
    /// can be checked rather than trusted. This matters more here than in `nam.rs`:
    /// `PreparedIr::process_block` **asserts** the block it is given is no longer than the one its
    /// partition schedule was built for, so installing an IR prepared at a different block size
    /// would panic on the audio thread rather than merely sound wrong.
    prepared_for: PrepareContext,
    /// D-8.1 step 4's holding pen — capacity one, for the reasons `nam.rs`'s identical field
    /// documents in full.
    retired: Option<Resource>,
    /// FR-IR-070 level, dB. Tracked alongside each channel's `GainRamp` target so `apply` doesn't
    /// need to re-derive it.
    level_db: f32,
    /// FR-IR-070's defeatable low-cut "on" state.
    low_cut_enabled: bool,
    /// FR-IR-070 low-cut corner, Hz.
    low_cut_freq_hz: f32,
    /// FR-IR-070's defeatable high-cut "on" state.
    high_cut_enabled: bool,
    /// FR-IR-070 high-cut corner, Hz.
    high_cut_freq_hz: f32,
    /// One independent `Biquad` (own `s1`/`s2` state) per physical output channel, all sharing the
    /// same coefficient *target* (`low_cut_target`, retargeted together by `retarget_low_cut`) —
    /// same per-channel-state-shared-target split as `eq.rs`'s bands. A high-pass, despite the
    /// field name matching FR-IR-070's "low cut" control naming (removes energy *below* the
    /// corner).
    low_cut: Vec<Biquad>,
    /// See [`IrStage::low_cut`]. A low-pass, matching FR-IR-070's "high cut" control naming
    /// (removes energy *above* the corner).
    high_cut: Vec<Biquad>,
    /// FR-IR-070 level, one `GainRamp` per physical output channel, all sharing the same target
    /// (set together in `apply`) but each keeping its own smoothing state — identical split to
    /// `out.rs`'s `ramps` field, for the identical reason.
    level: Vec<GainRamp>,
}

impl IrStage {
    /// Installs `ir` into the currently-inactive slot and begins a [`HANDOVER_CROSSFADE_MS`]-long
    /// equal-power fade into it (FR-IR-060/FR-IR-080; D-8.1 step 3 — the real cross-thread offer/
    /// retire wiring around this call is M4's job, exactly as `nam.rs`'s `load_model` documents).
    ///
    /// **Not RT-safe.** Builds a new [`IrSlot`] (`PreparedIr::new_state` allocates every ring
    /// buffer/accumulator the convolution needs). Must never be called from `Stage::process`.
    ///
    /// If a handover is already in progress when this is called, the slot currently fading *in*
    /// (`slots[1 - active]`) is replaced outright and the fade restarts at full duration —
    /// `active` is unaffected, matching `nam.rs`'s `load_model`'s identical rule.
    pub fn load_ir(&mut self, ir: Arc<PreparedIr>) {
        let ctx = self.prepared_for;
        self.install(Box::new(IrSlot::new(ir)), ctx);
    }

    /// **RT-safe.** Installs an already-built slot and starts the handover fade — D-8.1 step 3's
    /// entry point, mirroring `nam.rs`'s `install` exactly, including the rule that a displaced
    /// slot is **parked, never dropped**. See that method's doc comment for the full rationale.
    pub(crate) fn install(&mut self, slot: Box<IrSlot>, ctx: PrepareContext) -> Option<Resource> {
        if self.retired.is_some() {
            debug_assert!(
                false,
                "install with a retirement still parked: the engine's drain gate should \
                 have held this offer back"
            );
            return Some(Resource::ir(slot, ctx));
        }
        let inactive = 1 - self.active;
        if let Some(displaced) = self.slots[inactive].take() {
            // A move, not a drop.
            self.retired = Some(Resource::ir(displaced, self.prepared_for));
        }
        self.slots[inactive] = Some(slot);
        self.crossfade = Some(Crossfade {
            remaining: self.crossfade_total_samples,
            total: self.crossfade_total_samples,
        });
        self.recompute_mix_target();
        None
    }

    /// **RT-safe.** FR-STATE-070's "the state shall load with that stage empty", mirroring
    /// `nam.rs`'s `unload` exactly — see that method's doc comment for the full rationale.
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
            self.retired = Some(Resource::ir(displaced, self.prepared_for));
        }
        self.crossfade = Some(Crossfade {
            remaining: self.crossfade_total_samples,
            total: self.crossfade_total_samples,
        });
        self.recompute_mix_target();
    }

    /// `mix_target` is a function of exactly two inputs (`enabled`, `slots[active]`'s presence) —
    /// identical rule to `nam.rs`'s `recompute_mix_target`.
    fn recompute_mix_target(&mut self) {
        self.mix_target = if self.enabled && self.slots[self.active].is_some() {
            1.0
        } else {
            0.0
        };
    }

    /// The low-cut (high-pass) coefficient target right now: [`BiquadCoeffs::identity`] when off,
    /// the designed filter otherwise. Pure — does not touch `low_cut`; see `retarget_low_cut`.
    fn low_cut_target(&self) -> BiquadCoeffs {
        if self.low_cut_enabled {
            BiquadCoeffs::design(
                FilterKind::HighPass,
                self.low_cut_freq_hz as f64,
                LOW_CUT_HIGH_CUT_Q,
                0.0,
                self.sample_rate,
            )
        } else {
            BiquadCoeffs::identity()
        }
    }

    /// The high-cut (low-pass) coefficient target right now; see [`IrStage::low_cut_target`].
    fn high_cut_target(&self) -> BiquadCoeffs {
        if self.high_cut_enabled {
            BiquadCoeffs::design(
                FilterKind::LowPass,
                self.high_cut_freq_hz as f64,
                LOW_CUT_HIGH_CUT_Q,
                0.0,
                self.sample_rate,
            )
        } else {
            BiquadCoeffs::identity()
        }
    }

    /// Recomputes the low-cut target and starts every channel's `Biquad` ramping towards it over
    /// `max_block_size` samples (D-9.9) — same pattern as `eq.rs`'s `retarget`.
    fn retarget_low_cut(&mut self) {
        let target = self.low_cut_target();
        let ramp_samples = self.max_block_size as u32;
        for f in &mut self.low_cut {
            f.set_coeffs(target, ramp_samples);
        }
    }

    /// See [`IrStage::retarget_low_cut`].
    fn retarget_high_cut(&mut self) {
        let target = self.high_cut_target();
        let ramp_samples = self.max_block_size as u32;
        for f in &mut self.high_cut {
            f.set_coeffs(target, ramp_samples);
        }
    }

    /// The wet path (this module's doc comment): writes this block's convolved (or, with nothing
    /// loaded, dry-passthrough) result into every physical channel of `io`, reading mono input
    /// from `self.dry[0]` (already captured by `process` before this is called). Handles the same
    /// three shapes `nam.rs`'s `process_channel0` does — no handover (single slot, or pure
    /// passthrough if `slots[active]` is `None`); a handover in progress (equal-power blend, per
    /// physical channel, of both slots' wet output — see [`wet_channel_index`] for how a channel-
    /// count mismatch between the two slots, or between a slot and the chain, is resolved); and a
    /// handover completing partway through this block — but generalizes the blend itself to run
    /// once per physical channel rather than once on channel 0, per this module's doc comment.
    fn process_wet(&mut self, io: &mut StageIo<'_>, n: usize) {
        let physical_channels = io.channel_count();

        let Some(mut crossfade) = self.crossfade else {
            // FR-CHAIN-040: `None` active slot is a pure passthrough -- `io` already holds the
            // dry input (identical on every channel entering this stage), so there is nothing to
            // overwrite in that case.
            if let Some(slot) = &mut self.slots[self.active] {
                let produced = slot.channel_count();
                slot.process_wet(&self.dry[0][..n], &mut self.crossfade_outgoing, n);
                for ch in 0..physical_channels {
                    let idx = wet_channel_index(ch, produced);
                    io.channel(ch)
                        .copy_from_slice(&self.crossfade_outgoing[idx][..n]);
                }
            }
            return;
        };

        let outgoing_idx = self.active;
        let incoming_idx = 1 - self.active;

        if crossfade.remaining == 0 {
            // Deferred-finalization state -- see the finalization block below and `nam.rs`'s
            // identical fast path. The fade is mathematically complete but the retire pen is
            // still occupied, so run only the incoming slot rather than blending in an outgoing
            // one scaled by `cos(FRAC_PI_2)` (-4.4e-8 in f32, not exactly zero) and paying its
            // convolution cost for as long as the deferral lasts.
            if let Some(slot) = &mut self.slots[incoming_idx] {
                let produced = slot.channel_count();
                slot.process_wet(&self.dry[0][..n], &mut self.crossfade_incoming, n);
                for ch in 0..physical_channels {
                    let idx = wet_channel_index(ch, produced);
                    io.channel(ch)
                        .copy_from_slice(&self.crossfade_incoming[idx][..n]);
                }
            }
            return;
        }

        let outgoing_channels = match &mut self.slots[outgoing_idx] {
            Some(slot) => {
                let produced = slot.channel_count();
                slot.process_wet(&self.dry[0][..n], &mut self.crossfade_outgoing, n);
                produced
            }
            None => {
                self.crossfade_outgoing[0][..n].copy_from_slice(&self.dry[0][..n]);
                1
            }
        };
        let incoming_channels = match &mut self.slots[incoming_idx] {
            Some(slot) => {
                let produced = slot.channel_count();
                slot.process_wet(&self.dry[0][..n], &mut self.crossfade_incoming, n);
                produced
            }
            None => {
                self.crossfade_incoming[0][..n].copy_from_slice(&self.dry[0][..n]);
                1
            }
        };

        // Per-physical-channel equal-power blend (this module's doc comment: unlike `nam.rs`'s
        // mono-core version of this same loop, every physical channel needs its own theta
        // recurrence here since Ir's wet signal genuinely differs per channel). Every channel
        // recomputes from the same `start_remaining` and the same per-sample recurrence, so every
        // channel's fade stays in phase; only the last channel's trajectory is committed back to
        // `crossfade.remaining`.
        let total = crossfade.total.max(1);
        let start_remaining = crossfade.remaining;
        let last_ch = physical_channels - 1;
        for ch in 0..physical_channels {
            let o_idx = wet_channel_index(ch, outgoing_channels);
            let i_idx = wet_channel_index(ch, incoming_channels);
            let mut remaining = start_remaining;
            let out = io.channel(ch);
            for ((o, &outgoing), &incoming) in out
                .iter_mut()
                .zip(self.crossfade_outgoing[o_idx][..n].iter())
                .zip(self.crossfade_incoming[i_idx][..n].iter())
            {
                let progress = (total - remaining).min(total);
                let theta = (progress as f32 / total as f32) * FRAC_PI_2;
                *o = outgoing * theta.cos() + incoming * theta.sin();
                remaining = remaining.saturating_sub(1);
            }
            if ch == last_ch {
                crossfade.remaining = remaining;
            }
        }

        if crossfade.remaining == 0 {
            if self.retired.is_none() {
                // **The M2 P1 violation, closed** -- identical change and identical reasoning to
                // `nam.rs`'s finalization block; read that one for the full note. This used to be
                // `self.slots[outgoing_idx] = None`, a drop of the outgoing `IrState`'s
                // convolution ring buffers (and possibly the last `Arc<PreparedIr>`) on the audio
                // thread. `take()` moves instead. Do not "simplify" this back to an assignment.
                self.retired = self.slots[outgoing_idx]
                    .take()
                    .map(|slot| Resource::ir(slot, self.prepared_for));
                self.active = incoming_idx;
                self.crossfade = None;
                self.recompute_mix_target();
            } else {
                // Return ring full, worker not draining: defer the bookkeeping only. The audio is
                // already correct (the fast path above runs the incoming slot alone). D-8.1's
                // "degradation, not failure (P8)". Dropping the outgoing slot here to make
                // progress is the exact bug this milestone removes.
                self.crossfade = Some(crossfade);
            }
        } else {
            self.crossfade = Some(crossfade);
        }
    }
}

impl Stage for IrStage {
    fn process(&mut self, io: &mut StageIo<'_>) {
        let n = io.frames();
        let channel_count = io.channel_count();

        // Capture dry input for every channel -- for the shared bypass blend below, and (channel
        // 0 only) as the wet path's own mono input, per `process_wet`'s doc comment.
        for ch in 0..channel_count {
            self.dry[ch][..n].copy_from_slice(io.channel(ch));
        }

        self.process_wet(io, n);

        // FR-IR-070: low-cut -> high-cut -> level, all on the wet path, before the outer dry/wet
        // bypass blend (this module's doc comment).
        for ch in 0..channel_count {
            let buf = io.channel(ch);
            self.low_cut[ch].process(buf);
            self.high_cut[ch].process(buf);
            self.level[ch].process(buf);
        }

        // Shared per-stage bypass crossfade (FR-CHAIN-020) -- identical pattern to `nam.rs`/
        // `gate.rs`: same `start_mix` and per-sample recurrence for every channel, recomputed per
        // channel rather than carried over between channels, so every channel's fade stays in
        // phase; only the last channel's trajectory is committed back to `self.mix`.
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
        // RT-safe-resettable state only (mirrors `nam.rs`'s/`eq.rs`'s identical scoping decision):
        // each `Biquad`'s TDF-II state registers reset without allocating or touching coefficients
        // or any in-progress ramp; `GainRamp` deliberately keeps its current smoothed value (same
        // reasoning as `out.rs`'s identical treatment -- a reset is a transport stop/reposition,
        // not a parameter change). Known gap, not silently worked around: `IrState`'s own ring
        // buffers/accumulators have no public reset (`namir-ir`'s own scope), mirroring `nam.rs`'s
        // identical documented gap for `NamState` -- the only way to clear that history is a fresh
        // `new_state`, which allocates and is therefore not callable from here.
        for f in &mut self.low_cut {
            f.reset();
        }
        for f in &mut self.high_cut {
            f.reset();
        }
    }

    fn latency_samples(&self) -> u32 {
        // D-9.4/FR-IR-040: `PreparedIr::latency_samples()` is always 0 by construction (the head
        // partition equals the host block size), and unlike `nam.rs` there is no per-block
        // resampler here to add any (this module's doc comment) -- so this is unconditionally 0,
        // not read from `slots[active]` the way `nam.rs`'s equivalent is.
        0
    }

    fn tail_samples(&self) -> u32 {
        // Unlike every other stage in the fixed six-stage chain (which return 0), Ir is
        // `chain.rs`'s documented exception: the active slot's own IR length is exactly how many
        // samples of non-negligible output this stage can still produce after its input goes
        // silent. `slots[active]`, not whichever slot a handover is fading into, for the same
        // "post-handover, not mid-fade" reasoning `nam.rs`'s `latency_samples` documents.
        self.slots[self.active]
            .as_ref()
            .map_or(0, |slot| slot.tail_samples())
    }

    fn apply(&mut self, change: ParamChange) {
        if change.id == ENABLED_ID {
            // Stepped param value is the index as f32 (`ParamChange`'s own doc comment); index 1
            // is "On" per `ENABLED`'s descriptor.
            self.enabled = change.value >= 0.5;
            self.recompute_mix_target();
        } else if change.id == LEVEL_DB_ID {
            self.level_db = change.value;
            // Share the *target value* across every channel's ramp, not the ramp instance itself
            // (`out.rs`'s identical convention for its own per-channel gain ramps).
            for ramp in &mut self.level {
                ramp.set_target_db(change.value);
            }
        } else if change.id == LOW_CUT_ENABLED_ID {
            self.low_cut_enabled = change.value >= 0.5;
            self.retarget_low_cut();
        } else if change.id == LOW_CUT_FREQ_HZ_ID {
            self.low_cut_freq_hz = change.value;
            self.retarget_low_cut();
        } else if change.id == HIGH_CUT_ENABLED_ID {
            self.high_cut_enabled = change.value >= 0.5;
            self.retarget_high_cut();
        } else if change.id == HIGH_CUT_FREQ_HZ_ID {
            self.high_cut_freq_hz = change.value;
            self.retarget_high_cut();
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

    /// D-8.1 step 2, mirroring `nam.rs`'s `accept_resource` exactly (including the
    /// context-mismatch-is-retired-not-installed rule -- which matters more here, since
    /// `PreparedIr::process_block` asserts on an over-long block).
    fn accept_resource(&mut self, offer: &mut Option<Resource>) {
        let Some((slot, ctx)) = Resource::take_ir(offer) else {
            return;
        };
        if ctx != self.prepared_for {
            debug_assert!(
                self.retired.is_none(),
                "the engine's drain gate should have held this offer back"
            );
            self.retired = Some(Resource::ir(slot, ctx));
            return;
        }
        let ctx = self.prepared_for;
        if let Some(refused) = self.install(slot, ctx) {
            *offer = Some(refused);
        }
    }

    /// D-8.1 step 4. Moves a parked slot into the return ring, or keeps holding it if the ring is
    /// full -- never drops it.
    fn collect_retired(&mut self, out: &mut RetireSink<'_>) {
        if let Some(resource) = self.retired.take()
            && let Err(back) = out.push(resource)
        {
            self.retired = Some(back);
        }
    }

    /// M5: FR-STATE-070's "the state shall load with that stage empty", entry point. Mirrors
    /// `nam.rs`'s identical override.
    fn unload_resource(&mut self, kind: ResourceKind) {
        if kind == ResourceKind::Ir {
            self.unload();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;
    use namir_core::{ChannelConfig, db_to_linear, linear_to_db};

    fn ctx(sample_rate_hz: u32, channel_config: ChannelConfig) -> PrepareContext {
        PrepareContext::new(SampleRate::new(sample_rate_hz).unwrap(), 64, channel_config).unwrap()
    }

    fn stage(sample_rate_hz: u32, channel_config: ChannelConfig) -> IrStage {
        IrPrep
            .prepare(&ctx(sample_rate_hz, channel_config))
            .unwrap()
    }

    /// Writes a small in-memory mono WAV via `hound::WavWriter`, the same pattern
    /// `namir-ir`'s own `convolver.rs` test module uses (not reusable directly -- that helper is
    /// private to that crate).
    fn write_mono_wav(sample_rate: u32, samples: &[f32]) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut buf = Vec::new();
        {
            let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
            for &s in samples {
                writer.write_sample(s).unwrap();
            }
            writer.finalize().unwrap();
        }
        buf
    }

    /// See [`write_mono_wav`]; the stereo counterpart.
    fn write_stereo_wav(sample_rate: u32, left: &[f32], right: &[f32]) -> Vec<u8> {
        assert_eq!(left.len(), right.len());
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut buf = Vec::new();
        {
            let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
            for (&l, &r) in left.iter().zip(right.iter()) {
                writer.write_sample(l).unwrap();
                writer.write_sample(r).unwrap();
            }
            writer.finalize().unwrap();
        }
        buf
    }

    fn mono_ir(sample_rate_hz: u32, taps: &[f32], block_size: usize) -> Arc<PreparedIr> {
        let bytes = write_mono_wav(sample_rate_hz, taps);
        Arc::new(
            PreparedIr::from_wav_bytes(
                &bytes,
                SampleRate::new(sample_rate_hz).unwrap(),
                block_size,
            )
            .unwrap(),
        )
    }

    fn stereo_ir(
        sample_rate_hz: u32,
        left: &[f32],
        right: &[f32],
        block_size: usize,
    ) -> Arc<PreparedIr> {
        let bytes = write_stereo_wav(sample_rate_hz, left, right);
        Arc::new(
            PreparedIr::from_wav_bytes(
                &bytes,
                SampleRate::new(sample_rate_hz).unwrap(),
                block_size,
            )
            .unwrap(),
        )
    }

    /// Runs `total` samples of a constant `value` through a mono stage in 64-sample chunks
    /// (`ctx`'s own `max_block_size`), returning every output sample in order. Every `process`
    /// call is wrapped in `audio_section` -- safe here because every caller of this helper only
    /// ever drives a *single* `load_ir` from nothing (the outgoing slot in any handover this
    /// helper settles is always `None`, so completing it never drops a real `IrSlot` -- see
    /// `process_wet`'s documented M2 gap and `nam.rs`'s identical helper for why that specific
    /// case is safe to run inside the RT harness end to end).
    fn process_constant_in_chunks(stage: &mut IrStage, total: usize, value: f32) -> Vec<f32> {
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

    // trace: FR-IR-100
    #[test]
    fn nothing_loaded_is_exact_passthrough() {
        // FR-CHAIN-040/FR-IR-100: usable with no IR loaded, behaving as bypassed.
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
    fn loaded_ir_matches_direct_convolution_once_settled() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        let h = vec![0.6f32, -0.2, 0.1, 0.05, -0.03];
        let ir = mono_ir(sample_rate, &h, 64);

        stage.load_ir(ir);

        // Long enough to clear the ~20 ms handover crossfade and the ~15 ms bypass blend many
        // times over.
        let total = 48_000usize;
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

        let reference = namir_ir::direct_convolve(&h, &input);

        // Only the tail should match: same reasoning as `nam.rs`'s identical test -- the ~20 ms
        // handover crossfade finishes quickly, but the separate ~15 ms bypass blend only starts
        // once the handover completes and `active` flips, and a one-pole needs several time
        // constants to become numerically negligible. 400 ms is a comfortable margin.
        let settle = 19_200usize;
        for i in settle..total {
            assert!(
                (stage_out[i] - reference[i]).abs() < 1e-4,
                "sample {i}: stage {} vs reference {}",
                stage_out[i],
                reference[i]
            );
        }
    }

    #[test]
    fn tail_samples_reports_active_irs_length() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        assert_eq!(stage.tail_samples(), 0, "nothing loaded -> zero tail");

        let h = vec![0.1f32; 300];
        let ir = mono_ir(sample_rate, &h, 64);
        stage.load_ir(ir);
        process_constant_in_chunks(&mut stage, 48_000, 0.0); // settle the handover.

        assert_eq!(stage.tail_samples(), 300);
    }

    #[test]
    fn mono_ir_into_stereo_chain_is_dual_mono() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Stereo);
        let h = vec![0.5f32, 0.0, 0.25, -0.1];
        let ir = mono_ir(sample_rate, &h, 64);
        stage.load_ir(ir);

        let total = 48_000usize;
        let mut left_out = Vec::with_capacity(total);
        let mut right_out = Vec::with_capacity(total);
        let mut offset = 0usize;
        while offset < total {
            let n = 64usize.min(total - offset);
            let value = 0.2 * ((offset as f32) * 0.01).sin();
            let mut left = [value; 64];
            let mut right = [value; 64]; // identical input on both channels, per the chain invariant.
            let mut channels: [&mut [f32]; 2] = [&mut left[..n], &mut right[..n]];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            left_out.extend_from_slice(io.channel(0));
            right_out.extend_from_slice(io.channel(1));
            offset += n;
        }

        let settle = 19_200usize;
        for i in settle..total {
            assert!(
                (left_out[i] - right_out[i]).abs() < 1e-6,
                "sample {i}: dual-mono channels diverged, left {} vs right {}",
                left_out[i],
                right_out[i]
            );
        }
    }

    #[test]
    fn stereo_ir_into_stereo_chain_channels_are_independent() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Stereo);
        let mut left_h = vec![0.0f32; 4];
        left_h[0] = 1.0;
        let mut right_h = vec![0.0f32; 4];
        right_h[2] = 1.0; // a different delay on the right channel.
        let ir = stereo_ir(sample_rate, &left_h, &right_h, 64);
        stage.load_ir(ir);

        // Long enough to clear the ~20 ms handover crossfade and the ~15 ms bypass blend many
        // times over (same margin `loaded_ir_matches_direct_convolution_once_settled` uses).
        let total = 48_000usize;
        let mut input = vec![0.0f32; total];
        for (i, s) in input.iter_mut().enumerate() {
            *s = 0.2 * ((i as f32) * 0.03).sin();
        }

        let mut left_out = Vec::with_capacity(total);
        let mut right_out = Vec::with_capacity(total);
        let mut offset = 0usize;
        while offset < total {
            let end = (offset + 64).min(total);
            let n = end - offset;
            let mut left = input[offset..end].to_vec();
            let mut right = input[offset..end].to_vec();
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            left_out.extend_from_slice(io.channel(0));
            right_out.extend_from_slice(io.channel(1));
            offset = end;
        }

        let expected_left = namir_ir::direct_convolve(&left_h, &input);
        let expected_right = namir_ir::direct_convolve(&right_h, &input);

        let settle = 19_200usize; // 400 ms.
        for i in settle..total {
            assert!(
                (left_out[i] - expected_left[i]).abs() < 1e-4,
                "left sample {i}: stage {} vs expected {}",
                left_out[i],
                expected_left[i]
            );
            assert!(
                (right_out[i] - expected_right[i]).abs() < 1e-4,
                "right sample {i}: stage {} vs expected {}",
                right_out[i],
                expected_right[i]
            );
        }
        // The two channels must actually differ (otherwise the independence this test targets
        // would pass vacuously).
        let mut any_diff = false;
        for i in settle..total {
            if (left_out[i] - right_out[i]).abs() > 1e-3 {
                any_diff = true;
                break;
            }
        }
        assert!(any_diff, "expected left/right channels to differ");
    }

    #[test]
    fn handover_crossfade_has_no_large_single_sample_jump() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        let ir_a = mono_ir(sample_rate, &[0.4f32, 0.1, -0.2], 64);
        let ir_b = mono_ir(sample_rate, &[-0.3f32, 0.2, 0.15], 64);

        stage.load_ir(ir_a);
        // Settle the first handover and the bypass blend fully (outgoing slot was `None`, so
        // this is safe to drive entirely inside the RT harness, same reasoning as
        // `process_constant_in_chunks`'s own doc comment).
        process_constant_in_chunks(&mut stage, 48_000, 0.1);

        stage.load_ir(ir_b);

        // A steady input through the second handover: track the largest single-sample jump.
        //
        // This runs **inside** `rt_harness::audio_section`, and that is as much the point of the
        // test as the smoothness assertion. Before M4 it could not: the loop is long enough
        // (100 ms) to drive the handover to completion, and completion used to *drop*
        // `slots[outgoing_idx]` -- a real `IrSlot` this time, with its own convolution ring
        // buffers -- on the audio thread. D-8.1 step 4's return ring closes that, so this test now
        // covers smoothness and RT-safety across a *complete* real-to-real handover at once
        // (`nam.rs`'s identical test carries the same note). Do not relax it back out of the
        // harness.
        let total = 4_800usize; // 100 ms, comfortably longer than the 20 ms handover.
        let value = 0.1f32;
        let mut prev: Option<f32> = None;
        let mut max_delta = 0.0f32;
        let mut offset = 0usize;
        // Allocated up front, outside the harness, and reused -- test scaffolding, not part of
        // what `process` is allowed to do.
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

        // Two small, bounded-output IRs crossfading between each other cannot produce a jump
        // anywhere near a full-range discontinuity -- 0.5 is generous but well below what a
        // dropped/miscomputed handover step would show (this module's doc comment; `nam.rs`'s
        // identical test uses the same bound and reasoning).
        assert!(
            max_delta < 0.5,
            "handover crossfade produced a jump of {max_delta}, expected a smooth fade"
        );
    }

    #[test]
    fn disabled_stage_is_passthrough_even_with_an_ir_loaded() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        stage.apply(ParamChange {
            id: ENABLED_ID,
            value: 0.0,
        });
        stage.load_ir(mono_ir(sample_rate, &[0.9f32, 0.3, -0.1], 64));

        let input = 0.42f32;
        let out = process_constant_in_chunks(&mut stage, 48_000, input);
        let tail = *out.last().unwrap();
        assert!(
            (tail - input).abs() < 1e-4,
            "expected disabled stage to pass through even with an IR loaded, got {tail} vs {input}"
        );
    }

    /// Loads a single-tap ("identity") IR so the convolution itself contributes no shaping, then
    /// exercises FR-IR-070's low-cut/high-cut toggles the same DC/Nyquist-gain way `eq.rs`'s own
    /// HP/LP tests do. Needs a *loaded, enabled, settled* stage (not "nothing loaded") because the
    /// low-cut/high-cut filters run on the wet path, upstream of the outer bypass blend -- with
    /// nothing loaded, `mix` settles at 0 and the blend discards the filtered wet signal entirely
    /// (this module's doc comment's "FR-IR-070 order" section).
    fn identity_stage(sample_rate: u32) -> IrStage {
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        stage.load_ir(mono_ir(sample_rate, &[1.0f32], 64));
        process_constant_in_chunks(&mut stage, 48_000, 0.0); // settle fully wet.
        stage
    }

    /// See `eq.rs`'s identical helper for why a constant-input steady-state ratio is exactly a
    /// cascade's DC gain.
    fn process_constant_tail(stage: &mut IrStage, total: usize, value: f32) -> f32 {
        process_constant_in_chunks(stage, total, value)
            .last()
            .copied()
            .unwrap()
    }

    /// See `eq.rs`'s identical helper for why an alternating `+-value` sequence's steady-state
    /// output/input ratio is exactly a cascade's Nyquist gain.
    fn process_alternating_tail(stage: &mut IrStage, total: usize, value: f32) -> (f32, f32) {
        let mut buf = vec![0.0f32; total];
        for (n, s) in buf.iter_mut().enumerate() {
            *s = if n % 2 == 0 { value } else { -value };
        }
        let last_input = buf[total - 1];
        let mut offset = 0usize;
        while offset < buf.len() {
            let end = (offset + 64).min(buf.len());
            let n = end - offset;
            let mut channels: [&mut [f32]; 1] = [&mut buf[offset..end]];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            offset = end;
        }
        (buf[total - 1], last_input)
    }

    // trace-partial: FR-IR-070
    // uncovered: FR-IR-070 — of the four controls the "U per control" method names, the low cut's
    // uncovered: 20-500 Hz and high cut's 1 kHz-20 kHz ranges are set by no test:
    // uncovered: LOW_CUT_FREQ_HZ_ID and HIGH_CUT_FREQ_HZ_ID appear only in their declarations and
    // uncovered: live apply arms, and both cut tests run at the descriptor defaults probing DC
    // uncovered: and Nyquist, which is blind to the corner frequency; closes M9b
    #[test]
    fn low_cut_enabled_blocks_dc_and_passes_near_nyquist() {
        let sample_rate = 48_000;
        let mut stage = identity_stage(sample_rate);
        stage.apply(ParamChange {
            id: LOW_CUT_ENABLED_ID,
            value: 1.0,
        });

        let dc = 0.2f32;
        let dc_tail = process_constant_tail(&mut stage, 48_000, dc);
        assert!(
            (dc_tail / dc).abs() < 1e-2,
            "expected DC heavily attenuated once low-cut is enabled, got ratio {}",
            dc_tail / dc
        );

        let (nyq_out, nyq_in) = process_alternating_tail(&mut stage, 4_800, dc);
        let nyq_db = linear_to_db((nyq_out / nyq_in).abs());
        assert!(
            nyq_db.abs() < 0.3,
            "nyquist_db={nyq_db}, expected ~0 (passed)"
        );
    }

    #[test]
    fn high_cut_enabled_passes_dc_and_blocks_near_nyquist() {
        let sample_rate = 48_000;
        let mut stage = identity_stage(sample_rate);
        stage.apply(ParamChange {
            id: HIGH_CUT_ENABLED_ID,
            value: 1.0,
        });

        let dc = 0.2f32;
        let dc_tail = process_constant_tail(&mut stage, 48_000, dc);
        let dc_db = linear_to_db(dc_tail / dc);
        assert!(dc_db.abs() < 0.3, "dc_db={dc_db}, expected ~0 (passed)");

        let (nyq_out, nyq_in) = process_alternating_tail(&mut stage, 4_800, dc);
        assert!(
            (nyq_out / nyq_in).abs() < 1e-2,
            "expected Nyquist heavily attenuated once high-cut is enabled, got ratio {}",
            nyq_out / nyq_in
        );
    }

    #[test]
    fn level_db_is_applied_once_settled() {
        let sample_rate = 48_000;
        let mut stage = identity_stage(sample_rate);
        stage.apply(ParamChange {
            id: LEVEL_DB_ID,
            value: -6.0,
        });

        let dc = 0.3f32;
        let dc_tail = process_constant_tail(&mut stage, 48_000, dc);
        let expected = dc * db_to_linear(-6.0);
        assert!(
            (dc_tail - expected).abs() < 1e-3,
            "got {dc_tail}, expected {expected}"
        );
    }

    #[test]
    fn crossfade_in_progress_does_not_allocate() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Stereo);
        stage.load_ir(mono_ir(sample_rate, &[0.5f32, 0.2, -0.1], 64));
        // Still mid-handover (20 ms = 960 samples at 48 kHz; 64 samples in is well inside it).
        let mut left = [0.1f32; 64];
        let mut right = [0.1f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));

        stage.load_ir(stereo_ir(
            sample_rate,
            &[0.3f32, 0.0, 0.1],
            &[0.0f32, 0.4, -0.2],
            64,
        )); // start a second handover (mixed mono/stereo channel counts), still mid-first.
        let mut left = [0.1f32; 64];
        let mut right = [0.1f32; 64];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| stage.process(&mut io));
    }

    /// The highest-volume RT path: steady-state (post-handover, post-bypass-settle) stereo
    /// convolution with low-cut, high-cut and a non-unity level all engaged simultaneously.
    #[test]
    fn steady_state_process_does_not_allocate() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Stereo);
        stage.load_ir(stereo_ir(
            sample_rate,
            &[0.6f32, 0.1, -0.05],
            &[0.4f32, -0.2, 0.1],
            64,
        ));
        stage.apply(ParamChange {
            id: LOW_CUT_ENABLED_ID,
            value: 1.0,
        });
        stage.apply(ParamChange {
            id: HIGH_CUT_ENABLED_ID,
            value: 1.0,
        });
        stage.apply(ParamChange {
            id: LEVEL_DB_ID,
            value: -3.0,
        });

        for _ in 0..400 {
            // ~530 ms at 48 kHz/64 samples -- comfortably past both fades settling.
            let mut left = [0.2f32; 64];
            let mut right = [0.2f32; 64];
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut io = StageIo::new(&mut channels, 64);
            audio_section(|| stage.process(&mut io));
            for s in io.channel(0) {
                assert!(s.is_finite(), "non-finite output in steady state");
            }
            for s in io.channel(1) {
                assert!(s.is_finite(), "non-finite output in steady state");
            }
        }
    }
}
