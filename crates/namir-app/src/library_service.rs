//! Bootstraps `namir_worker::library::LibraryService` (FR-LIB-020) at the one filesystem location
//! `namir_platform::config_dir` computes for this crate: `namir_platform::paths`'s own doc comment
//! names exactly this crate's `LibraryService::open` call as the thing responsible for creating
//! the config directory if it is absent (that module computes a path only, per its own doc
//! comment) — this module is where that happens.
//!
//! **Deliberately minimal for this milestone:** one default library root under the config
//! directory (`<config_dir>/Library`), created on first launch if missing. FR-LIB's Must
//! requirements (scan, persist, search, favourites, next/previous) are all
//! `namir_worker::library::LibraryService`'s and `namir_library`'s own — this module only decides
//! *where* they point. A UI for adding/removing additional roots is a natural follow-up, not built
//! here: nothing in FR-LIB requires more than one root, and `LibraryService::roots` already
//! accepts a `Vec<PathBuf>`, so adding that UI later is additive.

use std::path::{Path, PathBuf};

use namir_worker::library::LibraryService;

/// The index file's path, and the one default root, under `config_dir`.
pub fn bootstrap_paths(config_dir: &Path) -> (PathBuf, Vec<PathBuf>) {
    let index_path = config_dir.join("library-index.json");
    let default_root = config_dir.join("Library");
    (index_path, vec![default_root])
}

/// Opens (or creates) the library at `config_dir`'s default location. Creates the default root
/// directory if it does not exist yet — an empty, freshly-installed library is the ordinary first
/// run, and a scan over a directory that doesn't exist would otherwise report every file as
/// "removed" relative to nothing, which is a confusing first impression for no benefit (P8:
/// degrade gracefully rather than surface a spurious warning on a first launch).
pub fn open(config_dir: &Path) -> (LibraryService, Vec<namir_worker::WorkerError>) {
    let (index_path, roots) = bootstrap_paths(config_dir);
    for root in &roots {
        let _ = std::fs::create_dir_all(root);
    }
    LibraryService::open(index_path, roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-app-library-service-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn bootstrap_paths_places_the_index_and_default_root_under_config_dir() {
        let dir = PathBuf::from("/config/dir");
        let (index_path, roots) = bootstrap_paths(&dir);
        assert_eq!(index_path, dir.join("library-index.json"));
        assert_eq!(roots, vec![dir.join("Library")]);
    }

    /// A first launch (no config directory yet at all) opens cleanly with an empty index and no
    /// warnings, and the default root now exists on disk for a future scan to walk.
    #[test]
    fn opening_on_a_first_launch_creates_the_default_root_and_reports_no_warnings() {
        let dir = temp_dir("first_launch");
        let (service, warnings) = open(&dir);
        assert!(warnings.is_empty());
        assert!(service.snapshot().is_empty());
        assert!(dir.join("Library").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
