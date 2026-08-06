//! Local error catalogue for `namir-worker`, following the same pattern `namir-nam` and
//! `namir-ir` use (D-16.1: `ErrorCode` is a shared *type*, not a closed enum, so each crate owns
//! its own consts rather than pushing them up into `namir-core`).
//!
//! These are the failure modes that belong to *orchestration* rather than to parsing. A `.nam`
//! file that fails to parse already has a precise `nam.load.*` id, and [`WorkerError`]'s `From`
//! impls pass it through **unchanged** — re-wrapping it into a `worker.*` id would erase exactly
//! the specific reason FR-ERR-020 exists to preserve.

use namir_core::{ErrorCode, Severity};

/// A worker job panicked and was contained at the job boundary (D-16.3/FR-ERR-040). The pool
/// keeps serving; this job's result is lost.
pub const JOB_PANICKED: ErrorCode = ErrorCode {
    id: "worker.job.panicked",
    severity: Severity::Fault,
    message_template: "An internal error occurred while loading {path}.",
};

/// The file could not be read at all — missing, permissions, or an I/O error partway through.
pub const FILE_UNREADABLE: ErrorCode = ErrorCode {
    id: "worker.file.unreadable",
    severity: Severity::Error,
    message_template: "The file {path} could not be read.",
};

/// NFR-SEC-020 wants "a documented upper bound on the resources a single file may cause it to
/// allocate". `namir-nam` and `namir-ir` both apply *dimension* ceilings, but only after parsing
/// bytes they are already holding — so the byte count itself is unbounded until someone bounds it,
/// and the worker is the first component that actually reads a file. This is that bound.
///
/// Deliberately distinct from NFR-PERF-050's 50 MB, which is a *performance target* ("loading
/// shall complete within 500 ms for files up to 50 MB"), not a limit. The ceiling here is set
/// well above it so a legitimate large file is slow rather than rejected.
pub const FILE_TOO_LARGE: ErrorCode = ErrorCode {
    id: "worker.file.too_large",
    severity: Severity::Error,
    message_template: "The file {path} is larger than the {limit_mb} MB limit.",
};

/// D-9.7 truncates an impulse response at ten seconds at the engine rate, and says so should be
/// reported — but no catalogue entry existed for it anywhere, and `PreparedIr::was_truncated()`
/// returned a bare `bool` that nothing consumed. The worker is the first layer that can report
/// anything to a user, so the entry lands here. A warning, not an error: the IR loaded fine and
/// is usable, it is simply shorter than the file.
pub const IR_TRUNCATED: ErrorCode = ErrorCode {
    id: "worker.ir.truncated",
    severity: Severity::Warning,
    message_template: "The impulse response {path} was longer than 10 seconds and was truncated.",
};

/// The prepared resource could not be handed to the audio thread within the submission deadline,
/// so it was dropped here, on the worker, rather than retried forever.
///
/// See [`crate::submit::CommandSubmitter::submit`] for why a deadline exists at all: a host that
/// deactivates a plugin stops calling `process`, and an unbounded retry would wedge a pool thread
/// permanently. D-7.2's "never drops a command silently" is honoured — this is neither silent nor
/// unbounded.
pub const NOT_DELIVERED: ErrorCode = ErrorCode {
    id: "worker.submit.not_delivered",
    severity: Severity::Error,
    message_template: "The engine did not accept {path} in time; it was not loaded.",
};

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-ERR-020: every user-visible error maps to exactly one catalogue entry, verified
    /// statically. Same check every other crate's catalogue carries.
    #[test]
    fn catalogue_ids_are_unique() {
        let all = [
            JOB_PANICKED,
            FILE_UNREADABLE,
            FILE_TOO_LARGE,
            IR_TRUNCATED,
            NOT_DELIVERED,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a.id, b.id, "duplicate catalogue id {}", a.id);
            }
            assert!(a.id.starts_with("worker."), "{} is not namespaced", a.id);
        }
    }
}
