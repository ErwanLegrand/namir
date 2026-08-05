//! D-7.5's RT-allocation test harness, mirrored from `namir-engine/src/rt_harness.rs`. Test-only
//! (this module is `#[cfg(test)]` in `lib.rs`) because a `#[global_allocator]` can only be
//! installed once per binary.
//!
//! `unsafe_code = "forbid"` (D-5.3) applies to this crate, and `GlobalAlloc`'s methods are
//! `unsafe fn` — there is no way to implement that trait inside this crate at all under `forbid`,
//! let alone locally relax it (a `forbid`-level lint cannot be downgraded by a nested `#[allow]`;
//! that's the difference between `forbid` and `deny`). So this composes the `assert_no_alloc`
//! crate instead: its `unsafe impl GlobalAlloc` lives in *that* crate, so installing it here (a
//! safe `static` declaration plus an attribute) needs no `unsafe` of ours.
//!
//! Its default behaviour on a violation is to abort the process
//! (`std::alloc::handle_alloc_error`), which `#[should_panic]` cannot observe — an abort kills
//! the whole test binary, not just the offending test. The `warn_debug` feature (enabled in
//! Cargo.toml, dev-dependency only) switches it to counting violations instead of aborting;
//! `audio_section` below turns that count back into an actual `panic!`, which is the observable
//! "panics on allocation" behaviour D-7.5 asks for.
//!
//! Declared once for the whole crate; reused by `biquad`, `gate`, `meter` and `dc_blocker`'s own
//! tests.

use assert_no_alloc::AllocDisabler;

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

/// Runs `f` inside the "audio section" marker. Every RT-safety test in this crate goes through
/// this.
pub fn audio_section<T>(f: impl FnOnce() -> T) -> T {
    assert_no_alloc::reset_violation_count();
    let result = assert_no_alloc::assert_no_alloc(f);
    assert_eq!(
        assert_no_alloc::violation_count(),
        0,
        "D-7.5: allocation occurred inside an audio section"
    );
    result
}
