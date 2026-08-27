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
//! lookup. Every other Must requirement is covered by either (a) a `trace:` annotation comment on
//! the line before the covering test/bench/static-check item, or (b) the id embedded in the
//! covering test function's own name in the pre-existing `fr_xxx_nnn_...` convention (so the 4
//! tests already written that way need no changes). This module's job is pure parsing/matching
//! logic, kept testable against synthetic strings with no filesystem access -- `main.rs` supplies
//! the real FRS text, manual-test filenames, and source file contents.
//!
//! **D-23.1 (M9a)** gives a tag a defined meaning -- "this artifact verifies the *whole*
//! requirement, by the requirement's own stated `Verify:` method" -- and adds the three-valued
//! vocabulary that meaning requires: a plain `trace:`, a `trace-partial:` that *must* be followed
//! by an `uncovered:` line naming the unspanned member and a closing milestone, or no tag at all.
//! [`scan_annotations`] enforces that, plus the three integrity holes §23's M9 note records: the
//! marker must **begin** a comment line, every token must have a requirement id's shape, and the
//! tag must sit immediately above the declaration it claims. Each of those is a hard error rather
//! than a silent drop, because silently discarding a malformed tag deletes a contributor's
//! intended coverage without saying so. [`check_partial_verify_code`] closes the last drop this
//! module had: a `trace-partial:` naming a `Verify: M` or `Verify: Process` requirement, which the
//! two `Verify:`-code arms of [`build_report`] and [`render_test_plan`] would resolve past without
//! ever consulting it.
//!
//! Residual limit, recorded rather than pretended closed: this scanner is line-based, so it cannot
//! see Rust block comments or multi-line string continuations. A string literal whose continuation
//! line began, at its own start, with exactly `// trace:` would still parse as a tag. The
//! begins-the-line rule shrinks that to a shape nothing in this tree has ever had (before it, the
//! tool's own test fixtures and this file's own header string parsed as tags); closing it fully
//! would need a Rust parser this project has no other use for.

// trace-partial: NFR-QUAL-010
// uncovered: NFR-QUAL-010 — the method's "fails on any uncovered Must" is not executed: CI's
// uncovered: required step passes --allow-uncovered and derives its exit status from plan freshness
// uncovered: and §14's denominators alone, while the plain form runs continue-on-error, so an
// uncovered: uncovered Must would leave CI green — none stands today, and nothing gates on that;
// uncovered: closes M9b

use std::collections::HashMap;

/// One `Must`-priority requirement parsed from the FRS, paired with its `*Verify:*` code
/// (`U`/`I`/`G`/`B`/`S`/`M` per FRS §1.5) and the number of the FRS heading in force at its own
/// line (`"4"`, `"5.1"`, ...; empty when no numbered heading preceded it). D-23.2 derives §14's
/// Must-count denominators from that section number, so it is parsed here rather than guessed
/// from the id's area token -- `## 4. Product configurations` carries no `(CFG)` suffix where
/// every `### 5.x`/`### 6.x` heading does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub id: String,
    pub verify: char,
    pub section: String,
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
    let mut current_section = String::new();

    let mut i = 0;
    while i < lines.len() {
        if let Some(number) = heading_section_number(lines[i]) {
            current_section = number;
        }
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
                // The section is the heading in force at the requirement's *own* line, not at its
                // `*Verify:*` line: the forward scan above may cross a heading, and D-23.2 keys
                // the grouping on the heading in force when the requirement is parsed.
                Some(verify) => out.push(Requirement {
                    id,
                    verify,
                    section: current_section.clone(),
                }),
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

/// `"## 4. Product configurations"` -> `Some("4")`, `"### 5.1 Signal chain (CHAIN)"` ->
/// `Some("5.1")`. `None` for anything that is not a `##`/`###` heading opening with a section
/// number.
///
/// The two real FRS forms differ (`:133` carries a trailing period after the number, `:161` does
/// not) and D-23.2's implementation note calls that out as the reason to derive the number from
/// the heading and the area token from the ids rather than amend the FRS. `# ` and `#### ` and
/// deeper are excluded deliberately: `# Namir — Functional Requirements Specification` is the
/// document title, and a `#### 5.1.1` sub-subsection is part of `5.1`, not a section of its own.
fn heading_section_number(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("### ")
        .or_else(|| line.strip_prefix("## "))?;
    let rest = rest.trim_start();

    let run_len = rest.len()
        - rest
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.')
            .len();
    let (run, after) = rest.split_at(run_len);
    if !(after.is_empty() || after.starts_with(|c: char| c.is_ascii_whitespace())) {
        return None;
    }

    let run = run.strip_suffix('.').unwrap_or(run);
    if run.is_empty()
        || !run.starts_with(|c: char| c.is_ascii_digit())
        || !run.ends_with(|c: char| c.is_ascii_digit())
        || run.matches('.').count() > 1
    {
        return None;
    }
    Some(run.to_string())
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

/// Both comment-prefix spellings a `trace:` annotation may use: `// trace:` in `.rs` source,
/// `# trace:` in `.yml`/`.toml` config -- a real, non-trivial slice of Must requirements (MSRV,
/// clippy-as-error, cargo-deny, mobile/no-C++ builds, network-free) are verified entirely by CI
/// workflow/build configuration, not by any Rust test function, and would be permanently
/// unresolvable without this. The three arrays are index-parallel: a `trace-partial:` in one
/// spelling must be paired with an `uncovered:` in the *same* spelling.
const TRACE_MARKERS: [&str; 2] = ["// trace:", "# trace:"];
const PARTIAL_MARKERS: [&str; 2] = ["// trace-partial:", "# trace-partial:"];
const UNCOVERED_MARKERS: [&str; 2] = ["// uncovered:", "# uncovered:"];

/// One parsed annotation. `uncovered: None` is a plain `trace:` -- an assertion that the annotated
/// artifact verifies the whole requirement by its stated `Verify:` method. `Some(text)` is a
/// `trace-partial:` carrying its joined `uncovered:` field (D-23.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub id: String,
    /// 1-based, for error messages only -- the caller prefixes it with the file path.
    pub line: usize,
    pub uncovered: Option<String>,
}

/// Every `trace:`/`trace-partial:` annotation in `source` (one file's text), under D-23.1's rules.
///
/// `Err` is a **hard error**: a malformed annotation, not a coverage gap. The caller prefixes the
/// returned string with the file path and aborts. Deliberately not a silent drop -- a marker line
/// that got this far is a real tag someone wrote, so discarding it would delete their intended
/// coverage without telling anyone.
///
/// The rules, in the order they are applied to a line:
///
/// 1. The marker must **begin** the (trimmed) line. Prose that merely names the marker, and string
///    literals containing one, stop here and are not tags at all.
/// 2. Every comma-separated token after the marker must have a requirement id's shape
///    (`FR-AREA-NNN`/`NFR-AREA-NNN`). A token that does not is a typo inside a line that already
///    passed rule 1, so it is an error rather than a silent drop.
/// 3. A `trace-partial:` carries exactly one id and **must** be followed, on the very next line, by
///    an `uncovered:` line of the same comment spelling; consecutive `uncovered:` lines join with a
///    single space into one field. That field must begin with the same id and end with a
///    `; closes M<n>` clause, which is what makes §22's R-13(a) mechanical rather than a
///    convention. An `uncovered:` line with no `trace-partial:` above it is equally an error.
/// 4. The tag must be immediately above the artifact it claims: the first non-blank line after it
///    (after the `uncovered:` block, for a partial) must not be another comment line.
pub fn scan_annotations(source: &str) -> Result<Vec<Annotation>, String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let line_no = i + 1;

        if let Some((_, rest)) = match_marker(trimmed, &TRACE_MARKERS) {
            let ids = parse_ids(rest, line_no)?;
            check_adjacency(&lines, i + 1, line_no, "trace", rest.trim())?;
            out.extend(ids.into_iter().map(|id| Annotation {
                id,
                line: line_no,
                uncovered: None,
            }));
            i += 1;
            continue;
        }

        if let Some((spelling, rest)) = match_marker(trimmed, &PARTIAL_MARKERS) {
            let ids = parse_ids(rest, line_no)?;
            let [id] = ids.as_slice() else {
                return Err(format!(
                    "{line_no}: `trace-partial:` carries {} ids -- D-23.1's form takes one id, \
                     because the single `uncovered:` line that must follow names one member of \
                     one requirement and could not be attributed among several",
                    ids.len()
                ));
            };
            let uncovered_marker = UNCOVERED_MARKERS[spelling];

            let mut parts = Vec::new();
            let mut j = i + 1;
            while j < lines.len()
                && let Some((_, text)) = match_marker(lines[j].trim_start(), &[uncovered_marker])
            {
                parts.push(text.trim());
                j += 1;
            }
            if parts.is_empty() {
                return Err(format!(
                    "{line_no}: `trace-partial: {id}` is not followed by an `uncovered:` line -- \
                     D-23.1 requires both or neither; add `{uncovered_marker} {id} — <the \
                     unspanned member or unexecuted half>; closes M<n>` on the next line"
                ));
            }
            let uncovered = parts.join(" ").trim().to_string();

            if !begins_with_id(&uncovered, id) {
                return Err(format!(
                    "{line_no}: the `uncovered:` line names `{}` but the `trace-partial:` above it \
                     names `{id}` -- D-23.1 pairs one `uncovered:` field with one partial tag, so \
                     the two must name the same requirement",
                    uncovered.split_whitespace().next().unwrap_or("")
                ));
            }
            if !ends_with_closing_milestone(&uncovered) {
                return Err(format!(
                    "{line_no}: the `uncovered:` line for {id} does not end with a closing \
                     milestone -- D-23.1 requires the form `; closes M<n>` so a partial carries a \
                     named due date rather than becoming permanent (§22 R-13(a)); found `{uncovered}`"
                ));
            }

            check_adjacency(&lines, j, line_no, "trace-partial", rest.trim())?;
            out.push(Annotation {
                id: id.clone(),
                line: line_no,
                uncovered: Some(uncovered),
            });
            i = j;
            continue;
        }

        if match_marker(trimmed, &UNCOVERED_MARKERS).is_some() {
            // Every `uncovered:` line belonging to a `trace-partial:` was consumed above, so
            // reaching one here means there is no partial tag on the line before it.
            return Err(format!(
                "{line_no}: an `uncovered:` line with no `trace-partial:` above it -- D-23.1 \
                 requires both or neither"
            ));
        }

        i += 1;
    }

    Ok(out)
}

/// `("// trace:", "  FR-X-010")` for a line that begins with that marker, paired with the marker's
/// index in `markers` so a `trace-partial:` can be matched to an `uncovered:` of the same spelling.
fn match_marker<'a>(trimmed: &'a str, markers: &[&str]) -> Option<(usize, &'a str)> {
    markers
        .iter()
        .enumerate()
        .find_map(|(k, m)| trimmed.strip_prefix(m).map(|rest| (k, rest)))
}

/// Splits a marker's remainder on `,` and validates every token's shape (rule 2).
fn parse_ids(rest: &str, line_no: usize) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for token in rest.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if !is_requirement_id(token) {
            return Err(format!(
                "{line_no}: `{token}` is not a requirement id -- a `trace:`/`trace-partial:` \
                 marker's comma-separated tokens must each read FR-AREA-NNN or NFR-AREA-NNN. \
                 Dropping it silently would delete whatever coverage it was meant to record"
            ));
        }
        ids.push(token.to_string());
    }
    Ok(ids)
}

/// `^N?FR-[A-Z]+-\d{3}$`, hand-rolled -- `xtask` carries no regex dependency and this is the only
/// pattern it needs.
fn is_requirement_id(token: &str) -> bool {
    let Some(body) = token
        .strip_prefix("NFR-")
        .or_else(|| token.strip_prefix("FR-"))
    else {
        return false;
    };
    let Some((area, number)) = body.rsplit_once('-') else {
        return false;
    };
    !area.is_empty()
        && area.bytes().all(|b| b.is_ascii_uppercase())
        && number.len() == 3
        && number.bytes().all(|b| b.is_ascii_digit())
}

/// Every requirement id occurring anywhere in `line`, left to right. The same id shape
/// [`is_requirement_id`] validates a whole token against, applied as a scanner instead -- defined
/// once here and called from [`crate::milestones`] rather than written twice.
///
/// A match must begin at the start of the line or after a non-ASCII-alphanumeric byte, and must not
/// be followed by an ASCII digit or letter. Those two boundary checks are what make the scan
/// trustworthy on real prose: `NFR-` is tried before `FR-` and the match consumes its own length, so
/// `NFR-PERF-030` cannot also yield a spurious `FR-PERF-030`; and the trailing check is the same
/// hazard [`fn_name_embeds_id`] guards by hand, so `FR-IO-0100` yields nothing rather than
/// `FR-IO-010`. Markdown emphasis and punctuation fall out for free: `**FR-NAM-150**`,
/// `FR-CLAP-030,` and `FR-IO-010's` all resolve.
///
/// Deliberately does **not** expand the shorthand runs the roadmap writes (`FR-PKG-010, -020,
/// -030`): only the first, full id resolves. Inferring that `-020` means `FR-PKG-020` is the kind of
/// guess that is wrong silently, and every id it would reach is spelled out in full somewhere else.
pub fn scan_requirement_ids(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let at_boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if at_boundary && let Some(len) = requirement_id_len(&bytes[i..]) {
            // Every byte of a match is ASCII, so `i` and `i + len` are both char boundaries even
            // when the surrounding line carries multi-byte text (these documents are full of em
            // dashes, and one sits immediately before an id in every FRS requirement line).
            out.push(line[i..i + len].to_string());
            i += len;
            continue;
        }
        i += 1;
    }
    out
}

/// The byte length of the requirement id starting at `rest[0]`, or `None`.
fn requirement_id_len(rest: &[u8]) -> Option<usize> {
    for prefix in [b"NFR-".as_slice(), b"FR-".as_slice()] {
        if !rest.starts_with(prefix) {
            continue;
        }
        let mut k = prefix.len();
        let area_start = k;
        while k < rest.len() && rest[k].is_ascii_uppercase() {
            k += 1;
        }
        if k == area_start || rest.get(k) != Some(&b'-') {
            continue;
        }
        k += 1;
        let digits_start = k;
        while k < rest.len() && rest[k].is_ascii_digit() {
            k += 1;
        }
        if k - digits_start != 3 {
            continue;
        }
        if rest
            .get(k)
            .is_some_and(|b| b.is_ascii_digit() || b.is_ascii_alphabetic())
        {
            continue;
        }
        return Some(k);
    }
    None
}

/// True if `text` starts with `id` at an identifier boundary -- so `FR-LIB-020 — ...` matches
/// FR-LIB-020 but `FR-LIB-0200 — ...` does not.
fn begins_with_id(text: &str, id: &str) -> bool {
    text.strip_prefix(id).is_some_and(|rest| {
        rest.chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-'))
    })
}

/// The `M<n>[a-z]` label an `uncovered:` field's trailing `; closes M<n>` clause names, or `None`
/// when it carries no well-formed clause. Hand-rolled `;\s*closes\s+M\d+[a-z]?\s*$`. The optional
/// trailing letter is required because M9 is split into phase labels M9a/M9b and D-23.1's own
/// worked example ends `; closes M9b`.
///
/// This is the only id -> milestone source in the tool that is *declared* rather than derived, and
/// it is declared by the annotation's own author beside the gap it names. `main.rs` prints it in
/// R-13's partial block; nothing reads it for an exit status.
pub fn closing_milestone(text: &str) -> Option<&str> {
    let text = text.trim_end();
    let idx = text.rfind(';')?;
    let rest = text[idx + 1..].trim_start().strip_prefix("closes")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let label = rest.trim_start();
    let digits_rest = label.strip_prefix('M')?;
    let digits = digits_rest.len()
        - digits_rest
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    if digits == 0 {
        return None;
    }
    let tail = &digits_rest[digits..];
    if tail.is_empty() || (tail.len() == 1 && tail.as_bytes()[0].is_ascii_lowercase()) {
        Some(label)
    } else {
        None
    }
}

fn ends_with_closing_milestone(text: &str) -> bool {
    closing_milestone(text).is_some()
}

/// Rule 4. `from` is the index to start looking for the anchor at: the line after the marker for a
/// plain `trace:`, or the line after the last `uncovered:` line for a `trace-partial:` (a literal
/// "next non-blank line" reading would make every partial fail, since its own `uncovered:` block
/// sits between the marker and the artifact).
///
/// D-23.1 names six admissible anchors (`#[test]`, `#[bench]`, `#[cfg(test)]`, `[[bench]]`, a CI
/// job or step declaration, and `fn main()` in a `benches/*.rs` target). Enforced as a literal
/// whitelist that rejects 13 live tag sites carrying 18 ids in this tree -- file- and item-level
/// tags above `#![no_main]`, a `use` statement, a `pub struct`, a `const`, and the four TOML table
/// or key lines -- which is the same failure the `fn main()` near-miss recorded at
/// `02-architecture.md:2740-2745` would have caused. The rule enforced here is the minimal one
/// that admits all six named members: the anchor must exist and must not itself be a comment.
///
/// **What this rule does not close, contrary to D-23.1's own clause.** That clause says "the
/// adjacency requirement is what stops prose that merely *names* the marker from parsing as a tag:
/// `ci.yml:109` is the standing instance". That attribution is wrong, and D-23.1's
/// *Consequence (added M9a)* note records it as wrong: adjacency closes **none** of the three false
/// positives the §23 M9 note lists. Rule 1 -- the marker must *begin* the trimmed line, matched by
/// [`match_marker`]'s `strip_prefix` -- kills both the string-literal class and `ci.yml:109` itself,
/// whose trimmed line reads ``# `// trace:`/manual-test coverage found…`` and is therefore a prefix
/// of neither marker spelling; the tightened [`fn_name_embeds_id`] kills the fn-name class. The
/// tests say so by name: `a_marker_that_does_not_begin_the_line_is_not_a_tag` labels its two cases
/// after those classes, and `rule_1_not_adjacency_is_what_stops_a_prose_mention_of_the_marker`
/// plants a line of `ci.yml:109`'s shape -- a prose mention of the marker, not that line verbatim
/// -- above a perfectly admissible anchor, so this function would accept
/// it and never runs.
///
/// What adjacency *does* close is a fourth class none of those reach: a **well-formed** tag, naming
/// real ids at the start of its own line, that sits above no declaration at all -- one inside a
/// prose or doc-comment block, one stranded at end of file, or one that has *drifted* from its
/// artifact because a doc comment or another item was inserted between the two. That is a
/// regression guard on tags that already exist, worth having on its own terms, and not what
/// D-23.1's clause claimed for it.
fn check_adjacency(
    lines: &[&str],
    from: usize,
    line_no: usize,
    marker: &str,
    ids: &str,
) -> Result<(), String> {
    let anchor = (from..lines.len()).find(|&j| !lines[j].trim().is_empty());
    let found = match anchor {
        None => "end of file".to_string(),
        Some(j) if is_comment_line(lines[j].trim()) => format!("a comment line at :{}", j + 1),
        Some(_) => return Ok(()),
    };
    Err(format!(
        "{line_no}: `{marker}: {ids}` is not immediately above the artifact it claims -- D-23.1 \
         requires the next non-blank line to be a test, bench, CI or item declaration; found \
         {found}. Move the tag below any doc comment, directly above the declaration"
    ))
}

/// `//`-anything is a Rust comment; `#`-anything is a YAML/TOML comment *except* `#[` and `#!`,
/// which are Rust attributes. Neither of those two ever begins a line in the four config files on
/// `main.rs`'s scanned list, so treating them as non-comments everywhere is safe and avoids
/// threading a per-file-type flag through the scanner.
fn is_comment_line(trimmed: &str) -> bool {
    if trimmed.starts_with("//") {
        return true;
    }
    trimmed
        .strip_prefix('#')
        .is_some_and(|rest| !rest.starts_with('[') && !rest.starts_with('!'))
}

/// True if `source` defines a function whose name embeds `id` in the pre-existing
/// `fr_xxx_nnn_description`/`nfr_xxx_nnn_description` snake-case convention (e.g. `FR-NAM-070` ->
/// `fn fr_nam_070_...`). Deliberately requires a `_` or `(` immediately after the id's snake form,
/// not a bare substring match, so `FR-IO-010` does not spuriously match a hypothetical
/// `fr_io_0100_...`.
///
/// D-23.1 (M9a) requires the identifier to be found "on a line beginning `fn ` that is itself
/// preceded by a test attribute" -- this was a whole-file substring test, matching neither a real
/// function nor even a line boundary, which is how this module's own doc comment and one of its
/// own string literals put `xtask` in the generated plan as a component covering FR-NAM-070, a
/// requirement `xtask` does not test (`02-architecture.md:2586-2604`).
pub fn fn_name_embeds_id(source: &str, id: &str) -> bool {
    let snake = id.to_lowercase().replace('-', "_");
    let with_underscore = format!("fn {snake}_");
    let with_paren = format!("fn {snake}(");
    let lines: Vec<&str> = source.lines().collect();

    lines.iter().enumerate().any(|(i, line)| {
        let trimmed = line.trim_start();
        if !(trimmed.starts_with(&with_underscore) || trimmed.starts_with(&with_paren)) {
            return false;
        }
        // The nearest preceding non-blank line must be the test attribute. All four real users of
        // this fallback are `#[test]` immediately above an (indented, inside `mod tests`) `fn`.
        (0..i)
            .rev()
            .find(|&j| !lines[j].trim().is_empty())
            .is_some_and(|j| {
                let prev = lines[j].trim();
                prev.starts_with("#[test]")
                    || prev.starts_with("#[bench]")
                    || prev.starts_with("#[cfg(test)]")
            })
    })
}

/// `"FR-IO-020"` -> `"fr-io-020"`, the filename prefix `docs/manual-tests/` files use.
pub fn manual_test_prefix(id: &str) -> String {
    id.to_lowercase()
}

/// D-23.1's **PARTIAL**-render guarantee, enforced at the one place it can be: a `trace-partial:`
/// naming a `Verify: M` or `Verify: Process` requirement is a **hard error**.
///
/// [`build_report`] and [`render_test_plan`] both dispatch on the `Verify:` code *before* they ever
/// consult `partial_hits` -- a `Verify: M` Must resolves to its manual-test document and a
/// `Verify: Process` one to the process line, in neither case looking at whether a partial named
/// it. Left alone, that makes D-23.1's own absolute -- "`xtask traceability` renders **every**
/// `trace-partial` as a **PARTIAL** row [...] so a partial cannot be introduced without appearing in
/// a generated, checked-in, diffable file in the same pull request"
/// (`docs/02-architecture.md:2710-2712`) -- false for 14 of the FRS's 130 Musts (13 `M`, 1
/// `Process`), and false *silently*: the tag would parse, its mandatory `uncovered:` field would be
/// validated against every rule [`scan_annotations`] applies, and then both would be dropped without
/// a word.
///
/// **Refusing is chosen over rendering the partial in those two arms**, and the reason is D-23.1's
/// own first sentence, which is exactly what the two arms are obeying: a tag asserts coverage **by
/// the requirement's own stated `Verify:` method**. For a `Verify: M` Must that method is a written
/// manual-test script under `docs/manual-tests/`, which no `.rs`/`.yml`/`.toml` file is or can be
/// part of; for `Verify: Process` the FRS's own definition is review and commit order, with no
/// artifact a build can inspect at all. Rendering would therefore write into a checked-in generated
/// document a claim the doctrine says cannot be made -- visible, and wrong -- and for `Verify: M` it
/// would additionally risk a source annotation standing in for the manual script NFR-QUAL-010
/// explicitly requires instead ("except where the Verify field states M, in which case it shall be
/// covered by a written manual test script"). An error is also strictly stronger than the guarantee
/// it defends: the partial cannot be introduced *at all*, rather than merely not introduced
/// invisibly, and it fails in the same pull request D-23.1 wants it visible in.
///
/// **Scoped to `trace-partial:` deliberately, and not extended to a plain `trace:`.** Three live
/// plain tags name `Verify: M` requirements today -- `crates/namir-ui/src/format.rs:49` and `:93`
/// for FR-UI-040, `crates/namir-ui/src/controls.rs:210` for FR-UI-050 -- and they are D-18.6's
/// split-evidence shape: an in-process test alongside the manual script, with the script still the
/// traced artifact and still what the plan renders. Those drop harmlessly, because the requirement
/// resolves by its own method either way and nothing their author wrote is lost. A `trace-partial:`
/// is different in kind: its `uncovered:` field is mandatory, names a gap and a due date, and exists
/// for no purpose other than to be rendered.
pub fn check_partial_verify_code(id: &str, verify: char, line: usize) -> Result<(), String> {
    let (code, reason) = match verify {
        // The FRS spells this code `Process`, not `P`; `extract_verify_code` keeps only the first
        // character, so the spelling is restored here rather than printing a letter no `*Verify:*`
        // line in the FRS actually carries.
        'M' => (
            "M",
            "a `Verify: M` Must is verified by a written manual-test script under \
             docs/manual-tests/, which no source or configuration file is or can be part of -- \
             record the unspanned member in that document instead",
        ),
        'P' => (
            "Process",
            "a `Verify: Process` Must is verified by review and commit order, with no artifact a \
             build can inspect, so there is nothing for a partial to be partial about",
        ),
        _ => return Ok(()),
    };
    Err(format!(
        "{line}: `trace-partial: {id}` names a `Verify: {code}` requirement -- D-23.1 asserts \
         coverage by the requirement's own stated `Verify:` method, and {reason}. This is refused \
         rather than dropped: the generated plan resolves such a requirement by its own method \
         without consulting partials, so the tag's `uncovered:` field would never reach the plan \
         D-23.1 requires it to appear in"
    ))
}

/// One `trace-partial:` hit, resolved to the component (crate name) it was found in by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialHit {
    pub component: String,
    pub uncovered: String,
}

/// The outcome of checking every Must requirement against real manual-test filenames and real
/// source-file hits. `source_hits`/`manual_hits` are `id -> [crate name]` / `id -> filename` for
/// requirements that *are* covered, kept for `render_test_plan`; `partial_hits` is the same for
/// requirements covered only in part (D-23.1); `missing` is every Must id this run found no
/// coverage for at all.
pub struct Report {
    pub missing: Vec<Requirement>,
    pub manual_hits: HashMap<String, String>,
    pub source_hits: HashMap<String, Vec<String>>,
    pub partial_hits: HashMap<String, Vec<PartialHit>>,
}

/// The opening of a manual-test document's requirement declaration. Two spellings exist in the
/// tree -- `**Requirement (literal):**` and `**Requirement (literal, Must):**` -- so the marker
/// stops before the parenthesis's contents rather than trying to enumerate them.
const DECLARATION_MARKER: &str = "**Requirement (literal";

/// Every requirement id a manual-test document **declares** it verifies: the ids occurring in its
/// `**Requirement (literal…):**` block, in order, deduplicated.
///
/// The block is the paragraph beginning with [`DECLARATION_MARKER`] and running to the first blank
/// line, not the marker's own line. That matters -- these declarations routinely wrap across three
/// or four lines, and `fr-io-010-device-enumeration.md`'s names FR-IO-010 on its first line and
/// FR-IO-040 on its third, so a single-line read would drop the one legitimate multi-requirement
/// document in the tree while keeping every false positive.
///
/// Only the **first** block is read. No document in the tree has two, and a rule that accumulated
/// every block would drift back towards the whole-file match this function exists to replace.
///
/// A document with no declaration block declares nothing and returns empty. That is deliberately
/// not an error: such a document can still resolve its own requirement by filename, which is the
/// other arm of [`build_report`]'s `'M'` match, and turning a missing line into a hard error would
/// make a documentation omission abort the whole gate. It does mean the filename arm's own
/// weakness is untouched by this function -- a correctly-named document recording "not executed"
/// credits its requirement identically to one recording a clean pass. Roadmap §15 item 15 names
/// that separately and it is not what this narrowing fixes.
pub fn declared_requirement_ids(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if !in_block {
            if !trimmed.starts_with(DECLARATION_MARKER) {
                continue;
            }
            in_block = true;
        } else if trimmed.is_empty() {
            break;
        }
        for id in scan_requirement_ids(line) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Reconciles `requirements` against `manual_test_docs` (every real `(filename, content)` pair
/// under `docs/manual-tests/`, e.g. `("fr-io-020-wasapi-exclusive-mode.md", "...")`), `source_hits`
/// (every id this run found a `trace:` annotation or matching test-fn name for, already resolved
/// to the crate name(s) it was found in by the caller) and `partial_hits` (the same for
/// `trace-partial:` annotations).
///
/// A manual-test file matches a `Verify: M` requirement if either its filename starts with the
/// id's lowercase prefix (the usual one-file-per-requirement case) *or* the id appears in the
/// document's own `**Requirement (literal…):**` declaration block, as
/// [`declared_requirement_ids`] reads it (a file documented as covering more than one requirement,
/// e.g. `fr-io-010-device-enumeration.md` also covering FR-IO-040, is real and must not be missed
/// just because its filename only names the first one).
///
/// The second arm read the **whole file** until M13 narrowed it, and that was a live defect rather
/// than a theoretical one: any prose mention of an id credited that id in full. Roadmap §15 item 15
/// caught FR-UI-020 resolving to `fr-clap-030-audio-ports-negotiation.md` on a parenthesis about
/// watching a meter, M12's close-out caught `fr-ui-110-brand-mark.md` crediting FR-PKG-030 on a
/// passing mention, and narrowing the arm turned up a third, FR-UI-070 resolving to
/// `fr-ui-010-standalone-window-renders.md` because its script says the canned snapshot carries
/// "one FR-UI-070 notice". None of the three documents verifies the requirement it was crediting.
/// Reading the declaration block instead makes the credit something the document's author wrote
/// deliberately, which is the whole of what this arm is for.
pub fn build_report(
    requirements: &[Requirement],
    manual_test_docs: &[(String, String)],
    source_hits: &HashMap<String, Vec<String>>,
    partial_hits: &HashMap<String, Vec<PartialHit>>,
) -> Report {
    let mut missing = Vec::new();
    let mut manual_hits = HashMap::new();

    // The `'M'` and `'P'` arms below resolve a requirement without consulting `partial_hits` at
    // all. That is correct -- neither code's evidence is a source annotation -- but it is only safe
    // because a partial naming one of those codes cannot reach here: `check_partial_verify_code`
    // refuses it in `main.rs`'s scan loop, upstream of this call and of every exit-status term. The
    // invariant is stated here rather than re-checked here because this function has no line or
    // file to name in an error, and a coverage reconciler is the wrong place to diagnose a
    // malformed input.
    for req in requirements {
        if req.verify == 'M' {
            let prefix = format!("{}-", manual_test_prefix(&req.id));
            match manual_test_docs.iter().find(|(name, content)| {
                name.to_lowercase().starts_with(&prefix)
                    || declared_requirement_ids(content).contains(&req.id)
            }) {
                Some((file, _)) => {
                    manual_hits.insert(req.id.clone(), file.clone());
                }
                None => missing.push(req.clone()),
            }
        } else if req.verify == 'P' {
            // Process-verified: by definition, verified by review/commit order, not by any
            // artifact this check can inspect. Nothing to look up; never "missing".
        } else if !source_hits.contains_key(&req.id) && !partial_hits.contains_key(&req.id) {
            // D-23.1: a `trace-partial` counts as coverage for the ordinary run. It must --
            // FR-NAM-030 is knowingly half-met until M10 Phase 4, and a gate that cannot go green
            // is the red-check-nobody-can-act-on problem M7 marked this check informational over.
            // The teeth are elsewhere: D-18.5's zero-uncovered half becomes required at M13's
            // close-out, and D-23.2 rules that a Partial is not Done for M8's exit checklist.
            missing.push(req.clone());
        }
    }

    Report {
        missing,
        manual_hits,
        source_hits: source_hits.clone(),
        partial_hits: partial_hits.clone(),
    }
}

/// The `Verify:` codes whose plan row resolves *through* `partial_hits`: every code except `M`,
/// whose evidence is a manual-test document, and `Process`, which has no build-inspectable artifact
/// at all (see [`check_partial_verify_code`] for why those two are refused rather than rendered).
///
/// Written once and read by both [`render_test_plan`]'s dispatch and [`partial_row_ids`], so the
/// rows the plan carries and the number R-13 prints cannot come from two different conditions.
fn resolves_through_partials(verify: char) -> bool {
    !matches!(verify, 'M' | 'P')
}

/// The ids [`render_test_plan`] emits a **PARTIAL** row for, sorted as the plan sorts them.
///
/// §22's **R-13** mitigation (d) is a *printed count*, and a count naming a different set from the
/// rows it is read against is not that mitigation -- it is a second number to reconcile. The plan
/// has rows for the FRS's **Must** requirements only, so a `trace-partial:` naming anything else --
/// a Should, a Could, an id the FRS does not carry at all -- parses, validates, lands in
/// `partial_hits` and renders nowhere, while still moving the printed number. This function is what
/// makes the two one set.
///
/// Those unrendered partials are **not** dropped: `main.rs` prints them under their own heading,
/// outside this count. Discarding them would be the same species of silent loss the rest of this
/// module has been removing -- a partial on a Should is still someone recording a gap.
///
/// The condition mirrors [`render_test_plan`]'s dispatch, and
/// `partial_row_ids_are_exactly_the_rows_the_plan_marks_partial` checks it against the rendered text
/// rather than against the reasoning, so the mirror cannot drift in silence.
pub fn partial_row_ids(requirements: &[Requirement], report: &Report) -> Vec<String> {
    let mut ids: Vec<String> = requirements
        .iter()
        .filter(|req| {
            resolves_through_partials(req.verify) && report.partial_hits.contains_key(&req.id)
        })
        .map(|req| req.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
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
         `trace:` annotation or matching test-function name for it (`Verify: U/I/G/B/S`). A row \
         marked \"PARTIAL\" carries a `// trace-partial:` tag whose artifact covers only part of \
         the requirement: the text after it is that tag's mandatory `// uncovered:` line verbatim, \
         naming the unspanned member and the milestone that closes it (D-23.1). A partial counts \
         as coverage for this check's exit status and is not Done for §14's table. A requirement \
         listed under \"UNRESOLVED\" has neither -- `cargo run -p xtask -- traceability` exits \
         non-zero while any remain. CI's **required** step passes `--allow-uncovered` and gates on \
         this file's freshness (and on §14's denominators) alone; the zero-uncovered half stays \
         informational until it becomes required at M9b's close-out (D-18.5).\n\n\
         | Requirement | Verify | Covered by |\n\
         |---|---|---|\n",
    );

    // Same dispatch, same invariant as `build_report`'s: the `'M'`/`'P'` arms never reach
    // `partial_hits`, and `check_partial_verify_code` is what makes that lossless.
    for req in &sorted {
        let covered_by = if resolves_through_partials(req.verify)
            && let Some(partials) = report.partial_hits.get(&req.id)
        {
            // A partial wins over a plain tag on the same id. The two assert contradictory things
            // (whole requirement vs. named unmet clause) and D-23.1 settles neither; rendering the
            // gap is the honest direction, and no such case exists in this tree today.
            render_partial(partials, report.source_hits.get(&req.id))
        } else if req.verify == 'M' {
            report
                .manual_hits
                .get(&req.id)
                .map(|f| format!("`docs/manual-tests/{f}`"))
                .unwrap_or_else(|| "**UNRESOLVED**".to_string())
        } else if req.verify == 'P' {
            "process (review + commit order, not build-inspectable)".to_string()
        } else if let Some(crates) = report.source_hits.get(&req.id) {
            backticked_components(crates)
        } else {
            "**UNRESOLVED**".to_string()
        };
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            req.id, req.verify, covered_by
        ));
    }

    // D-23.2's derived denominators, appended after the requirement table so the block above stays
    // byte-identical. On `Err` nothing is appended: the same `Err` is a hard failure in `main.rs`
    // (`check_section_table`), so a plan generated without this block can never pass the gate --
    // and rendering a diagnostic *into* a checked-in generated document would be worse.
    if let Ok(counts) = section_must_counts(requirements) {
        out.push_str(&render_section_counts(&counts));
    }

    out
}

/// D-23.1's **PARTIAL** cell: the components, then the `uncovered:` text verbatim, so a partial
/// cannot be introduced without appearing in a generated, checked-in, diffable file in the same
/// pull request. Sorted and deduplicated for byte-stability; if one id carries partials whose
/// texts differ (more than one covering artifact, each naming its own gap), every text is rendered
/// against its own component rather than one being silently dropped.
fn render_partial(partials: &[PartialHit], plain: Option<&Vec<String>>) -> String {
    let mut units: Vec<(String, String)> = partials
        .iter()
        .map(|p| (p.component.clone(), p.uncovered.clone()))
        .collect();
    units.sort();
    units.dedup();

    let mut texts: Vec<&String> = units.iter().map(|(_, text)| text).collect();
    texts.dedup();

    let body = if let [text] = texts.as_slice() {
        let mut components: Vec<String> = units.iter().map(|(c, _)| c.clone()).collect();
        components.extend(plain.into_iter().flatten().cloned());
        format!(
            "{}: {}",
            backticked_components(&components),
            escape_cell(text)
        )
    } else {
        units
            .iter()
            .map(|(component, text)| format!("`{component}`: {}", escape_cell(text)))
            .collect::<Vec<_>>()
            .join("; ")
    };
    format!("**PARTIAL** — {body}")
}

/// The `uncovered:` text is free prose written by whoever added the tag, so it may contain a `|`,
/// which would silently break the Markdown table CI diffs.
fn escape_cell(text: &str) -> String {
    text.replace('|', r"\|")
}

/// A sorted, deduplicated, backticked crate list -- the `Covered by` cell's usual form.
fn backticked_components(crates: &[String]) -> String {
    let mut names = crates.to_vec();
    names.sort();
    names.dedup();
    names
        .iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------------------------
// D-18.5: the split gate's CLI surface and its exit-status derivation.
//
// Both live here rather than in `main.rs` because both are load-bearing and neither was testable
// where it was: the exit status was an inline `&&` in the middle of a filesystem-walking function,
// and argument parsing was an `any(|a| a == "--write")` that silently ignored everything else.
// ---------------------------------------------------------------------------------------------

/// `traceability`'s parsed arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TraceabilityArgs {
    pub write: bool,
    pub allow_uncovered: bool,
}

/// Order-independent, repeat-tolerant, and **strict about anything else**.
///
/// Rejecting an unknown argument is a deliberate behaviour change: today's parser recognises only
/// `--write`, so a mistyped flag is silently ignored (`02-architecture.md:2025-2026`). Once the flag
/// selects between a required and an informational gate, a typo must be loud. It is fail-safe in
/// both directions -- a mistyped `--allow-uncovered` cannot make a lenient run look strict and
/// green, and at M9b's close-out a `ci.yml` that still passes the deleted flag hard-fails instead of
/// quietly running the strict form against a tree nobody expected it to gate.
pub fn parse_traceability_args(args: &[String]) -> Result<TraceabilityArgs, String> {
    let mut parsed = TraceabilityArgs::default();
    for arg in args {
        match arg.as_str() {
            "--write" => parsed.write = true,
            "--allow-uncovered" => parsed.allow_uncovered = true,
            other => {
                return Err(format!(
                    "traceability: unrecognised argument `{other}` (expected --write and/or \
                     --allow-uncovered)"
                ));
            }
        }
    }
    Ok(parsed)
}

/// D-18.5's split gate.
///
/// `required_half` is the conjunction of the two properties that are required from M9a onward: the
/// generated-plan diff (D-18.5) and §14's derived denominators (D-23.2, `02-architecture.md:2892`).
/// `coverage_clean` is the zero-uncovered half, which stays informational until M9b's close-out.
/// `--allow-uncovered` relaxes **only** the second; it never softens the first, and it never softens
/// a tool failure, which `main.rs` returns on before reaching here.
///
/// **This function takes no attribution argument, and that signature is the mechanical guarantee**
/// for D-18.5's "the exit status never depends on that attribution"
/// (`docs/02-architecture.md:2007-2008`). It is the sole producer of `run_traceability`'s return
/// value, so a later change that threaded a milestone label in here would be visibly wrong at the
/// call site rather than needing review to catch.
pub fn exit_ok(required_half: bool, coverage_clean: bool, allow_uncovered: bool) -> bool {
    if allow_uncovered {
        required_half
    } else {
        required_half && coverage_clean
    }
}

// ---------------------------------------------------------------------------------------------
// D-23.2: the per-FRS-section Must-count denominators.
//
// §14's Must-count column and its row set are *derived from the FRS*, not maintained by hand --
// they were wrong in two rows the day they were written, drifted in three more within one FRS
// revision, and omitted two sections entirely. This half generates the counts, emits them into
// `docs/03-test-plan.md`, and fails the build when §14's `### M9a re-audit` table disagrees. The
// three verdict columns stay hand-adjudicated and are outside this check (D-23.2, §22 R-14).
// ---------------------------------------------------------------------------------------------

/// One FRS section's Must-requirement denominator, in §14's row-label form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionCount {
    /// `"5.1 CHAIN"` -- the FRS section number, one ASCII space, the ids' area token.
    pub label: String,
    pub count: usize,
}

/// Groups `reqs` by FRS section, in first-appearance (i.e. FRS document) order.
///
/// Insertion-ordered by construction rather than by iterating a `HashMap`: this order reaches a
/// checked-in generated file, and a nondeterministic one would make `docs/03-test-plan.md` differ
/// between a local run and CI -- the same class of bug `main.rs`'s `read_dir` sort already fixed.
///
/// Two conditions are hard errors rather than a silently-plausible wrong denominator. Neither
/// arises on the tree today; both are specified so a future FRS edit fails loudly.
pub fn section_must_counts(reqs: &[Requirement]) -> Result<Vec<SectionCount>, String> {
    let mut out: Vec<SectionCount> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut areas: Vec<String> = Vec::new();

    for req in reqs {
        if req.section.is_empty() {
            return Err(format!(
                "{} appears before any numbered FRS heading, so D-23.2's per-section grouping has \
                 no section to attribute it to",
                req.id
            ));
        }
        let area = area_token(&req.id)?;
        match index.get(&req.section) {
            Some(&k) => {
                if areas[k] != area {
                    return Err(format!(
                        "FRS section {} carries Musts with disagreeing area tokens: {}, {} -- \
                         D-23.2's row label pairs one section number with one area, so this would \
                         silently split §14's row set in two",
                        req.section, areas[k], area
                    ));
                }
                out[k].count += 1;
            }
            None => {
                index.insert(req.section.clone(), out.len());
                areas.push(area.clone());
                out.push(SectionCount {
                    label: format!("{} {area}", req.section),
                    count: 1,
                });
            }
        }
    }

    Ok(out)
}

/// `"FR-CFG-010"` -> `"CFG"`, `"NFR-RT-010"` -> `"RT"`. D-23.2 takes the area from the ids rather
/// than from the heading, because §4's heading carries no `(CFG)` suffix.
fn area_token(id: &str) -> Result<String, String> {
    let parts: Vec<&str> = id.split('-').collect();
    if parts.len() < 3 || parts[..3].iter().any(|p| p.is_empty()) {
        return Err(format!(
            "`{id}` does not read FR-AREA-NNN or NFR-AREA-NNN, so no FRS area token can be taken \
             from it"
        ));
    }
    Ok(parts[1].to_string())
}

/// The `## Must requirements per FRS section` block appended to `docs/03-test-plan.md`.
///
/// Deliberately mirrors §14's row-label form and its bolded `**Total**` row so the generated block
/// and the hand-maintained table are diffable against each other by eye, not only by the tool.
pub fn render_section_counts(counts: &[SectionCount]) -> String {
    let mut out = String::from(
        "\n## Must requirements per FRS section\n\n\
         Derived from `docs/01-functional-requirements.md` per D-23.2: the section number comes \
         from the FRS heading in force, the area token from the requirement ids. \
         `docs/03-implementation-roadmap.md` §14's `### M9a re-audit` table must carry exactly \
         these rows, in this order, with these denominators; `cargo run -p xtask -- traceability` \
         fails when it does not. That table's three verdict columns are adjudicated by hand and \
         are outside this check.\n\n\
         | FRS area | Must count |\n\
         |---|---|\n",
    );
    for section in counts {
        out.push_str(&format!("| {} | {} |\n", section.label, section.count));
    }
    let total: usize = counts.iter().map(|s| s.count).sum();
    out.push_str(&format!("| **Total** | **{total}** |\n"));
    out
}

/// §14's `### M9a re-audit` table, as `(label, Must count)` rows in file order -- `**Total**`
/// included as an ordinary pair for [`compare_section_counts`] to special-case.
///
/// **Anchored on the heading, never on the header row.** The superseded M0 table
/// (`03-implementation-roadmap.md:1535`) carries a byte-identical five-column header and
/// known-wrong denominators that D-23.2 explicitly forbids rewriting, so an unanchored search
/// would check the wrong table and fail on arrival against a table this check must not touch.
/// Matching a line that *starts with* `### M9a re-audit` also excludes the two prose mentions of
/// the same phrase (`:1529` mid-sentence, `:2420` inside backticks) that a `contains` search hits.
///
/// The exact five-column header string is what fixes D-23.2's column order; "the first table after
/// the heading" would be wrong, because the reconciliation table (`:1785-1796`, header
/// `| | Musts |`) sits between the heading and the real one. The Done/Partial/Not-started cells are
/// read and discarded -- they are em dashes today and are outside any gate by D-23.2.
pub fn parse_roadmap_section_table(roadmap_text: &str) -> Result<Vec<(String, u32)>, String> {
    const HEADING: &str = "### M9a re-audit";
    const HEADER_ROW: &str = "| FRS area | Must count | Done | Partial | Not started |";
    const DELIMITER_ROW: &str = "|---|---|---|---|---|";

    let lines: Vec<&str> = roadmap_text.lines().collect();
    let headings: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_end().starts_with(HEADING))
        .map(|(i, _)| i)
        .collect();
    let heading = match headings.as_slice() {
        [i] => *i,
        [] => {
            return Err(format!(
                "docs/03-implementation-roadmap.md has no `{HEADING}` heading; D-23.2 fixes that \
                 heading as machine-parsed -- a rename must change xtask in the same commit"
            ));
        }
        many => {
            return Err(format!(
                "docs/03-implementation-roadmap.md has {} lines starting with `{HEADING}` (at {}); \
                 D-23.2 fixes one such heading, so which table to read is ambiguous",
                many.len(),
                many.iter()
                    .map(|i| format!(":{}", i + 1))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    };

    let mut header = None;
    for (i, line) in lines.iter().enumerate().skip(heading + 1) {
        if line.starts_with("## ") || line.starts_with("### ") {
            break;
        }
        if line.trim() == HEADER_ROW {
            header = Some(i);
            break;
        }
    }
    let Some(header) = header else {
        return Err(format!(
            "docs/03-implementation-roadmap.md's `{HEADING}` section has no `{HEADER_ROW}` row \
             before the next heading; D-23.2 fixes that column order as machine-parsed"
        ));
    };

    match lines.get(header + 1).map(|l| l.trim()) {
        Some(DELIMITER_ROW) => {}
        other => {
            return Err(format!(
                "docs/03-implementation-roadmap.md:{}: expected the `{DELIMITER_ROW}` row under \
                 `{HEADING}`'s table header, found `{}`",
                header + 2,
                other.unwrap_or("end of file")
            ));
        }
    }

    let mut rows = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(header + 2) {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            break;
        }
        let fields: Vec<&str> = trimmed.split('|').collect();
        // `"| a | b |"` splits to `["", " a ", " b ", ""]` -- the empty leading and trailing
        // fields are an artifact of the row's own delimiters, not cells.
        let cells: &[&str] = if fields.len() >= 2 {
            &fields[1..fields.len() - 1]
        } else {
            &[]
        };
        if cells.len() != 5 {
            return Err(format!(
                "docs/03-implementation-roadmap.md:{}: `{trimmed}` has {} cell(s); D-23.2's table \
                 has exactly five columns (FRS area, Must count, Done, Partial, Not started)",
                i + 1,
                cells.len()
            ));
        }
        let label = strip_bold(cells[0]);
        let raw = strip_bold(cells[1]);
        let count = raw
            .parse::<u32>()
            .map_err(|_| format!("row `{label}`: Must-count cell `{raw}` is not a number"))?;
        rows.push((label, count));
    }

    Ok(rows)
}

/// `"| **Total** |"`'s cells are bolded where the FRS-area rows' are bare, so both spellings must
/// reduce to the same string before comparison.
fn strip_bold(cell: &str) -> String {
    cell.replace("**", "").trim().to_string()
}

/// Every disagreement between the FRS-derived rows and §14's table, one legible line each; empty
/// when they agree. All of them are returned rather than the first, because a hand-edited table
/// typically drifts in several rows at once and one-at-a-time reporting turns one fix into five
/// runs.
///
/// `**Total**` is special-cased per D-23.2's implementation note: it is a **checked sum of the
/// Must-count column**, never matched against an FRS area and never counted as a row for the
/// set/order comparison. A row-set comparison that did not know this would reject the very table
/// the decision prescribes.
pub fn compare_section_counts(derived: &[SectionCount], table: &[(String, u32)]) -> Vec<String> {
    // The trailing row is split off only when it really is the Total row. Taking "the last row"
    // unconditionally would, on a table that simply lacks one, drop a genuine FRS-area row and
    // report it as missing on top of the real (missing-Total) defect.
    let has_total = table.last().is_some_and(|(label, _)| label == "Total");
    let (body, total) = match (has_total, table.split_last()) {
        (true, Some((last, rest))) => (rest, Some(last.1)),
        _ => (table, None),
    };

    let mut defects = Vec::new();

    for want in derived {
        if let Some((_, have)) = body.iter().find(|(label, _)| *label == want.label)
            && *have as usize != want.count
        {
            defects.push(format!(
                "row `{}`: the table says {have}, the FRS has {}",
                want.label, want.count
            ));
        }
    }
    let mut missing = 0usize;
    for want in derived {
        if !body.iter().any(|(label, _)| *label == want.label) {
            missing += 1;
            defects.push(format!(
                "row `{}`: missing from the table; the FRS has {} Must requirement(s) in that \
                 section",
                want.label, want.count
            ));
        }
    }
    let mut unknown = 0usize;
    for (label, _) in body {
        if !derived.iter().any(|want| want.label == *label) {
            unknown += 1;
            defects.push(format!(
                "row `{label}`: present in the table, but no FRS section with that number and area \
                 token has any Must requirement"
            ));
        }
    }

    // Order is checked only once the label sets agree -- on a set disagreement it would be noise
    // on top of the real defect. D-23.2 fixes the row-label form and the column order as
    // machine-parsed and the landed table is already in FRS document order, so this is strictly
    // stronger at zero cost today.
    if missing == 0
        && unknown == 0
        && body.len() == derived.len()
        && let Some(i) = (0..body.len()).find(|&i| body[i].0 != derived[i].label)
    {
        defects.push(format!(
            "row order: the table lists `{}` at row {}, the FRS order is `{}` there",
            body[i].0,
            i + 1,
            derived[i].label
        ));
    }

    match total {
        None => defects.push(
            "the table must end with a `| **Total** | … |` row; D-23.2 makes it a checked sum of \
             the Must-count column"
                .to_string(),
        ),
        Some(total) => {
            let body_sum: u32 = body.iter().map(|(_, n)| n).sum();
            if total != body_sum {
                defects.push(format!(
                    "`**Total**`: the table says {total}, but its own Must-count column sums to \
                     {body_sum}"
                ));
            }
            let derived_total: usize = derived.iter().map(|s| s.count).sum();
            if total as usize != derived_total {
                // Redundant when the row-by-row comparison above is already clean, but stated
                // separately so a table missing a row produces a legible message rather than only
                // an arithmetic one.
                defects.push(format!(
                    "`**Total**`: the table says {total}, the FRS has {derived_total} Must \
                     requirement(s) in {} sections",
                    derived.len()
                ));
            }
        }
    }

    defects
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real FR-LIB-020 `uncovered:` text D-23.1 prescribes
    /// (`docs/03-implementation-roadmap.md:2387-2388`), joined -- about 185 characters, which is
    /// why the wrapping rule exists at all.
    const FR_LIB_020_UNCOVERED: &str = "FR-LIB-020 — the off-the-audio-thread clause is exercised \
         only against a 6-file corpus in rt_stress.rs axis C, not the 10 000-file scale the Verify \
         method names; closes M9b";

    #[test]
    fn parses_a_simple_must_requirement() {
        let frs = "**FR-CHAIN-090 (Must)** — text.\n*Verify:* U.\n";
        let reqs = parse_must_requirements(frs).unwrap();
        assert_eq!(
            reqs,
            vec![Requirement {
                id: "FR-CHAIN-090".into(),
                verify: 'U',
                section: String::new(),
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
                verify: 'I',
                section: String::new(),
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

    fn ids(source: &str) -> Vec<String> {
        scan_annotations(source)
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect()
    }

    #[test]
    fn trace_annotation_parses_one_id() {
        assert_eq!(
            ids("// trace: FR-NAM-070\n#[test]\nfn t() {}\n"),
            ["FR-NAM-070"]
        );
    }

    #[test]
    fn trace_annotation_parses_several_ids() {
        assert_eq!(
            ids("    // trace: FR-NAM-070, NFR-RT-010\n    #[test]\n    fn t() {}\n"),
            ["FR-NAM-070", "NFR-RT-010"]
        );
    }

    #[test]
    fn trace_annotation_absent_is_empty() {
        assert!(ids("fn some_test() {}\n").is_empty());
    }

    #[test]
    fn a_marker_that_does_not_begin_the_line_is_not_a_tag() {
        // The `:310`/`:318` class: this module's own test fixtures used to parse as real tags
        // when the tool scanned its own source.
        assert!(ids("    let x = f(\"// trace: FR-NAM-070\");\n#[test]\nfn t() {}\n").is_empty());
        // The `ci.yml:109` class: prose that merely names the marker.
        assert!(
            ids(
                "      # `// trace:`/manual-test coverage found, and whether it is fresh\n  jobs:\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_non_id_shaped_token_is_a_hard_error() {
        let err =
            scan_annotations("// trace: FR-NAM-070, and whether the plan is\n#[test]\nfn t() {}\n")
                .unwrap_err();
        assert!(err.contains("and whether the plan is"), "{err}");
        assert!(err.starts_with("1:"), "{err}");
    }

    #[test]
    fn a_trace_partial_with_its_uncovered_line_parses() {
        let source = format!(
            "// trace-partial: FR-LIB-020\n// uncovered: {FR_LIB_020_UNCOVERED}\n#[test]\nfn t() {{}}\n"
        );
        let anns = scan_annotations(&source).unwrap();
        assert_eq!(
            anns,
            vec![Annotation {
                id: "FR-LIB-020".into(),
                line: 1,
                uncovered: Some(FR_LIB_020_UNCOVERED.to_string()),
            }]
        );
    }

    #[test]
    fn a_trace_partial_with_no_uncovered_line_is_a_hard_error() {
        let err =
            scan_annotations("\n// trace-partial: FR-LIB-020\n#[test]\nfn t() {}\n").unwrap_err();
        assert!(err.contains("FR-LIB-020"), "{err}");
        assert!(err.starts_with("2:"), "{err}");
    }

    #[test]
    fn an_uncovered_line_with_no_trace_partial_above_it_is_a_hard_error() {
        let err =
            scan_annotations("// uncovered: FR-LIB-020 — x; closes M9b\n#[test]\nfn t() {}\n")
                .unwrap_err();
        assert!(err.contains("uncovered"), "{err}");
        assert!(err.starts_with("1:"), "{err}");
    }

    #[test]
    fn consecutive_uncovered_lines_join_with_exactly_one_space() {
        // Each line's own leading and trailing whitespace is trimmed before the join, so the
        // trailing spaces on the first and the indentation on the second must not survive.
        //
        // The `\x20` escapes are load-bearing, not noise: `main.rs` scans this very file, and a
        // physical source line whose own start is `// uncovered:` parses as a real annotation
        // however deeply it is nested inside a string literal. That is the line-based scanner's
        // one residual limit, recorded in this module's header -- rule 1 shrank it from "anywhere
        // in a line" to "at the start of one", it did not close it. Writing the escape keeps the
        // fixture's *value* identical while stopping the physical line from starting with a
        // marker. Do not "tidy" these away.
        let source = "// trace-partial: FR-LIB-020\n\
             \x20// uncovered: FR-LIB-020 — the off-the-audio-thread clause is exercised only against a  \n\
             \x20   // uncovered: 6-file corpus in rt_stress.rs axis C, not the 10 000-file scale \
             the Verify method names; closes M9b\n\
             #[test]\nfn t() {}\n";
        let anns = scan_annotations(source).unwrap();
        assert_eq!(anns[0].uncovered.as_deref(), Some(FR_LIB_020_UNCOVERED));
    }

    #[test]
    fn an_uncovered_line_naming_a_different_id_is_a_hard_error() {
        let err = scan_annotations(
            "// trace-partial: FR-LIB-020\n// uncovered: FR-LIB-030 — x; closes M9b\n#[test]\nfn t() {}\n",
        )
        .unwrap_err();
        assert!(err.contains("FR-LIB-030"), "{err}");
    }

    #[test]
    fn an_uncovered_line_without_a_closing_milestone_is_a_hard_error() {
        let err = scan_annotations(
            "// trace-partial: FR-LIB-020\n// uncovered: FR-LIB-020 — the six-file corpus\n#[test]\nfn t() {}\n",
        )
        .unwrap_err();
        assert!(err.contains("closes"), "{err}");
    }

    #[test]
    fn a_closing_milestone_may_carry_a_phase_letter() {
        let source = "// trace-partial: FR-LIB-020\n// uncovered: FR-LIB-020 — x; closes M9b\n#[test]\nfn t() {}\n";
        assert!(scan_annotations(source).is_ok());
        let plain = "// trace-partial: FR-LIB-020\n// uncovered: FR-LIB-020 — x; closes M10\n#[test]\nfn t() {}\n";
        assert!(scan_annotations(plain).is_ok());
    }

    #[test]
    fn a_trace_partial_naming_more_than_one_id_is_a_hard_error() {
        let err = scan_annotations(
            "// trace-partial: FR-LIB-020, FR-LIB-030\n// uncovered: FR-LIB-020 — x; closes M9b\n#[test]\nfn t() {}\n",
        )
        .unwrap_err();
        assert!(err.contains("one id"), "{err}");
    }

    #[test]
    fn a_tag_above_a_comment_line_is_a_hard_error() {
        let err = scan_annotations("// trace: FR-NAM-070\n/// doc comment\n#[test]\nfn t() {}\n")
            .unwrap_err();
        assert!(err.contains("comment line at :2"), "{err}");
    }

    #[test]
    fn a_tag_with_nothing_after_it_is_a_hard_error() {
        let err = scan_annotations("// trace: FR-NAM-070\n\n\n").unwrap_err();
        assert!(err.contains("end of file"), "{err}");
    }

    #[test]
    fn rule_1_not_adjacency_is_what_stops_a_prose_mention_of_the_marker() {
        // D-23.1's own clause credits adjacency with stopping `ci.yml:109`; D-23.1's
        // *Consequence (added M9a)* note records that attribution as wrong, and `check_adjacency`'s
        // doc comment carries the same correction. This pins it: the prose line is followed by a
        // perfectly admissible anchor, so `check_adjacency` would *accept* the line -- it never
        // runs, because rule 1 stops the line from being a tag at all.
        assert!(
            ids(
                "      # `// trace:`/manual-test coverage found, and whether it is fresh\n  jobs:\n"
            )
            .is_empty()
        );
        // The positive control: same anchor, same file type, a marker that really does begin its
        // line. Without this the assertion above would pass for any reason at all.
        assert_eq!(ids("  # trace: NFR-QUAL-010\n  jobs:\n"), ["NFR-QUAL-010"]);

        // What adjacency does close is the fourth class: a well-formed tag naming real ids at the
        // start of its own line, detached from any declaration. Here, one adrift inside a prose
        // block; `a_tag_above_a_comment_line_is_a_hard_error` and
        // `a_tag_with_nothing_after_it_is_a_hard_error` cover the drifted and end-of-file shapes.
        // See `consecutive_uncovered_lines_join_with_exactly_one_space` for why `\x20` leads the
        // continuation line that would otherwise begin with a marker.
        let inside_prose = "/// A coverage note, in prose.\n\
             \x20// trace: NFR-QUAL-010\n\
             /// ...and the prose continues, so the tag anchors nothing.\n\
             pub fn f() {}\n";
        assert!(
            scan_annotations(inside_prose)
                .unwrap_err()
                .contains("comment line at :3"),
            "{:?}",
            scan_annotations(inside_prose)
        );
    }

    #[test]
    fn every_declaration_form_this_tree_actually_uses_is_an_admissible_anchor() {
        // One case per real tag-site class audited across the scanned set at M9a.
        for anchor in [
            "#[test]",
            "#[bench]",
            "#[cfg(test)]",
            "#![no_main]",
            "fn main() {",
            "pub struct Foo {",
            "use std::path::Path;",
            "pub mod ir;",
            "const FIXTURES: &str = \"namir-fixtures\";",
        ] {
            let source = format!("// trace: FR-NAM-070\n{anchor}\n");
            assert_eq!(ids(&source), ["FR-NAM-070"], "anchor: {anchor}");
        }
        for anchor in [
            "[workspace]",
            "missing_docs = \"warn\"",
            "deny = [",
            "[graph]",
        ] {
            let source = format!("# trace: FR-NAM-070\n{anchor}\n");
            assert_eq!(ids(&source), ["FR-NAM-070"], "anchor: {anchor}");
        }
        for anchor in ["build-test:", "  - name: check", "msrv:"] {
            let source = format!("  # trace: FR-NAM-070\n{anchor}\n");
            assert_eq!(ids(&source), ["FR-NAM-070"], "anchor: {anchor}");
        }
    }

    #[test]
    fn adjacency_for_a_trace_partial_resumes_after_the_uncovered_block() {
        // See `consecutive_uncovered_lines_join_with_exactly_one_space` for why `\x20` leads
        // every continuation line that would otherwise begin with a marker.
        let source = "// trace-partial: FR-LIB-020\n\
             \x20// uncovered: FR-LIB-020 — a; closes M9b\n\
             \x20// uncovered: still the same field; closes M9b\n\
             \n\
             #[test]\nfn t() {}\n";
        assert!(
            scan_annotations(source).is_ok(),
            "{:?}",
            scan_annotations(source)
        );
        let bad = "// trace-partial: FR-LIB-020\n\
             \x20// uncovered: FR-LIB-020 — a; closes M9b\n\
             /// a doc comment between the tag and the test\n\
             #[test]\nfn t() {}\n";
        assert!(
            scan_annotations(bad)
                .unwrap_err()
                .contains("comment line at :3")
        );
    }

    #[test]
    fn fn_name_matches_the_established_convention() {
        assert!(fn_name_embeds_id(
            "    #[test]\n    fn fr_nam_070_crossfade_glitch_free() {}",
            "FR-NAM-070"
        ));
        assert!(fn_name_embeds_id(
            "#[test]\nfn nfr_rt_010_three_axes_run_concurrently() {}",
            "NFR-RT-010"
        ));
    }

    #[test]
    fn fn_name_does_not_spuriously_match_a_longer_id() {
        assert!(!fn_name_embeds_id(
            "#[test]\nfn fr_io_0100_something() {}",
            "FR-IO-010"
        ));
    }

    #[test]
    fn fn_name_fallback_needs_a_test_attribute() {
        assert!(!fn_name_embeds_id(
            "fn fr_nam_070_crossfade_glitch_free() {}",
            "FR-NAM-070"
        ));
    }

    #[test]
    fn fn_name_fallback_ignores_a_doc_comment_mention() {
        // The `:135` class -- this module's own doc comment used to put `xtask` in the generated
        // plan as a component covering FR-NAM-070, which xtask does not test.
        assert!(!fn_name_embeds_id(
            "/// e.g. `FR-NAM-070` -> `fn fr_nam_070_...`\npub fn something() {}",
            "FR-NAM-070"
        ));
    }

    #[test]
    fn fn_name_fallback_ignores_a_string_literal() {
        // The `:331` class.
        assert!(!fn_name_embeds_id(
            "        #[test]\n        \"fn fr_nam_070_crossfade_glitch_free() {}\",",
            "FR-NAM-070"
        ));
    }

    #[test]
    fn build_report_flags_a_must_requirement_with_no_coverage() {
        let reqs = vec![Requirement {
            id: "FR-X-010".into(),
            verify: 'U',
            section: String::new(),
        }];
        let report = build_report(&reqs, &[], &HashMap::new(), &HashMap::new());
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].id, "FR-X-010");
    }

    #[test]
    fn build_report_resolves_a_manual_verified_requirement_by_filename() {
        let reqs = vec![Requirement {
            id: "FR-IO-020".into(),
            verify: 'M',
            section: String::new(),
        }];
        let docs = vec![(
            "fr-io-020-wasapi-exclusive-mode.md".to_string(),
            "irrelevant content".to_string(),
        )];
        let report = build_report(&reqs, &docs, &HashMap::new(), &HashMap::new());
        assert!(report.missing.is_empty());
        assert_eq!(
            report.manual_hits.get("FR-IO-020").unwrap(),
            "fr-io-020-wasapi-exclusive-mode.md"
        );
    }

    #[test]
    fn build_report_resolves_a_manual_verified_requirement_declared_in_the_literal_block() {
        // fr-io-010-device-enumeration.md's own filename only names FR-IO-010, but its
        // `**Requirement (literal):**` block declares FR-IO-040 too -- a real file this project
        // already has, and the one legitimate multi-requirement document in the tree.
        let reqs = vec![Requirement {
            id: "FR-IO-040".into(),
            verify: 'M',
            section: String::new(),
        }];
        let docs = vec![(
            "fr-io-010-device-enumeration.md".to_string(),
            "**Requirement (literal):** FR-IO-010 ... FR-IO-040 ...".to_string(),
        )];
        let report = build_report(&reqs, &docs, &HashMap::new(), &HashMap::new());
        assert!(report.missing.is_empty());
        assert_eq!(
            report.manual_hits.get("FR-IO-040").unwrap(),
            "fr-io-010-device-enumeration.md"
        );
    }

    #[test]
    fn a_prose_mention_outside_the_literal_block_credits_nothing() {
        // The defect roadmap §15 item 15 names, in the shape it actually had: FR-UI-020 resolving
        // to the CLAP audio-port script because that script mentions it once, in a parenthesis
        // about watching a meter. The document is a real one and the sentence is a reasonable
        // sentence; it is simply not a claim to verify FR-UI-020.
        let reqs = vec![Requirement {
            id: "FR-UI-020".into(),
            verify: 'M',
            section: String::new(),
        }];
        let docs = vec![(
            "fr-clap-030-audio-ports-negotiation.md".to_string(),
            "**Requirement (literal):** the plugin shall declare audio port configurations\n\
             corresponding to FR-CHAIN-060.\n\
             \n\
             ## Script\n\
             1. Play a signal and watch the meter (FR-UI-020) on both output channels.\n"
                .to_string(),
        )];
        let report = build_report(&reqs, &docs, &HashMap::new(), &HashMap::new());
        assert!(report.manual_hits.is_empty());
        assert_eq!(report.missing.len(), 1, "{:?}", report.missing);
        assert_eq!(report.missing[0].id, "FR-UI-020");
    }

    #[test]
    fn a_declaration_block_wrapping_across_lines_is_read_whole() {
        // The block ends at the first blank line, not at the end of the marker's own line. Real
        // declarations wrap: fr-io-010's names its second id three lines in.
        let ids = declared_requirement_ids(
            "# heading\n\
             \n\
             **Requirement (literal):** FR-IO-010 -- \"the user shall be able to select an audio\n\
             input device and an audio output device\" ... and FR-IO-040 -- \"the user shall be\n\
             able to select sample rate and buffer size\".\n\
             \n\
             **Verify: M.** FR-IO-090 is mentioned here and must not be picked up.\n",
        );
        assert_eq!(ids, vec!["FR-IO-010", "FR-IO-040"]);
    }

    #[test]
    fn a_document_with_no_declaration_block_declares_nothing() {
        // Not an error -- such a document still resolves its own requirement by filename. The
        // whole-file match this replaced would have credited every id in the prose below.
        assert!(
            declared_requirement_ids("# heading\n\nSee FR-UI-070 and FR-PKG-030.\n").is_empty()
        );
    }

    #[test]
    fn build_report_treats_process_verified_as_always_covered() {
        let reqs = vec![Requirement {
            id: "NFR-QUAL-020".into(),
            verify: 'P',
            section: String::new(),
        }];
        let report = build_report(&reqs, &[], &HashMap::new(), &HashMap::new());
        assert!(report.missing.is_empty());
    }

    #[test]
    fn build_report_resolves_a_source_verified_requirement() {
        let reqs = vec![Requirement {
            id: "FR-NAM-070".into(),
            verify: 'I',
            section: String::new(),
        }];
        let mut hits = HashMap::new();
        hits.insert("FR-NAM-070".to_string(), vec!["namir-engine".to_string()]);
        let report = build_report(&reqs, &[], &hits, &HashMap::new());
        assert!(report.missing.is_empty());
    }

    fn one_partial(id: &str, component: &str, text: &str) -> HashMap<String, Vec<PartialHit>> {
        let mut partials = HashMap::new();
        partials.insert(
            id.to_string(),
            vec![PartialHit {
                component: component.to_string(),
                uncovered: text.to_string(),
            }],
        );
        partials
    }

    #[test]
    fn build_report_counts_a_partial_as_coverage_for_the_ordinary_run() {
        // D-23.1: a partial counts as covered here. The teeth are D-18.5's M13 flip and D-23.2's
        // rule that a Partial is not Done -- not this gate.
        let reqs = vec![Requirement {
            id: "FR-LIB-020".into(),
            verify: 'I',
            section: String::new(),
        }];
        let partials = one_partial("FR-LIB-020", "namir-worker", FR_LIB_020_UNCOVERED);
        let report = build_report(&reqs, &[], &HashMap::new(), &partials);
        assert!(report.missing.is_empty());
    }

    #[test]
    fn a_partial_naming_a_manual_verified_requirement_is_a_hard_error() {
        // D-23.1's PARTIAL-render guarantee is an absolute, and `build_report`'s `'M'` arm resolves
        // FR-IO-020 by its manual-test document without ever consulting `partial_hits`. Refused
        // rather than rendered: the requirement's own `Verify:` method is a written script, and no
        // source annotation is one.
        let err = check_partial_verify_code("FR-IO-020", 'M', 7).unwrap_err();
        assert!(err.starts_with("7: "), "{err}");
        assert!(err.contains("FR-IO-020"), "{err}");
        assert!(err.contains("`Verify: M`"), "{err}");
        assert!(err.contains("manual-test script"), "{err}");
    }

    #[test]
    fn a_partial_naming_a_process_verified_requirement_is_a_hard_error() {
        // Reported with the FRS's own spelling of the code, `Process` -- the parser keeps only the
        // first character, and `Verify: P` is a code no FRS line carries.
        let err = check_partial_verify_code("NFR-QUAL-020", 'P', 3).unwrap_err();
        assert!(err.starts_with("3: "), "{err}");
        assert!(err.contains("`Verify: Process`"), "{err}");
        assert!(!err.contains("`Verify: P`"), "{err}");
    }

    #[test]
    fn a_partial_naming_any_other_verify_code_is_accepted() {
        // The five codes whose evidence really is an annotated artifact. FR-LIB-020, D-23.1's own
        // worked example, is `Verify: I`.
        for verify in ['U', 'I', 'G', 'B', 'S'] {
            assert!(
                check_partial_verify_code("FR-LIB-020", verify, 1).is_ok(),
                "Verify: {verify}"
            );
        }
    }

    #[test]
    fn render_test_plan_marks_unresolved_requirements_explicitly() {
        let reqs = vec![Requirement {
            id: "FR-X-010".into(),
            verify: 'U',
            section: String::new(),
        }];
        let report = build_report(&reqs, &[], &HashMap::new(), &HashMap::new());
        let text = render_test_plan(&reqs, &report);
        assert!(text.contains("FR-X-010"));
        assert!(text.contains("UNRESOLVED"));
    }

    #[test]
    fn render_test_plan_marks_a_partial_and_carries_its_uncovered_text_verbatim() {
        let reqs = vec![Requirement {
            id: "FR-LIB-020".into(),
            verify: 'I',
            section: String::new(),
        }];
        let partials = one_partial("FR-LIB-020", "namir-worker", FR_LIB_020_UNCOVERED);
        let report = build_report(&reqs, &[], &HashMap::new(), &partials);
        let text = render_test_plan(&reqs, &report);
        assert!(
            text.contains(&format!(
                "| FR-LIB-020 | I | **PARTIAL** — `namir-worker`: {FR_LIB_020_UNCOVERED} |"
            )),
            "{text}"
        );
        assert!(text.contains("PARTIAL"));
    }

    #[test]
    fn render_test_plan_escapes_a_pipe_inside_the_uncovered_text() {
        let reqs = vec![Requirement {
            id: "FR-LIB-020".into(),
            verify: 'I',
            section: String::new(),
        }];
        let partials = one_partial(
            "FR-LIB-020",
            "namir-worker",
            "FR-LIB-020 — the a|b split is untested; closes M9b",
        );
        let report = build_report(&reqs, &[], &HashMap::new(), &partials);
        let text = render_test_plan(&reqs, &report);
        assert!(text.contains(r"a\|b"), "{text}");
        // Exactly three unescaped cell separators plus the leading/trailing ones on the row.
        let row = text
            .lines()
            .find(|l| l.starts_with("| FR-LIB-020 "))
            .unwrap();
        assert_eq!(row.matches("\\|").count(), 1, "{row}");
    }

    #[test]
    fn partial_row_ids_are_exactly_the_rows_the_plan_marks_partial() {
        // R-13's printed count is this function's length, so a disagreement with
        // `render_test_plan`'s dispatch would make the number name a set the plan does not carry.
        // Checked against the rendered text rather than against the reasoning.
        //
        // The map carries one partial of every shape that can reach a `Report`: an ordinary
        // `Verify: I` Must (rendered), the two codes `check_partial_verify_code` refuses upstream
        // (resolved by their own method, consulting no partial), and an id the Must list does not
        // carry at all -- a Should, a Could, or one the FRS never had.
        let reqs = vec![
            Requirement {
                id: "FR-LIB-020".into(),
                verify: 'I',
                section: "5.10".into(),
            },
            Requirement {
                id: "FR-IO-020".into(),
                verify: 'M',
                section: "5.13".into(),
            },
            Requirement {
                id: "NFR-QUAL-020".into(),
                verify: 'P',
                section: "6.4".into(),
            },
        ];
        let mut partials = one_partial("FR-LIB-020", "namir-worker", FR_LIB_020_UNCOVERED);
        for id in ["FR-IO-020", "NFR-QUAL-020", "FR-CFG-040"] {
            partials.insert(
                id.to_string(),
                vec![PartialHit {
                    component: "namir-engine".into(),
                    uncovered: format!("{id} — a named gap; closes M9b"),
                }],
            );
        }
        let docs = vec![(
            "fr-io-020-wasapi-exclusive-mode.md".to_string(),
            String::new(),
        )];
        let report = build_report(&reqs, &docs, &HashMap::new(), &partials);

        assert_eq!(partial_row_ids(&reqs, &report), ["FR-LIB-020"]);

        let text = render_test_plan(&reqs, &report);
        let rows: Vec<String> = text
            .lines()
            .filter(|line| line.starts_with("| ") && line.contains("**PARTIAL**"))
            .map(|line| line.split('|').nth(1).unwrap().trim().to_string())
            .collect();
        assert_eq!(rows, partial_row_ids(&reqs, &report), "{text}");
        // The two refused codes still resolve by their own method, and the unlisted id has no row
        // to appear in at all -- which is exactly why counting it would be counting nothing.
        assert!(
            text.contains(
                "| FR-IO-020 | M | `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md` |"
            ),
            "{text}"
        );
        assert!(
            text.contains("| NFR-QUAL-020 | P | process (review + commit order"),
            "{text}"
        );
        assert!(!text.contains("FR-CFG-040"), "{text}");
    }

    // --- D-23.2: derived denominators, §14's table, and the `**Total**` special case -----------

    fn req(id: &str, section: &str) -> Requirement {
        Requirement {
            id: id.into(),
            verify: 'U',
            section: section.into(),
        }
    }

    fn counts(pairs: &[(&str, usize)]) -> Vec<SectionCount> {
        pairs
            .iter()
            .map(|(label, count)| SectionCount {
                label: (*label).into(),
                count: *count,
            })
            .collect()
    }

    fn table(pairs: &[(&str, u32)]) -> Vec<(String, u32)> {
        pairs
            .iter()
            .map(|(label, n)| ((*label).to_string(), *n))
            .collect()
    }

    #[test]
    fn heading_section_number_reads_both_real_frs_forms() {
        // `:133` carries a trailing period after the number; `:161` does not. D-23.2's
        // implementation note exists precisely because these two forms coexist.
        assert_eq!(
            heading_section_number("## 4. Product configurations").as_deref(),
            Some("4")
        );
        assert_eq!(
            heading_section_number("### 5.1 Signal chain (CHAIN)").as_deref(),
            Some("5.1")
        );
    }

    #[test]
    fn heading_section_number_rejects_non_section_headings() {
        for line in [
            "# Namir — Functional Requirements Specification",
            "#### 5.1.1 Something",
            "## Definitions",
            "##5.1 No space",
        ] {
            assert_eq!(heading_section_number(line), None, "line: {line}");
        }
    }

    #[test]
    fn a_requirement_takes_the_heading_in_force_at_its_own_line() {
        // The forward scan for `*Verify:*` crosses the `### 5.2` heading here. D-23.2 keys on the
        // heading in force when the requirement is *parsed*, so this stays 5.1.
        let frs = "### 5.1 Signal chain (CHAIN)\n\
                   **FR-CHAIN-010 (Must)** — text.\n\
                   ### 5.2 Input stage (IN)\n\
                   *Verify:* U.\n";
        let reqs = parse_must_requirements(frs).unwrap();
        assert_eq!(reqs[0].section, "5.1");
    }

    #[test]
    fn a_parent_heading_is_superseded_before_any_requirement_is_parsed() {
        // `## 5. Functional requirements` legitimately sets section 5, but `### 5.1` supersedes it
        // before the first Must line, so no `5 <AREA>` row is ever produced.
        let frs = "## 5. Functional requirements\n\
                   ### 5.1 Signal chain (CHAIN)\n\
                   **FR-CHAIN-010 (Must)** — text.\n\
                   *Verify:* U.\n";
        let reqs = parse_must_requirements(frs).unwrap();
        assert_eq!(reqs[0].section, "5.1");
    }

    #[test]
    fn section_must_counts_groups_by_section_in_first_appearance_order() {
        // 5.10 before 5.2 on purpose: the output order is documentary, not lexicographic.
        let reqs = vec![
            req("FR-CHAIN-010", "5.1"),
            req("FR-CHAIN-020", "5.1"),
            req("FR-LIB-010", "5.10"),
            req("FR-IN-010", "5.2"),
        ];
        assert_eq!(
            section_must_counts(&reqs).unwrap(),
            counts(&[("5.1 CHAIN", 2), ("5.10 LIB", 1), ("5.2 IN", 1)])
        );
    }

    #[test]
    fn section_must_counts_rejects_a_requirement_with_no_heading() {
        let err = section_must_counts(&[req("FR-CFG-010", "")]).unwrap_err();
        assert!(err.contains("FR-CFG-010"), "{err}");
    }

    #[test]
    fn section_must_counts_rejects_disagreeing_area_tokens_in_one_section() {
        let err = section_must_counts(&[req("FR-NAM-010", "5.4"), req("FR-OTHER-010", "5.4")])
            .unwrap_err();
        assert!(err.contains("5.4"), "{err}");
        assert!(err.contains("NAM") && err.contains("OTHER"), "{err}");
    }

    /// A synthetic roadmap carrying the M0/M9a collision: an earlier, byte-identical five-column
    /// header above a `### M9a re-audit` heading, plus the intervening two-column reconciliation
    /// table the real document has between the heading and the real table.
    fn two_table_roadmap(m9a_rows: &str) -> String {
        format!(
            "## 14. Appendix\n\
             \n\
             | FRS area | Must count | Done | Partial | Not started |\n\
             |---|---|---|---|---|\n\
             | 5.1 CHAIN | 7 | 0 | 2 | 5 |\n\
             | 5.4 NAM | 11 | 3 | 4 | 4 |\n\
             \n\
             ### M9a re-audit — corrected row set and denominators (2026-08-08)\n\
             \n\
             | | Musts |\n\
             |---|---|\n\
             | The table above | 117 |\n\
             \n\
             | FRS area | Must count | Done | Partial | Not started |\n\
             |---|---|---|---|---|\n\
             {m9a_rows}\
             \n\
             ## 15. Appendix\n"
        )
    }

    #[test]
    fn parse_roadmap_section_table_reads_the_m9a_table_not_the_m0_one() {
        let text = two_table_roadmap(
            "| 5.1 CHAIN | 8 | — | — | — |\n| 5.4 NAM | 13 | — | — | — |\n| **Total** | **21** | — | — | — |\n",
        );
        assert_eq!(
            parse_roadmap_section_table(&text).unwrap(),
            table(&[("5.1 CHAIN", 8), ("5.4 NAM", 13), ("Total", 21)])
        );
    }

    #[test]
    fn parse_roadmap_section_table_skips_an_intervening_unrelated_table() {
        // The real document puts the two-column reconciliation table (`| | Musts |`,
        // `03-implementation-roadmap.md:1785-1796`) between the heading and the real table, so
        // "the first table after the heading" is the wrong rule and the exact header is the right
        // one. Its `| The table above | 117 |` row must not reach the result.
        let text =
            two_table_roadmap("| 4 CFG | 3 | — | — | — |\n| **Total** | **3** | — | — | — |\n");
        let rows = parse_roadmap_section_table(&text).unwrap();
        assert_eq!(rows, table(&[("4 CFG", 3), ("Total", 3)]));
        assert!(!rows.iter().any(|(label, _)| label.contains("table above")));
    }

    #[test]
    fn parse_roadmap_section_table_strips_bold_and_ignores_em_dash_verdicts() {
        // The verdict cells are em dashes, not blanks -- a five-cell row whose last three cells are
        // non-empty must parse, and those cells must be discarded.
        let text = two_table_roadmap("| **Total** | **130** | — | — | — |\n");
        assert_eq!(
            parse_roadmap_section_table(&text).unwrap(),
            table(&[("Total", 130)])
        );
    }

    #[test]
    fn parse_roadmap_section_table_errors_without_the_heading() {
        let text = "## 14. Appendix\n\n\
                    | FRS area | Must count | Done | Partial | Not started |\n\
                    |---|---|---|---|---|\n\
                    | 5.1 CHAIN | 8 | — | — | — |\n";
        let err = parse_roadmap_section_table(text).unwrap_err();
        assert!(err.contains("M9a re-audit"), "{err}");
    }

    #[test]
    fn parse_roadmap_section_table_errors_when_the_exact_header_is_absent() {
        let text = "### M9a re-audit — x\n\n\
                    | FRS area | Musts | Done | Partial | Not started |\n\
                    |---|---|---|---|---|\n\
                    | 5.1 CHAIN | 8 | — | — | — |\n";
        let err = parse_roadmap_section_table(text).unwrap_err();
        assert!(err.contains("FRS area | Must count"), "{err}");
    }

    #[test]
    fn parse_roadmap_section_table_errors_on_a_malformed_delimiter() {
        let text = "### M9a re-audit — x\n\
                    | FRS area | Must count | Done | Partial | Not started |\n\
                    |---|---|---|\n\
                    | 5.1 CHAIN | 8 | — | — | — |\n";
        let err = parse_roadmap_section_table(text).unwrap_err();
        assert!(err.contains("|---|---|---|---|---|"), "{err}");
    }

    #[test]
    fn parse_roadmap_section_table_errors_on_a_row_with_the_wrong_cell_count() {
        let text = two_table_roadmap("| 5.1 CHAIN | 8 | — | — |\n");
        let err = parse_roadmap_section_table(&text).unwrap_err();
        assert!(err.contains("five columns"), "{err}");
        assert!(err.contains("5.1 CHAIN"), "{err}");
    }

    #[test]
    fn parse_roadmap_section_table_errors_on_a_non_numeric_count() {
        let text = two_table_roadmap("| 5.1 CHAIN | eight | — | — | — |\n");
        let err = parse_roadmap_section_table(&text).unwrap_err();
        assert!(err.contains("`eight` is not a number"), "{err}");
    }

    #[test]
    fn compare_section_counts_is_silent_on_a_matching_table() {
        let derived = counts(&[("5.1 CHAIN", 8), ("5.4 NAM", 13)]);
        let have = table(&[("5.1 CHAIN", 8), ("5.4 NAM", 13), ("Total", 21)]);
        assert_eq!(
            compare_section_counts(&derived, &have),
            Vec::<String>::new()
        );
    }

    #[test]
    fn compare_section_counts_reports_a_differing_denominator() {
        let derived = counts(&[("5.4 NAM", 13)]);
        let have = table(&[("5.4 NAM", 11), ("Total", 11)]);
        let defects = compare_section_counts(&derived, &have);
        assert_eq!(defects.len(), 2, "{defects:?}");
        assert_eq!(
            defects[0],
            "row `5.4 NAM`: the table says 11, the FRS has 13"
        );
        assert!(defects[1].contains("the FRS has 13 Must requirement(s) in 1 sections"));
    }

    #[test]
    fn compare_section_counts_reports_a_row_missing_from_the_table() {
        let derived = counts(&[("5.14 ERR", 6), ("5.15 PKG", 4)]);
        let have = table(&[("5.14 ERR", 6), ("Total", 6)]);
        let defects = compare_section_counts(&derived, &have);
        assert!(
            defects[0].starts_with(
                "row `5.15 PKG`: missing from the table; the FRS has 4 Must requirement(s)"
            ),
            "{defects:?}"
        );
    }

    #[test]
    fn compare_section_counts_reports_a_table_row_with_no_frs_counterpart() {
        let derived = counts(&[("5.1 CHAIN", 8)]);
        let have = table(&[("5.1 CHAIN", 8), ("9.9 GHOST", 0), ("Total", 8)]);
        let defects = compare_section_counts(&derived, &have);
        assert_eq!(defects.len(), 1, "{defects:?}");
        assert!(
            defects[0].starts_with("row `9.9 GHOST`: present in the table"),
            "{defects:?}"
        );
    }

    #[test]
    fn compare_section_counts_reports_a_reordered_row_set() {
        let derived = counts(&[("5.2 IN", 3), ("5.10 LIB", 5)]);
        let have = table(&[("5.10 LIB", 5), ("5.2 IN", 3), ("Total", 8)]);
        let defects = compare_section_counts(&derived, &have);
        assert_eq!(defects.len(), 1, "{defects:?}");
        assert!(defects[0].starts_with("row order:"), "{defects:?}");
        assert!(defects[0].contains("`5.10 LIB` at row 1"), "{defects:?}");
    }

    #[test]
    fn compare_section_counts_checks_total_against_the_columns_own_sum() {
        let derived = counts(&[("5.1 CHAIN", 8), ("5.4 NAM", 13)]);
        let have = table(&[("5.1 CHAIN", 8), ("5.4 NAM", 13), ("Total", 117)]);
        let defects = compare_section_counts(&derived, &have);
        assert_eq!(
            defects[0],
            "`**Total**`: the table says 117, but its own Must-count column sums to 21"
        );
    }

    #[test]
    fn compare_section_counts_reports_a_missing_total_row_without_losing_a_real_row() {
        // The last row here is a genuine FRS-area row. Splitting it off unconditionally would
        // report it as missing on top of the real defect.
        let derived = counts(&[("5.1 CHAIN", 8), ("5.4 NAM", 13)]);
        let have = table(&[("5.1 CHAIN", 8), ("5.4 NAM", 13)]);
        let defects = compare_section_counts(&derived, &have);
        assert_eq!(defects.len(), 1, "{defects:?}");
        assert!(
            defects[0].contains("must end with a `| **Total** | … |` row"),
            "{defects:?}"
        );
    }

    #[test]
    fn compare_section_counts_never_matches_total_against_an_frs_area() {
        // A derived row set that does not contain "Total" must not produce an "unknown row"
        // defect for the table's Total row -- the case D-23.2's implementation note calls out.
        let derived = counts(&[("6.8 DOC", 3)]);
        let have = table(&[("6.8 DOC", 3), ("Total", 3)]);
        assert!(compare_section_counts(&derived, &have).is_empty());
    }

    #[test]
    fn render_section_counts_emits_the_table_with_a_bolded_total() {
        let text = render_section_counts(&counts(&[("4 CFG", 3), ("5.1 CHAIN", 8)]));
        assert!(
            text.contains("\n## Must requirements per FRS section\n"),
            "{text}"
        );
        assert!(
            text.contains("| FRS area | Must count |\n|---|---|\n"),
            "{text}"
        );
        assert!(
            text.contains("\n| 4 CFG | 3 |\n| 5.1 CHAIN | 8 |\n"),
            "{text}"
        );
        assert!(text.ends_with("| **Total** | **11** |\n"), "{text}");
    }

    #[test]
    fn render_test_plan_appends_the_section_block_after_the_requirement_table() {
        let reqs = vec![Requirement {
            id: "FR-CFG-010".into(),
            verify: 'S',
            section: "4".into(),
        }];
        let report = build_report(&reqs, &[], &HashMap::new(), &HashMap::new());
        let text = render_test_plan(&reqs, &report);
        let (before, after) = text
            .split_once("\n## Must requirements per FRS section\n")
            .expect("the block is appended");
        // The requirement table is left byte-identical: it still ends with its own last row.
        assert!(
            before.ends_with("| FR-CFG-010 | S | **UNRESOLVED** |\n"),
            "{before}"
        );
        assert!(after.contains("| 4 CFG | 1 |\n"), "{after}");
        assert!(
            after.trim_end().ends_with("| **Total** | **1** |"),
            "{after}"
        );
    }

    #[test]
    fn the_real_frs_yields_the_denominators_d_23_2_states() {
        // D-23.2's "130 Musts across 24 sections", and the four denominators §14's own
        // reconciliation table (`03-implementation-roadmap.md:1785-1796`) singles out as
        // corrections. `include_str!` rather than a filesystem read, so this module keeps its
        // no-I/O contract.
        let frs = include_str!("../../docs/01-functional-requirements.md");
        let reqs = parse_must_requirements(frs).unwrap();
        let derived = section_must_counts(&reqs).unwrap();

        assert_eq!(derived.len(), 24, "{derived:?}");
        assert_eq!(derived.iter().map(|s| s.count).sum::<usize>(), 130);
        for (label, want) in [
            ("4 CFG", 3),
            ("5.1 CHAIN", 8),
            ("5.4 NAM", 13),
            ("5.12 CLAP", 11),
        ] {
            let got = derived
                .iter()
                .find(|s| s.label == label)
                .unwrap_or_else(|| panic!("no row {label} in {derived:?}"));
            assert_eq!(got.count, want, "row {label}");
        }
    }

    // --- D-18.5: the split gate's CLI surface and exit-status derivation -----------------------

    #[test]
    fn exit_ok_is_the_whole_truth_table() {
        // (required_half, coverage_clean, allow_uncovered) -> expected.
        for (required, coverage, allow, want) in [
            (true, true, false, true),
            (true, false, false, false),
            (false, true, false, false),
            (false, false, false, false),
            // The whole point of the flag: a fresh plan with uncovered Musts passes.
            (true, false, true, true),
            (true, true, true, true),
            // ...and the flag never softens the required half.
            (false, true, true, false),
            (false, false, true, false),
        ] {
            assert_eq!(
                exit_ok(required, coverage, allow),
                want,
                "required={required} coverage={coverage} allow={allow}"
            );
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn traceability_args_parse_in_any_order_and_tolerate_repeats() {
        for (input, write, allow) in [
            (vec![], false, false),
            (args(&["--write"]), true, false),
            (args(&["--allow-uncovered"]), false, true),
            (args(&["--write", "--allow-uncovered"]), true, true),
            (args(&["--allow-uncovered", "--write"]), true, true),
            (args(&["--write", "--write"]), true, false),
            (
                args(&["--allow-uncovered", "--allow-uncovered", "--write"]),
                true,
                true,
            ),
        ] {
            assert_eq!(
                parse_traceability_args(&input).unwrap(),
                TraceabilityArgs {
                    write,
                    allow_uncovered: allow
                },
                "{input:?}"
            );
        }
    }

    #[test]
    fn an_unrecognised_traceability_argument_is_an_error_naming_it() {
        // Fail-safe by design: no typo can select the lenient mode by accident, and at M13's
        // close-out a `ci.yml` still passing the deleted flag hard-fails rather than quietly
        // running the strict form.
        let err = parse_traceability_args(&args(&["--allow-uncoverd"])).unwrap_err();
        assert!(err.contains("--allow-uncoverd"), "{err}");
        assert!(err.contains("--allow-uncovered"), "{err}");
        assert!(parse_traceability_args(&args(&["--write", "-w"])).is_err());
    }

    #[test]
    fn scan_requirement_ids_reads_ids_out_of_real_prose() {
        assert_eq!(scan_requirement_ids("**FR-NAM-150**"), ["FR-NAM-150"]);
        // `NFR-` is tried first and the match consumes its own length, so no spurious FR-PERF-030.
        assert_eq!(scan_requirement_ids("NFR-PERF-030"), ["NFR-PERF-030"]);
        assert_eq!(
            scan_requirement_ids("FR-CLAP-030, FR-CLAP-040"),
            ["FR-CLAP-030", "FR-CLAP-040"]
        );
        assert_eq!(scan_requirement_ids("FR-IO-010's own text"), ["FR-IO-010"]);
        assert_eq!(
            scan_requirement_ids("`FR-CFG-020` — an M8 exit item"),
            ["FR-CFG-020"]
        );
    }

    #[test]
    fn scan_requirement_ids_rejects_near_misses_and_does_not_expand_shorthand() {
        assert!(scan_requirement_ids("FR-IO-0100 is four digits").is_empty());
        assert!(scan_requirement_ids("no requirement id on this line").is_empty());
        assert!(scan_requirement_ids("").is_empty());
        // An id must start at a boundary, so an embedded one is not a match.
        assert!(scan_requirement_ids("xFR-IO-010").is_empty());
        // Shorthand runs resolve only their first, full id -- guessing that `-020` means
        // FR-PKG-020 is exactly the inference that goes wrong silently.
        assert_eq!(
            scan_requirement_ids("FR-PKG-010/-020/-030/-040"),
            ["FR-PKG-010"]
        );
        assert_eq!(
            scan_requirement_ids("FR-PKG-010, -020, -030"),
            ["FR-PKG-010"]
        );
    }

    #[test]
    fn closing_milestone_reads_the_label_a_partial_declares() {
        assert_eq!(
            closing_milestone(FR_LIB_020_UNCOVERED),
            Some("M9b"),
            "{FR_LIB_020_UNCOVERED}"
        );
        assert_eq!(
            closing_milestone("FR-X-010 — the gap; closes M10"),
            Some("M10")
        );
        assert_eq!(closing_milestone("FR-X-010 — the gap"), None);
        assert_eq!(closing_milestone("FR-X-010 — the gap; closes soon"), None);
    }

    #[test]
    fn the_real_frs_agrees_with_the_real_section_14_table() {
        // The end-to-end shape of the gate, against both real documents.
        let frs = include_str!("../../docs/01-functional-requirements.md");
        let roadmap = include_str!("../../docs/03-implementation-roadmap.md");
        let derived = section_must_counts(&parse_must_requirements(frs).unwrap()).unwrap();
        let parsed = parse_roadmap_section_table(roadmap).unwrap();
        assert_eq!(
            compare_section_counts(&derived, &parsed),
            Vec::<String>::new()
        );
    }
}
