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
//! (FR-NAM-070)**". This binary measures the first and says nothing about the second: nothing here
//! runs an audio thread at all. The nearest evidence is `tests/rt_stress.rs`'s axis A, which drives
//! `Instance::load` in a loop against a live `AudioEngine` and asserts zero audio-thread
//! allocation, zero dropout blocks and a bounded worst block — real evidence, but an integration
//! test rather than the `Verify: B` NFR-PERF-050 names, and its models are `WaveNetShape::Nano`, so
//! "regardless of duration" is exercised at no long duration by anything in this tree.
//!
//! That second clause is why the tag above `main` is still a `// trace-partial:` rather than a
//! plain `// trace:` (D-23.1: a plain tag asserts the **whole** requirement by its stated `Verify:`
//! method) — and it is now the *only* reason, the file-read gap having closed here at M9b. Nothing
//! in this binary was extended to span it: an arm that ran a live `AudioEngine` alongside a 50 MB
//! load would be a second, differently-shaped benchmark, and claiming the clause on the strength of
//! the arms below would be exactly the over-claim D-23.1's two questions exist to catch.
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

// trace-partial: NFR-PERF-050
// uncovered: NFR-PERF-050 — the "shall never delay the audio thread regardless of duration"
// uncovered: clause: nothing in this binary runs an audio thread, so no arm here measures it. Its
// uncovered: only evidence is rt_stress.rs's axis A, an integration test rather than the Verify: B
// uncovered: this requirement names, whose concurrent loads are Nano fixtures and so exercise no
// uncovered: long duration. The sentence's other clause, "within 500 ms for files up to 50 MB",
// uncovered: closed at M9b: the file arms below time LoadSource::File, so fs::metadata and
// uncovered: fs::read are inside the asserted window; closes M8
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

    let _ = std::fs::remove_dir_all(&dir);

    println!(
        "\nThe 50 MB read, isolated: {:.2?} on the worst repetition, the difference between that \n\
         payload's file and bytes arms. A floor on what a cold-cache read would add, not an \n\
         estimate of it (D-2.5 condition 1).",
        oversized_file_max.saturating_sub(oversized_bytes_max)
    );
    println!(
        "\nPASS: every arm's slowest repetition stayed inside NFR-PERF-050's \
         {NFR_PERF_050_CEILING:?} ceiling, for files up to 50 MB read from a real path as well as \
         for payloads already in memory. The sentence's second clause -- \"shall never delay the \
         audio thread regardless of duration\" -- is not measured here; see this file's own \
         `// uncovered:` field."
    );
}
