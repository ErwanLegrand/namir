//! D-5.1's role for this crate: "Filesystem locations, config dirs, logging sink, thread
//! priority. The only crate with `#[cfg(target_os)]`." M1 built only the minimal slice
//! `03-implementation-roadmap.md` §5 asked for at that point — D-7.4's denormal-suppression guard,
//! which needs `#[cfg(target_arch)]` rather than `#[cfg(target_os)]` (MXCSR/FPCR access differs by
//! CPU architecture, not by operating system, so it never exercised the `target_os` carve-out
//! D-5.1's table names this crate for).
//!
//! **M6 brings this crate to D-13.2/D-13.3's full scope** (`03-implementation-roadmap.md` §10):
//! [`paths`] (config directory, log sink) and [`clap_paths`] (D-13.3's CLAP search-path table,
//! confirmed by S-4 to matter: Reaper silently ignores the naive `%APPDATA%` location) both land
//! now; thread-priority elevation follows in the same module doc-comment style.

mod clap_paths;
mod denormal;
mod paths;

pub use clap_paths::{ClapInstallScope, clap_install_dir};
pub use denormal::DenormalGuard;
pub use paths::{config_dir, log_file_path};
