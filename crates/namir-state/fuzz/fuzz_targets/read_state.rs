//! `cargo fuzz` target for `namir_state::State::read` (NFR-QUAL-040: "the preset and state
//! readers shall not panic, hang, over-allocate or read out of bounds on any input"). D-11.1
//! chose JSON specifically so there would be one parser to harden across every consumer of this
//! format (P6); this is the second one this project's fuzz machinery points at, after
//! `namir-nam/fuzz`'s `load_nam` — landing this in M5 rather than deferring it to M7 is what
//! actually makes good on that reason for the choice.
//!
//! The bar is exactly NFR-QUAL-040's, not "always reject garbage": `Ok` and `Err` are both
//! acceptable outcomes for arbitrary bytes. `State::read` exercises the whole read pipeline in
//! one call — `Document::parse`'s NFR-SEC-020 byte/recursion ceilings, `migrate`'s
//! `format_version` gate, every section's D-11.2 tolerant projection (`ParamValues`, `Global`,
//! `FileRef`/`EmbeddedRef` including base64 decoding) — so a single entry point covers the same
//! surface `crates/namir-state/tests/corpus.rs`'s targeted tests already prove correct on known
//! inputs; this target's job is continuous exploration of the input space those tests can't reach
//! by construction.
// trace: NFR-QUAL-040
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = namir_state::State::read(data);
});
