//! A minimal, whole-signal WaveNet forward pass used *only* to validate generated fixtures
//! (the RMS/degeneracy check D-19.1 requires of the generator itself). This is deliberately not
//! shared with `namir-engine` — that crate is being built independently in parallel and this
//! crate must not take a dependency on it — and it is not written for the RT audio path: no
//! blockwise state, no scratch reuse, allocates freely. It exists to answer one question
//! ("is this fixture's output sane?"), not to be a second inference implementation to keep in
//! sync with the real one.
//!
//! Operation order and flat-weight-array layout follow the same, separately-confirmed-against
//! `NeuralAmpModelerCore` semantics documented in `spikes/s1-nam-inference/src/lib.rs`: two
//! distinct signals thread between layer arrays (the residual "trunk" and the head-sum seed),
//! and the flat weights array carries a trailing `head_scale` float after all per-array weights.
//! Only the subset this crate's generator ever emits is supported (ungated, `Tanh`), since a
//! generator that never emits gated/other-activation models has no fixture to validate that path
//! against, and there is no reference JSON to test it correctly on.

use super::{LayerArrayConfig, NamModel};

struct WeightReader<'a> {
    weights: &'a [f32],
    pos: usize,
}

impl<'a> WeightReader<'a> {
    fn take(&mut self, n: usize) -> &'a [f32] {
        let slice = &self.weights[self.pos..self.pos + n];
        self.pos += n;
        slice
    }
}

/// `weight`: row-major `[out_ch][in_ch]`. `input`/return: flat `[ch * n]`.
fn conv1x1(weight: &[f32], bias: Option<&[f32]>, out_ch: usize, in_ch: usize, input: &[f32], n: usize) -> Vec<f32> {
    let mut out = vec![0f32; out_ch * n];
    for oc in 0..out_ch {
        let b = bias.map_or(0.0, |b| b[oc]);
        let out_row = &mut out[oc * n..(oc + 1) * n];
        out_row.fill(b);
        for ic in 0..in_ch {
            let w = weight[oc * in_ch + ic];
            let in_row = &input[ic * n..(ic + 1) * n];
            for t in 0..n {
                out_row[t] += w * in_row[t];
            }
        }
    }
    out
}

/// Causal dilated conv over a whole signal at once: history is implicitly all-zero before t=0,
/// so (unlike a blockwise engine) there's no state to carry — just left-zero-pad by
/// `(kernel_size - 1) * dilation` once. `weight`: row-major `[channels][channels][kernel]`, tap
/// `k = kernel_size - 1` is the current sample (matches PyTorch's native `Conv1d.weight` flatten,
/// per the S-1 spike's confirmed reading of the reference implementation). Channel count in
/// equals channel count out: this generator never emits gated models, whose dilated conv would
/// double the output channel count.
fn dilated_conv(weight: &[f32], bias: &[f32], channels: usize, kernel_size: usize, dilation: usize, input: &[f32], n: usize) -> Vec<f32> {
    let hl = (kernel_size - 1) * dilation;
    let pn = hl + n;
    let mut padded = vec![0f32; channels * pn];
    for ic in 0..channels {
        padded[ic * pn + hl..ic * pn + pn].copy_from_slice(&input[ic * n..(ic + 1) * n]);
    }

    let mut out = vec![0f32; channels * n];
    for oc in 0..channels {
        let out_row = &mut out[oc * n..(oc + 1) * n];
        out_row.fill(bias[oc]);
        for ic in 0..channels {
            let p = &padded[ic * pn..(ic + 1) * pn];
            for k in 0..kernel_size {
                let w = weight[(oc * channels + ic) * kernel_size + k];
                if w == 0.0 {
                    continue;
                }
                let offset = k * dilation;
                for t in 0..n {
                    out_row[t] += w * p[t + offset];
                }
            }
        }
    }
    out
}

fn tanh_inplace(x: &mut [f32]) {
    for v in x.iter_mut() {
        *v = v.tanh();
    }
}

/// `array_input`: this array's actual input (the model's raw signal for the first array, or the
/// previous array's residual trunk thereafter). `condition`: the mixin conditioning signal,
/// which per NAM's WaveNet is *always* the model's original raw input at every layer of every
/// array, not the local `array_input` — the two coincide only for the first array, which is why
/// this distinction is easy to miss (and was called out explicitly in the S-1 spike's own
/// notes on the two-signal chaining between arrays).
/// `head_seed`: the accumulator's initial contents — all zero for the first array, or the
/// previous array's head-rechannel output thereafter (the second of the two distinct signals
/// that thread between arrays; see the module doc comment).
fn run_array(
    cfg: &LayerArrayConfig,
    r: &mut WeightReader,
    array_input: &[f32],
    condition: &[f32],
    head_seed: &[f32],
    n: usize,
) -> (Vec<f32>, Vec<f32>) {
    assert_eq!(cfg.activation, "Tanh", "generator never emits non-Tanh models");
    assert!(!cfg.gated, "generator never emits gated models");

    let rechannel_w = r.take(cfg.channels * cfg.input_size);
    let mut trunk = conv1x1(rechannel_w, None, cfg.channels, cfg.input_size, array_input, n);

    let mut head_sum = head_seed.to_vec();
    for &dilation in &cfg.dilations {
        let dilated_w = r.take(cfg.channels * cfg.channels * cfg.kernel_size);
        let dilated_b = r.take(cfg.channels);
        let mut z = dilated_conv(dilated_w, dilated_b, cfg.channels, cfg.kernel_size, dilation, &trunk, n);

        let mixin_w = r.take(cfg.channels * cfg.condition_size);
        let mixin = conv1x1(mixin_w, None, cfg.channels, cfg.condition_size, condition, n);
        for (a, b) in z.iter_mut().zip(mixin.iter()) {
            *a += b;
        }

        tanh_inplace(&mut z);
        for (s, v) in head_sum.iter_mut().zip(z.iter()) {
            *s += v;
        }

        let residual_w = r.take(cfg.channels * cfg.channels);
        let residual_b = r.take(cfg.channels);
        let residual = conv1x1(residual_w, Some(residual_b), cfg.channels, cfg.channels, &z, n);
        for (t, v) in trunk.iter_mut().zip(residual.iter()) {
            *t += v;
        }
    }

    let head_w = r.take(cfg.head_size * cfg.channels);
    let head_b = if cfg.head_bias { Some(r.take(cfg.head_size)) } else { None };
    let head_out = conv1x1(head_w, head_b, cfg.head_size, cfg.channels, &head_sum, n);

    (trunk, head_out)
}

/// Runs `model` over `input` (mono) and returns the mono output, scaled by `head_scale`.
/// Panics on malformed weight counts or unsupported config — acceptable here because this
/// function only ever runs against this crate's own generator output, never external input.
pub(super) fn run(model: &NamModel, input: &[f32]) -> Vec<f32> {
    let n = input.len();
    let mut r = WeightReader { weights: &model.weights, pos: 0 };

    let mut cur_trunk = input.to_vec();
    // Doubles as both "seed for the next array's head accumulator" and, after the loop, the
    // final array's own head output — the two are the same value by construction.
    let mut head_out = Vec::new();
    for (i, cfg) in model.config.layers.iter().enumerate() {
        let array_input: &[f32] = if i == 0 { input } else { &cur_trunk };
        let head_seed = if i == 0 { vec![0f32; cfg.channels * n] } else { head_out };
        let (trunk, next_head_out) = run_array(cfg, &mut r, array_input, input, &head_seed, n);
        cur_trunk = trunk;
        head_out = next_head_out;
    }
    let last_head_out = head_out;

    // Trailing weight is the authoritative head_scale (matches the real .nam format per the S-1
    // spike's confirmed reading; `config.head_scale` is kept equal but this reads the buffer the
    // way any real consumer must).
    assert_eq!(r.pos, model.weights.len() - 1, "unexpected weight count");
    let head_scale = model.weights[model.weights.len() - 1];

    last_head_out.iter().map(|&v| v * head_scale).collect()
}
