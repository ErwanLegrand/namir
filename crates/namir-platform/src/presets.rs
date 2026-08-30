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
//! shared so that a name one product accepts is never one the other refuses. It is held to
//! Windows's naming rules on every platform — illegal characters *and* the reserved device names
//! `CON`/`NUL`/`COM1`/… — because a preset one platform can write and another cannot open is
//! exactly the interchangeability FR-STATE-030 claims, failing quietly. What it deliberately does
//! not cover, and why, is on [`sanitise_name`] itself: names that differ only in case.

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
/// (so a name can never reach a sibling directory), anything that is `.` or `..`, anything
/// containing a character Windows refuses in a filename, and anything Win32 resolves as a device
/// rather than as a file (`names_a_win32_device`, below). The last two are checked on every platform
/// on
/// purpose: a preset saved on Linux under a name Windows cannot represent would be a preset the
/// other half of FR-STATE-030's interchangeability claim cannot open.
///
/// # What this rule does *not* cover
///
/// Two names differing only in case — `Crunch` and `crunch` — are two files on Linux and one file
/// on Windows and on a default-configured macOS. This function cannot see that: it is given a name
/// and no directory, so it has nothing to compare against. Saving `crunch` where `Crunch` already
/// exists therefore silently replaces it on those platforms, and the recall list shows whichever
/// spelling the filesystem kept. Closing that needs a directory listing and a decision about what
/// to do when a collision is found (refuse, or ask the user to confirm an overwrite), both of which
/// belong to the shells' save flow rather than to a naming predicate. Recorded here rather than
/// silently left, so the limit is visible at the function every save goes through.
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
    if names_a_win32_device(name) {
        return None;
    }
    Some(name)
}

/// Whether Win32 would resolve `name` as one of its reserved device names rather than as a file.
///
/// `CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, `COM0`–`COM9` and `LPT0`–`LPT9`, matched
/// case-insensitively against the part of the name *before its first `.`* and ignoring trailing
/// spaces — because that is how Win32 itself resolves them. The extension is irrelevant
/// (`CON.namirpreset` is the console), and so is the directory
/// (`%APPDATA%\Namir\Presets\NUL.namirpreset` is the null device). A save under such a name
/// succeeds against the device, writes nothing to disk, and produces a preset that never appears in
/// the recall list — a silent data loss, which is why the name is refused before a path is built
/// rather than after the write appears to succeed.
///
/// `COM¹`/`COM²`/`COM³` (and the `LPT` equivalents) are included because Windows folds those
/// superscript digits onto `COM1`/`COM2`/`COM3`; they are the one non-ASCII case, and they cost one
/// `matches!` arm rather than an argument about whether anyone would type them.
///
/// Checked on every platform, not behind `#[cfg(windows)]`: this function was hoisted into
/// `namir-platform` precisely so both shells hold one rule, and a preset a Linux user saves under a
/// name Windows cannot open is FR-STATE-030's interchangeability failing in the direction nobody
/// tests for.
fn names_a_win32_device(name: &str) -> bool {
    // Win32 stops at the first '.' and ignores trailing spaces, so `CON.old` and `CON ` are both
    // the console. `split('.')` always yields at least one item, so the `unwrap_or` is unreachable
    // and present only to keep this total.
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
    {
        return true;
    }
    let mut chars = stem.chars();
    let (Some(c0), Some(c1), Some(c2), Some(c3), None) = (
        chars.next(),
        chars.next(),
        chars.next(),
        chars.next(),
        chars.next(),
    ) else {
        return false;
    };
    let com = c0.eq_ignore_ascii_case(&'C')
        && c1.eq_ignore_ascii_case(&'O')
        && c2.eq_ignore_ascii_case(&'M');
    let lpt = c0.eq_ignore_ascii_case(&'L')
        && c1.eq_ignore_ascii_case(&'P')
        && c2.eq_ignore_ascii_case(&'T');
    (com || lpt) && matches!(c3, '0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}')
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

    /// Win32 resolves a reserved device stem before it ever reaches the filesystem, whatever the
    /// extension and whatever the directory, so `CON.namirpreset` opens the console rather than
    /// creating a file. A save against one of these reports success and leaves nothing behind, and
    /// the preset never appears in the recall list. Checked on every platform for the same reason
    /// the illegal-character set is: a name Linux accepts and Windows cannot represent breaks
    /// FR-STATE-030's interchangeability.
    #[test]
    fn a_name_windows_resolves_as_a_device_is_refused_on_every_platform() {
        for name in [
            "CON",
            "con",
            "Con",
            "NUL",
            "nul",
            "PRN",
            "aux",
            "COM1",
            "com9",
            "COM0",
            "LPT1",
            "lpt9",
            "LPT0",
            "CONIN$",
            "conout$",
            "CON.old",
            "com1.backup",
            "nul   ",
            "  NUL  ",
            "CON.namirpreset",
        ] {
            assert_eq!(sanitise_name(name), None, "{name:?} must be refused");
            assert_eq!(preset_path(Path::new("/presets"), name), None, "{name:?}");
        }

        // Near misses that are ordinary names and must still be accepted -- the rule is the whole
        // stem, not a prefix.
        for name in [
            "CONTROL",
            "COM",
            "COM10",
            "COMA",
            "Console",
            "NULL",
            "Crunch",
            "LPT",
            "my CON",
            "CON2",
            "AUXILIARY",
        ] {
            assert!(
                sanitise_name(name).is_some(),
                "{name:?} is not a device name and must be accepted"
            );
        }
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
