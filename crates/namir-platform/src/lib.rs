//! D-5.1's role for this crate: "Filesystem locations, config dirs, logging sink, thread
//! priority. The only crate with `#[cfg(target_os)]`." M1 built only the minimal slice
//! `03-implementation-roadmap.md` §5 asked for at that point — D-7.4's denormal-suppression guard,
//! which needs `#[cfg(target_arch)]` rather than `#[cfg(target_os)]` (MXCSR/FPCR access differs by
//! CPU architecture, not by operating system, so it never exercised the `target_os` carve-out
//! D-5.1's table names this crate for).
//!
//! **M6 brings this crate to D-13.2/D-13.3's full scope** (`03-implementation-roadmap.md` §10):
//! [`paths`] (config directory, log sink), [`clap_paths`] (D-13.3's CLAP search-path table), and
//! [`thread_priority`] (OS scheduling-priority elevation). All three modules only *compute*
//! platform facts (a path, an elevation outcome) — none of them do I/O, none of them get wired
//! into an audio callback here. That wiring is explicitly **not** this crate's job: D-7.4's own
//! `DenormalGuard` and this milestone's `thread_priority::elevate_current_thread_priority` are
//! both meant to be acquired from inside `namir-app`'s `cpal` stream callback and `namir-clap`'s
//! `process()`-adjacent activation path — crates that do not exist yet as of this round. See each
//! module's own doc comment for the specifics of what it provides and, for `thread_priority`,
//! exactly when and how a future caller should invoke it.
//!
//! This is one of only two crates in the workspace (with `namir-clap`) D-5.3 permits to carry
//! `unsafe` at all, and the only one with the `#[cfg(target_os)]` carve-out D-5.2's layering lint
//! enforces (`xtask/src/layering.rs`'s `scan_platform_cfg`). Both carve-outs now have more than
//! one user: [`denormal`] for the first, [`thread_priority`] for the second alongside `denormal`'s
//! own `#[cfg(target_arch)]` (unsafe only), and [`paths`]/[`clap_paths`] for `#[cfg(target_os)]`
//! specifically (no `unsafe` — see each module for why).

mod clap_paths;
mod denormal;
mod paths;
mod thread_priority;

pub use clap_paths::{ClapInstallScope, clap_install_dir};
pub use denormal::DenormalGuard;
pub use paths::{config_dir, log_file_path};
pub use thread_priority::{ThreadPriorityOutcome, elevate_current_thread_priority};
