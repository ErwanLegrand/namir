//! wasm32 ABI over [`crate::harness::Harness`]. Empty placeholder: Task 3 builds this out.
//!
//! Exists now, rather than being added in Task 3, only because `lib.rs` already declares
//! `#[cfg(target_arch = "wasm32")] mod wasm_abi;` (per the Task 2 brief) and Task 2 must itself
//! verify a clean `wasm32-unknown-unknown` lib build -- a missing module file would fail that
//! build for a reason entirely internal to this spike crate.
