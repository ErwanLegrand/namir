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

/// Pins this thread to one core (D-2.1), **deliberately not core 0**.
///
/// # Why not core 0 — measured, not assumed
///
/// Every benchmark in this workspace originally pinned to `get_core_ids().next()`, i.e. logical
/// CPU 0. An elevated `xperf -on Latency` trace on the `docs/02-architecture.md` §2 reference
/// machine showed why that was the worst possible choice: `dxgkrnl.sys` (the DirectX/GPU kernel
/// driver) issues **6,494 interrupts of 128-512 µs** over a 39.4 s trace — about 165 per second —
/// and its ISR time lands on **CPU 0 exclusively** (1,670,068 µs on CPU 0, exactly 0 µs on all 31
/// other logical CPUs; a steady ~4.2% of CPU 0's wall clock, every second).
///
/// ISRs execute at DIRQL, above every thread priority, which is why raising the process to
/// Windows `High` priority changed nothing when that was tried. Pinned to CPU 0, the IR stage
/// measured p99.9 = 258 µs; on any other core the same binary measures 55 µs — a 4.7x difference
/// entirely attributable to the GPU driver, and on a clean core p99 (51.6 µs) and p99.9 (55.0 µs)
/// converge, which is the tight schedule-bounded distribution the cost model predicted all along.
///
/// The same trace shows CPU 0 is not the only contaminated core: in the **DPC** table
/// `ntoskrnl.exe` accumulates 50,151 µs on **CPU 2** — the highest of any core, a steady
/// ~1,500-2,100 µs every second — because Windows routes ISRs and much of its DPC load to
/// *different* cores. Measured on the chain benchmark in one interleaved sequence: core 0 gives
/// p99.9 ~34.8%, core 2 gives 24.4-25.2%, cores 4/8/12 give ~17-24%. The *ordering* (0 worst,
/// 2 next, 4+ best) is reproducible and corroborated by the trace above. The *absolute* figure is
/// **not** yet stable: one session measured cores 4/8/12 at a tight 16.5-18.1% across twelve runs,
/// a later session measured the same cores at ~24% across nine runs -- machine quiet, no ETW
/// session active, same binary, in both. That ~40% session-to-session shift is unexplained, and is
/// why no NFR-PERF-010 pass/fail verdict should be read off these numbers yet: only same-session,
/// interleaved comparisons are currently trustworthy on this machine.
///
/// So core 0 measurements were not measuring Namir. This defaults to **index 4**, the first core
/// the trace shows clean of both the GPU ISR load and the kernel DPC load; override with
/// `NAMIR_PIN_CORE=<index>` to reproduce the contaminated figures or to probe a specific core.
/// Index is clamped into range, so this is safe on machines with few cores.
///
/// **This is a measurement fix, not a product fix.** A real audio callback that happens to be
/// scheduled onto CPU 0 on a machine like this would suffer the same 128-512 µs stalls. Giving the
/// audio thread sensible affinity/priority is `namir-platform`'s job and is tracked as M6 work.
fn pin_to_measurement_core() {
    let Some(ids) = core_affinity::get_core_ids() else {
        return;
    };
    if ids.is_empty() {
        return;
    }
    // Default index 4: clean of both CPU 0's GPU ISRs and CPU 2's kernel DPC load per the trace
    // described above.
    //
    // Even indices are NOT required, contrary to an earlier revision of this comment which
    // reasoned that on an SMT machine whose logical CPUs pair as (0,1), (2,3), ... an odd index
    // is a sibling sharing execution resources. Measured, that reasoning is inert while the
    // sibling is idle: the contamination-immune estimator returns 15.26 / 15.32 / 15.53 / 15.39 /
    // 15.30 / 15.23% on cores 4 / 5 / 8 / 9 / 12 / 13 respectively -- odd and even
    // indistinguishable. It would matter only if something were actually scheduled on the paired
    // thread, which is a reason to prefer an idle core, not specifically an even one.
    let idx = std::env::var("NAMIR_PIN_CORE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4)
        .min(ids.len() - 1);
    core_affinity::set_for_current(ids[idx]);
}

fn main() {
    pin_to_measurement_core();

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
