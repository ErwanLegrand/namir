//! A from-scratch WaveNet inference engine, ported from `spikes/s1-nam-inference/src/lib.rs`.
//! Operation order and flat-weight-array layout are matched to `sdatkinson/NeuralAmpModelerCore`
//! (`NAM/wavenet/model.cpp`), confirmed by reading that project's source directly, not guessed —
//! see `spikes/s1-nam-inference/README.md`'s "Key facts established by reading
//! `NeuralAmpModelerCore` source directly" section for the citations, in particular the
//! **two distinct signals** that cross a layer-array boundary (the residual "trunk", dimension
//! `channels`, feeding the next array's rechannel input; and the head-rechannel output, dimension
//! `head_size`, separately seeding the next array's head-sum accumulator) — conflating these two
//! was the spike's own earlier bug, and this port preserves the two-buffer structure specifically
//! to not reintroduce it.
//!
//! `PreparedWaveNet` holds only immutable weights and configuration (and is `Sync` — no
//! `unsafe impl` needed, see below); `WaveNetState` holds only the per-instance causal-conv
//! history and reusable scratch. This is D-9.1's split, structural rather than conventional, same
//! as the spike.
//!
//! The one thing this port changes structurally from the spike (beyond the rename) is validation:
//! the spike used `panic!`/`assert!` because it only ever saw its own trusted generator's output.
//! This crate parses untrusted files (P6, FR-NAM-040), so every one of the spike's implicit
//! assumptions becomes an explicit, catalogued `Result` failure in `PreparedWaveNet::from_file`,
//! including a class of check the spike never needed at all: per NFR-SEC-020, every config
//! dimension is validated against a documented ceiling *before* any arithmetic or allocation is
//! derived from it. That ordering is load-bearing, not decorative — see the comment at the top of
//! `PreparedWaveNet::from_file` for why.
//!
//! # `PreparedWaveNet`/`WaveNetState`, not `PreparedNam`/`NamState`
//!
//! This module used to export `PreparedNam`/`NamState` directly (back when WaveNet was the only
//! architecture this crate supported). Now that `lstm.rs` implements FR-NAM-020's other Must
//! architecture, `PreparedNam`/`NamState` have moved to `model.rs`, as a small enum wrapping this
//! module's `PreparedWaveNet`/`WaveNetState` and `lstm.rs`'s `PreparedLstm`/`LstmState`. This
//! module's own types keep the plain, architecture-specific names an enum variant should have;
//! `model.rs`'s doc comment explains the forwarding.

use namir_core::SampleRate;
use wide::f32x8;

use crate::error_codes::{self, NamLoadError};
use crate::file::{LayerArrayConfig, NamFile, NamMetadata};
use crate::shared::{WeightReader, check_max, check_min1};

/// A flat, row-major multi-channel signal buffer: `data[channel * n + t]`. Ported verbatim from
/// the spike: one allocation per tensor rather than one per channel keeps `WaveNetState`'s scratch
/// buffers few and reusable instead of many-and-tiny.
type Sig = Vec<f32>;

// -------------------------------------------------------------------------------------------
// NFR-SEC-020 dimension ceilings: "Namir shall impose a documented upper bound on the resources
// a single file may cause it to allocate, and shall reject a file that exceeds it with a clear
// message rather than exhausting memory." These numbers are chosen generously above any
// plausible real WaveNet export (the S-1 spike's verified "standard" shape uses channels of 16
// and 8, kernel_size 3, 10 dilations) while still ruling out a hostile file that declares e.g.
// `channels: 4_000_000_000` to force a multi-gigabyte or overflowing allocation attempt.
// -------------------------------------------------------------------------------------------

const MAX_CHANNELS: usize = 8192;
const MAX_HEAD_SIZE: usize = 8192;
const MAX_INPUT_SIZE: usize = 8192;
const MAX_CONDITION_SIZE: usize = 8192;
const MAX_KERNEL_SIZE: usize = 64;
const MAX_DILATIONS_PER_LAYER_ARRAY: usize = 4096;
const MAX_LAYER_ARRAYS: usize = 64;
const MAX_TOTAL_WEIGHTS: usize = 200_000_000;

/// FRS §2's definitions: model sample rate is "typically 48 kHz" — the fallback when a `.nam`
/// file omits `sample_rate` entirely (real exported files sometimes do).
const DEFAULT_SAMPLE_RATE_HZ: u32 = 48_000;

// ---------------------------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------------------------

/// Replaces the spike's stringly-typed activation dispatch (which `panic!`d on an unrecognized
/// name) with a closed enum parsed once, during validated construction. By the time a `Layer`
/// exists, its activation is one of these four variants, so the per-sample `match` in
/// `Layer::apply_into` can never hit an unreachable case — the possibility of an invalid
/// activation string is moved entirely out of the RT path and into `PreparedWaveNet::from_file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Activation {
    Tanh,
    ReLU,
    Sigmoid,
    Identity,
}

impl Activation {
    /// Math identical to the spike's `activation_apply`. `Tanh`/`Sigmoid` route through
    /// [`vectorize_unary`] (R-4 follow-up, M3 §7's close-out pass) — see that function's doc
    /// comment for why these two, not `ReLU`, needed a dedicated fix.
    fn apply(self, x: &mut [f32]) {
        match self {
            Activation::Tanh => vectorize_unary(x, f32x8::tanh, f32::tanh),
            Activation::ReLU => {
                for v in x.iter_mut() {
                    *v = v.max(0.0);
                }
            }
            Activation::Sigmoid => vectorize_unary(
                x,
                |v| f32x8::ONE / (f32x8::ONE + (-v).exp()),
                |v| 1.0 / (1.0 + (-v).exp()),
            ),
            Activation::Identity => {}
        }
    }
}

/// Applies `vec_fn` to `x` 8 lanes at a time, `scalar_fn` to the `n % 8` leftover — the same
/// chunk-then-scalar-remainder shape [`axpy`] below uses, applied to a per-element nonlinearity
/// instead of a multiply-accumulate.
///
/// **Why this exists, on top of `axpy`'s existing vectorization:** this M3 close-out pass's own
/// reference-machine benchmarking (`benches/wavenet_inner_loops.rs`, certified run on
/// `docs/02-architecture.md` §2's pinned AMD Ryzen 9 5950X) measured the standard WaveNet shape's
/// assembled cost at p99.9 ≈ 33-34% of one core — still well over the 25% budget even with
/// `axpy` already vectorized. A targeted read of every non-`axpy` per-sample operation in this
/// file found exactly one hot, unvectorized loop: `Activation::Tanh`/`Sigmoid` in the (default,
/// `gated: false`) `Layer::apply_into` path, called once per layer over `channels * n` elements —
/// 15,360 scalar `f32::tanh()` calls per 64-sample block for the standard shape's two ten-layer
/// arrays (16 and 8 channels respectively), the only transcendental-heavy loop in the file and
/// the dominant remaining cost `axpy`'s multiply-accumulate vectorization never touched.
///
/// **Why this is safe against the -100 dB numeric-parity test**
/// (`tests/fixtures.rs::numeric_parity_against_an_independent_reference_implementation`):
/// `wide::f32x8::tanh`/`exp` are not a per-lane scalar fallback — `wide`'s own source
/// (`f32x8_.rs`) documents a range-reduced polynomial/`exp_m1`-based implementation with error
/// bounded below 1 ULP of `f32` (~1.2e-7 relative) across the whole domain, several orders of
/// magnitude inside the -100 dB (1e-5 relative) parity budget, and `Sigmoid`'s `f32x8::exp` plus
/// exact SIMD `Div` (not an approximate `recip`) carries the same guarantee.
#[inline]
fn vectorize_unary(x: &mut [f32], vec_fn: impl Fn(f32x8) -> f32x8, scalar_fn: impl Fn(f32) -> f32) {
    let n = x.len();
    let lanes = n - n % 8;
    let (vec_part, rem) = x.split_at_mut(lanes);
    for chunk in vec_part.chunks_exact_mut(8) {
        let v = vec_fn(f32x8::from(&*chunk));
        chunk.copy_from_slice(&v.to_array());
    }
    for v in rem.iter_mut() {
        *v = scalar_fn(*v);
    }
}

impl TryFrom<&str> for Activation {
    type Error = NamLoadError;

    fn try_from(name: &str) -> Result<Self, Self::Error> {
        match name {
            "Tanh" => Ok(Activation::Tanh),
            "ReLU" => Ok(Activation::ReLU),
            "Sigmoid" => Ok(Activation::Sigmoid),
            // The spike treated an empty string the same as "Identity"; preserved here.
            "Identity" | "" => Ok(Activation::Identity),
            other => Err(NamLoadError {
                code: error_codes::UNSUPPORTED_ACTIVATION,
                detail: format!("unsupported activation: {other:?}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// R-4 (M3 roadmap §7): vectorized AXPY, shared by all three of this file's hot inner loops.
//
// **Decision:** use the `wide` crate's `f32x8` for 8-lane float SIMD, not hand-written
// `core::arch` intrinsics.
//
// **Rationale:** S-1's spike measured the scalar version of this file at 41% of one core
// (p99.9) against NFR-PERF-010's 25% budget, and attributed the gap to "an absence of
// vectorization, not a structural cost" — i.e. exactly the kind of gap `wide` closes without
// otherwise changing the algorithm. `wide`'s public API (`f32x8::from`/`to_array`, `+`, `*`) is
// entirely safe, so it fits inside this crate's `forbid(unsafe_code)` (workspace lint, D-5.3)
// with no exception needed. It also compiles the same source for both x86_64 (SSE2/AVX, chosen
// by `target_feature` at compile time) and aarch64/NEON, which matters for NFR-PORT-030's mobile
// targets — hand-rolled intrinsics would need a second, separately-verified NEON path to match
// that reach, doubling the surface this file's -100 dB numeric-parity test has to hold for.
//
// **Consequence:** dispatch is a compile-time target-feature choice baked into the binary, not
// a runtime CPU-feature probe with a scalar fallback for older CPUs — accepted for this
// milestone since the reference and CI machines are all baseline-recent x86_64/aarch64; revisit
// if Namir ever needs to target hardware whose baseline excludes SSE2/NEON.
//
// **Alternatives rejected:** raw `core::arch` intrinsics (needs `unsafe`, forbidden here by
// D-5.3, plus a hand-written NEON port); `std::simd` portable-SIMD (nightly-only, unavailable on
// this workspace's stable `rust-version`, see the root `Cargo.toml`).
//
// **Measured** via `benches/wavenet_inner_loops.rs` (standard WaveNet shape, 64-sample blocks,
// 100,000 measured blocks after a 5,000-block warmup, `cargo bench -p namir-nam`).
//
// Earlier revisions of this note recorded a long, inconclusive scalar-vs-vector comparison run on
// a 4-core Intel Xeon sandbox, and concluded that whether `axpy`'s vectorization helped at all was
// **not verified** — the measured gap sat inside the run-to-run spread. That whole analysis has
// been superseded, and the reason it was inconclusive is now known: it was measuring two
// confounds rather than the code.
//
// 1. **No AVX was enabled.** The workspace had no `target-cpu` set anywhere, so `wide`'s `pick!`
//    macro selected its non-AVX path — two 4-lane SSE2 ops per `f32x8` operation rather than one
//    genuinely 8-wide AVX op — even on hardware that supports AVX2. That is now fixed workspace-
//    wide by **D-2.3** (`.cargo/config.toml`, `target-cpu=x86-64-v3`), and the effect is large and
//    unambiguous. Measured on `docs/02-architecture.md` §2's actual reference machine:
//
//      SSE2 baseline:  p50 8.77%   p99.9 30.3%  of the block period
//      AVX2 + FMA:     p50 ~6.5%   p99.9 ~10.5%
//
//    Numeric parity under FMA re-verified at -130.8 dB against the -100 dB bar (contracting a
//    multiply-add into one rounding step is a *smaller* error than two, not a larger one).
//
// 2. **The benchmark was pinned to the wrong core.** Every benchmark in this workspace pinned to
//    logical CPU 0, which on this machine absorbs `dxgkrnl.sys`'s GPU interrupts — 128-512 µs
//    each, ~165 per second, and zero on all 31 other cores (established by an elevated
//    `xperf -on Latency` trace). ISRs run at DIRQL, above every thread priority, so a large part
//    of every `p99.9` figure in the superseded analysis above was the GPU driver rather than this
//    file. See `pin_to_measurement_core` in any of this workspace's benchmarks.
//
// **Still true, and worth keeping:** this benchmark's `p50` is stable and trustworthy; its raw
// `p99.9` is not reproducible run-to-run on a general-purpose desktop even after both fixes
// (measured varying 17%-52% across ten identical runs of the chain benchmark, with `p50` pinned).
// For a figure immune to that, `namir-engine/benches/tail_structure.rs` reports a per-residue
// minimum estimate of the schedule's own worst-case block, which returns the same value on a busy
// machine as on a quiet one. The rationale for vectorizing at all (closing "an absence of
// vectorization" S-1 identified) never depended on these numbers, and is now also directly
// supported by the AVX2 measurement above.
// ---------------------------------------------------------------------------------------------

/// `out[t] += w * in_[t]` for every `t`, the one AXPY shape `Conv1x1::apply_into`,
/// `Conv1x1::apply_add_into` and `Conv1D::apply_into`'s tap loop all reduce to. Vectorized 8
/// lanes at a time; the `n % 8` leftover runs as a plain scalar loop, so callers may pass any
/// length including 0 and lengths under 8 (the self-consistency test exercises block size 1) —
/// correctness does not depend on `n` being a multiple of the lane width.
#[inline]
fn axpy(out: &mut [f32], in_: &[f32], w: f32) {
    debug_assert_eq!(out.len(), in_.len());
    let n = out.len();
    let lanes = n - n % 8;
    let (out_vec_part, out_rem) = out.split_at_mut(lanes);
    let (in_vec_part, in_rem) = in_.split_at(lanes);

    let w_vec = f32x8::splat(w);
    for (o, i) in out_vec_part
        .chunks_exact_mut(8)
        .zip(in_vec_part.chunks_exact(8))
    {
        let sum = f32x8::from(&*o) + w_vec * f32x8::from(i);
        o.copy_from_slice(&sum.to_array());
    }
    for (o, &i) in out_rem.iter_mut().zip(in_rem.iter()) {
        *o += w * i;
    }
}

// ---------------------------------------------------------------------------------------------
// Primitive layers (ported verbatim from the spike except for the `Result`-returning `read`s)
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
    /// `out_ch` and `in_ch` are always dimensions already checked against this crate's ceilings
    /// (see `validate_layer_array_dims`) by the time this is called, so `out_ch * in_ch` cannot
    /// overflow `usize` on any 64-bit target.
    fn read(
        r: &mut WeightReader,
        out_ch: usize,
        in_ch: usize,
        has_bias: bool,
    ) -> Result<Self, NamLoadError> {
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
                axpy(out_row, in_row, w);
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
                axpy(out_row, in_row, w);
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
    /// `out_ch`, `in_ch` and `kernel_size` are always dimensions already checked against this
    /// crate's ceilings by the time this is called, so `out_ch * in_ch * kernel_size` cannot
    /// overflow `usize` on any 64-bit target.
    fn read(
        r: &mut WeightReader,
        out_ch: usize,
        in_ch: usize,
        kernel_size: usize,
        dilation: usize,
    ) -> Result<Self, NamLoadError> {
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
                    axpy(out_row, &p[offset..offset + n], w);
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

// ---------------------------------------------------------------------------------------------
// Layer / LayerArray
// ---------------------------------------------------------------------------------------------

struct Layer {
    dilated: Conv1D,
    mixin: Conv1x1,
    residual: Conv1x1,
    activation: Activation,
    gated: bool,
    channels: usize,
}

/// Per-layer reusable scratch, sized once for a chosen max block size and reused across every
/// `process_block` call — no allocation on the hot path. `z_buf` always holds a materialized
/// copy of `z` even in the (common) ungated case, trading one cheap memcpy for keeping every
/// buffer a distinct, unambiguously-disjoint struct field.
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
    /// is `[condition_size * n]`, which is `[1 * n]` since `condition_size == 1` is enforced at
    /// load time).
    #[allow(clippy::too_many_arguments)]
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
            self.activation.apply(top);
            Activation::Sigmoid.apply(bottom);
            for i in 0..z_len {
                scratch.z_buf[i] = top[i] * bottom[i];
            }
        } else {
            self.activation.apply(&mut scratch.conv_buf[..z_len]);
            scratch.z_buf[..z_len].copy_from_slice(&scratch.conv_buf[..z_len]);
        }

        // `axpy(w=1.0)`, not a plain `+=` loop (this M3 close-out pass's own vectorization fix,
        // same rationale as `Activation::apply`'s `Tanh`/`Sigmoid` above): a straight `out[i] +=
        // in[i]` loop over disjoint `&mut`/`&` slices is exactly `axpy`'s shape with `w` fixed at
        // 1.0, so it costs nothing to route through the same vectorized primitive rather than
        // trust the optimizer to notice on its own.
        axpy(&mut head_sum[..z_len], &scratch.z_buf[..z_len], 1.0);

        self.residual
            .apply_into(&scratch.z_buf[..z_len], n, &mut next_input_out[..z_len]);
        axpy(&mut next_input_out[..z_len], &layer_input[..z_len], 1.0);
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

/// Immutable, `Sync` weights and configuration (D-9.1 / D-8.2). Shareable across instances — the
/// spike declared `unsafe impl Sync for PreparedWaveNet {}`, but every field here (`Vec<f32>`,
/// `bool`, `usize`, the `Copy` `Activation` enum) is already auto-`Sync` on its own, and this
/// crate forbids unsafe code anyway (workspace lint, D-5.3) — so no such impl exists here at all;
/// `Sync` is simply derived.
pub struct PreparedWaveNet {
    arrays: Vec<LayerArray>,
    head_scale: f32,
    sample_rate: SampleRate,
    metadata: NamMetadata,
}

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

// ---------------------------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------------------------

/// Validates one layer array's declared dimensions against this crate's NFR-SEC-020 ceilings,
/// its lower bounds, and the `condition_size == 1` constraint (this implementation always feeds
/// the raw mono input as the sole conditioning signal, matching every real WaveNet export — a
/// different declared `condition_size` isn't representable by this code and must be rejected
/// cleanly, not silently misread). Called *before* any weight reading or scratch sizing for this
/// array, so every later multiplication involving these fields is safe from `usize` overflow by
/// construction.
fn validate_layer_array_dims(cfg: &LayerArrayConfig, index: usize) -> Result<(), NamLoadError> {
    check_max(
        cfg.input_size,
        MAX_INPUT_SIZE,
        &format!("layer array {index}: input_size"),
    )?;
    check_max(
        cfg.condition_size,
        MAX_CONDITION_SIZE,
        &format!("layer array {index}: condition_size"),
    )?;
    check_max(
        cfg.head_size,
        MAX_HEAD_SIZE,
        &format!("layer array {index}: head_size"),
    )?;
    check_max(
        cfg.channels,
        MAX_CHANNELS,
        &format!("layer array {index}: channels"),
    )?;
    check_max(
        cfg.kernel_size,
        MAX_KERNEL_SIZE,
        &format!("layer array {index}: kernel_size"),
    )?;
    check_max(
        cfg.dilations.len(),
        MAX_DILATIONS_PER_LAYER_ARRAY,
        &format!("layer array {index}: dilations.len()"),
    )?;

    check_min1(cfg.input_size, &format!("layer array {index}: input_size"))?;
    check_min1(cfg.channels, &format!("layer array {index}: channels"))?;
    check_min1(
        cfg.kernel_size,
        &format!("layer array {index}: kernel_size"),
    )?;
    check_min1(cfg.head_size, &format!("layer array {index}: head_size"))?;

    if cfg.condition_size != 1 {
        return Err(NamLoadError {
            code: error_codes::UNSUPPORTED_CONDITION_SIZE,
            detail: format!(
                "layer array {index}: condition_size must be 1, found {}",
                cfg.condition_size
            ),
        });
    }
    Ok(())
}

impl PreparedWaveNet {
    /// The semantic half of P6's "one hardened place `.nam` bytes go through" (the other half is
    /// `NamFile::parse`'s JSON-shape parsing). Validation order:
    ///
    /// 1. `architecture == "WaveNet"` (LSTM and anything else: `UNSUPPORTED_ARCHITECTURE` — LSTM
    ///    support is a documented open scope gap, see the crate doc comment, not a bug).
    /// 2. `config.head.is_none()` (`UNSUPPORTED_HEAD_CONFIG`).
    /// 3. `sample_rate` is nonzero if present (`INVALID_SAMPLE_RATE`), else defaults to 48 kHz.
    /// 4. `config.layers` is non-empty (`EMPTY_LAYER_ARRAYS`).
    /// 5. `config.layers.len()` and `weights.len()` are within their NFR-SEC-020 ceilings
    ///    (`DIMENSION_LIMIT_EXCEEDED`).
    /// 6. Every layer array's dimensions are within their ceilings, at least 1, and
    ///    `condition_size == 1` (`DIMENSION_LIMIT_EXCEEDED` / `UNSUPPORTED_CONDITION_SIZE`).
    /// 7. Every layer array's `activation` string parses (`UNSUPPORTED_ACTIVATION`).
    ///
    /// Steps 5-7 all happen *before* step 8 reads a single weight or performs a single
    /// dimension-derived multiplication. This ordering is load-bearing, not decorative: once
    /// every dimension that ever appears in a product (`channels * input_size`,
    /// `channels * channels * kernel_size`, and so on) is bounded well below any value that could
    /// overflow `usize` on a 64-bit target, every such product later in this function and in
    /// `Conv1x1::read`/`Conv1D::read`/`WeightReader::take` is safe from overflow by construction.
    /// Do not "helpfully" add `checked_mul` throughout the rest of this file to reproduce a
    /// guarantee this section already provides, and do not remove this section without replacing
    /// that guarantee some other way.
    ///
    /// 8. Each `LayerArray` is built via `WeightReader`, in the order the spike's own reading of
    ///    `NeuralAmpModelerCore` established: per array `[rechannel (no bias), per-layer[dilated
    ///    (bias), mixin (no bias), residual (bias)], head_rechannel (bias iff head_bias)]`
    ///    (`WEIGHT_COUNT_MISMATCH` on exhaustion).
    /// 9. Adjacent arrays chain correctly: `head_size[i] == channels[i+1]` and
    ///    `channels[i] == input_size[i+1]` (`LAYER_ARRAY_CHAINING_MISMATCH`).
    /// 10. The trailing `head_scale` float is resolved exactly as the spike's confirmed reading
    ///     of `WaveNet::set_weights_`: if one float remains after step 8, it is authoritative; if
    ///     none remain, `config.head_scale` is used; anything else is `WEIGHT_COUNT_MISMATCH`.
    pub fn from_file(nam: &NamFile) -> Result<Self, NamLoadError> {
        if nam.architecture != "WaveNet" {
            return Err(NamLoadError {
                code: error_codes::UNSUPPORTED_ARCHITECTURE,
                detail: format!("architecture: {:?}", nam.architecture),
            });
        }
        if nam.config.head.is_some() {
            return Err(NamLoadError {
                code: error_codes::UNSUPPORTED_HEAD_CONFIG,
                detail: "config.head is non-null".to_string(),
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

        if nam.config.layers.is_empty() {
            return Err(NamLoadError {
                code: error_codes::EMPTY_LAYER_ARRAYS,
                detail: "config.layers is empty".to_string(),
            });
        }

        check_max(
            nam.config.layers.len(),
            MAX_LAYER_ARRAYS,
            "config.layers.len()",
        )?;
        check_max(nam.weights.len(), MAX_TOTAL_WEIGHTS, "weights.len()")?;
        for (i, cfg) in nam.config.layers.iter().enumerate() {
            validate_layer_array_dims(cfg, i)?;
        }

        let activations: Vec<Activation> = nam
            .config
            .layers
            .iter()
            .map(|cfg| Activation::try_from(cfg.activation.as_str()))
            .collect::<Result<_, _>>()?;

        let mut r = WeightReader::new(&nam.weights);
        let mut arrays = Vec::with_capacity(nam.config.layers.len());
        for (cfg, activation) in nam.config.layers.iter().zip(activations) {
            let out_mult = if cfg.gated { 2 } else { 1 };
            // No bias: NeuralAmpModelerCore's LayerArray ctor constructs `_rechannel` with
            // bias=false, confirmed by reading that constructor directly (see spike README).
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
                    activation,
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

        // Adjacent arrays chain via TWO separate signals (see the module doc comment): the
        // residual trunk (dim = channels) feeds the next array's rechannel input, while the
        // head-rechannel output (dim = head_size) separately seeds the next array's head
        // accumulator.
        for (i, w) in arrays.windows(2).enumerate() {
            if w[0].head_size != w[1].channels {
                return Err(NamLoadError {
                    code: error_codes::LAYER_ARRAY_CHAINING_MISMATCH,
                    detail: format!(
                        "layer array {i} head_size ({}) does not match layer array {} channels ({})",
                        w[0].head_size,
                        i + 1,
                        w[1].channels
                    ),
                });
            }
            if w[0].channels != w[1].input_size {
                return Err(NamLoadError {
                    code: error_codes::LAYER_ARRAY_CHAINING_MISMATCH,
                    detail: format!(
                        "layer array {i} channels ({}) does not match layer array {} input_size ({})",
                        w[0].channels,
                        i + 1,
                        w[1].input_size
                    ),
                });
            }
        }

        // The trailing float in the weights array is the authoritative head_scale (it's what
        // NeuralAmpModelerCore's WaveNet::set_weights_ actually uses; config.head_scale is
        // parsed but unconditionally overwritten by this trailing weight in the reference
        // implementation, though a correctly-exported file has them equal). By this point every
        // successful `WeightReader::take` above guarantees `r.pos <= nam.weights.len()` and, since
        // every layer array requires at least one weight, `r.pos >= 1`, so the subtraction below
        // cannot underflow.
        let head_scale = if r.pos == nam.weights.len() - 1 {
            nam.weights[nam.weights.len() - 1]
        } else if r.pos == nam.weights.len() {
            nam.config.head_scale
        } else {
            return Err(NamLoadError {
                code: error_codes::WEIGHT_COUNT_MISMATCH,
                detail: format!(
                    "consumed {} of {} weights (expected {} or {})",
                    r.pos,
                    nam.weights.len(),
                    r.pos,
                    r.pos + 1
                ),
            });
        };

        Ok(Self {
            arrays,
            head_scale,
            sample_rate,
            metadata: nam.metadata.clone(),
        })
    }

    /// FR-NAM-080: model metadata (name, `modeled_by`, gear/tone type, description).
    pub fn metadata(&self) -> &NamMetadata {
        &self.metadata
    }

    /// The model's declared sample rate (or the 48 kHz default if the file omitted it).
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// FR-NAM-110: "Namir shall report the model stage's processing latency in samples, and
    /// shall report zero if the architecture is causal and introduces none." This WaveNet's
    /// dilated convolutions are causal (left-padding only — see `Conv1D::apply_into`) and it
    /// produces exactly one output sample per input sample (block-preserving), so it introduces
    /// no added delay.
    pub fn latency_samples(&self) -> u32 {
        0
    }

    /// `max_block_size` is the largest block size this state will ever be asked to process;
    /// every scratch buffer is sized once, here, and reused for the state's whole lifetime.
    pub fn new_state(&self, max_block_size: usize) -> WaveNetState {
        WaveNetState::new(self, max_block_size)
    }

    /// The allocation-free RT-path entry point. Writes `input.len()` frames of output into
    /// `out` (`out.len() == head_size(last array) * input.len()`). Every intermediate buffer
    /// lives in `state` and is reused block to block; this function itself allocates nothing.
    ///
    /// Panics if `input.len()` exceeds `state`'s configured max block size — that is a call-site
    /// programming error (the caller is responsible for sizing blocks to at most the maximum
    /// declared at `new_state` time), not something that can happen from untrusted `.nam` file
    /// content, so a panic here is acceptable per the same reasoning
    /// `namir-engine/src/stage_io.rs`'s `StageIo::new` doc comment gives for its own analogous
    /// panic.
    pub fn process_block(&self, state: &mut WaveNetState, input: &[f32], out: &mut [f32]) {
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

    /// Convenience wrapper over `process_block` that allocates its own output buffer.
    /// **Not RT-safe** — for tests, tools, and other non-audio-thread callers only.
    pub fn process(&self, state: &mut WaveNetState, input: &[f32]) -> Vec<f32> {
        let out_len = self
            .arrays
            .last()
            .expect("at least one layer array")
            .head_size
            * input.len();
        let mut out = vec![0f32; out_len];
        self.process_block(state, input, &mut out);
        out
    }
}

#[cfg(test)]
pub(crate) mod rt_harness {
    //! Copied from `namir-engine/src/rt_harness.rs` (self-contained; see that file's module doc
    //! for the full rationale for using `assert_no_alloc` rather than a hand-rolled
    //! `GlobalAlloc` under this workspace's `unsafe_code = "forbid"` lint). `namir-ir`'s
    //! `convolver.rs` carries its own separate copy of the same pattern, which is fine there
    //! since it's a different crate with its own independent test binary.
    //!
    //! `pub(crate)`, not private: `#[global_allocator]` is a whole-*binary* constraint — a single
    //! test binary can register at most one. `namir-nam`'s `--lib` test binary compiles every
    //! module in this crate together, including `lstm.rs`, so `lstm.rs`'s own RT-allocation test
    //! reuses *this* copy rather than declaring a second one (which would fail to compile at all,
    //! "cannot define multiple global allocators"). That's the whole reason this crate has only
    //! one `rt_harness` module instead of one per architecture file, unlike the WaveNet/LSTM
    //! split everywhere else in this crate.

    use assert_no_alloc::AllocDisabler;

    #[global_allocator]
    static ALLOC: AllocDisabler = AllocDisabler;

    /// Runs `f` inside the "audio section" marker.
    pub fn audio_section<T>(f: impl FnOnce() -> T) -> T {
        assert_no_alloc::reset_violation_count();
        let result = assert_no_alloc::assert_no_alloc(f);
        assert_eq!(
            assert_no_alloc::violation_count(),
            0,
            "allocation occurred inside an audio section"
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{NamMetadata, WaveNetConfig};

    // -----------------------------------------------------------------------------------------
    // Ported near-verbatim from spikes/s1-nam-inference/src/lib.rs
    // -----------------------------------------------------------------------------------------

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
        // block2, and verify against the equivalent single continuous 8-sample run.
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

    // -----------------------------------------------------------------------------------------
    // New validation coverage — the spike had none of this, since it only ever saw its own
    // trusted generator's output.
    // -----------------------------------------------------------------------------------------

    fn minimal_layer_array() -> LayerArrayConfig {
        LayerArrayConfig {
            input_size: 1,
            condition_size: 1,
            head_size: 1,
            channels: 2,
            kernel_size: 2,
            dilations: vec![1],
            activation: "Tanh".to_string(),
            gated: false,
            head_bias: false,
        }
    }

    /// Computes exactly how many weights `PreparedWaveNet::from_file` will consume for `cfg`, so
    /// tests can hand-build a matching (or deliberately mismatched) flat weight array without
    /// going through JSON at all.
    fn weight_count_for(cfg: &LayerArrayConfig) -> usize {
        let out_mult = if cfg.gated { 2 } else { 1 };
        let mut n = cfg.channels * cfg.input_size; // rechannel, no bias
        for _ in &cfg.dilations {
            n += cfg.channels * out_mult * cfg.channels * cfg.kernel_size; // dilated weight
            n += cfg.channels * out_mult; // dilated bias
            n += cfg.channels * out_mult * cfg.condition_size; // mixin, no bias
            n += cfg.channels * cfg.channels; // residual weight
            n += cfg.channels; // residual bias
        }
        n += cfg.head_size * cfg.channels; // head_rechannel weight
        if cfg.head_bias {
            n += cfg.head_size;
        }
        n
    }

    /// `PreparedWaveNet` deliberately has no `Debug` impl (nothing in this crate's public API needs
    /// one), so `Result::unwrap_err` — which requires `T: Debug` — can't be used directly on
    /// `PreparedWaveNet::from_file`'s `Result`. This is the small workaround.
    fn expect_err(result: Result<PreparedWaveNet, NamLoadError>) -> NamLoadError {
        match result {
            Ok(_) => panic!("expected PreparedWaveNet::from_file to reject this file"),
            Err(e) => e,
        }
    }

    fn minimal_valid_file() -> NamFile {
        let cfg = minimal_layer_array();
        let n = weight_count_for(&cfg);
        let mut weights = vec![0.01f32; n];
        weights.push(0.5); // trailing head_scale
        NamFile {
            version: None,
            architecture: "WaveNet".to_string(),
            config: WaveNetConfig {
                layers: vec![cfg],
                head_scale: 0.5,
                head: None,
            },
            weights,
            sample_rate: Some(48_000),
            metadata: NamMetadata::default(),
        }
    }

    #[test]
    fn minimal_valid_file_loads_successfully() {
        let file = minimal_valid_file();
        let prepared = PreparedWaveNet::from_file(&file).expect("minimal valid file should load");
        assert_eq!(prepared.sample_rate().hz(), 48_000);
    }

    #[test]
    fn missing_sample_rate_defaults_to_48khz() {
        let mut file = minimal_valid_file();
        file.sample_rate = None;
        let prepared = PreparedWaveNet::from_file(&file).unwrap();
        assert_eq!(prepared.sample_rate().hz(), 48_000);
    }

    #[test]
    fn rejects_wrong_architecture() {
        let mut file = minimal_valid_file();
        file.architecture = "LSTM".to_string();
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_ARCHITECTURE.id);
    }

    #[test]
    fn rejects_non_null_head_config() {
        let mut file = minimal_valid_file();
        file.config.head = Some(serde_json::json!({"whatever": 1}));
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_HEAD_CONFIG.id);
    }

    #[test]
    fn rejects_empty_layer_arrays() {
        let mut file = minimal_valid_file();
        file.config.layers.clear();
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::EMPTY_LAYER_ARRAYS.id);
    }

    #[test]
    fn rejects_unsupported_activation() {
        let mut file = minimal_valid_file();
        file.config.layers[0].activation = "GELU".to_string();
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_ACTIVATION.id);
    }

    #[test]
    fn rejects_condition_size_other_than_one() {
        let mut file = minimal_valid_file();
        file.config.layers[0].condition_size = 2;
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_CONDITION_SIZE.id);
    }

    #[test]
    fn rejects_dimension_over_ceiling_without_attempting_a_huge_allocation() {
        let mut file = minimal_valid_file();
        // If this were not checked before use, `channels * channels * kernel_size` alone would
        // ask for on the order of 10^18 floats. The test completing at all (rather than hanging
        // or OOMing) demonstrates the ceiling check runs first.
        file.config.layers[0].channels = 999_999_999;
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::DIMENSION_LIMIT_EXCEEDED.id);
    }

    // trace: FR-NAM-040
    #[test]
    fn rejects_wrong_weight_count() {
        // Popping only the trailing `head_scale` float would leave `r.pos == weights.len()`,
        // which is the *valid* "no trailing float, use config.head_scale" case, not a mismatch.
        // Popping one more (an actual weight the layer array needs) forces genuine exhaustion.
        let mut file = minimal_valid_file();
        file.weights.pop();
        file.weights.pop();
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::WEIGHT_COUNT_MISMATCH.id);
    }

    #[test]
    fn rejects_mismatched_layer_array_chaining() {
        let cfg0 = minimal_layer_array(); // head_size = 1, channels = 2
        let mut cfg1 = minimal_layer_array();
        cfg1.input_size = cfg0.channels; // correct: chains on the trunk signal
        cfg1.channels = 3; // wrong: cfg0.head_size (1) != cfg1.channels (3)

        let mut weights = vec![0.01f32; weight_count_for(&cfg0) + weight_count_for(&cfg1)];
        weights.push(0.5);
        let file = NamFile {
            version: None,
            architecture: "WaveNet".to_string(),
            config: WaveNetConfig {
                layers: vec![cfg0, cfg1],
                head_scale: 0.5,
                head: None,
            },
            weights,
            sample_rate: Some(48_000),
            metadata: NamMetadata::default(),
        };
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::LAYER_ARRAY_CHAINING_MISMATCH.id);
    }

    #[test]
    fn rejects_zero_sample_rate() {
        let mut file = minimal_valid_file();
        file.sample_rate = Some(0);
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::INVALID_SAMPLE_RATE.id);
    }

    // trace: FR-NAM-110
    #[test]
    fn latency_samples_is_zero() {
        let prepared = PreparedWaveNet::from_file(&minimal_valid_file()).unwrap();
        assert_eq!(prepared.latency_samples(), 0);
    }

    #[test]
    fn parse_then_from_file_combines_parse_and_validate() {
        // Built directly as JSON (rather than round-tripping a `NamFile` value, which is
        // `Deserialize`-only and has no `Serialize` impl to spare) to exercise the full
        // bytes-to-`PreparedWaveNet` path, mirroring `minimal_layer_array`'s shape. The top-level
        // `crate::load` free function (now in `model.rs`, since it also has to dispatch to
        // `lstm.rs` for LSTM files) has its own equivalent coverage there; this test is scoped to
        // just this module's own `NamFile::parse` + `PreparedWaveNet::from_file` pair.
        let cfg = minimal_layer_array();
        let n = weight_count_for(&cfg);
        let mut weights = vec![0.01f32; n];
        weights.push(0.5);
        let json = serde_json::json!({
            "architecture": "WaveNet",
            "config": {
                "layers": [{
                    "input_size": cfg.input_size,
                    "condition_size": cfg.condition_size,
                    "head_size": cfg.head_size,
                    "channels": cfg.channels,
                    "kernel_size": cfg.kernel_size,
                    "dilations": cfg.dilations,
                    "activation": cfg.activation,
                    "gated": cfg.gated,
                    "head_bias": cfg.head_bias,
                }],
                "head_scale": 0.5,
            },
            "weights": weights,
            "sample_rate": 48_000,
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let file = NamFile::parse(&bytes).expect("round trip through JSON should parse");
        let prepared =
            PreparedWaveNet::from_file(&file).expect("round trip through JSON should load");
        assert_eq!(prepared.latency_samples(), 0);
    }

    #[test]
    fn process_block_does_not_allocate() {
        let file = minimal_valid_file();
        let prepared = PreparedWaveNet::from_file(&file).unwrap();
        let mut state = prepared.new_state(64);
        let input = vec![0.1f32; 64];
        let mut output = vec![0.0f32; 64]; // head_size == 1
        rt_harness::audio_section(|| {
            prepared.process_block(&mut state, &input, &mut output);
        });
    }
}
