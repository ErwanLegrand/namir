//! The `Stage` trait, the chain, RT-safe scheduling, resource handover, telemetry (D-5.1).
//!
//! See `docs/02-architecture.md` §6 (stage abstraction), §7 (threading/RT strategy) and §8
//! (resource lifecycle) for the rationale. This crate implements D-6.1's two-lifecycle trait
//! split, D-6.2's chain-owned scratch buffers, and D-7.5's RT-allocation test harness. Not yet
//! implemented, and out of scope for this task: the SPSC command ring (D-7.2), the lock-free
//! telemetry ring (D-7.3 — `telemetry` here is only the trait-facing shape), and the four-step
//! resource handover protocol (D-8.1). 1.0's six product stages (Trim/Gate/Nam/Ir/Eq/Out) are
//! also out of scope; `test_support` has a minimal real stage used only to exercise `Chain` and
//! the RT harness.

mod chain;
mod param;
mod prepare;
mod stage;
mod stage_io;
mod telemetry;

pub mod error_codes;

#[cfg(test)]
mod rt_harness;
#[cfg(test)]
mod test_support;

pub use chain::Chain;
pub use param::{ParamChange, ParamId};
pub use prepare::{PrepareContext, PrepareError};
pub use stage::{Stage, StagePrep};
pub use stage_io::StageIo;
pub use telemetry::{TelemetryEntry, TelemetrySink};
