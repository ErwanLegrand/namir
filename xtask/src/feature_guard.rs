//! §22 **R-17** (issue #25): "**Adding `--all-features` to any build or release command silently
//! links `clack-host` into the cdylib**", and the row's own mitigation says in as many words that
//! "nothing mechanical guards the linkage itself". This module is the mechanism that now does.
//!
//! # What the risk actually is
//!
//! D-18.7 puts `clack-extensions`' `clack-host` feature behind `namir-clap`'s **non-default**
//! feature `host-ext-tests`, which keeps it out of the default-feature resolve `xtask attribution`
//! walks and out of the shipped `cdylib`. That confinement is a property of every *invocation*, not
//! of the manifest: one `--all-features` in a release step turns the feature on for the `cdylib`
//! too, and the shipped plugin then links a host library while `THIRD-PARTY-NOTICES.md` — generated
//! from the default-feature resolve — no longer describes the artifact. R-17's error direction is a
//! real dependency entering a shipped binary, which is why it is guarded rather than merely watched.
//!
//! The only thing standing between the repository and that outcome was a written discipline
//! ("`--all-features` never appears in a build or release command in this repository") and a *late*
//! detector: `xtask attribution` goes red on the next merge, as an attribution error rather than as
//! a linkage error. A discipline nothing checks is a discipline until the first person who has not
//! read the row.
//!
//! # The two halves of the guard
//!
//! 1. **[`scan_for_all_features`]** — no `cargo` invocation in any of the repository's
//!    command-carrying files may pass `--all-features`. Blanket rather than restricted to
//!    `cargo build`, because that is the discipline R-17 states, and because the alternative reading
//!    ("only a *shipping* build matters") requires a line-based scanner to know which cargo
//!    subcommand a wrapped YAML `run:` block is invoking. The named alternative already exists and
//!    is already used: `--features host-ext-tests`.
//! 2. **[`NON_DEFAULT_FEATURES`] / [`check_feature_stays_non_default`]** — the manifest half. A
//!    feature listed there must not be reachable from `default`, and the dependency it gates must
//!    stay a dev-dependency. `--all-features` is not the only way to ship it: adding
//!    `default = ["host-ext-tests"]` would do it with no command-line change at all, and the
//!    `[features]` table's own comment says to re-read R-17 "whenever `namir-clap`'s `[features]`
//!    table gains a second entry".
//!
//! # Residual blind spots, stated rather than pretended closed
//!
//! - **Line-based**, like `layering`'s and `rt_logging`'s scanners. Lines whose trimmed form begins
//!   `#`, `//` or `;` are skipped, so prose about the flag does not trip it — load-bearing, since
//!   `.github/workflows/ci.yml:115` is a comment reading "**Never `--all-features`**" and the
//!   architecture document's R-17 row quotes the flag repeatedly.
//! - **A flag assembled at run time is invisible** (`FLAGS="--all-features"` then `cargo build
//!   $FLAGS`, or a workflow input interpolated into a `run:` line). So is one passed by a developer
//!   at their own terminal, which no in-repository check can see.
//! - **The scanned roots are hand-maintained** ([`COMMAND_ROOTS`]), the same device
//!   `rt_logging::AUDIO_THREAD_MODULES` uses and for the same reason: there is no machine-readable
//!   statement of which files carry build commands. A root that has gone missing is reported as a
//!   violation rather than skipped, so a rename fails the gate loudly.
//! - **It does not read the resolved dependency graph.** A direct answer — "does the `cdylib` this
//!   command produces link `clack_host`?" — is the per-shipped-path feature resolution R-15 and
//!   R-17 both nominate as the durable fix, and is out of scope here.

use std::path::Path;

/// Directories whose files carry build, test or packaging commands. Repo-root-relative, forward
/// slashes. Every file beneath each is scanned except `.md` documentation, which is prose about
/// commands rather than commands.
pub const COMMAND_ROOTS: &[(&str, &str)] = &[
    (".github", "holds every CI and release workflow"),
    (
        "packaging",
        "holds each platform's installer script, which invokes cargo to produce what it packages",
    ),
    (
        ".githooks",
        "holds the pre-commit hook's own cargo commands",
    ),
];

/// The flag R-17 names.
const FORBIDDEN_FLAG: &str = "--all-features";

/// Whether `line` is a comment in any of the three syntaxes the scanned roots use: YAML/shell `#`,
/// Inno Setup `;`, and `//` for completeness.
fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#') || trimmed.starts_with(';') || trimmed.starts_with("//")
}

/// Scans `source` for a `cargo` invocation passing [`FORBIDDEN_FLAG`], line by line, skipping
/// comments. Returns the 1-indexed line numbers. Pure string logic so it is unit-testable without a
/// filesystem; [`crate::main`] applies it to the real files.
pub fn scan_for_all_features(source: &str) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            !is_comment(line) && line.contains("cargo") && line.contains(FORBIDDEN_FLAG)
        })
        .map(|(idx, _)| idx + 1)
        .collect()
}

/// A feature that must stay non-default, the dependency it gates, and why. Hand-maintained; see the
/// module doc's residuals. `(crate manifest path, feature, gated dependency, why)`.
pub const NON_DEFAULT_FEATURES: &[(&str, &str, &str, &str)] = &[(
    "crates/namir-clap/Cargo.toml",
    "host-ext-tests",
    "clack-host",
    "D-18.7 keeps `clack-extensions`' host halves out of the shipped cdylib and out of the \
     default-feature resolve `xtask attribution` walks (R-17)",
)];

/// Checks one [`NON_DEFAULT_FEATURES`] entry against `manifest`'s text. Returns one message per
/// problem — empty means clean.
///
/// Two properties, both line-based over the manifest rather than parsed, deliberately: a TOML
/// parser would need a dependency this workspace does not otherwise want, and the shapes being
/// looked for are single lines in a hand-written file.
///
/// 1. The `[features]` table declares no `default` key. Not "no `default` naming this feature" —
///    any default feature can enable another, and a check that reasons about one level of that
///    reads as a guarantee it does not give. A crate that genuinely needs a default feature will
///    fail this and the reviewer will have to say why, which is the correct outcome for the one
///    crate D-18.7 governs.
/// 2. The gated dependency appears only after `[dev-dependencies]`. A dev-dependency is linked into
///    no shipped artifact whatever features are on; the same entry under `[dependencies]` is linked
///    into every one.
pub fn check_feature_stays_non_default(
    manifest: &str,
    feature: &str,
    gated_dependency: &str,
) -> Vec<String> {
    let mut violations = Vec::new();
    let mut section = "";
    let mut dependency_sections: Vec<&str> = Vec::new();

    for line in manifest.lines() {
        let trimmed = line.trim();
        if is_comment(trimmed) {
            continue;
        }
        if trimmed.starts_with('[') {
            section = trimmed;
            continue;
        }
        if section == "[features]" && trimmed.starts_with("default") {
            violations.push(format!(
                "declares a `default` feature ({trimmed}); `{feature}` must stay unreachable from \
                 `default`, and a check that traced one level of feature enabling would read as a \
                 guarantee it cannot give"
            ));
        }
        if section.contains("dependencies")
            && trimmed
                .split_once('=')
                .is_some_and(|(name, _)| name.trim() == gated_dependency)
        {
            dependency_sections.push(section);
        }
    }

    if dependency_sections.is_empty() {
        violations.push(format!(
            "declares no `{gated_dependency}` dependency at all -- this guard's subject has moved \
             or been renamed; update xtask's NON_DEFAULT_FEATURES by hand rather than deleting the \
             entry"
        ));
    }
    for section in dependency_sections {
        if section != "[dev-dependencies]" {
            violations.push(format!(
                "declares `{gated_dependency}` under `{section}`, not `[dev-dependencies]` -- it \
                 would then be linked into the shipped artifact whatever features are selected"
            ));
        }
    }

    violations
}

/// Every file under `root` that carries commands: not a directory, not `.md`.
pub fn command_files(root: &Path) -> Vec<std::path::PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_none_or(|ext| ext != "md"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_command_with_the_flag_is_flagged() {
        let source =
            "      - name: build\n        run: cargo build --release --workspace --all-features\n";
        assert_eq!(scan_for_all_features(source), vec![2]);
    }

    #[test]
    fn a_test_command_with_the_flag_is_flagged_too() {
        // Blanket, not restricted to `cargo build` -- see the module doc for why, and note that
        // `cargo test --all-features` builds the cdylib target as well.
        let source = "run: cargo test --workspace --all-features\n";
        assert_eq!(scan_for_all_features(source), vec![1]);
    }

    #[test]
    fn the_named_single_feature_invocation_is_clean() {
        // What `.github/workflows/ci.yml` actually runs, and the alternative every violation
        // message should push someone towards.
        let source = "run: cargo test -p namir-clap --features host-ext-tests\n";
        assert!(scan_for_all_features(source).is_empty());
    }

    #[test]
    fn a_comment_warning_against_the_flag_is_not_flagged() {
        // Load-bearing: `.github/workflows/ci.yml:115` is exactly this comment, and a guard that
        // tripped on the warning against the thing it guards would be uninstallable.
        let source = "      # It must stay a named, single-feature invocation. **Never `--all-features`**: that\n      # would switch the feature on for the shipped cdylib (cargo build).\n";
        assert!(scan_for_all_features(source).is_empty());
    }

    #[test]
    fn the_flag_without_a_cargo_invocation_is_not_flagged() {
        // `deny.toml`'s `[graph] all-features = true` is a different key in a different tool, and
        // auditing licences across every feature is correct. This is the shape that keeps a
        // hypothetical similar line elsewhere from tripping the guard.
        let source = "all-features = true\n";
        assert!(scan_for_all_features(source).is_empty());
    }

    /// `namir-clap`'s manifest reduced to the lines this check reads.
    fn manifest(features: &str, clack_host_section: &str) -> String {
        format!(
            "[package]\nname = \"namir-clap\"\n\n\
             [features]\n{features}\n\n\
             [dependencies]\nclack-plugin = \"0.1.1\"\n\n\
             {clack_host_section}\nclack-host = {{ version = \"0.1.1\" }}\n"
        )
    }

    #[test]
    fn the_shipping_manifest_shape_is_clean() {
        let text = manifest(
            "host-ext-tests = [\"clack-extensions/clack-host\"]",
            "[dev-dependencies]",
        );
        assert!(
            check_feature_stays_non_default(&text, "host-ext-tests", "clack-host").is_empty(),
            "{:#?}",
            check_feature_stays_non_default(&text, "host-ext-tests", "clack-host")
        );
    }

    #[test]
    fn a_default_feature_is_flagged() {
        // The way R-17 can fire with no command-line change at all.
        let text = manifest(
            "default = [\"host-ext-tests\"]\nhost-ext-tests = [\"clack-extensions/clack-host\"]",
            "[dev-dependencies]",
        );
        let violations = check_feature_stays_non_default(&text, "host-ext-tests", "clack-host");
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(violations[0].contains("default"), "{violations:#?}");
    }

    #[test]
    fn promoting_the_gated_dependency_to_a_normal_one_is_flagged() {
        let text = manifest(
            "host-ext-tests = [\"clack-extensions/clack-host\"]",
            "[dependencies]",
        );
        let violations = check_feature_stays_non_default(&text, "host-ext-tests", "clack-host");
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(violations[0].contains("[dependencies]"), "{violations:#?}");
    }

    #[test]
    fn a_vanished_dependency_is_a_violation_not_a_pass() {
        // The list is hand-maintained, so a rename must fail loudly rather than quietly
        // un-guarding the crate -- the same rule `rt_logging` applies to an unreadable module.
        let text = "[package]\nname = \"namir-clap\"\n\n[features]\nhost-ext-tests = []\n";
        let violations = check_feature_stays_non_default(text, "host-ext-tests", "clack-host");
        assert_eq!(violations.len(), 1, "{violations:#?}");
        assert!(violations[0].contains("no `clack-host`"), "{violations:#?}");
    }

    #[test]
    fn the_entry_list_is_non_empty_and_repo_relative() {
        for (path, feature, dep, why) in NON_DEFAULT_FEATURES {
            assert!(path.starts_with("crates/"), "{path}");
            assert!(!path.contains('\\'), "{path}");
            assert!(
                !feature.is_empty() && !dep.is_empty() && !why.is_empty(),
                "{path}"
            );
        }
        for (root, why) in COMMAND_ROOTS {
            assert!(!root.contains('\\'), "{root}");
            assert!(!why.is_empty(), "{root}");
        }
    }
}
