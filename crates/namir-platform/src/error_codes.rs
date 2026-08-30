//! Local error catalogue for `namir-platform`, following the same pattern `namir-nam`,
//! `namir-ir` and `namir-worker` use (D-16.1: `ErrorCode` is a shared *type*, not a closed enum,
//! so each crate owns its own consts rather than pushing them up into `namir-core`).
//!
//! This crate had no catalogue at all until M9b: `paths.rs`, `clap_paths.rs` and
//! `denormal.rs` all report by returning `Option`/an outcome enum rather than by raising a
//! catalogued error, and `thread_priority.rs` did the same. The first three entries below exist
//! because D-16.5 makes **every** log record catalogue-backed — `<code-id>` is a mandatory field
//! of the record format — so the log writer's own lifecycle events (a session opening, a rotation
//! happening, an unparseable `NAMIR_LOG`) need catalogue ids like everything else, rather than a
//! second, id-less record shape.
//!
//! The last two entries are `thread_priority.rs`'s, added when
//! [`crate::ThreadPriorityOutcome`] stopped being a value its only caller discarded: the outcome
//! enum is still the return type (a denial is not an error to propagate with `?`), and these are
//! what [`crate::ThreadPriorityOutcome::diagnostic`] hands a caller to *record*. Note where that
//! record may be written: both of today's callers invoke the elevation from inside an audio
//! callback, and FR-ERR-030 forbids logging there, so the outcome has to be carried off the
//! audio thread before it becomes a record -- see that method's own doc comment.

use namir_core::{ErrorCode, Severity};

/// The diagnostic log opened and the session's first record was written (D-16.5). Carries the
/// resolved verbosity level and the sink path, so a log a user sends in says which level produced
/// it without anyone having to ask.
pub const LOG_SESSION_STARTED: ErrorCode = ErrorCode::new(
    "platform.log.session_started",
    Severity::Info,
    "Diagnostic logging started ({detail}).",
    "Nothing to do; this record only marks where a session's log begins. Set NAMIR_LOG to `off`, \
     `error`, `info` or `verbose` to change how much follows it.",
);

/// The log reached [`crate::logging::LOG_MAX_BYTES`] and the generations were rotated (D-16.5).
/// Written as the first record of the *new* `namir.log`, so the seam between two generations is
/// visible from either side.
pub const LOG_ROTATED: ErrorCode = ErrorCode::new(
    "platform.log.rotated",
    Severity::Info,
    "The diagnostic log reached its size limit and was rotated ({detail}).",
    "Nothing to do; the previous generation is kept beside this file. Copy both if you are \
     attaching a log to a bug report.",
);

/// `NAMIR_LOG` was set to something the level parser does not recognise (D-16.5). The level falls
/// back exactly as if the variable were unset — never silently off — and this record names the
/// rejected value so the user who mistyped it can see that they did.
///
/// **Admitted regardless of the resolved level, `off` excepted** (issue #79). [`Severity::Warning`]
/// is below what [`crate::logging::LogLevel::Error`] admits, so routing this through the ordinary
/// `record` path discarded it for exactly the user who had already chosen a quiet log *and*
/// mistyped the variable — leaving the record's own promise above unkept. [`crate::logging::Logger::new`]
/// therefore writes it directly. The single exception is [`crate::logging::LogLevel::Off`], whose
/// contract is that the file is never opened or created at all; forcing a record past that would
/// create a log the user switched off.
///
/// **Severity divergence, recorded rather than glossed.** D-16.5's parameter prose calls these
/// three "`Severity::Info` consts" in one sentence and then calls this one's record a "`WARN
/// platform.log.bad_level` record" two paragraphs later; the two statements cannot both hold,
/// because the same decision makes `LEVEL` *derived* from the code's severity precisely so that
/// "the level and the catalogue severity are one fact rather than two that can disagree". This
/// const resolves the contradiction in favour of the derived-`LEVEL` invariant and the worked
/// spelling: [`Severity::Warning`], rendering `WARN`. Choosing `Info` instead would have printed
/// `INFO platform.log.bad_level`, contradicting the decision's own quoted line for a record whose
/// entire purpose is to be noticed.
pub const LOG_BAD_LEVEL: ErrorCode = ErrorCode::new(
    "platform.log.bad_level",
    Severity::Warning,
    "NAMIR_LOG was set to a value that is not a known verbosity level ({detail}).",
    "Set NAMIR_LOG to one of `off`, `error`, `info` or `verbose`, or unset it. Logging is running \
     at its normal level in the meantime, not switched off.",
);

/// The audio thread asked for a real-time scheduling priority and the OS refused for want of a
/// privilege, capability or resource limit (`EPERM`, or Win32 `ERROR_ACCESS_DENIED`) --
/// [`crate::ThreadPriorityOutcome::PermissionDenied`].
///
/// The single most likely outcome on a Linux or macOS system with no pro-audio privilege
/// configuration done, and the one non-`Elevated` outcome with a remedy the user can act on,
/// which is why it is its own entry rather than sharing [`THREAD_PRIORITY_NOT_ELEVATED`]. Audio
/// still processes correctly at ordinary priority; what degrades is the xrun rate under load
/// (FR-IO-060), and a user reporting that deserves to find this line in the log rather than
/// guess.
pub const THREAD_PRIORITY_DENIED: ErrorCode = ErrorCode::new(
    "platform.thread_priority.denied",
    Severity::Warning,
    "The audio thread could not be given real-time scheduling priority ({detail}).",
    "Grant this user a real-time scheduling allowance -- on Linux, an `rtprio` limit (the \
     `audio` group and `/etc/security/limits.d` on most distributions, the same configuration \
     JACK and PipeWire ask for) or `CAP_SYS_NICE`. Audio still runs without it, with a higher \
     chance of dropouts under system load.",
);

/// The audio thread did not get a real-time scheduling priority for a reason the user cannot act
/// on: the OS call failed for something other than a permission check
/// ([`crate::ThreadPriorityOutcome::OsError`]), or this target has no implementation
/// ([`crate::ThreadPriorityOutcome::Unsupported`]).
///
/// Split from [`THREAD_PRIORITY_DENIED`] on remedy, not on severity: both are `Warning` and both
/// degrade the same way, but this one has nothing to tell the user to do, and a remedy that does
/// not apply is worse than none. The `{detail}` carries the raw OS code or the target name, which
/// is what FR-ERR-050's diagnostic bundle wants.
pub const THREAD_PRIORITY_NOT_ELEVATED: ErrorCode = ErrorCode::new(
    "platform.thread_priority.not_elevated",
    Severity::Warning,
    "The audio thread is running at ordinary scheduling priority ({detail}).",
    "Nothing to configure; this is not a permission problem. If you are hearing dropouts, raise \
     the audio buffer size. Include this line if you report the problem.",
);

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-ERR-020: every user-visible error maps to exactly one catalogue entry, verified
    /// statically. Same check every other crate's catalogue carries.
    #[test]
    fn catalogue_ids_are_unique() {
        let all = [
            LOG_SESSION_STARTED,
            LOG_ROTATED,
            LOG_BAD_LEVEL,
            THREAD_PRIORITY_DENIED,
            THREAD_PRIORITY_NOT_ELEVATED,
        ];
        namir_core::assert_unique_ids(&all);
        for code in all {
            assert!(
                code.id.starts_with("platform."),
                "{} is not namespaced",
                code.id
            );
        }
    }
}
