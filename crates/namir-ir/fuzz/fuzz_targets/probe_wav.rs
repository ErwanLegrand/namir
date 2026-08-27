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

// M14 added `load_ir.rs` beside this file, which reaches everything past the header parse —
// `decode`'s allocation, the MAX_LOAD_SECONDS clamp, resampling and FFT planning — so the
// *artifact* half of the gap this tag recorded is closed. What is left is the method's other word:
// "continuously", which is a CI step this workstream could not add.
// trace-partial: NFR-QUAL-040
// uncovered: NFR-QUAL-040 — the method's "fuzz targets run in CI" is executed for three of the
// uncovered: four targets: .github/workflows/fuzz.yml has a job per load_nam, read_state and
// uncovered: probe_wav, and M14's load_ir — the only target that reaches decode, the
// uncovered: MAX_LOAD_SECONDS clamp, resampling and FFT planning — has none, so the deep audio
// uncovered: reader is fuzzable but not fuzzed continuously; closes M8
fuzz_target!(|data: &[u8]| {
    let _ = namir_ir::probe_wav(data);
});
