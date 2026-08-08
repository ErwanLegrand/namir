//! A cheap way to read a `.nam` file's declared architecture and display metadata (FR-NAM-080)
//! without materializing its weight vector — the field a full [`crate::NamFile::parse`] /
//! [`crate::LstmFile::parse`] must allocate (up to roughly 10 million `f32` for a large WaveNet
//! model) but a library index (`namir-library`, M5) has no use for.
//!
//! # Why this exists (added M5)
//!
//! Indexing a 10 000-file library through the full parse path would mean allocating and
//! immediately discarding every model's weights just to read five short display strings —
//! wasted work at scale, and exactly the kind of unbounded-allocation-for-untrusted-input surface
//! NFR-SEC-020 exists to bound. `namir-library` cannot avoid this by parsing `.nam` JSON itself:
//! P6 requires exactly one hardened, fuzzed parser per format, so a second parser for the same
//! format anywhere else in the workspace would violate the same principle NFR-QUAL-040 relies on.
//! [`sniff_architecture`](crate::file) already establishes the pattern of a partial parse reusing
//! `NamFile`/`LstmFile`'s field names without paying for the whole document; this generalizes it
//! to the fields a library index actually wants, and makes it `pub` rather than `pub(crate)`.
//!
//! # Why one probe shape serves both architectures
//!
//! Unlike [`crate::NamFile`]/[`crate::LstmFile`], which must diverge because their `config` shapes
//! genuinely differ (see `file.rs`'s "Two file shapes, not one `config: serde_json::Value`"), a
//! probe never looks inside `config` at all — it is read as [`serde::de::IgnoredAny`] regardless
//! of architecture, so one shape covers every `.nam` variant this crate will ever support without
//! needing a matching sibling type the way the full parsers do.

use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::error_codes::{self, NamLoadError};
use crate::file::NamMetadata;

/// What a library index can learn about a `.nam` file without materializing its weights.
#[derive(Debug, Clone, PartialEq)]
pub struct NamProbe {
    /// The declared architecture name (e.g. `"WaveNet"`, `"LSTM"`), unvalidated — a probe reports
    /// what the file claims, it does not confirm the architecture is one this crate supports.
    pub architecture: String,
    /// The exporter's format version string, when present.
    pub version: Option<String>,
    /// The declared model sample rate, when present (absent means the FR §2 "typically 48 kHz"
    /// convention applies, exactly as it does for a full parse).
    pub sample_rate: Option<u32>,
    /// FR-NAM-080's display metadata.
    pub metadata: NamMetadata,
}

/// The same field set [`NamFile`](crate::NamFile)/[`LstmFile`](crate::LstmFile) share, minus
/// `config` and `weights`, both read as [`IgnoredAny`] so this deserializer never has to know
/// which architecture's shape it is looking at and never allocates space for either's payload.
#[derive(Debug, Deserialize)]
struct ProbeShape {
    #[serde(default)]
    version: Option<String>,
    architecture: String,
    // Never read: their entire purpose is to make serde skip over the JSON value at this key
    // (whatever shape it is, for either architecture) without deserializing it into anything
    // that would allocate proportionally to the model's size. That "never read" is the point,
    // not an oversight -- see this module's doc comment.
    #[serde(default)]
    #[allow(dead_code)]
    config: IgnoredAny,
    #[serde(default)]
    #[allow(dead_code)]
    weights: IgnoredAny,
    #[serde(default)]
    sample_rate: Option<u32>,
    #[serde(default)]
    metadata: NamMetadata,
}

/// Parses just enough of `bytes` to answer FR-LIB-040's search fields and FR-NAM-080's display
/// metadata, deliberately never touching `weights`. Uses the same rejection code
/// ([`error_codes::MALFORMED_JSON`]) a full parse would give the same bytes, so a caller cannot
/// distinguish "probed" from "fully parsed" by the error alone — a file this rejects would be
/// rejected by [`crate::NamFile::parse`]/[`crate::LstmFile::parse`] too, not silently accepted
/// into an index a full load would then refuse.
pub fn probe_metadata(bytes: &[u8]) -> Result<NamProbe, NamLoadError> {
    let shape: ProbeShape = serde_json::from_slice(bytes).map_err(|e| NamLoadError {
        code: error_codes::MALFORMED_JSON,
        detail: e.to_string(),
    })?;
    Ok(NamProbe {
        architecture: shape.architecture,
        version: shape.version,
        sample_rate: shape.sample_rate,
        metadata: shape.metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{LstmFile, NamFile};

    fn wavenet_json_with_weights(weight_count: usize) -> Vec<u8> {
        serde_json::json!({
            "architecture": "WaveNet",
            "version": "0.5.4",
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
            "weights": vec![0.0_f32; weight_count],
            "sample_rate": 48_000,
            "metadata": {
                "name": "Plexi",
                "modeled_by": "Someone",
                "gear_type": "Amp",
                "tone_type": "Crunch",
                "description": "A test fixture"
            }
        })
        .to_string()
        .into_bytes()
    }

    fn lstm_json() -> Vec<u8> {
        serde_json::json!({
            "architecture": "LSTM",
            "config": { "num_layers": 1, "input_size": 1, "hidden_size": 2 },
            "weights": [0.0, 0.0, 0.0],
            "sample_rate": 44_100
        })
        .to_string()
        .into_bytes()
    }

    /// The headline property: a probe over a WaveNet file must agree with a full `NamFile::parse`
    /// on every field the probe claims to report.
    // trace: FR-NAM-080
    #[test]
    fn probe_agrees_with_the_full_wavenet_parse() {
        let bytes = wavenet_json_with_weights(5);
        let full = NamFile::parse(&bytes).unwrap();
        let probe = probe_metadata(&bytes).unwrap();

        assert_eq!(probe.architecture, full.architecture);
        assert_eq!(probe.version, full.version);
        assert_eq!(probe.sample_rate, full.sample_rate);
        assert_eq!(probe.metadata.name, full.metadata.name);
        assert_eq!(probe.metadata.modeled_by, full.metadata.modeled_by);
        assert_eq!(probe.metadata.gear_type, full.metadata.gear_type);
        assert_eq!(probe.metadata.tone_type, full.metadata.tone_type);
        assert_eq!(probe.metadata.description, full.metadata.description);
    }

    /// The reason this module exists: probing a file with an enormous declared weight vector
    /// must not pay for materializing it. `serde_json::from_slice` still has to *scan* the bytes
    /// (it is not a streaming skip), so this asserts the *outcome* — the probe succeeds and
    /// nothing about its behaviour depends on weight count — rather than trying to measure
    /// allocation directly, which the RT harness (namir-engine/namir-dsp only) does not cover
    /// off-audio-thread code like this anyway.
    #[test]
    fn probe_succeeds_on_a_file_with_a_very_large_declared_weight_vector() {
        let bytes = wavenet_json_with_weights(2_000_000);
        let probe = probe_metadata(&bytes).unwrap();
        assert_eq!(probe.architecture, "WaveNet");
    }

    #[test]
    fn probe_agrees_with_the_full_lstm_parse() {
        let bytes = lstm_json();
        let full = LstmFile::parse(&bytes).unwrap();
        let probe = probe_metadata(&bytes).unwrap();

        assert_eq!(probe.architecture, full.architecture);
        assert_eq!(probe.sample_rate, full.sample_rate);
    }

    #[test]
    fn probe_tolerates_missing_optional_fields() {
        let value = serde_json::json!({ "architecture": "WaveNet" });
        let bytes = serde_json::to_vec(&value).unwrap();
        let probe = probe_metadata(&bytes).unwrap();
        assert_eq!(probe.architecture, "WaveNet");
        assert_eq!(probe.version, None);
        assert_eq!(probe.sample_rate, None);
        assert_eq!(probe.metadata.name, "");
    }

    #[test]
    fn probe_rejects_malformed_json_with_the_same_code_a_full_parse_would() {
        let malformed = b"{not valid json";
        let probe_err = probe_metadata(malformed).unwrap_err();
        let full_err = NamFile::parse(malformed).unwrap_err();
        assert_eq!(probe_err.code.id, full_err.code.id);
        assert_eq!(probe_err.code.id, error_codes::MALFORMED_JSON.id);
    }

    #[test]
    fn probe_rejects_a_document_missing_architecture() {
        let value = serde_json::json!({ "sample_rate": 48_000 });
        let bytes = serde_json::to_vec(&value).unwrap();
        let err = probe_metadata(&bytes).unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_JSON.id);
    }
}
