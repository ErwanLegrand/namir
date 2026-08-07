//! This crate's own error catalogue (D-16.1: `ErrorCode` is a shared *type*, not a closed enum,
//! so each crate owns its own consts — the same pattern `namir-nam`/`namir-ir`/`namir-worker`
//! each follow). These are FR-IO's own failure modes: no existing crate's catalogue names "a
//! device failed to open" or "an xrun occurred", because no crate below this one touches a real
//! audio device at all.

use namir_core::{ErrorCode, Severity};

/// FR-IO-070: a device failed to open (missing, in use by another process exclusively,
/// unsupported configuration). Reported to the user; the application does not crash or hang.
pub const DEVICE_OPEN_FAILED: ErrorCode = ErrorCode {
    id: "app.audio_io.device_open_failed",
    severity: Severity::Error,
    message_template: "Could not open audio device {device}: {reason}.",
};

/// FR-IO-070: a device that was open and in use disappeared (unplugged, disabled, reclaimed by
/// the OS). The stream is stopped cleanly and the user is told which side (input/output) was
/// lost.
pub const DEVICE_LOST: ErrorCode = ErrorCode {
    id: "app.audio_io.device_lost",
    severity: Severity::Error,
    message_template: "The {direction} device \"{device}\" became unavailable and the stream was \
                        stopped.",
};

/// FR-IO-040: none of the sample rates/buffer sizes a device reports as supported could be
/// negotiated at all (an empty supported-configs list, or every candidate rejected by the
/// device when opening).
pub const NO_SUPPORTED_CONFIG: ErrorCode = ErrorCode {
    id: "app.audio_io.no_supported_config",
    severity: Severity::Error,
    message_template: "Device {device} reported no usable sample rate/buffer size combination.",
};

/// FR-IO-080: the remembered device from a previous session is no longer present. Not fatal —
/// [`crate::device_state`] degrades to a working default — but worth telling the user once.
pub const REMEMBERED_DEVICE_UNAVAILABLE: ErrorCode = ErrorCode {
    id: "app.audio_io.remembered_device_unavailable",
    severity: Severity::Warning,
    message_template: "The previously selected {direction} device \"{device}\" is no longer \
                        available; using {fallback} instead.",
};

/// FR-IO-080: the settings file on disk could not be parsed (corrupted, from an incompatible
/// future version). Degrades to defaults (P8) rather than refusing to start.
pub const SETTINGS_UNREADABLE: ErrorCode = ErrorCode {
    id: "app.settings.unreadable",
    severity: Severity::Warning,
    message_template: "Saved audio settings could not be read ({reason}); using defaults.",
};

/// The settings file could not be written back to disk (permissions, disk full, no config
/// directory available on this platform/environment). Degrades to "this session's choices are
/// not remembered" rather than failing the action that triggered the save.
pub const SETTINGS_UNWRITABLE: ErrorCode = ErrorCode {
    id: "app.settings.unwritable",
    severity: Severity::Warning,
    message_template: "Audio settings could not be saved ({reason}).",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_ids_are_unique_and_non_empty() {
        namir_core::assert_unique_ids(&[
            DEVICE_OPEN_FAILED,
            DEVICE_LOST,
            NO_SUPPORTED_CONFIG,
            REMEMBERED_DEVICE_UNAVAILABLE,
            SETTINGS_UNREADABLE,
            SETTINGS_UNWRITABLE,
        ]);
    }
}
