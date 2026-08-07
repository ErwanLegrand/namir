//! D-13.3: "The CLAP plugin installs to the **CLAP-specified search paths only**, and the
//! per-user path is the default." This module computes those paths; it does not install anything
//! to them (D-5.1's M6 deliverable list scopes this crate to "the default per this table, no
//! elevation needed" for the per-user path and "just return the path, don't attempt privileged
//! install logic" for the system-wide one -- an actual installer is out of scope here).
//!
//! *Why this table exists at all, cited the way this codebase cites its own decision provenance:*
//! S-4 (`spikes/s4-clack-clap`, `docs/02-architecture.md` §19) found, empirically and at the cost
//! of a failed first attempt, that Reaper does **not** scan `%APPDATA%\REAPER\UserPlugins\CLAP` --
//! a plugin placed there fails *silently*, never appearing in the host with no diagnostic
//! anywhere. The paths below are the CLAP-specification-defined ones, confirmed working in that
//! same spike. Getting this wrong is not a cosmetic bug: "plugin does not appear in my host" with
//! no error to search for is, per D-13.3's own *Consequence*, expected to be the single most
//! common support request this project gets if these paths are ever wrong.
//!
//! | Platform | Per-user (default) | System-wide (opt-in, needs elevation) |
//! |---|---|---|
//! | Windows | `%LOCALAPPDATA%\Programs\Common\CLAP` | `%COMMONPROGRAMFILES%\CLAP` |
//! | macOS | `~/Library/Audio/Plug-Ins/CLAP` | `/Library/Audio/Plug-Ins/CLAP` |
//! | Linux | `~/.clap` | `/usr/lib/clap` |
//!
//! Note the per-user Windows path is `%LOCALAPPDATA%\Programs\Common\CLAP`, not `%APPDATA%\...` --
//! the exact distinction S-4's failed attempt hinged on. It is also a *different* environment
//! variable, and a different subtree, from [`crate::paths::config_dir`]'s `%APPDATA%\Namir`: the
//! two must not be confused or merged into one lookup, since they answer different questions
//! (where Namir keeps its own settings, versus where the CLAP specification says a host will look
//! for plugins).

use std::ffi::OsString;
use std::path::PathBuf;

/// Which of D-13.3's two search paths to resolve. The per-user path is the default per D-13.3's
/// own wording ("the per-user path is the default") precisely because it needs no elevated
/// privileges to write to; the system-wide path is opt-in and, per D-13.3's *Consequence*,
/// resolving it here is only ever "return the path" -- actually writing to it is a future
/// installer's problem, not this crate's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClapInstallScope {
    /// `%LOCALAPPDATA%\Programs\Common\CLAP` / `~/Library/Audio/Plug-Ins/CLAP` / `~/.clap`.
    /// Writable without administrator/root privileges. This is the default a plugin build should
    /// install to unless the user explicitly opts into [`ClapInstallScope::SystemWide`].
    PerUser,
    /// `%COMMONPROGRAMFILES%\CLAP` / `/Library/Audio/Plug-Ins/CLAP` / `/usr/lib/clap`. Requires
    /// administrator/root privileges to write to on every platform in D-13.3's table. This crate
    /// only computes the path; it performs no privilege escalation and no write.
    SystemWide,
}

/// Resolves D-13.3's table for the given [`ClapInstallScope`] on the current platform.
///
/// Returns `None` only for [`ClapInstallScope::PerUser`] on Windows or macOS, where the path
/// depends on an environment variable (`%LOCALAPPDATA%` / `HOME`) this process cannot always
/// observe -- matching [`crate::paths::config_dir`]'s "degrade rather than guess" contract. Every
/// system-wide path, and Linux's per-user path once `HOME` is known, is a fixed, literal path per
/// D-13.3's table and does not vary by installation.
///
/// This function performs no I/O: it does not check whether the returned directory exists, does
/// not create it, and does not install anything into it. A future installer (out of this
/// milestone's scope, per D-5.1's M6 deliverable list) is the intended caller for writing a
/// `.clap` bundle there; a future `namir-app`/`namir-clap` settings screen is the intended caller
/// for *displaying* these paths (D-13.3's *Consequence*: they must be stated explicitly in
/// build/user documentation, and FR-ERR-050's diagnostic bundle should record which of them exist
/// and what they contain).
pub fn clap_install_dir(scope: ClapInstallScope) -> Option<PathBuf> {
    clap_install_dir_from(scope, |key| std::env::var_os(key))
}

/// Pure computation behind [`clap_install_dir`]; see [`crate::paths::config_dir_from`]'s doc
/// comment (same crate, same rationale) for why this takes an injectable environment lookup
/// instead of calling `std::env::var_os` directly.
fn clap_install_dir_from(
    scope: ClapInstallScope,
    getenv: impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let result = match scope {
        ClapInstallScope::PerUser => getenv("LOCALAPPDATA").map(|local| {
            PathBuf::from(local)
                .join("Programs")
                .join("Common")
                .join("CLAP")
        }),
        ClapInstallScope::SystemWide => {
            getenv("COMMONPROGRAMFILES").map(|common| PathBuf::from(common).join("CLAP"))
        }
    };

    #[cfg(target_os = "macos")]
    let result = match scope {
        ClapInstallScope::PerUser => {
            getenv("HOME").map(|home| PathBuf::from(home).join("Library/Audio/Plug-Ins/CLAP"))
        }
        ClapInstallScope::SystemWide => Some(PathBuf::from("/Library/Audio/Plug-Ins/CLAP")),
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    let result = match scope {
        ClapInstallScope::PerUser => getenv("HOME").map(|home| PathBuf::from(home).join(".clap")),
        ClapInstallScope::SystemWide => Some(PathBuf::from("/usr/lib/clap")),
    };

    #[cfg(not(any(target_os = "windows", unix)))]
    let result: Option<PathBuf> = None;

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    mod windows_tests {
        use super::*;

        #[test]
        fn per_user_is_localappdata_programs_common_clap() {
            let dir = clap_install_dir_from(ClapInstallScope::PerUser, |key| {
                (key == "LOCALAPPDATA").then(|| OsString::from(r"C:\Users\alice\AppData\Local"))
            });
            assert_eq!(
                dir,
                Some(PathBuf::from(
                    r"C:\Users\alice\AppData\Local\Programs\Common\CLAP"
                ))
            );
        }

        #[test]
        fn per_user_is_not_appdata_the_path_s4_found_reaper_ignores() {
            // The specific regression S-4 exists to prevent: a lookup keyed on the wrong env var
            // (`APPDATA` instead of `LOCALAPPDATA`) would silently resolve to
            // `%APPDATA%\...\CLAP`, exactly the location Reaper was found not to scan.
            let dir = clap_install_dir_from(ClapInstallScope::PerUser, |key| match key {
                "APPDATA" => Some(OsString::from(r"C:\Users\alice\AppData\Roaming")),
                _ => None,
            });
            assert_eq!(dir, None, "must not fall back to APPDATA");
        }

        #[test]
        fn system_wide_is_commonprogramfiles_clap() {
            let dir = clap_install_dir_from(ClapInstallScope::SystemWide, |key| {
                (key == "COMMONPROGRAMFILES")
                    .then(|| OsString::from(r"C:\Program Files\Common Files"))
            });
            assert_eq!(
                dir,
                Some(PathBuf::from(r"C:\Program Files\Common Files\CLAP"))
            );
        }

        #[test]
        fn per_user_is_none_without_localappdata() {
            let dir = clap_install_dir_from(ClapInstallScope::PerUser, |_| None);
            assert_eq!(dir, None);
        }
    }

    #[cfg(target_os = "macos")]
    mod macos_tests {
        use super::*;

        #[test]
        fn per_user_is_home_library_audio_plug_ins_clap() {
            let dir = clap_install_dir_from(ClapInstallScope::PerUser, |key| {
                (key == "HOME").then(|| OsString::from("/Users/alice"))
            });
            assert_eq!(
                dir,
                Some(PathBuf::from("/Users/alice/Library/Audio/Plug-Ins/CLAP"))
            );
        }

        #[test]
        fn system_wide_is_the_fixed_root_library_path() {
            let dir = clap_install_dir_from(ClapInstallScope::SystemWide, |_| None);
            assert_eq!(dir, Some(PathBuf::from("/Library/Audio/Plug-Ins/CLAP")));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    mod linux_tests {
        use super::*;

        #[test]
        fn per_user_is_home_dot_clap() {
            let dir = clap_install_dir_from(ClapInstallScope::PerUser, |key| {
                (key == "HOME").then(|| OsString::from("/home/alice"))
            });
            assert_eq!(dir, Some(PathBuf::from("/home/alice/.clap")));
        }

        #[test]
        fn system_wide_is_usr_lib_clap() {
            let dir = clap_install_dir_from(ClapInstallScope::SystemWide, |_| None);
            assert_eq!(dir, Some(PathBuf::from("/usr/lib/clap")));
        }
    }
}
