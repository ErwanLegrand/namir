//! Shared measurement harness. Compiled into both the native binary and the wasm
//! module; the only difference is the clock, which the caller supplies.

use std::sync::Arc;

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::{
    AudioEngine, Command, ParamChange, ParamId, PrepareContext, StageIo, WorkerEndpoint,
    build_default_engine,
};
use namir_params::stages::{eq, gate};

pub const BLOCK_SIZE: usize = 128;
pub const SAMPLE_RATE: u32 = 48_000;
pub const BLOCK_PERIOD_NS: f64 = BLOCK_SIZE as f64 / SAMPLE_RATE as f64 * 1e9;
/// 8192-sample IR schedule period divided by this block size. The native bench
/// uses 128 because its block is 64 samples.
pub const IR_PERIOD_BLOCKS: usize = 8192 / BLOCK_SIZE;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Band-limited noise at a steady level: the ordinary measurement input.
    Steady,
    /// Exponentially decaying to ~1e-30, to drive the chain into subnormals.
    Decaying,
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
            SampleRate::new(sample_rate).ok_or_else(|| format!("invalid sample rate: {sample_rate}"))?,
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
        self.left[..input.len()].copy_from_slice(input);
        self.right[..input.len()].copy_from_slice(input);
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

        for _ in 0..warmup {
            for s in block.iter_mut() {
                *s = self.next_sample();
            }
            self.process_block(&block);
        }

        self.durations_ns.clear();
        self.durations_ns.reserve(measured as usize);

        for _ in 0..measured {
            if signal == Signal::Decaying {
                // ~1.0 down to ~1e-30 over the run; restart when it bottoms out.
                amp *= 0.999_5;
                if amp < 1e-30 {
                    amp = 1.0;
                }
            }
            for s in block.iter_mut() {
                *s = self.next_sample() * amp;
            }
            let start = now_us();
            self.process_block(&block);
            let elapsed_ns = (now_us() - start) * 1000.0;
            self.durations_ns.push(elapsed_ns);
        }

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
