//! The 100-column convention, applied to comment prose (issue #176). `rustfmt`'s `max_width`
//! covers code only; the options that would wrap comments (`wrap_comments`,
//! `format_code_in_doc_comments`) are nightly-only, so with no `rustfmt.toml` in the tree
//! `cargo fmt --all -- --check` has always passed over over-width prose. AGENTS.md states the
//! convention in as many words ("so the field can wrap inside 100 columns"); this is the check
//! that enforces it.
//!
//! # The escape hatches, and why none of them is an annotation
//!
//! A long URL, a pinned SHA or a deep path cannot be wrapped, and a check that has no answer for
//! those gets fought and then disabled. All three hatches here are structural, so nothing has to
//! be annotated and none of them can be claimed for a long sentence of prose:
//!
//! 1. **An unbreakable token.** The token straddling column 100 is itself too long to fit on a
//!    comment line of its own, so moving it down would not help. Ordinary prose never qualifies,
//!    since prose at column 100 has spaces in it — but the rule looks only at that one token, so
//!    a line that puts a long URL *early* and then runs wrappable prose past column 100 is
//!    exempt too. Nothing in the tree does that; it is a hole, not a licence.
//! 2. **A Markdown table row** (the comment body both starts and ends with `|`). Wrapping one
//!    breaks the table, and the cells are content, not prose to reflow. The closing `|` is what
//!    keeps the rule off a sentence that merely begins with a pipe.
//! 3. **Inside a fenced block** (between two comment lines whose body starts with ```` ``` ````).
//!    Those lines are sample output or code, where a line break changes what is shown — and in a
//!    doc comment, what the doc test runs. Scoped to one run of comment lines: an unmatched
//!    fence closes at the end of its comment block, as rustdoc's own does.
//!
//! As of the sweep that introduced this check, no line relies on (1); the lines relying on (2)
//! and (3) are the tables in `denormal_guard.rs`, `library_scan.rs`, `paths.rs` and friends, and
//! `startup_probe.rs`'s two sample log lines.
//!
//! Line-based, like `layering`'s and `rt_logging`'s scanners: only lines whose trimmed form
//! begins `//` are considered, so a long string literal or a `/* */` block is invisible to it.

/// The convention, in characters. Matches `rustfmt`'s default `max_width` for code.
pub const MAX_WIDTH: usize = 100;

/// Whether the overflow of `line` is a single unbreakable token — see the module doc. `line` is
/// known to be wider than [`MAX_WIDTH`]. The token straddling the limit is measured against the
/// width a fresh continuation line would leave it: this line's indentation, its comment marker
/// and one space.
fn overflow_is_unbreakable(chars: &[char]) -> bool {
    let mut start = MAX_WIDTH;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let mut end = MAX_WIDTH;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    let indent = chars.iter().take_while(|c| c.is_whitespace()).count();
    let marker = chars[indent..]
        .iter()
        .take_while(|&&c| c == '/' || c == '!')
        .count();
    indent + marker + 1 + (end - start) > MAX_WIDTH
}

/// The comment body of `line`: what follows `//`, `///` or `//!` and the space after it.
/// Returns `None` for a line that is not a comment.
fn comment_body(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("//")?;
    Some(rest.trim_start_matches(['/', '!']).trim_start())
}

/// Scans `source` for comment lines wider than [`MAX_WIDTH`] characters, skipping the three
/// exempt shapes the module doc describes. Returns `(1-indexed line number, width in
/// characters)` per violation. Pure string logic so it is unit-testable without a filesystem;
/// [`crate::main`] applies it to the real files.
///
/// The fence state is scoped to one run of comment lines: rustdoc closes an unmatched fence at
/// the end of the doc comment, so an unmatched marker is not an error anywhere else, and letting
/// it run to the end of the file would silently exempt everything after it.
pub fn scan_over_width(source: &str) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    let mut in_fence = false;
    for (i, line) in source.lines().enumerate() {
        let Some(body) = comment_body(line) else {
            in_fence = false;
            continue;
        };
        if body.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || (body.starts_with('|') && body.ends_with('|')) {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        if chars.len() > MAX_WIDTH && !overflow_is_unbreakable(&chars) {
            hits.push((i + 1, chars.len()));
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One case per rule: over-width prose is reported with its real width, and each of the
    /// three exempt shapes is not. The fenced text is the same string as the unfenced one, so
    /// the fence is doing the work rather than the text — and the unmatched fence in the second
    /// block must not carry into the third, which is the failure mode that would exempt a whole
    /// file.
    #[test]
    fn over_width_prose_is_reported_and_the_three_exempt_shapes_are_not() {
        let prose = format!("/// {}", "word ".repeat(25));
        let url = format!("/// see https://example.invalid/{}", "x".repeat(100));
        let row = format!("/// | cell | {} |", "x y ".repeat(30));
        let ragged = format!("/// | smuggled prose {}", "word ".repeat(20));
        let fenced = format!("/// {}", "out put ".repeat(20));
        let source = format!(
            "{prose}\n{url}\n{row}\n{ragged}\n/// ```text\n{fenced}\n/// ```\n\
             {fenced}\n\nfn a() {{}}\n\n/// ```text\n{fenced}\n\nfn b() {{}}\n\n{fenced}\n"
        );

        assert!(url.chars().count() > MAX_WIDTH);
        assert!(row.chars().count() > MAX_WIDTH);
        let w = fenced.chars().count();
        assert_eq!(
            scan_over_width(&source),
            vec![
                (1, prose.chars().count()),
                (4, ragged.chars().count()),
                (8, w),
                (17, w)
            ]
        );
    }
}
