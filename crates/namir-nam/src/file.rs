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
//! # M10 / D-9.12: one widened grammar, not a second parse path for A2
//!
//! A2 files declare `architecture: "WaveNet"` exactly as A1 files do (D-9.12), so `WaveNetConfig`/
//! `LayerArrayConfig` are widened **in place** to also accept A2's fields, rather than gaining a
//! sibling type the way LSTM did — a sibling here would need its own dispatch key, and A2 has none
//! (see this module doc's "Two file shapes" section above, which is why LSTM *does* get one). Every
//! field A2 adds is `Option<_>`/`Vec<_>` with `#[serde(default)]`, so a file that parses today keeps
//! parsing unchanged; `wavenet::PreparedWaveNet::from_file` is where the A1/A2 semantic branch
//! actually happens, not here.
//!
//! **Two rules keep this widening from becoming the mirror-image of the bug FR-NAM-140 exists to
//! fix** (a well-formed-but-unsupported file misreported as malformed, or a genuinely malformed one
//! misreported as merely unsupported):
//!
//! 1. `serde_json::Value` is used only for fields this crate never *reads* the contents of — the
//!    permanently-rejected set (`condition_dsp`, `slimmable`, `gating_mode`, `secondary_activation`).
//!    Presence and JSON kind (object vs. non-object) is all `wavenet.rs` inspects. Every field that
//!    *is* consumed (`kernel_sizes`, `bottleneck`, `head.*`, activation parameters) keeps a concrete
//!    type, so a wrong-typed value still fails at `serde` and is still reported as malformed.
//! 2. **No untagged enum below gets a catch-all variant.** An `Other(serde_json::Value)` arm on
//!    [`ActivationEntry`], for instance, would turn every activation *type* error (a bad
//!    `negative_slope`, say) into a false "unsupported feature" claim instead of "malformed". This
//!    rule does not get relaxed for convenience.
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
    /// A2: DSP input channel count. Absent means 1, matching the reference exporter's own
    /// `config.value("in_channels", 1)`. Namir supports only 1 (core-A2 scope, D-9.12); a present
    /// value other than 1 is rejected by `wavenet::PreparedWaveNet::from_file`, not here.
    #[serde(default)]
    pub in_channels: Option<usize>,
    /// A2: a nested conditioning WaveNet. Kept opaque — its presence (non-null) alone is enough to
    /// reject the file, per D-9.12's explicit deferral of `condition_dsp`; nothing here or in
    /// `wavenet.rs` ever reads its contents.
    #[serde(default)]
    pub condition_dsp: Option<serde_json::Value>,
}

/// One WaveNet layer array's hyperparameters, as exported in the `.nam` JSON — field names match
/// the exporter's schema, not this crate's own naming.
///
/// A1 and A2 share this one type (see this module's doc comment). Every field A2 adds beyond A1's
/// original five required-plus-`kernel_size`/`gated`/`head_bias` set is `Option`, defaulting to
/// absent; which of A1's or A2's reading applies is `wavenet::PreparedWaveNet::from_file`'s job.
#[derive(Debug, Clone, Deserialize)]
pub struct LayerArrayConfig {
    /// Number of input channels into this layer array.
    pub input_size: usize,
    /// Number of conditioning channels feeding this layer array.
    pub condition_size: usize,
    /// Number of channels each layer in the array carries internally (the residual "trunk" width).
    pub channels: usize,
    /// Per-layer dilation factors, one entry per layer in the array.
    pub dilations: Vec<usize>,
    /// Activation function, interpreted by `wavenet::PreparedWaveNet::from_file`. A1 files carry a
    /// single name (`ActivationSpec::One(ActivationEntry::Name(_))`) shared by every layer in the
    /// array; A2 files may instead carry a per-layer array, and either form's entries may be an
    /// object naming activation-specific parameters (`ActivationEntry::Params`) rather than a bare
    /// name. Required: every real export, A1 or A2, states it.
    pub activation: ActivationSpec,

    /// A1's scalar kernel width, shared by every layer in the array. Exactly one of this and
    /// `kernel_sizes` must be present — `wavenet::PreparedWaveNet::from_file`'s job to check, not
    /// this module's.
    #[serde(default)]
    pub kernel_size: Option<usize>,
    /// A2's per-layer kernel widths — one entry per layer, so its length must agree with
    /// `dilations.len()`.
    #[serde(default)]
    pub kernel_sizes: Option<Vec<usize>>,
    /// A2's internal (dilated-conv / mixin) width, distinct from `channels` when grouped/bottleneck
    /// convolution narrows the residual path. Absent means equal to `channels` — A1's case, and the
    /// only case this crate supports as of M10's Phase 0/1 (core-A2 scope, D-9.12).
    #[serde(default)]
    pub bottleneck: Option<usize>,
    /// A1's legacy head width. Exactly one of this (paired with `head_bias`) and `head` must be
    /// present.
    #[serde(default)]
    pub head_size: Option<usize>,
    /// A1's legacy head bias flag, paired with `head_size`.
    #[serde(default)]
    pub head_bias: Option<bool>,
    /// A2's nested, convolutional head — replaces `head_size`/`head_bias`.
    #[serde(default)]
    pub head: Option<LayerArrayHeadConfig>,

    /// A1's legacy gating flag. Namir supports only `false` (core-A2 scope defers gating);
    /// consulted only when `gating_mode` is absent, matching the reference parser's own precedence.
    #[serde(default)]
    pub gated: Option<bool>,
    /// A2's gating mode (`"none"` / `"gated"` / `"blended"`, scalar or per-layer array). Kept
    /// opaque — Namir supports only the all-`"none"` case, so nothing beyond that fact is ever
    /// read from it.
    #[serde(default)]
    pub gating_mode: Option<serde_json::Value>,
    /// A2's blend/gate activation. Kept opaque: read only when gating is active, which Namir does
    /// not support, so this is parsed solely so a rejection can name it if present.
    #[serde(default)]
    pub secondary_activation: Option<serde_json::Value>,
    /// A2's dilated-conv group count. Namir supports only `1` (no grouped convolution kernels
    /// exist yet — core-A2 scope, D-9.12).
    #[serde(default)]
    pub groups_input: Option<usize>,
    /// A2's conditioning-mixin group count. Namir supports only `1`, for the same reason.
    #[serde(default)]
    pub groups_input_mixin: Option<usize>,
    /// A2's `bottleneck -> channels` 1x1 projection (A1's `residual` is the degenerate case with
    /// `bottleneck == channels`). Core A2 requires this active with `groups == 1`; an explicitly
    /// inactive `layer1x1` is a real, reference-supported shape this crate does not implement.
    #[serde(default)]
    pub layer1x1: Option<Conv1x1FeatureConfig>,
    /// A2's optional extra skip-path 1x1 projection. Namir supports only the inactive case.
    #[serde(default)]
    pub head1x1: Option<Conv1x1FeatureConfig>,
    /// A2's channel-slicing container config. Kept opaque; Namir supports only its absence.
    #[serde(default)]
    pub slimmable: Option<serde_json::Value>,

    /// FiLM conditioning at eight distinct points in the layer. Namir supports only every one of
    /// these being inactive (D-9.12 explicitly defers FiLM); each is parsed only so a rejection can
    /// name which one, if any, was active.
    /// FiLM before the dilated convolution.
    #[serde(default)]
    pub conv_pre_film: Option<FilmConfig>,
    /// FiLM after the dilated convolution.
    #[serde(default)]
    pub conv_post_film: Option<FilmConfig>,
    /// FiLM before the conditioning mixin.
    #[serde(default)]
    pub input_mixin_pre_film: Option<FilmConfig>,
    /// FiLM after the conditioning mixin.
    #[serde(default)]
    pub input_mixin_post_film: Option<FilmConfig>,
    /// FiLM before the primary activation.
    #[serde(default)]
    pub activation_pre_film: Option<FilmConfig>,
    /// FiLM after the primary activation.
    #[serde(default)]
    pub activation_post_film: Option<FilmConfig>,
    /// FiLM after the `layer1x1` projection.
    #[serde(default)]
    pub layer1x1_post_film: Option<FilmConfig>,
    /// FiLM after the `head1x1` projection.
    #[serde(default)]
    pub head1x1_post_film: Option<FilmConfig>,
}

/// A2's nested, convolutional layer-array head (`model.cpp`'s preferred head form, replacing A1's
/// `head_size`/`head_bias` pair). `out_channels`/`kernel_size` mirror `LayerArrayConfig`'s own
/// dimension fields; `head_dilation` defaults to 1 when absent, matching the reference parser.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct LayerArrayHeadConfig {
    /// Number of channels the head rechannel produces.
    pub out_channels: usize,
    /// Convolution kernel width of the head rechannel.
    pub kernel_size: usize,
    /// Dilation of the head rechannel; absent means 1.
    #[serde(default)]
    pub head_dilation: Option<usize>,
    /// Whether the head rechannel applies a bias term.
    pub bias: bool,
}

/// The shape of A2's `layer1x1`/`head1x1` config objects: whether the projection is active, and,
/// when active, its group count and (for `head1x1` only) output width.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Conv1x1FeatureConfig {
    /// Whether this projection is applied at all.
    pub active: bool,
    /// Group count for the projection; absent means 1 (ungrouped).
    #[serde(default)]
    pub groups: Option<usize>,
    /// Output channel count, meaningful only for `head1x1`; absent means the array's own width.
    #[serde(default)]
    pub out_channels: Option<usize>,
}

/// A2's FiLM-site config: either the literal `false` shorthand for "inactive", or an object whose
/// own `active` field (defaulting to `true` when the object is present but omits it, matching the
/// reference parser's `film_config.value("active", true)`) is the real answer. No variant here
/// reads `shift`/`groups` — Namir only ever needs to know whether a site is active, to reject the
/// file if so.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FilmConfig {
    /// The literal `false` shorthand: always inactive.
    Disabled(bool),
    /// An object form; `active` defaults to `true` when the key itself is absent.
    Params {
        /// Whether this FiLM site is active; absent defaults to `true`.
        #[serde(default)]
        active: Option<bool>,
    },
}

impl FilmConfig {
    /// Mirrors `NAM/wavenet/model.cpp`'s `parse_film_params`: absent or literal `false` is
    /// inactive; a present object defaults `active` to `true` when the key itself is omitted.
    pub fn is_active(&self) -> bool {
        match self {
            FilmConfig::Disabled(active) => *active,
            FilmConfig::Params { active } => active.unwrap_or(true),
        }
    }
}

/// One layer array's `activation` field: either one entry shared by every layer (A1's shape, and
/// still legal for A2), or one entry per layer (A2's shape). See [`ActivationEntry`] for what an
/// entry itself may be.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ActivationSpec {
    /// One entry, shared by every layer in the array.
    One(ActivationEntry),
    /// One entry per layer; length must agree with the array's `dilations.len()`.
    PerLayer(Vec<ActivationEntry>),
}

/// A single activation entry: either a bare name (`"Tanh"`), or an object naming the activation's
/// `type` plus whatever parameters that activation takes (`negative_slope`, `min_val`, ...). No
/// catch-all variant — see this module's doc comment for why that matters.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ActivationEntry {
    /// A bare activation name, e.g. `"Tanh"`.
    Name(String),
    /// An object naming the activation's `type` plus its parameters.
    Params(ActivationParams),
}

/// An activation's `type` plus every parameter any A2 activation this crate recognizes may carry.
/// Which fields are meaningful depends on `kind`; unused fields for a given `kind` are simply
/// absent from real files.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ActivationParams {
    /// The activation's name, e.g. `"LeakyReLU"` (JSON key `type`).
    #[serde(rename = "type")]
    pub kind: String,
    /// `LeakyReLU`'s slope for negative inputs.
    #[serde(default)]
    pub negative_slope: Option<f32>,
    /// `PReLU`'s per-channel slopes for negative inputs.
    #[serde(default)]
    pub negative_slopes: Option<Vec<f32>>,
    /// `LeakyHardtanh`'s lower clamp bound.
    #[serde(default)]
    pub min_val: Option<f32>,
    /// `LeakyHardtanh`'s upper clamp bound.
    #[serde(default)]
    pub max_val: Option<f32>,
    /// `LeakyHardtanh`'s slope below `min_val`.
    #[serde(default)]
    pub min_slope: Option<f32>,
    /// `LeakyHardtanh`'s slope above `max_val`.
    #[serde(default)]
    pub max_slope: Option<f32>,
}

/// FR-NAM-080: "Namir shall read and display the model's metadata where present: name, author
/// (`modeled_by`), gear make/model/type, tone type, and any free-text description." All fields
/// default to empty since real files may omit any of them — **or set them to JSON `null`**, which
/// a real exporter does for whichever fields a user left blank in its own metadata form (found
/// against real, community-exported `.nam` files, not this crate's own generated fixtures, which
/// D-19.1 never gives a reason to omit a key *or* null it). `#[serde(default)]` alone only covers
/// the absent-key case: `null` is a present value, so serde still tries to deserialize it as the
/// field's declared type and fails with exactly the "invalid type: null, expected a string" error
/// this shape produced. [`null_or_default`] closes that gap for every field here.
///
/// `PartialEq` added M5: `probe.rs`'s tests compare a probe's metadata against a full parse's.
///
/// `loudness` added M10 (D-9.12's *Consequence — FR-NAM-090/100 stop being blocked*): A2-era
/// exports carry `metadata.loudness` (an integrated-loudness figure in LUFS) alongside
/// `input_level_dbu`/`output_level_dbu` (FR-NAM-100 territory, not read by this crate at all —
/// out of scope for FR-NAM-090). A1 files never carried any of the three, so `loudness` is
/// genuinely optional with no schema-level assumption about presence, `Option<f32>` rather than
/// this struct's usual `String` + [`null_or_default`] pattern: unlike the display-only text
/// fields, "absent" and "present but zero LUFS" are not the same value here, so there is no
/// sensible non-`Option` default to fall back to. `Option<f32>`'s own `Deserialize` impl already
/// treats a present JSON `null` the same as an absent key (both become `None`), so this field
/// needs no `deserialize_with` of its own the way the `String` fields above do.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct NamMetadata {
    /// The model's display name.
    #[serde(default, deserialize_with = "null_or_default")]
    pub name: String,
    /// Author/creator credit, per FR-NAM-080's "author (`modeled_by`)".
    #[serde(default, deserialize_with = "null_or_default")]
    pub modeled_by: String,
    /// Modeled gear's make/model/type, as free text.
    #[serde(default, deserialize_with = "null_or_default")]
    pub gear_type: String,
    /// Modeled gear's tone type (e.g. "clean", "high gain"), as free text.
    #[serde(default, deserialize_with = "null_or_default")]
    pub tone_type: String,
    /// Free-text description, shown as-is.
    #[serde(default, deserialize_with = "null_or_default")]
    pub description: String,
    /// FR-NAM-090: the model's declared integrated loudness, in LUFS. `None` when the file omits
    /// the key (every A1 file; any A2 file that doesn't declare it either) — `namir-engine`'s Nam
    /// stage applies zero normalisation gain in that case rather than guessing a value.
    #[serde(default)]
    pub loudness: Option<f32>,
}

/// Treats a present-but-`null` JSON value the same as an absent key: both become `T::default()`.
/// Combined with `#[serde(default)]` (which only handles the absent-key case on its own), this is
/// the standard serde pattern for "optional in practice, but not typed `Option<T>`, because the
/// field is always logically present, just sometimes empty" — matching `NamMetadata`'s own FR-NAM-080
/// framing ("display the model's metadata where present"), rather than making every reader of this
/// struct unwrap an `Option` for a value that is always meaningfully either "text" or "no text".
fn null_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
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

    /// The bug this test exists to pin down: a real, community-exported `.nam` file (not this
    /// crate's own generated fixtures) sets unfilled `metadata` fields to JSON `null` rather than
    /// omitting the key, which `#[serde(default)]` alone does not cover -- only the missing-key
    /// case in [`tolerates_missing_optional_fields`] above. Every field a real exporter has been
    /// observed to null must parse to the same default a missing key would.
    // trace-partial: FR-NAM-080
    // uncovered: FR-NAM-080 — the "and display" half spans only the name field: UiSnapshot
    // uncovered: carries no metadata field beyond loaded_model_name, which namir-app fills from
    // uncovered: the file's basename rather than metadata.name, so author, gear make/model/type,
    // uncovered: tone type and description reach no screen; closes M9b
    #[test]
    fn tolerates_null_metadata_fields_the_same_as_missing_ones() {
        let mut value: serde_json::Value = serde_json::from_slice(&minimal_valid_json()).unwrap();
        value["metadata"] = serde_json::json!({
            "name": null,
            "modeled_by": null,
            "gear_type": null,
            "tone_type": null,
            "description": null,
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        let file = NamFile::parse(&bytes).unwrap();
        assert_eq!(file.metadata.name, "");
        assert_eq!(file.metadata.modeled_by, "");
        assert_eq!(file.metadata.gear_type, "");
        assert_eq!(file.metadata.tone_type, "");
        assert_eq!(file.metadata.description, "");
    }

    /// FR-NAM-090 (M10): A1 files, and any A2 file omitting the key, never carry
    /// `metadata.loudness` -- an absent key must parse to `None`, not `0.0` (this is exactly why
    /// the field is `Option<f32>` rather than the other metadata fields' `String` + default-empty
    /// shape -- see `NamMetadata`'s own doc comment).
    #[test]
    fn loudness_defaults_to_none_when_absent() {
        let file = NamFile::parse(&minimal_valid_json()).unwrap();
        assert_eq!(file.metadata.loudness, None);
    }

    /// A2-era files carry `metadata.loudness` as a plain number.
    #[test]
    fn loudness_parses_when_present() {
        let mut value: serde_json::Value = serde_json::from_slice(&minimal_valid_json()).unwrap();
        value["metadata"] = serde_json::json!({ "loudness": -18.3 });
        let bytes = serde_json::to_vec(&value).unwrap();
        let file = NamFile::parse(&bytes).unwrap();
        assert_eq!(file.metadata.loudness, Some(-18.3));
    }

    /// `Option<f32>`'s own `Deserialize` treats a present JSON `null` the same as an absent key --
    /// unlike the `String` fields above, this needs no `null_or_default` helper to get that for
    /// free (this test is what proves it, rather than trusting the claim in the doc comment).
    #[test]
    fn loudness_null_is_treated_the_same_as_absent() {
        let mut value: serde_json::Value = serde_json::from_slice(&minimal_valid_json()).unwrap();
        value["metadata"] = serde_json::json!({ "loudness": null });
        let bytes = serde_json::to_vec(&value).unwrap();
        let file = NamFile::parse(&bytes).unwrap();
        assert_eq!(file.metadata.loudness, None);
    }

    // trace: FR-NAM-040
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
