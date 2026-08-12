//! **FR-CLAP-130's `I` half**: "the plugin shall never block the audio thread waiting on the GUI
//! thread, the file system, or the host, under any user action including model loading, preset
//! recall and library scanning" — driven against the real `NamirClapPlugin` through the real C
//! vtable, with `process()` running continuously on its own thread while a host-driven preset
//! recall, a model load and a library-scan stand-in all contend underneath it.
//!
//! **Before writing a test against `support`, read that module's doc comment — in particular the
//! HAZARD about `start_library_scan` and the developer's real library index.** This file is the
//! one that gets closest to tripping it, and deliberately does not: see "the scan axis is a
//! proxy" below.
//!
//! # The shape
//!
//! One instance, activated at 48 kHz, its [`clack_host::prelude::StartedPluginAudioProcessor`]
//! moved onto a dedicated thread that loops `process()` for the whole run. The test's own thread
//! stays the CLAP `[main-thread]` and drives the user actions from there, which is exactly the
//! thread separation the requirement is about.
//!
//! **One warm-up block runs on the audio thread outside [`support::audio_section`], before the
//! measured loop.** `crate::audio`'s `process()` calls
//! `namir_platform::elevate_current_thread_priority` once, on its first invocation
//! (`src/audio.rs`'s `priority_elevated` latch), and that call allocates. It is D-13.2 behaving
//! exactly as designed — once per processor, not once per callback — so the harness's allocation
//! probe has to see the *steady state*, not the first block. Warming up on the audio thread
//! rather than on the main thread also means the priority elevation lands on the thread that
//! actually processes, as it would in a host.
//!
//! # The yardstick is a ratio against this run's own injected stall, never a wall-clock budget
//!
//! `cargo test` runs test binaries concurrently and this one shares a machine with whatever else
//! is building; an absolute "no block took longer than N ms" bound would flake, and D-2.1/D-2.5
//! forbid quoting a figure gathered under `AllocDisabler` in a debug build as a performance
//! number anyway. So the bound is derived from a stall this test injects and *measures on this
//! machine*: `max block time × MAX_BLOCK_STALL_DIVISOR ≤ measured stall`. Both sides move
//! together when the machine is slow or busy, which is the property an absolute budget lacks.
//!
//! `namir-worker`'s own `tests/rt_stress.rs` is the house precedent for the style and for what
//! the bound means — "this is a blocking detector, not a benchmark" (its `MAX_BLOCK_MULTIPLE`
//! doc comment). The failure mode being detected is total, not marginal: if `process()` took
//! `SharedInner`'s instance mutex, a block landing inside a recall would wait out the *whole*
//! remaining lock hold, so a coupled implementation scores a ratio near 1 against a bound of
//! 1/5. Nothing about this test is sensitive to a few milliseconds either way.
//!
//! # The injected stall, and why it is a real one
//!
//! [`stall_payload_bytes`] writes a syntactically valid WaveNet `.nam` whose `weights` array is
//! millions of floats long and whose declared topology needs seven of them. Every layer of the
//! load path therefore runs at full cost — `std::fs::read`, two `blake3` passes (the resolver's
//! content check and `ResourceCache`'s key), `sniff_architecture`'s full scan and
//! `NamFile::parse`'s materialisation of the whole vector — and only then does
//! `PreparedWaveNet::from_file` reject it with `WEIGHT_COUNT_MISMATCH`. The rejection is the
//! point: the stall is real, but no model reaches the engine, so the audio thread's own per-block
//! work is identical before, during and after, and a slower block cannot be explained away as
//! "it had more DSP to do".
//!
//! `namir_worker::recall::Instance::recall` does all of that **inside**
//! `SharedInner::with_instance`, i.e. holding the instance mutex (`src/worker_jobs.rs`'s
//! `spawn_recall`), on a `namir-worker` pool thread. That is the ~half-second lock hold this test
//! exists to prove the audio thread is not behind. [`time_one_stall`] measures the same work
//! directly, uncontended, before the audio loop starts, and [`calibrate`] grows the payload until
//! that measurement reaches [`TARGET_STALL`] — so the yardstick is neither guessed nor assumed,
//! and a fast machine gets a bigger file rather than a tighter bound.
//!
//! # The three enumerated user actions, and the one that is a proxy
//!
//! - **Preset recall** — the host's own `clap_plugin_state.load`, called on the `[main-thread]`
//!   with a real `namir_state` document (FR-CLAP-050's path, `src/state_ext.rs`).
//! - **Model loading** — that document names the payload above by absolute path, so the recall
//!   the load spawns performs a genuine read/hash/parse of a multi-megabyte model file.
//! - **Library scanning — a proxy, and this is not glossed over.** `support`'s HAZARD is that a
//!   scan started from a test walks zero configured roots, concludes every indexed path is gone,
//!   and erases the developer's real `library-index.json`. So no scan is started. What the scan
//!   axis contributes to the audio thread is a pool job holding the instance mutex while doing
//!   file I/O and hashing, and that is exactly what the recall job above already does; the raw
//!   `std::fs::read` + `ResourceCache::shared()` round the main thread runs alongside it adds the
//!   same file-system and process-global-cache pressure a scan's own jobs would. The requirement
//!   names three actions and this test drives two of them for real. The `uncovered:` field says
//!   so.
//!
//! What each round *dispatched* is counted; that the recall job then ran is by construction
//! (`spawn_recall` is the last statement of `PluginStateImpl::load`) rather than observed, because
//! `SharedInner` is `pub(crate)` and exposes no completion signal, no notice list and no job
//! counter to an integration test. What is asserted instead is that the audio loop stayed alive
//! across several stall-lengths of wall time, so "no block was slow" cannot mean "the audio thread
//! had already stopped before the contention started".
//!
//! # Why the whole file is behind `host-ext-tests` (D-18.7)
//!
//! Adopting a preset into a live instance is only reachable through `clap_plugin_state.load`, and
//! the host half of that extension exists only under `clack-extensions`' `clack-host` feature.
//! There is no un-gated route: `activate()`'s own replay (`src/audio.rs`) calls `spawn_recall`,
//! but `snapshot_state()` returns no resource reference until something has adopted one, so
//! without the `state` extension the recall returns immediately and nothing contends. CI runs
//! `cargo test -p namir-clap --features host-ext-tests` as a required step precisely so
//! feature-gated tests are executed rather than merely compiled.
#![cfg(feature = "host-ext-tests")]

mod support;

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clack_extensions::state::PluginState;
use clack_host::prelude::StoppedPluginAudioProcessor;
use namir_core::ContentHash;
use namir_state::{Document, FileRef, State};
use namir_worker::ResourceCache;

use support::{
    DEFAULT_MAX_BLOCK, DEFAULT_SAMPLE_RATE, StereoBuffers, TestHost, activate, all_finite,
    audio_section, config, instantiate_default, main_thread_handle, require_plugin_extension,
    sine_1k,
};

/// The block size every measured block uses — the harness's own maximum, so the buffers never
/// reallocate and `StereoBuffers::process_block` stays allocation-free inside the audio section.
const BLOCK_FRAMES: u32 = DEFAULT_MAX_BLOCK;

/// A short pause between blocks, so the audio thread behaves like a callback with a period rather
/// than a spin loop. `process()` elevates its own thread's priority (D-13.2), and an elevated
/// thread spinning flat out for the length of this run would starve the very worker threads whose
/// interference this test is trying to observe. Excluded from every measurement.
const BLOCK_PACE: Duration = Duration::from_millis(2);

/// How many `[main-thread]` user-action rounds run against the live instance.
const CONTENTION_ROUNDS: usize = 6;

/// How long one injected stall should last before the audio loop starts. [`calibrate`] grows the
/// payload until [`time_one_stall`] reaches this, so a fast machine reads a bigger file rather
/// than measuring against a shorter yardstick.
const TARGET_STALL: Duration = Duration::from_millis(400);

/// The floor below which the yardstick would be too short to mean anything, checked so this test
/// cannot pass by having injected nothing. Only reachable if [`calibrate`] hit [`MAX_WEIGHTS`]
/// without getting there.
const MIN_MEANINGFUL_STALL: Duration = Duration::from_millis(150);

/// The whole assertion: a measured block must be no more than this fraction of one injected
/// stall. See this file's doc comment for why the number is not delicate.
const MAX_BLOCK_STALL_DIVISOR: u32 = 5;

/// Where [`calibrate`] starts. Roughly an 8 MB file.
const INITIAL_WEIGHTS: usize = 2_000_000;

/// The ceiling [`calibrate`] will not grow past — roughly a 96 MB file, and still two orders of
/// magnitude under `namir-nam`'s own `MAX_TOTAL_WEIGHTS` ceiling, so the file is always rejected
/// for its weight *count* rather than for its size.
const MAX_WEIGHTS: usize = 24_000_000;

/// How many payloads [`calibrate`] may write before giving up and using what it has.
const CALIBRATION_STEPS: usize = 4;

/// A hard stop for the audio loop, so a wedged main thread fails this test rather than hanging
/// `cargo test`. Never reached in a passing run.
const AUDIO_LOOP_CAP: Duration = Duration::from_secs(60);

// -------------------------------------------------------------------------------------------
// The injected stall.
// -------------------------------------------------------------------------------------------

/// A scratch directory of this process's own, removed at the end of a passing run.
fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("namir-clap-rt-blocking-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory must be creatable");
    dir
}

/// A syntactically valid WaveNet `.nam` document with `weight_count` weights and a topology that
/// consumes seven, so the full parse runs and the semantic check then rejects it — see this
/// file's doc comment for why the rejection is the point.
///
/// Built by string concatenation rather than through `serde_json::json!`, which would need a
/// `Vec<serde_json::Value>` of the same length first: this is the one place in the file where the
/// allocation actually matters.
fn stall_payload_bytes(weight_count: usize) -> Vec<u8> {
    const HEAD: &str = concat!(
        r#"{"architecture":"WaveNet","version":"0.5.4","config":{"layers":[{"input_size":1,"#,
        r#""condition_size":1,"head_size":1,"channels":1,"kernel_size":1,"dilations":[1],"#,
        r#""activation":"Tanh","gated":false,"head_bias":false}],"head_scale":0.5,"#,
        r#""head":null},"weights":["#,
    );
    const TAIL: &str = r#"],"sample_rate":48000}"#;

    assert!(
        weight_count > 0,
        "a zero-length weight vector parses too fast to stall anything"
    );
    let mut json = String::with_capacity(HEAD.len() + weight_count * 4 + TAIL.len() + 1);
    json.push_str(HEAD);
    // Decimal literals, not integers: parsing the float is the work being bought here.
    let chunk = "0.5,".repeat(1024);
    for _ in 0..weight_count / 1024 {
        json.push_str(&chunk);
    }
    for _ in 0..weight_count % 1024 {
        json.push_str("0.5,");
    }
    json.pop(); // the trailing comma
    json.push_str(TAIL);
    json.into_bytes()
}

/// Writes a payload of `weight_count` weights and returns its path and content hash (P7: the
/// reference's identity, which `Instance::recall`'s resolver verifies before loading anything).
fn write_payload(dir: &Path, weight_count: usize) -> (PathBuf, ContentHash) {
    let bytes = stall_payload_bytes(weight_count);
    let hash = ContentHash::of(&bytes);
    let path = dir.join("stall.nam");
    std::fs::write(&path, &bytes).expect("the payload must be writable");
    (path, hash)
}

/// One injected stall, measured uncontended.
///
/// This is deliberately the *same* work `namir_worker::Instance::recall` performs while holding
/// `SharedInner`'s instance mutex — read the file, then hash and parse it through the process-wide
/// [`ResourceCache`] — and deliberately one `blake3` pass short of it, since `recall`'s resolver
/// hashes the bytes once more to check them against the reference. Short by construction, so the
/// yardstick under-states the real lock hold rather than flattering it.
///
/// Uses `ResourceCache::shared()` — the very cache every `namir-clap` instance in this process
/// resolves to (FR-CLAP-090) — rather than a private one, so this doubles as genuine contention
/// on a lock the plugin's own worker jobs take. A failed load leaves no entry behind
/// (`namir-worker`'s own `a_load_failure_leaves_no_entry_behind`), so nothing is polluted.
fn time_one_stall(path: &Path, cache: &ResourceCache) -> Duration {
    let started = Instant::now();
    let bytes = std::fs::read(path).expect("the payload must be readable");
    let result = cache.get_or_load_nam(&bytes);
    let elapsed = started.elapsed();
    assert!(
        result.is_err(),
        "the stall payload must be rejected -- a payload that actually loaded would change the \
         audio thread's own per-block work and invalidate every measurement below"
    );
    elapsed
}

/// Grows the payload until one stall reaches [`TARGET_STALL`], and returns the final payload's
/// path, hash and measured duration. Bounded by [`CALIBRATION_STEPS`] and [`MAX_WEIGHTS`].
fn calibrate(dir: &Path, cache: &ResourceCache) -> (PathBuf, ContentHash, Duration) {
    let mut weights = INITIAL_WEIGHTS;
    let (mut path, mut hash) = write_payload(dir, weights);
    let mut stall = time_one_stall(&path, cache);

    for _ in 1..CALIBRATION_STEPS {
        if stall >= TARGET_STALL || weights >= MAX_WEIGHTS {
            break;
        }
        // Scale by how far short the last measurement fell, clamped so one anomalously fast or
        // slow reading cannot make the next payload absurd in either direction.
        let factor = (TARGET_STALL.as_secs_f64() / stall.as_secs_f64()).ceil() as usize;
        weights = weights.saturating_mul(factor.clamp(2, 6)).min(MAX_WEIGHTS);
        (path, hash) = write_payload(dir, weights);
        stall = time_one_stall(&path, cache);
    }

    (path, hash, stall)
}

/// The preset a host hands to `clap_plugin_state.load`: FR-STATE-070's absolute-path candidate
/// pointing at the payload, with the matching content hash so the resolver actually accepts it
/// and proceeds to the (slow, rejected) load.
fn preset_bytes(path: &Path, hash: ContentHash) -> Vec<u8> {
    let mut state = State::defaults();
    state.nam = Some(FileRef {
        hash,
        library_relative: None,
        absolute: Some(path.to_string_lossy().into_owned()),
        display_name: "stall.nam".to_string(),
        embedded: None,
    });
    state.write_onto(&Document::empty()).to_pretty_bytes()
}

// -------------------------------------------------------------------------------------------
// The audio thread.
// -------------------------------------------------------------------------------------------

/// What the audio thread hands back. The processor comes back with it because
/// `PluginInstance::deactivate` must receive it before the instance is dropped (`support`'s own
/// contract), and `stop_processing` belongs on the thread that was processing.
struct AudioRun {
    processor: StoppedPluginAudioProcessor<TestHost>,
    max_block: Duration,
    blocks: usize,
    process_errors: usize,
    non_finite_blocks: usize,
}

// -------------------------------------------------------------------------------------------
// The test.
// -------------------------------------------------------------------------------------------

/// FR-CLAP-130's **`I` half**, driven end to end: see this file's doc comment for the shape, the
/// yardstick and the one enumerated action that is covered by proxy.
///
/// **The `S` half is `tests/fr_clap_130_rt_static.rs`** (added M14), and FR-CLAP-130's tag moved
/// there with it: a requirement whose method is "S plus I" is better booked against the artifact
/// that was missing than against the one that already existed. This test is unchanged and still
/// does the whole of what it always did.
#[test]
fn fr_clap_130_no_block_waits_on_a_preset_recall_a_model_load_or_the_host() {
    let dir = scratch_dir();
    let cache = ResourceCache::shared();

    // ---- The yardstick, measured on this machine before anything else runs. ----
    let (payload, payload_hash, stall) = calibrate(&dir, &cache);
    assert!(
        stall >= MIN_MEANINGFUL_STALL,
        "the injected stall calibrated to only {stall:?}, under the {MIN_MEANINGFUL_STALL:?} \
         floor -- the payload never got slow enough for the comparison below to mean anything"
    );
    let preset = preset_bytes(&payload, payload_hash);

    // ---- One live instance, processing on its own thread. ----
    let (_entry, mut instance) = instantiate_default();
    let state_ext = require_plugin_extension::<PluginState>(&mut instance);
    let stopped = activate(&mut instance, config(DEFAULT_SAMPLE_RATE, 1, BLOCK_FRAMES));
    let mut processor = stopped.start_processing().expect("processing must start");

    let mut bufs = StereoBuffers::default_size();
    let tone = sine_1k(bufs.max_frames(), DEFAULT_SAMPLE_RATE, 0.25);
    bufs.fill_input(|_channel, frame| tone[frame]);

    let stop = Arc::new(AtomicBool::new(false));
    let audio = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            // The warm-up block, outside every audio section -- see this file's doc comment.
            let warm_up = bufs.process_block(&mut processor, BLOCK_FRAMES);
            let mut process_errors = usize::from(warm_up.is_err());
            let mut max_block = Duration::ZERO;
            let mut blocks = 0usize;
            let mut non_finite_blocks = 0usize;

            let run_started = Instant::now();
            while !stop.load(Ordering::Acquire) && run_started.elapsed() < AUDIO_LOOP_CAP {
                bufs.poison_output(f32::NAN);

                let block_started = Instant::now();
                let status = audio_section(|| bufs.process_block(&mut processor, BLOCK_FRAMES));
                let elapsed = block_started.elapsed();

                max_block = max_block.max(elapsed);
                blocks += 1;
                if status.is_err() {
                    process_errors += 1;
                }
                let frames = BLOCK_FRAMES as usize;
                if (0..support::CHANNELS).any(|c| !all_finite(&bufs.output(c)[..frames])) {
                    non_finite_blocks += 1;
                }

                std::thread::sleep(BLOCK_PACE);
            }

            AudioRun {
                processor: processor.stop_processing(),
                max_block,
                blocks,
                process_errors,
                non_finite_blocks,
            }
        })
    };

    // ---- The `[main-thread]` user actions, concurrent with all of the above. ----
    let mut recalls = 0usize;
    let mut file_rounds = 0usize;
    let mut worst_main_thread_call = Duration::ZERO;
    for _ in 0..CONTENTION_ROUNDS {
        // Preset recall + model loading: `state_ext::load` adopts the document and dispatches
        // `spawn_recall`, whose job then holds the instance mutex across the whole slow load.
        let started = Instant::now();
        let mut handle = main_thread_handle(&mut instance);
        state_ext
            .load(&mut handle, &mut Cursor::new(&preset[..]))
            .expect("the host-driven state load must succeed");
        worst_main_thread_call = worst_main_thread_call.max(started.elapsed());
        recalls += 1;

        // The file-system and shared-cache pressure a library scan's own jobs would apply, run
        // while the recall job above is still in flight -- see this file's doc comment.
        let _ = time_one_stall(&payload, &cache);
        file_rounds += 1;
    }
    stop.store(true, Ordering::Release);

    let run = audio.join().expect("the audio thread must not panic");
    instance.deactivate(run.processor);
    drop(instance); // `clap_plugin.destroy`, which joins this instance's worker pool
    let _ = std::fs::remove_dir_all(&dir);

    // ---- The concurrency this test claims to have exercised actually happened. ----
    assert_eq!(
        recalls, CONTENTION_ROUNDS,
        "every preset recall must have been driven"
    );
    assert_eq!(
        file_rounds, CONTENTION_ROUNDS,
        "every file-system round must have been driven"
    );
    assert!(
        run.blocks > 50,
        "the audio loop ran only {} blocks -- too few to have overlapped the contention",
        run.blocks
    );
    // Ratio-based liveness, for the same reason the headline bound is: the audio loop has to have
    // been running across several stall-lengths of wall time, or "no block was slow" would only
    // mean it finished before the contention started. `BLOCK_PACE * blocks` under-states the real
    // span (it excludes the blocks themselves), so this is a floor, not an estimate.
    let audio_span = BLOCK_PACE * u32::try_from(run.blocks).unwrap_or(u32::MAX);
    assert!(
        audio_span >= stall * 2,
        "the audio loop spanned at most {audio_span:?}, under two {stall:?} stalls -- it did not \
         stay alive long enough to have overlapped the contention it is being measured against"
    );

    // ---- Nothing degraded while it happened. ----
    assert_eq!(
        run.process_errors, 0,
        "{} block(s) returned CLAP_PROCESS_ERROR while a load, a recall and file I/O were in \
         flight",
        run.process_errors
    );
    assert_eq!(
        run.non_finite_blocks, 0,
        "{} block(s) left a non-finite sample in the output -- either the plugin produced one or \
         it did not write the block at all",
        run.non_finite_blocks
    );

    // ---- The requirement. Zero allocations is asserted by `audio_section` itself, per block;
    // this is the "never blocks" half. ----
    assert!(
        run.max_block * MAX_BLOCK_STALL_DIVISOR <= stall,
        "the worst block took {:?}, more than 1/{} of the {stall:?} stall this run injected \
         (worst [main-thread] call: {worst_main_thread_call:?}, {} blocks) -- the audio thread \
         waited on the GUI thread, the file system or the host",
        run.max_block,
        MAX_BLOCK_STALL_DIVISOR,
        run.blocks
    );
}
