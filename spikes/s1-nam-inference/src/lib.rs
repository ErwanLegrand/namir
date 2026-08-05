//! S-1 spike: a from-scratch Rust port of `NeuralAmpModelerCore`'s WaveNet inference path.
//!
//! Operation order and flat-weight-array layout are matched to
//! `sdatkinson/NeuralAmpModelerCore` (`NAM/wavenet/model.cpp`), confirmed by reading that
//! project's source, not guessed. See `spikes/s1-nam-inference/README.md` for the citations.
//!
//! `PreparedWaveNet` holds only immutable weights (and is `Sync`); `WaveNetState` holds only
//! the per-instance causal-conv history. This split is D-9.1's requirement and is the thing
//! this spike is supposed to test structurally, not just assert.
//!
//! Signals are represented as a flat, row-major `Vec<f32>` (`Sig`), `data[channel * n + t]`,
//! rather than one `Vec<f32>` per channel: an early version used per-channel vecs and spent the
//! large majority of NFR-PERF-010 benchmark time in the allocator (~1000+ tiny allocations per
//! block) rather than in the arithmetic it was supposed to measure. One allocation per tensor
//! instead of one per channel cut that by roughly the channel count.

use serde::Deserialize;
use std::error::Error;
use std::fmt;

/// The default Windows CRT heap turned out to dominate the NFR-PERF-010 benchmark even after
/// the flat-`Sig` rewrite below (this is spike scratch code with per-block allocation, not the
/// zero-allocation audio thread P1 mandates — see the bench.rs / README notes on scope). mimalloc
/// is MIT-licensed, already a plausible pick for the product itself, and needs no other code
/// change: it applies process-wide to every binary in this crate.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// A flat, row-major multi-channel signal buffer: `data[channel * n + t]`.
type Sig = Vec<f32>;

#[derive(Debug, Deserialize)]
pub struct NamFile {
    pub architecture: String,
    pub config: WaveNetConfig,
    pub weights: Vec<f32>,
    #[serde(default)]
    pub sample_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct WaveNetConfig {
    pub layers: Vec<LayerArrayConfig>,
    pub head_scale: f32,
    #[serde(default)]
    pub head: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Clone)]
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

#[derive(Debug)]
pub struct NamError(String);

impl fmt::Display for NamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl Error for NamError {}

// ---------------------------------------------------------------------------------------------
// Primitive layers
// ---------------------------------------------------------------------------------------------

/// A 1x1 convolution: `out[oc][t] = bias[oc] + sum_ic weight[oc][ic] * in[ic][t]`.
/// Weight layout: row-major `[out_ch][in_ch]`, matching PyTorch's native flatten of a
/// `Conv1d(kernel_size=1).weight` tensor.
struct Conv1x1 {
    out_ch: usize,
    in_ch: usize,
    weight: Vec<f32>,
    bias: Option<Vec<f32>>,
}

impl Conv1x1 {
    fn read(
        r: &mut WeightReader,
        out_ch: usize,
        in_ch: usize,
        has_bias: bool,
    ) -> Result<Self, NamError> {
        let weight = r.take(out_ch * in_ch)?;
        let bias = if has_bias {
            Some(r.take(out_ch)?)
        } else {
            None
        };
        Ok(Self {
            out_ch,
            in_ch,
            weight,
            bias,
        })
    }

    /// `input`: flat `[in_ch * n]`. Overwrites `out[..out_ch * n]`. Allocation-free.
    fn apply_into(&self, input: &[f32], n: usize, out: &mut [f32]) {
        for oc in 0..self.out_ch {
            let bias = self.bias.as_ref().map_or(0.0, |b| b[oc]);
            let out_row = &mut out[oc * n..(oc + 1) * n];
            out_row.fill(bias);
            for ic in 0..self.in_ch {
                let w = self.weight[oc * self.in_ch + ic];
                if w == 0.0 {
                    continue;
                }
                let in_row = &input[ic * n..(ic + 1) * n];
                for t in 0..n {
                    out_row[t] += w * in_row[t];
                }
            }
        }
    }

    /// Same as `apply_into`, but accumulates (`+=`) into `out` instead of overwriting it.
    fn apply_add_into(&self, input: &[f32], n: usize, out: &mut [f32]) {
        for oc in 0..self.out_ch {
            let out_row = &mut out[oc * n..(oc + 1) * n];
            if let Some(b) = &self.bias {
                let bias = b[oc];
                for v in out_row.iter_mut() {
                    *v += bias;
                }
            }
            for ic in 0..self.in_ch {
                let w = self.weight[oc * self.in_ch + ic];
                if w == 0.0 {
                    continue;
                }
                let in_row = &input[ic * n..(ic + 1) * n];
                for t in 0..n {
                    out_row[t] += w * in_row[t];
                }
            }
        }
    }

    /// `input`: flat `[in_ch * n]`. Returns flat `[out_ch * n]`. Used only by tests, where
    /// allocating fresh output is simpler than threading scratch buffers through.
    #[cfg(test)]
    fn apply(&self, input: &Sig, n: usize) -> Sig {
        let mut out = vec![0f32; self.out_ch * n];
        self.apply_into(input, n, &mut out);
        out
    }
}

/// A dilated causal 1D convolution. Weight layout: row-major `[out_ch][in_ch][kernel]`, kernel
/// tap fastest-varying — matches PyTorch's native flatten of `Conv1d.weight`. Tap `k=0` is the
/// oldest sample in the receptive field (offset `dilation * (k + 1 - kernel_size)`), tap
/// `k = kernel_size - 1` is the current sample (offset 0) — standard causal left-padding.
struct Conv1D {
    out_ch: usize,
    in_ch: usize,
    kernel_size: usize,
    dilation: usize,
    weight: Vec<f32>,
    bias: Vec<f32>,
}

impl Conv1D {
    fn read(
        r: &mut WeightReader,
        out_ch: usize,
        in_ch: usize,
        kernel_size: usize,
        dilation: usize,
    ) -> Result<Self, NamError> {
        let weight = r.take(out_ch * in_ch * kernel_size)?;
        let bias = r.take(out_ch)?;
        Ok(Self {
            out_ch,
            in_ch,
            kernel_size,
            dilation,
            weight,
            bias,
        })
    }

    fn history_len(&self) -> usize {
        (self.kernel_size - 1) * self.dilation
    }

    /// `input`: flat `[in_ch * n]`. `history`: flat `[in_ch * history_len()]`, updated in place
    /// to the new tail history for the next block. `padded`: scratch, flat
    /// `[in_ch * (history_len() + n)]`. Overwrites `out[..out_ch * n]`. Allocation-free.
    fn apply_into(
        &self,
        input: &[f32],
        n: usize,
        history: &mut [f32],
        padded: &mut [f32],
        out: &mut [f32],
    ) {
        let hl = self.history_len();
        let pn = hl + n;

        // padded[ic] = history[ic] ++ input[ic], length pn, flattened [in_ch * pn].
        for ic in 0..self.in_ch {
            let dst = &mut padded[ic * pn..(ic + 1) * pn];
            dst[..hl].copy_from_slice(&history[ic * hl..(ic + 1) * hl]);
            dst[hl..].copy_from_slice(&input[ic * n..(ic + 1) * n]);
        }

        for oc in 0..self.out_ch {
            let out_row = &mut out[oc * n..(oc + 1) * n];
            out_row.fill(self.bias[oc]);
            for ic in 0..self.in_ch {
                let p = &padded[ic * pn..(ic + 1) * pn];
                for k in 0..self.kernel_size {
                    let w = self.weight[(oc * self.in_ch + ic) * self.kernel_size + k];
                    if w == 0.0 {
                        continue;
                    }
                    let offset = k * self.dilation;
                    for t in 0..n {
                        out_row[t] += w * p[t + offset];
                    }
                }
            }
        }

        // New history = last hl samples of padded.
        for ic in 0..self.in_ch {
            let p = &padded[ic * pn..(ic + 1) * pn];
            history[ic * hl..(ic + 1) * hl].copy_from_slice(&p[n..]);
        }
    }

    /// `input`: flat `[in_ch * n]`. Returns flat `[out_ch * n]`. Used only by tests.
    #[cfg(test)]
    fn apply(&self, input: &Sig, n: usize, history: &mut Sig) -> Sig {
        let pn = self.history_len() + n;
        let mut padded = vec![0f32; self.in_ch * pn];
        let mut out = vec![0f32; self.out_ch * n];
        self.apply_into(input, n, history, &mut padded, &mut out);
        out
    }
}

fn activation_apply(name: &str, x: &mut [f32]) {
    match name {
        "Tanh" => {
            for v in x.iter_mut() {
                *v = v.tanh();
            }
        }
        "ReLU" => {
            for v in x.iter_mut() {
                *v = v.max(0.0);
            }
        }
        "Sigmoid" => {
            for v in x.iter_mut() {
                *v = 1.0 / (1.0 + (-*v).exp());
            }
        }
        "Identity" | "" => {}
        other => panic!("unsupported activation: {other}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Layer / LayerArray
// ---------------------------------------------------------------------------------------------

struct Layer {
    dilated: Conv1D,
    mixin: Conv1x1,
    residual: Conv1x1,
    activation: String,
    gated: bool,
    channels: usize,
}

/// Per-layer reusable scratch, sized once for a chosen max block size and reused across every
/// `process_block` call — no allocation on the hot path (mirrors the D-6.2 pattern: scratch
/// owned by the caller, sized at preparation time). `z_buf` always holds a materialized copy of
/// `z` even in the (common) ungated case, trading one cheap memcpy for keeping every buffer a
/// distinct, unambiguously-disjoint struct field — simpler to get right than aliasing `z` onto
/// `conv_buf` would have been, for a cost nowhere near the allocator overhead it replaced.
struct LayerScratch {
    history: Sig,  // in_ch * history_len
    padded: Sig,   // in_ch * (history_len + max_n)
    conv_buf: Sig, // dilated.out_ch * max_n  (out_ch is 2x channels when gated)
    z_buf: Sig,    // channels * max_n
}

impl Layer {
    /// Allocation-free: writes into `scratch`'s buffers, accumulates into `head_sum`, and
    /// writes the next layer's input into caller-provided `next_input_out`. `layer_input`,
    /// `condition`, `head_sum` and `next_input_out` are all flat `[channels * n]` (`condition`
    /// is `[condition_size * n]`).
    fn apply_into(
        &self,
        layer_input: &[f32],
        condition: &[f32],
        n: usize,
        scratch: &mut LayerScratch,
        head_sum: &mut [f32],
        next_input_out: &mut [f32],
    ) {
        let conv_len = self.dilated.out_ch * n;
        let z_len = self.channels * n;

        self.dilated.apply_into(
            layer_input,
            n,
            &mut scratch.history,
            &mut scratch.padded,
            &mut scratch.conv_buf[..conv_len],
        );
        self.mixin
            .apply_add_into(condition, n, &mut scratch.conv_buf[..conv_len]);

        if self.gated {
            let (top, bottom) = scratch.conv_buf[..conv_len].split_at_mut(z_len);
            activation_apply(&self.activation, top);
            activation_apply("Sigmoid", bottom);
            for i in 0..z_len {
                scratch.z_buf[i] = top[i] * bottom[i];
            }
        } else {
            activation_apply(&self.activation, &mut scratch.conv_buf[..z_len]);
            scratch.z_buf[..z_len].copy_from_slice(&scratch.conv_buf[..z_len]);
        }

        for (s, z) in head_sum[..z_len].iter_mut().zip(scratch.z_buf[..z_len].iter()) {
            *s += z;
        }

        self.residual
            .apply_into(&scratch.z_buf[..z_len], n, &mut next_input_out[..z_len]);
        for i in 0..z_len {
            next_input_out[i] += layer_input[i];
        }
    }
}

struct LayerArray {
    rechannel: Conv1x1,
    layers: Vec<Layer>,
    head_rechannel: Conv1x1,
    input_size: usize,
    channels: usize,
    head_size: usize,
}

/// Per-array reusable scratch. `io_buf` is a ping-pong pair used as "current layer input" /
/// "next layer input" across the array's layer loop, so no per-layer allocation is needed for
/// that hand-off either; which half holds the final (trunk) value after the loop is
/// `layers.len() % 2` (deterministic from the immutable `LayerArray`, not stored here).
struct ArrayScratch {
    io_buf: [Sig; 2], // channels * max_n each
    head_sum: Sig,    // channels * max_n
    head_out: Sig,    // head_size * max_n
    layers: Vec<LayerScratch>,
}

/// Immutable, `Sync` weights and configuration. Shareable across instances (D-8.2 / D-9.1).
pub struct PreparedWaveNet {
    arrays: Vec<LayerArray>,
    head_scale: f32,
}

unsafe impl Sync for PreparedWaveNet {}

/// Per-instance mutable inference state: history plus reusable scratch, all sized once for a
/// chosen max block size. Never shared across instances (D-9.1).
pub struct WaveNetState {
    max_n: usize,
    condition: Sig, // max_n (mono audio input, reused as every layer's condition signal)
    arrays: Vec<ArrayScratch>,
}

impl WaveNetState {
    fn new(prepared: &PreparedWaveNet, max_n: usize) -> Self {
        let arrays = prepared
            .arrays
            .iter()
            .map(|arr| {
                let layers = arr
                    .layers
                    .iter()
                    .map(|layer| {
                        let hl = layer.dilated.history_len();
                        LayerScratch {
                            history: vec![0f32; hl * layer.dilated.in_ch],
                            padded: vec![0f32; layer.dilated.in_ch * (hl + max_n)],
                            conv_buf: vec![0f32; layer.dilated.out_ch * max_n],
                            z_buf: vec![0f32; layer.channels * max_n],
                        }
                    })
                    .collect();
                ArrayScratch {
                    io_buf: [
                        vec![0f32; arr.channels * max_n],
                        vec![0f32; arr.channels * max_n],
                    ],
                    head_sum: vec![0f32; arr.channels * max_n],
                    head_out: vec![0f32; arr.head_size * max_n],
                    layers,
                }
            })
            .collect();
        Self {
            max_n,
            condition: vec![0f32; max_n],
            arrays,
        }
    }
}

struct WeightReader<'a> {
    weights: &'a [f32],
    pos: usize,
}

impl<'a> WeightReader<'a> {
    fn new(weights: &'a [f32]) -> Self {
        Self { weights, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<Vec<f32>, NamError> {
        if self.pos + n > self.weights.len() {
            return Err(NamError(format!(
                "weight array exhausted: need {} more floats at offset {}, only {} available",
                n,
                self.pos,
                self.weights.len() - self.pos
            )));
        }
        let slice = self.weights[self.pos..self.pos + n].to_vec();
        self.pos += n;
        Ok(slice)
    }
}

impl PreparedWaveNet {
    pub fn from_nam_file(nam: &NamFile) -> Result<Self, NamError> {
        if nam.architecture != "WaveNet" {
            return Err(NamError(format!(
                "unsupported architecture: {}",
                nam.architecture
            )));
        }
        if nam.config.head.is_some() {
            return Err(NamError(
                "post-stack `head` config is not supported by this spike (ordinary exported \
                 models leave it null)"
                    .into(),
            ));
        }

        let mut r = WeightReader::new(&nam.weights);
        let mut arrays = Vec::with_capacity(nam.config.layers.len());
        for cfg in &nam.config.layers {
            let out_mult = if cfg.gated { 2 } else { 1 };
            // No bias: NeuralAmpModelerCore's LayerArray ctor constructs `_rechannel` with
            // bias=false (`Conv1x1(input_size, channels, false)` in NAM/wavenet/detail.h),
            // confirmed by reading that constructor directly.
            let rechannel = Conv1x1::read(&mut r, cfg.channels, cfg.input_size, false)?;

            let mut layers = Vec::with_capacity(cfg.dilations.len());
            for &dilation in &cfg.dilations {
                let dilated = Conv1D::read(
                    &mut r,
                    cfg.channels * out_mult,
                    cfg.channels,
                    cfg.kernel_size,
                    dilation,
                )?;
                let mixin =
                    Conv1x1::read(&mut r, cfg.channels * out_mult, cfg.condition_size, false)?;
                let residual = Conv1x1::read(&mut r, cfg.channels, cfg.channels, true)?;
                layers.push(Layer {
                    dilated,
                    mixin,
                    residual,
                    activation: cfg.activation.clone(),
                    gated: cfg.gated,
                    channels: cfg.channels,
                });
            }

            let head_rechannel = Conv1x1::read(&mut r, cfg.head_size, cfg.channels, cfg.head_bias)?;

            arrays.push(LayerArray {
                rechannel,
                layers,
                head_rechannel,
                input_size: cfg.input_size,
                channels: cfg.channels,
                head_size: cfg.head_size,
            });
        }

        // Adjacent arrays chain via TWO separate signals, confirmed by reading
        // WaveNet::process/LayerArray::ProcessInner directly (this is not the single shared
        // value earlier secondary-source research assumed):
        //   - the residual "trunk" output (GetLayerOutputs, dim = channels) feeds the next
        //     array's rechannel input, so array i+1's input_size must equal array i's channels;
        //   - the head-rechannel output (GetHeadOutputs, dim = head_size) separately seeds the
        //     next array's head accumulator, so array i+1's channels must equal array i's
        //     head_size (this one *is* enforced by the C++ constructor itself).
        for (i, w) in arrays.windows(2).enumerate() {
            if w[0].head_size != w[1].channels {
                return Err(NamError(format!(
                    "layer array {i} chaining mismatch: head_size {} does not match next array's \
                     channels {}",
                    w[0].head_size, w[1].channels
                )));
            }
            if w[0].channels != w[1].input_size {
                return Err(NamError(format!(
                    "layer array {i} chaining mismatch: channels {} does not match next array's \
                     input_size {}",
                    w[0].channels, w[1].input_size
                )));
            }
        }

        // The trailing float in the weights array is the authoritative head_scale (it's what
        // NeuralAmpModelerCore's WaveNet::set_weights_ actually uses; config.head_scale is
        // parsed but unconditionally overwritten by this trailing weight in the reference
        // implementation, though a correctly-exported file has them equal).
        let head_scale = if r.pos == nam.weights.len() - 1 {
            nam.weights[nam.weights.len() - 1]
        } else if r.pos == nam.weights.len() {
            nam.config.head_scale
        } else {
            return Err(NamError(format!(
                "weight count mismatch: consumed {} of {} floats (expected {} or {})",
                r.pos,
                nam.weights.len(),
                r.pos,
                r.pos + 1
            )));
        };

        Ok(Self { arrays, head_scale })
    }

    /// `max_n` is the largest block size this state will ever be asked to process; every
    /// scratch buffer is sized once, here, and reused for the state's whole lifetime.
    pub fn new_state(&self, max_n: usize) -> WaveNetState {
        WaveNetState::new(self, max_n)
    }

    /// Processes one block of mono input samples (`input.len() <= state`'s `max_n`), returning
    /// one block of mono output samples. Allocates exactly once, for the returned `Vec` itself —
    /// every intermediate buffer lives in `state` and is reused block to block.
    ///
    /// Two distinct signals thread between arrays (confirmed against `WaveNet::process` /
    /// `LayerArray::ProcessInner` directly — they are *not* the same tensor): the residual
    /// "trunk" (`GetLayerOutputs`, dim = `channels`) feeds the next array's rechannel input,
    /// while the head-rechannel output (`GetHeadOutputs`, dim = `head_size`) separately seeds
    /// the next array's head accumulator. The trunk lives in one half of each array's `io_buf`
    /// ping-pong pair; which half is determined by that array's layer count parity.
    pub fn process_block(&self, state: &mut WaveNetState, input: &[f32]) -> Vec<f32> {
        let out_len = self
            .arrays
            .last()
            .expect("at least one layer array")
            .head_size
            * input.len();
        let mut out = vec![0f32; out_len];
        self.process_block_into(state, input, &mut out);
        out
    }

    /// Same as `process_block`, but writes into a caller-provided `out` buffer
    /// (`out.len() == head_size(last array) * input.len()`) instead of allocating. Fully
    /// allocation-free — used by the NFR-PERF-010 benchmark so the measurement isn't diluted by
    /// an allocation `process_block`'s convenience API would otherwise cost every block.
    pub fn process_block_into(&self, state: &mut WaveNetState, input: &[f32], out: &mut [f32]) {
        let n = input.len();
        assert!(
            n <= state.max_n,
            "block size {n} exceeds this state's preallocated max {}",
            state.max_n
        );

        let WaveNetState {
            condition,
            arrays: state_arrays,
            ..
        } = state;
        condition[..n].copy_from_slice(input);

        for (a, arr) in self.arrays.iter().enumerate() {
            let clen = arr.channels * n;
            let (before, at_and_after) = state_arrays.split_at_mut(a);
            let ascratch = &mut at_and_after[0];

            if a == 0 {
                arr.rechannel
                    .apply_into(&condition[..n], n, &mut ascratch.io_buf[0][..clen]);
                ascratch.head_sum[..clen].fill(0.0);
            } else {
                let prev_arr = &self.arrays[a - 1];
                let prev_scratch = &before[a - 1];
                let prev_final_cur = prev_arr.layers.len() % 2;
                let in_len = arr.input_size * n;
                arr.rechannel.apply_into(
                    &prev_scratch.io_buf[prev_final_cur][..in_len],
                    n,
                    &mut ascratch.io_buf[0][..clen],
                );
                ascratch.head_sum[..clen].copy_from_slice(&prev_scratch.head_out[..clen]);
            }

            let mut cur = 0usize;
            for (l, layer) in arr.layers.iter().enumerate() {
                let (buf0, buf1) = ascratch.io_buf.split_at_mut(1);
                let (read_buf, write_buf): (&[f32], &mut [f32]) = if cur == 0 {
                    (&buf0[0][..clen], &mut buf1[0][..clen])
                } else {
                    (&buf1[0][..clen], &mut buf0[0][..clen])
                };
                layer.apply_into(
                    read_buf,
                    &condition[..n],
                    n,
                    &mut ascratch.layers[l],
                    &mut ascratch.head_sum[..clen],
                    write_buf,
                );
                cur = 1 - cur;
            }

            let head_len = arr.head_size * n;
            arr.head_rechannel.apply_into(
                &ascratch.head_sum[..clen],
                n,
                &mut ascratch.head_out[..head_len],
            );
        }

        let last_arr = self.arrays.last().expect("at least one layer array");
        let last_scratch = state_arrays.last().expect("at least one layer array");
        let out_len = last_arr.head_size * n;
        for (dst, &src) in out[..out_len]
            .iter_mut()
            .zip(last_scratch.head_out[..out_len].iter())
        {
            *dst = src * self.head_scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conv1x1_matches_hand_computation() {
        // out_ch=2, in_ch=2. weight = [[1,2],[3,4]] row-major, bias=[10,20].
        let conv = Conv1x1 {
            out_ch: 2,
            in_ch: 2,
            weight: vec![1.0, 2.0, 3.0, 4.0],
            bias: Some(vec![10.0, 20.0]),
        };
        // input channel 0 = [1,0], channel 1 = [0,1], flat = [1,0, 0,1].
        let input: Sig = vec![1.0, 0.0, 0.0, 1.0];
        let out = conv.apply(&input, 2);
        // out[0][0] = 10 + 1*1 + 2*0 = 11 ; out[0][1] = 10 + 1*0 + 2*1 = 12
        // out[1][0] = 20 + 3*1 + 4*0 = 23 ; out[1][1] = 20 + 3*0 + 4*1 = 24
        assert_eq!(&out[0..2], &[11.0, 12.0]);
        assert_eq!(&out[2..4], &[23.0, 24.0]);
    }

    #[test]
    fn dilated_conv_impulse_response_lands_at_expected_taps() {
        // 1 in, 1 out, kernel_size=3, dilation=2. weight taps [w0, w1, w2], bias 0.
        // Feed a unit impulse at t=0 of an otherwise-silent block, with zero history.
        // Expected: output[t] picks up weight w_k whenever t == k*dilation (per the
        // padded-index derivation: padded_index = i + k*dilation for output i, and the
        // impulse sits at the boundary between history (all zero) and the block at padded
        // index = history_len = (K-1)*dilation = 4).
        let conv = Conv1D {
            out_ch: 1,
            in_ch: 1,
            kernel_size: 3,
            dilation: 2,
            weight: vec![5.0, 7.0, 11.0], // w0, w1, w2
            bias: vec![0.0],
        };
        let hl = conv.history_len();
        assert_eq!(hl, 4);
        let mut history: Sig = vec![0f32; hl];
        let n = 8;
        let mut input: Sig = vec![0f32; n];
        input[0] = 1.0; // impulse at block-local t=0, i.e. padded index hl.
        let out = conv.apply(&input, n, &mut history);

        // out[t] = sum_k w[k] * padded[t + k*dilation]. padded[hl] = 1, all else 0.
        // t + k*dilation == hl=4 => t == 4 - 2k. k=0: t=4 (w0). k=1: t=2 (w1). k=2: t=0 (w2).
        let mut expected = vec![0f32; n];
        expected[4] = 5.0;
        expected[2] = 7.0;
        expected[0] = 11.0;
        assert_eq!(out, expected);
    }

    #[test]
    fn history_carries_across_blocks() {
        // Same conv as above (kernel_size=3, dilation=2, weights [5,7,11], history_len=4), but
        // place the impulse at the *end* of a 4-sample block1 so most of its response falls in
        // block2, and verify against the equivalent single continuous 8-sample run (test above
        // showed taps land at offsets 0/2/4 before a global impulse position; here the impulse
        // is at global index 7, so responses land at global 7 (w2), 5 (w1), 3 (w0)).
        let conv = Conv1D {
            out_ch: 1,
            in_ch: 1,
            kernel_size: 3,
            dilation: 2,
            weight: vec![5.0, 7.0, 11.0],
            bias: vec![0.0],
        };
        let mut history: Sig = vec![0f32; 4];
        let block1: Sig = vec![0.0, 0.0, 0.0, 1.0]; // impulse at global index 3 (block-local t=3)
        let out1 = conv.apply(&block1, 4, &mut history);
        // Global index 3 is only reachable by tap k=2 (offset 0) at t=3 itself: w2=11.
        assert_eq!(out1, vec![0.0, 0.0, 0.0, 11.0]);

        let block2: Sig = vec![0.0, 0.0, 0.0, 0.0];
        let out2 = conv.apply(&block2, 4, &mut history);
        // block2-local t maps to global index 4+t. Global 5 (t=1) is impulse+2 => tap k=1: w1=7.
        // Global 7 (t=3) is impulse+4 => tap k=0: w0=5.
        assert_eq!(out2, vec![0.0, 7.0, 0.0, 5.0]);
    }
}
