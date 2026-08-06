//! NFR-SEC-020: "Namir shall impose a documented upper bound on the resources a single file may
//! cause it to allocate, and shall reject a file that exceeds it with a clear message rather than
//! exhausting memory." One number, shared by every crate that reads an untrusted file off disk —
//! `.nam` models, IR WAVs (via `namir-worker`'s file loader), state/preset documents and the
//! library index (`namir-state`, `namir-library`) — so the bound is documented once rather than
//! risking four crates drifting to four different figures for what is meant to be one policy.
//!
//! # Provenance (added M5)
//!
//! This constant lived in `crates/namir-worker/src/lib.rs` from M4 onward, the only crate that
//! needed a byte ceiling at the time. M5 adds `namir-library`, which reads files off disk directly
//! (via its own `ScanFs` port, never through `namir-worker`) and needs the same bound — but D-5.1's
//! layering forbids `namir-library` from depending on `namir-worker`. Moving the constant to
//! `namir-core`, which both may depend on, is what keeps this "one documented bound" rather than
//! two copies of the same figure that could silently drift apart. `namir-worker` re-exports it
//! under its original name so nothing outside this crate had to change.

/// The upper bound, in bytes, on a single untrusted file Namir will read into memory in one piece
/// — deliberately larger than NFR-PERF-050's 50 MB *performance* target (a file between 50 MB and
/// this ceiling is accepted but not promised to load within budget) and smaller than what would let
/// a hostile or corrupted file exhaust memory on a modest machine outright.
pub const MAX_FILE_BYTES: usize = 256 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_larger_than_the_nfr_perf_050_performance_target() {
        // NFR-PERF-050's own figure, restated here as a literal rather than imported, so this
        // test fails loudly if the two ever drift instead of silently passing either way.
        // black_box hides the comparison's constant-ness from clippy's assertions-on-constants
        // lint -- both operands really are `const` today, and that's the point of the test.
        const NFR_PERF_050_TARGET_BYTES: usize = 50 * 1024 * 1024;
        assert!(
            std::hint::black_box(MAX_FILE_BYTES) > std::hint::black_box(NFR_PERF_050_TARGET_BYTES)
        );
    }
}
