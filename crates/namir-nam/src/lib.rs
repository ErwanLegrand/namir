//! Parses `.nam` model files and runs WaveNet or LSTM inference (D-5.1's role for this crate: NAM
//! model loading and inference, nothing else — resampling, crossfaded handover, loudness
//! calibration and cost reporting are other crates'/stages' jobs, see the scope note below).
//!
//! # The `PreparedNam` / `NamState` split (D-9.1, D-8.2)
//!
//! [`PreparedNam`] holds only immutable weights and configuration and is `Sync`; all per-instance
//! mutable inference state (WaveNet's dilated-conv history, LSTM's per-cell `h`/`c`, either way
//! reusable scratch) lives in [`NamState`], which is never shared. This is structural, not
//! conventional: it is what lets `namir-engine`'s D-8.2 process-global resource cache hand out
//! one `Arc<PreparedNam>` to every plugin instance loading the same model (FR-CLAP-090) while
//! each instance still gets its own independent inference history in its own `NamState`.
//! `docs/02-architecture.md` §9.1 documents that `NeuralAmpModelerCore`'s C++ `Conv1D` (and, per
//! `lstm.rs`'s own module doc comment, its `LSTMCell` too) does *not* have this split — it
//! couples weights and mutable state in one object — which is exactly the coupling this design
//! avoids by construction rather than by convention, for both architectures. `PreparedNam`
//! itself is a small enum over the two architectures' own `Prepared*`/`*State` pairs; see
//! `model.rs`'s doc comment for why that's invisible to every caller.
//!
//! # Provenance
//!
//! This crate's WaveNet support is ported from `spikes/s1-nam-inference`, a from-scratch Rust
//! WaveNet inference engine whose operation order and flat-weight-array layout were confirmed by
//! reading `NeuralAmpModelerCore`'s C++ source directly (see that spike's README) and validated
//! against it to -131 dB error (FR-NAM-030 requires only -90 dB) — the spike's *own* number, for
//! its own from-scratch implementation, not this crate's. The port changes exactly one thing
//! structurally: every place the spike used `panic!`/`assert!` to reject malformed input (because
//! it only ever saw its own trusted generator's output) is replaced here with a catalogued,
//! `Result`-based rejection (P6: "untrusted input is parsed in one hardened place per format, and
//! that place is fuzzed"; FR-NAM-040; NFR-QUAL-040: "shall not panic, hang, over-allocate ... on
//! any input"). The algorithm itself — weight layout, the two-signal chaining between layer
//! arrays, the trailing `head_scale` float — is unchanged; see `wavenet.rs`'s module doc comment
//! for the details this crate relies on the spike having already confirmed.
//!
//! LSTM support has no spike behind it: `lstm.rs`'s module doc comment records, in the same
//! spirit, exactly which facts about `NeuralAmpModelerCore`'s `NAM/lstm.h`/`NAM/lstm.cpp` it
//! relies on and were read directly from that source for this work.
//!
//! M10 closed the actual FR-NAM-030 gap for *this crate's own code* (rather than the spike's):
//! `tests/golden_reference.rs` compares `PreparedNam::process`'s real output against a real
//! `NeuralAmpModelerCore` render, for both architectures — WaveNet to -137 dB, LSTM to the bit
//! once the reference's default silent-prewarm behavior is matched (that file's own doc comment
//! explains why prewarming is a host-convenience default the reference DSP wrapper applies, not
//! part of the LSTM model's own mathematical definition, and so is reproduced only in the test,
//! not in this crate's production `LstmState` initialization).
//!
//! # Scope
//!
//! In scope: FR-NAM-010 (load/validate `.nam` files by content), FR-NAM-020 (both Must
//! architectures — WaveNet, `wavenet.rs`; LSTM, `lstm.rs` — unified behind
//! [`PreparedNam`]/[`NamState`] by `model.rs`'s small architecture-dispatching enum), FR-NAM-030
//! (M10: `tests/golden_reference.rs` — see the Provenance section above), FR-NAM-040 (malformed
//! files rejected with a specific reason, never a panic), FR-NAM-080 (metadata, both the full
//! parse's `NamFile::metadata`/`LstmFile::metadata` and, added M5, [`probe_metadata`]'s
//! weights-free read for `namir-library`'s FR-LIB-040 search index), FR-NAM-090 (M10: this
//! crate's own contribution is narrow — parsing and exposing `metadata.loudness` via
//! [`NamMetadata::loudness`] and each architecture's `loudness_lufs()` accessor; *applying* the
//! normalisation gain is `namir-engine`'s Nam stage's job, not this crate's, per D-5.1), FR-NAM-110
//! (latency = 0 — both architectures are causal and block-preserving, see each module's own doc
//! comment), FR-NAM-140 (M10: a supported-architecture file whose `config` uses a feature this
//! build does not implement is rejected with `error_codes::UNSUPPORTED_CONFIGURATION`/
//! `INCONSISTENT_CONFIGURATION`, distinct from `MALFORMED_JSON`), FR-NAM-150 (M10: core NAM
//! Architecture 2 — D-9.12's scope, `wavenet.rs`'s module doc comment). A `.nam` file whose
//! `architecture` is neither `"WaveNet"` nor `"LSTM"` is rejected via [`NamLoadError`] with
//! `error_codes::UNSUPPORTED_ARCHITECTURE`; a `"WaveNet"` file using a feature this build does not
//! implement (grouped/gated convolution, FiLM, `condition_dsp`, `slimmable`, ...) is rejected with
//! a distinct code naming the feature (FR-NAM-140) — neither case is silently misread.
//!
//! Out of scope, deliberately, for this crate:
//! - FR-NAM-050/060 (resampling to the model's declared sample rate) — a stage wrapping this one.
//! - FR-NAM-070 (crossfaded model-swap handover) — `namir-engine`'s job.
//! - FR-NAM-100 (dBu-calibrated operating levels, user-stated interface sensitivity) — a distinct,
//!   Should-priority requirement from FR-NAM-090 above; this crate reads neither
//!   `input_level_dbu` nor `output_level_dbu`, and no calibrated-mode UI exists anywhere.
//! - FR-NAM-120 (computational cost reporting) — needs a benchmark harness.
//! - Parametric/conditioning inputs for either architecture (WaveNet's `condition_size == 1`
//!   restriction, `wavenet.rs`; LSTM's `input_size == in_channels == out_channels == 1`
//!   restriction, `lstm.rs`) — both only ever feed the raw mono signal as input in 1.0 scope.

mod error_codes;
mod file;
mod lstm;
mod model;
mod probe;
mod shared;
mod wavenet;

pub use error_codes::NamLoadError;
pub use file::{
    ActivationEntry, ActivationParams, ActivationSpec, Conv1x1FeatureConfig, FilmConfig,
    LayerArrayConfig, LayerArrayHeadConfig, LstmConfigJson, LstmFile, NamFile, NamMetadata,
    WaveNetConfig,
};
pub use model::{NamState, PreparedNam, load};
pub use probe::{NamProbe, probe_metadata};
