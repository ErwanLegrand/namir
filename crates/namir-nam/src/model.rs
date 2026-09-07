//! `PreparedNam`/`NamState`: FR-NAM-020's public surface, unchanged in name and method
//! signatures from when this crate supported WaveNet only — a closed enum over
//! `wavenet::PreparedWaveNet` and `lstm::PreparedLstm` now that both of FR-NAM-020's Must
//! architectures are implemented, instead of a direct re-export of the WaveNet type.
//!
//! # Why an enum, and why `namir-engine` needs zero changes for it
//!
//! `namir-engine`'s `stages/nam.rs` imports `namir_nam::{NamState, PreparedNam}` and calls
//! `process_block`/`new_state`/`latency_samples`/`metadata`/`sample_rate` plus the free function
//! `namir_nam::load` — but it never matches on `PreparedNam`'s internal structure (confirmed by
//! reading that file: every use is a method call through `&PreparedNam`/`&mut NamState`, never a
//! destructure). That means the exact representation behind these two names is free to change as
//! long as the names and method signatures stay put, which is what makes an enum a genuinely
//! zero-cost design choice here rather than a compromise: [`PreparedNam`] and [`NamState`] below
//! wrap a private inner enum (not a `pub enum` with public variants — see the fields' doc
//! comments for why), forwarding every method to whichever architecture is active. Nothing
//! outside this crate can observe the wrapping at all.
//!
//! `PreparedNam::from_file(&NamFile)` is kept too, with its original WaveNet-only behavior
//! unchanged: `NamFile` can only ever represent the WaveNet config shape (see `file.rs`'s
//! "Two file shapes" doc comment), so a method that takes one has nothing else it *could* build.
//! `namir-engine`'s and this crate's own tests that construct a `NamFile` directly and call this
//! keep working unmodified.

use serde::Deserialize;

use namir_core::SampleRate;

use crate::error_codes::{self, NamLoadError};
use crate::file::{self, LstmFile, NamFile};
use crate::lstm::{LstmState, PreparedLstm};
use crate::wavenet::{PreparedWaveNet, WaveNetState};

/// Which architecture a loaded model is. Not `pub`: nothing outside this module needs to
/// distinguish the two (see this module's doc comment) — the whole point of wrapping this in
/// [`PreparedNam`] rather than exposing it directly is that callers only ever see method calls.
enum Architecture {
    WaveNet(PreparedWaveNet),
    Lstm(PreparedLstm),
}

/// FR-NAM-020: a loaded, validated, ready-to-run model of either Must architecture (WaveNet or
/// LSTM). See this module's doc comment for why this is a thin wrapper rather than a direct
/// re-export of one architecture's type.
pub struct PreparedNam(Architecture);

/// The state variant matching whichever [`Architecture`] a [`PreparedNam`] holds.
enum StateArchitecture {
    WaveNet(WaveNetState),
    Lstm(LstmState),
}

/// Per-instance mutable inference state (D-9.1) for whichever architecture the [`PreparedNam`]
/// it was built from ([`PreparedNam::new_state`]) holds. Never shared across instances.
pub struct NamState(StateArchitecture);

impl PreparedNam {
    /// Builds a `PreparedNam` from an already-parsed [`NamFile`] — see this module's doc comment
    /// for why this can only ever produce the WaveNet variant, and why that's not a limitation in
    /// practice: keeps the exact signature and behavior every existing caller (this crate's own
    /// tests, `namir-engine`'s) already depends on.
    pub fn from_file(nam: &NamFile) -> Result<Self, NamLoadError> {
        Ok(PreparedNam(Architecture::WaveNet(
            PreparedWaveNet::from_file(nam)?,
        )))
    }

    /// FR-NAM-080: model metadata (name, `modeled_by`, gear/tone type, description).
    pub fn metadata(&self) -> &crate::file::NamMetadata {
        match &self.0 {
            Architecture::WaveNet(p) => p.metadata(),
            Architecture::Lstm(p) => p.metadata(),
        }
    }

    /// FR-NAM-090: the model's declared integrated loudness (LUFS), or `None` when the source
    /// file's `metadata.loudness` was absent or `null` -- see
    /// `wavenet::PreparedWaveNet::loudness_lufs`'s doc comment for exactly which files that
    /// covers. `namir-engine`'s Nam stage reads this to compute its normalisation gain; it applies
    /// zero when this is `None` rather than guessing a value.
    pub fn loudness_lufs(&self) -> Option<f32> {
        match &self.0 {
            Architecture::WaveNet(p) => p.loudness_lufs(),
            Architecture::Lstm(p) => p.loudness_lufs(),
        }
    }

    /// The model's declared sample rate (or the 48 kHz default if the file omitted it).
    pub fn sample_rate(&self) -> SampleRate {
        match &self.0 {
            Architecture::WaveNet(p) => p.sample_rate(),
            Architecture::Lstm(p) => p.sample_rate(),
        }
    }

    /// FR-NAM-110: processing latency in samples. Both architectures are causal and
    /// block-preserving (see each module's own doc comment), so this is always zero today; kept
    /// as a per-instance call rather than a constant since a future architecture with real
    /// look-ahead would need to report a nonzero value here without changing this signature.
    pub fn latency_samples(&self) -> u32 {
        match &self.0 {
            Architecture::WaveNet(p) => p.latency_samples(),
            Architecture::Lstm(p) => p.latency_samples(),
        }
    }

    /// `max_block_size` is the largest block size this state will ever be asked to process.
    pub fn new_state(&self, max_block_size: usize) -> NamState {
        NamState(match &self.0 {
            Architecture::WaveNet(p) => StateArchitecture::WaveNet(p.new_state(max_block_size)),
            Architecture::Lstm(p) => StateArchitecture::Lstm(p.new_state(max_block_size)),
        })
    }

    /// The allocation-free RT-path entry point; forwards to whichever architecture is active.
    ///
    /// Panics if `state` was not built from *this* `PreparedNam` (via [`PreparedNam::new_state`])
    /// — mismatched architecture variants, same as `wavenet::PreparedWaveNet::process_block`'s
    /// own panic for an oversized block: a call-site programming error, never reachable from
    /// untrusted `.nam` file content, since nothing in this crate's public API can hand a caller
    /// a `NamState` whose variant disagrees with the `PreparedNam` it was built from.
    pub fn process_block(&self, state: &mut NamState, input: &[f32], out: &mut [f32]) {
        match (&self.0, &mut state.0) {
            (Architecture::WaveNet(p), StateArchitecture::WaveNet(s)) => {
                p.process_block(s, input, out)
            }
            (Architecture::Lstm(p), StateArchitecture::Lstm(s)) => p.process_block(s, input, out),
            _ => panic!(
                "NamState architecture does not match this PreparedNam — states must come from \
                 this same instance's new_state()"
            ),
        }
    }

    /// Convenience wrapper over `process_block` that allocates its own output buffer.
    /// **Not RT-safe** — for tests, tools, and other non-audio-thread callers only.
    pub fn process(&self, state: &mut NamState, input: &[f32]) -> Vec<f32> {
        match (&self.0, &mut state.0) {
            (Architecture::WaveNet(p), StateArchitecture::WaveNet(s)) => p.process(s, input),
            (Architecture::Lstm(p), StateArchitecture::Lstm(s)) => p.process(s, input),
            _ => panic!(
                "NamState architecture does not match this PreparedNam — states must come from \
                 this same instance's new_state()"
            ),
        }
    }

    /// Issue #172: applies a container-declared `sample_rate` to the loaded submodel when the
    /// submodel itself omitted one. `pub(crate)`: only `load_slimmable_container` calls it.
    pub(crate) fn set_sample_rate(&mut self, sample_rate: SampleRate) {
        match &mut self.0 {
            Architecture::WaveNet(p) => p.set_sample_rate(sample_rate),
            Architecture::Lstm(p) => p.set_sample_rate(sample_rate),
        }
    }

    /// Issue #172: fills any empty/`None` metadata field from the container's metadata, leaving
    /// the loaded submodel's own non-empty fields untouched — the same submodel-first,
    /// container-fallback resolution `probe::probe_metadata` applies.
    pub(crate) fn merge_metadata(&mut self, container: &crate::file::NamMetadata) {
        match &mut self.0 {
            Architecture::WaveNet(p) => p.merge_metadata(container),
            Architecture::Lstm(p) => p.merge_metadata(container),
        }
    }
}

/// Combines architecture sniffing, JSON-shape parsing, and semantic validation: the one function
/// P6 calls "the one hardened place" `.nam` bytes go through end to end, from raw bytes to a
/// validated, ready-to-run model of either architecture.
///
/// `file::sniff_architecture` reads only the `architecture` field before deciding which of
/// `NamFile`/`LstmFile`'s shapes to parse the rest of the document as (see `file.rs`'s "Two file
/// shapes" doc comment for why there are two shapes rather than one).
pub fn load(bytes: &[u8]) -> Result<PreparedNam, NamLoadError> {
    let architecture = file::sniff_architecture(bytes)?;
    match architecture.as_str() {
        "WaveNet" => {
            let file = NamFile::parse(bytes)?;
            Ok(PreparedNam(Architecture::WaveNet(
                PreparedWaveNet::from_file(&file)?,
            )))
        }
        "LSTM" => {
            let file = LstmFile::parse(bytes)?;
            Ok(PreparedNam(Architecture::Lstm(PreparedLstm::from_file(
                &file,
            )?)))
        }
        "SlimmableContainer" => load_slimmable_container(bytes),
        other => Err(NamLoadError {
            code: error_codes::UNSUPPORTED_ARCHITECTURE,
            detail: format!("architecture: {other:?}"),
        }),
    }
}

fn load_slimmable_container(bytes: &[u8]) -> Result<PreparedNam, NamLoadError> {
    let file = file::ContainerFile::parse(bytes)?;

    if file.config.submodels.is_empty() {
        return Err(NamLoadError {
            code: error_codes::INCONSISTENT_CONFIGURATION,
            detail: "config.submodels is empty".to_string(),
        });
    }

    // Issue #172 (NFR-SEC-020 memory bound): bound the submodel count before any per-submodel
    // scan. Only the selected (last) submodel is ever fully parsed; every other submodel is read
    // only through the weight-free `SubmodelPeek` below, so the ceiling is what keeps a hostile
    // container from forcing arbitrarily many scans.
    crate::shared::check_max(
        file.config.submodels.len(),
        crate::shared::MAX_SUBMODELS,
        "config.submodels.len()",
    )?;

    let mut prev_max = f64::NEG_INFINITY;
    let mut common_sample_rate: Option<u32> = None;
    for (i, entry) in file.config.submodels.iter().enumerate() {
        if !entry.max_value.is_finite() {
            return Err(NamLoadError {
                code: error_codes::NON_FINITE_VALUE,
                detail: format!("submodel {i} max_value is not finite"),
            });
        }
        if entry.max_value <= prev_max {
            return Err(NamLoadError {
                code: error_codes::INCONSISTENT_CONFIGURATION,
                detail: format!(
                    "submodels must be sorted by strictly ascending max_value (index {i} has max_value {} <= previous {prev_max})",
                    entry.max_value
                ),
            });
        }
        prev_max = entry.max_value;

        let peek = peek_submodel(&entry.model, i)?;
        if peek.architecture.as_deref() == Some("SlimmableContainer") {
            // Issue #172: without this explicit check, a submodel that is itself a container
            // would recurse through `load` indefinitely on a maliciously nested file. Nested
            // containers are out of scope, so they are rejected up front.
            return Err(NamLoadError {
                code: error_codes::UNSUPPORTED_CONFIGURATION,
                detail: "nested SlimmableContainer is not supported".to_string(),
            });
        }
        if let Some(sr) = peek.sample_rate {
            if sr == 0 {
                return Err(NamLoadError {
                    code: error_codes::INVALID_SAMPLE_RATE,
                    detail: format!("submodel {i} declares sample_rate 0 Hz"),
                });
            }
            if let Some(existing) = common_sample_rate {
                if existing != sr {
                    return Err(NamLoadError {
                        code: error_codes::INCONSISTENT_CONFIGURATION,
                        detail: format!(
                            "submodels have mismatched sample rates: {existing} vs {sr}"
                        ),
                    });
                }
            } else {
                common_sample_rate = Some(sr);
            }
        }
    }

    let last_max = file.config.submodels.last().unwrap().max_value;
    if last_max < 1.0 {
        return Err(NamLoadError {
            code: error_codes::INCONSISTENT_CONFIGURATION,
            detail: format!("last submodel max_value must be >= 1.0, found {last_max}"),
        });
    }

    if let Some(top_sr) = file.sample_rate {
        if top_sr == 0 {
            return Err(NamLoadError {
                code: error_codes::INVALID_SAMPLE_RATE,
                detail: "container declares sample_rate 0 Hz".to_string(),
            });
        }
        if let Some(sub_sr) = common_sample_rate
            && top_sr != sub_sr
        {
            return Err(NamLoadError {
                code: error_codes::INCONSISTENT_CONFIGURATION,
                detail: format!(
                    "container sample_rate ({top_sr}) does not match submodel sample_rate ({sub_sr})"
                ),
            });
        }
    }

    // Everything above has validated the container itself; now the selected (last) submodel is
    // parsed through the regular `load` path, feeding it the submodel's own raw bytes straight
    // out of the `RawValue` (zero re-serialization). Sample-rate and metadata resolution happen
    // after, so a submodel that omits `sample_rate`/metadata still reports the container's.
    let last = file.config.submodels.last().unwrap();
    let mut nam = load(last.model.get().as_bytes())?;
    if let Some(top_sr) = file.sample_rate {
        let sr = SampleRate::new(top_sr).expect("nonzero: top_sr == 0 is rejected above");
        nam.set_sample_rate(sr);
    }
    nam.merge_metadata(&file.metadata);
    Ok(nam)
}

/// Issue #172: reads only the fields a `SlimmableContainer` loader needs from a submodel's raw
/// JSON — its `architecture` (to reject a nested container) and `sample_rate` (consistency
/// across submodels) — without materializing the submodel's `weights` (up to ~10M `f32` in a
/// large WaveNet). Unknown fields are skipped by serde without allocating; only the selected
/// (last) submodel is ever fully parsed, by the recursive `load` call.
#[derive(Debug, Deserialize)]
struct SubmodelPeek {
    #[serde(default)]
    architecture: Option<String>,
    #[serde(default)]
    sample_rate: Option<u32>,
}

/// Parses a submodel entry's raw JSON as a [`SubmodelPeek`]. A non-object `model` (JSON allows
/// any value) fails the struct parse and is reported as `MALFORMED_JSON`, the same code the
/// pre-`RawValue` implementation used for "submodel model is not a JSON object".
fn peek_submodel(
    raw: &serde_json::value::RawValue,
    index: usize,
) -> Result<SubmodelPeek, NamLoadError> {
    serde_json::from_str(raw.get()).map_err(|e| NamLoadError {
        code: error_codes::MALFORMED_JSON,
        detail: format!("submodel {index} model is not a valid JSON object: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PreparedNam` deliberately has no `Debug` impl (same reasoning as
    /// `wavenet::PreparedWaveNet`'s — nothing in this crate's public API needs one), so
    /// `Result::unwrap_err` can't be used directly on `load`'s `Result`. Mirrors
    /// `wavenet.rs`'s own `expect_err` test helper.
    fn expect_err(result: Result<PreparedNam, NamLoadError>) -> NamLoadError {
        match result {
            Ok(_) => panic!("expected load to reject this input"),
            Err(e) => e,
        }
    }

    fn minimal_wavenet_json() -> Vec<u8> {
        // 7 weights (rechannel=1, [dilated_w=1, dilated_b=1, mixin=1, residual_w=1,
        // residual_b=1]=5, head_rechannel=1) plus a trailing head_scale float, mirroring
        // `wavenet.rs`'s own `minimal_valid_file` test fixture shape.
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
            "weights": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5],
            "sample_rate": 48000
        })
        .to_string()
        .into_bytes()
    }

    fn minimal_lstm_json() -> Vec<u8> {
        // num_layers=1, input_size=1, hidden_size=1: W(4x2)=8, b=4, h0=1, c0=1, head_weight=1,
        // head_bias=1 => 16 floats.
        let weights = vec![0.01f32; 16];
        serde_json::json!({
            "architecture": "LSTM",
            "config": {
                "num_layers": 1,
                "input_size": 1,
                "hidden_size": 1
            },
            "weights": weights,
            "sample_rate": 48000
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn load_dispatches_to_wavenet() {
        let prepared = load(&minimal_wavenet_json()).expect("wavenet file should load");
        assert_eq!(prepared.latency_samples(), 0);
        assert_eq!(prepared.sample_rate().hz(), 48_000);
    }

    /// FR-NAM-090: `loudness_lufs()` forwards through `PreparedNam`'s architecture dispatch (D-8.2's
    /// enum-wrapping shape) for both Must architectures, and reports `None` when the source file
    /// never declared a value (this fixture's `minimal_wavenet_json`/`minimal_lstm_json` omit
    /// `metadata` entirely).
    #[test]
    fn loudness_lufs_forwards_through_both_architectures() {
        let wavenet = load(&minimal_wavenet_json()).unwrap();
        assert_eq!(wavenet.loudness_lufs(), None);

        let mut wavenet_value: serde_json::Value =
            serde_json::from_slice(&minimal_wavenet_json()).unwrap();
        wavenet_value["metadata"] = serde_json::json!({ "loudness": -16.5 });
        let bytes = serde_json::to_vec(&wavenet_value).unwrap();
        let wavenet_with_loudness = load(&bytes).unwrap();
        assert_eq!(wavenet_with_loudness.loudness_lufs(), Some(-16.5));

        let lstm = load(&minimal_lstm_json()).unwrap();
        assert_eq!(lstm.loudness_lufs(), None);

        let mut lstm_value: serde_json::Value =
            serde_json::from_slice(&minimal_lstm_json()).unwrap();
        lstm_value["metadata"] = serde_json::json!({ "loudness": -22.1 });
        let bytes = serde_json::to_vec(&lstm_value).unwrap();
        let lstm_with_loudness = load(&bytes).unwrap();
        assert_eq!(lstm_with_loudness.loudness_lufs(), Some(-22.1));
    }

    #[test]
    fn load_dispatches_to_lstm() {
        let prepared = load(&minimal_lstm_json()).expect("lstm file should load");
        assert_eq!(prepared.latency_samples(), 0);
        assert_eq!(prepared.sample_rate().hz(), 48_000);
    }

    // trace: FR-NAM-040
    #[test]
    fn load_rejects_unknown_architecture() {
        let json = serde_json::json!({"architecture": "RNN"});
        let bytes = serde_json::to_vec(&json).unwrap().to_vec();
        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_ARCHITECTURE.id);
    }

    /// One JSON object's keys overwrite/insert into another's. `serde_json::Value` has no built-in
    /// merge; every table-driven case below needs "the minimal valid layer array, but with this
    /// one key added or changed," and this is the small helper that expresses that without 24
    /// near-duplicate full JSON literals.
    fn merge_object(
        mut base: serde_json::Value,
        overrides: serde_json::Value,
    ) -> serde_json::Value {
        let (Some(base_obj), serde_json::Value::Object(over_obj)) =
            (base.as_object_mut(), overrides)
        else {
            panic!("merge_object: both arguments must be JSON objects");
        };
        for (k, v) in over_obj {
            base_obj.insert(k, v);
        }
        base
    }

    /// A single minimal, otherwise-valid WaveNet layer array — the same shape
    /// `minimal_wavenet_json` uses, as a `Value` so test cases can merge one or two keys into it.
    fn minimal_layer_array_json() -> serde_json::Value {
        serde_json::json!({
            "input_size": 1,
            "condition_size": 1,
            "head_size": 1,
            "channels": 1,
            "kernel_size": 1,
            "dilations": [1],
            "activation": "Tanh",
            "gated": false,
            "head_bias": false
        })
    }

    /// Builds a full `.nam` WaveNet document from `config_overrides` (merged into `config`) and
    /// `layer_overrides` (merged into `config.layers[0]`), keeping every other field at
    /// `minimal_wavenet_json`'s known-valid values, including its weight count — every case below
    /// changes only *feature presence*, never a dimension, so the same 8-float weight array stays
    /// valid throughout (a case that were to change a dimension would need its own weight count,
    /// exactly like `wavenet.rs`'s `weight_count_for`).
    fn wavenet_json(
        config_overrides: serde_json::Value,
        layer_overrides: serde_json::Value,
    ) -> Vec<u8> {
        let layer = merge_object(minimal_layer_array_json(), layer_overrides);
        let config = merge_object(
            serde_json::json!({ "layers": [layer], "head_scale": 0.5, "head": null }),
            config_overrides,
        );
        serde_json::json!({
            "architecture": "WaveNet",
            "config": config,
            "weights": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5],
            "sample_rate": 48000
        })
        .to_string()
        .into_bytes()
    }

    /// FR-NAM-140 (Must): "A model file whose declared architecture, or whose configuration
    /// within a supported architecture, Namir does not support shall be rejected with an error
    /// that names the unsupported feature. That error shall be a distinct catalogue entry ...
    /// from the one reported for a malformed or truncated file." Table-driven over every member
    /// the requirement's "architecture, **or** ... configuration" quantifies over (D-23.1's first
    /// question): an unsupported architecture string, D-9.12's two top-level A2 rejections
    /// (`condition_dsp`, `in_channels`), and every **permanently** out-of-scope feature
    /// `wavenet::reject_unsupported_layer_features` rejects (D-9.12). M10 (A2, Steps A1-A4)
    /// implemented `kernel_sizes`, `bottleneck`, the nested `head`, and object/per-layer
    /// `activation` — those cases moved out of this table (they no longer belong in an "unsupported
    /// features" test; `wavenet.rs`'s own unit tests cover them loading successfully instead) — and
    /// narrowed `layer1x1`'s case from "any present object" to "present and inactive or grouped"
    /// (a present, active, `groups: 1` `layer1x1` is core A2's ordinary, supported shape). Each case
    /// asserts both halves the requirement's own `Verify: U` method names (D-23.1's second
    /// question): the error id differs from `MALFORMED_JSON`'s **and** `detail` names the offending
    /// key — asserting only the first would leave "names the unsupported feature" untested and this
    /// tag would be a `trace-partial`, not a plain one.
    ///
    /// Issue #46 added one more member to the set this quantifies over — a model declaring more
    /// than one output channel, i.e. a last layer array whose head is wider than 1 — and it is
    /// *not* in the table below, because unlike every case here it changes a dimension and so
    /// needs its own weight count. It is covered, with both the same assertions, by
    /// [`documents_that_used_to_load_and_then_misbehave_on_the_audio_thread_are_rejected`], which
    /// carries the same tag: the two tests jointly span the set, neither alone does (the same
    /// split, for the same reason, that FR-NAM-030's pair of golden-reference tests uses). Issue
    /// #47's `layers[0].input_size` is not a member at all — a self-contradictory file is
    /// `INCONSISTENT_CONFIGURATION`, which this requirement's text does not cover.
    // trace: FR-NAM-140
    #[test]
    fn unsupported_features_are_named_and_distinct_from_malformed() {
        let none = || serde_json::json!({});

        // (case name, top-level config overrides, layer-array overrides, substring `detail` must
        // contain — the actual naming of the unsupported feature FR-NAM-140 requires).
        let layer_cases: Vec<(&str, serde_json::Value, &str)> = vec![
            ("gated true", serde_json::json!({"gated": true}), "gated"),
            (
                "gating_mode",
                serde_json::json!({"gating_mode": "gated"}),
                "gating_mode",
            ),
            (
                "secondary_activation",
                serde_json::json!({"secondary_activation": "Sigmoid"}),
                "secondary_activation",
            ),
            (
                "groups_input",
                serde_json::json!({"groups_input": 2}),
                "groups_input",
            ),
            (
                "groups_input_mixin",
                serde_json::json!({"groups_input_mixin": 2}),
                "groups_input_mixin",
            ),
            (
                "layer1x1 inactive",
                serde_json::json!({"layer1x1": {"active": false, "groups": 1}}),
                "layer1x1",
            ),
            (
                "layer1x1 grouped",
                serde_json::json!({"layer1x1": {"active": true, "groups": 2}}),
                "layer1x1",
            ),
            (
                "head1x1 active",
                serde_json::json!({"head1x1": {"active": true, "groups": 1, "out_channels": 1}}),
                "head1x1",
            ),
            (
                "slimmable",
                serde_json::json!({"slimmable": {"method": "slice_channels_uniform"}}),
                "slimmable",
            ),
            (
                "conv_pre_film active",
                serde_json::json!({"conv_pre_film": {"active": true}}),
                "conv_pre_film",
            ),
            (
                "conv_post_film active",
                serde_json::json!({"conv_post_film": {"active": true}}),
                "conv_post_film",
            ),
            (
                "input_mixin_pre_film active",
                serde_json::json!({"input_mixin_pre_film": {"active": true}}),
                "input_mixin_pre_film",
            ),
            (
                "input_mixin_post_film active",
                serde_json::json!({"input_mixin_post_film": {"active": true}}),
                "input_mixin_post_film",
            ),
            (
                "activation_pre_film active",
                serde_json::json!({"activation_pre_film": {"active": true}}),
                "activation_pre_film",
            ),
            (
                "activation_post_film active",
                serde_json::json!({"activation_post_film": {"active": true}}),
                "activation_post_film",
            ),
            (
                "layer1x1_post_film active",
                serde_json::json!({"layer1x1_post_film": {"active": true}}),
                "layer1x1_post_film",
            ),
            (
                "head1x1_post_film active",
                serde_json::json!({"head1x1_post_film": {"active": true}}),
                "head1x1_post_film",
            ),
        ];
        for (name, layer_overrides, expect_substring) in layer_cases {
            let bytes = wavenet_json(none(), layer_overrides);
            let err = expect_err(load(&bytes));
            assert_ne!(
                err.code.id,
                error_codes::MALFORMED_JSON.id,
                "{name}: a well-formed-but-unsupported file must not be reported as malformed"
            );
            assert!(
                err.detail.contains(expect_substring),
                "{name}: detail {:?} does not name {expect_substring:?}",
                err.detail
            );
        }

        let top_level_cases: Vec<(&str, serde_json::Value, &str)> = vec![
            (
                "condition_dsp",
                serde_json::json!({"condition_dsp": {"architecture": "WaveNet"}}),
                "condition_dsp",
            ),
            (
                "in_channels",
                serde_json::json!({"in_channels": 2}),
                "in_channels",
            ),
        ];
        for (name, config_overrides, expect_substring) in top_level_cases {
            let bytes = wavenet_json(config_overrides, none());
            let err = expect_err(load(&bytes));
            assert_ne!(
                err.code.id,
                error_codes::MALFORMED_JSON.id,
                "{name}: a well-formed-but-unsupported file must not be reported as malformed"
            );
            assert!(
                err.detail.contains(expect_substring),
                "{name}: detail {:?} does not name {expect_substring:?}",
                err.detail
            );
        }

        // The architecture half of FR-NAM-140's "architecture, or ... configuration": a
        // recognized-but-unsupported architecture string is also distinct from `MALFORMED_JSON`
        // and names the offending value.
        let bytes = serde_json::json!({"architecture": "RNN"})
            .to_string()
            .into_bytes();
        let err = expect_err(load(&bytes));
        assert_ne!(err.code.id, error_codes::MALFORMED_JSON.id);
        assert!(err.detail.contains("RNN"));
    }

    /// Issues #46, #47 and #49: three `.nam` documents a user can really hold, each of which
    /// **loaded successfully** and then misbehaved on the audio thread, reached here through the
    /// same bytes-to-model path a real file takes (`load`), not through a hand-built `NamFile`.
    ///
    /// What each did before the load-time checks that now reject them:
    ///
    /// | document | observed |
    /// |---|---|
    /// | `head_size: 4` on the only (therefore last) layer array | `process_block` panicked: `range end index 32 out of range for slice of length 8` |
    /// | `input_size: 2` on the first layer array | `process_block` panicked: `range end index 16 out of range for slice of length 8` |
    /// | a `1e40` weight (in `f64` range, out of `f32` range, so `f32::INFINITY` after serde) | loaded, and `process` returned a non-finite block |
    ///
    /// A panic inside `process_block` is a panic on the host's audio thread — in `namir-clap`,
    /// the user's whole DAW session. None of the three is defensible there (the RT path may not
    /// allocate, and has no way to report anything), so all three are rejected here, at load, off
    /// the audio thread, where an error costs nothing. The weight counts below are exact for each
    /// mutated shape, so each document is genuinely *loadable-looking* — a weight-count mismatch
    /// would prove nothing about these checks.
    ///
    /// Tagged FR-NAM-140 for its first case only — a model with more than one output channel is
    /// an unsupported feature; see [`unsupported_features_are_named_and_distinct_from_malformed`]'s
    /// own doc comment for how the two tests split that requirement's set between them. The other
    /// two cases are a self-contradictory file and a corrupted one, neither of which FR-NAM-140
    /// covers.
    // trace: FR-NAM-140
    #[test]
    fn documents_that_used_to_load_and_then_misbehave_on_the_audio_thread_are_rejected() {
        // `1e40` is written as JSON text (an `f64`, in range there), not as an `f32`: it cannot
        // be a Rust `f32` literal at all (rustc's `overflowing_literals` lint refuses `1e40f32`
        // outright), and it cannot even be *re-serialized* from an `f32::INFINITY` value, since
        // `serde_json` writes every non-finite float as `null`. Only the text form reproduces
        // what a real file carries -- which is exactly why this slipped through: the value is
        // perfectly ordinary JSON, and becomes infinite only on the way into an `f32`.
        assert!(
            serde_json::from_str::<f32>("1e40").unwrap().is_infinite(),
            "serde_json deserializes the JSON number 1e40 into f32::INFINITY"
        );
        let mut infinite_weights = vec![serde_json::json!(0.0); 8];
        infinite_weights[6] = serde_json::json!(1e40);
        infinite_weights[7] = serde_json::json!(0.5);

        // (case, layer-array overrides, weights, expected code, substring `detail` must name)
        let cases: Vec<(&str, serde_json::Value, serde_json::Value, &str, &str)> = vec![
            (
                "issue #46: last layer array's head_size > 1",
                serde_json::json!({ "head_size": 4 }),
                // exact for this shape: 6 + head_size(4) * channels(1)
                serde_json::json!(vec![0.0f32; 10]),
                error_codes::UNSUPPORTED_CONFIGURATION.id,
                "head",
            ),
            (
                "issue #47: first layer array's input_size > 1",
                serde_json::json!({ "input_size": 2 }),
                // exact for this shape: rechannel is channels(1) * input_size(2)
                serde_json::json!(vec![0.0f32; 8]),
                // Inconsistent, not unsupported: this document declares a one-channel input (by
                // omitting `in_channels`) and a first layer array expecting two. See
                // `wavenet::PreparedWaveNet::from_file`'s comment at the check.
                error_codes::INCONSISTENT_CONFIGURATION.id,
                "input_size",
            ),
            (
                "issue #49: a non-finite weight",
                serde_json::json!({}),
                serde_json::Value::Array(infinite_weights),
                error_codes::NON_FINITE_VALUE.id,
                "weights[6]",
            ),
        ];

        for (case, layer_overrides, weights, expected_code, expect_substring) in cases {
            let layer = merge_object(minimal_layer_array_json(), layer_overrides);
            let bytes = serde_json::json!({
                "architecture": "WaveNet",
                "config": { "layers": [layer], "head_scale": 0.5, "head": null },
                "weights": weights,
                "sample_rate": 48000
            })
            .to_string()
            .into_bytes();

            let err = expect_err(load(&bytes));
            assert_eq!(err.code.id, expected_code, "{case}: wrong catalogue code");
            assert!(
                err.detail.contains(expect_substring),
                "{case}: detail {:?} does not name {expect_substring:?}",
                err.detail
            );
        }

        // The positive control: the same document, unmutated, still loads and still processes —
        // so the three rejections above are about the mutations, not about this shape.
        let good = load(&minimal_wavenet_json()).expect("the unmutated document still loads");
        let mut state = good.new_state(4);
        assert_eq!(good.process(&mut state, &[0.1, 0.2, 0.3, 0.4]).len(), 4);
    }

    /// Issue #49's LSTM half: `lstm::PreparedLstm::from_file` has the same check, reached through
    /// the same `load` path. LSTM has no `head_scale`, so `weights` is the whole of it.
    #[test]
    fn a_non_finite_lstm_weight_is_rejected_at_load() {
        let mut value: serde_json::Value = serde_json::from_slice(&minimal_lstm_json()).unwrap();
        value["weights"][3] = serde_json::json!(1e40);
        let bytes = serde_json::to_vec(&value).unwrap();
        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::NON_FINITE_VALUE.id);
        assert!(
            err.detail.contains("weights[3]"),
            "detail: {:?}",
            err.detail
        );
    }

    #[test]
    fn wavenet_and_lstm_models_both_process_through_the_same_api() {
        let wavenet = load(&minimal_wavenet_json()).unwrap();
        let mut wavenet_state = wavenet.new_state(4);
        let wavenet_out = wavenet.process(&mut wavenet_state, &[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(wavenet_out.len(), 4);

        let lstm = load(&minimal_lstm_json()).unwrap();
        let mut lstm_state = lstm.new_state(4);
        let lstm_out = lstm.process(&mut lstm_state, &[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(lstm_out.len(), 4);
    }

    #[test]
    #[should_panic(expected = "NamState architecture does not match")]
    fn process_block_panics_on_mismatched_state_architecture() {
        let wavenet = load(&minimal_wavenet_json()).unwrap();
        let lstm = load(&minimal_lstm_json()).unwrap();
        let mut lstm_state = lstm.new_state(4);
        let mut out = vec![0.0f32; 4];
        wavenet.process_block(&mut lstm_state, &[0.1, 0.2, 0.3, 0.4], &mut out);
    }

    fn minimal_container_json() -> Vec<u8> {
        serde_json::json!({
            "version": "0.7.0",
            "architecture": "SlimmableContainer",
            "config": {
                "submodels": [
                    {
                        "max_value": 0.5,
                        "model": {
                            "version": "0.7.0",
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
                            "weights": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5],
                            "sample_rate": 48000,
                            "metadata": {
                                "name": "Lite Version",
                                "loudness": -15.0
                            }
                        }
                    },
                    {
                        "max_value": 1.0,
                        "model": {
                            "version": "0.7.0",
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
                            "weights": [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.5],
                            "sample_rate": 48000,
                            "metadata": {
                                "name": "Full Version",
                                "loudness": -12.0
                            }
                        }
                    }
                ]
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn slimmable_container_loads_and_runs_last_submodel() {
        let bytes = minimal_container_json();
        let prepared = load(&bytes).expect("container should load");
        assert_eq!(prepared.sample_rate().hz(), 48000);
        assert_eq!(prepared.metadata().name, "Full Version");
        assert_eq!(prepared.loudness_lufs(), Some(-12.0));

        let mut state = prepared.new_state(4);
        let out = prepared.process(&mut state, &[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn slimmable_container_with_lstm_submodel_loads_and_runs() {
        let bytes = serde_json::json!({
            "version": "0.7.0",
            "architecture": "SlimmableContainer",
            "config": {
                "submodels": [
                    {
                        "max_value": 1.0,
                        "model": {
                            "version": "0.5.4",
                            "architecture": "LSTM",
                            "config": {
                                "num_layers": 1,
                                "input_size": 1,
                                "hidden_size": 1
                            },
                            "weights": vec![0.01f32; 16],
                            "sample_rate": 44100,
                            "metadata": {
                                "name": "LSTM Container Submodel"
                            }
                        }
                    }
                ]
            }
        })
        .to_string()
        .into_bytes();

        let prepared = load(&bytes).expect("container with LSTM submodel should load");
        assert_eq!(prepared.sample_rate().hz(), 44100);
        assert_eq!(prepared.metadata().name, "LSTM Container Submodel");
        let mut state = prepared.new_state(2);
        assert_eq!(prepared.process(&mut state, &[0.1, 0.2]).len(), 2);
    }

    #[test]
    fn slimmable_container_rejects_empty_submodels() {
        let bytes = serde_json::json!({
            "architecture": "SlimmableContainer",
            "config": { "submodels": [] }
        })
        .to_string()
        .into_bytes();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
        assert!(err.detail.contains("config.submodels is empty"));
    }

    #[test]
    fn slimmable_container_rejects_non_finite_max_value() {
        let bytes = br#"{"architecture": "SlimmableContainer", "config": {"submodels": [{"max_value": 1e400, "model": {"architecture": "WaveNet"}}]}}"#;
        let err = expect_err(load(bytes));
        assert!(
            err.code.id == error_codes::NON_FINITE_VALUE.id
                || err.code.id == error_codes::MALFORMED_JSON.id
        );
    }

    #[test]
    fn slimmable_container_rejects_unsorted_max_values() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["config"]["submodels"][0]["max_value"] = serde_json::json!(1.0);
        value["config"]["submodels"][1]["max_value"] = serde_json::json!(0.5);
        let bytes = serde_json::to_vec(&value).unwrap();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
        assert!(err.detail.contains("strictly ascending"));
    }

    #[test]
    fn slimmable_container_rejects_equal_max_values() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["config"]["submodels"][0]["max_value"] = serde_json::json!(1.0);
        value["config"]["submodels"][1]["max_value"] = serde_json::json!(1.0);
        let bytes = serde_json::to_vec(&value).unwrap();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
        assert!(err.detail.contains("strictly ascending"));
    }

    #[test]
    fn slimmable_container_rejects_last_max_value_less_than_one() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["config"]["submodels"][0]["max_value"] = serde_json::json!(0.2);
        value["config"]["submodels"][1]["max_value"] = serde_json::json!(0.8);
        let bytes = serde_json::to_vec(&value).unwrap();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
        assert!(
            err.detail
                .contains("last submodel max_value must be >= 1.0")
        );
    }

    #[test]
    fn slimmable_container_rejects_mismatched_submodel_sample_rates() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["config"]["submodels"][0]["model"]["sample_rate"] = serde_json::json!(44100);
        value["config"]["submodels"][1]["model"]["sample_rate"] = serde_json::json!(48000);
        let bytes = serde_json::to_vec(&value).unwrap();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
        assert!(err.detail.contains("mismatched sample rates"));
    }

    #[test]
    fn slimmable_container_rejects_zero_sample_rate_in_submodel() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["config"]["submodels"][0]["model"]["sample_rate"] = serde_json::json!(0);
        let bytes = serde_json::to_vec(&value).unwrap();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::INVALID_SAMPLE_RATE.id);
        assert!(err.detail.contains("0 Hz"));
    }

    #[test]
    fn slimmable_container_rejects_container_sample_rate_mismatch() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["sample_rate"] = serde_json::json!(96000);
        let bytes = serde_json::to_vec(&value).unwrap();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
        assert!(err.detail.contains("container sample_rate"));
    }

    #[test]
    fn slimmable_container_forwards_submodel_unsupported_architecture() {
        let bytes = serde_json::json!({
            "architecture": "SlimmableContainer",
            "config": {
                "submodels": [
                    {
                        "max_value": 1.0,
                        "model": {
                            "architecture": "UnknownNet",
                            "config": {}
                        }
                    }
                ]
            }
        })
        .to_string()
        .into_bytes();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_ARCHITECTURE.id);
        assert!(err.detail.contains("UnknownNet"));
    }

    #[test]
    fn slimmable_container_forwards_submodel_unsupported_configuration() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["config"]["submodels"][1]["model"]["config"]["condition_dsp"] = serde_json::json!({});
        let bytes = serde_json::to_vec(&value).unwrap();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_CONFIGURATION.id);
        assert!(err.detail.contains("condition_dsp"));
    }

    /// Issue #172 review finding 1: with the container declaring `sample_rate: 44100` and the
    /// submodels omitting it, the loaded model must report 44100 Hz (matching the probe), not
    /// fall back to 48 kHz. The container's declaration is authoritative — the whole point of
    /// declaring it once at the top level instead of once per submodel.
    #[test]
    fn slimmable_container_propagates_container_sample_rate_to_rate_less_submodels() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["sample_rate"] = serde_json::json!(44100);
        for i in 0..2 {
            value["config"]["submodels"][i]["model"]
                .as_object_mut()
                .unwrap()
                .remove("sample_rate");
        }
        let bytes = serde_json::to_vec(&value).unwrap();

        let prepared = load(&bytes).expect("container with container-level rate should load");
        assert_eq!(prepared.sample_rate().hz(), 44_100);

        // Probe and load must resolve the same way (review finding 2's "identical resolution").
        let probe = crate::probe::probe_metadata(&bytes).unwrap();
        assert_eq!(probe.sample_rate, Some(44_100));
        assert_eq!(probe.sample_rate, Some(prepared.sample_rate().hz()));
    }

    /// Issue #172 review finding 1, submodel-declared side: when the submodels *do* declare a
    /// rate and the container says the same, the loaded model reports it (no double-default).
    /// This is the pre-existing behavior, pinned here so the propagation fix cannot regress it.
    #[test]
    fn slimmable_container_propagates_matching_submodel_sample_rate() {
        let bytes = minimal_container_json();
        let prepared = load(&bytes).expect("container should load");
        assert_eq!(prepared.sample_rate().hz(), 48_000);

        let probe = crate::probe::probe_metadata(&bytes).unwrap();
        assert_eq!(probe.sample_rate, Some(48_000));
    }

    /// Issue #172 review finding 2: metadata resolves submodel-first, container-fallback — a
    /// non-empty submodel field must keep overriding the container's, and an empty submodel
    /// field must be filled from the container's. The loaded `PreparedNam` and the probe must
    /// agree field by field.
    ///
    /// Fixture: submodel 1 ("Full Version", `loudness: -12.0`) overrides the container's name;
    /// every submodel metadata field the submodels leave empty is filled from the container's.
    #[test]
    fn slimmable_container_metadata_resolution_matches_probe() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["metadata"] = serde_json::json!({
            "name": "Container Model",
            "modeled_by": "Container Author",
            "gear_type": "Container Gear",
            "tone_type": "Container Tone",
            "description": "Container description",
            "loudness": -9.5
        });
        let bytes = serde_json::to_vec(&value).unwrap();

        let prepared = load(&bytes).expect("container should load");
        // Submodel fields win where non-empty...
        assert_eq!(prepared.metadata().name, "Full Version");
        assert_eq!(prepared.loudness_lufs(), Some(-12.0));
        // ...container fields fill the submodel's gaps.
        assert_eq!(prepared.metadata().modeled_by, "Container Author");
        assert_eq!(prepared.metadata().gear_type, "Container Gear");
        assert_eq!(prepared.metadata().tone_type, "Container Tone");
        assert_eq!(prepared.metadata().description, "Container description");

        let probe = crate::probe::probe_metadata(&bytes).unwrap();
        assert_eq!(probe.metadata.name, prepared.metadata().name);
        assert_eq!(probe.metadata.modeled_by, prepared.metadata().modeled_by);
        assert_eq!(probe.metadata.gear_type, prepared.metadata().gear_type);
        assert_eq!(probe.metadata.tone_type, prepared.metadata().tone_type);
        assert_eq!(probe.metadata.description, prepared.metadata().description);
        assert_eq!(probe.metadata.loudness, prepared.metadata().loudness);
    }

    /// Issue #172 review finding 3: a submodel that is itself a `SlimmableContainer` is
    /// rejected up front with `UNSUPPORTED_CONFIGURATION` — without this check, a nested
    /// container would recurse through `load` indefinitely on a maliciously nested file.
    #[test]
    fn slimmable_container_rejects_nested_container_submodel() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&minimal_container_json()).unwrap();
        value["config"]["submodels"][1]["model"]["architecture"] =
            serde_json::json!("SlimmableContainer");
        let bytes = serde_json::to_vec(&value).unwrap();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_CONFIGURATION.id);
        assert!(
            err.detail
                .contains("nested SlimmableContainer is not supported")
        );
    }

    /// Issue #172 review finding 3: `submodels.len() > MAX_SUBMODELS` is rejected with
    /// `DIMENSION_LIMIT_EXCEEDED` — the NFR-SEC-020 ceiling that bounds how many submodels a
    /// hostile container may force the loader to scan. 33 entries, one past the ceiling of 32.
    #[test]
    fn slimmable_container_rejects_more_than_max_submodels() {
        let submodels: Vec<serde_json::Value> = (0..=crate::shared::MAX_SUBMODELS)
            .map(|i| {
                serde_json::json!({
                    "max_value": i as f64,
                    "model": { "architecture": "WaveNet" }
                })
            })
            .collect();
        let bytes = serde_json::json!({
            "architecture": "SlimmableContainer",
            "config": { "submodels": submodels }
        })
        .to_string()
        .into_bytes();

        let err = expect_err(load(&bytes));
        assert_eq!(err.code.id, error_codes::DIMENSION_LIMIT_EXCEEDED.id);
        assert!(err.detail.contains("config.submodels.len()"));
    }

    /// The boundary the ceiling produces: exactly `MAX_SUBMODELS` submodels passes the ceiling
    /// check (each is minimal, so the load still gets past the peek and fails later on the
    /// unsupported `"WaveNet"` stub — the point is only that it is *not* a ceiling rejection).
    #[test]
    fn slimmable_container_accepts_exactly_max_submodels() {
        let submodels: Vec<serde_json::Value> = (0..crate::shared::MAX_SUBMODELS)
            .map(|i| {
                serde_json::json!({
                    "max_value": i as f64,
                    "model": { "architecture": "WaveNet", "config": {} }
                })
            })
            .collect();
        let bytes = serde_json::json!({
            "architecture": "SlimmableContainer",
            "config": { "submodels": submodels }
        })
        .to_string()
        .into_bytes();

        let err = expect_err(load(&bytes));
        assert_ne!(err.code.id, error_codes::DIMENSION_LIMIT_EXCEEDED.id);
    }
}
