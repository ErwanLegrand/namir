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
/// [`Chain::prepare_crosscutting`] without one silently no-oping — they just record intent. See
/// `prepare_crosscutting`'s own doc comment for why that call is opt-in rather than folded into
/// `new`.
///
/// **What that call does and does not gate (issue #36).** It gates FR-CHAIN-080's NaN scan,
/// FR-CHAIN-090's ceiling, and FR-CHAIN-030's *latency compensation* — the three things that need
/// state allocated off the audio thread. It does **not** gate the bypass itself. It used to: a
/// chain with `global_bypass` set but no `cross_cutting` ran every stage anyway, which is a global
/// bypass that does not bypass, and the failure was indistinguishable from working because audio
/// kept flowing. `output_ceiling_linear` has no such trap — an unclamped block is a block, not a
/// contradiction in terms — so it stays gated.
pub struct Chain {
    stages: Vec<Box<dyn Stage>>,
    /// FR-CHAIN-030: when `true`, `process` routes the block to the output instead of running
    /// `stages` — unconditionally, whether or not `cross_cutting` is `Some` (issue #36). RT-safe
    /// to flip (see `set_global_bypass`) since it is read, never allocated, on the audio thread.
    ///
    /// Since issue #142 this is the *target* of `CrossCuttingState::mix`'s 15 ms crossfade rather
    /// than a routing switch read directly per block; on an unprepared chain, which has no blend
    /// state, it still routes outright.
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

/// Time constant of the dry/wet blend [`Chain::process`] runs across a global-bypass change
/// (issue #142), in milliseconds.
///
/// **The same 15 ms figure every *per-stage* bypass already uses** — `stages/gate.rs`,
/// `stages/nam.rs` and `stages/ir.rs` each declare a `BYPASS_CROSSFADE_TIME_CONSTANT_MS` of 15.0
/// for FR-CHAIN-020's click-free per-stage bypass. Before #142 the global bypass, which is the one
/// a host actually automates, was the only one in the chain that stepped: `set_global_bypass`
/// flipped a `bool` and `process` switched paths on it between one sample and the next. The
/// internal inconsistency is what made that a defect rather than a scope decision, so the fix
/// takes the figure the rest of the chain already agreed on rather than inventing a second one.
///
/// Deliberately *not* pulled out into a shared constant in `namir-dsp` or `namir-params`: the
/// per-stage figure is each stage's own documented engineering default (see `gate.rs`'s note that
/// it is "not derived from an FRS requirement"), and hoisting four independent defaults into one
/// knob would assert a coupling nothing has asked for. Matching the value is the point; sharing
/// the definition is not.
const BYPASS_CROSSFADE_TIME_CONSTANT_MS: f64 = 15.0;

/// One channel's bypass-compensation delay: a fixed-capacity circular buffer, written on **every**
/// block (both paths — see [`CrossCuttingState::capture_dry`]) and read back `delay` samples late
/// whenever the bypass side of the blend contributes to the block at all — which, since issue
/// #142's crossfade, is a window 15 ms wider than "while bypass is engaged" at each end.
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

    /// Pushes every sample of `input` into the line, in order, and — when `dry` is `Some` — writes
    /// the sample recorded `delay` positions earlier into it.
    ///
    /// **Writes the delayed signal somewhere else rather than over `input` (issue #142).** Until
    /// the global bypass learned to crossfade, the only two states were "bypassed" (emit delayed,
    /// in place) and "not" (record only), so the line could overwrite the block it was handed.
    /// A blend needs *both* the delayed dry and the stages' own output for the same frames, and
    /// the stages must be fed the undelayed input they have always been fed, so the dry copy now
    /// goes to caller-owned scratch (`CrossCuttingState::dry`, sized at preparation).
    ///
    /// **RT-safe:** no allocation, no branch whose bound depends on anything but `input.len()`,
    /// and one modulo for the whole block rather than one per sample. A `delay` above what this
    /// line was sized for is clamped rather than allowed to index out of bounds (D-16.3: degrade,
    /// don't panic on the audio thread); see [`MAX_BYPASS_COMPENSATION_MS`] for why that cannot
    /// happen for any chain this project ships.
    fn run(&mut self, input: &[f32], delay: usize, dry: Option<&mut [f32]>) {
        let cap = self.buf.len();
        let delay = delay.min(cap - 1);
        let mut write = self.write;
        // Trails `write` by `delay`, so the value read at each step is the one written `delay`
        // steps ago. At `delay == 0` the two indices coincide and the read would be stale by a
        // whole buffer — hence the `delay > 0` arm below; a zero-delay bypass wants the input
        // unchanged anyway.
        let mut read = (write + cap - delay) % cap;
        match dry {
            Some(dry) => {
                for (sample, dry) in input.iter().zip(dry.iter_mut()) {
                    *dry = if delay > 0 { self.buf[read] } else { *sample };
                    self.buf[write] = *sample;
                    write += 1;
                    if write == cap {
                        write = 0;
                    }
                    read += 1;
                    if read == cap {
                        read = 0;
                    }
                }
            }
            // Record-only: this block's output owes the bypass side nothing, but the line is
            // still fed (issue #59) so the next one that does can reach back into real signal.
            None => {
                for sample in input {
                    self.buf[write] = *sample;
                    write += 1;
                    if write == cap {
                        write = 0;
                    }
                }
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
    /// Per-channel scratch holding *this* block's delayed dry signal, so the blend has something
    /// to fade the stages' output against. One `Vec<f32>` per output channel, each sized to
    /// `ctx.max_block_size()` in `prepare_crosscutting`; never resized in `process`. Exactly the
    /// shape — and the reason — `stages/gate.rs`'s own `dry` field has.
    dry: Vec<Vec<f32>>,
    /// Current blend position: `0.0` = fully engaged (the block is the stage chain's output),
    /// `1.0` = fully bypassed (the block is the delayed dry). Advances toward its target by
    /// `mix_coeff` each sample, never jumps (issue #142).
    ///
    /// **One `f32`, not one per channel**, and that is deliberate: every channel recomputes the
    /// same trajectory from this same starting value inside [`Self::blend`] and only the last
    /// channel's endpoint is committed back here, so the channels stay in phase by construction
    /// rather than by four separate states happening to agree. `stages/gate.rs`'s `mix` carries
    /// the identical convention and its `process` says so in as many words.
    mix: f32,
    /// One-pole coefficient for `mix`, computed once in `prepare_crosscutting` from
    /// [`BYPASS_CROSSFADE_TIME_CONSTANT_MS`] and the sample rate.
    mix_coeff: f32,
    /// `false` until the first `process` call after preparation, which snaps `mix` to its target
    /// instead of fading into it — see that call's own comment for why (there is no previously
    /// rendered sample for a fade to be continuous with).
    started: bool,
    /// The context `prepare_crosscutting` was called with. See [`Chain::prepared_for`].
    prepared_for: crate::prepare::PrepareContext,
}

impl CrossCuttingState {
    /// FR-CHAIN-030's bypass path, and its always-on other half: feeds every channel's delay line
    /// from the block as handed in, and — when `capture` — leaves that channel's delayed copy in
    /// [`Self::dry`] for [`Self::emit_dry`] or [`Self::blend`] to use. The block itself is left
    /// untouched, so the stages still receive the undelayed input they always have.
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
    fn capture_dry(&mut self, io: &mut StageIo<'_>, delay: usize, capture: bool) {
        if delay == 0 && !capture {
            // Nothing to record and nothing to hand back: the line can only ever return what it
            // is given, so skipping it is not a state divergence.
            return;
        }
        let frames = io.frames();
        for ((line, dry), channel) in self
            .delay_lines
            .iter_mut()
            .zip(self.dry.iter_mut())
            .zip(io.channels_mut())
        {
            let dry = if capture {
                Some(&mut dry[..frames])
            } else {
                None
            };
            line.run(channel, delay, dry);
        }
    }

    /// The settled bypass path: this block *is* the delayed dry, copied over the block verbatim.
    ///
    /// Deliberately a copy rather than [`Self::blend`] evaluated at `mix == 1.0`: the two agree
    /// arithmetically, but a copy puts nothing at all between the delay line and the output, so
    /// FR-CHAIN-030's null test still nulls to the bit and the settled path is exactly the one
    /// that shipped before issue #142's crossfade existed.
    fn emit_dry(&self, io: &mut StageIo<'_>) {
        let frames = io.frames();
        for (dry, channel) in self.dry.iter().zip(io.channels_mut()) {
            channel.copy_from_slice(&dry[..frames]);
        }
    }

    /// Issue #142's crossfade: replaces the block (the stages' output, "wet") with its per-sample
    /// blend against [`Self::dry`], `mix` advancing one one-pole step per sample toward `target`.
    ///
    /// **The ceiling is applied to the wet term inside the blend, not to the blended result.**
    /// Issue #61 established that FR-CHAIN-090's ceiling is a statement about the output stage,
    /// which the bypass path does not run, and that FR-CHAIN-030's unity gain wins where the two
    /// collide. Clamping the *blend* would honour neither endpoint: a dry signal above the
    /// ceiling would be clipped for the whole fade and then jump to its true amplitude the sample
    /// the fade completed — the very click this method exists to remove. Clamping the wet term
    /// first is continuous in `mix` and exact at both ends: at `0.0` the block is the clamped
    /// stage output, at `1.0` it is the untouched dry.
    ///
    /// **Why it snaps.** An `f32` one-pole never reaches its target: the increment falls below
    /// half an ulp and the addition stops moving, some 2e-5 short at 48 kHz — which would leave
    /// FR-CHAIN-030's "routes input to output with unity gain" permanently ~94 dB approximate.
    /// So the last step is taken outright once the remainder is no larger than `mix_coeff`, one
    /// ordinary step of the fade itself, which bounds the snap by the fade's own steepest sample
    /// and keeps it from being the discontinuity.
    fn blend(&mut self, io: &mut StageIo<'_>, target: f32, ceiling_linear: f32) {
        let frames = io.frames();
        let start = self.mix;
        let mut end = start;
        for (dry, wet) in self.dry.iter().zip(io.channels_mut()) {
            let mut m = start;
            for (wet, dry) in wet.iter_mut().zip(dry[..frames].iter()) {
                m += self.mix_coeff * (target - m);
                let clamped = wet.clamp(-ceiling_linear, ceiling_linear);
                *wet = clamped * (1.0 - m) + dry * m;
            }
            end = m;
        }
        self.mix = if (target - end).abs() <= self.mix_coeff {
            target
        } else {
            end
        };
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
    ///
    /// It is also false *during* issue #142's crossfade, for a different reason: there the
    /// ceiling has already been applied, to the wet term alone, inside [`Self::blend`] — see that
    /// method's doc comment for why the clamp goes there rather than over the blended block.
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
    /// Deliberately leaves `cross_cutting` at `None` — FR-CHAIN-080/090, and FR-CHAIN-030's
    /// latency compensation, stay inactive until [`Chain::prepare_crosscutting`] is called
    /// explicitly (the bypass itself is not gated on it; see this struct's own doc comment and
    /// issue #36). See that method's doc comment for
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
    /// call onward, `process` also applies FR-CHAIN-030's *latency compensation* on the bypass
    /// path, FR-CHAIN-080 (NaN/Inf -> silence + fault flag), and FR-CHAIN-090 (output ceiling
    /// clamp). Before this call, `process` behaves exactly as it always has, save that global
    /// bypass now bypasses rather than running every stage (issue #36) — see `Chain::new`'s doc
    /// comment.
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
        // Issue #142's blend: one dry scratch buffer per channel, `max_block_size` long, plus the
        // one-pole coefficient for a `BYPASS_CROSSFADE_TIME_CONSTANT_MS` fade at this rate. Both
        // are computed here, off the audio thread, exactly as every stage's own bypass blend
        // computes them in its `prepare` (`stages/gate.rs`).
        let tau_samples = (BYPASS_CROSSFADE_TIME_CONSTANT_MS / 1000.0) * ctx.sample_rate().hz_f64();
        let mix_coeff = (1.0 - (-1.0_f64 / tau_samples).exp()) as f32;
        let dry = vec![vec![0.0; ctx.max_block_size()]; channel_count];
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
            dry,
            // Seeded at "engaged", but the first `process` after this call overwrites it with
            // whatever `global_bypass` says by then — see `started`, and `process`'s own comment.
            mix: 0.0,
            mix_coeff,
            started: false,
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
    /// **What "takes effect" means since issue #142.** This sets the *target* of a 15 ms dry/wet
    /// blend, not the block's routing: `process` runs both sides for the length of the fade and
    /// crossfades between them, so the change begins at the very next sample and completes about
    /// 100 ms later. It used to be a single-sample step, which is the click FR-CLAP-060 forbids —
    /// and every *per-stage* bypass in the chain had faded over the same 15 ms since M2, so the
    /// one parameter a host actually automates was the one that stepped.
    ///
    /// On a prepared chain or an unprepared one (issue #36), the bypass itself takes effect —
    /// but on an unprepared one it still steps. What
    /// [`prepare_crosscutting`](Chain::prepare_crosscutting) adds is the latency-compensation
    /// delay *and, since #142, the crossfade*: both need buffers allocated off the audio thread,
    /// and without them a bypassed block is the input undelayed and unblended, which is unity-gain
    /// passthrough but neither sample-aligned nor click-free. Until M14 the bypass was
    /// gated on that call and an unprepared chain ran every stage while nominally bypassed.
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
    /// is settled on, in which case the block passes to the output unmodified instead. Either
    /// way, once cross-cutting is active (`prepare_crosscutting` has been called), the block this
    /// produces is then scanned for NaN/Inf (FR-CHAIN-080) and ceiling-clamped (FR-CHAIN-090)
    /// before returning, and the bypass path is delayed by the chain's reported latency. See
    /// `prepare_crosscutting`'s doc comment for what a chain built via `Chain::new` and never
    /// prepared for cross-cutting skips — the bypass is not on that list (issue #36).
    ///
    /// # The three shapes a block can have (issue #142)
    ///
    /// A bypass change is a 15 ms crossfade, so between the two settled states there is a third,
    /// transitional one in which *both* sides are computed:
    ///
    /// 1. **Settled engaged** (`mix == 0`, bypass off) — the stage chain's output, ceiling-clamped.
    ///    Byte for byte what this method did before #142 existed.
    /// 2. **Settled bypassed** (`mix == 1`, bypass on) — the delayed dry, copied out. The stages
    ///    are not run at all, so this costs no more than it used to, and no arithmetic stands
    ///    between the delay line and the output.
    /// 3. **In transition** — the stages run *and* the delayed dry is captured, and the block is
    ///    the per-sample blend of the two. This is the only shape that costs more than before, it
    ///    lasts about 100 ms per change, and it is the whole of the fix.
    pub fn process(&mut self, io: &mut StageIo<'_>) {
        // Read *this block's* latency rather than a figure cached at preparation (issue #58):
        // installing a model whose declared rate differs from the engine's raises it mid-session,
        // which is the runtime change FR-CLAP-040 names. Six `Stage::latency_samples()` calls,
        // each a field read behind a vtable — cheap enough to pay per block, and the alternative
        // is a compensation that silently stops matching what the host was told.
        let latency = self.latency_samples() as usize;
        let bypassed = self.global_bypass;

        // Bypass is not conditional on `cross_cutting` (issue #36). With no ring and no dry
        // scratch built there is nothing to compensate the chain's latency with and nothing to
        // blend against, so an unprepared bypass is the input undelayed and unfaded rather than
        // the input delayed and crossfaded — but it is still the *input*, which is the whole of
        // what "bypass" claims. Running every stage instead, as this used to when
        // `prepare_crosscutting` had not been called, is the one reading the word cannot bear.
        let Some(cross_cutting) = self.cross_cutting.as_mut() else {
            if !bypassed {
                for stage in &mut self.stages {
                    stage.process(io);
                }
            }
            return;
        };

        let target = if bypassed { 1.0 } else { 0.0 };
        if !cross_cutting.started {
            // The first block after preparation starts settled wherever the bypass already is,
            // rather than fading into it: nothing has been rendered yet, so there is no previous
            // sample for a fade to be continuous with, and a host that activates the plugin with
            // bypass already on (restored with the session, say) wants that block bypassed, not
            // 15 ms of the chain it asked to skip. Same reasoning, in the same words, as
            // `stages/gate.rs`'s "no prior audio exists yet at stage creation; start settled".
            cross_cutting.started = true;
            cross_cutting.mix = target;
        }
        let mix = cross_cutting.mix;
        // Which side of the blend this block owes anything to. Both, unless the fade is settled
        // on one of its two endpoints — which is every block that is not inside a transition.
        let dry_contributes = mix > 0.0 || target > 0.0;
        let wet_contributes = mix < 1.0 || target < 1.0;

        // Feeds the delay line on both paths — see `capture_dry`'s doc comment (issue #59).
        cross_cutting.capture_dry(io, latency, dry_contributes);

        if wet_contributes {
            for stage in &mut self.stages {
                stage.process(io);
            }
        }

        let ceiling_linear = self.output_ceiling_linear;
        let cross_cutting = self
            .cross_cutting
            .as_mut()
            .expect("checked at the top of this call");
        match (dry_contributes, wet_contributes) {
            // Settled engaged. The ceiling is an output-stage statement and this is the path that
            // runs the output stage; the NaN scan applies to every path. See `scan_and_clamp`.
            (false, _) => {
                cross_cutting.scan_and_clamp(io, ceiling_linear, true, &mut self.fault_count);
            }
            // Settled bypassed: no ceiling (issue #61), no arithmetic, still scanned for faults.
            (true, false) => {
                cross_cutting.emit_dry(io);
                cross_cutting.scan_and_clamp(io, ceiling_linear, false, &mut self.fault_count);
            }
            // In transition: the ceiling is applied to the wet term inside `blend`, so the scan
            // that follows must not clamp the blended block on top of it.
            (true, true) => {
                cross_cutting.blend(io, target, ceiling_linear);
                cross_cutting.scan_and_clamp(io, ceiling_linear, false, &mut self.fault_count);
            }
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

    // --- Issues #58/#59/#61/#142: the bypass path's four defects. The first three are about the
    // *same* delay line and the fourth is about the blend that now reads from it, so all of them
    // share `VariableLatency` and `run_blocks` below. ---

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
    /// Four phases, because the fourth is what proves the fix rather than merely restating it:
    /// bypass off (the line must be filling), bypass on (the samples the delay reaches back for
    /// must be real input from the previous, unbypassed phase), off again, then on again (the
    /// line must still be coherent across a period nothing read it).
    ///
    /// Committed red-first: before the fix, phase one's first three samples are 0.0.
    ///
    /// **Rewritten for issue #142's crossfade, and this is the one place the fade genuinely
    /// costs coverage.** A blend that opens at `mix_coeff` attenuates the first bypassed samples
    /// — precisely the ones a stale ring corrupts — by about 720x, so an assertion on the settled
    /// output alone would no longer notice the bug at all: after 15 ms of bypass the ring holds
    /// bypass-period content whether or not it was fed while engaged. The early samples are
    /// therefore compared against the *closed-form* blend instead of against the delayed input
    /// directly (a one-pole from a settled endpoint has one: `m_k = 1 - (1 - coeff)^k`), and each
    /// comparison is paired with a check that the dry term it depends on is larger than the
    /// tolerance — so a line handing back silence still fails, by ~80x the bound.
    #[test]
    fn engaging_bypass_emits_the_real_signal_rather_than_stale_ring_content() {
        const BLOCK: usize = 64;
        const LATENCY: usize = 3;
        // One phase of off/on/off/on. Longer than the crossfade's settling window (`blend`'s doc
        // comment: about 99 ms at 48 kHz) so every phase contains a whole transition *and* ends
        // settled, and a whole number of blocks so a phase change lands on a block boundary.
        const PHASE: usize = 8_192;
        // How many samples after each engagement are compared against the closed-form blend. Any
        // prefix would do; five is enough to span the whole `LATENCY` and short enough to read.
        const PROBE: usize = 5;

        // `ConstantTail::process` is a no-op, so the unbypassed path is an exact passthrough and
        // every difference between the two paths is the compensation line alone.
        let mut chain = Chain::new(vec![Box::new(ConstantTail {
            latency: LATENCY as u32,
            tail: 0,
        })]);
        chain.prepare_crosscutting(&ctx());

        // Distinct within any 61-sample window, so a misalignment of one to three samples shows,
        // and bounded well under the 0 dBFS ceiling `blend` applies to its wet term.
        let input: Vec<f32> = (0..PHASE * 4)
            .map(|n| 0.01 * ((n % 61) + 1) as f32)
            .collect();
        let output = run_blocks(&mut chain, &input, BLOCK, |i, chain| {
            // off, on, off, on.
            chain.set_global_bypass((i * BLOCK / PHASE) % 2 == 1);
        });

        // Phase 0 (bypass off): a no-op stage passes the input straight through, from the first
        // sample — the block a `prepare_crosscutting`d chain starts settled on.
        assert_eq!(&output[..BLOCK], &input[..BLOCK]);

        let tau_samples = (BYPASS_CROSSFADE_TIME_CONSTANT_MS / 1000.0) * 48_000.0;
        let coeff = (1.0 - (-1.0_f64 / tau_samples).exp()) as f32;
        for start in [PHASE, 3 * PHASE] {
            let mut m = 0.0f32;
            for k in 0..PROBE {
                let n = start + k;
                m += coeff * (1.0 - m);
                let wet = input[n]; // the no-op stage's own output
                let dry = input[n - LATENCY]; // what the line must hand back
                let expected = wet * (1.0 - m) + dry * m;
                assert!(
                    (output[n] - expected).abs() <= 1e-7,
                    "sample {n}: engaging bypass emitted {} instead of the blend ({expected}) of \
                     the stage output ({wet}) and the input delayed by {LATENCY} ({dry}) at mix \
                     {m}",
                    output[n]
                );
                assert!(
                    (expected - wet * (1.0 - m)).abs() > 1e-6,
                    "sample {n}: the dry term is within the tolerance above, so this comparison \
                     would pass against a line that handed back silence — the check is vacuous"
                );
            }
            // ... and the phase ends settled on the delayed input, to the bit.
            for n in start + PHASE - BLOCK..start + PHASE {
                assert_eq!(
                    output[n],
                    input[n - LATENCY],
                    "sample {n}: a settled bypass must be the delayed input exactly"
                );
            }
        }

        // Phase 2 (bypass off again): passthrough once more, by the end of its own transition.
        for n in 3 * PHASE - BLOCK..3 * PHASE {
            assert_eq!(
                output[n], input[n],
                "sample {n}: releasing bypass must settle on the \
                 stage path"
            );
        }
    }

    /// **Issue #36.** `process` used to gate the bypass on `cross_cutting.is_some()`
    /// (`if !bypassed || !prepared { run every stage }`), so a chain that never had
    /// `prepare_crosscutting` called on it **ran the whole chain while nominally bypassed** — a
    /// global bypass that does not bypass. Nothing detected it: audio keeps flowing, so the only
    /// symptom is a bypass button that appears to do nothing, which a user attributes to their host
    /// or their own routing.
    ///
    /// Both product shells reach `process` only through `build_default_chain`, whose last statement
    /// before returning is `prepare_crosscutting`, so this was a latent trap rather than a shipped
    /// defect — but "latent" was the only thing standing between the two, and it was a property of
    /// a call ordering rather than of anything checked. Bypass now means bypass on both paths:
    /// `prepare_crosscutting` adds FR-CHAIN-030's *latency compensation*, not the passthrough
    /// itself, and an unprepared chain's `latency_samples()` is uncompensated exactly as an
    /// unprepared chain gets no NaN scan and no ceiling.
    ///
    /// Committed red-first: before the fix the first assertion reads `db_to_linear(6.0)` (≈2.0)
    /// against the input's 0.25 — the stage ran.
    #[test]
    fn global_bypass_bypasses_on_a_chain_that_never_prepared_crosscutting() {
        let prep = FixedGainPrep { gain_db: 6.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);
        assert_eq!(
            chain.prepared_for(),
            None,
            "the whole point of this test is the chain `Chain::new` alone leaves behind"
        );
        chain.set_global_bypass(true);

        let input = [0.25f32, -0.5, 0.75, -0.125];
        let mut buf = input;
        {
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, 4);
            audio_section(|| chain.process(&mut io));
        }
        assert_eq!(
            buf, input,
            "a bypassed chain must route its input to its output unmodified, whether or not \
             `prepare_crosscutting` has run"
        );

        // The converse, so the fix cannot be "an unprepared chain never runs its stages": with
        // bypass released the same chain gains by the same +6 dB it always did.
        chain.set_global_bypass(false);
        let mut buf = input;
        {
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, 4);
            audio_section(|| chain.process(&mut io));
        }
        let gain = namir_core::db_to_linear(6.0);
        for (out, inp) in buf.iter().zip(&input) {
            assert!(
                (out - inp * gain).abs() < 1e-4,
                "releasing bypass on an unprepared chain must run the stages again: {out} vs {}",
                inp * gain
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
        /// Passes of `input` (512 frames each) run after releasing bypass and before measuring,
        /// to clear issue #142's crossfade — about 4 740 frames at 48 kHz, so twelve passes is
        /// a little over 6 000 and the measured pass is fully settled.
        const SETTLING_RUNS: usize = 12;
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
        //
        // **Settled first, since issue #142.** Releasing bypass now fades onto the stage path
        // over about 100 ms instead of switching onto it, and for the length of that fade the
        // block still carries the dry term — which is exactly what this test's first half proves
        // the ceiling must not touch. So the peak is legitimately above the ceiling until the
        // fade completes. The assertion is about the *engaged* path, so the fade is skipped
        // rather than the bound loosened: loosening it would stop testing #61's converse at all.
        chain.set_global_bypass(false);
        for _ in 0..SETTLING_RUNS {
            run_blocks(&mut chain, &input, BLOCK, |_, _| {});
        }
        let clamped = run_blocks(&mut chain, &input, BLOCK, |_, _| {});
        let peak = clamped.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak <= 1.0 + 1e-6,
            "the non-bypassed path must still clamp to the 0 dBFS default, peaked at {peak}"
        );
    }

    /// **Issue #142.** `set_global_bypass` used to flip a `bool` that `process` switched paths on
    /// between one sample and the next, so automating global bypass — the parameter a host is
    /// most likely to automate — stepped, while every *per-stage* bypass in the chain has faded
    /// over 15 ms since M2 (FR-CHAIN-020). FR-CLAP-060 asks the plugin's bypass to be
    /// "sample-accurate and click-free, equivalent to FR-CHAIN-030"; this is the click-free half,
    /// measured where the defect actually lived.
    ///
    /// The bound is `stages/gate.rs`'s, in its own words: **no single sample may move further
    /// than a linear 15 ms ramp over the same range would**, which a one-pole of the same time
    /// constant clears by about 0.1%. A constant input and a fixed-gain stage make the whole of
    /// every observed delta the crossfade's own — there is no signal movement to subtract — and
    /// each phase's first delta is measured from the *last settled sample of the previous phase*,
    /// which is where the old single-sample step was.
    ///
    /// Both directions, because a fade that only ran one way would still click on the other, and
    /// each direction's settled endpoint is asserted exactly: the dry side to the bit (that is
    /// FR-CHAIN-030's unity gain, and `blend`'s snap is what makes it exact rather than 94 dB
    /// approximate), the wet side to within the stage's own tolerance.
    ///
    /// Committed red-first: before the fix each transition's first sample moves the full range at
    /// once, 720x the bound below.
    #[test]
    fn global_bypass_crossfades_in_both_directions_rather_than_stepping() {
        const BLOCK: usize = 64;
        const DRY: f32 = 0.25;
        /// One phase, in frames: 250 ms at 48 kHz, comfortably past the crossfade's own settling
        /// window (`blend`'s doc comment: about 99 ms) so every phase ends settled.
        const PHASE: usize = 12_000;

        let prep = FixedGainPrep { gain_db: 6.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);
        chain.prepare_crosscutting(&ctx());

        // Constant, so every sample-to-sample movement below is the blend and nothing else. At
        // +6 dB the stage path sits at 0.5, inside FR-CHAIN-090's default 0 dBFS ceiling, so the
        // clamp `blend` applies to its wet term never fires and cannot be mistaken for the fade.
        let input = vec![DRY; PHASE];
        let engaged = run_blocks(&mut chain, &input, BLOCK, |_, _| {});
        chain.set_global_bypass(true);
        let engaging = run_blocks(&mut chain, &input, BLOCK, |_, _| {});
        chain.set_global_bypass(false);
        let releasing = run_blocks(&mut chain, &input, BLOCK, |_, _| {});

        let wet = DRY * namir_core::db_to_linear(6.0);
        assert!(
            (engaged[PHASE - 1] - wet).abs() < 1e-6,
            "the chain must start settled on the stage path, not fade onto it: {}",
            engaged[PHASE - 1]
        );
        assert_eq!(
            engaging[PHASE - 1],
            DRY,
            "an engaged bypass must settle on the dry signal exactly -- a one-pole alone stalls \
             about 2e-5 short, which is what `blend`'s snap is for"
        );
        assert!(
            (releasing[PHASE - 1] - wet).abs() < 1e-6,
            "releasing bypass must settle back on the stage path: {}",
            releasing[PHASE - 1]
        );

        // |wet - dry| is the blend's whole range; a linear ramp over 15 ms of it is the bound.
        let ideal_max_delta = (wet - DRY) / (0.015 * 48_000.0);
        for (name, previous, phase) in [
            ("engaging", engaged[PHASE - 1], &engaging),
            ("releasing", engaging[PHASE - 1], &releasing),
        ] {
            let mut previous = previous;
            let mut max_delta = 0.0f32;
            for &sample in phase.iter() {
                max_delta = max_delta.max((sample - previous).abs());
                previous = sample;
            }
            assert!(
                max_delta <= ideal_max_delta * 1.01,
                "{name}: max_delta={max_delta} exceeds the 15 ms linear ramp bound \
                 {ideal_max_delta} -- global bypass is stepping where every per-stage bypass fades"
            );
            assert!(max_delta > 0.0, "{name}: the blend never advanced");
        }
    }

    /// The one case that is deliberately *not* faded: the first block after preparation starts
    /// settled wherever `global_bypass` already is. Nothing has been rendered yet, so there is no
    /// previous sample for a fade to be continuous with, and a host that activates a plugin whose
    /// bypass was restored with the session wants that block bypassed rather than 15 ms of the
    /// chain it asked to skip. `stages/gate.rs` seeds its own bypass mix the same way and for the
    /// same reason.
    ///
    /// The converse is asserted in the same test, so "start settled" cannot quietly become
    /// "never fade": the *second* switch, on a chain that has rendered a block, does fade.

    #[test]
    fn the_first_block_after_preparation_starts_settled_rather_than_fading() {
        let prep = FixedGainPrep { gain_db: 6.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);
        chain.prepare_crosscutting(&ctx());
        chain.set_global_bypass(true);

        let input = [0.1f32, 0.2, 0.3, 0.4];
        let mut first = input;
        {
            let mut channels: [&mut [f32]; 1] = [&mut first];
            let mut io = StageIo::new(&mut channels, 4);
            audio_section(|| chain.process(&mut io));
        }
        assert_eq!(
            first, input,
            "the first block of a chain prepared with bypass already on must be the input \
             exactly, not the first 4 samples of a fade out of a chain that never ran"
        );

        // The converse: releasing bypass now, with a block already rendered, fades. The first
        // sample must still be far nearer the dry it is leaving than the +6 dB stage path.
        chain.set_global_bypass(false);
        let mut second = input;
        {
            let mut channels: [&mut [f32]; 1] = [&mut second];
            let mut io = StageIo::new(&mut channels, 4);
            audio_section(|| chain.process(&mut io));
        }
        let wet = input[0] * namir_core::db_to_linear(6.0);
        assert!(
            (second[0] - input[0]).abs() < (second[0] - wet).abs(),
            "a switch after the first block must fade, not step: got {} against a dry {} and a \
             stage path {wet}",
            second[0],
            input[0]
        );
        assert_ne!(
            second[0], input[0],
            "...and it must actually have started moving"
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

    /// All three shapes a `process` call can take (that method's own doc comment), each inside
    /// [`audio_section`]: settled bypassed, in transition, settled engaged. The middle one is
    /// issue #142's blend, which is the only one that touches both sides in the same block.
    #[test]
    fn cross_cutting_process_does_not_allocate_in_any_of_the_three_block_shapes() {
        // Bypass path, nonzero latency (exercises the delay ring). The first block starts settled
        // (`process`'s own comment), so this one is shape 2: the delayed dry, copied out.
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

        // Shape 3, the transition: with the mix settled at 1.0 and the target now 0.0, this block
        // captures the dry, runs the stages and blends the two -- every line issue #142 added.
        chain.set_global_bypass(false);
        let mut buf2 = [0.1f32; 64];
        let mut channels2: [&mut [f32]; 1] = [&mut buf2];
        let mut io2 = StageIo::new(&mut channels2, 64);
        audio_section(|| chain.process(&mut io2));

        // Shape 1, settled engaged: the fault scan, the ceiling clamp, and -- since issue #59 --
        // the delay line being *fed* while bypass is off, which is the one path in `process` that
        // is new work on every block of ordinary playback. Reached by letting the fade above run
        // to its snap, which is also the only way to reach it.
        let settling = vec![0.1f32; 12_000];
        run_blocks(&mut chain, &settling, 64, |_, _| {});
        let mut buf3 = [0.1f32; 64];
        let mut channels3: [&mut [f32]; 1] = [&mut buf3];
        let mut io3 = StageIo::new(&mut channels3, 64);
        audio_section(|| chain.process(&mut io3));
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
