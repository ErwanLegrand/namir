//! Is a large partition's FFT trigger **arithmetic-bound or memory-bound**?
//!
//! # Why this exists
//!
//! M3's IR tail investigation established that the measured p99.9 is set by the cost of a single
//! largest-partition (8192) trigger, at roughly 265 µs on the reference machine — and that this
//! figure is stubbornly immune to every arithmetic-side intervention tried: sweeping
//! `max_partition` 8192 → 2048 (which should cut FFT work ~4x), replacing the overlap-add's 64-bit
//! `%` with a mask, and enabling AVX2/FMA (which transformed the *NAM* stage's cost, 30.3% → 10.5%
//! p99.9, but barely touched the IR stage).
//!
//! A cache-based account predicts exactly that pattern. One size-8192 partition's hot data is
//! `h_spectrum` (~8P bytes) + `in_buf` (4P) + `time_scratch` (8P) + `freq_scratch` (~8P) ≈ **28P ≈
//! 229 KB** — versus 512 KB of L2 on this CPU, shared with 23 other partitions plus a 512 KB ring
//! (total working set ≈ 3.26 MB per channel). Since a given 8192 partition fires only once every
//! 128 host blocks, its data is *guaranteed* evicted from L2 between triggers. 229 KB is 3,584
//! cache lines; at DRAM latency with prefetching defeated (FFT butterflies are strided and
//! bit-reversed, the classic prefetcher-hostile pattern) that alone lands in the right order of
//! magnitude. Critically, this account also explains the flatness: the total spectrum data is
//! ~8 bytes per IR tap **regardless of how it is partitioned**, so repartitioning moves no bytes.
//!
//! # Result: that cache account is REFUTED, and it took the premise with it
//!
//! Measured on the §2 reference machine, medians over 4,000 iterations per condition:
//!
//! | partition size | WARM | COLD | ratio |
//! |---|---|---|---|
//! | 8192 | 31.1-38.8 µs | 31.7-33.0 µs | **~1.0x** |
//! | 4096 | 14.6 µs | 14.9 µs | 1.02x |
//! | 2048 | 6.5 µs | 6.7 µs | 1.03x |
//! | 512 | 1.6 µs | 1.6 µs | 1.00x |
//!
//! **Residency is irrelevant** — cold and warm are within noise at every size, so the trigger is
//! arithmetic-bound, not memory-bound. The FFT also scales exactly as textbook `P·log P` predicts
//! (512 → 8192 is 16x the size for ~20x the time), i.e. there is no cache cliff anywhere in range.
//!
//! The far more consequential result is the **absolute** number. A complete 8192 trigger costs
//! ~31 µs. Against a 4.6 µs p50, the schedule's own worst host block should therefore land near
//! 35-50 µs — and `perf_bench 96000 64` measures **p99 = 52.6 µs**, matching closely. But its
//! **p99.9 = 284 µs**, a 5.4x jump above p99 that no partition schedule can account for, since the
//! schedule's entire per-block inventory is already spent by p99.
//!
//! So the IR stage's real worst-case DSP cost is ~52 µs — **3.9% of the 1.333 ms block period**,
//! comfortably inside NFR-PERF-010's 25% budget. The stage was never the problem. What the p99.9
//! figure is mostly measuring is ~100 excursions per second of roughly 230 µs each, which are
//! **not** ordinary thread preemption: re-running the same binary at Windows `High` process
//! priority changed nothing (p99.9 280.5 µs vs 284.1 µs). That leaves interference which preempts
//! irrespective of user-thread priority — DPCs/ISRs at elevated IRQL, or SMIs, which are invisible
//! to the OS altogether. Confirming *which* needs an elevated `xperf -on Latency` trace (the
//! Windows Performance Toolkit is already installed on this machine) or a purpose-built DPC/ISR
//! latency tool; it is not answerable from user-mode timing alone, which is the limit this binary
//! reaches.
//!
//! The standing lesson, recorded because this milestone re-learned it several times: a plausible
//! quantitative story (the 3,584-cache-lines-times-DRAM-latency arithmetic above lands within 10%
//! of the observed figure) is not evidence. It took ~40 lines of measurement to overturn, and the
//! arithmetic that made it convincing was numerology.
//!
//! # What this binary measures
//!
//! The same real `realfft` R2C → spectral-multiply → C2R sequence `fft_stage_process_sample`
//! performs, under two contrasting residency conditions, with everything else held identical:
//!
//! - **WARM** — one single buffer set, transformed repeatedly back to back. After the first
//!   iteration everything is L1/L2-resident. This isolates the pure arithmetic cost.
//! - **COLD** — `COLD_SETS` independent buffer sets, cycled round-robin so that by the time any
//!   one set is revisited it has been evicted by the others. This reproduces the residency a real
//!   partition actually experiences between triggers.
//!
//! Same instructions, same data sizes, same code path; only residency differs. A large
//! cold/warm ratio is direct evidence the trigger is memory-bound, which would mean further
//! arithmetic optimisation (smaller FFTs, cheaper inner loops, wider SIMD) cannot move the tail,
//! and the productive directions are instead locality and prefetch: making a partition's buffers
//! contiguous, software-prefetching the next trigger's spectrum (the schedule is fully
//! deterministic, so *which* partition fires next and *when* are both known in advance), or
//! splitting a large FFT across blocks so its traffic is spread rather than burst.
//!
//! Not a D-2.2 benchmark and gates nothing: it reports medians over many iterations to compare two
//! conditions, not a percentile against a budget.
//!
//! Usage: `cargo run --release -p namir-ir --example fft_cost_probe [size]` (default 8192).

use realfft::RealFftPlanner;
use realfft::num_complex::Complex32;
use std::time::Instant;

/// Enough independent sets that their combined footprint far exceeds L2 (512 KB on the §2
/// reference machine), so a revisited set is reliably evicted. At size 8192 each set is ~229 KB,
/// so 40 sets ≈ 9 MB — comfortably past L2, sitting in L3/DRAM exactly as the real convolver's
/// 3.26 MB working set does.
const COLD_SETS: usize = 40;
const WARMUP: usize = 200;
const ITERS: usize = 4_000;

/// One partition's worth of buffers — the same four arrays `FftStageImmutable`/`FftStageState`
/// hold between them.
struct BufSet {
    h_spectrum: Vec<Complex32>,
    time_scratch: Vec<f32>,
    freq_scratch: Vec<Complex32>,
}

fn median(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    v[v.len() / 2]
}

fn main() {
    if let Some(id) = core_affinity::get_core_ids().and_then(|ids| ids.into_iter().next()) {
        core_affinity::set_for_current(id);
    }

    let size: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8192);
    let fft_len = 2 * size;

    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(fft_len);
    let c2r = planner.plan_fft_inverse(fft_len);
    let spec_len = r2c.make_output_vec().len();

    let make_set = |seed: u32| {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32) - 0.5
        };
        BufSet {
            h_spectrum: (0..spec_len)
                .map(|_| Complex32::new(next(), next()))
                .collect(),
            time_scratch: (0..fft_len).map(|_| next()).collect(),
            freq_scratch: vec![Complex32::new(0.0, 0.0); spec_len],
        }
    };

    // One trigger's real work: forward transform, spectral multiply, inverse transform.
    let trigger = |s: &mut BufSet| {
        r2c.process(&mut s.time_scratch, &mut s.freq_scratch)
            .expect("r2c");
        for (f, h) in s.freq_scratch.iter_mut().zip(s.h_spectrum.iter()) {
            *f *= h;
        }
        // The inverse real transform requires a Hermitian-symmetric spectrum: purely real at DC
        // and Nyquist. In the real convolver both operands are spectra *of real signals*, so this
        // holds by construction; here `h_spectrum` is synthetic random data, so it must be
        // imposed explicitly. Two stores per trigger — negligible against the work being timed,
        // and it keeps the measured path otherwise byte-identical to the real one.
        let last = s.freq_scratch.len() - 1;
        s.freq_scratch[0].im = 0.0;
        s.freq_scratch[last].im = 0.0;
        c2r.process(&mut s.freq_scratch, &mut s.time_scratch)
            .expect("c2r");
    };

    // ---- WARM: one set, reused every iteration.
    let mut warm_set = make_set(1);
    for _ in 0..WARMUP {
        trigger(&mut warm_set);
    }
    let mut warm = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t0 = Instant::now();
        trigger(&mut warm_set);
        warm.push(t0.elapsed().as_nanos() as u64);
        std::hint::black_box(&warm_set.time_scratch[0]);
    }

    // ---- COLD: cycle through many sets so each is evicted before it is revisited.
    let mut sets: Vec<BufSet> = (0..COLD_SETS).map(|i| make_set(i as u32 + 7)).collect();
    for (i, s) in sets.iter_mut().enumerate() {
        if i < WARMUP {
            trigger(s);
        }
    }
    let mut cold = Vec::with_capacity(ITERS);
    for i in 0..ITERS {
        let s = &mut sets[i % COLD_SETS];
        let t0 = Instant::now();
        trigger(s);
        cold.push(t0.elapsed().as_nanos() as u64);
        std::hint::black_box(&s.time_scratch[0]);
    }

    let w = median(warm);
    let c = median(cold);
    let bytes_per_set = spec_len * 8 + fft_len * 4 + spec_len * 8;

    println!("=== FFT trigger: arithmetic-bound or memory-bound? ===");
    println!("partition size {size} (fft_len {fft_len}), {ITERS} iterations each");
    println!(
        "per-set hot data: {:.0} KB; cold pool: {COLD_SETS} sets = {:.1} MB",
        bytes_per_set as f64 / 1024.0,
        (bytes_per_set * COLD_SETS) as f64 / (1024.0 * 1024.0)
    );
    println!();
    println!(
        "  WARM (L1/L2-resident):   {w:>8} ns  ({:.1} us)",
        w as f64 / 1000.0
    );
    println!(
        "  COLD (evicted between):  {c:>8} ns  ({:.1} us)",
        c as f64 / 1000.0
    );
    println!(
        "  cold / warm ratio:       {:.2}x",
        c as f64 / w.max(1) as f64
    );
    println!();
    println!(
        "For reference, the in-situ IR-stage p99.9 attributable to one such trigger is ~265 us."
    );
    println!(
        "A large ratio => memory-bound: arithmetic-side optimisation cannot move the tail, and \
         locality/prefetch/splitting are the productive directions instead."
    );
}
