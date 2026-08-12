//! FR-ERR-020 (Must, `Verify:` **S**): "Every user-visible error shall be drawn from a catalogue
//! with a stable identifier... *Verify:* S — the catalogue **is enumerable** *and* **every error
//! path in the code maps to an entry**."
//!
//! # The conjunct that had no artifact
//!
//! The first conjunct has one: each crate's `error_codes.rs` ends in an `ALL` slice and a test
//! calling `namir_core::assert_unique_ids` over it. That is enumerability, and it works.
//!
//! The second had **nothing**. No check read error paths, and until M14 `namir_core::ErrorCode`'s
//! three fields were all `pub` on an ordinary struct, so any expression anywhere could build a code
//! inline — and two live sites did:
//!
//! - `crates/namir-ui/examples/manual_window_smoke.rs` declared `ui.manual_smoke.example_notice` as
//!   a bare file-level `const`, the instance FR-ERR-020's own `uncovered:` field named;
//! - `crates/namir-app/src/host.rs`'s `AppHost::handle` built `app.host.scan_warning` **inline in
//!   the argument list of a `push_notice` call**, once per warning a finished library scan reports.
//!   That one was not in the field: it is a real, user-visible error path whose code belonged to no
//!   catalogue and would have appeared in no enumeration of one.
//!
//! # What is enforced
//!
//! `namir_core::ErrorCode` is `#[non_exhaustive]` since M14, so outside `namir-core` the only way
//! to build one is `ErrorCode::new` — one greppable token. On top of that, this module requires of
//! every construction:
//!
//! 1. **It is bound to a named `const`.** The `ErrorCode::new(` line, or the non-blank line above
//!    it, must declare `const <NAME>: ErrorCode =`. An anonymous code built in an expression is a
//!    code with no name to enumerate, which is `host.rs`'s defect exactly.
//! 2. **It is inside a catalogue.** Either the file is a crate's `error_codes.rs` (or
//!    `namir-core`'s own `error.rs`, where the type lives), or the construction is inside a
//!    `mod ...error_codes { ... }` block — the shape `namir-app`'s `local_error_codes` already had
//!    and the shape the UI example was given.
//!
//! Together those make "every error path maps to a catalogue entry" mechanical in the only
//! direction a static check can make it: an error path can name a catalogue `const` or it can fail
//! the build.
//!
//! # Residual blind spots, stated rather than pretended closed
//!
//! - **It does not verify that a catalogue `const` is in its crate's `ALL` slice.** A `const`
//!   declared in `error_codes.rs` and left out of `ALL` is enumerable-in-principle and unenumerated
//!   in fact. Checking it needs to resolve names, not lines. What is closed is the larger hole:
//!   nothing can be built *outside* a catalogue module at all.
//! - **`#[cfg(test)]` regions are skipped**, and so are `tests/`, `benches/` and `fuzz/`
//!   directories. Test fixtures build throwaway codes on purpose (`namir-platform`'s logging tests
//!   need one per severity) and none of them is a product error path. Examples are **in** scope:
//!   an example runs the real UI and a reader takes it as a model.
//! - **Line-based**, like every scanner in this crate. Comment lines are skipped, so this file's
//!   own prose does not trip it.

use std::path::Path;

/// File stems that are a catalogue by virtue of what the file is: each crate's `error_codes.rs`,
/// and `namir-core`'s `error.rs`, which defines the type itself.
const CATALOGUE_FILE_STEMS: &[&str] = &["error_codes", "error"];

/// Directory names whose contents are out of scope — see the module doc's second residual.
const SKIPPED_DIRS: &[&str] = &["tests", "benches", "fuzz", "target"];

/// The construction token every `ErrorCode` now goes through, `#[non_exhaustive]` having removed
/// the struct-literal form outside `namir-core`. The literal form is looked for too, so a
/// regression inside `namir-core` — the one crate where it would still compile — is caught rather
/// than being the one place the rule does not reach.
const CONSTRUCTION_TOKENS: &[&str] = &["ErrorCode::new(", "ErrorCode {"];

/// Whether a file at `path` should be scanned at all.
pub fn is_scanned(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("rs") {
        return false;
    }
    !path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| SKIPPED_DIRS.contains(&s))
    })
}

/// Whether `path`'s own name makes the whole file a catalogue.
fn is_catalogue_file(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| CATALOGUE_FILE_STEMS.contains(&stem))
}

/// Whether `line` opens a module whose name marks it a catalogue (`error_codes`,
/// `local_error_codes`, ...).
fn opens_catalogue_module(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("mod ").or_else(|| {
        trimmed
            .strip_prefix("pub mod ")
            .or_else(|| trimmed.strip_prefix("pub(crate) mod "))
    }) else {
        return false;
    };
    let name = rest.trim_end_matches(['{', ' ']).trim();
    name.ends_with("error_codes") && line.contains('{')
}

/// Whether `line` declares a `const` of type `ErrorCode`.
fn declares_error_code_const(line: &str) -> bool {
    let trimmed = line.trim_start();
    (trimmed.starts_with("const ")
        || trimmed.starts_with("pub const ")
        || trimmed.starts_with("pub(crate) const "))
        && trimmed.contains(": ErrorCode =")
}

/// Scans one file's `source` for constructions that break either rule. `path` decides whether the
/// whole file counts as a catalogue. Returns one message per problem, each prefixed with the
/// 1-indexed line number, so the caller only has to add the path.
///
/// Pure string logic so it is unit-testable without a filesystem; [`crate::main`] applies it to the
/// real tree.
pub fn scan(path: &Path, source: &str) -> Vec<String> {
    let file_is_catalogue = is_catalogue_file(path);
    let lines: Vec<&str> = source.lines().collect();
    let mut problems = Vec::new();

    // Brace-depth bookkeeping. `skip_until`/`catalogue_until` each hold the depth the enclosing
    // block was opened *at*; the region ends when depth returns to it.
    let mut depth: i32 = 0;
    let mut pending_cfg_test = false;
    let mut skip_until: Option<i32> = None;
    let mut catalogue_until: Option<i32> = None;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let depth_before = depth;
        for ch in line.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }

        if trimmed.starts_with("//") {
            continue;
        }

        if pending_cfg_test && depth > depth_before {
            skip_until = Some(depth_before);
            pending_cfg_test = false;
        } else if trimmed.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
        }
        if opens_catalogue_module(line) && catalogue_until.is_none() {
            catalogue_until = Some(depth_before);
        }

        let in_skipped = skip_until.is_some_and(|d| depth_before > d || depth > d);
        if !in_skipped && CONSTRUCTION_TOKENS.iter().any(|t| line.contains(t)) {
            // Three shapes name the type without building one: its own declaration, an `impl`
            // block (`impl ErrorCode {`, `impl Display for ErrorCode {`), and a function
            // returning it (`-> ErrorCode {`).
            let is_definition = trimmed.starts_with("impl ")
                || line.contains("struct ErrorCode")
                || line.contains("-> ErrorCode");
            if !is_definition {
                let bound = declares_error_code_const(line)
                    || lines[..idx]
                        .iter()
                        .rev()
                        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("//"))
                        .is_some_and(|l| declares_error_code_const(l));
                if !bound {
                    problems.push(format!(
                        "{}: builds an ErrorCode that is not bound to a named `const` -- an \
                         anonymous code has no name for any catalogue to enumerate (FR-ERR-020). \
                         Declare it as a `const` in this crate's catalogue and name it here",
                        idx + 1
                    ));
                } else if !file_is_catalogue && catalogue_until.is_none() {
                    problems.push(format!(
                        "{}: declares an ErrorCode outside a catalogue -- move it into this \
                         crate's `error_codes.rs`, or into a `mod ...error_codes {{ }}` block in \
                         this file, so it is part of an enumerable catalogue (FR-ERR-020)",
                        idx + 1
                    ));
                }
            }
        }

        if let Some(d) = skip_until
            && depth <= d
        {
            skip_until = None;
        }
        if let Some(d) = catalogue_until
            && depth <= d
        {
            catalogue_until = None;
        }
    }

    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(path: &str, source: &str) -> Vec<String> {
        scan(Path::new(path), source)
    }

    #[test]
    fn a_const_in_a_catalogue_file_is_clean() {
        let source = "use namir_core::{ErrorCode, Severity};\n\
                      pub const A: ErrorCode = ErrorCode::new(\"a.b\", Severity::Error, \"t\");\n";
        assert!(src("crates/namir-nam/src/error_codes.rs", source).is_empty());
    }

    #[test]
    fn a_const_wrapped_onto_the_next_line_is_clean() {
        // rustfmt's output when the declaration does not fit: the `ErrorCode::new(` lands on the
        // line below the `const`.
        let source = "pub const SCAN_WARNING: ErrorCode =\n    ErrorCode::new(\"a.b\", Severity::Warning, \"{detail}\");\n";
        assert!(src("crates/namir-app/src/error_codes.rs", source).is_empty());
    }

    #[test]
    fn an_inline_construction_in_an_expression_is_flagged() {
        // `namir-app/src/host.rs`'s live defect, in the shape it had: a code built in the argument
        // list of the call that reports it.
        let source = "fn handle(&mut self) {\n    self.push_notice(\n        ErrorCode::new(\"app.host.scan_warning\", Severity::Warning, \"{detail}\"),\n        warning,\n    );\n}\n";
        let problems = src("crates/namir-app/src/host.rs", source);
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert!(problems[0].starts_with("3: builds"), "{problems:#?}");
    }

    #[test]
    fn a_named_const_outside_any_catalogue_is_flagged() {
        // `manual_window_smoke.rs`'s live defect: a real name, no catalogue.
        let source = "const SAMPLE_NOTICE: ErrorCode = ErrorCode::new(\"ui.x\", Severity::Warning, \"t\");\n";
        let problems = src("crates/namir-ui/examples/manual_window_smoke.rs", source);
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert!(problems[0].contains("outside a catalogue"), "{problems:#?}");
    }

    #[test]
    fn the_same_const_inside_a_catalogue_module_is_clean() {
        // The fix that example was given, and the shape `namir-app`'s `local_error_codes` already
        // had.
        let source = "mod error_codes {\n    use namir_core::{ErrorCode, Severity};\n    pub const SAMPLE: ErrorCode = ErrorCode::new(\"ui.x\", Severity::Warning, \"t\");\n}\n";
        assert!(src("crates/namir-ui/examples/manual_window_smoke.rs", source).is_empty());
        let local = "mod local_error_codes {\n    pub const A: ErrorCode = ErrorCode::new(\"a\", Severity::Error, \"t\");\n}\n";
        assert!(src("crates/namir-app/src/host.rs", local).is_empty());
    }

    #[test]
    fn a_catalogue_module_does_not_stay_open_past_its_closing_brace() {
        // The bookkeeping that makes the rule mean anything: a construction *after* the module
        // must not inherit its exemption.
        let source = "mod error_codes {\n    pub const A: ErrorCode = ErrorCode::new(\"a\", Severity::Error, \"t\");\n}\n\nconst B: ErrorCode = ErrorCode::new(\"b\", Severity::Error, \"t\");\n";
        let problems = src("crates/namir-app/src/host.rs", source);
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert!(problems[0].starts_with("5:"), "{problems:#?}");
    }

    #[test]
    fn a_test_fixture_is_skipped_and_the_skip_ends_with_the_module() {
        // `namir-platform/src/logging.rs` and `namir-ui/src/notices.rs` both declare throwaway
        // codes inside `#[cfg(test)] mod tests`. Neither is a product error path.
        let source = "#[cfg(test)]\nmod tests {\n    const INFO: ErrorCode = ErrorCode::new(\"t.i\", Severity::Info, \"\");\n}\n\nconst LEAKED: ErrorCode = ErrorCode::new(\"x\", Severity::Error, \"t\");\n";
        let problems = src("crates/namir-platform/src/logging.rs", source);
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert!(problems[0].starts_with("6:"), "{problems:#?}");
    }

    #[test]
    fn the_declaration_an_impl_block_and_a_returning_function_are_not_constructions() {
        let source = "pub struct ErrorCode {\n    pub id: &'static str,\n}\n\
                      impl ErrorCode {\n    pub const fn new() -> Self { Self { id: \"\" } }\n}\n\
                      pub fn stream_failure_code(d: Direction) -> ErrorCode {\n    DEVICE_LOST\n}\n";
        assert!(src("crates/namir-core/src/error.rs", source).is_empty());
    }

    #[test]
    fn prose_about_a_construction_is_not_flagged() {
        let source = "// Never write ErrorCode::new(..) here.\n/// See `ErrorCode {` for the old form.\nlet x = 1;\n";
        assert!(src("crates/namir-app/src/host.rs", source).is_empty());
    }

    #[test]
    fn the_scanned_set_excludes_tests_benches_and_fuzz() {
        assert!(is_scanned(Path::new("crates/namir-app/src/host.rs")));
        assert!(is_scanned(Path::new(
            "crates/namir-ui/examples/manual_window_smoke.rs"
        )));
        assert!(!is_scanned(Path::new(
            "crates/namir-platform/tests/logging.rs"
        )));
        assert!(!is_scanned(Path::new("crates/namir-ir/benches/tail.rs")));
        assert!(!is_scanned(Path::new(
            "crates/namir-ir/fuzz/fuzz_targets/load_ir.rs"
        )));
        assert!(!is_scanned(Path::new("crates/namir-app/Cargo.toml")));
    }
}
