//! FR-LIB-020 — "Library scanning shall occur off the audio thread and shall not block the user
//! interface. Progress shall be visible and the scan cancellable" — driven end to end at the scale
//! that requirement's own `*Verify:*` method names: `I with a synthetic library of at least 10 000
//! files`. One test, four clauses, one corpus.
//!
//! # What M9a left open, and what this closes
//!
//! Until this file existed, the "off the audio thread" clause had no evidence at that scale. The
//! only place a real [`namir_engine::AudioEngine`] and a real scan ran at once was
//! `tests/rt_stress.rs`'s axis C, whose corpus is six files. That is deliberate there and was
//! deliberately left alone: `write_small_scan_corpus`'s own doc comment wants *many fast scan
//! cycles* inside that test's run window rather than one slow one, so re-pointing it at
//! `namir-fixtures`' 10,000-file shared corpus would have destroyed the axis it exists to run
//! rather than extended it. This is the new, separate harness M9b owed that clause.
//!
//! # Why a second test binary rather than a second `#[test]` in `rt_stress.rs`
//!
//! Two reasons, both about not perturbing what already passes. A `#[global_allocator]` is one per
//! compiled *binary* and every file under `tests/` is its own binary, so installing
//! [`AllocDisabler`] here is an independent installation rather than a conflicting second copy —
//! the same argument `rt_stress.rs`'s module doc comment makes for duplicating `namir-engine`'s
//! `rt_harness`, and the reason the small amount of harness code below is copied rather than
//! shared. And `cargo test` runs test *binaries* one at a time but the `#[test]`s inside one
//! binary in parallel: a full-corpus scan plus a spinning audio loop living in `rt_stress.rs`
//! would run concurrently with `nfr_rt_010_...`'s own two-second budget and its
//! `MAX_BLOCK_MULTIPLE` bound, turning an unrelated test's timing assertions into collateral of
//! this one's load.
//!
//! # How each of FR-LIB-020's four clauses is spanned here, at 10,000 files
//!
//! 1. **"shall occur off the audio thread".** This thread is the audio thread for the whole
//!    duration of the scan: it processes 64-sample blocks through a live `AudioEngine` inside
//!    [`audio_section`] (D-7.5's `assert_no_alloc` harness) from before `start_scan` until the
//!    terminal `ScanOutcome` arrives. Zero allocations, no silent block, and no block taking
//!    drastically longer than one block's period — a scan running *on* this thread, or behind a
//!    lock this thread takes, could not leave all three true.
//! 2. **"shall not block the user interface".** A second thread plays the UI's part at ~60 Hz for
//!    the same duration, doing per frame exactly what `namir-ui`'s library view does per frame:
//!    take a [`LibraryService::snapshot`], read [`LibraryService::is_scanning`], and run
//!    `namir_library::filter` over the snapshot (the `LibraryViewState::ensure_filtered` scan).
//!    Every frame is timed, and the test asserts the UI thread genuinely observed the scan in
//!    flight rather than only frames before and after it. **The honest limit**, stated here rather
//!    than left to be discovered: no *rendered* frame is involved. `namir-ui` sits below this
//!    crate in D-5.1's table and nothing at this layer may drive egui, so what is proven is that
//!    the scan never blocks the calls a UI makes; that a real window keeps painting during one is
//!    FR-UI-060's own requirement, `docs/02-architecture.md` §22's **R-12**, and the human residue
//!    recorded in `docs/manual-tests/fr-lib-020-ui-responsiveness-during-scan.md` — which under
//!    D-18.6 is supplementary evidence and never the traced artifact, FR-LIB-020 being `Verify: I`.
//! 3. **"Progress shall be visible".** The full scan's `on_progress` calls are counted and their
//!    peak `files_examined` recorded: more than one call (so at least one came from `start_scan`'s
//!    50 ms cadence branch rather than the unconditional terminal report alone) and a report that
//!    actually reached the corpus's full file count.
//! 4. **"the scan cancellable".** A second scan over the same corpus, cancelled immediately, must
//!    report `complete == false` and no removals — with the audio loop still running, so
//!    cancellation is exercised under the same concurrency as completion.
//!
//! Deliberately **not** measured here: anything anyone could quote as a performance figure. This
//! binary is a debug build running under `AllocDisabler`, so per D-2.5 every duration below is a
//! blocking detector with a generous ceiling, never a benchmark.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

use assert_no_alloc::AllocDisabler;

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::{AudioEngine, PrepareContext, StageIo, build_default_engine};
use namir_library::{Query, filter};
use namir_worker::ThreadPool;
use namir_worker::library::{LibraryService, ScanOutcome};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

/// D-7.5's harness, duplicated per this file's own module doc comment.
fn audio_section<T>(f: impl FnOnce() -> T) -> T {
    assert_no_alloc::reset_violation_count();
    let result = assert_no_alloc::assert_no_alloc(f);
    assert_eq!(
        assert_no_alloc::violation_count(),
        0,
        "allocation occurred inside an audio section while a 10,000-file scan was running"
    );
    result
}

const SR: u32 = 48_000;
const BLOCK: usize = 64;

/// FR-NAM-070's dropout threshold, the same figure `rt_stress.rs` reuses rather than re-invents.
const DROPOUT_PEAK_THRESHOLD: f32 = 1e-4;

/// A generous multiple of one block's period (`BLOCK / SR`), copied from `rt_stress.rs` along with
/// its reasoning: **not a performance measurement**, since this binary runs under `AllocDisabler`
/// and a wall-clock figure gathered here would misrepresent NFR-PERF-010 if quoted as one
/// (D-2.1/D-2.5). What it detects is the audio thread genuinely blocked on something — which is
/// exactly the failure mode FR-LIB-020's "off the audio thread" clause forbids.
const MAX_BLOCK_MULTIPLE: u32 = 200;

/// The UI thread's frame interval — 60 Hz, the rate `namir-ui` is written against and twice the
/// 50 ms cadence `LibraryService`'s progress callback fires at, so a frame lands on both sides of
/// every progress report.
const UI_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// The ceiling one simulated UI frame's *work* (snapshot + `is_scanning` + filter) may take.
/// Deliberately **not** FR-UI-060's 100 ms frame budget: this is a debug binary under
/// `AllocDisabler` on a machine that may be running the rest of `cargo test --workspace`
/// concurrently, so a figure gathered here could not certify a frame budget and must not be read
/// as one (D-2.5). It is sized to catch the thing this clause is about — a UI call blocked behind
/// the scan, which would park for the scan's whole multi-second duration, not for 250 ms.
const MAX_UI_FRAME: Duration = Duration::from_millis(250);

/// How long the audio loop will keep waiting for a scan's terminal outcome before failing. Matches
/// `library.rs`'s own `FULL_CORPUS_SCAN_BUDGET` and for the same recorded reason: reading and
/// content-hashing 10,000 files while sharing a disk with the rest of the test suite is a
/// different order of work from every other scan in this crate's tests, and a budget sized for the
/// median run intermittently times out in CI.
const SCAN_BUDGET: Duration = Duration::from_secs(180);

/// The corpus seed, matching `library.rs`'s two full-corpus tests so all three share one entry in
/// `namir-fixtures`' on-disk corpus cache instead of each cold-building their own 10,000 files.
const CORPUS_SEED: u64 = 1;

/// Never dropped, for the reason `rt_stress.rs`'s module doc comment gives at length:
/// `assert_no_alloc`'s violation counters are thread-local and `ThreadPool::drop` joins its
/// threads, so a pool that outlives the process avoids joining a thread whose thread-local state
/// is already tearing down.
fn pool() -> &'static ThreadPool {
    static POOL: OnceLock<ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| ThreadPool::with_threads(1))
}

fn ctx() -> PrepareContext {
    PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Mono).unwrap()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "namir-worker-lib-scale-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The audio thread, carried across both phases of the test so the engine, the sine's phase and
/// the accumulated evidence are continuous rather than restarted per scan.
struct AudioThread {
    engine: AudioEngine,
    buf: [f32; BLOCK],
    phase: f32,
    blocks_run: usize,
    dropout_windows: usize,
    max_block_duration: Duration,
}

impl AudioThread {
    fn new() -> AudioThread {
        let (engine, _endpoint) = build_default_engine(&ctx()).unwrap();
        AudioThread {
            engine,
            buf: [0.0; BLOCK],
            phase: 0.0,
            blocks_run: 0,
            dropout_windows: 0,
            max_block_duration: Duration::ZERO,
        }
    }

    /// One block: generate a 220 Hz sine, process it inside [`audio_section`], and record what the
    /// assertions at the end of the test read.
    fn process_one_block(&mut self) {
        let step = std::f32::consts::TAU * 220.0 / SR as f32;
        for s in self.buf.iter_mut() {
            *s = 0.5 * self.phase.sin();
            self.phase += step;
            if self.phase > std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            }
        }
        let mut channels: [&mut [f32]; 1] = [&mut self.buf];
        let mut io = StageIo::new(&mut channels, BLOCK);

        let started = Instant::now();
        audio_section(|| self.engine.process(&mut io));
        let elapsed = started.elapsed();

        let peak = io.channel(0).iter().fold(0.0f32, |m, s| m.max(s.abs()));
        self.max_block_duration = self.max_block_duration.max(elapsed);
        if peak <= DROPOUT_PEAK_THRESHOLD {
            self.dropout_windows += 1;
        }
        self.blocks_run += 1;
    }

    /// Processes blocks continuously — unpaced, like `rt_stress.rs`'s own loop, since the point is
    /// to cover the scan's entire duration rather than to model real-time pacing — until the scan
    /// reports its terminal outcome.
    fn run_until_scan_finishes(&mut self, rx: &mpsc::Receiver<ScanOutcome>) -> ScanOutcome {
        let started = Instant::now();
        loop {
            match rx.try_recv() {
                Ok(outcome) => return outcome,
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("the scan job dropped its completion channel without reporting")
                }
            }
            assert!(
                started.elapsed() < SCAN_BUDGET,
                "the scan did not report an outcome within {SCAN_BUDGET:?} ({} blocks processed \
                 meanwhile)",
                self.blocks_run
            );
            self.process_one_block();
        }
    }
}

/// What the simulated UI thread reports back. Every field is written by that thread and read once,
/// after it has been joined.
#[derive(Default)]
struct UiEvidence {
    frames: AtomicUsize,
    max_frame_nanos: AtomicU64,
    saw_scan_in_flight: AtomicBool,
    stop: AtomicBool,
}

/// One simulated UI frame's work: exactly the calls `namir-ui`'s library view makes per frame
/// (see this file's module doc comment on what this does and does not prove).
fn ui_frame(service: &LibraryService, evidence: &UiEvidence) {
    let started = Instant::now();
    let index = service.snapshot();
    let scanning = service.is_scanning();
    let query = Query::parse("nam");
    let hits = filter(&index, &query).count();
    let entries = index.len();
    let elapsed = started.elapsed();

    std::hint::black_box((hits, entries));
    if scanning {
        evidence.saw_scan_in_flight.store(true, Ordering::Release);
    }
    evidence
        .max_frame_nanos
        .fetch_max(elapsed.as_nanos() as u64, Ordering::Relaxed);
    evidence.frames.fetch_add(1, Ordering::Relaxed);
}

/// FR-LIB-020 end to end against `namir-fixtures`' 10,000-file shared corpus, with a live audio
/// thread and a simulated UI thread running for the scan's whole duration. See this file's module
/// doc comment for how each of the requirement's four clauses is spanned, and for the one thing
/// this cannot reach (a rendered frame, which belongs to FR-UI-060).
// trace: FR-LIB-020
#[test]
fn fr_lib_020_a_ten_thousand_file_scan_blocks_neither_the_audio_thread_nor_the_ui() {
    let corpus = namir_fixtures::library::generate_shared_corpus(CORPUS_SEED)
        .expect("the shared 10,000-file corpus should generate");
    assert!(
        corpus.entries.len() >= 10_000,
        "FR-LIB-020's Verify method names at least 10 000 files; this corpus has {}",
        corpus.entries.len()
    );

    let mut audio = AudioThread::new();
    let block_period = Duration::from_secs_f64(BLOCK as f64 / SR as f64);

    // ---- Phase 1: a full, uncancelled scan of the whole corpus. ----
    let full_dir = temp_dir("full");
    let (service, warnings) =
        LibraryService::open(full_dir.join("index.json"), vec![corpus.root.clone()]);
    assert!(warnings.is_empty(), "first-run open should be clean");
    let service = Arc::new(service);

    let progress_calls = Arc::new(AtomicUsize::new(0));
    let max_files_examined = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();
    {
        let calls = Arc::clone(&progress_calls);
        let examined = Arc::clone(&max_files_examined);
        service
            .start_scan(
                pool(),
                move |progress| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    examined.fetch_max(progress.files_examined, Ordering::Relaxed);
                },
                move |outcome| {
                    let _ = tx.send(outcome);
                },
            )
            .expect("no scan should already be running");
    }

    // The UI thread starts *after* `start_scan` returned, so `is_scanning()` is already true and
    // "did a frame see the scan in flight" is not a race with the pool picking the job up.
    let evidence = Arc::new(UiEvidence::default());
    let ui_thread = {
        let service = Arc::clone(&service);
        let evidence = Arc::clone(&evidence);
        std::thread::spawn(move || {
            while !evidence.stop.load(Ordering::Acquire) {
                ui_frame(&service, &evidence);
                std::thread::sleep(UI_FRAME_INTERVAL);
            }
        })
    };

    let outcome = audio.run_until_scan_finishes(&rx);
    evidence.stop.store(true, Ordering::Release);
    assert!(ui_thread.join().is_ok(), "the simulated UI thread panicked");

    assert!(
        outcome.complete,
        "this scan is never cancelled, so it must run every root to completion"
    );
    assert_eq!(
        outcome.upserted,
        namir_fixtures::library::TOTAL_COUNT,
        "a first scan of the shared corpus must upsert every file in it"
    );
    assert!(!service.is_scanning());

    // ---- Clause 3: progress was visible, at this corpus's scale. ----
    let calls = progress_calls.load(Ordering::Relaxed);
    assert!(
        calls >= 2,
        "a full {}-file scan reported progress {calls} time(s); at least one call must come from \
         the 50 ms cadence branch on top of the unconditional terminal report, or \"progress shall \
         be visible\" is true of the end of the scan only",
        namir_fixtures::library::TOTAL_COUNT
    );
    assert!(
        max_files_examined.load(Ordering::Relaxed) >= namir_fixtures::library::TOTAL_COUNT,
        "the progress reports peaked at {} files examined, short of the {} this corpus holds -- \
         progress that never reaches the scale the requirement names is not progress over it",
        max_files_examined.load(Ordering::Relaxed),
        namir_fixtures::library::TOTAL_COUNT
    );

    // ---- Clause 2: the UI's own calls stayed answerable throughout. ----
    let frames = evidence.frames.load(Ordering::Relaxed);
    let max_ui_frame = Duration::from_nanos(evidence.max_frame_nanos.load(Ordering::Relaxed));
    assert!(
        evidence.saw_scan_in_flight.load(Ordering::Acquire),
        "no simulated UI frame observed the scan in flight, so this run proves nothing about \
         frames drawn *during* a scan"
    );
    // A floor on "the UI thread genuinely kept running while the scan did", not a frame-rate
    // measurement: a debug run of this scan takes seconds and completes ~110 frames, but an
    // optimised run on a warm page cache is several times shorter, and the load-bearing evidence
    // is `saw_scan_in_flight` above rather than any particular count.
    assert!(
        frames >= 5,
        "the simulated UI thread completed only {frames} frame(s) during a full 10,000-file scan"
    );
    assert!(
        max_ui_frame <= MAX_UI_FRAME,
        "a simulated UI frame's work took {max_ui_frame:?} (ceiling {MAX_UI_FRAME:?}) while the \
         scan ran -- a UI call blocked behind the scan, not a slow frame"
    );

    // ---- Phase 2: cancellation, at the same scale, with the audio thread still running. ----
    let cancel_dir = temp_dir("cancel");
    let (cancel_service, _) =
        LibraryService::open(cancel_dir.join("index.json"), vec![corpus.root.clone()]);
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let handle = cancel_service
        .start_scan(
            pool(),
            |_| {},
            move |outcome| {
                let _ = cancel_tx.send(outcome);
            },
        )
        .expect("no scan should already be running");
    handle.cancel();
    let cancelled = audio.run_until_scan_finishes(&cancel_rx);

    assert!(
        !cancelled.complete,
        "cancelling right after start should stop a 10,000-file scan before it finishes"
    );
    assert!(
        cancelled.upserted < namir_fixtures::library::TOTAL_COUNT,
        "a cancelled scan reported every one of the corpus's {} files as upserted, so it did not \
         actually stop early",
        namir_fixtures::library::TOTAL_COUNT
    );
    assert_eq!(
        cancelled.removed, 0,
        "an incomplete scan must never report removals"
    );
    assert!(!cancel_service.is_scanning());

    // ---- Clause 1: the audio thread, across both scans. ----
    assert_eq!(
        audio.dropout_windows, 0,
        "{}/{} blocks were silent while a 10,000-file scan ran -- FR-CHAIN-040's dry passthrough \
         should have kept the sine audible throughout",
        audio.dropout_windows, audio.blocks_run
    );
    assert!(
        audio.max_block_duration <= block_period * MAX_BLOCK_MULTIPLE,
        "a block took {:?}, over {MAX_BLOCK_MULTIPLE}x the block period {block_period:?} -- the \
         audio thread waited on something while the library was being scanned",
        audio.max_block_duration
    );
    assert!(
        audio.blocks_run > 1_000,
        "the audio loop ran only {} blocks across both scans, too few to claim it covered them",
        audio.blocks_run
    );

    let _ = std::fs::remove_dir_all(&full_dir);
    let _ = std::fs::remove_dir_all(&cancel_dir);
}
