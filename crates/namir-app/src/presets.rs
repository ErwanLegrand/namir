//! FR-STATE-030's named-preset half, for the standalone application: where a `.namirpreset` lives,
//! how the set of them is listed for [`namir_ui::UiSnapshot::presets`], and the naming rule the two
//! file operations [`namir_ui::UiIntent::SavePreset`]/`RecallPreset` are held to.
//!
//! # ⚠ This resolution belongs in `namir-platform`, not here ⚠
//!
//! FR-STATE-030's presets are "interchangeable between the two products", and interchangeability
//! fails at the *discovery* step — not at the format — if the two shells look in two different
//! directories. `namir-worker`'s [`namir_worker::library::LibraryService::open_default`] is this
//! workspace's own precedent and its own written warning: `namir-app` and `namir-clap` each
//! computing the library's default location independently is what let their library wiring drift
//! apart once already, and the fix was to make one function the only way either shell can ask.
//!
//! The same fix is owed here, and this module is **not** it: D-13.2 puts filesystem locations in
//! `namir-platform` ("Filesystem locations, config directories, log sinks … live in
//! `namir-platform` and nowhere else"), so [`preset_dir_under`] below should be a `preset_dir()`
//! beside `namir_platform::config_dir()`/`log_file_path()`, with `namir-clap` calling the same
//! function. It is here only because this change could not touch another crate.
//!
//! **`crates/namir-clap/src/presets.rs` is the other copy, and the two agree by construction of
//! this file**: the directory name (`Presets`, chosen to match `LibraryService::open_at`'s own
//! `<config_dir>/Library`), the extension, [`sanitise_name`]'s rejection set and
//! [`list_presets`]'s "regular files only, named by stem, sorted, unreadable directory is an empty
//! list" semantics are the same rule written twice. Hoisting it is a two-caller deletion; changing
//! either copy alone silently breaks FR-STATE-030's interchangeability claim at discovery.
//!
//! # Naming
//!
//! [`namir_ui::UiIntent::SavePreset`] carries "a name, not a path", already trimmed and non-empty,
//! and says in as many words that a name illegal as a filename is *the host's* to reject. This
//! module is that host: [`sanitise_name`] refuses anything that could escape the preset directory
//! or name something other than a plain file in it, and [`crate::host::AppHost`] reports the
//! refusal as an FR-UI-070 notice rather than writing somewhere the user did not ask for.

use std::path::{Path, PathBuf};

use namir_ui::PresetSummary;

/// The extension `docs/04-state-and-preset-format.md` gives the preset document.
pub const PRESET_EXTENSION: &str = "namirpreset";

/// The subdirectory of the per-user configuration directory both products must agree on.
pub const PRESET_DIR_NAME: &str = "Presets";

/// The preset directory under an already-resolved configuration directory.
///
/// Takes the configuration directory rather than resolving one, because this crate has two:
/// [`namir_platform::config_dir`], and [`crate::startup_probe`]'s override of it, which points a
/// NFR-PERF-030 measurement run at a directory the harness owns. A probed launch never opens a
/// window and so never lists or writes a preset, but taking the directory as a parameter is what
/// keeps that true by construction rather than by argument.
#[must_use]
pub fn preset_dir_under(config_dir: &Path) -> PathBuf {
    config_dir.join(PRESET_DIR_NAME)
}

/// The file a preset called `name` is stored in, or `None` if `name` is not one this shell will
/// write — see [`sanitise_name`].
#[must_use]
pub fn preset_path(dir: &Path, name: &str) -> Option<PathBuf> {
    Some(dir.join(format!("{}.{PRESET_EXTENSION}", sanitise_name(name)?)))
}

/// The name, if it is one that can only ever name a plain file directly inside the preset
/// directory.
///
/// Rejected: anything empty once trimmed, anything containing a path separator of either platform
/// (so a name can never reach a sibling directory), anything that is `.` or `..`, anything with a
/// Windows drive prefix, and anything containing a character Windows refuses in a filename. The
/// last is checked on every platform on purpose: a preset saved on Linux under a name Windows
/// cannot represent would be a preset the other half of FR-STATE-030's interchangeability claim
/// cannot open.
#[must_use]
pub fn sanitise_name(name: &str) -> Option<&str> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name.chars().any(|c| {
        matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
    }) {
        return None;
    }
    Some(name)
}

/// Every `.namirpreset` directly inside `dir`, named by its file stem, sorted by name.
///
/// Non-recursive, and a directory that does not exist (or cannot be read) is an empty list rather
/// than an error: "no presets saved yet" is the ordinary first-run state, and there is nothing for
/// a user to act on in being told about it. The empty list is what the UI renders as a disabled
/// recall control, which is what [`namir_ui::UiSnapshot::presets`] documents for "the host knows
/// of none".
///
/// **Blocking:** this reads a directory, so it runs on [`crate::worker`]'s thread, never inside
/// [`namir_ui::UiHost::snapshot`].
#[must_use]
pub fn list_presets(dir: &Path) -> Vec<PresetSummary> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut presets: Vec<PresetSummary> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(PRESET_EXTENSION))
        })
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Some(PresetSummary { name, path })
        })
        .collect();
    // A deterministic order, so the list does not reshuffle between frames on a filesystem whose
    // `read_dir` order is not stable.
    presets.sort_by(|a, b| a.name.cmp(&b.name));
    presets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-app-presets-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// FR-STATE-030's "interchangeable between the two products" begins at discovery: this shell
    /// and `namir-clap` must name the same directory under the same configuration root. Pinned on
    /// the two constants that encode it, since the other copy is in a crate this test cannot see.
    #[test]
    fn the_preset_directory_sits_beside_the_library_under_the_shared_config_directory() {
        assert_eq!(PRESET_DIR_NAME, "Presets");
        assert_eq!(PRESET_EXTENSION, "namirpreset");
        let config = Path::new("/somewhere/config");
        assert_eq!(
            preset_dir_under(config),
            config.join("Presets"),
            "both shells must resolve presets under the one config directory they share, the way \
             LibraryService::open_at resolves <config_dir>/Library"
        );
    }

    #[test]
    fn a_name_that_could_escape_the_preset_directory_is_refused() {
        for hostile in [
            "../evil",
            "..\\evil",
            "sub/dir",
            "sub\\dir",
            "C:evil",
            "..",
            ".",
            "   ",
            "bad\u{0}name",
        ] {
            assert!(
                sanitise_name(hostile).is_none(),
                "{hostile:?} must not be accepted as a preset name"
            );
            assert!(preset_path(Path::new("/presets"), hostile).is_none());
        }
        assert_eq!(sanitise_name("  Crunch Rhythm  "), Some("Crunch Rhythm"));
        assert_eq!(
            preset_path(Path::new("/presets"), " Crunch Rhythm "),
            Some(PathBuf::from("/presets").join("Crunch Rhythm.namirpreset"))
        );
    }

    #[test]
    fn listing_finds_only_preset_files_and_names_them_by_stem() {
        let dir = temp_dir("listing");
        std::fs::write(dir.join("Clean.namirpreset"), b"{}").unwrap();
        std::fs::write(dir.join("Lead.NAMIRPRESET"), b"{}").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("Nested.namirpreset")).unwrap();

        let presets = list_presets(&dir);
        let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Clean", "Lead"],
            "only regular .namirpreset files, named by stem, sorted"
        );
        assert_eq!(presets[0].path, dir.join("Clean.namirpreset"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_that_does_not_exist_lists_nothing_rather_than_failing() {
        let dir = temp_dir("absent").join("never-created");
        assert!(list_presets(&dir).is_empty());
    }
}
