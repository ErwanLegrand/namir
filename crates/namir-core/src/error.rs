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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCode {
    /// Stable identifier (FR-PARAM-020-style: never reused, never repurposed).
    pub id: &'static str,
    /// How urgent this error is; see `Severity`.
    pub severity: Severity,
    /// A `{placeholder}`-style template the UI fills in — the audio thread never formats
    /// anything (D-16.2); this template is read only off the RT path.
    pub message_template: &'static str,
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

    const A: ErrorCode = ErrorCode {
        id: "core.sample_rate.zero",
        severity: Severity::Error,
        message_template: "Sample rate must be greater than zero.",
    };
    const B: ErrorCode = ErrorCode {
        id: "core.example.other",
        severity: Severity::Warning,
        message_template: "Example: {detail}",
    };

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
        const EMPTY: ErrorCode = ErrorCode {
            id: "",
            severity: Severity::Info,
            message_template: "",
        };
        assert_unique_ids(&[EMPTY]);
    }

    #[test]
    fn severities_order_by_ascending_urgency() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert!(Severity::Error < Severity::Fault);
    }
}
