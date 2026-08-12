//! D-16.1: "One enumerated error catalogue in `namir-core`, each entry with a stable identifier,
//! a severity, and a user-facing message template."
//!
//! Read literally as one closed `enum` this would make `namir-core` — which D-5.2 forbids from
//! depending on anything above it — the crate that has to know about every error any higher
//! crate can produce, growing on every unrelated change elsewhere in the tree. So the catalogue
//! lives here as one shared *type* (`ErrorCode`, plus `Severity`) that any crate instantiates as
//! `const`s; FR-ERR-020's "every user-visible error maps to a catalogue entry, verified
//! statically" is satisfied per-crate by `assert_unique_ids` over that crate's own consts, not by
//! namir-core enumerating the whole system. This is a design call the architecture document
//! doesn't spell out to this level of detail; recorded here so it isn't rediscovered.

/// How urgent an `ErrorCode` is, ordered ascending by urgency (`Info < Warning < Error <
/// Fault`) so callers can threshold with a plain comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Purely informational; no degradation and nothing the user need act on.
    Info,
    /// Something is off but the engine's output is still trustworthy.
    Warning,
    /// The engine's output for the affected stage or file is not trustworthy as-is.
    Error,
    /// A fault the engine cannot continue from without degrading (P8) — a stage silencing
    /// itself, not the host process.
    Fault,
}

/// One entry in a crate's error catalogue (D-16.1): a stable identifier, a severity, and a
/// message template — never a formatted string, so the audio thread never allocates or formats
/// one (D-16.2).
///
/// # `#[non_exhaustive]`: why a call site may read one but not build one (M14, FR-ERR-020)
///
/// FR-ERR-020's method has two conjuncts — "the catalogue is enumerable **and** every error path in
/// the code maps to an entry" — and the second had no artifact of any kind. Part of the reason was
/// this type: all three fields were `pub` on an ordinary struct, so any expression anywhere could
/// build an `ErrorCode` inline, and two live sites did — `crates/namir-ui/examples/
/// manual_window_smoke.rs` invented `ui.manual_smoke.example_notice`, and `namir-app`'s
/// `AppHost::handle` built `app.host.scan_warning` in the middle of a `push_notice` call. Neither
/// id belonged to any catalogue; neither would have appeared in any enumeration of one.
///
/// `#[non_exhaustive]` makes the struct-literal form unavailable outside `namir-core`, so **every**
/// construction in the tree now goes through [`ErrorCode::new`] — one token, greppable, and the
/// thing `xtask error-catalogue` looks for. The fields stay `pub` for *reading*: nothing about a
/// call site consulting `code.severity` is a problem, and making the reads go through accessors
/// would have churned ~40 sites to no end.
///
/// This is not by itself a guarantee — `ErrorCode::new` is still callable anywhere. It is what
/// makes the guarantee *checkable*, and the check is `xtask error-catalogue`: every `ErrorCode::new`
/// in the tree must be a named `const` in a catalogue module. See that module's doc comment for
/// what it can and cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ErrorCode {
    /// Stable identifier (FR-PARAM-020-style: never reused, never repurposed).
    pub id: &'static str,
    /// How urgent this error is; see `Severity`.
    pub severity: Severity,
    /// A `{placeholder}`-style template the UI fills in — the audio thread never formats
    /// anything (D-16.2); this template is read only off the RT path.
    pub message_template: &'static str,
}

impl ErrorCode {
    /// The one way to build an `ErrorCode`. `const` so a catalogue entry stays a `const` — every
    /// one in the tree is declared that way, and a runtime constructor would have forced them all
    /// into `LazyLock` for nothing.
    pub const fn new(id: &'static str, severity: Severity, message_template: &'static str) -> Self {
        Self {
            id,
            severity,
            message_template,
        }
    }
}

/// A crate-time check that a catalogue slice has no duplicate or empty identifiers. Each crate
/// that defines `ErrorCode` consts calls this from its own tests over its own catalogue.
pub fn assert_unique_ids(codes: &[ErrorCode]) {
    let mut seen = std::collections::HashSet::new();
    for code in codes {
        assert!(!code.id.is_empty(), "ErrorCode with an empty id");
        assert!(
            seen.insert(code.id),
            "duplicate ErrorCode id: {:?}",
            code.id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: ErrorCode = ErrorCode::new(
        "core.sample_rate.zero",
        Severity::Error,
        "Sample rate must be greater than zero.",
    );
    const B: ErrorCode =
        ErrorCode::new("core.example.other", Severity::Warning, "Example: {detail}");

    // The method's first conjunct, "the catalogue is enumerable": this test and the matching one in
    // every crate's own `error_codes.rs`. Its second, "every error path in the code maps to an
    // entry", is `xtask error-catalogue` since M14 — `#[non_exhaustive]` above puts every
    // construction through `ErrorCode::new`, and that check requires each one to be a named `const`
    // inside a catalogue module.
    // trace: FR-ERR-020
    #[test]
    fn unique_ids_pass() {
        assert_unique_ids(&[A, B]);
    }

    #[test]
    #[should_panic(expected = "duplicate ErrorCode id")]
    fn duplicate_ids_fail() {
        assert_unique_ids(&[A, A]);
    }

    #[test]
    #[should_panic(expected = "empty id")]
    fn empty_id_fails() {
        const EMPTY: ErrorCode = ErrorCode::new("", Severity::Info, "");
        assert_unique_ids(&[EMPTY]);
    }

    #[test]
    fn severities_order_by_ascending_urgency() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert!(Severity::Error < Severity::Fault);
    }
}
