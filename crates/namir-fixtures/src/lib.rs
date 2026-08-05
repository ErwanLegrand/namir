//! D-19.1: generated (never captured) test fixtures — `.nam` models, IR test signals, and
//! mutation utilities for fuzzing. This crate is the "build-time generator from a fixed seed"
//! D-19.1 mandates: everything in it is deterministic from a seed, nothing reaches for OS
//! randomness, and nothing here is captured audio.
//!
//! - [`nam`] — WaveNet `.nam` fixtures (parity + performance rows).
//! - [`ir`] — convolution correctness fixtures (delta / delayed delta / decaying noise /
//!   designed minimum-phase).
//! - [`mutate`] — seeded mutation operators for fuzzer-corpus seeding (robustness row).
//!
//! Prior art: `spikes/s1-nam-inference` (constrained-init WaveNet generation) and
//! `spikes/s2-ir-convolution` (the `fixtures` module this crate's `ir` module ports).

pub mod ir;
pub mod mutate;
pub mod nam;
