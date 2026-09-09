//! Preset path resolution and listing re-exports for the standalone application.

use namir_ui::PresetSummary;
use std::path::Path;

pub use namir_platform::presets::{preset_dir_under, preset_path};

/// Every `.namirpreset` in `dir` as the interface's own summary, named by stem and sorted.
#[must_use]
pub fn list_presets(dir: &Path) -> Vec<PresetSummary> {
    PresetSummary::from_pairs(namir_platform::presets::list_preset_files(dir))
}
