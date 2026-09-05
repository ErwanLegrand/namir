//! Shared measurement harness. Compiled into both the native binary and the wasm
//! module; the only difference is the clock, which the caller supplies.

use std::sync::Arc;

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::{
    AudioEngine, Command, ParamChange, ParamId, PrepareContext, StageIo, TelemetryEntry,
    WorkerEndpoint, build_default_engine,
};
use namir_params::stages::{eq, gate};

pub const BLOCK_SIZE: usize = 128;
pub const SAMPLE_RATE: u32 = 48_000;
pub const BLOCK_PERIOD_NS: f64 = BLOCK_SIZE as f64 / SAMPLE_RATE as f64 * 1e9;
/// 8192-sample IR schedule period divided by this block size. The native bench
/// uses 128 because its block is 64 samples.
pub const IR_PERIOD_BLOCKS: usize = 8192 / BLOCK_SIZE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    /// Band-limited noise at a steady level: the ordinary measurement input.
    Steady,
    /// Exponentially decaying by a fixed 0.999_5-per-block factor (see `fill_block`).
    ///
    /// **This is an amplitude-decay test, and it was mislabelled `Decaying` until
    /// Task 5.** Its *driving signal* never leaves f32's normal range: over
    /// `MEASURED_BLOCKS` = 100,000 blocks the amplitude reaches only
    /// `0.999_5^100_000` ~ 1.9e-22, sixteen orders of magnitude above f32's smallest
    /// normal `f32::MIN_POSITIVE` ~ 1.175_494_35e-38.
    ///
    /// What Task 5 then measured, and what neither the original comment nor its first
    /// correction predicted: the *chain* goes subnormal under this signal anyway. Over
    /// 20 000 blocks with `a1_standard`, **47.6% of blocks carry at least one subnormal
    /// output sample** (min |x| = 1e-45, the smallest f32 subnormal) and the CPU raises
    /// MXCSR's denormal-operand flag on **52.3%**. Re-running the same measurement with
    /// FTZ/DAZ installed removes essentially the whole cost (a1_standard p50
    /// 9.23% -> 6.59% against a 6.48% steady baseline), so this mode's ~40% penalty is
    /// **mostly a denormal effect after all** -- the opposite of what Task 2's correction
    /// concluded from the input amplitude alone.
    ///
    /// It is still the wrong probe for the denormal question, because amplitude and
    /// subnormality move together here and the two cannot be separated by this signal.
    /// [`Signal::SubnormalTail`] is the one that holds amplitude fixed.
    AmplitudeDecay,
    /// Full-scale noise for [`TAIL_BURST_BLOCKS`] blocks, then **exact silence** for the
    /// rest of a [`TAIL_PERIOD_BLOCKS`] cycle, repeating.
    ///
    /// This is the classic audio denormal shape: not a slowly shrinking input, but signal
    /// followed by nothing, leaving the chain's own IIR state (EQ biquads, gate envelope,
    /// gain ramps, the DC blocker) and the convolution tail to decay through f32's
    /// subnormal range under their own poles. Whether that actually happens is *measured*,
    /// not assumed -- see [`Harness::census`].
    SubnormalTail,
}

/// [`Signal::SubnormalTail`]'s burst length, in blocks.
pub const TAIL_BURST_BLOCKS: u32 = 16;
/// [`Signal::SubnormalTail`]'s full cycle, in blocks. 16 of every 512 blocks (3.1%) carry
/// signal; the other 96.9% are exact silence, which is the window the census inspects.
pub const TAIL_PERIOD_BLOCKS: u32 = 512;

impl Signal {
    pub fn label(self) -> &'static str {
        match self {
            Signal::Steady => "steady",
            Signal::AmplitudeDecay => "amp-decay",
            Signal::SubnormalTail => "subnormal",
        }
    }

    pub fn from_code(code: u32) -> Signal {
        match code {
            1 => Signal::AmplitudeDecay,
            2 => Signal::SubnormalTail,
            _ => Signal::Steady,
        }
    }
}

/// What a measured window actually contained, numerically. Task 5's whole point: the
/// denormal sub-experiment must *prove* subnormals occur rather than assume they do.
#[derive(Clone, Copy, Debug, Default)]
pub struct Census {
    pub blocks: u64,
    /// Blocks whose stereo output carried at least one subnormal (non-zero) f32.
    pub subnormal_output_blocks: u64,
    /// Individual subnormal output samples, out of `blocks * BLOCK_SIZE * 2`.
    pub subnormal_output_samples: u64,
    /// Blocks during which the CPU itself raised MXCSR's Denormal-operand (DE) or
    /// Underflow (UE) status bit inside `process_block`. **This is the strong witness**:
    /// it reports subnormal arithmetic *inside* the chain, including state the harness
    /// cannot see from the output buffer. Native x86-64 only; always 0 on wasm32, which
    /// has no such register.
    pub denormal_flag_blocks: u64,
    pub underflow_flag_blocks: u64,
    /// Smallest non-zero |output sample| seen. Below `f32::MIN_POSITIVE` this is itself a
    /// subnormal witness.
    pub min_abs_nonzero: f32,
}

/// Reads and clears MXCSR's exception-status bits, returning what was set.
/// Native x86-64 only.
#[cfg(all(target_arch = "x86_64", not(target_arch = "wasm32")))]
#[allow(deprecated)]
fn take_fp_status() -> u32 {
    // SAFETY: `_mm_getcsr`/`_mm_setcsr` are unconditionally available on x86-64 (SSE2 is
    // baseline). Clearing only the six status bits leaves the control bits (rounding
    // mode, FTZ/DAZ, masks) exactly as found.
    unsafe {
        let csr = core::arch::x86_64::_mm_getcsr();
        core::arch::x86_64::_mm_setcsr(csr & !0x3f);
        csr & 0x3f
    }
}

#[cfg(not(all(target_arch = "x86_64", not(target_arch = "wasm32"))))]
fn take_fp_status() -> u32 {
    0
}

/// MXCSR Denormal-operand status bit.
pub const FP_DE: u32 = 0x02;
/// MXCSR Underflow status bit.
pub const FP_UE: u32 = 0x10;

/// Installs FTZ + DAZ on this thread, the way `namir-platform`'s `DenormalGuard` does for
/// shipped native Namir. The spike deliberately does not depend on `namir-platform`
/// (D-5.1 would allow it, but the wasm side cannot have it, and a guard on one side only
/// would confound the comparison), so Task 5 prices the guard with the two instructions
/// it comes down to rather than by taking the dependency. Returns false where there is no
/// such mode -- notably wasm32, whose whole point here is that it has none.
#[allow(deprecated)]
pub fn set_flush_to_zero(on: bool) -> bool {
    #[cfg(all(target_arch = "x86_64", not(target_arch = "wasm32")))]
    {
        const FTZ: u32 = 0x8000;
        const DAZ: u32 = 0x0040;
        // SAFETY: as `take_fp_status`. FTZ|DAZ are the two control bits
        // `namir-platform/src/denormal.rs` sets; nothing else in MXCSR is touched.
        unsafe {
            let csr = core::arch::x86_64::_mm_getcsr();
            let next = if on {
                csr | FTZ | DAZ
            } else {
                csr & !(FTZ | DAZ)
            };
            core::arch::x86_64::_mm_setcsr(next);
            core::arch::x86_64::_mm_getcsr() & (FTZ | DAZ) == if on { FTZ | DAZ } else { 0 }
        }
    }
    #[cfg(not(all(target_arch = "x86_64", not(target_arch = "wasm32"))))]
    {
        let _ = on;
        false
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub p50: f64,
    pub p99: f64,
    pub p999: f64,
    pub max: f64,
    pub estimator: f64,
}

impl Stats {
    /// Mirrors `six_stage_chain.rs`'s VALIDITY_MARGIN_PCT = 5.0.
    pub fn is_quotable(&self) -> bool {
        self.p999 - self.estimator <= 5.0
    }
}

pub struct Harness {
    engine: AudioEngine,
    endpoint: WorkerEndpoint,
    ctx: PrepareContext,
    left: Vec<f32>,
    right: Vec<f32>,
    durations_ns: Vec<f64>,
    rng: u32,
}

impl Harness {
    pub fn new(sample_rate: u32, block: usize) -> Result<Self, String> {
        // Same construction as crates/namir-engine/benches/six_stage_chain.rs:439-441.
        let ctx = PrepareContext::new(
            SampleRate::new(sample_rate)
                .ok_or_else(|| format!("invalid sample rate: {sample_rate}"))?,
            block,
            ChannelConfig::Stereo,
        )
        .map_err(|e| format!("{e:?}"))?;
        let (engine, endpoint) = build_default_engine(&ctx).map_err(|e| format!("{e:?}"))?;
        let mut h = Self {
            engine,
            endpoint,
            ctx,
            left: vec![0.0; block],
            right: vec![0.0; block],
            durations_ns: Vec::new(),
            rng: 0x2545_F491,
        };
        h.engage_gate_and_eq()?;
        Ok(h)
    }

    /// NFR-PERF-010's literal condition names gate and EQ as *active*. Without this
    /// both sit at their defaults and a bypassed identity path is measured instead of
    /// real per-sample DSP -- the figure would be optimistic and wrong. Mirrors
    /// six_stage_chain.rs:477-503, but delivered over the command ring rather than by
    /// calling `Stage::apply` on a concrete stage.
    fn engage_gate_and_eq(&mut self) -> Result<(), String> {
        const GATE_THRESHOLD_DB: f32 = -60.0;
        const EQ_LOW_SHELF_GAIN_DB: f32 = 6.0;
        let changes = [
            (ParamId(gate::ENABLED.id.0), 1.0), // stepped index 1 == "On"
            (ParamId(gate::THRESHOLD_DB.id.0), GATE_THRESHOLD_DB),
            (ParamId(eq::ENABLED.id.0), 1.0),
            (ParamId(eq::LOW_SHELF_GAIN_DB.id.0), EQ_LOW_SHELF_GAIN_DB),
        ];
        for (id, value) in changes {
            self.endpoint
                .commands
                .try_push(Command::Param(ParamChange { id, value }))
                .map_err(|_| "command ring full while engaging gate/EQ".to_string())?;
        }
        Ok(())
    }

    /// Positive confirmation that the queued `load_nam`/`load_ir` commands actually
    /// completed D-8.1's handover, not merely that `try_push` accepted them into the
    /// ring -- a stage still has to take the offer, and a silently-empty stage would
    /// produce a plausible-looking but wrong (too-fast) timing figure. Reads
    /// `telemetry.{nam,ir}.loaded`, which `stages/nam.rs`/`stages/ir.rs`'s own
    /// `telemetry` impl sets to `1.0` only once `self.slots[self.active].is_some()` --
    /// i.e. the active slot, not merely an in-flight offer. Called after warmup (by
    /// construction several thousand blocks, far more than one `HANDOVER_CROSSFADE_MS`
    /// crossfade needs), so both resources should be fully installed by the time this
    /// runs; panics loudly if either is not, rather than letting a silently-empty stage
    /// through as a timing figure.
    pub fn assert_resources_loaded(&mut self) {
        const NAM_LOADED_ID: u32 = namir_params::ParamId::from_key("telemetry.nam.loaded").0;
        const IR_LOADED_ID: u32 = namir_params::ParamId::from_key("telemetry.ir.loaded").0;

        let mut buf = [TelemetryEntry { id: 0, value: 0.0 }; 256];
        let drain = self.endpoint.telemetry.drain(&mut buf);
        let mut nam_loaded = None;
        let mut ir_loaded = None;
        for entry in &buf[..drain.read] {
            if entry.id == NAM_LOADED_ID {
                nam_loaded = Some(entry.value);
            } else if entry.id == IR_LOADED_ID {
                ir_loaded = Some(entry.value);
            }
        }
        assert_eq!(
            nam_loaded,
            Some(1.0),
            "NAM model did not complete its handover before measurement began \
             (telemetry.nam.loaded != 1.0)"
        );
        assert_eq!(
            ir_loaded,
            Some(1.0),
            "IR did not complete its handover before measurement began \
             (telemetry.ir.loaded != 1.0)"
        );
    }

    /// The per-residue estimator is periodic in the IR schedule's own period. Confirm
    /// IR_PERIOD_BLOCKS against the real schedule rather than trusting the constant --
    /// the same check, for the same reason, as six_stage_chain.rs:520-530.
    pub fn assert_ir_period(ir_len_samples: usize) {
        let schedule = namir_ir::build_schedule(
            ir_len_samples,
            BLOCK_SIZE,
            namir_ir::DEFAULT_GROWTH_FACTOR,
            namir_ir::DEFAULT_MAX_PARTITION,
        );
        let largest = schedule.iter().map(|s| s.size).max().unwrap_or(BLOCK_SIZE);
        assert_eq!(
            largest / BLOCK_SIZE,
            IR_PERIOD_BLOCKS,
            "IR_PERIOD_BLOCKS must match the real schedule's largest partition"
        );
    }

    pub fn load_nam(&mut self, bytes: &[u8]) -> Result<(), String> {
        let model = Arc::new(namir_nam::load(bytes).map_err(|e| format!("{e:?}"))?);
        self.endpoint
            .commands
            .try_push(Command::load_nam(model, &self.ctx))
            .map_err(|_| "command ring full".to_string())
    }

    pub fn load_ir(&mut self, bytes: &[u8]) -> Result<(), String> {
        let ir = Arc::new(
            namir_ir::PreparedIr::from_wav_bytes(bytes, self.ctx.sample_rate(), BLOCK_SIZE)
                .map_err(|e| format!("{e:?}"))?,
        );
        self.endpoint
            .commands
            .try_push(Command::load_ir(ir, &self.ctx))
            .map_err(|_| "command ring full".to_string())
    }

    fn next_sample(&mut self) -> f32 {
        // xorshift32, so the signal is identical across native and wasm.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    pub fn process_block(&mut self, input: &[f32]) {
        // A short chunk is zero-padded, not shortened: `StageIo` would accept a smaller
        // `frames`, but the IR convolver's partition schedule is built for a fixed
        // BLOCK_SIZE, so the honest short-block semantics here are pad-with-silence.
        // Without the `fill` the tail carried the *previous* block's samples, which is
        // the deferred bug Task 2 left. Web Audio's quantum is exactly BLOCK_SIZE, so
        // this only bites `render` on a non-multiple length.
        let n = input.len().min(BLOCK_SIZE);
        self.left[..n].copy_from_slice(&input[..n]);
        self.right[..n].copy_from_slice(&input[..n]);
        self.left[n..].fill(0.0);
        self.right[n..].fill(0.0);
        let mut chans: [&mut [f32]; 2] = [&mut self.left, &mut self.right];
        let mut io = StageIo::new(&mut chans, BLOCK_SIZE);
        self.engine.process(&mut io);
        // Retire returned resources so the handover does not stall (engine.rs:158-170).
        while self.endpoint.retire.try_pop().is_some() {}
    }

    /// Renders `input` through the chain, writing the left output channel.
    /// Used by the wasm-vs-native output parity check.
    pub fn render(&mut self, input: &[f32], out_left: &mut [f32]) {
        for (chunk, out) in input
            .chunks(BLOCK_SIZE)
            .zip(out_left.chunks_mut(BLOCK_SIZE))
        {
            self.process_block(chunk);
            out.copy_from_slice(&self.left[..out.len()]);
        }
        // `render` bypasses `run`, so it must carry `run`'s two guards itself: a parity
        // render from a chain whose NAM/IR never landed, or one that tripped
        // FR-CHAIN-080's fault path, is exactly the silent-output failure the parity
        // check exists to catch -- and it would otherwise be compared against a native
        // reference produced the same broken way, and "pass".
        self.assert_resources_loaded();
        assert_eq!(
            self.engine.chain().fault_count(),
            0,
            "the parity render must not have hit FR-CHAIN-080's NaN/Inf fault path"
        );
    }

    /// FR-CHAIN-080's NaN/Inf fault counter. A plain read, so the `process` export can
    /// afford it per block.
    pub fn fault_count(&self) -> u64 {
        self.engine.chain().fault_count()
    }

    /// Left output channel of the most recent `process_block`. Lets the wasm `process`
    /// export be genuinely in-place for Task 6's AudioWorklet.
    pub fn output_left(&self) -> &[f32] {
        &self.left
    }

    /// The one place a measurement signal is generated, shared by `run` and `census` so
    /// the timed window and the window that proves subnormals occur are the same signal.
    fn fill_block(&mut self, block: &mut [f32], signal: Signal, i: u32, amp: &mut f32) {
        match signal {
            Signal::Steady => {
                for s in block.iter_mut() {
                    *s = self.next_sample();
                }
            }
            Signal::AmplitudeDecay => {
                *amp *= 0.999_5;
                if *amp < 1e-30 {
                    *amp = 1.0;
                }
                let a = *amp;
                for s in block.iter_mut() {
                    *s = self.next_sample() * a;
                }
            }
            Signal::SubnormalTail => {
                if i % TAIL_PERIOD_BLOCKS < TAIL_BURST_BLOCKS {
                    for s in block.iter_mut() {
                        *s = self.next_sample();
                    }
                } else {
                    // Exact zero, not a small number: the phenomenon under test is the
                    // chain's own state decaying with nothing driving it.
                    block.fill(0.0);
                }
            }
        }
    }

    /// Runs the same warmup/measured window as `run`, **untimed**, and reports what was
    /// numerically present. Kept as a separate pass rather than folded into `run` so that
    /// nothing in the census -- least of all the MXCSR read-modify-write, which is not
    /// cheap -- can land inside a timed span.
    pub fn census(&mut self, warmup: u32, measured: u32, signal: Signal) -> Census {
        let mut block = vec![0.0f32; BLOCK_SIZE];
        let mut amp = 1.0f32;
        for i in 0..warmup {
            self.fill_block(&mut block, signal, i, &mut amp);
            self.process_block(&block);
        }
        self.assert_resources_loaded();

        let mut c = Census {
            min_abs_nonzero: f32::MAX,
            ..Census::default()
        };
        for i in 0..measured {
            self.fill_block(&mut block, signal, i, &mut amp);
            let _ = take_fp_status(); // clear, then attribute what follows to this block
            self.process_block(&block);
            let status = take_fp_status();
            c.blocks += 1;
            if status & FP_DE != 0 {
                c.denormal_flag_blocks += 1;
            }
            if status & FP_UE != 0 {
                c.underflow_flag_blocks += 1;
            }
            let mut hit = false;
            for ch in [&self.left, &self.right] {
                for &v in ch.iter() {
                    let a = v.abs();
                    if a != 0.0 {
                        if a < c.min_abs_nonzero {
                            c.min_abs_nonzero = a;
                        }
                        if v.is_subnormal() {
                            c.subnormal_output_samples += 1;
                            hit = true;
                        }
                    }
                }
            }
            if hit {
                c.subnormal_output_blocks += 1;
            }
        }
        assert_eq!(
            self.engine.chain().fault_count(),
            0,
            "the census run must not have hit FR-CHAIN-080's NaN/Inf fault path"
        );
        if c.min_abs_nonzero == f32::MAX {
            c.min_abs_nonzero = 0.0;
        }
        c
    }

    pub fn run(
        &mut self,
        warmup: u32,
        measured: u32,
        signal: Signal,
        now_us: &dyn Fn() -> f64,
    ) -> Stats {
        let mut block = vec![0.0f32; BLOCK_SIZE];
        let mut amp = 1.0f32;

        // Warmup always runs the *measured* signal, so a SubnormalTail run enters its
        // measured window with the chain already in the silent phase of a cycle rather
        // than freshly excited.
        for i in 0..warmup {
            self.fill_block(&mut block, signal, i, &mut amp);
            self.process_block(&block);
        }

        // D-8.1's handover must have actually completed by now, not merely been queued --
        // see `assert_resources_loaded`'s own doc comment.
        self.assert_resources_loaded();

        self.durations_ns.clear();
        self.durations_ns.reserve(measured as usize);

        for i in 0..measured {
            self.fill_block(&mut block, signal, i, &mut amp);
            let start = now_us();
            self.process_block(&block);
            let elapsed_ns = (now_us() - start) * 1000.0;
            self.durations_ns.push(elapsed_ns);
        }

        // FR-CHAIN-080's NaN/Inf fault path must not have fired during the measured run --
        // same check, same reason, as six_stage_chain.rs:594-598: a run that hit it cannot
        // be quoted as a timing figure.
        assert_eq!(
            self.engine.chain().fault_count(),
            0,
            "the measured run must not have hit FR-CHAIN-080's NaN/Inf fault path"
        );

        self.reduce()
    }

    fn reduce(&self) -> Stats {
        let pct = |v: f64| v / BLOCK_PERIOD_NS * 100.0;

        // Per-residue minimum, in acquisition order, before sorting.
        let mut per_residue_min = [f64::MAX; IR_PERIOD_BLOCKS];
        for (i, &v) in self.durations_ns.iter().enumerate() {
            let r = i % IR_PERIOD_BLOCKS;
            if v < per_residue_min[r] {
                per_residue_min[r] = v;
            }
        }
        let estimator = per_residue_min
            .iter()
            .copied()
            .filter(|v| *v != f64::MAX)
            .fold(0.0f64, f64::max);

        let mut sorted = self.durations_ns.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let percentile = |p: f64| {
            let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
            sorted[idx]
        };

        Stats {
            p50: pct(percentile(0.50)),
            p99: pct(percentile(0.99)),
            p999: pct(percentile(0.999)),
            max: pct(*sorted.last().unwrap_or(&0.0)),
            estimator: pct(estimator),
        }
    }
}
