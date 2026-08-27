//! A from-scratch LSTM inference engine — FR-NAM-020's other Must architecture (WaveNet is
//! `wavenet.rs`). Operation order and flat-weight-array layout are matched to
//! `sdatkinson/NeuralAmpModelerCore`'s `NAM/lstm.h` and `NAM/lstm.cpp`, read directly for this
//! task (not from a secondhand summary — see the crate-level doc comment's account of the S-1
//! spike's own near-miss on exactly this point, with WaveNet's array-to-array chaining). The
//! facts this module relies on, as read from that source:
//!
//! - `LSTMCell` holds a weight matrix `W` (`Eigen::MatrixXf`, shape `(4*hidden_size)` rows by
//!   `(cell_input_size + hidden_size)` columns), a bias vector `b` (`4*hidden_size`), and mutable
//!   state: a concatenated `[x, h]` vector `xh` (whose *tail* — the last `hidden_size` entries —
//!   doubles as the hidden state itself, read back out via `get_hidden_state`), a gate-output
//!   vector `ifgo` (`4*hidden_size`), and a cell-state vector `c` (`hidden_size`).
//! - Per-cell weight consumption order (`LSTMCell`'s constructor, `lstm.cpp` lines ~9-28): `W`
//!   row-major (outer loop over the `4*hidden_size` output/gate rows, inner loop over the
//!   `cell_input_size + hidden_size` columns — "Assign in row-major because that's how PyTorch
//!   goes," the source's own comment), then `b` (`4*hidden_size` floats), then a **learned**
//!   initial hidden state `h0` (`hidden_size` floats, written directly into `xh`'s tail — not
//!   zero), then a **learned** initial cell state `c0` (`hidden_size` floats, into `c`).
//! - Forward pass per time step (`LSTMCell::process_`): `xh = concat(x_t, h_prev)`;
//!   `ifgo = W @ xh + b`, one affine transform producing all four gates; gate order within the
//!   `4*hidden_size` output is `i` (input) at `[0, H)`, `f` (forget) at `[H, 2H)`, `g` (cell
//!   candidate) at `[2H, 3H)`, `o` (output) at `[3H, 4H)`, `H = hidden_size`;
//!   `c_t[k] = sigmoid(f[k]) * c_prev[k] + sigmoid(i[k]) * tanh(g[k])`;
//!   `h_t[k] = sigmoid(o[k]) * tanh(c_t[k])`. The source's `else` branch (`using_fast_tanh ==
//!   false`) is the one this module ports — `sigmoid`/`tanhf`, not the fast approximations —
//!   matching `wavenet.rs`'s own exact-math precedent (`f32::tanh`, exp-based sigmoid) and
//!   FR-NAM-030's accuracy floor.
//! - Layers stack sequentially (`LSTM::_process_sample`): layer 0's per-timestep input is the
//!   model's raw input (width `input_size`); layer `i > 0`'s input is layer `i - 1`'s hidden
//!   state from the *same* timestep (`this->_layers[i].process_(this->_layers[i - 1]
//!   .get_hidden_state())` — no delay), so layer `i > 0`'s cell has `cell_input_size ==
//!   hidden_size`, not `input_size`.
//! - After the last layer, a head applies to its final hidden state:
//!   `output = head_weight @ h_last + head_bias`, `head_weight` shape `(out_channels x
//!   hidden_size)` row-major, `head_bias` shape `(out_channels)`. Weight order across the *whole*
//!   model (`LSTM`'s constructor): every layer's `(W, b, h0, c0)` in layer order, **then**
//!   `head_weight`, **then** `head_bias` — the head comes after every layer, not interleaved, and
//!   there is **no trailing scalar** after `head_bias` (`assert(it == weights.end())` right after
//!   reading it) — unlike WaveNet's trailing `head_scale` float.
//! - Top-level `.nam` config fields (`lstm::parse_config_json`): `num_layers`, `input_size`,
//!   `hidden_size`, and `in_channels`/`out_channels` both defaulting to `1` if absent
//!   (`config.value("in_channels", 1)` / `config.value("out_channels", 1)`).
//!
//! **Not ported:** `LSTM::GetPrewarmSamples` (a half-second-of-audio warm-up the reference
//! recommends feeding a fresh instance before trusting its output, "Hacky, but ... seems to work
//! for most models" per that method's own comment). That is a caller-side *audio quality*
//! recommendation, not part of FR-NAM-110's "processing latency in samples" — this forward pass
//! still produces exactly one output sample per input sample with no added delay regardless of
//! whether the caller prewarms it, so `latency_samples() == 0` is still correct either way. Not
//! implementing prewarming is a known, undocumented-elsewhere gap (same spirit as the crate-level
//! doc comment's other explicit out-of-scope FRs), not a silent omission.
//!
//! # `PreparedLstm`/`LstmState`, the D-9.1 split
//!
//! Same structural split as `wavenet.rs`: [`PreparedLstm`] holds only immutable weights
//! (including the learned `h0`/`c0` above — those are *weights*, read once at load time, not
//! per-instance state) and is `Sync`; [`LstmState`] holds each layer's actual per-instance
//! evolving `xh`/`ifgo`/`c`, seeded from `h0`/`c0` at construction and never shared.
//!
//! # Scope restriction: `input_size == in_channels == out_channels == 1`
//!
//! Mirrors `wavenet.rs`'s `condition_size == 1` restriction and for the identical reason: this
//! implementation only ever feeds the raw mono signal as input, with no parametric/conditioning
//! support in 1.0 scope. A file declaring anything else is rejected with
//! `error_codes::UNSUPPORTED_LSTM_CHANNELS`, not silently misread (e.g. by reading only the first
//! of several conditioning channels as if it were the whole input).

use namir_core::SampleRate;

use crate::error_codes::{self, NamLoadError};
use crate::file::{LstmConfigJson, LstmFile, NamMetadata};
use crate::shared::{WeightReader, check_max, check_min1};

/// FRS §2's definitions: model sample rate is "typically 48 kHz" — the fallback when a `.nam`
/// file omits `sample_rate` entirely, same default `wavenet.rs` uses.
const DEFAULT_SAMPLE_RATE_HZ: u32 = 48_000;

// -------------------------------------------------------------------------------------------
// NFR-SEC-020 dimension ceilings, chosen the same way `wavenet.rs`'s are: generously above any
// plausible real LSTM export, while still ruling out a hostile file that declares e.g.
// `hidden_size: 4_000_000_000` to force a multi-gigabyte or overflowing allocation attempt.
//
// **Not verified against a canonical source** (unlike WaveNet's `MAX_CHANNELS`, which cites the
// S-1 spike's confirmed "standard" shape): this crate has no equivalent researched LSTM shape.
// Community NAM LSTM exports this crate's authors are aware of anecdotally use hidden sizes on
// the order of 8-160 and 1-4 layers, but that impression is not backed by a citation the way
// WaveNet's ceiling comment's is, so the ceilings below are deliberately far above even a
// generous multiple of that impression rather than tight to it — the goal is "clearly bigger
// than anything real, still small enough to block a hostile allocation," not "tight to the real
// distribution." If real preset numbers surface later with a citation, tightening these (not the
// architecture) would be the right follow-up.
//
// With these ceilings, the largest single per-layer weight-matrix read is bounded by
// `4 * MAX_LSTM_HIDDEN_SIZE * (MAX_LSTM_HIDDEN_SIZE + MAX_LSTM_HIDDEN_SIZE)` =
// `8 * 8192^2` ≈ 5.4e8, and the largest total across `MAX_LSTM_LAYERS` layers ≈ 3.4e10 — both
// far below `usize::MAX` on any 64-bit target, so no multiplication in this module can overflow
// once every dimension has passed its ceiling check. As in `wavenet.rs`, that ordering (bound
// every dimension *before* it appears in an arithmetic expression) is load-bearing; see the
// comment at the top of `PreparedLstm::from_file`.
// -------------------------------------------------------------------------------------------

const MAX_LSTM_LAYERS: usize = 64;
const MAX_LSTM_INPUT_SIZE: usize = 8192;
const MAX_LSTM_HIDDEN_SIZE: usize = 8192;
const MAX_LSTM_CHANNELS: usize = 8192;
const MAX_LSTM_TOTAL_WEIGHTS: usize = 200_000_000;

#[inline]
fn sigmoid(x: f32) -> f32 {
    // Exact, not the reference's `fast_sigmoid` branch — matches `wavenet.rs`'s
    // `Activation::Sigmoid` precedent (FR-NAM-030's accuracy floor is about exact math).
    1.0 / (1.0 + (-x).exp())
}

/// One `LSTMCell`'s immutable, `Sync` weights (D-9.1). `input_size` here is the *cell's* input
/// width (`cell_input_size` in the module doc comment) — `input_size` for layer 0, `hidden_size`
/// for every later layer — not the model's own top-level `input_size` config field.
struct LstmCellWeights {
    input_size: usize,
    hidden_size: usize,
    /// Flat, row-major `(4*hidden_size) x (input_size+hidden_size)`.
    w: Vec<f32>,
    /// `4*hidden_size`.
    b: Vec<f32>,
    /// The learned initial hidden state, `hidden_size` floats — see the module doc comment for
    /// why this is a weight, not a per-instance zero.
    h0: Vec<f32>,
    /// The learned initial cell state, `hidden_size` floats.
    c0: Vec<f32>,
}

impl LstmCellWeights {
    fn read(
        r: &mut WeightReader,
        input_size: usize,
        hidden_size: usize,
    ) -> Result<Self, NamLoadError> {
        let rows = 4 * hidden_size;
        let cols = input_size + hidden_size;
        let w = r.take(rows * cols)?;
        let b = r.take(rows)?;
        let h0 = r.take(hidden_size)?;
        let c0 = r.take(hidden_size)?;
        Ok(Self {
            input_size,
            hidden_size,
            w,
            b,
            h0,
            c0,
        })
    }
}

/// The output head's immutable weights: `head_weight` is flat row-major `(out_channels x
/// hidden_size)`, `head_bias` is `(out_channels)`.
struct LstmHead {
    hidden_size: usize,
    out_channels: usize,
    weight: Vec<f32>,
    bias: Vec<f32>,
}

/// Immutable, `Sync` weights and configuration (D-9.1 / D-8.2), the LSTM analogue of
/// `wavenet::PreparedWaveNet`. Every field here (`Vec<f32>`, `usize`) is already auto-`Sync`, so
/// as with `PreparedWaveNet`, `Sync` is simply derived — no `unsafe impl` (this crate forbids
/// unsafe code, workspace lint D-5.3).
pub struct PreparedLstm {
    layers: Vec<LstmCellWeights>,
    head: LstmHead,
    sample_rate: SampleRate,
    metadata: NamMetadata,
}

/// Per-cell mutable state: `xh`'s tail *is* the current hidden state (mirrors the reference's own
/// `LSTMCell::_xh`/`get_hidden_state`, see the module doc comment) — kept as one buffer rather
/// than a separate `x`/`h` pair specifically so this state layout stays visibly parallel to the
/// source it was read from, which is exactly the kind of structural fidelity the S-1 spike's own
/// near-miss (see the crate doc comment) argues for.
struct LstmCellScratch {
    xh: Vec<f32>,
    ifgo: Vec<f32>,
    c: Vec<f32>,
}

impl LstmCellScratch {
    /// Seeds `xh`'s tail and `c` from the cell's *learned* initial state (`h0`/`c0`), not zero —
    /// this is what makes a freshly-constructed `LstmState` match the reference's own
    /// freshly-constructed `LSTMCell` (whose constructor writes `h0`/`c0` into exactly these
    /// slots — see the module doc comment's per-cell weight consumption order).
    fn new(weights: &LstmCellWeights) -> Self {
        let mut xh = vec![0f32; weights.input_size + weights.hidden_size];
        xh[weights.input_size..].copy_from_slice(&weights.h0);
        Self {
            xh,
            ifgo: vec![0f32; 4 * weights.hidden_size],
            c: weights.c0.clone(),
        }
    }
}

/// Per-instance mutable inference state (D-9.1): every layer's evolving `xh`/`ifgo`/`c`, plus one
/// `hidden_size`-wide scratch buffer used to hand a layer's hidden state to the next layer within
/// a single time step (see `PreparedLstm::process_block`). Never shared across instances.
///
/// Unlike `wavenet::WaveNetState`'s scratch, none of this state's buffer sizes depend on the
/// block size — an LSTM is processed one time step at a time regardless of how many time steps
/// are in a call's `input` slice, so there is nothing here to size proportionally to a "maximum
/// block size" the way WaveNet's convolution scratch is. `max_n` is kept anyway, purely so
/// `PreparedLstm::process_block`'s "panics if the block exceeds the size declared at
/// `new_state`" contract matches `WaveNetState`'s exactly — `model.rs`'s enum wrapper forwards to
/// whichever variant is active without knowing which one it is, so both variants need to behave
/// identically at that contract boundary even though only one of them actually needs the number.
pub struct LstmState {
    max_n: usize,
    layers: Vec<LstmCellScratch>,
    /// Scratch: the `hidden_size`-wide output of whichever layer was processed most recently
    /// within the current time step, about to be fed to the next layer (or the head).
    prev_h: Vec<f32>,
}

impl LstmState {
    fn new(prepared: &PreparedLstm, max_n: usize) -> Self {
        let layers = prepared.layers.iter().map(LstmCellScratch::new).collect();
        Self {
            max_n,
            layers,
            prev_h: vec![0f32; prepared.head.hidden_size],
        }
    }
}

/// Runs one `LSTMCell` for one time step: writes `x` into `scratch.xh`'s head, computes
/// `ifgo = W @ xh + b`, updates `scratch.c` and `scratch.xh`'s tail (the new hidden state) in
/// place. Allocation-free. `x.len()` must equal `weights.input_size` (the cell's own input
/// width, debug-checked below — every call site in this module derives `x` from a buffer already
/// sized to match by construction, so this can never fail outside a bug in this module itself).
fn step_cell(weights: &LstmCellWeights, scratch: &mut LstmCellScratch, x: &[f32]) {
    debug_assert_eq!(x.len(), weights.input_size);
    let input_size = weights.input_size;
    let hidden_size = weights.hidden_size;
    let cols = input_size + hidden_size;

    scratch.xh[..input_size].copy_from_slice(x);

    // ifgo = W @ xh + b (one affine transform for all four gates at once, matching the
    // reference's single `_ifgo.noalias() = _w * _xh; _ifgo += _b;`).
    for row in 0..4 * hidden_size {
        let mut acc = weights.b[row];
        let w_row = &weights.w[row * cols..(row + 1) * cols];
        for (w, x) in w_row.iter().zip(scratch.xh.iter()) {
            acc += w * x;
        }
        scratch.ifgo[row] = acc;
    }

    let i_off = 0;
    let f_off = hidden_size;
    let g_off = 2 * hidden_size;
    let o_off = 3 * hidden_size;

    // c_t[k] = sigmoid(f[k]) * c_prev[k] + sigmoid(i[k]) * tanh(g[k]).
    for k in 0..hidden_size {
        let f_gate = sigmoid(scratch.ifgo[f_off + k]);
        let i_gate = sigmoid(scratch.ifgo[i_off + k]);
        let g_val = scratch.ifgo[g_off + k].tanh();
        scratch.c[k] = f_gate * scratch.c[k] + i_gate * g_val;
    }
    // h_t[k] = sigmoid(o[k]) * tanh(c_t[k]) — reads the *just-updated* c_t, per the reference's
    // own two-separate-loops structure (first update every c[k], then every h[k]).
    for k in 0..hidden_size {
        let o_gate = sigmoid(scratch.ifgo[o_off + k]);
        scratch.xh[input_size + k] = o_gate * scratch.c[k].tanh();
    }
}

impl PreparedLstm {
    /// The semantic half of P6's "one hardened place `.nam` bytes go through" for LSTM files
    /// (the other half is `LstmFile::parse`'s JSON-shape parsing). Validation order mirrors
    /// `wavenet::PreparedWaveNet::from_file`'s:
    ///
    /// 1. `architecture == "LSTM"` (`UNSUPPORTED_ARCHITECTURE`).
    /// 2. `sample_rate` is nonzero if present (`INVALID_SAMPLE_RATE`), else defaults to 48 kHz.
    /// 3. `num_layers`, `input_size`, `hidden_size`, `in_channels`, `out_channels` are all within
    ///    their NFR-SEC-020 ceilings (`DIMENSION_LIMIT_EXCEEDED`) and at least 1
    ///    (`DIMENSION_LIMIT_EXCEEDED`) — *before* any of them appears in an arithmetic
    ///    expression, same ordering discipline as `wavenet.rs` (see the ceiling constants'
    ///    comment above for why this is safe from `usize` overflow by construction once this
    ///    step passes).
    /// 4. `input_size == in_channels == out_channels == 1` (`UNSUPPORTED_LSTM_CHANNELS`) — this
    ///    crate's scope restriction, see the module doc comment.
    /// 5. `weights.len()` is within its ceiling (`DIMENSION_LIMIT_EXCEEDED`).
    /// 6. Each layer's weights are read in the module doc comment's confirmed order (`W`, `b`,
    ///    `h0`, `c0`), then the head's `(head_weight, head_bias)` (`WEIGHT_COUNT_MISMATCH` on
    ///    exhaustion).
    /// 7. Unlike WaveNet, there is no trailing scalar: exactly `weights.len()` floats must be
    ///    consumed, no more, no fewer (`WEIGHT_COUNT_MISMATCH`).
    pub fn from_file(nam: &LstmFile) -> Result<Self, NamLoadError> {
        if nam.architecture != "LSTM" {
            return Err(NamLoadError {
                code: error_codes::UNSUPPORTED_ARCHITECTURE,
                detail: format!("architecture: {:?}", nam.architecture),
            });
        }

        let sample_rate_hz = match nam.sample_rate {
            Some(0) => {
                return Err(NamLoadError {
                    code: error_codes::INVALID_SAMPLE_RATE,
                    detail: "sample_rate is 0 Hz".to_string(),
                });
            }
            Some(hz) => hz,
            None => DEFAULT_SAMPLE_RATE_HZ,
        };
        let sample_rate = SampleRate::new(sample_rate_hz)
            .expect("nonzero: checked above (Some(0) rejected), or the fixed nonzero fallback");

        let LstmConfigJson {
            num_layers,
            input_size,
            hidden_size,
            in_channels,
            out_channels,
        } = nam.config;

        check_max(num_layers, MAX_LSTM_LAYERS, "config.num_layers")?;
        check_max(input_size, MAX_LSTM_INPUT_SIZE, "config.input_size")?;
        check_max(hidden_size, MAX_LSTM_HIDDEN_SIZE, "config.hidden_size")?;
        check_max(in_channels, MAX_LSTM_CHANNELS, "config.in_channels")?;
        check_max(out_channels, MAX_LSTM_CHANNELS, "config.out_channels")?;
        check_min1(num_layers, "config.num_layers")?;
        check_min1(input_size, "config.input_size")?;
        check_min1(hidden_size, "config.hidden_size")?;
        check_min1(in_channels, "config.in_channels")?;
        check_min1(out_channels, "config.out_channels")?;
        check_max(nam.weights.len(), MAX_LSTM_TOTAL_WEIGHTS, "weights.len()")?;

        if input_size != 1 || in_channels != 1 || out_channels != 1 {
            return Err(NamLoadError {
                code: error_codes::UNSUPPORTED_LSTM_CHANNELS,
                detail: format!(
                    "input_size={input_size}, in_channels={in_channels}, out_channels={out_channels} (all must be 1)"
                ),
            });
        }

        let mut r = WeightReader::new(&nam.weights);
        let mut layers = Vec::with_capacity(num_layers);
        for i in 0..num_layers {
            let cell_input_size = if i == 0 { input_size } else { hidden_size };
            layers.push(LstmCellWeights::read(&mut r, cell_input_size, hidden_size)?);
        }

        let head_weight = r.take(out_channels * hidden_size)?;
        let head_bias = r.take(out_channels)?;

        if r.pos != nam.weights.len() {
            return Err(NamLoadError {
                code: error_codes::WEIGHT_COUNT_MISMATCH,
                detail: format!(
                    "consumed {} of {} weights (LSTM has no trailing scalar, unlike WaveNet's \
                     head_scale — these must match exactly)",
                    r.pos,
                    nam.weights.len()
                ),
            });
        }

        Ok(Self {
            layers,
            head: LstmHead {
                hidden_size,
                out_channels,
                weight: head_weight,
                bias: head_bias,
            },
            sample_rate,
            metadata: nam.metadata.clone(),
        })
    }

    /// FR-NAM-080: model metadata (name, `modeled_by`, gear/tone type, description).
    pub fn metadata(&self) -> &NamMetadata {
        &self.metadata
    }

    /// FR-NAM-090: the model's declared integrated loudness (LUFS), or `None` when absent. See
    /// `wavenet::PreparedWaveNet::loudness_lufs`'s identical doc comment.
    pub fn loudness_lufs(&self) -> Option<f32> {
        self.metadata.loudness
    }

    /// The model's declared sample rate (or the 48 kHz default if the file omitted it).
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// FR-NAM-110: this LSTM produces exactly one output sample per input sample, with no added
    /// delay — see the module doc comment's note on why `GetPrewarmSamples` (a caller-side
    /// audio-quality recommendation in the reference) does not change this.
    pub fn latency_samples(&self) -> u32 {
        0
    }

    /// `max_block_size` is the largest block size this state will ever be asked to process; see
    /// `LstmState`'s doc comment for why this doesn't actually size anything here the way it does
    /// for `WaveNetState` — kept only so both variants share the same panic contract.
    pub fn new_state(&self, max_block_size: usize) -> LstmState {
        LstmState::new(self, max_block_size)
    }

    /// The allocation-free RT-path entry point. Writes `input.len()` frames of output into `out`
    /// (`out.len() == out_channels * input.len()`, and `out_channels == 1` per this module's
    /// scope restriction). Every intermediate buffer lives in `state` and is reused sample to
    /// sample and block to block; this function itself allocates nothing.
    ///
    /// Panics if `input.len()` exceeds `state`'s configured max block size — a call-site
    /// programming error, not something untrusted `.nam` content can trigger, same reasoning as
    /// `wavenet::PreparedWaveNet::process_block`'s identical panic contract.
    pub fn process_block(&self, state: &mut LstmState, input: &[f32], out: &mut [f32]) {
        let n = input.len();
        assert!(
            n <= state.max_n,
            "block size {n} exceeds this state's preallocated max {}",
            state.max_n
        );

        let hidden_size = self.head.hidden_size;
        for t in 0..n {
            // Layer 0 consumes the model's raw input sample directly (input_size == 1, enforced
            // at load time).
            let x0 = [input[t]];
            step_cell(&self.layers[0], &mut state.layers[0], &x0);
            state.prev_h[..hidden_size].copy_from_slice(&state.layers[0].xh[1..]);

            // Layer i > 0 consumes layer (i - 1)'s hidden state from the *same* time step (no
            // delay — see the module doc comment).
            for l in 1..self.layers.len() {
                step_cell(
                    &self.layers[l],
                    &mut state.layers[l],
                    &state.prev_h[..hidden_size],
                );
                state.prev_h[..hidden_size].copy_from_slice(&state.layers[l].xh[hidden_size..]);
            }

            // Head: output = head_weight @ h_last + head_bias, one row per output channel.
            for oc in 0..self.head.out_channels {
                let hw = &self.head.weight[oc * hidden_size..(oc + 1) * hidden_size];
                let mut acc = self.head.bias[oc];
                for (w, h) in hw.iter().zip(state.prev_h[..hidden_size].iter()) {
                    acc += w * h;
                }
                out[oc * n + t] = acc;
            }
        }
    }

    /// Convenience wrapper over `process_block` that allocates its own output buffer.
    /// **Not RT-safe** — for tests, tools, and other non-audio-thread callers only.
    pub fn process(&self, state: &mut LstmState, input: &[f32]) -> Vec<f32> {
        let out_len = self.head.out_channels * input.len();
        let mut out = vec![0f32; out_len];
        self.process_block(state, input, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::NamMetadata;

    fn minimal_config() -> LstmConfigJson {
        LstmConfigJson {
            num_layers: 2,
            input_size: 1,
            hidden_size: 3,
            in_channels: 1,
            out_channels: 1,
        }
    }

    /// Mirrors `wavenet.rs`'s `weight_count_for`: computes exactly how many weights
    /// `PreparedLstm::from_file` will consume for `cfg`, so tests can hand-build a matching (or
    /// deliberately mismatched) flat weight array without going through JSON.
    fn weight_count_for(cfg: &LstmConfigJson) -> usize {
        let mut n = 0;
        for i in 0..cfg.num_layers {
            let cell_input = if i == 0 {
                cfg.input_size
            } else {
                cfg.hidden_size
            };
            let rows = 4 * cfg.hidden_size;
            let cols = cell_input + cfg.hidden_size;
            n += rows * cols; // W
            n += rows; // b
            n += cfg.hidden_size; // h0
            n += cfg.hidden_size; // c0
        }
        n += cfg.out_channels * cfg.hidden_size; // head_weight
        n += cfg.out_channels; // head_bias
        n
    }

    fn expect_err(result: Result<PreparedLstm, NamLoadError>) -> NamLoadError {
        match result {
            Ok(_) => panic!("expected PreparedLstm::from_file to reject this file"),
            Err(e) => e,
        }
    }

    fn minimal_valid_file() -> LstmFile {
        let cfg = minimal_config();
        let n = weight_count_for(&cfg);
        let weights: Vec<f32> = (0..n).map(|i| 0.01 * ((i % 5) as f32 - 2.0)).collect();
        LstmFile {
            version: None,
            architecture: "LSTM".to_string(),
            config: cfg,
            weights,
            sample_rate: Some(48_000),
            metadata: NamMetadata::default(),
        }
    }

    #[test]
    fn minimal_valid_file_loads_successfully() {
        let file = minimal_valid_file();
        let prepared = PreparedLstm::from_file(&file).expect("minimal valid file should load");
        assert_eq!(prepared.sample_rate().hz(), 48_000);
    }

    #[test]
    fn missing_sample_rate_defaults_to_48khz() {
        let mut file = minimal_valid_file();
        file.sample_rate = None;
        let prepared = PreparedLstm::from_file(&file).unwrap();
        assert_eq!(prepared.sample_rate().hz(), 48_000);
    }

    #[test]
    fn rejects_wrong_architecture() {
        let mut file = minimal_valid_file();
        file.architecture = "WaveNet".to_string();
        let err = expect_err(PreparedLstm::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_ARCHITECTURE.id);
    }

    #[test]
    fn rejects_zero_sample_rate() {
        let mut file = minimal_valid_file();
        file.sample_rate = Some(0);
        let err = expect_err(PreparedLstm::from_file(&file));
        assert_eq!(err.code.id, error_codes::INVALID_SAMPLE_RATE.id);
    }

    #[test]
    fn rejects_input_size_other_than_one() {
        let mut file = minimal_valid_file();
        file.config.input_size = 2;
        let err = expect_err(PreparedLstm::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_LSTM_CHANNELS.id);
    }

    #[test]
    fn rejects_in_channels_other_than_one() {
        let mut file = minimal_valid_file();
        file.config.in_channels = 2;
        let err = expect_err(PreparedLstm::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_LSTM_CHANNELS.id);
    }

    #[test]
    fn rejects_out_channels_other_than_one() {
        let mut file = minimal_valid_file();
        file.config.out_channels = 2;
        let err = expect_err(PreparedLstm::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_LSTM_CHANNELS.id);
    }

    #[test]
    fn rejects_zero_num_layers() {
        let mut file = minimal_valid_file();
        file.config.num_layers = 0;
        let err = expect_err(PreparedLstm::from_file(&file));
        assert_eq!(err.code.id, error_codes::DIMENSION_LIMIT_EXCEEDED.id);
    }

    #[test]
    fn rejects_dimension_over_ceiling_without_attempting_a_huge_allocation() {
        let mut file = minimal_valid_file();
        file.config.hidden_size = 999_999_999;
        let err = expect_err(PreparedLstm::from_file(&file));
        assert_eq!(err.code.id, error_codes::DIMENSION_LIMIT_EXCEEDED.id);
    }

    #[test]
    fn rejects_wrong_weight_count_too_few() {
        let mut file = minimal_valid_file();
        file.weights.pop();
        let err = expect_err(PreparedLstm::from_file(&file));
        assert_eq!(err.code.id, error_codes::WEIGHT_COUNT_MISMATCH.id);
    }

    #[test]
    fn rejects_wrong_weight_count_trailing_extra() {
        // Unlike WaveNet, LSTM has no trailing scalar — one extra float left over must be
        // rejected, not silently accepted the way WaveNet's optional head_scale is.
        let mut file = minimal_valid_file();
        file.weights.push(0.0);
        let err = expect_err(PreparedLstm::from_file(&file));
        assert_eq!(err.code.id, error_codes::WEIGHT_COUNT_MISMATCH.id);
    }

    // trace-partial: FR-NAM-110
    // uncovered: FR-NAM-110 — the method's "cross-correlate an impulse through the stage" is
    // uncovered: performed by nothing: both tagged tests read an accessor whose body is the
    // uncovered: literal 0 and assert it equals 0, so they would pass unchanged if inference did
    // uncovered: introduce delay, and the one path that reports a nonzero figure — NamStage's
    // uncovered: resampler latency — is asserted only as > 0 and is documented as not
    // uncovered: sample-exact; closes M8
    #[test]
    fn latency_samples_is_zero() {
        let prepared = PreparedLstm::from_file(&minimal_valid_file()).unwrap();
        assert_eq!(prepared.latency_samples(), 0);
    }

    #[test]
    fn single_step_matches_hand_computation() {
        // One layer, hidden_size=1, input_size=1: W (4x2) row-major [i_x,i_h, f_x,f_h, g_x,g_h,
        // o_x,o_h], b all 0, h0=0, c0=0. Feed x=1.0 and verify against the gate equations by
        // hand.
        let cfg = LstmConfigJson {
            num_layers: 1,
            input_size: 1,
            hidden_size: 1,
            in_channels: 1,
            out_channels: 1,
        };
        #[rustfmt::skip]
        let w = [
            1.0, 0.0, // i: sigmoid(1*x)
            0.5, 0.0, // f: sigmoid(0.5*x)
            2.0, 0.0, // g: tanh(2*x)
            0.25, 0.0, // o: sigmoid(0.25*x)
        ];
        let mut weights = w.to_vec();
        weights.extend([0.0, 0.0, 0.0, 0.0]); // b
        weights.extend([0.0]); // h0
        weights.extend([0.0]); // c0
        weights.extend([1.0]); // head_weight (1x1)
        weights.extend([0.0]); // head_bias
        let file = LstmFile {
            version: None,
            architecture: "LSTM".to_string(),
            config: cfg,
            weights,
            sample_rate: Some(48_000),
            metadata: NamMetadata::default(),
        };
        let prepared = PreparedLstm::from_file(&file).unwrap();
        let mut state = prepared.new_state(1);
        let out = prepared.process(&mut state, &[1.0]);

        let x = 1.0f32;
        let i_gate = sigmoid(1.0 * x);
        let f_gate = sigmoid(0.5 * x);
        let g_val = (2.0 * x).tanh();
        let o_gate = sigmoid(0.25 * x);
        let c = f_gate * 0.0 + i_gate * g_val;
        let h = o_gate * c.tanh();
        assert!((out[0] - h).abs() < 1e-6, "expected {h}, got {}", out[0]);
    }

    #[test]
    fn process_block_does_not_allocate() {
        use crate::wavenet::rt_harness;

        let file = minimal_valid_file();
        let prepared = PreparedLstm::from_file(&file).unwrap();
        let mut state = prepared.new_state(64);
        let input = vec![0.1f32; 64];
        let mut output = vec![0.0f32; 64];
        rt_harness::audio_section(|| {
            prepared.process_block(&mut state, &input, &mut output);
        });
    }
}
