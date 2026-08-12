//! FR-PKG-010's in-repo assertion: does `.github/workflows/release.yml` actually have the shape the
//! requirement's own `Verify:` method describes?
//!
//! > **FR-PKG-010 (Must)** — Namir shall produce an installable distribution for each supported
//! > platform, built by CI from a tagged source tree.
//! > *Verify:* S — the release workflow is triggered by a tag, runs on every tier-1 and tier-2
//! > platform, and every published distribution is an artifact of that workflow rather than of a
//! > local build.
//!
//! Three clauses, checked by three separate functions ([`clause_1_triggered_by_a_tag`],
//! [`clause_2_every_tier_1_and_tier_2_platform`], [`clause_3_every_distribution_is_this_workflows`])
//! so that a failure names which one broke rather than "the workflow is wrong". Each returns a
//! violations list in the same shape as `bundle::check` and `identity::check`.
//!
//! # Why a test here and not a `# trace:` in the workflow
//!
//! `docs/03-implementation-roadmap.md` §15 item 10 was resolved at M13's start, and this module is
//! that resolution: `xtask traceability`'s scanned-file list (`main.rs`) stays hard-coded to
//! `ci.yml` and `fuzz.yml`, and FR-PKG-010 closes through an in-repo assertion instead. FRS §10's
//! adequacy rule admits "an annotated test **or** `xtask` subcommand"; a test is the cheaper of the
//! two and `xtask/**` is already scanned. The reasons that decided it are worth restating where the
//! code is: a `# trace:` line in a workflow asserts nothing a reader can check, FR-PKG-010 has
//! separable clauses that one bare tag would flatten into a single unexamined claim, and a workflow
//! is inspected by nobody between releases while a test runs on every pull request.
//!
//! # Why there is a YAML parser in here
//!
//! There is no YAML crate in `xtask`, and adding one would need a row in `docs/02-architecture.md`
//! §17's dependency register. That is a decision, not a detail, so this module does not take it: it
//! parses the block-style subset of YAML that GitHub Actions workflows are written in
//! ([`parse`]) — nested mappings by indentation, block sequences, flow sequences of scalars,
//! block scalars (`|`, `>`) and comments — and refuses anything it does not understand rather than
//! guessing. That subset is fixed by what `release.yml` and `ci.yml` are allowed to contain, which
//! is this repository's own business; a workflow that needed anchors, flow mappings or multi-
//! document streams would fail to parse here and loudly, which is the right failure for a check
//! whose whole job is to understand the file.
//!
//! **The parser is the weakest link, so it is tested on its own** rather than trusted through the
//! clause checks: [`mod tests`]'s first block feeds it structures with known shapes. What it
//! deliberately does *not* do is interpret YAML semantics — `on` stays the string `"on"` rather
//! than becoming YAML 1.1's boolean `true`, quoted and unquoted scalars are the same thing once
//! the quotes come off, and no type inference happens anywhere.
//!
//! # What "structural" means here, and why it matters
//!
//! A check that greps a workflow for the word `windows-latest` and calls that "runs on every
//! platform" is exactly the tag D-23.1 exists to stop. So each clause below reads the parsed
//! document: which keys `on:` has, which jobs exist, what each job's `runs-on` is, which steps a
//! job has *and in what order*, what a step's `with:` mapping contains. The negative tests in this
//! module's second block are the evidence that this has teeth — each one mutates a workflow that
//! passes into one that must not, and asserts the clause reports it.

use std::path::Path;

/// The workflow this module asserts against, relative to the repository root.
pub const WORKFLOW_PATH: &str = ".github/workflows/release.yml";

/// The FRS, from which [`tier_1_and_2_platforms`] reads §1.4's platform table.
pub const FRS_PATH: &str = "docs/01-functional-requirements.md";

// ---------------------------------------------------------------------------------------------
// A YAML subset
// ---------------------------------------------------------------------------------------------

/// A parsed node of the block-style YAML subset [`parse`] accepts.
///
/// Mappings keep their source order (a `Vec` of pairs, not a map) because step order is
/// load-bearing for [`clause_3_every_distribution_is_this_workflows`] and duplicate keys are a
/// malformed workflow rather than something to silently resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Yaml {
    /// A plain scalar, with surrounding quotes removed and block scalars joined by newlines.
    Scalar(String),
    /// A block or flow sequence.
    Seq(Vec<Yaml>),
    /// A mapping, in source order.
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    /// The value for `key`, if this is a mapping containing it.
    pub fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Yaml::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// This node's text, if it is a scalar.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Yaml::Scalar(text) => Some(text),
            _ => None,
        }
    }

    /// This node's items, if it is a sequence.
    pub fn as_seq(&self) -> Option<&[Yaml]> {
        match self {
            Yaml::Seq(items) => Some(items),
            _ => None,
        }
    }

    /// This node's entries, if it is a mapping.
    pub fn as_map(&self) -> Option<&[(String, Yaml)]> {
        match self {
            Yaml::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// The scalar at `key`, if this is a mapping whose value for `key` is a scalar.
    pub fn str_at(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Yaml::as_str)
    }

    /// The items at `key`, treating a lone scalar as a one-item sequence — which is how YAML and
    /// GitHub Actions both read `needs: publish` versus `needs: [a, b]`.
    pub fn seq_at(&self, key: &str) -> Vec<&Yaml> {
        match self.get(key) {
            Some(Yaml::Seq(items)) => items.iter().collect(),
            Some(other @ Yaml::Scalar(_)) => vec![other],
            _ => Vec::new(),
        }
    }
}

/// One source line, kept raw: comment stripping happens per-line at interpretation time, never up
/// front, because a `#` inside a block scalar is a shell comment and must survive.
struct Line {
    indent: usize,
    raw: String,
}

impl Line {
    fn is_ignorable(&self) -> bool {
        let trimmed = self.raw.trim();
        trimmed.is_empty() || trimmed.starts_with('#')
    }

    fn is_seq_item(&self) -> bool {
        let trimmed = self.raw.trim_start();
        trimmed == "-" || trimmed.starts_with("- ")
    }
}

/// Parses the block-style YAML subset described in this module's header.
///
/// # Errors
///
/// Returns a message naming the line number for anything the subset does not cover: tab
/// indentation, a mapping line with no `:`, a duplicate key, an unexpected indent, or a document
/// that is neither a mapping nor a sequence at the top level.
pub fn parse(text: &str) -> Result<Yaml, String> {
    let mut lines = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let raw = raw.trim_end_matches('\r').to_string();
        if raw.trim().is_empty() {
            lines.push(Line { indent: 0, raw });
            continue;
        }
        if raw.starts_with('\t') || raw.trim_start_matches(' ').starts_with('\t') {
            return Err(format!(
                "line {}: tab indentation -- YAML forbids it and this parser will not guess",
                index + 1
            ));
        }
        let indent = raw.len() - raw.trim_start().len();
        lines.push(Line { indent, raw });
    }

    // `---` document markers are accepted and skipped at the head; a second document is not.
    let mut cursor = 0;
    skip_ignorable(&lines, &mut cursor);
    if cursor < lines.len() && lines[cursor].raw.trim() == "---" {
        cursor += 1;
    }

    let node = parse_node(&lines, &mut cursor, 0)?;
    skip_ignorable(&lines, &mut cursor);
    if cursor < lines.len() {
        return Err(format!(
            "line {}: trailing content after the document (multi-document streams are not \
             supported)",
            cursor + 1
        ));
    }
    Ok(node)
}

fn skip_ignorable(lines: &[Line], cursor: &mut usize) {
    while *cursor < lines.len() && lines[*cursor].is_ignorable() {
        *cursor += 1;
    }
}

fn parse_node(lines: &[Line], cursor: &mut usize, indent: usize) -> Result<Yaml, String> {
    skip_ignorable(lines, cursor);
    if *cursor >= lines.len() {
        return Ok(Yaml::Scalar(String::new()));
    }
    if lines[*cursor].is_seq_item() {
        parse_seq(lines, cursor, indent)
    } else {
        parse_map(lines, cursor, indent)
    }
}

fn parse_map(lines: &[Line], cursor: &mut usize, indent: usize) -> Result<Yaml, String> {
    let mut entries: Vec<(String, Yaml)> = Vec::new();

    loop {
        skip_ignorable(lines, cursor);
        if *cursor >= lines.len() {
            break;
        }
        let line = &lines[*cursor];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(format!(
                "line {}: unexpected indentation (expected {indent} spaces)",
                *cursor + 1
            ));
        }
        if line.is_seq_item() {
            return Err(format!(
                "line {}: a sequence item where a mapping key was expected",
                *cursor + 1
            ));
        }

        let content = strip_comment(line.raw.trim());
        let (key, rest) = split_key(&content, *cursor + 1)?;
        if entries.iter().any(|(existing, _)| *existing == key) {
            return Err(format!("line {}: duplicate key `{key}`", *cursor + 1));
        }
        let key_line = *cursor;
        *cursor += 1;

        let value = if let Some(chomp) = block_scalar_indicator(&rest) {
            parse_block_scalar(lines, cursor, indent, chomp)
        } else if rest.is_empty() {
            parse_child(lines, cursor, indent)?
        } else if rest.starts_with('[') {
            parse_flow_seq(&rest, key_line + 1)?
        } else if rest.starts_with('{') {
            return Err(format!(
                "line {}: flow mappings are not supported by this parser",
                key_line + 1
            ));
        } else {
            Yaml::Scalar(unquote(&rest))
        };
        entries.push((key, value));
    }

    Ok(Yaml::Map(entries))
}

/// The value of a key whose own line carried none: either a more-indented block, or a sequence at
/// the key's own indentation (both are legal YAML), or nothing.
fn parse_child(lines: &[Line], cursor: &mut usize, indent: usize) -> Result<Yaml, String> {
    let mut lookahead = *cursor;
    skip_ignorable(lines, &mut lookahead);
    if lookahead >= lines.len() {
        return Ok(Yaml::Scalar(String::new()));
    }
    let next = &lines[lookahead];
    if next.indent > indent {
        *cursor = lookahead;
        let child_indent = next.indent;
        return parse_node(lines, cursor, child_indent);
    }
    if next.indent == indent && next.is_seq_item() {
        *cursor = lookahead;
        return parse_seq(lines, cursor, indent);
    }
    Ok(Yaml::Scalar(String::new()))
}

fn parse_seq(lines: &[Line], cursor: &mut usize, indent: usize) -> Result<Yaml, String> {
    let mut items = Vec::new();

    loop {
        skip_ignorable(lines, cursor);
        if *cursor >= lines.len() {
            break;
        }
        let line = &lines[*cursor];
        if line.indent < indent || !line.is_seq_item() {
            break;
        }
        if line.indent > indent {
            return Err(format!(
                "line {}: unexpected indentation in a sequence (expected {indent} spaces)",
                *cursor + 1
            ));
        }

        let after_dash = &line.raw[line.indent + 1..];
        let content_offset = after_dash.len() - after_dash.trim_start().len();
        let content = after_dash.trim_start().to_string();
        let content_column = line.indent + 1 + content_offset;
        *cursor += 1;

        if content.is_empty() {
            // `-` alone: the item is the block beneath it.
            items.push(parse_child(lines, cursor, indent)?);
            continue;
        }

        // An item that begins with `key: ...` is a mapping whose first key sits on the dash line
        // and whose remaining keys sit at the content column. Re-project it as its own line list
        // and parse that, so one code path handles every nesting depth inside an item.
        let mut item_lines = vec![Line {
            indent: content_column,
            raw: format!("{}{content}", " ".repeat(content_column)),
        }];
        while *cursor < lines.len() {
            let candidate = &lines[*cursor];
            if candidate.is_ignorable() {
                item_lines.push(Line {
                    indent: candidate.indent,
                    raw: candidate.raw.clone(),
                });
                *cursor += 1;
                continue;
            }
            if candidate.indent < content_column {
                break;
            }
            item_lines.push(Line {
                indent: candidate.indent,
                raw: candidate.raw.clone(),
            });
            *cursor += 1;
        }
        // Trailing blank lines belong to whatever comes next, not to this item.
        while item_lines.last().is_some_and(Line::is_ignorable) {
            item_lines.pop();
            *cursor -= 1;
        }

        let mut item_cursor = 0;
        let item = if split_key(&strip_comment(&content), 0).is_ok() {
            parse_map(&item_lines, &mut item_cursor, content_column)?
        } else if item_lines.len() == 1 {
            Yaml::Scalar(unquote(&strip_comment(&content)))
        } else {
            return Err(format!(
                "line {}: a sequence item that is neither a scalar nor a mapping",
                *cursor
            ));
        };
        items.push(item);
    }

    Ok(Yaml::Seq(items))
}

/// `|`, `|-`, `>`, `>+` and friends. Returns the folding flag; the chomping indicator is parsed but
/// not acted on, since nothing here depends on a trailing newline.
fn block_scalar_indicator(rest: &str) -> Option<bool> {
    let rest = rest.trim();
    let (marker, tail) = rest.split_at(rest.chars().next().map_or(0, char::len_utf8));
    match marker {
        "|" | ">"
            if tail
                .chars()
                .all(|c| c == '-' || c == '+' || c.is_ascii_digit()) =>
        {
            Some(marker == ">")
        }
        _ => None,
    }
}

fn parse_block_scalar(lines: &[Line], cursor: &mut usize, indent: usize, fold: bool) -> Yaml {
    let mut collected: Vec<String> = Vec::new();
    let mut block_indent: Option<usize> = None;

    while *cursor < lines.len() {
        let line = &lines[*cursor];
        if line.raw.trim().is_empty() {
            collected.push(String::new());
            *cursor += 1;
            continue;
        }
        if line.indent <= indent {
            break;
        }
        let base = *block_indent.get_or_insert(line.indent);
        let stripped = if line.raw.len() >= base {
            line.raw[base.min(line.raw.len())..].to_string()
        } else {
            line.raw.trim_start().to_string()
        };
        collected.push(stripped);
        *cursor += 1;
    }

    while collected.last().is_some_and(|l| l.is_empty()) {
        collected.pop();
    }
    Yaml::Scalar(collected.join(if fold { " " } else { "\n" }))
}

fn parse_flow_seq(rest: &str, line_no: usize) -> Result<Yaml, String> {
    let trimmed = rest.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| {
            format!("line {line_no}: an unterminated flow sequence (it must close on one line)")
        })?;
    if inner.contains('[') || inner.contains('{') {
        return Err(format!(
            "line {line_no}: nested flow collections are not supported by this parser"
        ));
    }
    let items = inner
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| Yaml::Scalar(unquote(item)))
        .collect();
    Ok(Yaml::Seq(items))
}

/// Splits `key: value` / `key:`. A `:` only separates when followed by a space or end of line, so
/// `run: echo a:b` keeps its colon.
fn split_key(content: &str, line_no: usize) -> Result<(String, String), String> {
    let bytes = content.as_bytes();
    let mut quote: Option<u8> = None;
    for (index, &byte) in bytes.iter().enumerate() {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b':' => {
                let next = bytes.get(index + 1);
                if next.is_none() || next == Some(&b' ') {
                    let key = unquote(content[..index].trim());
                    if key.is_empty() {
                        return Err(format!("line {line_no}: a mapping entry with an empty key"));
                    }
                    let value = content[index + 1..].trim().to_string();
                    return Ok((key, value));
                }
            }
            None => {}
        }
    }
    Err(format!(
        "line {line_no}: `{content}` is neither a mapping entry nor a sequence item"
    ))
}

/// Removes a trailing ` # comment`, respecting quotes. A `#` that is not preceded by whitespace is
/// part of the value (`#ff6600`, `refs/tags/#`), which is YAML's own rule.
fn strip_comment(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut quote: Option<u8> = None;
    for (index, &byte) in bytes.iter().enumerate() {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'#' && (index == 0 || bytes[index - 1] == b' ') => {
                return content[..index].trim_end().to_string();
            }
            None => {}
        }
    }
    content.to_string()
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    for quote in ['"', '\''] {
        if trimmed.len() >= 2 && trimmed.starts_with(quote) && trimmed.ends_with(quote) {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

// ---------------------------------------------------------------------------------------------
// The workflow, as this check reads it
// ---------------------------------------------------------------------------------------------

/// One job of a parsed workflow.
pub struct Job<'a> {
    /// The job's key under `jobs:` — what another job's `needs:` names.
    pub id: &'a str,
    node: &'a Yaml,
}

impl<'a> Job<'a> {
    /// The runner label, e.g. `windows-latest`.
    pub fn runs_on(&self) -> Option<&'a str> {
        self.node.str_at("runs-on")
    }

    /// The job's steps, in order. An absent or malformed `steps:` is an empty slice; the clause
    /// checks report that as a violation rather than this accessor.
    pub fn steps(&self) -> &'a [Yaml] {
        self.node.get("steps").and_then(Yaml::as_seq).unwrap_or(&[])
    }

    /// The job ids this job declares a dependency on.
    pub fn needs(&self) -> Vec<&'a str> {
        self.node
            .seq_at("needs")
            .into_iter()
            .filter_map(Yaml::as_str)
            .collect()
    }

    /// Every step's `run:` text, in order.
    pub fn runs(&self) -> Vec<&'a str> {
        self.steps().iter().filter_map(step_run).collect()
    }

    /// The index of the first step whose `run:` contains `needle`.
    pub fn index_of_run(&self, needle: &str) -> Option<usize> {
        self.steps()
            .iter()
            .position(|step| step_run(step).is_some_and(|text| text.contains(needle)))
    }

    /// The index of the first step whose `uses:` names `action` (matched before the `@version`).
    pub fn index_of_uses(&self, action: &str) -> Option<usize> {
        self.steps()
            .iter()
            .position(|step| step_uses(step).is_some_and(|uses| uses.starts_with(action)))
    }

    /// Every step whose `uses:` names `action`.
    pub fn steps_using(&self, action: &str) -> Vec<&'a Yaml> {
        self.steps()
            .iter()
            .filter(|step| step_uses(step).is_some_and(|uses| uses.starts_with(action)))
            .collect()
    }
}

/// A step's `uses:` value.
pub fn step_uses(step: &Yaml) -> Option<&str> {
    step.str_at("uses")
}

/// A step's `run:` text.
pub fn step_run(step: &Yaml) -> Option<&str> {
    step.str_at("run")
}

/// The jobs of a parsed workflow, in source order.
///
/// # Errors
///
/// Returns a message if there is no `jobs:` mapping at all.
pub fn jobs(doc: &Yaml) -> Result<Vec<Job<'_>>, String> {
    let jobs = doc
        .get("jobs")
        .and_then(Yaml::as_map)
        .ok_or_else(|| "the workflow has no `jobs:` mapping".to_string())?;
    Ok(jobs
        .iter()
        .map(|(id, node)| Job { id, node })
        .collect::<Vec<_>>())
}

/// The action every build job publishes its distribution with, and the one the release job
/// collects them with. Named once so the clause checks and their negative tests agree.
const UPLOAD_ACTION: &str = "actions/upload-artifact";
const DOWNLOAD_ACTION: &str = "actions/download-artifact";
const CHECKOUT_ACTION: &str = "actions/checkout";

/// A job that uploads a distribution artifact. What makes a job a *build* job for every clause
/// below is exactly this — it publishes something into the run — rather than its name.
fn is_build_job(job: &Job<'_>) -> bool {
    !job.steps_using(UPLOAD_ACTION).is_empty()
}

/// The job that creates the GitHub Release.
fn is_publish_job(job: &Job<'_>) -> bool {
    job.runs()
        .iter()
        .any(|run| run.contains("gh release create"))
}

// ---------------------------------------------------------------------------------------------
// Clause 1 — triggered by a tag, from a tagged source tree
// ---------------------------------------------------------------------------------------------

/// FR-PKG-010's "built by CI **from a tagged source tree**" and its `Verify:` line's "triggered by
/// a tag".
///
/// Four things, and the last is the one a substring search would miss: a workflow can be tag-
/// triggered and still build something else entirely if a job checks out a different ref.
///
/// 1. `on:` carries `push.tags`, non-empty.
/// 2. `on.push` carries no `branches`/`branches-ignore` — a branch push must not cut a release.
/// 3. `push` is the *only* trigger. A `workflow_dispatch` would let a release be cut from an
///    arbitrary ref, which is the case "from a tagged source tree" exists to exclude, and
///    `schedule`/`pull_request` would each publish from something that is not a tag.
/// 4. Every `actions/checkout` step declares no `ref:` — the default is the pushed tag's own
///    commit, and naming a ref is how a workflow silently stops building the tagged tree.
pub fn clause_1_triggered_by_a_tag(doc: &Yaml) -> Vec<String> {
    let mut violations = Vec::new();

    // `on` survives as the string key here: this parser does no YAML 1.1 boolean coercion (see the
    // module header), which is what the file on disk means and what GitHub reads.
    let Some(triggers) = doc.get("on") else {
        violations.push("clause 1: the workflow has no `on:` key at all".to_string());
        return violations;
    };
    let Some(trigger_map) = triggers.as_map() else {
        violations.push("clause 1: `on:` is not a mapping, so it declares no tag trigger".into());
        return violations;
    };

    for (name, _) in trigger_map {
        if name != "push" {
            violations.push(format!(
                "clause 1: `on.{name}` -- `push` on a tag must be the only trigger, since any \
                 other one can fire against a ref that is not a tag"
            ));
        }
    }

    let Some(push) = triggers.get("push") else {
        violations.push("clause 1: `on:` declares no `push:` trigger".to_string());
        return violations;
    };

    let tags = push.seq_at("tags");
    if tags.is_empty() {
        violations.push(
            "clause 1: `on.push` declares no `tags:` filter, so it is not tag-triggered".into(),
        );
    }
    for key in ["branches", "branches-ignore"] {
        if push.get(key).is_some() {
            violations.push(format!(
                "clause 1: `on.push.{key}` -- a branch push must not produce a release"
            ));
        }
    }

    match jobs(doc) {
        Err(e) => violations.push(format!("clause 1: {e}")),
        Ok(jobs) => {
            for job in &jobs {
                for step in job.steps_using(CHECKOUT_ACTION) {
                    if let Some(with) = step.get("with")
                        && let Some(reference) = with.str_at("ref")
                    {
                        violations.push(format!(
                            "clause 1: job `{}` checks out `ref: {reference}` -- with no `ref:` \
                             the checkout is the pushed tag's own commit, which is what \"from a \
                             tagged source tree\" means",
                            job.id
                        ));
                    }
                }
            }
        }
    }

    violations
}

// ---------------------------------------------------------------------------------------------
// Clause 2 — every tier-1 and tier-2 platform
// ---------------------------------------------------------------------------------------------

/// One row of FRS §1.4's platform table, split per platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierPlatform {
    /// The FRS's own tier word: `Primary` or `Secondary`.
    pub tier: String,
    /// The platform name as written, e.g. `Windows 11`.
    pub platform: String,
    /// The architecture in the parentheses, e.g. `x86-64`.
    pub architecture: String,
}

/// Platform name prefix, architecture, and the runner-label prefix that provides them.
///
/// **The FRS never defines "tier-1" and "tier-2".** §1.4's table says Primary / Secondary /
/// Prospective, and four requirements (FR-PKG-010, NFR-PORT-040, NFR-PORT-060, NFR-QUAL-050) then
/// use the tier vocabulary. `docs/03-implementation-roadmap.md` §14 records that gap explicitly and
/// takes the only available reading — tier-1 = Primary, tier-2 = Secondary. This table is where
/// that reading is encoded for this check, in one place, so that a future FRS §1.4 row this
/// mapping does not know about is reported by name instead of being silently skipped.
const RUNNER_FOR_PLATFORM: [(&str, &str, &str); 3] = [
    ("Windows", "x86-64", "windows-"),
    ("Linux", "x86-64", "ubuntu-"),
    ("macOS", "aarch64", "macos-"),
];

/// The tier-1 (Primary) and tier-2 (Secondary) rows of FRS §1.4's target-platform table.
///
/// # Errors
///
/// Returns a message if the table is absent, if a row's platform cell is not `Name (arch)`, or if a
/// row carries a tier word this function does not know — a new tier is a decision, not something to
/// guess at.
pub fn tier_1_and_2_platforms(frs: &str) -> Result<Vec<TierPlatform>, String> {
    let mut rows = Vec::new();
    let mut in_table = false;

    for line in frs.lines() {
        let trimmed = line.trim();
        if !in_table {
            if trimmed.starts_with("| Tier ") && trimmed.contains("| Platform ") {
                in_table = true;
            }
            continue;
        }
        if !trimmed.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() < 2 || cells[0].starts_with("---") {
            continue;
        }
        let tier = cells[0];
        match tier {
            "Primary" | "Secondary" => {}
            "Prospective" => continue,
            other => {
                return Err(format!(
                    "FRS §1.4 has a tier this check does not know: `{other}`. Tier-1 and tier-2 are \
                     undefined in the FRS and read here as Primary and Secondary; a new tier needs \
                     a decision, not a guess"
                ));
            }
        }
        for entry in cells[1].split(',') {
            let entry = entry.trim();
            let (platform, architecture) = entry
                .split_once('(')
                .and_then(|(name, rest)| rest.strip_suffix(')').map(|arch| (name.trim(), arch)))
                .ok_or_else(|| {
                    format!("FRS §1.4: platform cell `{entry}` is not of the form `Name (arch)`")
                })?;
            rows.push(TierPlatform {
                tier: tier.to_string(),
                platform: platform.to_string(),
                architecture: architecture.trim().to_string(),
            });
        }
    }

    if rows.is_empty() {
        return Err("FRS §1.4's target-platform table was not found".to_string());
    }
    Ok(rows)
}

/// FR-PKG-010's "an installable distribution **for each supported platform**" and its `Verify:`
/// line's "runs on **every** tier-1 and tier-2 platform".
///
/// The set is not hard-coded: it is read out of FRS §1.4 by [`tier_1_and_2_platforms`], mapped to
/// runner-label prefixes by [`RUNNER_FOR_PLATFORM`], and each member must be served by a job that
/// both runs on such a runner **and uploads an artifact** — a job that runs on macOS and publishes
/// nothing does not give the platform a distribution.
pub fn clause_2_every_tier_1_and_tier_2_platform(doc: &Yaml, frs: &str) -> Vec<String> {
    let mut violations = Vec::new();

    let platforms = match tier_1_and_2_platforms(frs) {
        Ok(platforms) => platforms,
        Err(e) => return vec![format!("clause 2: {e}")],
    };
    let jobs = match jobs(doc) {
        Ok(jobs) => jobs,
        Err(e) => return vec![format!("clause 2: {e}")],
    };

    for required in &platforms {
        let Some((_, _, runner_prefix)) = RUNNER_FOR_PLATFORM.iter().find(|(name, arch, _)| {
            required.platform.starts_with(name) && required.architecture == *arch
        }) else {
            violations.push(format!(
                "clause 2: FRS §1.4 lists {} `{}` ({}) and this check knows no runner that \
                 provides it -- extend RUNNER_FOR_PLATFORM deliberately, or the release does not \
                 cover a supported platform",
                required.tier, required.platform, required.architecture
            ));
            continue;
        };

        let serving: Vec<&Job<'_>> = jobs
            .iter()
            .filter(|job| {
                job.runs_on()
                    .is_some_and(|label| label.starts_with(runner_prefix))
            })
            .collect();

        if serving.is_empty() {
            violations.push(format!(
                "clause 2: no job runs on a `{runner_prefix}*` runner, so {} `{}` ({}) gets no \
                 distribution",
                required.tier, required.platform, required.architecture
            ));
            continue;
        }
        if !serving.iter().any(|job| is_build_job(job)) {
            violations.push(format!(
                "clause 2: {} `{}` ({}) has a job but none of its `{runner_prefix}*` jobs uploads \
                 an artifact, so nothing is produced for it",
                required.tier, required.platform, required.architecture
            ));
        }
    }

    violations
}

// ---------------------------------------------------------------------------------------------
// Clause 3 — every published distribution is an artifact of this workflow
// ---------------------------------------------------------------------------------------------

/// Runner-label prefix -> the packaging command that produces that platform's distribution.
/// Each is the entry point the platform's own `packaging/<os>/README.md` names.
const PACKAGING_STEP: [(&str, &str); 3] = [
    ("windows-", "iscc"),
    ("macos-", "make_installer.sh"),
    ("ubuntu-", "tar --create"),
];

/// Commands that build or package. A publish job running any of these would be producing a
/// distribution at publish time rather than publishing one the build jobs produced.
const BUILD_COMMANDS: [&str; 6] = [
    "cargo build",
    "cargo run",
    "iscc",
    "make_installer.sh",
    "tar --create",
    "Compress-Archive",
];

/// Repository directories. A path from one of these in the publish job would mean a published file
/// came from a source tree rather than from this run's artifacts.
const REPOSITORY_PATHS: [&str; 4] = ["target/", "packaging/", "crates/", "images/"];

/// FR-PKG-010's "every published distribution is an artifact of that workflow rather than of a
/// local build".
///
/// This is a property of the *shape* of the pipeline, and it is checked as one — never by looking
/// for a filename. Two halves.
///
/// **The publish job cannot publish anything it did not receive from this run.** Exactly one job
/// creates the release; it needs every build job; it checks nothing out (so there is no source tree
/// under it); its only input is `actions/download-artifact` with none of `run-id`, `repository` or
/// `github-token`, the three inputs that would let it reach into another run or repository; it runs
/// no build or packaging command; and no run step in it names a repository path.
///
/// **Each build job produced what it uploaded, here, in D-18.3's order.** `cargo build --release`,
/// then `xtask bundle`, then that platform's packaging entry point, then the upload — by step
/// index, so a reordered job fails — with every uploaded path under `target/` (a build output, not
/// a checked-in file) and `if-no-files-found: error`, so an empty artifact cannot be published as
/// if it were a distribution.
pub fn clause_3_every_distribution_is_this_workflows(doc: &Yaml) -> Vec<String> {
    let mut violations = Vec::new();

    let jobs = match jobs(doc) {
        Ok(jobs) => jobs,
        Err(e) => return vec![format!("clause 3: {e}")],
    };
    let build_jobs: Vec<&Job<'_>> = jobs.iter().filter(|job| is_build_job(job)).collect();
    let publish_jobs: Vec<&Job<'_>> = jobs.iter().filter(|job| is_publish_job(job)).collect();

    if build_jobs.is_empty() {
        violations.push("clause 3: no job uploads an artifact, so nothing is produced here".into());
    }

    let publish = match publish_jobs.as_slice() {
        [only] => Some(*only),
        [] => {
            violations.push(
                "clause 3: no job creates a GitHub Release (`gh release create`), so what gets \
                 published is outside this workflow"
                    .to_string(),
            );
            None
        }
        many => {
            violations.push(format!(
                "clause 3: {} jobs create a GitHub Release -- publication must have one place a \
                 reviewer can check",
                many.len()
            ));
            None
        }
    };

    if let Some(publish) = publish {
        violations.extend(check_publish_job(publish, &build_jobs));
    }
    for job in &build_jobs {
        violations.extend(check_build_job(job));
    }

    violations
}

fn check_publish_job(publish: &Job<'_>, build_jobs: &[&Job<'_>]) -> Vec<String> {
    let mut violations = Vec::new();
    let id = publish.id;

    let needs = publish.needs();
    for build in build_jobs {
        if !needs.contains(&build.id) {
            violations.push(format!(
                "clause 3: publish job `{id}` does not `needs: {}` -- it could publish while that \
                 platform's distribution is missing or still building",
                build.id
            ));
        }
    }

    if is_build_job(publish) {
        violations.push(format!(
            "clause 3: publish job `{id}` uploads an artifact of its own -- publication and \
             production must not be the same job"
        ));
    }

    if publish.index_of_uses(CHECKOUT_ACTION).is_some() {
        violations.push(format!(
            "clause 3: publish job `{id}` checks the repository out -- with no source tree under \
             it, it cannot publish a locally built file even by accident"
        ));
    }

    let downloads = publish.steps_using(DOWNLOAD_ACTION);
    if downloads.is_empty() {
        violations.push(format!(
            "clause 3: publish job `{id}` never downloads this run's artifacts, so whatever it \
             publishes came from somewhere else"
        ));
    }

    let mut download_paths = Vec::new();
    for step in &downloads {
        let with = step.get("with");
        // These three inputs are exactly what turns `download-artifact` from "this run" into "some
        // other run, or another repository entirely".
        for input in ["run-id", "repository", "github-token"] {
            if with.and_then(|with| with.get(input)).is_some() {
                violations.push(format!(
                    "clause 3: publish job `{id}`'s download step passes `{input}` -- that lets it \
                     take artifacts from a run other than this one"
                ));
            }
        }
        match with.and_then(|with| with.str_at("path")) {
            Some(path) => download_paths.push(path.trim_end_matches('/').to_string()),
            None => violations.push(format!(
                "clause 3: publish job `{id}`'s download step declares no `path:`, so nothing here \
                 can tell where the published files come from"
            )),
        }
    }

    for run in publish.runs() {
        for command in BUILD_COMMANDS {
            if run.contains(command) {
                violations.push(format!(
                    "clause 3: publish job `{id}` runs `{command}` -- it must publish what the \
                     build jobs produced, not produce anything itself"
                ));
            }
        }
        for path in REPOSITORY_PATHS {
            if run.contains(path) {
                violations.push(format!(
                    "clause 3: publish job `{id}` names the repository path `{path}` -- every \
                     published file must come from the download directory"
                ));
            }
        }
    }

    if let Some(release_step) = publish.index_of_run("gh release create") {
        let run = step_run(&publish.steps()[release_step]).unwrap_or_default();
        if !download_paths
            .iter()
            .any(|path| run.contains(&format!("{path}/")))
        {
            violations.push(format!(
                "clause 3: publish job `{id}`'s release command does not draw its files from the \
                 download directory ({})",
                if download_paths.is_empty() {
                    "there is none".to_string()
                } else {
                    download_paths.join(", ")
                }
            ));
        }
    }

    violations
}

fn check_build_job(job: &Job<'_>) -> Vec<String> {
    let mut violations = Vec::new();
    let id = job.id;

    let Some(runner) = job.runs_on() else {
        violations.push(format!("clause 3: job `{id}` declares no `runs-on:`"));
        return violations;
    };
    let Some((_, packaging)) = PACKAGING_STEP
        .iter()
        .find(|(prefix, _)| runner.starts_with(prefix))
    else {
        violations.push(format!(
            "clause 3: job `{id}` uploads an artifact from runner `{runner}`, for which this check \
             knows no packaging step -- extend PACKAGING_STEP deliberately"
        ));
        return violations;
    };

    // D-18.3's fixed order, by step index rather than by presence.
    let build = job.index_of_run("cargo build --release");
    let bundle = job.index_of_run("xtask -- bundle");
    let package = job.index_of_run(packaging);
    let upload = job
        .steps()
        .iter()
        .position(|step| step_uses(step).is_some_and(|uses| uses.starts_with(UPLOAD_ACTION)));

    match (build, bundle, package, upload) {
        (Some(build), Some(bundle), Some(package), Some(upload)) => {
            if !(build < bundle && bundle < package && package < upload) {
                violations.push(format!(
                    "clause 3: job `{id}` runs build/bundle/package/upload out of order (steps \
                     {build}, {bundle}, {package}, {upload}) -- D-18.3 fixes it as build -> xtask \
                     bundle -> package -> release"
                ));
            }
        }
        (build, bundle, package, upload) => {
            for (what, found) in [
                ("cargo build --release", build.is_some()),
                ("xtask -- bundle", bundle.is_some()),
                (packaging, package.is_some()),
                (UPLOAD_ACTION, upload.is_some()),
            ] {
                if !found {
                    violations.push(format!(
                        "clause 3: job `{id}` uploads a distribution but has no `{what}` step, so \
                         what it uploads was not produced here"
                    ));
                }
            }
        }
    }

    for step in job.steps_using(UPLOAD_ACTION) {
        let Some(with) = step.get("with") else {
            violations.push(format!("clause 3: job `{id}`'s upload step has no `with:`"));
            continue;
        };
        match with.str_at("if-no-files-found") {
            Some("error") => {}
            _ => violations.push(format!(
                "clause 3: job `{id}`'s upload step does not set `if-no-files-found: error` -- an \
                 empty artifact would publish as if it were a distribution"
            )),
        }
        match with.str_at("path") {
            None => violations.push(format!(
                "clause 3: job `{id}`'s upload step declares no `path:`"
            )),
            Some(paths) => {
                for path in paths.lines().map(str::trim).filter(|p| !p.is_empty()) {
                    if !path.starts_with("target/") {
                        violations.push(format!(
                            "clause 3: job `{id}` uploads `{path}`, which is not under `target/` \
                             -- a published distribution must be something this run built, not a \
                             file that was already in the tree"
                        ));
                    }
                }
            }
        }
    }

    violations
}

// ---------------------------------------------------------------------------------------------
// Reading the two files
// ---------------------------------------------------------------------------------------------

/// Reads and parses `release.yml` from `root`.
///
/// # Errors
///
/// Returns a message if the file cannot be read or does not parse as the supported subset.
pub fn load(root: &Path) -> Result<Yaml, String> {
    let path = root.join(WORKFLOW_PATH);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: could not be read ({e})", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask's manifest dir always has a parent")
            .to_path_buf()
    }

    fn workflow() -> Yaml {
        load(&repo_root()).expect("the release workflow must parse")
    }

    fn frs() -> String {
        std::fs::read_to_string(repo_root().join(FRS_PATH)).expect("the FRS must be readable")
    }

    // --- the parser, tested on its own -------------------------------------------------------

    #[test]
    fn nested_mappings_and_sequences_parse_by_indentation() {
        let doc = parse(
            "name: Release\non:\n  push:\n    tags:\n      - \"v*\"\njobs:\n  linux:\n    \
             runs-on: ubuntu-latest\n",
        )
        .unwrap();
        assert_eq!(doc.str_at("name"), Some("Release"));
        let tags = doc.get("on").unwrap().get("push").unwrap().seq_at("tags");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].as_str(), Some("v*"));
        let jobs = jobs(&doc).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].runs_on(), Some("ubuntu-latest"));
    }

    #[test]
    fn a_sequence_item_that_starts_with_a_key_is_a_mapping_with_the_rest_of_its_keys() {
        let doc = parse(
            "steps:\n  - uses: actions/checkout@v4\n  - name: build\n    run: cargo build\n    \
             env:\n      A: b\n",
        )
        .unwrap();
        let steps = doc.get("steps").and_then(Yaml::as_seq).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(step_uses(&steps[0]), Some("actions/checkout@v4"));
        assert_eq!(step_run(&steps[1]), Some("cargo build"));
        assert_eq!(steps[1].get("env").unwrap().str_at("A"), Some("b"));
    }

    #[test]
    fn block_scalars_keep_their_lines_their_hashes_and_their_relative_indentation() {
        let doc = parse(
            "run: |\n  set -eu\n  # a shell comment, not a YAML one\n    indented\nnext: 1\n",
        )
        .unwrap();
        assert_eq!(
            doc.str_at("run"),
            Some("set -eu\n# a shell comment, not a YAML one\n  indented")
        );
        assert_eq!(doc.str_at("next"), Some("1"));
    }

    #[test]
    fn a_folded_block_scalar_joins_its_lines() {
        let doc = parse("run: >\n  cargo run\n  -p xtask\n").unwrap();
        assert_eq!(doc.str_at("run"), Some("cargo run -p xtask"));
    }

    #[test]
    fn flow_sequences_and_comments_and_quotes() {
        let doc = parse(
            "# leading comment\nneeds: [windows, macos, linux]   # trailing comment\ncolour: \
             \"#ff6600\"\nurl: https://example.invalid/x#y\n",
        )
        .unwrap();
        let needs = doc.seq_at("needs");
        assert_eq!(needs.len(), 3);
        assert_eq!(needs[2].as_str(), Some("linux"));
        // A `#` not preceded by a space is part of the value, per YAML's own rule.
        assert_eq!(doc.str_at("colour"), Some("#ff6600"));
        assert_eq!(doc.str_at("url"), Some("https://example.invalid/x#y"));
    }

    #[test]
    fn a_colon_inside_a_value_is_not_a_key_separator() {
        let doc = parse("run: echo a:b\nwith: ${{ github.token }}\n").unwrap();
        assert_eq!(doc.str_at("run"), Some("echo a:b"));
        assert_eq!(doc.str_at("with"), Some("${{ github.token }}"));
    }

    #[test]
    fn the_parser_refuses_what_it_does_not_understand_rather_than_guessing() {
        for (source, expected) in [
            ("a:\n\tb: 1\n", "tab"),
            ("a: 1\na: 2\n", "duplicate key"),
            ("not a mapping at all\n", "neither a mapping entry"),
            ("a: {b: 1}\n", "flow mappings"),
            ("a: [b, [c]]\n", "nested flow collections"),
        ] {
            let error = parse(source).unwrap_err();
            assert!(
                error.contains(expected),
                "parsing {source:?} should have failed with {expected:?}, got {error:?}"
            );
        }
    }

    #[test]
    fn the_real_release_workflow_parses_and_has_the_jobs_it_claims() {
        let doc = workflow();
        let ids: Vec<&str> = jobs(&doc).unwrap().iter().map(|job| job.id).collect();
        assert_eq!(ids, ["windows", "macos", "linux", "publish"], "{ids:?}");
    }

    // --- the tier table, read out of the FRS rather than assumed -----------------------------

    #[test]
    fn the_tier_table_is_read_from_the_frs_and_every_row_maps_to_a_runner() {
        let platforms = tier_1_and_2_platforms(&frs()).unwrap();
        assert_eq!(
            platforms
                .iter()
                .map(|p| (
                    p.tier.as_str(),
                    p.platform.as_str(),
                    p.architecture.as_str()
                ))
                .collect::<Vec<_>>(),
            [
                ("Primary", "Windows 11", "x86-64"),
                ("Secondary", "Linux", "x86-64"),
                ("Secondary", "macOS", "aarch64"),
            ]
        );
        for platform in &platforms {
            assert!(
                RUNNER_FOR_PLATFORM.iter().any(|(name, arch, _)| {
                    platform.platform.starts_with(name) && platform.architecture == *arch
                }),
                "no runner is known for {platform:?}"
            );
        }
    }

    #[test]
    fn a_tier_word_this_check_does_not_know_is_an_error_not_a_skip() {
        let table = "| Tier | Platform | Commitment |\n|---|---|---|\n| Tertiary | BeOS (ppc) | \
                     maybe |\n";
        let error = tier_1_and_2_platforms(table).unwrap_err();
        assert!(error.contains("Tertiary"), "{error}");
    }

    #[test]
    fn a_tier_2_platform_with_no_runner_mapping_is_reported_rather_than_skipped() {
        let table = "| Tier | Platform | Commitment |\n|---|---|---|\n| Secondary | FreeBSD \
                     (x86-64) | supported |\n";
        let violations = clause_2_every_tier_1_and_tier_2_platform(&workflow(), table);
        assert!(
            violations.iter().any(|v| v.contains("FreeBSD")),
            "{violations:#?}"
        );
    }

    // --- each clause has teeth: a workflow that breaks it is reported -------------------------

    /// A minimal but *passing* workflow, so each negative test below can break exactly one thing.
    fn minimal_workflow() -> String {
        let mut text = String::from(
            r#"name: Release
on:
  push:
    tags:
      - "v*"
permissions:
  contents: read
jobs:
"#,
        );
        for (job, runner, packaging) in [
            (
                "windows",
                "windows-latest",
                "iscc packaging/windows/namir.iss",
            ),
            (
                "macos",
                "macos-latest",
                "bash packaging/macos/make_installer.sh",
            ),
            (
                "linux",
                "ubuntu-latest",
                "tar --create --file - x | gzip -9 -n > x.tar.gz",
            ),
        ] {
            text.push_str(&format!(
                r#"  {job}:
    runs-on: {runner}
    steps:
      - uses: actions/checkout@v4
      - name: build
        run: cargo build --release --workspace
      - name: bundle
        run: cargo run -p xtask -- bundle
      - name: package
        run: {packaging}
      - uses: actions/upload-artifact@v4
        with:
          name: namir-{job}
          if-no-files-found: error
          path: |
            target/dist/*
"#
            ));
        }
        text.push_str(
            r#"  publish:
    needs: [windows, macos, linux]
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: dist
      - name: release
        run: gh release create "$GITHUB_REF_NAME" dist/*
"#,
        );
        text
    }

    fn assert_minimal_workflow_passes_every_clause() -> Yaml {
        let doc = parse(&minimal_workflow()).unwrap();
        assert_eq!(clause_1_triggered_by_a_tag(&doc), Vec::<String>::new());
        assert_eq!(
            clause_2_every_tier_1_and_tier_2_platform(&doc, &frs()),
            Vec::<String>::new()
        );
        assert_eq!(
            clause_3_every_distribution_is_this_workflows(&doc),
            Vec::<String>::new()
        );
        doc
    }

    #[test]
    fn the_fixture_this_modules_negative_tests_mutate_passes_every_clause_unmutated() {
        assert_minimal_workflow_passes_every_clause();
    }

    #[test]
    fn clause_1_rejects_a_branch_push_a_manual_dispatch_and_a_pinned_ref() {
        assert_minimal_workflow_passes_every_clause();

        let branch = minimal_workflow().replace(
            "  push:\n    tags:\n      - \"v*\"\n",
            "  push:\n    branches: [trunk]\n",
        );
        let violations = clause_1_triggered_by_a_tag(&parse(&branch).unwrap());
        assert!(
            violations.iter().any(|v| v.contains("no `tags:` filter"))
                && violations.iter().any(|v| v.contains("branches")),
            "{violations:#?}"
        );

        let dispatch =
            minimal_workflow().replace("on:\n  push:\n", "on:\n  workflow_dispatch:\n  push:\n");
        let violations = clause_1_triggered_by_a_tag(&parse(&dispatch).unwrap());
        assert!(
            violations
                .iter()
                .any(|v| v.contains("on.workflow_dispatch")),
            "{violations:#?}"
        );

        let pinned = minimal_workflow().replace(
            "      - uses: actions/checkout@v4\n",
            "      - uses: actions/checkout@v4\n        with:\n          ref: trunk\n",
        );
        let violations = clause_1_triggered_by_a_tag(&parse(&pinned).unwrap());
        assert!(
            violations.iter().any(|v| v.contains("ref: trunk")),
            "{violations:#?}"
        );
    }

    #[test]
    fn clause_2_rejects_a_workflow_that_drops_a_platform_or_stops_producing_for_it() {
        assert_minimal_workflow_passes_every_clause();

        // The macOS job removed entirely: a tier-2 platform with no job at all.
        let text = minimal_workflow();
        let start = text.find("  macos:").unwrap();
        let end = text.find("  linux:").unwrap();
        let mut dropped = text.clone();
        dropped.replace_range(start..end, "");
        let violations =
            clause_2_every_tier_1_and_tier_2_platform(&parse(&dropped).unwrap(), &frs());
        assert!(
            violations
                .iter()
                .any(|v| v.contains("macos-") && v.contains("macOS")),
            "{violations:#?}"
        );

        // The macOS job present, but uploading nothing -- it runs and produces no distribution.
        let silent = text.replace(
            r#"      - uses: actions/upload-artifact@v4
        with:
          name: namir-macos
"#,
            r#"      - name: no upload
        run: echo nothing
        with:
          name: namir-macos
"#,
        );
        let violations =
            clause_2_every_tier_1_and_tier_2_platform(&parse(&silent).unwrap(), &frs());
        assert!(
            violations.iter().any(|v| v.contains("uploads an artifact")),
            "{violations:#?}"
        );
    }

    #[test]
    fn clause_3_rejects_a_publish_job_that_could_publish_something_from_outside_this_run() {
        assert_minimal_workflow_passes_every_clause();

        for (mutation, replacement, expected) in [
            // Reaching into another run's artifacts.
            (
                "        with:\n          path: dist\n",
                "        with:\n          path: dist\n          run-id: 12345\n",
                "run-id",
            ),
            // Publishing a file from a source tree instead of from the download directory.
            (
                "        run: gh release create \"$GITHUB_REF_NAME\" dist/*\n",
                "        run: gh release create \"$GITHUB_REF_NAME\" target/dist/*\n",
                "repository path `target/`",
            ),
            // Building at publish time.
            (
                "      - name: release\n",
                "      - name: build here instead\n        run: cargo build --release\n      - name: release\n",
                "runs `cargo build`",
            ),
            // A source tree under the publish job.
            (
                "    steps:\n      - uses: actions/download-artifact@v4\n",
                "    steps:\n      - uses: actions/checkout@v4\n      - uses: actions/download-artifact@v4\n",
                "checks the repository out",
            ),
            // Publishing without waiting for a platform.
            (
                "    needs: [windows, macos, linux]\n",
                "    needs: [windows, linux]\n",
                "does not `needs: macos`",
            ),
        ] {
            let text = minimal_workflow();
            assert!(text.contains(mutation), "fixture drifted: {mutation:?}");
            let mutated = text.replacen(mutation, replacement, 1);
            let violations =
                clause_3_every_distribution_is_this_workflows(&parse(&mutated).unwrap());
            assert!(
                violations.iter().any(|v| v.contains(expected)),
                "expected a violation naming {expected:?}, got {violations:#?}"
            );
        }
    }

    #[test]
    fn clause_3_rejects_a_build_job_that_uploads_something_it_did_not_produce() {
        assert_minimal_workflow_passes_every_clause();

        for (mutation, replacement, expected) in [
            // Packaging before bundling: D-18.3's order broken, every step still present.
            (
                "      - name: bundle\n        run: cargo run -p xtask -- bundle\n      - name: \
                 package\n        run: iscc packaging/windows/namir.iss\n",
                "      - name: package\n        run: iscc packaging/windows/namir.iss\n      - \
                 name: bundle\n        run: cargo run -p xtask -- bundle\n",
                "out of order",
            ),
            // No packaging step at all: the upload cannot be a distribution this job produced.
            (
                "      - name: package\n        run: iscc packaging/windows/namir.iss\n",
                "",
                "has no `iscc` step",
            ),
            // Uploading a checked-in file rather than a build output.
            (
                "            target/dist/*\n",
                "            README.md\n",
                "uploads `README.md`",
            ),
            // An artifact that may legitimately be empty.
            (
                "          if-no-files-found: error\n",
                "",
                "if-no-files-found: error",
            ),
        ] {
            let text = minimal_workflow();
            assert!(text.contains(mutation), "fixture drifted: {mutation:?}");
            let mutated = text.replacen(mutation, replacement, 1);
            let violations =
                clause_3_every_distribution_is_this_workflows(&parse(&mutated).unwrap());
            assert!(
                violations.iter().any(|v| v.contains(expected)),
                "expected a violation naming {expected:?}, got {violations:#?}"
            );
        }
    }

    // --- the requirement itself ---------------------------------------------------------------

    /// FR-PKG-010, against the real `.github/workflows/release.yml`, one assertion per clause of
    /// the requirement's own `Verify:` method, all three evaluated before anything panics so a
    /// failure names every clause that broke rather than the first.
    ///
    /// **What the tag claims, and what it does not.** The three clause checks span the method as
    /// written — the trigger, the platform set (read out of FRS §1.4 rather than assumed), and the
    /// provenance of every published file. What no static check can reach is the requirement's own
    /// verb: *produce*. This workflow has never run, on any runner; neither has `iscc` against
    /// `namir.iss`, nor `make_installer.sh`, nor the Linux `tar` block, as each packaging README
    /// says of itself. So the tag is `trace-partial:`, and the `uncovered:` field says exactly
    /// that. Promoting it needs a tagged run that produced the artifacts, not a change here.
    // trace-partial: FR-PKG-010
    // uncovered: FR-PKG-010 — the three clauses of the requirement's own `Verify: S` method are
    // uncovered: asserted against release.yml's parsed structure, but "shall **produce** an
    // uncovered: installable distribution" is unspanned: no tagged run has ever executed this
    // uncovered: workflow, and the packaging entry points it calls (rcedit + iscc, macOS
    // uncovered: make_installer.sh, the Linux tar block) have never run anywhere either, so a
    // uncovered: structurally correct workflow is all that is evidenced; closes M13
    #[test]
    fn the_release_workflow_meets_every_clause_of_fr_pkg_010s_verify_method() {
        let doc = workflow();
        let frs = frs();

        let trigger = clause_1_triggered_by_a_tag(&doc);
        let platforms = clause_2_every_tier_1_and_tier_2_platform(&doc, &frs);
        let provenance = clause_3_every_distribution_is_this_workflows(&doc);

        assert!(
            trigger.is_empty() && platforms.is_empty() && provenance.is_empty(),
            "{WORKFLOW_PATH} does not satisfy FR-PKG-010:\n  clause 1 (tag-triggered, tagged \
             source tree): {trigger:#?}\n  clause 2 (every tier-1 and tier-2 platform): \
             {platforms:#?}\n  clause 3 (every published distribution is this workflow's): \
             {provenance:#?}"
        );
    }
}
