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
//!
//! # ...and why a second scenario, added M14, does the opposite
//!
//! An embedded state carries its resources *with* it, so nothing about restoring one depends on
//! finding the right file — which means the embedded scenario cannot say anything about
//! FR-STATE-060's "**including the identity of the loaded model and IR files**" clause. That was
//! the gap this file's `// uncovered:` field named from M9a until M14.
//! `fr_state_060_a_path_referenced_state_restores_the_same_files_in_a_fresh_process` closes it by
//! running the same two-process comparison against a state that names its resources only by path
//! (library-relative *and* absolute, no embedded copy), with decoy files of the same kind planted
//! in the same directory. Its children configure a real library root, so locating and
//! content-verifying the recorded files is the only way either of them can render at all.
//!
//! The two scenarios are complements, not a duplicate and a better one: between them they cover
//! both of FR-STATE-070's resolution regimes, and the embedded one remains the case that needs no
//! shared filesystem configuration.

use std::path::{Path, PathBuf};

const CHILD_ENV_VAR: &str = "NAMIR_CROSS_PROCESS_RESTORE_CHILD";
const STATE_PATH_ENV_VAR: &str = "NAMIR_CROSS_PROCESS_RESTORE_STATE_PATH";
const OUTPUT_PATH_ENV_VAR: &str = "NAMIR_CROSS_PROCESS_RESTORE_OUTPUT_PATH";
/// Set only by the path-referenced scenario (added M14): the one library root the child
/// configures its resolver with. Absent means the embedded scenario, whose resolver has no roots
/// at all -- see this file's own doc comment.
const ROOT_ENV_VAR: &str = "NAMIR_CROSS_PROCESS_RESTORE_ROOT";

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
/// `test_name`), points it at `state_path`, and returns its rendered output bytes. `root`, when
/// given, is the one library root the child configures its resolver with — see [`ROOT_ENV_VAR`].
fn run_in_a_fresh_process(
    test_name: &str,
    state_path: &Path,
    root: Option<&Path>,
    tag: &str,
) -> Vec<u8> {
    let output_path = temp_dir().join(format!("output-{tag}.raw"));
    let exe = std::env::current_exe().expect("current_exe should resolve under cargo test");

    let mut command = std::process::Command::new(&exe);
    command
        .arg(test_name)
        .arg("--exact")
        .arg("--test-threads=1")
        .env(CHILD_ENV_VAR, "1")
        .env(STATE_PATH_ENV_VAR, state_path)
        .env(OUTPUT_PATH_ENV_VAR, &output_path);
    if let Some(root) = root {
        command.env(ROOT_ENV_VAR, root);
    }
    let status = command.status().expect("failed to spawn the child process");
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

    // The embedded scenario configures no roots at all -- every external candidate misses by
    // construction, so a successful recall there is only possible through FR-STATE-080's embedded
    // fallback (see this file's own doc comment). The path-referenced scenario (added M14) sets
    // ROOT_ENV_VAR instead, and its state carries no embedded data at all, so the only way its
    // recall can succeed is by resolving a reference to a real file *whose content hash matches
    // what the state recorded* -- which is this requirement's "identity of the loaded model and IR
    // files" clause, enforced inside `recall::locate` and asserted here.
    let roots: Vec<PathBuf> = std::env::var_os(ROOT_ENV_VAR)
        .map(|r| vec![PathBuf::from(r)])
        .unwrap_or_default();
    let by_path = !roots.is_empty();
    let resolver = namir_library::RootsOnlyResolver::new(&roots);
    let outcome = instance.recall(&cache, &state, &resolver);
    if by_path {
        for (slot, recall) in [("nam", &outcome.nam), ("ir", &outcome.ir)] {
            assert!(
                matches!(recall, namir_worker::recall::ResourceRecall::Loaded(_)),
                "the {slot} reference must resolve to a real file whose content hash matches what \
                 the state recorded; got {recall:?}"
            );
        }
    }

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

/// Builds a state whose model and IR references carry **only paths** — a library-relative one and
/// an absolute one, no embedded copy — and plants the two files under `root`. Added M14: this is
/// the scenario in which the requirement's "including the identity of the loaded model and IR
/// files" clause has something to be true of. Its sibling above embeds the resources, which is
/// FR-STATE-080's fallback and deliberately makes file identity irrelevant.
///
/// A **decoy** is planted alongside each real file: same extension, same directory, different
/// content. If a restore matched on anything weaker than content — a name, a directory position,
/// an iteration order — the decoy is what it would find, and the recall assertion in
/// [`run_as_child`] would fail rather than the two children agreeing on the wrong file.
fn write_path_referenced_state(path: &Path, root: &Path) {
    let library = root.join("bank");
    std::fs::create_dir_all(&library).expect("create the library root");

    let model_bytes = namir_fixtures::nam::generate(namir_fixtures::nam::WaveNetShape::Nano, 7)
        .expect("fixture should generate")
        .to_json_bytes();
    let model_path = library.join("referenced-model.nam");
    std::fs::write(&model_path, &model_bytes).expect("plant the model");
    let decoy_model = namir_fixtures::nam::generate(namir_fixtures::nam::WaveNetShape::Nano, 8)
        .expect("fixture should generate")
        .to_json_bytes();
    std::fs::write(library.join("decoy-model.nam"), &decoy_model).expect("plant the decoy model");

    let ir_bytes = namir_fixtures::ir::to_mono_wav_bytes(
        &namir_fixtures::ir::decaying_noise(256, 9, 64.0),
        SR,
    );
    let ir_path = library.join("referenced-ir.wav");
    std::fs::write(&ir_path, &ir_bytes).expect("plant the IR");
    let decoy_ir = namir_fixtures::ir::to_mono_wav_bytes(
        &namir_fixtures::ir::decaying_noise(256, 10, 64.0),
        SR,
    );
    std::fs::write(library.join("decoy-ir.wav"), &decoy_ir).expect("plant the decoy IR");

    let reference = |name: &str, rel: &str, absolute: &Path, bytes: &[u8]| namir_state::FileRef {
        hash: namir_core::ContentHash::of(bytes),
        library_relative: Some(
            namir_state::RelPath::parse(rel).expect("a valid library-relative path"),
        ),
        absolute: Some(absolute.to_string_lossy().into_owned()),
        display_name: name.to_string(),
        embedded: None,
    };

    let mut state = namir_state::State::defaults();
    state.nam = Some(reference(
        "referenced-model.nam",
        "bank/referenced-model.nam",
        &model_path,
        &model_bytes,
    ));
    state.ir = Some(reference(
        "referenced-ir.wav",
        "bank/referenced-ir.wav",
        &ir_path,
        &ir_bytes,
    ));

    std::fs::write(path, state.write()).expect("write the path-referenced state file");
}

/// See this file's own doc comment for the whole design. This one test function is both the
/// parent and the child, distinguished by `CHILD_ENV_VAR` at the top.
// trace-partial: FR-STATE-060
// uncovered: FR-STATE-060 — the method's "save a project, restart the host, reopen" is executed by
// uncovered: nothing: both scenarios here re-invoke this test binary and drive
// uncovered: namir_worker::Instance directly, never loading namir-clap or calling its state
// uncovered: extension, so what is proven is that a saved state reproduces the tone in a fresh
// uncovered: process, not that a CLAP host's save/reopen round trip carries it. The "identity of
// uncovered: the loaded model and IR files" clause closed at M14, in the path-referenced scenario
// uncovered: beside this one; closes M8
#[test]
fn fr_state_060_two_independent_processes_produce_bit_identical_output() {
    if std::env::var(CHILD_ENV_VAR).is_ok() {
        run_as_child();
        return;
    }

    let state_path = temp_dir().join("shared_state.namirpreset");
    write_shared_state(&state_path);

    let name = "fr_state_060_two_independent_processes_produce_bit_identical_output";
    let output_a = run_in_a_fresh_process(name, &state_path, None, "a");
    let output_b = run_in_a_fresh_process(name, &state_path, None, "b");

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

/// FR-STATE-060's "**including the identity of the loaded model and IR files**" clause, which the
/// embedded scenario above cannot reach: an embedded state carries its resources *with* it, so
/// nothing about it depends on finding the right file. Added M14.
///
/// Same two-process comparison, with a state that names its resources only by path — a
/// library-relative one and an absolute one — beside decoys of the same kind in the same
/// directory. A fresh process therefore has to locate the recorded files and verify them by
/// content hash before it can render anything at all; the child asserts both slots came back
/// `Loaded` rather than `Missing`, which under `recall::locate` is exactly the statement that the
/// bytes it found hashed to what the state recorded (P7: identity is the content hash, paths are
/// hints). The bit-identical comparison then says the two processes reproduced the same tone from
/// that identity, which is the requirement's first clause.
///
/// The tag stays on the sibling test above; see its `// uncovered:` field for what is still open.
#[test]
fn fr_state_060_a_path_referenced_state_restores_the_same_files_in_a_fresh_process() {
    if std::env::var(CHILD_ENV_VAR).is_ok() {
        run_as_child();
        return;
    }

    let root = temp_dir().join("library-root");
    let _ = std::fs::remove_dir_all(&root);
    let state_path = temp_dir().join("path_referenced_state.namirpreset");
    write_path_referenced_state(&state_path, &root);

    let name = "fr_state_060_a_path_referenced_state_restores_the_same_files_in_a_fresh_process";
    let output_a = run_in_a_fresh_process(name, &state_path, Some(&root), "path-a");
    let output_b = run_in_a_fresh_process(name, &state_path, Some(&root), "path-b");

    assert_eq!(
        output_a.len(),
        BLOCKS * BLOCK * 4,
        "unexpected output length"
    );
    assert_eq!(
        output_a, output_b,
        "two independent processes given the same path-referenced state must produce \
         bit-identical output"
    );

    // The decoys have to have been a real alternative, or their presence proves nothing: a
    // resolver that found nothing at all would have produced `Missing` and failed the child.
    let library = root.join("bank");
    assert!(library.join("decoy-model.nam").is_file());
    assert!(library.join("decoy-ir.wav").is_file());

    let _ = std::fs::remove_file(&state_path);
    let _ = std::fs::remove_dir_all(&root);
}
