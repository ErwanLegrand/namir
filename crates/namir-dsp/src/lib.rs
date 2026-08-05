//! D-5.1's "Primitive DSP: biquads, gate detector, meters, gain ramps, DC blocker" — the
//! reusable signal-processing building blocks used by `namir-engine`'s product stages
//! (Trim/Gate/Nam/Ir/Eq/Out). This crate has no notion of a `Stage`, a chain, or parameter IDs;
//! it only knows how to turn one set of numeric controls into a sample-accurate DSP operation on
//! a `&mut [f32]` buffer. Stage assembly, ordering (e.g. D-9.8's gate-before-trim), and telemetry
//! wiring belong to `namir-engine`, not here.

mod biquad;
mod dc_blocker;
mod gain_ramp;
mod gate;
mod meter;

#[cfg(test)]
mod rt_harness;

pub use biquad::{Biquad, BiquadCoeffs, FilterKind};
pub use dc_blocker::DcBlocker;
pub use gain_ramp::GainRamp;
pub use gate::{GateParams, GateStatus, NoiseGate};
pub use meter::Meter;
