//! `cargo fuzz` target for `namir_nam::load` (NFR-QUAL-040, docs/03-implementation-roadmap.md §5
//! M1 quick win). The bar this holds `load` to is exactly NFR-QUAL-040's: "shall not panic, hang,
//! over-allocate or read out of bounds on any input" — not "always reject garbage". `Ok` and `Err`
//! are both acceptable outcomes for arbitrary bytes; rejecting malformed input cleanly is
//! `namir_nam`'s own job via its `NamLoadError` catalogue, already exercised by
//! `crates/namir-nam/tests/fixtures.rs`'s `rejects_mutated_variants_without_panicking`. This
//! target's job is continuous exploration of the input space that test can't reach by itself.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = namir_nam::load(data);
});
