//! FR-STATE-030's named-preset half for the plugin: the one line of the preset rule that is
//! legitimately per-shell.
//!
//! Where a preset lives, what it is called and which names are legal are
//! [`namir_platform::presets`]'s, so that this plugin and `namir-app` agree by construction rather
//! than by two copies happening to match — FR-STATE-030's "interchangeable between the two
//! products" fails at *discovery* if they do not. What stays here is the mapping into
//! [`namir_ui::PresetSummary`], which `namir-platform` cannot build: D-5.1 lets it depend on
//! `namir-core` and nothing else.

use std::path::Path;

use namir_ui::PresetSummary;

pub(crate) use namir_platform::presets::{preset_dir, preset_path};

/// Every `.namirpreset` in `dir` as the interface's own summary, named by stem and sorted.
///
/// **Blocking:** reads a directory, so it runs on the worker pool, never inside a GUI frame.
pub(crate) fn list_presets(dir: &Path) -> Vec<PresetSummary> {
    namir_platform::presets::list_preset_files(dir)
        .into_iter()
        .map(|(name, path)| PresetSummary { name, path })
        .collect()
}
