use crate::param::ParamChange;
use crate::prepare::{PrepareContext, PrepareError};
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
}
