//! Where a `.namirpreset` lives, and the naming rule both products are held to.
//!
//! FR-STATE-030's presets are "interchangeable between the two products", and interchangeability
//! fails at the **discovery** step — not at the format — if the two shells look in two different
//! directories. `namir-worker`'s `LibraryService::open_default` is this workspace's own precedent
//! and its own written warning: `namir-app` and `namir-clap` each computing the library's default
//! location independently is what let their library wiring drift apart once already, and the fix
//! was to make one function the only way either shell can ask. This module is that function for
//! presets, and D-13.2 is why it lives here: "filesystem locations, config directories, log sinks
//! … live in `namir-platform` and nowhere else".
//!
//! # What is here and what is not
//!
//! [`list_preset_files`] returns `(name, path)` pairs rather than the `PresetSummary` the interface
//! renders. D-5.1 lets this crate depend on `namir-core` and nothing else, so the shape the UI
//! seam names cannot be built here; each shell maps the pairs into it, which is the one line of
//! this rule that is legitimately per-shell.
//!
//! # Naming
//!
//! `UiIntent::SavePreset` carries "a name, not a path", already trimmed and non-empty, and says
//! that a name illegal as a filename is *the host's* to reject. [`sanitise_name`] is that rule,
//! shared so that a name one product accepts is never one the other refuses.

use std::path::{Path, PathBuf};

/// The extension `docs/04-state-and-preset-format.md` gives the preset document.
pub const PRESET_EXTENSION: &str = "namirpreset";

/// The subdirectory of the per-user configuration directory both products must agree on.
///
/// `Presets`, matching `LibraryService::open_at`'s own `<config_dir>/Library`.
pub const PRESET_DIR_NAME: &str = "Presets";

/// The preset directory under an already-resolved configuration directory.
///
/// Takes the configuration directory rather than resolving one, because `namir-app` has two:
/// [`crate::config_dir`], and its `startup_probe` override, which points an NFR-PERF-030
/// measurement run at a directory the harness owns. A probed launch never opens a window and so
/// never lists or writes a preset, but taking the directory as a parameter is what keeps that true
/// by construction rather than by argument. [`preset_dir`] is the ordinary form.
#[must_use]
pub fn preset_dir_under(config_dir: &Path) -> PathBuf {
    config_dir.join(PRESET_DIR_NAME)
}

/// The preset directory beneath this user's configuration directory, or `None` where
/// [`crate::config_dir`] resolves none.
#[must_use]
pub fn preset_dir() -> Option<PathBuf> {
    crate::config_dir().map(|dir| preset_dir_under(&dir))
}

/// The file a preset called `name` is stored in, or `None` if `name` is not one either product
/// will write — see [`sanitise_name`].
#[must_use]
pub fn preset_path(dir: &Path, name: &str) -> Option<PathBuf> {
    Some(dir.join(format!("{}.{PRESET_EXTENSION}", sanitise_name(name)?)))
}

/// The name, if it is one that can only ever name a plain file directly inside the preset
/// directory.
///
/// Rejected: anything empty once trimmed, anything containing a path separator of either platform
/// (so a name can never reach a sibling directory), anything that is `.` or `..`, and anything
/// containing a character Windows refuses in a filename. The last is checked on every platform on
/// purpose: a preset saved on Linux under a name Windows cannot represent would be a preset the
/// other half of FR-STATE-030's interchangeability claim cannot open.
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

/// Every `.namirpreset` directly inside `dir` as a `(name, path)` pair, named by file stem, sorted
/// by name.
///
/// Non-recursive, and a directory that does not exist (or cannot be read) is an empty list rather
/// than an error: "no presets saved yet" is the ordinary first-run state, and there is nothing for
/// a user to act on in being told about it.
///
/// **Blocking:** this reads a directory, so it belongs on a worker thread, never inside a
/// `UiHost::snapshot` call.
#[must_use]
pub fn list_preset_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut presets: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(PRESET_EXTENSION))
        })
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_owned();
            Some((name, path))
        })
        .collect();
    presets.sort_by(|a, b| a.0.cmp(&b.0));
    presets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-platform-presets-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// FR-STATE-030's interchangeability begins at discovery, and this is the function that makes
    /// the two products agree by construction rather than by two copies happening to match. The
    /// assertion is on the composition, not just the constants: a shell that resolved
    /// `<config>/Presets` itself would still drift the day either half changed.
    #[test]
    fn the_preset_directory_is_one_rule_both_products_get_from_here() {
        assert_eq!(PRESET_DIR_NAME, "Presets");
        assert_eq!(PRESET_EXTENSION, "namirpreset");
        let config = Path::new("/somewhere/config");
        assert_eq!(preset_dir_under(config), config.join("Presets"));
        assert_eq!(
            preset_path(&preset_dir_under(config), "Crunch"),
            Some(config.join("Presets").join("Crunch.namirpreset"))
        );
    }

    #[test]
    fn a_name_that_could_escape_the_preset_directory_is_refused() {
        for name in [
            "", "   ", ".", "..", "a/b", "a\\b", "C:name", "a:b", "a*b", "a?b", "a\"b", "a<b",
            "a>b", "a|b", "a\u{0}b",
        ] {
            assert_eq!(sanitise_name(name), None, "{name:?} must be refused");
            assert_eq!(preset_path(Path::new("/presets"), name), None);
        }
        assert_eq!(sanitise_name("  Crunch  "), Some("Crunch"));
    }

    #[test]
    fn listing_names_by_stem_sorted_and_ignores_everything_else() {
        let dir = temp_dir("listing");
        for file in ["Beta.namirpreset", "alpha.namirpreset", "notes.txt"] {
            std::fs::write(dir.join(file), b"{}").expect("write");
        }
        std::fs::create_dir_all(dir.join("Nested.namirpreset")).expect("create dir");

        let found = list_preset_files(&dir);
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Beta", "alpha"], "sorted by name, files only");
        assert_eq!(found[0].1, dir.join("Beta.namirpreset"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_directory_lists_as_empty_rather_than_failing() {
        assert!(list_preset_files(Path::new("/no/such/preset/directory")).is_empty());
    }
}
