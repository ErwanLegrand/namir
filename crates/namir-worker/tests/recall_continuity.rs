//! FR-STATE-050 (`Verify: I`): "Preset recall shall not glitch the audio thread; the constraints
//! of FR-NAM-070 apply to any model or IR change a preset implies."
//!
//! # Why this file exists, and what it adds to what was already here
//!
//! Two artifacts already covered parts of this requirement, and neither asserted the clause the
//! requirement's own sentence imports.
//!
//! - `recall.rs`'s `recalling_both_a_model_and_an_ir_never_offers_them_simultaneously` proves the
//!   *structural* precondition — R-7's serialisation, that a recall's two handovers never overlap
//!   — through a real `State`, a real `FileResolver`, a real `ResourceCache` and a real command
//!   ring. It processes no audio at all, so "no click, no dropout" is asserted there by nothing.
//!   Serialisation is a necessary condition for that clause, not the clause.
//! - `rt_stress.rs` runs recalls against a live `AudioEngine` and asserts no block goes silent,
//!   which is the *dropout* half — but it makes no discontinuity assertion, so a changeover that
//!   clicked audibly while staying above the dropout floor would pass it.
//!
//! This file asserts the clause itself, in FR-NAM-070's own terms and with FR-NAM-070's own
//! method: drive a continuous sine through a live engine, trigger a **preset recall** partway
//! through, and require both that the recall introduces no first-difference materially larger
//! than the same run without one, and that nothing across the changeover goes silent. The
//! discontinuity threshold is self-calibrating for the same reason `namir-engine`'s
//! `fr_nam_070_swapping_models_under_a_sine_has_no_discontinuity_or_dropout` calibrates its own: a
//! fixed constant would quietly stop meaning anything the first time a fixture changed. Two
//! baselines, not one, for that test's own reason as well — the post-recall window is mostly the
//! *incoming* resource's output, so calibrating against the outgoing one alone would charge the
//! crossfade for whatever the two fixtures differ by in steady state.
//!
//! # "any model or IR change a preset implies" is a set, and all three members are driven
//!
//! A preset can imply three different changes to a resource slot, and the crossfade path is not
//! the same code in each: a **swap** (loaded -> different resource loaded), an **unload** (loaded
//! -> the state names nothing, FR-STATE-070's "the state shall load with that stage empty"), and a
//! **load into an empty slot** (nothing -> loaded). `Instance::unload` is a handover to nothing —
//! D-8.1's own M5 consequence note — so it is subject to the same no-click constraint as a swap
//! and is the member most likely to be got wrong, since the tempting implementation drops the
//! resource rather than fading out of it. Each scenario below drives one of the three, on **both**
//! slots at once, which is also the case R-7's serialisation exists for.
//!
//! # Why the audio loop is paced in real time, and only in the run that recalls
//!
//! R-7's serialisation (`Instance::serialise_against_other_target`) is a **wall-clock** sleep on
//! the worker thread, and the crossfade it is spacing the two handovers out over is counted in
//! *samples* on the audio thread. Those two only line up if the audio loop advances at something
//! near real time, so the recalling run sleeps to the block period. The baseline runs schedule no
//! recall and nothing runs concurrently with them, so their output is a pure function of their
//! commands and pacing them would only cost wall-clock time — they run flat out.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use namir_core::{ChannelConfig, ContentHash, SampleRate};
use namir_engine::{PrepareContext, StageIo, build_default_engine};
use namir_library::RootsOnlyResolver;
use namir_state::{FileRef, State};
use namir_worker::recall::{RecallOutcome, ResourceRecall};
use namir_worker::{EngineConfig, Instance, LoadSource, ResourceCache, Target};

const SR: u32 = 48_000;
const BLOCK: usize = 64;

/// Blocks of audio in each measured run — ~533 ms at 48 kHz, comfortably past both the crossfade
/// and R-7's serialisation window even with the two stacked.
const BLOCKS: usize = 400;
/// Which block the recall is triggered at. Everything from here on is the measured window.
const RECALL_AT: usize = 100;
/// Blocks run before the measured window starts, so the initial resources' own fade-in has
/// finished and is not measured as though it were the recall's.
const SETTLE_BLOCKS: usize = 200;

/// FR-NAM-070's own test's factor, reused rather than re-invented.
const DISCONTINUITY_FACTOR: f32 = 3.0;
/// FR-NAM-070's own dropout floor, likewise. FR-CHAIN-040 makes an unloaded stage a dry
/// passthrough rather than silence, so this holds through an unload scenario too.
const DROPOUT_PEAK_THRESHOLD: f32 = 1e-4;

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

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "namir-worker-recall-continuity-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// Writes one fixture out and returns a `FileRef` naming it by absolute path — the candidate
/// `RootsOnlyResolver` resolves, so the recall below goes through FR-STATE-070's *external*
/// resolution path and a real `std::fs::read`, not through FR-STATE-080's embedded fallback.
fn planted_ref(name: &str, bytes: &[u8]) -> FileRef {
    let path = scratch_dir().join(name);
    std::fs::write(&path, bytes).expect("write a fixture");
    FileRef {
        hash: ContentHash::of(bytes),
        library_relative: None,
        absolute: Some(path.to_string_lossy().into_owned()),
        display_name: name.to_string(),
        embedded: None,
    }
}

/// The pair of resources a run starts with, or `None` for "start with both stages empty".
type Initial = Option<(Arc<[u8]>, Arc<[u8]>)>;

fn bytes_arc(v: Vec<u8>) -> Arc<[u8]> {
    Arc::from(v.into_boxed_slice())
}

/// Largest absolute first difference between consecutive samples — `namir-engine`'s own
/// discontinuity measure, duplicated here because that crate's is `#[cfg(test)]`-private.
fn max_abs_first_difference(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max)
}

/// One run: settle, then `BLOCKS` measured blocks of a continuous 220 Hz sine, optionally
/// triggering `recall_to` on a worker thread at [`RECALL_AT`]. Returns the measured samples and,
/// when a recall was scheduled, its outcome.
fn run(
    cache: &Arc<ResourceCache>,
    initial: Initial,
    recall_to: Option<State>,
) -> (Vec<f32>, Option<RecallOutcome>) {
    let c = ctx();
    let (mut engine, endpoint) = build_default_engine(&c).expect("build the engine");
    let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

    if let Some((nam, ir)) = initial {
        instance.load(cache, Target::Nam, LoadSource::Bytes(nam));
        instance.load(cache, Target::Ir, LoadSource::Bytes(ir));
    }

    let mut phase = 0.0f32;
    let step = std::f32::consts::TAU * 220.0 / SR as f32;
    let mut buf = [0.0f32; BLOCK];
    let mut process_one =
        |engine: &mut namir_engine::AudioEngine, phase: &mut f32| -> [f32; BLOCK] {
            for s in buf.iter_mut() {
                *s = 0.5 * phase.sin();
                *phase += step;
                if *phase > std::f32::consts::TAU {
                    *phase -= std::f32::consts::TAU;
                }
            }
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = StageIo::new(&mut channels, BLOCK);
            engine.process(&mut io);
            let mut out = [0.0f32; BLOCK];
            out.copy_from_slice(io.channel(0));
            out
        };

    // Settle: the initial resources' own fade-in happens here, before anything is recorded.
    for _ in 0..SETTLE_BLOCKS {
        process_one(&mut engine, &mut phase);
    }
    instance.drain_retired();

    // The recall runs on its own thread: `Instance::recall` sleeps in wall-clock time to keep the
    // two handovers apart (R-7), and doing that on the audio loop's own thread would stop the
    // audio for exactly the window this test is measuring.
    let (state_tx, state_rx) = mpsc::channel::<State>();
    let (outcome_tx, outcome_rx) = mpsc::channel::<RecallOutcome>();
    let cache_for_worker = Arc::clone(cache);
    let worker = std::thread::spawn(move || {
        let roots: Vec<PathBuf> = Vec::new();
        let resolver = RootsOnlyResolver::new(&roots);
        while let Ok(state) = state_rx.recv() {
            let outcome = instance.recall(&cache_for_worker, &state, &resolver);
            let _ = outcome_tx.send(outcome);
        }
        instance
    });

    let paced = recall_to.is_some();
    let block_period = Duration::from_secs_f64(BLOCK as f64 / SR as f64);
    let mut pending = recall_to;
    let mut out = Vec::with_capacity(BLOCKS * BLOCK);
    let started = Instant::now();
    for b in 0..BLOCKS {
        if b == RECALL_AT
            && let Some(state) = pending.take()
        {
            state_tx.send(state).expect("the recall thread is alive");
        }
        if paced {
            let target = started + block_period * (b as u32);
            let now = Instant::now();
            if target > now {
                std::thread::sleep(target - now);
            }
        }
        out.extend_from_slice(&process_one(&mut engine, &mut phase));
    }

    drop(state_tx);
    let outcome = outcome_rx.recv_timeout(Duration::from_secs(5)).ok();
    let mut instance = worker.join().expect("the recall thread panicked");
    instance.drain_retired();
    (out, outcome)
}

/// Asserts one scenario: `initial` loaded, then `target` recalled, against baselines run with
/// `initial` and with `baseline_after` respectively.
fn assert_recall_is_glitch_free(
    label: &str,
    cache: &Arc<ResourceCache>,
    initial: Initial,
    baseline_after: Initial,
    target: State,
) {
    let (baseline_before_out, _) = run(cache, initial.clone(), None);
    let (baseline_after_out, _) = run(cache, baseline_after, None);
    let window = RECALL_AT * BLOCK;
    let baseline_jump = max_abs_first_difference(&baseline_before_out[window..])
        .max(max_abs_first_difference(&baseline_after_out[window..]));

    let (recalled, outcome) = run(cache, initial, Some(target));
    let outcome =
        outcome.unwrap_or_else(|| panic!("{label}: the recall never reported an outcome"));
    assert_eq!(
        outcome.commands_not_delivered, 0,
        "{label}: {} recall commands never reached the audio thread",
        outcome.commands_not_delivered
    );
    for (slot, recall) in [("nam", &outcome.nam), ("ir", &outcome.ir)] {
        assert!(
            !matches!(recall, ResourceRecall::Missing { .. }),
            "{label}: the {slot} reference did not resolve ({recall:?}) -- this scenario would \
             then be measuring a failed recall rather than a changeover"
        );
    }

    // FR-NAM-070's first half: no discontinuity beyond what the same run shows without a recall.
    let recall_jump = max_abs_first_difference(&recalled[window..]);
    assert!(
        recall_jump <= DISCONTINUITY_FACTOR * baseline_jump,
        "{label}: the recall introduced a discontinuity of {recall_jump} against a no-recall \
         baseline of {baseline_jump} (allowed {DISCONTINUITY_FACTOR}x) -- FR-STATE-050's imported \
         'no click' constraint"
    );

    // FR-NAM-070's second half: no dropout. FR-CHAIN-040 makes an unloaded stage a dry
    // passthrough, so this holds through the unload scenario as well as the two loading ones.
    for (i, chunk) in recalled[window..].chunks(32).enumerate() {
        let peak = chunk.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak > DROPOUT_PEAK_THRESHOLD,
            "{label}: window {i} after the recall went silent (peak {peak}) -- FR-STATE-050's \
             imported 'no dropout' constraint"
        );
    }
}

/// FR-STATE-050, driven end to end: a real `State`, resolved through a real `FileResolver` against
/// real files, loaded through a real `ResourceCache` and a real command ring, onto a live engine
/// processing a continuous sine — and asserted in FR-NAM-070's own terms, which is the whole of
/// what this requirement says.
// trace: FR-STATE-050
#[test]
fn a_preset_recall_never_clicks_or_drops_out_for_any_change_it_implies() {
    let cache = Arc::new(ResourceCache::new());

    let nam_a = model_bytes(41);
    let ir_a = ir_bytes(42);
    let nam_b = model_bytes(43);
    let ir_b = ir_bytes(44);

    let ref_b_nam = planted_ref("recall_b.nam", &nam_b);
    let ref_b_ir = planted_ref("recall_b.wav", &ir_b);

    let loaded_a = || Some((bytes_arc(model_bytes(41)), bytes_arc(ir_bytes(42))));
    let loaded_b = || Some((bytes_arc(model_bytes(43)), bytes_arc(ir_bytes(44))));
    let state_naming_b = || {
        let mut s = State::defaults();
        s.nam = Some(ref_b_nam.clone());
        s.ir = Some(ref_b_ir.clone());
        s
    };

    // Warm the cache so parsing time is not part of any changeover's wall clock -- the same
    // convention `recall.rs`'s own R-7 test uses.
    let _ = cache.get_or_load_nam(&nam_a).unwrap();
    let _ = cache.get_or_load_nam(&nam_b).unwrap();
    let c = ctx();
    let _ = cache
        .get_or_load_ir(&ir_a, c.sample_rate(), c.max_block_size())
        .unwrap();
    let _ = cache
        .get_or_load_ir(&ir_b, c.sample_rate(), c.max_block_size())
        .unwrap();

    // 1. Swap: both slots loaded, the preset names different files for both.
    assert_recall_is_glitch_free("swap", &cache, loaded_a(), loaded_b(), state_naming_b());

    // 2. Unload: both slots loaded, the preset names nothing (FR-STATE-070's "the state shall
    //    load with that stage empty"). `Instance::unload` is a handover to nothing, and is
    //    subject to the same constraint as a swap.
    assert_recall_is_glitch_free("unload", &cache, loaded_a(), None, State::defaults());

    // 3. Load into an empty slot: nothing loaded, the preset names both.
    assert_recall_is_glitch_free("load", &cache, None, loaded_b(), state_naming_b());

    let _ = std::fs::remove_dir_all(scratch_dir());
}
