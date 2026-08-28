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
pub const JOB_PANICKED: ErrorCode = ErrorCode::new(
    "worker.job.panicked",
    Severity::Fault,
    "An internal error interrupted a background job ({detail}).",
    "Audio is unaffected and Namir keeps running; retry whatever you were loading. If it happens \
     every time on the same file, that file is the thing to report -- namir.log in Namir's \
     configuration directory carries the record.",
);

/// The file could not be read at all — missing, permissions, or an I/O error partway through.
pub const FILE_UNREADABLE: ErrorCode = ErrorCode::new(
    "worker.file.unreadable",
    Severity::Error,
    "The file could not be read ({detail}).",
    "Check the file is still where the library lists it and that you have permission to read it. \
     If it has moved or been deleted, rescan the library so the list matches the disk.",
);

/// NFR-SEC-020 wants "a documented upper bound on the resources a single file may cause it to
/// allocate". `namir-nam` and `namir-ir` both apply *dimension* ceilings, but only after parsing
/// bytes they are already holding — so the byte count itself is unbounded until someone bounds it,
/// and the worker is the first component that actually reads a file. This is that bound.
///
/// Deliberately distinct from NFR-PERF-050's 50 MB, which is a *performance target* ("loading
/// shall complete within 500 ms for files up to 50 MB"), not a limit. The ceiling here is set
/// well above it so a legitimate large file is slow rather than rejected.
pub const FILE_TOO_LARGE: ErrorCode = ErrorCode::new(
    "worker.file.too_large",
    Severity::Error,
    "The file is larger than the limit Namir will load ({detail}).",
    "Use a smaller file. A `.nam` model or `.wav` impulse response this large is almost always a \
     mistake -- the wrong file extension, or a recording saved in place of an export.",
);

/// The path names something that is not a regular file — a directory, a device, a FIFO.
///
/// Issue #107's other half, and the one a byte ceiling cannot cover: on Unix, opening a FIFO
/// blocks until a writer appears, and a character device reports `len() == 0` while streaming
/// forever. Neither is answered by bounding the read, only by not opening the thing — so the file
/// *type* is checked before anything is opened, exactly as `namir-library`'s `StdFs::read_file`
/// does after the same fix.
pub const FILE_NOT_REGULAR: ErrorCode = ErrorCode::new(
    "worker.file.not_regular",
    Severity::Error,
    "That path is not a regular file, so Namir will not load it ({detail}).",
    "Point Namir at the `.nam` or `.wav` file itself. If a preset names this path, the file it \
     was saved against has been replaced by a folder or a device -- locate the original and load \
     it again.",
);

/// D-9.7 truncates an impulse response at ten seconds at the engine rate, and says so should be
/// reported — but no catalogue entry existed for it anywhere, and `PreparedIr::was_truncated()`
/// returned a bare `bool` that nothing consumed. The worker is the first layer that can report
/// anything to a user, so the entry lands here. A warning, not an error: the IR loaded fine and
/// is usable, it is simply shorter than the file.
pub const IR_TRUNCATED: ErrorCode = ErrorCode::new(
    "worker.ir.truncated",
    Severity::Warning,
    "The impulse response was longer than 10 seconds and only its first 10 seconds are being used \
     ({detail}).",
    "Nothing needs doing -- the impulse response is loaded and audible. If you meant to use its \
     tail, trim the file to 10 seconds or less in an audio editor and load it again.",
);

/// The prepared resource could not be handed to the audio thread within the submission deadline,
/// so it was dropped here, on the worker, rather than retried forever.
///
/// See [`crate::submit::CommandSubmitter::submit`] for why a deadline exists at all: a host that
/// deactivates a plugin stops calling `process`, and an unbounded retry would wedge a pool thread
/// permanently. D-7.2's "never drops a command silently" is honoured — this is neither silent nor
/// unbounded.
pub const NOT_DELIVERED: ErrorCode = ErrorCode::new(
    "worker.submit.not_delivered",
    Severity::Error,
    "The audio engine did not accept the prepared file in time, so it was not loaded ({detail}).",
    "Load it again. If Namir is a plugin, check the host's transport is running -- a deactivated \
     plugin has no audio thread to hand the file to.",
);

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
            FILE_NOT_REGULAR,
            IR_TRUNCATED,
            NOT_DELIVERED,
        ];
        // `assert_unique_ids` also carries FR-UI-070's remedy clause and issue #15's
        // one-placeholder rule; the namespacing check below is this crate's own addition.
        namir_core::assert_unique_ids(&all);
        for a in &all {
            assert!(a.id.starts_with("worker."), "{} is not namespaced", a.id);
        }
    }
}
