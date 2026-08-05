//! S-2 spike: non-uniform partitioned convolution, per `docs/02-architecture.md` D-9.4/D-9.5/D-9.6.
//!
//! `build_schedule` turns (IR length, host block size, growth factor, max partition) into a
//! list of `StageSpec`s: the first partition equals the host block size and is processed in the
//! time domain (D-9.4's zero-latency head); every later partition is FFT-based and grows
//! geometrically. `PartitionedConvolver` runs that schedule; `direct_convolve` is the D-9.5
//! reference every schedule is checked against in `verify.rs`.
//!
//! **Causality note, worth recording so it isn't rediscovered:** a size-`P` FFT partition at IR
//! offset `off` can only be computed once `P` samples of input feeding it have arrived, and its
//! output is due starting at time `off` relative to the start of that input window — so it is
//! only computable in time if `off >= P`. Growing the partition size by `growth_factor` after
//! exactly `growth_factor` partitions of the current size (not a fixed count) is what keeps that
//! invariant true at every size transition, by induction — see the schedule doc comment below
//! for the derivation. Using a fixed count (e.g. always 2, which is the classic scheme for
//! `growth_factor == 2`) breaks causality for `growth_factor > 2`.
//!
//! **Scope note:** the ring accumulator here is sized to the whole IR length, which is fine for
//! an offline cost-measurement tool but is *not* how the shipping IR stage should allocate — a
//! real-time implementation would bound it to the largest single stage's own slack and reuse
//! that space across stages. Sizing it to the IR is a spike simplification, not a product design
//! decision; D-9.4/D-9.6 as written say nothing about buffer sizing and shouldn't be read as
//! endorsing this shortcut.

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::collections::HashMap;
use std::sync::Arc;

/// The default Windows CRT heap dominates the FFT-planning/setup path (many small allocations
/// building spectra); measured processing itself allocates nothing (see `PartitionedConvolver`
/// scratch buffers), but mimalloc is applied process-wide anyway for consistency with S-1 and
/// because it's a plausible pick for the shipping product.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// One FFT-based partition: covers IR taps `[offset, offset + actual_len)`, using a nominal
/// block/FFT size of `size` (`actual_len <= size`; less only for the final partition covering a
/// tail shorter than a full partition).
#[derive(Debug, Clone, Copy)]
pub struct StageSpec {
    pub offset: usize,
    pub size: usize,
    pub actual_len: usize,
}

/// Builds the D-9.4 non-uniform schedule for an IR of `ir_len` taps.
///
/// The head (direct, time-domain) partition is `min(block_size, ir_len)` taps and is *not*
/// included in the returned list — `PartitionedConvolver` handles it separately. Every returned
/// `StageSpec` is FFT-based.
///
/// `growth_factor == 1` degenerates to uniform partitioned convolution (every FFT partition is
/// `block_size`), useful as a sweep baseline. `max_partition == block_size` also degenerates to
/// uniform, regardless of `growth_factor`.
pub fn build_schedule(
    ir_len: usize,
    block_size: usize,
    growth_factor: usize,
    max_partition: usize,
) -> Vec<StageSpec> {
    assert!(block_size > 0 && growth_factor >= 1 && max_partition >= block_size);
    let head = block_size.min(ir_len);
    let mut stages = Vec::new();
    let mut offset = head;
    let mut size = block_size;
    let per_level = growth_factor.max(1);
    while offset < ir_len {
        for _ in 0..per_level {
            if offset >= ir_len {
                break;
            }
            let actual_len = (ir_len - offset).min(size);
            stages.push(StageSpec {
                offset,
                size,
                actual_len,
            });
            offset += size;
        }
        if size < max_partition && growth_factor > 1 {
            size = (size * growth_factor).min(max_partition);
        } else if growth_factor <= 1 {
            // growth_factor <= 1 has no valid "grow" step; treat as uniform (already handled by
            // per_level == 1, this branch just avoids an infinite loop at size == max_partition).
        }
    }
    stages
}

/// FFT machinery for one partition size, cached so multiple `StageSpec`s at the same nominal
/// `size` (there are always >= 1, often `growth_factor`, per level) share one plan.
struct FftPlan {
    fft_len: usize,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
}

struct FftStage {
    offset: usize,
    size: usize,
    actual_len: usize,
    fft_len: usize,
    h_spectrum: Vec<Complex32>,
    in_buf: Vec<f32>,
    in_pos: usize,
    // Reusable scratch, allocated once at setup — the measured `process_block` path allocates
    // nothing, matching the lesson from S-1's bench.rs about allocator noise dominating timing.
    time_scratch: Vec<f32>,
    freq_scratch: Vec<Complex32>,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
}

impl FftStage {
    fn new(spec: StageSpec, h: &[f32], plan: &FftPlan) -> Self {
        let mut time_buf = vec![0f32; plan.fft_len];
        time_buf[..spec.actual_len]
            .copy_from_slice(&h[spec.offset..spec.offset + spec.actual_len]);
        let mut h_spectrum = plan.r2c.make_output_vec();
        plan.r2c
            .process(&mut time_buf, &mut h_spectrum)
            .expect("r2c on h segment");
        FftStage {
            offset: spec.offset,
            size: spec.size,
            actual_len: spec.actual_len,
            fft_len: plan.fft_len,
            h_spectrum,
            in_buf: vec![0f32; spec.size],
            in_pos: 0,
            time_scratch: vec![0f32; plan.fft_len],
            freq_scratch: plan.r2c.make_output_vec(),
            r2c: Arc::clone(&plan.r2c),
            c2r: Arc::clone(&plan.c2r),
        }
    }

    /// Feeds one new input sample (the sample at absolute time `t_abs`). Returns `true` if this
    /// completed the stage's input block and triggered an FFT (the caller uses this to attribute
    /// cost / count trigger events; the ring write itself always happens here).
    #[inline]
    fn process_sample(&mut self, x: f32, t_abs: u64, ring: &mut [f32]) -> bool {
        self.in_buf[self.in_pos] = x;
        self.in_pos += 1;
        if self.in_pos < self.size {
            return false;
        }
        self.in_pos = 0;

        self.time_scratch[..self.size].copy_from_slice(&self.in_buf);
        for v in &mut self.time_scratch[self.size..] {
            *v = 0.0;
        }
        self.r2c
            .process(&mut self.time_scratch, &mut self.freq_scratch)
            .expect("r2c");
        for (f, h) in self.freq_scratch.iter_mut().zip(self.h_spectrum.iter()) {
            *f *= h;
        }
        self.c2r
            .process(&mut self.freq_scratch, &mut self.time_scratch)
            .expect("c2r");

        // Causality: this block covered input samples [t_abs - size + 1, t_abs]. Convolved with
        // an `h` segment starting at IR tap `offset`, the earliest affected output sample is
        // t_abs - size + 1 + offset; the linear-convolution result is `size + actual_len - 1`
        // samples long.
        let start_abs = t_abs + 1 - self.size as u64 + self.offset as u64;
        let valid_len = self.size + self.actual_len - 1;
        let scale = 1.0 / self.fft_len as f32; // realfft's inverse transform is unnormalized
        let ring_len = ring.len() as u64;
        for i in 0..valid_len {
            let pos = ((start_abs + i as u64) % ring_len) as usize;
            ring[pos] += self.time_scratch[i] * scale;
        }
        true
    }
}

/// Runs the D-9.4 schedule: a direct time-domain head partition plus a chain of FFT-based
/// `FftStage`s, summed through a shared ring accumulator (see the module-level scope note on why
/// the ring is sized to the whole IR here rather than to one stage's slack).
pub struct PartitionedConvolver {
    head: Vec<f32>,
    head_history: Vec<f32>, // ring buffer, length == head.len()
    stages: Vec<FftStage>,
    ring: Vec<f32>,
    t: u64,
}

impl PartitionedConvolver {
    pub fn new(h: &[f32], block_size: usize, growth_factor: usize, max_partition: usize) -> Self {
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
            stages.push(FftStage::new(*spec, h, plan));
        }

        let max_reach = schedule
            .iter()
            .map(|s| s.offset + 2 * s.size)
            .max()
            .unwrap_or(head_len);
        let ring_len = (ir_len + 2 * block_size).max(max_reach).max(head_len).next_power_of_two();

        PartitionedConvolver {
            head: h[..head_len].to_vec(),
            head_history: vec![0f32; head_len.max(1)],
            stages,
            ring: vec![0f32; ring_len],
            t: 0,
        }
    }

    pub fn latency_samples(&self) -> usize {
        0 // D-9.4: the head partition makes this zero-latency by construction.
    }

    /// Processes one host block in place. Allocates nothing.
    pub fn process_block(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), output.len());
        let head_len = self.head.len();
        let ring_len = self.ring.len() as u64;
        for i in 0..input.len() {
            let x = input[i];
            let t = self.t;

            if head_len > 0 {
                self.head_history[(t as usize) % head_len] = x;
            }
            let mut y = 0f32;
            for k in 0..head_len {
                let dt = t.wrapping_sub(k as u64);
                if dt > t {
                    break; // k > t: no history yet this far back
                }
                y += self.head[k] * self.head_history[(dt as usize) % head_len];
            }

            let pos = (t % ring_len) as usize;
            y += self.ring[pos];
            self.ring[pos] = 0.0;
            output[i] = y;

            for stage in &mut self.stages {
                stage.process_sample(x, t, &mut self.ring);
            }

            self.t += 1;
        }
    }
}

/// D-9.5 reference: full time-domain convolution, no partitioning, no FFT. Deliberately naive —
/// its only job is to be obviously correct.
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

/// RMS(a - b) in dB below RMS(a), for correctness checks. `None` if `a` is silent.
pub fn rms_error_db(a: &[f32], b: &[f32]) -> Option<f64> {
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

/// Generated (per D-19.1) fixtures — cost/behaviour follows length and shape, not tap values,
/// so a synthetic IR is as good as a captured one for this spike's purpose.
pub mod fixtures {
    use rand::Rng;
    use rand::SeedableRng;

    /// A unit impulse: the simplest possible analytically-known IR (D-9.5's list).
    pub fn delta(len: usize) -> Vec<f32> {
        let mut h = vec![0f32; len];
        if len > 0 {
            h[0] = 1.0;
        }
        h
    }

    /// An impulse delayed by `delay` samples — exercises non-zero-offset partitions cleanly.
    pub fn delayed_delta(len: usize, delay: usize) -> Vec<f32> {
        let mut h = vec![0f32; len];
        if delay < len {
            h[delay] = 1.0;
        }
        h
    }

    /// Exponentially decaying white noise: the standard stand-in for a "realistic-shaped" IR
    /// (a real cabinet/room IR's *cost* depends only on its length, not its exact taps).
    pub fn decaying_noise(len: usize, seed: u64, tau_samples: f64) -> Vec<f32> {
        let mut rng = rand_pcg::Pcg64::seed_from_u64(seed);
        (0..len)
            .map(|i| {
                let env = (-(i as f64) / tau_samples).exp();
                (rng.gen_range(-1.0f64..1.0) * env) as f32
            })
            .collect()
    }

    /// White noise test signal, seeded.
    pub fn white_noise(len: usize, seed: u64) -> Vec<f32> {
        let mut rng = rand_pcg::Pcg64::seed_from_u64(seed);
        (0..len).map(|_| rng.gen_range(-0.8f32..0.8)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fixtures::*;

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
        for i in 0..head {
            covered[i] = true;
        }
        for s in &stages {
            for i in s.offset..s.offset + s.actual_len {
                assert!(!covered[i], "tap {i} covered twice");
                covered[i] = true;
            }
        }
        assert!(covered.iter().all(|&c| c), "some tap never covered");
    }

    #[test]
    fn partitioned_matches_direct_on_delta() {
        let h = delta(500);
        let x = white_noise(2000, 1);
        let direct = direct_convolve(&h, &x);
        let mut conv = PartitionedConvolver::new(&h, 64, 2, 256);
        let mut y = vec![0f32; x.len()];
        for chunk_start in (0..x.len()).step_by(64) {
            let end = (chunk_start + 64).min(x.len());
            conv.process_block(&x[chunk_start..end], &mut y[chunk_start..end]);
        }
        let err = rms_error_db(&direct, &y).unwrap();
        assert!(err < -100.0, "delta case error too high: {err} dB");
    }
}
