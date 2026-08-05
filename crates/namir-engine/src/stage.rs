use crate::param::ParamChange;
use crate::prepare::{PrepareContext, PrepareError};
use crate::stage_io::StageIo;
use crate::telemetry::TelemetrySink;

/// D-6.1. Non-RT: runs on a worker thread, may allocate, may fail, may take milliseconds.
pub trait StagePrep {
    type Prepared: Stage;
    fn prepare(&self, ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError>;
}

/// D-6.1. RT: runs on the audio thread. Must not allocate, lock, block, or fail — enforced in
/// this crate by `unsafe_code = "forbid"` ruling out the usual escape hatches, by review
/// (D-16.3's stated "honest limitation": this can't be fully proven by tooling), and by the
/// D-7.5 harness every engine test runs `Stage::process` under.
pub trait Stage: Send {
    fn process(&mut self, io: &mut StageIo<'_>);
    fn reset(&mut self);
    fn latency_samples(&self) -> u32;
    fn tail_samples(&self) -> u32;
    fn apply(&mut self, change: ParamChange);
    fn telemetry(&self, out: &mut TelemetrySink<'_>);
}
