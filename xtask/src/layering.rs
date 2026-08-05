//! D-5.2: "CI runs (a) a dependency-graph check rejecting any edge not in the table above, (b) a
//! lint rejecting `#[cfg(target_os` / `#[cfg(windows` / `#[cfg(unix` outside `namir-platform`...".
//! This module implements both halves of that decision as pure, independently-testable checking
//! logic (`allowed_dependents`/`check_edges` and `scan_platform_cfg`); [`crate::main`] wires them
//! to the real repository (`cargo metadata` and a filesystem walk respectively).
//!
//! [`LAYERING_TABLE`] is a manually-maintained mirror of `docs/02-architecture.md` §5's D-5.1
//! table and must be kept in sync by hand whenever that table changes. Quoting its rows as they
//! stand today, so there is no ambiguity about what this file is supposed to match:
//!
//! | Crate | May depend on |
//! |---|---|
//! | `namir-core` | — |
//! | `namir-params` | core |
//! | `namir-dsp` | core |
//! | `namir-nam` | core, dsp |
//! | `namir-ir` | core, dsp |
//! | `namir-engine` | core, params, dsp, nam, ir |
//! | `namir-state` | core, params |
//! | `namir-library` | core, nam, ir, state |
//! | `namir-platform` | core |
//! | `namir-worker` | everything above (core, params, dsp, nam, ir, engine, state, library, platform) |
//! | `namir-ui` | core, params, library, state |
//! | `namir-app` | everything |
//! | `namir-clap` | everything except app |
//!
//! `namir-fixtures` is dev/test tooling (D-19.1), like `xtask` itself — it is not a row in
//! D-5.1's table and is exempted entirely from edge-checking below, rather than forced into a
//! table that doesn't describe it. Only normal (non-dev, non-build) dependency edges are
//! checked: `namir-nam`'s dev-dependency on `namir-fixtures` for its parity tests is legitimate
//! and must never be flagged, which is why [`crate::cargo_meta::normal_namir_edges`] filters by
//! dependency kind before any edge reaches [`check_edges`].

const FIXTURES: &str = "namir-fixtures";

/// D-5.1's table, `(crate, &[crates it may depend on])`. See the module doc comment for the
/// prose version this must stay in sync with by hand.
pub const LAYERING_TABLE: &[(&str, &[&str])] = &[
    ("namir-core", &[]),
    ("namir-params", &["namir-core"]),
    ("namir-dsp", &["namir-core"]),
    ("namir-nam", &["namir-core", "namir-dsp"]),
    ("namir-ir", &["namir-core", "namir-dsp"]),
    (
        "namir-engine",
        &[
            "namir-core",
            "namir-params",
            "namir-dsp",
            "namir-nam",
            "namir-ir",
        ],
    ),
    ("namir-state", &["namir-core", "namir-params"]),
    (
        "namir-library",
        &["namir-core", "namir-nam", "namir-ir", "namir-state"],
    ),
    ("namir-platform", &["namir-core"]),
    (
        "namir-worker",
        &[
            "namir-core",
            "namir-params",
            "namir-dsp",
            "namir-nam",
            "namir-ir",
            "namir-engine",
            "namir-state",
            "namir-library",
            "namir-platform",
        ],
    ),
    (
        "namir-ui",
        &["namir-core", "namir-params", "namir-library", "namir-state"],
    ),
    (
        "namir-app",
        &[
            "namir-core",
            "namir-params",
            "namir-dsp",
            "namir-nam",
            "namir-ir",
            "namir-engine",
            "namir-state",
            "namir-library",
            "namir-platform",
            "namir-worker",
            "namir-ui",
        ],
    ),
    (
        "namir-clap",
        &[
            "namir-core",
            "namir-params",
            "namir-dsp",
            "namir-nam",
            "namir-ir",
            "namir-engine",
            "namir-state",
            "namir-library",
            "namir-platform",
            "namir-worker",
            "namir-ui",
        ],
    ),
];

fn allowed_dependents(krate: &str) -> Option<&'static [&'static str]> {
    LAYERING_TABLE
        .iter()
        .find(|(name, _)| *name == krate)
        .map(|(_, allowed)| *allowed)
}

/// Checks every `(from, to)` normal-dependency edge between two `namir-*` crates against
/// [`LAYERING_TABLE`]. `namir-fixtures` is exempt on either side of an edge (see module doc).
/// Returns one human-readable violation string per problem found — empty means clean. A crate
/// that isn't in the table at all (D-5.1 names more crates than exist yet, so this is expected
/// for those; it is a real problem only once such a crate's package actually appears in
/// `cargo metadata`'s output) is itself reported, since a silently-unchecked crate would defeat
/// the point of the gate.
pub fn check_edges(edges: &[(String, String)]) -> Vec<String> {
    let mut violations = Vec::new();
    for (from, to) in edges {
        if from == FIXTURES || to == FIXTURES {
            continue;
        }
        match allowed_dependents(from) {
            None => violations.push(format!(
                "'{from}' is not present in xtask's LAYERING_TABLE (docs/02-architecture.md §5 \
                 D-5.1 must be consulted and this table updated by hand before this edge can be \
                 evaluated)"
            )),
            Some(allowed) => {
                if !allowed.contains(&to.as_str()) {
                    violations.push(format!(
                        "disallowed edge: '{from}' -> '{to}' is not permitted by D-5.1's \
                         layering table"
                    ));
                }
            }
        }
    }
    violations
}

const PLATFORM_CFG_PATTERNS: &[&str] = &["#[cfg(target_os", "#[cfg(windows", "#[cfg(unix"];
/// The one crate D-5.1 permits to carry the patterns [`PLATFORM_CFG_PATTERNS`] flags.
pub const PLATFORM_CFG_EXEMPT_CRATE: &str = "namir-platform";

/// Scans `source`'s text for D-5.2(b)'s three platform-conditional substrings, line by line.
/// Returns `(1-indexed line number, matched pattern)` for every occurrence — a line containing
/// more than one pattern yields more than one entry. Pure string logic so it can be unit-tested
/// without touching the filesystem; [`crate::main`] is what applies this to real files, skipping
/// [`PLATFORM_CFG_EXEMPT_CRATE`].
pub fn scan_platform_cfg(source: &str) -> Vec<(usize, &'static str)> {
    let mut hits = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        for pattern in PLATFORM_CFG_PATTERNS {
            if line.contains(pattern) {
                hits.push((idx + 1, *pattern));
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legitimate_edge_is_not_flagged() {
        let edges = vec![("namir-dsp".to_string(), "namir-core".to_string())];
        assert!(check_edges(&edges).is_empty());
    }

    #[test]
    fn backwards_edge_is_flagged() {
        // namir-core may depend on nothing -- core -> dsp is backwards from what D-5.1 allows.
        let edges = vec![("namir-core".to_string(), "namir-dsp".to_string())];
        let violations = check_edges(&edges);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("namir-core"));
        assert!(violations[0].contains("namir-dsp"));
    }

    #[test]
    fn edge_into_a_crate_not_in_the_table_is_flagged() {
        let edges = vec![("namir-core".to_string(), "namir-does-not-exist".to_string())];
        let violations = check_edges(&edges);
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn edge_from_a_crate_not_in_the_table_is_flagged() {
        let edges = vec![("namir-does-not-exist".to_string(), "namir-core".to_string())];
        let violations = check_edges(&edges);
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn fixtures_is_exempt_as_either_endpoint() {
        let edges = vec![
            ("namir-nam".to_string(), "namir-fixtures".to_string()),
            ("namir-fixtures".to_string(), "namir-core".to_string()),
        ];
        assert!(check_edges(&edges).is_empty());
    }

    #[test]
    fn engine_may_depend_on_everything_its_row_lists() {
        let edges = vec![
            ("namir-engine".to_string(), "namir-core".to_string()),
            ("namir-engine".to_string(), "namir-params".to_string()),
            ("namir-engine".to_string(), "namir-dsp".to_string()),
            ("namir-engine".to_string(), "namir-nam".to_string()),
            ("namir-engine".to_string(), "namir-ir".to_string()),
        ];
        assert!(check_edges(&edges).is_empty());
    }

    #[test]
    fn engine_may_not_depend_on_state() {
        let edges = vec![("namir-engine".to_string(), "namir-state".to_string())];
        assert_eq!(check_edges(&edges).len(), 1);
    }

    #[test]
    fn empty_edge_list_is_clean() {
        assert!(check_edges(&[]).is_empty());
    }

    #[test]
    fn scan_finds_target_os_cfg_with_correct_line_number() {
        let source = "fn a() {}\n#[cfg(target_os = \"windows\")]\nfn b() {}\n";
        let hits = scan_platform_cfg(source);
        assert_eq!(hits, vec![(2, "#[cfg(target_os")]);
    }

    #[test]
    fn scan_finds_windows_and_unix_cfg() {
        let source = "#[cfg(windows)]\nfn a() {}\n#[cfg(unix)]\nfn b() {}\n";
        let hits = scan_platform_cfg(source);
        assert_eq!(hits, vec![(1, "#[cfg(windows"), (3, "#[cfg(unix")]);
    }

    #[test]
    fn scan_of_clean_source_is_empty() {
        let source = "fn a() {}\n#[cfg(test)]\nmod tests {}\n";
        assert!(scan_platform_cfg(source).is_empty());
    }

    #[test]
    fn scan_reports_every_pattern_on_a_shared_line() {
        // Contrived, but exercises the per-pattern loop rather than assuming at most one match
        // per line.
        let source = "#[cfg(windows)] #[cfg(unix)]\n";
        let hits = scan_platform_cfg(source);
        assert_eq!(hits, vec![(1, "#[cfg(windows"), (1, "#[cfg(unix")]);
    }
}
