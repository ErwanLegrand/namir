//! Non-uniform partitioned convolution, ported from `spikes/s2-ir-convolution/src/lib.rs`
//! (D-9.4/D-9.5/D-9.6) with three changes beyond a straight port:
//!
//! 1. **The `PreparedIr` / `IrState` split (D-9.1, D-8.2)**, mirroring
//!    `namir-nam/src/wavenet.rs`'s `PreparedNam` / `NamState` split: the spike's
//!    `PartitionedConvolver` couples immutable per-partition FFT machinery (spectra, plans) with
//!    mutable per-call state (`in_buf`, `in_pos`, `ring`, `t`) in one struct. Here, [`PreparedIr`]
//!    holds only the immutable, `Sync`, `Arc`-shareable half — one or two (mono or stereo)
//!    independent `{head taps, FFT partition specs+spectra}` sets — and [`IrState`] holds only
//!    the mutable, per-instance, never-shared half (ring buffers, `in_pos`, `t`).
//!
//! 2. **R-8's fix, built in from the start, not retrofitted**: the spike's key finding (see its
//!    README) is that every FFT partition of the same nominal size `P` starts accumulating input
//!    at absolute stream time `t=0`, so every partition in a same-size group triggers its FFT on
//!    the *same* block, forever — piling that whole group's cost onto one recurring block instead
//!    of spreading it out. The fix, implemented in [`build_schedule`]: each partition sharing
//!    nominal size `P` gets a `stagger` (a multiple of the host block size, round-robin across
//!    the size level's whole schedule — see `build_schedule`'s doc comment for exactly how, and
//!    "R-8, verified and tuned (M3)" below for why it's shaped this way).
//!    [`PreparedIr::from_wav_bytes_with_schedule`] then pre-zeros that partition's input
//!    accumulation buffer and initializes its fill position to `stagger` instead of `0`. This is
//!    causally valid: input before the stream's `t=0` is defined as silence, and a nonzero
//!    starting fill position is equivalent to having already "received" `stagger` samples of that
//!    pre-stream silence — see `fft_stage_process_sample`'s doc comment for the derivation showing
//!    this changes only *when* each partition's FFT fires, never the numerical result. D-9.5's
//!    test suite below exercises this directly (IR lengths chosen so some size-level group has
//!    more than one member).
//!
//! 3. **Untrusted input hardening**, in `wav.rs` (WAV parsing) rather than here: this crate parses
//!    real files from disk, unlike the spike's own trusted generated fixtures. See `wav.rs`'s
//!    module doc comment.
//!
//! The FFT-stage arithmetic itself (accumulate into a fixed-size buffer, trigger an R2C FFT and
//! spectral multiply against a precomputed `h` segment spectrum, C2R back and overlap-add into a
//! ring accumulator) and the head partition's direct time-domain convolution are otherwise
//! unchanged from the spike.
//!
//! # R-8, verified and tuned (M3, `docs/03-implementation-roadmap.md` §7)
//!
//! S-2 recorded staggering as *required follow-up*, not a proven fix — M2 built it in, but never
//! measured whether it actually closed the gap S-2 found (90-400% of a block's own period at
//! FR-IR-050's own minimum). R-8's M3 task was to run that measurement for real, against this
//! crate (not the spike's copy), via `examples/perf_sweep.rs` (S-2's comparative pass, reused)
//! and `examples/perf_bench.rs` (S-2's D-2.2-rigor confirmatory pass, reused).
//!
//! **Decision: M2's per-group stagger (`stagger = j * size / group_len`, `j` reset to 0 at every
//! group boundary) is replaced by a per-*size* stagger — one running counter shared by every
//! group at that nominal size, block-aligned (`stagger` is always a multiple of `block_size`).**
//!
//! **Rationale.** The measurement found M2's scheme only half-fixed the lockstep problem. Two
//! independent gaps, both visible once actually measured:
//! - *Not block-aligned.* A size-`P` partition's FFT triggers once every `P` samples; which
//!   *host block* (of `block_size` samples) that lands in depends only on the trigger's phase
//!   modulo `block_size`. `j * size / group_len` is not generally a multiple of `block_size` (it
//!   only happens to be, for `group_len <= growth_factor` at levels past the first, when
//!   `size / group_len` divides evenly — true for the shipped `growth_factor = 2` default at
//!   every level except the very first, where `size == block_size` and there is only one
//!   possible host block regardless, so staggering can't help there by construction).
//! - *Not per-size.* This was the real gap. Once a level's `size` reaches `max_partition`, growth
//!   stops (D-9.6) but the schedule keeps adding new groups at that same size for as long as the
//!   IR has taps left — a multi-second IR can have dozens of such groups. M2's `j` reset to 0 at
//!   every group boundary, so *every* group's first member got `stagger = 0` and every group's
//!   second got `stagger = size / 2` — collapsing what could have been `size / block_size`
//!   distinct host-block phases down to just 2, no matter how many groups piled up. Direct
//!   inspection of `build_schedule`'s own output confirmed this: a 10 s@192 kHz IR at
//!   `block_size = 32` produces 233 partitions at `size = max_partition = 8192` (256 possible
//!   phases), split M2-style into exactly 2 groups of ~117 — barely better than no staggering at
//!   all for that size level.
//!
//! **Measured impact of the fix, single-core-pinned, `growth_factor = 2` / `max_partition = 8192`
//! (this crate's shipped defaults), `decaying_noise` fixture (S-2's own choice: convolution cost
//! is a function of IR length, not tap values). *Hardware caveat, stated because it must be:*
//! measured on this task's sandbox (a 4-core Intel Xeon @ 2.10 GHz), **not**
//! `docs/02-architecture.md` §2's pinned reference machine (AMD Ryzen 9 5950X, 3.4 GHz base,
//! Windows 11) — these are directionally useful, and the *relative* before/after comparison below
//! (same machine, same code otherwise, only `build_schedule`'s stagger formula changed) is valid,
//! but neither figure is the certified NFR-PERF-010 number that requires the reference machine.*
//!
//! | Condition | Before (M2 stagger), p99.9 / max | After (this fix), p99.9 / max |
//! |---|---|---|
//! | NFR-PERF-010's own condition (48 kHz, 64-sample block, 2 s IR) | 337.7% / 602.5% | **16.8% / 41.3%** |
//! | FR-IR-050 floor (48 kHz, 32-sample block, 2 s IR) | 616.0% / 1290.7% | **30.7% / 70.4%** |
//! | 10 s@192 kHz IR, 1024-sample block | 111.1% / 137.3% | 67.4% / 81.8% |
//! | 10 s@192 kHz IR, 2048-sample block | 129.7% / 131.8% | 117.8% / 131.4% |
//!
//! (Each row re-run from `perf_bench.rs`, >= 200 repetitions of that combination's worst-tier
//! period per D-2.2's periodic-not-rare reasoning — see that file's module doc comment.)
//!
//! The fix closes the gap by roughly 15-20x at exactly the two conditions R-8 names
//! (NFR-PERF-010's own condition and FR-IR-050's floor), taking both from several times over a
//! full core's budget to comfortably under half of one core on this hardware. **The IR-stage
//! *scheduling defect* R-8 describes is closed** (the remaining gaps below are pre-existing,
//! separately-tracked cost characteristics, not this defect) — this does not by itself retire
//! R-8 or close NFR-PERF-010 as milestone risks, which per
//! `docs/03-implementation-roadmap.md` §7 requires the certified full-six-stage-chain benchmark
//! on the §2 reference machine, still outstanding.
//!
//! **What the fix does *not* close, recorded rather than glossed over:**
//! - **The single largest block size in the required matrix (2048) at the longest/highest-rate
//!   IRs stays just over budget** (117.8% p99.9 / 131.4% max at 192 kHz for a 10 s IR, barely
//!   moved by this fix — see the table). 1024-sample blocks *did* improve substantially (from
//!   111.1%/137.3% to 67.4%/81.8%, back under budget), so this is not simply "large blocks are
//!   untouched by staggering"; at 2048 specifically, the direct-convolution head partition's
//!   inherent `O(block_size^2)` cost (confirmed by measurement: an all-zero `delta` fixture costs
//!   the same as `decaying_noise` at a given block size, so tap content plays no role, only
//!   `block_size` does) has grown large enough to dominate the total regardless of how well the
//!   FFT partitions are staggered. This matches S-2's own framing (its README: "only at the
//!   large-block end... does the picture become merely over budget by a factor of 2-4") — a
//!   pre-existing, separately-tracked cost characteristic of the head partition, not something
//!   FFT-partition staggering could ever touch. Closing it, if ever required, is a different
//!   piece of work (vectorizing or restructuring the head partition), out of R-8's scope.
//! - **The smallest block size (32) at the highest rate (192 kHz) still shows an elevated `max`**
//!   (as high as several hundred percent of that combination's own ~166 ns block period across
//!   repeated runs) even though `p99.9` — the actual D-2.2 gate — stays near 120%. Two repeated
//!   measurements of the same combination gave `max` values of 335% and 510% while `p99.9` held
//!   at 118-123%, i.e. this is a single-sample noise/jitter signature (plausibly this sandbox's
//!   shared-CPU scheduling, not a reproducible periodic pileup), not evidence of a residual
//!   scheduling defect — but it is *not verified* to be pure measurement noise rather than a real
//!   tail effect, since a dedicated single-tenant reference machine wasn't available to confirm.
//!   A 32-sample block at 192 kHz is also, independent of this crate, an inherently tight budget
//!   (a 166 ns block period) that FR-IR-050 does not actually require (its Must floor is stated at
//!   48 kHz).
//!
//! **Alternative considered and not needed: amortizing each partition's FFT across several block
//! calls (splitting R2C/spectral-multiply/C2R into a state machine) instead of computing it
//! synchronously on the triggering block.** This was the task's other candidate direction, flagged
//! as justified only if block-aligned staggering still left a large gap at the grid's worst
//! corners. It didn't: the measured results above show the low-risk stagger-formula change alone
//! closes both of R-8's named conditions by more than an order of magnitude, so the bigger,
//! riskier amortization rework was not implemented.

use std::collections::HashMap;
use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use wide::f32x8;

use namir_core::SampleRate;

use crate::error_codes::IrLoadError;
use crate::wav;

/// `out[t] += w * in_[t]` for every `t`, vectorized 8 lanes at a time with a scalar remainder —
/// identical shape and rationale to `namir-nam/src/wavenet.rs`'s own `axpy` (this M3 close-out
/// pass's reference-machine benchmarking found the head partition's direct-convolution tap loop,
/// below, was the other hot unvectorized loop in the assembled six-stage chain once WaveNet's
/// own activations were fixed). Not shared as a `pub` item between the two crates for the same
/// reason this codebase's other small per-crate duplicates give (see e.g. `rt_harness`'s doc
/// comment in this crate/`namir-nam`): a five-line function isn't worth a shared crate or a
/// public API surface neither crate otherwise needs.
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

/// D-9.6's S-2-measured default growth factor.
pub const DEFAULT_GROWTH_FACTOR: usize = 2;
/// D-9.6's S-2-measured default maximum partition size, in samples.
pub const DEFAULT_MAX_PARTITION: usize = 8192;

/// D-9.7's 10-second ceiling, re-applied here at the *engine* rate after resampling. This is
/// independent of (not a substitute for) `wav.rs`'s own file-rate application of the same
/// ceiling: that one bounds `wav::decode`'s own allocation from an untrusted declared file size,
/// this one bounds the final tap count actually scheduled and convolved, which depends on the
/// caller's `engine_rate` and is only known after resampling.
const MAX_LOAD_SECONDS_AT_ENGINE_RATE: u64 = 10;

// ---------------------------------------------------------------------------------------------
// D-9.4 schedule
// ---------------------------------------------------------------------------------------------

/// One FFT-based partition: covers IR taps `[offset, offset + actual_len)`, using a nominal
/// block/FFT size of `size` (`actual_len <= size`; less only for the final partition covering a
/// tail shorter than a full partition). `stagger` is R-8's fix (see the module doc comment): the
/// number of samples of pre-stream silence this partition's input accumulator starts "already
/// having received", staggering its first FFT trigger relative to other partitions sharing the
/// same `size`.
#[derive(Debug, Clone, Copy)]
pub struct StageSpec {
    /// The IR tap this partition's `h` segment starts at.
    pub offset: usize,
    /// This partition's nominal FFT block size (also its input-accumulator capacity).
    pub size: usize,
    /// The number of real IR taps in this partition's `h` segment (`<= size`; less only for the
    /// final partition, whose `h` segment is shorter than a full `size`-length block).
    pub actual_len: usize,
    /// R-8's stagger: this partition's input accumulator starts as if it had already received
    /// this many samples of pre-stream silence (`< size`).
    pub stagger: usize,
}

/// Builds the D-9.4 non-uniform schedule for an IR of `ir_len` taps, with R-8's stagger (tuned
/// per the module doc comment's "R-8, verified and tuned (M3)" section) assigned to every
/// returned `StageSpec`.
///
/// The head (direct, time-domain) partition is `min(block_size, ir_len)` taps and is *not*
/// included in the returned list — [`PreparedIr`] handles it separately. Every returned
/// `StageSpec` is FFT-based.
///
/// `growth_factor == 1` degenerates to uniform partitioned convolution (every FFT partition is
/// `block_size`), useful as a schedule to compare against. `max_partition == block_size` also
/// degenerates to uniform, regardless of `growth_factor`.
///
/// **Causality**, ported from the spike's derivation: a size-`P` FFT partition at IR offset
/// `off` can only be computed once `P` samples of input feeding it have arrived, and its output
/// is due starting at time `off` relative to the start of that input window — so it is only
/// computable in time if `off >= P`. Growing the partition size by `growth_factor` after exactly
/// `growth_factor` partitions of the current size (not a fixed count) is what keeps that
/// invariant true at every size transition, by induction.
///
/// **R-8 stagger, block-aligned and per-size rather than per-group.** A size-`P` partition's FFT
/// triggers once every `P` samples; which *host block* (of `block_size` samples) that trigger
/// lands in only depends on the trigger's phase modulo `block_size` — so a stagger that is
/// itself a multiple of `block_size` deterministically controls which of the `P / block_size`
/// possible host blocks a partition's FFT lands in, spreading its cost as widely as it can go.
/// (A stagger that *isn't* block-aligned, M2's original scheme, only rearranges which sample
/// *within* a host block the FFT happens to fire on — it can't change which block bears the
/// cost, so it does nothing for the metric that matters.) The counter driving this is tracked
/// per nominal `size` across the *whole* schedule, not reset at each group boundary: this is
/// what spreads partitions across a size level's blocks not just within one `growth_factor`-
/// sized group, but also across the many separate groups a multi-second IR accumulates once
/// `size` reaches `max_partition` and stops growing — see the module doc comment's measurement
/// for why that inter-group case, not the intra-group one, was the fix's actual target.
pub fn build_schedule(
    ir_len: usize,
    block_size: usize,
    growth_factor: usize,
    max_partition: usize,
) -> Vec<StageSpec> {
    assert!(block_size > 0 && growth_factor >= 1 && max_partition >= block_size);
    let head = block_size.min(ir_len);
    let per_level = growth_factor.max(1);

    // --- Pass 1: enumerate the partitions themselves. Stagger is deliberately left 0 here and
    // assigned in pass 2, which needs each size's *finished* population count -- see that pass
    // for why a single streaming pass cannot compute the same assignment.
    let mut stages = Vec::new();
    let mut offset = head;
    let mut size = block_size;
    // The distinct nominal sizes, in the order they first appear (ascending, and contiguous once
    // `size` saturates at `max_partition`). A size's index here is its "level", used below.
    let mut size_order: Vec<usize> = Vec::new();
    while offset < ir_len {
        for _ in 0..per_level {
            if offset >= ir_len {
                break;
            }
            if size_order.last() != Some(&size) {
                size_order.push(size);
            }
            stages.push(StageSpec {
                offset,
                size,
                actual_len: (ir_len - offset).min(size),
                stagger: 0,
            });
            offset += size;
        }
        if size < max_partition && growth_factor > 1 {
            size = (size * growth_factor).min(max_partition);
        }
    }

    // --- Pass 2: assign staggers, decorrelating in two independent dimensions.
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for s in &stages {
        *counts.entry(s.size).or_insert(0) += 1;
    }
    let mut assigned: HashMap<usize, usize> = HashMap::new();
    for spec in stages.iter_mut() {
        // >= 1: size >= block_size always (size starts at block_size and only grows), so this is
        // never a divide-by-zero.
        let num_phases = (spec.size / block_size).max(1);
        let count = counts[&spec.size];
        let i = assigned.entry(spec.size).or_insert(0);

        // (a) WITHIN a size: spread this size's members across its available phases. When the
        // size has no more members than phases, spread them *evenly* across the whole period
        // (`i * num_phases / count`) rather than packing them onto consecutive phases as the
        // previous `i % num_phases` did -- consecutive packing left the ten max_partition-sized
        // partitions of a 2 s IR firing on ten adjacent host blocks, a long contiguous burst
        // rather than a spread. When members outnumber phases, fall back to round-robin, which
        // is exactly balanced (at most `ceil(count / num_phases)` on any phase) and is what the
        // wraparound case actually wants.
        let within = if count <= num_phases {
            (*i * num_phases) / count
        } else {
            *i % num_phases
        };
        *i += 1;

        // (b) ACROSS sizes: shift every size's phase assignment by its own level index.
        //
        // Without this shift, each size's phase-0 member lands on the *last* host block of its
        // own period: a size-`P` partition with `stagger == 0` first fires at absolute sample
        // `P - 1`, i.e. host block `P / block_size - 1`. Since every size's period (in blocks)
        // divides the largest size's period, all of those "last block of my period" positions
        // coincide on one single host block, which therefore collected one partition of *every*
        // size simultaneously -- measured at the NFR-PERF-010 condition (2 s IR, 64-sample
        // block, growth_factor 2, max_partition 8192) as a worst block carrying ~11.9x the mean
        // block's FFT work, with the largest partition contributing about half of it.
        //
        // This is not phase exhaustion: within each size the assignment above already uses
        // distinct, well-spread phases, and every size here has at least as many phases as
        // members. The defect was purely that all sizes' phase counters started at 0 and phase 0
        // maps to the same residue for every size. A per-level shift breaks that alignment with
        // no other effect -- it is a rotation of an already-valid assignment, so it preserves
        // both `stagger < size` and the round-robin balance property above.
        let level = size_order.iter().position(|&s| s == spec.size).unwrap_or(0);

        spec.stagger = ((within + level) % num_phases) * block_size;
    }
    stages
}

// ---------------------------------------------------------------------------------------------
// FFT-stage machinery: immutable (Prepared) vs. mutable (State)
// ---------------------------------------------------------------------------------------------

/// FFT machinery for one partition size, cached so multiple `StageSpec`s at the same nominal
/// `size` (there are always >= 1, often `growth_factor`, per level) share one plan.
struct FftPlan {
    fft_len: usize,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
}

/// Immutable per-partition FFT data: the precomputed spectrum of this partition's `h` segment,
/// plus the plan it was built with. `Sync` (all fields are), shareable via `Arc<PreparedIr>`.
struct FftStageImmutable {
    offset: usize,
    size: usize,
    actual_len: usize,
    fft_len: usize,
    stagger: usize,
    h_spectrum: Vec<Complex32>,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
}

impl FftStageImmutable {
    fn new(spec: StageSpec, h: &[f32], plan: &FftPlan) -> Self {
        let mut time_buf = vec![0f32; plan.fft_len];
        time_buf[..spec.actual_len].copy_from_slice(&h[spec.offset..spec.offset + spec.actual_len]);
        let mut h_spectrum = plan.r2c.make_output_vec();
        plan.r2c
            .process(&mut time_buf, &mut h_spectrum)
            .expect("r2c on h segment");
        FftStageImmutable {
            offset: spec.offset,
            size: spec.size,
            actual_len: spec.actual_len,
            fft_len: plan.fft_len,
            stagger: spec.stagger,
            h_spectrum,
            r2c: Arc::clone(&plan.r2c),
            c2r: Arc::clone(&plan.c2r),
        }
    }
}

/// Mutable per-instance state for one FFT partition: the input accumulator and reusable scratch.
/// Never shared across instances.
struct FftStageState {
    in_buf: Vec<f32>,
    in_pos: usize,
    time_scratch: Vec<f32>,
    freq_scratch: Vec<Complex32>,
}

impl FftStageState {
    /// R-8: `in_buf` is zero-initialized (representing pre-stream silence) and `in_pos` starts at
    /// `stage.stagger` rather than `0` — see the module doc comment and
    /// `fft_stage_process_sample`'s doc comment for why this is causally valid.
    fn new(stage: &FftStageImmutable) -> Self {
        FftStageState {
            in_buf: vec![0f32; stage.size],
            in_pos: stage.stagger,
            time_scratch: vec![0f32; stage.fft_len],
            freq_scratch: stage.r2c.make_output_vec(),
        }
    }
}

/// Feeds one new input sample (the real stream sample at absolute time `t_abs`; `t_abs` counts
/// only real samples, unaffected by staggering) into one FFT stage, triggering its FFT and
/// overlap-adding into `ring` when its accumulator fills.
///
/// **Why the R-8 stagger doesn't change the numerical result:** let `T0` be the absolute (signed)
/// time of `in_buf[0]`. In the unstaggered case `T0 = t_abs - size + 1 >= 0` always. With a
/// nonzero `stagger`, on this stage's *first* trigger only, `in_buf[0..stagger)` hold pre-stream
/// silence at virtual times `[-stagger, -1)` and `in_buf[stagger..size)` hold real samples
/// `x[0..size-stagger)`, so `T0 = -stagger` for that one trigger (still exactly `t_abs - size + 1`,
/// with `t_abs = size - stagger - 1` — the same formula, just possibly negative). Every later
/// trigger for this stage resets `in_buf` to a normal, all-real window, so `T0 >= 0` from then on.
/// Standard convolution algebra (`y_local[i]` contributes to absolute output time `T0 + offset +
/// i`, independent of everything else) shows this shifted `T0` is exactly correct — not an
/// approximation — because `in_buf[0..stagger)` genuinely *is* the correct value of the input at
/// those virtual times (silence, by the definition of what "before the stream starts" means).
/// `start_abs` is computed in `i64` (not `u64`) so a negative `T0 + offset` on that first trigger
/// doesn't panic on subtraction underflow; any resulting negative output-time index is skipped
/// (skipping is exact, not an approximation: `y_local[i]` for `i` small enough to map to a
/// negative time is a convolution of only the zero-valued prefix of `in_buf`, i.e. mathematically
/// zero in exact arithmetic).
#[inline]
fn fft_stage_process_sample(
    stage: &FftStageImmutable,
    state: &mut FftStageState,
    x: f32,
    t_abs: u64,
    ring: &mut [f32],
) {
    state.in_buf[state.in_pos] = x;
    state.in_pos += 1;
    if state.in_pos < stage.size {
        return;
    }
    state.in_pos = 0;

    state.time_scratch[..stage.size].copy_from_slice(&state.in_buf);
    for v in &mut state.time_scratch[stage.size..] {
        *v = 0.0;
    }
    stage
        .r2c
        .process(&mut state.time_scratch, &mut state.freq_scratch)
        .expect("r2c");
    for (f, h) in state.freq_scratch.iter_mut().zip(stage.h_spectrum.iter()) {
        *f *= h;
    }
    stage
        .c2r
        .process(&mut state.freq_scratch, &mut state.time_scratch)
        .expect("c2r");

    let start_abs: i64 = t_abs as i64 + 1 - stage.size as i64 + stage.offset as i64;
    let valid_len = stage.size + stage.actual_len - 1;
    let scale = 1.0 / stage.fft_len as f32; // realfft's inverse transform is unnormalized
    let ring_len = ring.len() as i64;
    for i in 0..valid_len {
        let real_time = start_abs + i as i64;
        if real_time < 0 {
            continue; // exactly-zero contribution (see doc comment above) -- skip, don't wrap.
        }
        let pos = (real_time % ring_len) as usize;
        ring[pos] += state.time_scratch[i] * scale;
    }
}

// ---------------------------------------------------------------------------------------------
// Per-channel Prepared / State
// ---------------------------------------------------------------------------------------------

/// One channel's immutable convolution setup: the direct-convolution head partition plus every
/// FFT stage's precomputed spectrum.
struct PreparedChannel {
    head: Vec<f32>,
    stages: Vec<FftStageImmutable>,
    ring_len: usize,
    /// The host block size this channel was prepared for (D-9.4: equals the head partition's own
    /// intended size, though `head.len()` itself can be shorter when `ir_len < block_size`). The
    /// only bound `process_block` relies on: every call's `input.len()` must be `<= block_size`,
    /// exactly as `head_scratch`'s fixed size below assumes.
    block_size: usize,
}

/// One channel's mutable runtime state.
struct ChannelState {
    /// The last `head.len().saturating_sub(1)` samples before the current block, oldest first —
    /// `namir-nam/src/wavenet.rs`'s `Conv1D` history convention, not a modulo-indexed ring buffer
    /// (see `process_block`'s doc comment for why this changed from the original ring-buffer
    /// scheme).
    head_history: Vec<f32>,
    /// Scratch: `head_history ++ this block's input`, length `head_history.len() + block_size`,
    /// sized once and reused every call (no per-call allocation).
    head_scratch: Vec<f32>,
    stage_states: Vec<FftStageState>,
    ring: Vec<f32>,
    t: u64,
}

impl PreparedChannel {
    fn new(h: &[f32], block_size: usize, growth_factor: usize, max_partition: usize) -> Self {
        let ir_len = h.len();
        let head_len = block_size.min(ir_len);
        let schedule = build_schedule(ir_len, block_size, growth_factor, max_partition);

        let mut planner = RealFftPlanner::<f32>::new();
        let mut plans: HashMap<usize, FftPlan> = HashMap::new();
        let mut stages = Vec::with_capacity(schedule.len());
        for spec in &schedule {
            let fft_len = 2 * spec.size;
            let plan = plans.entry(fft_len).or_insert_with(|| FftPlan {
                fft_len,
                r2c: planner.plan_fft_forward(fft_len),
                c2r: planner.plan_fft_inverse(fft_len),
            });
            stages.push(FftStageImmutable::new(*spec, h, plan));
        }

        let max_reach = schedule
            .iter()
            .map(|s| s.offset + 2 * s.size)
            .max()
            .unwrap_or(head_len);
        let ring_len = (ir_len + 2 * block_size)
            .max(max_reach)
            .max(head_len)
            .next_power_of_two();

        PreparedChannel {
            head: h[..head_len].to_vec(),
            stages,
            ring_len,
            block_size,
        }
    }

    fn new_state(&self) -> ChannelState {
        let history_len = self.head.len().saturating_sub(1);
        ChannelState {
            head_history: vec![0f32; history_len],
            head_scratch: vec![0f32; history_len + self.block_size],
            stage_states: self.stages.iter().map(FftStageState::new).collect(),
            ring: vec![0f32; self.ring_len],
            t: 0,
        }
    }

    /// Ported near-verbatim from the spike's `PartitionedConvolver::process_block`, except for
    /// the head partition's own tap loop (see below). Allocates nothing.
    ///
    /// Panics if `input.len()` exceeds this channel's prepared `block_size` — a call-site
    /// programming error (the caller must size blocks to at most the value it originally passed
    /// to `PreparedChannel::new`/`PreparedIr::from_wav_bytes`), same contract and rationale as
    /// `namir-nam`'s `PreparedWaveNet::process_block`.
    ///
    /// **Head partition: vectorized block-at-a-time, not per-sample-with-modulo.** This M3
    /// close-out pass's reference-machine benchmarking found the original per-sample loop —
    /// `y += head[k] * head_history[(t - k) % head_len]`, a `head_len`-deep scalar loop *with a
    /// modulo per tap* run once per sample — was a second major unvectorized cost alongside
    /// `namir-nam`'s WaveNet activations (see that crate's `wavenet.rs` for the matching fix).
    /// The replacement builds `padded = head_history ++ input` once per block (the same
    /// history-plus-input-window technique `namir-nam/src/wavenet.rs`'s `Conv1D::apply_into`
    /// already uses) and, for each tap `k`, accumulates `output[i] += head[k] *
    /// padded[history_len - k + i]` for the whole block via [`axpy`] — `head_len` vectorized
    /// passes over the block instead of `head_len * block_size` scalar multiply-and-modulo steps.
    /// `padded[history_len - k + i] == x` at block-local time `i - k`, matching the original
    /// tap's `x[t - k]` exactly (`history_len - k >= 0` always, since `k < head_len =
    /// history_len + 1`); a zero-initialized `head_history` at stream start reproduces the
    /// original loop's explicit `dt > t` early-break for "no signal before t=0" without needing
    /// the special case, since multiplying a still-zero history sample by `head[k]` is a no-op
    /// either way.
    fn process_block(&self, state: &mut ChannelState, input: &[f32], output: &mut [f32]) {
        let n = input.len();
        assert!(
            n <= self.block_size,
            "block size {n} exceeds this channel's prepared block_size {}",
            self.block_size
        );
        let head_len = self.head.len();
        let history_len = state.head_history.len();

        if head_len > 0 {
            let padded = &mut state.head_scratch[..history_len + n];
            padded[..history_len].copy_from_slice(&state.head_history);
            padded[history_len..].copy_from_slice(input);

            output[..n].fill(0.0);
            for k in 0..head_len {
                let w = self.head[k];
                if w == 0.0 {
                    continue;
                }
                let offset = history_len - k;
                axpy(&mut output[..n], &padded[offset..offset + n], w);
            }

            state.head_history.copy_from_slice(&padded[n..]);
        } else {
            output[..n].fill(0.0);
        }

        let ring_len = state.ring.len() as u64;
        for i in 0..n {
            let x = input[i];
            let t = state.t;

            let pos = (t % ring_len) as usize;
            output[i] += state.ring[pos];
            state.ring[pos] = 0.0;

            for (stage, st) in self.stages.iter().zip(state.stage_states.iter_mut()) {
                fft_stage_process_sample(stage, st, x, t, &mut state.ring);
            }

            state.t += 1;
        }
    }
}

/// D-9.7's 10-second ceiling, re-applied at `engine_hz` to every channel's tap array in place.
/// Returns `true` if any channel was truncated.
///
/// **Why this check exists as its own function, independently testable from `wav::decode`'s own
/// file-rate ceiling:** in the normal `from_wav_bytes` path, resampling preserves real-time
/// duration (a signal capped at 10 s at its file rate resamples to at most 10 s of frames at any
/// engine rate, since `new_len = round(file_frames * engine_hz / file_hz) <=
/// round(10 * file_hz * engine_hz / file_hz) = 10 * engine_hz`), so this second application of
/// the ceiling is not reachable through that path alone — it is a defense-in-depth re-check per
/// this crate's build instructions, not a check for a case that path can currently trigger. This
/// function is exercised directly (bypassing `wav::decode`/resampling) in this module's tests for
/// exactly that reason.
fn truncate_to_engine_ceiling(channel_taps: &mut [Vec<f32>], engine_hz: u32) -> bool {
    let max_engine_frames = MAX_LOAD_SECONDS_AT_ENGINE_RATE as usize * engine_hz as usize;
    let mut truncated = false;
    for taps in channel_taps.iter_mut() {
        if taps.len() > max_engine_frames {
            taps.truncate(max_engine_frames);
            truncated = true;
        }
    }
    truncated
}

// ---------------------------------------------------------------------------------------------
// Public PreparedIr / IrState
// ---------------------------------------------------------------------------------------------

/// Immutable, `Sync` convolver setup for one loaded impulse response (D-9.1 / D-8.2's split, see
/// the module doc comment). Holds one channel's worth of `{head taps, FFT partition
/// specs+spectra}` for a mono IR, or two independent such sets for a stereo IR (FR-CHAIN-060).
pub struct PreparedIr {
    channels: Vec<PreparedChannel>,
    len_samples: usize,
    was_truncated: bool,
}

/// Per-instance mutable convolution state: one `{ring buffers, in_pos, t}` set per
/// [`PreparedIr`] channel. Never shared across instances.
pub struct IrState {
    channels: Vec<ChannelState>,
}

impl PreparedIr {
    /// Loads a WAV impulse response from `bytes`, resamples it to `engine_rate` if its native
    /// rate differs (FR-IR-030, via `rubato` configured to the same quality bar D-9.3 sets for
    /// FR-NAM-060 — see `resample_mono`'s doc comment), truncates at D-9.7's 10-second-at-engine-
    /// rate ceiling, and builds the D-9.4 schedule with R-8's staggering baked in, using this
    /// crate's [`DEFAULT_GROWTH_FACTOR`] / [`DEFAULT_MAX_PARTITION`].
    pub fn from_wav_bytes(
        bytes: &[u8],
        engine_rate: SampleRate,
        block_size: usize,
    ) -> Result<Self, IrLoadError> {
        Self::from_wav_bytes_with_schedule(
            bytes,
            engine_rate,
            block_size,
            DEFAULT_GROWTH_FACTOR,
            DEFAULT_MAX_PARTITION,
        )
    }

    /// As [`Self::from_wav_bytes`], but with an explicit `growth_factor`/`max_partition` instead
    /// of this crate's defaults — kept public so correctness tests (here and downstream) can
    /// exercise multiple schedules, mirroring the spike's own `verify.rs` approach.
    pub fn from_wav_bytes_with_schedule(
        bytes: &[u8],
        engine_rate: SampleRate,
        block_size: usize,
        growth_factor: usize,
        max_partition: usize,
    ) -> Result<Self, IrLoadError> {
        let decoded = wav::decode(bytes)?;
        let mut was_truncated = decoded.was_truncated;

        let mut channel_taps: Vec<Vec<f32>> = Vec::with_capacity(decoded.channel_data.len());
        for ch in decoded.channel_data {
            let taps = if decoded.sample_rate == engine_rate.hz() {
                ch
            } else {
                resample_mono(&ch, decoded.sample_rate, engine_rate.hz())
            };
            channel_taps.push(taps);
        }

        was_truncated |= truncate_to_engine_ceiling(&mut channel_taps, engine_rate.hz());

        let len_samples = channel_taps.first().map(Vec::len).unwrap_or(0);

        let channels = channel_taps
            .into_iter()
            .map(|taps| PreparedChannel::new(&taps, block_size, growth_factor, max_partition))
            .collect();

        Ok(PreparedIr {
            channels,
            len_samples,
            was_truncated,
        })
    }

    /// `1` for a mono IR, `2` for a stereo IR (FR-CHAIN-060's "stereo IR, or dual mono IR" — dual
    /// mono duplication of a single mono convolver's output is the engine `Ir` stage's job, not
    /// this crate's; see the crate doc comment).
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// D-9.7: "truncation... reported". `true` if the source WAV's declared duration exceeded the
    /// 10-second ceiling at its own file rate, or if the resampled-to-engine-rate tap count
    /// exceeded the same ceiling at the engine rate.
    pub fn was_truncated(&self) -> bool {
        self.was_truncated
    }

    /// Impulse response length in taps at the engine rate, after truncation. The engine `Ir`
    /// stage uses this as its own `Stage::tail_samples()`.
    pub fn len_samples(&self) -> usize {
        self.len_samples
    }

    /// D-9.4: this convolver's head partition equals the host block size, so it introduces zero
    /// added latency by construction.
    pub fn latency_samples(&self) -> u32 {
        0
    }

    /// Allocates fresh per-instance runtime state. Not RT-safe — call this off the audio thread,
    /// mirroring `PreparedNam::new_state`.
    pub fn new_state(&self) -> IrState {
        IrState {
            channels: self
                .channels
                .iter()
                .map(PreparedChannel::new_state)
                .collect(),
        }
    }

    /// The allocation-free RT path. `input` is one mono signal — the engine's `Ir` stage always
    /// feeds the same mono post-NAM signal into every IR channel; true stereo width comes from
    /// the IR's own taps differing per channel, not from a stereo input at this point in the
    /// chain. `outputs.len()` must equal [`Self::channel_count`].
    ///
    /// Panics (a call-site programming error, same contract as `PreparedNam::process_block`) if
    /// any output slice's length differs from `input.len()`, or if `outputs.len() !=
    /// self.channel_count()`.
    pub fn process_block(&self, state: &mut IrState, input: &[f32], outputs: &mut [&mut [f32]]) {
        assert_eq!(
            outputs.len(),
            self.channels.len(),
            "outputs.len() ({}) must equal channel_count() ({})",
            outputs.len(),
            self.channels.len()
        );
        for out in outputs.iter() {
            assert_eq!(
                out.len(),
                input.len(),
                "output slice length ({}) must equal input length ({})",
                out.len(),
                input.len()
            );
        }
        for ((chan, cstate), out) in self
            .channels
            .iter()
            .zip(state.channels.iter_mut())
            .zip(outputs.iter_mut())
        {
            chan.process_block(cstate, input, out);
        }
    }
}

/// D-9.5's permanent reference: full time-domain convolution, no partitioning, no FFT.
/// Deliberately naive — its only job is to be obviously correct. Ported verbatim from the spike.
pub fn direct_convolve(h: &[f32], x: &[f32]) -> Vec<f32> {
    let mut y = vec![0f32; x.len()];
    for n in 0..x.len() {
        let kmax = h.len().min(n + 1);
        let mut acc = 0f64; // f64 accumulation so the reference isn't the noisier of the two
        for k in 0..kmax {
            acc += h[k] as f64 * x[n - k] as f64;
        }
        y[n] = acc as f32;
    }
    y
}

// ---------------------------------------------------------------------------------------------
// Resampling (D-9.3's quality bar, FR-IR-030)
// ---------------------------------------------------------------------------------------------

/// Resamples one mono signal from `from_hz` to `to_hz` using `rubato`'s `SincFixedIn`, configured
/// as a high-quality sinc resampler (`sinc_len = 256`, `BlackmanHarris2` window — good rolloff
/// and the best stopband attenuation `rubato` offers, per `rubato::WindowFunction`'s own doc
/// comments — `oversampling_factor = 256`, cubic interpolation between sinc-interpolated points,
/// and `f_cutoff` computed by `rubato::calculate_cutoff` for this `sinc_len`/window pair rather
/// than guessed), following the offline-clip recipe `rubato`'s own README documents: feed fixed-
/// size chunks through `process_into_buffer`, flush the resampler's reported `output_delay` via
/// `process_partial_into_buffer`, then drop the leading `output_delay` output frames and truncate
/// to the exact expected output length.
///
/// This is not a substitute for a direct frequency-response measurement against D-9.3's quality
/// bar (>= 100 dB stopband, <= 0.1 dB ripple to 20 kHz) — see this crate's test suite for what
/// coverage exists and its module doc comment's note on scope.
fn resample_mono(input: &[f32], from_hz: u32, to_hz: u32) -> Vec<f32> {
    use rubato::{
        Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
        calculate_cutoff,
    };

    if input.is_empty() || from_hz == to_hz {
        return input.to_vec();
    }

    let ratio = to_hz as f64 / from_hz as f64;
    let new_length = ((input.len() as f64) * ratio).round() as usize;

    let sinc_len = 256;
    let window = WindowFunction::BlackmanHarris2;
    let f_cutoff = calculate_cutoff(sinc_len, window);
    let params = SincInterpolationParameters {
        sinc_len,
        f_cutoff,
        oversampling_factor: 256,
        interpolation: SincInterpolationType::Cubic,
        window,
    };
    let chunk_size = 1024.min(input.len());
    let mut resampler = SincFixedIn::<f32>::new(ratio, 1.0, params, chunk_size.max(1), 1)
        .expect("ratio > 0 and max_relative_ratio == 1.0 are always valid construction inputs");

    let delay = resampler.output_delay();
    let mut out: Vec<f32> = Vec::with_capacity(new_length + delay + chunk_size);
    let mut outbuf = resampler.output_buffer_allocate(true);

    let mut pos = 0usize;
    loop {
        let need = resampler.input_frames_next();
        if input.len() - pos < need {
            break;
        }
        let chunk = [&input[pos..pos + need]];
        let (n_in, n_out) = resampler
            .process_into_buffer(&chunk, &mut outbuf, None)
            .expect("resample process_into_buffer");
        out.extend_from_slice(&outbuf[0][..n_out]);
        pos += n_in;
    }
    if pos < input.len() {
        let chunk = [&input[pos..]];
        let (_, n_out) = resampler
            .process_partial_into_buffer(Some(&chunk), &mut outbuf, None)
            .expect("resample process_partial_into_buffer");
        out.extend_from_slice(&outbuf[0][..n_out]);
    }
    // Flush any frames still held in the resampler's internal delay line.
    while out.len() < new_length + delay {
        let (_, n_out) = resampler
            .process_partial_into_buffer::<&[f32], Vec<f32>>(None, &mut outbuf, None)
            .expect("resample flush");
        if n_out == 0 {
            break;
        }
        out.extend_from_slice(&outbuf[0][..n_out]);
    }

    if out.len() > delay {
        out.drain(0..delay);
    } else {
        out.clear();
    }
    out.resize(new_length, 0.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_fixtures::ir::{decaying_noise, delayed_delta, delta, minimum_phase_lowpass};

    // -------------------------------------------------------------------------------------
    // Schedule tests, ported from the spike plus new stagger-specific coverage.
    // -------------------------------------------------------------------------------------

    #[test]
    fn schedule_is_causal() {
        for growth_factor in [2usize, 3, 4, 8] {
            for max_partition in [256usize, 1024, 4096] {
                let block_size = 64;
                if max_partition < block_size {
                    continue;
                }
                let stages = build_schedule(200_000, block_size, growth_factor, max_partition);
                for s in &stages {
                    assert!(
                        s.offset >= s.size,
                        "g={growth_factor} max={max_partition}: offset {} < size {} (would need future input)",
                        s.offset,
                        s.size
                    );
                }
            }
        }
    }

    #[test]
    fn schedule_covers_ir_exactly_once() {
        let ir_len = 10_007; // deliberately not a multiple of anything
        let stages = build_schedule(ir_len, 64, 2, 1024);
        let mut covered = vec![false; ir_len];
        let head = 64usize.min(ir_len);
        for c in covered.iter_mut().take(head) {
            *c = true;
        }
        for s in &stages {
            for (i, c) in covered
                .iter_mut()
                .enumerate()
                .skip(s.offset)
                .take(s.actual_len)
            {
                assert!(!*c, "tap {i} covered twice");
                *c = true;
            }
        }
        assert!(covered.iter().all(|&c| c), "some tap never covered");
    }

    #[test]
    fn stagger_is_zero_for_a_size_level_with_only_one_partition() {
        // growth_factor = 1: uniform, every level has exactly one partition.
        let stages = build_schedule(10_000, 64, 1, 8192);
        assert!(stages.iter().all(|s| s.stagger == 0));
    }

    #[test]
    fn the_block_size_level_cannot_be_staggered_and_correctly_reports_zero() {
        // At size == block_size, a partition's FFT triggers exactly once per host block no
        // matter what -- there is only one possible host block per period (period_blocks =
        // size / block_size = 1), so there is nothing to spread. This is a real constraint, not
        // a bug: verify it's reported honestly as stagger == 0 rather than the old scheme's
        // cosmetic within-block-only offset (which changed *when inside a block* the FFT ran
        // but never *which block*, i.e. never touched the metric R-8 actually cares about).
        let stages = build_schedule(1_000, 64, 2, 8192);
        let first_level: Vec<&StageSpec> = stages.iter().filter(|s| s.size == 64).collect();
        assert_eq!(
            first_level.len(),
            2,
            "expected a full 2-member group at size 64"
        );
        assert!(first_level.iter().all(|s| s.stagger == 0));
    }

    #[test]
    fn stagger_spreads_across_a_multi_member_group_once_period_blocks_exceeds_one() {
        // growth_factor = 2, IR long enough that the size-128 level (one level past block_size,
        // so period_blocks = 128/64 = 2 -- two host blocks actually exist to spread across) has
        // 2 full members.
        //
        // Asserted as a *property* (the two members occupy the two distinct block-aligned phases)
        // rather than as literal values. An earlier revision pinned `level[0].stagger == 0` and
        // `level[1].stagger == 64` exactly; the cross-size decorrelation shift added to
        // `build_schedule` (see its pass-2 comment (b)) rotates each size's assignment by its own
        // level index, which for this 2-phase level swaps which member gets which phase. That
        // rotation is the entire point of the fix and changes nothing this test actually cares
        // about -- both phases are still used, exactly once each -- so the assertion is written
        // against the invariant instead of against one particular rotation of it.
        let stages = build_schedule(1_000, 64, 2, 8192);
        let level: Vec<&StageSpec> = stages.iter().filter(|s| s.size == 128).collect();
        assert_eq!(level.len(), 2, "expected a full 2-member group at size 128");
        assert_ne!(
            level[0].stagger, level[1].stagger,
            "the two members must not share a phase"
        );
        let mut phases: Vec<usize> = level.iter().map(|s| s.stagger).collect();
        phases.sort_unstable();
        assert_eq!(
            phases,
            vec![0, 64],
            "both block-aligned phases of a 2-phase level should be used exactly once"
        );
    }

    #[test]
    fn stagger_spreads_across_repeated_groups_at_the_max_partition_ceiling() {
        // R-8's actual measured gap (see convolver.rs's module doc comment, "R-8, verified and
        // tuned"): once `size` reaches `max_partition` it stops growing, so a long IR produces
        // many separate groups all sharing that one nominal size, which M2's per-group-only
        // stagger (reset to {0, size/2} at every group boundary) collapsed onto just 2 host
        // blocks no matter how many groups piled up. Confirms the fix: staggers at this size
        // level now spread across far more than 2 distinct phases, and no phase is loaded with
        // more than ceil(n_partitions / period_blocks) partitions.
        //
        // `ir_len` here is deliberately past D-9.7's real 10-second-at-engine-rate ceiling
        // (`build_schedule` itself has no such cap -- that's applied elsewhere, by
        // `truncate_to_engine_ceiling`, before a real IR ever reaches this function): the point
        // is to comfortably exceed `period_blocks` partitions at the ceiling size so the
        // round-robin wraparound path is actually exercised, not to claim IRs this long ship.
        let ir_len = 3_000_000;
        let block_size = 32;
        let stages = build_schedule(
            ir_len,
            block_size,
            DEFAULT_GROWTH_FACTOR,
            DEFAULT_MAX_PARTITION,
        );
        let ceiling: Vec<&StageSpec> = stages
            .iter()
            .filter(|s| s.size == DEFAULT_MAX_PARTITION)
            .collect();
        let period_blocks = DEFAULT_MAX_PARTITION / block_size;
        assert!(
            ceiling.len() > period_blocks,
            "test assumption: this grid point needs more max_partition-sized partitions ({}) \
             than period_blocks ({period_blocks}) for the round-robin wraparound case to be \
             exercised at all",
            ceiling.len()
        );
        let mut counts: HashMap<usize, usize> = HashMap::new();
        for s in &ceiling {
            *counts.entry(s.stagger).or_insert(0) += 1;
        }
        assert!(
            counts.len() > 2,
            "expected staggers spread across far more than the old scheme's 2 phases, got {} \
             distinct phases across {} partitions: {counts:?}",
            counts.len(),
            ceiling.len()
        );
        let max_per_phase = counts.values().copied().max().unwrap_or(0);
        let expected_max_per_phase = ceiling.len().div_ceil(period_blocks);
        assert_eq!(
            max_per_phase, expected_max_per_phase,
            "round-robin should load at most ceil(n/period_blocks) partitions onto any one phase"
        );
    }

    /// R-8 regression guard, quantitative rather than structural: no single host block may carry
    /// a wildly disproportionate share of the schedule's total FFT work.
    ///
    /// Every other stagger test above checks a *structural* property (phases are distinct, are
    /// block-aligned, are balanced round-robin). None of them would have caught the cross-size
    /// alignment defect `build_schedule`'s pass-2 comment (b) describes, because that defect
    /// violated no structural property: within every size the phases were distinct, spread and
    /// balanced. It only showed up once the sizes were considered *together*, as a pileup on one
    /// host block. This test is the one that would have caught it, so it exists permanently.
    ///
    /// The model here is deliberately simple and self-contained (no measurement, no dependency on
    /// this machine): a size-`P` partition triggers its FFT every `P` samples, first at absolute
    /// sample `P - stagger - 1`, and costs `2P * log2(2P)` -- the standard real-FFT operation
    /// count, used only to weight partitions against each other, so its absolute scale is
    /// irrelevant. What is asserted is the *ratio* of the worst host block's weighted load to the
    /// mean, over one full schedule period.
    #[test]
    fn no_single_host_block_carries_a_disproportionate_share_of_fft_work() {
        // NFR-PERF-010's own literal condition: 2 s IR at 48 kHz, 64-sample host block, this
        // crate's shipped schedule defaults.
        let block_size = 64;
        let stages = build_schedule(
            96_000,
            block_size,
            DEFAULT_GROWTH_FACTOR,
            DEFAULT_MAX_PARTITION,
        );
        let largest = stages.iter().map(|s| s.size).max().expect("non-empty");
        let period_blocks = largest / block_size;

        let mut load = vec![0f64; period_blocks];
        for s in &stages {
            let weight = 2.0 * s.size as f64 * ((2 * s.size) as f64).log2();
            let first_block = (s.size - s.stagger - 1) / block_size;
            let stride = (s.size / block_size).max(1);
            let mut b = first_block % period_blocks;
            // Walk this partition's whole orbit within one period.
            for _ in 0..(period_blocks / stride).max(1) {
                load[b] += weight;
                b = (b + stride) % period_blocks;
            }
        }

        let total: f64 = load.iter().sum();
        let mean = total / period_blocks as f64;
        let worst = load.iter().copied().fold(f64::MIN, f64::max);
        let ratio = worst / mean;
        println!("worst-host-block / mean FFT load: {ratio:.3}x (period {period_blocks} blocks)");

        // The floor is set by physics, not by scheduling: the single largest partition's FFT is
        // atomic -- it cannot be split across host blocks by any stagger -- so some block must
        // carry it, and at this condition that alone is ~6.5x the mean. The bound below leaves
        // headroom above that floor while still failing loudly if the cross-size alignment
        // defect (measured at ~11.9x) is ever reintroduced.
        assert!(
            ratio < 8.0,
            "worst host block carries {ratio:.2}x the mean block's FFT work (bound: 8.0). \
             Load profile: {load:?}"
        );
    }

    #[test]
    fn stagger_is_always_less_than_size() {
        for growth_factor in [2usize, 3, 4, 8] {
            let stages = build_schedule(200_000, 64, growth_factor, 8192);
            for s in &stages {
                assert!(
                    s.stagger < s.size,
                    "stagger {} >= size {}",
                    s.stagger,
                    s.size
                );
            }
        }
    }

    // -------------------------------------------------------------------------------------
    // D-9.5 verification: PreparedIr::process_block vs. direct_convolve. Permanent, not a
    // one-off -- this is what proves R-8's staggering didn't silently break correctness.
    // -------------------------------------------------------------------------------------

    /// RMS(a - b) in dB below RMS(a). `None` if `a` is silent. Ported from the spike's
    /// `rms_error_db`.
    fn rms_error_db(a: &[f32], b: &[f32]) -> Option<f64> {
        let n = a.len().min(b.len());
        let rms = |s: &[f32]| -> f64 {
            if s.is_empty() {
                return 0.0;
            }
            (s.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / s.len() as f64).sqrt()
        };
        let ref_rms = rms(&a[..n]);
        if ref_rms <= 0.0 {
            return None;
        }
        let diff: Vec<f32> = (0..n).map(|i| a[i] - b[i]).collect();
        Some(20.0 * (rms(&diff) / ref_rms).log10())
    }

    /// A small deterministic xorshift-based white noise generator for driving test input
    /// signals. Not a `namir-fixtures` fixture: it's a *test input signal*, not an IR under test.
    fn white_noise(len: usize, seed: u64) -> Vec<f32> {
        let mut state = seed.max(1);
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                // Top 24 bits -> roughly uniform in [-0.8, 0.8].
                let top24 = (state >> 40) as u32;
                let unit = top24 as f64 / (1u32 << 24) as f64;
                (unit * 1.6 - 0.8) as f32
            })
            .collect()
    }

    /// Runs `h` through `PreparedIr`'s partitioned path (built directly from an in-memory IR via
    /// `PreparedChannel`, bypassing WAV decode/resample entirely, mirroring the spike's own
    /// `PartitionedConvolver::new` test usage) block-by-block and compares against
    /// `direct_convolve`.
    fn assert_matches_direct(
        h: &[f32],
        x: &[f32],
        block_size: usize,
        growth_factor: usize,
        max_partition: usize,
    ) {
        let direct = direct_convolve(h, x);

        let chan = PreparedChannel::new(h, block_size, growth_factor, max_partition);
        let mut state = chan.new_state();
        let mut y = vec![0f32; x.len()];
        for chunk_start in (0..x.len()).step_by(block_size) {
            let end = (chunk_start + block_size).min(x.len());
            chan.process_block(&mut state, &x[chunk_start..end], &mut y[chunk_start..end]);
        }

        let err = rms_error_db(&direct, &y).unwrap_or(f64::NEG_INFINITY);
        assert!(
            err < -100.0,
            "h.len()={} block_size={block_size} growth_factor={growth_factor} \
             max_partition={max_partition}: error too high: {err} dB",
            h.len()
        );
    }

    #[test]
    fn partitioned_matches_direct_across_fixtures_block_sizes_and_ir_lengths() {
        // IR lengths chosen so that, at growth_factor=2/max_partition=8192, at least one size
        // level has more than one member (exercising the R-8 stagger logic for real, not just
        // the degenerate single-partition-per-level case): e.g. 6000 taps at block_size=32 puts
        // several same-size groups with 2 members each well within the IR.
        let ir_lens = [500usize, 6_000];
        let block_sizes = [32usize, 256, 1024];
        let x = white_noise(9_000, 1);

        for &ir_len in &ir_lens {
            let fixtures: Vec<Vec<f32>> = vec![
                delta(ir_len),
                delayed_delta(ir_len, ir_len / 3),
                decaying_noise(ir_len, 7, ir_len as f64 / 4.0),
                minimum_phase_lowpass(ir_len, 48_000.0, 4_000.0, 4),
            ];
            for h in &fixtures {
                for &block_size in &block_sizes {
                    assert_matches_direct(
                        h,
                        &x,
                        block_size,
                        DEFAULT_GROWTH_FACTOR,
                        DEFAULT_MAX_PARTITION,
                    );
                }
            }
        }
    }

    #[test]
    fn a_size_level_with_multiple_members_is_actually_exercised() {
        // Sanity check for the test above: confirm build_schedule really does produce a
        // multi-member group for the larger IR length / block size combination used there.
        let stages = build_schedule(6_000, 32, DEFAULT_GROWTH_FACTOR, DEFAULT_MAX_PARTITION);
        let mut counts: HashMap<usize, usize> = HashMap::new();
        for s in &stages {
            *counts.entry(s.size).or_insert(0) += 1;
        }
        assert!(
            counts.values().any(|&c| c > 1),
            "expected some size level with more than one member, got {counts:?}"
        );
    }

    #[test]
    fn uniform_degenerate_schedule_also_matches_direct() {
        let h = decaying_noise(2_000, 3, 400.0);
        let x = white_noise(4_000, 2);
        assert_matches_direct(&h, &x, 64, 1, 64);
    }

    // -------------------------------------------------------------------------------------
    // PreparedIr public-API tests: WAV -> PreparedIr -> process_block, mono and stereo.
    // -------------------------------------------------------------------------------------

    fn write_mono_wav(sample_rate: u32, samples: &[f32]) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut buf = Vec::new();
        {
            let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
            for &s in samples {
                writer.write_sample(s).unwrap();
            }
            writer.finalize().unwrap();
        }
        buf
    }

    fn write_stereo_wav(sample_rate: u32, left: &[f32], right: &[f32]) -> Vec<u8> {
        assert_eq!(left.len(), right.len());
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut buf = Vec::new();
        {
            let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
            for (&l, &r) in left.iter().zip(right.iter()) {
                writer.write_sample(l).unwrap();
                writer.write_sample(r).unwrap();
            }
            writer.finalize().unwrap();
        }
        buf
    }

    #[test]
    fn mono_wav_loads_as_single_channel_and_convolves_correctly() {
        let h = delayed_delta(300, 50);
        let bytes = write_mono_wav(48_000, &h);
        let engine_rate = SampleRate::new(48_000).unwrap();
        let prepared = PreparedIr::from_wav_bytes(&bytes, engine_rate, 64).unwrap();
        assert_eq!(prepared.channel_count(), 1);
        assert_eq!(prepared.len_samples(), 300);
        assert_eq!(prepared.latency_samples(), 0);
        assert!(!prepared.was_truncated());

        let mut state = prepared.new_state();
        let x = white_noise(1_000, 11);
        let mut y = vec![0f32; x.len()];
        for chunk_start in (0..x.len()).step_by(64) {
            let end = (chunk_start + 64).min(x.len());
            let mut out_slice = &mut y[chunk_start..end];
            prepared.process_block(
                &mut state,
                &x[chunk_start..end],
                std::slice::from_mut(&mut out_slice),
            );
        }
        let direct = direct_convolve(&h, &x);
        let err = rms_error_db(&direct, &y).unwrap();
        assert!(err < -100.0, "error too high: {err} dB");
    }

    #[test]
    fn stereo_wav_loads_as_two_independent_channels() {
        let left = delayed_delta(400, 10);
        let right = delayed_delta(400, 100);
        let bytes = write_stereo_wav(44_100, &left, &right);
        let engine_rate = SampleRate::new(44_100).unwrap();
        let prepared = PreparedIr::from_wav_bytes(&bytes, engine_rate, 128).unwrap();
        assert_eq!(prepared.channel_count(), 2);

        let mut state = prepared.new_state();
        let x = white_noise(800, 5);
        let mut yl = vec![0f32; x.len()];
        let mut yr = vec![0f32; x.len()];
        for chunk_start in (0..x.len()).step_by(128) {
            let end = (chunk_start + 128).min(x.len());
            let (yl_slice, yr_slice) = (&mut yl[chunk_start..end], &mut yr[chunk_start..end]);
            let mut outputs: [&mut [f32]; 2] = [yl_slice, yr_slice];
            prepared.process_block(&mut state, &x[chunk_start..end], &mut outputs);
        }
        let direct_l = direct_convolve(&left, &x);
        let direct_r = direct_convolve(&right, &x);
        assert!(rms_error_db(&direct_l, &yl).unwrap() < -100.0);
        assert!(rms_error_db(&direct_r, &yr).unwrap() < -100.0);
    }

    #[test]
    #[should_panic(expected = "outputs.len()")]
    fn process_block_panics_on_wrong_output_channel_count() {
        let h = delta(100);
        let bytes = write_mono_wav(48_000, &h);
        let engine_rate = SampleRate::new(48_000).unwrap();
        let prepared = PreparedIr::from_wav_bytes(&bytes, engine_rate, 32).unwrap();
        let mut state = prepared.new_state();
        let input = vec![0f32; 32];
        let mut out_a = vec![0f32; 32];
        let mut out_b = vec![0f32; 32];
        let mut outputs: [&mut [f32]; 2] = [&mut out_a, &mut out_b];
        prepared.process_block(&mut state, &input, &mut outputs);
    }

    #[test]
    #[should_panic(expected = "output slice length")]
    fn process_block_panics_on_mismatched_output_length() {
        let h = delta(100);
        let bytes = write_mono_wav(48_000, &h);
        let engine_rate = SampleRate::new(48_000).unwrap();
        let prepared = PreparedIr::from_wav_bytes(&bytes, engine_rate, 32).unwrap();
        let mut state = prepared.new_state();
        let input = vec![0f32; 32];
        let mut out_a = vec![0f32; 16];
        let mut outputs: [&mut [f32]; 1] = [&mut out_a];
        prepared.process_block(&mut state, &input, &mut outputs);
    }

    // -------------------------------------------------------------------------------------
    // Resampling: a known-frequency test tone through PreparedIr::from_wav_bytes at a source
    // rate different from engine_rate, checking the effect is sane. See resample_mono's doc
    // comment for the scope note: this is not a rigorous stopband/ripple measurement.
    // -------------------------------------------------------------------------------------

    #[test]
    fn resampling_a_pure_tone_preserves_its_frequency_and_energy_roughly() {
        // A 1 kHz tone at 44.1 kHz, resampled (via being loaded as an "IR") to 48 kHz. This is
        // an odd use of an IR (impulse responses aren't tones), but it is a convenient way to
        // drive resample_mono with a signal whose frequency-domain behaviour is easy to check.
        let source_rate = 44_100u32;
        let engine_hz = 48_000u32;
        let freq = 1_000.0f64;
        let n = 4_096usize;
        let tone: Vec<f32> = (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / source_rate as f64).sin() as f32
            })
            .collect();
        let bytes = write_mono_wav(source_rate, &tone);
        let engine_rate = SampleRate::new(engine_hz).unwrap();
        let prepared = PreparedIr::from_wav_bytes(&bytes, engine_rate, 64).unwrap();

        // Resampled length should track the sample-rate ratio closely.
        let expected_len = (n as f64 * engine_hz as f64 / source_rate as f64).round() as usize;
        let got_len = prepared.len_samples();
        let len_err = (got_len as isize - expected_len as isize).unsigned_abs();
        assert!(
            len_err <= 4,
            "resampled length {got_len} too far from expected {expected_len}"
        );

        // Measure the dominant frequency bin of the resampled "IR" (its taps ARE the resampled
        // tone) via a simple Goertzel-style DFT at the expected bin, and confirm most of the
        // signal's energy landed there rather than being smeared/aliased away. We don't have
        // direct access to the taps through the public API, so instead run the resampled IR
        // through direct convolution with a unit impulse (its own definition) is circular; use
        // process_block with a silent-then-impulse input at the head partition size instead: the
        // engine's own IR *is* the resampled tone (an IR loaded from a WAV of a tone has taps
        // equal to that tone), so a unit impulse in produces exactly the tone back out.
        let mut state = prepared.new_state();
        let mut impulse = vec![0f32; got_len.max(1)];
        impulse[0] = 1.0;
        let mut out = vec![0f32; impulse.len()];
        let block = 64usize;
        for chunk_start in (0..impulse.len()).step_by(block) {
            let end = (chunk_start + block).min(impulse.len());
            let mut out_slice = &mut out[chunk_start..end];
            prepared.process_block(
                &mut state,
                &impulse[chunk_start..end],
                std::slice::from_mut(&mut out_slice),
            );
        }

        // Goertzel magnitude at the expected engine-rate bin for `freq`.
        let goertzel = |signal: &[f32], sample_rate: f64, target_hz: f64| -> f64 {
            let n = signal.len();
            let k = (0.5 + (n as f64 * target_hz) / sample_rate).floor();
            let w = 2.0 * std::f64::consts::PI * k / n as f64;
            let cosine = w.cos();
            let coeff = 2.0 * cosine;
            let (mut q1, mut q2) = (0.0f64, 0.0f64);
            for &s in signal {
                let q0 = coeff * q1 - q2 + s as f64;
                q2 = q1;
                q1 = q0;
            }
            (q1 * q1 + q2 * q2 - q1 * q2 * coeff).sqrt()
        };

        let mag_at_tone = goertzel(&out, engine_hz as f64, freq);
        let mag_at_dc = goertzel(&out, engine_hz as f64, 1.0);
        assert!(
            mag_at_tone > mag_at_dc * 10.0,
            "expected the resampled tone's energy concentrated at {freq} Hz: \
             mag_at_tone={mag_at_tone}, mag_at_dc={mag_at_dc}"
        );
    }

    #[test]
    fn resampling_same_rate_is_a_no_op_length_wise() {
        let h = delta(500);
        let bytes = write_mono_wav(48_000, &h);
        let engine_rate = SampleRate::new(48_000).unwrap();
        let prepared = PreparedIr::from_wav_bytes(&bytes, engine_rate, 64).unwrap();
        assert_eq!(prepared.len_samples(), 500);
    }

    // -------------------------------------------------------------------------------------
    // Truncation reporting at the engine rate.
    // -------------------------------------------------------------------------------------

    #[test]
    fn engine_rate_ceiling_truncates_directly() {
        // Exercises truncate_to_engine_ceiling directly (bypassing wav::decode/resampling) --
        // see that function's doc comment for why the full from_wav_bytes path cannot reliably
        // trigger this second, independent application of D-9.7's ceiling: resampling preserves
        // real-time duration, so a file already capped at 10s by wav.rs's own ceiling can never
        // produce more than 10s of taps at any engine rate.
        let engine_hz = 16_000u32;
        let mut taps = vec![vec![0f32; engine_hz as usize * 11]]; // 11s at engine rate
        let truncated = truncate_to_engine_ceiling(&mut taps, engine_hz);
        assert!(truncated);
        assert_eq!(taps[0].len(), engine_hz as usize * 10);
    }

    #[test]
    fn engine_rate_ceiling_does_not_truncate_at_or_under_ten_seconds() {
        let engine_hz = 16_000u32;
        let mut taps = vec![vec![0f32; engine_hz as usize * 10]];
        let truncated = truncate_to_engine_ceiling(&mut taps, engine_hz);
        assert!(!truncated);
        assert_eq!(taps[0].len(), engine_hz as usize * 10);
    }

    #[test]
    fn from_wav_bytes_reports_truncation_from_the_file_rate_ceiling() {
        // The end-to-end path DOES surface truncation when wav.rs's own file-rate ceiling fires
        // (this is the realistic way D-9.7's truncation gets reported in practice).
        let sample_rate = 8_000u32;
        let frames = sample_rate as usize * 11; // 11s at file rate
        let tone = white_noise(frames, 42);
        let bytes = write_mono_wav(sample_rate, &tone);
        let engine_rate = SampleRate::new(sample_rate).unwrap();
        let prepared = PreparedIr::from_wav_bytes(&bytes, engine_rate, 64).unwrap();
        assert!(prepared.was_truncated());
        assert_eq!(prepared.len_samples(), sample_rate as usize * 10);
    }

    // -------------------------------------------------------------------------------------
    // process_block allocation test, mirroring namir-nam's rt_harness usage.
    // -------------------------------------------------------------------------------------

    mod rt_harness {
        //! Copied from `namir-nam/src/wavenet.rs`'s own `rt_harness` module (itself copied from
        //! `namir-engine/src/rt_harness.rs`; self-contained here for the same reason).

        use assert_no_alloc::AllocDisabler;

        #[global_allocator]
        static ALLOC: AllocDisabler = AllocDisabler;

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

    #[test]
    fn process_block_does_not_allocate() {
        let h = decaying_noise(4_000, 9, 500.0);
        let bytes = write_mono_wav(48_000, &h);
        let engine_rate = SampleRate::new(48_000).unwrap();
        let prepared = PreparedIr::from_wav_bytes(&bytes, engine_rate, 64).unwrap();
        let mut state = prepared.new_state();
        let input = vec![0.1f32; 64];
        let mut out_a = vec![0f32; 64];
        rt_harness::audio_section(|| {
            let mut outputs: [&mut [f32]; 1] = [&mut out_a];
            prepared.process_block(&mut state, &input, &mut outputs);
        });
    }
}
