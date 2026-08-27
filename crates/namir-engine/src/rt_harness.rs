//! D-7.5: "a test harness that installs a global allocator which panics on allocation while an
//! 'audio section' marker is active, and every engine test runs inside that marker." Test-only
//! (this module is `#[cfg(test)]` in `lib.rs`) per D-7.5's own consequence: a `#[global_allocator]`
//! can only be installed once per binary, so this must not exist outside the test build.
//!
//! `unsafe_code = "forbid"` (D-5.3) applies to this crate, and `GlobalAlloc`'s methods are
//! `unsafe fn` — there is no way to implement that trait inside this crate at all under
//! `forbid`, let alone locally relax it (a `forbid`-level lint cannot be downgraded by a nested
//! `#[allow]`; that's the difference between `forbid` and `deny`). So this composes the
//! `assert_no_alloc` crate instead: its `unsafe impl GlobalAlloc` lives in *that* crate, so
//! installing it here (a safe `static` declaration plus an attribute) needs no `unsafe` of ours.
//!
//! Its default behaviour on a violation is to abort the process
//! (`std::alloc::handle_alloc_error`), which `#[should_panic]` cannot observe — an abort kills
//! the whole test binary, not just the offending test. The `warn_debug`/`warn_release` features
//! (both enabled in Cargo.toml, dev-dependency only) switch it to counting violations instead of
//! aborting -- one per profile, since each is gated on its own `debug_assertions` state, so both
//! are needed for `cargo test` and `cargo test --release` alike;
//! `audio_section` below turns that count back into an actual `panic!`, which is the observable
//! "panics on allocation" behaviour D-7.5 asks for.

use assert_no_alloc::AllocDisabler;

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

/// Runs `f` inside the "audio section" marker. Every engine test that calls into
/// `Stage::process` goes through this.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepare::PrepareContext;
    use crate::stage::{Stage, StagePrep};
    use crate::stage_io::StageIo;
    use crate::test_support::{AllocatingStage, FixedGainPrep};
    use namir_core::{ChannelConfig, SampleRate};

    fn ctx() -> PrepareContext {
        PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, ChannelConfig::Mono).unwrap()
    }

    /// (a): a deliberately-allocating fake `Stage` trips the harness.
    ///
    /// This is FR-ERR-030's **allocation** limb, and it carried that requirement's only annotation
    /// until M9b. It no longer does: the tag moved to `xtask/src/main.rs`'s
    /// `the_real_tree_names_no_logger_in_any_audio_thread_module`, beside the `xtask rt-logging`
    /// check M9b built, because the gap the old `uncovered:` field named — "no static check for
    /// logging calls exists in xtask or anywhere else" — is what that check closes, and the
    /// requirement's `Verify:` code is **S**. A non-allocating log call (a below-threshold level
    /// check returns without touching the sink) still passes this harness clean, which is exactly
    /// why the static check had to exist separately rather than be claimed here.
    #[test]
    #[should_panic(expected = "allocation occurred inside an audio section")]
    fn harness_catches_a_real_allocation() {
        let mut stage = AllocatingStage;
        let mut buf = [0.0f32; 4];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| stage.process(&mut io));
    }

    /// (b): a real, non-allocating `Stage` passes clean inside the harness.
    #[test]
    fn a_real_non_allocating_stage_passes_clean() {
        let prep = FixedGainPrep { gain_db: -6.0 };
        let mut stage = prep.prepare(&ctx()).unwrap();
        let mut buf = [1.0f32; 4];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| stage.process(&mut io));
        let expected = namir_core::db_to_linear(-6.0);
        for s in io.channel(0) {
            assert!((*s - expected).abs() < 1e-4);
        }
    }
}
