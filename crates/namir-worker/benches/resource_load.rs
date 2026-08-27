//! NFR-PERF-050: "loading shall complete within 500 ms for files up to 50 MB." A wall-clock
//! figure by its own literal wording, and legitimately measured as one: D-2.5 (added M5) scopes
//! D-2.1's "never wall-clock" rule to audio-thread per-block budgets specifically, and everything
//! this benchmark times is worker-side, off-audio-thread work (D-8.1 step 1) — `Instance::load`
//! is documented "not RT-safe" for exactly that reason.
//!
//! # Three payloads, each timed twice
//!
//! - **A standard model** — `WaveNetShape::Standard`, the one architecture `namir-fixtures`' own
//!   generator cites against a real `neural-amp-modeler` config (see that module's doc comment):
//!   the realistic case a user's load time actually looks like.
//! - **A 2 s stereo IR** — the same shape `namir-engine`'s `handover_crossfade.rs` and
//!   `six_stage_chain.rs` already measure against, so this crate's figure is comparable to
//!   theirs rather than an independently-invented size.
//! - **A ~50 MB oversized, uncalibrated model** —
//!   `namir_fixtures::nam::generate_oversized_uncalibrated`, at the channel count
//!   `namir-fixtures`' own test pins to ~49.6 MB. **Honest caveat, repeated from that function's
//!   own doc comment: a 50 MB `.nam` file is not a shape the NAM ecosystem actually produces** — a
//!   real exported model runs from a few hundred KB to a few MB. It is measured anyway because
//!   NFR-PERF-050 states its ceiling in terms of file size, and this is the only way to give that
//!   number a concrete file to mean something against; the realistic case above is reported
//!   alongside it precisely so this worst case is not mistaken for a typical one.
//!
//! Each is measured through **both** [`LoadSource`] variants, and the pairing is the point:
//!
//! - **`bytes`** — [`LoadSource::Bytes`], an `Arc<[u8]>` that already exists in memory, so the
//!   window contains parse and prepare cost and nothing else. This is what the binary measured
//!   exclusively until M9b, and it is kept because isolating Namir's own cost from the storage
//!   stack's is worth a row of its own: a regression in one is not a regression in the other.
//! - **`file`** — [`LoadSource::File`], the variant a product shell actually uses, which performs a
//!   `std::fs::metadata` and a `std::fs::read` of the whole file *inside* `Instance::load`
//!   (`namir-worker/src/lib.rs`'s `LoadSource::read`). NFR-PERF-050 states its ceiling **"for files
//!   up to 50 MB"**, and until M9b this binary timed a 50 MB *payload* and never a 50 MB *file* —
//!   the syscalls sat outside the window and every figure was a lower bound on what a user waits
//!   for. The `file` rows close that: the harness writes each fixture to a scratch path and hands
//!   `Instance::load` the path, so the read is inside the measured window and the 500 ms assertion
//!   is against the duration the requirement's own sentence describes.
//!
//! **Page-cache state, per D-2.5's first condition, stated rather than assumed:** the harness wrote
//! each file moments before measuring it, so every `file` row is a **warm-cache** read. That is the
//! honest ceiling this benchmark can offer without platform-specific cache-dropping (which would be
//! `namir-platform`'s business and `unsafe`, for a measurement) — a first-ever read of a 50 MB file
//! from cold storage will be slower by whatever that volume costs, and the `file` minus `bytes`
//! delta printed below is a floor on that, not an estimate of it. The volume the scratch directory
//! lands on, and whether a real-time anti-malware scanner was active, are D-2.5 conditions 2 and 3
//! and belong in whatever record quotes these numbers; the binary prints the path so the first is
//! at least visible.
//!
//! # Measured at M9b on the §2 reference machine, with the file arms in place
//!
//! Five runs of this binary, pinned to core 4 on `docs/02-architecture.md` §2's machine (NTFS,
//! `%TEMP%` on the system volume), **re-taken on an idle machine** after this milestone's build
//! work had finished. Worst repetition across the runs, against the 500 ms ceiling:
//!
//! | arm | `bytes` | `file` |
//! |---|---|---|
//! | ~50 MB oversized (52,012,406 B) | **129.6 ms** | **152.5 ms** |
//!
//! Headroom on the 500 ms ceiling: 74.1% on the `bytes` arm, 69.5% on the `file` arm. **Every arm
//! passes**, but only the oversized pair's figures were recorded from the idle re-run, it being the
//! pair that carries this clause — so no re-taken number is quoted here for the standard-model or
//! IR arms, whose disqualified figures appear below and whose margin is orders of magnitude.
//!
//! **NFR-PERF-050's first clause passes with the read inside the window**: a 49.6 MiB file, opened
//! by path, loads in ~152 ms against a 500 ms ceiling. The read itself costs about **23 ms** of
//! that — the `file` minus `bytes` delta, warm cache — so **~85% of what a user waits for on a
//! 50 MB file is parse and prepare**, and the storage stack is the small term. Because those reads
//! are warm (see the page-cache paragraph above), the ~23 ms is a *floor* on cold-read cost rather
//! than an estimate of it. At the realistic sizes the delta is in the tens of microseconds and
//! disappears into the run-to-run spread, which is why the oversized pair is the one that carries
//! this clause.
//!
//! ## The first M9b set, disqualified and kept on the record
//!
//! An earlier set of five runs was measured with this session's own agent tooling running, so the
//! machine was not quiet and **D-2.4 condition 2 disqualifies it**. It is kept rather than deleted,
//! this project's convention being to leave a corrected finding on the record:
//!
//! | arm | `bytes` | `file` |
//! |---|---|---|
//! | standard model (229 KB) | 0.89 - 0.97 ms | 0.94 - 1.47 ms |
//! | 2 s stereo IR (384 KB) | 4.64 - 4.72 ms | 4.47 - 4.96 ms |
//! | ~50 MB oversized (52,012,406 B) | 127.7 - 133.4 ms | 148.1 - 149.4 ms |
//!
//! Its `file` minus `bytes` delta read 15 - 21 ms against the idle re-run's ~23 ms, so the
//! parse-dominates-I/O conclusion above survives the correction — but the figures this milestone
//! quotes are the idle ones, not these.
//!
//! # Cold, not cached
//!
//! Each repetition builds a fresh [`namir_engine::AudioEngine`]/[`namir_worker::Instance`] and a
//! fresh, empty [`namir_worker::ResourceCache`] before loading — `ResourceCache` is
//! process-global-shaped (D-8.2) precisely so a *second* instance loading the same bytes is
//! nearly free, which is the wrong thing to measure here: NFR-PERF-050 is about the load a user
//! actually waits on, the first one. This is Namir's own cache and is orthogonal to the OS page
//! cache discussed above; both are stated because a "cold" claim that meant only one of them would
//! be the kind of half-truth D-2.5 exists to forbid.
//!
//! # The other half of the sentence: the audio-thread clause
//!
//! NFR-PERF-050 is two clauses joined by an "and" — "shall complete within 500 ms for files up to
//! 50 MB **on the reference machine, and shall never delay the audio thread regardless of duration
//! (FR-NAM-070)**". Until M14 this binary measured the first and said nothing about the second:
//! nothing here ran an audio thread at all. The nearest evidence was `tests/rt_stress.rs`'s axis A,
//! which drives `Instance::load` in a loop against a live `AudioEngine` and asserts zero
//! audio-thread allocation, zero dropout blocks and a bounded worst block — real evidence, but an
//! integration test rather than the `Verify: B` NFR-PERF-050 names, and its models are
//! `WaveNetShape::Nano`, so "regardless of duration" was exercised at no long duration by anything
//! in this tree.
//!
//! **M14 adds the audio-thread arm** (`measure_the_audio_thread_clause`, at the bottom of this
//! file): a real `AudioEngine` running a real standard-model-plus-2 s-IR chain, paced to the block
//! period on the measurement core, while a worker thread on a *different* core performs one whole
//! `Instance::load` of the 50 MB file by path. Per-block times are collected with and without that
//! load in flight, and the loading arm's p99.9 is asserted to stay inside a factor of the quiet
//! arm's. That is the clause, measured as a benchmark with a numeric threshold, which is what its
//! `Verify: B` asks for.
//!
//! **The tag above `main` nonetheless stays a `// trace-partial:`, with a narrower gap named.**
//! The arm's measured window ends when `Instance::load` returns — i.e. when the offer has been
//! submitted — and discards a few trailing blocks, because the model being offered is
//! `generate_oversized_uncalibrated`'s 430-channel *size* fixture, whose per-block inference cost
//! is enormous and is not a load cost at all. So what is measured is the window in which the long
//! load is genuinely in flight, and the offer-and-crossfade half of a 50 MB changeover is measured
//! by nothing at that size. That half belongs to FR-NAM-070 and is measured by
//! `namir-engine/benches/handover_crossfade.rs` — against a standard model, not a 50 MB one.
//! Claiming the whole clause on the strength of this arm would be exactly the over-claim D-23.1's
//! two questions exist to catch.
//!
//! ## First run of the audio-thread arm (INFORMATIONAL — sandbox, not the reference machine)
//!
//! Five runs of the binary in this shared development sandbox, pinned per D-2.1 but on a machine
//! that was not quiet: quiet-arm p99.9 775.6 / 873.6 / 895.2 / 786.6 / 784.0 µs, loading-arm p99.9
//! 851.0 / 839.5 / 725.7 / 827.2 / 799.5 µs, so a p99.9 ratio of **0.81 - 1.10x** against the 2.0x
//! bound. Two of the five ratios are *below* 1.0, which is the shape to expect if the load is
//! genuinely off this thread: the difference between the two arms is then run-to-run noise rather
//! than a signal, and noise goes both ways. Per D-2.4 none of these are certified figures — the
//! reference machine's own run is what closes anything.
//!
//! # Read this before quoting any number from this binary
//!
//! D-2.4 governs, same as every other benchmark in this workspace: pin away from CPU 0 (absorbs
//! `dxgkrnl.sys`'s GPU interrupts) and CPU 2 (heaviest kernel DPC load) — this defaults to core 4,
//! override with `NAMIR_PIN_CORE` — on a machine verified quiet, across >= 5 repetitions with the
//! spread reported. `RUSTFLAGS` replaces `.cargo/config.toml`'s `-C target-cpu=x86-64-v3` rather
//! than appending to it; an unexpectedly-set `RUSTFLAGS` silently measures without AVX2.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::{PrepareContext, build_default_engine};
use namir_fixtures::ir::{decaying_noise, to_stereo_wav_bytes};
use namir_fixtures::nam::{WaveNetShape, generate, generate_oversized_uncalibrated};
use namir_worker::{EngineConfig, Instance, JobResult, LoadSource, ResourceCache, Target};

const SR: u32 = 48_000;
const BLOCK: usize = 64;
/// The channel count `namir-fixtures`' own `oversized_fixture_at_430_channels_is_close_to_50mb`
/// test pins at ~49.6 MB. If that test's assertion range ever needs to move, this constant moves
/// with it.
const OVERSIZED_CHANNELS: usize = 430;

/// NFR-PERF-050's ceiling, **asserted** rather than printed as a closing line for a human to
/// compare against by eye. The FRS defines `Verify: B` as "benchmark with a numeric threshold", so
/// until this constant had an `assert!` behind it (M9a) this binary was not a `B` at all.
///
/// The assertion is necessary for a tag, not sufficient for a plain one: this binary still spans
/// only part of NFR-PERF-050's sentence — its second clause, the audio-thread one — so the tag
/// above `main` remains a `// trace-partial:` naming that one gap. The first clause's own gap, that
/// no arm put a real file read inside the measured window, closed at M9b; see "Three payloads, each
/// timed twice" above.
///
/// Per D-2.4 a failing assertion means *re-run before believing it*: this bench is not on CI's
/// critical path (only `six_stage_chain` runs there), so an absolute wall-clock threshold cannot
/// make CI flaky, and the certified figure remains a §2-reference-machine matter across >= 5
/// repetitions on a quiet machine.
const NFR_PERF_050_CEILING: Duration = Duration::from_millis(500);

/// The "for files up to 50 MB" half of the same sentence, in the MiB the fixture's own size test
/// uses. The 500 ms ceiling is only claimed for payloads at or under this size, so an arm that
/// outgrew it would be asserting something the requirement does not say — checked rather than
/// assumed, because the oversized arm's size is a fixture property ([`OVERSIZED_CHANNELS`]) that
/// can drift out from under this file.
const NFR_PERF_050_MAX_BYTES: usize = 50 * 1024 * 1024;

fn ctx() -> PrepareContext {
    PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Stereo).unwrap()
}

/// See `namir-engine`'s `handover_crossfade.rs`/`six_stage_chain.rs` identical function for the
/// full measured argument against CPU 0 and CPU 2. Defaults to index 4; override with
/// `NAMIR_PIN_CORE`.
fn pin_to_measurement_core() {
    let Some(ids) = core_affinity::get_core_ids() else {
        return;
    };
    if ids.is_empty() {
        return;
    }
    let idx = std::env::var("NAMIR_PIN_CORE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4)
        .min(ids.len() - 1);
    core_affinity::set_for_current(ids[idx]);
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

/// The in-memory arm: the clock starts with the bytes already in hand, so the window is parse and
/// prepare cost with no filesystem in it.
fn measure_bytes(label: &str, target: Target, bytes: &Arc<[u8]>, reps: usize) -> Duration {
    measure(label, "bytes", target, bytes.len(), reps, || {
        LoadSource::Bytes(Arc::clone(bytes))
    })
}

/// The real-file arm: `Instance::load` is handed a **path**, so `LoadSource::read`'s
/// `std::fs::metadata` and `std::fs::read` of the whole file happen *inside* the measured window —
/// which is what makes this the arm NFR-PERF-050's "for files up to 50 MB" wording is actually
/// about. Returns the measured worst repetition so the caller can print the read's cost against the
/// same payload's `bytes` row.
///
/// The `LoadSource` (and with it the one `PathBuf` clone this needs) is built *before* the clock
/// starts, exactly as the byte arm's `Arc::clone` is: harness bookkeeping is not what either arm is
/// measuring.
fn measure_file(label: &str, target: Target, path: &Path, reps: usize) -> Duration {
    let len = std::fs::metadata(path)
        .expect("the harness wrote this file moments ago")
        .len() as usize;
    measure(label, "file", target, len, reps, || {
        LoadSource::File(path.to_path_buf())
    })
}

/// Measures `reps` independent cold loads against `target`, each against a fresh
/// engine/instance/cache — see this file's own "Cold, not cached" section — and asserts
/// NFR-PERF-050's ceiling against the slowest of them. Returns that slowest repetition.
fn measure(
    label: &str,
    via: &str,
    target: Target,
    payload_bytes: usize,
    reps: usize,
    source: impl Fn() -> LoadSource,
) -> Duration {
    assert!(
        payload_bytes <= NFR_PERF_050_MAX_BYTES,
        "{label}: this arm's payload is {} bytes, past NFR-PERF-050's {NFR_PERF_050_MAX_BYTES}-byte \
         \"files up to 50 MB\" clause -- the 500 ms ceiling is not claimed for it, so asserting the \
         ceiling here would assert something the requirement does not say. Shrink the fixture, or \
         report this arm without the assertion and say so",
        payload_bytes
    );

    let mut durations = Vec::with_capacity(reps);
    for _ in 0..reps {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);
        let src = source();

        let started = Instant::now();
        let outcome = instance.load(&cache, target, src);
        let elapsed = started.elapsed();

        match outcome.result {
            JobResult::Loaded { .. } => {}
            other => panic!("{label} ({via}): expected a successful load, got {other:?}"),
        }
        durations.push(elapsed);
    }
    durations.sort_unstable();
    let p50 = percentile(&durations, 0.5);
    // At small `reps` (the oversized arm's 5) this is indistinguishable from `max` -- expected,
    // not a bug: a true p99 needs a sample size nothing this slow to generate can afford.
    let p99 = percentile(&durations, 0.99);
    let max = *durations.last().unwrap();
    println!(
        "{label:<32} {via:<5} bytes={payload_bytes:>11} reps={reps:>3} | p50 {p50:>9.2?} | \
         p99 {p99:>9.2?} | max {max:>9.2?}"
    );

    // Printed first, asserted second, deliberately: a failing arm still leaves its own measured row
    // above the panic, which is what a reader needs to judge whether the run was contaminated.
    //
    // On `max` rather than `p50`: "shall complete within 500 ms" is a statement about a load, not
    // about a median load, and at both `reps` values used here `percentile(.., 0.99)` already
    // resolves to the last element, so a p99 assertion would be the same assertion wearing a
    // narrower-sounding name.
    assert!(
        max <= NFR_PERF_050_CEILING,
        "NFR-PERF-050: {label} via {via} ({payload_bytes} bytes) took {max:.2?} on its slowest of \
         {reps} repetitions, over the {NFR_PERF_050_CEILING:?} ceiling (p50 {p50:.2?}, p99 \
         {p99:.2?}). D-2.4: one reading on a machine that was not verified quiet is not evidence \
         of a regression -- re-run pinned (NAMIR_PIN_CORE) >= 5 times before believing this, and \
         note that a certified figure is a reference-machine (02-architecture.md section 2) figure \
         only"
    );
    max
}

/// Where the `file` arms' fixtures are written. Process-scoped so two concurrent runs of this
/// binary cannot measure each other's files, and removed at the end of `main`.
fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("namir-nfr-perf-050-{}", std::process::id()))
}

/// Writes one fixture out and returns its path. The write is deliberately *not* timed: what
/// NFR-PERF-050 measures is a load, and a user's file arrived on the volume long before they
/// clicked it. Its side effect on the page cache is stated in this file's own doc comment.
fn plant(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("a scratch fixture should be writable");
    path
}

// ---------------------------------------------------------------------------------------------
// The audio-thread arm (added M14) — NFR-PERF-050's second clause.
//
// See "The other half of the sentence" in this file's module doc comment for what this arm is
// answering and why the arms above cannot. In outline: a real `AudioEngine` runs a real chain
// (standard model, 2 s stereo IR) at 48 kHz / 64-sample blocks, paced to the block period as a
// real callback thread is, while a worker thread performs one whole `Instance::load` of the 50 MB
// file **by path** — `fs::metadata`, `fs::read`, parse, prepare, offer. Every block's own duration
// is recorded. The same audio loop is then run with nothing happening beside it, and the two
// distributions' p99.9 are compared.
//
// Three design points that are not incidental.
//
// **The measured window ends when `Instance::load` returns, not later.** `load` returns once the
// offer has been *submitted*, so a block or two after the signal may already be running the
// incoming model — and the incoming model here is a 430-channel size fixture whose *inference*
// cost is enormous and is not a load cost at all. Charging it to this clause would be measuring
// the wrong thing, so [`TRAILING_BLOCKS_DISCARDED`] blocks are dropped from the tail of every
// repetition. What is left is the window in which the long-duration work is genuinely in flight,
// which is the window the clause is about. The handover's own cost is FR-NAM-070's, and is
// measured by `namir-engine/benches/handover_crossfade.rs`.
//
// **The worker thread is pinned to a different core than the audio loop.** `pin_to_measurement_core`
// runs on the main thread and a spawned thread inherits its affinity mask, so without this the
// worker and the audio loop would contend for one core and the arm would fail for a reason that is
// a property of the harness rather than of the code under test. A real product runs them on
// different cores; so does this.
//
// **A fresh engine, instance and cache per repetition**, for the same "Cold, not cached" reason
// the arms above give: `ResourceCache` would otherwise make the second load nearly free, which is
// the opposite of the long duration this arm needs.

/// Blocks dropped from the tail of each repetition — see the section comment above.
const TRAILING_BLOCKS_DISCARDED: usize = 5;

/// Repetitions of the audio-thread arm. Each one is bounded by how long a 50 MB load takes
/// (~150 ms on the §2 reference machine), so five repetitions is on the order of a second of
/// measured audio, or roughly 500 blocks per arm.
const AUDIO_ARM_REPS: usize = 5;

/// How far the loading arm's p99.9 per-block time may exceed the quiet arm's.
///
/// **Not a performance measurement**, exactly as `rt_stress.rs`'s `MAX_BLOCK_MULTIPLE` is not:
/// what this bound detects is the audio thread *waiting* on the load — a lock, an allocation
/// serialised behind the worker's, a ring push that blocked. NFR-PERF-010 is where the chain's
/// absolute per-block budget is judged, on `six_stage_chain.rs`'s figure and not on this one. A
/// factor rather than an absolute so the bound means the same thing on a slow machine as on a
/// fast one, and 2.0 rather than something tighter because a p99.9 taken over a few hundred blocks
/// on a machine that is not a benchmarking rig carries real run-to-run spread of its own — see
/// D-2.4.
const AUDIO_DELAY_FACTOR: f64 = 2.0;

/// One repetition of the audio-thread arm. Runs the audio loop, paced to the block period, until
/// `work` (running on its own thread, on its own core) signals completion — or until
/// `max_blocks`, whichever comes first — and returns each block's own duration with the tail
/// discarded.
fn audio_blocks_while<F>(max_blocks: usize, work: F) -> Vec<Duration>
where
    F: FnOnce() + Send + 'static,
{
    let c = ctx();
    let (mut engine, endpoint) = build_default_engine(&c).unwrap();
    let cache = ResourceCache::new();
    let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

    // A realistic chain to measure: the standard model and a 2 s stereo IR, the same pair
    // `six_stage_chain.rs` measures NFR-PERF-010 against.
    let standard: Arc<[u8]> = Arc::from(
        generate(WaveNetShape::Standard, 1)
            .expect("standard fixture should generate")
            .to_json_bytes()
            .into_boxed_slice(),
    );
    let ir_len = 2 * SR as usize;
    let ir: Arc<[u8]> = Arc::from(
        to_stereo_wav_bytes(
            &decaying_noise(ir_len, 21, 8_000.0),
            &decaying_noise(ir_len, 22, 8_000.0),
            SR,
        )
        .into_boxed_slice(),
    );
    instance.load(&cache, Target::Nam, LoadSource::Bytes(standard));
    instance.load(&cache, Target::Ir, LoadSource::Bytes(ir));
    drop(instance);

    let mut left = vec![0f32; BLOCK];
    let mut right = vec![0f32; BLOCK];
    let mut phase = 0.0f32;
    let step = std::f32::consts::TAU * 220.0 / SR as f32;
    let mut fill_and_process = |engine: &mut namir_engine::AudioEngine| -> Duration {
        for i in 0..BLOCK {
            let s = 0.5 * phase.sin();
            phase += step;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
            left[i] = s;
            right[i] = s;
        }
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
        let started = Instant::now();
        engine.process(&mut io);
        started.elapsed()
    };

    // Settle: both initial handovers complete here, before anything is recorded.
    for _ in 0..SETTLE_BLOCKS {
        fill_and_process(&mut engine);
    }

    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            pin_worker_away_from_the_measurement_core();
            work();
            done.store(true, std::sync::atomic::Ordering::Release);
        })
    };

    let block_period = Duration::from_secs_f64(BLOCK as f64 / SR as f64);
    let mut durations = Vec::with_capacity(max_blocks);
    let started = Instant::now();
    for b in 0..max_blocks {
        let target = started + block_period * (b as u32);
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        }
        durations.push(fill_and_process(&mut engine));
        if done.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
    }
    worker.join().expect("the loading thread panicked");

    durations.truncate(durations.len().saturating_sub(TRAILING_BLOCKS_DISCARDED));
    durations
}

/// Moves the calling (worker) thread off the core [`pin_to_measurement_core`] put the audio loop
/// on — see the section comment above for why a thread that inherited that affinity would make
/// this arm measure the harness rather than the code.
fn pin_worker_away_from_the_measurement_core() {
    let Some(ids) = core_affinity::get_core_ids() else {
        return;
    };
    if ids.len() < 2 {
        return;
    }
    let audio = std::env::var("NAMIR_PIN_CORE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4)
        .min(ids.len() - 1);
    let worker = if audio == 0 { 1 } else { audio - 1 };
    core_affinity::set_for_current(ids[worker]);
}

/// NFR-PERF-050's second clause, asserted. Returns nothing — it panics on failure, like every
/// other arm here.
fn measure_the_audio_thread_clause(oversized_path: &Path) {
    // Enough blocks that a repetition is bounded by the load rather than by this cap, and small
    // enough that a load which somehow completed instantly cannot spin here for long.
    const MAX_BLOCKS: usize = 3_000;

    let mut quiet = Vec::new();
    let mut loading = Vec::new();
    let mut loads_completed = 0usize;

    for _ in 0..AUDIO_ARM_REPS {
        // The quiet arm: the same audio loop, the same length as the loading arm's typical
        // window, with nothing happening beside it. `SETTLE_BLOCKS` of quiet come first in both.
        quiet.extend(audio_blocks_while(QUIET_ARM_BLOCKS, || {
            std::thread::sleep(Duration::from_secs_f64(
                BLOCK as f64 / SR as f64 * QUIET_ARM_BLOCKS as f64,
            ));
        }));

        let path = oversized_path.to_path_buf();
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&completed);
        loading.extend(audio_blocks_while(MAX_BLOCKS, move || {
            let c = ctx();
            let (_engine, endpoint) = build_default_engine(&c).unwrap();
            let cache = ResourceCache::new();
            let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);
            let outcome = instance.load(&cache, Target::Nam, LoadSource::File(path));
            if matches!(outcome.result, JobResult::Loaded { .. }) {
                flag.store(true, std::sync::atomic::Ordering::Release);
            }
        }));
        if completed.load(std::sync::atomic::Ordering::Acquire) {
            loads_completed += 1;
        }
    }

    assert_eq!(
        loads_completed, AUDIO_ARM_REPS,
        "only {loads_completed} of {AUDIO_ARM_REPS} repetitions completed their 50 MB load -- \
         this arm would then be reporting an audio loop that ran beside nothing"
    );
    assert!(
        loading.len() > 100 && quiet.len() > 100,
        "too few measured blocks to compare ({} loading, {} quiet)",
        loading.len(),
        quiet.len()
    );

    quiet.sort_unstable();
    loading.sort_unstable();
    let quiet_p50 = percentile(&quiet, 0.50);
    let quiet_p999 = percentile(&quiet, 0.999);
    let loading_p50 = percentile(&loading, 0.50);
    let loading_p999 = percentile(&loading, 0.999);

    println!(
        "\nNFR-PERF-050, second clause: per-block audio-thread cost with and without a 50 MB \n\
         load in flight on another core ({} / {} blocks measured, tail {TRAILING_BLOCKS_DISCARDED} \n\
         blocks per repetition discarded so the incoming size fixture's own inference cost is not \n\
         charged to the load).",
        loading.len(),
        quiet.len()
    );
    println!("  quiet  : p50 {quiet_p50:>9.2?}   p99.9 {quiet_p999:>9.2?}");
    println!("  loading: p50 {loading_p50:>9.2?}   p99.9 {loading_p999:>9.2?}");

    let ratio = loading_p999.as_secs_f64() / quiet_p999.as_secs_f64();
    println!(
        "  p99.9 ratio: {ratio:.2}x (bound {AUDIO_DELAY_FACTOR:.1}x -- a delay detector, not a \
         performance measurement)"
    );
    assert!(
        ratio <= AUDIO_DELAY_FACTOR,
        "NFR-PERF-050: with a 50 MB load in flight the audio thread's p99.9 per-block time was \
         {loading_p999:.2?} against a quiet baseline of {quiet_p999:.2?} ({ratio:.2}x, bound \
         {AUDIO_DELAY_FACTOR:.1}x) -- the load delayed the audio thread. D-2.4: one reading on a \
         machine that was not verified quiet is not evidence of a regression; re-run pinned \
         (NAMIR_PIN_CORE) >= 5 times before believing this"
    );
}

/// How long the quiet baseline arm runs for, in blocks — chosen to be the same order as a 50 MB
/// load's own window so the two p99.9 figures are taken over comparable sample sizes.
const QUIET_ARM_BLOCKS: usize = 300;

/// Blocks run before either arm starts recording, so the initial model/IR handovers are finished
/// and are not measured as though they were the arm's own.
const SETTLE_BLOCKS: usize = 200;

// trace-partial: NFR-PERF-050
// uncovered: NFR-PERF-050 — the "regardless of duration" clause is asserted at M14 only over the
// uncovered: window in which the load itself is in flight: the audio-thread arm below discards the
// uncovered: blocks after Instance::load returns, because the incoming fixture is a 430-channel
// uncovered: size model whose inference cost is not a load cost. So the offer-and-crossfade half of
// uncovered: a 50 MB changeover is measured by nothing at that size — handover_crossfade.rs, which
// uncovered: owns that half under FR-NAM-070, drives a standard model; closes M8
fn main() {
    pin_to_measurement_core();

    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory for the file arms");

    println!("NFR-PERF-050: resource load time (worker-side, wall-clock -- D-2.5's scoping)");
    println!(
        "D-2.4: pin away from CPU 0/2 (this run used NAMIR_PIN_CORE={}), verify the machine is \n\
         quiet, and take >= 5 repetitions before quoting anything below.",
        std::env::var("NAMIR_PIN_CORE").unwrap_or_else(|_| "4 (default)".into())
    );
    println!(
        "Each payload is timed twice: `bytes` = LoadSource::Bytes (parse + prepare only), `file` \n\
         = LoadSource::File (fs::metadata + fs::read of the whole file inside the window, which \n\
         is the arm the requirement's \"for files up to 50 MB\" wording is about)."
    );
    println!(
        "D-2.5 conditions: the file arms read from {} -- WARM page cache, the harness having \n\
         written each file moments earlier; name that volume's filesystem and whether a real-time \n\
         anti-malware scanner was active alongside any figure quoted from here.\n",
        dir.display()
    );

    let standard_model: Arc<[u8]> = Arc::from(
        generate(WaveNetShape::Standard, 1)
            .expect("standard fixture should generate")
            .to_json_bytes()
            .into_boxed_slice(),
    );
    let standard_path = plant(&dir, "standard.nam", &standard_model);
    measure_bytes("standard model", Target::Nam, &standard_model, 20);
    measure_file("standard model", Target::Nam, &standard_path, 20);

    let ir_len = 2 * SR as usize;
    let left = decaying_noise(ir_len, 21, 8_000.0);
    let right = decaying_noise(ir_len, 22, 8_000.0);
    let stereo_ir: Arc<[u8]> = Arc::from(to_stereo_wav_bytes(&left, &right, SR).into_boxed_slice());
    let ir_path = plant(&dir, "stereo-2s.wav", &stereo_ir);
    measure_bytes("2 s stereo IR", Target::Ir, &stereo_ir, 20);
    measure_file("2 s stereo IR", Target::Ir, &ir_path, 20);

    println!(
        "\nHonest caveat (see generate_oversized_uncalibrated's own doc comment): a 50 MB .nam \n\
         file is not a shape the NAM ecosystem actually produces -- measured only because \n\
         NFR-PERF-050 states its ceiling in file-size terms, not as a realistic case. Fewer \n\
         repetitions than the two arms above: each one is itself slow to generate and to load."
    );
    let oversized: Arc<[u8]> = Arc::from(
        generate_oversized_uncalibrated(OVERSIZED_CHANNELS, 1)
            .to_json_bytes()
            .into_boxed_slice(),
    );
    let oversized_path = plant(&dir, "oversized.nam", &oversized);
    let oversized_bytes_max = measure_bytes(
        "~50 MB oversized (uncalibrated)",
        Target::Nam,
        &oversized,
        5,
    );
    let oversized_file_max = measure_file(
        "~50 MB oversized (uncalibrated)",
        Target::Nam,
        &oversized_path,
        5,
    );

    println!(
        "\nThe 50 MB read, isolated: {:.2?} on the worst repetition, the difference between that \n\
         payload's file and bytes arms. A floor on what a cold-cache read would add, not an \n\
         estimate of it (D-2.5 condition 1).",
        oversized_file_max.saturating_sub(oversized_bytes_max)
    );

    // The sentence's second clause. Runs last because it is the slowest arm and because a reader
    // watching the output should see the first clause's rows before this one starts.
    measure_the_audio_thread_clause(&oversized_path);

    let _ = std::fs::remove_dir_all(&dir);

    println!(
        "\nPASS: every arm's slowest repetition stayed inside NFR-PERF-050's \
         {NFR_PERF_050_CEILING:?} ceiling, for files up to 50 MB read from a real path as well as \
         for payloads already in memory -- and a 50 MB load in flight on another core did not \
         raise the audio thread's own p99.9 per-block time beyond {AUDIO_DELAY_FACTOR:.1}x its \
         quiet baseline. What remains unmeasured at 50 MB scale is the offer-and-crossfade half; \
         see this file's own `// uncovered:` field."
    );
}
