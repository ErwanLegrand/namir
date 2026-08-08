//! NFR-PERF-050: "loading shall complete within 500 ms for files up to 50 MB." A wall-clock
//! figure by its own literal wording, and legitimately measured as one: D-2.5 (added M5) scopes
//! D-2.1's "never wall-clock" rule to audio-thread per-block budgets specifically, and everything
//! this benchmark times is worker-side, off-audio-thread work (D-8.1 step 1) — `Instance::load`
//! is documented "not RT-safe" for exactly that reason.
//!
//! # Three arms
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
//! # Cold, not cached
//!
//! Each repetition builds a fresh [`namir_engine::AudioEngine`]/[`namir_worker::Instance`] and a
//! fresh, empty [`namir_worker::ResourceCache`] before loading — `ResourceCache` is
//! process-global-shaped (D-8.2) precisely so a *second* instance loading the same bytes is
//! nearly free, which is the wrong thing to measure here: NFR-PERF-050 is about the load a user
//! actually waits on, the first one.
//!
//! # What the measured window does *not* include: the file read
//!
//! **Recorded plainly rather than left to be discovered.** All three arms call `Instance::load`
//! with [`LoadSource::Bytes`] — an `Arc<[u8]>` that already exists in memory — so the clock starts
//! after the bytes are there. [`LoadSource::File`], the variant a product shell actually uses, adds
//! a `std::fs::metadata` and a `std::fs::read` of the whole file inside the same call
//! (`namir-worker/src/lib.rs`'s `LoadSource::read`). NFR-PERF-050 states its ceiling **"for files
//! up to 50 MB"**, and this binary measures a 50 MB *payload*, never a 50 MB *file*: the disk read
//! is outside the window, so every figure below is a lower bound on what a user waits for, by
//! however long the volume takes to hand the bytes over. That is a deliberate choice — parse and
//! prepare cost is Namir's, and a `std::fs::read` figure is a property of the machine's storage —
//! but it means the 500 ms assertion this binary makes is an assertion about the loader, not about
//! the whole wall-clock duration the requirement's sentence describes.
//!
//! # The other half of the sentence: the audio-thread clause
//!
//! NFR-PERF-050 is two clauses joined by an "and" — "shall complete within 500 ms for files up to
//! 50 MB **on the reference machine, and shall never delay the audio thread regardless of duration
//! (FR-NAM-070)**". This binary measures the first and says nothing about the second: nothing here
//! runs an audio thread at all. The nearest evidence is `tests/rt_stress.rs`'s axis A, which drives
//! `Instance::load` in a loop against a live `AudioEngine` and asserts zero audio-thread
//! allocation, zero dropout blocks and a bounded worst block — real evidence, but an integration
//! test rather than the `Verify: B` NFR-PERF-050 names, and its models are `WaveNetShape::Nano`, so
//! "regardless of duration" is exercised at no long duration by anything in this tree.
//!
//! Both gaps are why the tag above `main` is a `// trace-partial:` rather than a plain `// trace:`
//! (D-23.1: a plain tag asserts the **whole** requirement by its stated `Verify:` method). Closing
//! them is M9b's, alongside the certified re-measurement M9b already owes this requirement.
//!
//! # Read this before quoting any number from this binary
//!
//! D-2.4 governs, same as every other benchmark in this workspace: pin away from CPU 0 (absorbs
//! `dxgkrnl.sys`'s GPU interrupts) and CPU 2 (heaviest kernel DPC load) — this defaults to core 4,
//! override with `NAMIR_PIN_CORE` — on a machine verified quiet, across >= 5 repetitions with the
//! spread reported. `RUSTFLAGS` replaces `.cargo/config.toml`'s `-C target-cpu=x86-64-v3` rather
//! than appending to it; an unexpectedly-set `RUSTFLAGS` silently measures without AVX2.

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
/// only part of NFR-PERF-050's sentence, so the tag above `main` is a `// trace-partial:` and names
/// both gaps — see its own `// uncovered:` field, and this file's "What the measured window does
/// *not* include" section above.
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

/// Measures `reps` independent cold loads of `bytes` against `target`, each against a fresh
/// engine/instance/cache — see this file's own "Cold, not cached" section.
fn measure(label: &str, target: Target, bytes: &Arc<[u8]>, reps: usize) {
    assert!(
        bytes.len() <= NFR_PERF_050_MAX_BYTES,
        "{label}: this arm's payload is {} bytes, past NFR-PERF-050's {NFR_PERF_050_MAX_BYTES}-byte \
         \"files up to 50 MB\" clause -- the 500 ms ceiling is not claimed for it, so asserting the \
         ceiling here would assert something the requirement does not say. Shrink the fixture, or \
         report this arm without the assertion and say so",
        bytes.len()
    );

    let mut durations = Vec::with_capacity(reps);
    for _ in 0..reps {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        let started = Instant::now();
        let outcome = instance.load(&cache, target, LoadSource::Bytes(Arc::clone(bytes)));
        let elapsed = started.elapsed();

        match outcome.result {
            JobResult::Loaded { .. } => {}
            other => panic!("{label}: expected a successful load, got {other:?}"),
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
        "{label:<32} bytes={:>11} reps={reps:>3} | p50 {p50:>9.2?} | p99 {p99:>9.2?} | max {max:>9.2?}",
        bytes.len(),
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
        "NFR-PERF-050: {label} ({} bytes) took {max:.2?} on its slowest of {reps} repetitions, over \
         the {NFR_PERF_050_CEILING:?} ceiling (p50 {p50:.2?}, p99 {p99:.2?}). D-2.4: one reading on \
         a machine that was not verified quiet is not evidence of a regression -- re-run pinned \
         (NAMIR_PIN_CORE) >= 5 times before believing this, and note that a certified figure is a \
         reference-machine (02-architecture.md section 2) figure only",
        bytes.len()
    );
}

// trace-partial: NFR-PERF-050
// uncovered: NFR-PERF-050 — (a) the "for files up to 50 MB" clause: every arm loads
// uncovered: LoadSource::Bytes, so the fs::metadata + fs::read that LoadSource::File performs is
// uncovered: outside the measured window, and this binary therefore times a 50 MB payload and
// uncovered: never a 50 MB file. (b) the "shall never delay the audio thread regardless of
// uncovered: duration" clause is not measured here at all: its only evidence is rt_stress.rs's
// uncovered: axis A, an integration test rather than the Verify: B this requirement names, whose
// uncovered: concurrent loads are Nano fixtures and so exercise no long duration; closes M9b
fn main() {
    pin_to_measurement_core();

    println!("NFR-PERF-050: resource load time (worker-side, wall-clock -- D-2.5's scoping)");
    println!(
        "D-2.4: pin away from CPU 0/2 (this run used NAMIR_PIN_CORE={}), verify the machine is \n\
         quiet, and take >= 5 repetitions before quoting anything below.\n",
        std::env::var("NAMIR_PIN_CORE").unwrap_or_else(|_| "4 (default)".into())
    );

    let standard_model: Arc<[u8]> = Arc::from(
        generate(WaveNetShape::Standard, 1)
            .expect("standard fixture should generate")
            .to_json_bytes()
            .into_boxed_slice(),
    );
    measure("standard model", Target::Nam, &standard_model, 20);

    let ir_len = 2 * SR as usize;
    let left = decaying_noise(ir_len, 21, 8_000.0);
    let right = decaying_noise(ir_len, 22, 8_000.0);
    let stereo_ir: Arc<[u8]> = Arc::from(to_stereo_wav_bytes(&left, &right, SR).into_boxed_slice());
    measure("2 s stereo IR", Target::Ir, &stereo_ir, 20);

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
    measure(
        "~50 MB oversized (uncalibrated)",
        Target::Nam,
        &oversized,
        5,
    );

    println!(
        "\nPASS: every arm's slowest repetition stayed inside NFR-PERF-050's \
         {NFR_PERF_050_CEILING:?} ceiling, for payloads up to 50 MB. Read this file's \"What the \
         measured window does not include\" section before quoting it against the requirement's \
         own \"for files up to 50 MB\" wording."
    );
}
