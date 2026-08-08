//! `cargo fuzz` target for `namir_ir::probe_wav` (NFR-QUAL-040: "the [...] readers shall not
//! panic, hang, over-allocate or read out of bounds on any input" -- P6's "one hardened place per
//! format, and that place is fuzzed"). `probe_wav` is the header-only entry point
//! (`namir-ir/src/wav.rs`'s own doc comment) that both `namir-library`'s scanner and this fuzz
//! target exercise; `decode`/`PreparedIr::from_wav_bytes` apply the identical header validation
//! plus sample-data decoding on top, so hardening `probe_wav`'s parse path hardens both.
//!
//! The bar is exactly NFR-QUAL-040's, not "always reject garbage": `Ok` and `Err` are both
//! acceptable outcomes for arbitrary bytes. Landing this at M7 rather than earlier is a real gap
//! this milestone closes, not a deferral by design the way M1's `.nam`-only start was (see
//! `docs/03-implementation-roadmap.md` §11) -- `namir-nam`/`namir-state` already had this coverage
//! since M1/M5.
// trace: NFR-QUAL-040, NFR-SEC-010
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = namir_ir::probe_wav(data);
});
