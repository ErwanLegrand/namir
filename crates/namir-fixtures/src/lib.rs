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
// The manifest the method names is `assets.lock` beside this crate's `Cargo.toml`, added M14 and
// gated by `cargo run -p xtask -- assets`: every checked-in file under `crates/` that is not source
// or build configuration, with its size, its BLAKE3 and a declared provenance. This crate stays the
// *generator* D-19.1 mandates; the manifest is the record NFR-LIC-050 asks for on top of it.
//
// The gap was larger than the field that recorded it said: it named fifteen assets under
// `crates/*/fuzz/corpus`, and the real count is 33 -- the fuzz corpora plus `namir-nam`'s five
// golden `.nam`/`.wav` files, `namir-state`'s five `.namirpreset` corpus documents,
// `namir-clap`'s golden input vector and preset, and `namir-ui`'s brand-mark blob.
// trace: NFR-LIC-050

pub mod ir;
pub mod library;
pub mod mutate;
pub mod nam;
pub mod resample_response;
