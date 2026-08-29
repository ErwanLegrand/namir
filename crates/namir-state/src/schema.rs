//! FR-STATE-040's `S` half: the **schema check** its `*Verify:*` line names, over the format
//! `docs/04-state-and-preset-format.md` §§3–7 documents.
//!
//! # Why this module exists
//!
//! FR-STATE-040's method is compound — `M plus S (schema check)` — and until M15 only the `M` half
//! existed. `xtask traceability` kept the first code of a `*Verify:*` line and no more, so the
//! requirement resolved from its manual-test document alone and read fully covered while the `S`
//! half was executed by nothing anywhere in the tree (issue #27; FRS §5.9's
//! `*Consequence (added M14, 2026-08-12)*` note is the decision that this is built rather than the
//! FRS line narrowed, and it is that note that nominates §§3–7 as the schema, the FRS itself not
//! saying what the schema is).
//!
//! # What it checks, and against what
//!
//! §§3–7 of the format document, clause by clause: §3's top-level structure, §4's `format_version`,
//! §5's legacy `global` section, §6's `parameters`, and §7's `references` including §7.1's file
//! reference shape, §7.2's `embedded` and §7.3's `library_relative` syntax. §2 (encoding and byte
//! ceiling) is enforced by [`Document::parse`] before this module ever sees a document, and §8
//! (unknown-field preservation) is a property of a load-modify-save *cycle* rather than of a
//! document, which is why FR-STATE-010's round-trip test is where that lives.
//!
//! **The rules are restated here from the prose, deliberately, rather than delegated to the
//! reader's own parsing code.** A validator built out of `FileRef::from_value` and
//! `RelPath::parse` would check the reader against itself and agree by construction — the same
//! objection D-23.1's second question makes to a `Verify: G` satisfied by a second in-house
//! implementation. Restating them independently is what lets this module disagree with the reader,
//! and it already does in one place: §7.3 says a stored `library_relative` is "always
//! `/`-separated ... regardless of which platform wrote it", while [`crate::RelPath::parse`]
//! accepts a backslash-separated string and normalises it. The reader is right to be tolerant and
//! the document is right about what conforms, so a stored backslash is reported here as a
//! [`Severity::Recovered`] violation rather than silently blessed.
//!
//! # Severity: what the *reader* does, not how bad it is
//!
//! [`Severity::Rejected`] means the format document says a reader refuses the whole document over
//! this — §4's `format_version` is the only such clause, "the one thing this format treats as
//! fatal rather than tolerated". Everything else is [`Severity::Recovered`]: the value is off-schema
//! and the documented reader behaviour is to carry on with a default, a clamp, or the reference
//! treated as absent (D-11.2's tolerant deserialisation). A `Recovered` violation is still a
//! violation — it is exactly the class a hand-editor (FR-STATE-040's whole point) produces and
//! never hears about otherwise, because tolerant loading is silent by design.

use serde_json::Value;

use crate::document::Document;
use crate::reference::MAX_EMBEDDED_BYTES;

/// What the documented reader does with a document carrying this violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The format document says the whole document is refused (§4 only).
    Rejected,
    /// The value is off-schema and the reader recovers locally — a default, a clamp, or the
    /// reference treated as absent.
    Recovered,
}

/// One clause of §§3–7 that a document does not satisfy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolation {
    /// Where, as a JSON-pointer-style path from the document root (`/references/nam/hash`).
    /// Built for a human reading a validation report; not parsed by anything.
    pub pointer: String,
    /// The section of `docs/04-state-and-preset-format.md` the violated clause is in (`"7.1"`),
    /// so a report says which prose to go and read.
    pub section: &'static str,
    /// What is wrong, in the format document's own terms.
    pub message: String,
    /// What the documented reader does about it.
    pub severity: Severity,
}

impl std::fmt::Display for SchemaViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (§{}): {}", self.pointer, self.section, self.message)
    }
}

/// §7.1's file-reference fields and whether each is required, and §7.2's `embedded` fields the
/// same — the inventories [`validate`] checks against, made public so that the transcription can
/// be held up against the format document's own tables rather than trusted.
///
/// This is the one residue a hand-written schema check has that a generated one would not: these
/// rules are prose in `docs/04-state-and-preset-format.md` and code here, and nothing about the
/// two makes them agree. `tests/schema.rs` parses the `| Field | Required | ... |` tables of §7.1
/// and §7.2 and asserts that they name exactly these fields with exactly these required flags, so
/// a field added to the format document without being added here fails a test rather than
/// silently leaving a clause unchecked.
pub const FILE_REFERENCE_FIELDS: &[(&str, bool)] = &[
    ("hash", true),
    ("library_relative", false),
    ("absolute", false),
    ("display_name", false),
    ("embedded", false),
];

/// §7.2's `embedded` fields. See [`FILE_REFERENCE_FIELDS`].
pub const EMBEDDED_FIELDS: &[(&str, bool)] =
    &[("encoding", true), ("media_type", false), ("data", true)];

/// Every §§3–7 clause `document` violates, in document order (which is sorted-key order — see
/// [`Document`]'s own note on why this workspace never enables `serde_json`'s `preserve_order`).
/// An empty result is the document conforming.
///
/// Deliberately a **list**, not a `Result`: a hand-edited preset with three mistakes in it should
/// report three, the way `xtask identity` and `xtask bundle` report lists for the same reason. And
/// deliberately total — it never stops early, so a `Rejected` `format_version` does not hide a
/// malformed reference underneath it.
pub fn validate(document: &Document) -> Vec<SchemaViolation> {
    let mut out = Vec::new();
    check_format_version(document, &mut out);
    check_object_shaped_sections(document, &mut out);
    check_legacy_global(document, &mut out);
    check_parameters(document, &mut out);
    check_references(document, &mut out);
    out
}

/// [`validate`] over raw bytes: §2's byte ceiling and "the top level is a JSON object" are
/// [`Document::parse`]'s, and are reported as the `Err` they already are rather than restated as
/// violations — a document that is not JSON has no schema to check.
pub fn validate_bytes(bytes: &[u8]) -> Result<Vec<SchemaViolation>, crate::StateError> {
    Ok(validate(&Document::parse(bytes)?))
}

fn violation(
    pointer: &str,
    section: &'static str,
    severity: Severity,
    message: impl Into<String>,
) -> SchemaViolation {
    SchemaViolation {
        pointer: pointer.to_string(),
        section,
        message: message.into(),
        severity,
    }
}

/// §4: "An unsigned integer, required. | Absent, or present but not an integer | **Rejected
/// outright**". The one `Rejected` clause in the format.
fn check_format_version(document: &Document, out: &mut Vec<SchemaViolation>) {
    match document.top_level("format_version") {
        None => out.push(violation(
            "/format_version",
            "4",
            Severity::Rejected,
            "required, and absent -- there is no defensible default for \"which schema is this\"",
        )),
        Some(value) if value.as_u64().is_none() => out.push(violation(
            "/format_version",
            "4",
            Severity::Rejected,
            format!(
                "must be an unsigned integer, found {}",
                json_type_name(value)
            ),
        )),
        Some(_) => {}
    }
    // A version *greater* than this build's is explicitly not a violation: §4 loads it tolerantly
    // with a warning, and §8's unknown-field preservation is what makes that safe. The corpus's
    // `future-version.namirpreset` is that case, and it conforms.
}

/// §3: each of the three keys this build owns, plus §5's legacy `global`, is an object when it is
/// present at all. Every one of them is optional — §3's minimal document is `{"format_version":
/// 1}` and nothing else — and any *other* top-level key is legal by §3's own sentence ("A document
/// may carry other top-level keys ... This build preserves them byte-identically"), so unknown
/// keys are not checked here and must not be.
fn check_object_shaped_sections(document: &Document, out: &mut Vec<SchemaViolation>) {
    for (key, section) in [("parameters", "6"), ("references", "7"), ("global", "5")] {
        if let Some(value) = document.top_level(key)
            && !value.is_object()
        {
            out.push(violation(
                &format!("/{key}"),
                section,
                Severity::Recovered,
                format!(
                    "must be a JSON object, found {} -- a reader reads nothing out of it and \
                     applies defaults",
                    json_type_name(value)
                ),
            ));
        }
    }
}

/// §5's legacy shape, `"global": { "bypass": false, "output_ceiling_db": 0.0 }`. A current writer
/// never emits it and a current reader still accepts it, so a document carrying one is conforming
/// — but its two fields have documented types, and "the field is ... wrongly-typed" is a case §5
/// itself calls out as falling back to the default. Any *other* key inside `global` is a §8
/// unrecognised key: preserved, not applied, not a violation.
fn check_legacy_global(document: &Document, out: &mut Vec<SchemaViolation>) {
    let Some(global) = document.top_level("global").and_then(Value::as_object) else {
        return;
    };
    if let Some(value) = global.get("bypass")
        && !value.is_boolean()
    {
        out.push(violation(
            "/global/bypass",
            "5",
            Severity::Recovered,
            format!(
                "legacy section: must be a boolean, found {} -- falls back to `false`",
                json_type_name(value)
            ),
        ));
    }
    if let Some(value) = global.get("output_ceiling_db")
        && !value.is_number()
    {
        out.push(violation(
            "/global/output_ceiling_db",
            "5",
            Severity::Recovered,
            format!(
                "legacy section: must be a number, found {} -- falls back to `0.0`",
                json_type_name(value)
            ),
        ));
    }
}

/// §6: "A flat JSON object mapping a **stable string key** ... to a number."
///
/// Only the *value* type is checked. An unrecognised key is explicitly legal ("preserved ... but
/// not applied to anything"), and an out-of-range value is explicitly legal too ("clamped into
/// range" — a conforming document, a value a reader adjusts). What is not legal is a value that is
/// not a number at all, which §6 says "resets that one parameter to its default": the reader
/// recovers, and the document is still off-schema.
fn check_parameters(document: &Document, out: &mut Vec<SchemaViolation>) {
    let Some(parameters) = document.top_level("parameters").and_then(Value::as_object) else {
        return;
    };
    for (key, value) in parameters {
        if !value.is_number() {
            out.push(violation(
                &format!("/parameters/{key}"),
                "6",
                Severity::Recovered,
                format!(
                    "must be a number in the parameter's own physical unit, found {} -- resets \
                     this one parameter to its default",
                    json_type_name(value)
                ),
            ));
        }
    }
}

/// §7: up to two keys, `nam` and `ir`, each a §7.1 file reference.
///
/// A key beside those two is **not** reported. §7's own sentence says "up to two keys", but
/// `Document::remove_from_section`'s contract is explicit that "an unrecognised key sitting
/// alongside `nam`/`ir` inside `references` survives a save that clears one of them" — that is
/// §8's second guarantee, and a carrier this build deliberately keeps. Reporting it would be
/// reporting a document for using a facility the format promises it.
fn check_references(document: &Document, out: &mut Vec<SchemaViolation>) {
    let Some(references) = document.top_level("references").and_then(Value::as_object) else {
        return;
    };
    for slot in ["ir", "nam"] {
        let Some(value) = references.get(slot) else {
            continue;
        };
        let pointer = format!("/references/{slot}");
        let Some(reference) = value.as_object() else {
            out.push(violation(
                &pointer,
                "7.1",
                Severity::Recovered,
                format!(
                    "must be a file reference object, found {} -- the stage loads empty",
                    json_type_name(value)
                ),
            ));
            continue;
        };
        check_hash(&pointer, reference.get("hash"), out);
        check_library_relative(&pointer, reference.get("library_relative"), out);
        for (field, section) in [("absolute", "7.1"), ("display_name", "7.1")] {
            if let Some(value) = reference.get(field)
                && !value.is_string()
            {
                out.push(violation(
                    &format!("{pointer}/{field}"),
                    section,
                    Severity::Recovered,
                    format!("must be a string, found {}", json_type_name(value)),
                ));
            }
        }
        check_embedded(&pointer, reference.get("embedded"), out);
    }
}

/// §7.1: `hash` is the one **required** field of a file reference — "string, 64 lowercase hex
/// characters", the reference's identity (P7). "If `hash` is missing, or is present but is not a
/// well-formed 64-hex-character string, the whole reference is malformed: this build's reader
/// treats it as absent ... with a warning, rather than failing the whole document over one bad
/// reference."
fn check_hash(pointer: &str, value: Option<&Value>, out: &mut Vec<SchemaViolation>) {
    let where_ = format!("{pointer}/hash");
    let Some(value) = value else {
        out.push(violation(
            &where_,
            "7.1",
            Severity::Recovered,
            "required, and absent -- the whole reference is malformed and the stage loads empty",
        ));
        return;
    };
    let Some(text) = value.as_str() else {
        out.push(violation(
            &where_,
            "7.1",
            Severity::Recovered,
            format!(
                "must be a 64-character lowercase hex string, found {}",
                json_type_name(value)
            ),
        ));
        return;
    };
    // Restated from §7.1 rather than delegated to `ContentHash`'s own parser, so that this check
    // can disagree with the reader instead of agreeing with it by construction.
    let well_formed = text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !well_formed {
        out.push(violation(
            &where_,
            "7.1",
            Severity::Recovered,
            format!(
                "must be exactly 64 lowercase hex characters, found {} character(s) -- the whole \
                 reference is malformed and the stage loads empty",
                text.chars().count()
            ),
        ));
    }
}

/// §7.3's four rules for a stored `library_relative`, plus §7.3's opening sentence.
///
/// The opening sentence — "Always `/`-separated in the stored document, regardless of which
/// platform wrote it" — is the clause this module and [`crate::RelPath::parse`] disagree about,
/// and the disagreement is deliberate (see this module's header). A backslash in a stored path is
/// off-schema; the reader normalises it and carries on, so the severity is `Recovered`.
fn check_library_relative(pointer: &str, value: Option<&Value>, out: &mut Vec<SchemaViolation>) {
    let Some(value) = value else {
        return;
    };
    let where_ = format!("{pointer}/library_relative");
    let Some(text) = value.as_str() else {
        out.push(violation(
            &where_,
            "7.3",
            Severity::Recovered,
            format!(
                "must be a `/`-separated string, found {}",
                json_type_name(value)
            ),
        ));
        return;
    };
    let mut say = |message: &str| {
        out.push(violation(&where_, "7.3", Severity::Recovered, message));
    };
    if text.is_empty() {
        say("must be non-empty");
        return;
    }
    if text.contains('\\') {
        say(
            "must be `/`-separated in the stored document regardless of which platform wrote it; \
             a `\\` is platform syntax the format never stores (this build's reader normalises it \
             anyway)",
        );
    }
    if text.starts_with('/') {
        say("must not be rooted -- an absolute path belongs in `absolute` instead");
    }
    if text.len() >= 2 && text.as_bytes()[1] == b':' {
        say("must not be drive-prefixed -- an absolute path belongs in `absolute` instead");
    }
    for segment in text.split(['/', '\\']) {
        match segment {
            "" => say("must contain no empty segment"),
            "." => say("must contain no `.` segment"),
            ".." => say("must contain no `..` segment -- no traversal"),
            _ => {}
        }
    }
}

/// §7.2: `encoding` is required and is always `"base64"` ("Any other value is rejected"), `data`
/// is required and is the base64 text, `media_type` is optional and informational. The encoded
/// text is bounded by §2's ceiling "checked against the encoded string's own length, before any
/// base64 decoding happens".
fn check_embedded(pointer: &str, value: Option<&Value>, out: &mut Vec<SchemaViolation>) {
    let Some(value) = value else {
        return;
    };
    let where_ = format!("{pointer}/embedded");
    let Some(embedded) = value.as_object() else {
        out.push(violation(
            &where_,
            "7.2",
            Severity::Recovered,
            format!("must be an object, found {}", json_type_name(value)),
        ));
        return;
    };

    match embedded.get("encoding") {
        None => out.push(violation(
            &format!("{where_}/encoding"),
            "7.2",
            Severity::Recovered,
            "required, and absent -- `\"base64\"` is the only encoding this format defines",
        )),
        Some(Value::String(text)) if text == "base64" => {}
        Some(Value::String(text)) => out.push(violation(
            &format!("{where_}/encoding"),
            "7.2",
            Severity::Recovered,
            format!(
                "`\"{text}\"` is rejected -- `\"base64\"` is the only encoding this format defines"
            ),
        )),
        Some(other) => out.push(violation(
            &format!("{where_}/encoding"),
            "7.2",
            Severity::Recovered,
            format!(
                "must be the string `\"base64\"`, found {}",
                json_type_name(other)
            ),
        )),
    }

    match embedded.get("data") {
        None => out.push(violation(
            &format!("{where_}/data"),
            "7.2",
            Severity::Recovered,
            "required, and absent -- an `embedded` block with no data carries nothing",
        )),
        Some(Value::String(text)) => {
            if text.len() > MAX_EMBEDDED_BYTES {
                out.push(violation(
                    &format!("{where_}/data"),
                    "7.2",
                    Severity::Recovered,
                    format!(
                        "is {} encoded bytes, over the {} MB ceiling §2 puts on the whole \
                         document -- checked on the encoded length, before any decoding",
                        text.len(),
                        MAX_EMBEDDED_BYTES / (1024 * 1024)
                    ),
                ));
            } else if !is_standard_base64(text) {
                out.push(violation(
                    &format!("{where_}/data"),
                    "7.2",
                    Severity::Recovered,
                    "must be base64 in the standard alphabet, with padding",
                ));
            }
        }
        Some(other) => out.push(violation(
            &format!("{where_}/data"),
            "7.2",
            Severity::Recovered,
            format!("must be a base64 string, found {}", json_type_name(other)),
        )),
    }

    if let Some(value) = embedded.get("media_type")
        && !value.is_string()
    {
        out.push(violation(
            &format!("{where_}/media_type"),
            "7.2",
            Severity::Recovered,
            format!(
                "must be a string when present, found {} -- informational only",
                json_type_name(value)
            ),
        ));
    }
}

/// §7.2's "standard alphabet, with padding", spelled out here for the same reason §7.1's hex rule
/// is: a check that called the crate's own decoder would agree with the decoder rather than with
/// the document. Length a multiple of four, `=` only as one or two trailing characters, every
/// other character in `A-Za-z0-9+/`.
fn is_standard_base64(text: &str) -> bool {
    if !text.len().is_multiple_of(4) {
        return false;
    }
    let body = text.trim_end_matches('=');
    if text.len() - body.len() > 2 {
        return false;
    }
    body.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate_text(text: &str) -> Vec<SchemaViolation> {
        validate_bytes(text.as_bytes()).expect("the fixture is a JSON object within §2's ceiling")
    }

    fn pointers(violations: &[SchemaViolation]) -> Vec<&str> {
        violations.iter().map(|v| v.pointer.as_str()).collect()
    }

    #[test]
    fn the_minimal_document_of_section_3_conforms() {
        assert_eq!(validate_text(r#"{"format_version": 1}"#), Vec::new());
    }

    #[test]
    fn a_document_carrying_every_section_conforms() {
        let violations = validate_text(
            r#"{
                "format_version": 1,
                "global": { "bypass": true, "output_ceiling_db": -6.0 },
                "parameters": { "trim.gain_db": 2.5, "global.bypass": 0.0 },
                "references": {
                    "nam": {
                        "hash": "dc57749e025523f24f989853b68405829607c4c84942579df0c3368694a531e3",
                        "library_relative": "marshall/plexi.nam",
                        "absolute": "C:\\Users\\erwan\\Models\\plexi.nam",
                        "display_name": "plexi.nam",
                        "embedded": {
                            "encoding": "base64",
                            "media_type": "application/vnd.namir.nam+json",
                            "data": "eyJmYWtlIjoxfQ=="
                        }
                    }
                }
            }"#,
        );
        assert_eq!(violations, Vec::new());
    }

    /// §3's "A document may carry other top-level keys ... preserved byte-identically", and §6's
    /// "A key this reader does not recognise is preserved ... but not applied". Neither is a
    /// violation, and a validator that reported them would be arguing with the format.
    #[test]
    fn unrecognised_keys_are_not_violations_at_either_level() {
        let violations = validate_text(
            r#"{
                "format_version": 1,
                "future_top_level_section": { "nested": { "deeper": [1, 2, 3] } },
                "parameters": { "comp.ratio": 4.0 },
                "references": { "future_slot": { "whatever": true } }
            }"#,
        );
        assert_eq!(violations, Vec::new());
    }

    /// §4's table, both rows of it: absent and present-but-not-an-integer, and both `Rejected` --
    /// the only clause in §§3–7 that is.
    #[test]
    fn a_missing_or_non_integer_format_version_is_the_one_rejecting_clause() {
        for text in [
            r#"{"parameters": {}}"#,
            r#"{"format_version": "1"}"#,
            r#"{"format_version": 1.5}"#,
            r#"{"format_version": -1}"#,
        ] {
            let violations = validate_text(text);
            assert_eq!(pointers(&violations), ["/format_version"], "{text}");
            assert_eq!(violations[0].severity, Severity::Rejected, "{text}");
        }
    }

    /// §4: "Greater than this build's version | Loaded **tolerantly**, with a warning." A newer
    /// document conforms; the corpus's `future-version.namirpreset` is this case.
    #[test]
    fn a_newer_format_version_conforms() {
        assert_eq!(validate_text(r#"{"format_version": 2}"#), Vec::new());
    }

    #[test]
    fn a_section_that_is_not_an_object_is_reported_per_section() {
        let violations = validate_text(
            r#"{"format_version": 1, "parameters": 3, "references": "none", "global": []}"#,
        );
        assert_eq!(
            pointers(&violations),
            ["/parameters", "/references", "/global"]
        );
        assert!(violations.iter().all(|v| v.severity == Severity::Recovered));
    }

    /// §6: the value type, and only the value type. An out-of-range value is a *conforming*
    /// document the reader clamps, so it must not be reported.
    #[test]
    fn a_parameter_value_must_be_a_number_and_may_be_out_of_range() {
        let violations = validate_text(
            r#"{"format_version": 1,
                "parameters": {"a.out_of_range": 1e9, "b.string": "0.5", "c.null": null}}"#,
        );
        assert_eq!(
            pointers(&violations),
            ["/parameters/b.string", "/parameters/c.null"]
        );
    }

    #[test]
    fn the_legacy_global_sections_two_fields_have_documented_types() {
        let violations = validate_text(
            r#"{"format_version": 1, "global": {"bypass": 1, "output_ceiling_db": "0"}}"#,
        );
        assert_eq!(
            pointers(&violations),
            ["/global/bypass", "/global/output_ceiling_db"]
        );
    }

    /// §7.1's one required field, in all three of its failing shapes.
    #[test]
    fn a_reference_without_a_well_formed_hash_is_malformed() {
        for (text, expected) in [
            (r#"{"library_relative": "a/b.nam"}"#, "required, and absent"),
            (r#"{"hash": 7}"#, "found a number"),
            (r#"{"hash": "abc"}"#, "found 3 character(s)"),
            (
                // Uppercase hex: 64 characters, and §7.1 says lowercase.
                r#"{"hash": "DC57749E025523F24F989853B68405829607C4C84942579DF0C3368694A531E3"}"#,
                "64 lowercase hex characters",
            ),
        ] {
            let violations = validate_text(&format!(
                r#"{{"format_version": 1, "references": {{"nam": {text}}}}}"#
            ));
            assert_eq!(pointers(&violations), ["/references/nam/hash"], "{text}");
            assert!(
                violations[0].message.contains(expected),
                "{text}: {}",
                violations[0]
            );
        }
    }

    #[test]
    fn a_reference_that_is_not_an_object_is_reported_once() {
        let violations =
            validate_text(r#"{"format_version": 1, "references": {"ir": "1960a.wav"}}"#);
        assert_eq!(pointers(&violations), ["/references/ir"]);
    }

    /// §7.3's four rules. Each fixture carries a well-formed hash so the only thing under test is
    /// the path.
    #[test]
    fn library_relative_follows_section_7_3s_path_syntax() {
        const HASH: &str = "dc57749e025523f24f989853b68405829607c4c84942579df0c3368694a531e3";
        for (path, expected) in [
            ("", "non-empty"),
            ("/cabs/1960a.wav", "must not be rooted"),
            ("C:/cabs/1960a.wav", "drive-prefixed"),
            ("cabs//1960a.wav", "no empty segment"),
            ("cabs/./1960a.wav", "no `.` segment"),
            ("../1960a.wav", "no `..` segment"),
            ("cabs\\1960a.wav", "`/`-separated in the stored document"),
        ] {
            let violations = validate_text(&format!(
                r#"{{"format_version": 1, "references": {{"ir":
                   {{"hash": "{HASH}", "library_relative": "{}"}}}}}}"#,
                path.replace('\\', "\\\\")
            ));
            assert!(
                violations
                    .iter()
                    .any(|v| v.pointer == "/references/ir/library_relative"
                        && v.message.contains(expected)),
                "{path}: {violations:?}"
            );
        }
    }

    /// The disagreement with [`crate::RelPath::parse`] this module's header records, asserted so
    /// it stays deliberate: the reader accepts the backslash form and this check does not.
    #[test]
    fn a_backslash_path_is_off_schema_even_though_the_reader_accepts_it() {
        assert!(crate::RelPath::parse("cabs\\1960a.wav").is_ok());
        const HASH: &str = "dc57749e025523f24f989853b68405829607c4c84942579df0c3368694a531e3";
        let violations = validate_text(&format!(
            r#"{{"format_version": 1, "references": {{"ir":
               {{"hash": "{HASH}", "library_relative": "cabs\\1960a.wav"}}}}}}"#
        ));
        assert_eq!(pointers(&violations), ["/references/ir/library_relative"]);
    }

    /// §7.2, every clause of its own table.
    #[test]
    fn embedded_follows_section_7_2() {
        const HASH: &str = "dc57749e025523f24f989853b68405829607c4c84942579df0c3368694a531e3";
        for (embedded, expected_pointer, expected) in [
            (r#"7"#, "/references/nam/embedded", "must be an object"),
            (
                r#"{"data": "eyJhIjoxfQ=="}"#,
                "/references/nam/embedded/encoding",
                "required, and absent",
            ),
            (
                r#"{"encoding": "hex", "data": "eyJhIjoxfQ=="}"#,
                "/references/nam/embedded/encoding",
                "is rejected",
            ),
            (
                r#"{"encoding": "base64"}"#,
                "/references/nam/embedded/data",
                "required, and absent",
            ),
            (
                r#"{"encoding": "base64", "data": "not valid base64!"}"#,
                "/references/nam/embedded/data",
                "standard alphabet",
            ),
            (
                r#"{"encoding": "base64", "data": "eyJhIjoxfQ==", "media_type": 3}"#,
                "/references/nam/embedded/media_type",
                "must be a string when present",
            ),
        ] {
            let violations = validate_text(&format!(
                r#"{{"format_version": 1, "references": {{"nam":
                   {{"hash": "{HASH}", "embedded": {embedded}}}}}}}"#
            ));
            assert!(
                violations
                    .iter()
                    .any(|v| v.pointer == expected_pointer && v.message.contains(expected)),
                "{embedded}: {violations:?}"
            );
        }
    }

    /// Every violation is reported, not the first one: a hand-edited preset with three mistakes
    /// should say three things, and a `Rejected` `format_version` must not mask what is under it.
    #[test]
    fn validation_is_total_rather_than_stopping_at_the_first_violation() {
        let violations = validate_text(
            r#"{"parameters": {"trim.gain_db": "loud"},
                "references": {"ir": {"hash": "short"}}}"#,
        );
        assert_eq!(
            pointers(&violations),
            [
                "/format_version",
                "/parameters/trim.gain_db",
                "/references/ir/hash"
            ]
        );
    }

    #[test]
    fn a_document_that_is_not_json_has_no_schema_to_check() {
        assert!(validate_bytes(b"not json at all").is_err());
        assert!(validate_bytes(b"[1, 2, 3]").is_err());
    }
}
