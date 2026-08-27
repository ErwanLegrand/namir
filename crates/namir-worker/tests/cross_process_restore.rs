//! FR-STATE-060's M5-resolvable half: "the plugin shall restore identically... across a host
//! restart." M5 has no host to restart (M6 territory), so the resolvable half this crate can
//! actually build is restated as: **two independent OS processes**, given the same saved state,
//! produce bit-identical audio. Neither process shares so much as an `Arc` with the other -- if
//! anything in the recall/prepare/process path depended on process-local state (a random seed
//! never persisted, an uninitialised buffer, iteration order over a `HashMap`), this is the test
//! that would catch it and a same-process round trip would not.
//!
//! # Why this test *is* both processes
//!
//! A `tests/*.rs` integration test compiles to its own binary with no separate "helper binary"
//! mechanism available without adding a `[[bin]]` target purely for testing. The standard
//! workaround, used here: the test binary re-invokes itself via `std::env::current_exe()`,
//! filtered to run only this one test (`--exact <name>`), with an environment variable set. The
//! very first thing the test function does is check that variable -- if set, it is the **child**:
//! it does the actual recall-and-render work and writes raw output bytes to a path named in
//! another environment variable, then returns (no assertions, no further recursion). If unset, it
//! is the **parent**: it writes a shared state file once, spawns two children pointed at it, and
//! asserts their outputs are byte-for-byte identical.
//!
//! # Why the state embeds its resources rather than pointing at library files
//!
//! Exercising FR-STATE-080 at the same time is not incidental convenience -- it is what makes
//! this test possible to write *at all* without also standing up a real library and root
//! directory for two independently-spawned processes to agree on. An embedded model and IR mean
//! each child needs nothing but the one state file: no shared library configuration, no shared
//! root path, nothing this crate's own D-5.1 boundary would call platform-specific. `recall.rs`'s
//! resolver is `namir_library::RootsOnlyResolver::new(&[])` -- deliberately configured with *no*
//! library roots at all, so every external candidate misses and the embedded copy is the only
//! path that can possibly succeed. If FR-STATE-080's fallback were broken, this test would fail
//! with `Missing`, not with mismatched output.

use std::path::{Path, PathBuf};

const CHILD_ENV_VAR: &str = "NAMIR_CROSS_PROCESS_RESTORE_CHILD";
const STATE_PATH_ENV_VAR: &str = "NAMIR_CROSS_PROCESS_RESTORE_STATE_PATH";
const OUTPUT_PATH_ENV_VAR: &str = "NAMIR_CROSS_PROCESS_RESTORE_OUTPUT_PATH";

const SR: u32 = 48_000;
const BLOCK: usize = 64;
const BLOCKS: usize = 200;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "namir-worker-cross-process-restore-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Builds a state whose model and IR references carry only an embedded copy -- no
/// `library_relative`, no `absolute` -- and saves it to `path`.
fn write_shared_state(path: &Path) {
    let model_bytes = namir_fixtures::nam::generate(namir_fixtures::nam::WaveNetShape::Nano, 1)
        .expect("fixture should generate")
        .to_json_bytes();
    let model_hash = namir_core::ContentHash::of(&model_bytes);

    let ir_samples = namir_fixtures::ir::decaying_noise(256, 2, 64.0);
    let ir_bytes = namir_fixtures::ir::to_mono_wav_bytes(&ir_samples, SR);
    let ir_hash = namir_core::ContentHash::of(&ir_bytes);

    let mut state = namir_state::State::defaults();
    state.nam = Some(namir_state::FileRef {
        hash: model_hash,
        library_relative: None,
        absolute: None,
        display_name: "embedded-model.nam".to_string(),
        embedded: Some(namir_state::EmbeddedRef {
            media_type: "application/vnd.namir.nam+json".to_string(),
            data: model_bytes,
        }),
    });
    state.ir = Some(namir_state::FileRef {
        hash: ir_hash,
        library_relative: None,
        absolute: None,
        display_name: "embedded-ir.wav".to_string(),
        embedded: Some(namir_state::EmbeddedRef {
            media_type: "audio/wav".to_string(),
            data: ir_bytes,
        }),
    });

    std::fs::write(path, state.write()).expect("write shared state file");
}

/// Spawns a fresh child process (a re-invocation of this same test binary, filtered to run only
/// this test), points it at `state_path`, and returns its rendered output bytes.
fn run_in_a_fresh_process(state_path: &Path, tag: &str) -> Vec<u8> {
    let output_path = temp_dir().join(format!("output-{tag}.raw"));
    let exe = std::env::current_exe().expect("current_exe should resolve under cargo test");

    let status = std::process::Command::new(&exe)
        .arg("fr_state_060_two_independent_processes_produce_bit_identical_output")
        .arg("--exact")
        .arg("--test-threads=1")
        .env(CHILD_ENV_VAR, "1")
        .env(STATE_PATH_ENV_VAR, state_path)
        .env(OUTPUT_PATH_ENV_VAR, &output_path)
        .status()
        .expect("failed to spawn the child process");
    assert!(
        status.success(),
        "child process (tag {tag}) exited with {status}"
    );

    std::fs::read(&output_path).expect("child should have written its output")
}

/// The child's whole job: read the shared state, build a fresh engine and instance, recall the
/// state (FR-STATE-080's embedded fallback is the only path that can succeed -- see this file's
/// own doc comment), render a fixed number of blocks of a fixed sine, and write every sample as
/// raw little-endian `f32` bytes to the path named in `OUTPUT_PATH_ENV_VAR`.
///
/// Deliberately does not skip the crossfade's own transient before recording: two independent
/// processes given identical inputs and an identical recalled state must reproduce the *same*
/// transient too, bit for bit, not just agree once things settle -- recording from block zero
/// makes that part of what this test proves rather than an assumption it works around.
fn run_as_child() {
    let state_path =
        std::env::var(STATE_PATH_ENV_VAR).expect("state path env var must be set in child mode");
    let output_path =
        std::env::var(OUTPUT_PATH_ENV_VAR).expect("output path env var must be set in child mode");

    let bytes = std::fs::read(&state_path).expect("read shared state file");
    let (state, warnings) = namir_state::State::read(&bytes).expect("state should parse");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    let ctx = namir_engine::PrepareContext::new(
        namir_core::SampleRate::new(SR).unwrap(),
        BLOCK,
        namir_core::ChannelConfig::Mono,
    )
    .unwrap();
    let (mut engine, endpoint) = namir_engine::build_default_engine(&ctx).unwrap();
    let cache = namir_worker::ResourceCache::new();
    let mut instance = namir_worker::Instance::new(namir_worker::EngineConfig { ctx }, endpoint);

    // No library roots at all -- every external candidate misses by construction, so a
    // successful recall here is only possible through FR-STATE-080's embedded fallback. See this
    // file's own doc comment.
    let resolver = namir_library::RootsOnlyResolver::new(&[]);
    instance.recall(&cache, &state, &resolver);

    let mut out = Vec::with_capacity(BLOCKS * BLOCK * 4);
    let mut phase = 0.0f32;
    let step = std::f32::consts::TAU * 220.0 / SR as f32;
    for _ in 0..BLOCKS {
        let mut buf = [0.0f32; BLOCK];
        for s in buf.iter_mut() {
            *s = 0.5 * phase.sin();
            phase += step;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
        }
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
        engine.process(&mut io);
        for s in io.channel(0) {
            out.extend_from_slice(&s.to_le_bytes());
        }
    }

    std::fs::write(&output_path, &out).expect("write child output");
}

/// See this file's own doc comment for the whole design. This one test function is both the
/// parent and the child, distinguished by `CHILD_ENV_VAR` at the top.
// trace-partial: FR-STATE-060
// uncovered: FR-STATE-060 — the method's "save a project, restart the host, reopen" is executed
// uncovered: by nothing: the artifact re-invokes its own test binary twice and drives
// uncovered: namir_worker::Instance directly, never loading namir-clap or calling its state
// uncovered: extension, and it writes both file references with an embedded blob and no path
// uncovered: against an empty resolver, so the requirement's "identity of the loaded model and IR
// uncovered: files" clause is bypassed; closes M8
#[test]
fn fr_state_060_two_independent_processes_produce_bit_identical_output() {
    if std::env::var(CHILD_ENV_VAR).is_ok() {
        run_as_child();
        return;
    }

    let state_path = temp_dir().join("shared_state.namirpreset");
    write_shared_state(&state_path);

    let output_a = run_in_a_fresh_process(&state_path, "a");
    let output_b = run_in_a_fresh_process(&state_path, "b");

    assert!(
        !output_a.is_empty(),
        "the child process should have produced output"
    );
    assert_eq!(
        output_a.len(),
        BLOCKS * BLOCK * 4,
        "unexpected output length"
    );
    assert_eq!(
        output_a, output_b,
        "two independent processes given the same saved state must produce bit-identical output"
    );

    let _ = std::fs::remove_file(&state_path);
}
