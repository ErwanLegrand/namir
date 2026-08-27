//! This crate's own error catalogue (D-16.1: `ErrorCode` is a shared *type*, not a closed enum,
//! so each crate owns its own consts — the same pattern `namir-nam`/`namir-ir`/`namir-worker`
//! each follow). These are FR-IO's own failure modes: no existing crate's catalogue names "a
//! device failed to open" or "an xrun occurred", because no crate below this one touches a real
//! audio device at all.
//!
//! **Every remedy here names editing `audio-settings.json` and restarting where that is the only
//! way out.** That is not a placeholder for a nicer answer: there is no device-selection surface in
//! either shell (FR-IO-070's third clause, roadmap §15 item 16, issue #26), so a remedy that told
//! the user to "choose another device" would be describing a control that does not exist. When that
//! item is answered, these strings are the list of what has to change with it.

use namir_core::{ErrorCode, Severity};

/// FR-IO-070: a device failed to open (missing, in use by another process exclusively,
/// unsupported configuration). Reported to the user; the application does not crash or hang.
pub const DEVICE_OPEN_FAILED: ErrorCode = ErrorCode::new(
    "app.audio_io.device_open_failed",
    Severity::Error,
    "The audio device could not be opened ({detail}).",
    "Check that the device is connected and that no other application holds it exclusively, then \
     restart Namir. To use a different device, name it in audio-settings.json in Namir's \
     configuration directory first.",
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
    "Exclusive mode is not available, so the device was opened in shared mode ({detail}).",
    "Close whatever else is using the device and restart Namir to try again, or set \
     \"exclusive_mode\": false in audio-settings.json to stop asking for it.",
);

/// FR-IO-070: a device that was open and in use disappeared (unplugged, disabled, reclaimed by
/// the OS). The stream is stopped cleanly and the user is told which side (input/output) was
/// lost — in the `detail`, which [`crate::app`]'s stream-error callback builds from the direction
/// *and* the device's own name (issue #43: both facts were known and both were dropped).
pub const DEVICE_LOST: ErrorCode = ErrorCode::new(
    "app.audio_io.device_lost",
    Severity::Error,
    "An audio device became unavailable and the stream was stopped ({detail}).",
    "Reconnect the device and restart Namir — audio does not resume by itself. To carry on with a \
     different device, name it in audio-settings.json before restarting.",
);

/// A stream stopped for a reason the backend did **not** classify as a device loss or an xrun.
///
/// Added M14 (issue #44). Until then [`DEVICE_LOST`] was reported for this case as well, because
/// the code was chosen from the stream's *direction* and never from its classification — so an
/// unrelated driver fault would have told the user their interface had been unplugged. The
/// 2026-08-27 manual run is the reason this is not hypothetical: a physical unplug arrived as
/// `StreamFailure::Other` carrying an unmapped OS error, and the right answer happened to be
/// reported by luck. `crate::audio_io::classifies_as_device_loss` now recovers the device-loss
/// cases that reach `Other`; whatever is left over lands here rather than being mislabelled.
pub const STREAM_FAILED: ErrorCode = ErrorCode::new(
    "app.audio_io.stream_failed",
    Severity::Error,
    "The audio stream stopped because of a device or driver error ({detail}).",
    "Restart Namir to reopen the stream. If it keeps happening, update the device's driver, or \
     raise buffer_size_frames in audio-settings.json to give the driver more slack.",
);

/// FR-IO-040: none of the sample rates/buffer sizes a device reports as supported could be
/// negotiated at all (an empty supported-configs list, or every candidate rejected by the
/// device when opening). **A device exists** — see [`NO_AUDIO_DEVICE`] for the case where none
/// does, which this entry was also being used for until M14 (issue #40).
pub const NO_SUPPORTED_CONFIG: ErrorCode = ErrorCode::new(
    "app.audio_io.no_supported_config",
    Severity::Error,
    "The audio device reported no usable sample rate and buffer size combination ({detail}).",
    "Set sample_rate_hz and buffer_size_frames in audio-settings.json to a pair the device \
     supports, or remove both so Namir negotiates from the device's own defaults, then restart.",
);

/// No audio device could be opened at all: the system reports none, or every candidate failed
/// before a configuration was ever negotiated. A window still opens and parameters stay editable
/// (`crate::app::open_window_without_audio`), which is what this entry has to say.
///
/// Added M14 (issue #40). [`NO_SUPPORTED_CONFIG`] was reported here, and its own text is FR-IO-040's
/// "none of the rates **a device** reports could be negotiated" — with no device present that
/// sentence has no subject, and the notice named a device the window did not have. Two lines away,
/// the same function already passes `None` for the share-mode indicator rather than a
/// "truthful-looking Shared"; this entry is that judgement applied to the notice as well.
pub const NO_AUDIO_DEVICE: ErrorCode = ErrorCode::new(
    "app.audio_io.no_device",
    Severity::Error,
    "No audio device could be opened, so nothing is being processed. Parameters can still be \
     edited ({detail}).",
    "Connect an audio interface, or enable a device in the operating system's sound settings, then \
     restart Namir.",
);

/// FR-IO-080: the remembered device from a previous session is no longer present. Not fatal —
/// [`crate::device_state`] degrades to a working default — but worth telling the user once.
pub const REMEMBERED_DEVICE_UNAVAILABLE: ErrorCode = ErrorCode::new(
    "app.audio_io.remembered_device_unavailable",
    Severity::Warning,
    "The device remembered from the last session is no longer available, so a different one is in \
     use ({detail}).",
    "Reconnect the remembered device and restart Namir to go back to it. Closing Namir now saves \
     the substitute as the remembered device instead.",
);

/// FR-IO-080: the settings file on disk could not be parsed (corrupted, from an incompatible
/// future version). Degrades to [`crate::settings::AppSettings::default`] (P8) rather than
/// refusing to start — and, since M14, only after the unreadable file has been preserved beside
/// it, which is what the remedy can point at (issue #45).
pub const SETTINGS_UNREADABLE: ErrorCode = ErrorCode::new(
    "app.settings.unreadable",
    Severity::Warning,
    "Saved audio settings could not be read, so this session starts from defaults ({detail}).",
    "The unreadable file was kept beside it, with `.corrupt` added to its name. Repair it and \
     rename it back before closing Namir; otherwise set your device up again, because this \
     session's choices are written over the original when Namir exits.",
);

/// The settings file could not be written back to disk (permissions, disk full, no config
/// directory available on this platform/environment). Degrades to "this session's choices are
/// not remembered" rather than failing the action that triggered the save.
pub const SETTINGS_UNWRITABLE: ErrorCode = ErrorCode::new(
    "app.settings.unwritable",
    Severity::Warning,
    "Audio settings could not be saved, so this session's choices will not be remembered \
     ({detail}).",
    "Check that Namir's configuration directory exists, is writable, and has free space. Until it \
     does, each launch starts from the last settings that saved successfully.",
);

/// Every entry above, for this crate's own enumerability check (FR-ERR-020's first conjunct).
#[cfg(test)]
const ALL: &[ErrorCode] = &[
    DEVICE_OPEN_FAILED,
    EXCLUSIVE_MODE_UNAVAILABLE,
    DEVICE_LOST,
    STREAM_FAILED,
    NO_SUPPORTED_CONFIG,
    NO_AUDIO_DEVICE,
    REMEMBERED_DEVICE_UNAVAILABLE,
    SETTINGS_UNREADABLE,
    SETTINGS_UNWRITABLE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_ids_are_unique_and_non_empty() {
        namir_core::assert_unique_ids(ALL);
    }

    /// FR-IO-020's degradation is a `Warning`, not an `Error` — see the const's own doc comment.
    /// Pinned as a test because the severity is the one thing about this entry a reader could
    /// change without noticing it changes what the notice means (audio running vs. no audio).
    #[test]
    fn an_unavailable_exclusive_mode_is_a_warning_because_audio_still_runs() {
        assert_eq!(EXCLUSIVE_MODE_UNAVAILABLE.severity, Severity::Warning);
        assert_eq!(DEVICE_OPEN_FAILED.severity, Severity::Error);
    }

    /// Issue #40: "no device at all" and "this device offers nothing usable" are different facts
    /// and must not share an id. The distinction is only worth anything if the *texts* differ too,
    /// so both halves are pinned.
    #[test]
    fn no_device_and_no_usable_config_are_separate_entries() {
        assert_ne!(NO_AUDIO_DEVICE.id, NO_SUPPORTED_CONFIG.id);
        assert!(
            !NO_AUDIO_DEVICE.message_template.contains("device reported"),
            "the no-device entry must not claim a device reported anything"
        );
    }
}
