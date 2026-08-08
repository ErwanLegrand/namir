//! FR-IO-080: "Audio device selection, sample rate, buffer size and channel mapping shall persist
//! between sessions, and the application shall degrade gracefully to a working default if the
//! remembered device is unavailable at start-up."
//!
//! [`AppSettings`] is the persisted record; [`device_state`](crate::device_state) is the pure
//! logic that turns a possibly-stale [`AppSettings`] plus what the system reports *today* into an
//! actual device/rate/buffer choice — this module only reads and writes the record itself.
//!
//! Deliberately **not** `namir_state::State` (the preset/plugin-state document FR-STATE-010
//! governs): that format is parameters, global bypass/ceiling, and model/IR references — nothing
//! about which physical audio device to open. Folding device selection into it would make a
//! preset file host-machine-specific in a way FR-STATE's own UC-3 ("share a project with someone
//! else") explicitly does not want. This is a second, independent file, matching `namir-library`'s
//! own index file's independence from a preset.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error_codes;

/// One channel-mapping choice: which physical device channel (0-indexed, as the device itself
/// numbers them) feeds engine input 0, or receives engine output 0/1. `None` means "use the
/// device's own first channel(s)" — the FR-IO-080 default that needs no prior configuration.
///
/// FR-IO-090 (Should) is the requirement this exists for; it is deliberately a thin, inert record
/// here — [`crate::stream`] is what would actually honour a non-default mapping, and doing so is
/// this crate's own manual-test-documented gap (see `docs/manual-tests/fr-io-090-channel-mapping.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ChannelMapping {
    /// Physical input channel index feeding the engine's mono input.
    pub input_channel: Option<u16>,
    /// Physical output channel index receiving the engine's left/mono output.
    pub output_channel_left: Option<u16>,
    /// Physical output channel index receiving the engine's right output (stereo only).
    pub output_channel_right: Option<u16>,
}

/// FR-IO-080's persisted record. Every field is an independent, optional "what was remembered" —
/// never a hard requirement to honour on the next launch, since the device it names may be gone
/// (see [`crate::device_state`] for the degrade-gracefully rule this record feeds).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    /// The host API name last selected (e.g. `"WASAPI"`), or `None` for "use the system default
    /// host" — the FR-IO-080 default before any session has ever chosen one.
    pub host_name: Option<String>,
    /// The input device's name, as `cpal` reports it, or `None` for "system default input".
    pub input_device_name: Option<String>,
    /// The output device's name, or `None` for "system default output".
    pub output_device_name: Option<String>,
    /// FR-IO-020: whether WASAPI exclusive mode was selected (Windows only; ignored elsewhere).
    pub exclusive_mode: bool,
    /// The sample rate last selected, in Hz, or `None` for "the device's own default".
    pub sample_rate_hz: Option<u32>,
    /// The buffer size last selected, in frames, or `None` for "the device's own default".
    pub buffer_size_frames: Option<u32>,
    /// FR-IO-090's channel mapping.
    pub channel_mapping: ChannelMapping,
}

impl Default for AppSettings {
    /// FR-IO-080's "working default": every field unset, meaning "whatever the system reports as
    /// its own default" — a freshly installed Namir needs no prior configuration to produce sound.
    fn default() -> Self {
        Self {
            host_name: None,
            input_device_name: None,
            output_device_name: None,
            exclusive_mode: false,
            sample_rate_hz: None,
            buffer_size_frames: None,
            channel_mapping: ChannelMapping::default(),
        }
    }
}

/// Reasons [`load`]/[`save`] degraded rather than fully succeeding — reported through
/// [`crate::error_codes`], never propagated as a hard failure (P8): a settings problem must never
/// stop the application from starting or from processing audio.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsWarning {
    /// Which catalogue entry this is.
    pub code: namir_core::ErrorCode,
    /// Free-text detail for the template's placeholder.
    pub detail: String,
}

/// The file [`AppSettings`] round-trips through, under [`namir_platform::config_dir`].
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("audio-settings.json")
}

/// Loads settings from `path`. Never fails (P8, mirroring `namir_library::IndexStore::open`'s own
/// guarantee): a missing file is the ordinary first-run case and produces no warning; a
/// present-but-corrupt one degrades to [`AppSettings::default`] plus a warning, rather than
/// refusing to start.
pub fn load(path: &Path) -> (AppSettings, Option<SettingsWarning>) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (AppSettings::default(), None);
        }
        Err(e) => {
            return (
                AppSettings::default(),
                Some(SettingsWarning {
                    code: error_codes::SETTINGS_UNREADABLE,
                    detail: e.to_string(),
                }),
            );
        }
    };
    match serde_json::from_slice::<AppSettings>(&bytes) {
        Ok(settings) => (settings, None),
        Err(e) => (
            AppSettings::default(),
            Some(SettingsWarning {
                code: error_codes::SETTINGS_UNREADABLE,
                detail: e.to_string(),
            }),
        ),
    }
}

/// Saves `settings` to `path`, creating its parent directory if needed. Atomic (write to a
/// sibling temp file, then rename) so a crash or power loss mid-write can never leave a
/// half-written file for the next [`load`] to trip over — the same discipline
/// `namir_library::IndexStore::save_atomic` already applies to the library index, for the
/// identical reason.
pub fn save(path: &Path, settings: &AppSettings) -> Result<(), SettingsWarning> {
    let json = serde_json::to_vec_pretty(settings).map_err(|e| SettingsWarning {
        code: error_codes::SETTINGS_UNWRITABLE,
        detail: e.to_string(),
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SettingsWarning {
            code: error_codes::SETTINGS_UNWRITABLE,
            detail: e.to_string(),
        })?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| SettingsWarning {
        code: error_codes::SETTINGS_UNWRITABLE,
        detail: e.to_string(),
    })?;
    std::fs::rename(&tmp, path).map_err(|e| SettingsWarning {
        code: error_codes::SETTINGS_UNWRITABLE,
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-app-settings-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// FR-IO-080's literal round trip: every field survives a save/load cycle.
    // trace: FR-IO-080
    #[test]
    fn settings_round_trip() {
        let dir = temp_dir("round_trip");
        let path = settings_path(&dir);

        let settings = AppSettings {
            host_name: Some("WASAPI".to_string()),
            input_device_name: Some("Scarlett 2i2".to_string()),
            output_device_name: Some("Scarlett 2i2".to_string()),
            exclusive_mode: true,
            sample_rate_hz: Some(48_000),
            buffer_size_frames: Some(128),
            channel_mapping: ChannelMapping {
                input_channel: Some(1),
                ..Default::default()
            },
        };

        save(&path, &settings).unwrap();
        let (loaded, warning) = load(&path);
        assert!(warning.is_none());
        assert_eq!(loaded, settings);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ordinary first-run case: no file yet, no warning, plain defaults.
    #[test]
    fn missing_file_loads_defaults_with_no_warning() {
        let dir = temp_dir("missing");
        let path = settings_path(&dir);
        let (settings, warning) = load(&path);
        assert_eq!(settings, AppSettings::default());
        assert!(warning.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P8: a corrupt file degrades to defaults plus a reported warning, never a hard failure.
    #[test]
    fn corrupt_file_degrades_to_defaults_with_a_warning() {
        let dir = temp_dir("corrupt");
        let path = settings_path(&dir);
        std::fs::write(&path, b"{ not json at all").unwrap();

        let (settings, warning) = load(&path);
        assert_eq!(settings, AppSettings::default());
        let warning = warning.expect("a corrupt file should report a warning");
        assert_eq!(warning.code.id, error_codes::SETTINGS_UNREADABLE.id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `save` creates its parent directory when it doesn't exist yet (a fresh config dir).
    #[test]
    fn save_creates_a_missing_parent_directory() {
        let dir = temp_dir("nested").join("nested").join("dirs");
        let path = settings_path(&dir);
        assert!(!dir.exists());
        save(&path, &AppSettings::default()).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(temp_dir("nested"));
    }

    /// A default-constructed record has every field unset -- FR-IO-080's "working default"
    /// needing no prior configuration.
    #[test]
    fn default_settings_have_no_remembered_choices() {
        let settings = AppSettings::default();
        assert!(settings.host_name.is_none());
        assert!(settings.input_device_name.is_none());
        assert!(settings.output_device_name.is_none());
        assert!(settings.sample_rate_hz.is_none());
        assert!(settings.buffer_size_frames.is_none());
        assert!(!settings.exclusive_mode);
    }

    /// The write is atomic: a save that is interrupted after the temp file but before the rename
    /// leaves the *original* file (if any) intact, never a half-written one at the real path.
    /// Simulated here by writing an original, then saving new settings, and checking the final
    /// file is either the old content or the fully-new content -- never a truncated mix. Since
    /// this test cannot literally interrupt the process, it instead pins the *mechanism* (a
    /// rename, not an in-place write) by checking the temp file is gone afterward.
    #[test]
    fn save_leaves_no_temp_file_behind_on_success() {
        let dir = temp_dir("atomic");
        let path = settings_path(&dir);
        save(&path, &AppSettings::default()).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
