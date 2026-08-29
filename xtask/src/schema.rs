//! `cargo run -p xtask -- schema [path...]`: FR-STATE-040's `S` half as a **build-time check**,
//! over `docs/04-state-and-preset-format.md` §§3–7.
//!
//! FR-STATE-040's `*Verify:*` line is `M plus S (schema check)`. Its `M` half is
//! `docs/manual-tests/fr-state-040-diffability-and-hand-editability.md`; its `S` half did not
//! exist until M15, and `xtask traceability` could not say so because it kept only the first code
//! of a compound method (issue #27). The check itself is [`namir_state::validate`], which restates
//! §§3–7 independently of this project's own reader — see that module's header for why a validator
//! built out of the reader's own parsing code would agree with it by construction and prove
//! nothing.
//!
//! This subcommand is the *build-time* shape of that check, which is what FRS §1.5's `S` names
//! ("static analysis or build-time check"), and the shape every other `Verify: S` requirement in
//! this repository takes: `layering`, `rt-logging`, `params-lock`, `assets`. `namir-state`'s own
//! `tests/schema.rs` runs the same validator over the same corpus on every `cargo test
//! --workspace`; this is the form a human, a manual-test script, or a CI step can invoke directly,
//! and the form that can be pointed at a real preset file a user is having trouble with.
//!
//! Reports a **list**, like `identity` and `bundle`: one document's malformed reference must not
//! hide another's missing `format_version`.

use std::path::{Path, PathBuf};

use namir_state::{Severity, validate_bytes};

/// The documents checked when no path is given: every hand-authored document in `namir-state`'s
/// checked-in corpus, plus the sample `xtask preset` itself writes.
///
/// The corpus is the interesting half. Those six files were written by hand against the format
/// document, not produced by this build's writer, so they are the ones that can disagree with the
/// validator — a check run only over bytes this project's own writer produced would have the
/// writer and the validator agreeing with each other, which is `corpus.rs`'s own argument for
/// keeping a hand-authored corpus at all.
pub const CORPUS_DIR: &str = "crates/namir-state/tests/corpus";

/// One document that failed the check, and every clause of §§3–7 it breaks.
struct Report {
    label: String,
    violations: Vec<String>,
}

/// Runs the subcommand. `args` is everything after `schema` on the command line: zero or more
/// paths to `.namirpreset`/state documents. With none, the default set above is checked.
///
/// The tag below is the `S` half of FR-STATE-040's compound method, and it is a **plain** tag
/// under D-23.1 for the `S` half specifically: the check exists, executes §§3–7 clause by clause
/// (`namir_state::schema`'s unit tests, one fixture per clause), spans the documents this build
/// writes and the hand-authored corpus (`namir-state/tests/schema.rs`), and asserts rather than
/// prints. The `M` half is unaffected and is still traced by
/// `docs/manual-tests/fr-state-040-diffability-and-hand-editability.md`, per D-18.6 — with the
/// parser change of issue #27, the requirement now needs both and resolves only when both are
/// there.
// trace: FR-STATE-040
pub fn run(root: &Path, args: &[String]) -> bool {
    let targets: Vec<(String, Vec<u8>)> = if args.is_empty() {
        match default_targets(root) {
            Ok(targets) => targets,
            Err(e) => {
                println!("schema: could not assemble the default document set: {e}");
                return false;
            }
        }
    } else {
        let mut out = Vec::new();
        for path in args {
            match std::fs::read(path) {
                Ok(bytes) => out.push((path.clone(), bytes)),
                Err(e) => {
                    println!("schema: could not read {path}: {e}");
                    return false;
                }
            }
        }
        out
    };

    let mut reports = Vec::new();
    for (label, bytes) in &targets {
        match validate_bytes(bytes) {
            // Not a schema violation but a document that has no schema to check: §2's byte ceiling,
            // or bytes that are not a JSON object at all. Reported as its own failing document
            // rather than as a violation list, because there are no clauses to list.
            Err(e) => reports.push(Report {
                label: label.clone(),
                violations: vec![format!("does not parse as a state document at all: {e}")],
            }),
            Ok(violations) if violations.is_empty() => {}
            Ok(violations) => reports.push(Report {
                label: label.clone(),
                violations: violations
                    .iter()
                    .map(|v| {
                        let severity = match v.severity {
                            Severity::Rejected => "rejected",
                            Severity::Recovered => "recovered",
                        };
                        format!("{v} [{severity}]")
                    })
                    .collect(),
            }),
        }
    }

    if reports.is_empty() {
        println!(
            "schema: clean ({} document(s) conform to docs/04-state-and-preset-format.md §§3-7)",
            targets.len()
        );
        return true;
    }

    println!(
        "schema: {} of {} document(s) do not conform to docs/04-state-and-preset-format.md §§3-7 \
         (FR-STATE-040):",
        reports.len(),
        targets.len()
    );
    for report in &reports {
        println!("  {}:", report.label);
        for violation in &report.violations {
            println!("    - {violation}");
        }
    }
    false
}

fn default_targets(root: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let corpus = root.join(CORPUS_DIR);
    collect_documents(&corpus, &mut paths)?;
    // Sorted: `read_dir`'s order is filesystem-dependent, and a check that lists its findings in a
    // different order on each platform is unreadable in a CI log diff -- the same reasoning
    // `traceability`'s manual-test read already records.
    paths.sort();
    if paths.is_empty() {
        return Err(format!("{} holds no documents", corpus.display()));
    }

    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for path in paths {
        let label = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push((label, bytes));
    }
    out.push((
        "xtask preset (generated sample)".to_string(),
        crate::preset::sample_bytes(),
    ));
    Ok(out)
}

/// Every file under `dir`, recursively. Deliberately not filtered by extension: the corpus's own
/// manifest test is what says which files belong there, and a check that silently skipped a file
/// whose extension it did not recognise would be the "quietly checking a smaller set" failure this
/// repository has already been bitten by more than once.
fn collect_documents(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_documents(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}
