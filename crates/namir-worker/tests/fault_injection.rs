//! FR-ERR-040 (`Verify: I` — "inject a fault into each non-audio subsystem"): "A panic or
//! unexpected fault in a non-audio thread shall not take down the host process. In the plugin
//! configuration, Namir shall contain such a fault and continue passing audio, degraded if
//! necessary."
//!
//! # What this file adds, and what it deliberately cannot reach
//!
//! Until M14 this requirement's only artifact was `pool.rs`'s
//! `a_panicking_job_is_contained_and_the_pool_keeps_serving`, which injects one fault into one
//! subsystem — the generic worker thread pool — and runs no audio at all. The method says **each**
//! non-audio subsystem, and the requirement's second sentence is about audio continuing. This file
//! is the breadth half and the audio half together: a live [`namir_engine::AudioEngine`] processes
//! a continuous sine on this thread for the whole run, while faults are injected one after another
//! into every non-audio subsystem `namir-worker` can see.
//!
//! | Subsystem | The fault injected |
//! |---|---|
//! | Worker thread pool (D-16.3) | A job that panics, followed by a job that must still run |
//! | Library index persistence | A corrupt index file at the store's own path |
//! | Library scanner | A configured root that is a regular file, and a corpus of garbage `.nam` bytes |
//! | Resource cache | Bytes that are not a `.nam` at all, and bytes that are not a WAV |
//! | Resource load (file path) | A path that does not exist, and a path that is a directory |
//! | State document parsing | A `.namirpreset` that is not JSON, and one that is JSON of the wrong shape |
//! | Preset recall | A `State` whose references resolve to nothing |
//!
//! Two subsystems the uncovered field on the tag names are **not** here, for reasons that are
//! structural rather than oversights. **Settings I/O** lives in `namir-app`
//! (`crate::settings::load`/`save`), which `namir-worker` cannot see — D-5.1 runs that edge the
//! other way — so its fault injection is `crates/namir-app/tests/settings_faults.rs`. **The GUI
//! thread** cannot be reached from here either, and the requirement's own second sentence scopes
//! the containment claim to "the plugin configuration", which is `namir-clap`'s `gui.rs` and its
//! host harness. Both are named in the `// uncovered:` field above the covering test.
//!
//! # The audio assertion, and why it is dropout rather than a timing bound
//!
//! "Continue passing audio, degraded if necessary" is a statement about the signal, not about a
//! deadline: a fault that made the engine stall would show here as silence. FR-CHAIN-040 makes an
//! unloaded stage a dry *passthrough* rather than silence, so the sine reaches the output through
//! every one of these faults — which is what makes a hard "no block was silent" assertion
//! achievable rather than a statistical one. This is `rt_stress.rs`'s own
//! `DROPOUT_PEAK_THRESHOLD`, reused rather than re-invented.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use namir_core::{ChannelConfig, ContentHash, SampleRate};
use namir_engine::{PrepareContext, StageIo, build_default_engine};
use namir_state::{Document, FileRef, State};
use namir_worker::library::LibraryService;
use namir_worker::recall::ResourceRecall;
use namir_worker::{
    EngineConfig, Instance, JobResult, LoadSource, ResourceCache, Target, ThreadPool, WorkerError,
};

const SR: u32 = 48_000;
const BLOCK: usize = 64;
/// `rt_stress.rs`'s own floor, reused — see this file's module doc comment.
const DROPOUT_PEAK_THRESHOLD: f32 = 1e-4;

fn ctx() -> PrepareContext {
    PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Mono).unwrap()
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "namir-worker-fault-injection-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// `rt_stress.rs`'s own check, reused: an error's catalogue id has to look genuinely namespaced
/// (`"worker.file.unreadable"`) rather than be an ad-hoc string. FR-ERR-040's containment is only
/// worth anything if what comes back out of a contained fault is reportable (FR-ERR-020).
fn assert_catalogued(label: &str, error: &WorkerError) {
    assert!(
        !error.code.id.is_empty() && error.code.id.contains('.'),
        "{label}: the contained fault produced an uncatalogued error id {:?}",
        error.code.id
    );
}

/// Runs the audio loop on its own thread until told to stop, reporting whether any block was
/// silent and how many blocks ran.
struct AudioProbe {
    stop: Arc<AtomicBool>,
    silent_blocks: Arc<AtomicUsize>,
    blocks: Arc<AtomicUsize>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl AudioProbe {
    fn start() -> AudioProbe {
        let stop = Arc::new(AtomicBool::new(false));
        let silent_blocks = Arc::new(AtomicUsize::new(0));
        let blocks = Arc::new(AtomicUsize::new(0));
        let (ready_tx, ready_rx) = mpsc::channel();

        let handle = {
            let stop = Arc::clone(&stop);
            let silent_blocks = Arc::clone(&silent_blocks);
            let blocks = Arc::clone(&blocks);
            std::thread::spawn(move || {
                let c = ctx();
                let (mut engine, _endpoint) = build_default_engine(&c).expect("build the engine");
                let mut buf = [0.0f32; BLOCK];
                let mut phase = 0.0f32;
                let step = std::f32::consts::TAU * 220.0 / SR as f32;
                let block_period = Duration::from_secs_f64(BLOCK as f64 / SR as f64);
                ready_tx.send(()).expect("the test thread is waiting");
                while !stop.load(Ordering::Acquire) {
                    for s in buf.iter_mut() {
                        *s = 0.5 * phase.sin();
                        phase += step;
                        if phase > std::f32::consts::TAU {
                            phase -= std::f32::consts::TAU;
                        }
                    }
                    let mut channels: [&mut [f32]; 1] = [&mut buf];
                    let mut io = StageIo::new(&mut channels, BLOCK);
                    engine.process(&mut io);
                    let peak = io.channel(0).iter().fold(0.0f32, |m, s| m.max(s.abs()));
                    if peak <= DROPOUT_PEAK_THRESHOLD {
                        silent_blocks.fetch_add(1, Ordering::Relaxed);
                    }
                    blocks.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(block_period);
                }
            })
        };

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the audio probe should start");
        AudioProbe {
            stop,
            silent_blocks,
            blocks,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> (usize, usize) {
        self.stop.store(true, Ordering::Release);
        let handle = self.handle.take().expect("finish is called once");
        assert!(handle.join().is_ok(), "the audio probe thread panicked");
        (
            self.blocks.load(Ordering::Relaxed),
            self.silent_blocks.load(Ordering::Relaxed),
        )
    }
}

// trace-partial: FR-ERR-040
// uncovered: FR-ERR-040 — two of the subsystems the method's "each" quantifies over are outside
// uncovered: this crate and unreached from here: the GUI thread, whose containment is namir-clap's
// uncovered: gui.rs and reachable only through that crate's clack-host harness, and with it the
// uncovered: requirement's second sentence, which scopes "shall contain such a fault and continue
// uncovered: passing audio" to the plugin configuration specifically — the audio probe below is a
// uncovered: bare AudioEngine, not a plugin instance driven by a host's process() call. Settings
// uncovered: I/O is covered separately, in namir-app/tests/settings_faults.rs; closes M8
#[test]
fn a_fault_in_any_non_audio_subsystem_is_contained_and_audio_keeps_flowing() {
    let dir = scratch();
    let audio = AudioProbe::start();

    // ---- Subsystem 1: the worker thread pool (D-16.3). ----
    {
        // The hook is silenced for exactly this section, and no wider: the injected fault is a
        // deliberate panic whose backtrace would otherwise make the suite's output unreadable, but
        // silencing it across the whole test would also swallow this test's *own* assertion
        // failures and turn a real regression into an unexplained FAILED line.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let pool = ThreadPool::with_threads(1);
        let (tx, rx) = mpsc::channel();
        pool.spawn(|| panic!("an injected fault in a pool job"));
        pool.spawn(move || tx.send(42u32).unwrap());
        let served = rx.recv_timeout(Duration::from_secs(5)).ok();

        std::panic::set_hook(previous_hook);
        assert_eq!(
            served,
            Some(42),
            "the pool stopped serving after a job panicked"
        );
    }

    // ---- Subsystem 2: library index persistence -- a corrupt index file. ----
    let index_path = dir.join("library-index.json");
    std::fs::write(&index_path, b"{ this is not a library index").unwrap();
    let (service, warnings) = LibraryService::open(index_path.clone(), vec![dir.clone()]);
    assert!(
        !warnings.is_empty(),
        "a corrupt index must degrade with a warning rather than silently"
    );
    for w in &warnings {
        assert_catalogued("corrupt index", w);
    }
    assert_eq!(
        service.snapshot().len(),
        0,
        "a corrupt index must degrade to an empty one, not to whatever parsed"
    );

    // ---- Subsystem 3: the library scanner -- a root that is a file, and garbage `.nam` bytes. ----
    {
        // Garbage that *looks* scannable: the right extensions, contents that parse as nothing.
        std::fs::write(dir.join("garbage.nam"), b"\x00\x01\x02 not json at all").unwrap();
        std::fs::write(dir.join("garbage.wav"), b"RIFFnope").unwrap();
        let not_a_directory = dir.join("root-that-is-a-file");
        std::fs::write(&not_a_directory, b"a regular file posing as a scan root").unwrap();

        let pool = ThreadPool::with_threads(1);
        let (service, _) = LibraryService::open(
            dir.join("scan-index.json"),
            vec![not_a_directory, dir.clone()],
        );
        let (tx, rx) = mpsc::channel();
        let started = service.start_scan(
            &pool,
            |_| {},
            move |outcome| {
                let _ = tx.send(outcome);
            },
        );
        assert!(started.is_some(), "the scan should have started");
        let outcome = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the scan must finish rather than hang or take the process down");
        for w in &outcome.warnings {
            assert_catalogued("scanner", w);
        }
    }

    // ---- Subsystem 4: the resource cache -- payloads that are not what they claim. ----
    {
        let cache = ResourceCache::new();
        let c = ctx();
        // `.err()` rather than `expect_err`: the `Ok` side is a prepared resource, and neither
        // `PreparedNam` nor `PreparedIr` is `Debug` (nor should be -- they hold megabytes of
        // weights and FFT plans).
        let nam_error = cache
            .get_or_load_nam(b"not a nam file")
            .err()
            .expect("garbage must not parse as a model");
        assert_catalogued("cache/nam", &nam_error);
        let ir_error = cache
            .get_or_load_ir(b"not a wav file", c.sample_rate(), c.max_block_size())
            .err()
            .expect("garbage must not parse as an IR");
        assert_catalogued("cache/ir", &ir_error);
    }

    // ---- Subsystem 5: a resource load by path -- a missing file and a directory. ----
    {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        for (label, path) in [
            ("missing file", dir.join("definitely-not-here.nam")),
            ("a directory", dir.clone()),
        ] {
            let outcome = instance.load(&cache, Target::Nam, LoadSource::File(path));
            match outcome.result {
                JobResult::Failed(e) => assert_catalogued(label, &e),
                other => panic!("{label}: expected a contained failure, got {other:?}"),
            }
        }
    }

    // ---- Subsystem 6: state document parsing. ----
    {
        // An oversized document is its own injected fault, and the one a hostile or truncated file
        // is most likely to be: `Document::parse` refuses past `MAX_DOCUMENT_BYTES` rather than
        // allocating whatever the bytes ask for.
        let oversized = vec![b'{'; namir_state::MAX_DOCUMENT_BYTES + 1];
        for (label, bytes) in [
            ("not JSON", b"}{".as_slice()),
            (
                "a JSON array where an object is required",
                b"[1,2,3]".as_slice(),
            ),
            ("past MAX_DOCUMENT_BYTES", oversized.as_slice()),
        ] {
            let err = Document::parse(bytes)
                .err()
                .unwrap_or_else(|| panic!("{label}: must not parse as a state document"));
            assert!(
                !format!("{err}").is_empty(),
                "{label}: a state error must say something"
            );
        }
    }

    // ---- Subsystem 7: preset recall against references that resolve to nothing. ----
    {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);
        let roots: Vec<PathBuf> = Vec::new();
        let resolver = namir_library::RootsOnlyResolver::new(&roots);

        let mut state = State::defaults();
        state.nam = Some(FileRef {
            hash: ContentHash::of(b"never existed"),
            library_relative: None,
            absolute: Some(dir.join("gone.nam").to_string_lossy().into_owned()),
            display_name: "gone.nam".to_string(),
            embedded: None,
        });
        let outcome = instance.recall(&cache, &state, &resolver);
        match &outcome.nam {
            ResourceRecall::Missing { missing, .. } => {
                assert_eq!(missing.display_name, "gone.nam");
                assert!(
                    !missing.warning().code.id.is_empty(),
                    "a missing reference must carry a catalogue code"
                );
            }
            other => panic!("expected a contained Missing, got {other:?}"),
        }
        assert_eq!(outcome.commands_not_delivered, 0);
    }

    // ---- And through all of it, audio never stopped. ----
    let (blocks, silent) = audio.finish();
    assert!(
        blocks > 50,
        "the audio probe ran only {blocks} blocks -- too few for 'audio kept flowing' to mean \
         anything about the faults injected beside it"
    );
    assert_eq!(
        silent, 0,
        "{silent} of {blocks} blocks went silent while faults were being injected into the \
         non-audio subsystems -- FR-ERR-040's 'continue passing audio' clause"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
