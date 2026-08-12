//! Dev-tooling binary (never shipped in a release product binary). Implements the two CI checks
//! `docs/03-implementation-roadmap.md` §5 (M1) assigns to this milestone that don't yet have a
//! home: D-5.2's layering lint and FR-PARAM-020's `params.lock` diff. `xtask` consumes
//! `namir-params` as a path dependency to reach its manifest-render function; that is tooling
//! consuming a product crate's public API, not a product edge, so `xtask` itself sits outside
//! D-5.1's layering table (see `layering.rs`'s module doc).

mod attribution;
mod bundle;
mod cargo_meta;
mod feature_guard;
mod identity;
mod layering;
mod milestones;
mod nam_parity;
mod params_lock;
mod preset;
// M13: FR-PKG-010's in-repo assertion over `.github/workflows/release.yml`
// (`docs/03-implementation-roadmap.md` §15 item 10, resolved at M13's start). `#[cfg(test)]`
// because it is exactly a test and nothing else: it adds no subcommand -- FRS §10 admits "an
// annotated test **or** `xtask` subcommand" and a test is the cheaper of the two here -- so
// compiling its checks into the shipped `xtask` binary would leave dead code behind a `-D warnings`
// gate for no gain. `xtask traceability` scans files, not compiled items, so the annotation in it
// resolves either way.
#[cfg(test)]
mod release_workflow;
mod rt_logging;
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

/// Walks every `.rs` file under every crate — **not** `crates/*/src` alone — plus every crate's
/// own `Cargo.toml` and the workspace root manifest, excluding `crates/namir-platform` (D-5.1's one
/// carve-out), for D-5.2(b)'s platform-cfg lint.
///
/// M14 widened the scanned set with the pattern set (`layering::scan_platform_cfg`). Restricting it
/// to `src` exempted every test, bench and example in the tree from a Must-level portability lint,
/// and `[target.'cfg(...)']` manifest tables — a platform conditional expressed in TOML — were
/// outside it entirely. `target/` is skipped by name rather than filtered afterwards, for the same
/// reason `walk_rs_files` skips it.
fn scan_repo_for_platform_cfg(root: &Path) -> Vec<String> {
    let crates_dir = root.join("crates");
    let mut violations = Vec::new();

    let entries = match std::fs::read_dir(&crates_dir) {
        Ok(entries) => entries,
        Err(e) => {
            return vec![format!("could not read {}: {e}", crates_dir.display())];
        }
    };

    let mut manifests = vec![root.join("Cargo.toml")];

    for entry in entries.flatten() {
        let crate_dir = entry.path();
        if !crate_dir.is_dir() {
            continue;
        }
        if entry.file_name() == layering::PLATFORM_CFG_EXEMPT_CRATE {
            continue;
        }
        manifests.push(crate_dir.join("Cargo.toml"));

        for file in walkdir::WalkDir::new(&crate_dir)
            .into_iter()
            .filter_entry(|e| e.file_name() != "target")
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
            for (line, key) in layering::scan_platform_cfg(&content) {
                violations.push(format!(
                    "{}:{line}: names the platform cfg predicate '{key}' (only namir-platform may \
                     carry platform conditionals)",
                    path.display()
                ));
            }
        }
    }

    for manifest in manifests {
        let Ok(content) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        for (line, header) in layering::scan_cargo_target_tables(&content) {
            violations.push(format!(
                "{}:{line}: declares '{header}' (only namir-platform may take a \
                 platform-conditional dependency)",
                manifest.display()
            ));
        }
    }

    violations
}

/// M9b's FR-ERR-030 gate: no audio-thread module in `namir-app`/`namir-clap` may name
/// `namir-platform`'s logger. See `rt_logging.rs`'s module doc for why the ban is module-scoped,
/// why file granularity is the honest granularity here, and what it cannot see.
///
/// A listed file that cannot be read is a violation rather than a skip: the list is hand-maintained
/// (`rt_logging::AUDIO_THREAD_MODULES`), so a module that was renamed or moved must fail this gate
/// loudly instead of quietly dropping out of it.
fn run_rt_logging(root: &Path) -> bool {
    let mut violations = Vec::new();

    for (rel, why) in rt_logging::AUDIO_THREAD_MODULES {
        let path = root.join(rel);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                violations.push(format!(
                    "{rel}: could not read ({e}) -- this module is on FR-ERR-030's audio-thread \
                     list because it {why}. If it moved or was renamed, update xtask's \
                     AUDIO_THREAD_MODULES by hand; do not delete the entry"
                ));
                continue;
            }
        };
        for (line, name) in rt_logging::scan_logger_names(&content) {
            violations.push(format!(
                "{rel}:{line}: names `{name}` -- FR-ERR-030 forbids logging on the audio thread, \
                 and this module {why}. Route the diagnostic through a non-audio module the way \
                 `activate()` routes it through `SharedInner::push_notice`"
            ));
        }
    }

    if violations.is_empty() {
        println!(
            "rt-logging: clean (none of the {} audio-thread module(s) names namir-platform's \
             logger)",
            rt_logging::AUDIO_THREAD_MODULES.len()
        );
        true
    } else {
        println!(
            "rt-logging: {} violation(s) found (FR-ERR-030):",
            violations.len()
        );
        for v in &violations {
            println!("  - {v}");
        }
        false
    }
}

/// M14's R-17 gate (issue #25): no `cargo` invocation in this repository's command-carrying files
/// passes `--all-features`, and `namir-clap`'s `host-ext-tests` stays unreachable from `default`
/// with `clack-host` a dev-dependency. See `feature_guard.rs`'s module doc for why both halves are
/// needed and what neither can see.
///
/// A scanned root that has gone missing is a violation rather than a skip, on the same terms as
/// `run_rt_logging`'s module list: the list is hand-maintained, so a rename must fail loudly
/// instead of quietly un-guarding the tree.
fn run_feature_guard(root: &Path) -> bool {
    let mut violations = Vec::new();

    for (rel, why) in feature_guard::COMMAND_ROOTS {
        let dir = root.join(rel);
        if !dir.is_dir() {
            violations.push(format!(
                "{rel}: not a directory -- it is on R-17's scanned list because it {why}. If it \
                 moved or was renamed, update xtask's COMMAND_ROOTS by hand; do not delete the entry"
            ));
            continue;
        }
        for file in feature_guard::command_files(&dir) {
            let Ok(content) = std::fs::read_to_string(&file) else {
                violations.push(format!("{}: could not read", file.display()));
                continue;
            };
            for line in feature_guard::scan_for_all_features(&content) {
                violations.push(format!(
                    "{}:{line}: a cargo invocation passes --all-features, which switches \
                     namir-clap's host-ext-tests on for the shipped cdylib and links clack-host \
                     into it (R-17). Name the feature instead: `--features host-ext-tests`",
                    file.display()
                ));
            }
        }
    }

    for (manifest_rel, feature, dependency, why) in feature_guard::NON_DEFAULT_FEATURES {
        let path = root.join(manifest_rel);
        let Ok(content) = std::fs::read_to_string(&path) else {
            violations.push(format!(
                "{manifest_rel}: could not read -- `{feature}` must stay non-default because \
                 {why}. Update xtask's NON_DEFAULT_FEATURES by hand if the manifest moved"
            ));
            continue;
        };
        for problem in feature_guard::check_feature_stays_non_default(&content, feature, dependency)
        {
            violations.push(format!("{manifest_rel}: {problem} ({why})"));
        }
    }

    if violations.is_empty() {
        println!(
            "feature-guard: clean (no cargo invocation under {} passes --all-features; {} \
             feature(s) still non-default with their dependency dev-only)",
            feature_guard::COMMAND_ROOTS
                .iter()
                .map(|(root, _)| *root)
                .collect::<Vec<_>>()
                .join(", "),
            feature_guard::NON_DEFAULT_FEATURES.len()
        );
        true
    } else {
        println!(
            "feature-guard: {} violation(s) found (R-17):",
            violations.len()
        );
        for v in &violations {
            println!("  - {v}");
        }
        false
    }
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

/// M12's product-identity gate (NFR-DOC-040, NFR-LIC-070, and the brand mark FR-UI-110 asks for),
/// extended at M13 with FR-UI-110's application icon: the checked-in alpha blob and
/// `images/namir.ico` each match a fresh render of `images/namir.png`, and the two identity
/// documents carry the statements the two requirements name. Unlike `params-lock`/`attribution`,
/// which have one artifact each and so one status line, this check has four and reports a list --
/// a missing README should not hide a stale blob.
fn run_identity(root: &Path, write: bool) -> bool {
    if write {
        return match identity::write_generated(root) {
            Ok(messages) => {
                for message in messages {
                    println!("identity: {message}");
                }
                true
            }
            Err(e) => {
                println!("identity: could not run check: {e}");
                false
            }
        };
    }

    match identity::check(root) {
        Ok(violations) if violations.is_empty() => {
            println!(
                "identity: clean (brand mark and application icon up to date; README.md and TRADEMARK.md carry every required statement)"
            );
            true
        }
        Ok(violations) => {
            println!("identity: {} violation(s) found:", violations.len());
            for v in &violations {
                println!("  - {v}");
            }
            false
        }
        Err(e) => {
            println!("identity: could not run check: {e}");
            false
        }
    }
}

/// M13's packaging primitive (D-18.3): stage one platform's release artifacts in the form
/// FR-PKG-020 requires, with FR-PKG-040's three documents beside them.
///
/// Reports a **list** of violations, like `run_identity` and for the same reason. A materialising
/// run asserts its own output before returning, so that "the packaging step asserts the produced
/// layout against the required form for the platform it targets, and fails the build on any
/// deviation" (FR-PKG-020's `Verify:` method) is true of `bundle` itself, not only of
/// `bundle --check`.
fn run_bundle(root: &Path, args: &bundle::BundleArgs) -> bool {
    let layout = bundle::plan(args.platform);
    let staging_root = bundle::staging_root(root, args.platform);

    for line in bundle::describe(&layout) {
        println!("{line}");
    }
    if args.mode == bundle::Mode::Plan {
        return true;
    }

    if args.mode == bundle::Mode::Materialise {
        match bundle::materialise(root, &bundle::build_dir(root), &staging_root, &layout) {
            Ok(message) => println!("bundle: {message}"),
            Err(e) => {
                println!("bundle: {e}");
                return false;
            }
        }
    }

    match bundle::check(&staging_root, &layout) {
        Ok(violations) if violations.is_empty() => {
            println!(
                "bundle: {} is the form the {} plugin loader requires, and carries the attribution \
                 file and both licence texts",
                staging_root.display(),
                args.platform.name()
            );
            true
        }
        Ok(violations) => {
            println!("bundle: {} violation(s) found:", violations.len());
            for v in &violations {
                println!("  - {v}");
            }
            false
        }
        Err(e) => {
            println!("bundle: could not run check: {e}");
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

/// What one traceability invocation produced: the exit verdict, plus §22 R-13's partial block as
/// the exact lines it is to be printed as.
///
/// The block is *returned* rather than printed at the point it is computed, for one reason: a unit
/// test inside a binary target cannot capture `println!`, and R-13's mitigation (d) **is** the
/// printed count -- "the ordinary run prints the partial count on every invocation, so the number is
/// in front of whoever runs the gate rather than buried in a table"
/// (`docs/02-architecture.md:2564`). Asserting only on the generated plan would leave the one
/// mechanism R-13 names untested end to end. [`run_traceability`] prints it, in the same position it
/// has always occupied: last, after the uncovered list.
struct TraceabilityRun {
    ok: bool,
    partial_lines: Vec<String>,
}

impl TraceabilityRun {
    /// A run that aborted before reaching the partial block -- an unreadable input, an unparsable
    /// FRS, or a malformed annotation. Nothing to print, which is exactly what the caller of the
    /// pre-existing `bool`-returning form saw on those paths.
    fn failed() -> Self {
        Self {
            ok: false,
            partial_lines: Vec::new(),
        }
    }
}

fn run_traceability(root: &Path, write: bool, allow_uncovered: bool) -> bool {
    let run = traceability_outcome(root, write, allow_uncovered);
    for line in &run.partial_lines {
        println!("{line}");
    }
    run.ok
}

fn traceability_outcome(root: &Path, write: bool, allow_uncovered: bool) -> TraceabilityRun {
    let frs_path = root.join("docs/01-functional-requirements.md");
    let frs_text = match std::fs::read_to_string(&frs_path) {
        Ok(t) => t,
        Err(e) => {
            println!("traceability: could not read {}: {e}", frs_path.display());
            return TraceabilityRun::failed();
        }
    };
    let requirements = match traceability::parse_must_requirements(&frs_text) {
        Ok(r) => r,
        Err(e) => {
            println!("traceability: could not parse FRS: {e}");
            return TraceabilityRun::failed();
        }
    };

    // The roadmap feeds two things with very different standing. D-23.2's §14 denominator check is
    // a **gated** input on the required half, which is why an unreadable roadmap is fatal here;
    // D-18.5's owning-milestone attribution is printed text that no exit status reads. Read once,
    // and the fatality is D-23.2's, not attribution's -- if the denominator check ever moves out of
    // the required half, this read must become non-fatal with every id rendering `[unattributed]`,
    // because a required check going red for a cosmetic lookup is the inversion D-18.5 forbids.
    let roadmap_path = root.join("docs/03-implementation-roadmap.md");
    let roadmap_text = match std::fs::read_to_string(&roadmap_path) {
        Ok(t) => t,
        Err(e) => {
            println!(
                "traceability: could not read {}: {e}",
                roadmap_path.display()
            );
            return TraceabilityRun::failed();
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

    // D-23.1's `Verify:`-code guard needs a code per id; indexed once here rather than searched
    // per annotation. Only Musts appear in `requirements`, so an id that is not one is simply
    // absent and passes the guard -- this tool has never restricted what a tag may *name*, only
    // what a tag *means*.
    let verify_codes: HashMap<&str, char> = requirements
        .iter()
        .map(|req| (req.id.as_str(), req.verify))
        .collect();

    let mut source_hits: HashMap<String, Vec<String>> = HashMap::new();
    let mut partial_hits: HashMap<String, Vec<traceability::PartialHit>> = HashMap::new();
    for (file, crate_name) in &files_with_crate {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        // D-23.1's malformed-annotation errors abort the whole run here, upstream of the `--write`
        // branch and of every exit-status computation below: the tool must never write or diff a
        // plan generated from annotations it could not parse. Deliberately upstream of D-18.5's
        // `--allow-uncovered` too -- that flag relaxes the *coverage* verdict, and a malformed tag
        // is a malformed input, not a coverage gap. No flag can reach past this `return`.
        let annotations = match traceability::scan_annotations(&source) {
            Ok(a) => a,
            Err(e) => {
                println!("traceability: {}:{e}", file.display());
                return TraceabilityRun::failed();
            }
        };
        for ann in annotations {
            match ann.uncovered {
                None => source_hits
                    .entry(ann.id)
                    .or_default()
                    .push(crate_name.clone()),
                Some(uncovered) => {
                    // D-23.1's PARTIAL-render guarantee, checked here because here is the last
                    // point that still knows the file and line: `build_report` and
                    // `render_test_plan` both dispatch on the `Verify:` code before they consult
                    // `partial_hits`, so a partial naming a `Verify: M`/`Verify: Process` Must
                    // would be parsed, validated and then dropped in silence. Refused with the same
                    // weight and in the same place as a malformed tag, and upstream of the
                    // `--write` branch and of every exit-status term for the same reason: a tag the
                    // tool refuses must never reach a plan it writes or diffs.
                    if let Some(&verify) = verify_codes.get(ann.id.as_str())
                        && let Err(e) =
                            traceability::check_partial_verify_code(&ann.id, verify, ann.line)
                    {
                        println!("traceability: {}:{e}", file.display());
                        return TraceabilityRun::failed();
                    }
                    partial_hits
                        .entry(ann.id)
                        .or_default()
                        .push(traceability::PartialHit {
                            component: crate_name.clone(),
                            uncovered,
                        })
                }
            }
        }
        for req in &requirements {
            if req.verify == 'M'
                || source_hits.contains_key(&req.id)
                || partial_hits.contains_key(&req.id)
            {
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

    let report = traceability::build_report(
        &requirements,
        &manual_test_docs,
        &source_hits,
        &partial_hits,
    );
    let test_plan_path = root.join("docs/03-test-plan.md");
    let expected = traceability::render_test_plan(&requirements, &report);

    // Deliberately *not* gated on `write`. `--write` forces `plan_up_to_date` true below because it
    // regenerates `docs/03-test-plan.md`; it cannot regenerate §14, which D-23.2 keeps
    // hand-maintained by design. Suppressing this under `--write` would make the flag a one-step
    // bypass of the very gate that decision creates.
    let section_table_ok = check_section_table(&requirements, &roadmap_text);

    let plan_up_to_date = if write {
        if let Err(e) = std::fs::write(&test_plan_path, &expected) {
            println!(
                "traceability: failed to write {}: {e}",
                test_plan_path.display()
            );
            return TraceabilityRun::failed();
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

    // Computed and printed **unconditionally**, in both modes. D-18.5's mechanism is explicit that
    // `--allow-uncovered` "prints exactly what it prints today but derives its exit status from the
    // plan diff alone" (`02-architecture.md:2022-2023`) -- so no `if !allow_uncovered` guard, no
    // early return, no short-circuit. The flag reaches the exit status and nothing else.
    let coverage_clean = if report.missing.is_empty() {
        println!(
            "traceability: clean -- all {} Must requirements are covered",
            requirements.len()
        );
        true
    } else {
        let owners = milestones::attribute(&roadmap_text);
        println!(
            "traceability: {} Must requirement(s) with no coverage found (NFR-QUAL-010):",
            report.missing.len()
        );
        for req in &report.missing {
            println!(
                "{}",
                uncovered_line(req, &owners, report.manual_unexecuted.get(&req.id))
            );
        }
        // Mandatory rather than decorative: without it a derived label reads as a curated ownership
        // register, which is the artifact D-18.5 rejects in every form it can take.
        println!(
            "traceability: milestone labels are derived, not curated -- each is the last `## <n>. \
             M<k>` section of docs/03-implementation-roadmap.md that names the id, at that \
             document's own granularity (never a phase such as M9a or M10 Phase 0). Printed only; \
             the exit status never reads them (D-18.5)."
        );
        if allow_uncovered {
            // So a green step that just printed a list of gaps is not mysterious.
            println!(
                "traceability: uncovered Musts are informational under --allow-uncovered -- exit \
                 status reflects the generated-plan diff and \u{a7}14's denominators only. This \
                 half becomes required at M9b's close-out (D-18.5)."
            );
        }
        false
    };

    // D-18.5 splits this check in two halves with different flip dates, and D-23.2
    // (`02-architecture.md:2891-2895`) puts the derived-denominator check on the **required**
    // plan-diff half rather than the informational zero-uncovered one. `--allow-uncovered` relaxes
    // `coverage_clean` alone -- `section_table_ok` belongs beside `plan_up_to_date` in `required`,
    // never in the term the flag weakens.
    let required = plan_up_to_date && section_table_ok;
    TraceabilityRun {
        ok: traceability::exit_ok(required, coverage_clean, allow_uncovered),
        partial_lines: partial_count_lines(&requirements, &report),
    }
}

/// One line of D-18.5's uncovered list: today's shape (`main.rs:299` before this change) with the
/// owning milestone appended. An id no milestone section names renders `[unattributed]` and is
/// printed with exactly the same weight, and counts identically toward the total, as one whose
/// milestone derives -- the attribution never removes, reorders or suppresses an entry.
/// `manual` is the `(filename, reason)` pair a `Verify: M` Must carries when its script exists but
/// records no clean pass (issue #34). Appended so the reader is not left hunting for a document
/// that is right there: an id printed with nothing after it means no document at all, which is a
/// materially different thing to fix.
fn uncovered_line(
    req: &traceability::Requirement,
    owners: &HashMap<String, String>,
    manual: Option<&(String, String)>,
) -> String {
    let owner = owners
        .get(&req.id)
        .map_or(milestones::UNATTRIBUTED, String::as_str);
    let mut line = format!("  - {} (Verify: {}) [{owner}]", req.id, req.verify);
    if let Some((file, reason)) = manual {
        line.push_str(&format!(" -- docs/manual-tests/{file} {reason}"));
    }
    line
}

/// §22's **R-13** mitigation (d): "the ordinary run prints the partial count on **every**
/// invocation, so the number is in front of whoever runs the gate rather than buried in a table"
/// (`docs/02-architecture.md:2564`).
///
/// Returned rather than printed here, and printed by [`run_traceability`] one call up, so an
/// end-to-end test can assert on the exact number R-13 puts in front of the reader -- see
/// [`TraceabilityRun`]. The lines and their order are unchanged by that; the first is always the
/// count.
///
/// Emitted when the count is zero too. A line that appears only when there is bad news does not put
/// a number in front of anyone, and R-13 is mitigated by the count *falling* -- which cannot be
/// watched if the zero baseline is invisible.
///
/// The count is never folded into, subtracted from or offset against the uncovered count. A partial
/// counts as covered for the ordinary run (D-23.1), so the two numbers are separate lines saying
/// separate things.
///
/// **The counted set is exactly the set of `**PARTIAL**` rows the generated plan carries**
/// ([`traceability::partial_row_ids`]), not every key of `partial_hits`. The two differ: the plan's
/// rows are the FRS's Musts, so a `trace-partial:` naming a Should, a Could or an id the FRS does
/// not carry parses, validates and lands in `partial_hits` while rendering no row. Counting those
/// would print a number that mitigations (b) and (d) then disagree about -- the reader is told to
/// find *n* partials in a checked-in file that shows fewer -- which is precisely the reconciliation
/// R-13 exists to spare them.
///
/// They are printed under their own heading rather than dropped, and the "print, don't drop" choice
/// is the same one [`traceability::scan_annotations`] makes for a malformed tag: a partial on a
/// non-Must is still someone recording a gap, and deleting it from the output because no row
/// happens to exist for it would lose exactly what its author wrote it to say. That block is
/// emitted only when it is non-empty -- unlike the count line, whose zero baseline is the thing a
/// falling count is watched against, an absent block asserts nothing.
fn partial_count_lines(
    requirements: &[traceability::Requirement],
    report: &traceability::Report,
) -> Vec<String> {
    let rendered = traceability::partial_row_ids(requirements, report);
    let mut lines = vec![format!(
        "traceability: {} requirement(s) counted as covered by a `// trace-partial:` annotation \
         (R-13)",
        rendered.len()
    )];
    lines.extend(
        rendered
            .iter()
            .map(|id| partial_line(id, &report.partial_hits[id])),
    );

    // Sorted: `HashMap` iteration order is arbitrary, and a list that reshuffles between runs is
    // unreadable in a CI log diff.
    let mut unrendered: Vec<&String> = report
        .partial_hits
        .keys()
        .filter(|id| !rendered.contains(*id))
        .collect();
    unrendered.sort();
    if !unrendered.is_empty() {
        lines.push(format!(
            "traceability: {} further `// trace-partial:` annotation(s) name a requirement \
             docs/03-test-plan.md carries no `**PARTIAL**` row for -- the plan's rows are the FRS's \
             Must requirements, so a partial on a Should, a Could or an unlisted id renders \
             nowhere. Outside the count above, which names exactly the rendered rows (R-13), and \
             printed rather than dropped -- each is still a recorded gap:",
            unrendered.len()
        ));
        lines.extend(
            unrendered
                .into_iter()
                .map(|id| partial_line(id, &report.partial_hits[id])),
        );
    }
    lines
}

/// One partial's line: its id and the closing milestone its own mandatory `uncovered:` field
/// declares. The cheap half of R-13's "re-read the partial list at each milestone close" -- the
/// alternative is a reader opening a 130-row generated table to find out what is outstanding.
fn partial_line(id: &str, hits: &[traceability::PartialHit]) -> String {
    let mut closes: Vec<&str> = hits
        .iter()
        .filter_map(|hit| traceability::closing_milestone(&hit.uncovered))
        .collect();
    closes.sort_unstable();
    closes.dedup();
    // `scan_annotations` rejects an `uncovered:` line carrying no `; closes M<n>` clause, so the
    // empty case is unreachable through that path; rendered rather than unwrapped anyway, since a
    // panic here would take down a gate over a cosmetic line.
    if closes.is_empty() {
        format!("  - {id} -> closing milestone not stated")
    } else {
        format!("  - {id} -> closes {}", closes.join(", "))
    }
}

/// D-23.2's derived half: §14's `### M9a re-audit` table's row set and Must-count column must
/// agree with what the FRS itself says. The three verdict columns are hand-adjudicated and are
/// outside this check.
fn check_section_table(requirements: &[traceability::Requirement], roadmap_text: &str) -> bool {
    let derived = match traceability::section_must_counts(requirements) {
        Ok(d) => d,
        Err(e) => {
            println!("traceability: {e}");
            return false;
        }
    };
    let parsed = match traceability::parse_roadmap_section_table(roadmap_text) {
        Ok(t) => t,
        Err(e) => {
            println!("traceability: {e}");
            return false;
        }
    };

    let defects = traceability::compare_section_counts(&derived, &parsed);
    if defects.is_empty() {
        let total: usize = derived.iter().map(|s| s.count).sum();
        println!(
            "traceability: \u{a7}14's M9a re-audit table matches the FRS -- {} section rows, \
             {total} Must requirements",
            derived.len()
        );
        return true;
    }

    // The remedy line is the part that matters: the usual xtask remedy is `--write`, and here that
    // is wrong. The FRS is the source of truth and the roadmap table is hand-edited.
    println!(
        "traceability: docs/03-implementation-roadmap.md \u{a7}14's `### M9a re-audit` table \
         disagrees with the\n  Must counts derived from docs/01-functional-requirements.md \
         (D-23.2). The FRS is the source of\n  truth; fix the roadmap table by hand -- `--write` \
         regenerates docs/03-test-plan.md and never\n  touches the roadmap. Do not edit the \
         superseded M0 table above it:"
    );
    for defect in &defects {
        println!("  - {defect}");
    }
    false
}

fn print_usage() {
    println!(
        "usage: cargo run -p xtask -- <layering|rt-logging|feature-guard|params-lock [--write]|attribution [--write]|identity [--write]|traceability [--write] [--allow-uncovered]|preset [output-path]|preset --verify <path>|nam-parity --model <path> --input <path> --reference <path>|bundle [--target <windows|macos|linux>] [--check|--plan]>"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repo_root();

    let ok = match args.first().map(String::as_str) {
        Some("layering") => run_layering(&root),
        Some("rt-logging") => run_rt_logging(&root),
        Some("feature-guard") => run_feature_guard(&root),
        Some("params-lock") => {
            let write = args.iter().skip(1).any(|a| a == "--write");
            run_params_lock(&root, write)
        }
        Some("attribution") => {
            let write = args.iter().skip(1).any(|a| a == "--write");
            run_attribution(&root, write)
        }
        Some("identity") => {
            let write = args.iter().skip(1).any(|a| a == "--write");
            run_identity(&root, write)
        }
        // The one subcommand that parses strictly rather than by `any(|a| a == "--write")`: its
        // flag selects between a required and an informational gate (D-18.5), so a typo must be
        // loud. Exit 2, the same code the unknown-subcommand arm below uses.
        Some("traceability") => match traceability::parse_traceability_args(&args[1..]) {
            Ok(parsed) => run_traceability(&root, parsed.write, parsed.allow_uncovered),
            Err(e) => {
                println!("{e}");
                print_usage();
                std::process::exit(2);
            }
        },
        Some("preset") => preset::run(&args[1..]),
        // Strict, for the same reason `traceability` and `nam-parity` are: `--check`/`--plan`
        // select between materialising, asserting and describing, and `--target` between three
        // platforms' layouts, so a typo silently falling back to "materialise for the host" would
        // be the worst of the four outcomes.
        Some("bundle") => match bundle::parse_args(&args[1..]) {
            Ok(parsed) => run_bundle(&root, &parsed),
            Err(e) => {
                println!("{e}");
                print_usage();
                std::process::exit(2);
            }
        },
        // Strict, like `traceability`'s own parse: see nam_parity's module comment for why an
        // unrecognised flag here should be loud rather than silently ignored.
        Some("nam-parity") => match nam_parity::parse_args(&args[1..]) {
            Ok(parsed) => nam_parity::run(&parsed),
            Err(e) => {
                println!("{e}");
                print_usage();
                std::process::exit(2);
            }
        },
        _ => {
            print_usage();
            std::process::exit(2);
        }
    };

    if !ok {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- §22 R-17 (issue #25): the --all-features guard, wired to the real tree -----------------

    /// The gate as CI should run it, against the real repository. Doubles as the existence check
    /// for every entry in `feature_guard::COMMAND_ROOTS` and `NON_DEFAULT_FEATURES`, since a
    /// missing one is a violation.
    #[test]
    fn the_real_tree_passes_the_all_features_guard() {
        assert!(run_feature_guard(&repo_root()));
    }

    #[test]
    fn a_planted_all_features_build_command_fails_the_guard() {
        // R-17 broken once, on purpose, in the exact shape the row describes: a release step that
        // adds the flag. Every scanned root is copied verbatim into a scratch tree, which must
        // start clean, and one workflow file then gains the line -- so the tree differs from the
        // real one in precisely that.
        let dir = std::env::temp_dir().join(format!("xtask-r17-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let root = repo_root();
        for (rel, _) in feature_guard::COMMAND_ROOTS {
            for file in feature_guard::command_files(&root.join(rel)) {
                let dest = dir.join(file.strip_prefix(&root).unwrap());
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                std::fs::copy(&file, &dest).unwrap();
            }
        }
        for (manifest, _, _, _) in feature_guard::NON_DEFAULT_FEATURES {
            let dest = dir.join(manifest);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::copy(root.join(manifest), &dest).unwrap();
        }
        assert!(run_feature_guard(&dir), "the copy must start clean");

        let victim = dir.join(".github/workflows/release.yml");
        let mut source = std::fs::read_to_string(&victim).unwrap();
        source.push_str("      - name: smuggled\n        run: cargo build --release --workspace --all-features\n");
        std::fs::write(&victim, source).unwrap();
        assert!(!run_feature_guard(&dir));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_scanned_root_is_a_violation_rather_than_a_skip() {
        // The list is hand-maintained; a root that has moved must fail loudly instead of silently
        // shrinking the guard to nothing.
        let dir = std::env::temp_dir().join(format!("xtask-r17-empty-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!run_feature_guard(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- NFR-PORT-020 / D-5.2(b): the platform-cfg lint, wired to the real tree -----------------

    /// The lint as CI runs it, over the whole scanned set — every `.rs` file under every crate bar
    /// `namir-platform`, plus every manifest. Tests, benches and examples are inside that set since
    /// M14; before then only `crates/*/src` was.
    // trace: NFR-PORT-020
    #[test]
    fn the_real_tree_carries_no_platform_conditional_outside_namir_platform() {
        assert!(scan_repo_for_platform_cfg(&repo_root()).is_empty());
    }

    #[test]
    fn a_planted_conditional_outside_src_fails_the_lint() {
        // The negative control, and specifically for the part of the scanned set M14 added: a
        // conditional in a crate's `tests/` directory, which the `crates/*/src` walk could not see.
        // Every real crate file is left alone -- the scratch root holds one crate and one file.
        let dir = std::env::temp_dir().join(format!("xtask-port-020-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("crates/namir-engine/tests")).unwrap();
        std::fs::write(
            dir.join("crates/namir-engine/tests/probe.rs"),
            "fn a() {}\n",
        )
        .unwrap();
        assert!(
            scan_repo_for_platform_cfg(&dir).is_empty(),
            "the scratch tree must start clean"
        );

        std::fs::write(
            dir.join("crates/namir-engine/tests/probe.rs"),
            "#[cfg(not(windows))]\nfn a() {}\n",
        )
        .unwrap();
        let violations = scan_repo_for_platform_cfg(&dir);
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(
            violations[0].contains("tests/probe.rs:1"),
            "{violations:#?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_planted_target_table_in_a_crate_manifest_fails_the_lint() {
        // The manifest half: a platform conditional expressed in TOML, which no source-level scan
        // can see because the conditional is not in the source.
        let dir = std::env::temp_dir().join(format!("xtask-port-020-toml-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("crates/namir-engine/src")).unwrap();
        std::fs::write(
            dir.join("crates/namir-engine/Cargo.toml"),
            "[package]\nname = \"namir-engine\"\n\n\
             [target.'cfg(windows)'.dependencies]\nwindows-sys = \"0.59\"\n",
        )
        .unwrap();

        let violations = scan_repo_for_platform_cfg(&dir);
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(violations[0].contains("Cargo.toml:4"), "{violations:#?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn namir_platforms_own_conditionals_stay_exempt() {
        // D-5.1's one carve-out, over the widened scanned set: `namir-platform`'s manifest carries
        // the tree's only legitimate `[target.'cfg(...)']` table and its `src` the only legitimate
        // `#[cfg(target_os)]` attributes. Both must remain invisible to this lint.
        let dir =
            std::env::temp_dir().join(format!("xtask-port-020-exempt-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("crates/namir-platform/src")).unwrap();
        std::fs::write(
            dir.join("crates/namir-platform/src/paths.rs"),
            "#[cfg(target_os = \"windows\")]\nfn a() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("crates/namir-platform/Cargo.toml"),
            "[package]\nname = \"namir-platform\"\n\n\
             [target.'cfg(any(target_os = \"linux\"))'.dependencies]\nalsa = \"0.9\"\n",
        )
        .unwrap();

        assert!(scan_repo_for_platform_cfg(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- FR-ERR-030: the audio-thread logging ban, wired to the real tree --------------------------

    /// The gate as CI runs it. Doubles as the existence check for every path in
    /// `rt_logging::AUDIO_THREAD_MODULES`, since an unreadable entry is a violation — so a module
    /// renamed out from under the list fails here as well as in CI.
    // trace-partial: FR-ERR-030
    // uncovered: FR-ERR-030 — the S half's logging limb only. The allocation limb is D-7.5's
    // uncovered: assert_no_alloc harness rather than this check; the "diagnostics ... communicated
    // uncovered: to a non-real-time thread without blocking" clause is spanned by nothing; and the
    // uncovered: `plus I` half of the Verify line has no integration test driving a real audio
    // uncovered: callback and asserting no record was emitted; closes M8
    #[test]
    fn the_real_tree_names_no_logger_in_any_audio_thread_module() {
        assert!(run_rt_logging(&repo_root()));
    }

    #[test]
    fn a_listed_audio_thread_module_that_cannot_be_read_is_a_violation() {
        // The negative control for the check's own wiring, and the mechanism `rt_logging.rs`'s
        // residual 2 names: the list is hand-maintained, so a path that resolves to nothing must
        // fail loudly instead of silently un-covering the module.
        let dir = std::env::temp_dir().join(format!("xtask-rt-logging-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!run_rt_logging(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_planted_record_call_in_an_audio_thread_module_fails_the_gate() {
        // The other negative control, and the one that matters: the check fires on the exact hazard
        // FR-ERR-030 names. Every listed module is copied verbatim into a scratch root and one of
        // them has a logging call appended, so the tree differs from the real one in precisely that.
        let dir = std::env::temp_dir().join(format!("xtask-rt-logging-hit-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let root = repo_root();
        for (rel, _) in rt_logging::AUDIO_THREAD_MODULES {
            let dest = dir.join(rel);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::copy(root.join(rel), &dest).unwrap();
        }
        assert!(run_rt_logging(&dir), "the copy must start clean");

        let victim = dir.join(rt_logging::AUDIO_THREAD_MODULES[0].0);
        let mut source = std::fs::read_to_string(&victim).unwrap();
        source
            .push_str("fn smuggled() {\n    namir_platform::logging::record(CODE, \"oops\");\n}\n");
        std::fs::write(&victim, source).unwrap();
        assert!(!run_rt_logging(&dir));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A minimal synthetic repo root: one `Verify: M` Must, its manual-test document, and a §14
    /// table carrying `chain_count` as 5.1 CHAIN's denominator. Everything else `run_traceability`
    /// looks for (`crates/`, `xtask/`, the CI workflows) is simply absent, which the walkers treat
    /// as empty.
    fn synthetic_root(name: &str, chain_count: u32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("docs/manual-tests")).unwrap();

        std::fs::write(
            dir.join("docs/01-functional-requirements.md"),
            "### 5.1 Signal chain (CHAIN)\n\n**FR-CHAIN-010 (Must)** — text.\n*Verify:* M.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("docs/manual-tests/fr-chain-010-signal-chain.md"),
            "**Result: PASS.** Executed this session.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("docs/03-implementation-roadmap.md"),
            format!(
                "### M9a re-audit — corrected row set and denominators (2026-08-08)\n\n\
                 | FRS area | Must count | Done | Partial | Not started |\n\
                 |---|---|---|---|---|\n\
                 | 5.1 CHAIN | {chain_count} | — | — | — |\n\
                 | **Total** | **{chain_count}** | — | — | — |\n"
            ),
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_section_14_disagreement_fails_even_under_write() {
        // The composition guard for D-23.2 (`02-architecture.md:2891-2895`): `--write` forces the
        // plan-diff half true because it regenerates the plan, and it must not be able to do the
        // same to the denominator check -- the roadmap table is hand-maintained by design, so a
        // `--write` that silenced this would be a one-flag bypass of the gate.
        let dir = synthetic_root("traceability-section-14-wrong", 2);
        assert!(!run_traceability(&dir, true, false));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_same_tree_with_a_correct_table_passes_under_write() {
        // The positive control: without it the test above would pass for any reason at all.
        let dir = synthetic_root("traceability-section-14-right", 1);
        assert!(run_traceability(&dir, true, false));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn allow_uncovered_does_not_soften_a_section_14_disagreement() {
        // D-18.5's flag relaxes `coverage_clean` alone. `section_table_ok` sits in the required
        // term beside the plan diff, and folding it into the term the flag weakens would make
        // D-23.2's whole check informational with nothing saying so.
        let dir = synthetic_root("traceability-section-14-lenient", 2);
        assert!(!run_traceability(&dir, true, true));
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- D-18.5: the split gate ----------------------------------------------------------------

    /// A synthetic repo root for the exit-status matrix. `covered` chooses between a `Verify: M`
    /// Must with its manual-test document (nothing uncovered) and a `Verify: U` Must with no
    /// artifact anywhere (one uncovered). `names_the_id` controls whether a milestone section of
    /// the roadmap names the requirement, i.e. whether it attributes or renders `[unattributed]`.
    fn split_gate_root(name: &str, covered: bool, names_the_id: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("docs/manual-tests")).unwrap();

        let verify = if covered { "M" } else { "U" };
        std::fs::write(
            dir.join("docs/01-functional-requirements.md"),
            format!(
                "### 5.1 Signal chain (CHAIN)\n\n**FR-CHAIN-010 (Must)** — text.\n*Verify:* {verify}.\n"
            ),
        )
        .unwrap();
        if covered {
            std::fs::write(
                dir.join("docs/manual-tests/fr-chain-010-signal-chain.md"),
                "**Result: PASS.** Executed this session.\n",
            )
            .unwrap();
        }
        // The milestone section comes *before* the `### M9a re-audit` heading: D-23.2's table parse
        // runs forward from that heading to the next heading of any level.
        std::fs::write(
            dir.join("docs/03-implementation-roadmap.md"),
            format!(
                "## 16. M9 — Verification truth-up\n\n{names_the_id}\n\n\
                 ### M9a re-audit — corrected row set and denominators (2026-08-08)\n\n\
                 | FRS area | Must count | Done | Partial | Not started |\n\
                 |---|---|---|---|---|\n\
                 | 5.1 CHAIN | 1 | — | — | — |\n\
                 | **Total** | **1** | — | — | — |\n"
            ),
        )
        .unwrap();
        dir
    }

    /// Regenerates the plan so the plan-diff half is true. Uses `--write --allow-uncovered`, the
    /// combination that regenerates and exits 0 whatever the coverage verdict.
    fn make_plan_fresh(dir: &Path) {
        assert!(
            run_traceability(dir, true, true),
            "--write --allow-uncovered must succeed on a well-formed tree"
        );
    }

    #[test]
    fn the_exit_status_matrix_is_d_18_5s_split() {
        // uncovered present/absent x flag present/absent x plan stale/fresh. The two cells that
        // carry the decision are `(uncovered, fresh, --allow-uncovered) -> pass`, which is the
        // whole point of the flag, and `(covered, stale, --allow-uncovered) -> fail`, which is the
        // flag never softening the plan diff.
        for (covered, fresh, allow, want) in [
            (true, true, false, true),
            (true, true, true, true),
            (true, false, false, false),
            (true, false, true, false),
            (false, true, false, false),
            (false, true, true, true),
            (false, false, false, false),
            (false, false, true, false),
        ] {
            let dir = split_gate_root(
                &format!("split-{covered}-{fresh}-{allow}"),
                covered,
                "FR-CHAIN-010 is owned here.",
            );
            make_plan_fresh(&dir);
            if !fresh {
                std::fs::write(dir.join("docs/03-test-plan.md"), "deliberately stale\n").unwrap();
            }
            assert_eq!(
                run_traceability(&dir, false, allow),
                want,
                "covered={covered} fresh={fresh} allow_uncovered={allow}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn write_alone_still_reports_an_uncovered_must_but_composes_with_the_flag() {
        // Today's behaviour, preserved: `--write` regenerates and *still* exits 1 while any Must is
        // uncovered, which is what makes a regeneration run report the gap. Adding
        // `--allow-uncovered` is the combination the end-of-workflow regeneration wants.
        let dir = split_gate_root("write-composition", false, "FR-CHAIN-010 is owned here.");
        assert!(!run_traceability(&dir, true, false));
        assert!(run_traceability(&dir, true, true));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_milestone_attribution_cannot_reach_the_exit_status() {
        // Two trees identical but for whether the roadmap's milestone section names the uncovered
        // id -- i.e. whether it prints `[M9]` or `[unattributed]`. Both must return the same value
        // in both modes. The structural guarantee is `exit_ok`'s signature (it takes no
        // attribution); this is the behavioural one.
        for (name, prose) in [
            ("attributed", "FR-CHAIN-010 is owned here."),
            ("unattributed", "This milestone names no requirement id."),
        ] {
            let dir = split_gate_root(&format!("attribution-{name}"), false, prose);
            make_plan_fresh(&dir);
            assert!(!run_traceability(&dir, false, false), "{name}, plain");
            assert!(run_traceability(&dir, false, true), "{name}, lenient");
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn an_uncovered_line_carries_its_derived_milestone() {
        let req = traceability::Requirement {
            id: "FR-CFG-020".into(),
            verify: 'G',
            section: "4".into(),
        };
        let mut owners = HashMap::new();
        owners.insert("FR-CFG-020".to_string(), "M9".to_string());
        assert_eq!(
            uncovered_line(&req, &owners, None),
            "  - FR-CFG-020 (Verify: G) [M9]"
        );
    }

    #[test]
    fn an_uncovered_manual_must_names_the_document_and_what_it_records() {
        // Issue #34: a `Verify: M` Must whose script exists but has not been run is uncovered, and
        // the line has to say which of the two it is -- "no document" and "a document saying NOT
        // EXECUTED" are different pieces of work.
        let req = traceability::Requirement {
            id: "FR-UI-020".into(),
            verify: 'M',
            section: "5.13".into(),
        };
        let manual = (
            "fr-ui-020-single-screen-elements.md".to_string(),
            "records `NOT EXECUTED.`".to_string(),
        );
        assert_eq!(
            uncovered_line(&req, &HashMap::new(), Some(&manual)),
            "  - FR-UI-020 (Verify: M) [unattributed] -- \
             docs/manual-tests/fr-ui-020-single-screen-elements.md records `NOT EXECUTED.`"
        );
    }

    #[test]
    fn an_uncovered_line_with_no_owner_says_so_rather_than_guessing() {
        let req = traceability::Requirement {
            id: "FR-XXXX-010".into(),
            verify: 'U',
            section: "9.9".into(),
        };
        assert_eq!(
            uncovered_line(&req, &HashMap::new(), None),
            "  - FR-XXXX-010 (Verify: U) [unattributed]"
        );
    }

    #[test]
    fn a_partial_line_carries_the_milestone_its_own_uncovered_field_declares() {
        // The FR-LIB-020 text D-23.1 prescribes (`03-implementation-roadmap.md:2387-2388`). Unlike
        // the uncovered list's derived label, this one is *declared* by the annotation's author
        // beside the gap it names -- and it is still printed only, never read for an exit status.
        let hits = vec![traceability::PartialHit {
            component: "namir-worker".into(),
            uncovered: "FR-LIB-020 — the off-the-audio-thread clause is exercised only against a \
                        6-file corpus in rt_stress.rs axis C, not the 10 000-file scale the Verify \
                        method names; closes M9b"
                .into(),
        }];
        assert_eq!(
            partial_line("FR-LIB-020", &hits),
            "  - FR-LIB-020 -> closes M9b"
        );
    }

    // --- D-23.1 end to end: a real annotation in a real crate file ------------------------------
    //
    // Everything above drives one link of the partial path in isolation. Nothing drove a real
    // wrapped tag sitting in a scanned source file through the scan loop, into `partial_hits`, into
    // `build_report`, into the rendered plan and into R-13's printed count -- and `synthetic_root`
    // and `split_gate_root` create no `crates/` directory at all, so the routing that decides
    // whether an annotation is collected in the first place was never exercised for a partial.

    /// The two-slash comment opener, assembled at run time and never written inline below.
    ///
    /// `traceability_outcome` scans `<root>/xtask`, and on a real invocation that is **this file**.
    /// A fixture written as a multi-line string literal whose *physical* line began with a marker
    /// would therefore be read as a genuine `xtask` annotation however deeply it is nested inside a
    /// literal -- the line-based scanner's one residual limit, recorded in `traceability.rs`'s
    /// module header, which rule 1 shrank rather than closed. `traceability.rs`'s own fixtures avoid
    /// it with a leading `\x20`; the fixtures here need the marker at the true start of the
    /// generated line, so they interpolate this instead. Do not inline it.
    const SLASHES: &str = "//";

    /// A `trace-partial:`/`uncovered:` pair whose `uncovered:` field is **wrapped** across two
    /// comment lines, returned as (the source to plant, the single field D-23.1's joining rule
    /// produces from it). Wrapped on purpose: joining is the one parsing rule the decision added
    /// specifically because its own worked example does not fit in 100 columns, and an end-to-end
    /// test that planted a one-line field would not exercise it.
    fn wrapped_partial() -> (String, String) {
        let source = format!(
            "{SLASHES} A leading comment, to prove the tag need not be the file's first line.\n\
             {SLASHES} trace-partial: FR-CHAIN-010\n\
             {SLASHES} uncovered: FR-CHAIN-010 — stage ordering is asserted for the gate stage\n\
             {SLASHES} uncovered: alone, not for every stage the requirement names; closes M9b\n\
             #[test]\n\
             fn stage_order_places_the_gate_first() {{}}\n"
        );
        let joined = "FR-CHAIN-010 — stage ordering is asserted for the gate stage alone, not for \
                      every stage the requirement names; closes M9b";
        (source, joined.to_string())
    }

    /// A synthetic root that really does have a `crates/<name>/src/*.rs`, carrying `source`
    /// verbatim. `verify` is the FRS `*Verify:*` code its single Must declares, so one fixture
    /// drives both the ordinary annotated-artifact path and D-23.1's two refused codes; the
    /// manual-test document is written only for `M`, where it is that requirement's real evidence.
    fn annotated_crate_root(name: &str, verify: &str, source: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("docs/manual-tests")).unwrap();
        std::fs::create_dir_all(dir.join("crates/namir-engine/src")).unwrap();

        std::fs::write(
            dir.join("docs/01-functional-requirements.md"),
            format!(
                "### 5.1 Signal chain (CHAIN)\n\n**FR-CHAIN-010 (Must)** — text.\n*Verify:* {verify}.\n"
            ),
        )
        .unwrap();
        if verify == "M" {
            std::fs::write(
                dir.join("docs/manual-tests/fr-chain-010-signal-chain.md"),
                "**Result: PASS.** Executed this session.\n",
            )
            .unwrap();
        }
        std::fs::write(dir.join("crates/namir-engine/src/chain.rs"), source).unwrap();
        std::fs::write(
            dir.join("docs/03-implementation-roadmap.md"),
            "## 16. M9 — Verification truth-up\n\nFR-CHAIN-010 is owned here.\n\n\
             ### M9a re-audit — corrected row set and denominators (2026-08-08)\n\n\
             | FRS area | Must count | Done | Partial | Not started |\n\
             |---|---|---|---|---|\n\
             | 5.1 CHAIN | 1 | — | — | — |\n\
             | **Total** | **1** | — | — | — |\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_wrapped_partial_in_a_crate_file_reaches_the_plan_and_r_13s_printed_count() {
        let (source, joined) = wrapped_partial();
        let dir = annotated_crate_root("partial-e2e", "U", &source);

        let run = traceability_outcome(&dir, true, false);
        // No `--allow-uncovered`: the partial is the *only* coverage FR-CHAIN-010 has, and D-23.1
        // makes that enough for the ordinary run.
        assert!(
            run.ok,
            "a partial counts as coverage for the ordinary run (D-23.1)"
        );

        let plan = std::fs::read_to_string(dir.join("docs/03-test-plan.md")).unwrap();
        assert!(
            plan.contains(&format!(
                "| FR-CHAIN-010 | U | **PARTIAL** — `namir-engine`: {joined} |"
            )),
            "the rendered row must carry the joined `uncovered:` text verbatim, against the crate \
             the annotation was found in:\n{plan}"
        );

        assert_eq!(
            run.partial_lines,
            vec![
                format!(
                    "traceability: 1 requirement(s) counted as covered by a `{SLASHES} \
                     trace-partial:` annotation (R-13)"
                ),
                "  - FR-CHAIN-010 -> closes M9b".to_string(),
            ]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_same_tree_without_the_annotation_leaves_the_requirement_uncovered() {
        // The control the test above needs: it must pass *because of* the partial, not because a
        // one-Must synthetic tree passes anyway. Also pins R-13's zero baseline, which is printed
        // rather than suppressed precisely so a falling count can be watched.
        let dir = annotated_crate_root(
            "partial-e2e-control",
            "U",
            "#[test]\nfn stage_order_places_the_gate_first() {}\n",
        );

        let run = traceability_outcome(&dir, true, false);
        assert!(!run.ok);
        assert_eq!(
            run.partial_lines,
            vec![format!(
                "traceability: 0 requirement(s) counted as covered by a `{SLASHES} \
                 trace-partial:` annotation (R-13)"
            )]
        );

        let plan = std::fs::read_to_string(dir.join("docs/03-test-plan.md")).unwrap();
        assert!(
            plan.contains("| FR-CHAIN-010 | U | **UNRESOLVED** |"),
            "{plan}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A synthetic root whose FRS carries one **Must** and one **Should** in the same section, and
    /// whose one crate file carries a `trace-partial:` on each. §14's denominator counts the Must
    /// alone, which is precisely what leaves the Should with no row in the generated plan.
    fn must_and_should_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("docs/manual-tests")).unwrap();
        std::fs::create_dir_all(dir.join("crates/namir-engine/src")).unwrap();

        std::fs::write(
            dir.join("docs/01-functional-requirements.md"),
            "### 5.1 Signal chain (CHAIN)\n\n\
             **FR-CHAIN-010 (Must)** — text.\n*Verify:* U.\n\n\
             **FR-CHAIN-020 (Should)** — text.\n*Verify:* U.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("crates/namir-engine/src/chain.rs"),
            format!(
                "{SLASHES} trace-partial: FR-CHAIN-010\n\
                 {SLASHES} uncovered: FR-CHAIN-010 — the gate stage alone; closes M9b\n\
                 #[test]\n\
                 fn stage_order_places_the_gate_first() {{}}\n\
                 \n\
                 {SLASHES} trace-partial: FR-CHAIN-020\n\
                 {SLASHES} uncovered: FR-CHAIN-020 — the bypass path is untested; closes M11\n\
                 #[test]\n\
                 fn bypass_is_click_free() {{}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("docs/03-implementation-roadmap.md"),
            "## 16. M9 — Verification truth-up\n\nFR-CHAIN-010 is owned here.\n\n\
             ### M9a re-audit — corrected row set and denominators (2026-08-08)\n\n\
             | FRS area | Must count | Done | Partial | Not started |\n\
             |---|---|---|---|---|\n\
             | 5.1 CHAIN | 1 | — | — | — |\n\
             | **Total** | **1** | — | — | — |\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_partial_on_a_non_must_is_printed_outside_r_13s_count_end_to_end() {
        // The same invariant as `r_13s_printed_count_names_exactly_the_plans_partial_rows`, driven
        // through the real scan loop against a real crate file: a `trace-partial:` naming a Should
        // is accepted (this tool has never restricted what a tag may *name*), reaches
        // `partial_hits`, and must not move the number the plan's rows are read against.
        let dir = must_and_should_root("partial-non-must");
        let run = traceability_outcome(&dir, true, false);
        assert!(
            run.ok,
            "the Must's partial is coverage for the ordinary run"
        );

        let plan = std::fs::read_to_string(dir.join("docs/03-test-plan.md")).unwrap();
        let rows = plan
            .lines()
            .filter(|line| line.starts_with("| ") && line.contains("**PARTIAL**"))
            .count();
        assert_eq!(rows, 1, "{plan}");
        assert!(
            !plan.contains("FR-CHAIN-020"),
            "the plan's rows are Must requirements only:\n{plan}"
        );

        assert_eq!(run.partial_lines.len(), 4, "{:#?}", run.partial_lines);
        assert_eq!(
            run.partial_lines[0],
            format!(
                "traceability: {rows} requirement(s) counted as covered by a `{SLASHES} \
                 trace-partial:` annotation (R-13)"
            ),
            "the count must name exactly the rendered rows"
        );
        assert_eq!(run.partial_lines[1], "  - FR-CHAIN-010 -> closes M9b");
        assert!(
            run.partial_lines[2].starts_with("traceability: 1 further"),
            "{:#?}",
            run.partial_lines
        );
        assert_eq!(run.partial_lines[3], "  - FR-CHAIN-020 -> closes M11");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_partial_naming_a_manual_or_process_verified_must_is_refused_end_to_end() {
        // Finding A, end to end. `build_report` and `render_test_plan` both dispatch on the
        // `Verify:` code before they consult `partial_hits`, so for these two codes D-23.1's
        // "a partial cannot be introduced without appearing in a generated, checked-in, diffable
        // file" had a hole: the tag parsed, its mandatory field validated, and both dropped without
        // a word. Refusing is strictly stronger than rendering, and rendering would have written
        // into a checked-in document a claim D-23.1's own first sentence forbids -- for `M`, that a
        // source file part-verifies a requirement whose method is a written manual script.
        //
        // Note the `M` tree carries that script, so without the guard this run would have gone
        // green with the plan resolving the requirement by its document and the gap invisible.
        let (source, _) = wrapped_partial();
        for (verify, name) in [("M", "manual"), ("Process", "process")] {
            let dir = annotated_crate_root(&format!("partial-{name}-refused"), verify, &source);

            // `--write` *and* `--allow-uncovered`: a malformed input is not a coverage gap, so
            // neither the regeneration flag nor D-18.5's leniency flag may reach past the abort.
            let run = traceability_outcome(&dir, true, true);
            assert!(!run.ok, "Verify: {verify}");
            assert!(
                run.partial_lines.is_empty(),
                "Verify: {verify}: the run aborts before R-13's block"
            );
            assert!(
                !dir.join("docs/03-test-plan.md").exists(),
                "Verify: {verify}: a plan must never be written from a tag the tool refused"
            );

            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn r_13s_printed_count_names_exactly_the_plans_partial_rows() {
        // R-13's mitigation (d) is a number put in front of whoever runs the gate, and (b) is the
        // rows that number is read against. A partial naming a non-Must lands in `partial_hits`
        // and renders no row, so counting it makes the two disagree. It moves to its own block
        // rather than being dropped -- it is still someone recording a gap.
        let requirements = vec![traceability::Requirement {
            id: "FR-CHAIN-010".into(),
            verify: 'U',
            section: "5.1".into(),
        }];
        let mut partial_hits = HashMap::new();
        for (id, closes) in [("FR-CHAIN-010", "M9b"), ("FR-CFG-040", "M11")] {
            partial_hits.insert(
                id.to_string(),
                vec![traceability::PartialHit {
                    component: "namir-engine".into(),
                    uncovered: format!("{id} — a named gap; closes {closes}"),
                }],
            );
        }
        let report = traceability::Report {
            missing: Vec::new(),
            manual_hits: HashMap::new(),
            manual_unexecuted: HashMap::new(),
            source_hits: HashMap::new(),
            partial_hits,
        };

        let lines = partial_count_lines(&requirements, &report);
        assert_eq!(lines.len(), 4, "{lines:#?}");
        assert!(
            lines[0].starts_with("traceability: 1 requirement(s) counted as covered"),
            "{lines:#?}"
        );
        assert_eq!(lines[1], "  - FR-CHAIN-010 -> closes M9b");
        assert!(
            lines[2].starts_with("traceability: 1 further"),
            "{lines:#?}"
        );
        assert!(
            lines[2].contains("printed rather than dropped"),
            "{lines:#?}"
        );
        assert_eq!(lines[3], "  - FR-CFG-040 -> closes M11");
    }

    #[test]
    fn a_partial_line_lists_every_distinct_closing_milestone() {
        let hits = vec![
            traceability::PartialHit {
                component: "namir-worker".into(),
                uncovered: "FR-X-010 — a; closes M9b".into(),
            },
            traceability::PartialHit {
                component: "namir-engine".into(),
                uncovered: "FR-X-010 — b; closes M10".into(),
            },
        ];
        assert_eq!(
            partial_line("FR-X-010", &hits),
            "  - FR-X-010 -> closes M10, M9b"
        );
    }
}
