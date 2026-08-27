//! `cargo fuzz` target for `namir_ir::probe_wav` (NFR-QUAL-040: "the [...] readers shall not
//! panic, hang, over-allocate or read out of bounds on any input" -- P6's "one hardened place per
//! format, and that place is fuzzed"). `probe_wav` is the header-only entry point
//! (`namir-ir/src/wav.rs`'s own doc comment) that both `namir-library`'s scanner and this fuzz
//! target exercise; `decode`/`PreparedIr::from_wav_bytes` apply the identical header validation
//! and so inherit whatever this target hardens about that validation -- but only about it. They
//! then do strictly more on top (sample-data decoding, the `MAX_LOAD_SECONDS` clamp, resampling
//! and FFT planning), none of which this target reaches. An earlier version of this comment
//! claimed hardening `probe_wav` "hardens both"; `namir-ir/src/wav.rs`'s own doc comment says the
//! opposite in as many words -- `probe_wav` is *deliberately shallower* -- and the M9a sweep
//! records the residue in the `uncovered:` field on the tag below.
//!
//! The bar is exactly NFR-QUAL-040's, not "always reject garbage": `Ok` and `Err` are both
//! acceptable outcomes for arbitrary bytes. Landing this at M7 rather than earlier is a real gap
//! this milestone closes, not a deferral by design the way M1's `.nam`-only start was (see
//! `docs/03-implementation-roadmap.md` §11) -- `namir-nam`/`namir-state` already had this coverage
//! since M1/M5.
// trace: NFR-SEC-010
#![no_main]

use libfuzzer_sys::fuzz_target;

// trace-partial: NFR-QUAL-040
// uncovered: NFR-QUAL-040 — of the parsers the method enumerates, the audio file reader is fuzzed
// uncovered: header-only: the target calls probe_wav, which the crate documents as deliberately
// uncovered: shallower, so decode's per-channel Vec::with_capacity, its MAX_LOAD_SECONDS clamp
// uncovered: and PreparedIr::from_wav_bytes's resample and FFT planning — where a hang or an
// uncovered: over-allocation would actually live — are never reached; closes M8
fuzz_target!(|data: &[u8]| {
    let _ = namir_ir::probe_wav(data);
});
