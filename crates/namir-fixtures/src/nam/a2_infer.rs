//! A minimal, whole-signal A2 forward pass used *only* to validate generated A2 fixtures (the
//! RMS/degeneracy check D-19.1 requires of the generator itself). The A2 counterpart of
//! `infer.rs` — see that module's doc comment, which applies here unchanged: not shared with
//! `namir-engine`, not written for the RT audio path (no blockwise state, no scratch reuse,
//! allocates freely), and exists to answer one question ("is this fixture's output sane?"), not
//! to be a second inference implementation kept in sync with `namir-nam`'s real one. **This module
//! is written independently of `crates/namir-nam`'s own A2 work** (R-9, `docs/02-architecture.md`
//! §22): it is derived only from `NeuralAmpModelerCore` (pinned `3cde95c`, `C:\namref`), never from
//! reading or consulting that crate.
//!
//! A2's per-layer computation differs structurally from A1's, not just in width. Read alongside
//! `super::build_a2_weights`'s doc comment (the weight order this module consumes), the essential
//! difference from A1 is:
//!
//! - The dilated conv and mixin now write to `bottleneck` channels, not `channels` — A1 never
//!   distinguished the two.
//! - There is no separate "residual conv" module. The single `layer1x1` projection (`bottleneck ->
//!   channels`) *is* the residual path: `trunk_next = trunk_in + layer1x1(activated z)`, structurally
//!   identical to A1's `residual` step (`infer.rs`'s `run_array`), just narrower on the way in.
//! - The per-layer head contribution is the *activated* `z` itself (bottleneck width), summed
//!   across every layer in the array — exactly like A1's `head_sum += z` — but unlike A1, that sum
//!   is *not* the array's final head output. It is fed through a genuine k-tap, dilated causal
//!   `Conv1D` (`head_rechannel`, `bottleneck -> head.out_channels`, kernel/dilation/bias from each
//!   array's own nested `head` config) after all layers have contributed. A1's per-array head step
//!   is a bare 1x1; A2's is a real convolution — this is the one genuinely new piece of DSP A2
//!   needs versus A1 (per `LayerArray::ProcessInner`, `model.cpp:450-511`, and `LayerArray`'s
//!   constructor, `model.cpp:380-384`).
//! - Activation is `LeakyReLU` (`x > 0 ? x : negative_slope * x`), read from each layer's own JSON
//!   config, not `Tanh`.
//!
//! Confirmed against `model.cpp:744-772` (`WaveNet::process`): the `condition` signal fed to every
//! layer array's mixin is *always* the model's original raw input, never a local array's own
//! input — the same two-signal convention `infer.rs` already documents for A1, unchanged for A2.

use super::{A2HeadRechannelConfig, A2LayerArrayConfig, A2Model};

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

/// `weight`: row-major `[out_ch][in_ch]`. `input`/return: flat `[ch * n]`. Identical in shape and
/// semantics to `infer.rs`'s `conv1x1` — kept as a separate copy rather than shared, per this
/// module's independence from any other crate's own inference code (and to keep this module
/// self-contained the same way `infer.rs` is).
fn conv1x1(
    weight: &[f32],
    bias: Option<&[f32]>,
    out_ch: usize,
    in_ch: usize,
    input: &[f32],
    n: usize,
) -> Vec<f32> {
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

/// Channel/kernel/dilation shape for one `conv1d` call, bundled into its own type (rather than
/// four more scalar parameters) purely to keep `conv1d` under clippy's argument-count lint —
/// `infer.rs`'s `dilated_conv` stays under it for free by assuming `in_ch == out_ch`, which does
/// not hold here (see `conv1d`'s own doc comment).
struct ConvShape {
    out_ch: usize,
    in_ch: usize,
    kernel_size: usize,
    dilation: usize,
}

/// General causal, left-zero-padded dilated `Conv1D`: unlike `infer.rs`'s `dilated_conv` (which
/// assumes `in_ch == out_ch`, true for every A1 layer since A1 has no `bottleneck`/`channels`
/// distinction), A2's dilated conv maps `channels -> bottleneck` and its per-array head rechannel
/// maps `bottleneck -> head.out_channels`, so this takes both counts independently. `weight`:
/// row-major `[out_ch][in_ch][kernel]`, tap `k = kernel_size - 1` is the current sample
/// (`Conv1D::set_weights_`, `conv1d.cpp:38-50`: for each output channel, for each input channel,
/// for each tap — tap innermost, matching PyTorch's `(out, in/groups, k)` row-major flatten;
/// `groups == 1` always for core A2, so the group loop collapses and is not modeled here). Bias is
/// mandatory (unlike `conv1x1`'s optional bias): every `Conv1D` this module builds always has one
/// (the dilated conv's `do_bias` is unconditionally `true`, `detail.h:45-46`; this crate always
/// generates `head.bias == true`, matching both real A2 shapes).
fn conv1d(weight: &[f32], bias: &[f32], shape: ConvShape, input: &[f32], n: usize) -> Vec<f32> {
    let ConvShape {
        out_ch,
        in_ch,
        kernel_size,
        dilation,
    } = shape;
    let hl = (kernel_size - 1) * dilation;
    let pn = hl + n;
    let mut padded = vec![0f32; in_ch * pn];
    for ic in 0..in_ch {
        padded[ic * pn + hl..ic * pn + pn].copy_from_slice(&input[ic * n..(ic + 1) * n]);
    }

    let mut out = vec![0f32; out_ch * n];
    for oc in 0..out_ch {
        let out_row = &mut out[oc * n..(oc + 1) * n];
        out_row.fill(bias[oc]);
        for ic in 0..in_ch {
            let p = &padded[ic * pn..(ic + 1) * pn];
            for k in 0..kernel_size {
                let w = weight[(oc * in_ch + ic) * kernel_size + k];
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

/// `LeakyReLU(x) = x if x >= 0 else negative_slope * x` (`activations.h`/`activations.cpp`),
/// applied in place.
fn leaky_relu_inplace(x: &mut [f32], negative_slope: f32) {
    for v in x.iter_mut() {
        if *v < 0.0 {
            *v *= negative_slope;
        }
    }
}

/// `array_input`: this array's actual input (the model's raw signal for the first array, or the
/// previous array's residual trunk thereafter). `condition`: the mixin conditioning signal, always
/// the model's original raw input at every layer of every array (see this module's doc comment).
/// `head_seed`: the accumulator's initial contents — all zero for the first array, or the previous
/// array's head-rechannel output thereafter (the second of the two signals that thread between
/// arrays; mirrors `infer.rs`'s `run_array` doc comment, generalized to A2's real convolutional
/// head). Returns `(trunk, head_out)`, where `head_out` is this array's own head-rechannel output
/// — width `cfg.head.out_channels`, not `cfg.bottleneck` (unlike the seed it started from).
fn run_array(
    cfg: &A2LayerArrayConfig,
    r: &mut WeightReader,
    array_input: &[f32],
    condition: &[f32],
    head_seed: &[f32],
    n: usize,
) -> (Vec<f32>, Vec<f32>) {
    assert_eq!(
        cfg.dilations.len(),
        cfg.kernel_sizes.len(),
        "kernel_sizes/dilations length mismatch"
    );
    assert_eq!(
        cfg.dilations.len(),
        cfg.activation.len(),
        "activation length mismatch"
    );
    assert!(
        cfg.layer1x1.active,
        "generator never emits layer1x1-inactive A2 models"
    );

    let rechannel_w = r.take(cfg.channels * cfg.input_size);
    let mut trunk = conv1x1(
        rechannel_w,
        None,
        cfg.channels,
        cfg.input_size,
        array_input,
        n,
    );

    let mut head_sum = head_seed.to_vec();
    for (li, &dilation) in cfg.dilations.iter().enumerate() {
        let kernel_size = cfg.kernel_sizes[li];
        let act = &cfg.activation[li];
        assert_eq!(
            act.kind, "LeakyReLU",
            "generator never emits non-LeakyReLU A2 models"
        );

        let dilated_w = r.take(cfg.bottleneck * cfg.channels * kernel_size);
        let dilated_b = r.take(cfg.bottleneck);
        let mut z = conv1d(
            dilated_w,
            dilated_b,
            ConvShape {
                out_ch: cfg.bottleneck,
                in_ch: cfg.channels,
                kernel_size,
                dilation,
            },
            &trunk,
            n,
        );

        let mixin_w = r.take(cfg.bottleneck * cfg.condition_size);
        let mixin = conv1x1(
            mixin_w,
            None,
            cfg.bottleneck,
            cfg.condition_size,
            condition,
            n,
        );
        for (a, b) in z.iter_mut().zip(mixin.iter()) {
            *a += b;
        }

        leaky_relu_inplace(&mut z, act.negative_slope);
        for (s, v) in head_sum.iter_mut().zip(z.iter()) {
            *s += v;
        }

        let layer1x1_w = r.take(cfg.channels * cfg.bottleneck);
        let layer1x1_b = r.take(cfg.channels);
        let layer1x1_out = conv1x1(
            layer1x1_w,
            Some(layer1x1_b),
            cfg.channels,
            cfg.bottleneck,
            &z,
            n,
        );
        for (t, v) in trunk.iter_mut().zip(layer1x1_out.iter()) {
            *t += v;
        }
    }

    let head_out = run_head_rechannel(&cfg.head, r, cfg.bottleneck, &head_sum, n);
    (trunk, head_out)
}

/// The layer array's own nested `head`: a real dilated causal `Conv1D` (`bottleneck ->
/// head.out_channels`), not a 1x1 — see this module's doc comment. `head.bias` is always `true`
/// for fixtures this crate generates (matching both real A2 shapes), so the bias read is
/// unconditional, unlike A1's optional `head_bias`.
fn run_head_rechannel(
    head: &A2HeadRechannelConfig,
    r: &mut WeightReader,
    bottleneck: usize,
    head_sum: &[f32],
    n: usize,
) -> Vec<f32> {
    assert!(
        head.bias,
        "generator always emits head.bias == true (matches both real A2 shapes)"
    );
    let head_w = r.take(head.out_channels * bottleneck * head.kernel_size);
    let head_b = r.take(head.out_channels);
    conv1d(
        head_w,
        head_b,
        ConvShape {
            out_ch: head.out_channels,
            in_ch: bottleneck,
            kernel_size: head.kernel_size,
            dilation: head.head_dilation,
        },
        head_sum,
        n,
    )
}

/// Runs `model` over `input` (mono) and returns the mono output, scaled by `head_scale`. Panics on
/// malformed weight counts or unsupported config — acceptable here because this function only
/// ever runs against this crate's own generator output, never external input.
pub(super) fn run(model: &A2Model, input: &[f32]) -> Vec<f32> {
    let n = input.len();
    let mut r = WeightReader {
        weights: &model.weights,
        pos: 0,
    };

    let mut cur_trunk = input.to_vec();
    // Doubles as both "seed for the next array's head accumulator" and, after the loop, the
    // final array's own head output — the two are the same value by construction (mirrors
    // `infer.rs`'s `run` for A1).
    let mut head_out = Vec::new();
    for (i, cfg) in model.config.layers.iter().enumerate() {
        let array_input: &[f32] = if i == 0 { input } else { &cur_trunk };
        let head_seed = if i == 0 {
            vec![0f32; cfg.bottleneck * n]
        } else {
            head_out
        };
        let (trunk, next_head_out) = run_array(cfg, &mut r, array_input, input, &head_seed, n);
        cur_trunk = trunk;
        head_out = next_head_out;
    }
    let last_head_out = head_out;

    // Trailing weight is the authoritative head_scale (matches the real .nam format, confirmed
    // against `WaveNet::set_weights_`, `model.cpp:632`, and independently against
    // `A2FastModel::_load_weights`, `a2_fast.cpp:268-269`); `config.head_scale` is kept equal but
    // this reads the buffer the way any real consumer must.
    assert_eq!(r.pos, model.weights.len() - 1, "unexpected weight count");
    let head_scale = model.weights[model.weights.len() - 1];

    last_head_out.iter().map(|&v| v * head_scale).collect()
}
