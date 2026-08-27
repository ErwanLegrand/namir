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

/// One entry in a crate's error catalogue (D-16.1): a stable identifier, a severity, a
/// message template and a remedy — never a formatted string, so the audio thread never allocates
/// or formats one (D-16.2).
///
/// # `remedy`: why it is a field and not a second sentence in the template (M14, issue #41)
///
/// FR-UI-070's third clause — "an error shall state ... what the user can do" — was met by **none**
/// of the catalogue entries in the tree when a human ran
/// `docs/manual-tests/fr-ui-070-non-modal-error-notices.md` on 2026-08-27. Two entries came closest
/// by saying what the *system* did instead ("using {fallback} instead"), which is not the same
/// thing.
///
/// Folding the remedy into [`Self::message_template`] as a trailing sentence was considered and
/// rejected twice over. It cannot be checked mechanically — no scanner can tell a remedy sentence
/// from any other sentence — so the clause would go back to being met by review alone, which is how
/// it came to be met nowhere. And the UI cannot style or omit it separately: a remedy belongs in a
/// weaker style beneath the failure, and a caller that has no room for one (a log line, a
/// `Display` impl) has no way to drop just that half of a single string.
///
/// So it is its own `&'static str`, and `xtask error-catalogue` rejects an empty one.
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
    /// The user-facing sentence describing what failed, with **at most one** placeholder, which
    /// must be the exact token [`DETAIL_PLACEHOLDER`] — see [`ErrorCode::render`] for why that is
    /// the whole substitution vocabulary, and `xtask error-catalogue` for the check that keeps it
    /// so. The audio thread never formats anything (D-16.2); this template is read only off the RT
    /// path.
    pub message_template: &'static str,
    /// What the user can do about it (FR-UI-070's third clause) — an action they can take, not a
    /// restatement of what the system already did. Never empty; see this type's doc comment for
    /// why it is a field rather than a sentence appended to `message_template`.
    pub remedy: &'static str,
}

/// The one placeholder a [`ErrorCode::message_template`] may contain, filled from the caller's
/// `detail` string by [`ErrorCode::render`].
pub const DETAIL_PLACEHOLDER: &str = "{detail}";

impl ErrorCode {
    /// The one way to build an `ErrorCode`. `const` so a catalogue entry stays a `const` — every
    /// one in the tree is declared that way, and a runtime constructor would have forced them all
    /// into `LazyLock` for nothing.
    pub const fn new(
        id: &'static str,
        severity: Severity,
        message_template: &'static str,
        remedy: &'static str,
    ) -> Self {
        Self {
            id,
            severity,
            message_template,
            remedy,
        }
    }

    /// Renders [`Self::message_template`] against one caller-supplied `detail` string.
    ///
    /// # The substitution, and why the vocabulary is one token wide (M14, issue #15)
    ///
    /// Until M14 **nothing in the tree substituted anything**: `namir-ui`'s `notice_text` printed
    /// `message_template` verbatim and appended the detail in parentheses, so a template reading
    /// `The {direction} device "{device}" became unavailable` reached a real screen with those
    /// braces in it. The manual FR-UI-070 run of 2026-08-27 transcribed eight distinct literal
    /// tokens off a real window.
    ///
    /// The fix could have been a named field map — `{device}` filled from a `device` value. It is
    /// not, for two reasons the same run supplies. Every producer of a catalogued error in this
    /// tree (`NamLoadError`, `IrLoadError`, `LibraryError`, `StateWarning`, `WorkerError`) already
    /// carries exactly one free-text `detail`, and every one of them already puts the file, device,
    /// key or number into it; a field map would have meant threading names through five crates to
    /// arrive at the same sentence. And a template with several named slots is exactly what makes
    /// a notice "say everything twice" once the detail is appended as well — the failure mode that
    /// run named against step 11.
    ///
    /// So: one insertion point per template, spelled [`DETAIL_PLACEHOLDER`]. A template that has
    /// one gets the detail *inside* the sentence; a template that has none — because it is
    /// self-contained — gets it appended in parentheses, as before. Either way no brace survives
    /// to the screen, and `xtask error-catalogue` refuses any other placeholder token.
    ///
    /// # RT-safety, by construction rather than by discipline
    ///
    /// This allocates, and it may never run on the audio thread. It cannot: D-16.2 puts template
    /// formatting on the UI/worker side, and the audio thread never holds an `ErrorCode` at all —
    /// `namir-engine`'s audio path reports through the telemetry ring as plain numbers, and the
    /// non-RT side is what maps a number to a catalogue entry. There is no call path from
    /// `AudioEngine::process` to this function to guard, which is why the guard is a design
    /// property recorded here rather than a runtime check.
    #[must_use]
    pub fn render(&self, detail: &str) -> String {
        let detail = detail.trim();
        if self.message_template.contains(DETAIL_PLACEHOLDER) {
            let filled = if detail.is_empty() {
                "no further detail"
            } else {
                detail
            };
            self.message_template.replace(DETAIL_PLACEHOLDER, filled)
        } else if detail.is_empty() {
            self.message_template.to_string()
        } else {
            format!("{} ({detail})", self.message_template)
        }
    }
}

/// A crate-time check that a catalogue slice has no duplicate or empty identifiers, and that every
/// entry offers the user a remedy (FR-UI-070's third clause). Each crate that defines `ErrorCode`
/// consts calls this from its own tests over its own catalogue.
///
/// The remedy assertion duplicates `xtask error-catalogue`'s deliberately: that check is
/// line-based and sees only what a source line spells out, while this one runs against the real
/// `const` values a crate actually enumerates.
pub fn assert_unique_ids(codes: &[ErrorCode]) {
    let mut seen = std::collections::HashSet::new();
    for code in codes {
        assert!(!code.id.is_empty(), "ErrorCode with an empty id");
        assert!(
            seen.insert(code.id),
            "duplicate ErrorCode id: {:?}",
            code.id
        );
        assert!(
            !code.remedy.trim().is_empty(),
            // Worded to avoid the literal `ErrorCode` + brace sequence: that is the struct-literal
            // token `xtask error-catalogue` scans for, and prose about a construction is not one.
            "the error code {:?} has no remedy -- FR-UI-070 requires every error to state what \
             the user can do",
            code.id
        );
        assert!(
            !mentions_unknown_placeholder(code.message_template),
            "the error code {:?} has a message_template carrying a placeholder other than {:?}: \
             {:?}",
            code.id,
            DETAIL_PLACEHOLDER,
            code.message_template
        );
    }
}

/// Whether `template` contains a `{...}` token that is not [`DETAIL_PLACEHOLDER`].
fn mentions_unknown_placeholder(template: &str) -> bool {
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open..];
        let Some(close) = after.find('}') else {
            return false;
        };
        if &after[..=close] != DETAIL_PLACEHOLDER {
            return true;
        }
        rest = &after[close + 1..];
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: ErrorCode = ErrorCode::new(
        "core.sample_rate.zero",
        Severity::Error,
        "Sample rate must be greater than zero.",
        "Choose a sample rate your device supports and try again.",
    );
    const B: ErrorCode = ErrorCode::new(
        "core.example.other",
        Severity::Warning,
        "Example: {detail}",
        "Nothing to do; this entry exists only for this crate's own tests.",
    );

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
        const EMPTY: ErrorCode = ErrorCode::new("", Severity::Info, "", "");
        assert_unique_ids(&[EMPTY]);
    }

    /// FR-UI-070's third clause, mechanically: an entry that tells the user nothing they can do is
    /// not a catalogue entry this project accepts. Issue #41 is the record of all ~69 of them
    /// having been in exactly that state until M14.
    // No tag: FR-UI-070's `Verify:` is M and its manual document is the traced artifact (D-18.6),
    // so no test here may claim it. This one carries FR-ERR-020's catalogue half instead.
    #[test]
    #[should_panic(expected = "has no remedy")]
    fn an_entry_with_no_remedy_fails() {
        const NO_REMEDY: ErrorCode =
            ErrorCode::new("core.example.no_remedy", Severity::Info, "Something.", "  ");
        assert_unique_ids(&[NO_REMEDY]);
    }

    /// Issue #15: the eight literal tokens a human transcribed off a real screen were all of this
    /// shape. One placeholder spelling is allowed and every other one is a catalogue defect.
    #[test]
    #[should_panic(expected = "carrying a placeholder other than")]
    fn a_template_with_a_named_placeholder_fails() {
        const NAMED: ErrorCode = ErrorCode::new(
            "core.example.named",
            Severity::Info,
            "The {direction} device \"{device}\" went away.",
            "Reconnect it.",
        );
        assert_unique_ids(&[NAMED]);
    }

    /// The substitution issue #15 asks for: a template's one placeholder is filled, never printed.
    #[test]
    fn render_substitutes_the_detail_placeholder_rather_than_printing_it() {
        let text = B.render("C:/models/plexi.nam");
        assert_eq!(text, "Example: C:/models/plexi.nam");
        assert!(!text.contains('{'), "{text}");
    }

    /// A self-contained template keeps the pre-M14 shape: the detail is appended in parentheses.
    #[test]
    fn render_appends_the_detail_when_the_template_has_no_placeholder() {
        assert_eq!(
            A.render("48001 Hz"),
            "Sample rate must be greater than zero. (48001 Hz)"
        );
        assert_eq!(A.render("   "), "Sample rate must be greater than zero.");
    }

    /// An empty detail must not leave a bare colon or a dangling word where the fact should be.
    #[test]
    fn render_names_a_missing_detail_rather_than_leaving_a_hole() {
        assert_eq!(B.render(""), "Example: no further detail");
    }

    #[test]
    fn severities_order_by_ascending_urgency() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert!(Severity::Error < Severity::Fault);
    }
}
