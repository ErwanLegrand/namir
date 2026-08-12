//! D-5.2: "CI runs (a) a dependency-graph check rejecting any edge not in the table above, (b) a
//! lint rejecting `#[cfg(target_os` / `#[cfg(windows` / `#[cfg(unix` outside `namir-platform`...".
//! This module implements both halves of that decision as pure, independently-testable checking
//! logic (`allowed_dependents`/`check_edges` and `scan_platform_cfg`); [`crate::main`] wires them
//! to the real repository (`cargo metadata` and a filesystem walk respectively).
//!
//! NFR-PORT-020's "no platform-conditional code at all" in the engine/params/state/library is
//! exactly [`scan_platform_cfg`]'s check. FR-CFG-030 ("the standalone app shall not require the
//! CLAP plugin to be installed") follows structurally from [`LAYERING_TABLE`]'s own asymmetry:
//! `namir-app`'s row never names `namir-clap`, so nothing in `namir-app` can reach it at compile
//! time, let alone at runtime.
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

// NFR-PORT-020's own partial sits beside `scan_platform_cfg`'s pattern list below, which is the
// artifact its gap is about; D-23.1's adjacency rule takes one anchor per tag, so the two ids this
// site carried until M9a are now annotated separately rather than stacked here.
// trace-partial: FR-CFG-030
// uncovered: FR-CFG-030 — the Verify: I method's "each is installed alone into a clean
// uncovered: environment and exercised" is executed by nothing: the artifact is xtask layering's
// uncovered: compile-time dependency-edge lint over LAYERING_TABLE, which argues compile-time
// uncovered: reachability and neither installs nor exercises either product; closes M8

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

/// The `cfg` forms a platform conditional can be written in. All three are checked because
/// D-5.2(b)'s literal three-substring list saw only the first, and the other two compile to the
/// same thing: `cfg!(windows)` is a run-time-shaped expression the compiler folds to a constant,
/// and `#[cfg_attr(windows, ...)]` applies an attribute per platform.
// The lint this list opens now spans every spelling of a platform conditional rather than three
// literal attribute prefixes, and `crate::main` applies it to every `.rs` file under `crates/` plus
// every crate manifest, rather than to `crates/*/src` alone. See `scan_platform_cfg`'s doc comment
// for what it still cannot see and why each residual is the honest direction of error.
// trace: NFR-PORT-020
const CFG_OPENERS: &[&str] = &["cfg(", "cfg!(", "cfg_attr("];

/// Predicate keys that name a *platform*, flagged wherever they are named as a whole identifier on
/// a non-comment line. Every one of these is a `cfg` predicate key and nothing else in Rust — none
/// has an ordinary meaning as an identifier — so they need no neighbouring `cfg` opener to be
/// recognised, which is what lets a wrapped multi-line `#[cfg(any(\n target_os = "linux",\n ...))]`
/// be caught on its continuation lines.
///
/// **`target_has_atomic` is deliberately absent, and it is the one live `cfg!` in the tree**
/// (`crates/namir-engine/src/telemetry_ring.rs:40`). It names a target *capability the code
/// requires everywhere*, and the site asserts it — `const _: () = assert!(cfg!(target_has_atomic =
/// "64"), ...)` — rather than branching on it, so no second code path exists on any target. That is
/// the opposite of the thing NFR-PORT-020 forbids ("the engine ... shall contain no
/// platform-conditional code at all"): a build that fails loudly off-platform, not one that behaves
/// differently. Adding a `target_has_atomic` **branch** anywhere would be a real violation and this
/// list would not catch it; that residual is stated here rather than glossed.
const PLATFORM_CFG_KEYS: &[&str] = &[
    "target_os",
    "target_arch",
    "target_family",
    "target_env",
    "target_vendor",
    "target_abi",
    "target_endian",
    "target_pointer_width",
];

/// Predicate keys that are also ordinary words. `#[cfg(windows)]` is a platform conditional;
/// `let windows = data.windows(2)`, a path containing "unix" and the string value in
/// `target_os = "windows"` are not, and a whole-identifier match alone cannot tell them apart.
/// These are therefore flagged only when one of [`CFG_OPENERS`] appears on the same line **and**
/// the word sits in predicate position ([`names_bare_predicate`]) — which is where they occur in
/// practice, `not(...)`, `any(...)` and `all(...)` wrappers included.
const PLATFORM_CFG_BARE_KEYS: &[&str] = &["windows", "unix", "wasm"];

/// The one crate D-5.1 permits to carry the conditionals [`scan_platform_cfg`] flags.
pub const PLATFORM_CFG_EXEMPT_CRATE: &str = "namir-platform";

/// Whether `line` names `ident` as a whole identifier — neither neighbouring byte being part of an
/// identifier. Same rule, and the same reason, as `rt_logging`'s: `target_osx_hack` must not match
/// `target_os`, while `target_os = "windows"` must.
fn names_identifier(line: &str, ident: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(ident).any(|(start, _)| {
        let end = start + ident.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
        before_ok && after_ok
    })
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Whether `line` names `ident` in **predicate position** — bracketed by `(`/`,` on the left and
/// `)`/`,` on the right, ignoring spaces. This is what separates `#[cfg(any(unix, windows))]` from
/// `target_os = "windows"` (the word is a string *value* there, and the line is already flagged for
/// `target_os`) and from `data.windows(2)` (an ordinary method call).
fn names_bare_predicate(line: &str, ident: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(ident).any(|(start, _)| {
        let end = start + ident.len();
        if (start > 0 && is_ident_byte(bytes[start - 1]))
            || (end < bytes.len() && is_ident_byte(bytes[end]))
        {
            return false;
        }
        let before = bytes[..start]
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map(|i| bytes[i]);
        let after = bytes[end..]
            .iter()
            .find(|b| !b.is_ascii_whitespace())
            .copied();
        matches!(before, Some(b'(') | Some(b',')) && matches!(after, Some(b')') | Some(b',') | None)
    })
}

/// Scans `source` for D-5.2(b)'s platform conditionals, line by line. Returns
/// `(1-indexed line number, the key matched)` per occurrence, keys in
/// [`PLATFORM_CFG_KEYS`]-then-[`PLATFORM_CFG_BARE_KEYS`] order, at most one entry per key per line.
/// Pure string logic so it can be unit-tested without touching the filesystem; [`crate::main`]
/// applies it to real files, skipping [`PLATFORM_CFG_EXEMPT_CRATE`].
///
/// # What M14 widened, and what it still cannot see
///
/// Until M14 this matched three literal substrings — `#[cfg(target_os`, `#[cfg(windows`,
/// `#[cfg(unix` — so `#[cfg(not(windows))]`, `#[cfg(any(unix, windows))]`, `#[cfg_attr(windows,
/// ...)]`, `cfg!(...)` in any form, and every `target_arch`/`target_family`/`target_env` predicate
/// passed unseen. A lint that a reformatting misses is not a lint; NFR-PORT-020's own method is "a
/// lint over the source tree rejects platform conditionals outside designated modules", and three
/// prefixes are not that.
///
/// Residuals, stated rather than pretended closed, each an **over**-approximation or a documented
/// blind spot in the direction that cannot let a real conditional through silently:
///
/// 1. **Line-based**, like `layering`'s edge check and `rt_logging`'s scanner. Trimmed lines
///    beginning `//` are skipped — added at M14 so prose about a conditional does not trip it,
///    which the previous literal match made impossible (see `crates/namir-worker/src/lib.rs`'s own
///    comment, which describes the attribute rather than spelling it for exactly that reason). A
///    key inside a `/* */` block or a string literal is a false positive; nothing in this tree has
///    one.
/// 2. **`target_has_atomic` is not on the list** — see [`PLATFORM_CFG_KEYS`] for the argument.
/// 3. **A platform read at run time is invisible.** `std::env::consts::OS`, a `cfg`-free
///    `if cfg.platform == ...` on a value threaded in from elsewhere: neither is a *conditional
///    compilation* and neither is what D-5.2(b) is about, but neither is caught here either.
pub fn scan_platform_cfg(source: &str) -> Vec<(usize, &'static str)> {
    let mut hits = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for key in PLATFORM_CFG_KEYS {
            if names_identifier(line, key) {
                hits.push((idx + 1, *key));
            }
        }
        if CFG_OPENERS.iter().any(|opener| line.contains(opener)) {
            for key in PLATFORM_CFG_BARE_KEYS {
                if names_bare_predicate(line, key) {
                    hits.push((idx + 1, *key));
                }
            }
        }
    }
    hits
}

/// The manifest half of the same lint: a `[target.'cfg(...)'.dependencies]` table declares a
/// dependency for one platform only, which is a platform conditional expressed in TOML rather than
/// in Rust. `namir-platform`'s own manifest carries the tree's one legitimate instance
/// (`alsa`/`coreaudio` — see its line 42); any other crate growing one would be taking a platform
/// dependency the source-level lint cannot see at all, because the conditional is not in the source.
///
/// Returns `(1-indexed line number, the table header as written)`.
pub fn scan_cargo_target_tables(source: &str) -> Vec<(usize, String)> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("[target."))
        .map(|(idx, line)| (idx + 1, line.trim().to_string()))
        .collect()
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
        assert_eq!(hits, vec![(2, "target_os")]);
    }

    #[test]
    fn scan_finds_windows_and_unix_cfg() {
        let source = "#[cfg(windows)]\nfn a() {}\n#[cfg(unix)]\nfn b() {}\n";
        let hits = scan_platform_cfg(source);
        assert_eq!(hits, vec![(1, "windows"), (3, "unix")]);
    }

    #[test]
    fn scan_of_clean_source_is_empty() {
        let source = "fn a() {}\n#[cfg(test)]\nmod tests {}\n#[cfg(feature = \"host-ext-tests\")]\nfn c() {}\n#[cfg(debug_assertions)]\nfn d() {}\n";
        assert!(scan_platform_cfg(source).is_empty());
    }

    #[test]
    fn scan_reports_every_pattern_on_a_shared_line() {
        // Contrived, but exercises the per-key loop rather than assuming at most one match
        // per line.
        let source = "#[cfg(windows)] #[cfg(unix)]\n";
        let hits = scan_platform_cfg(source);
        assert_eq!(hits, vec![(1, "windows"), (1, "unix")]);
    }

    // --- M14: the four shapes the three-substring form let through (NFR-PORT-020) --------------

    #[test]
    fn scan_finds_a_negated_or_combined_predicate() {
        // `#[cfg(not(windows))]` and `#[cfg(any(unix, windows))]` are as platform-conditional as
        // `#[cfg(windows)]`; neither begins with any of the three prefixes the old form matched.
        assert_eq!(
            scan_platform_cfg("#[cfg(not(windows))]\nfn a() {}\n"),
            vec![(1, "windows")]
        );
        assert_eq!(
            scan_platform_cfg("#[cfg(any(unix, windows))]\nfn a() {}\n"),
            vec![(1, "windows"), (1, "unix")]
        );
        assert_eq!(
            scan_platform_cfg("#[cfg(all(unix, not(target_os = \"macos\")))]\nfn a() {}\n"),
            vec![(1, "target_os"), (1, "unix")]
        );
    }

    #[test]
    fn scan_finds_the_expression_and_attribute_forms() {
        assert_eq!(
            scan_platform_cfg("let sep = if cfg!(windows) { '\\\\' } else { '/' };\n"),
            vec![(1, "windows")]
        );
        assert_eq!(
            scan_platform_cfg("#[cfg_attr(windows, path = \"win.rs\")]\nmod imp;\n"),
            vec![(1, "windows")]
        );
    }

    #[test]
    fn scan_finds_every_target_family_of_predicate_key() {
        for key in [
            "target_arch",
            "target_family",
            "target_env",
            "target_vendor",
            "target_abi",
            "target_endian",
            "target_pointer_width",
        ] {
            let source = format!("#[cfg({key} = \"x\")]\nfn a() {{}}\n");
            assert_eq!(scan_platform_cfg(&source), vec![(1, key)], "{key}");
        }
    }

    #[test]
    fn a_target_key_is_found_on_a_wrapped_continuation_line() {
        // rustfmt splits a long predicate across lines, and the `cfg(` opener is then on a
        // different line from the key. `target_*` keys are recognised without one for this reason.
        let source =
            "#[cfg(any(\n    target_os = \"linux\",\n    target_os = \"macos\"\n))]\nfn a() {}\n";
        assert_eq!(
            scan_platform_cfg(source),
            vec![(2, "target_os"), (3, "target_os")]
        );
    }

    #[test]
    fn a_comment_about_a_platform_conditional_is_not_flagged() {
        // Load-bearing in both directions. `crates/namir-worker/src/lib.rs` carries a comment that
        // deliberately *describes* the attribute rather than spelling it, precisely because the
        // old form would have tripped on the prose; and this module's own doc comment now names
        // every key in the list.
        let source = "// Nothing here is #[cfg(target_os = \"windows\")]-conditional.\n\
                      /// See `cfg!(unix)` for why not.\n\
                      let x = 1;\n";
        assert!(scan_platform_cfg(source).is_empty());
    }

    #[test]
    fn an_ordinary_word_is_not_a_platform_predicate_without_a_cfg_opener() {
        // `windows`/`unix`/`wasm` are ordinary words, which is why they need an opener on the line.
        let source = "for pair in data.windows(2) {}\nlet path = \"/usr/unix/share\";\n";
        assert!(scan_platform_cfg(source).is_empty());
    }

    #[test]
    fn target_has_atomic_is_deliberately_not_flagged() {
        // `crates/namir-engine/src/telemetry_ring.rs:40`, the tree's one live `cfg!`. It asserts a
        // capability the code requires on every target rather than selecting between code paths --
        // see PLATFORM_CFG_KEYS for the full argument, and note that a `target_has_atomic` *branch*
        // would not be caught here.
        let source =
            "const _: () = assert!(\n    cfg!(target_has_atomic = \"64\"),\n    \"msg\"\n);\n";
        assert!(scan_platform_cfg(source).is_empty());
    }

    #[test]
    fn a_cargo_target_table_is_flagged_with_its_header() {
        let source = "[dependencies]\nfoo = \"1\"\n\n[target.'cfg(any(target_os = \"linux\"))'.dependencies]\nalsa = \"0.9\"\n";
        assert_eq!(
            scan_cargo_target_tables(source),
            vec![(
                4,
                "[target.'cfg(any(target_os = \"linux\"))'.dependencies]".to_string()
            )]
        );
    }

    #[test]
    fn an_ordinary_manifest_declares_no_target_table() {
        let source =
            "[package]\nname = \"namir-core\"\n\n[dependencies]\n\n[lints]\nworkspace = true\n";
        assert!(scan_cargo_target_tables(source).is_empty());
    }
}
