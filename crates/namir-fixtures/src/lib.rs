//! D-19.1: generated (never captured) test fixtures — `.nam` models, IR test signals, and
//! mutation utilities for fuzzing. This crate is the "build-time generator from a fixed seed"
//! D-19.1 mandates: everything in it is deterministic from a seed, nothing reaches for OS
//! randomness, and nothing here is captured audio.
//!
//! - [`nam`] — WaveNet `.nam` fixtures (parity + performance rows).
//! - [`ir`] — convolution correctness fixtures (delta / delayed delta / decaying noise /
//!   designed minimum-phase).
//! - [`mutate`] — seeded mutation operators for fuzzer-corpus seeding (robustness row).
//! - [`resample_response`] — M9b's frequency-response instrument for FR-NAM-060/FR-IR-030: a
//!   measuring device rather than a fixture, kept here because the two crates that own a
//!   resampler must be measured against the same yardstick, not two copies of one.
//! - [`library`] — M5's cached, 10,000-file synthetic model+IR library (FR-LIB-020,
//!   NFR-PERF-060/FR-LIB-030, FR-LIB-070; earmarked for M6's FR-UI-060).
//!
//! Prior art: `spikes/s1-nam-inference` (constrained-init WaveNet generation) and
//! `spikes/s2-ir-convolution` (the `fixtures` module this crate's `ir` module ports).
//!
// trace-partial: NFR-LIC-050
// uncovered: NFR-LIC-050 — the manifest the method names does not exist, and the traced artifact
// uncovered: is a runtime generator whose output is never committed: the fifteen assets tracked
// uncovered: under crates/*/fuzz/corpus, including a .wav impulse response and a .nam model, are
// uncovered: recorded nowhere and no check would catch a captured asset added beside them;
// uncovered: closes M9b

pub mod ir;
pub mod library;
pub mod mutate;
pub mod nam;
pub mod resample_response;
