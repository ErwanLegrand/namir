//! FR-STATE-010/020's parameter block: [`ParamValues`], a value for every entry of
//! [`namir_params::REGISTRY`], stored and read by each parameter's stable string key.
//!
//! # A complete array, not a map
//!
//! **Decision:** `ParamValues` holds exactly `REGISTRY.len()` values, indexed positionally in
//! `REGISTRY`'s own order — not a `HashMap<String, f32>` covering only the keys a particular
//! document happens to mention.
//!
//! **Rationale:** FR-STATE-020 requires "any parameter absent from the document takes its
//! documented default". A map-backed representation can only make that true by remembering, at
//! every read site, to fall back to the default when a lookup misses — a rule a future change can
//! forget to apply. An array initialised from `REGISTRY`'s own defaults and then selectively
//! overwritten by whatever the document *does* contain cannot represent "this parameter has no
//! value" at all, so the FR-STATE-020 guarantee holds structurally rather than by every caller's
//! discipline.
//!
//! # Keys, not ids, on disk
//!
//! A document stores `"eq.mid_q": 0.7`, not `"2519697679": 0.7`. FR-STATE-040 requires a document
//! a user can inspect and hand-edit; a raw FNV-1a hash is not that. This is safe *because*
//! `ParamId::from_key` derives the id from the key (`namir_params::id`) and `params.lock`'s own
//! tombstone discipline (D-10.1) already treats an existing entry's key as permanent — so
//! `params.lock` was, without anyone deciding it explicitly until now, already protecting the
//! state format's on-disk vocabulary, not only host automation's numeric ids.
//!
//! # Units, not normalised 0..1
//!
//! A value is stored in the parameter's own unit (`"ir.level_db": -3.0`, not `"ir.level_db":
//! 0.4375`). A normalised encoding would silently re-map every saved document the moment a
//! range widens — precisely the FR-STATE-020 compatibility failure this format exists to
//! prevent, since a normalised `0.4375` means a different physical value once the range it was
//! computed against changes. A stepped parameter stores its selected index as a plain number
//! (`"eq.enabled": 1.0`) for the same reason applied to `ParamKind::Stepped`: storing the
//! *display name* instead would make a purely cosmetic rename of a named value a compatibility
//! break, which FR-STATE-020 also forbids.

use std::collections::BTreeMap;

use namir_params::{ParamDescriptor, ParamId, ParamKind, REGISTRY};
use serde_json::{Map, Number, Value};

use crate::error::StateWarning;
use crate::error_codes;

/// One value per [`REGISTRY`] entry, in `REGISTRY` order.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamValues(Box<[f32]>);

impl ParamValues {
    /// Every parameter at its `ParamDescriptor`'s own declared default — FR-STATE-020's
    /// "documented default", read directly from the single source of truth `namir-params`
    /// already is, never duplicated here.
    pub fn defaults() -> Self {
        let values = REGISTRY.iter().map(default_of).collect::<Vec<_>>();
        Self(values.into_boxed_slice())
    }

    fn index_of(key: &str) -> Option<usize> {
        REGISTRY.iter().position(|d| d.key == key)
    }

    fn index_of_id(id: ParamId) -> Option<usize> {
        REGISTRY.iter().position(|d| d.id == id)
    }

    /// The current value for `key`, or `None` if `key` names no `REGISTRY` entry.
    pub fn get(&self, key: &str) -> Option<f32> {
        Self::index_of(key).map(|i| self.0[i])
    }

    /// The current value for a `namir_params::ParamId`, or `None` if it names no `REGISTRY`
    /// entry — the lookup a caller holding an id rather than a key (e.g. `namir-worker`,
    /// converting to `namir_engine::ParamId` to push a `Command::Param`) actually has in hand.
    pub fn get_by_id(&self, id: ParamId) -> Option<f32> {
        Self::index_of_id(id).map(|i| self.0[i])
    }

    /// Sets `key`'s value, clamping it to the descriptor's range (`Continuous`) or to
    /// `0..values.len() - 1` (`Stepped`) rather than rejecting an out-of-range value outright —
    /// the same tolerance [`Self::from_document_section`] applies to a value read from a file,
    /// applied here so a programmatic caller (a UI control, a test) gets identical behaviour
    /// rather than a second set of rules. Fails only when `key` names no `REGISTRY` entry at
    /// all, since there is then no descriptor to clamp against.
    pub fn set(&mut self, key: &str, value: f32) -> Result<(), UnknownParameter> {
        let index = Self::index_of(key).ok_or_else(|| UnknownParameter(key.to_string()))?;
        self.0[index] = clamp_to_descriptor(&REGISTRY[index], value);
        Ok(())
    }

    /// Every `(descriptor, current value)` pair, in `REGISTRY` order — what a save path or a UI
    /// listing every control iterates over.
    pub fn iter(&self) -> impl Iterator<Item = (&'static ParamDescriptor, f32)> + '_ {
        REGISTRY.iter().zip(self.0.iter().copied())
    }

    /// D-11.2's tolerant read, applied to the `parameters` section of a [`crate::Document`].
    /// Three rules, each producing a [`StateWarning`] rather than failing the whole document:
    ///
    /// - a key `section` contains that no `REGISTRY` entry claims is **left at its default and
    ///   not applied** (`error_codes::UNKNOWN_PARAMETER`) — the value itself is not lost, though:
    ///   the caller is expected to keep `section`'s own `Map` around (in the `Document` carrier)
    ///   and write it back verbatim on save, which is what actually satisfies D-11.2's "a project
    ///   saved by a newer Namir... does not silently lose settings" for a parameter this build
    ///   has never heard of;
    /// - a recognised key whose value is a finite number outside its descriptor's range is
    ///   **clamped** (`error_codes::PARAMETER_OUT_OF_RANGE`);
    /// - a recognised key whose value is not a finite number at all (wrong JSON type, `NaN`,
    ///   `Infinity`) is **reset to its default** (`error_codes::PARAMETER_INVALID`) — there is no
    ///   nearby value to clamp a non-number to.
    pub fn from_document_section(section: &Map<String, Value>) -> (Self, Vec<StateWarning>) {
        let mut values = Self::defaults();
        let mut warnings = Vec::new();

        for (key, raw) in section {
            let Some(index) = Self::index_of(key) else {
                warnings.push(StateWarning::new(
                    error_codes::UNKNOWN_PARAMETER,
                    key.clone(),
                ));
                continue;
            };
            let descriptor = &REGISTRY[index];
            match raw.as_f64().filter(|v| v.is_finite()) {
                Some(v) => {
                    let clamped = clamp_to_descriptor(descriptor, v as f32);
                    if clamped != v as f32 {
                        warnings.push(StateWarning::new(
                            error_codes::PARAMETER_OUT_OF_RANGE,
                            format!("{key}: {v} clamped to {clamped}"),
                        ));
                    }
                    values.0[index] = clamped;
                }
                None => {
                    warnings.push(StateWarning::new(
                        error_codes::PARAMETER_INVALID,
                        key.clone(),
                    ));
                    // values.0[index] is already the default from Self::defaults() above.
                }
            }
        }

        (values, warnings)
    }

    /// The inverse of [`Self::from_document_section`]: every `REGISTRY` entry's current value,
    /// keyed by its stable string key, ready to hand to [`crate::Document::set_section`]. Uses a
    /// `BTreeMap` on the way in purely so construction order never matters — `serde_json::Map`'s
    /// own `BTreeMap` backing (see `document.rs`'s module doc comment) is what actually makes the
    /// *output* sorted; this is belt-and-suspenders, not load-bearing.
    pub fn to_document_section(&self) -> Map<String, Value> {
        let sorted: BTreeMap<&'static str, f32> = self.iter().map(|(d, v)| (d.key, v)).collect();
        sorted
            .into_iter()
            .map(|(key, value)| (key.to_string(), number_value(value)))
            .collect()
    }
}

/// `key` named no entry in [`REGISTRY`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownParameter(pub String);

impl std::fmt::Display for UnknownParameter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "\"{}\" is not a recognised parameter", self.0)
    }
}

impl std::error::Error for UnknownParameter {}

fn default_of(descriptor: &ParamDescriptor) -> f32 {
    match descriptor.kind {
        ParamKind::Continuous { default, .. } => default,
        ParamKind::Stepped { default_index, .. } => default_index.0 as f32,
    }
}

fn clamp_to_descriptor(descriptor: &ParamDescriptor, value: f32) -> f32 {
    match descriptor.kind {
        ParamKind::Continuous { min, max, .. } => value.clamp(min, max),
        ParamKind::Stepped { values, .. } => {
            let max_index = (values.len().saturating_sub(1)) as f32;
            value.round().clamp(0.0, max_index)
        }
    }
}

/// `serde_json::Value::from(f32)` doesn't exist (`Value` has no direct `f32` conversion; `From`
/// is implemented for `f64`), and going through `f64` can produce a `Number` that round-trips
/// back to a *different* `f32` than it started from for some values because `f64` has more
/// precision — a `serde_json::to_string` of `0.1_f32 as f64` prints extra trailing digits rather
/// than `0.1`. Converting through `f64::from(value)` and letting `serde_json`'s own `f64`
/// formatter (ryu-based, shortest-round-trip) handle it is correct here specifically because
/// every value stored is a genuine `f32` widened losslessly to `f64` — the shortest `f64`
/// decimal that reads back exactly does not, in that specific case, need more digits than the
/// value's `f32` precision actually carries. Pinned by a test rather than trusted from this
/// comment alone.
fn number_value(value: f32) -> Value {
    Number::from_f64(f64::from(value))
        .map(Value::Number)
        .unwrap_or(Value::Null) // value is always finite (clamp_to_descriptor never produces
    // NaN/Infinity from a finite descriptor range), so this arm is unreachable in practice; Null
    // rather than a panic keeps that unreachability a documented assumption, not a crash site.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_every_registry_descriptor() {
        let values = ParamValues::defaults();
        for (descriptor, value) in values.iter() {
            assert_eq!(value, default_of(descriptor), "{}", descriptor.key);
        }
    }

    #[test]
    fn get_and_set_round_trip_for_a_continuous_parameter() {
        let mut values = ParamValues::defaults();
        values.set("trim.gain_db", 6.0).unwrap();
        assert_eq!(values.get("trim.gain_db"), Some(6.0));
    }

    #[test]
    fn set_clamps_a_continuous_value_to_its_range() {
        let mut values = ParamValues::defaults();
        values.set("trim.gain_db", 999.0).unwrap();
        // trim.gain_db's range is -24..24 (crates/namir-params/src/stages/trim.rs).
        assert_eq!(values.get("trim.gain_db"), Some(24.0));
    }

    #[test]
    fn set_rejects_an_unknown_key() {
        let mut values = ParamValues::defaults();
        let err = values.set("not.a.real.parameter", 1.0).unwrap_err();
        assert_eq!(err.0, "not.a.real.parameter");
    }

    #[test]
    fn get_returns_none_for_an_unknown_key() {
        assert_eq!(ParamValues::defaults().get("not.a.real.parameter"), None);
    }

    /// The completeness property FR-STATE-010's round-trip test depends on: every `REGISTRY`
    /// key must actually appear in a serialised default document, or "absent parameters take
    /// their default" would be true only by accident (a document that serialises nothing would
    /// trivially "round-trip").
    #[test]
    fn to_document_section_contains_every_registry_key() {
        let section = ParamValues::defaults().to_document_section();
        for descriptor in REGISTRY {
            assert!(
                section.contains_key(descriptor.key),
                "{} missing from serialised parameters section",
                descriptor.key
            );
        }
        assert_eq!(section.len(), REGISTRY.len());
    }

    #[test]
    fn from_document_section_reads_back_a_value_to_document_section_wrote() {
        let mut values = ParamValues::defaults();
        values.set("eq.mid_q", 1.25).unwrap();
        let section = values.to_document_section();
        let (restored, warnings) = ParamValues::from_document_section(&section);
        assert!(warnings.is_empty());
        assert_eq!(restored.get("eq.mid_q"), Some(1.25));
    }

    #[test]
    fn from_document_section_leaves_an_unknown_key_at_default_and_warns() {
        let mut section = Map::new();
        section.insert("comp.ratio".to_string(), Value::from(4.0));
        let (values, warnings) = ParamValues::from_document_section(&section);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.id, error_codes::UNKNOWN_PARAMETER.id);
        // Every real parameter is still at its default -- the unknown key affected nothing.
        assert_eq!(values, ParamValues::defaults());
    }

    #[test]
    fn from_document_section_clamps_an_out_of_range_value_and_warns() {
        let mut section = Map::new();
        section.insert("trim.gain_db".to_string(), Value::from(999.0));
        let (values, warnings) = ParamValues::from_document_section(&section);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.id, error_codes::PARAMETER_OUT_OF_RANGE.id);
        assert_eq!(values.get("trim.gain_db"), Some(24.0));
    }

    #[test]
    fn from_document_section_resets_a_non_numeric_value_to_default_and_warns() {
        let mut section = Map::new();
        section.insert("trim.gain_db".to_string(), Value::from("not a number"));
        let (values, warnings) = ParamValues::from_document_section(&section);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.id, error_codes::PARAMETER_INVALID.id);
        assert_eq!(values.get("trim.gain_db"), Some(0.0)); // trim.gain_db's default
    }

    /// JSON itself has no representation for NaN/Infinity (`serde_json::Number::from_f64`
    /// returns `None` for both), so a well-formed document can never carry one in a number
    /// position — `from_document_section`'s `is_finite()` filter exists for defence in depth
    /// against a future numeric type change, not because a real document can trigger it today.
    /// This is asserted directly rather than by trying to construct an unconstructable `Value`.
    #[test]
    fn json_cannot_represent_nan_or_infinity_so_the_finite_check_is_a_documented_invariant() {
        assert!(Number::from_f64(f64::NAN).is_none());
        assert!(Number::from_f64(f64::INFINITY).is_none());
    }

    #[test]
    fn number_value_round_trips_f32_precision_through_f64() {
        // The property number_value's own doc comment claims: converting a genuine f32 through
        // f64 and back via JSON text must reproduce the exact same f32, for values this format
        // actually stores (parameter values, which are always finite f32s in sane ranges).
        for v in [0.1_f32, -6.0, 0.707, 48000.0, -0.5, 18000.0] {
            let json = serde_json::to_string(&number_value(v)).unwrap();
            let parsed: f64 = json.parse().unwrap();
            assert_eq!(parsed as f32, v, "{v} did not round-trip through {json}");
        }
    }
}
