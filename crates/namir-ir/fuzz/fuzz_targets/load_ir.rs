//! `cargo fuzz` target for `namir_ir::PreparedIr::from_wav_bytes` — the **deep** half of
//! NFR-QUAL-040's "the audio file readers" (M14).
//!
//! # Why a second WAV target rather than a wider first one
//!
//! `probe_wav.rs` beside this file fuzzes `probe_wav`, which `namir-ir/src/wav.rs`'s own doc
//! comment describes as *deliberately shallower*: it parses and validates the header and stops.
//! Everything an attacker-supplied IR could actually do damage with is past that point and was
//! reached by no fuzz target at all —
//!
//! - `wav::decode`'s per-channel `Vec::with_capacity(frames_to_read)`, sized from the header's
//!   declared frame count;
//! - the `MAX_LOAD_SECONDS` clamp that bounds it (NFR-SEC-020), and the sample-reading loop that
//!   fills the vectors;
//! - `resample_mono`'s `rubato` pass, run whenever the file's rate differs from the engine's;
//! - `build_schedule`'s partitioning and the FFT planning that follows it.
//!
//! A hang or an over-allocation lives in one of those, not in the header parse. The two targets are
//! kept separate rather than one being widened: `probe_wav` is a real entry point in its own right
//! (`namir-library`'s scanner calls it on every file it indexes, and calls nothing deeper), so a
//! target that only ever reached it through `from_wav_bytes` would stop covering the scanner's
//! actual code path.
//!
//! # The two leading bytes
//!
//! `from_wav_bytes` takes an engine rate and a block size as well as the file. Both are Namir's
//! own values rather than the attacker's, so they are not fuzzed freely — they are *selected* from
//! the sets the product really uses, by the input's first two bytes, and the rest of the input is
//! the candidate WAV. This is what puts the resampler on the path: with a fixed 48 kHz engine rate
//! most inputs would take the rate-matched branch and `resample_mono` would never run.
//!
//! The bar is exactly NFR-QUAL-040's, as in every other target here: "shall not panic, hang,
//! over-allocate or read out of bounds on any input". `Ok` and `Err` are both acceptable outcomes.

// trace: NFR-QUAL-040, NFR-SEC-010
#![no_main]

use libfuzzer_sys::fuzz_target;
use namir_core::SampleRate;
use namir_ir::PreparedIr;

/// FR-IR-030's engine rates. Includes both ends of the supported range so a file at 8 kHz against
/// a 192 kHz engine (and the reverse) is reachable — the largest and smallest resampling ratios
/// the product can be asked for.
const ENGINE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// Block sizes spanning what a host may present, smallest and largest first: the schedule's first
/// partition is the block size, so this is the axis `build_schedule` varies over.
const BLOCK_SIZES: [usize; 5] = [16, 64, 512, 1024, 4096];

fuzz_target!(|data: &[u8]| {
    let Some((&rate_selector, rest)) = data.split_first() else {
        return;
    };
    let Some((&block_selector, bytes)) = rest.split_first() else {
        return;
    };

    let hz = ENGINE_RATES[rate_selector as usize % ENGINE_RATES.len()];
    let block_size = BLOCK_SIZES[block_selector as usize % BLOCK_SIZES.len()];
    let Some(engine_rate) = SampleRate::new(hz) else {
        return;
    };

    let _ = PreparedIr::from_wav_bytes(bytes, engine_rate, block_size);
});
