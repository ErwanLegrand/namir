//! D-11.1's envelope, and D-11.2's write-back mechanism.
//!
//! # Why a carrier, not `#[serde(flatten)]`
//!
//! `#[serde(flatten)] extra: serde_json::Map<String, Value>` is the idiomatic serde way to
//! preserve unknown fields, but it only works at the level a struct declares it — an unknown key
//! *inside* a typed sub-struct (a nested object a caller didn't think to add a catch-all to) is
//! silently dropped, because `flatten` never sees inside a field it has already handed off to a
//! nested `Deserialize` impl. D-11.2 promises "unknown fields are preserved and written back" at
//! every nesting level, not only the top one, and this codebase's own `.nam` reader
//! (`namir_nam::file`) demonstrates the failure mode directly: it tolerates unknown fields by
//! simply not declaring them, which means it *drops* them, not preserves them — fine for a format
//! this project only ever reads, wrong for one it also writes back.
//!
//! So [`Document`] never discards the parsed [`serde_json::Value`] at all. It holds the whole
//! parsed object; [`crate::state::State`] (built on top, in `state.rs`) reads out the fields it
//! understands and, on save, overwrites only those specific keys in a clone of the original
//! object — every other key, at any depth, is untouched. This makes FR-STATE-010's round-trip
//! property (serialise, restore, serialise again, assert equality) hold by construction for the
//! *unknown* portion of a document, rather than by hoping every nested type remembered its own
//! catch-all.
//!
//! # Stable key ordering (D-11.1) is free
//!
//! This workspace never enables `serde_json`'s `preserve_order` feature (see
//! `namir-state/Cargo.toml`'s dependency comment), so [`serde_json::Map`] is backed by a
//! `BTreeMap` and every object serialises with its keys sorted, with zero effort from this crate.
//! `preserve_order` would need `indexmap` to preserve *insertion* order instead — order that is
//! not stable across code paths (different call sites build a document's fields in different
//! sequences), where sorted order is stable by construction. This is D-17.1's reasoning
//! ("adding a second parser to the fuzzing surface... is a poor trade") applied to a dependency
//! choice rather than a format choice: `preserve_order` would be strictly worse for FR-STATE-040's
//! diffability, not merely unnecessary.

use serde_json::{Map, Value};

use crate::error::StateError;
use crate::error_codes;

/// D-11.1's `format_version` field. `1` today; `crate::migrate` is the seam that lets this grow
/// without redesigning this module.
pub const FORMAT_VERSION: u64 = 1;

/// NFR-SEC-020: the documented upper bound on a state/preset document's raw byte size, checked
/// **before** any parsing is attempted — a document over this size is rejected without ever
/// building a `serde_json::Value` from it, so its size alone cannot be used to force a large
/// allocation. Deliberately the same figure as `namir_core::MAX_FILE_BYTES` (`namir-worker`'s
/// pre-M5 ceiling on a loaded model/IR file): one documented bound for "an untrusted file Namir
/// reads into memory in one piece", not a second number to keep in sync with the first. Set well
/// above what any real preset — even one embedding a large model per FR-STATE-080 — needs; a
/// document within this bound is a slow save, not a rejected one.
pub const MAX_DOCUMENT_BYTES: usize = namir_core::MAX_FILE_BYTES;

/// The parsed document, exactly as read, plus nothing. Always a JSON object — a state/preset
/// document that isn't a `{...}` at the top level is rejected at [`Document::parse`], not
/// represented here as some other shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    root: Map<String, Value>,
}

impl Document {
    /// Parses `bytes` into a `Document`. Checks [`MAX_DOCUMENT_BYTES`] before parsing (NFR-SEC-020)
    /// and requires the top-level JSON value to be an object (every Namir state/preset document
    /// is one; anything else is `error_codes::MALFORMED_JSON`, the same code a syntactically
    /// invalid document gets — a caller has no need to distinguish "not JSON" from "JSON but not
    /// shaped like a document").
    pub fn parse(bytes: &[u8]) -> Result<Document, StateError> {
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(StateError::new(
                error_codes::DOCUMENT_TOO_LARGE,
                format!(
                    "{} bytes, limit {} MB",
                    bytes.len(),
                    MAX_DOCUMENT_BYTES / (1024 * 1024)
                ),
            ));
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| StateError::new(error_codes::MALFORMED_JSON, e.to_string()))?;
        match value {
            Value::Object(root) => Ok(Document { root }),
            other => Err(StateError::new(
                error_codes::MALFORMED_JSON,
                format!(
                    "top-level value must be a JSON object, found {}",
                    json_type_name(&other)
                ),
            )),
        }
    }

    /// A document with nothing in it but [`FORMAT_VERSION`] — the starting point for a freshly
    /// created preset, before [`crate::state::State`] fills in its own sections.
    pub fn empty() -> Document {
        let mut root = Map::new();
        root.insert("format_version".to_string(), Value::from(FORMAT_VERSION));
        Document { root }
    }

    /// The document's `format_version` field, if present and representable as a `u64`. `None`
    /// covers both "the field is absent" and "the field is present but not an unsigned integer" —
    /// both are `error_codes::MISSING_FORMAT_VERSION` to a caller that requires one, per D-11.1's
    /// "the one field this format treats as non-negotiable" (see `error_codes.rs`).
    pub fn format_version(&self) -> Option<u64> {
        self.root.get("format_version")?.as_u64()
    }

    /// Pretty-printed, sorted-key bytes with a trailing newline (the conventional POSIX text-file
    /// ending, and what keeps a hand-edited-then-saved-again file from growing a spurious
    /// no-trailing-newline diff under most editors/VCS configurations — FR-STATE-040's
    /// diffability, extended past "the JSON is sorted" to "the file itself behaves like a normal
    /// text file").
    ///
    /// **Unchecked against [`MAX_DOCUMENT_BYTES`]** — a document carrying a large enough
    /// FR-STATE-080 embedded copy serialises to bytes [`Self::parse`] would then refuse with
    /// `DOCUMENT_TOO_LARGE`. Use [`Self::try_to_pretty_bytes`] (or
    /// [`State::try_write`](crate::State::try_write)) anywhere the bytes are destined for a file
    /// or a host's state blob, i.e. anywhere they will be read back; this infallible form is for
    /// a caller that has already established the document is small (a test, a freshly-defaulted
    /// state) and does not want a `Result` it cannot act on.
    pub fn to_pretty_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(&Value::Object(self.root.clone())).expect(
            "a Document's root is always a valid serde_json::Map and cannot fail to serialise",
        );
        bytes.push(b'\n');
        bytes
    }

    /// As [`Self::to_pretty_bytes`], but refuses to hand back bytes [`Self::parse`] would reject:
    /// over [`MAX_DOCUMENT_BYTES`] is `error_codes::DOCUMENT_TOO_LARGE`, the same code the read
    /// side uses for the same condition.
    ///
    /// This is the write half of NFR-SEC-020's ceiling, which the read half alone cannot keep:
    /// nothing else in this crate bounds how many bytes an [`EmbeddedRef`](crate::EmbeddedRef)
    /// contributes, so without this a save could produce a document that only fails on the *next*
    /// load — the point at which the user's own settings are already the thing being lost, and
    /// the least useful moment to discover it.
    pub fn try_to_pretty_bytes(&self) -> Result<Vec<u8>, StateError> {
        self.to_pretty_bytes_within(MAX_DOCUMENT_BYTES)
    }

    /// [`Self::try_to_pretty_bytes`] with the ceiling as a parameter — the seam its tests drive,
    /// so the enforcement is checked at a byte count a test can build in microseconds rather than
    /// only at the real 256 MiB figure. The public method is this one at [`MAX_DOCUMENT_BYTES`].
    pub(crate) fn to_pretty_bytes_within(&self, limit: usize) -> Result<Vec<u8>, StateError> {
        let bytes = self.to_pretty_bytes();
        if bytes.len() > limit {
            return Err(StateError::new(
                error_codes::DOCUMENT_TOO_LARGE,
                format!(
                    "{} bytes written, limit {} MB",
                    bytes.len(),
                    limit / (1024 * 1024)
                ),
            ));
        }
        Ok(bytes)
    }

    /// A named top-level section as an object, if present and shaped as one. `None` covers both
    /// "absent" and "present but not an object" — callers that need to tell those apart (none do,
    /// today: an absent section and a malformed one both mean "nothing usable is here") can add a
    /// method that does, but collapsing them keeps every call site's tolerant-loading logic
    /// (D-11.2) from having to handle a third case it would otherwise treat identically anyway.
    pub(crate) fn section(&self, key: &str) -> Option<&Map<String, Value>> {
        self.root.get(key)?.as_object()
    }

    /// Replaces (or creates) a named top-level section wholesale, discarding whatever was there
    /// before. Correct only when there is nothing worth keeping — building a section from a
    /// document that started empty ([`Self::empty`], [`State::into_document`](crate::State)) is
    /// the one legitimate use; anywhere a section might already carry keys this build doesn't
    /// recognise (the load-modify-save path), [`Self::merge_section`] is the one that actually
    /// keeps D-11.2's promise, not this one.
    pub(crate) fn set_section(&mut self, key: &str, value: Map<String, Value>) {
        self.root.insert(key.to_string(), Value::Object(value));
    }

    /// Inserts every key of `additions` into the named section, **preserving any key already
    /// there that `additions` doesn't mention** — creating the section if it didn't exist. This
    /// is D-11.2's write-back promise applied one level deeper than [`Self::set_section`]: a
    /// section this build owns (`parameters`, and later `global`/`references`/`meta`) can still
    /// carry keys it doesn't recognise (an unrecognised parameter, a field a newer Namir added),
    /// and those must survive a save that only touches a *different*, recognised key in the same
    /// section — which `set_section`, replacing the section wholesale, cannot do.
    pub(crate) fn merge_section(&mut self, key: &str, additions: Map<String, Value>) {
        let mut merged = self.section(key).cloned().unwrap_or_default();
        merged.extend(additions);
        self.set_section(key, merged);
    }

    /// Deletes one key from a named section, leaving every other key in that section — and the
    /// section itself, even if this empties it — alone. A no-op if either the section or the key
    /// is absent, or if the section isn't an object.
    ///
    /// [`Self::merge_section`] can only ever *add* a key, which makes it unable to express "this
    /// slot is now empty" for a section whose keys are themselves the state (`references`, whose
    /// two keys are optional by §7 of `docs/04-state-and-preset-format.md`: "absent means nothing
    /// of that kind is loaded"). This is the counterpart that can. It is deliberately per-key
    /// rather than per-section, so D-11.2's promise still holds around it: an unrecognised key
    /// sitting alongside `nam`/`ir` inside `references` survives a save that clears one of them.
    pub(crate) fn remove_from_section(&mut self, key: &str, field: &str) {
        if let Some(Value::Object(section)) = self.root.get_mut(key) {
            section.remove(field);
        }
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_document_carries_only_format_version() {
        let doc = Document::empty();
        assert_eq!(doc.format_version(), Some(FORMAT_VERSION));
    }

    /// FR-STATE-010's literal *Verify*: serialise, restore, serialise again, assert equality.
    /// At the `Document` level (component evidence; `state.rs`'s tests do the same at the full
    /// `State` level, which is the authoritative claim for the requirement).
    #[test]
    fn round_trips_through_parse_and_to_pretty_bytes() {
        let doc = Document::empty();
        let bytes = doc.to_pretty_bytes();
        let restored = Document::parse(&bytes).unwrap();
        let bytes_again = restored.to_pretty_bytes();
        assert_eq!(bytes, bytes_again);
    }

    /// D-11.1's stable key ordering: unrelated to insertion order, purely alphabetical, because
    /// `serde_json::Map` is a `BTreeMap` in this workspace's build (no `preserve_order` feature
    /// anywhere in the dependency graph — see this module's doc comment).
    #[test]
    fn keys_serialise_in_sorted_not_insertion_order() {
        let mut doc = Document::empty();
        let mut zeta = Map::new();
        zeta.insert("x".to_string(), Value::from(1));
        doc.set_section("zeta_section", zeta);
        let mut alpha = Map::new();
        alpha.insert("y".to_string(), Value::from(2));
        doc.set_section("alpha_section", alpha);

        let text = String::from_utf8(doc.to_pretty_bytes()).unwrap();
        let alpha_pos = text.find("alpha_section").unwrap();
        let zeta_pos = text.find("zeta_section").unwrap();
        let format_version_pos = text.find("format_version").unwrap();
        assert!(
            alpha_pos < format_version_pos,
            "\"alpha_section\" sorts before \"format_version\""
        );
        assert!(
            format_version_pos < zeta_pos,
            "\"format_version\" sorts before \"zeta_section\""
        );
    }

    #[test]
    fn pretty_bytes_end_with_a_single_trailing_newline() {
        let bytes = Document::empty().to_pretty_bytes();
        assert!(bytes.ends_with(b"\n"));
        assert!(!bytes.ends_with(b"\n\n"));
    }

    // The state/preset kind of the four `namir-core`'s limits doc comment enumerates. M14 closed
    // the one that had no artifact -- the `.nam`/IR disk-load ceiling, now driven by
    // `namir-worker`'s `a_file_over_the_ceiling_is_refused_before_its_bytes_are_read` -- so all
    // four are refused by a test: this one, that one, `namir-ir`'s `wav.rs` MAX_LOAD_SECONDS
    // ceiling and `namir-library`'s `scan.rs` MAX_INDEXED_FILE_BYTES.
    // trace: NFR-SEC-020
    #[test]
    fn rejects_documents_over_the_size_ceiling() {
        // A byte slice over the ceiling never even reaches the JSON parser -- constructing it
        // as a run of spaces (trivially cheap to allocate for the test) outside a string/array
        // context would itself be invalid JSON, which is exactly the point: size is checked
        // first, so this is rejected for its size, not incidentally for being malformed JSON.
        let oversized = vec![b' '; MAX_DOCUMENT_BYTES + 1];
        let err = Document::parse(&oversized).unwrap_err();
        assert_eq!(err.code.id, error_codes::DOCUMENT_TOO_LARGE.id);
    }

    #[test]
    fn rejects_malformed_json() {
        let err = Document::parse(b"{not valid json").unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }

    #[test]
    fn rejects_a_top_level_value_that_is_not_an_object() {
        let err = Document::parse(b"[1, 2, 3]").unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }

    #[test]
    fn format_version_is_none_when_absent() {
        let doc = Document::parse(b"{}").unwrap();
        assert_eq!(doc.format_version(), None);
    }

    #[test]
    fn format_version_is_none_when_not_an_integer() {
        let doc = Document::parse(br#"{"format_version": "one"}"#).unwrap();
        assert_eq!(doc.format_version(), None);
    }

    #[test]
    fn section_returns_none_for_an_absent_key() {
        let doc = Document::empty();
        assert!(doc.section("parameters").is_none());
    }

    #[test]
    fn section_returns_none_when_the_key_is_not_an_object() {
        let doc = Document::parse(br#"{"parameters": "not an object"}"#).unwrap();
        assert!(doc.section("parameters").is_none());
    }

    #[test]
    fn set_section_is_read_back_by_section() {
        let mut doc = Document::empty();
        let mut params = Map::new();
        params.insert("trim.gain_db".to_string(), Value::from(2.5));
        doc.set_section("parameters", params.clone());
        assert_eq!(doc.section("parameters"), Some(&params));
    }

    #[test]
    fn merge_section_creates_the_section_when_absent() {
        let mut doc = Document::empty();
        let mut additions = Map::new();
        additions.insert("a".to_string(), Value::from(1));
        doc.merge_section("parameters", additions.clone());
        assert_eq!(doc.section("parameters"), Some(&additions));
    }

    #[test]
    fn merge_section_preserves_keys_it_does_not_mention() {
        let mut doc = Document::empty();
        let mut original = Map::new();
        original.insert("kept".to_string(), Value::from("stays"));
        original.insert("overwritten".to_string(), Value::from("old"));
        doc.set_section("parameters", original);

        let mut additions = Map::new();
        additions.insert("overwritten".to_string(), Value::from("new"));
        doc.merge_section("parameters", additions);

        let merged = doc.section("parameters").unwrap();
        assert_eq!(merged.get("kept"), Some(&Value::from("stays")));
        assert_eq!(merged.get("overwritten"), Some(&Value::from("new")));
    }

    #[test]
    fn remove_from_section_deletes_only_the_named_key() {
        let mut doc = Document::empty();
        let mut references = Map::new();
        references.insert("nam".to_string(), Value::from("the model"));
        references.insert("ir".to_string(), Value::from("the cab"));
        doc.set_section("references", references);

        doc.remove_from_section("references", "nam");

        let section = doc.section("references").unwrap();
        assert!(!section.contains_key("nam"));
        assert_eq!(section.get("ir"), Some(&Value::from("the cab")));
    }

    #[test]
    fn remove_from_section_is_a_no_op_for_an_absent_section_or_key() {
        let mut doc = Document::empty();
        doc.remove_from_section("references", "nam"); // no such section
        assert!(doc.section("references").is_none());

        doc.set_section("references", Map::new());
        doc.remove_from_section("references", "nam"); // no such key
        assert_eq!(doc.section("references"), Some(&Map::new()));
    }

    /// Issue #115's write half: the byte ceiling NFR-SEC-020 states is enforced on the way *out*
    /// too, not only on the way in — otherwise a save can produce a document whose only failure
    /// mode is the next load. Driven through the limit-taking seam so the property is checked at
    /// a size a test can build instantly; `try_to_pretty_bytes` is this same code path at
    /// `MAX_DOCUMENT_BYTES`.
    #[test]
    fn to_pretty_bytes_within_refuses_a_document_over_the_limit() {
        let mut doc = Document::empty();
        let mut params = Map::new();
        params.insert("trim.gain_db".to_string(), Value::from(1.0));
        doc.set_section("parameters", params);

        let unlimited = doc.to_pretty_bytes();
        assert!(doc.to_pretty_bytes_within(unlimited.len()).is_ok());

        let err = doc.to_pretty_bytes_within(unlimited.len() - 1).unwrap_err();
        assert_eq!(err.code.id, error_codes::DOCUMENT_TOO_LARGE.id);
    }

    /// The ceiling the public method actually applies is the one `parse` enforces, so a document
    /// that survives `try_to_pretty_bytes` is by construction one `parse` accepts.
    #[test]
    fn try_to_pretty_bytes_returns_the_same_bytes_for_a_document_within_the_ceiling() {
        let doc = Document::empty();
        assert_eq!(doc.try_to_pretty_bytes().unwrap(), doc.to_pretty_bytes());
        assert!(Document::parse(&doc.try_to_pretty_bytes().unwrap()).is_ok());
    }

    // -----------------------------------------------------------------------------------
    // NFR-PORT-050: "byte order, path separators, line endings and text encoding shall be
    // handled such that preset and state files written on one platform load identically on
    // another." The path-separator half is `reference.rs`'s `RelPath`'s job, added later in
    // this milestone; the invariants below are the half this module owns: the byte shape of
    // the document itself, independent of anything it stores.
    // -----------------------------------------------------------------------------------

    // trace: NFR-PORT-050
    #[test]
    fn written_bytes_never_contain_a_carriage_return() {
        // A regression that started emitting CRLF would previously have been silently repaired
        // by this repository's own `* text=auto eol=lf` .gitattributes rule on commit -- masking
        // exactly the bug this test exists to catch. That gap is why `*.namirpreset` is now
        // listed `binary` in `.gitattributes` (D-2.5/M5's correction) rather than left to text
        // normalisation, and why this assertion runs directly on the writer's raw output bytes
        // rather than on whatever Git decided to store.
        let bytes = Document::empty().to_pretty_bytes();
        assert!(
            !bytes.contains(&b'\r'),
            "output must be LF-only, found a CR byte"
        );
    }

    // trace: NFR-PORT-050
    #[test]
    fn written_bytes_carry_no_byte_order_mark() {
        const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
        let bytes = Document::empty().to_pretty_bytes();
        assert!(
            !bytes.starts_with(&UTF8_BOM),
            "output must not carry a UTF-8 BOM"
        );
    }

    // trace: NFR-PORT-050
    #[test]
    fn written_bytes_are_valid_utf8() {
        let bytes = Document::empty().to_pretty_bytes();
        assert!(std::str::from_utf8(&bytes).is_ok());
    }

    /// Canonical float formatting: `serde_json`'s number writer always uses `.` as the decimal
    /// separator regardless of host locale (JSON's own grammar has no other option — this is a
    /// property of the format, not of this crate's code — but it is exactly the kind of "surely
    /// that's fine" assumption NFR-PORT-050 asks to have verified rather than trusted).
    #[test]
    fn numbers_use_a_period_as_the_decimal_separator() {
        let mut doc = Document::empty();
        let mut params = Map::new();
        params.insert("trim.gain_db".to_string(), Value::from(2.5_f64));
        doc.set_section("parameters", params);
        let text = String::from_utf8(doc.to_pretty_bytes()).unwrap();
        assert!(text.contains("2.5"));
        assert!(!text.contains("2,5"));
    }
}
