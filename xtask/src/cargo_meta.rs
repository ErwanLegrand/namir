//! Wraps `cargo metadata` (via `cargo_metadata`) to produce the real, structured normal-
//! dependency edges [`crate::layering::check_edges`] checks. Kept separate from `layering.rs` so
//! that module's checking logic stays testable against synthetic data with no `cargo metadata`
//! invocation involved (D-5.2's check is the logic in `layering.rs`; this module is just where
//! real data comes from).

use std::path::Path;

use cargo_metadata::{DependencyKind, MetadataCommand};

/// Runs `cargo metadata` against the workspace `manifest_dir` belongs to and returns every
/// normal-dependency edge `(from, to)` between two workspace-member crates whose names both
/// start with `namir-`. Dev- and build-dependencies are excluded by construction (D-5.2 only
/// governs what ships) — `namir-nam`'s dev-dependency on `namir-fixtures` never reaches this
/// list.
pub fn normal_namir_edges(manifest_dir: &Path) -> Result<Vec<(String, String)>, String> {
    let metadata = MetadataCommand::new()
        .manifest_path(manifest_dir.join("Cargo.toml"))
        .no_deps()
        .exec()
        .map_err(|e| format!("cargo metadata failed: {e}"))?;

    let mut edges = Vec::new();
    for package in &metadata.packages {
        if !metadata.workspace_members.contains(&package.id) || !package.name.starts_with("namir-")
        {
            continue;
        }
        for dep in &package.dependencies {
            if dep.kind != DependencyKind::Normal || !dep.name.starts_with("namir-") {
                continue;
            }
            edges.push((package.name.clone(), dep.name.clone()));
        }
    }
    Ok(edges)
}
