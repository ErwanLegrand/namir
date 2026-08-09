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
        other => Err(NamLoadError {
            code: error_codes::UNSUPPORTED_ARCHITECTURE,
            detail: format!("architecture: {other:?}"),
        }),
    }
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
}
