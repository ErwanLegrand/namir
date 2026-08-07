//! D-8.1's payload, in both directions: what a worker hands the audio thread, and what the audio
//! thread hands back.
//!
//! # One type for the offer and the retirement
//!
//! **Decision:** [`Resource`] is the element type of *both* the offer (D-8.1 step 2) and the return
//! ring (step 4), rather than two separate types.
//!
//! **Rationale:** a resource that no stage accepted and a resource that has finished fading out are
//! the same thing — something the audio thread is holding and must not drop. Giving them one type
//! removes a whole class of "which ring does this belong in" mistakes, and makes the never-drop
//! obligation a property of a single type rather than a rule spread across two.
//!
//! # Why the slot is built by the worker, and boxed
//!
//! **Decision:** a `Resource` carries a fully-built, **boxed** [`NamSlot`]/[`IrSlot`] — not a bare
//! `Arc<PreparedNam>`/`Arc<PreparedIr>`.
//!
//! **Rationale, part one (why a slot):** D-8.1 step 1 says the worker prepares the resource "fully
//! allocated, fully warmed". A bare `Arc` is not that — installing one still requires building this
//! instance's own `NamState` (and, at a mismatched rate, a whole `rubato` resampler pair and its
//! FIFOs), which allocates. M2's `NamStage::load_model` does exactly that and is documented "**Not
//! RT-safe**" for the reason. Moving the slot construction to the worker is what actually makes the
//! install side allocation-free, so the slot is the thing that has to travel. The same argument
//! applies in the retire direction, and `nam.rs`'s own M2 note already made it: dropping a slot
//! "frees its `NamState` scratch **and** may drop the last `Arc<PreparedNam>` reference". Both
//! halves are illegal on the audio thread; D-8.1's rationale names the `Arc` because that is the
//! *subtle* half (it only deallocates when the refcount happens to reach zero, so it hides in
//! testing), not because it is the only half.
//!
//! **Rationale, part two (why boxed):** D-7.2 requires "fixed-size command records" that "contain
//! no owned heap data — a model handover command carries an `Arc<PreparedNam>` (a pointer), never
//! the model." Boxing satisfies that *literally* rather than by argument: a `Resource` is a
//! discriminant plus a pointer plus the context it was prepared for, whatever `NamSlot` grows into.
//! Without the box, the ring's preallocated storage would be sized to the larger of the two slot
//! types and every retirement would `memcpy` several hundred bytes instead of moving a pointer.
//!
//! **Consequence:** the `Box` is allocated by the worker (in [`crate::Command::load_nam`]) and
//! freed by the worker (after popping the return ring). The audio thread only ever moves it.
//! Deref coercion means the stages' own call sites (`slot.process_wet(..)` and friends) are
//! unaffected by the indirection.

use crate::prepare::PrepareContext;
use crate::stages::ir::IrSlot;
use crate::stages::nam::NamSlot;

/// Which stage a [`Resource`] belongs to. Readable without consuming the resource, so a caller can
/// decide whether it has room to absorb one *before* committing to taking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// Belongs to the Nam stage.
    Nam,
    /// Belongs to the Ir stage.
    Ir,
}

/// A prepared resource in flight — either being offered to a stage or being handed back for the
/// worker to drop. See this module's doc comment for why it is one type for both directions.
///
/// Deliberately opaque: `namir-worker` constructs one and drops one, but never needs to look
/// inside, so the slot types stay `pub(crate)` and this crate keeps the freedom to change them.
pub struct Resource {
    /// The context the slot's buffers were sized for. Checked by the receiving stage against its
    /// own rather than trusted — a `Resource` prepared for a different sample rate or block size
    /// would otherwise install silently-wrong-sized buffers.
    ctx: PrepareContext,
    payload: Payload,
}

pub(crate) enum Payload {
    Nam(Box<NamSlot>),
    Ir(Box<IrSlot>),
}

impl Resource {
    pub(crate) fn nam(slot: Box<NamSlot>, ctx: PrepareContext) -> Self {
        Self {
            ctx,
            payload: Payload::Nam(slot),
        }
    }

    pub(crate) fn ir(slot: Box<IrSlot>, ctx: PrepareContext) -> Self {
        Self {
            ctx,
            payload: Payload::Ir(slot),
        }
    }

    /// Which stage this belongs to.
    pub fn kind(&self) -> ResourceKind {
        match self.payload {
            Payload::Nam(_) => ResourceKind::Nam,
            Payload::Ir(_) => ResourceKind::Ir,
        }
    }

    /// The `PrepareContext` this resource's buffers were sized for.
    pub fn context(&self) -> PrepareContext {
        self.ctx
    }

    /// Takes the Nam slot out of `offer` **only if** that is what it holds, leaving `offer`
    /// untouched otherwise.
    ///
    /// The peek-then-take dance lives here rather than being re-derived in both stages: a stage
    /// that got it subtly wrong would silently swallow the other stage's resource, and the
    /// broadcast in [`crate::Chain::offer`] would never notice.
    pub(crate) fn take_nam(offer: &mut Option<Resource>) -> Option<(Box<NamSlot>, PrepareContext)> {
        match offer {
            Some(r) if r.kind() == ResourceKind::Nam => match offer.take() {
                Some(Resource {
                    ctx,
                    payload: Payload::Nam(slot),
                }) => Some((slot, ctx)),
                // Unreachable: the guard above already matched on `kind()`, which reads the same
                // discriminant. Restoring rather than panicking keeps the never-drop obligation
                // intact even if that ever stops being true.
                other => {
                    *offer = other;
                    None
                }
            },
            _ => None,
        }
    }

    /// Takes the Ir slot out of `offer` only if that is what it holds. See [`Self::take_nam`].
    pub(crate) fn take_ir(offer: &mut Option<Resource>) -> Option<(Box<IrSlot>, PrepareContext)> {
        match offer {
            Some(r) if r.kind() == ResourceKind::Ir => match offer.take() {
                Some(Resource {
                    ctx,
                    payload: Payload::Ir(slot),
                }) => Some((slot, ctx)),
                other => {
                    *offer = other;
                    None
                }
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-7.2: "Commands are fixed-size and contain no owned heap data — a model handover command
    /// carries an `Arc<PreparedNam>` (a pointer), never the model." Boxing the slot is what makes
    /// that literally true, so this pins the record's size. If a future change unboxes a slot or
    /// inlines a buffer here, the ring's preallocated storage silently grows by hundreds of bytes
    /// per element and every retirement becomes a large `memcpy` — this test is the tripwire.
    #[test]
    fn a_resource_record_stays_small_and_fixed_size() {
        assert!(
            size_of::<Resource>() <= 48,
            "Resource grew to {} bytes; it should be a discriminant, a pointer, and a \
             PrepareContext (see this module's doc comment)",
            size_of::<Resource>()
        );
    }

    /// A resource is `Send` (it crosses to the audio thread and back) but must not need `Sync`:
    /// exactly one thread owns it at any instant, which is what the two rings enforce.
    #[test]
    fn a_resource_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Resource>();
    }
}
