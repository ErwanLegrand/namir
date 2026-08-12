//! This crate's own error catalogue (D-16.1: `ErrorCode` is a shared *type*, not a closed enum,
//! so each crate owns its own consts — the same pattern `namir-nam`/`namir-ir`/`namir-worker`
//! each follow). These are FR-IO's own failure modes: no existing crate's catalogue names "a
//! device failed to open" or "an xrun occurred", because no crate below this one touches a real
//! audio device at all.

use namir_core::{ErrorCode, Severity};

/// FR-IO-070: a device failed to open (missing, in use by another process exclusively,
/// unsupported configuration). Reported to the user; the application does not crash or hang.
pub const DEVICE_OPEN_FAILED: ErrorCode = ErrorCode::new(
    "app.audio_io.device_open_failed",
    Severity::Error,
    "Could not open audio device {device}: {reason}.",
);

/// FR-IO-020: exclusive mode was asked for (`crate::settings::AppSettings::exclusive_mode`) and at
/// least one of the session's two devices could not provide it, so the session opens shared
/// instead. **`Warning`, not `Error`, deliberately:** audio still runs, which is exactly the
/// degradation `docs/02-architecture.md` D-13.4 requires ("the settings path must degrade to shared
/// rather than leave the app with no audio"); reporting it at `Error` would put a working session
/// next to [`DEVICE_OPEN_FAILED`], which means no audio at all. Reported once, at start-up, rather
/// than silently — roadmap §18 asks for "the user told which mode they actually got", and the
/// notice is the half of that a mode indicator alone cannot give (it says *why*).
pub const EXCLUSIVE_MODE_UNAVAILABLE: ErrorCode = ErrorCode::new(
    "app.audio_io.exclusive_mode_unavailable",
    Severity::Warning,
    "Exclusive mode is not available for {device} ({reason}); using shared mode.",
);

/// FR-IO-070: a device that was open and in use disappeared (unplugged, disabled, reclaimed by
/// the OS). The stream is stopped cleanly and the user is told which side (input/output) was
/// lost.
pub const DEVICE_LOST: ErrorCode = ErrorCode::new(
    "app.audio_io.device_lost",
    Severity::Error,
    "The {direction} device \"{device}\" became unavailable and the stream was \
                        stopped.",
);

/// FR-IO-040: none of the sample rates/buffer sizes a device reports as supported could be
/// negotiated at all (an empty supported-configs list, or every candidate rejected by the
/// device when opening).
pub const NO_SUPPORTED_CONFIG: ErrorCode = ErrorCode::new(
    "app.audio_io.no_supported_config",
    Severity::Error,
    "Device {device} reported no usable sample rate/buffer size combination.",
);

/// FR-IO-080: the remembered device from a previous session is no longer present. Not fatal —
/// [`crate::device_state`] degrades to a working default — but worth telling the user once.
pub const REMEMBERED_DEVICE_UNAVAILABLE: ErrorCode = ErrorCode::new(
    "app.audio_io.remembered_device_unavailable",
    Severity::Warning,
    "The previously selected {direction} device \"{device}\" is no longer \
                        available; using {fallback} instead.",
);

/// FR-IO-080: the settings file on disk could not be parsed (corrupted, from an incompatible
/// future version). Degrades to defaults (P8) rather than refusing to start.
pub const SETTINGS_UNREADABLE: ErrorCode = ErrorCode::new(
    "app.settings.unreadable",
    Severity::Warning,
    "Saved audio settings could not be read ({reason}); using defaults.",
);

/// The settings file could not be written back to disk (permissions, disk full, no config
/// directory available on this platform/environment). Degrades to "this session's choices are
/// not remembered" rather than failing the action that triggered the save.
pub const SETTINGS_UNWRITABLE: ErrorCode = ErrorCode::new(
    "app.settings.unwritable",
    Severity::Warning,
    "Audio settings could not be saved ({reason}).",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_ids_are_unique_and_non_empty() {
        namir_core::assert_unique_ids(&[
            DEVICE_OPEN_FAILED,
            EXCLUSIVE_MODE_UNAVAILABLE,
            DEVICE_LOST,
            NO_SUPPORTED_CONFIG,
            REMEMBERED_DEVICE_UNAVAILABLE,
            SETTINGS_UNREADABLE,
            SETTINGS_UNWRITABLE,
        ]);
    }

    /// FR-IO-020's degradation is a `Warning`, not an `Error` — see the const's own doc comment.
    /// Pinned as a test because the severity is the one thing about this entry a reader could
    /// change without noticing it changes what the notice means (audio running vs. no audio).
    #[test]
    fn an_unavailable_exclusive_mode_is_a_warning_because_audio_still_runs() {
        assert_eq!(EXCLUSIVE_MODE_UNAVAILABLE.severity, Severity::Warning);
        assert_eq!(DEVICE_OPEN_FAILED.severity, Severity::Error);
    }
}
