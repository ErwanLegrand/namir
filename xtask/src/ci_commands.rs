//! `cargo run -p xtask -- ci-commands`: the commands `README.md` documents are the commands
//! `.github/workflows/ci.yml` runs.
//!
//! NFR-BUILD-020 (Must): "The repository shall document how to build, test and run every product
//! configuration on every supported platform, and that documentation shall be exercised by CI so
//! it cannot drift." `Verify: S`. M12 closed the *document* half — `xtask identity` asserts the
//! README still contains the build, run and test commands, so they cannot silently disappear from
//! it. Nothing compared either side against the other, and by M14 they had drifted in both
//! directions at once: `ci.yml` ran `cargo run -p xtask -- rt-logging`, which the README's gate
//! block did not list, and the README documented a `.clap` that "has to be assembled by hand",
//! which `xtask bundle` had done since M13.
//!
//! # What this asserts, in two directions
//!
//! 1. **Every command the README documents is run by CI** — [`check_documented_are_run`]. The
//!    documented set is every line beginning `cargo ` inside a fenced code block of `README.md`
//!    ([`documented_commands`]), which is where a reader copies a command from. A documented
//!    command is *exercised* when some `run:` step in `ci.yml` invokes it verbatim or with extra
//!    trailing arguments ([`is_exercised_by`]).
//! 2. **Every `xtask` subcommand CI runs is documented in the README** —
//!    [`check_run_are_documented`]. This is the direction that catches the drift a growing tool
//!    produces: a new subcommand is wired into CI and the README is not touched, and nothing
//!    notices until someone reads both files side by side.
//!
//! # The two deliberate weaknesses, named rather than hidden
//!
//! **The extension limb of [`is_exercised_by`] is not a proof.** `cargo build --workspace
//! --all-targets` runs strictly more than the documented `cargo build --workspace`, so the
//! documented command being broken is not something the CI invocation could hide — but an
//! extending flag is free to *weaken* what the base command asserts, and one in this repository
//! does: `traceability --allow-uncovered` derives its exit status from half the gate. That case is
//! covered because `ci.yml` also runs the plain form, not because this check would have caught it.
//! An addition that only weakens is a thing a reader of `ci.yml` has to notice.
//!
//! **`cargo deny check` is the one narrowing case that is not left to a reader** (M15 review). Its
//! extra argument is not a flag at all — it *selects a sub-check*, so `cargo deny check licenses`
//! runs strictly **less** than the documented bare `cargo deny check`, which runs all four of
//! advisories, bans, licenses and sources. The token-prefix rule certified the narrow invocation as
//! exercising the broad one, and the consequence was concrete rather than theoretical: `ci.yml` ran
//! `licenses`, `sources` and `bans` and never `advisories`, and `deny.toml` has no `[advisories]`
//! section, so a RUSTSEC advisory could enter `Cargo.lock` with this gate reporting agreement and
//! the README's own annotation calling itself an "advisory ... audit". Requiring an exact match
//! everywhere was rejected — it would break `cargo build --workspace` against CI's
//! `--all-targets` form, which is a genuine extension — so the narrowing is named where it happens:
//! [`is_exercised_by`] refuses to satisfy a bare `cargo deny check` with a sub-check invocation,
//! and [`check_documented_are_run`] satisfies it from the **union** of the sub-checks `ci.yml`
//! runs, reporting by name any of [`DENY_SUBCHECKS`] that no step runs.
//!
//! **Limb 2 is limited to `xtask` subcommands, deliberately.** Requiring *every* `cargo` command in
//! `ci.yml` to appear in the README would demand that the README document a coverage run, three
//! cross-build targets and two benchmark invocations, which is not what NFR-BUILD-020 asks of a
//! README and would push a contributor to satisfy the gate by pasting CI into the document.
//!
//! Both are why NFR-BUILD-020's annotation stays a `trace-partial:` — its `uncovered:` field in
//! `ci.yml` names what is still unmet after this check exists.
//!
//! # No new parser
//!
//! `ci.yml` is read through [`crate::release_workflow::parse`], the block-style YAML subset M13
//! wrote for `release.yml`. A second parser for a second workflow file is exactly the duplication
//! that module's header declined to create for a second dependency, and parsing `ci.yml` here also
//! means a workflow edit that leaves it unparseable fails this check loudly rather than being
//! discovered by a runner.

use std::path::Path;

use crate::release_workflow::{Yaml, jobs, parse, step_run, step_uses};

/// The workflow this check reads, relative to the repository root.
pub const WORKFLOW_PATH: &str = ".github/workflows/ci.yml";

/// The document this check reads, relative to the repository root.
pub const README_PATH: &str = "README.md";

/// The action `ci.yml` runs `cargo deny` through, and the `with:` key carrying its arguments.
///
/// The licence audit is not a `run:` step — it is `EmbarkStudios/cargo-deny-action` with
/// `command: check licenses` — so without this the README's documented `cargo deny check` would
/// read as undocumented-by-CI while CI runs three of its sub-checks. Mapped to the command line it
/// stands for rather than exempted, so the mapping is visible and the extension limb applies to it
/// like any other invocation.
const DENY_ACTION: &str = "EmbarkStudios/cargo-deny-action";

/// Every sub-check a bare `cargo deny check` runs, which is what `README.md` documents.
///
/// `cargo deny`'s own default when `check` is given no argument. Named here rather than inferred,
/// because the whole point is that a documented bare `check` is only exercised when CI's
/// invocations *between them* cover all four — and the one that was missing until M15,
/// `advisories`, is the one whose absence nothing else in the repository would have shown.
pub const DENY_SUBCHECKS: [&str; 4] = ["advisories", "bans", "licenses", "sources"];

/// Commands the README documents that no GitHub-hosted runner can execute, each with the reason it
/// is here. An exemption list rather than a silent skip: this is precisely the residue
/// NFR-BUILD-020's `uncovered:` field has to name, so it is enumerated in one place a reader can
/// find, and adding to it is a visible diff — the array's length being part of its type is what
/// makes a second entry impossible to add quietly.
const UNEXERCISABLE: [(&str, &str); 1] = [(
    "cargo run -p namir-app",
    "the standalone opens an audio input device and a window; no GitHub-hosted runner has either \
     (§22 R-16), the same wall FR-UI-020's and FR-UI-070's manual scripts hit",
)];

/// The remedy every violation ends with, in one place so the check and its tests cannot drift on
/// the exact instruction a reader is given (`identity`'s `WRITE_REMEDY` is the same device).
const REMEDY: &str = "Nothing regenerates this: fix README.md or .github/workflows/ci.yml by hand \
                      so the two agree, or record the command in this module's UNEXERCISABLE \
                      table with its reason.";

/// Every command `README.md` documents: each line beginning `cargo ` inside a fenced code block,
/// trailing ` # comment` removed, in source order and de-duplicated.
///
/// Fenced blocks only, and that is the contract rather than an implementation detail: a command in
/// a fence is one a reader copies and runs, and one in prose (`` `cargo build --release -p
/// namir-app` `` appears in a sentence in the Running section) is describing rather than
/// instructing. Widening this to inline code spans would sweep up fragments and file names.
pub fn documented_commands(readme: &str) -> Vec<String> {
    let mut commands: Vec<String> = Vec::new();
    let mut inside = false;
    for line in readme.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            inside = !inside;
            continue;
        }
        if !inside || !trimmed.starts_with("cargo ") {
            continue;
        }
        let command = strip_trailing_comment(trimmed);
        if !command.is_empty() && !commands.iter().any(|existing| existing == &command) {
            commands.push(command);
        }
    }
    commands
}

/// Every `cargo` command line `ci.yml` runs: one per line of every step's `run:` script, plus the
/// [`DENY_ACTION`] steps rendered as the `cargo deny` command line they stand for.
///
/// # Errors
///
/// Returns a message if the workflow has no `jobs:` mapping.
pub fn workflow_commands(doc: &Yaml) -> Result<Vec<String>, String> {
    let mut commands = Vec::new();
    for job in jobs(doc)? {
        for step in job.steps() {
            if let Some(script) = step_run(step) {
                for line in script.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("cargo ") {
                        commands.push(strip_trailing_comment(trimmed));
                    }
                }
            }
            if step_uses(step).is_some_and(|uses| uses.starts_with(DENY_ACTION))
                && let Some(arguments) = step.get("with").and_then(|w| w.str_at("command"))
            {
                commands.push(format!("cargo deny {}", arguments.trim()));
            }
        }
    }
    Ok(commands)
}

/// Whether the command line is exactly `cargo deny check`, whose extra arguments select a
/// sub-check and therefore *narrow* it rather than extending it.
pub fn is_bare_deny_check(command: &str) -> bool {
    command.split_whitespace().collect::<Vec<_>>() == ["cargo", "deny", "check"]
}

/// Whether `ci` runs `documented`: the same command, or the same command with extra trailing
/// arguments. Token-wise, never by substring — `cargo test --workspace` must not be satisfied by
/// `cargo test --workspace-does-not-exist`, and a prefix test on the raw strings would say it is.
///
/// One exception, and it is inside the predicate rather than at a call site so that no caller can
/// forget it: a bare `cargo deny check` is **not** exercised by `cargo deny check <sub-check>`.
/// Those trailing tokens select one of [`DENY_SUBCHECKS`] and run less than the documented command,
/// not more. [`check_documented_are_run`] is where the union of them is judged instead.
pub fn is_exercised_by(documented: &str, ci: &str) -> bool {
    if is_bare_deny_check(documented) && !is_bare_deny_check(ci) {
        return false;
    }
    let mut documented_tokens = documented.split_whitespace();
    let mut ci_tokens = ci.split_whitespace();
    loop {
        match (documented_tokens.next(), ci_tokens.next()) {
            (None, _) => return true,
            (Some(_), None) => return false,
            (Some(a), Some(b)) if a != b => return false,
            _ => {}
        }
    }
}

/// The `xtask` subcommand a command line invokes, if it is one: `cargo run -p xtask -- layering`
/// yields `layering`. Flags after the subcommand are not part of it — `traceability` and
/// `traceability --allow-uncovered` are the same subcommand documented once.
pub fn xtask_subcommand(command: &str) -> Option<&str> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let separator = tokens.iter().position(|t| *t == "--")?;
    let named_xtask = tokens
        .windows(2)
        .take(separator)
        .any(|pair| pair == ["-p", "xtask"]);
    if !named_xtask || tokens.first() != Some(&"cargo") {
        return None;
    }
    tokens.get(separator + 1).copied()
}

/// The sub-checks of [`DENY_SUBCHECKS`] that no invocation in `ci` runs, empty when a bare
/// `cargo deny check` covers all four at once.
///
/// One invocation may name several (`cargo deny check bans sources` is legal), so the answer is
/// the union over every `cargo deny check ...` line, not a per-line comparison.
pub fn missing_deny_subchecks(ci: &[String]) -> Vec<&'static str> {
    if ci.iter().any(|run| is_bare_deny_check(run)) {
        return Vec::new();
    }
    DENY_SUBCHECKS
        .iter()
        .copied()
        .filter(|sub| {
            !ci.iter().any(|run| {
                let tokens: Vec<&str> = run.split_whitespace().collect();
                tokens.len() > 3
                    && tokens[..3] == ["cargo", "deny", "check"]
                    && tokens[3..].contains(sub)
            })
        })
        .collect()
}

/// Limb 1: every documented command is run by CI, or is in [`UNEXERCISABLE`] with its reason.
///
/// `cargo deny check` is judged by the union rule above rather than by [`is_exercised_by`] — see
/// this module's header for why its sub-check arguments narrow the command instead of extending
/// it, and what that hid until M15.
pub fn check_documented_are_run(documented: &[String], ci: &[String]) -> Vec<String> {
    let mut violations = Vec::new();
    for command in documented {
        if UNEXERCISABLE
            .iter()
            .any(|(exempt, _)| exempt == &command.as_str())
        {
            continue;
        }
        if is_bare_deny_check(command) {
            let missing = missing_deny_subchecks(ci);
            if !missing.is_empty() {
                violations.push(format!(
                    "README.md documents `{command}`, which runs all of {}, but no step in \
                     {WORKFLOW_PATH} runs: {}. A `cargo deny check <sub-check>` step narrows the \
                     documented command rather than extending it, so the documented one is only \
                     exercised when the steps between them cover every sub-check. {REMEDY}",
                    DENY_SUBCHECKS.join(", "),
                    missing.join(", ")
                ));
            }
            continue;
        }
        if !ci.iter().any(|run| is_exercised_by(command, run)) {
            violations.push(format!(
                "README.md documents `{command}`, which no step in {WORKFLOW_PATH} runs. {REMEDY}"
            ));
        }
    }
    violations
}

/// Limb 2: every `xtask` subcommand CI runs is documented in the README.
pub fn check_run_are_documented(documented: &[String], ci: &[String]) -> Vec<String> {
    let documented_subcommands: Vec<&str> = documented
        .iter()
        .filter_map(|c| xtask_subcommand(c))
        .collect();
    let mut violations = Vec::new();
    for subcommand in ci.iter().filter_map(|c| xtask_subcommand(c)) {
        if !documented_subcommands.contains(&subcommand)
            && !violations.iter().any(|v: &String| v.contains(subcommand))
        {
            violations.push(format!(
                "{WORKFLOW_PATH} runs `cargo run -p xtask -- {subcommand}`, which README.md does \
                 not document. {REMEDY}"
            ));
        }
    }
    violations
}

/// Every way `README.md` and `ci.yml` disagree about what this repository's commands are, empty
/// meaning the gate passes.
///
/// `Err` is reserved for an input that cannot be evaluated at all — an unreadable README, an
/// unreadable or unparseable workflow — as distinct from a violation, which is a finding about two
/// files that were both read.
pub fn check(repo_root: &Path) -> Result<Vec<String>, String> {
    let readme_path = repo_root.join(README_PATH);
    let readme = std::fs::read_to_string(&readme_path)
        .map_err(|e| format!("{}: could not be read ({e})", readme_path.display()))?;
    let workflow_path = repo_root.join(WORKFLOW_PATH);
    let workflow_text = std::fs::read_to_string(&workflow_path)
        .map_err(|e| format!("{}: could not be read ({e})", workflow_path.display()))?;
    let doc = parse(&workflow_text).map_err(|e| format!("{}: {e}", workflow_path.display()))?;

    let documented = documented_commands(&readme);
    if documented.is_empty() {
        return Err(format!(
            "{}: no fenced `cargo` command at all -- NFR-BUILD-020's document half is not \
             satisfied and this check has nothing to compare",
            readme_path.display()
        ));
    }
    let ci = workflow_commands(&doc).map_err(|e| format!("{}: {e}", workflow_path.display()))?;

    let mut violations = check_documented_are_run(&documented, &ci);
    violations.extend(check_run_are_documented(&documented, &ci));
    Ok(violations)
}

/// One status line per limb for a passing run's CI log, so the step says what it compared rather
/// than only that it was happy.
pub fn summary(repo_root: &Path) -> Result<Vec<String>, String> {
    let readme = std::fs::read_to_string(repo_root.join(README_PATH))
        .map_err(|e| format!("{README_PATH}: could not be read ({e})"))?;
    let documented = documented_commands(&readme);
    Ok(vec![
        format!(
            "ci-commands: {} command(s) documented in {README_PATH}, {} of them exempt as \
             unrunnable on a CI runner",
            documented.len(),
            documented
                .iter()
                .filter(|c| UNEXERCISABLE.iter().any(|(e, _)| e == &c.as_str()))
                .count()
        ),
        format!(
            "ci-commands: every other one is run by {WORKFLOW_PATH}, and every xtask subcommand {WORKFLOW_PATH} runs is documented"
        ),
    ])
}

/// Removes a trailing ` # ...` comment — the README's gate block annotates each command with one —
/// and trims what is left. A `#` with no space before it is left alone: nothing in either file
/// needs it, and cutting on a bare `#` would truncate a shell variable expansion.
fn strip_trailing_comment(line: &str) -> String {
    match line.find(" #") {
        Some(at) => line[..at].trim().to_string(),
        None => line.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn real_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask's manifest dir always has a parent")
            .to_path_buf()
    }

    #[test]
    fn documented_commands_reads_fenced_lines_and_drops_prose_and_comments() {
        let readme = "\
# Namir

Run `cargo build --release -p namir-app` for a release build.

```bash
cargo test --workspace
cargo run -p xtask -- layering       # crate dependency-graph lint
git config core.hooksPath .githooks
```

```text
cargo test --workspace
```
";
        assert_eq!(
            documented_commands(readme),
            vec![
                "cargo test --workspace".to_string(),
                "cargo run -p xtask -- layering".to_string(),
            ],
            "prose mentions are excluded, trailing comments are cut, and a repeat is not listed \
             twice"
        );
    }

    #[test]
    fn exercised_by_matches_tokens_not_substrings() {
        assert!(is_exercised_by(
            "cargo test --workspace",
            "cargo test --workspace"
        ));
        assert!(is_exercised_by(
            "cargo build --workspace",
            "cargo build --workspace --all-targets"
        ));
        assert!(!is_exercised_by(
            "cargo test --workspace",
            "cargo test --workspace-not-a-flag"
        ));
        assert!(!is_exercised_by(
            "cargo build --workspace --all-targets",
            "cargo build --workspace"
        ));
        assert!(!is_exercised_by(
            "cargo deny check",
            "cargo test --workspace"
        ));
    }

    #[test]
    fn xtask_subcommand_names_the_token_after_the_separator() {
        assert_eq!(
            xtask_subcommand("cargo run -p xtask -- traceability --allow-uncovered"),
            Some("traceability")
        );
        assert_eq!(xtask_subcommand("cargo test --workspace"), None);
        assert_eq!(xtask_subcommand("cargo run -p namir-app"), None);
        assert_eq!(xtask_subcommand("cargo run -p xtask --"), None);
    }

    #[test]
    fn a_documented_command_ci_never_runs_is_a_violation_naming_it() {
        let violations = check_documented_are_run(
            &["cargo run -p xtask -- bundle".to_string()],
            &["cargo test --workspace".to_string()],
        );
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(violations[0].contains("bundle"), "{violations:#?}");
        assert!(violations[0].contains(REMEDY), "{violations:#?}");
    }

    #[test]
    fn an_exempt_command_is_not_a_violation_and_a_neighbouring_one_still_is() {
        let violations = check_documented_are_run(
            &[
                UNEXERCISABLE[0].0.to_string(),
                "cargo run -p xtask -- invented".to_string(),
            ],
            &[],
        );
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(violations[0].contains("invented"), "{violations:#?}");
    }

    #[test]
    fn an_undocumented_xtask_subcommand_in_ci_is_a_violation_reported_once() {
        let violations = check_run_are_documented(
            &["cargo run -p xtask -- layering".to_string()],
            &[
                "cargo run -p xtask -- layering".to_string(),
                "cargo run -p xtask -- rt-logging".to_string(),
                "cargo run -p xtask -- rt-logging".to_string(),
            ],
        );
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(violations[0].contains("rt-logging"), "{violations:#?}");
    }

    /// The `cargo deny` mapping: the licence audit is an action step, not a `run:`, and without
    /// [`DENY_ACTION`] the README's documented `cargo deny check` would read as unexercised.
    #[test]
    fn the_cargo_deny_action_is_read_as_the_command_it_stands_for() {
        let doc = parse(
            "\
jobs:
  license-audit:
    runs-on: ubuntu-latest
    steps:
      - uses: EmbarkStudios/cargo-deny-action@v2
        with:
          command: check licenses
",
        )
        .unwrap();
        let commands = workflow_commands(&doc).unwrap();
        assert_eq!(commands, vec!["cargo deny check licenses".to_string()]);
    }

    /// M15 review, note (a). The assertion above this one used to continue "...and that satisfies
    /// the documented `cargo deny check`", which was the defect: `check licenses` runs one of the
    /// four sub-checks the bare command runs, so certifying it as exercising the bare command let
    /// `ci.yml` skip `advisories` — with `deny.toml` carrying no `[advisories]` section — while
    /// this gate reported the two files in agreement.
    #[test]
    fn a_sub_check_alone_does_not_exercise_the_bare_cargo_deny_check() {
        let violations = check_documented_are_run(
            &["cargo deny check".to_string()],
            &[
                "cargo deny check licenses".to_string(),
                "cargo deny check sources".to_string(),
                "cargo deny check bans".to_string(),
            ],
        );
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(
            violations[0].contains("runs: advisories"),
            "{violations:#?}"
        );
        assert_eq!(
            missing_deny_subchecks(&[
                "cargo deny check licenses".to_string(),
                "cargo deny check sources".to_string(),
                "cargo deny check bans".to_string(),
            ]),
            vec!["advisories"],
            "only the sub-check nothing runs is named"
        );
    }

    /// And the union is what satisfies it: four steps between them, in any order, and one step
    /// naming two sub-checks counts for both.
    #[test]
    fn the_union_of_the_sub_checks_exercises_the_bare_cargo_deny_check() {
        for ci in [
            vec![
                "cargo deny check advisories".to_string(),
                "cargo deny check bans".to_string(),
                "cargo deny check licenses".to_string(),
                "cargo deny check sources".to_string(),
            ],
            vec![
                "cargo deny check bans sources".to_string(),
                "cargo deny check advisories licenses".to_string(),
            ],
            vec!["cargo deny check".to_string()],
        ] {
            assert!(
                check_documented_are_run(&["cargo deny check".to_string()], &ci).is_empty(),
                "{ci:#?}"
            );
            assert!(missing_deny_subchecks(&ci).is_empty(), "{ci:#?}");
        }
    }

    /// The narrowing rule lives inside [`is_exercised_by`] so no caller can forget it, and it is
    /// narrow itself: an ordinary extending flag is still an extension.
    #[test]
    fn a_narrowing_argument_is_refused_where_an_extending_one_is_not() {
        assert!(!is_exercised_by(
            "cargo deny check",
            "cargo deny check bans"
        ));
        assert!(is_exercised_by("cargo deny check", "cargo deny check"));
        assert!(is_exercised_by(
            "cargo build --workspace",
            "cargo build --workspace --all-targets"
        ));
        assert!(is_bare_deny_check("cargo deny check"));
        assert!(!is_bare_deny_check("cargo deny check advisories"));
    }

    /// M14 Phase 5: NFR-BUILD-020's second half against the real pair of files. Its first half —
    /// that the README still *contains* the commands — is `xtask identity`'s, and this is the one
    /// that stops the two files drifting apart, which by M14 they already had.
    ///
    /// The tag is on this test and not on the module's `use` line because this is the artifact
    /// that fails when the drift is real; the module compiling is not evidence of anything.
    // trace-partial: NFR-BUILD-020
    // uncovered: NFR-BUILD-020 — "every product configuration on every supported platform" is
    // uncovered: unspanned: nothing installs either product at its documented install path or
    // uncovered: loads the plugin in a real host, docs/user-guide.md's per-platform install and
    // uncovered: run instructions are read by no check, and this module's UNEXERCISABLE table
    // uncovered: holds one command that is documented but unrunnable on any runner; closes M8
    #[test]
    fn the_readme_and_ci_yml_agree_about_this_repositorys_commands() {
        let violations = check(&real_root()).expect("both files exist and the workflow parses");
        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn a_repository_root_with_no_readme_is_an_error_not_a_violation_list() {
        let dir = std::env::temp_dir().join(format!("xtask-ci-commands-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let err = check(&dir).unwrap_err();
        assert!(err.contains(README_PATH), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
