//! **NFR-PERF-030**: "the standalone application shall reach an audible state (audio streaming,
//! default state loaded) within 3 seconds on the reference machine with a warm library index."
//! `*Verify:* B` — so this binary **asserts** the 3 s ceiling rather than printing a figure for a
//! reader to judge, which is the whole difference between a `Verify: B` artifact and a report
//! (D-23.1's second question, and the sin NFR-PERF-010's own bench is on record for).
//!
//! # What is measured, exactly
//!
//! One real `namir` process per repetition. The clock starts immediately before
//! `Command::spawn` and stops the instant `crate::startup_probe`'s **audible marker** is read off
//! that process's stdout — emitted by `namir-app` at the instant
//! `crate::stream::RunningStreams::play` returns `Ok(())`, which that method's own doc comment
//! calls "the one call that actually makes audio flow". The interval therefore contains everything
//! a user waits through: process creation, image load and dynamic linking, settings load, device
//! enumeration and negotiation, engine build, library-index read, worker start-up, stream
//! construction, and the stream start itself.
//!
//! It deliberately does **not** contain the window. `namir-app` opens its window
//! (`namir_ui::open_blocking`) *after* audio is running, so "audible" and "visible" are different
//! instants and this requirement asks for the first.
//!
//! Two figures are reported that are not the measured one, and neither is asserted:
//! `in_process_ms`, the probe's own view from the first statement of `namir_app::app::run`, which
//! attributes an over-budget measurement to either the process launch or Namir's own work; and the
//! warm-up repetition, below.
//!
//! # "With a warm library index"
//!
//! A precondition this harness *establishes* rather than inherits. It writes an index of
//! [`INDEX_ENTRIES`] entries into a throwaway configuration directory and points the probed process
//! at it ([`namir_app::startup_probe`]'s one environment variable carries that directory), then
//! runs one **discarded warm-up** launch before the measured ones, so the index file is in the
//! page cache and no measured repetition is paying a first-touch cost the requirement excludes.
//! 10 000 is this project's own stated library scale — FR-LIB-020 and NFR-PERF-060 both name it —
//! rather than a number invented here.
//!
//! No file is generated behind those entries, and none is needed: start-up *reads* the index and
//! never scans (a scan is `UiIntent`-driven, `namir_worker::library::LibraryService::start_scan`),
//! so what a launch pays for a warm library is the parse of that file, which this reproduces
//! exactly. Every measured launch consequently starts from a fresh, default `AppSettings` too — no
//! remembered device, no remembered rate — which is a first-launch-shaped negotiation with a warm
//! index, and the slower of the two device paths rather than the flattering one.
//!
//! The two preconditions in the requirement's own parenthetical are checked, not assumed: each
//! marker carries the index size the launch actually read and the number of parameters
//! `namir_state::State::defaults()` carried, and both are asserted (against [`INDEX_ENTRIES`] and
//! against `namir_params::REGISTRY`'s own length) before any timing is believed.
//!
//! # Three outcomes, not two
//!
//! - **Audible** — the marker arrived; the repetition is timed.
//! - **Not audible** — the process reported that it settled without audio. Two shapes: no usable
//!   device (or an engine that would not prepare), and the softer case where devices were found,
//!   the engine was wired, and `stream::open`/`play` then failed — a launch that in ordinary use
//!   opens its window with no audio behind it. Both are reported as *what they are*; neither is
//!   counted as a slow start-up, and each is printed with the probe's own `detail` text and the
//!   process's stderr behind it, since one of the two produces neither on its own. A machine in
//!   this state cannot certify NFR-PERF-030 at all, so by
//!   default this is a **hard failure**: a benchmark whose threshold quietly evaporates on the
//!   machines it happens to run on is worse than one that says it could not run. Set
//!   [`ALLOW_NOT_AUDIBLE_ENV`] to make it an explicit, printed skip — CI does, because no
//!   GitHub-hosted runner has an audio device.
//! - **Timeout** — no marker of either kind within [`WATCHDOG`]. Distinct from both: the process
//!   is neither audible nor admitting it isn't.
//!
//! # Read this before quoting any number from this binary
//!
//! D-2.4 governs, with one part of it that does **not** apply here and is worth saying so
//! explicitly: this binary does not pin a core. Every other benchmark in this workspace measures
//! work on one thread and pins it away from CPU 0/2; this one measures a whole process starting up
//! across several threads plus the OS's own loader, and pinning the parent would not reach the
//! child while pinning the child would measure something no user experiences. The rest of D-2.4
//! binds as usual: take >= 5 repetitions (this binary's default, overridable with [`REPS_ENV`]),
//! run on a machine verified quiet, and remember that the **certified** figure for this Must is
//! only ever one measured on `docs/02-architecture.md` §2's pinned reference machine — a sandbox or
//! dev-machine number is informational and never the number recorded as closing the requirement.
//!
//! This benchmark is not on CI's critical path in any form that asserts, so an absolute wall-clock
//! threshold here cannot make CI flaky.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use namir_app::startup_probe::{
    AUDIBLE_MARKER, NOT_AUDIBLE_MARKER, PROBE_ENV, REASON_NO_AUDIO_DEVICE,
    REASON_STREAM_NOT_STARTED, detail, field,
};
use namir_core::ContentHash;
use namir_library::{FileTime, Index, IndexStore, ItemKind, ItemMetadata, LibraryEntry, Origin};

/// NFR-PERF-030's ceiling, asserted against the slowest measured repetition.
const CEILING: Duration = Duration::from_secs(3);

/// How many entries the warm index this harness plants carries — FR-LIB-020's and NFR-PERF-060's
/// own figure for "a library", so the warm-index precondition is exercised at this project's
/// stated scale rather than at whatever the measuring machine happens to own.
const INDEX_ENTRIES: usize = 10_000;

/// Measured repetitions, after the discarded warm-up. D-2.4's ">= 5 repetitions, not one".
const DEFAULT_REPS: usize = 5;

/// How long one launch is given to emit a marker of either kind before it is called a timeout and
/// killed. Ten times the ceiling deliberately: a launch that is merely over budget must be
/// *measured and reported* against the threshold, never truncated into a different failure.
const WATCHDOG: Duration = Duration::from_secs(30);

/// How long a launch is given to exit after its marker, before it is killed. Only affects how
/// quickly the next repetition starts; nothing measured happens after the marker.
const EXIT_GRACE: Duration = Duration::from_secs(10);

/// Overrides [`DEFAULT_REPS`].
const REPS_ENV: &str = "NAMIR_STARTUP_REPS";

/// Set (to anything) to turn "this machine never became audible" from a failure into a printed
/// skip. For a machine with no audio device — every CI runner — and nothing else.
const ALLOW_NOT_AUDIBLE_ENV: &str = "NAMIR_STARTUP_ALLOW_NOT_AUDIBLE";

/// How one launch ended.
enum Outcome {
    Audible {
        /// Wall clock from immediately before `Command::spawn` to the marker being read.
        elapsed: Duration,
        /// The probe's own view, from the first statement of `namir_app::app::run`. Diagnostic.
        in_process: Duration,
        /// The size of the index that launch read — checked against [`INDEX_ENTRIES`].
        library_index_entries: usize,
        /// The size of the default state that launch built — checked against `REGISTRY`.
        default_state_params: usize,
    },
    /// The launch said it settled without audio, with the probe's reason token and whatever
    /// detail it carried. The detail matters for `stream-not-started`: the underlying error goes
    /// into a UI notice a probed launch never opens a window to show, so without it this outcome
    /// arrives with an empty stderr behind it.
    NotAudible {
        reason: String,
        detail: Option<String>,
    },
    /// No marker of either kind within [`WATCHDOG`].
    Timeout,
}

// trace-partial: NFR-PERF-030
// uncovered: NFR-PERF-030 — "audible" is taken to be RunningStreams::play() returning Ok(()), the
// uncovered: instant both streams are told to run: no output callback is observed to have
// uncovered: processed a block, so a launch that started its streams and then produced no sound
// uncovered: would still be timed as having reached an audible state, and nothing automated
// uncovered: anywhere confirms audio left the interface. The stronger marking event was
// uncovered: considered and rejected: it needs an observable inside crate::stream's audio
// uncovered: callback, added purely to enable a measurement; closes M9b
fn main() {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_namir"));
    let reps = std::env::var(REPS_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_REPS)
        .max(1);

    println!("NFR-PERF-030: standalone start-up to an audible state (whole-process wall clock)");
    println!(
        "D-2.4: no core pinning here (see this file's own doc comment for why); verify the machine \n\
         is quiet and take >= 5 repetitions. A certified figure is a reference-machine \n\
         (02-architecture.md section 2) figure only.\n"
    );

    let config_dir = scratch_config_dir();
    let _ = std::fs::remove_dir_all(&config_dir);
    std::fs::create_dir_all(&config_dir).expect("a scratch configuration directory");
    let index_bytes = plant_warm_index(&config_dir, INDEX_ENTRIES);
    println!(
        "warm library index: {INDEX_ENTRIES} entries, {index_bytes} bytes, at {}",
        config_dir.display()
    );
    println!("binary under measurement: {}\n", exe.display());

    // The warm-up. Discarded, but reported: the difference between it and the measured runs is the
    // first-touch cost the requirement's "warm" excludes, and a reader should be able to see it.
    let warm_up = launch(&exe, &config_dir);
    match &warm_up {
        Outcome::Audible { elapsed, .. } => {
            println!("warm-up (discarded)          {elapsed:>9.2?}");
        }
        other => {
            finish_without_measuring(other, &config_dir);
            return;
        }
    }

    let mut measured = Vec::with_capacity(reps);
    for rep in 1..=reps {
        let outcome = launch(&exe, &config_dir);
        match outcome {
            Outcome::Audible {
                elapsed,
                in_process,
                library_index_entries,
                default_state_params,
            } => {
                println!(
                    "rep {rep:>2}/{reps}                    {elapsed:>9.2?} | in-process \
                     {in_process:>9.2?} | index {library_index_entries} | default params \
                     {default_state_params}"
                );
                assert_eq!(
                    library_index_entries, INDEX_ENTRIES,
                    "NFR-PERF-030's \"with a warm library index\" precondition did not hold: this \
                     launch read an index of {library_index_entries} entries, not the \
                     {INDEX_ENTRIES} this harness planted, so whatever it timed was not the \
                     condition the requirement states"
                );
                assert_eq!(
                    default_state_params,
                    namir_params::REGISTRY.len(),
                    "NFR-PERF-030's \"default state loaded\" precondition did not hold: this \
                     launch built a default state of {default_state_params} parameters against a \
                     REGISTRY of {}",
                    namir_params::REGISTRY.len()
                );
                measured.push(elapsed);
            }
            other => {
                finish_without_measuring(&other, &config_dir);
                return;
            }
        }
    }

    measured.sort_unstable();
    let min = measured[0];
    let median = measured[measured.len() / 2];
    let max = *measured.last().unwrap();
    println!("\n{reps} measured: min {min:.2?} | median {median:.2?} | max {max:.2?}");

    let _ = std::fs::remove_dir_all(&config_dir);

    // Printed first, asserted second — the same order `namir-worker`'s `resource_load.rs` uses, and
    // for the same reason: a failing run still leaves its own measured rows above the panic, which
    // is what a reader needs in order to judge whether the machine was contaminated.
    //
    // On `max` rather than the median: "shall reach an audible state ... within 3 seconds" is a
    // statement about a launch, not about a median launch.
    assert!(
        max <= CEILING,
        "NFR-PERF-030: the slowest of {reps} launches took {max:.2?}, over the {CEILING:?} \
         ceiling (min {min:.2?}, median {median:.2?}). D-2.4: one reading on a machine that was \
         not verified quiet is not evidence of a regression -- re-run >= 5 times on a quiet \
         machine before believing this, and note that a certified figure is a reference-machine \
         (02-architecture.md section 2) figure only"
    );
    println!(
        "\nPASS: every one of {reps} launches reached an audible state inside NFR-PERF-030's \
         {CEILING:?} ceiling, with a warm {INDEX_ENTRIES}-entry library index."
    );
}

/// The one place a non-audible or timed-out launch is turned into a verdict, so the default
/// (refuse to pass) and the opt-out (an explicit, printed skip) are stated once.
fn finish_without_measuring(outcome: &Outcome, config_dir: &Path) {
    let _ = std::fs::remove_dir_all(config_dir);
    let what = match outcome {
        Outcome::Audible { .. } => unreachable!("an audible launch is measured, not skipped"),
        Outcome::NotAudible { reason, detail } => {
            let said = match reason.as_str() {
                REASON_NO_AUDIO_DEVICE => {
                    "this machine has no usable audio device (or the engine would not prepare for \
                     it), so `namir` never opened a stream at all"
                }
                REASON_STREAM_NOT_STARTED => {
                    "devices were found and the engine was wired, but the stream failed to open or \
                     to start -- a distinct failure from a slow start-up, and not a timing result"
                }
                _ => "`namir` settled without audio",
            };
            match detail {
                Some(detail) => format!("{said} (reason={reason}: {detail})"),
                None => format!("{said} (reason={reason})"),
            }
        }
        Outcome::Timeout => format!(
            "`namir` emitted no marker of any kind within {WATCHDOG:?} -- neither audible nor \
             admitting that it would not be"
        ),
    };

    if std::env::var_os(ALLOW_NOT_AUDIBLE_ENV).is_some() {
        println!(
            "\nSKIPPED -- NOT MEASURED, NOTHING ASSERTED: {what}. {ALLOW_NOT_AUDIBLE_ENV} is set, \
             so this is a skip rather than a failure. NFR-PERF-030 is unverified by this run."
        );
        return;
    }
    panic!(
        "NFR-PERF-030 cannot be measured on this machine: {what}. This is not a timing failure \
         and says nothing about the 3 s ceiling. Set {ALLOW_NOT_AUDIBLE_ENV} to make this an \
         explicit skip on a machine that is known to have no audio device"
    );
}

/// Spawns one `namir` under the probe and times it to its marker.
fn launch(exe: &Path, config_dir: &Path) -> Outcome {
    let mut command = Command::new(exe);
    command
        .env(PROBE_ENV, config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // The clock starts here, before the process exists: creating it is part of launching it, and a
    // user waits for that too.
    let started = Instant::now();
    let mut child = command
        .spawn()
        .expect("the `namir` binary should be spawnable");

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // Drained on its own thread rather than left to fill its pipe -- `namir-app` writes several
    // start-up lines to stderr, and a full pipe would block the process being timed.
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        let mut stderr = stderr;
        let _ = stderr.read_to_string(&mut buffer);
        buffer
    });

    // The marker's arrival is stamped on the reading thread, not after a channel hop.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.starts_with(AUDIBLE_MARKER) || line.starts_with(NOT_AUDIBLE_MARKER) {
                let _ = tx.send((Instant::now(), line));
                return;
            }
        }
    });

    let received = rx.recv_timeout(WATCHDOG);
    let outcome = match received {
        Ok((at, line)) if line.starts_with(AUDIBLE_MARKER) => {
            let elapsed = at.duration_since(started);
            Outcome::Audible {
                elapsed,
                in_process: parse_millis(&line),
                library_index_entries: parse_usize(&line, "library_index_entries"),
                default_state_params: parse_usize(&line, "default_state_params"),
            }
        }
        Ok((_, line)) => Outcome::NotAudible {
            reason: field(&line, "reason").unwrap_or("unstated").to_string(),
            detail: detail(&line).map(str::to_string),
        },
        // Disconnected: the process closed stdout without ever marking -- it exited or crashed
        // before reaching either branch of the seam. Reported as a timeout's sibling rather than
        // as a slow launch, since no interval was measured either way.
        Err(mpsc::RecvTimeoutError::Disconnected) => Outcome::NotAudible {
            reason: "process-exited-without-marking".to_string(),
            detail: None,
        },
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            Outcome::Timeout
        }
    };

    wait_for_exit(&mut child);
    let stderr = stderr_reader.join().unwrap_or_default();
    if !matches!(outcome, Outcome::Audible { .. }) {
        // A launch that produced no figure has to say why in the reader's terms, not just in the
        // probe's one-token one.
        for line in stderr.lines() {
            println!("  [namir stderr] {line}");
        }
    }
    outcome
}

/// Nothing measured happens after the marker, so this only bounds how long the next repetition
/// waits for the previous process to release its device.
fn wait_for_exit(child: &mut Child) {
    let deadline = Instant::now() + EXIT_GRACE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn parse_usize(line: &str, key: &str) -> usize {
    field(line, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("the audible marker should carry a numeric {key}: {line}"))
}

fn parse_millis(line: &str) -> Duration {
    let ms: f64 = field(line, "in_process_ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(f64::NAN);
    if ms.is_finite() && ms >= 0.0 {
        Duration::from_secs_f64(ms / 1000.0)
    } else {
        Duration::ZERO
    }
}

/// A per-process scratch directory, so two runs of this binary cannot share an index file.
fn scratch_config_dir() -> PathBuf {
    std::env::temp_dir().join(format!("namir-nfr-perf-030-{}", std::process::id()))
}

/// Writes an index of `entries` rows under `config_dir`, at the path
/// `LibraryService::open_at` will read (`<config_dir>/library-index.json`). Returns its size in
/// bytes, which is the thing a start-up actually pays for.
///
/// The rows are synthetic but realistically *shaped* — a plausible path, a real content hash and a
/// populated `NamItemMetadata` — because what start-up pays is a JSON parse, and an index of 10 000
/// rows with empty metadata would understate it. No file exists behind any row (D-19.1's
/// generated-never-captured rule is not even in play: nothing is read but this index).
fn plant_warm_index(config_dir: &Path, entries: usize) -> u64 {
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
                modeled_by: "NFR-PERF-030 harness".to_string(),
                gear_type: "Amplifier".to_string(),
                tone_type: "Crunch".to_string(),
                description: "A synthetic library-index row, planted to make the warm-index \
                              precondition a stated condition of the measurement."
                    .to_string(),
            }),
            origin: Origin::Local,
        });
    }
    let (store, _existing, _warnings) = IndexStore::open(index_path.clone());
    store
        .save_atomic(&index)
        .expect("the scratch index should be writable");
    std::fs::metadata(&index_path).map(|m| m.len()).unwrap_or(0)
}
