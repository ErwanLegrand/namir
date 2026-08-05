//! D-5.1's role for this crate: "Filesystem locations, config dirs, logging sink, thread
//! priority. The only crate with `#[cfg(target_os)]`." This crate is currently only the minimal
//! slice `03-implementation-roadmap.md` §5 (M1) asks for — D-7.4's denormal-suppression guard.
//! Everything else D-5.1 assigns here (filesystem paths, CLAP install paths, thread priority)
//! waits for M6, when something actually consumes it; adding it speculatively now would be
//! untested, unconsumed surface area.
//!
//! This guard needs `#[cfg(target_arch)]`, not `#[cfg(target_os)]` — MXCSR/FPCR access differs by
//! CPU architecture, not by operating system, so it doesn't exercise the `target_os` carve-out
//! D-5.1's table names this crate for. That carve-out is still reserved for this crate alone; it
//! simply has no user yet.

mod denormal;

pub use denormal::DenormalGuard;
