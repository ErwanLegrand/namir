//! Local error catalogue for `namir-platform`, following the same pattern `namir-nam`,
//! `namir-ir` and `namir-worker` use (D-16.1: `ErrorCode` is a shared *type*, not a closed enum,
//! so each crate owns its own consts rather than pushing them up into `namir-core`).
//!
//! This crate had no catalogue at all until M9b: `paths.rs`, `clap_paths.rs` and
//! `denormal.rs` all report by returning `Option`/an outcome enum rather than by raising a
//! catalogued error, and `thread_priority.rs` does the same. The three entries below exist
//! because D-16.5 makes **every** log record catalogue-backed — `<code-id>` is a mandatory field
//! of the record format — so the log writer's own lifecycle events (a session opening, a rotation
//! happening, an unparseable `NAMIR_LOG`) need catalogue ids like everything else, rather than a
//! second, id-less record shape.

use namir_core::{ErrorCode, Severity};

/// The diagnostic log opened and the session's first record was written (D-16.5). Carries the
/// resolved verbosity level and the sink path, so a log a user sends in says which level produced
/// it without anyone having to ask.
pub const LOG_SESSION_STARTED: ErrorCode = ErrorCode::new(
    "platform.log.session_started",
    Severity::Info,
    "Diagnostic logging started at level {level}.",
);

/// The log reached [`crate::logging::LOG_MAX_BYTES`] and the generations were rotated (D-16.5).
/// Written as the first record of the *new* `namir.log`, so the seam between two generations is
/// visible from either side.
pub const LOG_ROTATED: ErrorCode = ErrorCode::new(
    "platform.log.rotated",
    Severity::Info,
    "The diagnostic log reached {bytes} bytes and was rotated.",
);

/// `NAMIR_LOG` was set to something the level parser does not recognise (D-16.5). The level falls
/// back exactly as if the variable were unset — never silently off — and this record names the
/// rejected value so the user who mistyped it can see that they did.
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
    "NAMIR_LOG was set to {value}, which is not a known verbosity level.",
);

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-ERR-020: every user-visible error maps to exactly one catalogue entry, verified
    /// statically. Same check every other crate's catalogue carries.
    #[test]
    fn catalogue_ids_are_unique() {
        let all = [LOG_SESSION_STARTED, LOG_ROTATED, LOG_BAD_LEVEL];
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
