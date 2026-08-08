//! [`State`]: the typed projection over a [`Document`] — what FR-STATE-010's "complete
//! user-settable state" round-trips through. Two sections: `parameters` ([`ParamValues`]/
//! `REGISTRY`, which since D-10.4 includes `global.bypass`/`global.output_ceiling_db` alongside
//! every stage's own parameters) and `references.nam` / `references.ir` ([`FileRef`], D-11.3).
//!
//! # D-10.4: `global` is no longer a section of its own
//!
//! Before D-10.4, FR-CHAIN-030's bypass and FR-CHAIN-090's output ceiling had no
//! `ParamDescriptor` at all, so this crate carried them in a second, parallel `global` document
//! section backed by nothing but a plain struct (`docs/02-architecture.md`'s D-10.3 consequence
//! note; the type used to live here as `crate::global::Global`). Now that
//! `namir_params::global::GLOBAL_BYPASS`/`OUTPUT_CEILING_DB` are ordinary `REGISTRY` entries, both
//! values are ordinary [`ParamValues`] entries too — `State` has no `global` field, and
//! [`Self::global_bypass`]/[`Self::output_ceiling_db`] (and their `set_*` counterparts) are thin
//! convenience accessors over `self.params`, not a second source of truth.
//!
//! [`Self::from_document`] still *reads* the old `global` section, as a fallback for whichever of
//! the two keys `parameters` doesn't itself carry (D-11.2: "a project saved by a newer Namir and
//! opened by an older one does not silently lose settings" applied to this crate's own past
//! format, not just a hypothetical future one). Every write goes through [`Self::into_document`]/
//! [`Self::write_onto`], neither of which ever produces a `global` section again — see those
//! methods' own doc comments.
//!
//! # Why a typed projection over a carrier, not a plain `#[derive(Deserialize)]` struct
//!
//! See [`crate::document`]'s module doc comment for the full argument; the short version is that
//! `Document` never discards the parsed JSON object, so `State::write_onto` can overwrite exactly
//! the sections this crate understands and leave everything else in the original document
//! untouched, at any nesting depth — which is D-11.2's write-back promise, not merely its
//! top-level approximation.

use namir_params::global::{GLOBAL_BYPASS, OUTPUT_CEILING_DB};
use serde_json::{Map, Value};

use crate::document::Document;
use crate::error::{StateError, StateWarning};
use crate::migrate;
use crate::params::ParamValues;
use crate::reference::FileRef;

/// The typed view of a state/preset document this build understands.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// Every `namir_params::REGISTRY` entry's current value — since D-10.4, this includes
    /// `global.bypass`/`global.output_ceiling_db` (see [`Self::global_bypass`]/
    /// [`Self::output_ceiling_db`]) alongside every stage's own parameters.
    pub params: ParamValues,
    /// FR-STATE-070's reference to the loaded NAM model, if any.
    pub nam: Option<FileRef>,
    /// FR-STATE-070's reference to the loaded IR, if any.
    pub ir: Option<FileRef>,
}

impl State {
    /// A freshly created state: every parameter at its documented default (FR-STATE-020) —
    /// global bypass off, output ceiling at 0 dB included, since both are `REGISTRY` entries — no
    /// model or IR referenced.
    pub fn defaults() -> Self {
        Self {
            params: ParamValues::defaults(),
            nam: None,
            ir: None,
        }
    }

    /// FR-CHAIN-030's chain-wide bypass, read from `self.params`'s `global.bypass` entry (a
    /// `namir_params::ParamKind::Stepped` value: `>= 0.5` is "On", the same convention every
    /// stepped parameter uses at the `namir-engine` boundary).
    pub fn global_bypass(&self) -> bool {
        self.params.get(GLOBAL_BYPASS.key).is_some_and(|v| v >= 0.5)
    }

    /// Sets `self.params`'s `global.bypass` entry. Infallible — `global.bypass` is always a
    /// `REGISTRY` entry — unlike [`ParamValues::set`]'s own `Result`, which exists for a caller
    /// that has to handle an arbitrary, possibly-unknown key.
    pub fn set_global_bypass(&mut self, enabled: bool) {
        self.params
            .set(GLOBAL_BYPASS.key, if enabled { 1.0 } else { 0.0 })
            .expect("global.bypass is always a REGISTRY entry");
    }

    /// FR-CHAIN-090's output ceiling in dB, read from `self.params`'s `global.output_ceiling_db`
    /// entry.
    pub fn output_ceiling_db(&self) -> f32 {
        self.params.get(OUTPUT_CEILING_DB.key).unwrap_or(0.0)
    }

    /// Sets `self.params`'s `global.output_ceiling_db` entry (clamped to that descriptor's range,
    /// exactly as [`ParamValues::set`] clamps any other continuous parameter). Infallible for the
    /// same reason [`Self::set_global_bypass`] is.
    pub fn set_output_ceiling_db(&mut self, db: f32) {
        self.params
            .set(OUTPUT_CEILING_DB.key, db)
            .expect("global.output_ceiling_db is always a REGISTRY entry");
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

        let params_section = document.section("parameters");
        let mut params = match params_section {
            Some(section) => {
                let (params, param_warnings) = ParamValues::from_document_section(section);
                warnings.extend(param_warnings);
                params
            }
            None => ParamValues::defaults(),
        };

        // D-10.4: a document written before this decision carries `global.bypass`/
        // `global.output_ceiling_db` in a separate, now-retired `global` section instead of as
        // `parameters` entries -- read that shape as a fallback, but only for whichever of the
        // two new keys `parameters` doesn't itself already carry. `parameters` (this build's own
        // current shape) always wins when both are present, so a document this build's own
        // writer already produced is never second-guessed by a stray legacy section a
        // hand-editor left behind (see this module's own doc comment for why a legacy `global`
        // section is otherwise left untouched by a write, not deleted).
        if let Some(legacy) = document.section("global") {
            let already_has_bypass =
                params_section.is_some_and(|s| s.contains_key(GLOBAL_BYPASS.key));
            let already_has_ceiling =
                params_section.is_some_and(|s| s.contains_key(OUTPUT_CEILING_DB.key));
            if !already_has_bypass
                && let Some(bypass) = legacy.get("bypass").and_then(Value::as_bool)
            {
                params
                    .set(GLOBAL_BYPASS.key, if bypass { 1.0 } else { 0.0 })
                    .expect("global.bypass is always a REGISTRY entry");
            }
            if !already_has_ceiling
                && let Some(ceiling) = legacy
                    .get("output_ceiling_db")
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite())
            {
                params
                    .set(OUTPUT_CEILING_DB.key, ceiling as f32)
                    .expect("global.output_ceiling_db is always a REGISTRY entry");
            }
        }

        let (nam, ir) = match document.section("references") {
            Some(section) => {
                let nam = read_reference(section, "nam", &mut warnings);
                let ir = read_reference(section, "ir", &mut warnings);
                (nam, ir)
            }
            None => (None, None),
        };

        (State { params, nam, ir }, warnings)
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
    ///
    /// **D-10.4:** never produces a `global` section — `global.bypass`/`global.output_ceiling_db`
    /// are already inside `self.params.to_document_section()`, since both are `REGISTRY` entries.
    pub fn into_document(self) -> Document {
        let mut document = Document::empty();
        document.set_section("parameters", self.params.to_document_section());
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
    ///
    /// **D-10.4:** if `onto` carries a legacy `global` section (D-11.2 tolerance: this build can
    /// still have read one, via [`Self::from_document`]), it is left exactly as it is here — the
    /// same treatment any other section this build no longer owns gets. It becomes inert rather
    /// than actively wrong the moment this call also writes `parameters.global.bypass`/
    /// `global.output_ceiling_db`: [`Self::from_document`] always prefers the `parameters` shape
    /// over the legacy one once both are present, so the stale section is simply never read
    /// again. Deleting it outright was considered and rejected — it would make `write_onto`
    /// special-case one specific section name instead of treating "unrecognised" uniformly,
    /// which is exactly the discipline D-11.2's write-back promise depends on.
    pub fn write_onto(&self, onto: &Document) -> Document {
        let mut document = onto.clone();
        document.merge_section("parameters", self.params.to_document_section());
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
    // trace: FR-STATE-010
    #[test]
    fn round_trips_serialise_restore_serialise() {
        let mut state = State::defaults();
        state.params.set("trim.gain_db", 3.5).unwrap();
        state.params.set("eq.mid_q", 1.1).unwrap();
        state.set_global_bypass(true);
        state.set_output_ceiling_db(-3.0);
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
        state.set_global_bypass(true);
        state.set_output_ceiling_db(-12.0);
        let (restored, warnings) = State::from_document(state.clone().into_document());
        assert!(warnings.is_empty());
        assert_eq!(restored.global_bypass(), state.global_bypass());
        assert_eq!(restored.output_ceiling_db(), state.output_ceiling_db());
    }

    /// D-10.4: `global.bypass`/`global.output_ceiling_db` are `REGISTRY` entries now, so they
    /// live inside `parameters` like any other parameter -- there is no separate `global` section
    /// in a document this build's own writer produces.
    #[test]
    fn into_document_never_produces_a_global_section() {
        let mut state = State::defaults();
        state.set_global_bypass(true);
        state.set_output_ceiling_db(-6.0);
        let document = state.into_document();
        assert!(document.section("global").is_none());
        let params = document.section("parameters").unwrap();
        assert_eq!(params.get("global.bypass"), Some(&Value::from(1.0)));
        assert_eq!(
            params.get("global.output_ceiling_db"),
            Some(&Value::from(-6.0))
        );
    }

    /// D-10.4's backward-compatibility half (D-11.2): a document written before this decision
    /// carries `global.bypass`/`global.output_ceiling_db` in the old, now-retired `global`
    /// section rather than inside `parameters` -- this build must still read it correctly rather
    /// than silently reverting an existing preset's bypass/ceiling to its default.
    #[test]
    fn a_legacy_global_section_is_read_as_a_fallback() {
        let mut document = Document::empty();
        let mut legacy = Map::new();
        legacy.insert("bypass".to_string(), Value::from(true));
        legacy.insert("output_ceiling_db".to_string(), Value::from(-9.0));
        document.set_section("global", legacy);

        let (state, warnings) = State::from_document(document);
        assert!(warnings.is_empty());
        assert!(state.global_bypass());
        assert_eq!(state.output_ceiling_db(), -9.0);
    }

    /// The `parameters` shape wins when a document somehow carries both -- this build's own
    /// current shape is never second-guessed by a stray legacy section.
    #[test]
    fn parameters_shape_takes_precedence_over_a_legacy_global_section() {
        let mut document = Document::empty();
        let mut legacy = Map::new();
        legacy.insert("bypass".to_string(), Value::from(true));
        legacy.insert("output_ceiling_db".to_string(), Value::from(-9.0));
        document.set_section("global", legacy);

        let mut params = Map::new();
        params.insert("global.bypass".to_string(), Value::from(0.0));
        params.insert("global.output_ceiling_db".to_string(), Value::from(-1.0));
        document.set_section("parameters", params);

        let (state, warnings) = State::from_document(document);
        assert!(warnings.is_empty());
        assert!(!state.global_bypass());
        assert_eq!(state.output_ceiling_db(), -1.0);
    }

    /// A load-modify-save cycle of a legacy document upgrades it to the new shape in `parameters`
    /// -- "always write the new shape" -- while leaving the now-inert legacy `global` section
    /// alone (this module's own doc comment on `write_onto` explains why that section is left in
    /// place rather than deleted).
    #[test]
    fn a_legacy_document_upgrades_to_the_new_shape_on_save() {
        let mut original = Document::empty();
        let mut legacy = Map::new();
        legacy.insert("bypass".to_string(), Value::from(true));
        legacy.insert("output_ceiling_db".to_string(), Value::from(-9.0));
        original.set_section("global", legacy);

        let (state, _) = State::from_document(original.clone());
        let saved = state.write_onto(&original);

        let params = saved.section("parameters").unwrap();
        assert_eq!(params.get("global.bypass"), Some(&Value::from(1.0)));
        assert_eq!(
            params.get("global.output_ceiling_db"),
            Some(&Value::from(-9.0))
        );
        // The stale legacy section survives (D-11.2's uniform "unrecognised section" treatment)
        // but is inert: from_document above already preferred the parameters shape once written.
        assert!(saved.section("global").is_some());
    }
}
