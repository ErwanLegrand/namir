//! FR-ERR-030 (Must, `Verify:` **S** plus I): "No logging, allocation or formatting for logging
//! shall occur on the audio thread." This module implements the **S** half's logging limb — the one
//! `assert_no_alloc` structurally cannot see, since a level check that returns below threshold
//! allocates nothing and so passes D-7.5's harness clean while still being a logging call on the
//! audio thread.
//!
//! # Why this check has to exist at all, and why it is module-scoped
//!
//! For eleven of the thirteen crates in D-5.1's table the property is already structural and needs
//! no check of its own: `namir-engine` and everything below it may not depend on `namir-platform`,
//! so no code there can so much as *name* `namir_platform::logging`, and `xtask layering` fails the
//! build on the edge that would change that. `crates/namir-platform/src/logging.rs`'s own module
//! doc comment states the residue in as many words: `namir-app` and `namir-clap` depend on
//! everything *and* own the audio callbacks, so those two crates could emit a record from inside
//! `cpal`'s data callback or CLAP's `process()`, and "nothing mechanical stops them". This module is
//! the mechanism that now does.
//!
//! **Module-scoped, not crate-scoped.** A crate-wide ban would be wrong, not merely strict: the
//! logger's legitimate callers live in exactly these two crates — `namir-app`'s `AppHost::
//! push_notice` (`crates/namir-app/src/host.rs`), `namir-clap`'s `SharedInner::push_notice` and
//! `log_worker_warning` (`crates/namir-clap/src/shared.rs`), and each shell's `logging::init` call.
//! Every one of those is on the UI or main thread, and FR-ERR-010's log would have no records at all
//! if they were forbidden. So the unit of the ban is the module, and [`AUDIO_THREAD_MODULES`] is the
//! hand-maintained list of the ones that carry audio-thread code — the same "manually-maintained
//! mirror, kept in sync by hand" device [`crate::layering::LAYERING_TABLE`] uses for D-5.1's table,
//! and for the same reason: there is no machine-readable statement of which function runs on which
//! thread to derive it from.
//!
//! # File granularity, deliberately, and why that is honest
//!
//! Two of the listed modules mix threads. `namir-clap`'s `audio.rs` holds `process()`, `reset()` and
//! `apply_direct_and_mirror()` — audio thread — beside `activate()`/`deactivate()`, which CLAP
//! declares `[main-thread]`; `params_ext.rs` holds `PluginAudioProcessorParams::flush` (audio
//! thread) beside the whole `PluginMainThreadParams` impl. A line-based scanner cannot tell which
//! function a line belongs to without a Rust parser this project has no other use for, so the ban is
//! applied to the whole file.
//!
//! That is an **over**-approximation, and the direction matters: it can raise a false alarm on a
//! main-thread function, and can never let an audio-thread call through. A check that erred the
//! other way would be worse than none, because it would read as a guarantee. The escape hatch for a
//! genuine main-thread caller in a listed file already exists and is already the house pattern:
//! `audio.rs`'s `activate()` reports its unusable-sample-rate condition through
//! `shared.inner.push_notice(...)`, and it is `shared.rs` — not `audio.rs` — that names the logger.
//! A new main-thread diagnostic in a listed module goes the same way.
//!
//! # What it forbids: the names, not a call
//!
//! [`FORBIDDEN_NAMES`] are matched as **whole identifiers**, so every spelling of the module and of
//! its re-exported items is caught wherever the path is broken: `namir_platform::logging::record`,
//! `use namir_platform::logging;` then `logging::record(..)`, `use namir_platform::logging::record
//! as emit;`, `use namir_platform::Logger;` (the crate root re-exports `LogLevel` and `Logger`).
//! `record` is deliberately **not** on the list: `XrunCounter::record` is a legitimate, RT-safe
//! atomic increment called from `stream.rs`'s own output callback, and a line-based scanner cannot
//! tell `xruns.record()` from a bare `record()` reached through `use ...logging::record`. What is
//! banned instead is the import that would introduce that bare spelling — which, in Rust, is
//! necessarily in the same file.
//!
//! # Residual blind spots, stated rather than pretended closed
//!
//! 1. **Not transitive.** This forbids *naming* the logger in an audio-thread module, not *reaching*
//!    it. A helper defined elsewhere that logs internally can still be called from a listed module,
//!    and today one is: `activate()` calls `push_notice`, which logs. That call is legitimate
//!    (`activate` is `[main-thread]`), but the check would not have objected if it were not. Only
//!    review, and D-7.5's allocation harness, cover that.
//! 2. **The list is hand-maintained.** New audio-callback code in a module not listed here is
//!    unchecked. Mitigated as far as a static check can be: [`crate::main`] treats an unreadable or
//!    missing listed file as a violation, so a rename or a move fails the gate loudly instead of
//!    silently un-covering the module.
//! 3. **Re-export chains that drop the module name.** `pub use namir_platform::logging::record;` in
//!    a third crate, called as `that_crate::record(..)`, names nothing on the list. So does a glob
//!    (`use crate::prelude::*`) that re-exports it.
//! 4. **Line-based, like `layering`'s and `traceability`'s scanners.** Lines whose trimmed form
//!    begins `//` are skipped, so prose about the logger — including this file's own — does not trip
//!    it; a name inside a `/* */` block comment or a string literal would be a false positive, and a
//!    name assembled at run time (a `macro_rules!` expansion, a function pointer taken elsewhere and
//!    called here) is invisible.
//! 5. **Only these two crates matter, and only because of D-5.1.** If the layering table ever grants
//!    another crate an edge to `namir-platform`, this list must grow with it; nothing links the two
//!    tables mechanically.

/// The modules that carry code executing on the audio thread, in the two crates D-5.1 permits to
/// depend on `namir-platform`, each with the reason it is on the list. Paths are repo-root-relative
/// and use forward slashes; [`crate::main`] joins them to the real root.
///
/// A file listed here that cannot be read is itself a violation (see the module doc's residual 2).
pub const AUDIO_THREAD_MODULES: &[(&str, &str)] = &[
    (
        "crates/namir-clap/src/audio.rs",
        "is CLAP's `[audio-thread]` half: `process()`, `reset()` and `apply_direct_and_mirror()`",
    ),
    (
        "crates/namir-clap/src/params_ext.rs",
        "carries `PluginAudioProcessorParams::flush`, which clack documents as the audio thread",
    ),
    (
        "crates/namir-clap/src/param_mirror.rs",
        "is written from the audio thread by `apply_direct_and_mirror`",
    ),
    (
        "crates/namir-app/src/stream.rs",
        "owns both `cpal` data callbacks, and runs `AudioEngine::process` in the output one",
    ),
    (
        "crates/namir-app/src/bridge.rs",
        "is the SPSC ring whose two ends live inside those two `cpal` callbacks",
    ),
    (
        "crates/namir-app/src/xrun.rs",
        "is incremented from the output callback (`XrunCounter::record`)",
    ),
];

/// The identifiers an audio-thread module may not name. Whole-identifier matches, so a path is
/// caught wherever it is broken across `use`/call sites — see the module doc for why `record` is
/// absent and what is banned in its place.
///
/// `logging` covers the module however it is imported or aliased at its source; `Logger` and
/// `LogLevel` are `namir-platform`'s crate-root re-exports of the two public types
/// (`crates/namir-platform/src/lib.rs`), reachable without the module name; `record_verbose` is
/// distinctive enough to ban bare, unlike `record`.
pub const FORBIDDEN_NAMES: &[&str] = &["logging", "Logger", "LogLevel", "record_verbose"];

/// Whether `line` names `ident` as a whole identifier — i.e. with neither neighbouring byte being
/// part of an identifier. Keeps `xrun_logging_thread` and `Loggerish` from matching `logging` and
/// `Logger`, while `namir_platform::logging::record` matches on both `:` boundaries.
fn names_identifier(line: &str, ident: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(ident).any(|(start, _)| {
        let end = start + ident.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
        before_ok && after_ok
    })
}

/// ASCII-only, which is all Rust paths in this tree use. A non-ASCII identifier byte is `>= 0x80`
/// and is treated as part of an identifier so a multi-byte character never looks like a boundary.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Scans `source` for [`FORBIDDEN_NAMES`], line by line, skipping lines whose trimmed form begins
/// `//` (see the module doc's residual 4). Returns `(1-indexed line number, matched name)` per
/// occurrence — a line naming more than one yields more than one entry, in [`FORBIDDEN_NAMES`]
/// order. Pure string logic so it is unit-testable without a filesystem; [`crate::main`] applies it
/// to the real files.
pub fn scan_logger_names(source: &str) -> Vec<(usize, &'static str)> {
    let mut hits = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for name in FORBIDDEN_NAMES {
            if names_identifier(line, name) {
                hits.push((idx + 1, *name));
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fully_qualified_record_call_is_flagged() {
        let source = "fn process() {\n    namir_platform::logging::record(CODE, \"detail\");\n}\n";
        assert_eq!(scan_logger_names(source), vec![(2, "logging")]);
    }

    #[test]
    fn an_audio_callback_that_names_nothing_is_clean() {
        // The shape `stream.rs`'s output callback actually has, `xruns.record()` included: the one
        // spelling that would false-positive if `record` were on the banned list.
        let source = "let _guard = DenormalGuard::new();\nif padded > 0 {\n    xruns.record();\n}\nengine.process(&mut io);\n";
        assert!(scan_logger_names(source).is_empty());
    }

    #[test]
    fn importing_the_module_is_flagged_even_without_a_call() {
        let source = "use namir_platform::logging;\n";
        assert_eq!(scan_logger_names(source), vec![(1, "logging")]);
    }

    #[test]
    fn a_renamed_import_of_the_free_function_is_flagged_at_the_use() {
        // The bare `emit(..)` below is invisible to a line scanner; the `use` that made it resolve
        // is not, and in Rust it is necessarily in the same file.
        let source = "use namir_platform::logging::record as emit;\nfn process() {\n    emit(CODE, \"d\");\n}\n";
        assert_eq!(scan_logger_names(source), vec![(1, "logging")]);
    }

    #[test]
    fn the_crate_root_reexports_are_flagged_without_the_module_name() {
        let source = "use namir_platform::{DenormalGuard, LogLevel, Logger};\n";
        assert_eq!(
            scan_logger_names(source),
            vec![(1, "Logger"), (1, "LogLevel")]
        );
    }

    #[test]
    fn record_verbose_is_flagged_bare() {
        let source = "record_verbose(CODE, \"per-block detail\");\n";
        assert_eq!(scan_logger_names(source), vec![(1, "record_verbose")]);
    }

    #[test]
    fn a_comment_mentioning_the_logger_is_not_flagged() {
        // Load-bearing: this file's own module doc, and the doc comments the listed modules carry,
        // discuss `namir_platform::logging` at length. A check that tripped on prose about itself
        // would be uninstallable.
        let source = "// FR-ERR-030: never call namir_platform::logging::record from here.\n    /// See `Logger` for why.\nlet x = 1;\n";
        assert!(scan_logger_names(source).is_empty());
    }

    #[test]
    fn a_trailing_comment_does_not_hide_a_real_call_on_the_same_line() {
        let source = "logging::record(CODE, \"d\"); // fine, honest\n";
        assert_eq!(scan_logger_names(source), vec![(1, "logging")]);
    }

    #[test]
    fn identifiers_that_merely_contain_a_forbidden_name_are_not_flagged() {
        let source = "let xrun_logging_thread = 1;\nstruct Loggerish;\nfn preloggingly() {}\n";
        assert!(scan_logger_names(source).is_empty());
    }

    #[test]
    fn every_hit_on_a_line_is_reported_with_its_line_number() {
        let source =
            "fn a() {}\nuse namir_platform::logging::{record, record_verbose};\nfn b() {}\n";
        assert_eq!(
            scan_logger_names(source),
            vec![(2, "logging"), (2, "record_verbose")]
        );
    }

    #[test]
    fn the_module_list_is_non_empty_and_uses_repo_relative_forward_slashes() {
        assert!(!AUDIO_THREAD_MODULES.is_empty());
        for (path, why) in AUDIO_THREAD_MODULES {
            assert!(path.starts_with("crates/"), "{path}");
            assert!(!path.contains('\\'), "{path}");
            assert!(!why.is_empty(), "{path} has no stated reason");
        }
    }
}
