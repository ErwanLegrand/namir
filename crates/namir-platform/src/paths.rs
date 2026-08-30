//! D-13.2: "Filesystem locations, config directories, log sinks ... live in `namir-platform` and
//! nowhere else (P5, NFR-PORT-020)." This module is the filesystem-locations/config-dir/log-sink
//! third of that decision; D-13.3's CLAP-specific search paths live in [`crate::clap_paths`]
//! instead, and thread-priority elevation (D-13.2's fourth clause) lives in
//! [`crate::thread_priority`] -- kept apart because each has a distinct provenance and a reader
//! chasing one shouldn't have to wade through the other two.
//!
//! Where Namir's own configuration/state directory and log file live is not specified anywhere
//! in the FRS or `docs/04-state-and-preset-format.md` (checked: both define the *preset document*
//! format FR-STATE-010 governs, not where Namir itself keeps settings) -- D-13.2 names the
//! *responsibility* ("filesystem locations ... live in `namir-platform`") without dictating a
//! path, so this follows each OS's own documented convention for a per-user application data
//! directory rather than inventing one:
//!
//! | Platform | Config directory | Env var(s) consulted |
//! |---|---|---|
//! | Windows | `%APPDATA%\Namir` | `APPDATA` |
//! | macOS | `~/Library/Application Support/Namir` | `HOME` |
//! | Linux (and other Unix) | `$XDG_CONFIG_HOME/namir`, else `~/.config/namir` | `XDG_CONFIG_HOME`, `HOME` |
//!
//! No third-party crate (e.g. `directories`) is taken on for this: the table above is three short
//! env-var lookups, D-13.3's CLAP table below needs its own hand-rolled per-OS logic regardless
//! (its paths don't match any general-purpose "app dirs" abstraction -- `%LOCALAPPDATA%\Programs\
//! Common\CLAP` is not a `directories`-crate-shaped path), and this workspace's adoption bar for a
//! new dependency (set by `rtrb`'s adoption, restated at D-12.3: "zero transitive dependencies, no
//! build script, `no_std`-capable pure Rust, MSRV far below this workspace's own") is met more
//! cheaply by the standard library alone here than by evaluating a new crate against it.
//!
//! Every function here returns [`Option`], never a default or a panic: NFR-PORT-030 already
//! commits this crate to degrading rather than assuming an environment it cannot verify (the same
//! reasoning [`crate::denormal::DenormalGuard`] applies per-architecture, applied here per the
//! handful of env vars an unusual environment might not set). A caller that gets `None` back is
//! expected to fall back to *something* it owns the choice of (an in-memory-only session, a
//! caller-supplied override) rather than this crate silently guessing.

use std::ffi::OsString;
use std::path::PathBuf;

/// The directory Namir stores its own configuration and persistent state in (the library index
/// of D-12.3, the settings a future `namir-app`/`namir-clap` will read/write) -- per this
/// module's own doc comment, `%APPDATA%\Namir` on Windows, `~/Library/Application Support/Namir`
/// on macOS, `$XDG_CONFIG_HOME/namir` (falling back to `~/.config/namir`) elsewhere on Unix.
///
/// Returns `None` if the environment variable(s) this needs are unset, or on a target this crate
/// has no convention for (D-5.1 marks this crate "builds for mobile: yes", and neither Android
/// nor iOS has an equivalent of a `HOME`-relative dotfile directory a sandboxed app can rely on --
/// this crate does not invent one). This function performs no I/O beyond reading environment
/// variables: it computes a path, it does not create the directory the path names. Whoever
/// consumes the path (a future `namir-app`/`namir-clap`, `namir-worker`'s `LibraryService::open`
/// per D-5.3's M5 consequence note) is responsible for creating it if absent, the same "caller
/// supplies/owns the path" discipline `namir-library` already applies to its own index path.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_from(|key| std::env::var_os(key))
}

/// The file Namir's log sink writes to: a `logs` subdirectory of [`config_dir`], so a diagnostic
/// bundle (FR-ERR-050, Should) can package the whole `logs` directory without also sweeping in
/// settings or the library index that live directly under [`config_dir`].
///
/// Returns `None` under exactly the same conditions [`config_dir`] does, for the same reason: a
/// log sink with nowhere well-defined to write is a caller decision (log to stderr, disable
/// logging, prompt for a location), not something this crate should default silently.
///
/// This function computes a path only. It is *not* a logging implementation -- no formatting, no
/// file handle, no writer. FR-ERR-030 forbids logging on the audio thread at all, so whatever
/// eventually opens this path for writing must do so off the audio thread, which is out of this
/// crate's scope and belongs to whichever crate owns the actual log sink (a future milestone).
pub fn log_file_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("logs").join("namir.log"))
}

/// Pure computation behind [`config_dir`], taking an environment-lookup closure instead of
/// calling `std::env::var_os` directly so it can be exercised with a fake environment in tests
/// without mutating the real process environment (`std::env::set_var` needs `unsafe` as of this
/// workspace's edition and is process-global besides, an unnecessary and unsafe complication for
/// what should be a pure function) -- the same "pure, independently testable logic, wired to the
/// real world only at the edge" split `xtask/src/layering.rs`'s `check_edges`/`scan_platform_cfg`
/// already establish for this codebase.
fn config_dir_from(getenv: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let result = getenv("APPDATA").map(|appdata| PathBuf::from(appdata).join("Namir"));

    #[cfg(target_os = "macos")]
    let result =
        getenv("HOME").map(|home| PathBuf::from(home).join("Library/Application Support/Namir"));

    #[cfg(all(unix, not(target_os = "macos")))]
    let result = getenv("XDG_CONFIG_HOME")
        .map(|xdg| PathBuf::from(xdg).join("namir"))
        .or_else(|| getenv("HOME").map(|home| PathBuf::from(home).join(".config/namir")));

    // Same fallback, same reason, same fix as `clap_paths.rs`'s -- see that arm's comment: on a
    // target that is neither Windows nor `unix` this function's one parameter goes unread, and
    // `unused_variables` under CI's `-D warnings` would fail the build on exactly the target the
    // fallback exists to keep building (NFR-PORT-030).
    #[cfg(not(any(target_os = "windows", unix)))]
    let result: Option<PathBuf> = {
        let _ = &getenv;
        None
    };

    result
}

#[cfg(test)]
mod tests {
    // Every test below lives in a per-OS submodule, so on a target with no row in the table above
    // this import has no user and `-D warnings` would fail the test build -- the same defect
    // `config_dir_from`'s own fallback arm carries, one target away. Scoped to the
    // import and to that target, so it cannot hide an unused import anywhere real.
    #[cfg_attr(not(any(target_os = "windows", unix)), allow(unused_imports))]
    use super::*;

    #[cfg(target_os = "windows")]
    mod windows_tests {
        use super::*;

        #[test]
        fn config_dir_joins_appdata_with_namir() {
            let dir = config_dir_from(|key| {
                (key == "APPDATA").then(|| OsString::from(r"C:\Users\alice\AppData\Roaming"))
            });
            assert_eq!(
                dir,
                Some(PathBuf::from(r"C:\Users\alice\AppData\Roaming\Namir"))
            );
        }

        #[test]
        fn config_dir_is_none_without_appdata() {
            let dir = config_dir_from(|_| None);
            assert_eq!(dir, None);
        }

        #[test]
        fn log_file_lives_under_a_logs_subdirectory_of_config_dir() {
            // Exercises the real config_dir() (not the closure-injected version) since
            // log_file_path is defined in terms of it directly -- if APPDATA happens to be unset
            // on the CI runner this degrades to None like everything else here, which the
            // assertion below tolerates explicitly rather than assuming a real environment.
            if let Some(log_path) = log_file_path() {
                assert_eq!(log_path.file_name().unwrap(), "namir.log");
                assert_eq!(log_path.parent().unwrap().file_name().unwrap(), "logs");
            }
        }
    }

    #[cfg(target_os = "macos")]
    mod macos_tests {
        use super::*;

        #[test]
        fn config_dir_joins_home_with_the_application_support_path() {
            let dir =
                config_dir_from(|key| (key == "HOME").then(|| OsString::from("/Users/alice")));
            assert_eq!(
                dir,
                Some(PathBuf::from(
                    "/Users/alice/Library/Application Support/Namir"
                ))
            );
        }

        #[test]
        fn config_dir_is_none_without_home() {
            let dir = config_dir_from(|_| None);
            assert_eq!(dir, None);
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    mod linux_tests {
        use super::*;

        #[test]
        fn config_dir_prefers_xdg_config_home() {
            let dir = config_dir_from(|key| match key {
                "XDG_CONFIG_HOME" => Some(OsString::from("/home/alice/.config")),
                "HOME" => Some(OsString::from("/home/alice")),
                _ => None,
            });
            assert_eq!(dir, Some(PathBuf::from("/home/alice/.config/namir")));
        }

        #[test]
        fn config_dir_falls_back_to_home_dot_config_without_xdg() {
            let dir = config_dir_from(|key| (key == "HOME").then(|| OsString::from("/home/alice")));
            assert_eq!(dir, Some(PathBuf::from("/home/alice/.config/namir")));
        }

        #[test]
        fn config_dir_is_none_without_either_variable() {
            let dir = config_dir_from(|_| None);
            assert_eq!(dir, None);
        }
    }
}
