//! M3's exit criterion (`docs/03-implementation-roadmap.md` §7): "the full six-stage chain,
//! assembled for real, meets NFR-PERF-010's literal condition (48 kHz, 64-sample block, standard
//! WaveNet, 2 s stereo IR, gate + EQ active) at the 99.9th percentile per D-2.1/D-2.2's
//! methodology". Every prior number in this repo (S-1's NAM spike, S-2's IR spike, R-4's
//! `namir-nam` inner-loop bench, R-8's `namir-ir` `perf_bench.rs`) measured one stage in
//! isolation; this is the first benchmark that assembles the *real* product chain — every
//! `StagePrep::prepare` call the shipped `build_default_chain` makes, a real generated WaveNet
//! model and a real 2 s stereo IR actually loaded, gate and EQ actually engaged with non-default
//! values — and runs `Chain::process` end to end, gate → trim → nam → ir → eq → out, per block.
//!
//! # Why this duplicates `stages::build_default_chain`'s body instead of calling it
//!
//! **Decision:** hand-assemble the six stages here rather than call
//! `namir_engine::build_default_chain`.
//!
//! **Rationale:** `build_default_chain` returns `Chain`, which owns its stages only as
//! `Box<dyn Stage>` — the concrete `NamStage`/`IrStage` types (and their `load_model`/`load_ir`
//! methods, which are not part of the `Stage` trait at all) are gone the moment they're boxed.
//! There is no way to reach into an assembled `Chain` and load a resource into one of its stages
//! after the fact. The only way to load real resources *and* end up with the same chain
//! `build_default_chain` builds is to do what it does, here, keeping the concrete types around
//! just long enough to call `load_model`/`load_ir`, then boxing exactly as it does.
//!
//! **Consequence:** this file's stage-assembly block (the six `*Prep::prepare(&ctx)?` calls, the
//! `Vec<Box<dyn Stage>>`, `Chain::new` + `prepare_crosscutting`) must be kept in sync with
//! `stages/mod.rs::build_default_chain` by hand if that function's stage list or order ever
//! changes. Accepted: a `[[bench]]` binary cannot depend on `namir-engine`'s own private stage
//! fields any other way without changing `Chain`'s public API just for this one measurement, and
//! duplicating six lines of assembly is a smaller, more legible cost than either.
//!
//! # Methodology
//!
//! Same D-2.1/D-2.2 methodology as `spikes/s1-nam-inference/src/bin/bench.rs` and
//! `namir-nam/benches/wavenet_inner_loops.rs`, extended to the whole chain: single-core-pinned
//! (`core_affinity`), 5,000 warmup blocks discarded, >= 100,000 measured blocks of 64 samples at
//! 48 kHz per repetition, D-2.4's >= 5 repetitions, p50/p99/p99.9/max reported as a percentage of
//! the 1.333 ms block period. `chain.process(&mut io)` is called directly, without this crate's
//! `rt_harness::audio_section` wrapper — that harness exists to turn an accidental allocation into
//! a test failure (D-7.5), which is a correctness concern for `#[test]`s, not a timing concern for
//! a `[[bench]]` binary; wrapping every timed call in it here would only add the harness's own
//! bookkeeping overhead to every measured sample for no benefit this file needs.
//!
//! Each repetition's durations are kept in **acquisition order** until they are reduced, because
//! the estimator described below is a function of a block's *index*; the sort happens on a copy.
//!
//! # What this binary asserts, and why it asserts *those* statistics
//!
//! NFR-PERF-010's `Verify:` is `B, as a CI regression gate`, and the FRS defines `B` as a
//! "benchmark with a numeric threshold" — so this binary **asserts** the 25% budget rather than
//! printing a verdict line for a human to read past. Until M9b it printed one, in both branches,
//! and its own `// trace-partial:` said so.
//!
//! The reason that took a milestone to fix is that the obvious assertion is a bad one. A bare
//! `assert!(p99_9 <= budget)` over a single run of a *per-block audio-thread* measurement is a gate
//! that fails on a busy machine, and a gate that fails for reasons the code cannot control is one
//! people learn to ignore — which is worse than one that prints. Measured on the §2 reference
//! machine across ten consecutive runs of this binary with nothing changed between them, raw
//! `p99.9` varied from **17% to 52%** of the block period while `p50` stayed pinned near 7.8%. The
//! causes were found and are documented where they belong:
//!
//! - `pin_to_measurement_core` below: pinning to CPU 0 — which every benchmark here used to do —
//!   put the measurement on the one core absorbing `dxgkrnl.sys`'s 128-512 µs GPU interrupts,
//!   ~165 per second. That single change accounts for the largest share.
//! - Residual run-to-run drift from ordinary background load, which on this machine at times
//!   doubled `p50` on its own.
//!
//! So the gate is in **two parts**, with different failure semantics, and neither part is a
//! statistic that a co-scheduled process can inflate into a false failure:
//!
//! 1. **The schedule's own worst-case block, asserted unconditionally.** Over the durations kept in
//!    acquisition order, the largest per-residue minimum modulo [`IR_PERIOD_BLOCKS`] — the same
//!    contamination-immune estimator `benches/tail_structure.rs` reports and D-2.4 promotes to a
//!    permanent part of the methodology, computed here so the assertion and the chain assembly live
//!    in one binary. Interference is additive and aperiodic while the IR partition schedule is
//!    periodic, so each residue's cheapest occurrence out of ~780 is one that nothing landed on,
//!    and **nothing can make a block finish faster than its own arithmetic allows**. Background
//!    load cannot push this number up; only the code can. It is a *necessary* condition for the
//!    requirement rather than the requirement itself — if the uncontaminated worst block is over
//!    budget then `p99.9` certainly is — and it is the part that catches a real regression on any
//!    machine, quiet or not.
//! 2. **Raw `p99.9`, asserted over every repetition D-2.4 permits quoting.** D-2.2's gate is kept
//!    exactly as written; D-2.4 explicitly *rejected* replacing it with the estimator, so part 1
//!    alone would not span the requirement's own wording. What makes this non-flaky is D-2.4's own
//!    condition 4 rather than a margin invented here: a repetition whose raw `p99.9` substantially
//!    exceeds its own estimator was contaminated, and its figure "must be discarded, not quoted".
//!    A discarded repetition is not asserted against. A repetition can therefore only fail this
//!    assertion by being clean *by D-2.4's own instrument* and over budget at the same time, which
//!    is a regression and not a busy afternoon.
//!
//! [`VALIDITY_MARGIN_PCT`] is where the two parts meet, and it is deliberately set wider than
//! D-2.4's own "within a couple of percentage points". A *wider* margin retains more repetitions
//! and so makes part 2 fire **more** often, which is the conservative direction for a gate; set
//! narrow, it would quietly discard its way into silence on an ordinary developer machine (the
//! reference machine with this session's own agent tooling running showed gaps of 2.8-9.0 points,
//! where M3's certified quiet-machine runs showed 1.3-1.9).
//!
//! If every repetition is discarded, part 2 does not fire and the binary says so in as many words
//! — but part 1 has still fired, so the threshold does not evaporate. A `Verify: B` artifact whose
//! threshold quietly evaporates on the machines it happens to run on is the failure mode
//! `namir-app/benches/startup_to_audible.rs` is on record against.
//!
//! # Measured at M9b on the §2 reference machine, with the assertions in place
//!
//! Three runs of this binary, five repetitions each (fifteen in total), pinned to core 4 on
//! `docs/02-architecture.md` §2's machine, **re-taken on an idle machine** after this milestone's
//! build work had finished. Both parts pass against the 25%-of-one-core budget:
//!
//! | | across the three runs |
//! |---|---|
//! | part 1, the estimator | **14.56%** of one core, worst run |
//! | part 2, raw `p99.9`, worst *quotable* repetition | **16.39%** |
//!
//! That is 41.8% headroom on part 1 and 34.4% on part 2. An idle machine is still not a *verified
//! quiet* one, and the binary's own output says so — see the last section of this comment; a PASS
//! line here is not by itself a certified figure.
//!
//! **D-2.4's validity check earned its place on this very run**, which is the argument for part 1
//! made by the instrument rather than about it. One of the three repetition sets discarded **2 of
//! its 5** repetitions — raw `p99.9` of **24.35%** and **24.54%**, one of them with a `max` of
//! **76.09%** — while the contamination-immune estimator across those same five repetitions read
//! **14.38%**, moving by hundredths of a point. An idle machine is not a quiet one, the difference
//! is measurable, and the estimator is the figure to trust where the two disagree.
//!
//! ## The first M9b set, disqualified and kept on the record
//!
//! An earlier set of the same shape — three runs, five repetitions each — was measured with this
//! session's own agent tooling running throughout, so the machine was not quiet and **D-2.4
//! condition 2 disqualifies it**. It is kept rather than deleted, this project's convention being
//! to leave a corrected finding on the record, and it still carries the sharpest illustration of
//! why part 1 exists:
//!
//! | | across 15 repetitions |
//! |---|---|
//! | part 1, the estimator | 14.36 - 14.61% of one core |
//! | part 2, raw `p99.9`, worst *quotable* repetition per run | 19.28 / 19.37 / 19.44% |
//! | raw `p99.9` including discarded repetitions | 17.17 - 23.48% |
//! | repetitions discarded by D-2.4's validity check | 6 of 15 |
//! | `p50` | 7.41 - 7.79% |
//!
//! The estimator's 0.25-point spread against raw `p99.9`'s 6.3-point spread over the same fifteen
//! repetitions is the whole argument for part 1 in one line — and the comparison across the two
//! sets says it again: the disqualified set's estimator (14.36 - 14.61%) brackets the idle
//! re-run's 14.56% almost exactly, while its raw `p99.9` ran about three points worse. Part 1
//! barely noticed the contamination; part 2 did. Sharper still: re-run pinned to **core 0** — the
//! core `dxgkrnl.sys` puts ~165 interrupts/second of 128-512 µs on — the estimator read
//! 14.47-14.48% while `max` blew out to 43.23%. Deliberate contamination moved the asserted
//! statistic by a hundredth of a point. That is what "background load cannot inflate it" means, and
//! it was checked rather than assumed.
//!
//! # The one machine class that cannot run this gate
//!
//! NFR-PERF-010's budget is 25% of one core **of the reference machine** (`docs/02-architecture.md`
//! §2). On slower hardware the requirement states nothing at all, so an absolute 25% assertion
//! there is not a weaker measurement — it is a meaningless one. [`INFORMATIONAL_ENV`] turns both
//! parts into an explicit, printed skip for exactly that case, and CI's `nfr-perf-010-chain-bench`
//! job sets it, because a GitHub-hosted shared runner is precisely the variable hardware D-2.1 pins
//! a single reference machine *away* from. Do not set it anywhere else, and note what it costs:
//! `Verify: B, **as a CI regression gate**` is consequently still unspanned by anything in this
//! repository, which is what this file's `// trace-partial:` now names and all it now names.
//!
//! `benches/tail_structure.rs` remains the fuller instrument — residue occupancy, run lengths,
//! lag-1 autocorrelation and a histogram — and is what to reach for when this binary's two parts
//! disagree with each other.

use std::sync::Arc;
use std::time::Instant;

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::stages::eq::{EqPrep, EqStage};
use namir_engine::stages::gate::{GatePrep, GateStage};
use namir_engine::stages::ir::{IrPrep, IrStage};
use namir_engine::stages::nam::{NamPrep, NamStage};
use namir_engine::stages::out::OutPrep;
use namir_engine::stages::trim::TrimPrep;
use namir_engine::{Chain, ParamChange, ParamId, PrepareContext, Stage, StageIo, StagePrep};
use namir_fixtures::ir::decaying_noise;
use namir_fixtures::nam::{WaveNetShape, generate};
use namir_params::stages::{eq, gate};

const BLOCK_SIZE: usize = 64;
const SAMPLE_RATE_HZ: u32 = 48_000;
const SAMPLE_RATE_F64: f64 = 48_000.0;
const WARMUP_BLOCKS: usize = 5_000;
const MEASURED_BLOCKS: usize = 100_000; // >= 100,000 per D-2.2, per repetition
const NFR_PERF_010_BUDGET_PCT: f64 = 25.0;

/// D-2.4 condition 3: ">= 5 repetitions, with the spread reported, never a single run".
const DEFAULT_REPS: usize = 5;
/// The same floor, enforced rather than documented: [`REPS_ENV`] is clamped up to it, so no
/// invocation of this binary can assert from a sample D-2.4 would not let anyone quote.
const MIN_REPS: usize = 5;
/// Overrides [`DEFAULT_REPS`] upward. Each repetition is ~12 s on the §2 reference machine.
const REPS_ENV: &str = "NAMIR_CHAIN_REPS";

/// Set (to anything) to turn both halves of the gate into an explicit, printed skip. For hardware
/// that is not `docs/02-architecture.md` §2's reference machine — where a 25%-of-one-core budget
/// states nothing — and for nothing else. CI's `nfr-perf-010-chain-bench` job sets it; see this
/// file's "The one machine class that cannot run this gate".
const INFORMATIONAL_ENV: &str = "NAMIR_PERF_010_INFORMATIONAL";

/// D-2.4 condition 4, as a number: how far a repetition's raw `p99.9` may exceed its own
/// contamination-immune estimator before that repetition counts as contaminated and its figure is
/// discarded rather than asserted against.
///
/// Wider than the decision's own "a clean run has the two within a couple of percentage points",
/// deliberately and in the safe direction: a wider margin *retains* repetitions, so it makes the
/// `p99.9` assertion fire more often rather than less. It is still nowhere near wide enough to
/// admit real contamination — M3 measured contaminated runs at 47-49% raw against a ~15% estimator,
/// a gap of over thirty points.
const VALIDITY_MARGIN_PCT: f64 = 5.0;

/// The IR schedule's own period in host blocks at this condition: `DEFAULT_MAX_PARTITION /
/// BLOCK_SIZE`. Same constant, same reasoning and same staleness check as `tail_structure.rs`'s:
/// hard-coded so this file stays a passive observer of the schedule rather than a second consumer
/// of it, and asserted against the real schedule in `main` so it cannot silently drift.
const IR_PERIOD_BLOCKS: usize = 8192 / BLOCK_SIZE;

/// A fixed seed for the "standard" WaveNet fixture — D-19.1's generated-not-captured corpus,
/// same shape S-1/R-4 measured (`WaveNetShape::Standard`: the only shape confirmed against
/// `neural-amp-modeler`'s own `get_wavenet_config`, see `namir-fixtures`' own doc comment).
const NAM_SEED: u64 = 0xC0FF_EE01;
/// Seeds for the 2 s stereo IR's two independent channels (this module's own choice: a real
/// stereo IR's two channels are not the same signal, so two distinct seeds are used rather than
/// duplicating one mono decay onto both channels — see `stereo_ir_wav_bytes`).
const IR_SEED_LEFT: u64 = 0xBEEF_0001;
const IR_SEED_RIGHT: u64 = 0xBEEF_0002;
/// 2 seconds at 48 kHz, per NFR-PERF-010's own literal condition.
const IR_LEN_SAMPLES: usize = 2 * SAMPLE_RATE_HZ as usize;
/// Decay time constant for the `decaying_noise` fixture: short enough that the IR's energy is
/// concentrated well inside its own 2 s length (a flat, undecayed 2 s noise burst would not
/// resemble any real cabinet/room IR and is not what NFR-PERF-010's "2 s stereo IR" condition is
/// describing), long enough that essentially the whole 2 s length still carries convolution cost
/// (`namir-ir`'s own R-8 section: cost follows tap *count*, not tap values, so this only affects
/// how IR-shaped the fixture looks, not what it costs).
const IR_DECAY_TAU_SAMPLES: f64 = 8_000.0;

/// A real, non-default gate threshold (FR-GATE-010's -100..0 dBFS range; the descriptor default
/// is -70 dBFS) — chosen above the driving signal's own RMS level (see `gen_block`) so the gate
/// stays open throughout the measured run: this benchmark is timing steady-state per-sample DSP
/// cost (envelope detection + smoothing, which runs identically whether the gate is open or
/// closed), not attack/release transient behaviour, so an open gate keeps the measured condition
/// simple without changing what is actually being measured.
const GATE_THRESHOLD_DB: f32 = -40.0;
/// A real, non-default EQ low-shelf gain (FR-EQ-010's +-15 dB range; the descriptor default is
/// 0 dB) — proves the EQ cascade is doing real shaping work, not running at its identity
/// coefficients.
const EQ_LOW_SHELF_GAIN_DB: f32 = 6.0;

fn percentile(sorted_nanos: &[u64], p: f64) -> u64 {
    let idx = ((sorted_nanos.len() as f64 - 1.0) * p).round() as usize;
    sorted_nanos[idx]
}

/// One repetition, reduced. Every field is a percentage of the block period, because that is the
/// unit NFR-PERF-010's budget is stated in (D-2.1: never wall-clock, always a fraction of one core)
/// and carrying raw nanoseconds alongside it only invites the two being compared to each other.
struct Rep {
    p50: f64,
    p99: f64,
    p999: f64,
    max: f64,
    /// The contamination-immune estimator: the largest per-residue minimum, i.e. the IR schedule's
    /// own worst-case block with whatever else the machine was doing subtracted out. See this
    /// file's "What this binary asserts" section, and `tail_structure.rs` for the fuller argument.
    estimator: f64,
}

impl Rep {
    /// D-2.4 condition 4 as a predicate: may this repetition's raw `p99.9` be quoted at all?
    ///
    /// "If raw p99.9 substantially exceeds the estimator, the run was contaminated and the figure
    /// must be discarded, not quoted." A figure that may not be quoted may not be asserted against
    /// either — that is the whole reason this benchmark can carry an absolute threshold without
    /// becoming a coin flip on a shared desktop.
    fn is_quotable(&self) -> bool {
        self.p999 - self.estimator <= VALIDITY_MARGIN_PCT
    }
}

/// Reduces one repetition's per-block durations — **in acquisition order** — to a [`Rep`].
///
/// The estimator is a function of each block's index, so the ordering must survive to here; every
/// other benchmark in this workspace sorts in place at the point of measurement and destroys it.
/// The sort below is on a copy for exactly that reason.
fn analyse(durations_ns: &[u64], block_period_ns: u64) -> Rep {
    let pct = |v: u64| v as f64 / block_period_ns as f64 * 100.0;

    let mut per_residue_min = [u64::MAX; IR_PERIOD_BLOCKS];
    for (i, &v) in durations_ns.iter().enumerate() {
        let r = i % IR_PERIOD_BLOCKS;
        per_residue_min[r] = per_residue_min[r].min(v);
    }
    let estimator = per_residue_min
        .iter()
        .copied()
        .filter(|&v| v != u64::MAX)
        .max()
        .expect("MEASURED_BLOCKS far exceeds IR_PERIOD_BLOCKS, so every residue is populated");

    let mut sorted = durations_ns.to_vec();
    sorted.sort_unstable();
    Rep {
        p50: pct(percentile(&sorted, 0.50)),
        p99: pct(percentile(&sorted, 0.99)),
        p999: pct(percentile(&sorted, 0.999)),
        max: pct(*sorted
            .last()
            .expect("a repetition measures MEASURED_BLOCKS blocks")),
        estimator: pct(estimator),
    }
}

/// A small, seeded, dependency-free xorshift64* generator for the per-block driving signal —
/// same approach `namir-nam/benches/wavenet_inner_loops.rs`'s own `gen_block` uses, duplicated
/// for the same reason that file's doc comment gives (no shared path between a `benches/` binary
/// here and one in another crate).
fn gen_block(x: &mut u64, out: &mut [f32]) {
    for s in out.iter_mut() {
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        let noise = ((*x % 2_000_003) as f32 / 1_000_001.5) - 1.0; // roughly [-1, 1)
        *s = 0.1 * noise; // ~-20 dBFS RMS: comfortably above GATE_THRESHOLD_DB, keeps the gate open.
    }
}

/// Writes a small in-memory stereo WAV via `hound::WavWriter`, the same pattern
/// `namir-ir/src/convolver.rs`'s own test module's `write_stereo_wav` uses (not reusable
/// directly — that helper is private to that crate's `#[cfg(test)]` module) and
/// `namir-engine/src/stages/ir.rs`'s own test module already duplicates for the identical
/// reason.
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

/// Builds the 2 s stereo IR's WAV bytes: `namir_fixtures::ir::decaying_noise` (D-9.5's
/// "realistic-shaped" fixture — a real cabinet/room IR's cost depends only on length, not exact
/// taps, per that module's own doc comment) on each channel independently, with different seeds
/// so the two channels are genuinely distinct signals rather than one mono decay duplicated —
/// FR-CHAIN-060's "stereo IR" case, not "dual mono".
fn stereo_ir_wav_bytes() -> Vec<u8> {
    let left = decaying_noise(IR_LEN_SAMPLES, IR_SEED_LEFT, IR_DECAY_TAU_SAMPLES);
    let right = decaying_noise(IR_LEN_SAMPLES, IR_SEED_RIGHT, IR_DECAY_TAU_SAMPLES);
    write_stereo_wav(SAMPLE_RATE_HZ, &left, &right)
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

// trace-partial: NFR-PERF-010
// uncovered: NFR-PERF-010 — the "as a CI regression gate" half of Verify: B. M9b made the
// uncovered: threshold an in-process assertion twice over (the contamination-immune estimator
// uncovered: unconditionally, raw p99.9 over every repetition D-2.4 permits quoting), but the
// uncovered: budget is 25% of one core of the 02-architecture.md section 2 reference machine and
// uncovered: CI runs on GitHub-hosted shared runners, so ci.yml's nfr-perf-010-chain-bench job
// uncovered: sets NAMIR_PERF_010_INFORMATIONAL and asserts nothing: no automated runner anywhere
// uncovered: in this project gates on this number, which is what the requirement's method asks
// uncovered: for and would need a self-hosted runner on that machine; closes M8
fn main() {
    // Pin to one core, per D-2.1: every figure is single-core, and cross-core migration would
    // pollute the tail with scheduler noise unrelated to the chain's own cost.
    pin_to_measurement_core();

    let sample_rate = SampleRate::new(SAMPLE_RATE_HZ).expect("48 kHz is a valid SampleRate");
    let ctx = PrepareContext::new(sample_rate, BLOCK_SIZE, ChannelConfig::Stereo)
        .expect("BLOCK_SIZE is nonzero");

    // --- Assemble the real six-stage chain (this file's own doc comment explains why this can't
    // just call `build_default_chain`), keeping the concrete `NamStage`/`IrStage` types long
    // enough to load real resources into them below.
    let mut gate_stage: GateStage = GatePrep.prepare(&ctx).expect("GatePrep::prepare");
    let trim_stage = TrimPrep.prepare(&ctx).expect("TrimPrep::prepare");
    let mut nam_stage: NamStage = NamPrep.prepare(&ctx).expect("NamPrep::prepare");
    let mut ir_stage: IrStage = IrPrep.prepare(&ctx).expect("IrPrep::prepare");
    let mut eq_stage: EqStage = EqPrep.prepare(&ctx).expect("EqPrep::prepare");
    let out_stage = OutPrep.prepare(&ctx).expect("OutPrep::prepare");

    // --- Load a real "standard" WaveNet model (D-19.1: generated, fixed seed, never captured)
    // through the real `namir_nam::load` parser, exactly as a future M4 worker would.
    let nam_model = generate(WaveNetShape::Standard, NAM_SEED)
        .expect("standard WaveNet fixture should generate");
    let nam_bytes = nam_model.to_json_bytes();
    let prepared_nam =
        Arc::new(namir_nam::load(&nam_bytes).expect("generated WaveNet fixture should load"));
    nam_stage.load_model(prepared_nam);

    // --- Load a real 2 s stereo IR through the real `PreparedIr::from_wav_bytes`, at this
    // chain's own block size (matching `IrPrep::prepare`'s sizing and every other caller's
    // convention, e.g. `stages/ir.rs`'s own tests).
    let ir_bytes = stereo_ir_wav_bytes();
    let prepared_ir = Arc::new(
        namir_ir::PreparedIr::from_wav_bytes(&ir_bytes, sample_rate, BLOCK_SIZE)
            .expect("generated stereo IR should load"),
    );
    assert_eq!(
        prepared_ir.channel_count(),
        2,
        "IR fixture must be genuinely stereo for NFR-PERF-010's own literal condition"
    );
    ir_stage.load_ir(prepared_ir);

    // --- Activate gate + EQ (NFR-PERF-010's literal condition names both "active"): explicit
    // ENABLED=on (already the descriptor default for both, per `gate.rs`/`eq.rs`'s own `prepare`,
    // but set here anyway so this doesn't silently depend on that default never changing) plus
    // one real non-default value each, so real per-sample DSP work happens rather than a
    // bypassed/identity passthrough. Same `ParamId`-wrapping pattern `stages/gate.rs`/`eq.rs`
    // themselves use: `namir_params`'s stable id, reinterpreted as this crate's own `ParamId`.
    let gate_enabled_id = ParamId(gate::ENABLED.id.0);
    let gate_threshold_id = ParamId(gate::THRESHOLD_DB.id.0);
    let eq_enabled_id = ParamId(eq::ENABLED.id.0);
    let eq_low_shelf_gain_id = ParamId(eq::LOW_SHELF_GAIN_DB.id.0);

    gate_stage.apply(ParamChange {
        id: gate_enabled_id,
        value: 1.0, // Stepped index 1 == "On" (ParamChange's own doc comment).
    });
    gate_stage.apply(ParamChange {
        id: gate_threshold_id,
        value: GATE_THRESHOLD_DB,
    });

    eq_stage.apply(ParamChange {
        id: eq_enabled_id,
        value: 1.0,
    });
    eq_stage.apply(ParamChange {
        id: eq_low_shelf_gain_id,
        value: EQ_LOW_SHELF_GAIN_DB,
    });

    // --- Box into the real chain, in the real runtime order (`stages/mod.rs`'s doc comment:
    // "gate before trim", D-9.8), and turn on the same cross-cutting features
    // `build_default_chain` does (FR-CHAIN-030/080/090).
    let stages: Vec<Box<dyn Stage>> = vec![
        Box::new(gate_stage),
        Box::new(trim_stage),
        Box::new(nam_stage),
        Box::new(ir_stage),
        Box::new(eq_stage),
        Box::new(out_stage),
    ];
    let mut chain = Chain::new(stages);
    chain.prepare_crosscutting(&ctx);

    // The estimator below is periodic in the IR schedule's own period, so confirm the constant
    // against the real schedule rather than trusting it -- same check, same reason, as
    // `tail_structure.rs`'s.
    let schedule = namir_ir::build_schedule(
        IR_LEN_SAMPLES,
        BLOCK_SIZE,
        namir_ir::DEFAULT_GROWTH_FACTOR,
        namir_ir::DEFAULT_MAX_PARTITION,
    );
    let largest = schedule.iter().map(|s| s.size).max().unwrap_or(BLOCK_SIZE);
    assert_eq!(
        largest / BLOCK_SIZE,
        IR_PERIOD_BLOCKS,
        "IR_PERIOD_BLOCKS is stale relative to the real schedule, so the contamination-immune \
         estimator this binary asserts on would be computed modulo the wrong period"
    );

    let reps = std::env::var(REPS_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_REPS)
        .max(MIN_REPS);
    let block_period_ns = (BLOCK_SIZE as f64 / SAMPLE_RATE_F64 * 1e9) as u64;

    println!("=== NFR-PERF-010: REAL six-stage chain (gate -> trim -> nam -> ir -> eq -> out) ===");
    println!(
        "48 kHz, {BLOCK_SIZE}-sample blocks, standard WaveNet, 2 s stereo IR, gate + EQ active"
    );
    println!(
        "{reps} repetitions (D-2.4) x {MEASURED_BLOCKS} measured blocks (D-2.2), warmup \
         {WARMUP_BLOCKS} discarded"
    );
    println!(
        "block period (D-2.1): {block_period_ns} ns ({:.4} ms); budget \
         {NFR_PERF_010_BUDGET_PCT:.0}% of one core of the section 2 reference machine",
        block_period_ns as f64 / 1e6
    );
    println!(
        "estimator = largest per-residue minimum mod {IR_PERIOD_BLOCKS} blocks: the IR schedule's \
         own worst-case block, which background load cannot inflate\n"
    );

    let mut left = vec![0f32; BLOCK_SIZE];
    let mut right = vec![0f32; BLOCK_SIZE];
    let mut rng_state = 0xC0DE_CAFEu64 ^ 0x9E37_79B9_7F4A_7C15;

    // Warmed once, not once per repetition. The repetitions exist to sample D-2.4's spread, whose
    // source is the machine rather than this chain's own state, and a chain that has already run
    // 105,000 blocks is not going to become colder between two of them.
    for _ in 0..WARMUP_BLOCKS {
        gen_block(&mut rng_state, &mut left);
        right.copy_from_slice(&left);
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, BLOCK_SIZE);
        chain.process(&mut io);
        std::hint::black_box(io.channel(0));
    }

    let mut durations_ns = Vec::with_capacity(MEASURED_BLOCKS);
    let mut measured = Vec::with_capacity(reps);
    for rep in 1..=reps {
        durations_ns.clear();
        for _ in 0..MEASURED_BLOCKS {
            gen_block(&mut rng_state, &mut left);
            right.copy_from_slice(&left);
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut io = StageIo::new(&mut channels, BLOCK_SIZE);
            let start = Instant::now();
            chain.process(&mut io);
            let elapsed = start.elapsed();
            std::hint::black_box(io.channel(0));
            durations_ns.push(elapsed.as_nanos() as u64);
        }

        assert_eq!(
            chain.fault_count(),
            0,
            "the measured run must not have hit FR-CHAIN-080's NaN/Inf fault path"
        );

        let r = analyse(&durations_ns, block_period_ns);
        println!(
            "rep {rep:>2}/{reps}: p50 {:>6.2}% | p99 {:>6.2}% | p99.9 {:>6.2}% | max {:>6.2}% | \
             estimator {:>6.2}% | gap {:>6.2} pts -> {}",
            r.p50,
            r.p99,
            r.p999,
            r.max,
            r.estimator,
            r.p999 - r.estimator,
            if r.is_quotable() {
                "quotable"
            } else {
                "DISCARDED (D-2.4)"
            }
        );
        measured.push(r);
    }

    verdict(&measured);
}

/// The whole gate, in one place: both assertions, the discard rule between them, and the one
/// opt-out. Separated from `main` so the measurement above reads as measurement and this reads as
/// adjudication — and so there is exactly one place to look for what this binary actually enforces.
fn verdict(reps: &[Rep]) {
    // The estimator can only be pushed *up* by interference, never down, so the smallest of the
    // repetitions' estimates is the least-contaminated view of the schedule's own worst block.
    let best_estimator = reps
        .iter()
        .map(|r| r.estimator)
        .fold(f64::INFINITY, f64::min);
    let quotable: Vec<&Rep> = reps.iter().filter(|r| r.is_quotable()).collect();
    let worst_p999 = quotable
        .iter()
        .map(|r| r.p999)
        .fold(f64::NEG_INFINITY, f64::max);

    println!();
    println!(
        "part 1 -- schedule's own worst-case block (contamination-immune): {best_estimator:.2}% \
         of one core, best of {} repetitions",
        reps.len()
    );
    if quotable.is_empty() {
        println!(
            "part 2 -- raw p99.9 (D-2.2's gate): NOT ASSERTED. Every one of {} repetitions failed \
             D-2.4's own validity check (raw p99.9 more than {VALIDITY_MARGIN_PCT:.0} points above \
             its own estimator), and a figure D-2.4 says must be discarded rather than quoted is \
             not one to gate on. The machine was not quiet; part 1 above still applies.",
            reps.len()
        );
    } else {
        println!(
            "part 2 -- raw p99.9 (D-2.2's gate): {worst_p999:.2}% of one core, worst of the {} \
             repetition(s) of {} that D-2.4 permits quoting",
            quotable.len(),
            reps.len()
        );
    }

    // Printed first, asserted second, on the house pattern from
    // `crates/namir-worker/benches/resource_load.rs`: a failing run still leaves every measured row
    // above the panic, which is what a reader needs in order to judge the run at all.
    if std::env::var(INFORMATIONAL_ENV).is_ok() {
        println!(
            "\nINFORMATIONAL ({INFORMATIONAL_ENV} is set) -- NOTHING ASSERTED. NFR-PERF-010's \
             budget is {NFR_PERF_010_BUDGET_PCT:.0}% of one core of a specific machine \
             (02-architecture.md section 2); on any other hardware the requirement states nothing, \
             so the figures above are a report and not a verdict."
        );
        return;
    }

    assert!(
        best_estimator <= NFR_PERF_010_BUDGET_PCT,
        "NFR-PERF-010 (part 1): the IR schedule's own worst-case block measured \
         {best_estimator:.2}% of one core, over the {NFR_PERF_010_BUDGET_PCT:.0}% budget, on the \
         least-contaminated of {} repetitions. This statistic is a per-residue MINIMUM: background \
         load cannot inflate it, so unlike raw p99.9 this is not something a busy machine explains \
         away -- either the chain's own per-block cost regressed, or this is not the section 2 \
         reference machine (in which case set {INFORMATIONAL_ENV}, and read this file's \"The one \
         machine class that cannot run this gate\")",
        reps.len()
    );

    if quotable.is_empty() {
        println!(
            "\nPARTIAL PASS: part 1 held at {best_estimator:.2}% against the \
             {NFR_PERF_010_BUDGET_PCT:.0}% budget. Part 2 did not run -- quieten the machine and \
             re-run to exercise D-2.2's own statistic."
        );
        return;
    }

    assert!(
        worst_p999 <= NFR_PERF_010_BUDGET_PCT,
        "NFR-PERF-010 (part 2): raw p99.9 measured {worst_p999:.2}% of one core, over the \
         {NFR_PERF_010_BUDGET_PCT:.0}% budget, on the worst of the {} repetition(s) of {} that \
         passed D-2.4's validity check -- i.e. on a repetition whose p99.9 sat within \
         {VALIDITY_MARGIN_PCT:.0} points of its own contamination-immune estimator, which is what \
         makes this reading D-2.4-quotable rather than a busy-machine artifact. The estimator read \
         {best_estimator:.2}%. Re-run >= 5 times on a verified-quiet machine before believing it, \
         and note that a certified figure is a section 2 reference-machine figure only",
        quotable.len(),
        reps.len()
    );

    println!(
        "\nPASS: both parts inside NFR-PERF-010's {NFR_PERF_010_BUDGET_PCT:.0}% budget -- the \
         schedule's own worst block at {best_estimator:.2}%, and D-2.2's raw p99.9 at \
         {worst_p999:.2}% on the worst repetition D-2.4 lets us quote. A *certified* figure is one \
         measured on 02-architecture.md section 2's machine, quiet, and this line is not by itself \
         evidence that it was."
    );
}
