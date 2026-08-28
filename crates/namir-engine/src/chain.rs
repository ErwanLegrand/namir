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

/// Ceiling on the bypass-compensation delay [`Chain::prepare_crosscutting`] pre-sizes each
/// channel's line to, expressed in milliseconds of the engine rate.
///
/// **Why a ceiling at all (issue #58).** The chain's latency is not fixed at preparation: a NAM
/// model whose declared rate differs from the engine's engages `stages/nam.rs`'s `SlotResampler`
/// the moment it is installed, and FR-CLAP-040 names exactly that as a runtime latency change.
/// The compensation therefore has to track `Chain::latency_samples()` *per block*, and the only
/// way to do that without allocating on the audio thread (P1) is to allocate once, generously,
/// for a latency the chain will not exceed.
///
/// 250 ms is that figure. The largest latency anything in the 1.0 chain can report is one
/// `SlotResampler`'s (a few hundred samples — 640 for a 44.1 kHz model in a 48 kHz engine, the
/// configuration `chain_probes.rs` measures), so this is roughly two orders of magnitude of
/// headroom; a chain whose latency exceeded a quarter of a second would be unusable as a live
/// amp simulator long before this line ran out. A latency above the ceiling is clamped rather
/// than allowed to allocate or panic (D-16.3) — see [`DelayLine::run`].
const MAX_BYPASS_COMPENSATION_MS: f64 = 250.0;

/// One channel's bypass-compensation delay: a fixed-capacity circular buffer, written on **every**
/// block (both paths — see [`CrossCuttingState::run_delay`]) and read back `delay` samples late
/// only while bypass is engaged.
///
/// A circular `Vec` rather than the `VecDeque` this used to be, because the delay is now a
/// per-block input rather than a constant fixed at preparation: a `VecDeque` expresses "delay by
/// exactly its own length", so changing the delay would mean resizing it, which is an allocation
/// on the audio thread. Indexing a buffer whose length is the *maximum* delay expresses any delay
/// up to that maximum at no cost.
struct DelayLine {
    /// Capacity is `max delay + 1`, so the read index can trail the write index by the maximum
    /// delay without colliding with it. Never resized after construction.
    buf: Vec<f32>,
    /// Where the next sample will be written.
    write: usize,
}

impl DelayLine {
    /// **Not RT-safe** (allocates once, at preparation).
    fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0.0; capacity.max(1)],
            write: 0,
        }
    }

    /// Pushes every sample of `channel` into the line, in order, and — when `emit_delayed` — also
    /// replaces each with the sample written `delay` positions earlier.
    ///
    /// **RT-safe:** no allocation, no branch whose bound depends on anything but `channel.len()`,
    /// and one modulo for the whole block rather than one per sample. A `delay` above what this
    /// line was sized for is clamped rather than allowed to index out of bounds (D-16.3: degrade,
    /// don't panic on the audio thread); see [`MAX_BYPASS_COMPENSATION_MS`] for why that cannot
    /// happen for any chain this project ships.
    fn run(&mut self, channel: &mut [f32], delay: usize, emit_delayed: bool) {
        let cap = self.buf.len();
        let delay = delay.min(cap - 1);
        let mut write = self.write;
        // Trails `write` by `delay`, so the value read at each step is the one written `delay`
        // steps ago. At `delay == 0` the two indices coincide and the read would be stale by a
        // whole buffer — hence the `delay > 0` guard below; a zero-delay bypass wants the input
        // unchanged anyway.
        let mut read = (write + cap - delay) % cap;
        for sample in channel.iter_mut() {
            let delayed = self.buf[read];
            self.buf[write] = *sample;
            if emit_delayed && delay > 0 {
                *sample = delayed;
            }
            write += 1;
            if write == cap {
                write = 0;
            }
            read += 1;
            if read == cap {
                read = 0;
            }
        }
        self.write = write;
    }
}

/// Non-RT-allocated state that only exists once [`Chain::prepare_crosscutting`] has run:
/// FR-CHAIN-030's per-channel latency-compensation delay for the bypass path, and the
/// [`PrepareContext`](crate::prepare::PrepareContext) the chain was prepared against, which is
/// what lets [`crate::AudioEngine::process`] check the block it is handed instead of trusting it
/// (issue #60).
struct CrossCuttingState {
    /// One line per channel (`ctx.channel_config().output_channels()` many — `stage_io.rs`'s own
    /// doc comment: `StageIo`'s channel count is fixed for the whole chain to that figure).
    delay_lines: Vec<DelayLine>,
    /// The context `prepare_crosscutting` was called with. See [`Chain::prepared_for`].
    prepared_for: crate::prepare::PrepareContext,
}

impl CrossCuttingState {
    /// FR-CHAIN-030's bypass path, and its always-on other half.
    ///
    /// **Every block feeds the line, whether bypass is engaged or not (issue #59).** Writing it
    /// only while bypassed left it holding whatever the *last* bypass period ended with (zeros,
    /// the first time), so engaging bypass emitted `delay` samples of stale content followed by a
    /// hard discontinuity, and disengaging dropped the same number of samples — a click at both
    /// ends of every transition, which is exactly what FR-CLAP-060 forbids. Feeding it always
    /// costs one pass over the block on the non-bypassed path (nothing at all when the chain
    /// reports zero latency, which is the whole of 1.0 with no resampled model loaded) and makes
    /// the transition sample-accurate in both directions.
    ///
    /// `delay` is read from the chain's *current* `latency_samples()` on every block rather than
    /// cached at preparation, so a model change that alters the reported latency (FR-CLAP-040)
    /// moves the compensation with it — issue #58.
    fn run_delay(&mut self, io: &mut StageIo<'_>, delay: usize, bypassed: bool) {
        if delay == 0 && !bypassed {
            // Nothing to record and nothing to emit: the line can only ever hand back what it is
            // given, so skipping it is not a state divergence.
            return;
        }
        for (line, channel) in self.delay_lines.iter_mut().zip(io.channels_mut()) {
            line.run(channel, delay, bypassed);
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
    ///
    /// **`apply_ceiling` is false on the bypass path (issue #61).** FR-CHAIN-090 is a statement
    /// about "the output stage"; FR-CHAIN-030 is a statement about a path that does not run the
    /// output stage at all, and its own `Verify:` method — bypassed output minus delayed input is
    /// silence to within −120 dBFS — is simply false above 0 dBFS if the default ceiling clamps
    /// the bypassed signal. The two requirements collide only on the bypass path, and
    /// FR-CHAIN-030 wins there because "routes input to output with unity gain" leaves no room
    /// for a gain of anything else. The NaN scan still runs: fault containment (FR-CHAIN-080) is
    /// about not sending a damaging non-finite sample to hardware, which the bypass path can do
    /// just as easily as the stage path.
    fn scan_and_clamp(
        &mut self,
        io: &mut StageIo<'_>,
        ceiling_linear: f32,
        apply_ceiling: bool,
        fault_count: &mut u64,
    ) {
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
        if !apply_ceiling {
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
        // Sized to the ceiling, not to today's `latency_samples()` (issue #58): with nothing
        // loaded that figure is 0, and installing a resampled model raises it *after* this call
        // has returned. `max` rather than a bare conversion so a chain that somehow already
        // reports more than the ceiling still gets a line long enough for it.
        let ceiling =
            (ctx.sample_rate().hz_f64() * MAX_BYPASS_COMPENSATION_MS / 1000.0).ceil() as usize;
        let capacity = ceiling.max(self.latency_samples() as usize) + 1;
        let delay_lines = (0..channel_count)
            .map(|_| DelayLine::new(capacity))
            .collect();
        self.cross_cutting = Some(CrossCuttingState {
            delay_lines,
            prepared_for: *ctx,
        });
    }

    /// The [`PrepareContext`](crate::prepare::PrepareContext) this chain was prepared against, or
    /// `None` on a chain built through [`Chain::new`] alone (see `prepare_crosscutting`'s doc
    /// comment for why that path is deliberately raw).
    ///
    /// Exists so [`crate::AudioEngine::process`] can check the `StageIo` it is handed against the
    /// block size and channel count every stage sized its buffers to, rather than trusting a
    /// caller and panicking inside a stage when the trust is misplaced (issue #60).
    pub fn prepared_for(&self) -> Option<crate::prepare::PrepareContext> {
        self.cross_cutting.as_ref().map(|cc| cc.prepared_for)
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
        // Read *this block's* latency rather than a figure cached at preparation (issue #58):
        // installing a model whose declared rate differs from the engine's raises it mid-session,
        // which is the runtime change FR-CLAP-040 names. Six `Stage::latency_samples()` calls,
        // each a field read behind a vtable — cheap enough to pay per block, and the alternative
        // is a compensation that silently stops matching what the host was told.
        let latency = self.latency_samples() as usize;
        let bypassed = self.global_bypass;

        let prepared = self.cross_cutting.is_some();
        if let Some(cross_cutting) = self.cross_cutting.as_mut() {
            // Runs on both paths — see `run_delay`'s doc comment (issue #59).
            cross_cutting.run_delay(io, latency, bypassed);
        }
        if !bypassed || !prepared {
            // No line to bypass through (prepare_crosscutting was never called): today's
            // behaviour, unchanged. See set_global_bypass's doc comment.
            for stage in &mut self.stages {
                stage.process(io);
            }
        }

        if let Some(cross_cutting) = self.cross_cutting.as_mut() {
            let ceiling_linear = self.output_ceiling_linear;
            // The ceiling is an output-stage statement and the bypass path does not run the
            // output stage; the NaN scan applies to both. See `scan_and_clamp` (issue #61).
            cross_cutting.scan_and_clamp(io, ceiling_linear, !bypassed, &mut self.fault_count);
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

    /// One half of FR-CHAIN-030 pinned exactly; the requirement's own null-test method is
    /// executed by `bypassed_output_nulls_against_delayed_input_to_within_120_dbfs` below, which
    /// carries the tag.
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

    /// FR-CHAIN-030's own `Verify:` method, executed as written: "null test: bypassed output
    /// minus delayed input is silence to within -120 dBFS". The two tests above each pin one
    /// half of the requirement's sentence with an exact four/five-sample comparison; neither
    /// subtracts a delayed input from a bypassed output, and neither spans both latency cases.
    /// This one does both, over 512 samples of a deterministic non-trivial signal pushed through
    /// in 64-sample blocks, so the compensation ring is exercised *across* block boundaries as
    /// well as within one — including a declared latency longer than the block size, where the
    /// null depends on the ring carrying samples over several calls.
    ///
    /// The +6 dB stage ahead of the delay-declaring one is the unity-gain half: a bypass that
    /// merely skipped clamping, or that ran the stages and then delayed, could not null. Signal
    /// amplitude stays at 0.5 so FR-CHAIN-090's 0 dBFS ceiling (active from
    /// `prepare_crosscutting` onward) cannot clip it and be mistaken for a null.
    // trace: FR-CHAIN-030
    #[test]
    fn bypassed_output_nulls_against_delayed_input_to_within_120_dbfs() {
        const BLOCK: usize = 64;
        const BLOCKS: usize = 8;
        const TOTAL: usize = BLOCK * BLOCKS;

        // -120 dBFS as a linear amplitude: the null floor the requirement's method names.
        let null_floor = namir_core::db_to_linear(-120.0);

        // Zero latency (nothing to compensate for), a latency shorter than one block, and one
        // longer than a block so the ring must carry samples between `process` calls.
        for latency in [0u32, 3, 97] {
            let input: Vec<f32> = (0..TOTAL)
                .map(|n| {
                    let t = n as f32 / 48_000.0;
                    0.25 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                        + 0.25 * (2.0 * std::f32::consts::PI * 3_001.0 * t).sin()
                })
                .collect();

            let stages: Vec<Box<dyn Stage>> = vec![
                Box::new(FixedGainPrep { gain_db: 6.0 }.prepare(&ctx()).unwrap()),
                Box::new(ConstantTail { latency, tail: 0 }),
            ];
            let mut chain = Chain::new(stages);
            assert_eq!(chain.latency_samples(), latency);
            chain.prepare_crosscutting(&ctx());
            chain.set_global_bypass(true);

            let mut output = Vec::with_capacity(TOTAL);
            for block in input.chunks(BLOCK) {
                let mut buffer = block.to_vec();
                {
                    let mut channels: [&mut [f32]; 1] = [&mut buffer];
                    let mut io = StageIo::new(&mut channels, block.len());
                    audio_section(|| chain.process(&mut io));
                }
                output.extend_from_slice(&buffer);
            }
            assert_eq!(output.len(), TOTAL);

            // The delayed input: `latency` samples of silence, then the input itself. Only the
            // alignment delay FR-CHAIN-030 permits, and nothing else.
            let delay = latency as usize;
            let delayed_input: Vec<f32> = std::iter::repeat_n(0.0f32, delay)
                .chain(input.iter().copied())
                .take(TOTAL)
                .collect();

            let peak_residual = output
                .iter()
                .zip(&delayed_input)
                .map(|(out, delayed)| (out - delayed).abs())
                .fold(0.0f32, f32::max);
            assert!(
                peak_residual <= null_floor,
                "bypassed output minus input delayed by {latency} samples peaked at \
                 {peak_residual:e}, above the -120 dBFS null floor {null_floor:e}"
            );
        }
    }

    /// **No FR-CHAIN-080 tag any more** (M14). `NanOnce` writes into an *output* buffer at the end
    /// of a chain of one, so this reaches no product stage's state and never executes the
    /// requirement's "inject a NaN into each stage's state". It still proves the containment
    /// mechanism itself — whole block silenced, counter incremented exactly once, next block
    /// normal — which is why it stays; the requirement resolves through `crate::chain_probes`,
    /// which puts a NaN into each of the six product stages in turn.
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

    // trace: FR-CHAIN-090
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

    // --- Issues #58/#59/#61: the bypass path's three defects, one test each. All three are
    // about the *same* delay line, so they share `VariableLatency` and `run_blocks` below. ---

    /// Id `VariableLatency` answers to. Any value `Chain::apply` does not recognise itself is
    /// broadcast to every stage, so this needs only to differ from the two chain-level ids.
    const LATENCY_PARAM_ID: ParamId = ParamId(4242);

    /// A stage whose *declared* latency changes at runtime, which is what `NamStage` does the
    /// moment a model whose declared rate differs from the engine's is installed (FR-CLAP-040,
    /// `stages/nam.rs`'s `SlotResampler`). `process` is a no-op, so anything the output shows can
    /// only have come from the chain's own compensation.
    struct VariableLatency {
        latency: u32,
    }

    impl Stage for VariableLatency {
        fn process(&mut self, _io: &mut StageIo<'_>) {}
        fn reset(&mut self) {}
        fn latency_samples(&self) -> u32 {
            self.latency
        }
        fn tail_samples(&self) -> u32 {
            0
        }
        fn apply(&mut self, change: ParamChange) {
            if change.id == LATENCY_PARAM_ID {
                self.latency = change.value as u32;
            }
        }
        fn telemetry(&self, _out: &mut TelemetrySink<'_>) {}
    }

    /// Drives `input` through `chain` in `block`-frame blocks inside the RT harness, calling
    /// `at_block` before each one so a test can flip bypass or a parameter mid-stream.
    fn run_blocks(
        chain: &mut Chain,
        input: &[f32],
        block: usize,
        mut at_block: impl FnMut(usize, &mut Chain),
    ) -> Vec<f32> {
        let mut out = Vec::with_capacity(input.len());
        for (i, chunk) in input.chunks(block).enumerate() {
            at_block(i, chain);
            let mut buf = chunk.to_vec();
            {
                let mut channels: [&mut [f32]; 1] = [&mut buf];
                let mut io = StageIo::new(&mut channels, chunk.len());
                audio_section(|| chain.process(&mut io));
            }
            out.extend_from_slice(&buf);
        }
        out
    }

    /// **Issue #58.** `CrossCuttingState` used to cache `Chain::latency_samples()` at
    /// `prepare_crosscutting` and size a `VecDeque` to exactly that. `build_default_chain` calls
    /// that once, with nothing loaded, so the cached figure is always 0 — and
    /// `NamStage::latency_samples()` becomes nonzero later, the moment a model at a different
    /// declared rate is installed, which FR-CLAP-040 names explicitly as a runtime latency change.
    /// The chain then reported a nonzero latency to the host while compensating for none of it.
    ///
    /// Committed red-first: before the fix the assertion below fails on the very first compared
    /// sample, because the bypassed output is the *undelayed* input.
    #[test]
    fn bypass_compensation_follows_a_latency_change_made_after_prepare() {
        const BLOCK: usize = 16;
        const LATENCY: usize = 5;
        const CHANGE_AT: usize = 2;

        let mut chain = Chain::new(vec![Box::new(VariableLatency { latency: 0 })]);
        chain.prepare_crosscutting(&ctx());
        chain.set_global_bypass(true);
        assert_eq!(
            chain.latency_samples(),
            0,
            "the line is sized while the chain still reports zero -- that is the whole setup"
        );

        // A ramp: every sample distinct, so a misalignment of even one sample is visible.
        let input: Vec<f32> = (0..BLOCK * 8).map(|n| 0.001 * n as f32).collect();
        let output = run_blocks(&mut chain, &input, BLOCK, |i, chain| {
            if i == CHANGE_AT {
                chain.apply(ParamChange {
                    id: LATENCY_PARAM_ID,
                    value: LATENCY as f32,
                });
            }
        });

        assert_eq!(chain.latency_samples(), LATENCY as u32);
        for n in CHANGE_AT * BLOCK..input.len() {
            let expected = input[n - LATENCY];
            assert!(
                (output[n] - expected).abs() < 1e-6,
                "sample {n}: bypassed output {} against an input delayed by the {LATENCY} samples \
                 the chain now reports ({expected})",
                output[n]
            );
        }
    }

    /// **Issue #59.** The delay line used to be written only while bypass was engaged, so it held
    /// whatever the *last* bypass period ended with — zeros, the first time. Engaging bypass then
    /// emitted `latency_samples` of that stale content followed by a hard discontinuity, and
    /// disengaging dropped the same number of samples: a click at both ends of every transition,
    /// which is exactly what FR-CLAP-060 ("sample-accurate and click-free, equivalent to
    /// FR-CHAIN-030") forbids.
    ///
    /// Three phases, because the third is what proves the fix rather than merely restating it:
    /// bypass off (the line must be filling), bypass on (the first `LATENCY` samples must be the
    /// last `LATENCY` samples of the *previous, unbypassed* block), bypass off again, then on
    /// again (the line must still be coherent across a period it was not being read from).
    ///
    /// Committed red-first: before the fix, phase two's first three samples are 0.0.
    #[test]
    fn engaging_bypass_emits_the_real_signal_rather_than_stale_ring_content() {
        const BLOCK: usize = 8;
        const LATENCY: usize = 3;

        // `ConstantTail::process` is a no-op, so the unbypassed path is an exact passthrough and
        // every difference between the two paths is the compensation line alone.
        let mut chain = Chain::new(vec![Box::new(ConstantTail {
            latency: LATENCY as u32,
            tail: 0,
        })]);
        chain.prepare_crosscutting(&ctx());

        let input: Vec<f32> = (0..BLOCK * 4).map(|n| 0.01 * (n + 1) as f32).collect();
        let output = run_blocks(&mut chain, &input, BLOCK, |i, chain| {
            // off, on, off, on.
            chain.set_global_bypass(i % 2 == 1);
        });

        // Phase 0 (bypass off): a no-op stage passes the input straight through.
        assert_eq!(&output[..BLOCK], &input[..BLOCK]);
        // Phase 1 (bypass on): delayed by LATENCY, and the samples that delay reaches back for
        // are real input from phase 0 -- not the zeros a line written only while bypassed holds.
        for n in BLOCK..2 * BLOCK {
            assert!(
                (output[n] - input[n - LATENCY]).abs() < 1e-6,
                "sample {n}: engaging bypass emitted {} instead of the input delayed by \
                 {LATENCY} ({})",
                output[n],
                input[n - LATENCY]
            );
        }
        // Phase 2 (bypass off again): passthrough once more.
        assert_eq!(&output[2 * BLOCK..3 * BLOCK], &input[2 * BLOCK..3 * BLOCK]);
        // Phase 3 (bypass on again): the line stayed coherent through a period nothing read it.
        for n in 3 * BLOCK..4 * BLOCK {
            assert!(
                (output[n] - input[n - LATENCY]).abs() < 1e-6,
                "sample {n}: re-engaging bypass emitted {} instead of {}",
                output[n],
                input[n - LATENCY]
            );
        }
    }

    /// **Issue #61.** `scan_and_clamp` used to run in full on the bypass path, so FR-CHAIN-090's
    /// ceiling (default 0 dBFS) clipped a bypassed signal — and FR-CHAIN-030's own `Verify:`
    /// method, the null test, is simply false for any input above that ceiling. The two bypass
    /// tests above this one keep their amplitudes deliberately under it and say so in comments,
    /// so the behaviour was known and untested.
    ///
    /// This is `bypassed_output_nulls_against_delayed_input_to_within_120_dbfs` at an amplitude
    /// that ceiling would clip, plus the converse — the clamp must still apply when bypass is
    /// *off*, so the fix cannot be "stop clamping".
    ///
    /// Committed red-first: before the fix the residual peaks at ~0.5 (the clipped half of a 1.5
    /// peak), roughly 114 dB above the −120 dBFS floor.
    #[test]
    fn bypass_does_not_clamp_a_signal_above_the_output_ceiling() {
        const BLOCK: usize = 64;
        const TOTAL: usize = BLOCK * 8;
        const LATENCY: usize = 7;
        let null_floor = namir_core::db_to_linear(-120.0);

        // Peak 1.5, comfortably above the default 0 dBFS ceiling `prepare_crosscutting` activates.
        let input: Vec<f32> = (0..TOTAL)
            .map(|n| {
                let t = n as f32 / 48_000.0;
                1.5 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
            })
            .collect();

        let mut chain = Chain::new(vec![Box::new(ConstantTail {
            latency: LATENCY as u32,
            tail: 0,
        })]);
        chain.prepare_crosscutting(&ctx());
        chain.set_global_bypass(true);
        let output = run_blocks(&mut chain, &input, BLOCK, |_, _| {});

        let delayed: Vec<f32> = std::iter::repeat_n(0.0f32, LATENCY)
            .chain(input.iter().copied())
            .take(TOTAL)
            .collect();
        let peak_residual = output
            .iter()
            .zip(&delayed)
            .map(|(o, d)| (o - d).abs())
            .fold(0.0f32, f32::max);
        assert!(
            peak_residual <= null_floor,
            "bypassed output minus delayed input peaked at {peak_residual:e}, above the \
             -120 dBFS null floor {null_floor:e}: the output ceiling is clipping a path that \
             FR-CHAIN-030 says routes input to output at unity gain"
        );

        // The converse: with bypass off, the ceiling still applies. Fixing #61 must not have
        // turned FR-CHAIN-090 off.
        chain.set_global_bypass(false);
        let clamped = run_blocks(&mut chain, &input, BLOCK, |_, _| {});
        let peak = clamped.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak <= 1.0 + 1e-6,
            "the non-bypassed path must still clamp to the 0 dBFS default, peaked at {peak}"
        );
    }

    /// FR-CHAIN-080 is *not* what issue #61 turns off on the bypass path: a non-finite sample must
    /// still silence the block and raise the fault counter, whichever path produced it.
    #[test]
    fn fault_containment_still_runs_on_the_bypass_path() {
        let mut chain = Chain::new(Vec::new());
        chain.prepare_crosscutting(&ctx());
        chain.set_global_bypass(true);

        let mut buf = [1.0f32, f32::NAN, 3.0, 4.0];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));

        for s in io.channel(0) {
            assert_eq!(
                *s, 0.0,
                "a NaN reaching the bypass path must still silence the block"
            );
        }
        assert_eq!(chain.fault_count(), 1);
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

        // Normal (non-bypassed) path, cross-cutting still active: exercises the fault scan, the
        // ceiling clamp, and -- since issue #59 -- the delay line being *fed* while bypass is off,
        // which is the one path in `process` that is new work on every block of ordinary playback.
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
