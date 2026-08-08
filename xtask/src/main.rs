//! Dev-tooling binary (never shipped in a release product binary). Implements the two CI checks
//! `docs/03-implementation-roadmap.md` §5 (M1) assigns to this milestone that don't yet have a
//! home: D-5.2's layering lint and FR-PARAM-020's `params.lock` diff. `xtask` consumes
//! `namir-params` as a path dependency to reach its manifest-render function; that is tooling
//! consuming a product crate's public API, not a product edge, so `xtask` itself sits outside
//! D-5.1's layering table (see `layering.rs`'s module doc).

mod attribution;
mod cargo_meta;
mod layering;
mod params_lock;
mod preset;
mod traceability;

use std::collections::HashMap;
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

fn run_attribution(root: &Path, write: bool) -> bool {
    match attribution::check_or_write(root, write) {
        Ok((ok, message)) => {
            println!("attribution: {message}");
            ok
        }
        Err(e) => {
            println!("attribution: could not run check: {e}");
            false
        }
    }
}

/// Real data for NFR-QUAL-010's traceability check: every `.rs` file under `dir` (skipping
/// `target/` directories entirely, not just filtering their contents afterward -- a shipped
/// `cargo build`'s `target/` can hold tens of thousands of files, and descending into it here
/// would make this check needlessly slow). `dir`'s own top-level child directory name is
/// hard-coded per caller as the "crate name" for every file beneath it, since that's the
/// granularity `docs/02-architecture.md` §23's M7 Consequence note commits to.
fn walk_rs_files(dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| e.file_name() != "target")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn run_traceability(root: &Path, write: bool) -> bool {
    let frs_path = root.join("docs/01-functional-requirements.md");
    let frs_text = match std::fs::read_to_string(&frs_path) {
        Ok(t) => t,
        Err(e) => {
            println!("traceability: could not read {}: {e}", frs_path.display());
            return false;
        }
    };
    let requirements = match traceability::parse_must_requirements(&frs_text) {
        Ok(r) => r,
        Err(e) => {
            println!("traceability: could not parse FRS: {e}");
            return false;
        }
    };

    let manual_tests_dir = root.join("docs/manual-tests");
    let mut manual_test_docs: Vec<(String, String)> = std::fs::read_dir(&manual_tests_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let content = std::fs::read_to_string(e.path()).ok()?;
                    Some((name, content))
                })
                .collect()
        })
        .unwrap_or_default();
    // `std::fs::read_dir`'s order is filesystem-dependent, not guaranteed stable across platforms
    // -- sorted here so `build_report`'s `.find()` (for an id matched by more than one manual-test
    // file's content) picks the same file on every OS. Without this, this function's own output
    // could differ byte-for-byte between a local Windows run and Linux CI, which is exactly the
    // "stale" false-positive this session found the hard way.
    manual_test_docs.sort_by(|a, b| a.0.cmp(&b.0));

    // (crate_root, crate_name-per-first-path-component) -- xtask has no further nesting, so its
    // own directory name is used directly rather than derived per file.
    let mut files_with_crate: Vec<(PathBuf, String)> = Vec::new();
    let crates_dir = root.join("crates");
    for file in walk_rs_files(&crates_dir) {
        if let Ok(rel) = file.strip_prefix(&crates_dir)
            && let Some(crate_name) = rel.components().next()
        {
            files_with_crate.push((
                file.clone(),
                crate_name.as_os_str().to_string_lossy().into_owned(),
            ));
        }
    }
    for file in walk_rs_files(&root.join("xtask")) {
        files_with_crate.push((file, "xtask".to_string()));
    }

    // A real, non-trivial slice of Must requirements are verified entirely by CI workflow or
    // build configuration (MSRV, clippy-as-error, cargo-deny, mobile/no-C++ builds, network-free)
    // rather than by any Rust test function -- `# trace:` in these files is how they become
    // discoverable at all. Crate name "ci" for workflow files, "workspace" for root-level build
    // configuration, since neither is owned by any one product crate's test suite.
    for name in ["ci.yml", "fuzz.yml"] {
        let path = root.join(".github/workflows").join(name);
        if path.is_file() {
            files_with_crate.push((path, "ci".to_string()));
        }
    }
    for name in ["Cargo.toml", "deny.toml"] {
        let path = root.join(name);
        if path.is_file() {
            files_with_crate.push((path, "workspace".to_string()));
        }
    }

    let mut source_hits: HashMap<String, Vec<String>> = HashMap::new();
    for (file, crate_name) in &files_with_crate {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        for id in traceability::trace_annotations(&source) {
            source_hits.entry(id).or_default().push(crate_name.clone());
        }
        for req in &requirements {
            if req.verify == 'M' || source_hits.contains_key(&req.id) {
                continue;
            }
            if traceability::fn_name_embeds_id(&source, &req.id) {
                source_hits
                    .entry(req.id.clone())
                    .or_default()
                    .push(crate_name.clone());
            }
        }
    }

    let report = traceability::build_report(&requirements, &manual_test_docs, &source_hits);
    let test_plan_path = root.join("docs/03-test-plan.md");
    let expected = traceability::render_test_plan(&requirements, &report);

    let plan_up_to_date = if write {
        if let Err(e) = std::fs::write(&test_plan_path, &expected) {
            println!(
                "traceability: failed to write {}: {e}",
                test_plan_path.display()
            );
            return false;
        }
        println!("traceability: wrote {}", test_plan_path.display());
        true
    } else {
        // Compares with CRLF/LF normalized away on both sides: `.gitattributes`' `eol=lf` should
        // already guarantee an LF checkout on every platform, but this check has no reason to be
        // sensitive to line-ending representation specifically (NFR-PORT-050's own spirit) when
        // the only thing that actually matters is the text content.
        match std::fs::read_to_string(&test_plan_path) {
            Ok(actual) if actual.replace("\r\n", "\n") == expected.replace("\r\n", "\n") => {
                println!("traceability: {} is up to date", test_plan_path.display());
                true
            }
            Ok(actual) => {
                let actual_lines: std::collections::HashSet<&str> = actual.lines().collect();
                let expected_lines: std::collections::HashSet<&str> = expected.lines().collect();
                let extra = actual_lines.difference(&expected_lines).count();
                let missing = expected_lines.difference(&actual_lines).count();
                println!(
                    "traceability: {} is stale -- {extra} line(s) present only in the checked-in \
                     file, {missing} line(s) only in the freshly generated one. Run `cargo run -p \
                     xtask -- traceability --write` to regenerate it",
                    test_plan_path.display()
                );
                false
            }
            Err(e) => {
                println!(
                    "traceability: could not read {}: {e} -- run `cargo run -p xtask -- \
                     traceability --write` to generate it",
                    test_plan_path.display()
                );
                false
            }
        }
    };

    let coverage_clean = if report.missing.is_empty() {
        println!(
            "traceability: clean -- all {} Must requirements are covered",
            requirements.len()
        );
        true
    } else {
        println!(
            "traceability: {} Must requirement(s) with no coverage found (NFR-QUAL-010):",
            report.missing.len()
        );
        for req in &report.missing {
            println!("  - {} (Verify: {})", req.id, req.verify);
        }
        false
    };

    plan_up_to_date && coverage_clean
}

fn print_usage() {
    println!(
        "usage: cargo run -p xtask -- <layering|params-lock [--write]|attribution [--write]|traceability [--write]|preset [output-path]|preset --verify <path>>"
    );
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
        Some("attribution") => {
            let write = args.iter().skip(1).any(|a| a == "--write");
            run_attribution(&root, write)
        }
        Some("traceability") => {
            let write = args.iter().skip(1).any(|a| a == "--write");
            run_traceability(&root, write)
        }
        Some("preset") => preset::run(&args[1..]),
        _ => {
            print_usage();
            std::process::exit(2);
        }
    };

    if !ok {
        std::process::exit(1);
    }
}
