//! [`State`]: the typed projection over a [`Document`] — what FR-STATE-010's "complete
//! user-settable state" round-trips through. Four sections: `parameters`
//! ([`ParamValues`]/`REGISTRY`), `global` (bypass, output ceiling — see [`crate::global`]'s
//! module doc comment for why these need a section of their own), and `references.nam` /
//! `references.ir` ([`FileRef`], D-11.3).
//!
//! # Why a typed projection over a carrier, not a plain `#[derive(Deserialize)]` struct
//!
//! See [`crate::document`]'s module doc comment for the full argument; the short version is that
//! `Document` never discards the parsed JSON object, so `State::write_onto` can overwrite exactly
//! the sections this crate understands and leave everything else in the original document
//! untouched, at any nesting depth — which is D-11.2's write-back promise, not merely its
//! top-level approximation.

use serde_json::{Map, Value};

use crate::document::Document;
use crate::error::{StateError, StateWarning};
use crate::global::Global;
use crate::migrate;
use crate::params::ParamValues;
use crate::reference::FileRef;

/// The typed view of a state/preset document this build understands.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// Every `namir_params::REGISTRY` entry's current value.
    pub params: ParamValues,
    /// FR-CHAIN-030/090's bypass and output ceiling — not a `ParamDescriptor`; see
    /// [`crate::global`]'s module doc comment.
    pub global: Global,
    /// FR-STATE-070's reference to the loaded NAM model, if any.
    pub nam: Option<FileRef>,
    /// FR-STATE-070's reference to the loaded IR, if any.
    pub ir: Option<FileRef>,
}

impl State {
    /// A freshly created state: every parameter at its documented default (FR-STATE-020), global
    /// bypass off, output ceiling at 0 dB, no model or IR referenced.
    pub fn defaults() -> Self {
        Self {
            params: ParamValues::defaults(),
            global: Global::defaults(),
            nam: None,
            ir: None,
        }
    }

    /// Parses `bytes` into a `State`: [`Document::parse`], then [`crate::migrate::migrate`]
    /// (D-11.2's `format_version` gate), then [`Self::from_document`]. Collects every warning
    /// the pipeline produces — a document with, say, one unrecognised parameter and a newer
    /// `format_version` still loads, with both facts reported rather than hidden or treated as
    /// fatal.
    pub fn read(bytes: &[u8]) -> Result<(State, Vec<StateWarning>), StateError> {
        let document = Document::parse(bytes)?;
        let (document, mut warnings) = migrate::migrate(document)?;
        let (state, more) = Self::from_document(document);
        warnings.extend(more);
        Ok((state, warnings))
    }

    /// As [`Self::read`], but from an already-migrated [`Document`] — the projection step alone,
    /// with no `format_version` check. Used directly by tests that don't need a real
    /// `format_version` gate, and by [`Self::read`] itself once migration has run.
    pub fn from_document(document: Document) -> (State, Vec<StateWarning>) {
        let mut warnings = Vec::new();

        let params = match document.section("parameters") {
            Some(section) => {
                let (params, param_warnings) = ParamValues::from_document_section(section);
                warnings.extend(param_warnings);
                params
            }
            None => ParamValues::defaults(),
        };

        let global = document
            .section("global")
            .map(Global::from_value)
            .unwrap_or_else(Global::defaults);

        let (nam, ir) = match document.section("references") {
            Some(section) => {
                let nam = read_reference(section, "nam", &mut warnings);
                let ir = read_reference(section, "ir", &mut warnings);
                (nam, ir)
            }
            None => (None, None),
        };

        (
            State {
                params,
                global,
                nam,
                ir,
            },
            warnings,
        )
    }

    /// Serialises this state back into pretty-printed, sorted-key bytes (D-11.1). Equivalent to
    /// [`Self::into_document`] followed by [`Document::to_pretty_bytes`], for a caller with no
    /// reason to keep the intermediate `Document` around and nothing to preserve from an
    /// existing one — most usefully, creating a brand-new preset from scratch.
    pub fn write(&self) -> Vec<u8> {
        self.clone().into_document().to_pretty_bytes()
    }

    /// Builds a fresh [`Document`] from this state — every section this crate owns, sorted,
    /// nothing else. Used by [`Self::write`] directly.
    pub fn into_document(self) -> Document {
        let mut document = Document::empty();
        document.set_section("parameters", self.params.to_document_section());
        document.set_section("global", self.global.to_value());
        document.set_section("references", references_section(&self.nam, &self.ir));
        document
    }

    /// Merges this state's known sections onto a clone of `onto`, leaving every other key in
    /// `onto` — at any nesting depth — untouched. This is D-11.2's actual write-back mechanism:
    /// [`Self::into_document`] alone would build a document from scratch and lose whatever
    /// `onto` carried that this build doesn't understand, and `set_section` alone would lose an
    /// unrecognised key *inside* a section this build does own (see
    /// [`Document::merge_section`]'s doc comment) — this is the method that keeps both promises
    /// at once. The one exception is `references.nam`/`references.ir`'s own internal shape — see
    /// [`FileRef`]'s doc comment on why an unrecognised field *inside* a single reference object
    /// is not yet preserved.
    pub fn write_onto(&self, onto: &Document) -> Document {
        let mut document = onto.clone();
        document.merge_section("parameters", self.params.to_document_section());
        document.merge_section("global", self.global.to_value());
        document.merge_section("references", references_section(&self.nam, &self.ir));
        document
    }
}

fn references_section(nam: &Option<FileRef>, ir: &Option<FileRef>) -> Map<String, Value> {
    let mut obj = Map::new();
    if let Some(r) = nam {
        obj.insert("nam".to_string(), r.to_value());
    }
    if let Some(r) = ir {
        obj.insert("ir".to_string(), r.to_value());
    }
    obj
}

/// Reads one of `references`' two slots. A malformed reference (unparseable hash, wrong shape)
/// degrades to "absent" with a warning rather than failing the whole document — P8 ("failure
/// degrades; it does not propagate") applied to a single field, the same tolerance an
/// unrecognised parameter key gets.
fn read_reference(
    references: &Map<String, Value>,
    key: &str,
    warnings: &mut Vec<StateWarning>,
) -> Option<FileRef> {
    let value = references.get(key)?;
    match FileRef::from_value(value) {
        Ok(reference) => Some(reference),
        Err(e) => {
            warnings.push(StateWarning::new(e.code, e.detail));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::ContentHash;

    fn a_reference(name: &str) -> FileRef {
        FileRef {
            hash: ContentHash::of(name.as_bytes()),
            library_relative: None,
            absolute: None,
            display_name: name.to_string(),
            embedded: None,
        }
    }

    /// FR-STATE-010's literal *Verify*, at the `State` level — the authoritative form of this
    /// property for this crate (see `document.rs`'s own version of this test for the
    /// component-level evidence). Exercises every section this crate owns, not just parameters.
    #[test]
    fn round_trips_serialise_restore_serialise() {
        let mut state = State::defaults();
        state.params.set("trim.gain_db", 3.5).unwrap();
        state.params.set("eq.mid_q", 1.1).unwrap();
        state.global.bypass = true;
        state.global.output_ceiling_db = -3.0;
        state.nam = Some(a_reference("plexi.nam"));
        state.ir = Some(a_reference("1960a.wav"));

        let bytes = state.write();
        let (restored, warnings) = State::read(&bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restored, state);

        let bytes_again = restored.write();
        assert_eq!(bytes, bytes_again);
    }

    /// FR-STATE-020: a parameter absent from the document takes its documented default; the same
    /// applies to every other section.
    #[test]
    fn a_document_with_no_sections_restores_all_defaults() {
        let (state, warnings) = State::from_document(Document::empty());
        assert!(warnings.is_empty());
        assert_eq!(state, State::defaults());
    }

    /// D-11.2's write-back promise, exercised through `State` rather than `ParamValues` directly:
    /// a section this build does not own is preserved by `write_onto`, verbatim, alongside the
    /// sections it does own being genuinely updated.
    #[test]
    fn write_onto_preserves_an_unrelated_section_while_updating_parameters() {
        let mut original = Document::empty();
        let mut host_section = Map::new();
        host_section.insert("plugin_id".to_string(), Value::from("com.example.foo"));
        original.set_section("host", host_section.clone());

        let mut state = State::defaults();
        state.params.set("out.gain_db", -6.0).unwrap();
        let saved = state.write_onto(&original);

        assert_eq!(saved.section("host"), Some(&host_section));
        let (restored, _) = State::from_document(saved);
        assert_eq!(restored.params.get("out.gain_db"), Some(-6.0));
    }

    /// D-11.2's write-back promise at the depth that actually matters: an unknown key **inside**
    /// a section this build *does* own must still survive a load-modify-save round trip. This is
    /// the case a `#[serde(flatten)]`-based design cannot satisfy at all (see `document.rs`'s
    /// module doc comment) and the previous test doesn't reach, since "host" is a section this
    /// build never touches — "parameters" is a section it actively rewrites, which is exactly
    /// where a naive rewrite-from-scratch would drop anything it doesn't recognise.
    #[test]
    fn write_onto_preserves_an_unknown_key_inside_the_parameters_section_it_owns() {
        let mut original = Document::empty();
        let mut params_section = Map::new();
        params_section.insert("comp.ratio".to_string(), Value::from(4.0));
        params_section.insert("trim.gain_db".to_string(), Value::from(1.0));
        original.set_section("parameters", params_section);

        let (mut state, warnings) = State::from_document(original.clone());
        assert_eq!(warnings.len(), 1, "comp.ratio is not a REGISTRY key");
        state.params.set("trim.gain_db", 2.0).unwrap(); // modify a *known* field
        let saved = state.write_onto(&original);

        let saved_params = saved.section("parameters").unwrap();
        assert_eq!(
            saved_params.get("comp.ratio"),
            Some(&Value::from(4.0)),
            "an unrecognised parameter key must survive a save that touches a *different*, \
             recognised key in the same section"
        );
        assert_eq!(
            saved_params.get("trim.gain_db"),
            Some(&Value::from(2.0)),
            "the field actually modified must reflect the new value"
        );
    }

    #[test]
    fn nam_and_ir_references_round_trip() {
        let mut state = State::defaults();
        state.nam = Some(a_reference("plexi.nam"));
        let (restored, warnings) = State::from_document(state.clone().into_document());
        assert!(warnings.is_empty());
        assert_eq!(restored.nam, state.nam);
        assert_eq!(restored.ir, None);
    }

    #[test]
    fn a_malformed_reference_degrades_to_absent_with_a_warning() {
        let mut document = Document::empty();
        let mut references = Map::new();
        // Missing "hash" entirely -- FileRef::from_value's MALFORMED_JSON case.
        let mut broken = Map::new();
        broken.insert("display_name".to_string(), Value::from("broken.nam"));
        references.insert("nam".to_string(), Value::Object(broken));
        document.set_section("references", references);

        let (state, warnings) = State::from_document(document);
        assert_eq!(state.nam, None);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.id, crate::error_codes::MALFORMED_JSON.id);
    }

    #[test]
    fn global_round_trips() {
        let mut state = State::defaults();
        state.global.bypass = true;
        state.global.output_ceiling_db = -12.0;
        let (restored, warnings) = State::from_document(state.clone().into_document());
        assert!(warnings.is_empty());
        assert_eq!(restored.global, state.global);
    }
}
