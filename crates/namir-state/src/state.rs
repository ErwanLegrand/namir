//! [`State`]: the typed projection over a [`Document`] — what FR-STATE-010's "complete
//! user-settable state" round-trips through. Landing incrementally: this version covers the
//! parameter block only ([`ParamValues`]); file references and the `global` block (bypass,
//! output ceiling) join it later in the same milestone, in the same struct, as this crate's
//! `reference`/`resolve` modules land — see `lib.rs`'s "Scope" section for the running tally.
//!
//! # Why a typed projection over a carrier, not a plain `#[derive(Deserialize)]` struct
//!
//! See [`crate::document`]'s module doc comment for the full argument; the short version is that
//! `Document` never discards the parsed JSON object, so `State::write` can overwrite exactly the
//! sections this crate understands (today: `parameters`) and leave everything else in the
//! original document untouched, at any nesting depth — which is D-11.2's write-back promise, not
//! merely its top-level approximation.

use crate::document::Document;
use crate::error::{StateError, StateWarning};
use crate::params::ParamValues;

/// The typed view of a state/preset document this build understands.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// Every `namir_params::REGISTRY` entry's current value.
    pub params: ParamValues,
}

impl State {
    /// A freshly created state: every parameter at its documented default (FR-STATE-020).
    pub fn defaults() -> Self {
        Self {
            params: ParamValues::defaults(),
        }
    }

    /// Parses `bytes` into a `State`, applying D-11.2's tolerant rules and collecting whatever
    /// [`StateWarning`]s they produce — a document with, say, one unrecognised parameter still
    /// loads, with that fact reported rather than hidden or treated as fatal.
    pub fn read(bytes: &[u8]) -> Result<(State, Vec<StateWarning>), StateError> {
        let document = Document::parse(bytes)?;
        Ok(Self::from_document(document))
    }

    /// As [`Self::read`], but from an already-parsed [`Document`] — the entry point
    /// `crate::migrate` will sit in front of once it exists (parse → migrate → project), so this
    /// method, not [`Self::read`], is the one that will grow a `format_version` argument later in
    /// this milestone rather than `read` needing to change shape twice.
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
        (State { params }, warnings)
    }

    /// Serialises this state back into pretty-printed, sorted-key bytes (D-11.1). Equivalent to
    /// [`Self::into_document`] followed by [`Document::to_pretty_bytes`], provided for callers
    /// that have no reason to keep the intermediate `Document` around.
    pub fn write(&self) -> Vec<u8> {
        self.clone().into_document().to_pretty_bytes()
    }

    /// Builds a fresh [`Document`] from this state — every section this crate owns, sorted,
    /// nothing else. Used by [`Self::write`] directly; exposed separately for a caller (`state.rs`
    /// itself, once file references land) that wants to merge onto an *existing* document's
    /// unknown sections rather than starting from an empty one — see this module's doc comment on
    /// why that distinction matters for D-11.2.
    pub fn into_document(self) -> Document {
        let mut document = Document::empty();
        document.set_section("parameters", self.params.to_document_section());
        document
    }

    /// Merges this state's known sections onto a clone of `onto`, leaving every other key in
    /// `onto` — at any nesting depth — untouched. This is D-11.2's actual write-back mechanism:
    /// [`Self::into_document`] alone would build a document from scratch and lose whatever
    /// `onto` carried that this build doesn't understand.
    pub fn write_onto(&self, onto: &Document) -> Document {
        let mut document = onto.clone();
        document.set_section("parameters", self.params.to_document_section());
        document
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-STATE-010's literal *Verify*, at the `State` level — the authoritative form of this
    /// property for this crate (see `document.rs`'s own version of this test for the
    /// component-level evidence).
    #[test]
    fn round_trips_serialise_restore_serialise() {
        let mut state = State::defaults();
        state.params.set("trim.gain_db", 3.5).unwrap();
        state.params.set("eq.mid_q", 1.1).unwrap();

        let bytes = state.write();
        let (restored, warnings) = State::read(&bytes).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(restored, state);

        let bytes_again = restored.write();
        assert_eq!(bytes, bytes_again);
    }

    /// FR-STATE-020: a parameter absent from the document takes its documented default.
    #[test]
    fn a_document_with_no_parameters_section_restores_all_defaults() {
        let (state, warnings) = State::from_document(Document::empty());
        assert!(warnings.is_empty());
        assert_eq!(state, State::defaults());
    }

    /// D-11.2's write-back promise, exercised through `State` rather than `ParamValues` directly:
    /// a section this build does not own is preserved by `write_onto`, verbatim, alongside the
    /// section it does own being genuinely updated.
    #[test]
    fn write_onto_preserves_an_unrelated_section_while_updating_parameters() {
        let mut original = Document::empty();
        let mut host_section = serde_json::Map::new();
        host_section.insert(
            "plugin_id".to_string(),
            serde_json::Value::from("com.example.foo"),
        );
        original.set_section("host", host_section.clone());

        let mut state = State::defaults();
        state.params.set("out.gain_db", -6.0).unwrap();
        let saved = state.write_onto(&original);

        assert_eq!(saved.section("host"), Some(&host_section));
        let (restored, _) = State::from_document(saved);
        assert_eq!(restored.params.get("out.gain_db"), Some(-6.0));
    }
}
