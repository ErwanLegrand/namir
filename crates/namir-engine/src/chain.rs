use std::collections::VecDeque;

use namir_params::global::{GLOBAL_BYPASS, OUTPUT_CEILING_DB};

use crate::command::RetireSink;
use crate::param::{ParamChange, ParamId};
use crate::resource::{Resource, ResourceKind};
use crate::stage::Stage;
use crate::stage_io::StageIo;
use crate::telemetry::{TelemetryEntry, TelemetrySink};

/// Telemetry signal id for FR-CHAIN-080's fault counter — the chain's own reading, alongside the
/// per-stage ones. Same readout-not-parameter convention every stage's telemetry id uses, so it is
/// never added to `namir_params::REGISTRY` and `params.lock` is unaffected.
const TELEMETRY_FAULT_COUNT: u32 = namir_params::ParamId::from_key("telemetry.chain.fault_count").0;

/// D-10.4: this chain's own RT-facing `namir_engine::ParamId`s for FR-CHAIN-030's global bypass
/// and FR-CHAIN-090's output ceiling, converted once from `namir_params`'s own ids for the same
/// keys — the identical per-stage convention `stages/trim.rs`'s `GAIN_DB_ID` documents, applied
/// here to the two chain-level (not stage-owned) descriptors `namir_params::global` declares.
/// [`Chain::apply`] matches on these the same way a stage's own `apply` matches on its ids.
const GLOBAL_BYPASS_ID: ParamId = ParamId(GLOBAL_BYPASS.id.0);
/// See [`GLOBAL_BYPASS_ID`].
const OUTPUT_CEILING_DB_ID: ParamId = ParamId(OUTPUT_CEILING_DB.id.0);

/// D-6.1: "the chain is `Vec<Box<dyn Stage>>` built once during preparation." Building that
/// vector — running each configured stage's `StagePrep::prepare` and boxing the result — is the
/// caller's job. 1.0's fixed six-stage assembly and any future dynamic chain-building (RD-2)
/// both belong to whatever owns the stage *list* (worker/adapter code, not yet built), not to
/// `Chain` itself.
///
/// # Cross-cutting features (FR-CHAIN-030/080/090)
///
/// `global_bypass` and `output_ceiling_linear` are plain fields, not folded into
/// `cross_cutting`, precisely so [`Chain::set_global_bypass`] and
/// [`Chain::set_output_ceiling_db`] are callable in either order relative to
/// [`Chain::prepare_crosscutting`] without one silently no-oping — they just record intent.
/// Whether that intent has any effect on `process` depends only on whether `cross_cutting` is
/// `Some`, i.e. whether `prepare_crosscutting` was ever called. See `prepare_crosscutting`'s own
/// doc comment for why that call is opt-in rather than folded into `new`.
pub struct Chain {
    stages: Vec<Box<dyn Stage>>,
    /// FR-CHAIN-030: when `true` *and* `cross_cutting` is `Some`, `process` takes the bypass path
    /// instead of running `stages`. RT-safe to flip (see `set_global_bypass`) since it is read,
    /// never allocated, on the audio thread.
    global_bypass: bool,
    /// FR-CHAIN-090's ceiling, already converted to a linear multiplier (so `process` never calls
    /// `db_to_linear` itself — that conversion happens once, in `set_output_ceiling_db`, off the
    /// audio thread). Defaults to `db_to_linear(0.0)` = unity, i.e. 0 dBFS.
    output_ceiling_linear: f32,
    /// FR-CHAIN-080's fault counter: incremented once per `process` call in which any produced
    /// sample was NaN/infinite, never reset by anything short of a new `Chain` (a reset/transport
    /// stop is not "the fault didn't happen").
    fault_count: u64,
    /// `None` until `prepare_crosscutting` runs; `Some` afterward. Gates *all three* new
    /// behaviours' actual effect on `process` — see this struct's own doc comment.
    cross_cutting: Option<CrossCuttingState>,
}

/// Non-RT-allocated state that only exists once [`Chain::prepare_crosscutting`] has run:
/// FR-CHAIN-030's per-channel latency-compensation ring for the bypass path. Sized once, off the
/// audio thread, to exactly `latency_samples` per channel — `process` only ever pops one sample
/// and pushes one sample per input sample, so the ring's length never moves outside
/// `[0, latency_samples]`, and it therefore never needs to grow (P1).
struct CrossCuttingState {
    /// One ring per channel (`ctx.channel_config().output_channels()` many — `stage_io.rs`'s own
    /// doc comment: `StageIo`'s channel count is fixed for the whole chain to that figure).
    /// Empty (zero-capacity, never touched) when `latency_samples == 0`; see `apply_bypass`.
    delay_rings: Vec<VecDeque<f32>>,
    /// Cached copy of `Chain::latency_samples()` as it stood when `prepare_crosscutting` ran, so
    /// `apply_bypass` doesn't need to re-walk `stages` (and doesn't have to borrow `stages`
    /// alongside `cross_cutting`) on every block.
    latency_samples: u32,
}

impl CrossCuttingState {
    /// FR-CHAIN-030's bypass path: "input routed to output at unity gain, with only the latency
    /// compensation needed for sample alignment." Pop-then-push, not the more literal
    /// push-then-pop: a FIFO's oldest element is unaffected by what gets appended after it, so
    /// the value released is identical either way, but popping first means the ring's length
    /// only ever dips to `latency_samples - 1` and returns to `latency_samples` — it never
    /// touches `latency_samples + 1`, so the `VecDeque` `prepare_crosscutting` sized can never
    /// need to grow (P1).
    fn apply_bypass(&mut self, io: &mut StageIo<'_>) {
        if self.latency_samples == 0 {
            // Nothing to compensate for: leaving the buffer untouched already *is* "input routed
            // to output at unity gain" with zero latency (prepare_crosscutting's doc comment).
            return;
        }
        for (ring, channel) in self.delay_rings.iter_mut().zip(io.channels_mut()) {
            for sample in channel.iter_mut() {
                // Prefilled with `latency_samples` zeros by `prepare_crosscutting`, so this
                // `unwrap_or` only ever falls back to 0.0 in principle, never in practice — kept
                // as a fallback rather than `.unwrap()` so a future bug here degrades to silence
                // instead of a panic on the audio thread (D-16.3).
                let delayed = ring.pop_front().unwrap_or(0.0);
                ring.push_back(*sample);
                *sample = delayed;
            }
        }
    }

    /// FR-CHAIN-080/090, run once per `process` call after either the stage loop or the bypass
    /// path has produced this block's samples. First scans for any non-finite sample: if found,
    /// the *entire* block — every channel, every sample, not just the offending one — is
    /// overwritten with silence and `fault_count` increments by exactly one (one fault *event*
    /// per call, however many non-finite samples it contained), then returns without clamping —
    /// zero is already within any ceiling, so there is nothing left to clamp. Otherwise clamps
    /// every sample's magnitude to `ceiling_linear`, sign preserved via `f32::clamp`'s own
    /// symmetric-range behaviour.
    fn scan_and_clamp(&mut self, io: &mut StageIo<'_>, ceiling_linear: f32, fault_count: &mut u64) {
        let faulted = io
            .channels_mut()
            .any(|channel| channel.iter().any(|s| !s.is_finite()));
        if faulted {
            for channel in io.channels_mut() {
                channel.fill(0.0);
            }
            *fault_count += 1;
            return;
        }
        for channel in io.channels_mut() {
            for sample in channel.iter_mut() {
                *sample = sample.clamp(-ceiling_linear, ceiling_linear);
            }
        }
    }
}

impl Chain {
    /// Wraps an already-`prepare`d stage list. Building that list is the caller's job; see this
    /// struct's doc comment.
    ///
    /// Deliberately leaves `cross_cutting` at `None` — FR-CHAIN-030/080/090 stay inactive until
    /// [`Chain::prepare_crosscutting`] is called explicitly. See that method's doc comment for
    /// why this constructor doesn't do it implicitly: this file's own 8 pre-existing tests (and
    /// any future test scaffolding built directly on `Chain::new`) rely on a raw, untouched
    /// `process` — only the real product path (`build_default_chain`, once wired) is expected to
    /// call `prepare_crosscutting`.
    pub fn new(stages: Vec<Box<dyn Stage>>) -> Self {
        Self {
            stages,
            global_bypass: false,
            output_ceiling_linear: namir_core::db_to_linear(0.0),
            fault_count: 0,
            cross_cutting: None,
        }
    }

    /// Non-RT setup call that switches the chain into "cross-cutting active" mode: from this
    /// call onward, `process` also applies FR-CHAIN-030 (global bypass, once
    /// [`set_global_bypass`](Chain::set_global_bypass) turns it on), FR-CHAIN-080 (NaN/Inf ->
    /// silence + fault flag), and FR-CHAIN-090 (output ceiling clamp). Before this call, `process`
    /// behaves exactly as it always has — see `Chain::new`'s doc comment.
    ///
    /// May allocate (it is not run on the audio thread): it pre-sizes one delay ring per channel,
    /// each `self.latency_samples()` long, using this chain's *own* `latency_samples()` (computed
    /// from `stages` exactly as `Chain::latency_samples` already does — this is not a second,
    /// possibly-divergent notion of latency). Channel count comes from
    /// `ctx.channel_config().output_channels()`, matching every stage's own sizing convention
    /// (`stage_io.rs`'s doc comment, `trim.rs`'s "`StageIo`'s channel count is fixed for the whole
    /// chain" note) — the same count `process`'s `StageIo` will carry on every call.
    ///
    /// The real product path (a future `build_default_chain()`, not yet wired — see
    /// `stages/mod.rs`) is expected to always call this right after assembling the chain, before
    /// the first `process`. `Chain::new`'s raw/direct-construction path — this file's own
    /// existing tests, and any future scaffolding built the same way — intentionally does not,
    /// so FR-CHAIN-080/090 apply to the shipped product without retrofitting behaviour onto
    /// already-proven test fixtures (see the module-level `CRITICAL CONSTRAINT` this was written
    /// against: `apply_broadcasts_to_every_stage` produces `db_to_linear(6.0)^2 ~= 3.98`, above 0
    /// dBFS, and must keep doing so unmodified).
    pub fn prepare_crosscutting(&mut self, ctx: &crate::prepare::PrepareContext) {
        let channel_count = ctx.channel_config().output_channels() as usize;
        let latency_samples = self.latency_samples();
        let delay_rings = (0..channel_count)
            .map(|_| {
                // Zero-capacity when latency is 0: `apply_bypass` special-cases that to a no-op
                // and never touches the ring, so there is nothing worth preallocating.
                let mut ring = VecDeque::with_capacity(latency_samples as usize);
                ring.resize(latency_samples as usize, 0.0);
                ring
            })
            .collect();
        self.cross_cutting = Some(CrossCuttingState {
            delay_rings,
            latency_samples,
        });
    }

    /// FR-CHAIN-030: turns the chain-wide bypass on or off. RT-safe — flips one `bool`, nothing
    /// else — so this may be called from the audio thread's own command-handling path as well as
    /// from setup code.
    ///
    /// Only has any effect once [`prepare_crosscutting`](Chain::prepare_crosscutting) has been
    /// called: with no delay ring built, there is nothing for `process` to route input through
    /// besides the stages themselves, so `process` just runs them as it always has. No existing
    /// test calls this — it is exercised only by this module's new cross-cutting tests, which do
    /// call `prepare_crosscutting` first.
    ///
    /// **D-10.4:** the product path no longer calls this directly — a `global.bypass` change now
    /// arrives as an ordinary [`ParamChange`] through [`Chain::apply`], exactly like every stage
    /// parameter, and `apply` calls this method internally. It stays `pub` as the low-level setter
    /// this module's own tests (and any other direct `Chain` construction) use.
    pub fn set_global_bypass(&mut self, enabled: bool) {
        self.global_bypass = enabled;
    }

    /// FR-CHAIN-090: sets the output ceiling, in dB, that `process` clamps every sample's
    /// magnitude to (sign preserved) once cross-cutting is active. Converts to a linear
    /// multiplier once, here, so `process` itself never calls `db_to_linear` (that would be pure
    /// arithmetic either way, but keeping *all* dB math off the audio thread is this crate's
    /// consistent convention). Defaults to `db_to_linear(0.0)` = 1.0, i.e. 0 dBFS, from
    /// `Chain::new` onward — set this before or after `prepare_crosscutting`, in either order;
    /// see this struct's own doc comment for why the two are independent.
    ///
    /// **D-10.4:** see [`Self::set_global_bypass`]'s identical note — the product path now reaches
    /// this through [`Chain::apply`] and a `global.output_ceiling_db` [`ParamChange`].
    pub fn set_output_ceiling_db(&mut self, db: f32) {
        self.output_ceiling_linear = namir_core::db_to_linear(db);
    }

    /// FR-CHAIN-080's fault counter: how many `process` calls (not how many faulted samples —
    /// see `CrossCuttingState::scan_and_clamp`'s doc comment) have produced at least one
    /// NaN/infinite sample since this `Chain` was constructed. Stays `0` forever on a chain that
    /// never calls `prepare_crosscutting`, since the scan that would increment it never runs.
    pub fn fault_count(&self) -> u64 {
        self.fault_count
    }

    /// Runs every stage in order, on the audio thread (RT) — unless global bypass (FR-CHAIN-030)
    /// is active, in which case the bypass path runs instead. Either way, once cross-cutting is
    /// active (`prepare_crosscutting` has been called), the block this produces is then scanned
    /// for NaN/Inf (FR-CHAIN-080) and ceiling-clamped (FR-CHAIN-090) before returning. See
    /// `prepare_crosscutting`'s doc comment for why a chain built via `Chain::new` and never
    /// prepared for cross-cutting skips all of that and behaves exactly as before this feature
    /// existed.
    pub fn process(&mut self, io: &mut StageIo<'_>) {
        if self.global_bypass {
            if let Some(cross_cutting) = self.cross_cutting.as_mut() {
                cross_cutting.apply_bypass(io);
            } else {
                // No ring to bypass through (prepare_crosscutting was never called): today's
                // behaviour, unchanged. See set_global_bypass's doc comment.
                for stage in &mut self.stages {
                    stage.process(io);
                }
            }
        } else {
            for stage in &mut self.stages {
                stage.process(io);
            }
        }

        if let Some(cross_cutting) = self.cross_cutting.as_mut() {
            let ceiling_linear = self.output_ceiling_linear;
            cross_cutting.scan_and_clamp(io, ceiling_linear, &mut self.fault_count);
        }
    }

    /// Resets every stage's internal state, e.g. on transport stop/reposition.
    pub fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
    }

    /// Each stage's delay accumulates serially through the chain — stage *i+1* receives stage
    /// *i*'s already-delayed output — so this is a plain sum.
    pub fn latency_samples(&self) -> u32 {
        self.stages.iter().map(|s| s.latency_samples()).sum()
    }

    /// Deliberately not a sum. "Tail" is how long a stage keeps producing non-negligible output
    /// after its *own* input goes silent (e.g. convolution/reverb decay). For a chain, the tail
    /// that reaches the chain's output is whichever internal stage's tail takes longest to
    /// *exit* — and a tail produced partway through the chain still has to cross every later
    /// stage's latency before it does.
    ///
    /// So stage `i` contributes `tail_i + sum(latency_j for j after i)`, and the chain's tail is
    /// the **max** over stages, not the sum: these are delayed views of the *same* physical
    /// decay reaching the output at different times, not independent decays that stack. Summing
    /// would be the right model if two stages independently re-decayed the *same* signal — e.g.
    /// two convolution/reverb stages in series, where the true combined tail is closer to the
    /// sum of both impulse-response lengths — but 1.0's six-stage chain has at most one stage
    /// with a nonzero tail (the IR stage), so that compounding case doesn't arise yet. If RD-2
    /// ever puts two tail-bearing stages in series, this is the first place to revisit.
    pub fn tail_samples(&self) -> u32 {
        let mut downstream_latency = 0u32;
        let mut max_contribution = 0u32;
        for stage in self.stages.iter().rev() {
            let contribution = stage.tail_samples().saturating_add(downstream_latency);
            max_contribution = max_contribution.max(contribution);
            downstream_latency = downstream_latency.saturating_add(stage.latency_samples());
        }
        max_contribution
    }

    /// D-10.4: first checks `change` against the chain's own two descriptors
    /// (`global.bypass`/`global.output_ceiling_db` — [`GLOBAL_BYPASS_ID`]/[`OUTPUT_CEILING_DB_ID`])
    /// before falling back to broadcasting to every stage, exactly mirroring how a stage's own
    /// `apply` matches its ids. This is the one place `Chain` itself, rather than a `Stage`, owns
    /// a `ParamId` — before D-10.4 these two values had no `ParamChange` routing at all and were
    /// only reachable through [`Self::set_global_bypass`]/[`Self::set_output_ceiling_db`] directly
    /// (still called here, so the two setters remain the single place that actually mutates the
    /// fields).
    ///
    /// A change that matches neither is broadcast to every stage. RD-2's per-instance parameter
    /// addressing (D-10.2) is future work by design — 1.0's fixed chain has no ambiguity to
    /// resolve, so each stage just ignores ids it doesn't own.
    pub fn apply(&mut self, change: ParamChange) {
        if change.id == GLOBAL_BYPASS_ID {
            // Stepped param value is the index as f32 (`ParamChange`'s own doc comment); index 1
            // is "On" per `GLOBAL_BYPASS`'s descriptor -- the same `>= 0.5` convention
            // `stages/trim.rs`'s `DC_BLOCKER_ENABLED` handling uses.
            self.set_global_bypass(change.value >= 0.5);
            return;
        }
        if change.id == OUTPUT_CEILING_DB_ID {
            self.set_output_ceiling_db(change.value);
            return;
        }
        for stage in &mut self.stages {
            stage.apply(change);
        }
    }

    /// D-8.1 step 2: broadcasts one prepared resource, exactly as [`Chain::apply`] broadcasts a
    /// parameter, stopping as soon as a stage takes it.
    ///
    /// **`offer` is still `Some` on return if no stage wanted it**, and the caller then owns
    /// D-8.1's never-drop obligation for it — the chain does not discard a resource it could not
    /// place. For 1.0's fixed six-stage chain that cannot happen (there is exactly one Nam stage
    /// and one Ir stage), but RD-2's dynamic chain could omit either, so the contract is stated
    /// and handled rather than assumed away.
    pub fn offer(&mut self, offer: &mut Option<Resource>) {
        for stage in &mut self.stages {
            stage.accept_resource(offer);
            if offer.is_none() {
                return;
            }
        }
    }

    /// M5's mirror of [`Chain::offer`]: broadcasts an unload request for `kind` to every stage,
    /// exactly as [`Chain::apply`] broadcasts a parameter change. Unlike `offer` there is no
    /// payload to stop early for — `kind` names a stage rather than carrying a resource that one
    /// stage removes from circulation — so every stage sees the call and each ignores a `kind`
    /// it does not own.
    pub fn unload(&mut self, kind: ResourceKind) {
        for stage in &mut self.stages {
            stage.unload_resource(kind);
        }
    }

    /// D-8.1 step 4: gives every stage the chance to move a finished resource into the return
    /// ring. Cheap when there is nothing to retire — one `Option::is_none()` check per stage.
    pub fn collect_retired(&mut self, out: &mut RetireSink<'_>) {
        for stage in &mut self.stages {
            stage.collect_retired(out);
        }
    }

    /// D-7.3: drains every stage's current readings into `out`, then adds the chain's own.
    ///
    /// FR-CHAIN-080's fault counter is one of the four signals D-7.3 names explicitly ("meters,
    /// gate reduction, fault flags, xrun counts") and had no route off the audio thread until M4.
    ///
    /// Note the ordering this implies, deliberately: `process` increments `fault_count` *after*
    /// the stage loop, so telemetry published later in the same block reports a count that already
    /// includes this block's fault. That is the useful answer — a fault should surface on the
    /// block it happened, not one block late — so do not "fix" the ordering.
    pub fn telemetry(&self, out: &mut TelemetrySink<'_>) {
        for stage in &self.stages {
            stage.telemetry(out);
        }
        out.push(TelemetryEntry {
            id: TELEMETRY_FAULT_COUNT,
            value: self.fault_count as f32,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::ParamId;
    use crate::prepare::PrepareContext;
    use crate::rt_harness::audio_section;
    use crate::stage::StagePrep;
    use crate::test_support::{ConstantTail, FixedGainPrep, GAIN_PARAM_ID};
    use namir_core::{ChannelConfig, SampleRate};

    fn ctx() -> PrepareContext {
        PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, ChannelConfig::Mono).unwrap()
    }

    #[test]
    fn empty_chain_has_zero_latency_and_tail() {
        let chain = Chain::new(Vec::new());
        assert_eq!(chain.latency_samples(), 0);
        assert_eq!(chain.tail_samples(), 0);
    }

    #[test]
    fn latency_sums_across_stages() {
        let stages: Vec<Box<dyn Stage>> = vec![
            Box::new(ConstantTail {
                latency: 10,
                tail: 0,
            }),
            Box::new(ConstantTail {
                latency: 5,
                tail: 0,
            }),
        ];
        let chain = Chain::new(stages);
        assert_eq!(chain.latency_samples(), 15);
    }

    #[test]
    fn tail_of_a_single_stage_passes_through_unchanged() {
        let stages: Vec<Box<dyn Stage>> = vec![Box::new(ConstantTail {
            latency: 0,
            tail: 100,
        })];
        let chain = Chain::new(stages);
        assert_eq!(chain.tail_samples(), 100);
    }

    #[test]
    fn tail_from_an_earlier_stage_gains_downstream_latency() {
        // Stage 1 has the tail; stage 2 has no tail but adds latency the tail must cross.
        let stages: Vec<Box<dyn Stage>> = vec![
            Box::new(ConstantTail {
                latency: 0,
                tail: 100,
            }),
            Box::new(ConstantTail {
                latency: 20,
                tail: 0,
            }),
        ];
        let chain = Chain::new(stages);
        assert_eq!(chain.tail_samples(), 120);
    }

    #[test]
    fn tail_is_the_max_contribution_not_the_sum() {
        // Stage 1's contribution: 100 + 20 (downstream latency) = 120.
        // Stage 2's contribution: 30 + 0 = 30.
        // A sum (150, or 120 + 30) would overcount: these are the same input's decay observed
        // at two points, not two independent decays.
        let stages: Vec<Box<dyn Stage>> = vec![
            Box::new(ConstantTail {
                latency: 0,
                tail: 100,
            }),
            Box::new(ConstantTail {
                latency: 20,
                tail: 30,
            }),
        ];
        let chain = Chain::new(stages);
        assert_eq!(chain.tail_samples(), 120);
    }

    #[test]
    fn later_stage_tail_can_dominate() {
        let stages: Vec<Box<dyn Stage>> = vec![
            Box::new(ConstantTail {
                latency: 5,
                tail: 10,
            }),
            Box::new(ConstantTail {
                latency: 0,
                tail: 200,
            }),
        ];
        let chain = Chain::new(stages);
        assert_eq!(chain.tail_samples(), 200);
    }

    #[test]
    fn apply_broadcasts_to_every_stage() {
        let prep = FixedGainPrep { gain_db: 0.0 };
        let a = prep.prepare(&ctx()).unwrap();
        let b = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(a), Box::new(b)]);

        chain.apply(ParamChange {
            id: GAIN_PARAM_ID,
            value: 6.0,
        });

        let mut left = [1.0f32; 4];
        let mut channels: [&mut [f32]; 1] = [&mut left];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));

        // Both stages picked up the change, so gain was applied twice (cascaded).
        let expected = namir_core::db_to_linear(6.0) * namir_core::db_to_linear(6.0);
        for s in io.channel(0) {
            assert!((*s - expected).abs() < 1e-4);
        }
    }

    #[test]
    fn apply_ignores_unrelated_ids() {
        let prep = FixedGainPrep { gain_db: 0.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);

        chain.apply(ParamChange {
            id: ParamId(999),
            value: 6.0,
        });

        let mut left = [1.0f32; 4];
        let mut channels: [&mut [f32]; 1] = [&mut left];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));
        for s in io.channel(0) {
            assert!((*s - 1.0).abs() < 1e-6);
        }
    }

    // --- FR-CHAIN-030/080/090: cross-cutting features below this point. All exercise the new
    // `prepare_crosscutting` opt-in path; none of the 8 tests above call it, so they keep
    // covering the pre-existing, cross-cutting-inactive behaviour unchanged. ---

    /// Local test-only fake (this module's own convention, matching `test_support.rs`'s doc
    /// comment on why its fakes live next to their one use): writes a NaN into each channel's
    /// first sample on its *first* `process` call only, then behaves as a silent passthrough
    /// (does nothing) on every call after — lets a single test drive both "a fault happened" and
    /// "processing continued normally afterward" (FR-CHAIN-080) without a second stage type.
    struct NanOnce {
        injected: bool,
    }

    impl Stage for NanOnce {
        fn process(&mut self, io: &mut StageIo<'_>) {
            if !self.injected {
                self.injected = true;
                for channel in io.channels_mut() {
                    if let Some(first) = channel.first_mut() {
                        *first = f32::NAN;
                    }
                }
            }
        }
        fn reset(&mut self) {}
        fn latency_samples(&self) -> u32 {
            0
        }
        fn tail_samples(&self) -> u32 {
            0
        }
        fn apply(&mut self, _change: ParamChange) {}
        fn telemetry(&self, _out: &mut crate::telemetry::TelemetrySink<'_>) {}
    }

    #[test]
    fn prepare_crosscutting_bypass_is_unity_gain_passthrough_at_zero_latency() {
        // +6 dB stage: if bypass were merely "skip clamping" rather than "skip the stages
        // entirely", this would come out gained. Zero latency means the delay ring
        // (prepare_crosscutting builds one anyway) never needs to touch the buffer at all.
        // Values kept within the default 0 dBFS output ceiling (also active once
        // prepare_crosscutting runs) so that clamp can't be mistaken for a bypass bug.
        let prep = FixedGainPrep { gain_db: 6.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);
        chain.prepare_crosscutting(&ctx());
        chain.set_global_bypass(true);

        let mut left = [0.1f32, 0.2, 0.3, 0.4];
        let mut channels: [&mut [f32]; 1] = [&mut left];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));

        assert_eq!(io.channel(0), &[0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn prepare_crosscutting_bypass_delays_by_declared_latency_for_sample_alignment() {
        // ConstantTail::process is a no-op, so any change in the output can only have come from
        // the bypass path's own delay ring, not from the stage running. Values kept within the
        // default 0 dBFS output ceiling (see the zero-latency test's identical note) so that
        // clamp can't be mistaken for a delay-alignment bug.
        let stages: Vec<Box<dyn Stage>> = vec![Box::new(ConstantTail {
            latency: 3,
            tail: 0,
        })];
        let mut chain = Chain::new(stages);
        assert_eq!(chain.latency_samples(), 3);
        chain.prepare_crosscutting(&ctx());
        chain.set_global_bypass(true);

        let mut left = [0.1f32, 0.2, 0.3, 0.4, 0.5];
        let mut channels: [&mut [f32]; 1] = [&mut left];
        let mut io = StageIo::new(&mut channels, 5);
        audio_section(|| chain.process(&mut io));

        // First 3 samples are the ring's zero prefill; from sample 3 onward, output[n] ==
        // input[n - 3] -- exactly latency_samples() of alignment delay, unity gain otherwise.
        assert_eq!(io.channel(0), &[0.0, 0.0, 0.0, 0.1, 0.2]);
    }

    #[test]
    fn fault_detection_zeroes_whole_block_then_processing_continues_next_call() {
        let mut chain = Chain::new(vec![Box::new(NanOnce { injected: false })]);
        chain.prepare_crosscutting(&ctx());

        let mut buf = [1.0f32, 2.0, 3.0, 4.0];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));

        for s in io.channel(0) {
            assert_eq!(
                *s, 0.0,
                "a single NaN must silence the *whole* block, not just the offending sample"
            );
        }
        assert_eq!(chain.fault_count(), 1);

        // FR-CHAIN-080: "continue processing subsequent blocks" -- the next call, with clean
        // input, must produce ordinary output and must not re-increment the fault counter.
        let mut buf2 = [0.25f32, 0.25, 0.25, 0.25];
        let mut channels2: [&mut [f32]; 1] = [&mut buf2];
        let mut io2 = StageIo::new(&mut channels2, 4);
        audio_section(|| chain.process(&mut io2));

        for s in io2.channel(0) {
            assert!((*s - 0.25).abs() < 1e-6);
        }
        assert_eq!(
            chain.fault_count(),
            1,
            "a clean block must not increment the fault counter again"
        );
    }

    #[test]
    fn output_ceiling_clamps_magnitude_preserving_sign() {
        // x10 linear (+20 dB), comfortably over a -6 dB ceiling in both directions.
        let prep = FixedGainPrep { gain_db: 20.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);
        chain.prepare_crosscutting(&ctx());
        chain.set_output_ceiling_db(-6.0);
        let ceiling = namir_core::db_to_linear(-6.0);

        let mut buf = [1.0f32, -1.0, 0.01, -0.01];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));

        let out = io.channel(0);
        assert!(
            (out[0] - ceiling).abs() < 1e-5,
            "positive overshoot must clamp to +ceiling"
        );
        assert!(
            (out[1] - (-ceiling)).abs() < 1e-5,
            "negative overshoot must clamp to -ceiling, preserving sign"
        );
        // 0.01 * 10 = 0.1, well under the ~0.501 ceiling: must pass through unclamped.
        assert!((out[2] - 0.1).abs() < 1e-5);
        assert!((out[3] - (-0.1)).abs() < 1e-5);
    }

    #[test]
    fn cross_cutting_process_does_not_allocate_in_either_path() {
        // Bypass path, nonzero latency (exercises the delay ring).
        let stages: Vec<Box<dyn Stage>> = vec![Box::new(ConstantTail {
            latency: 4,
            tail: 0,
        })];
        let mut chain = Chain::new(stages);
        chain.prepare_crosscutting(&ctx());
        chain.set_global_bypass(true);

        let mut buf = [0.1f32; 64];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 64);
        audio_section(|| chain.process(&mut io));

        // Normal (non-bypassed) path, cross-cutting still active: exercises the fault scan and
        // ceiling clamp instead of the bypass ring.
        chain.set_global_bypass(false);
        let mut buf2 = [0.1f32; 64];
        let mut channels2: [&mut [f32]; 1] = [&mut buf2];
        let mut io2 = StageIo::new(&mut channels2, 64);
        audio_section(|| chain.process(&mut io2));
    }

    // --- D-10.4: `apply` now routes `global.bypass`/`global.output_ceiling_db` `ParamChange`s
    // the same way it routes any stage's own parameters, instead of only being reachable through
    // `set_global_bypass`/`set_output_ceiling_db` directly. These mirror the two
    // `prepare_crosscutting`/`output_ceiling_clamps_magnitude_preserving_sign` tests above, driven
    // through `apply` instead, to prove the new path produces the identical effect. ---

    #[test]
    fn apply_routes_global_bypass_param_change_to_the_bypass_path() {
        // +6 dB stage: if `apply`'s GLOBAL_BYPASS_ID handling didn't actually flip
        // `global_bypass`, this would come out gained rather than passed straight through -- the
        // same "unity gain passthrough" signature
        // `prepare_crosscutting_bypass_is_unity_gain_passthrough_at_zero_latency` checks against
        // `set_global_bypass` directly.
        let prep = FixedGainPrep { gain_db: 6.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);
        chain.prepare_crosscutting(&ctx());

        chain.apply(ParamChange {
            id: GLOBAL_BYPASS_ID,
            value: 1.0, // Stepped index 1 == "On", per GLOBAL_BYPASS's descriptor.
        });

        let mut left = [0.1f32, 0.2, 0.3, 0.4];
        let mut channels: [&mut [f32]; 1] = [&mut left];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));

        assert_eq!(io.channel(0), &[0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn apply_routes_global_bypass_off_value_back_through_the_stage_path() {
        // The inverse of the test above: index 0 ("Off") through `apply` must leave the chain
        // running its stages, not stuck bypassed from a prior change.
        let prep = FixedGainPrep { gain_db: 6.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);
        chain.prepare_crosscutting(&ctx());
        chain.set_global_bypass(true);

        chain.apply(ParamChange {
            id: GLOBAL_BYPASS_ID,
            value: 0.0,
        });

        // Small input: with the stage's +6 dB applied (bypass off), 0.1 * db_to_linear(6.0) stays
        // comfortably under the default 0 dBFS output ceiling that `prepare_crosscutting` also
        // activates -- a larger input here would have this test's own gain clamp against that
        // ceiling instead of exercising the bypass-off path it means to check.
        let mut left = [0.1f32; 4];
        let mut channels: [&mut [f32]; 1] = [&mut left];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));

        let expected = 0.1 * namir_core::db_to_linear(6.0);
        for s in io.channel(0) {
            assert!((*s - expected).abs() < 1e-4, "got {s}, expected {expected}");
        }
    }

    #[test]
    fn apply_routes_output_ceiling_param_change_to_the_clamp() {
        // Same setup and assertions as `output_ceiling_clamps_magnitude_preserving_sign`, but the
        // ceiling arrives through `apply` rather than `set_output_ceiling_db` directly.
        let prep = FixedGainPrep { gain_db: 20.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);
        chain.prepare_crosscutting(&ctx());

        chain.apply(ParamChange {
            id: OUTPUT_CEILING_DB_ID,
            value: -6.0,
        });
        let ceiling = namir_core::db_to_linear(-6.0);

        let mut buf = [1.0f32, -1.0];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 2);
        audio_section(|| chain.process(&mut io));

        let out = io.channel(0);
        assert!((out[0] - ceiling).abs() < 1e-5);
        assert!((out[1] - (-ceiling)).abs() < 1e-5);
    }

    #[test]
    fn apply_does_not_broadcast_global_ids_to_stages() {
        // A stage that panics if `apply` ever reaches it with any id -- proves `Chain::apply`
        // truly intercepts GLOBAL_BYPASS_ID/OUTPUT_CEILING_DB_ID rather than merely handling them
        // *in addition to* the broadcast.
        struct PanicsOnApply;
        impl Stage for PanicsOnApply {
            fn process(&mut self, _io: &mut StageIo<'_>) {}
            fn reset(&mut self) {}
            fn latency_samples(&self) -> u32 {
                0
            }
            fn tail_samples(&self) -> u32 {
                0
            }
            fn apply(&mut self, change: ParamChange) {
                panic!("stage should never see a chain-level id, got {change:?}");
            }
            fn telemetry(&self, _out: &mut TelemetrySink<'_>) {}
        }

        let mut chain = Chain::new(vec![Box::new(PanicsOnApply)]);
        chain.apply(ParamChange {
            id: GLOBAL_BYPASS_ID,
            value: 1.0,
        });
        chain.apply(ParamChange {
            id: OUTPUT_CEILING_DB_ID,
            value: -3.0,
        });
    }
}
