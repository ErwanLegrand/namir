//! The `Stage` trait, the chain, RT-safe scheduling, resource handover, telemetry (D-5.1).
//!
//! See `docs/02-architecture.md` §6 (stage abstraction), §7 (threading/RT strategy) and §8
//! (resource lifecycle) for the rationale. This crate implements D-6.1's two-lifecycle trait
//! split, D-6.2's chain-owned scratch buffers, D-7.5's RT-allocation test harness, and — since M4
//! — the whole audio-thread side of the threading model: D-7.2's SPSC command ring and D-8.1's
//! return ring (`ring`, on `rtrb`), D-7.3's lock-free telemetry ring (`telemetry_ring`, on plain
//! atomics), and the four-step handover protocol wired end to end in [`AudioEngine`].
//!
//! Two entry points, deliberately. [`build_default_engine`] is the product path: it returns an
//! [`AudioEngine`] for the audio thread and a [`WorkerEndpoint`] for a worker, already paired.
//! [`build_default_chain`] returns a bare [`Chain`] with no rings, and remains what the benchmarks
//! and the stage-level tests use.
//!
//! `telemetry` is still the per-block, borrowed-buffer sink a `Stage` writes into; `telemetry_ring`
//! is the cross-thread structure those readings are then published into. Both exist on purpose —
//! see `telemetry_ring`'s module doc for why the sink was given a destination rather than replaced.
//!
//! `test_support` has a minimal real stage used only to exercise `Chain` and the RT harness; it is
//! not one of the six.

mod chain;
mod command;
mod engine;
mod param;
mod prepare;
mod resource;
mod ring;
mod stage;
mod stage_io;
mod telemetry;
mod telemetry_ring;

pub mod error_codes;
pub mod stages;

#[cfg(test)]
mod rt_harness;
#[cfg(test)]
mod test_support;

pub use chain::Chain;
pub use command::{Command, CommandKind, RetireSink};
pub use engine::{AudioEngine, RingCapacities, WorkerEndpoint, build_default_engine, split};
pub use param::{ParamChange, ParamId};
pub use prepare::{PrepareContext, PrepareError};
pub use resource::{Resource, ResourceKind};
pub use ring::{RingConsumer, RingProducer, ring};
pub use stage::{Stage, StagePrep};
pub use stage_io::StageIo;
pub use stages::{HANDOVER_CROSSFADE_MS, build_default_chain};
pub use telemetry::{TelemetryEntry, TelemetrySink};
pub use telemetry_ring::{TelemetryDrain, TelemetryProducer, TelemetryReader, telemetry_ring};
