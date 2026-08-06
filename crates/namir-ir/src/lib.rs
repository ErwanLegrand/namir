//! WAV decoding, resample-to-engine-rate, and non-uniform partitioned convolution
//! (`docs/03-implementation-roadmap.md` §6's `namir-ir` deliverable; `docs/02-architecture.md`
//! D-9.4 through D-9.7, §9.2). This crate does **not** do the HP/LP filtering or level control
//! FR-IR-070 describes — those belong to `namir-engine`'s `Ir` stage, a separate, later piece of
//! work that consumes this crate's public API.
//!
//! # The `PreparedIr` / `IrState` split (D-9.1, D-8.2)
//!
//! [`PreparedIr`] holds only immutable per-partition FFT machinery (spectra, plans) and is
//! `Sync`; all per-instance mutable convolution state (ring buffers, input accumulators, the
//! stream time counter) lives in [`IrState`], which is never shared. This mirrors
//! `namir-nam`'s `PreparedNam` / `NamState` split exactly — see `namir-nam/src/wavenet.rs`'s
//! module doc comment for the rationale this crate reuses without changes: it is what lets
//! `namir-engine`'s D-8.2 process-global resource cache hand out one `Arc<PreparedIr>` to every
//! plugin instance loading the same IR file (FR-CLAP-090) while each instance still gets its own
//! independent convolution state in its own `IrState`.
//!
//! # Provenance
//!
//! This crate is ported from `spikes/s2-ir-convolution`, a from-scratch Rust non-uniform
//! partitioned convolution engine whose partition-schedule geometry and causality derivation are
//! confirmed by that spike's own worked proof (see its README's "Causality note"), and whose
//! measured defaults (`growth_factor = 2`, `max_partition = 8192`) come from that spike's S-2
//! cost-curve sweep (`docs/02-architecture.md` §19, D-9.6). The port changes three things beyond
//! the spike, all required by this crate's build instructions and documented at
//! [`convolver`]'s module doc comment: the `PreparedIr`/`IrState` split above; R-8's same-size-
//! partition phase-staggering fix, built in from the start rather than retrofitted; and, in
//! [`wav`], catalogued `Result`-based rejection of every untrusted-input failure mode in place of
//! the spike's `panic!`/`assert!` (P6, NFR-SEC-020), matching `namir-nam/src/wavenet.rs`'s own
//! precedent for porting a spike into a hardened crate.
//!
//! # Scope
//!
//! In scope: FR-IR-010 (WAV decode: mono/stereo, 16/24/32-bit int, 32-bit float,
//! `8_000..=192_000` Hz), FR-IR-030 (resample to the engine's rate), FR-IR-040/D-9.4
//! (zero-latency-by-construction non-uniform partitioned convolution), FR-IR-050/D-9.7 (IRs up to
//! a documented 10-second-at-engine-rate ceiling, processed in full; longer ones truncated with
//! the truncation reported), D-9.5 (a permanent direct-convolution reference the partitioned path
//! is verified against), R-8 (same-size-partition phase staggering).
//!
//! Out of scope, deliberately, for this crate:
//! - **FR-IR-070** (high-pass/low-pass shaping and level control around the convolution) —
//!   `namir-engine`'s `Ir` stage's job, built on top of this crate's public API.
//! - **FR-IR-020** (Should: AIFF/FLAC decode) — this crate reads WAV only.
//! - **FR-IR-080** (Should: dual IR slots / crossfaded IR swap) — a stage-level concern, same
//!   category as `namir-nam`'s out-of-scope crossfaded model swap.
//! - **FR-IR-090** (Should: IR normalization/loudness matching) — not implemented; a caller gets
//!   back exactly the gain the source WAV encodes.

mod convolver;
mod error_codes;
mod wav;

pub use convolver::{
    DEFAULT_GROWTH_FACTOR, DEFAULT_MAX_PARTITION, IrState, PreparedIr, StageSpec, build_schedule,
    direct_convolve,
};
pub use error_codes::IrLoadError;
