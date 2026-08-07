//! NFR-RT-010's three-axis stress test: "with concurrent model loading, preset recall and library
//! scanning" running against a live [`namir_engine::AudioEngine`] processing continuously. M4's
//! close-out left this Partial specifically because the axes it names did not exist yet
//! (`namir-state`/`namir-library` were both M5) -- this is the test that lets it move to Done, and
//! `namir-worker` is the only crate that can see `namir-engine`, `namir-state` and `namir-library`
//! all at once, so this is the only place it could be written.
//!
//! # Why this duplicates `namir-engine`'s `rt_harness.rs` rather than reusing it
//!
//! `namir-engine::rt_harness` is `#[cfg(test)]`-private to its own crate, and even if it were
//! public, a `#[global_allocator]` can only be installed once per compiled *binary*. This file is
//! its own binary (every file under `tests/` is), so installing [`AllocDisabler`] here is a
//! second, independent installation rather than a conflicting second copy of the first one --
//! exactly the thing that "forced `rt_harness` to be duplicated" inside `namir-engine` itself
//! (`nam.rs`, `ir.rs` and this crate's own dev-dependency all repeat the same dozen lines) stops
//! being a problem once the boundary is a whole binary rather than a module.
//!
//! # Why the pool used for the scanning axis is never dropped
//!
//! `assert_no_alloc`'s violation counters are **thread-local** -- the property that makes this
//! whole test possible, since [`namir_worker::pool::ThreadPool`]'s worker threads (loading,
//! parsing, hashing files) allocate constantly while the audio thread sits inside
//! [`audio_section`], and only the audio thread's own counter is ever asserted against. A spike
//! run before this test was built confirmed those counters behave correctly across a pool
//! thread's ordinary lifetime, but `ThreadPool::drop` joins its threads, and joining a thread
//! whose thread-local state has already begun tearing down is exactly the kind of order-dependent
//! hazard worth designing away rather than trusting to timing. The mitigation: a `OnceLock`-held
//! pool that this test never drops at all -- its threads live until the process exits, so the
//! join-at-drop path this paragraph is worried about never runs during this test's lifetime.
//!
//! # The five things asserted
//!
//! 1. Zero allocations in every `audio_section` (the whole point).
//! 2. No dropout -- every block's peak stays above [`DROPOUT_PEAK_THRESHOLD`], the same numeric
//!    threshold `namir-engine`'s `fr_nam_070_swapping_models_under_a_sine_has_no_discontinuity_or_dropout`
//!    test uses, reused rather than re-invented. Achievable as a hard zero here (not "mostly")
//!    because FR-CHAIN-040 makes an unloaded stage a dry *passthrough*, not silence -- the test's
//!    sine keeps reaching the output even at the instant both stages are being swapped.
//! 3. No panic, and every error either axis produced is catalogue-coded (`ErrorCode`-backed, not
//!    an ad-hoc string) -- checked by inspecting every `JobResult`/`RecallOutcome` this test
//!    collects, not merely by the absence of a panic.
//! 4. No block exceeds a generous multiple of one block's period. **This is not a performance
//!    measurement** -- see [`MAX_BLOCK_MULTIPLE`]'s own doc comment -- it exists only to catch the
//!    audio thread genuinely stalled behind something, which nothing in this design should cause.
//! 5. Counters proving all three axes actually produced work, so this test cannot pass because the
//!    concurrency it claims to exercise never materialised.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use assert_no_alloc::AllocDisabler;

use namir_core::{ChannelConfig, ContentHash, SampleRate};
use namir_engine::{PrepareContext, StageIo, build_default_engine};
use namir_state::{EmbeddedRef, FileRef, State};
use namir_worker::library::LibraryService;
use namir_worker::recall::ResourceRecall;
use namir_worker::{
    EngineConfig, Instance, JobResult, LoadSource, ResourceCache, Target, ThreadPool, WorkerError,
};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

/// D-7.5's harness, duplicated per this file's own module doc comment.
fn audio_section<T>(f: impl FnOnce() -> T) -> T {
    assert_no_alloc::reset_violation_count();
    let result = assert_no_alloc::assert_no_alloc(f);
    assert_eq!(
        assert_no_alloc::violation_count(),
        0,
        "allocation occurred inside an audio section"
    );
    result
}

const SR: u32 = 48_000;
const BLOCK: usize = 64;

/// How long the audio loop runs for. Generous enough for all three worker axes to genuinely
/// interleave with it many times over; short enough this test does not dominate `cargo test
/// --workspace`'s wall time.
const RUN_FOR: Duration = Duration::from_secs(2);

/// FR-NAM-070's own dropout threshold, reused rather than re-invented -- see this file's module
/// doc comment.
const DROPOUT_PEAK_THRESHOLD: f32 = 1e-4;

/// A generous multiple of one block's period (`BLOCK / SR`). **Not a performance measurement**:
/// this binary runs under `AllocDisabler`, which is itself measurably slower than a release
/// build, and a wall-clock figure gathered here would misrepresent NFR-PERF-010 if quoted as one
/// (D-2.1/D-2.5's own rule). What this bound actually detects is the audio thread genuinely
/// blocked on something -- a lock a future change accidentally introduced -- which every existing
/// per-block operation (ring push/pop, `Chain::process`) is wait-free and should never do.
const MAX_BLOCK_MULTIPLE: u32 = 200;

/// Never dropped -- see this file's module doc comment for why that matters.
fn pool() -> &'static ThreadPool {
    static POOL: OnceLock<ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| ThreadPool::with_threads(2))
}

fn ctx() -> PrepareContext {
    PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Mono).unwrap()
}

fn model_bytes(seed: u64) -> Vec<u8> {
    namir_fixtures::nam::generate(namir_fixtures::nam::WaveNetShape::Nano, seed)
        .expect("fixture should generate")
        .to_json_bytes()
}

fn ir_bytes(seed: u64) -> Vec<u8> {
    let taps = namir_fixtures::ir::decaying_noise(256, seed, 64.0);
    namir_fixtures::ir::to_mono_wav_bytes(&taps, SR)
}

fn embedded_ref(display_name: &str, data: Vec<u8>, media_type: &str) -> FileRef {
    let hash = ContentHash::of(&data);
    FileRef {
        hash,
        library_relative: None,
        absolute: None,
        display_name: display_name.to_string(),
        embedded: Some(EmbeddedRef {
            media_type: media_type.to_string(),
            data,
        }),
    }
}

/// A small, freshly-written directory of `.nam` files for the scanning axis. Deliberately not
/// `namir-fixtures`' 10,000-file shared corpus: this test wants *many fast scan cycles* within
/// [`RUN_FOR`], not one slow one, and a handful of files is enough to exercise
/// `namir_library::Scanner` genuinely running on a pool thread concurrently with the audio loop.
fn write_small_scan_corpus() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "namir-worker-rt-stress-corpus-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..6u64 {
        std::fs::write(dir.join(format!("model_{i}.nam")), model_bytes(100 + i)).unwrap();
    }
    dir
}

/// Records whether `error`'s catalogue id looks genuinely namespaced (`"worker.file.unreadable"`,
/// `"nam.load.malformed"`, ...) rather than an ad-hoc string -- the check behind this test's
/// "every job outcome is catalogue-coded" claim.
fn check_catalogued(error: &WorkerError, uncatalogued: &AtomicUsize) {
    if error.code.id.is_empty() || !error.code.id.contains('.') {
        uncatalogued.fetch_add(1, Ordering::Relaxed);
    }
}

fn check_job_result(result: &JobResult, uncatalogued: &AtomicUsize) {
    match result {
        JobResult::Failed(e) | JobResult::NotDelivered(e) => check_catalogued(e, uncatalogued),
        JobResult::Loaded {
            warning: Some(e), ..
        } => check_catalogued(e, uncatalogued),
        JobResult::Loaded { warning: None, .. } | JobResult::Unloaded { .. } => {}
    }
}

fn check_recall_outcome(nam: &ResourceRecall, ir: &ResourceRecall, uncatalogued: &AtomicUsize) {
    for r in [nam, ir] {
        match r {
            ResourceRecall::Unloaded(o) | ResourceRecall::Loaded(o) => {
                check_job_result(&o.result, uncatalogued);
            }
            ResourceRecall::Missing { unload, missing } => {
                check_job_result(&unload.result, uncatalogued);
                if missing.warning().code.id.is_empty() {
                    uncatalogued.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

#[test]
fn nfr_rt_010_three_axes_run_concurrently_with_zero_audio_thread_allocation() {
    let c = ctx();
    let (mut engine, endpoint) = build_default_engine(&c).unwrap();
    let instance = Arc::new(Mutex::new(Instance::new(EngineConfig { ctx: c }, endpoint)));
    let cache = Arc::new(ResourceCache::new());
    let stop = Arc::new(AtomicBool::new(false));
    let uncatalogued_errors = Arc::new(AtomicUsize::new(0));

    // ---- Axis A: model loading (Instance::load, directly). ----
    let loads_completed = Arc::new(AtomicUsize::new(0));
    let load_thread = {
        let instance = Arc::clone(&instance);
        let cache = Arc::clone(&cache);
        let stop = Arc::clone(&stop);
        let loads_completed = Arc::clone(&loads_completed);
        let uncatalogued_errors = Arc::clone(&uncatalogued_errors);
        std::thread::spawn(move || {
            let models: Vec<Vec<u8>> = (0..4).map(model_bytes).collect();
            let mut i = 0usize;
            while !stop.load(Ordering::Acquire) {
                let bytes = models[i % models.len()].clone();
                i += 1;
                let outcome = instance.lock().unwrap().load(
                    &cache,
                    Target::Nam,
                    LoadSource::Bytes(Arc::from(bytes.into_boxed_slice())),
                );
                check_job_result(&outcome.result, &uncatalogued_errors);
                if matches!(outcome.result, JobResult::Loaded { .. }) {
                    loads_completed.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };

    // ---- Axis B: preset recall (Instance::recall, exercising R4's load/unload delegation and
    // R-7's serialisation under real concurrent pressure from Axis A on the same instance). ----
    let recalls_completed = Arc::new(AtomicUsize::new(0));
    let recall_thread = {
        let instance = Arc::clone(&instance);
        let cache = Arc::clone(&cache);
        let stop = Arc::clone(&stop);
        let recalls_completed = Arc::clone(&recalls_completed);
        let uncatalogued_errors = Arc::clone(&uncatalogued_errors);
        std::thread::spawn(move || {
            // Two states, alternated: B additionally names an IR the A-only state does not --
            // every other recall therefore unloads or loads the Ir stage, not just the Nam one.
            let mut state_a = State::defaults();
            state_a.nam = Some(embedded_ref(
                "a.nam",
                model_bytes(201),
                "application/vnd.namir.nam+json",
            ));
            let mut state_b = State::defaults();
            state_b.nam = Some(embedded_ref(
                "b.nam",
                model_bytes(202),
                "application/vnd.namir.nam+json",
            ));
            state_b.ir = Some(embedded_ref("b.wav", ir_bytes(203), "audio/wav"));

            // No library roots configured -- both states resolve purely through FR-STATE-080's
            // embedded fallback, exercised under concurrency rather than in isolation this time.
            let resolver = namir_library::RootsOnlyResolver::new(&[]);

            let mut i = 0usize;
            while !stop.load(Ordering::Acquire) {
                let state = if i.is_multiple_of(2) {
                    &state_a
                } else {
                    &state_b
                };
                i += 1;
                let outcome = instance.lock().unwrap().recall(&cache, state, &resolver);
                check_recall_outcome(&outcome.nam, &outcome.ir, &uncatalogued_errors);
                if matches!(outcome.nam, ResourceRecall::Loaded(_))
                    || matches!(outcome.ir, ResourceRecall::Loaded(_))
                {
                    recalls_completed.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };

    // ---- Axis C: library scanning (LibraryService driven on the never-dropped pool). ----
    let scans_completed = Arc::new(AtomicUsize::new(0));
    let scan_thread = {
        let stop = Arc::clone(&stop);
        let scans_completed = Arc::clone(&scans_completed);
        std::thread::spawn(move || {
            let scan_dir = write_small_scan_corpus();
            let index_path = scan_dir.join("index.json");
            let (service, _warnings) = LibraryService::open(index_path, vec![scan_dir.clone()]);
            while !stop.load(Ordering::Acquire) {
                let (tx, rx) = std::sync::mpsc::channel();
                let started = service.start_scan(
                    pool(),
                    |_| {},
                    move |outcome| {
                        let _ = tx.send(outcome);
                    },
                );
                if started.is_none() {
                    // A previous scan is still finishing (unexpected at this corpus size, but
                    // handled rather than assumed away) -- brief backoff, try again.
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                if let Ok(outcome) = rx.recv_timeout(Duration::from_secs(5))
                    && outcome.complete
                {
                    scans_completed.fetch_add(1, Ordering::Relaxed);
                }
            }
            let _ = std::fs::remove_dir_all(&scan_dir);
        })
    };

    // ---- The audio thread: this test's own thread, the whole time the three axes above run. ----
    let mut buf = [0.0f32; BLOCK];
    let mut phase = 0.0f32;
    let step = std::f32::consts::TAU * 220.0 / SR as f32;
    let mut max_block_duration = Duration::ZERO;
    let mut dropout_windows = 0usize;
    let mut blocks_run = 0usize;
    let block_period = Duration::from_secs_f64(BLOCK as f64 / SR as f64);

    let run_started = Instant::now();
    while run_started.elapsed() < RUN_FOR {
        for s in buf.iter_mut() {
            *s = 0.5 * phase.sin();
            phase += step;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
        }
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = StageIo::new(&mut channels, BLOCK);

        let block_started = Instant::now();
        audio_section(|| engine.process(&mut io));
        let elapsed = block_started.elapsed();
        max_block_duration = max_block_duration.max(elapsed);

        let peak = io.channel(0).iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak <= DROPOUT_PEAK_THRESHOLD {
            dropout_windows += 1;
        }
        blocks_run += 1;
    }

    stop.store(true, Ordering::Release);
    let load_join = load_thread.join();
    let recall_join = recall_thread.join();
    let scan_join = scan_thread.join();

    // ---- 3: no panic. ----
    assert!(load_join.is_ok(), "the model-loading thread panicked");
    assert!(recall_join.is_ok(), "the recall thread panicked");
    assert!(scan_join.is_ok(), "the scanning thread panicked");

    // ---- 3 (continued): every error either axis produced was catalogue-coded. ----
    assert_eq!(
        uncatalogued_errors.load(Ordering::Relaxed),
        0,
        "an uncatalogued (non-namespaced) error occurred during the run"
    );

    // ---- 2: no dropout, anywhere in the whole run. ----
    assert_eq!(
        dropout_windows, 0,
        "{dropout_windows}/{blocks_run} blocks were silent while loading, recall and scanning \
         ran concurrently -- FR-CHAIN-040's dry passthrough should have kept the sine audible \
         through every handover"
    );

    // ---- 4: no block took drastically longer than one block's period. Not a performance
    // measurement -- see MAX_BLOCK_MULTIPLE's own doc comment. ----
    assert!(
        max_block_duration <= block_period * MAX_BLOCK_MULTIPLE,
        "a block took {max_block_duration:?}, over {MAX_BLOCK_MULTIPLE}x the block period \
         {block_period:?} -- the audio thread waited on something"
    );

    // ---- 5: all three axes actually produced work -- without this, 1/2/3/4 above could all
    // pass on a run where the concurrency this test claims to exercise never materialised. ----
    let loads = loads_completed.load(Ordering::Relaxed);
    let recalls = recalls_completed.load(Ordering::Relaxed);
    let scans = scans_completed.load(Ordering::Relaxed);
    assert!(
        loads >= 3,
        "model loading axis did not run enough to prove real concurrency: {loads} completed loads"
    );
    assert!(
        recalls >= 3,
        "preset recall axis did not run enough to prove real concurrency: {recalls} completed \
         recalls"
    );
    assert!(
        scans >= 1,
        "library scanning axis never completed a single scan during the run"
    );
    assert!(
        blocks_run > 100,
        "the audio loop itself did not run enough blocks to be a meaningful stress run: \
         {blocks_run}"
    );
}
