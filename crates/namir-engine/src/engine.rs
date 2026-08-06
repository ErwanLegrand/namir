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

/// Ring capacities, fixed at preparation (D-7.2: "pre-allocated at preparation").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingCapacities {
    /// Inbound command ring. Generous: a burst of UI parameter moves must not stall the producer.
    pub commands: usize,
    /// D-8.1's return ring. Only needs depth for a few in-flight handovers; each consumes at most
    /// two slots.
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
    let (retire_tx, retire_rx) = ring::<Resource>(caps.retire);
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
        self.retry_stalled_offer();
        self.drain_commands();
        self.collect_retired();
        self.chain.process(io);
        self.collect_retired();
        self.publish_telemetry();
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

    fn retry_stalled_offer(&mut self) {
        let Some(resource) = self.stalled_offer.take() else {
            return;
        };
        if let Err(back) = self.retire.try_push(resource) {
            self.stalled_offer = Some(back);
        }
    }

    /// Drains up to [`MAX_COMMANDS_PER_BLOCK`] commands, and at most one resource offer per kind.
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

        for _ in 0..MAX_COMMANDS_PER_BLOCK {
            let Some(kind) = self.commands.peek().map(Command::kind) else {
                return;
            };
            if matches!(kind, CommandKind::LoadNam | CommandKind::LoadIr) {
                let already = match kind {
                    CommandKind::LoadNam => nam_offered,
                    _ => ir_offered,
                };
                if already || self.retire_backlog || self.retire.slots() < RETIRE_HEADROOM_PER_OFFER
                {
                    self.deferred_blocks += 1;
                    return;
                }
                match kind {
                    CommandKind::LoadNam => nam_offered = true,
                    _ => ir_offered = true,
                }
            }
            let Some(command) = self.commands.try_pop() else {
                return;
            };
            self.apply_command(command);
        }
        // Hit the per-block cap; whatever is left waits for the next block.
        self.deferred_blocks += 1;
    }

    fn apply_command(&mut self, command: Command) {
        match command {
            Command::Param(change) => self.chain.apply(change),
            Command::SetGlobalBypass(on) => self.chain.set_global_bypass(on),
            Command::SetOutputCeilingDb(db) => self.chain.set_output_ceiling_db(db),
            Command::Reset => self.chain.reset(),
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
        for entry in sink.entries() {
            telemetry.push(entry);
        }
    }
}
