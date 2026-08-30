//! FR-STATE-030's named-preset half for the standalone application: the one line of the preset
//! rule that is legitimately per-shell.
//!
//! Where a preset lives, what it is called and which names are legal are
//! [`namir_platform::presets`]'s, so that the two products agree by construction rather than by
//! two copies happening to match — FR-STATE-030's "interchangeable between the two products"
//! fails at *discovery* if they do not, which is the same failure `LibraryService::open_default`
//! exists to prevent. What stays here is the mapping into [`namir_ui::PresetSummary`], which
//! `namir-platform` cannot build: D-5.1 lets it depend on `namir-core` and nothing else.

use std::path::Path;

use namir_ui::PresetSummary;

pub use namir_platform::presets::{preset_dir_under, preset_path};

/// Every `.namirpreset` in `dir` as the interface's own summary, named by stem and sorted.
///
/// **Blocking:** reads a directory, so it runs on [`crate::worker`]'s thread, never inside
/// [`namir_ui::UiHost::snapshot`].
#[must_use]
pub fn list_presets(dir: &Path) -> Vec<PresetSummary> {
    namir_platform::presets::list_preset_files(dir)
        .into_iter()
        .map(|(name, path)| PresetSummary { name, path })
        .collect()
}
