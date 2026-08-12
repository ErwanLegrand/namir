//! D-18.5's owning-milestone attribution: which milestone the roadmap makes responsible for each
//! uncovered Must requirement, **derived from `docs/03-implementation-roadmap.md`'s own section
//! structure** rather than tabulated here.
//!
//! That constraint is the decision's, not a preference: "it must not be a hard-coded table of ids
//! inside `xtask`, which would be the allowlist rejected below wearing a different name"
//! (`docs/02-architecture.md:2030-2032`). Nothing about any requirement id is written into this
//! module. Deleting a milestone's roadmap text changes the label it derives; adding a milestone
//! section needs no code change at all.
//!
//! **The rule:** an id is attributed to the **last** `## <n>. M<k>` section of the roadmap that
//! *names* it. One pass, later section overwrites earlier, so the result is deterministic. A
//! milestone section runs from its `##` heading to the next `##` heading of any kind; `###`
//! subheadings belong to the enclosing section. That last part is load-bearing rather than
//! incidental -- `### M13 scope note` is the *only* place NFR-PERF-030's move to M13 is recorded,
//! and `### M9a status` subsections sit inside §16. A parser that reset attribution at `###` would
//! get NFR-PERF-030 wrong.
//!
//! **The honest limit, stated rather than glossed:** the predicate this implements is "the last
//! milestone section that *names* the id", which is not the same predicate as "owns it". It tracks
//! ownership on this document because the document is written in milestone order and its convention
//! is to append rather than rewrite, so a requirement that moves is named again later --
//! NFR-PERF-030 is exactly that case, named by M5, M7, M9 and finally M13. A future milestone that
//! names an id only in passing would take the label. That is tolerable *precisely because* the label
//! is printed text with no code path reading it (D-18.5's Consequence,
//! `docs/02-architecture.md:2047-2051`); it would not be tolerable for anything that gates, and
//! this derivation must not later be reused for something that does.
//!
//! Granularity is the roadmap's section number and nothing finer. The tool can derive `M9`, never
//! `M9a`/`M9b`, and never `M10 Phase 0` -- phases are prose inside a section, not sections. That is
//! the promised granularity, said so in the legend `main.rs` prints, rather than guessed at by a
//! heuristic.

use std::collections::HashMap;

use crate::traceability::scan_requirement_ids;

/// What an uncovered id renders as when no milestone section names it.
///
/// A first-class, meaningful output rather than a degraded one: D-18.5 says in as many words that
/// "an uncovered id with no owner named beside it is a gap nobody has claimed, which is the state
/// this pass found §14 in and the state the printed line makes visible on every run"
/// (`docs/02-architecture.md:2049-2051`). It must never be replaced by a guess or a nearest match.
pub const UNATTRIBUTED: &str = "unattributed";

/// `id -> milestone label`, for every requirement id named anywhere inside a milestone section.
pub fn attribute(roadmap: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut current: Option<&str> = None;

    for line in roadmap.lines() {
        // Only `## ` opens or closes a section. `### `/`#### ` fall through to the id scan below,
        // which is what puts a `###` note's ids in the enclosing milestone.
        if let Some(rest) = line.strip_prefix("## ") {
            current = milestone_label(rest);
            continue;
        }
        if let Some(milestone) = current {
            for id in scan_requirement_ids(line) {
                map.insert(id, milestone.to_string());
            }
        }
    }

    map
}

/// `"16. M9 — Verification truth-up"` -> `Some("M9")`. `None` for every `##` heading that is not a
/// milestone section.
///
/// Deliberately matches on the `M<digits>` token alone and never on the em dash (U+2014) the
/// headings use. Verified against the real document: this accepts exactly the thirteen milestone
/// headings and rejects every other `##` heading, including the three near-misses --
/// `## 2. Current state (M0) — …`, `## 14. Appendix: Must-requirement status snapshot (M0)` (both
/// name a milestone, neither is one) and `## Milestones added 2026-08-08 — M9 through M13`, which
/// has no `". "` at all.
pub fn milestone_label(heading_rest: &str) -> Option<&str> {
    let (number, rest) = heading_rest.split_once(". ")?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let token = rest.split_whitespace().next()?;
    let digits = token.strip_prefix('M')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn milestone_label_accepts_every_real_milestone_heading_form() {
        for (heading, want) in [
            ("5. M1 — Foundations for real stages", "M1"),
            ("16. M9 — Verification truth-up", "M9"),
            ("20. M13 — Distribution and packaging", "M13"),
        ] {
            assert_eq!(milestone_label(heading), Some(want), "heading: {heading}");
        }
    }

    #[test]
    fn milestone_label_rejects_the_near_misses() {
        // The first two name a milestone in parentheses without being one; the third is the
        // 2026-08-08 divider heading, which carries no section number at all.
        for heading in [
            "2. Current state (M0) — as of this document's date",
            "14. Appendix: Must-requirement status snapshot (M0)",
            "13. Explicit non-goals for this roadmap (restated, not re-decided)",
            "Milestones added 2026-08-08 — M9 through M13",
            "M9 — no section number",
            "5. Mx — not a number",
        ] {
            assert_eq!(milestone_label(heading), None, "heading: {heading}");
        }
    }

    #[test]
    fn attribute_gives_an_id_to_the_last_milestone_section_that_names_it() {
        // NFR-PERF-030's real shape: claimed by an earlier milestone, moved by a later one.
        let roadmap = "## 9. M5 — State\n\
                       NFR-PERF-030 is measured here.\n\
                       ## 20. M13 — Distribution\n\
                       NFR-PERF-030 moves into this milestone.\n";
        let map = attribute(roadmap);
        assert_eq!(map.get("NFR-PERF-030").map(String::as_str), Some("M13"));
    }

    #[test]
    fn an_id_under_a_sub_heading_belongs_to_the_enclosing_milestone_section() {
        // `### M13 scope note` is NFR-PERF-030's only M13 claim in the real document. A parser that
        // reset attribution at `###` would lose it.
        let roadmap = "## 20. M13 — Distribution\n\
                       ### M13 scope note (added 2026-08-08)\n\
                       NFR-PERF-030 moves from M9 into this milestone.\n";
        assert_eq!(
            attribute(roadmap).get("NFR-PERF-030").map(String::as_str),
            Some("M13")
        );
    }

    #[test]
    fn an_id_outside_every_milestone_section_is_absent_from_the_map() {
        let roadmap = "FR-CFG-020 before any heading at all.\n\
                       ## 14. Appendix: Must-requirement status snapshot (M0)\n\
                       FR-NAM-090 under a non-milestone section.\n\
                       ## Milestones added 2026-08-08 — M9 through M13\n\
                       FR-PKG-010 under the divider heading.\n";
        let map = attribute(roadmap);
        assert!(map.is_empty(), "{map:?}");
    }

    #[test]
    fn a_non_milestone_heading_closes_the_section_before_it() {
        let roadmap = "## 16. M9 — Verification truth-up\n\
                       FR-CFG-020 is M9's.\n\
                       ## 17. Appendix\n\
                       FR-NAM-090 is nobody's.\n";
        let map = attribute(roadmap);
        assert_eq!(map.get("FR-CFG-020").map(String::as_str), Some("M9"));
        assert_eq!(map.get("FR-NAM-090"), None);
    }

    #[test]
    fn an_empty_document_yields_an_empty_map() {
        assert!(attribute("").is_empty());
    }

    #[test]
    fn the_real_roadmap_still_has_exactly_the_fourteen_milestone_sections() {
        // The test that catches a heading-form change silently breaking the whole derivation.
        // Deliberately asserts on the *section structure* and on no id -> milestone pair, so
        // ordinary roadmap prose edits never churn it. `include_str!` rather than a filesystem
        // read, so this module stays free of I/O at runtime.
        //
        // Updated at M14 (2026-08-12), from thirteen sections to fourteen, and the update is the
        // guard working rather than the guard being in the way: §21 adds a genuinely new milestone
        // section, which is precisely the event this assertion exists to make somebody look at.
        // Only a real new `## <n>. M<k>` heading may edit this list -- a prose change that alters
        // it means the heading form drifted, which is the failure the comment above describes.
        // Note the list is in *section* order, which is not execution order: M8 sits between M7
        // and M9 here and runs last of all (§12's execution-order note, and its M14 consequence).
        let roadmap = include_str!("../../docs/03-implementation-roadmap.md");
        let sections: Vec<&str> = roadmap
            .lines()
            .filter_map(|line| line.strip_prefix("## "))
            .filter_map(milestone_label)
            .collect();
        assert_eq!(
            sections,
            [
                "M1", "M2", "M3", "M4", "M5", "M6", "M7", "M8", "M9", "M10", "M11", "M12", "M13",
                "M14"
            ]
        );
    }
}
