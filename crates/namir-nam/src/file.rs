//! The `.nam` JSON schema, deserialized as directly as possible so the mapping from bytes to
//! struct fields is easy to audit against real exported files. This module does **not** validate
//! anything beyond what `serde` itself enforces (types, required-ness) — semantic validation
//! (architecture, dimension ceilings, weight-count consistency, chaining) is
//! `wavenet::PreparedWaveNet::from_file`'s / `lstm::PreparedLstm::from_file`'s job, kept separate
//! so this module stays the "one hardened place" (P6) that only has to reason about JSON shape,
//! not model semantics.
//!
//! Deliberately no `#[serde(deny_unknown_fields)]` anywhere: real `.nam` files may carry extra
//! fields this crate doesn't read yet (training metadata, etc.), and FR-NAM-040 only asks that
//! *malformed* files be rejected — unknown-but-otherwise-valid extra fields are not malformed.
//!
//! # Two file shapes, not one `config: serde_json::Value`
//!
//! **Decision:** `NamFile.config` stays typed as `WaveNetConfig` (unchanged), and LSTM gets its
//! own sibling type, [`LstmFile`] / [`LstmConfigJson`], rather than loosening `NamFile.config` to
//! a raw `serde_json::Value` that each architecture module downcasts after checking
//! `architecture`.
//!
//! **Rationale:** `NamFile`'s exact field set — including `config`'s type — is this crate's
//! already-stable public API: `namir-engine`'s own test module constructs a `NamFile` by struct
//! literal (`NamFile { config: WaveNetConfig { .. }, .. }`), which only compiles if `config`'s
//! declared type is exactly `WaveNetConfig`. Loosening it to `serde_json::Value` would be a
//! breaking change to a downstream crate this task is explicitly scoped to leave untouched (see
//! the crate-level doc comment's zero-namir-engine-changes goal) — struct-literal field types
//! don't get an `Into` coercion, so there is no non-breaking way to change this field's type.
//! Since `NamFile` can therefore only ever represent the WaveNet config shape, an LSTM file needs
//! a type of its own; [`sniff_architecture`] reads just the `architecture` field (common to both
//! shapes) so `model::load` can decide which one to fully parse before committing to either.
//!
//! **Consequence:** the two file shapes don't share a parse path, and adding a third architecture
//! later means a third sibling type here plus a third arm in `model::load`'s dispatch, not a
//! change to this decision.
//!
//! **Alternatives rejected:** `config: serde_json::Value` (breaks `namir-engine`, see above); an
//! enum `NamConfig { WaveNet(WaveNetConfig), Lstm(LstmConfigJson) }` as `NamFile.config`'s type
//! (same problem — the literal `WaveNetConfig { .. }` no longer matches the field's declared
//! type without an explicit `NamConfig::WaveNet(..)` wrapper namir-engine's existing test source
//! doesn't have).

use serde::Deserialize;

use crate::error_codes::{self, NamLoadError};

/// Top-level `.nam` JSON document, deserialized as directly as possible (see this module's doc
/// comment).
#[derive(Debug, Clone, Deserialize)]
pub struct NamFile {
    /// The exporter's format version string, when present. Not itself validated here.
    #[serde(default)]
    pub version: Option<String>,
    /// The model architecture name (e.g. `"WaveNet"`). Semantic validation of the value is
    /// `wavenet::PreparedWaveNet::from_file`'s job, not this module's.
    pub architecture: String,
    /// The WaveNet topology and weights layout, unvalidated beyond JSON shape.
    pub config: WaveNetConfig,
    /// The flat weight vector, in the order `wavenet::PreparedWaveNet::from_file` expects to consume
    /// it. Not shape-checked here.
    pub weights: Vec<f32>,
    /// Real files may omit this; FR §2's definitions note the model sample rate is "typically 48
    /// kHz", which `wavenet::PreparedWaveNet::from_file` uses as the fallback when absent.
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// Display-only metadata (FR-NAM-080); see `NamMetadata`.
    #[serde(default)]
    pub metadata: NamMetadata,
}

/// The WaveNet model's top-level configuration: its stack of layer arrays plus the output head.
#[derive(Debug, Clone, Deserialize)]
pub struct WaveNetConfig {
    /// The model's layer-array stack, in evaluation order.
    pub layers: Vec<LayerArrayConfig>,
    /// Output scaling applied after the head.
    pub head_scale: f32,
    /// Opaque head configuration, kept as raw JSON since this module does not interpret it —
    /// only `wavenet::PreparedWaveNet::from_file` does.
    #[serde(default)]
    pub head: Option<serde_json::Value>,
}

/// One WaveNet layer array's hyperparameters, as exported in the `.nam` JSON — field names match
/// the exporter's schema, not this crate's own naming.
#[derive(Debug, Clone, Deserialize)]
pub struct LayerArrayConfig {
    /// Number of input channels into this layer array.
    pub input_size: usize,
    /// Number of conditioning channels feeding this layer array.
    pub condition_size: usize,
    /// Number of channels feeding the array's head.
    pub head_size: usize,
    /// Number of channels each layer in the array carries internally.
    pub channels: usize,
    /// Convolution kernel width shared by every layer in the array.
    pub kernel_size: usize,
    /// Per-layer dilation factors, one entry per layer in the array.
    pub dilations: Vec<usize>,
    /// Activation function name (e.g. `"Tanh"`), interpreted by
    /// `wavenet::PreparedWaveNet::from_file`.
    pub activation: String,
    /// Whether the layer array uses gated activation.
    pub gated: bool,
    /// Whether the array's head applies a bias term.
    pub head_bias: bool,
}

/// FR-NAM-080: "Namir shall read and display the model's metadata where present: name, author
/// (`modeled_by`), gear make/model/type, tone type, and any free-text description." All fields
/// default to empty since real files may omit any of them.
///
/// `PartialEq` added M5: `probe.rs`'s tests compare a probe's metadata against a full parse's.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct NamMetadata {
    /// The model's display name.
    #[serde(default)]
    pub name: String,
    /// Author/creator credit, per FR-NAM-080's "author (`modeled_by`)".
    #[serde(default)]
    pub modeled_by: String,
    /// Modeled gear's make/model/type, as free text.
    #[serde(default)]
    pub gear_type: String,
    /// Modeled gear's tone type (e.g. "clean", "high gain"), as free text.
    #[serde(default)]
    pub tone_type: String,
    /// Free-text description, shown as-is.
    #[serde(default)]
    pub description: String,
}

impl NamFile {
    /// The JSON-shape half of P6's "one hardened place `.nam` bytes go through": turns arbitrary
    /// bytes into a `NamFile` or a catalogued `NamLoadError`, never a panic (FR-NAM-040,
    /// NFR-QUAL-040). Semantic validation happens afterwards, in
    /// `wavenet::PreparedWaveNet::from_file`.
    pub fn parse(bytes: &[u8]) -> Result<Self, NamLoadError> {
        serde_json::from_slice(bytes).map_err(|e| NamLoadError {
            code: error_codes::MALFORMED_JSON,
            detail: e.to_string(),
        })
    }
}

// -------------------------------------------------------------------------------------------
// LSTM's file shape — a sibling of `NamFile`/`WaveNetConfig`, not a variant of them; see this
// module's doc comment ("Two file shapes, not one `config: serde_json::Value`") for why.
// -------------------------------------------------------------------------------------------

/// Top-level `.nam` JSON document for an LSTM model — the LSTM analogue of [`NamFile`], sharing
/// everything except `config`'s type. Semantic validation of `architecture`'s value is
/// `lstm::PreparedLstm::from_file`'s job, not this module's.
#[derive(Debug, Clone, Deserialize)]
pub struct LstmFile {
    /// The exporter's format version string, when present. Not itself validated here.
    #[serde(default)]
    pub version: Option<String>,
    /// The model architecture name; expected to be `"LSTM"`, checked by
    /// `lstm::PreparedLstm::from_file`.
    pub architecture: String,
    /// The LSTM topology, unvalidated beyond JSON shape.
    pub config: LstmConfigJson,
    /// The flat weight vector, in the order `lstm::PreparedLstm::from_file` expects to consume
    /// it. Not shape-checked here.
    pub weights: Vec<f32>,
    /// Real files may omit this; see `NamFile::sample_rate`'s identical doc comment.
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// Display-only metadata (FR-NAM-080); see `NamMetadata`.
    #[serde(default)]
    pub metadata: NamMetadata,
}

/// One LSTM model's top-level configuration, as exported in the `.nam` JSON — field names match
/// the real exporter's schema (`nam::lstm::parse_config_json` in `NeuralAmpModelerCore`), not
/// this crate's own naming. `in_channels`/`out_channels` default to 1 when the file omits them,
/// matching the reference exporter's own `config.value("in_channels", 1)` /
/// `config.value("out_channels", 1)`.
#[derive(Debug, Clone, Deserialize)]
pub struct LstmConfigJson {
    /// Number of stacked LSTM cells.
    pub num_layers: usize,
    /// Width of the vector fed to the first cell (this crate only supports `1`, the raw mono
    /// signal — see `lstm::PreparedLstm::from_file`'s scope-restriction check).
    pub input_size: usize,
    /// Width of every cell's hidden (and cell) state.
    pub hidden_size: usize,
    /// Number of physical audio input channels; defaults to 1.
    #[serde(default = "default_lstm_channel_count")]
    pub in_channels: usize,
    /// Number of physical audio output channels; defaults to 1.
    #[serde(default = "default_lstm_channel_count")]
    pub out_channels: usize,
}

fn default_lstm_channel_count() -> usize {
    1
}

impl LstmFile {
    /// The LSTM analogue of `NamFile::parse`; see that method's doc comment.
    pub fn parse(bytes: &[u8]) -> Result<Self, NamLoadError> {
        serde_json::from_slice(bytes).map_err(|e| NamLoadError {
            code: error_codes::MALFORMED_JSON,
            detail: e.to_string(),
        })
    }
}

/// Just the `architecture` field, common to both [`NamFile`] and [`LstmFile`]'s JSON shape.
/// `model::load` reads this first, before committing to parsing the rest of the document as
/// either shape, since which one applies is exactly what this field says.
#[derive(Debug, Deserialize)]
struct ArchitectureOnly {
    architecture: String,
}

/// Reads just enough of `bytes` to learn the declared architecture, without assuming either
/// [`NamFile`]'s or [`LstmFile`]'s `config` shape. A JSON document missing `architecture`
/// entirely, or that isn't valid JSON at all, is `MALFORMED_JSON` here (the same rejection
/// either full parse would give it) — this is not a third weaker parse tier, just the one field
/// every recognized `.nam` shape shares in common, read first.
pub(crate) fn sniff_architecture(bytes: &[u8]) -> Result<String, NamLoadError> {
    serde_json::from_slice::<ArchitectureOnly>(bytes)
        .map(|a| a.architecture)
        .map_err(|e| NamLoadError {
            code: error_codes::MALFORMED_JSON,
            detail: e.to_string(),
        })
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

    fn minimal_valid_lstm_json() -> Vec<u8> {
        serde_json::json!({
            "architecture": "LSTM",
            "config": {
                "num_layers": 1,
                "input_size": 1,
                "hidden_size": 2
            },
            "weights": [0.0],
            "sample_rate": 48000
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn parses_minimal_valid_lstm_file() {
        let file = LstmFile::parse(&minimal_valid_lstm_json()).unwrap();
        assert_eq!(file.architecture, "LSTM");
        assert_eq!(file.config.num_layers, 1);
        assert_eq!(file.config.hidden_size, 2);
        assert_eq!(file.version, None, "field omitted from the fixture JSON");
    }

    #[test]
    fn lstm_config_defaults_in_and_out_channels_to_one() {
        let file = LstmFile::parse(&minimal_valid_lstm_json()).unwrap();
        assert_eq!(file.config.in_channels, 1);
        assert_eq!(file.config.out_channels, 1);
    }

    #[test]
    fn malformed_lstm_json_is_rejected_not_panicking() {
        let err = LstmFile::parse(b"{not valid json").unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }

    #[test]
    fn sniff_architecture_reads_wavenet_files() {
        let arch = sniff_architecture(&minimal_valid_json()).unwrap();
        assert_eq!(arch, "WaveNet");
    }

    #[test]
    fn sniff_architecture_reads_lstm_files_without_assuming_wavenet_config_shape() {
        // The point of this test: `minimal_valid_lstm_json`'s "config" object has no "layers" or
        // "head_scale" fields at all, so if `sniff_architecture` accidentally tried to parse the
        // whole document as `NamFile` (config: `WaveNetConfig`) this would fail. It must not.
        let arch = sniff_architecture(&minimal_valid_lstm_json()).unwrap();
        assert_eq!(arch, "LSTM");
    }

    #[test]
    fn sniff_architecture_rejects_malformed_json() {
        let err = sniff_architecture(b"{not valid json").unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }
}
