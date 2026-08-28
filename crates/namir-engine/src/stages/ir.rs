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
            level.push(level_ramp_at_default(sample_rate, level_db_default));
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
            // identical fast path, whose comment carries the full account of issue #56. The fade
            // is mathematically complete but the retire pen was still occupied when it ended, so
            // retry the finalization first, on every block: falling straight through to the render
            // made this state a dead end nothing re-tested, permanently for the session.
            self.try_finalize_handover();

            // Then run only the incoming slot rather than blending in an outgoing one scaled by
            // `cos(FRAC_PI_2)` (-4.4e-8 in f32, not exactly zero) and paying its convolution cost
            // for as long as the deferral lasts. `incoming_idx` names the same slot either way: a
            // successful finalization sets `self.active` *to* it.
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
            if !self.try_finalize_handover() {
                // Return ring full, worker not draining: defer the bookkeeping only. The audio is
                // already correct (the fast path above runs the incoming slot alone, and retries
                // this finalization on every block until the pen clears -- issue #56). D-8.1's
                // "degradation, not failure (P8)". Dropping the outgoing slot here to make
                // progress is the exact bug this milestone removes.
                self.crossfade = Some(crossfade);
            }
        } else {
            self.crossfade = Some(crossfade);
        }
    }

    /// `nam.rs`'s `try_finalize_handover`, for this stage -- same contract, same two call sites,
    /// same `false`-means-the-pen-is-still-occupied return. Read that one's doc comment.
    fn try_finalize_handover(&mut self) -> bool {
        if self.retired.is_some() {
            return false;
        }
        let outgoing_idx = self.active;
        // **The M2 P1 violation, closed** -- identical change and identical reasoning to
        // `nam.rs`'s. This used to be `self.slots[outgoing_idx] = None`, a drop of the outgoing
        // `IrState`'s convolution ring buffers (and possibly the last `Arc<PreparedIr>`) on the
        // audio thread. `take()` moves instead. Do not "simplify" this back to an assignment.
        self.retired = self.slots[outgoing_idx]
            .take()
            .map(|slot| Resource::ir(slot, self.prepared_for));
        self.active = 1 - outgoing_idx;
        self.crossfade = None;
        self.recompute_mix_target();
        true
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

/// Issue #127's follow-up: the one place this stage's per-channel level ramp is constructed, so `prepare` and the
/// test that pins its start point cannot drift apart. `GainRamp::new_at_db` rather than
/// `GainRamp::new` followed by `set_target_db`: the latter leaves `current` at unity and `target`
/// at the default, so the first ~25 ms of audio after every prepare, sample-rate change or
/// re-prepare ramps from 0 dB to the parameter's real default. That is inaudible only because
/// `ir.level_db` happens to default to 0.0 dB today — see `GainRamp::new_at_db`'s own doc comment.
fn level_ramp_at_default(sample_rate: SampleRate, default_db: f32) -> GainRamp {
    GainRamp::new_at_db(sample_rate, LEVEL_RAMP_TIME_CONSTANT_MS, default_db)
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

    /// Issue #127's follow-up. The defect is invisible at the shipped 0.0 dB default — a ramp from
    /// unity to unity has nowhere to travel — so this drives the same constructor `prepare` uses
    /// with a default that is *not* unity, which is the shape the trap is set for. Red against
    /// `GainRamp::new` + `set_target_db`: that starts `current` at 1.0 and the first block fades.
    #[test]
    fn the_level_ramp_is_built_settled_at_a_non_unity_default() {
        let sample_rate = SampleRate::new(48_000).unwrap();
        let mut ramp = level_ramp_at_default(sample_rate, -12.0);
        assert!(
            (ramp.current_db() - (-12.0)).abs() < 1e-4,
            "the ramp starts at {} dB, not the -12.0 dB default it was built with",
            ramp.current_db()
        );

        let expected = db_to_linear(-12.0);
        let mut buf = [1.0f32; 64];
        ramp.process(&mut buf);
        for (i, x) in buf.iter().enumerate() {
            assert!(
                (x - expected).abs() < 1e-6,
                "sample {i} of the first block is {x}, not {expected} -- the ramp is \
                 travelling to its default instead of starting there"
            );
        }
    }

    /// The other half: `prepare` really does route through level_ramp_at_default, so the assertion above is
    /// about this stage and not just about `namir-dsp`. At the shipped default this is a
    /// tripwire rather than a live check -- it starts failing the day the default moves and the
    /// construction has drifted back.
    #[test]
    fn a_prepared_stage_starts_settled_at_the_level_default() {
        let stage = stage(48_000, ChannelConfig::Stereo);
        let default_db = continuous_default(LEVEL_DB);
        for (index, ramp) in stage.level.iter().enumerate() {
            assert!(
                (ramp.current_db() - default_db).abs() < 1e-4,
                "channel {index}'s level ramp sits at {} dB, not its {default_db} dB default",
                ramp.current_db()
            );
        }
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

    /// **Issue #56's Ir half.** `nam.rs`'s
    /// `a_handover_deferred_by_a_full_retire_pen_finalizes_once_the_pen_clears` carries the full
    /// account; this is the same defect in the same shape, in `process_wet`'s `remaining == 0`
    /// fast path, and it needs its own test because the two stages carry two copies of the state
    /// machine.
    ///
    /// Committed red-first: before the fix, `crossfade` is still `Some(remaining: 0)` and `active`
    /// is still 1 after the pen has been drained and further blocks processed.
    #[test]
    fn a_handover_deferred_by_a_full_retire_pen_finalizes_once_the_pen_clears() {
        const SR: u32 = 48_000;
        /// 20 ms at 48 kHz is 960 samples; this is comfortably past a whole fade.
        const PAST_A_FADE: usize = 2_048;
        /// Well inside one, so the next install displaces a slot still fading in.
        const MID_FADE: usize = 128;

        let mut stage = stage(SR, ChannelConfig::Mono);
        let taps = [0.6f32, -0.2, 0.1];

        stage.load_ir(mono_ir(SR, &taps, 64));
        process_constant_in_chunks(&mut stage, PAST_A_FADE, 0.1);
        assert_eq!(stage.active, 1);
        assert!(stage.crossfade.is_none());
        assert!(stage.retired.is_none());

        stage.load_ir(mono_ir(SR, &taps, 64));
        process_constant_in_chunks(&mut stage, MID_FADE, 0.1);
        stage.load_ir(mono_ir(SR, &taps, 64));
        assert!(
            stage.retired.is_some(),
            "the displaced slot should be parked in the pen"
        );

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

        let (mut producer, mut consumer) = crate::ring::ring::<Resource>(4);
        {
            let mut sink = RetireSink::new(&mut producer);
            stage.collect_retired(&mut sink);
        }
        assert!(consumer.try_pop().is_some());

        process_constant_in_chunks(&mut stage, 64, 0.1);
        assert!(
            stage.crossfade.is_none(),
            "the deferred handover must finalize once the pen clears, not stay in it forever"
        );
        assert_eq!(stage.active, 0);
        assert!(stage.retired.is_some());
        assert_eq!(stage.mix_target, 1.0);
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

    /// **FR-CHAIN-020's "U per stage" limb for this stage**, which nothing executed until M14:
    /// `disabled_stage_is_passthrough_even_with_an_ir_loaded` below applies `ENABLED = 0` *before
    /// any audio is processed* and then asserts a steady state, so it never sees a bypass toggle —
    /// the requirement's "without an audible click or discontinuity" was untested here.
    ///
    /// A constant input, exactly as `gate.rs`'s and `nam.rs`'s counterparts use, so every step
    /// measured belongs to the crossfade and none to the signal. The bound is this stage's own
    /// [`BYPASS_CROSSFADE_TIME_CONSTANT_MS`] one-pole: at most `range · (1 − e^(−1/τ))` per sample.
    // trace: FR-CHAIN-020
    #[test]
    fn bypass_toggle_mid_signal_is_click_free() {
        let sample_rate = 48_000;
        let mut stage = stage(sample_rate, ChannelConfig::Mono);
        // A multi-tap IR with a large DC gain, so bypassing it is a big, unambiguous change.
        stage.load_ir(mono_ir(sample_rate, &[0.9f32, 0.7, 0.5, 0.3], 64));

        let value = 0.2f32;
        let settled_wet = *process_constant_in_chunks(&mut stage, 48_000, value)
            .last()
            .expect("the settle run produced output");
        let range = (settled_wet - value).abs();
        assert!(
            range > 1e-3,
            "the convolved output is indistinguishable from its input ({settled_wet} vs {value}), \
             so bypassing it changes nothing and this test would pass vacuously"
        );

        stage.apply(ParamChange {
            id: ENABLED_ID,
            value: 0.0,
        });
        let out = process_constant_in_chunks(&mut stage, 9_600, value);

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
        let tail = *out.last().unwrap();
        assert!(
            (tail - value).abs() < 1e-3,
            "expected the bypassed stage to settle onto its input, got {tail} vs {value}"
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

    /// The rate every test in this module runs at.
    const TEST_SR: f64 = 48_000.0;
    /// Discarded before measuring: 100 ms, many times any of these filters' settling times, and
    /// long enough for the handover and bypass blends `identity_stage` starts to be long gone.
    const PROBE_WARMUP: usize = 4_800;
    /// The measurement window: 0.5 s, so any *even* probe frequency completes a whole number of
    /// cycles inside it and the single-bin DFT below needs no window function. Same apparatus, and
    /// same reasoning, as `eq.rs`'s FR-EQ-010 probe.
    const PROBE_WINDOW: usize = 24_000;
    /// Probe amplitude, well inside `f32`'s comfortable range at every setting swept below.
    const PROBE_AMPLITUDE: f32 = 0.2;

    /// `stage`'s measured magnitude response at `probe_hz`, in dB, through the real
    /// `Stage::process` path. See `eq.rs`'s identical helper for the single-bin-DFT reasoning.
    fn measure_magnitude_db(stage: &mut IrStage, probe_hz: f64) -> f64 {
        assert!(
            probe_hz.fract() == 0.0 && (probe_hz as u64).is_multiple_of(2),
            "probe frequencies must be even integers so {PROBE_WINDOW} samples is a whole number \
             of cycles; got {probe_hz}"
        );
        let total = PROBE_WARMUP + PROBE_WINDOW;
        let mut buf: Vec<f32> = (0..total)
            .map(|n| {
                (f64::from(PROBE_AMPLITUDE)
                    * (std::f64::consts::TAU * probe_hz * n as f64 / TEST_SR).sin())
                    as f32
            })
            .collect();

        let mut offset = 0usize;
        while offset < total {
            let end = (offset + 64).min(total);
            let n = end - offset;
            let mut channels: [&mut [f32]; 1] = [&mut buf[offset..end]];
            let mut io = StageIo::new(&mut channels, n);
            audio_section(|| stage.process(&mut io));
            offset = end;
        }

        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, &s) in buf[PROBE_WARMUP..].iter().enumerate() {
            let w = std::f64::consts::TAU * probe_hz * n as f64 / TEST_SR;
            re += f64::from(s) * w.cos();
            im += f64::from(s) * w.sin();
        }
        let amplitude = 2.0 * (re * re + im * im).sqrt() / (total - PROBE_WARMUP) as f64;
        20.0 * (amplitude / f64::from(PROBE_AMPLITUDE)).log10()
    }

    /// **FR-IR-070's table, control by control.** The requirement tabulates a range and a default
    /// for all four controls and nothing read any of them back — `params.lock` records key, id,
    /// kind tag and smoothing and carries no bounds, so every number below could change with the
    /// manifest untouched. Read off the descriptors this stage seeds itself from in `prepare`.
    #[test]
    fn every_control_matches_the_range_and_default_the_requirement_tabulates() {
        for (descriptor, min, max, default) in [
            (LEVEL_DB, -24.0f32, 24.0f32, 0.0f32),
            (LOW_CUT_FREQ_HZ, 20.0, 500.0, 80.0),
            (HIGH_CUT_FREQ_HZ, 1_000.0, 20_000.0, 8_000.0),
        ] {
            let ParamKind::Continuous {
                min: got_min,
                max: got_max,
                default: got_default,
            } = descriptor.kind
            else {
                panic!("{} must be Continuous", descriptor.key);
            };
            assert_eq!(got_min, min, "{}: minimum", descriptor.key);
            assert_eq!(got_max, max, "{}: maximum", descriptor.key);
            assert_eq!(got_default, default, "{}: default", descriptor.key);
        }

        // The three stepped controls: "on / off" for the stage, and "off, or <range>" for each cut,
        // whose "off" is the separate enable the table folds into the same cell.
        for (descriptor, default_index) in [
            (ENABLED, 1usize),
            (LOW_CUT_ENABLED, 0),
            (HIGH_CUT_ENABLED, 0),
        ] {
            let ParamKind::Stepped {
                values,
                default_index: got,
            } = descriptor.kind
            else {
                panic!("{} must be Stepped", descriptor.key);
            };
            assert_eq!(values, &["Off", "On"], "{}: named values", descriptor.key);
            assert_eq!(
                got.0 as usize, default_index,
                "{}: default index",
                descriptor.key
            );
        }
    }

    /// **FR-IR-070's low cut and high cut, at corners across their tabulated ranges.** Both cut
    /// tests below run at the descriptor defaults and probe DC and Nyquist, which is blind to where
    /// the corner actually sits: `LOW_CUT_FREQ_HZ_ID` and `HIGH_CUT_FREQ_HZ_ID` were written by
    /// nothing in the workspace outside their own declarations and `apply` arms.
    ///
    /// Each corner is checked at the one frequency that pins it — its own — against the −3.0103 dB
    /// a Butterworth-aligned second-order section has there, plus an octave either side to show the
    /// slope runs the right way. The stage is otherwise an identity IR, so what is measured is the
    /// filter and not the convolution.
    // trace: FR-IR-070
    #[test]
    fn each_cut_corner_lands_where_the_parameter_puts_it() {
        /// A Butterworth-aligned second-order corner, exactly.
        const MINUS_THREE_DB: f64 = -3.010_299_956_639_812;
        /// The stage designs its cuts in `f64` and runs them in `f32`; a tenth of a dB is the
        /// tolerance FR-EQ-010 states for the same filters in the EQ stage, reused here.
        const TOLERANCE_DB: f64 = 0.1;

        // Every corner (and its half and double) is an even integer, which is what
        // [`measure_magnitude_db`]'s whole-number-of-cycles requirement needs.
        for corner in [20.0f64, 80.0, 240.0, 500.0] {
            let mut stage = identity_stage(48_000);
            stage.apply(ParamChange {
                id: LOW_CUT_ENABLED_ID,
                value: 1.0,
            });
            stage.apply(ParamChange {
                id: LOW_CUT_FREQ_HZ_ID,
                value: corner as f32,
            });

            let at_corner = measure_magnitude_db(&mut stage, corner);
            assert!(
                (at_corner - MINUS_THREE_DB).abs() < TOLERANCE_DB,
                "low cut at {corner} Hz measures {at_corner:.4} dB at its own corner, not -3.01 dB"
            );

            // An octave below is ~12 dB down, an octave above ~1 dB down: asserted as an ordering
            // rather than as figures, so this stays a statement about the corner's *placement*.
            let below = measure_magnitude_db(&mut stage, corner / 2.0);
            let above = measure_magnitude_db(&mut stage, corner * 2.0);
            assert!(
                below < at_corner - 3.0 && above > at_corner + 1.0,
                "low cut at {corner} Hz is not sloped around its corner: {below:.2} / \
                 {at_corner:.2} / {above:.2} dB at half, at, and twice the corner"
            );
        }

        for corner in [1_000.0f64, 8_000.0, 20_000.0] {
            let mut stage = identity_stage(48_000);
            stage.apply(ParamChange {
                id: HIGH_CUT_ENABLED_ID,
                value: 1.0,
            });
            stage.apply(ParamChange {
                id: HIGH_CUT_FREQ_HZ_ID,
                value: corner as f32,
            });

            let at_corner = measure_magnitude_db(&mut stage, corner);
            assert!(
                (at_corner - MINUS_THREE_DB).abs() < TOLERANCE_DB,
                "high cut at {corner} Hz measures {at_corner:.4} dB at its own corner, not -3.01 dB"
            );

            let below = measure_magnitude_db(&mut stage, corner / 2.0);
            assert!(
                below > at_corner + 1.0,
                "high cut at {corner} Hz is not sloped around its corner: {below:.2} dB at half \
                 the corner against {at_corner:.2} dB at it"
            );
            // Above the corner only where there is room for it below Nyquist.
            if corner * 2.0 <= 20_000.0 {
                let above = measure_magnitude_db(&mut stage, corner * 2.0);
                assert!(
                    above < at_corner - 3.0,
                    "high cut at {corner} Hz does not roll off above its corner: {above:.2} dB at \
                     twice it against {at_corner:.2} dB at it"
                );
            }
        }
    }

    /// **FR-IR-070's Level control across its tabulated −24…+24 dB range**, both endpoints and the
    /// 0 dB default, where `level_db_is_applied_once_settled` below checks one interior value.
    // trace: FR-IR-070
    #[test]
    fn level_spans_its_whole_tabulated_range() {
        for level_db in [-24.0f32, -12.0, 0.0, 12.0, 24.0] {
            let mut stage = identity_stage(48_000);
            stage.apply(ParamChange {
                id: LEVEL_DB_ID,
                value: level_db,
            });
            let dc = 0.03f32; // +24 dB of it is still comfortably inside full scale.
            let tail = process_constant_tail(&mut stage, 48_000, dc);
            let measured_db = linear_to_db(tail / dc);
            assert!(
                (measured_db - level_db).abs() < 0.1,
                "level {level_db} dB measured {measured_db} dB"
            );
        }
    }

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
