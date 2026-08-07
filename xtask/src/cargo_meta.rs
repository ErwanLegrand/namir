//! Wraps `cargo metadata` (via `cargo_metadata`) to produce the real, structured normal-
//! dependency edges [`crate::layering::check_edges`] checks, and (M7) the real third-party
//! dependency set [`crate::attribution::render`] renders. Kept separate from `layering.rs`/
//! `attribution.rs` so those modules' logic stays testable against synthetic data with no
//! `cargo metadata` invocation involved (the checks/rendering live in those modules; this module
//! is just where real data comes from).

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Package};

use crate::attribution::ThirdPartyDep;

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

/// NFR-LIC-030: "every dependency [...] shipped with the binaries." Runs the full (non-`--no-deps`)
/// `cargo metadata` resolve and returns every third-party package reachable from `namir-app` and
/// `namir-clap` -- the two shipped products -- by a path where **every** edge carries at least one
/// `DependencyKind::Normal` `dep_kinds` entry. This deliberately excludes dev-only and build-only
/// dependency subtrees (e.g. `namir-fixtures`, `assert_no_alloc`, `xtask` itself): they are never
/// linked into a release binary, so listing them would overstate what NFR-LIC-030 asks for.
/// Workspace members (`namir-*`, `xtask`) are excluded from the returned list -- this crate's own
/// code isn't a "dependency" to attribute.
pub fn third_party_runtime_dependencies(manifest_dir: &Path) -> Result<Vec<ThirdPartyDep>, String> {
    let metadata = MetadataCommand::new()
        .manifest_path(manifest_dir.join("Cargo.toml"))
        .exec()
        .map_err(|e| format!("cargo metadata failed: {e}"))?;

    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("cargo metadata returned no resolve graph (did --no-deps leak in?)")?;

    let mut roots = Vec::new();
    for root_name in ["namir-app", "namir-clap"] {
        let id = metadata
            .packages
            .iter()
            .find(|p| p.name == root_name && metadata.workspace_members.contains(&p.id))
            .map(|p| p.id.clone())
            .ok_or_else(|| format!("workspace member '{root_name}' not found in cargo metadata"))?;
        roots.push(id);
    }

    let mut visited: HashSet<cargo_metadata::PackageId> = HashSet::new();
    let mut queue: VecDeque<cargo_metadata::PackageId> = roots.into_iter().collect();
    while let Some(id) = queue.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(node) = resolve.nodes.iter().find(|n| n.id == id) else {
            continue;
        };
        for dep in &node.deps {
            let is_normal = dep.dep_kinds.is_empty()
                || dep
                    .dep_kinds
                    .iter()
                    .any(|dk| dk.kind == DependencyKind::Normal);
            if is_normal && !visited.contains(&dep.pkg) {
                queue.push_back(dep.pkg.clone());
            }
        }
    }

    Ok(third_party_packages(&metadata, &visited))
}

fn third_party_packages(
    metadata: &Metadata,
    visited: &HashSet<cargo_metadata::PackageId>,
) -> Vec<ThirdPartyDep> {
    let mut out: Vec<ThirdPartyDep> = metadata
        .packages
        .iter()
        .filter(|p| visited.contains(&p.id) && !metadata.workspace_members.contains(&p.id))
        .map(ThirdPartyDep::from)
        .collect();
    out.sort();
    out
}

impl From<&Package> for ThirdPartyDep {
    fn from(p: &Package) -> Self {
        let license = match (&p.license, &p.license_file) {
            (Some(spdx), _) => spdx.clone(),
            (None, Some(file)) => format!("see {file}"),
            (None, None) => "UNKNOWN".to_string(),
        };
        ThirdPartyDep {
            name: p.name.clone(),
            version: p.version.to_string(),
            license,
        }
    }
}
