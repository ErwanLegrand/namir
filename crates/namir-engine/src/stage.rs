use crate::command::RetireSink;
use crate::param::ParamChange;
use crate::prepare::{PrepareContext, PrepareError};
use crate::resource::{Resource, ResourceKind};
use crate::stage_io::StageIo;
use crate::telemetry::TelemetrySink;

/// D-6.1. Non-RT: runs on a worker thread, may allocate, may fail, may take milliseconds.
pub trait StagePrep {
    /// The RT-safe `Stage` this preparation produces.
    type Prepared: Stage;
    /// Builds the RT-safe stage, allocating and sizing everything it will need up front so
    /// `Stage::process` never has to.
    fn prepare(&self, ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError>;
}

/// D-6.1. RT: runs on the audio thread. Must not allocate, lock, block, or fail — enforced in
/// this crate by `unsafe_code = "forbid"` ruling out the usual escape hatches, by review
/// (D-16.3's stated "honest limitation": this can't be fully proven by tooling), and by the
/// D-7.5 harness every engine test runs `Stage::process` under.
///
/// # Why the two resource-handover methods carry default bodies (M4)
///
/// **Decision:** [`Stage::accept_resource`] and [`Stage::collect_retired`] have default no-op
/// bodies; the original six methods deliberately do not.
///
/// **Rationale:** the original six are *universal* stage concerns — every stage processes, resets,
/// reports latency, and so on, so a missing impl is always a bug and the compiler should say so.
/// Holding a swappable resource is not universal: it is an opt-in capability of exactly two of the
/// fixed six stages (Nam and Ir). Requiring the other four, plus `test_support`'s three fakes, to
/// each write two empty methods would be noise that obscures the two impls that matter.
///
/// **Consequence, and the trap to be aware of:** a resource-holding stage that forgets to override
/// `accept_resource` will silently swallow every handover offered to it — the offer stays `Some`,
/// the chain hands it straight back to the return ring, and nothing anywhere errors. If you are
/// adding a stage that owns a resource, these defaults are the thing that will hide the bug.
///
/// **Alternatives rejected:** making `Stage` an `Any` supertrait and downcasting to the concrete
/// `NamStage`/`IrStage` — that requires a `'static` bound this trait deliberately does not have,
/// and makes *type* the addressing mechanism for the chain, which is directly at odds with D-10.2
/// (a `u32` id addresses a parameter) and with RD-2's future dynamic chain, where two NAM stages
/// would be indistinguishable by type. Returning typed handles from `build_default_chain` — the
/// concrete stage is moved into the `Box<dyn Stage>`, so this needs either aliasing
/// (`Rc<RefCell<..>>`, which would cost `Chain`'s `Send`) or indices plus downcasting, collapsing
/// into the first option.
pub trait Stage: Send {
    /// Processes one block of audio in place. Must not allocate, lock, block, or fail; see this
    /// trait's doc comment for how that's enforced.
    fn process(&mut self, io: &mut StageIo<'_>);
    /// Clears internal state (filter memory, envelope followers, etc.) without changing
    /// parameters.
    fn reset(&mut self);
    /// Fixed processing delay this stage adds, in samples.
    fn latency_samples(&self) -> u32;
    /// How many samples of non-negligible output this stage can still produce after its own
    /// input goes silent (e.g. convolution/reverb decay); `0` if none.
    fn tail_samples(&self) -> u32;
    /// Applies a parameter update, ignoring any `ParamId` this stage does not own (see
    /// `Chain::apply`'s doc comment).
    fn apply(&mut self, change: ParamChange);
    /// Writes this stage's current telemetry readings into `out`.
    fn telemetry(&self, out: &mut TelemetrySink<'_>);

    /// D-8.1 step 2 ("offer"): the chain broadcasts one prepared resource to every stage, and the
    /// stage that owns that *kind* of resource takes it out of the `Option` and begins its
    /// crossfade. Every other stage leaves it exactly as it found it.
    ///
    /// A resource no stage takes stays `Some` and is returned to the caller, which pushes it into
    /// the return ring — it is never dropped here.
    ///
    /// Default: no-op (see this trait's own doc comment for why these two methods carry defaults
    /// when the other six do not).
    fn accept_resource(&mut self, _offer: &mut Option<Resource>) {}

    /// D-8.1 step 4 ("retire"): moves any resource this stage has finished with into `out`.
    ///
    /// If `out` is full the stage **must take the resource back and keep it**, retrying on a later
    /// block. Dropping it here is the P1 violation the whole return ring exists to prevent, and
    /// [`RetireSink::push`]'s `#[must_use] Result<(), Resource>` is shaped to make that hard to do
    /// by accident.
    ///
    /// Default: no-op.
    fn collect_retired(&mut self, _out: &mut RetireSink<'_>) {}

    /// M5's mirror of [`Stage::accept_resource`]: the chain broadcasts an unload request for
    /// `kind`, and the stage that owns that kind of resource begins a crossfade toward `None` —
    /// the same crossfade an `accept_resource` install begins, just fading toward nothing rather
    /// than toward a new slot. Every other stage ignores a `kind` it does not own, exactly as
    /// [`Stage::apply`] ignores a `ParamId` it does not own.
    ///
    /// Default: no-op — same default the other resource-handover methods carry, and the same
    /// trap applies (see this trait's own doc comment): a resource-holding stage that forgets to
    /// override this will silently ignore every unload request for its kind.
    fn unload_resource(&mut self, _kind: ResourceKind) {}
}
