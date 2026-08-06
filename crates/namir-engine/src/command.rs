//! D-7.2's command records — what travels on the inbound ring — and [`RetireSink`], the
//! audio-thread end of D-8.1's return ring.
//!
//! The two live together because they are the two halves of one contract: a [`Command`] is how a
//! resource gets *in*, a [`RetireSink`] is the only way one gets *out*, and neither may ever drop
//! what it carries.

use std::sync::Arc;

use namir_ir::PreparedIr;
use namir_nam::PreparedNam;

use crate::prepare::PrepareContext;
use crate::resource::{Resource, ResourceKind};
use crate::ring::RingProducer;
use crate::stages::ir::IrSlot;
use crate::stages::nam::NamSlot;

/// What a [`Command`] will do, readable without consuming it.
///
/// This exists so the audio thread's drain can check "do I have room to absorb this?" *before*
/// popping — a command it pops but cannot handle would have to be held somewhere, and D-7.2 is
/// explicit that a command is never dropped. Leaving it in the ring is simpler and strictly safer
/// than inventing a holding pen for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// A parameter update.
    Param,
    /// A prepared NAM model, to be installed with a crossfade.
    LoadNam,
    /// A prepared impulse response, to be installed with a crossfade.
    LoadIr,
    /// M5: unload the Nam stage's model, crossfading to dry (FR-STATE-070's "the state shall
    /// load with that stage empty"). See [`Command::Unload`].
    UnloadNam,
    /// M5: unload the Ir stage's impulse response, crossfading to dry. See [`Command::Unload`].
    UnloadIr,
    /// A global-bypass change (FR-CHAIN-030).
    SetGlobalBypass,
    /// An output-ceiling change in dB (FR-CHAIN-090).
    SetOutputCeilingDb,
    /// Clear every stage's internal state.
    Reset,
}

/// One record on D-7.2's inbound command ring.
///
/// Fixed-size and pointer-carrying, per D-7.2's own consequence clause — see
/// [`crate::resource`]'s module doc for why the resource variants box their slot rather than
/// inlining it.
///
/// Deliberately not padded out speculatively: these are the operations that exist today, mirroring
/// the methods [`crate::Chain`] already exposes. M5's preset recall and M6's host-driven bypass add
/// variants when they exist, not before.
pub enum Command {
    /// A parameter update, routed exactly as [`crate::Chain::apply`] routes it.
    Param(crate::param::ParamChange),
    /// D-8.1 step 2: install a prepared resource, beginning a crossfade.
    Load(Resource),
    /// M5's mirror image of [`Self::Load`]: FR-STATE-070's "the state shall load with that
    /// stage empty" has no way to say so without this. Names the *stage*, not a resource —
    /// there is nothing to hand over — and starts exactly the same crossfade `Load` starts,
    /// fading toward `None` rather than toward a new slot. `NamStage`/`IrStage`'s existing
    /// dry-passthrough handling of a `None` slot mid-crossfade (see `stages/nam.rs`'s module
    /// doc comment) is the entire mechanism; this needs no new DSP. Consequence note under
    /// D-8.1 (`docs/02-architecture.md`): an unload is a handover to nothing, and is therefore
    /// also subject to R-7's serialisation rule.
    Unload(ResourceKind),
    /// FR-CHAIN-030's global bypass.
    SetGlobalBypass(bool),
    /// FR-CHAIN-090's output ceiling, in dB.
    SetOutputCeilingDb(f32),
    /// Clear every stage's internal state, e.g. on transport stop/reposition.
    Reset,
}

impl Command {
    /// **Not RT-safe — this is D-8.1 step 1, and it belongs on a worker thread.**
    ///
    /// Builds this instance's whole Nam slot: `PreparedNam::new_state`'s inference scratch and,
    /// when `model`'s declared rate differs from `ctx`'s engine rate, the D-9.2 resampler pair and
    /// its FIFOs. All of that allocates, which is exactly why it happens here rather than at
    /// install time — by the time the audio thread sees this command, there is nothing left to do
    /// but move a pointer.
    ///
    /// `ctx` must be the `PrepareContext` the target chain was prepared with. A mismatch is
    /// *checked* by the receiving stage rather than trusted, and degrades to a retirement plus a
    /// fault reading rather than a wrongly-sized buffer.
    pub fn load_nam(model: Arc<PreparedNam>, ctx: &PrepareContext) -> Self {
        let slot = NamSlot::new(model, ctx.sample_rate(), ctx.max_block_size());
        Self::Load(Resource::nam(Box::new(slot), *ctx))
    }

    /// **Not RT-safe — D-8.1 step 1, worker-side.** The Ir analogue of [`Self::load_nam`]; builds
    /// this instance's `IrState` (its convolution ring buffers and accumulators).
    pub fn load_ir(ir: Arc<PreparedIr>, ctx: &PrepareContext) -> Self {
        let slot = IrSlot::new(ir);
        Self::Load(Resource::ir(Box::new(slot), *ctx))
    }

    /// What this command will do, without consuming it. See [`CommandKind`].
    pub fn kind(&self) -> CommandKind {
        match self {
            Self::Param(_) => CommandKind::Param,
            Self::Load(r) => match r.kind() {
                ResourceKind::Nam => CommandKind::LoadNam,
                ResourceKind::Ir => CommandKind::LoadIr,
            },
            Self::Unload(kind) => match kind {
                ResourceKind::Nam => CommandKind::UnloadNam,
                ResourceKind::Ir => CommandKind::UnloadIr,
            },
            Self::SetGlobalBypass(_) => CommandKind::SetGlobalBypass,
            Self::SetOutputCeilingDb(_) => CommandKind::SetOutputCeilingDb,
            Self::Reset => CommandKind::Reset,
        }
    }
}

/// The audio thread's write end of D-8.1's return ring, as handed to
/// [`crate::Stage::collect_retired`] for the duration of one pass.
///
/// A wrapper rather than a bare [`RingProducer`] for one reason worth stating plainly: it exposes
/// *only* a push that hands the value back on failure. A stage physically cannot drop a retired
/// resource through this type, and that impossibility is the invariant D-8.1 step 4 is made of.
/// Deliberately shaped like [`crate::TelemetrySink`] — a borrowed, fixed-capacity destination a
/// `Stage` has no way to grow — so the two RT-facing sinks read the same way.
pub struct RetireSink<'a> {
    producer: &'a mut RingProducer<Resource>,
    rejected: usize,
}

impl<'a> RetireSink<'a> {
    /// Wraps the engine-owned return-ring producer for one `collect_retired` pass.
    pub(crate) fn new(producer: &'a mut RingProducer<Resource>) -> Self {
        Self {
            producer,
            rejected: 0,
        }
    }

    /// Hands `resource` to the worker, or gives it **back** as `Err(resource)` if the ring is full.
    ///
    /// Wait-free and allocation-free. A caller that gets `Err` must keep holding the value and
    /// retry on a later block — see D-8.1's degradation clause ("If the worker dies, the ring fills
    /// and memory is retained but audio continues. Degradation, not failure") and
    /// `stages/nam.rs`'s deferred finalization.
    ///
    /// The return type is `Result<(), Resource>` rather than `rtrb`'s own error precisely so the
    /// value is impossible to discard by accident. **If you are reading this because a lint
    /// suggested `let _ = sink.push(..)` or `.ok()` — don't.** Either would deallocate on the audio
    /// thread, which is the exact P1 violation this milestone exists to remove.
    #[must_use = "a rejected resource must be retained and retried, never dropped (D-8.1 step 4)"]
    pub fn push(&mut self, resource: Resource) -> Result<(), Resource> {
        match self.producer.try_push(resource) {
            Ok(()) => Ok(()),
            Err(back) => {
                self.rejected += 1;
                Err(back)
            }
        }
    }

    /// How many pushes this pass rejected. The engine turns a nonzero count into back-pressure on
    /// the next block's command drain, so a stage is never offered a resource it has no room to
    /// park the displaced one into.
    pub fn rejected(&self) -> usize {
        self.rejected
    }
}
