//! NFR-QUAL-010 (Must): "Every requirement [...] marked Must shall be covered by at least one
//! automated test, except where the Verify field states M, in which case it shall be covered by
//! a written manual test script." FRS §10 spells out a three-level mapping (architecture doc ->
//! component, `03-test-plan.md` -> test identifiers, test source -> machine-readable annotation).
//! `docs/03-test-plan.md` never existed (the roadmap document took the "03" slot instead) -- this
//! module generates it rather than asking anyone to hand-maintain a document that would drift the
//! moment a test moved, following the same generate-and-diff precedent `params.lock` already
//! established. See `docs/01-functional-requirements.md` §10's `*Consequence (added M7)*` note and
//! `docs/02-architecture.md` §23's matching note for the recorded scope of what this mechanism
//! actually promises (crate-granularity component mapping, not module/function-granularity).
//!
//! **Coverage convention**: a Must requirement with `Verify: M` is covered by a
//! `docs/manual-tests/<lowercase-id>-*.md` file (already true for the 17 that exist). A
//! requirement with `Verify: Process` (found missing from FRS §1.5's own legend while building
//! this check -- fixed there at M7; NFR-QUAL-020 is the FRS's one user of it) is, by definition,
//! verified by review and commit order, not by any artifact a build can inspect -- there is
//! nothing to mechanically trace, so it is treated as satisfied without a source or manual-test
//! lookup. Every other Must requirement is covered by either (a) a `// trace: FR-XXX-NNN[, ...]`
//! comment on the line before the covering `#[test]`/`#[bench]`/static-check item, or (b) the id
//! embedded in the covering test function's own name in the pre-existing `fr_xxx_nnn_...`
//! convention (so the 4 tests already written that way need no changes). This module's job is
//! pure parsing/matching logic, kept testable against synthetic strings with no filesystem access
//! -- `main.rs` supplies the real FRS text, manual-test filenames, and source file contents.

// trace: NFR-QUAL-010

use std::collections::HashMap;

/// One `Must`-priority requirement parsed from the FRS, paired with its `*Verify:*` code
/// (`U`/`I`/`G`/`B`/`S`/`M` per FRS §1.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub id: String,
    pub verify: char,
}

/// Parses every `**FR-*/NFR-* (Must)**` requirement out of the FRS's raw text, paired with the
/// `*Verify:*` code on the next line that carries one (a table, code block, or `*Rationale:*` line
/// may sit between the requirement's own line and its `*Verify:*` line -- both are skipped over,
/// not treated as a parse boundary). A requirement line with no `*Verify:*` found before either
/// the next requirement or end of file is a malformed-FRS error, surfaced rather than silently
/// dropped, since a silently-skipped requirement would defeat the whole point of this check.
pub fn parse_must_requirements(frs_text: &str) -> Result<Vec<Requirement>, String> {
    let lines: Vec<&str> = frs_text.lines().collect();
    let mut out = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        if let Some(id) = extract_must_id(lines[i]) {
            let mut verify = None;
            let mut j = i + 1;
            while j < lines.len() {
                if extract_must_id(lines[j]).is_some() {
                    break;
                }
                if let Some(v) = extract_verify_code(lines[j]) {
                    verify = Some(v);
                    break;
                }
                j += 1;
            }
            match verify {
                Some(verify) => out.push(Requirement { id, verify }),
                None => {
                    return Err(format!(
                        "no *Verify:* line found for {id} before the next requirement or end of \
                         file -- the FRS is malformed, or this parser's assumptions about its \
                         layout no longer hold"
                    ));
                }
            }
        }
        i += 1;
    }

    Ok(out)
}

/// `"**FR-CHAIN-010 (Must)** — ..."` -> `Some("FR-CHAIN-010")`. `None` for `(Should)`/`(Could)`/
/// `(Won't)` lines, and for anything not starting with a bolded `FR-`/`NFR-` id.
fn extract_must_id(line: &str) -> Option<String> {
    let rest = line.strip_prefix("**")?;
    let end = rest.find("**")?;
    let inside = &rest[..end];
    let (id_part, tag_part) = inside.split_once(" (")?;
    if tag_part.trim_end_matches(')') != "Must" {
        return None;
    }
    if !(id_part.starts_with("FR-") || id_part.starts_with("NFR-")) {
        return None;
    }
    Some(id_part.to_string())
}

/// `"*Verify:* U — measure ..."` -> `Some('U')`.
fn extract_verify_code(line: &str) -> Option<char> {
    const MARKER: &str = "*Verify:*";
    let idx = line.find(MARKER)?;
    line[idx + MARKER.len()..]
        .trim_start()
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
}

/// Both comment-prefix spellings the `trace:` annotation may use: `// trace:` in `.rs` source,
/// `# trace:` in `.yml`/`.toml` config -- a real, non-trivial slice of Must requirements (MSRV,
/// clippy-as-error, cargo-deny, mobile/no-C++ builds, network-free) are verified entirely by CI
/// workflow/build configuration, not by any Rust test function, and would be permanently
/// unresolvable without this.
const TRACE_MARKERS: [&str; 2] = ["// trace:", "# trace:"];

/// Extracts every id from every `trace: ID[, ID...]` comment in `source` (one file's text),
/// recognizing either [`TRACE_MARKERS`] spelling.
pub fn trace_annotations(source: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in source.lines() {
        for marker in TRACE_MARKERS {
            if let Some(idx) = line.find(marker) {
                for token in line[idx + marker.len()..].split(',') {
                    let id = token.trim();
                    if !id.is_empty() {
                        ids.push(id.to_string());
                    }
                }
                break;
            }
        }
    }
    ids
}

/// True if `source` defines a function whose name embeds `id` in the pre-existing
/// `fr_xxx_nnn_description`/`nfr_xxx_nnn_description` snake-case convention (e.g. `FR-NAM-070` ->
/// `fn fr_nam_070_...`). Deliberately requires a `_` or `(` immediately after the id's snake form,
/// not a bare substring match, so `FR-IO-010` does not spuriously match a hypothetical
/// `fr_io_0100_...`.
pub fn fn_name_embeds_id(source: &str, id: &str) -> bool {
    let snake = id.to_lowercase().replace('-', "_");
    source.contains(&format!("fn {snake}_")) || source.contains(&format!("fn {snake}("))
}

/// `"FR-IO-020"` -> `"fr-io-020"`, the filename prefix `docs/manual-tests/` files use.
pub fn manual_test_prefix(id: &str) -> String {
    id.to_lowercase()
}

/// The outcome of checking every Must requirement against real manual-test filenames and real
/// source-file hits. `source_hits`/`manual_hits` are `id -> [crate name]` / `id -> filename` for
/// requirements that *are* covered, kept for `render_test_plan`; `missing` is every Must id this
/// run found no coverage for at all.
pub struct Report {
    pub missing: Vec<Requirement>,
    pub manual_hits: HashMap<String, String>,
    pub source_hits: HashMap<String, Vec<String>>,
}

/// Reconciles `requirements` against `manual_test_docs` (every real `(filename, content)` pair
/// under `docs/manual-tests/`, e.g. `("fr-io-020-wasapi-exclusive-mode.md", "...")`) and
/// `source_hits` (every id this run found a `// trace:` annotation or matching test-fn name for,
/// already resolved to the crate name(s) it was found in by the caller).
///
/// A manual-test file matches a `Verify: M` requirement if either its filename starts with the
/// id's lowercase prefix (the usual one-file-per-requirement case) *or* its content contains the
/// literal id (a file documented as covering more than one requirement in its
/// `**Requirement (literal):**` line, e.g. `fr-io-010-device-enumeration.md` also covering
/// FR-IO-040, is real and must not be missed just because its filename only names the first one).
pub fn build_report(
    requirements: &[Requirement],
    manual_test_docs: &[(String, String)],
    source_hits: &HashMap<String, Vec<String>>,
) -> Report {
    let mut missing = Vec::new();
    let mut manual_hits = HashMap::new();

    for req in requirements {
        if req.verify == 'M' {
            let prefix = format!("{}-", manual_test_prefix(&req.id));
            match manual_test_docs.iter().find(|(name, content)| {
                name.to_lowercase().starts_with(&prefix) || content.contains(&req.id)
            }) {
                Some((file, _)) => {
                    manual_hits.insert(req.id.clone(), file.clone());
                }
                None => missing.push(req.clone()),
            }
        } else if req.verify == 'P' {
            // Process-verified: by definition, verified by review/commit order, not by any
            // artifact this check can inspect. Nothing to look up; never "missing".
        } else if !source_hits.contains_key(&req.id) {
            missing.push(req.clone());
        }
    }

    Report {
        missing,
        manual_hits,
        source_hits: source_hits.clone(),
    }
}

/// Renders the generated `docs/03-test-plan.md` body. Sorted by id for a stable, diffable file.
pub fn render_test_plan(requirements: &[Requirement], report: &Report) -> String {
    let mut sorted: Vec<&Requirement> = requirements.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    let mut out = String::from(
        "# Test plan\n\n\
         Machine-generated by `cargo run -p xtask -- traceability --write` (NFR-QUAL-010, FRS §10). \
         Do not hand-edit -- regenerate instead. Maps every Must-priority requirement to how it is \
         verified: a manual-test document (`Verify: M`) or the crate(s) whose test source carries a \
         `// trace:` annotation or matching test-function name for it (`Verify: U/I/G/B/S`). A \
         requirement listed under \"UNRESOLVED\" has neither -- `cargo run -p xtask -- traceability` \
         fails the build while any remain.\n\n\
         | Requirement | Verify | Covered by |\n\
         |---|---|---|\n",
    );

    for req in &sorted {
        let covered_by = if req.verify == 'M' {
            report
                .manual_hits
                .get(&req.id)
                .map(|f| format!("`docs/manual-tests/{f}`"))
                .unwrap_or_else(|| "**UNRESOLVED**".to_string())
        } else if req.verify == 'P' {
            "process (review + commit order, not build-inspectable)".to_string()
        } else if let Some(crates) = report.source_hits.get(&req.id) {
            let mut names = crates.clone();
            names.sort();
            names.dedup();
            names
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            "**UNRESOLVED**".to_string()
        };
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            req.id, req.verify, covered_by
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_must_requirement() {
        let frs = "**FR-CHAIN-090 (Must)** — text.\n*Verify:* U.\n";
        let reqs = parse_must_requirements(frs).unwrap();
        assert_eq!(
            reqs,
            vec![Requirement {
                id: "FR-CHAIN-090".into(),
                verify: 'U'
            }]
        );
    }

    #[test]
    fn ignores_should_and_could_and_wont() {
        let frs = "**FR-X-010 (Should)** — a.\n*Verify:* U.\n\
                   **FR-X-020 (Could)** — b.\n*Verify:* U.\n\
                   **FR-X-030 (Won't)** — c.\n*Verify:* U.\n";
        assert!(parse_must_requirements(frs).unwrap().is_empty());
    }

    #[test]
    fn skips_a_table_and_rationale_between_id_and_verify() {
        let frs = "**FR-CHAIN-060 (Must)** — text.\n\n\
                   | a | b |\n|---|---|\n| 1 | 2 |\n\n\
                   *Rationale:* something.\n\
                   *Verify:* I per configuration.\n";
        let reqs = parse_must_requirements(frs).unwrap();
        assert_eq!(
            reqs,
            vec![Requirement {
                id: "FR-CHAIN-060".into(),
                verify: 'I'
            }]
        );
    }

    #[test]
    fn takes_the_first_code_when_verify_lists_more_than_one() {
        let frs = "**FR-CHAIN-020 (Must)** — text.\n\
                   *Verify:* U per stage; I for click-freedom.\n";
        let reqs = parse_must_requirements(frs).unwrap();
        assert_eq!(reqs[0].verify, 'U');
    }

    #[test]
    fn missing_verify_line_is_an_error_not_a_silent_drop() {
        let frs = "**FR-X-010 (Must)** — text with no Verify line.\n\
                   **FR-X-020 (Must)** — the next requirement.\n\
                   *Verify:* U.\n";
        let err = parse_must_requirements(frs).unwrap_err();
        assert!(err.contains("FR-X-010"));
    }

    #[test]
    fn trace_annotation_parses_one_id() {
        assert_eq!(
            trace_annotations("// trace: FR-NAM-070\n"),
            vec!["FR-NAM-070"]
        );
    }

    #[test]
    fn trace_annotation_parses_several_ids() {
        assert_eq!(
            trace_annotations("    // trace: FR-NAM-070, NFR-RT-010\n"),
            vec!["FR-NAM-070", "NFR-RT-010"]
        );
    }

    #[test]
    fn trace_annotation_absent_is_empty() {
        assert!(trace_annotations("fn some_test() {}\n").is_empty());
    }

    #[test]
    fn fn_name_matches_the_established_convention() {
        assert!(fn_name_embeds_id(
            "fn fr_nam_070_crossfade_glitch_free() {}",
            "FR-NAM-070"
        ));
        assert!(fn_name_embeds_id(
            "fn nfr_rt_010_three_axes_run_concurrently() {}",
            "NFR-RT-010"
        ));
    }

    #[test]
    fn fn_name_does_not_spuriously_match_a_longer_id() {
        assert!(!fn_name_embeds_id(
            "fn fr_io_0100_something() {}",
            "FR-IO-010"
        ));
    }

    #[test]
    fn build_report_flags_a_must_requirement_with_no_coverage() {
        let reqs = vec![Requirement {
            id: "FR-X-010".into(),
            verify: 'U',
        }];
        let report = build_report(&reqs, &[], &HashMap::new());
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].id, "FR-X-010");
    }

    #[test]
    fn build_report_resolves_a_manual_verified_requirement_by_filename() {
        let reqs = vec![Requirement {
            id: "FR-IO-020".into(),
            verify: 'M',
        }];
        let docs = vec![(
            "fr-io-020-wasapi-exclusive-mode.md".to_string(),
            "irrelevant content".to_string(),
        )];
        let report = build_report(&reqs, &docs, &HashMap::new());
        assert!(report.missing.is_empty());
        assert_eq!(
            report.manual_hits.get("FR-IO-020").unwrap(),
            "fr-io-020-wasapi-exclusive-mode.md"
        );
    }

    #[test]
    fn build_report_resolves_a_manual_verified_requirement_named_only_in_content() {
        // fr-io-010-device-enumeration.md's own filename only names FR-IO-010, but its content
        // documents FR-IO-040 too -- a real file this project already has, so this must resolve.
        let reqs = vec![Requirement {
            id: "FR-IO-040".into(),
            verify: 'M',
        }];
        let docs = vec![(
            "fr-io-010-device-enumeration.md".to_string(),
            "**Requirement (literal):** FR-IO-010 ... FR-IO-040 ...".to_string(),
        )];
        let report = build_report(&reqs, &docs, &HashMap::new());
        assert!(report.missing.is_empty());
        assert_eq!(
            report.manual_hits.get("FR-IO-040").unwrap(),
            "fr-io-010-device-enumeration.md"
        );
    }

    #[test]
    fn build_report_treats_process_verified_as_always_covered() {
        let reqs = vec![Requirement {
            id: "NFR-QUAL-020".into(),
            verify: 'P',
        }];
        let report = build_report(&reqs, &[], &HashMap::new());
        assert!(report.missing.is_empty());
    }

    #[test]
    fn build_report_resolves_a_source_verified_requirement() {
        let reqs = vec![Requirement {
            id: "FR-NAM-070".into(),
            verify: 'I',
        }];
        let mut hits = HashMap::new();
        hits.insert("FR-NAM-070".to_string(), vec!["namir-engine".to_string()]);
        let report = build_report(&reqs, &[], &hits);
        assert!(report.missing.is_empty());
    }

    #[test]
    fn render_test_plan_marks_unresolved_requirements_explicitly() {
        let reqs = vec![Requirement {
            id: "FR-X-010".into(),
            verify: 'U',
        }];
        let report = build_report(&reqs, &[], &HashMap::new());
        let text = render_test_plan(&reqs, &report);
        assert!(text.contains("FR-X-010"));
        assert!(text.contains("UNRESOLVED"));
    }
}
