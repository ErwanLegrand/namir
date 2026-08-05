//! The `.nam` JSON schema, deserialized as directly as possible so the mapping from bytes to
//! struct fields is easy to audit against real exported files. This module does **not** validate
//! anything beyond what `serde` itself enforces (types, required-ness) — semantic validation
//! (architecture, dimension ceilings, weight-count consistency, chaining) is
//! `wavenet::PreparedNam::from_file`'s job, kept separate so this module stays the "one hardened
//! place" (P6) that only has to reason about JSON shape, not model semantics.
//!
//! Deliberately no `#[serde(deny_unknown_fields)]` anywhere: real `.nam` files may carry extra
//! fields this crate doesn't read yet (training metadata, etc.), and FR-NAM-040 only asks that
//! *malformed* files be rejected — unknown-but-otherwise-valid extra fields are not malformed.

use serde::Deserialize;

use crate::error_codes::{self, NamLoadError};

#[derive(Debug, Clone, Deserialize)]
pub struct NamFile {
    #[serde(default)]
    pub version: Option<String>,
    pub architecture: String,
    pub config: WaveNetConfig,
    pub weights: Vec<f32>,
    /// Real files may omit this; FR §2's definitions note the model sample rate is "typically 48
    /// kHz", which `wavenet::PreparedNam::from_file` uses as the fallback when absent.
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub metadata: NamMetadata,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaveNetConfig {
    pub layers: Vec<LayerArrayConfig>,
    pub head_scale: f32,
    #[serde(default)]
    pub head: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LayerArrayConfig {
    pub input_size: usize,
    pub condition_size: usize,
    pub head_size: usize,
    pub channels: usize,
    pub kernel_size: usize,
    pub dilations: Vec<usize>,
    pub activation: String,
    pub gated: bool,
    pub head_bias: bool,
}

/// FR-NAM-080: "Namir shall read and display the model's metadata where present: name, author
/// (`modeled_by`), gear make/model/type, tone type, and any free-text description." All fields
/// default to empty since real files may omit any of them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NamMetadata {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub modeled_by: String,
    #[serde(default)]
    pub gear_type: String,
    #[serde(default)]
    pub tone_type: String,
    #[serde(default)]
    pub description: String,
}

impl NamFile {
    /// The JSON-shape half of P6's "one hardened place `.nam` bytes go through": turns arbitrary
    /// bytes into a `NamFile` or a catalogued `NamLoadError`, never a panic (FR-NAM-040,
    /// NFR-QUAL-040). Semantic validation happens afterwards, in
    /// `wavenet::PreparedNam::from_file`.
    pub fn parse(bytes: &[u8]) -> Result<Self, NamLoadError> {
        serde_json::from_slice(bytes).map_err(|e| NamLoadError {
            code: error_codes::MALFORMED_JSON,
            detail: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_valid_json() -> Vec<u8> {
        serde_json::json!({
            "architecture": "WaveNet",
            "config": {
                "layers": [{
                    "input_size": 1,
                    "condition_size": 1,
                    "head_size": 1,
                    "channels": 1,
                    "kernel_size": 1,
                    "dilations": [1],
                    "activation": "Tanh",
                    "gated": false,
                    "head_bias": false
                }],
                "head_scale": 0.5,
                "head": null
            },
            "weights": [0.0, 0.0, 0.0, 0.0, 0.0],
            "sample_rate": 48000,
            "metadata": {}
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn parses_minimal_valid_file() {
        let file = NamFile::parse(&minimal_valid_json()).unwrap();
        assert_eq!(file.architecture, "WaveNet");
        assert_eq!(file.config.layers.len(), 1);
        assert_eq!(file.sample_rate, Some(48_000));
    }

    #[test]
    fn tolerates_unknown_extra_fields() {
        let mut value: serde_json::Value = serde_json::from_slice(&minimal_valid_json()).unwrap();
        value["totally_unrecognized_field"] = serde_json::json!("some training metadata");
        value["config"]["totally_unrecognized_nested_field"] = serde_json::json!(42);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(NamFile::parse(&bytes).is_ok());
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        let value = serde_json::json!({
            "architecture": "WaveNet",
            "config": {
                "layers": [],
                "head_scale": 0.5
            },
            "weights": []
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        let file = NamFile::parse(&bytes).unwrap();
        assert_eq!(file.sample_rate, None);
        assert_eq!(file.version, None);
        assert_eq!(file.metadata.name, "");
        assert_eq!(file.config.head, None);
    }

    #[test]
    fn malformed_json_is_rejected_not_panicking() {
        let err = NamFile::parse(b"{not valid json").unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }

    #[test]
    fn missing_required_field_is_rejected_as_malformed() {
        let value =
            serde_json::json!({ "config": { "layers": [], "head_scale": 0.0 }, "weights": [] });
        let bytes = serde_json::to_vec(&value).unwrap();
        let err = NamFile::parse(&bytes).unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }
}
