//! The audio thread's half of D-7.2/D-7.3/D-8.1, and the worker's half, paired.
//!
//! # Why this is a new type rather than more fields on `Chain`
//!
//! **Decision:** the rings live in [`AudioEngine`], which owns a [`Chain`]; `Chain` itself is left
//! exactly as it was — a pure DSP object with no knowledge that threads exist.
//!
//! **Rationale:** three concrete reasons, in decreasing order of weight.
//!
//! 1. `benches/six_stage_chain.rs` measures `Chain::process` directly, and its figure is what
//!    NFR-PERF-010 was certified against in M3. Putting a ring drain inside `Chain::process` would
//!    silently change the thing that benchmark measures, and D-2.4 exists precisely because this
//!    project has learned how easily a performance figure gets contaminated.
//! 2. A dozen existing tests construct chains through `Chain::new(vec![..])` to exercise stage
//!    ordering, latency arithmetic and bypass. None of them wants a command ring, and requiring one
//!    would make every one of them carry irrelevant scaffolding.
//! 3. `chain.rs`'s own doc comment already said assembling the stage list "belongs to whatever owns
//!    the stage *list* (worker/adapter code, not yet built)". This is that owner, finally built.
//!
//! **Consequence:** there are now two entry points. [`build_default_engine`] is what a product
//! shell uses; `build_default_chain` stays for benchmarks and for tests that want a bare chain, and
//! is no longer the product path.
//!
//! # The per-block sequence, and why it collects retirements twice
//!
//! ```text
//! 1. retry any stalled offer      (never dropped, so it may still be held from last block)
//! 2. drain commands               (gated -- see `drain_commands`)
//! 3. collect_retired  (pass 1)    absorbs retirements caused by step 2's installs
//! 4. chain.process(io)            the actual audio
//! 5. collect_retired  (pass 2)    absorbs retirements caused by a crossfade completing in step 4
//! 6. publish telemetry
//! ```
//!
//! Two passes rather than a deeper parking buffer in each stage. A resource stage's retire pen
//! holds exactly one slot, and two different events can fill it in a single block: an install that
//! displaces a slot still fading in (step 2), and a crossfade reaching its end (step 4). Those can
//! genuinely collide — at a 2048-sample block size, a block is longer than the 20 ms fade, so an
//! offer installed at the top of a block can start *and* finish a fade inside the same block.
//! Collecting between the two events keeps "at most one thing is parked at any instant" true, which
//! is a far easier invariant to state, comment and test than any capacity above one. Each pass
//! costs six `Option::is_none()` checks.

use namir_params::ParamId as ParamsId;

use crate::chain::Chain;
use crate::command::{Command, CommandKind, RetireSink};
use crate::prepare::{PrepareContext, PrepareError};
use crate::resource::Resource;
use crate::ring::{RingConsumer, RingProducer, ring};
use crate::stage_io::StageIo;
use crate::telemetry::{TelemetryEntry, TelemetrySink};
use crate::telemetry_ring::{TelemetryProducer, TelemetryReader, telemetry_ring};

/// How many commands one block will drain before leaving the rest for the next one.
///
/// A bound is required, not a nicety: without one, a UI that enqueues faster than the audio thread
/// drains would let a single block do unbounded work, which is NFR-RT-010's "no unbounded loop" and
/// NFR-RT-040's "worst-case per-block time shall not depend on ... how long the engine has been
/// running". 64 is comfortably above any plausible per-block burst (a host sends a handful of
/// automation points per block, not dozens) while keeping the worst case small.
const MAX_COMMANDS_PER_BLOCK: usize = 64;

/// Free slots the return ring must have before an offer is allowed through: one for a displacement
/// the install may cause, one for the crossfade completion that follows it.
const RETIRE_HEADROOM_PER_OFFER: usize = 2;

/// Entries the per-block telemetry scratch is sized to. Today's real six-stage stereo chain emits
/// 17 (gate 1, trim 4, nam 2, ir 2, eq 0, out 4x2) plus 3 from the chain and engine; 64 leaves room
/// for growth without being wasteful. `TelemetrySink` overwrites *oldest* on overflow, so an
/// undersized buffer would silently starve whichever stage runs first rather than erroring — hence
/// the test that pins the real chain's count below this, not just this comment.
const TELEMETRY_SCRATCH_ENTRIES: usize = 64;

/// Telemetry: blocks in which the command drain stopped early. A persistently rising value is the
/// first thing to look at if a user reports a control that stopped responding.
const TELEMETRY_DEFERRED_BLOCKS: u32 = ParamsId::from_key("telemetry.engine.deferred_blocks").0;

/// Telemetry: whether a retirement is currently stuck because the return ring is full — i.e. the
/// worker is not draining (D-8.1's degradation case, made observable rather than silent).
const TELEMETRY_RETIRE_BACKLOG: u32 = ParamsId::from_key("telemetry.engine.retire_backlog").0;

/// Telemetry: blocks refused because the [`StageIo`] did not match the [`PrepareContext`] the
/// chain was prepared with (issue #60). Any nonzero value is a driver bug — the host handed a
/// block bigger than the maximum it declared, or a channel count the chain was never prepared
/// for — and the alternative to refusing was a panic inside a stage, on the audio thread.
const TELEMETRY_REJECTED_BLOCKS: u32 = ParamsId::from_key("telemetry.engine.rejected_blocks").0;

/// Ring capacities, fixed at preparation (D-7.2: "pre-allocated at preparation").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingCapacities {
    /// Inbound command ring. Generous: a burst of UI parameter moves must not stall the producer.
    pub commands: usize,
    /// D-8.1's return ring. Only needs depth for a few in-flight handovers; each consumes at most
    /// two slots — and [`split`] raises anything below that to
    /// [`RETIRE_HEADROOM_PER_OFFER`](self::RETIRE_HEADROOM_PER_OFFER), because a return ring
    /// shallower than one offer's headroom can never satisfy the drain gate (issue #57).
    pub retire: usize,
    /// D-7.3's telemetry ring. Rounded up to a power of two by [`telemetry_ring`].
    pub telemetry: usize,
}

impl Default for RingCapacities {
    fn default() -> Self {
        Self {
            commands: 256,
            retire: 8,
            telemetry: 256,
        }
    }
}

/// The audio thread's half. Moved onto the audio thread once, at activation.
///
/// `Send` but deliberately not `Sync`: the ring ends inside are `Send`-not-`Sync`, so the SPSC
/// contract ("exactly one thread at each end") is enforced by the type system rather than by a
/// comment.
pub struct AudioEngine {
    chain: Chain,
    commands: RingConsumer<Command>,
    retire: RingProducer<Resource>,
    telemetry: TelemetryProducer,
    /// Sized once in [`split`]; `Chain::telemetry` drains stages into this before it is published.
    telemetry_scratch: Vec<TelemetryEntry>,
    /// An offer no stage took, which the return ring was too full to accept. Held, never dropped,
    /// and it blocks the drain until it clears.
    stalled_offer: Option<Resource>,
    /// Set when a `collect_retired` pass rejected a push. Gates the next block's drain, so a stage
    /// is never handed an offer when it has nowhere to park what that offer displaces.
    retire_backlog: bool,
    deferred_blocks: u64,
    /// See [`TELEMETRY_REJECTED_BLOCKS`].
    rejected_blocks: u64,
}

/// The worker thread's half.
///
/// D-7.2's "mutex on the producer side only, serialising UI and worker submissions" is
/// deliberately **not** implemented here: this crate hands out exactly one producer, which *is* the
/// SPSC guarantee, and says nothing about how the worker shares it. Wrapping this in a mutex is
/// `namir-worker`'s job, because the sharing policy is a worker concern and the engine should not
/// impose one on a caller that has only a single submitting thread.
pub struct WorkerEndpoint {
    /// Submissions to the audio thread. Full means the audio thread has not drained yet; D-7.2
    /// says the producer waits and retries rather than dropping.
    pub commands: RingProducer<Command>,
    /// D-8.1 step 4's drain. **This must be drained regularly.** Everything popped from it is
    /// dropped on the worker's thread, which is the entire point of the ring.
    pub retire: RingConsumer<Resource>,
    /// D-7.3's readings, for the UI.
    pub telemetry: TelemetryReader,
}

/// Pairs a prepared [`Chain`] with freshly-allocated rings.
///
/// **Not RT-safe** — this is where every ring allocation happens, once.
pub fn split(chain: Chain, caps: RingCapacities) -> (AudioEngine, WorkerEndpoint) {
    let (command_tx, command_rx) = ring::<Command>(caps.commands);
    // **Raised, not trusted (issue #57).** [`Self::drain_commands`]'s gate refuses to consume an
    // offer unless the return ring has `RETIRE_HEADROOM_PER_OFFER` free slots. A ring whose whole
    // *capacity* is below that figure can never satisfy the gate, so the first Load or Unload to
    // arrive is left in the command ring and never popped — and because the gate returns rather
    // than skipping, every parameter change and reset queued behind it is stalled too, for the
    // life of the engine. That is not the graceful degradation D-8.1's own clause describes (a
    // ring that *fills* still drains again when the worker recovers); it is a permanent
    // head-of-line block from which nothing recovers, caused by a capacity choice made once, off
    // the audio thread, with no error path anywhere.
    //
    // Raising it here rather than returning an error keeps `split`'s infallible signature, which
    // `namir-app` and `namir-clap` both call directly; a caller that asked for a ring too small to
    // work gets a working one, and the figure it asked for was never observable from outside.
    let (retire_tx, retire_rx) = ring::<Resource>(caps.retire.max(RETIRE_HEADROOM_PER_OFFER));
    let (telemetry_tx, telemetry_rx) = telemetry_ring(caps.telemetry);
    (
        AudioEngine {
            chain,
            commands: command_rx,
            retire: retire_tx,
            telemetry: telemetry_tx,
            telemetry_scratch: vec![
                TelemetryEntry { id: 0, value: 0.0 };
                TELEMETRY_SCRATCH_ENTRIES
            ],
            stalled_offer: None,
            retire_backlog: false,
            deferred_blocks: 0,
            rejected_blocks: 0,
        },
        WorkerEndpoint {
            commands: command_tx,
            retire: retire_rx,
            telemetry: telemetry_rx,
        },
    )
}

/// Builds the fixed 1.0 chain (FR-CHAIN-010) and wires it to a worker, with default capacities.
/// This is the product entry point M4 delivers; `crate::build_default_chain` remains for
/// benchmarks and bare-chain tests.
pub fn build_default_engine(
    ctx: &PrepareContext,
) -> Result<(AudioEngine, WorkerEndpoint), PrepareError> {
    Ok(split(
        crate::stages::build_default_chain(ctx)?,
        RingCapacities::default(),
    ))
}

impl AudioEngine {
    /// The whole audio-thread contract in one call — see this module's doc comment for the
    /// sequence and for why retirements are collected twice.
    ///
    /// Wait-free throughout (NFR-RT-020): every ring operation is a bounded number of atomic
    /// loads and stores, with no loop whose exit depends on another thread making progress.
    pub fn process(&mut self, io: &mut StageIo<'_>) {
        if !self.io_matches_preparation(io) {
            // Refused whole (issue #60). `io` is left exactly as the caller handed it in, which
            // for an in-place buffer is unity-gain passthrough — audible, but the engine's job
            // here is not to crash the host. Telemetry carries the count; see
            // `io_matches_preparation` for why this is a check rather than a `debug_assert`.
            self.rejected_blocks += 1;
            self.publish_telemetry();
            return;
        }
        self.retry_stalled_offer();
        self.drain_commands();
        self.collect_retired();
        self.chain.process(io);
        self.collect_retired();
        self.publish_telemetry();
    }

    /// Whether `io` is within what the chain was prepared for: at most `max_block_size` frames,
    /// and exactly the prepared channel count.
    ///
    /// **Why this is here at all (issue #60).** `stage_io.rs` puts the obligation on "whatever
    /// drives `Chain::process`" — and this *is* that driver, while `namir-clap/src/audio.rs`
    /// passes the host's own `frames_count` straight through. Nothing enforced it, and both ways
    /// of violating it are a panic on the audio thread rather than a wrong sound: an over-long
    /// block indexes past the per-stage dry-capture scratch (`nam.rs`, `gate.rs`, `ir.rs`), and a
    /// channel count below the prepared one indexes past `io`'s own channel list (`eq.rs`).
    ///
    /// **A check, not a `debug_assert`.** D-16.3's rule for the audio thread is to degrade rather
    /// than panic, and a `debug_assert` degrades in exactly the wrong direction — it is loud in
    /// the test build, where the caller is a test that could simply be fixed, and absent in the
    /// release build a host actually runs. Refusing the block behaves identically in both profiles
    /// and is therefore also testable in both.
    ///
    /// Returns `true` unconditionally for a chain that never had `prepare_crosscutting` called on
    /// it (`Chain::prepared_for` is `None`): that path is documented test/scaffolding-only, has no
    /// recorded context to check against, and behaved this way before this check existed.
    fn io_matches_preparation(&self, io: &StageIo<'_>) -> bool {
        let Some(ctx) = self.chain.prepared_for() else {
            return true;
        };
        io.frames() <= ctx.max_block_size()
            && io.channel_count() == ctx.channel_config().output_channels() as usize
    }

    /// Applies one parameter change immediately, **bypassing the command ring entirely**.
    ///
    /// **Who this is for, and why it's sound:** a caller that already holds `&mut AudioEngine`
    /// *from the audio thread itself* — M6's `namir-clap`, specifically, for host automation
    /// (`ParamValueEvent`s) arriving in CLAP's `process()` callback. That callback already *is*
    /// Namir's audio thread for a plugin instance, and the borrow checker's `&mut self` here is
    /// the same exclusivity guarantee the ring exists to simulate across an actual thread
    /// boundary — there is no boundary to cross, so there is nothing wait-free machinery would
    /// buy over calling straight through to [`Chain::apply`]. FR-PARAM-030 ("parameter changes
    /// ... converge to the same engine state regardless of source") still holds: this calls the
    /// identical [`Chain::apply`] the ring-drain path in [`Self::process`] calls, so a UI/worker
    /// change and a host-automation change of the same parameter produce the same state either
    /// way — only same-block ordering relative to a ring-drained change is unspecified (this
    /// runs before `process`'s own drain in every call site that uses it), which is documented
    /// at the call site rather than here.
    ///
    /// Wait-free (NFR-RT-010): identical cost to one [`Chain::apply`] call, no ring, no lock, no
    /// allocation.
    pub fn apply_param_direct(&mut self, change: crate::param::ParamChange) {
        self.chain.apply(change);
    }

    /// [`Chain::reset`], direct — the same audio-thread-exclusivity argument
    /// [`Self::apply_param_direct`]'s doc comment makes, applied to CLAP's `PluginAudioProcessor::
    /// reset` (transport stop/reposition), which the host calls directly on the audio thread
    /// rather than through any ring.
    pub fn reset_direct(&mut self) {
        self.chain.reset();
    }

    /// Read-only access to the chain, for callers that need its latency/tail reporting.
    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// Blocks in which the command drain stopped early. Also published as telemetry.
    pub fn deferred_blocks(&self) -> u64 {
        self.deferred_blocks
    }

    /// Whether a retirement is currently stuck behind a full return ring.
    pub fn retire_backlog(&self) -> bool {
        self.retire_backlog
    }

    /// Blocks refused because the [`StageIo`] did not match the chain's [`PrepareContext`]. Any
    /// nonzero value is a driver bug — see [`Self::io_matches_preparation`]. Also published as
    /// telemetry.
    pub fn rejected_blocks(&self) -> u64 {
        self.rejected_blocks
    }

    fn retry_stalled_offer(&mut self) {
        let Some(resource) = self.stalled_offer.take() else {
            return;
        };
        if let Err(back) = self.retire.try_push(resource) {
            self.stalled_offer = Some(back);
        }
    }

    /// Drains up to [`MAX_COMMANDS_PER_BLOCK`] commands, and at most one resource offer or
    /// unload per target stage. M5's `Unload` shares a target's `*_offered` flag and headroom
    /// check with its `Load` counterpart, because both start a crossfade and can produce a
    /// retirement in the same stage — the gate cannot tell them apart for headroom purposes and
    /// does not try to.
    ///
    /// # The drain gate, and the head-of-line cost it accepts
    ///
    /// An offer is only consumed when there is room to absorb everything it can cause: the stage's
    /// retire pen must be free (guaranteed by not offering twice to the same stage in one block,
    /// plus the `collect_retired` pass that precedes the next block's drain) and the return ring
    /// must have [`RETIRE_HEADROOM_PER_OFFER`] free slots. When it is not, the command is **left in
    /// the ring** rather than popped and held somewhere — D-7.2 is explicit that a command is never
    /// dropped, and leaving it in place is simpler and strictly safer than inventing a holding pen
    /// with its own overflow question.
    ///
    /// The honest cost: a blocked offer also stalls parameter changes queued behind it. That is
    /// acceptable because the only way to reach the blocked state is a worker that submits but does
    /// not drain the return ring — and the worker drains before it submits — so in practice it
    /// means the worker has died, in which case nothing is being enqueued behind the offer anyway.
    /// The state is published as `telemetry.engine.deferred_blocks` rather than being invisible.
    ///
    /// **Alternatives rejected:** popping the offer and holding it (needs unbounded holding state,
    /// or a drop when a newer offer supersedes it); a second ring for parameters (contradicts
    /// D-7.2's single inbound ring, and would let a parameter change overtake the resource load it
    /// was meant to follow).
    fn drain_commands(&mut self) {
        if self.stalled_offer.is_some() {
            self.deferred_blocks += 1;
            return;
        }
        let mut nam_offered = false;
        let mut ir_offered = false;
        // **A running reservation, not a fresh `slots()` read per offer (issue #62).**
        // `RETIRE_HEADROOM_PER_OFFER` is documented as headroom *per offer*, but both stages used
        // to test the same pre-drain snapshot: a block carrying one Nam offer and one Ir offer
        // could commit to four potential retirements having verified only two free slots. It
        // degraded safely — the second retirement lands in the stage's own pen and defers — but
        // the invariant the constant states simply did not hold. Every accepted offer now consumes
        // its own headroom, so what the gate checks is what the comment claims.
        //
        // `slots()` is still re-read each time rather than snapshotted: the worker only ever
        // *frees* slots, so a later read can be larger but never smaller, and re-reading lets a
        // second offer through in the same block if the worker drained in between.
        let mut reserved = 0usize;

        for _ in 0..MAX_COMMANDS_PER_BLOCK {
            let Some(kind) = self.commands.peek().map(Command::kind) else {
                return;
            };
            if matches!(
                kind,
                CommandKind::LoadNam
                    | CommandKind::LoadIr
                    | CommandKind::UnloadNam
                    | CommandKind::UnloadIr
            ) {
                let already = match kind {
                    CommandKind::LoadNam | CommandKind::UnloadNam => nam_offered,
                    _ => ir_offered,
                };
                let needed = reserved + RETIRE_HEADROOM_PER_OFFER;
                if already || self.retire_backlog || self.retire.slots() < needed {
                    self.deferred_blocks += 1;
                    return;
                }
                reserved = needed;
                match kind {
                    CommandKind::LoadNam | CommandKind::UnloadNam => nam_offered = true,
                    _ => ir_offered = true,
                }
            }
            let Some(command) = self.commands.try_pop() else {
                return;
            };
            self.apply_command(command);
        }
        // Hit the per-block cap. Only a *deferral* if something is actually left behind (issue
        // #63): a drain whose 64th pop emptied the ring stopped because there was nothing more to
        // do, not early, and counting it made the one telemetry signal that says "a control may
        // have stopped responding" fire on a perfectly healthy burst of exactly 64 commands.
        if self.commands.peek().is_some() {
            self.deferred_blocks += 1;
        }
    }

    fn apply_command(&mut self, command: Command) {
        match command {
            // D-10.4: `global.bypass`/`global.output_ceiling_db` changes arrive as an ordinary
            // `Command::Param` now -- `Chain::apply` recognises those two ids itself (see its own
            // doc comment) before falling back to broadcasting to every stage. There is no longer
            // a dedicated `Command::SetGlobalBypass`/`SetOutputCeilingDb` variant to match here.
            Command::Param(change) => self.chain.apply(change),
            Command::Reset => self.chain.reset(),
            Command::Unload(kind) => self.chain.unload(kind),
            Command::Load(resource) => {
                let mut offer = Some(resource);
                self.chain.offer(&mut offer);
                if let Some(unwanted) = offer {
                    // No stage wanted it (RD-2 territory; impossible for the fixed six-stage
                    // chain). The drain gate just verified there is headroom, so this normally
                    // succeeds -- and if it somehow does not, the resource is held, never dropped.
                    if let Err(back) = self.retire.try_push(unwanted) {
                        self.stalled_offer = Some(back);
                    }
                }
            }
        }
    }

    fn collect_retired(&mut self) {
        let mut sink = RetireSink::new(&mut self.retire);
        self.chain.collect_retired(&mut sink);
        if sink.rejected() > 0 {
            self.retire_backlog = true;
        } else {
            // Nothing was rejected this pass. That only clears the backlog flag if nothing is
            // still parked, which `Chain::collect_retired` guarantees by having just tried every
            // stage: a stage that still holds a resource would have attempted a push and failed.
            self.retire_backlog = false;
        }
    }

    fn publish_telemetry(&mut self) {
        // Split the borrows explicitly: the sink borrows the scratch mutably for its whole
        // lifetime, so the producer has to be reached through a separate binding.
        let AudioEngine {
            chain,
            telemetry,
            telemetry_scratch,
            deferred_blocks,
            retire_backlog,
            rejected_blocks,
            ..
        } = self;
        let mut sink = TelemetrySink::new(telemetry_scratch);
        chain.telemetry(&mut sink);
        sink.push(TelemetryEntry {
            id: TELEMETRY_DEFERRED_BLOCKS,
            value: *deferred_blocks as f32,
        });
        sink.push(TelemetryEntry {
            id: TELEMETRY_RETIRE_BACKLOG,
            value: if *retire_backlog { 1.0 } else { 0.0 },
        });
        sink.push(TelemetryEntry {
            id: TELEMETRY_REJECTED_BLOCKS,
            value: *rejected_blocks as f32,
        });
        for entry in sink.entries() {
            telemetry.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::{ParamChange, ParamId};
    use crate::resource::ResourceKind;
    use crate::rt_harness::audio_section;
    use namir_core::{ChannelConfig, SampleRate};
    use namir_fixtures::ir::decaying_noise;
    use namir_fixtures::nam::{WaveNetShape, generate};
    use namir_ir::PreparedIr;
    use std::sync::Arc;

    const SR: u32 = 48_000;
    const BLOCK: usize = 64;

    fn ctx() -> PrepareContext {
        PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Mono).unwrap()
    }

    fn model(seed: u64) -> Arc<namir_nam::PreparedNam> {
        let bytes = generate(WaveNetShape::Nano, seed)
            .expect("fixture should generate")
            .to_json_bytes();
        Arc::new(namir_nam::load(&bytes).expect("generated fixture should load"))
    }

    fn ir(seed: u64) -> Arc<PreparedIr> {
        let taps = decaying_noise(512, seed, 128.0);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SR,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut buf = Vec::new();
        {
            let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
            for &t in &taps {
                w.write_sample(t).unwrap();
            }
            w.finalize().unwrap();
        }
        Arc::new(PreparedIr::from_wav_bytes(&buf, SampleRate::new(SR).unwrap(), BLOCK).unwrap())
    }

    /// Drives `engine` for `blocks` blocks of a continuous sine, entirely inside the D-7.5
    /// harness, returning every output sample in order. `at_block` runs before the block of that
    /// index, so a test can submit a command mid-stream without leaving the harness.
    fn run_sine(
        engine: &mut AudioEngine,
        blocks: usize,
        freq_hz: f32,
        mut at_block: impl FnMut(usize),
    ) -> Vec<f32> {
        let mut out = Vec::with_capacity(blocks * BLOCK);
        let mut buf = vec![0.0f32; BLOCK];
        let mut phase = 0.0f32;
        let step = std::f32::consts::TAU * freq_hz / SR as f32;
        for b in 0..blocks {
            at_block(b);
            for s in buf.iter_mut() {
                *s = 0.5 * phase.sin();
                phase += step;
                if phase > std::f32::consts::TAU {
                    phase -= std::f32::consts::TAU;
                }
            }
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, BLOCK);
            audio_section(|| engine.process(&mut io));
            out.extend_from_slice(io.channel(0));
        }
        out
    }

    /// `Command` deliberately implements no `Debug` — it owns prepared slots, and a derived
    /// `Debug` would drag one onto every type they contain for no product benefit. So tests push
    /// through this rather than `.unwrap()`.
    fn submit(worker: &mut WorkerEndpoint, command: Command) {
        assert!(
            worker.commands.try_push(command).is_ok(),
            "the command ring was unexpectedly full"
        );
    }

    fn max_abs_first_difference(samples: &[f32]) -> f32 {
        samples
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    }

    /// D-8.2's hard constraint: "`Prepared*` must be immutable and `Sync`." `PreparedIr` is the
    /// one that actually needed checking — it holds `Arc<dyn RealToComplex<f32>>` from `realfft`,
    /// and `namir-ir`'s own doc comment asserted `Sync` in prose with nothing testing it. The
    /// worker's process-global cache is the first thing that will depend on it being true.
    #[test]
    fn prepared_resources_are_send_and_sync() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<namir_nam::PreparedNam>();
        assert_send_sync::<PreparedIr>();
        const fn assert_send<T: Send>() {}
        assert_send::<AudioEngine>();
        assert_send::<WorkerEndpoint>();
        assert_send::<Command>();
        assert_send::<Resource>();
    }

    /// NFR-RT-010: the whole per-block sequence — drain, two retire passes, process, publish —
    /// allocates nothing, even with nothing to do.
    #[test]
    fn a_block_with_an_empty_ring_does_not_allocate() {
        let (mut engine, _worker) = build_default_engine(&ctx()).unwrap();
        run_sine(&mut engine, 4, 220.0, |_| {});
    }

    /// **FR-NAM-070's own literal *Verify* method: "swap models under a continuous sine input and
    /// assert no discontinuity exceeding a stated threshold and no dropout."**
    ///
    /// The threshold is self-calibrating rather than a magic number: the same run is measured
    /// without a swap first, and the swap is required not to introduce a first difference more
    /// than `DISCONTINUITY_FACTOR` times that baseline. A fixed constant would quietly stop
    /// meaning anything the first time the fixture changed.
    ///
    /// The whole run — including the block where the crossfade completes and the old slot retires
    /// — is inside `audio_section`, so this is simultaneously the FR-NAM-070 evidence and the
    /// NFR-RT-010 evidence for a handover driven end to end through the real command ring.
    #[test]
    fn fr_nam_070_swapping_models_under_a_sine_has_no_discontinuity_or_dropout() {
        const DISCONTINUITY_FACTOR: f32 = 3.0;
        const BLOCKS: usize = 200; // ~267 ms, well past the 20 ms fade
        const SWAP_AT: usize = 100;

        let c = ctx();

        // Baselines: each model alone, settled, no swap.
        //
        // Both are needed, not just the first. The post-swap window is mostly the *second* model's
        // output, so calibrating against the first alone would charge the crossfade for whatever
        // the two fixtures happen to differ by in steady-state roughness. That is not a
        // hypothetical: the sibling FR-IR-060 test below measured 0.025 for one fixture and 0.078
        // for the other, so a first-model-only baseline failed it at 3.2x while the fade itself
        // was contributing about 4%.
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_nam(model(1), &c));
        let baseline_a = run_sine(&mut engine, BLOCKS, 220.0, |_| {});
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_nam(model(2), &c));
        let baseline_b = run_sine(&mut engine, BLOCKS, 220.0, |_| {});
        let baseline_jump = max_abs_first_difference(&baseline_a[SWAP_AT * BLOCK..])
            .max(max_abs_first_difference(&baseline_b[SWAP_AT * BLOCK..]));

        // Same again, but swapping to a structurally different model partway through.
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_nam(model(1), &c));
        let swapped = run_sine(&mut engine, BLOCKS, 220.0, |b| {
            if b == SWAP_AT {
                submit(&mut worker, Command::load_nam(model(2), &c));
            }
        });
        let swap_jump = max_abs_first_difference(&swapped[SWAP_AT * BLOCK..]);

        assert!(
            swap_jump <= DISCONTINUITY_FACTOR * baseline_jump,
            "swap introduced a discontinuity of {swap_jump} against a no-swap baseline of \
             {baseline_jump} (allowed {DISCONTINUITY_FACTOR}x)"
        );

        // No dropout: FR-NAM-070 says the previous model keeps processing until the new one is
        // ready, so nothing across the swap may go silent. A muted changeover — the failure this
        // half of the requirement exists to catch — would show as a run of near-zero peaks.
        let post = &swapped[SWAP_AT * BLOCK..];
        for (i, window) in post.chunks(32).enumerate() {
            let peak = window.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(
                peak > 1e-4,
                "window {i} after the swap went silent (peak {peak}): a dropout"
            );
        }

        // And the worker gets the retired slot back, rather than the audio thread having freed it.
        assert!(
            worker.retire.try_pop().is_some(),
            "the outgoing slot should have arrived on the return ring (D-8.1 step 4)"
        );
    }

    /// **FR-IR-060: "the same no-glitch, crossfaded changeover requirement as FR-NAM-070."** Same
    /// test body, same self-calibrating threshold and — since M14 — the same *two* assertions,
    /// against the Ir stage.
    ///
    /// The no-dropout half used to be missing here while its Nam sibling had it, and its absence
    /// was not cosmetic: a changeover that faded both slots to silence and back would *lower* the
    /// discontinuity figure this test bounds and pass more comfortably the worse it got. Nothing
    /// else covered it either — `rt_stress.rs`'s own dropout assertion submits `Target::Nam` alone,
    /// so it never has an IR handover in flight.
    // trace: FR-IR-060
    #[test]
    fn fr_ir_060_swapping_irs_under_a_sine_has_no_discontinuity_or_dropout() {
        const DISCONTINUITY_FACTOR: f32 = 3.0;
        const BLOCKS: usize = 200;
        const SWAP_AT: usize = 100;

        let c = ctx();

        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_ir(ir(11), &c));
        let baseline_a = run_sine(&mut engine, BLOCKS, 220.0, |_| {});

        // The *second* IR's own steady-state roughness matters too: the post-swap window is mostly
        // its output, so calibrating against the first IR alone would charge the fade for whatever
        // the fixtures happen to differ by.
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_ir(ir(12), &c));
        let baseline_b = run_sine(&mut engine, BLOCKS, 220.0, |_| {});
        let baseline_jump = max_abs_first_difference(&baseline_a[SWAP_AT * BLOCK..])
            .max(max_abs_first_difference(&baseline_b[SWAP_AT * BLOCK..]));

        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_ir(ir(11), &c));
        let swapped = run_sine(&mut engine, BLOCKS, 220.0, |b| {
            if b == SWAP_AT {
                submit(&mut worker, Command::load_ir(ir(12), &c));
            }
        });
        let swap_jump = max_abs_first_difference(&swapped[SWAP_AT * BLOCK..]);

        assert!(
            swap_jump <= DISCONTINUITY_FACTOR * baseline_jump,
            "IR swap introduced a discontinuity of {swap_jump} against a no-swap baseline of \
             {baseline_jump} (allowed {DISCONTINUITY_FACTOR}x)"
        );

        // No dropout, the other half of FR-NAM-070's method that this requirement imports: the
        // outgoing IR keeps convolving until the incoming one has faded in, so nothing across the
        // changeover may go silent. Identical window and floor to the Nam test above.
        let post = &swapped[SWAP_AT * BLOCK..];
        for (i, window) in post.chunks(32).enumerate() {
            let peak = window.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(
                peak > 1e-4,
                "window {i} after the IR swap went silent (peak {peak}): a dropout"
            );
        }

        assert!(
            worker.retire.try_pop().is_some(),
            "the outgoing IR slot should have arrived on the return ring"
        );
    }

    /// **M5, FR-STATE-070: "the state shall load with that stage empty."** `Command::Unload`
    /// must crossfade smoothly to dry — no click — and must retire the outgoing slot through the
    /// return ring exactly as a `Load` handover does; it must not simply drop it. Same
    /// self-calibrating-threshold shape as `fr_nam_070_swapping_models_under_a_sine_has_no_discontinuity_or_dropout`
    /// above: what's being measured is the sample-to-sample jump at the transition, not the
    /// absolute level either side of it (a wet-to-dry transition is expected to change level; it
    /// must not click while doing so).
    ///
    /// Committed red-first (NFR-QUAL-020): at this commit, `NamStage::unload` is a no-op stub,
    /// so this test must fail — no crossfade starts, the model never leaves, and nothing reaches
    /// the return ring.
    #[test]
    fn command_unload_nam_crossfades_to_dry_and_retires_the_slot() {
        const DISCONTINUITY_FACTOR: f32 = 3.0;
        const BLOCKS: usize = 200; // ~267 ms, well past the 20 ms fade
        const SWAP_AT: usize = 100;

        let c = ctx();

        // Baselines, exactly as fr_nam_070's: the model loaded the whole run, and nothing ever
        // loaded at all -- neither one changes state at SWAP_AT, so the "jump" measured there is
        // just each signal's own steady-state roughness, establishing the noise floor a genuine
        // unload's transition must not exceed by more than DISCONTINUITY_FACTOR.
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_nam(model(1), &c));
        let baseline_loaded = run_sine(&mut engine, BLOCKS, 220.0, |_| {});
        let (mut engine, _worker) = build_default_engine(&c).unwrap();
        let baseline_empty = run_sine(&mut engine, BLOCKS, 220.0, |_| {});
        let baseline_jump = max_abs_first_difference(&baseline_loaded[SWAP_AT * BLOCK..])
            .max(max_abs_first_difference(&baseline_empty[SWAP_AT * BLOCK..]));

        // The real thing: loaded, then unloaded mid-stream.
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_nam(model(1), &c));
        let unloaded = run_sine(&mut engine, BLOCKS, 220.0, |b| {
            if b == SWAP_AT {
                submit(&mut worker, Command::Unload(ResourceKind::Nam));
            }
        });
        let unload_jump = max_abs_first_difference(&unloaded[SWAP_AT * BLOCK..]);

        assert!(
            unload_jump <= DISCONTINUITY_FACTOR * baseline_jump,
            "unload introduced a discontinuity of {unload_jump} against a baseline of \
             {baseline_jump} (allowed {DISCONTINUITY_FACTOR}x)"
        );

        // No dropout: the fade must keep processing, not go silent, throughout the transition.
        let post = &unloaded[SWAP_AT * BLOCK..];
        for (i, window) in post.chunks(32).enumerate() {
            let peak = window.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(
                peak > 1e-4,
                "window {i} after the unload went silent (peak {peak}): a dropout"
            );
        }

        // And the worker gets the retired slot back -- an unload must not simply drop it.
        assert!(
            worker.retire.try_pop().is_some(),
            "the unloaded slot should have arrived on the return ring (D-8.1 step 4)"
        );
    }

    /// **M5's Ir mirror of the Nam unload test above.** Same shape, same threshold.
    #[test]
    fn command_unload_ir_crossfades_to_dry_and_retires_the_slot() {
        const DISCONTINUITY_FACTOR: f32 = 3.0;
        const BLOCKS: usize = 200;
        const SWAP_AT: usize = 100;

        let c = ctx();

        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_ir(ir(11), &c));
        let baseline_loaded = run_sine(&mut engine, BLOCKS, 220.0, |_| {});
        let (mut engine, _worker) = build_default_engine(&c).unwrap();
        let baseline_empty = run_sine(&mut engine, BLOCKS, 220.0, |_| {});
        let baseline_jump = max_abs_first_difference(&baseline_loaded[SWAP_AT * BLOCK..])
            .max(max_abs_first_difference(&baseline_empty[SWAP_AT * BLOCK..]));

        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        submit(&mut worker, Command::load_ir(ir(11), &c));
        let unloaded = run_sine(&mut engine, BLOCKS, 220.0, |b| {
            if b == SWAP_AT {
                submit(&mut worker, Command::Unload(ResourceKind::Ir));
            }
        });
        let unload_jump = max_abs_first_difference(&unloaded[SWAP_AT * BLOCK..]);

        assert!(
            unload_jump <= DISCONTINUITY_FACTOR * baseline_jump,
            "IR unload introduced a discontinuity of {unload_jump} against a baseline of \
             {baseline_jump} (allowed {DISCONTINUITY_FACTOR}x)"
        );

        let post = &unloaded[SWAP_AT * BLOCK..];
        for (i, window) in post.chunks(32).enumerate() {
            let peak = window.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(
                peak > 1e-4,
                "window {i} after the IR unload went silent (peak {peak}): a dropout"
            );
        }

        assert!(
            worker.retire.try_pop().is_some(),
            "the unloaded IR slot should have arrived on the return ring"
        );
    }

    /// **D-8.1's degradation clause, exercised rather than asserted:** "If the worker dies, the
    /// ring fills and memory is retained but audio continues. Degradation, not failure (P8)."
    ///
    /// A shallow return ring that is never drained, with several handovers submitted. Audio must
    /// keep flowing, finite, with no allocation — and, the load-bearing part, **no resource
    /// dropped on the audio thread**.
    ///
    /// # What this test used to be, and why it proved nothing (issue #57)
    ///
    /// It asked for `retire: 1`, which is below [`RETIRE_HEADROOM_PER_OFFER`]. The drain gate
    /// therefore refused the very first `Load` and every command behind it, forever, so **no model
    /// was ever installed** — there was no handover to degrade, and the run was indistinguishable
    /// from an idle engine. Its two assertions could not see that: `retire_backlog() ||
    /// deferred_blocks() > 0` passed on the second disjunct alone (which a permanently-blocked
    /// gate raises on every block), and `Arc::strong_count(m) >= 1` is tautological, because the
    /// test itself holds each `Arc` for the whole of its own body.
    ///
    /// Three things changed. The capacity is now `RETIRE_HEADROOM_PER_OFFER`, the shallowest ring
    /// that can carry a handover at all. A model is asserted to have *actually installed*, by
    /// comparing the audio against an otherwise-identical engine that was never sent one. And the
    /// strong-count assertion asks for `>= 2`: one reference is the test's own, so the second is
    /// the engine still holding the slot — installed, parked in a stage's pen, or sitting in the
    /// return ring. That is the assertion that fails if the audio thread ever drops one.
    #[test]
    fn a_never_drained_return_ring_retains_memory_and_audio_continues() {
        const BLOCKS: usize = 400;
        let c = ctx();
        let (mut engine, mut worker) = split(
            crate::stages::build_default_chain(&c).unwrap(),
            RingCapacities {
                commands: 16,
                retire: RETIRE_HEADROOM_PER_OFFER,
                telemetry: 16,
            },
        );

        let models: Vec<_> = (0..4).map(|i| model(100 + i)).collect();
        let mut submitted = 0usize;
        let out = run_sine(&mut engine, BLOCKS, 220.0, |b| {
            if b % 60 == 10 && submitted < models.len() {
                // May be refused once the command ring backs up; that is the point.
                let _ = worker
                    .commands
                    .try_push(Command::load_nam(Arc::clone(&models[submitted]), &c));
                submitted += 1;
            }
        });

        for (i, s) in out.iter().enumerate() {
            assert!(
                s.is_finite(),
                "sample {i} was not finite under back-pressure"
            );
        }

        // **A model actually installed.** Without this the whole test is satisfiable by an engine
        // that refused every command it was ever sent, which is exactly what it used to be.
        let (mut idle_engine, _idle_worker) = build_default_engine(&c).unwrap();
        let idle = run_sine(&mut idle_engine, BLOCKS, 220.0, |_| {});
        let difference = out
            .iter()
            .zip(&idle)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            difference > 1e-3,
            "the run is indistinguishable from one where nothing was ever loaded (peak difference \
             {difference}): no handover happened, so nothing was degraded"
        );

        // Real, observable back-pressure -- and specifically the gate stalling, not the per-block
        // command cap: only four commands are ever submitted, far below MAX_COMMANDS_PER_BLOCK.
        assert!(
            engine.deferred_blocks() > 0,
            "a two-deep, never-drained return ring should have stalled the drain"
        );

        // Nothing was freed by the audio thread: every model this test handed over is still
        // reachable from the engine -- installed, parked in a stage's pen, held in the return
        // ring, or still sitting in the command ring -- so each carries the test's own reference
        // plus at least one more.
        for (i, m) in models.iter().take(submitted).enumerate() {
            assert!(
                Arc::strong_count(m) >= 2,
                "model {i} is held only by the test ({} references): the audio thread dropped the \
                 slot instead of retaining it",
                Arc::strong_count(m)
            );
        }

        // Draining the worker end releases the backlog and normal service resumes: a command that
        // was stuck behind the gate gets consumed.
        let stuck = worker.commands.slots();
        while worker.retire.try_pop().is_some() {}
        let after = run_sine(&mut engine, 60, 220.0, |_| {});
        assert!(after.iter().all(|s| s.is_finite()));
        assert!(
            worker.commands.slots() > stuck,
            "draining the return ring must let the stalled drain make progress again"
        );
    }

    /// **Issue #57's other half: `split` used to trust `caps.retire`.** Any value below
    /// [`RETIRE_HEADROOM_PER_OFFER`] makes `self.retire.slots() < RETIRE_HEADROOM_PER_OFFER`
    /// permanently true, so the drain gate returns on the first `Load`/`Unload` and never pops
    /// it — head-of-line blocking that silently kills every parameter change and reset queued
    /// behind it, for the life of the engine.
    ///
    /// Committed red-first: before the fix the ceiling change queued behind the load never
    /// arrives, and the block comes out unclamped at 0.8.
    #[test]
    fn a_retire_capacity_below_one_offers_headroom_does_not_stall_the_drain() {
        let c = ctx();
        let (mut engine, mut worker) = split(
            crate::stages::build_default_chain(&c).unwrap(),
            RingCapacities {
                commands: 16,
                retire: 1,
                telemetry: 16,
            },
        );

        let empty = worker.commands.slots();
        submit(&mut worker, Command::load_nam(model(9), &c));
        submit(
            &mut worker,
            Command::Param(ParamChange {
                id: ParamId(namir_params::global::OUTPUT_CEILING_DB.id.0),
                value: -20.0,
            }),
        );

        let mut buf = [0.8f32; BLOCK];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, BLOCK);
        audio_section(|| engine.process(&mut io));

        let ceiling = namir_core::db_to_linear(-20.0);
        for s in io.channel(0) {
            assert!(
                s.abs() <= ceiling + 1e-4,
                "sample {s} exceeded the -20 dB ceiling: the parameter change queued behind the \
                 load never drained"
            );
        }
        assert_eq!(
            worker.commands.slots(),
            empty,
            "both commands should have drained, leaving the ring empty"
        );
    }

    /// **Issue #62.** [`RETIRE_HEADROOM_PER_OFFER`] is documented as headroom *per offer*, but
    /// both stages used to test the same pre-drain `slots()` snapshot, so a block carrying one Nam
    /// offer and one Ir offer could commit to four potential retirements having verified only two
    /// free slots.
    ///
    /// A three-slot ring makes the difference observable: one offer fits (2 ≤ 3), two do not
    /// (4 > 3). Committed red-first — before the fix both commands drain in the first block.
    #[test]
    fn a_second_offer_in_the_same_block_reserves_its_own_retire_headroom() {
        let c = ctx();
        let (mut engine, mut worker) = split(
            crate::stages::build_default_chain(&c).unwrap(),
            RingCapacities {
                commands: 16,
                retire: RETIRE_HEADROOM_PER_OFFER + 1,
                telemetry: 16,
            },
        );

        submit(&mut worker, Command::load_nam(model(21), &c));
        submit(&mut worker, Command::load_ir(ir(22), &c));
        let before = worker.commands.slots();

        run_sine(&mut engine, 1, 220.0, |_| {});
        assert_eq!(
            worker.commands.slots() - before,
            1,
            "one block verified {RETIRE_HEADROOM_PER_OFFER} free slots and accepted two offers, \
             each of which may retire that many"
        );
        assert!(engine.deferred_blocks() > 0);

        // The reservation is per block, not sticky: the next block takes the second offer.
        run_sine(&mut engine, 1, 220.0, |_| {});
        assert_eq!(
            worker.commands.slots() - before,
            2,
            "the second offer should go through on the following block"
        );
    }

    /// **Issue #63.** `deferred_blocks` was incremented unconditionally after the
    /// [`MAX_COMMANDS_PER_BLOCK`] loop, including when the 64th pop emptied the ring — so the one
    /// telemetry signal that says "a control may have stopped responding" fired on a perfectly
    /// healthy burst of exactly 64 commands.
    ///
    /// Committed red-first: before the fix the first assertion reads 1.
    #[test]
    fn a_drain_that_exactly_empties_the_ring_is_not_a_deferral() {
        let c = ctx();
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        let empty = worker.commands.slots();
        for i in 0..MAX_COMMANDS_PER_BLOCK as u32 {
            submit(
                &mut worker,
                Command::Param(ParamChange {
                    id: ParamId(i),
                    value: 0.0,
                }),
            );
        }
        run_sine(&mut engine, 1, 220.0, |_| {});
        assert_eq!(
            worker.commands.slots(),
            empty,
            "the whole burst should have drained"
        );
        assert_eq!(
            engine.deferred_blocks(),
            0,
            "a drain that consumed every queued command did not stop early"
        );

        // One more than the cap *is* a deferral, so the counter still means something.
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        for i in 0..MAX_COMMANDS_PER_BLOCK as u32 + 1 {
            submit(
                &mut worker,
                Command::Param(ParamChange {
                    id: ParamId(i),
                    value: 0.0,
                }),
            );
        }
        run_sine(&mut engine, 1, 220.0, |_| {});
        assert_eq!(engine.deferred_blocks(), 1);
    }

    /// **Issue #60: `AudioEngine::process` accepted any [`StageIo`] at all.** A block longer than
    /// the `PrepareContext`'s `max_block_size` indexes past every stage's dry-capture scratch —
    /// `nam.rs`, `gate.rs` and `ir.rs` all slice `self.dry[ch][..n]` — which is a panic on the
    /// audio thread, from a figure `namir-clap/src/audio.rs` passes straight through from the
    /// host.
    ///
    /// Committed red-first: before the fix this test does not fail an assertion, it panics with a
    /// slice range error inside `NamStage::process`.
    #[test]
    fn a_block_longer_than_the_prepared_maximum_is_refused_rather_than_panicking() {
        let c = ctx(); // max_block_size == BLOCK.
        let (mut engine, _worker) = build_default_engine(&c).unwrap();

        let mut buf = [0.25f32; BLOCK * 2];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, BLOCK * 2);
        audio_section(|| engine.process(&mut io));

        assert_eq!(engine.rejected_blocks(), 1);
        for s in io.channel(0) {
            assert_eq!(
                *s, 0.25,
                "a refused block must be left exactly as the caller handed it in"
            );
        }

        // A block *within* the maximum is still processed normally, so the guard is a bound rather
        // than a blanket refusal.
        let mut ok_buf = [0.25f32; BLOCK];
        let mut ok_channels: [&mut [f32]; 1] = [&mut ok_buf];
        let mut ok_io = StageIo::new(&mut ok_channels, BLOCK);
        audio_section(|| engine.process(&mut ok_io));
        assert_eq!(engine.rejected_blocks(), 1);
    }

    /// Issue #60's other limb: a channel count the chain was never prepared for. Below the
    /// prepared count `eq.rs` indexes past `io`'s own channel list; above it, every stage's
    /// `self.dry[ch]` is short. Both are panics on the audio thread.
    #[test]
    fn a_block_with_the_wrong_channel_count_is_refused_rather_than_panicking() {
        let c = ctx(); // Mono.
        let (mut engine, _worker) = build_default_engine(&c).unwrap();

        let mut left = [0.25f32; BLOCK];
        let mut right = [0.5f32; BLOCK];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, BLOCK);
        audio_section(|| engine.process(&mut io));

        assert_eq!(engine.rejected_blocks(), 1);
        assert_eq!(io.channel(0)[0], 0.25);
        assert_eq!(io.channel(1)[0], 0.5);
    }

    /// The per-block command drain is bounded — NFR-RT-040 ("worst-case per-block processing time
    /// shall not depend on ... how long the engine has been running") and NFR-RT-010's "no
    /// unbounded loop". A UI that enqueues faster than the audio thread drains must not be able to
    /// make one block do arbitrary work.
    #[test]
    fn the_command_drain_is_bounded_per_block() {
        let c = ctx();
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        for i in 0..200u32 {
            submit(
                &mut worker,
                Command::Param(ParamChange {
                    id: ParamId(i),
                    value: 0.0,
                }),
            );
        }
        let before = worker.commands.slots();
        run_sine(&mut engine, 1, 220.0, |_| {});
        let consumed = worker.commands.slots() - before;
        assert!(consumed > 0, "the drain should have consumed some commands");
        assert!(
            consumed <= MAX_COMMANDS_PER_BLOCK,
            "one block consumed {consumed} commands, above the {MAX_COMMANDS_PER_BLOCK} bound"
        );
    }

    /// The telemetry scratch must hold the whole real chain's readings, because `TelemetrySink`
    /// overwrites *oldest* on overflow — an undersized buffer would silently starve whichever
    /// stage runs first rather than failing loudly. A comment cannot enforce that; this does.
    #[test]
    fn the_telemetry_scratch_holds_the_whole_real_chain() {
        let c = PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Stereo)
            .unwrap();
        let chain = crate::stages::build_default_chain(&c).unwrap();
        let mut storage = vec![TelemetryEntry { id: 0, value: 0.0 }; TELEMETRY_SCRATCH_ENTRIES];
        let mut sink = TelemetrySink::new(&mut storage);
        chain.telemetry(&mut sink);
        assert!(
            sink.len() < TELEMETRY_SCRATCH_ENTRIES,
            "the real stereo chain emits {} entries against a {TELEMETRY_SCRATCH_ENTRIES}-entry \
             scratch; raise TELEMETRY_SCRATCH_ENTRIES",
            sink.len()
        );
    }

    /// D-7.3 end to end: readings written on the audio thread reach a UI-side reader.
    #[test]
    fn telemetry_published_on_the_audio_thread_reaches_the_reader() {
        let c = ctx();
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        run_sine(&mut engine, 2, 220.0, |_| {});
        let mut out = [TelemetryEntry { id: 0, value: 0.0 }; 128];
        let drain = worker.telemetry.drain(&mut out);
        assert!(drain.read > 0, "no telemetry reached the reader");
        assert!(
            out[..drain.read]
                .iter()
                .any(|e| e.id == TELEMETRY_DEFERRED_BLOCKS),
            "the engine's own readings should be present alongside the stages'"
        );
    }

    /// **M6, `namir-clap`'s host-automation path.** [`AudioEngine::apply_param_direct`] must take
    /// effect on the very next `process` — it bypasses the ring, so there is no drain step for it
    /// to wait on — and must converge to the same state a ring-delivered [`Command::Param`] of the
    /// same change would (FR-PARAM-030).
    // trace-partial: FR-PARAM-030
    // uncovered: FR-PARAM-030 — of the three sources the requirement names, only two
    // uncovered: engine-internal delivery paths are compared: no artifact loads a
    // uncovered: namir_state::State and asserts the resulting engine state equals the state the
    // uncovered: same values reach via a parameter change, and no artifact drives
    // uncovered: namir_ui::UiIntent::SetParam into the comparison; closes M8
    #[test]
    fn apply_param_direct_takes_effect_on_the_next_process_call_like_a_ring_delivered_change() {
        let c = ctx();
        let (mut direct_engine, _worker) = build_default_engine(&c).unwrap();
        let (mut ring_engine, mut ring_worker) = build_default_engine(&c).unwrap();

        let change = ParamChange {
            id: ParamId(namir_params::global::OUTPUT_CEILING_DB.id.0),
            value: -6.0,
        };

        direct_engine.apply_param_direct(change);
        let direct_out = run_sine(&mut direct_engine, 4, 220.0, |_| {});

        submit(&mut ring_worker, Command::Param(change));
        let ring_out = run_sine(&mut ring_engine, 4, 220.0, |_| {});

        assert_eq!(
            direct_out, ring_out,
            "a direct-applied change must converge to the same engine state as the same change \
             delivered through the ring"
        );
    }

    // --- D-10.4: `global.bypass`/`global.output_ceiling_db` now travel the real command ring as
    // ordinary `Command::Param`s, exactly like a stage's own parameters -- there is no longer a
    // dedicated `Command::SetGlobalBypass`/`SetOutputCeilingDb` to submit instead. `chain.rs`'s
    // own tests cover `Chain::apply`'s routing directly; these two prove the same change also
    // survives the full worker -> ring -> `AudioEngine::process` path end to end. ---

    /// A large trim cut makes "did bypass actually engage" observable: if the `global.bypass`
    /// param change reached `Chain::apply` but were merely broadcast to stages (ignored, since no
    /// stage owns that id) instead of intercepted, the -24 dB trim would still apply and this
    /// test would fail.
    #[test]
    fn command_param_toggles_global_bypass_end_to_end() {
        let c = ctx();
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        let gain_id = ParamId(namir_params::stages::trim::GAIN_DB.id.0);
        let bypass_id = ParamId(namir_params::global::GLOBAL_BYPASS.id.0);

        submit(
            &mut worker,
            Command::Param(ParamChange {
                id: gain_id,
                value: -24.0,
            }),
        );
        submit(
            &mut worker,
            Command::Param(ParamChange {
                id: bypass_id,
                value: 1.0, // Stepped index 1 == "On", per GLOBAL_BYPASS's descriptor.
            }),
        );

        // build_default_chain reports zero latency with nothing loaded (see that module's own
        // test), so the bypass ring's delay never touches the buffer -- no settling window
        // needed, and the -24 dB trim ramp starting from its own default has nothing to settle
        // either, since bypass must keep it from ever being applied at all.
        let mut buf = [0.5f32; BLOCK];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, BLOCK);
        audio_section(|| engine.process(&mut io));

        for s in io.channel(0) {
            assert!(
                (*s - 0.5).abs() < 1e-4,
                "expected unity-gain bypass passthrough, got {s} (the -24 dB trim must not have \
                 applied)"
            );
        }
    }

    /// The output-ceiling half of the pair above: a -20 dB ceiling submitted via `Command::Param`
    /// must clamp a comfortably-over-ceiling input, exactly as it would through
    /// `Chain::set_output_ceiling_db` called directly.
    #[test]
    fn command_param_sets_output_ceiling_end_to_end() {
        let c = ctx();
        let (mut engine, mut worker) = build_default_engine(&c).unwrap();
        let ceiling_id = ParamId(namir_params::global::OUTPUT_CEILING_DB.id.0);

        submit(
            &mut worker,
            Command::Param(ParamChange {
                id: ceiling_id,
                value: -20.0,
            }),
        );

        let mut buf = [0.8f32; BLOCK];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, BLOCK);
        audio_section(|| engine.process(&mut io));

        let ceiling = namir_core::db_to_linear(-20.0);
        for s in io.channel(0) {
            assert!(
                s.abs() <= ceiling + 1e-4,
                "sample {s} exceeded the -20 dB output ceiling set via Command::Param"
            );
        }
    }
}
