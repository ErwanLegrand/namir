//! Local error catalogue for `namir-clap` (D-16.1) — failure modes that belong to the CLAP
//! adapter itself, distinct from a parse failure (already carries a precise `nam.load.*`/
//! `ir.load.*`/... id from the crate that detected it, passed through unchanged) or a
//! `namir-worker` orchestration failure (`worker.*`).

use namir_core::{ErrorCode, Severity};

/// The GUI asked to (re)scan the library, but this process has no library service: either
/// `namir_platform::config_dir()` returned `None` (an environment this crate has no per-user
/// directory convention for) or, more commonly today, no library roots have ever been
/// configured — `namir-ui`'s `UiIntent` set does not yet offer a way to add one (see
/// `crate::shared`'s module doc comment).
pub const LIBRARY_UNAVAILABLE: ErrorCode = ErrorCode::new(
    "clap.library.unavailable",
    Severity::Warning,
    "The library scan could not start: {detail}.",
    "Add a library folder before scanning. Namir's standalone application creates and scans one by \
     default, so running it once is the quickest way to get a library the plugin can then see.",
);

/// The host handed `set_parent` a window handle that could not be interpreted as valid — see
/// `crate::gui`'s written safety argument for the trust boundary this crosses.
pub const GUI_INVALID_PARENT: ErrorCode = ErrorCode::new(
    "clap.gui.invalid_parent",
    Severity::Error,
    "The host-supplied editor window handle was not usable.",
    "Close and reopen the plugin's editor in your host. If it keeps failing, the host's window \
     handling is the thing to report -- namir.log records what was handed over.",
);

/// The host presented a sample rate this crate could not represent (zero, or non-finite/negative
/// once rounded) — `namir_core::SampleRate` requires a nonzero `u32`.
pub const INVALID_SAMPLE_RATE: ErrorCode = ErrorCode::new(
    "clap.activate.invalid_sample_rate",
    Severity::Error,
    "The host presented an unusable sample rate ({detail}).",
    "Set your host's sample rate to a normal audio rate between 44.1 kHz and 192 kHz, then reload \
     the plugin.",
);

/// A named preset could not be placed: this environment has no per-user configuration directory
/// (so [`crate::presets::preset_dir`] resolved nothing), or the name the user typed is not one
/// that can name a plain file inside it. `namir_ui::UiIntent::SavePreset`'s own doc comment makes
/// rejecting such a name the host's responsibility, and this is that rejection made visible
/// instead of silent.
pub const PRESET_UNAVAILABLE: ErrorCode = ErrorCode::new(
    "clap.preset.unavailable",
    Severity::Warning,
    "The preset could not be saved: {detail}.",
    "Pick a name without slashes, colons or other characters a filename cannot contain. If no \
     preset folder exists at all, Namir's standalone application creates one the first time it \
     saves a preset.",
);

/// A preset file could not be written or read back — a full disk, a read-only folder, a file
/// another program holds open.
pub const PRESET_IO_FAILED: ErrorCode = ErrorCode::new(
    "clap.preset.io_failed",
    Severity::Error,
    "The preset file could not be read or written ({detail}).",
    "Check that the preset folder exists and is writable, then try again. namir.log records the \
     exact path and the operating system's own reason.",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_has_no_duplicate_or_empty_ids() {
        namir_core::assert_unique_ids(&[
            LIBRARY_UNAVAILABLE,
            GUI_INVALID_PARENT,
            INVALID_SAMPLE_RATE,
            PRESET_UNAVAILABLE,
            PRESET_IO_FAILED,
        ]);
    }
}
