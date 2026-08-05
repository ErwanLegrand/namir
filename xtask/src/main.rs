//! Dev-tooling binary (never shipped in a release product binary). Implements the two CI checks
//! `docs/03-implementation-roadmap.md` §5 (M1) assigns to this milestone that don't yet have a
//! home: D-5.2's layering lint and FR-PARAM-020's `params.lock` diff. `xtask` consumes
//! `namir-params` as a path dependency to reach its manifest-render function; that is tooling
//! consuming a product crate's public API, not a product edge, so `xtask` itself sits outside
//! D-5.1's layering table (see `layering.rs`'s module doc).

mod cargo_meta;
mod layering;
mod params_lock;

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // `xtask` lives at `<repo_root>/xtask`, one level below the workspace root, regardless of
    // the shell's current directory when `cargo run -p xtask` is invoked.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask's manifest dir always has a parent")
        .to_path_buf()
}

fn run_layering(root: &Path) -> bool {
    let mut violations = Vec::new();

    match cargo_meta::normal_namir_edges(root) {
        Ok(edges) => violations.extend(layering::check_edges(&edges)),
        Err(e) => violations.push(format!("dependency-graph check could not run: {e}")),
    }

    violations.extend(scan_repo_for_platform_cfg(root));

    if violations.is_empty() {
        println!(
            "layering: clean (dependency graph matches D-5.1; no platform cfg outside namir-platform)"
        );
        true
    } else {
        println!("layering: {} violation(s) found (D-5.2):", violations.len());
        for v in &violations {
            println!("  - {v}");
        }
        false
    }
}

/// Walks every `.rs` file under every `crates/*/src`, excluding
/// `crates/namir-platform/src` (D-5.1's one carve-out), for D-5.2(b)'s platform-cfg lint.
fn scan_repo_for_platform_cfg(root: &Path) -> Vec<String> {
    let crates_dir = root.join("crates");
    let mut violations = Vec::new();

    let entries = match std::fs::read_dir(&crates_dir) {
        Ok(entries) => entries,
        Err(e) => {
            return vec![format!("could not read {}: {e}", crates_dir.display())];
        }
    };

    for entry in entries.flatten() {
        let crate_dir = entry.path();
        if !crate_dir.is_dir() {
            continue;
        }
        let crate_name = entry.file_name();
        if crate_name == layering::PLATFORM_CFG_EXEMPT_CRATE {
            continue;
        }
        let src_dir = crate_dir.join("src");
        if !src_dir.is_dir() {
            continue;
        }

        for file in walkdir::WalkDir::new(&src_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
        {
            let path = file.path();
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    violations.push(format!("could not read {}: {e}", path.display()));
                    continue;
                }
            };
            for (line, pattern) in layering::scan_platform_cfg(&content) {
                violations.push(format!(
                    "{}:{line}: contains '{pattern}' (only namir-platform may carry platform cfg)",
                    path.display()
                ));
            }
        }
    }

    violations
}

fn run_params_lock(root: &Path, write: bool) -> bool {
    match params_lock::check_or_write(root, write) {
        Ok((ok, message)) => {
            println!("params-lock: {message}");
            ok
        }
        Err(e) => {
            println!("params-lock: could not run check: {e}");
            false
        }
    }
}

fn print_usage() {
    println!("usage: cargo run -p xtask -- <layering|params-lock [--write]>");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repo_root();

    let ok = match args.first().map(String::as_str) {
        Some("layering") => run_layering(&root),
        Some("params-lock") => {
            let write = args.iter().skip(1).any(|a| a == "--write");
            run_params_lock(&root, write)
        }
        _ => {
            print_usage();
            std::process::exit(2);
        }
    };

    if !ok {
        std::process::exit(1);
    }
}
