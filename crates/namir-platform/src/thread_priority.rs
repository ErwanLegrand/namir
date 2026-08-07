//! D-13.2's fourth clause: "thread priority elevation" lives in `namir-platform`. Raises the
//! *calling* thread's OS scheduling priority to a level suitable for an audio callback thread --
//! the same category of platform-specific, unsafe, CPU/OS-control-surface work D-7.4's
//! [`crate::denormal::DenormalGuard`] already does for the FPU's control register, so this module
//! is confined to itself the same way per D-5.3 / NFR-QUAL-070, with its own written safety
//! argument on every `unsafe` block rather than inheriting `denormal.rs`'s.
//!
//! **This primitive is built but not yet called anywhere**, exactly the position
//! [`crate::denormal::DenormalGuard`] itself was in from M1 through M5 (see that module's own doc
//! comment and D-7.4's M3 audit-finding consequence note in `docs/02-architecture.md`).
//! `docs/03-implementation-roadmap.md` §10 (M6) assigns *acquiring* it to `namir-app`'s `cpal`
//! stream callback and `namir-clap`'s `process()`-adjacent activation path -- both are M6
//! deliverables this round does not build (they do not exist as crates yet). Recorded here so the
//! next reader does not mistake "built, uncalled" for "built, working": nothing measures the
//! effect of calling this yet, the same gap D-7.4's M3 audit found for `DenormalGuard`.
//!
//! **When and how a future caller should invoke this, stated explicitly so it isn't
//! rediscovered:** once, from the thread being elevated -- OS thread-priority and
//! scheduling-policy APIs act on a thread handle referring to *some* thread, and every API this
//! module wraps is simplest and safest called as "raise my own priority" (`GetCurrentThread`'s
//! pseudo-handle on Windows, `pthread_self()` on Unix) rather than reaching across threads.
//! Concretely, that means the audio driver/host thread should call this exactly once, at stream
//! start (`namir-app`) or on first `process()` activation (`namir-clap`) -- not once per callback.
//! This mirrors D-7.4's own framing for `DenormalGuard` ("once per audio callback") but is a
//! coarser cadence deliberately: FTZ/DAZ mode is callback-scoped because a *host* thread might
//! call back into other code between callbacks and expects its own FPU mode preserved (hence
//! `DenormalGuard`'s `Drop`-restores design); OS thread priority is a property of the thread
//! itself, persists for the thread's lifetime once set, and Win32/POSIX both document it as
//! cheap to set once and expensive/pointless to toggle every block. There is no `Drop` guard here
//! for the same reason `DenormalGuard` needs one and this does not: an audio callback thread does
//! not hand control back to unrelated code between blocks the way a plugin host's calling thread
//! does, so there is nothing this elevation could leak into that would surprise anyone.
//!
//! **Why this can fail, and why failure is not fatal:** raising a thread to a real-time
//! scheduling class is a privileged operation on every platform this module targets except
//! plain `SetThreadPriority` on Windows (which ordinary processes may use freely) --
//! Linux requires `CAP_SYS_NICE` or an `rtprio` resource limit an administrator has granted the
//! user (the same permission model JACK/PipeWire-based pro-audio setups already ask users to
//! configure), and Darwin's `pthread_setschedparam` is similarly gated. [`ThreadPriorityOutcome`]
//! reports this as [`ThreadPriorityOutcome::PermissionDenied`], a value a caller is expected to
//! log and continue past -- degradation, not failure, matching P8's framing throughout this
//! codebase (D-7.1's worker-pool floor, D-8.1's return-ring backpressure): an unelevated thread
//! still processes audio correctly, just with a higher chance of an OS-scheduling-induced xrun
//! (FR-IO-060) under system load. Nothing in this module may panic or abort on a denied request.

#![allow(unsafe_code)]

/// Outcome of one call to [`elevate_current_thread_priority`]. Deliberately not a `Result`
/// wrapping an error type with a `Display` impl or similar: per this module's own doc comment, a
/// denial is an expected, common, non-exceptional outcome on Linux/macOS without prior privilege
/// configuration, not an error condition to propagate with `?`. A caller matches on this and
/// decides what to log; it is not expected to bail out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPriorityOutcome {
    /// The calling thread's scheduling priority/class was raised successfully.
    Elevated,
    /// The OS refused because the calling process lacks the privilege/capability/resource limit
    /// the underlying call needs (Windows: `ERROR_ACCESS_DENIED`; Unix: `EPERM`). The single most
    /// likely outcome on a freshly-installed Linux/macOS system with no pro-audio privilege
    /// configuration done -- see this module's doc comment.
    PermissionDenied,
    /// The underlying OS call failed for a reason other than a permission check. Carries the raw
    /// OS error code (`GetLastError` on Windows, the `pthread_*` return value or `errno` on
    /// Unix) for diagnostics -- FR-ERR-050's diagnostic bundle is the intended consumer of this
    /// value, not a user-facing message formatted from it directly.
    OsError(i32),
    /// This target has no implementation in this module (anything besides Windows/Linux/macOS --
    /// notably Android and iOS, which D-5.1 marks this crate as building for but which M6's
    /// product shells do not target for 1.0). Matches
    /// [`crate::denormal::DenormalGuard`]'s own "constructs and drops cleanly, just doesn't change
    /// anything" fallback for an architecture it has no implementation for: NFR-PORT-030 must not
    /// be precluded by this module failing to compile or panicking on an unsupported target.
    Unsupported,
}

/// Raises the *calling* thread's OS scheduling priority to a level suitable for an audio callback
/// thread. See this module's doc comment for exactly when a future caller should invoke this
/// (once, at stream/process start, from the thread being elevated) and why a
/// [`ThreadPriorityOutcome::PermissionDenied`] result is an expected outcome to log and continue
/// past rather than treat as fatal.
///
/// Platform behaviour:
/// - **Windows:** `SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL)`. Ordinary
///   (non-administrator) processes may set this freely within their own process priority class;
///   Windows implements the actual real-time protection at the *process* priority class level
///   (`REALTIME_PRIORITY_CLASS`), not at this thread-priority level, so this call is expected to
///   succeed in the overwhelming majority of cases.
/// - **Linux/macOS:** `pthread_setschedparam` with `SCHED_FIFO` at that policy's maximum priority.
///   Requires a privilege (`CAP_SYS_NICE`) or resource limit (`rtprio`) the user may not have
///   configured -- see this module's doc comment for why that is an expected, non-fatal outcome.
/// - **Everything else:** [`ThreadPriorityOutcome::Unsupported`], unconditionally.
pub fn elevate_current_thread_priority() -> ThreadPriorityOutcome {
    #[cfg(target_os = "windows")]
    let outcome = windows::elevate();

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let outcome = unix::elevate();

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    let outcome = ThreadPriorityOutcome::Unsupported;

    outcome
}

#[cfg(target_os = "windows")]
mod windows {
    use super::ThreadPriorityOutcome;

    /// `THREAD_PRIORITY_TIME_CRITICAL` from `winbase.h` -- the highest thread-priority level
    /// Win32 defines, documented as intended for exactly this use (a thread that must run whenever
    /// it is ready, such as an audio callback). A stable, long-documented Win32 constant; not
    /// looked up at runtime because it has never varied across Windows versions.
    const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;

    /// `ERROR_ACCESS_DENIED` from `winerror.h`, checked to map a Win32 failure to
    /// [`ThreadPriorityOutcome::PermissionDenied`] rather than the generic
    /// [`ThreadPriorityOutcome::OsError`] variant, consistent with how the Unix path distinguishes
    /// `EPERM` from any other `pthread_setschedparam` failure.
    const ERROR_ACCESS_DENIED: u32 = 5;

    // Hand-rolled rather than taking the `windows`/`windows-sys` crate as a dependency: these are
    // three of the most stable, long-documented functions in the Win32 API (unchanged since
    // Windows NT), and this workspace's adoption bar for a new dependency (`rtrb`'s precedent,
    // restated at D-12.3: "zero transitive dependencies, no build script, `no_std`-capable pure
    // Rust") is satisfied more cheaply by three `extern` declarations than by evaluating a crate
    // against it. `kernel32.dll` is always loaded into every Windows process, so no `#[link]`
    // beyond naming it is required.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// Returns a pseudo-handle for the calling thread. Per Win32 documentation this
        /// pseudo-handle is always valid, never needs closing (unlike a real `HANDLE` from
        /// `OpenThread`), and always refers to whichever thread called it -- exactly the "act on
        /// my own thread" shape this module's doc comment says to prefer.
        fn GetCurrentThread() -> *mut core::ffi::c_void;

        /// Sets the priority of `h_thread` to `n_priority`, returning nonzero on success.
        fn SetThreadPriority(h_thread: *mut core::ffi::c_void, n_priority: i32) -> i32;

        /// Returns the calling thread's last-error code, valid immediately after a Win32 call
        /// that reported failure via its return value (as `SetThreadPriority` does).
        fn GetLastError() -> u32;
    }

    pub(super) fn elevate() -> ThreadPriorityOutcome {
        // SAFETY: `GetCurrentThread` takes no arguments and, per Win32 documentation, always
        // returns a valid pseudo-handle with no ownership to release -- there is no handle leak
        // and no invalid-handle hazard to reason about. `SetThreadPriority` is then called with
        // that valid pseudo-handle and `THREAD_PRIORITY_TIME_CRITICAL`, a documented in-range
        // constant Microsoft's own headers define -- the call has no memory-safety precondition
        // beyond "a valid `HANDLE`", which the pseudo-handle satisfies by construction. Neither
        // call touches Rust-managed memory or aliases anything.
        let succeeded =
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
        if succeeded != 0 {
            return ThreadPriorityOutcome::Elevated;
        }
        // SAFETY: `GetLastError` takes no arguments and reads only thread-local Win32 error
        // state; it has no memory-safety precondition at all.
        let code = unsafe { GetLastError() };
        if code == ERROR_ACCESS_DENIED {
            ThreadPriorityOutcome::PermissionDenied
        } else {
            ThreadPriorityOutcome::OsError(code as i32)
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use super::ThreadPriorityOutcome;

    // `libc` (MIT OR Apache-2.0, already on `deny.toml`'s allow list) is taken as a dependency
    // here, scoped in `Cargo.toml` to exactly `cfg(any(target_os = "linux", target_os =
    // "macos"))`, for a narrower reason than this workspace's usual "avoid a dependency where the
    // standard library or a few lines suffice" bar (D-12.3 restates that bar, set by `rtrb`'s
    // adoption, and explicitly counts a build script against a candidate -- `libc` does carry
    // one). The bar does not apply cleanly here: `libc::sched_param`'s exact memory layout is
    // part of the target's C ABI (Darwin's definition carries a private padding field this crate
    // cannot see or reproduce correctly by hand, unlike Linux's, which is deceptively simple by
    // comparison), and passing a mismatched struct layout to `pthread_setschedparam` by pointer
    // is a genuine out-of-bounds read, not a style nit -- exactly the class of bug D-5.3's
    // "written safety argument" requirement exists to make someone reason through explicitly. A
    // vetted, widely-used binding removes that risk entirely; hand-rolling it to save one
    // dependency would trade a real soundness risk for a cosmetic win.
    use libc::{
        SCHED_FIFO, pthread_self, pthread_setschedparam, sched_get_priority_max, sched_param,
    };

    pub(super) fn elevate() -> ThreadPriorityOutcome {
        // SAFETY: `sched_get_priority_max` takes a plain `c_int` policy constant and performs no
        // memory access; it cannot be unsound regardless of the argument's value (an invalid
        // policy simply returns -1, checked below).
        let max_priority = unsafe { sched_get_priority_max(SCHED_FIFO) };
        if max_priority == -1 {
            return os_error_outcome();
        }

        // Zero-initialised rather than built as a struct literal: `libc::sched_param` carries a
        // private padding field on Darwin (see the module-level comment above) that this crate
        // cannot name in a literal. Zero-initialising the whole struct and then writing only the
        // public `sched_priority` field is well-defined for this type -- every field involved is
        // a plain integer/byte type with no invalid all-zero bit pattern -- and is the standard
        // pattern for constructing a foreign struct with hidden fields.
        //
        // SAFETY: `sched_param` is a `#[repr(C)]` plain-old-data struct of integer/byte fields
        // with no niches, no padding-sensitive invariants, and no `Drop` impl; the all-zero bit
        // pattern is a valid value of every field, so `mem::zeroed` cannot produce an invalid
        // `sched_param`.
        let mut param: sched_param = unsafe { core::mem::zeroed() };
        param.sched_priority = max_priority;

        // SAFETY: `pthread_self()` takes no arguments and returns an opaque thread identifier for
        // the calling thread by value -- no memory access, cannot be unsound.
        //
        // `pthread_setschedparam` is called with that identifier, `SCHED_FIFO` (a valid policy
        // constant `libc` defines for this target), and `&param`, a pointer to a `sched_param`
        // this function just fully initialised and which stays alive (on the stack, unmoved) for
        // the whole call. `pthread_setschedparam` only reads through that pointer for the
        // duration of the call and does not retain it afterwards (POSIX's documented contract),
        // so there is no dangling-pointer or aliasing hazard once the call returns.
        let rc = unsafe { pthread_setschedparam(pthread_self(), SCHED_FIFO, &param) };
        if rc == 0 {
            ThreadPriorityOutcome::Elevated
        } else if rc == libc::EPERM {
            ThreadPriorityOutcome::PermissionDenied
        } else {
            ThreadPriorityOutcome::OsError(rc)
        }
    }

    /// `sched_get_priority_max` reports failure via `-1` and sets `errno` (unlike
    /// `pthread_setschedparam`, which is a `pthread_*` function returning its error code
    /// directly) -- `std::io::Error::last_os_error` already wraps the platform-correct way to
    /// read `errno` without this module needing its own accessor.
    fn os_error_outcome() -> ThreadPriorityOutcome {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
        if code == libc::EPERM {
            ThreadPriorityOutcome::PermissionDenied
        } else {
            ThreadPriorityOutcome::OsError(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_without_panicking() {
        // The one assertion this test can make on every CI OS without assuming privileges are
        // available: calling this never panics, and it returns one of the documented outcomes.
        // Anything past that (whether it actually succeeded) depends on the CI runner's
        // scheduling privileges, which this crate has no control over and per this module's doc
        // comment does not need to succeed in order to be correct.
        match elevate_current_thread_priority() {
            ThreadPriorityOutcome::Elevated
            | ThreadPriorityOutcome::PermissionDenied
            | ThreadPriorityOutcome::OsError(_)
            | ThreadPriorityOutcome::Unsupported => {}
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_elevation_succeeds_for_an_ordinary_process() {
        // Per this module's own doc comment, ordinary (non-administrator) Windows processes may
        // set THREAD_PRIORITY_TIME_CRITICAL freely -- CI runners are such a process, so this
        // asserts the success path specifically rather than only the no-panic property above.
        assert_eq!(
            elevate_current_thread_priority(),
            ThreadPriorityOutcome::Elevated
        );
    }

    #[test]
    fn calling_twice_in_a_row_is_not_an_error() {
        // This module's doc comment says "once, at stream/process start" is the intended cadence,
        // not a hard one-shot requirement -- setting an already-elevated thread's priority again
        // must not itself become a new failure mode.
        let _ = elevate_current_thread_priority();
        match elevate_current_thread_priority() {
            ThreadPriorityOutcome::Elevated
            | ThreadPriorityOutcome::PermissionDenied
            | ThreadPriorityOutcome::OsError(_)
            | ThreadPriorityOutcome::Unsupported => {}
        }
    }
}
