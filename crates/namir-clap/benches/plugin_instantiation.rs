//! **NFR-PERF-040**: "plugin instantiation in a host shall complete within 200 ms, excluding model
//! loading." `Verify: B`, and D-2.6 (`docs/02-architecture.md` §2, added at M9b's P0 decision pass)
//! elects **this** in-process `clack-host` harness as the requirement's **certified** vehicle: a
//! real-DAW figure is supplementary evidence recorded in a manual document where one is taken, and
//! is explicitly *not* a precondition for the requirement closing.
//!
//! # The timing window: where it opens, where it closes, and why that is "instantiation in a host"
//!
//! The window opens immediately before `PluginInstance::new` and closes immediately after
//! `activate` returns. In CLAP's own vocabulary that is
//! `clap_plugin_factory.create_plugin` → `clap_plugin.init` → `clap_plugin.activate`, and it is
//! chosen because it is exactly the interval between a host deciding to add Namir to a track and
//! Namir being able to process audio for it. A user waits for all of it and for none of it
//! separately, which is why the assertion is on the **total** — the two halves are reported
//! individually so a regression can be attributed, not so either can be quoted on its own.
//!
//! Both halves do real work, and neither is a formality:
//!
//! - **`create_plugin` + `init`** runs `NamirShared::new` → `SharedInner::new`
//!   (`crates/namir-clap/src/shared.rs`), which starts the instance's worker pool — proven live by
//!   `tests/clap_host_teardown.rs`'s `live_worker_threads() > baseline` assertion at exactly this
//!   point — and opens the library index (see the next section).
//! - **`activate`** is `crates/namir-clap/src/audio.rs:119`, the seam D-2.6 names: it builds the
//!   whole six-stage chain through `build_default_engine` (`:151`) and wraps it in
//!   `Instance::new` (`:157`). This is where every per-block buffer in the chain is sized and
//!   allocated, which is why this file measures more than one audio configuration.
//!
//! ## What is deliberately **outside** the window
//!
//! **`clap_entry.init`** — `PluginEntry::load_from_clack`, loaded once before any measurement and
//! reported separately as a diagnostic. A host pays it once per *shared library*, not once per
//! instance, so folding it into a per-instantiation figure would charge every instance for work
//! only the first one causes. NFR-PERF-040 says "plugin instantiation", not "plugin scanning".
//!
//! **`clap_plugin.destroy`** — teardown joins the instance's worker pool (`impl Drop for
//! NamirShared`, the fix `clap_host_teardown.rs` exists to guard) and a user does not wait on it
//! to start playing. It happens inside the repetition loop but outside the clock.
//!
//! **The recall job.** `activate` finishes by dispatching `crate::worker_jobs::spawn_recall` onto
//! the pool (`audio.rs:178`). That is asynchronous by construction and its cost is not in the
//! window — and for a freshly-created instance with nothing to recall it is a no-op anyway, which
//! is the next section's subject.
//!
//! # "Excluding model loading" — structural, not a promise
//!
//! Nothing here loads a model or an IR, and the reason is stronger than "the harness does not call
//! `load`": **this binary has no model or IR to load.** It takes no `namir-fixtures` dependency,
//! contains no `.nam` bytes and no WAV bytes, and the sandbox's `Library` root (below) is an empty
//! directory. Each instance is created with an empty `ParamMirror` and no resource references, so
//! `spawn_recall` has nothing to replay, and the NAM and IR stages stay the pass-throughs
//! `build_default_engine` builds them as. The requirement's exclusion is therefore a property of
//! the measurement's construction rather than a discipline the reader has to trust.
//!
//! NFR-PERF-050 (`crates/namir-worker/benches/resource_load.rs`) is where the loading half lives.
//!
//! # The library index **is** inside the window, and this binary plants the one it measures
//!
//! `SharedInner::new` calls `namir_worker::library::LibraryService::open_default`, which reads
//! `<config_dir>/library-index.json` and parses it. That is genuinely part of what a host-driven
//! instantiation costs — a user with a large library pays it on every instance they add — so
//! excluding it would measure something no user ever experiences. It stays in.
//!
//! Left alone, though, it would make the figure a property of *this developer's machine state*:
//! `open_default` resolves the **real** per-user configuration directory, so the number would
//! silently depend on whatever `library-index.json` happened to be sitting there on the day, and
//! would not reproduce on another machine or on the same machine a month later. That is precisely
//! the failure mode where a benchmark means something other than what it claims. So the measured
//! index is **planted**, at a stated scale, exactly as NFR-PERF-030's
//! `crates/namir-app/benches/startup_to_audible.rs` plants its warm index:
//!
//! - an **empty** index — the plugin's own instantiation cost, with the library contribution zero;
//! - a **10 000-entry** index — [`INDEX_ENTRIES`], the figure FR-LIB-020 and NFR-PERF-060 use for
//!   "a library", so the realistic worst case is measured at this project's own stated scale
//!   rather than at whatever the measuring machine owns.
//!
//! ## How the plant is made possible without `unsafe`, and why that also disposes of the hazard
//!
//! `open_default` takes no injection point, so the only lever is the environment
//! `namir_platform::config_dir()` reads (`APPDATA`, `HOME`, `XDG_CONFIG_HOME`). `std::env::set_var`
//! is `unsafe` under edition 2024, and this crate's `unsafe_code = "deny"` admits exactly one
//! `#![allow(unsafe_code)]` file (`src/gui.rs`) — with tests and benches getting no exemption
//! (D-5.3's M9 consequence). `Command::env` is safe, so `main` **re-execs this same binary** as a
//! child with all three variables pointed at a per-process scratch root, and the child does every
//! instantiation. All three are set unconditionally rather than one per platform: only one is read
//! on any given OS, and picking between them would need the `#[cfg(target_os)]` that `xtask
//! layering` rejects outside `namir-platform`.
//!
//! This also closes the hazard `tests/support/mod.rs` documents at length. That module's rule is
//! "never call `start_library_scan`, because a scan against unconfigured roots concludes every
//! known path was removed and erases the developer's real `library-index.json`". This binary never
//! starts a scan either — but it additionally never *resolves* to the real config directory at all,
//! because the child process's environment does not point there. The index it opens, reads, plants
//! into and deletes is the sandbox's, and the sandbox is removed when the parent exits.
//!
//! # Audio configurations
//!
//! Two, because `activate` sizes the chain from them and a single configuration would leave the
//! allocation cost of every other one unmeasured: 48 kHz with 512-frame blocks (this workspace's
//! reference configuration, `tests/support/mod.rs`'s default), and 192 kHz with 4096-frame blocks
//! — the heaviest configuration a host is realistically going to present. Both are asserted
//! against the same 200 ms ceiling, since the requirement states one.
//!
//! # Measured at M9b on the §2 reference machine — and where the 200 ms actually goes
//!
//! Six runs of this binary (one plus D-2.4's five), 20 repetitions per arm, pinned to core 4 on the
//! reference machine of `docs/02-architecture.md` §2. **NFR-PERF-040 passes**, and the
//! attribution is worth stating plainly because it is not where a reader would guess:
//!
//! | arm | median | worst of 6 runs |
//! |---|---|---|
//! | empty library index, 48 kHz / 512 | ~0.17 ms | 0.24 ms |
//! | 10 000-entry index, 48 kHz / 512 | ~159 ms | 163.76 ms |
//! | 10 000-entry index, 192 kHz / 4096 | ~159 ms | 172.59 ms |
//!
//! **Namir's own instantiation is ~0.17 ms, about a tenth of a percent of the budget.** Building
//! the entire six-stage chain — the `activate` half, `audio.rs:119` and everything under it — is a
//! *median 37–119 µs* depending on configuration, and the whole 16-fold jump between the two audio
//! configurations moves the total by less than 0.1 ms. Essentially the entire measured figure is
//! one thing: **the JSON parse of `library-index.json`**, ~161 ms for 10 000 entries (7.57 MB),
//! inside `SharedInner::new`'s `LibraryService::open_default`.
//!
//! Two consequences a future reader should have in front of them:
//!
//! 1. **The margin against 200 ms is a property of the user's library size, not of plugin code.**
//!    At ~16 µs per index entry the parse alone reaches 200 ms somewhere around 12 000–12 500
//!    entries, so a user with a library meaningfully larger than FR-LIB-020's stated 10 000 would
//!    exceed this requirement — on a machine at least as fast as the reference one. Nothing here
//!    is regressing; the requirement is simply being met with ~14% headroom rather than with
//!    orders of magnitude, and this paragraph exists so that is a known position rather than a
//!    surprise. It also means a regression in *chain construction* — the part this crate actually
//!    owns — could be a hundredfold before this benchmark noticed, which is why the create/activate
//!    split is reported per arm and not just the total.
//! 2. **The empty-index arm is the sensitive one.** It is the arm that would catch a real
//!    instantiation regression, and it currently sits four orders of magnitude under the ceiling.
//!    Whoever tightens this requirement's guard should tighten it there, not on the total.
//!
//! # Why the tag above `main` is a plain `// trace:` and not a `trace-partial:`
//!
//! D-2.6 settles it. A `Verify: B` is satisfied by a benchmark that **asserts** its numeric
//! threshold in-process, which this does on the slowest of at least five repetitions per arm
//! (the house pattern from `crates/namir-worker/benches/resource_load.rs:181-189`, print first so a
//! failing arm still leaves its own row above the panic). The residue a real DAW would add —
//! plugin scanning, process or thread sandboxing, contention with a host UI thread and with other
//! already-loaded instances — is recorded in D-2.6's own *Honest limitation* paragraph as an
//! anticipated property of the certified figure, not as an unspanned half of the requirement: the
//! decision says in as many words that a DAW measurement "is **not** a precondition for the
//! requirement closing". Booking it as an `// uncovered:` field here would contradict the decision
//! this benchmark was written to implement. What the number certifies is the plugin's own
//! instantiation cost, which is the part Namir controls and the part a regression appears in.
//!
//! # Read this before quoting any number from this binary
//!
//! D-2.4, same as every other benchmark here: pin away from CPU 0 (absorbs `dxgkrnl.sys`'s GPU
//! interrupts) and CPU 2 (heaviest kernel DPC load) — this defaults to core 4, override with
//! `NAMIR_PIN_CORE` — on a machine verified quiet, across >= 5 repetitions with the spread
//! reported. A **certified** figure is a `docs/02-architecture.md` §2 reference-machine figure
//! only; a sandbox or dev-machine reading is informational.

use std::ffi::CStr;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use clack_host::prelude::{
    HostHandlers, HostInfo, PluginAudioConfiguration, PluginEntry, PluginInstance,
};
use clack_plugin::prelude::{DefaultPluginFactory, SinglePluginEntry};
use namir_clap::NamirClapPlugin;
use namir_core::ContentHash;
use namir_library::{FileTime, Index, IndexStore, ItemKind, ItemMetadata, LibraryEntry, Origin};

/// NFR-PERF-040's ceiling, **asserted** against the slowest repetition of every arm rather than
/// printed for a human to compare against by eye.
///
/// The FRS defines `Verify: B` as "benchmark with a numeric threshold"; a binary that only prints
/// its figure is not a `B` at all, which is the defect `crates/namir-engine/benches/
/// six_stage_chain.rs`'s own `trace-partial: NFR-PERF-010` records against itself. D-2.6's
/// *Consequence* clause names this the requirement's closing condition.
const CEILING: Duration = Duration::from_millis(200);

/// Measured repetitions per arm, after a discarded warm-up. Comfortably over D-2.4's ">= 5
/// repetitions, not one"; an instantiation is cheap enough that a wider sample costs nothing.
const DEFAULT_REPS: usize = 20;

/// Overrides [`DEFAULT_REPS`]. Clamped to at least D-2.4's five, so no invocation of this binary
/// can produce a figure below the bar the decision sets for one.
const REPS_ENV: &str = "NAMIR_INSTANTIATION_REPS";

/// D-2.4's floor on repetitions, enforced rather than documented.
const MIN_REPS: usize = 5;

/// Set by the parent process on the sandboxed child it re-execs — see this file's
/// "How the plant is made possible without `unsafe`" section. Its presence is the only difference
/// between the two branches of [`main`].
const CHILD_ENV: &str = "NAMIR_PERF_040_SANDBOX_CHILD";

/// How many rows the planted library index carries in the arms that plant one: FR-LIB-020's and
/// NFR-PERF-060's own figure for "a library", and the same figure NFR-PERF-030's harness plants.
const INDEX_ENTRIES: usize = 10_000;

/// This workspace's reference sample rate (NFR-PERF-010's, and `tests/support/mod.rs`'s default).
const REFERENCE_RATE: f64 = 48_000.0;

/// The reference maximum block size, matching `tests/support/mod.rs`'s `DEFAULT_MAX_BLOCK`.
const REFERENCE_MAX_BLOCK: u32 = 512;

/// The heaviest sample rate a host is realistically going to present.
const HEAVY_RATE: f64 = 192_000.0;

/// The heaviest maximum block size a host is realistically going to present. `activate` sizes every
/// per-block buffer in the chain from this, so it is the axis instantiation cost actually moves on.
const HEAVY_MAX_BLOCK: u32 = 4_096;

/// The minimum a host has to be, borrowed verbatim from `tests/clap_host_teardown.rs`.
///
/// `()` already implements all three handler traits (clack-host's own "QoL implementations"), and
/// nothing here observes the plugin through a host callback — the measurement is a wall clock
/// around two calls — so none of them needs a body. Notably this needs no CLAP *extension* and
/// therefore not the `host-ext-tests` feature: `PluginEntry::load_from_clack` lives in `clack-host`
/// itself, so this bench compiles and runs under a plain `cargo build --bench`.
struct BenchHost;

impl HostHandlers for BenchHost {
    type Shared<'a> = ();
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

/// One instantiation's split. `create` + `activate` == `total` by construction; all three are kept
/// so a regression can be attributed to a half rather than only observed on the sum.
#[derive(Clone, Copy)]
struct Sample {
    /// `clap_plugin_factory.create_plugin` + `clap_plugin.init` — the worker pool starting and the
    /// library index opening.
    create: Duration,
    /// `clap_plugin.activate` — `crates/namir-clap/src/audio.rs:119`, and the whole chain build
    /// behind it.
    activate: Duration,
    /// The asserted quantity: the window from before `create_plugin` to after `activate` returns.
    total: Duration,
}

// trace: NFR-PERF-040
fn main() {
    if std::env::var_os(CHILD_ENV).is_some() {
        measure_in_sandbox();
    } else {
        run_sandboxed_child();
    }
}

/// The parent branch: build a scratch configuration root, re-exec this same binary with the
/// environment `namir_platform::config_dir()` reads pointed at it, and propagate the child's
/// verdict.
///
/// The child inherits the rest of the environment, so `NAMIR_PIN_CORE` and [`REPS_ENV`] work from
/// the command line exactly as they would on a single-process benchmark. Its stdio is inherited
/// too, so its rows and its panic (if any) land on this binary's own output unmediated.
fn run_sandboxed_child() {
    let exe = std::env::current_exe().expect("this benchmark binary must have a resolvable path");
    let sandbox = std::env::temp_dir().join(format!("namir-nfr-perf-040-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sandbox);
    std::fs::create_dir_all(&sandbox).expect("a scratch configuration root must be creatable");

    println!("NFR-PERF-040: plugin instantiation in a host (create_plugin + init + activate)");
    println!(
        "Sandboxed: this binary re-execs itself with APPDATA / HOME / XDG_CONFIG_HOME pointed at\n\
         {}, so LibraryService::open_default resolves there and never at the real per-user\n\
         configuration directory. No library scan is started, here or anywhere in this crate's\n\
         benches -- see this file's own hazard note.\n",
        sandbox.display()
    );

    let status = Command::new(&exe)
        .env(CHILD_ENV, "1")
        .env("APPDATA", &sandbox)
        .env("HOME", &sandbox)
        .env("XDG_CONFIG_HOME", &sandbox)
        .status()
        .expect("the sandboxed child must be spawnable");

    let _ = std::fs::remove_dir_all(&sandbox);

    if !status.success() {
        // The child already printed its own rows and its own assertion message; adding a second
        // diagnosis here would only bury it. Just carry the verdict out.
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// The child branch: everything that touches the plugin.
fn measure_in_sandbox() {
    pin_to_measurement_core();

    let config_dir = namir_platform::config_dir().expect(
        "the sandboxed child sets APPDATA, HOME and XDG_CONFIG_HOME, so config_dir() must resolve",
    );
    std::fs::create_dir_all(&config_dir).expect("the sandbox configuration directory");
    let index_path = config_dir.join("library-index.json");

    let reps = std::env::var(REPS_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_REPS)
        .max(MIN_REPS);

    println!(
        "D-2.4: pinned away from CPU 0/2 (this run used NAMIR_PIN_CORE={}); verify the machine is\n\
         quiet before quoting anything below, and note that a certified figure is a reference-\n\
         machine (02-architecture.md section 2) figure only. {reps} measured repetitions per arm,\n\
         after one discarded warm-up.\n",
        std::env::var("NAMIR_PIN_CORE").unwrap_or_else(|_| "4 (default)".into())
    );
    println!("sandbox config dir: {}", config_dir.display());

    // Outside every window, on purpose: `clap_entry.init` is once per shared library, not once per
    // instance. Timed anyway, because a reader is entitled to know how big the thing being excluded
    // is before accepting that excluding it was fair.
    let entry_started = Instant::now();
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<NamirClapPlugin>>(c"")
        .expect("the in-process entry must load");
    println!(
        "clap_entry.init (once per library, EXCLUDED from every window below): {:.2?}\n",
        entry_started.elapsed()
    );

    // FR-CLAP-010's id, taken from the plugin's own descriptor rather than restated here, so this
    // harness cannot drift from what the entry actually advertises.
    let descriptor = <NamirClapPlugin as DefaultPluginFactory>::get_descriptor();
    let plugin_id = descriptor.id().expect("the descriptor must carry an id");
    let host_info = HostInfo::new(
        "Namir NFR-PERF-040 harness",
        "Namir",
        "https://example.invalid",
        env!("CARGO_PKG_VERSION"),
    )
    .expect("host info must be constructible from static strings");

    println!(
        "{:<44} {:>9} {:>9} {:>9} | {:>9} {:>9}",
        "arm", "min", "median", "max", "med crt", "med act"
    );

    // --- Arm 1: no library index at all. The plugin's own instantiation cost, with the library
    // contribution held at zero, so arms 2 and 3 are readable as "that, plus the index".
    let _ = std::fs::remove_file(&index_path);
    measure(
        "empty library index, 48 kHz / 512",
        &entry,
        plugin_id,
        &host_info,
        config(REFERENCE_RATE, REFERENCE_MAX_BLOCK),
        reps,
    );

    // --- Arms 2 and 3: the realistic worst case, at this project's own stated library scale.
    let index_bytes = plant_index(&config_dir, INDEX_ENTRIES);
    println!(
        "\nplanted library index: {INDEX_ENTRIES} entries, {index_bytes} bytes, at {}",
        index_path.display()
    );
    measure(
        "10 000-entry index, 48 kHz / 512",
        &entry,
        plugin_id,
        &host_info,
        config(REFERENCE_RATE, REFERENCE_MAX_BLOCK),
        reps,
    );
    measure(
        "10 000-entry index, 192 kHz / 4096",
        &entry,
        plugin_id,
        &host_info,
        config(HEAVY_RATE, HEAVY_MAX_BLOCK),
        reps,
    );

    println!(
        "\nPASS: every arm's slowest instantiation stayed inside NFR-PERF-040's {CEILING:?}\n\
         ceiling, excluding model loading (this binary owns no model or IR to load) and excluding\n\
         the once-per-library clap_entry.init reported above."
    );
}

/// Runs `reps` measured instantiations of `configuration` after one discarded warm-up, reports the
/// distribution, and asserts NFR-PERF-040's ceiling against the slowest of them.
fn measure(
    label: &str,
    entry: &PluginEntry,
    plugin_id: &CStr,
    host_info: &HostInfo,
    configuration: PluginAudioConfiguration,
    reps: usize,
) {
    // Discarded: the first instantiation in a process pays one-time costs no later one does --
    // `namir_platform::logging::init`'s `Once`, the allocator's first touch of every arena the
    // chain uses, and first-fault paging of code neither of the two calls has executed yet. A host
    // pays those once per *session*, not once per instance, and NFR-PERF-040 is a per-instance
    // requirement. It is not silently dropped: the arm's own row below is preceded by nothing, so
    // the warm-up figure is printed rather than merely subtracted.
    let warm_up = instantiate_once(entry, plugin_id, host_info, configuration);

    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        samples.push(instantiate_once(entry, plugin_id, host_info, configuration));
    }

    let mut totals: Vec<Duration> = samples.iter().map(|s| s.total).collect();
    let mut creates: Vec<Duration> = samples.iter().map(|s| s.create).collect();
    let mut activates: Vec<Duration> = samples.iter().map(|s| s.activate).collect();
    totals.sort_unstable();
    creates.sort_unstable();
    activates.sort_unstable();

    let min = totals[0];
    let median = totals[totals.len() / 2];
    let max = *totals.last().expect("reps is at least MIN_REPS");
    let median_create = creates[creates.len() / 2];
    let median_activate = activates[activates.len() / 2];

    // Printed first, asserted second, on the house pattern from `namir-worker`'s `resource_load.rs`
    // and for its stated reason: a failing arm still leaves its own measured row above the panic,
    // which is what a reader needs in order to judge whether the run was contaminated.
    println!(
        "{label:<44} {min:>9.2?} {median:>9.2?} {max:>9.2?} | {median_create:>9.2?} \
         {median_activate:>9.2?}   (warm-up {:.2?}, discarded)",
        warm_up.total
    );

    // On `max` rather than the median: "instantiation ... shall complete within 200 ms" is a
    // statement about an instantiation, not about a median instantiation.
    assert!(
        max <= CEILING,
        "NFR-PERF-040: {label} -- the slowest of {reps} instantiations took {max:.2?}, over the \
         {CEILING:?} ceiling (min {min:.2?}, median {median:.2?}; median split: create+init \
         {median_create:.2?}, activate {median_activate:.2?}). D-2.4: one reading on a machine \
         that was not verified quiet is not evidence of a regression -- re-run pinned \
         (NAMIR_PIN_CORE) >= 5 times before believing this, and note that a certified figure is a \
         reference-machine (02-architecture.md section 2) figure only"
    );
}

/// One create/init/activate/deactivate/destroy cycle, with the clock around the first three.
///
/// Deactivation and destroy are outside the window (see this file's doc comment) but inside the
/// loop, so each repetition starts from the same state: `impl Drop for NamirShared` joins the
/// instance's worker pool before returning, which `tests/clap_host_teardown.rs` asserts directly.
fn instantiate_once(
    entry: &PluginEntry,
    plugin_id: &CStr,
    host_info: &HostInfo,
    configuration: PluginAudioConfiguration,
) -> Sample {
    let opened = Instant::now();
    let mut instance =
        PluginInstance::<BenchHost>::new(|_| (), |_| (), entry, plugin_id, host_info)
            .expect("the plugin must instantiate");
    let created = Instant::now();
    let processor = instance
        .activate(|_, _| (), configuration)
        .expect("the plugin must activate");
    let closed = Instant::now();

    instance.deactivate(processor);
    drop(instance); // `clap_plugin.destroy`

    Sample {
        create: created.duration_since(opened),
        activate: closed.duration_since(created),
        total: closed.duration_since(opened),
    }
}

/// A `PluginAudioConfiguration` accepting 1..=`max_frames` frames at `rate`.
fn config(rate: f64, max_frames: u32) -> PluginAudioConfiguration {
    PluginAudioConfiguration {
        sample_rate: rate,
        min_frames_count: 1,
        max_frames_count: max_frames,
    }
}

/// Writes an index of `entries` rows where `LibraryService::open_at` will read it
/// (`<config_dir>/library-index.json`). Returns its size in bytes, which is the thing an
/// instantiation actually pays for.
///
/// Copied in shape from `crates/namir-app/benches/startup_to_audible.rs`'s `plant_warm_index`, and
/// for the same reason: the rows are synthetic but realistically *shaped* — a plausible path, a
/// real content hash and a populated `NamItemMetadata` — because what instantiation pays is a JSON
/// parse, and 10 000 rows of empty metadata would understate it. No file exists behind any row;
/// nothing is ever read but this index, so D-19.1's generated-never-captured rule is not in play.
fn plant_index(config_dir: &Path, entries: usize) -> u64 {
    let index_path = config_dir.join("library-index.json");
    let root = config_dir.join("Library");
    let mut index = Index::empty();
    for n in 0..entries {
        let name = format!("maker-{:03}/model-{n:05}.nam", n % 250);
        index.upsert(LibraryEntry {
            path: root.join(&name),
            kind: ItemKind::Nam,
            size: 512 * 1024 + n as u64,
            mtime: FileTime::now(),
            hash: Some(ContentHash::of(name.as_bytes())),
            metadata: ItemMetadata::Nam(namir_library::NamItemMetadata {
                architecture: "WaveNet".to_string(),
                sample_rate: Some(48_000),
                name: format!("Model {n:05}"),
                modeled_by: "NFR-PERF-040 harness".to_string(),
                gear_type: "Amplifier".to_string(),
                tone_type: "Crunch".to_string(),
                description:
                    "A synthetic library-index row, planted so the index this measurement \
                              opens is a stated scale rather than whatever this machine owns."
                        .to_string(),
            }),
            origin: Origin::Local,
        });
    }
    let (store, _existing, _warnings) = IndexStore::open(index_path.clone());
    store
        .save_atomic(&index)
        .expect("the sandbox index should be writable");
    std::fs::metadata(&index_path).map(|m| m.len()).unwrap_or(0)
}

/// See `namir-engine`'s `six_stage_chain.rs` for the full measured argument against CPU 0 (GPU
/// driver ISRs) and CPU 2 (kernel DPCs) on the reference machine. Defaults to index 4; override
/// with `NAMIR_PIN_CORE`. Index is clamped into range, so this is safe on machines with few cores.
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
