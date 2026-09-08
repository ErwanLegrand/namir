//! Preset path resolution and listing re-exports for the plugin.

use namir_ui::PresetSummary;
use std::path::Path;

#[cfg(test)]
pub(crate) use namir_platform::presets::preset_dir_under;
pub(crate) use namir_platform::presets::{preset_dir, preset_path};

/// **Blocking:** reads a directory, so it runs on the worker pool/thread, never inside a GUI frame / `UiHost::snapshot`.
pub(crate) fn list_presets(dir: &Path) -> Vec<PresetSummary> {
    PresetSummary::from_pairs(namir_platform::presets::list_preset_files(dir))
}
