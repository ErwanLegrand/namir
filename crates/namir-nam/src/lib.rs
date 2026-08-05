//! Parses `.nam` model files and runs WaveNet inference (D-5.1's role for this crate: NAM model
//! loading and inference, nothing else — resampling, crossfaded handover, loudness calibration
//! and cost reporting are other crates'/stages' jobs, see the scope note below).
//!
//! # The `PreparedNam` / `NamState` split (D-9.1, D-8.2)
//!
//! [`PreparedNam`] holds only immutable weights and configuration and is `Sync`; all per-instance
//! mutable inference state (dilated-conv history, reusable scratch) lives in [`NamState`], which
//! is never shared. This is structural, not conventional: it is what lets `namir-engine`'s D-8.2
//! process-global resource cache hand out one `Arc<PreparedNam>` to every plugin instance loading
//! the same model (FR-CLAP-090) while each instance still gets its own independent inference
//! history in its own `NamState`. `docs/02-architecture.md` §9.1 documents that
//! `NeuralAmpModelerCore`'s C++ `Conv1D` does *not* have this split — it couples weights and
//! mutable ring-buffer state in one object — which is exactly the coupling this design avoids by
//! construction rather than by convention.
//!
//! # Provenance
//!
//! This crate is ported from `spikes/s1-nam-inference`, a from-scratch Rust WaveNet inference
//! engine whose operation order and flat-weight-array layout were confirmed by reading
//! `NeuralAmpModelerCore`'s C++ source directly (see that spike's README) and validated against
//! it to -131 dB error (FR-NAM-030 requires only -90 dB). The port changes exactly one thing
//! structurally: every place the spike used `panic!`/`assert!` to reject malformed input (because
//! it only ever saw its own trusted generator's output) is replaced here with a catalogued,
//! `Result`-based rejection (P6: "untrusted input is parsed in one hardened place per format, and
//! that place is fuzzed"; FR-NAM-040; NFR-QUAL-040: "shall not panic, hang, over-allocate ... on
//! any input"). The algorithm itself — weight layout, the two-signal chaining between layer
//! arrays, the trailing `head_scale` float — is unchanged; see `wavenet.rs`'s module doc comment
//! for the details this crate relies on the spike having already confirmed.
//!
//! # Scope
//!
//! In scope: FR-NAM-010 (load/validate `.nam` files by content), FR-NAM-020 (**WaveNet only** —
//! see below), FR-NAM-040 (malformed files rejected with a specific reason, never a panic),
//! FR-NAM-080 (metadata), FR-NAM-110 (latency = 0, this WaveNet is causal and block-preserving).
//!
//! Out of scope, deliberately, for this crate:
//! - FR-NAM-050/060 (resampling to the model's declared sample rate) — a stage wrapping this one.
//! - FR-NAM-070 (crossfaded model-swap handover) — `namir-engine`'s job.
//! - FR-NAM-090/100 (loudness normalisation/calibration) — needs metadata fields the current
//!   `.nam` schema this crate reads doesn't carry.
//! - FR-NAM-120 (computational cost reporting) — needs a benchmark harness.
//! - **LSTM.** FR-NAM-020 lists `WaveNet` and `LSTM` as both Must. The S-1 spike's own scope note
//!   says "LSTM is unaddressed and remains an open implementation risk" — this crate inherits
//!   that exact scope boundary unchanged. A `.nam` file whose `architecture` is not `"WaveNet"`
//!   (including `"LSTM"`) is rejected the same way any other unsupported architecture is, via
//!   [`NamLoadError`] with `error_codes::UNSUPPORTED_ARCHITECTURE`, not silently misread.

mod error_codes;
mod file;
mod wavenet;

pub use error_codes::NamLoadError;
pub use file::{LayerArrayConfig, NamFile, NamMetadata, WaveNetConfig};
pub use wavenet::{NamState, PreparedNam, load};
