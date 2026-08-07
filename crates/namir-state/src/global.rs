//! FR-STATE-010's "complete user-settable state" includes two values that are not a
//! `ParamDescriptor` at all: `namir_engine::Chain::set_global_bypass` (FR-CHAIN-030) and
//! `Chain::set_output_ceiling_db` (FR-CHAIN-090). `namir_params::REGISTRY` has no entry for
//! either — `namir-params`' own crate doc names bypass explicitly as this crate's job, not its
//! own ("Model selection/loading itself is not a `ParamDescriptor`" is the same argument applied
//! to a different kind of non-parameter engine state). Without a place to store them, "the
//! complete user-settable state" would be false by omission.
//!
//! *Consequence (recorded, not silently worked around):* this means there are now genuinely two
//! mechanisms for user-settable values in this format — `parameters` (backed by `REGISTRY`,
//! automatable, host-visible per FR-PARAM-*) and `global` (backed by nothing but this struct).
//! M6's CLAP adapter will want bypass exposed as a **host** parameter (most DAWs give bypass
//! special transport-level treatment), which this shape does not provide for. Flagged here as a
//! decision M6 needs to make, not solved by this crate pre-emptively guessing at CLAP's shape.

use serde_json::{Map, Value};

/// The two pieces of engine state FR-STATE-010 requires but `namir_params::REGISTRY` doesn't
/// carry. Defaults match `namir_engine::Chain::new`'s own (`global_bypass: false`,
/// `output_ceiling_linear: db_to_linear(0.0)`) — restated here as the literal values rather than
/// imported, since `namir-state` may not depend on `namir-engine` (D-5.1) to read them off the
/// real type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Global {
    /// FR-CHAIN-030's chain-wide bypass.
    pub bypass: bool,
    /// FR-CHAIN-090's output ceiling, in dB.
    pub output_ceiling_db: f32,
}

impl Global {
    /// `bypass: false`, `output_ceiling_db: 0.0` — `namir_engine::Chain::new`'s own defaults.
    pub fn defaults() -> Self {
        Self {
            bypass: false,
            output_ceiling_db: 0.0,
        }
    }

    pub(crate) fn to_value(self) -> Map<String, Value> {
        let mut obj = Map::new();
        obj.insert("bypass".to_string(), Value::from(self.bypass));
        obj.insert(
            "output_ceiling_db".to_string(),
            Value::from(f64::from(self.output_ceiling_db)),
        );
        obj
    }

    /// D-11.2's tolerant read: a missing or wrongly-typed field takes the default rather than
    /// failing the whole document — the same tolerance `params.rs` applies per-parameter, applied
    /// here to a struct with only two fields, so a full `StateWarning` machine feels
    /// disproportionate; a field silently defaulting is the entire behaviour either way.
    pub(crate) fn from_value(section: &Map<String, Value>) -> Self {
        let defaults = Self::defaults();
        let bypass = section
            .get("bypass")
            .and_then(Value::as_bool)
            .unwrap_or(defaults.bypass);
        let output_ceiling_db = section
            .get("output_ceiling_db")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .map(|v| v as f32)
            .unwrap_or(defaults.output_ceiling_db);
        Self {
            bypass,
            output_ceiling_db,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_value() {
        let original = Global {
            bypass: true,
            output_ceiling_db: -6.0,
        };
        let restored = Global::from_value(&original.to_value());
        assert_eq!(restored, original);
    }

    #[test]
    fn absent_section_yields_defaults() {
        let restored = Global::from_value(&Map::new());
        assert_eq!(restored, Global::defaults());
    }

    #[test]
    fn wrongly_typed_fields_fall_back_to_defaults() {
        let mut section = Map::new();
        section.insert("bypass".to_string(), Value::from("not a bool"));
        section.insert("output_ceiling_db".to_string(), Value::from("not a number"));
        assert_eq!(Global::from_value(&section), Global::defaults());
    }
}
