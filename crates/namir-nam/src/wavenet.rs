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
//!
//! # NAM Architecture 2 (M10, D-9.12)
//!
//! A2 files declare `architecture: "WaveNet"` exactly as A1 files do — D-9.12 extends this one
//! parser/inference path rather than adding a second. Core A2 (enough to load and run the
//! "A2 standard"/"A2 nano" configurations, FR-NAM-150) turned out to need three real structural
//! additions on top of A1's shape, all provably inert when the file is A1 (`bottleneck` absent,
//! `layer1x1` absent, a single per-array activation): a `bottleneck` width distinct from the
//! residual trunk's `channels` (threaded through `Layer`/`LayerArray`/the weight walk); a
//! per-layer, parameterized [`Activation`] (`LeakyReLU`/`SiLU`/`Hardswish`/`Softsign`/
//! `LeakyHardtanh`/`PReLU`, replacing A1's four zero-payload variants); and a real k-tap, dilated
//! `head_rechannel` (unified onto `Conv1D`, with A1's `head_size`/`head_bias` legacy shape now the
//! `kernel_size = 1` degenerate case). `reject_unsupported_layer_features` enforces D-9.12's
//! permanent scope boundary: `condition_dsp`, FiLM (all eight sites), gating, non-unity
//! `groups_*`, `slimmable`, an active `head1x1`, and an inactive/grouped `layer1x1` are rejected
//! by name (`UNSUPPORTED_CONFIGURATION`, FR-NAM-140), not silently ignored or misparsed as
//! malformed. Cross-checked against a real reference render (`NeuralAmpModelerCore`, pinned
//! `3cde95c`, built with `-DNAM_USE_INLINE_GEMM -DNAM_ENABLE_A2_FAST=OFF`) and against
//! `namir-fixtures`'s independently-derived A2 oracle (`tests/a2_fixtures.rs`) — see R-9
//! (`docs/02-architecture.md` §22) for why that independence, not just a passing test, is the
//! actual risk mitigation this milestone's process was built around.

use namir_core::SampleRate;
use wide::f32x8;

use crate::error_codes::{self, NamLoadError};
use crate::file::{self, LayerArrayConfig, NamFile, NamMetadata};
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

/// M10 addition, closing a gap found while scoping A2 support (present in A1 all along): a single
/// dilation value never appears in any weight count, so nothing above bounded it before this —
/// `{channels: 1, kernel_size: 2, dilations: [4_000_000_000]}` needs only ~8 weights, passes every
/// other check, and then `WaveNetState::new` would attempt to allocate on the order of 16 GB for
/// that one layer's causal-conv history. The real S-1-verified "standard" shape's largest dilation
/// is 512 (10 layers, `1 << 9`); 8192 is generous headroom above any plausible export while still
/// ruling out a hostile one.
const MAX_DILATION: usize = 8_192;

/// M10 addition, alongside `MAX_DILATION`: bounds the padded causal-conv history buffer a layer's
/// `Conv1D` allocates, `channels * (kernel_size - 1) * dilation` elements. This is not a redundant
/// guard duplicating the ordering guarantee `PreparedWaveNet::from_file`'s doc comment describes —
/// `channels`, `kernel_size` and `dilation` are each individually bounded by the checks above and
/// by `MAX_DILATION`, but their *product* is what the history buffer actually allocates, and no
/// other single ceiling bounds that product. `saturating_mul` is used deliberately: the ceilings on
/// the three factors are generous enough that legitimate values never approach `usize::MAX`, so
/// saturation only ever affects a value this check was going to reject anyway.
const MAX_CONV_HISTORY_ELEMENTS: usize = 16_777_216;

/// FRS §2's definitions: model sample rate is "typically 48 kHz" — the fallback when a `.nam`
/// file omits `sample_rate` entirely (real exported files sometimes do).
const DEFAULT_SAMPLE_RATE_HZ: u32 = 48_000;

/// `LeakyReLU`/`PReLU`'s default slope when a `.nam` file's activation entry (bare name, or an
/// object omitting its own `negative_slope`) doesn't state one — matching
/// `NeuralAmpModelerCore`'s own hard-coded default (`activations.cpp`'s singleton constructions
/// and `ActivationConfig::from_json`'s `j.value("negative_slope", 0.01f)`).
const DEFAULT_LEAKY_SLOPE: f32 = 0.01;

// ---------------------------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------------------------

/// Replaces the spike's stringly-typed activation dispatch (which `panic!`d on an unrecognized
/// name) with a closed enum parsed once, during validated construction. By the time a `Layer`
/// exists, its activation is one of these variants, so the per-sample `match` in
/// `Layer::apply_into` can never hit an unreachable case — the possibility of an invalid
/// activation string is moved entirely out of the RT path and into `PreparedWaveNet::from_file`.
///
/// M10 (A2, Step A2): grew from four zero-payload variants (A1's whole vocabulary) to ten, six of
/// which carry parameters read from the `.nam` file's `activation` entry — never from the weight
/// stream, see `activations.h`/`.cpp`, read directly rather than guessed. No longer `Copy`
/// (`PReLU`'s per-channel slopes are a `Vec<f32>`), so each `Layer` now clones its own resolved
/// `Activation` out of a per-array-or-per-layer list rather than copying one shared value — A1
/// files still resolve every layer in an array to the same cloned value (one list entry,
/// `Vec::clone`d `dilations.len()` times), so this is provably inert for A1: same enum value, same
/// math, just no longer literally the same bits via `Copy`.
#[derive(Debug, Clone, PartialEq)]
enum Activation {
    Tanh,
    ReLU,
    Sigmoid,
    Identity,
    /// `x > 0 ? x : negative_slope * x`.
    LeakyReLU {
        negative_slope: f32,
    },
    /// `x * sigmoid(x)` (aka Swish).
    SiLU,
    /// `x * clamp(x + 3, 0, 6) / 6`.
    Hardswish,
    /// `x / (1 + |x|)`.
    Softsign,
    /// Piecewise-linear clamp: identity inside `[min_val, max_val]`, a shallow line of slope
    /// `min_slope`/`max_slope` outside it.
    LeakyHardtanh {
        min_val: f32,
        max_val: f32,
        min_slope: f32,
        max_slope: f32,
    },
    /// `LeakyReLU` with either one slope shared by every channel, or one slope per channel.
    PReLU(PReluSlopes),
}

/// `PReLU`'s negative-slope parameter, resolved from the `.nam` file's `negative_slope` (scalar,
/// shared by every channel) or `negative_slopes` (one entry per channel) key — see
/// `activations.cpp`'s `ActivationConfig::from_json`, which prefers a present `negative_slope`
/// over `negative_slopes` when (unusually) both are present.
#[derive(Debug, Clone, PartialEq)]
enum PReluSlopes {
    Scalar(f32),
    PerChannel(Vec<f32>),
}

impl Activation {
    /// `x`: flat `[channels * n]` in this crate's `Sig` layout (`data[channel * n + t]`, channel
    /// slowest-varying). `n` (the block length) is consulted only by `PReLU(PerChannel(_))`, to
    /// find each channel's own row within `x` — every other variant applies uniformly across the
    /// whole slice and ignores `n` entirely.
    ///
    /// **Why `n`, not a channel index or count:** `NeuralAmpModelerCore`'s own
    /// `ActivationPReLU::apply(float* data, long size)` indexes by `pos % negative_slopes.len()`,
    /// which is correct there because its buffer is column-major, `(channels, frames)` with frame
    /// slowest-varying — consecutive `pos` values cycle through channels within one frame. That
    /// indexing does **not** transfer to this crate's row-major-by-channel layout: here,
    /// consecutive `pos` values stay within one channel's row for `n` steps before advancing to
    /// the next channel, so the right index is `pos / n` (equivalently, one `apply_leaky_relu`
    /// call per `n`-wide row), not `pos % negative_slopes.len()`.
    fn apply(&self, x: &mut [f32], n: usize) {
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
            Activation::LeakyReLU { negative_slope } => apply_leaky_relu(x, *negative_slope),
            Activation::SiLU => vectorize_unary(
                x,
                |v| v * (f32x8::ONE / (f32x8::ONE + (-v).exp())),
                |v| v / (1.0 + (-v).exp()),
            ),
            Activation::Hardswish => vectorize_unary(
                x,
                |v| {
                    (v + f32x8::splat(3.0)).clamp(f32x8::ZERO, f32x8::splat(6.0))
                        * v
                        * f32x8::splat(1.0 / 6.0)
                },
                |v| {
                    let t = v + 3.0;
                    let clamped = t.clamp(0.0, 6.0);
                    v * clamped * (1.0 / 6.0)
                },
            ),
            Activation::Softsign => {
                vectorize_unary(x, |v| v / (f32x8::ONE + v.abs()), |v| v / (1.0 + v.abs()))
            }
            Activation::LeakyHardtanh {
                min_val,
                max_val,
                min_slope,
                max_slope,
            } => {
                let (min_val, max_val, min_slope, max_slope) =
                    (*min_val, *max_val, *min_slope, *max_slope);
                vectorize_unary(
                    x,
                    move |v| {
                        let below_mask = v.simd_lt(f32x8::splat(min_val));
                        let above_mask = v.simd_gt(f32x8::splat(max_val));
                        let below = (v - f32x8::splat(min_val)) * f32x8::splat(min_slope)
                            + f32x8::splat(min_val);
                        let above = (v - f32x8::splat(max_val)) * f32x8::splat(max_slope)
                            + f32x8::splat(max_val);
                        below_mask.select(below, above_mask.select(above, v))
                    },
                    move |v| {
                        if v < min_val {
                            (v - min_val) * min_slope + min_val
                        } else if v > max_val {
                            (v - max_val) * max_slope + max_val
                        } else {
                            v
                        }
                    },
                );
            }
            Activation::PReLU(PReluSlopes::Scalar(slope)) => apply_leaky_relu(x, *slope),
            Activation::PReLU(PReluSlopes::PerChannel(slopes)) => {
                if n == 0 {
                    return;
                }
                debug_assert_eq!(
                    x.len(),
                    slopes.len() * n,
                    "PReLU per-channel slopes.len() must equal x's channel count"
                );
                for (row, &slope) in x.chunks_exact_mut(n).zip(slopes.iter()) {
                    apply_leaky_relu(row, slope);
                }
            }
        }
    }
}

/// `x > 0 ? x : negative_slope * x`, shared by `LeakyReLU` and both `PReLU` shapes (a per-channel
/// `PReLU` is exactly this formula, applied once per channel row with that channel's own slope).
/// Vectorized via a comparison mask rather than `x.max(negative_slope * x)`, since the latter is
/// only equivalent for `negative_slope <= 1.0` — a `.nam` file's slope is not guaranteed to be,
/// and this must match `leaky_relu`'s unconditional branch (`activations.h`) for any slope value.
#[inline]
fn apply_leaky_relu(x: &mut [f32], negative_slope: f32) {
    vectorize_unary(
        x,
        move |v| {
            v.simd_gt(f32x8::ZERO)
                .select(v, v * f32x8::splat(negative_slope))
        },
        move |v| if v > 0.0 { v } else { negative_slope * v },
    );
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
    for chunk in vec_part.as_chunks_mut::<8>().0 {
        let v = vec_fn(f32x8::from(*chunk));
        chunk.copy_from_slice(&v.to_array());
    }
    for v in rem.iter_mut() {
        *v = scalar_fn(*v);
    }
}

impl TryFrom<&str> for Activation {
    type Error = NamLoadError;

    /// A bare activation name resolves every parameter to its reference default — matching
    /// `ActivationConfig::from_json`'s string branch, which builds a config with every
    /// `std::optional` parameter unset, and `Activation::get_activation` then falling back to
    /// each type's own hard-coded default (`0.01` slope, `[-1, 1]` clamp bounds, etc.).
    fn try_from(name: &str) -> Result<Self, Self::Error> {
        match name {
            "Tanh" => Ok(Activation::Tanh),
            "ReLU" => Ok(Activation::ReLU),
            "Sigmoid" => Ok(Activation::Sigmoid),
            // The spike treated an empty string the same as "Identity"; preserved here.
            "Identity" | "" => Ok(Activation::Identity),
            "LeakyReLU" => Ok(Activation::LeakyReLU {
                negative_slope: DEFAULT_LEAKY_SLOPE,
            }),
            "SiLU" => Ok(Activation::SiLU),
            "Hardswish" => Ok(Activation::Hardswish),
            "Softsign" => Ok(Activation::Softsign),
            // Reference supports both casings (`activations.cpp`'s `type_map`); mirrored here.
            "LeakyHardtanh" | "LeakyHardTanh" => Ok(Activation::LeakyHardtanh {
                min_val: -1.0,
                max_val: 1.0,
                min_slope: DEFAULT_LEAKY_SLOPE,
                max_slope: DEFAULT_LEAKY_SLOPE,
            }),
            "PReLU" => Ok(Activation::PReLU(PReluSlopes::Scalar(DEFAULT_LEAKY_SLOPE))),
            other => Err(NamLoadError {
                code: error_codes::UNSUPPORTED_ACTIVATION,
                detail: format!("unsupported activation: {other:?}"),
            }),
        }
    }
}

/// Resolves one `.nam` layer's `activation` entry (bare name, or an object naming `type` plus
/// parameters — [`file::ActivationEntry`]) to this file's `Activation`. `bottleneck` is the
/// layer's internal width, needed only to validate a per-channel `PReLU`'s `negative_slopes`
/// length; `array_index`/`layer_index` are for error messages only (a per-layer `activation`
/// array resolves one entry per layer, so a bad entry needs to name which layer it was).
fn resolve_activation_entry(
    entry: &file::ActivationEntry,
    bottleneck: usize,
    array_index: usize,
    layer_index: usize,
) -> Result<Activation, NamLoadError> {
    match entry {
        file::ActivationEntry::Name(name) => Activation::try_from(name.as_str()),
        file::ActivationEntry::Params(p) => match p.kind.as_str() {
            "Tanh" => Ok(Activation::Tanh),
            "ReLU" => Ok(Activation::ReLU),
            "Sigmoid" => Ok(Activation::Sigmoid),
            "Identity" | "" => Ok(Activation::Identity),
            "LeakyReLU" => Ok(Activation::LeakyReLU {
                negative_slope: p.negative_slope.unwrap_or(DEFAULT_LEAKY_SLOPE),
            }),
            "SiLU" => Ok(Activation::SiLU),
            "Hardswish" => Ok(Activation::Hardswish),
            "Softsign" => Ok(Activation::Softsign),
            "LeakyHardtanh" | "LeakyHardTanh" => Ok(Activation::LeakyHardtanh {
                min_val: p.min_val.unwrap_or(-1.0),
                max_val: p.max_val.unwrap_or(1.0),
                min_slope: p.min_slope.unwrap_or(DEFAULT_LEAKY_SLOPE),
                max_slope: p.max_slope.unwrap_or(DEFAULT_LEAKY_SLOPE),
            }),
            // Reference precedence (`ActivationConfig::from_json`): a present `negative_slope`
            // wins over `negative_slopes` even if both are present.
            "PReLU" => {
                if let Some(slope) = p.negative_slope {
                    Ok(Activation::PReLU(PReluSlopes::Scalar(slope)))
                } else if let Some(slopes) = &p.negative_slopes {
                    if slopes.len() != bottleneck {
                        return Err(NamLoadError {
                            code: error_codes::INCONSISTENT_CONFIGURATION,
                            detail: format!(
                                "layer array {array_index} layer {layer_index}: PReLU negative_slopes.len() ({}) does not match bottleneck ({bottleneck})",
                                slopes.len()
                            ),
                        });
                    }
                    Ok(Activation::PReLU(PReluSlopes::PerChannel(slopes.clone())))
                } else {
                    Ok(Activation::PReLU(PReluSlopes::Scalar(DEFAULT_LEAKY_SLOPE)))
                }
            }
            other => Err(NamLoadError {
                code: error_codes::UNSUPPORTED_ACTIVATION,
                detail: format!(
                    "layer array {array_index} layer {layer_index}: unsupported activation: {other:?}"
                ),
            }),
        },
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
        .as_chunks_mut::<8>()
        .0
        .iter_mut()
        .zip(in_vec_part.as_chunks::<8>().0)
    {
        let sum = f32x8::from(*o) + w_vec * f32x8::from(*i);
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
    ///
    /// `has_bias`: M10 (A2, Step A1) addition. Every dilated conv in this crate's scope has bias
    /// (`Layer::_conv` is always constructed with `true`, per `detail.h`), but a layer array's
    /// head rechannel does not always — A1's legacy `head_bias: false` must read **zero** bias
    /// floats (`LayerArray`'s ctor sizes the head Conv1D's bias vector to `head_bias ? 1 : 0` per
    /// output channel), not silently consume weights the file never wrote. `bias` is still always
    /// stored as an `out_ch`-length vector (zero-filled when `has_bias` is false) so
    /// `apply_into`'s bias-add stays branch-free; only weight *consumption* differs.
    fn read(
        r: &mut WeightReader,
        out_ch: usize,
        in_ch: usize,
        kernel_size: usize,
        dilation: usize,
        has_bias: bool,
    ) -> Result<Self, NamLoadError> {
        let weight = r.take(out_ch * in_ch * kernel_size)?;
        let bias = if has_bias {
            r.take(out_ch)?
        } else {
            vec![0.0; out_ch]
        };
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

        // M10 (A2, Step A1): `kernel_size == 1` (A1's degenerate head-rechannel case, and any
        // other 1x1-shaped Conv1D) has no causal history at all — reading straight from `input`
        // is both simpler and, unlike the general path below, allocates no `padded` scratch to
        // size (see `ArrayScratch::head_padded`'s construction), keeping this exactly as cheap as
        // the `Conv1x1` this replaced. Mathematically identical to the general path with `hl == 0`
        // (empty `history`, `padded == input`), just without touching `history`/`padded` at all.
        if hl == 0 {
            for oc in 0..self.out_ch {
                let out_row = &mut out[oc * n..(oc + 1) * n];
                out_row.fill(self.bias[oc]);
                for ic in 0..self.in_ch {
                    // kernel_size == 1, so the general formula's `* kernel_size + k` collapses to
                    // just `oc * in_ch + ic`.
                    let w = self.weight[oc * self.in_ch + ic];
                    if w == 0.0 {
                        continue;
                    }
                    let in_row = &input[ic * n..(ic + 1) * n];
                    axpy(out_row, in_row, w);
                }
            }
            return;
        }

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

/// M10 (A2, Step A3) renaming: `channels` is the trunk width this layer's residual connection
/// carries in and out (`layer_input`/`next_input_out`'s width); `bottleneck` is the internal width
/// the dilated conv and mixin produce and the activation operates on (`z`'s width). A1 files never
/// set `bottleneck` (it defaults to `channels`, D-9.12), so for A1 these two are always the same
/// number — this split is provably inert for A1, just no longer conflating two quantities that
/// happen to be equal there. What A1 called `residual` (a `channels -> channels` `Conv1x1`) is
/// `NeuralAmpModelerCore`'s `layer1x1` (a `bottleneck -> channels` `Conv1x1`); same weight-stream
/// slot, same dimensions when `bottleneck == channels`, renamed to match the reference and to stop
/// implying it's *only* ever a residual projection.
struct Layer {
    dilated: Conv1D,
    mixin: Conv1x1,
    layer1x1: Conv1x1,
    activation: Activation,
    gated: bool,
    channels: usize,
    bottleneck: usize,
}

/// Per-layer reusable scratch, sized once for a chosen max block size and reused across every
/// `process_block` call — no allocation on the hot path. `z_buf` always holds a materialized
/// copy of `z` even in the (common) ungated case, trading one cheap memcpy for keeping every
/// buffer a distinct, unambiguously-disjoint struct field.
struct LayerScratch {
    history: Sig,  // in_ch * history_len
    padded: Sig,   // in_ch * (history_len + max_n)
    conv_buf: Sig, // dilated.out_ch * max_n  (out_ch is 2x bottleneck when gated)
    z_buf: Sig,    // bottleneck * max_n
}

impl Layer {
    /// Allocation-free: writes into `scratch`'s buffers, accumulates into `head_sum`, and
    /// writes the next layer's input into caller-provided `next_input_out`. `layer_input` and
    /// `next_input_out` are flat `[channels * n]` (the trunk width); `head_sum` is flat
    /// `[bottleneck * n]`; `condition` is `[condition_size * n]`, which is `[1 * n]` since
    /// `condition_size == 1` is enforced at load time.
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
        let bn_len = self.bottleneck * n;
        let trunk_len = self.channels * n;

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
            let (top, bottom) = scratch.conv_buf[..conv_len].split_at_mut(bn_len);
            self.activation.apply(top, n);
            Activation::Sigmoid.apply(bottom, n);
            for i in 0..bn_len {
                scratch.z_buf[i] = top[i] * bottom[i];
            }
        } else {
            self.activation.apply(&mut scratch.conv_buf[..bn_len], n);
            scratch.z_buf[..bn_len].copy_from_slice(&scratch.conv_buf[..bn_len]);
        }

        // `axpy(w=1.0)`, not a plain `+=` loop (this M3 close-out pass's own vectorization fix,
        // same rationale as `Activation::apply`'s `Tanh`/`Sigmoid` above): a straight `out[i] +=
        // in[i]` loop over disjoint `&mut`/`&` slices is exactly `axpy`'s shape with `w` fixed at
        // 1.0, so it costs nothing to route through the same vectorized primitive rather than
        // trust the optimizer to notice on its own.
        axpy(&mut head_sum[..bn_len], &scratch.z_buf[..bn_len], 1.0);

        self.layer1x1.apply_into(
            &scratch.z_buf[..bn_len],
            n,
            &mut next_input_out[..trunk_len],
        );
        axpy(
            &mut next_input_out[..trunk_len],
            &layer_input[..trunk_len],
            1.0,
        );
    }
}

/// M10 (A2, Step A3/A4) additions: `bottleneck` (the array's internal width — `head_rechannel`'s
/// input width, since `head1x1` is permanently unsupported in this crate's scope, D-9.12) sits
/// alongside `channels` (the trunk width) for the same reason `Layer` grew the same split. A1
/// files never set `bottleneck`, so it always equals `channels` there.
struct LayerArray {
    rechannel: Conv1x1,
    layers: Vec<Layer>,
    /// A1's legacy head was a bias-optional `Conv1x1` (kernel 1, no dilation). M10 (A2, Step A1)
    /// unifies it onto `Conv1D`, constructed with `kernel_size = 1, dilation = 1` for A1 files —
    /// see `Conv1D::apply_into`'s `history_len() == 0` fast path for why this costs nothing extra
    /// for A1. A2's nested `head` object supplies a real kernel size/dilation instead.
    head_rechannel: Conv1D,
    input_size: usize,
    channels: usize,
    bottleneck: usize,
    head_size: usize,
}

/// Per-array reusable scratch. `io_buf` is a ping-pong pair used as "current layer input" /
/// "next layer input" across the array's layer loop, so no per-layer allocation is needed for
/// that hand-off either; which half holds the final (trunk) value after the loop is
/// `layers.len() % 2` (deterministic from the immutable `LayerArray`, not stored here).
struct ArrayScratch {
    io_buf: [Sig; 2], // channels * max_n each
    head_sum: Sig,    // bottleneck * max_n
    head_out: Sig,    // head_size * max_n
    /// `head_rechannel`'s causal-conv history/padded scratch (M10, A2 Step A4) — empty for A1
    /// (`head_rechannel.history_len() == 0` there), so this costs nothing extra for A1; see
    /// `Conv1D::apply_into`'s fast path.
    head_history: Sig,
    head_padded: Sig,
    layers: Vec<LayerScratch>,
}

/// Immutable, `Sync` weights and configuration (D-9.1 / D-8.2). Shareable across instances — the
/// spike declared `unsafe impl Sync for PreparedWaveNet {}`, but every field here (`Vec<f32>`,
/// `bool`, `usize`, the `Clone` `Activation` enum) is already auto-`Sync` on its own, and this
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
                            z_buf: vec![0f32; layer.bottleneck * max_n],
                        }
                    })
                    .collect();
                let head_hl = arr.head_rechannel.history_len();
                ArrayScratch {
                    io_buf: [
                        vec![0f32; arr.channels * max_n],
                        vec![0f32; arr.channels * max_n],
                    ],
                    head_sum: vec![0f32; arr.bottleneck * max_n],
                    head_out: vec![0f32; arr.head_size * max_n],
                    head_history: vec![0f32; head_hl * arr.head_rechannel.in_ch],
                    // M10 (A2, Step A4): zero-length for A1 (`head_hl == 0` there), matching
                    // `Conv1D::apply_into`'s fast path, which never touches this buffer when the
                    // conv has no history — see that fast path's own doc comment.
                    head_padded: if head_hl == 0 {
                        Vec::new()
                    } else {
                        vec![0f32; arr.head_rechannel.in_ch * (head_hl + max_n)]
                    },
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

/// The subset of a layer array's config this crate actually builds a `Layer`/`LayerArray` from,
/// once [`resolve_layer_array`] has confirmed every feature the array uses is one this build
/// supports (either A1's original shape, or the core-A2 subset D-9.12 scopes in — see
/// [`reject_unsupported_layer_features`]). Carries the *resolved* per-layer
/// `kernel_sizes`/`activations`, the resolved `bottleneck`/head fields, and `gated`, even though
/// `LayerArrayConfig`'s own fields are `Option` (A1 scalar vs. A2 alternative), so nothing
/// downstream has to re-derive or re-match either.
struct ResolvedLayerArrayShape {
    /// One entry per layer (`dilations.len()`); A1's scalar `kernel_size` broadcasts to every
    /// entry, A2's `kernel_sizes` array is used verbatim.
    kernel_sizes: Vec<usize>,
    /// The array's internal (dilated-conv / mixin / activation) width. A1 never sets `bottleneck`,
    /// so this defaults to `channels` there — the same number `Layer`'s `channels` field carries,
    /// which is exactly why keeping the two conflated never mattered before A2.
    bottleneck: usize,
    /// `head_rechannel`'s output width — A1's `head_size`, or A2's nested `head.out_channels`.
    head_out_channels: usize,
    /// `head_rechannel`'s kernel width — always 1 for A1's legacy shape, A2's `head.kernel_size`
    /// otherwise.
    head_kernel_size: usize,
    /// `head_rechannel`'s dilation — always 1 for A1's legacy shape (and when A2's `head_dilation`
    /// is absent).
    head_dilation: usize,
    head_bias: bool,
    gated: bool,
    /// One entry per layer (`dilations.len()`); A1's single shared `activation` broadcasts to
    /// every entry, A2's per-layer `activation` array is used verbatim.
    activations: Vec<Activation>,
}

/// D-9.12's core-A2 boundary: rejects any WaveNet feature this build does not implement, with
/// `UNSUPPORTED_CONFIGURATION` naming the offending key — the whole point of this function
/// existing is that a well-formed but out-of-scope file gets a true statement about *why* it
/// doesn't load ("condition_dsp is not yet supported") instead of the misleading `MALFORMED_JSON`
/// it got before M10. Every key rejected below is **permanently** out of scope per D-9.12: none of
/// `condition_dsp`, FiLM (all eight sites), gating, non-unity `groups_*`, `slimmable`, an active
/// `head1x1`, or an inactive/grouped `layer1x1` is planned for any future milestone. M10's earlier
/// phases (Step A1-A4) *removed* the temporary rejections this function used to also carry for
/// `kernel_sizes`, `bottleneck`, the nested `head`, and object/per-layer `activation` — those are
/// now real, implemented core-A2 features, resolved by [`resolve_layer_array`] instead of rejected
/// here.
///
/// Called before any dimension ceiling check or weight read, alongside (and ahead of, in
/// `PreparedWaveNet::from_file`'s ordering) [`validate_layer_array_dims`] — a file that is both
/// unsupported and over some ceiling should be told which feature is unsupported, since that is
/// the actionable message.
fn reject_unsupported_layer_features(
    cfg: &LayerArrayConfig,
    index: usize,
) -> Result<(), NamLoadError> {
    fn unsupported(index: usize, key: &str, detail: impl std::fmt::Display) -> NamLoadError {
        NamLoadError {
            code: error_codes::UNSUPPORTED_CONFIGURATION,
            detail: format!("layer array {index}: {key} {detail}"),
        }
    }

    if cfg.gated == Some(true) {
        return Err(unsupported(
            index,
            "gated",
            "true (gating) is not supported",
        ));
    }
    if cfg.gating_mode.is_some() {
        return Err(unsupported(index, "gating_mode", "is not supported"));
    }
    if cfg.secondary_activation.is_some() {
        return Err(unsupported(
            index,
            "secondary_activation",
            "is not supported",
        ));
    }
    if let Some(g) = cfg.groups_input
        && g != 1
    {
        return Err(unsupported(
            index,
            "groups_input",
            format!("{g} != 1 is not supported"),
        ));
    }
    if let Some(g) = cfg.groups_input_mixin
        && g != 1
    {
        return Err(unsupported(
            index,
            "groups_input_mixin",
            format!("{g} != 1 is not supported"),
        ));
    }
    // M10 (A2, Step A3): `layer1x1` defaults to active/groups=1 when the key is absent (matching
    // `model.cpp`'s `bool layer1x1_active = true;` default) — A1 has always relied on exactly that
    // default (its "residual" *is* this projection). A present object narrows what's permitted:
    // core A2 requires it active with groups 1; an explicitly inactive or grouped `layer1x1` is a
    // real, reference-supported shape this crate does not implement.
    if let Some(l1x1) = &cfg.layer1x1 {
        if !l1x1.active {
            return Err(unsupported(
                index,
                "layer1x1.active",
                "false is not supported",
            ));
        }
        if let Some(g) = l1x1.groups
            && g != 1
        {
            return Err(unsupported(
                index,
                "layer1x1.groups",
                format!("{g} != 1 is not supported"),
            ));
        }
    }
    if let Some(head1x1) = &cfg.head1x1
        && head1x1.active
    {
        return Err(unsupported(index, "head1x1", "active is not supported"));
    }
    if cfg.slimmable.is_some() {
        return Err(unsupported(index, "slimmable", "is not supported"));
    }
    for (name, film) in [
        ("conv_pre_film", &cfg.conv_pre_film),
        ("conv_post_film", &cfg.conv_post_film),
        ("input_mixin_pre_film", &cfg.input_mixin_pre_film),
        ("input_mixin_post_film", &cfg.input_mixin_post_film),
        ("activation_pre_film", &cfg.activation_pre_film),
        ("activation_post_film", &cfg.activation_post_film),
        ("layer1x1_post_film", &cfg.layer1x1_post_film),
        ("head1x1_post_film", &cfg.head1x1_post_film),
    ] {
        if let Some(f) = film
            && f.is_active()
        {
            return Err(unsupported(
                index,
                name,
                "active (FiLM conditioning) is not supported",
            ));
        }
    }
    Ok(())
}

/// Confirms `cfg` uses only features this build supports (via [`reject_unsupported_layer_features`])
/// and resolves its `Option`/alternative-shaped A1-or-A2 fields to the concrete, per-layer values
/// `PreparedWaveNet::from_file`'s weight walk needs. Both-or-neither-present and
/// length-disagreement cases (`kernel_size`/`kernel_sizes`, `head_size`+`head_bias`/`head`, a
/// per-layer `kernel_sizes`/`activation` array whose length disagrees with `dilations`) are
/// self-contradictory files — well-formed JSON, every feature it names supported, but internally
/// inconsistent about which shape it is — hence `INCONSISTENT_CONFIGURATION` rather than
/// `UNSUPPORTED_CONFIGURATION`.
fn resolve_layer_array(
    cfg: &LayerArrayConfig,
    index: usize,
) -> Result<ResolvedLayerArrayShape, NamLoadError> {
    reject_unsupported_layer_features(cfg, index)?;

    fn inconsistent(index: usize, detail: impl std::fmt::Display) -> NamLoadError {
        NamLoadError {
            code: error_codes::INCONSISTENT_CONFIGURATION,
            detail: format!("layer array {index}: {detail}"),
        }
    }

    // NFR-SEC-020 ordering, checked here rather than only in `validate_layer_array_dims`: this
    // function is about to allocate two `Vec`s sized to `dilations.len()`
    // (`kernel_sizes`/`activations` below), which is itself a dimension-derived allocation and so
    // must not happen before this dimension is bounded — see `PreparedWaveNet::from_file`'s
    // load-bearing ordering doc comment. `validate_layer_array_dims` still checks this same bound
    // again afterwards (harmless — `check_max` is a pure comparison); that copy documents the bound
    // as part of "every declared dimension", this one is what actually guards the allocations below.
    check_max(
        cfg.dilations.len(),
        MAX_DILATIONS_PER_LAYER_ARRAY,
        &format!("layer array {index}: dilations.len()"),
    )?;

    let num_layers = cfg.dilations.len();

    let kernel_sizes = match (cfg.kernel_size, &cfg.kernel_sizes) {
        (Some(_), Some(_)) => {
            return Err(inconsistent(
                index,
                "both kernel_size and kernel_sizes are present",
            ));
        }
        (None, None) => {
            return Err(inconsistent(
                index,
                "neither kernel_size nor kernel_sizes is present",
            ));
        }
        (Some(k), None) => vec![k; num_layers],
        (None, Some(ks)) => {
            if ks.len() != num_layers {
                return Err(inconsistent(
                    index,
                    format!(
                        "kernel_sizes.len() ({}) does not match dilations.len() ({num_layers})",
                        ks.len()
                    ),
                ));
            }
            ks.clone()
        }
    };

    let bottleneck = cfg.bottleneck.unwrap_or(cfg.channels);

    let (head_out_channels, head_kernel_size, head_dilation, head_bias) =
        match (cfg.head_size, &cfg.head) {
            (Some(_), Some(_)) => {
                return Err(inconsistent(
                    index,
                    "both head_size/head_bias and head are present",
                ));
            }
            (None, None) => {
                return Err(inconsistent(index, "neither head_size nor head is present"));
            }
            (Some(head_size), None) => (head_size, 1, 1, cfg.head_bias.unwrap_or(false)),
            (None, Some(h)) => (
                h.out_channels,
                h.kernel_size,
                h.head_dilation.unwrap_or(1),
                h.bias,
            ),
        };

    let gated = cfg.gated.unwrap_or(false);

    let activations = match &cfg.activation {
        file::ActivationSpec::One(entry) => {
            let activation = resolve_activation_entry(entry, bottleneck, index, 0)?;
            vec![activation; num_layers]
        }
        file::ActivationSpec::PerLayer(entries) => {
            if entries.len() != num_layers {
                return Err(inconsistent(
                    index,
                    format!(
                        "activation.len() ({}) does not match dilations.len() ({num_layers})",
                        entries.len()
                    ),
                ));
            }
            entries
                .iter()
                .enumerate()
                .map(|(layer_idx, e)| resolve_activation_entry(e, bottleneck, index, layer_idx))
                .collect::<Result<Vec<_>, _>>()?
        }
    };

    Ok(ResolvedLayerArrayShape {
        kernel_sizes,
        bottleneck,
        head_out_channels,
        head_kernel_size,
        head_dilation,
        head_bias,
        gated,
        activations,
    })
}

/// Validates one layer array's declared dimensions against this crate's NFR-SEC-020 ceilings,
/// its lower bounds, and the `condition_size == 1` constraint (this implementation always feeds
/// the raw mono input as the sole conditioning signal, matching every real WaveNet export — a
/// different declared `condition_size` isn't representable by this code and must be rejected
/// cleanly, not silently misread). Called *before* any weight reading or scratch sizing for this
/// array, so every later multiplication involving these fields is safe from `usize` overflow by
/// construction. `resolved` is [`resolve_layer_array`]'s output for the same `cfg`/`index`.
fn validate_layer_array_dims(
    cfg: &LayerArrayConfig,
    resolved: &ResolvedLayerArrayShape,
    index: usize,
) -> Result<(), NamLoadError> {
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
        resolved.head_out_channels,
        MAX_HEAD_SIZE,
        &format!("layer array {index}: head out_channels"),
    )?;
    check_max(
        cfg.channels,
        MAX_CHANNELS,
        &format!("layer array {index}: channels"),
    )?;
    check_max(
        resolved.bottleneck,
        MAX_CHANNELS,
        &format!("layer array {index}: bottleneck"),
    )?;
    check_max(
        resolved.head_kernel_size,
        MAX_KERNEL_SIZE,
        &format!("layer array {index}: head kernel_size"),
    )?;
    check_max(
        resolved.head_dilation,
        MAX_DILATION,
        &format!("layer array {index}: head_dilation"),
    )?;
    check_max(
        cfg.dilations.len(),
        MAX_DILATIONS_PER_LAYER_ARRAY,
        &format!("layer array {index}: dilations.len()"),
    )?;

    check_min1(cfg.input_size, &format!("layer array {index}: input_size"))?;
    check_min1(cfg.channels, &format!("layer array {index}: channels"))?;
    check_min1(
        resolved.bottleneck,
        &format!("layer array {index}: bottleneck"),
    )?;
    check_min1(
        resolved.head_out_channels,
        &format!("layer array {index}: head out_channels"),
    )?;
    check_min1(
        resolved.head_kernel_size,
        &format!("layer array {index}: head kernel_size"),
    )?;
    check_min1(
        resolved.head_dilation,
        &format!("layer array {index}: head_dilation"),
    )?;
    check_min1(
        cfg.dilations.len(),
        &format!("layer array {index}: dilations.len()"),
    )?;

    if cfg.condition_size != 1 {
        return Err(NamLoadError {
            code: error_codes::UNSUPPORTED_CONDITION_SIZE,
            detail: format!(
                "layer array {index}: condition_size must be 1, found {}",
                cfg.condition_size
            ),
        });
    }

    // NFR-SEC-020: the head rechannel's own causal-conv history buffer
    // (`bottleneck * (head_kernel_size - 1) * head_dilation` elements — `head_rechannel`'s input
    // width is `bottleneck`, since `head1x1` is permanently unsupported here) is a product none of
    // the individual per-factor ceilings above bounds on its own. Mirrors the per-layer check
    // below; see `MAX_CONV_HISTORY_ELEMENTS`'s own doc comment.
    let head_history_elements = resolved
        .bottleneck
        .saturating_mul(resolved.head_kernel_size.saturating_sub(1))
        .saturating_mul(resolved.head_dilation);
    check_max(
        head_history_elements,
        MAX_CONV_HISTORY_ELEMENTS,
        &format!("layer array {index}: bottleneck * (head kernel_size - 1) * head_dilation"),
    )?;

    // NFR-SEC-020, M10 addition: a dilation value appears in no weight count — unlike every other
    // dimension checked above — so nothing else here bounds it, and the padded causal-conv history
    // buffer `Conv1D` allocates (`channels * (kernel_size - 1) * dilation` elements — the dilated
    // conv's input width is `channels`, the trunk width, not `bottleneck`) is a product none of
    // the individual per-factor ceilings above bounds either. See `MAX_DILATION`'s and
    // `MAX_CONV_HISTORY_ELEMENTS`' own doc comments for why this closes a real gap, not a
    // hypothetical one. M10 (A2, Step A3): `kernel_size` is now per-layer
    // (`resolved.kernel_sizes[layer_idx]`), not a single array-wide scalar.
    for (layer_idx, (&dilation, &kernel_size)) in
        cfg.dilations.iter().zip(&resolved.kernel_sizes).enumerate()
    {
        check_max(
            dilation,
            MAX_DILATION,
            &format!("layer array {index} layer {layer_idx}: dilation"),
        )?;
        check_min1(
            dilation,
            &format!("layer array {index} layer {layer_idx}: dilation"),
        )?;
        check_max(
            kernel_size,
            MAX_KERNEL_SIZE,
            &format!("layer array {index} layer {layer_idx}: kernel_size"),
        )?;
        check_min1(
            kernel_size,
            &format!("layer array {index} layer {layer_idx}: kernel_size"),
        )?;
        let history_elements = cfg
            .channels
            .saturating_mul(kernel_size.saturating_sub(1))
            .saturating_mul(dilation);
        check_max(
            history_elements,
            MAX_CONV_HISTORY_ELEMENTS,
            &format!(
                "layer array {index} layer {layer_idx}: channels * (kernel_size - 1) * dilation"
            ),
        )?;
    }

    Ok(())
}

impl PreparedWaveNet {
    /// The semantic half of P6's "one hardened place `.nam` bytes go through" (the other half is
    /// `NamFile::parse`'s JSON-shape parsing). Validation order:
    ///
    /// 1. `architecture == "WaveNet"` (LSTM and anything else: `UNSUPPORTED_ARCHITECTURE` — LSTM
    ///    support is a documented open scope gap, see the crate doc comment, not a bug).
    /// 2. `config.head.is_none()` (`UNSUPPORTED_HEAD_CONFIG` — this is the top-level post-stack
    ///    head; a layer array's own nested `head` is a different field, checked in step 8).
    /// 3. `config.condition_dsp.is_none()` (`UNSUPPORTED_CONFIGURATION` — D-9.12 defers this A2
    ///    feature permanently).
    /// 4. `config.in_channels` is absent or `1` (`UNSUPPORTED_CONFIGURATION` — core-A2 scope).
    /// 5. `sample_rate` is nonzero if present (`INVALID_SAMPLE_RATE`), else defaults to 48 kHz.
    /// 6. `config.layers` is non-empty (`EMPTY_LAYER_ARRAYS`).
    /// 7. `config.layers.len()` and `weights.len()` are within their NFR-SEC-020 ceilings
    ///    (`DIMENSION_LIMIT_EXCEEDED`).
    /// 8. Every layer array is resolved via `resolve_layer_array` (M10, FR-NAM-140/D-9.12): any
    ///    permanently out-of-scope feature the array uses is rejected by name
    ///    (`UNSUPPORTED_CONFIGURATION`), a self-contradictory shape (both-or-neither of an A1/A2
    ///    field pair present, or an array length disagreeing with `dilations.len()`) is rejected as
    ///    such (`INCONSISTENT_CONFIGURATION`, including `dilations.len()` itself against its own
    ///    ceiling — see that function's own doc comment for why that one check can't wait for step
    ///    8's next part), then its dimensions (now including A2's per-layer `kernel_sizes`,
    ///    `bottleneck`, and the nested head's `out_channels`/`kernel_size`/`head_dilation`) are
    ///    checked against their ceilings, at least 1, and `condition_size == 1`
    ///    (`DIMENSION_LIMIT_EXCEEDED` / `UNSUPPORTED_CONDITION_SIZE`), including the per-layer and
    ///    per-head NFR-SEC-020 product checks `validate_layer_array_dims` performs.
    ///
    /// Step 8 all happens *before* step 9 reads a single weight or performs a single
    /// dimension-derived multiplication or allocation. This ordering is load-bearing, not
    /// decorative: once every dimension that ever appears in a product (`channels * input_size`,
    /// `bottleneck * channels * kernel_size`, and so on) is bounded well below any value that could
    /// overflow `usize` on a 64-bit target, every such product later in this function and in
    /// `Conv1x1::read`/`Conv1D::read`/`WeightReader::take` is safe from overflow by construction.
    /// Do not "helpfully" add `checked_mul` throughout the rest of this file to reproduce a
    /// guarantee this section already provides, and do not remove this section without replacing
    /// that guarantee some other way. `MAX_CONV_HISTORY_ELEMENTS`'s checks inside
    /// `validate_layer_array_dims` (one per layer, one for the head rechannel) are themselves
    /// *part* of that guarantee, not a redundant check on top of it — see that constant's own doc
    /// comment.
    ///
    /// 9. Each `LayerArray` is built via `WeightReader`, in the order the spike's own reading of
    ///    `NeuralAmpModelerCore` established and A2's `a2_fast.cpp` independently restates: per
    ///    array `[rechannel (no bias), per-layer[dilated (bias), mixin (no bias), layer1x1 (bias)],
    ///    head_rechannel (bias iff head.bias/head_bias)]` (`WEIGHT_COUNT_MISMATCH` on exhaustion).
    ///    `layer1x1` is A1's `residual` renamed (same weight-stream slot; A1's degenerate case has
    ///    `bottleneck == channels`), and the dilated conv's output / mixin's output / activation /
    ///    `layer1x1`'s input are all `bottleneck`-wide rather than `channels`-wide once A2's
    ///    `bottleneck` differs from `channels` — see `Layer`'s own doc comment for the split.
    /// 10. Adjacent arrays chain via two separate signals, per the module doc comment: the residual
    ///     trunk feeds the next array's rechannel input (`channels[i] == input_size[i+1]`), and the
    ///     head-rechannel output separately seeds the next array's head-sum accumulator, which is
    ///     `bottleneck`-wide (`head_size[i] == bottleneck[i+1]`, corrected in M10 from an
    ///     A1-era check against `channels[i+1]` that only ever agreed with the true constraint
    ///     because A1 has no way to make `bottleneck` differ from `channels`) — both
    ///     `LAYER_ARRAY_CHAINING_MISMATCH` on mismatch.
    /// 11. The trailing `head_scale` float is resolved exactly as the spike's confirmed reading
    ///     of `WaveNet::set_weights_`: if one float remains after step 9, it is authoritative; if
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
        if nam.config.condition_dsp.is_some() {
            return Err(NamLoadError {
                code: error_codes::UNSUPPORTED_CONFIGURATION,
                detail: "config.condition_dsp is not yet supported".to_string(),
            });
        }
        if let Some(in_channels) = nam.config.in_channels
            && in_channels != 1
        {
            return Err(NamLoadError {
                code: error_codes::UNSUPPORTED_CONFIGURATION,
                detail: format!("config.in_channels must be 1, found {in_channels}"),
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

        let mut resolved_arrays = Vec::with_capacity(nam.config.layers.len());
        for (i, cfg) in nam.config.layers.iter().enumerate() {
            let resolved = resolve_layer_array(cfg, i)?;
            validate_layer_array_dims(cfg, &resolved, i)?;
            resolved_arrays.push(resolved);
        }

        let mut r = WeightReader::new(&nam.weights);
        let mut arrays = Vec::with_capacity(nam.config.layers.len());
        for (cfg, resolved) in nam.config.layers.iter().zip(&resolved_arrays) {
            let out_mult = if resolved.gated { 2 } else { 1 };
            // No bias: NeuralAmpModelerCore's LayerArray ctor constructs `_rechannel` with
            // bias=false, confirmed by reading that constructor directly (see spike README).
            let rechannel = Conv1x1::read(&mut r, cfg.channels, cfg.input_size, false)?;

            let mut layers = Vec::with_capacity(cfg.dilations.len());
            for (li, (&dilation, &kernel_size)) in
                cfg.dilations.iter().zip(&resolved.kernel_sizes).enumerate()
            {
                // Dilated conv: channels (trunk) -> bottleneck (or 2x when gated), always biased
                // (`Layer::_conv` in `detail.h` is always constructed with `do_bias = true`).
                let dilated = Conv1D::read(
                    &mut r,
                    resolved.bottleneck * out_mult,
                    cfg.channels,
                    kernel_size,
                    dilation,
                    true,
                )?;
                let mixin = Conv1x1::read(
                    &mut r,
                    resolved.bottleneck * out_mult,
                    cfg.condition_size,
                    false,
                )?;
                // layer1x1: bottleneck -> channels (trunk), always biased when present, and
                // `reject_unsupported_layer_features` has already confirmed it's either absent
                // (A1's implicit default) or present-and-active-with-groups-1.
                let layer1x1 = Conv1x1::read(&mut r, cfg.channels, resolved.bottleneck, true)?;
                layers.push(Layer {
                    dilated,
                    mixin,
                    layer1x1,
                    activation: resolved.activations[li].clone(),
                    gated: resolved.gated,
                    channels: cfg.channels,
                    bottleneck: resolved.bottleneck,
                });
            }

            // Head rechannel: bottleneck -> head_out_channels (head1x1 is permanently
            // unsupported here, so the head accumulator's width is always `bottleneck`).
            let head_rechannel = Conv1D::read(
                &mut r,
                resolved.head_out_channels,
                resolved.bottleneck,
                resolved.head_kernel_size,
                resolved.head_dilation,
                resolved.head_bias,
            )?;

            arrays.push(LayerArray {
                rechannel,
                layers,
                head_rechannel,
                input_size: cfg.input_size,
                channels: cfg.channels,
                bottleneck: resolved.bottleneck,
                head_size: resolved.head_out_channels,
            });
        }

        // Adjacent arrays chain via TWO separate signals (see the module doc comment): the
        // residual trunk (dim = channels) feeds the next array's rechannel input, while the
        // head-rechannel output (dim = head_size) separately seeds the next array's head
        // accumulator, which is `bottleneck`-wide (head1x1 is permanently unsupported here, so
        // the accumulator's width is always the next array's own `bottleneck`, not its
        // `channels` — these only ever agreed for A1, where `bottleneck` can't differ from
        // `channels` at all).
        for (i, w) in arrays.windows(2).enumerate() {
            if w[0].head_size != w[1].bottleneck {
                return Err(NamLoadError {
                    code: error_codes::LAYER_ARRAY_CHAINING_MISMATCH,
                    detail: format!(
                        "layer array {i} head_size ({}) does not match layer array {} bottleneck ({})",
                        w[0].head_size,
                        i + 1,
                        w[1].bottleneck
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

    /// FR-NAM-090: the model's declared integrated loudness (LUFS), or `None` when the source
    /// file's `metadata.loudness` was absent or `null` (every A1 file; any A2 file that doesn't
    /// declare it either). A thin forward over `metadata().loudness` -- kept as its own accessor,
    /// mirroring `metadata()`/`sample_rate()`, so a caller that only needs this one figure doesn't
    /// have to know it lives inside `NamMetadata`.
    pub fn loudness_lufs(&self) -> Option<f32> {
        self.metadata.loudness
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
            let clen = arr.channels * n; // trunk width
            let bn_len = arr.bottleneck * n; // head-sum / activation width
            let (before, at_and_after) = state_arrays.split_at_mut(a);
            let ascratch = &mut at_and_after[0];

            if a == 0 {
                arr.rechannel
                    .apply_into(&condition[..n], n, &mut ascratch.io_buf[0][..clen]);
                ascratch.head_sum[..bn_len].fill(0.0);
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
                // `bn_len` here equals `prev_arr.head_size * n` by the chaining check in
                // `PreparedWaveNet::from_file` (`head_size[i] == bottleneck[i+1]`), so this is the
                // same length as `prev_scratch.head_out`'s valid prefix.
                ascratch.head_sum[..bn_len].copy_from_slice(&prev_scratch.head_out[..bn_len]);
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
                    &mut ascratch.head_sum[..bn_len],
                    write_buf,
                );
                cur = 1 - cur;
            }

            let head_len = arr.head_size * n;
            arr.head_rechannel.apply_into(
                &ascratch.head_sum[..bn_len],
                n,
                &mut ascratch.head_history,
                &mut ascratch.head_padded,
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

    /// M10: every field beyond the five A1 requires is now `Option`, to make room for A2's
    /// alternatives (see `file.rs`'s module doc comment) — so a "minimal" A1 layer array has to
    /// state all of them explicitly (`None` where A1 has no opinion) rather than rely on a
    /// `Default` impl, since `ActivationSpec` has no sensible default value to derive one from.
    fn minimal_layer_array() -> LayerArrayConfig {
        LayerArrayConfig {
            input_size: 1,
            condition_size: 1,
            channels: 2,
            dilations: vec![1],
            activation: file::ActivationSpec::One(file::ActivationEntry::Name("Tanh".to_string())),
            kernel_size: Some(2),
            kernel_sizes: None,
            bottleneck: None,
            head_size: Some(1),
            head_bias: Some(false),
            head: None,
            gated: Some(false),
            gating_mode: None,
            secondary_activation: None,
            groups_input: None,
            groups_input_mixin: None,
            layer1x1: None,
            head1x1: None,
            slimmable: None,
            conv_pre_film: None,
            conv_post_film: None,
            input_mixin_pre_film: None,
            input_mixin_post_film: None,
            activation_pre_film: None,
            activation_post_film: None,
            layer1x1_post_film: None,
            head1x1_post_film: None,
        }
    }

    /// Computes exactly how many weights `PreparedWaveNet::from_file` will consume for `cfg`, so
    /// tests can hand-build a matching (or deliberately mismatched) flat weight array without
    /// going through JSON at all. Assumes `cfg` is a plain A1 shape (`bottleneck == channels`, a
    /// legacy scalar `head_size`/`head_bias`) — see [`a2_weight_count_for`] below for the core-A2
    /// analogue used by this module's A2 tests.
    fn weight_count_for(cfg: &LayerArrayConfig) -> usize {
        let out_mult = if cfg.gated == Some(true) { 2 } else { 1 };
        let kernel_size = cfg.kernel_size.expect("A1 fixture: kernel_size is set");
        let head_size = cfg.head_size.expect("A1 fixture: head_size is set");
        let mut n = cfg.channels * cfg.input_size; // rechannel, no bias
        for _ in &cfg.dilations {
            n += cfg.channels * out_mult * cfg.channels * kernel_size; // dilated weight
            n += cfg.channels * out_mult; // dilated bias
            n += cfg.channels * out_mult * cfg.condition_size; // mixin, no bias
            n += cfg.channels * cfg.channels; // residual weight
            n += cfg.channels; // residual bias
        }
        n += head_size * cfg.channels; // head_rechannel weight
        if cfg.head_bias == Some(true) {
            n += head_size;
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
                in_channels: None,
                condition_dsp: None,
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
        file.config.layers[0].activation =
            file::ActivationSpec::One(file::ActivationEntry::Name("GELU".to_string()));
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

    /// M10: closes a gap present in A1 all along and found while scoping A2 — a dilation value
    /// appears in no weight count, so nothing bounded it before `MAX_DILATION` existed. Without
    /// that check, `dilations: [4_000_000_000]` needs only a handful of weights and would attempt
    /// to allocate on the order of 8 GB for one layer's causal-conv history
    /// (`channels * (kernel_size - 1) * dilation`, with this fixture's `channels: 2,
    /// kernel_size: 2`). The test completing at all demonstrates the ceiling check runs first.
    #[test]
    fn rejects_dilation_over_ceiling_without_attempting_a_huge_allocation() {
        let mut file = minimal_valid_file();
        file.config.layers[0].dilations = vec![4_000_000_000];
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::DIMENSION_LIMIT_EXCEEDED.id);
    }

    /// A dilation that individually passes `MAX_DILATION` but whose implied history buffer
    /// (`channels * (kernel_size - 1) * dilation`) is still enormous — the product ceiling
    /// `MAX_CONV_HISTORY_ELEMENTS` exists specifically because no single-factor ceiling bounds
    /// this product on its own.
    #[test]
    fn rejects_dilation_whose_history_product_exceeds_its_own_ceiling() {
        let mut file = minimal_valid_file();
        file.config.layers[0].channels = MAX_CHANNELS;
        file.config.layers[0].kernel_size = Some(MAX_KERNEL_SIZE);
        file.config.layers[0].dilations = vec![MAX_DILATION];
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
                in_channels: None,
                condition_dsp: None,
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

    // trace-partial: FR-NAM-110
    // uncovered: FR-NAM-110 — the method's "cross-correlate an impulse through the stage" is
    // uncovered: performed by nothing: both tagged tests read an accessor whose body is the
    // uncovered: literal 0 and assert it equals 0, so they would pass unchanged if inference did
    // uncovered: introduce delay, and the one path that reports a nonzero figure — NamStage's
    // uncovered: resampler latency — is asserted only as > 0 and is documented as not
    // uncovered: sample-exact; closes M8
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
                    // `ActivationSpec` is `Deserialize`-only (see this module's own doc comment on
                    // why file.rs stays that way), so this is written as the literal JSON shape
                    // `minimal_layer_array`'s `activation` field resolves to, not `cfg.activation`.
                    "activation": "Tanh",
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

    // -----------------------------------------------------------------------------------------
    // M10 (A2, Steps A1-A4): core-A2 feature coverage. Small-scale hand-built fixtures, not the
    // full 23-layer "A2 standard"/"A2 nano" shapes `a2_fast.h` describes — nothing about that
    // scale exercises different code than a 1-2 layer array does, per this crate's house style
    // (see `minimal_layer_array`'s own doc comment on the same tradeoff). Per D-9.12/this PR's
    // scope, `namir-fixtures` is deliberately never used or waited on here — see the module doc
    // comment on why parity against that independent oracle is a separate, later integration step.
    // -----------------------------------------------------------------------------------------

    /// A hand-built core-A2 layer array: per-layer `kernel_sizes`, a `bottleneck` distinct from
    /// `channels`, a real (non-`k=1`) nested `head`, a per-layer `activation` array mixing a bare
    /// name and a parameterized object, and an explicit, active, ungrouped `layer1x1` — one array
    /// that exercises every new code path Step A2/A3/A4 added.
    fn a2_minimal_layer_array() -> LayerArrayConfig {
        LayerArrayConfig {
            input_size: 1,
            condition_size: 1,
            channels: 3,
            dilations: vec![1, 2],
            activation: file::ActivationSpec::PerLayer(vec![
                file::ActivationEntry::Params(file::ActivationParams {
                    kind: "LeakyReLU".to_string(),
                    negative_slope: Some(0.1),
                    negative_slopes: None,
                    min_val: None,
                    max_val: None,
                    min_slope: None,
                    max_slope: None,
                }),
                file::ActivationEntry::Name("Tanh".to_string()),
            ]),
            kernel_size: None,
            kernel_sizes: Some(vec![2, 3]),
            bottleneck: Some(2),
            head_size: None,
            head_bias: None,
            head: Some(file::LayerArrayHeadConfig {
                out_channels: 1,
                kernel_size: 3,
                head_dilation: Some(2),
                bias: true,
            }),
            gated: Some(false),
            gating_mode: None,
            secondary_activation: None,
            groups_input: None,
            groups_input_mixin: None,
            layer1x1: Some(file::Conv1x1FeatureConfig {
                active: true,
                groups: Some(1),
                out_channels: None,
            }),
            head1x1: None,
            slimmable: None,
            conv_pre_film: None,
            conv_post_film: None,
            input_mixin_pre_film: None,
            input_mixin_post_film: None,
            activation_pre_film: None,
            activation_post_film: None,
            layer1x1_post_film: None,
            head1x1_post_film: None,
        }
    }

    /// The A2 analogue of `weight_count_for`: computes the exact weight count
    /// `PreparedWaveNet::from_file` consumes for a core-A2-shaped `cfg` (per-layer
    /// `kernel_sizes`, a `bottleneck` that may differ from `channels`, a nested `head`), so tests
    /// can hand-build a matching flat weight array without going through JSON.
    fn a2_weight_count_for(cfg: &LayerArrayConfig) -> usize {
        let bottleneck = cfg.bottleneck.unwrap_or(cfg.channels);
        let kernel_sizes: Vec<usize> = match (&cfg.kernel_size, &cfg.kernel_sizes) {
            (Some(k), None) => vec![*k; cfg.dilations.len()],
            (None, Some(ks)) => ks.clone(),
            _ => panic!("a2_weight_count_for: exactly one of kernel_size/kernel_sizes expected"),
        };
        let mut n = cfg.channels * cfg.input_size; // rechannel, no bias
        for &k in &kernel_sizes {
            n += bottleneck * cfg.channels * k; // dilated weight
            n += bottleneck; // dilated bias
            n += bottleneck * cfg.condition_size; // mixin, no bias
            n += cfg.channels * bottleneck; // layer1x1 weight
            n += cfg.channels; // layer1x1 bias
        }
        let (head_out, head_kernel, head_bias) = match &cfg.head {
            Some(h) => (h.out_channels, h.kernel_size, h.bias),
            None => (
                cfg.head_size.expect("A2 fixture: head_size or head is set"),
                1,
                cfg.head_bias.unwrap_or(false),
            ),
        };
        n += head_out * bottleneck * head_kernel; // head_rechannel weight
        if head_bias {
            n += head_out;
        }
        n
    }

    fn a2_minimal_valid_file() -> NamFile {
        let cfg = a2_minimal_layer_array();
        let n = a2_weight_count_for(&cfg);
        let mut weights = vec![0.01f32; n];
        weights.push(0.5); // trailing head_scale
        NamFile {
            version: None,
            architecture: "WaveNet".to_string(),
            config: WaveNetConfig {
                layers: vec![cfg],
                head_scale: 0.5,
                head: None,
                in_channels: None,
                condition_dsp: None,
            },
            weights,
            sample_rate: Some(48_000),
            metadata: NamMetadata::default(),
        }
    }

    #[test]
    fn a2_layer_array_loads_successfully_and_produces_finite_output() {
        let file = a2_minimal_valid_file();
        let prepared = PreparedWaveNet::from_file(&file).expect("core-A2 layer array should load");
        let mut state = prepared.new_state(8);
        let input: Vec<f32> = (0..8).map(|i| 0.1 * i as f32).collect();
        let out = prepared.process(&mut state, &input);
        assert_eq!(out.len(), 8); // head.out_channels == 1
        assert!(
            out.iter().all(|v| v.is_finite()),
            "output must be finite, not NaN/inf: {out:?}"
        );
        assert!(
            out.iter().any(|&v| v != 0.0),
            "output must be non-degenerate (not all zero): {out:?}"
        );
    }

    #[test]
    fn rejects_both_kernel_size_and_kernel_sizes_present() {
        let mut file = a2_minimal_valid_file();
        file.config.layers[0].kernel_size = Some(2);
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
    }

    #[test]
    fn rejects_kernel_sizes_length_mismatch_with_dilations() {
        let mut file = a2_minimal_valid_file();
        file.config.layers[0].kernel_sizes = Some(vec![2]); // dilations has 2 entries
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
    }

    #[test]
    fn rejects_both_head_size_and_head_present() {
        let mut file = a2_minimal_valid_file();
        file.config.layers[0].head_size = Some(1);
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
    }

    #[test]
    fn rejects_activation_per_layer_length_mismatch_with_dilations() {
        let mut file = a2_minimal_valid_file();
        file.config.layers[0].activation =
            file::ActivationSpec::PerLayer(vec![file::ActivationEntry::Name("Tanh".to_string())]);
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
    }

    #[test]
    fn rejects_layer1x1_inactive_even_when_other_a2_features_are_used() {
        let mut file = a2_minimal_valid_file();
        file.config.layers[0].layer1x1 = Some(file::Conv1x1FeatureConfig {
            active: false,
            groups: Some(1),
            out_channels: None,
        });
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_CONFIGURATION.id);
        assert!(err.detail.contains("layer1x1"));
    }

    #[test]
    fn rejects_layer1x1_grouped_even_when_other_a2_features_are_used() {
        let mut file = a2_minimal_valid_file();
        file.config.layers[0].layer1x1 = Some(file::Conv1x1FeatureConfig {
            active: true,
            groups: Some(2),
            out_channels: None,
        });
        let err = expect_err(PreparedWaveNet::from_file(&file));
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_CONFIGURATION.id);
        assert!(err.detail.contains("layer1x1"));
    }

    #[test]
    fn resolves_bare_and_object_activation_entries() {
        let leaky = resolve_activation_entry(
            &file::ActivationEntry::Name("LeakyReLU".to_string()),
            4,
            0,
            0,
        )
        .unwrap();
        assert_eq!(
            leaky,
            Activation::LeakyReLU {
                negative_slope: DEFAULT_LEAKY_SLOPE
            }
        );

        let silu =
            resolve_activation_entry(&file::ActivationEntry::Name("SiLU".to_string()), 4, 0, 0)
                .unwrap();
        assert_eq!(silu, Activation::SiLU);

        let custom_leaky = resolve_activation_entry(
            &file::ActivationEntry::Params(file::ActivationParams {
                kind: "LeakyReLU".to_string(),
                negative_slope: Some(0.2),
                negative_slopes: None,
                min_val: None,
                max_val: None,
                min_slope: None,
                max_slope: None,
            }),
            4,
            0,
            0,
        )
        .unwrap();
        assert_eq!(
            custom_leaky,
            Activation::LeakyReLU {
                negative_slope: 0.2
            }
        );

        let prelu_scalar = resolve_activation_entry(
            &file::ActivationEntry::Params(file::ActivationParams {
                kind: "PReLU".to_string(),
                negative_slope: None,
                negative_slopes: None,
                min_val: None,
                max_val: None,
                min_slope: None,
                max_slope: None,
            }),
            4,
            0,
            0,
        )
        .unwrap();
        assert_eq!(
            prelu_scalar,
            Activation::PReLU(PReluSlopes::Scalar(DEFAULT_LEAKY_SLOPE))
        );
    }

    #[test]
    fn rejects_prelu_negative_slopes_length_disagreeing_with_bottleneck() {
        let entry = file::ActivationEntry::Params(file::ActivationParams {
            kind: "PReLU".to_string(),
            negative_slope: None,
            negative_slopes: Some(vec![0.1, 0.2, 0.3]), // 3 slopes, bottleneck is 2
            min_val: None,
            max_val: None,
            min_slope: None,
            max_slope: None,
        });
        let err = resolve_activation_entry(&entry, 2, 0, 0).unwrap_err();
        assert_eq!(err.code.id, error_codes::INCONSISTENT_CONFIGURATION.id);
    }

    /// The single strongest detector for a mis-indexed per-channel `PReLU`: this crate's `Sig`
    /// layout is `data[channel * n + t]` (channel slowest-varying), the *opposite* of
    /// `NeuralAmpModelerCore`'s column-major `(channels, frames)` buffer its own `pos %
    /// negative_slopes.len()` indexing assumes — see `Activation::apply`'s doc comment. Two
    /// channels, both entirely negative but with different slopes and different magnitudes, so a
    /// `pos % 2`-style bug (which would alternate slopes *within* a channel's own row instead of
    /// applying one slope per whole row) produces visibly different numbers than the correct
    /// per-row application asserted here.
    #[test]
    fn prelu_per_channel_applies_correct_slope_to_each_channel_row() {
        let n = 3;
        let activation = Activation::PReLU(PReluSlopes::PerChannel(vec![0.1, 0.5]));
        let mut x = vec![-1.0, -2.0, -3.0, -4.0, -5.0, -6.0];
        activation.apply(&mut x, n);
        assert_eq!(&x[0..3], &[-0.1, -0.2, -0.3]);
        assert_eq!(&x[3..6], &[-2.0, -2.5, -3.0]);
    }

    /// Mirrors `tests/fixtures.rs`'s `chunked_processing_matches_monolithic_processing`, but as a
    /// hand-built unit test using a real (non-`k=1`) nested head (`a2_minimal_layer_array`'s
    /// `head.kernel_size == 3`) — the single strongest detector for a mis-sized or missing head
    /// history buffer (`ArrayScratch::head_history`/`head_padded`, M10 Step A4), since a wrong
    /// buffer diverges immediately at chunk size 1.
    #[test]
    fn chunk_size_one_processing_matches_monolithic_for_a2_nested_head() {
        let file = a2_minimal_valid_file();
        let prepared = PreparedWaveNet::from_file(&file).unwrap();
        let input: Vec<f32> = (0..16).map(|i| (i as f32 * 0.37).sin() * 0.5).collect();

        let mut mono_state = prepared.new_state(16);
        let mono_out = prepared.process(&mut mono_state, &input);
        let per_sample = mono_out.len() / input.len();

        let mut chunked_state = prepared.new_state(16);
        let mut chunked_out = vec![0f32; mono_out.len()];
        for (i, &sample) in input.iter().enumerate() {
            let mut block_out = vec![0f32; per_sample];
            prepared.process_block(&mut chunked_state, &[sample], &mut block_out);
            chunked_out[i * per_sample..(i + 1) * per_sample].copy_from_slice(&block_out);
        }

        for (i, (&mono, &chunked)) in mono_out.iter().zip(chunked_out.iter()).enumerate() {
            assert!(
                (mono - chunked).abs() < 1e-4,
                "sample {i}: monolithic {mono} vs. chunked {chunked} diverge"
            );
        }
    }

    #[test]
    fn process_block_does_not_allocate_for_a2_layer_array() {
        let file = a2_minimal_valid_file();
        let prepared = PreparedWaveNet::from_file(&file).unwrap();
        let mut state = prepared.new_state(64);
        let input = vec![0.1f32; 64];
        let mut output = vec![0.0f32; 64]; // head.out_channels == 1
        rt_harness::audio_section(|| {
            prepared.process_block(&mut state, &input, &mut output);
        });
    }
}
