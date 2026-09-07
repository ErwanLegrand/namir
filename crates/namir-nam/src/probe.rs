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
//!
//! # SlimmableContainer probing (issue #172)
//!
//! [`ProbeConfig`] exists to read one container-level thing — the `submodels` array — and even
//! then only each entry's `max_value` and `model` field. Each [`ProbeSubmodel`] reads the fields
//! a probe reports (version, `sample_rate`, metadata) and deliberately ignores everything else:
//! its inner `config` and its `weights` are both [`serde::de::IgnoredAny`], so probing a
//! container with a very large submodel never allocates that submodel's weight vector — the same
//! guarantee the top-level probe gives a plain model, extended to the nested shape.
//!
//! **Resolution precedence:** for a container, the *selected* (last) submodel is the model the
//! container actually represents once loaded, so its own fields win, and the container's
//! top-level fields fill in whatever the submodel omits — `version`: container first, selected
//! submodel as fallback (a container-declared value must win, see
//! `model::load_slimmable_container`'s consistency check); metadata (`name`, `modeled_by`,
//! `gear_type`, `tone_type`, `description`, `loudness`): selected submodel first, container as
//! fallback. **`sample_rate` is container first and then *any* submodel that declares one**, not
//! only the selected one: `load` proves every declared submodel rate is identical, so the first
//! one found is the container's rate whichever submodel states it. Reading only the selected
//! submodel reported `None` for a file that states its rate one submodel earlier, while `load`
//! reported the rate — the same probe/load disagreement the first review round raised, one
//! submodel over.
//! `model::load_slimmable_container` applies exactly the same resolution to the loaded
//! `PreparedNam`, so probing and loading a container always agree.

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
/// `weights` and model layer configs, read as [`IgnoredAny`] so this deserializer never has to know
/// which architecture's shape it is looking at and never allocates space for either's payload.
#[derive(Debug, Deserialize)]
struct ProbeShape {
    #[serde(default)]
    version: Option<String>,
    architecture: String,
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

/// The container-only second pass (issue #172, and the review of PR #177 that sent it back).
///
/// This exists as a *separate* shape, deserialized from the same bytes only when [`ProbeShape`]
/// has already reported `architecture == "SlimmableContainer"`, rather than as a typed `config`
/// field on `ProbeShape` itself. That is not a stylistic preference — it is the only arrangement
/// that keeps [`ProbeShape`]'s stated invariant true. A typed `config` there is imposed on *every*
/// document, and `#[serde(default)]` only covers an **absent** key, not a present non-object one:
/// `"config": null`, `5`, `[1,2]` and `"x"` are all values serde must then fail to deserialize
/// into a struct, so every such file — plain WaveNet and LSTM included — is rejected as
/// `nam.load.malformed_json` by a probe that used to accept it, and in `namir-library` the entry
/// stays indexed while silently losing its name, architecture and sample rate.
///
/// `config: null` is not a shape today's real exports use. It is exactly the shape AGENTS.md's
/// testing-philosophy section names from a real post-M6 defect ("a metadata field set to JSON
/// `null` rather than omitted"), and the generated fixtures cannot catch it because they never
/// emit it — which is why `file.rs`'s `NamMetadata` already carries a `null_or_default`
/// deserializer for the same trap.
///
/// The cost of the second pass is one extra parse of container files only, and nothing at all for
/// every other document.
#[derive(Debug, Deserialize)]
struct ProbeContainerShape {
    #[serde(default)]
    config: ProbeConfig,
}

#[derive(Debug, Default, Deserialize)]
struct ProbeConfig {
    #[serde(default)]
    submodels: Vec<ProbeSubmodelEntry>,
}

#[derive(Debug, Deserialize)]
struct ProbeSubmodelEntry {
    #[serde(default)]
    #[allow(dead_code)]
    max_value: Option<f64>,
    #[serde(default)]
    model: Option<ProbeSubmodel>,
}

#[derive(Debug, Deserialize)]
struct ProbeSubmodel {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    architecture: Option<String>,
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

    let architecture = shape.architecture;
    let mut version = shape.version;
    let mut sample_rate = shape.sample_rate;
    // Metadata resolves submodel-first, container-fallback (issue #172) — exactly the order
    // `model::load_slimmable_container` applies to the loaded `PreparedNam`, so probe and load
    // agree. `sample_rate`/`version` resolve the other way around (container first, submodel as
    // fallback): a container-declared rate is the authoritative one (see `model.rs`'s consistency
    // check), and probing must match what loading enforces.
    let mut metadata = NamMetadata::default();

    if architecture == "SlimmableContainer" {
        // Second, container-only parse of the same bytes (see `ProbeContainerShape`). A container
        // whose `config` is not the shape a container needs is malformed *as a container*, and
        // `model::load_slimmable_container` would reject the same bytes through
        // `ContainerFile::parse`, so reporting the same code keeps this function's stated contract
        // -- "a file this rejects would be rejected by a full parse too" -- intact.
        let container: ProbeContainerShape =
            serde_json::from_slice(bytes).map_err(|e| NamLoadError {
                code: error_codes::MALFORMED_JSON,
                detail: e.to_string(),
            })?;

        // NFR-SEC-020, and the asymmetry is the point: `load` has carried `MAX_SUBMODELS` since
        // the first round of review on PR #177 and this path did not, so the *bounded* path
        // rejected an over-ceiling container while the unbounded one accepted it and allocated a
        // `String` per metadata field per submodel. That is backwards relative to exposure --
        // `load` runs only on a file the user deliberately selected, while a library scan walks
        // every `.nam` under a scan root through here and swallows the error
        // (`namir-library`'s `probe.rs` degrades a probe failure to `ItemMetadata::None`).
        crate::shared::check_max(
            container.config.submodels.len(),
            crate::shared::MAX_SUBMODELS,
            "config.submodels.len()",
        )?;

        // The *selected* submodel's own rate, never an earlier one's. PR #177's review (item 10)
        // asked for the container-wide agreed rate here instead; that was implemented and then
        // withdrawn on reading the reference, which consults only the active submodel's own rate
        // when the container declares none -- see `model::load_slimmable_container`'s comment at
        // the `set_sample_rate` call for the full argument. Probe and load resolve identically,
        // which is the property that matters, and they now do so by both matching the reference
        // rather than by both diverging from it.
        if let Some(last_entry) = container.config.submodels.last()
            && let Some(sub) = &last_entry.model
        {
            if version.is_none() {
                version = sub.version.clone();
            }
            if sample_rate.is_none() {
                sample_rate = sub.sample_rate;
            }
            metadata = sub.metadata.clone();
        }
    }

    // Container metadata as fallback: fill only the fields the submodel left empty/`None`. Shared
    // with `PreparedWaveNet`/`PreparedLstm`'s `merge_metadata` rather than enumerated a third time
    // here, so probe and load cannot drift when `NamMetadata` grows a field.
    metadata.fill_empty_from(&shape.metadata);

    Ok(NamProbe {
        architecture,
        version,
        sample_rate,
        metadata,
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
    // trace-partial: FR-NAM-080
    // uncovered: FR-NAM-080 — the "and display" half spans only the name field: UiSnapshot
    // uncovered: carries no metadata field beyond loaded_model_name, which namir-app fills from
    // uncovered: the file's basename rather than metadata.name, so author, gear make/model/type,
    // uncovered: tone type and description reach no screen; closes M8
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

    /// PR #177's review, item 1 — the regression that sent this file back, pinned so it cannot
    /// return. `#[serde(default)]` covers an *absent* key, never a present one holding the wrong
    /// type, so a typed `config` field on `ProbeShape` turned every one of these into a hard
    /// rejection of a file a probe had always accepted — for plain WaveNet and LSTM documents,
    /// nothing to do with containers. In `namir-library` the entry stays indexed and silently
    /// loses its name, architecture and sample rate, which is the quiet half of the failure.
    ///
    /// `null` is first in the list on purpose: AGENTS.md's testing-philosophy section names
    /// exactly this shape from a real post-M6 defect, and the generated fixtures cannot produce it.
    #[test]
    fn probe_accepts_a_config_that_is_not_an_object_for_every_architecture() {
        for config in ["null", "5", "[1,2]", "\"x\"", "{}"] {
            for architecture in ["WaveNet", "LSTM"] {
                let bytes = format!(
                    r#"{{"architecture": "{architecture}", "config": {config}, "weights": [1.0], "sample_rate": 48000, "metadata": {{"name": "N"}}}}"#
                )
                .into_bytes();
                let probe = probe_metadata(&bytes).unwrap_or_else(|e| {
                    panic!(
                        "config {config} on {architecture} should probe, got {}",
                        e.code.id
                    )
                });
                assert_eq!(probe.architecture, architecture);
                assert_eq!(probe.sample_rate, Some(48_000));
                assert_eq!(probe.metadata.name, "N");
            }
        }
    }

    /// PR #177's review, item 9. `load` has carried `MAX_SUBMODELS` since the first review round
    /// and this path did not, so the bounded path rejected an over-ceiling container while the
    /// unbounded one accepted it — backwards relative to exposure, since a library scan walks
    /// every `.nam` under a scan root through here and swallows the error.
    #[test]
    fn probe_rejects_a_container_with_more_than_max_submodels() {
        let submodels: Vec<String> = (0..crate::shared::MAX_SUBMODELS + 1)
            .map(|i| {
                format!(
                    r#"{{"max_value": {}, "model": {{"architecture": "WaveNet", "metadata": {{"name": "s{i}"}}}}}}"#,
                    i + 1
                )
            })
            .collect();
        let bytes = format!(
            r#"{{"architecture": "SlimmableContainer", "config": {{"submodels": [{}]}}}}"#,
            submodels.join(",")
        )
        .into_bytes();

        let err = probe_metadata(&bytes).expect_err("over-ceiling container should be rejected");
        assert_eq!(err.code.id, error_codes::DIMENSION_LIMIT_EXCEEDED.id);
    }

    /// PR #177's review, item 10, probe half — pinned to the *reference's* answer rather than to
    /// the one the item asked for. A rate stated only by a non-selected submodel is not the
    /// selected model's rate: with no top-level `sample_rate` the reference's container rate is
    /// `NAM_UNKNOWN_EXPECTED_SAMPLE_RATE` and the active model reports whatever it itself
    /// declares, which here is nothing. `None` is therefore the honest answer, and it means what
    /// `NamProbe::sample_rate`'s doc says it means — the file declares nothing, so the "typically
    /// 48 kHz" convention applies, which is exactly what the loaded model then applies.
    #[test]
    fn probe_ignores_a_rate_declared_only_by_a_non_selected_submodel() {
        let bytes = br#"{"architecture": "SlimmableContainer", "config": {"submodels": [
            {"max_value": 0.5, "model": {"architecture": "WaveNet", "sample_rate": 44100}},
            {"max_value": 1.0, "model": {"architecture": "WaveNet", "metadata": {"name": "full"}}}
        ]}}"#;
        let probe = probe_metadata(bytes).unwrap();
        assert_eq!(probe.sample_rate, None);
        assert_eq!(probe.metadata.name, "full");
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

    #[test]
    fn probe_extracts_metadata_from_slimmable_container_submodel() {
        let bytes = serde_json::json!({
            "version": "0.7.0",
            "architecture": "SlimmableContainer",
            "config": {
                "submodels": [
                    {
                        "max_value": 0.5,
                        "model": {
                            "architecture": "WaveNet",
                            "version": "0.7.0",
                            "config": {},
                            "weights": vec![0.0; 100],
                            "sample_rate": 48_000,
                            "metadata": {
                                "name": "Lite Amp",
                                "modeled_by": "Author",
                                "gear_type": "Amp",
                                "tone_type": "Clean",
                                "description": "Lite model",
                                "loudness": -14.5
                            }
                        }
                    },
                    {
                        "max_value": 1.0,
                        "model": {
                            "architecture": "WaveNet",
                            "version": "0.7.0",
                            "config": {},
                            "weights": vec![0.0; 1000],
                            "sample_rate": 48_000,
                            "metadata": {
                                "name": "Full Amp",
                                "modeled_by": "Author",
                                "gear_type": "Amp",
                                "tone_type": "Lead",
                                "description": "Full model",
                                "loudness": -12.0
                            }
                        }
                    }
                ]
            }
        })
        .to_string()
        .into_bytes();

        let probe = probe_metadata(&bytes).unwrap();
        assert_eq!(probe.architecture, "SlimmableContainer");
        assert_eq!(probe.version.as_deref(), Some("0.7.0"));
        assert_eq!(probe.sample_rate, Some(48_000));
        assert_eq!(probe.metadata.name, "Full Amp");
        assert_eq!(probe.metadata.modeled_by, "Author");
        assert_eq!(probe.metadata.gear_type, "Amp");
        assert_eq!(probe.metadata.tone_type, "Lead");
        assert_eq!(probe.metadata.description, "Full model");
        assert_eq!(probe.metadata.loudness, Some(-12.0));
    }

    #[test]
    fn probe_succeeds_on_container_with_large_submodel_weights() {
        let bytes = serde_json::json!({
            "version": "0.7.0",
            "architecture": "SlimmableContainer",
            "config": {
                "submodels": [
                    {
                        "max_value": 1.0,
                        "model": {
                            "architecture": "WaveNet",
                            "config": {},
                            "weights": vec![0.0f32; 2_000_000],
                            "sample_rate": 48_000,
                            "metadata": {
                                "name": "Big Model"
                            }
                        }
                    }
                ]
            }
        })
        .to_string()
        .into_bytes();

        let probe = probe_metadata(&bytes).unwrap();
        assert_eq!(probe.architecture, "SlimmableContainer");
        assert_eq!(probe.metadata.name, "Big Model");
    }

    /// Issue #172 review finding 1, probe half: a container declaring `sample_rate` with
    /// rate-less submodels must probe at the container's rate (the 48 kHz fallback must not
    /// apply), matching what `model::load` reports for the same bytes.
    #[test]
    fn probe_propagates_container_sample_rate_to_rate_less_submodels() {
        let bytes = serde_json::json!({
            "version": "0.7.0",
            "architecture": "SlimmableContainer",
            "config": {
                "submodels": [
                    {
                        "max_value": 1.0,
                        "model": {
                            "architecture": "WaveNet",
                            "config": {},
                            "weights": vec![0.0f32; 100]
                        }
                    }
                ]
            },
            "sample_rate": 44_100
        })
        .to_string()
        .into_bytes();

        let probe = probe_metadata(&bytes).unwrap();
        assert_eq!(probe.sample_rate, Some(44_100));
    }

    /// Issue #172 review finding 2, probe half: metadata resolves submodel-first,
    /// container-fallback — a non-empty selected-submodel field overrides the container's, and
    /// the container fills the submodel's empty/`None` fields. Same resolution as the load path.
    #[test]
    fn probe_metadata_precedence_is_submodel_first_container_fallback() {
        let bytes = serde_json::json!({
            "version": "0.7.0",
            "architecture": "SlimmableContainer",
            "metadata": {
                "name": "Container Model",
                "modeled_by": "Container Author",
                "gear_type": "Container Gear",
                "tone_type": "Container Tone",
                "description": "Container description",
                "loudness": -9.5
            },
            "config": {
                "submodels": [
                    {
                        "max_value": 1.0,
                        "model": {
                            "architecture": "WaveNet",
                            "config": {},
                            "weights": vec![0.0f32; 100],
                            "metadata": {
                                "name": "Submodel Model",
                                "loudness": -12.0
                            }
                        }
                    }
                ]
            }
        })
        .to_string()
        .into_bytes();

        let probe = probe_metadata(&bytes).unwrap();
        // Submodel fields win where non-empty...
        assert_eq!(probe.metadata.name, "Submodel Model");
        assert_eq!(probe.metadata.loudness, Some(-12.0));
        // ...container fields fill the submodel's gaps.
        assert_eq!(probe.metadata.modeled_by, "Container Author");
        assert_eq!(probe.metadata.gear_type, "Container Gear");
        assert_eq!(probe.metadata.tone_type, "Container Tone");
        assert_eq!(probe.metadata.description, "Container description");

        // Load-agreement for the exact same resolution lives in `model.rs`'s
        // `slimmable_container_metadata_resolution_matches_probe` (it needs a fully valid
        // submodel to load; this probe-only fixture deliberately has none).
    }
}
