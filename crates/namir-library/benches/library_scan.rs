//! NFR-PERF-060: "an unchanged 10,000-file library rescan completes within 2 seconds," and
//! FR-LIB-030: "the second (incremental) scan of an unchanged library is measurably faster than
//! the first." Follows `namir-engine/benches/handover_crossfade.rs`'s structure and its D-2.4
//! discipline.
//!
//! # Honest note on build order
//!
//! The plan this milestone followed wanted this benchmark landed against a full-rescan-only
//! `Scanner` first, to record a genuine "before" number, with the incremental path (D-12.1) built
//! and re-measured afterward — the same measure-before-mitigating discipline M4 used for R-7. That
//! opportunity has already passed by the time this benchmark exists: `scan.rs`'s incremental rule
//! was built in from `namir-library`'s first commit this milestone, so there is no "before"
//! implementation left to measure against. Recorded here rather than pretended around: what
//! follows is the first real measurement of both costs, not a before/after comparison.
//!
//! # Five arms
//!
//! | Arm | What runs | Purpose |
//! |---|---|---|
//! | A | Full scan, first touch of this run's own working copy | The cold number. Reported, not gated — see below for why "first touch" here is not literal cold-page-cache. |
//! | B | Full scan again, same corpus, page cache now warm | The honest denominator for FR-LIB-030's "first start-up" — comparing A to E would measure cache-warming as much as incrementality. |
//! | C | Incremental scan, index present **in memory**, corpus unchanged | **NFR-PERF-060's own figure.** Gated at 2 s. |
//! | D | Incremental scan, 1% (100) of files modified | Correctness guard — B and C alone are passed trivially by an incremental scan that checks nothing and returns the stale index instantly. |
//! | E | `IndexStore::open` off disk **then** an incremental scan | **FR-LIB-030's own figure** (added M14). Gated at `max(E) < min(B)`. |
//!
//! Assertions never compare two means: NFR-PERF-060 is `max` over 5 repetitions of arm C `<= 2.0
//! s`, absolute, as the requirement literally states it; FR-LIB-030's "measurably faster" is
//! `max(E) < min(B)` across the same repetitions — no overlap between the two distributions. If
//! the ranges touch, the run **fails**, rather than being rounded to a pass.
//!
//! # Why arm E exists, and why arm C is not FR-LIB-030's figure (added M14)
//!
//! FR-LIB-030 is "the library index shall be **persisted between sessions** and updated
//! incrementally, so that startup does not require a full rescan", verified by "second start-up
//! with an unchanged 10 000-file library shall be measurably faster than the first". Arm C never
//! touches the disk: its prior index is one this process's own earlier scan left in the heap, so
//! it measures incrementality with the persistence half assumed. Arm E runs the sequence a real
//! second start-up runs — `IndexStore::open` off the index file, then `Scanner` against what came
//! back — and times both halves together, because both are start-up cost. Arm B is the same
//! sequence with no index file to find, which is what a *first* start-up is. Two consequences
//! worth stating: `E - C` is what persistence itself costs (the JSON parse of a 10 000-entry
//! index), and a regression that made the index file unreadable would show up here as arm E
//! failing its reload assertion rather than as a silently-still-fast arm C.
//!
//! Until M14 this file's FR-LIB-030 comparison was printed as `CONCLUSIVE`/`INCONCLUSIVE` for a
//! human to read and asserted by nothing, which under D-23.1's second question is not a
//! `Verify: B` being executed. It is an assertion now.
//!
//! # First run of arm E, and the finding it produced (INFORMATIONAL — sandbox, not the reference
//! machine)
//!
//! Five repetitions in this shared development sandbox, un-quiesced, on a Linux tmpfs-backed
//! working directory: `min(B)` 120.4–132.4 ms, `max(C)` 29.4–36.4 ms, **`max(E)` 103.1–113.1 ms**.
//! Every repetition passed, and none passed comfortably. The gap between arms C and E is the
//! finding: **reloading the persisted index costs about 70 ms — roughly twice the incremental scan
//! it enables.** `IndexStore` writes the whole index as one JSON document (`store.rs`'s `OnDisk`)
//! and `serde_json` re-parses 10 000 entries on every start-up, so FR-LIB-030's own margin over a
//! full rescan is roughly 15 %, not the ~4x that arm C alone would suggest. That margin is what
//! this assertion now defends, and it would be the first thing to go if the index format or the
//! entry count grew. Recorded here rather than left for the next reader to rediscover; per D-2.4
//! none of these are certified figures, and the reference machine's own ratio may differ (its
//! filesystem is NTFS, where the directory walk in arms B and E costs more and the JSON parse
//! costs the same).
//!
//! # Why every file's mtime is backdated by an hour before any scan runs
//!
//! D-12.1's mtime-settling-window fix (`scan.rs`'s own module doc, added this milestone) treats
//! *any* file whose mtime lands within ~2 s of the previous scan's completion time as suspect and
//! rehashes it regardless of whether size/mtime otherwise match — deliberately, so a same-length
//! edit landing inside one mtime tick is never permanently invisible. Copying the 10,000-file
//! corpus and then immediately scanning it would put every file's mtime within that window of the
//! very scan meant to record it as a baseline, forcing arm C to rehash everything and both
//! defeating the point of this benchmark and blowing NFR-PERF-060's 2 s ceiling on a false
//! positive. Backdating every file's mtime by an hour before the baseline scan runs sidesteps this
//! deterministically rather than racing real wall-clock time.
//!
//! # Why the corpus is copied rather than scanned in place
//!
//! `namir_fixtures::library::generate_shared_corpus`'s tree is deliberately read-only and shared
//! across every caller and test run in this workspace (see that module's own doc comment) — arm D
//! needs to mutate 100 files, which would violate that invariant for everyone else. This
//! benchmark copies the shared corpus into a private working directory once, outside every timed
//! window, and only ever touches that copy.
//!
//! # Read this before quoting any number from this binary
//!
//! D-2.4 governs: pin away from CPU 0/2 (default core 4, override `NAMIR_PIN_CORE`), on a machine
//! verified quiet, and do not poll the run.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use namir_core::ContentHash;
use namir_library::{Index, IndexStore, Scanner, StdFs};

const REPETITIONS: usize = 5;
const MODIFIED_FRACTION_COUNT: usize = 100; // 1% of the 10,000-file corpus.
const SETTLING_BACKDATE_SECONDS: u64 = 3_600; // See this file's own module doc comment.

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

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create working directory");
    for entry in std::fs::read_dir(src).expect("read corpus directory") {
        let entry = entry.expect("read corpus directory entry");
        let dest_path = dst.join(entry.file_name());
        if entry.file_type().expect("entry file type").is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path);
        } else {
            std::fs::copy(entry.path(), &dest_path).expect("copy corpus file");
        }
    }
}

/// Backdates every regular file under `dir`, recursively — see this file's module doc comment on
/// why this runs once, up front, before any scan establishes a baseline.
fn age_all_mtimes(dir: &Path, seconds_ago: u64) {
    let past = std::time::SystemTime::now() - Duration::from_secs(seconds_ago);
    for entry in std::fs::read_dir(dir).expect("read directory to age") {
        let entry = entry.expect("read directory entry to age");
        let path = entry.path();
        if entry.file_type().expect("entry file type").is_dir() {
            age_all_mtimes(&path, seconds_ago);
        } else {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .expect("open file to age")
                .set_modified(past)
                .expect("set mtime");
        }
    }
}

fn full_scan(roots: Vec<PathBuf>) -> (Duration, Index) {
    let started = Instant::now();
    let delta = Scanner::new(roots, &Index::empty()).run_to_completion(&StdFs);
    let elapsed = started.elapsed();
    let mut index = Index::empty();
    index.apply(delta);
    (elapsed, index)
}

fn incremental_scan(roots: Vec<PathBuf>, prior: &Index) -> (Duration, namir_library::ScanDelta) {
    let started = Instant::now();
    let delta = Scanner::new(roots, prior).run_to_completion(&StdFs);
    let elapsed = started.elapsed();
    (elapsed, delta)
}

/// Arm D's mutation: XOR-flips every byte of `count` real files already in `index` (works
/// regardless of whether a given path is `.nam` JSON text or `.wav` binary, and guarantees
/// different content — and therefore a different hash — without needing to construct a
/// format-valid replacement). Preserves file length, so this is a genuine same-size edit, not a
/// truncation or growth `Scanner`'s size check alone could already catch — exactly the case
/// D-12.1's own fix targets. Returns the modified paths for the correctness assertions.
fn modify_files(index: &Index, count: usize) -> Vec<PathBuf> {
    let paths: Vec<PathBuf> = index.iter().take(count).map(|e| e.path.clone()).collect();
    for path in &paths {
        let mut bytes = std::fs::read(path).expect("read file to modify");
        for b in bytes.iter_mut() {
            *b ^= 0xFF;
        }
        std::fs::write(path, &bytes).expect("write modified file");
    }
    paths
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    pin_to_measurement_core();

    println!("NFR-PERF-060 / FR-LIB-030: library scan cost, 10,000-file shared corpus");
    println!(
        "D-2.4: pin away from CPU 0/2 (this run used NAMIR_PIN_CORE={}), verify the machine is \n\
         quiet, and do not poll the run.\n",
        std::env::var("NAMIR_PIN_CORE").unwrap_or_else(|_| "4 (default)".into())
    );

    let corpus =
        namir_fixtures::library::generate_shared_corpus(1).expect("corpus should generate");
    let work_dir = std::env::temp_dir().join(format!("namir-library-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work_dir);

    println!(
        "copying the shared corpus into a private, mutable working directory (setup, not measured)..."
    );
    let copy_started = Instant::now();
    copy_dir_recursive(&corpus.root, &work_dir);
    println!("copy took {:?}", copy_started.elapsed());

    println!(
        "backdating every file's mtime by {SETTLING_BACKDATE_SECONDS}s (see this file's own doc comment on D-12.1's settling window)...\n"
    );
    age_all_mtimes(&work_dir, SETTLING_BACKDATE_SECONDS);

    let roots = vec![work_dir.clone()];

    // Arm A: full scan, first touch of this run's own working copy. Reported, not gated -- "first
    // touch" here means this benchmark's own first read, not literal OS cold-page-cache, which no
    // unprivileged process can reliably force. Its own copy step already read every source byte
    // once, so treat this as a lower bound on the cold number, not a rigorous one.
    let (a_elapsed, _a_index) = full_scan(roots.clone());
    println!(
        "A  full scan, first touch                : {:>9.1} ms",
        ms(a_elapsed)
    );

    // Arm B: full scan again, same corpus, page cache warm from arm A. >= 5 repetitions.
    let mut b_durations = Vec::with_capacity(REPETITIONS);
    for _ in 0..REPETITIONS {
        let (elapsed, _index) = full_scan(roots.clone());
        b_durations.push(elapsed);
    }
    for (i, d) in b_durations.iter().enumerate() {
        println!(
            "B{i}  full scan, warm cache, no index       : {:>9.1} ms",
            ms(*d)
        );
    }

    // The prior index arms C and D both scan against: one more full scan (cache is thoroughly
    // warm by now), not timed as part of either arm -- it is setup, establishing the "index
    // present, corpus unchanged" starting condition.
    let (_prior_elapsed, prior_index) = full_scan(roots.clone());
    assert_eq!(
        prior_index.len(),
        corpus.entries.len(),
        "the working copy's scan should find exactly the corpus's own file count"
    );

    // Arm C: incremental scan, unchanged corpus. >= 5 repetitions. NFR-PERF-060's own figure.
    let mut c_durations = Vec::with_capacity(REPETITIONS);
    for _ in 0..REPETITIONS {
        let (elapsed, delta) = incremental_scan(roots.clone(), &prior_index);
        assert!(
            delta.upserts.is_empty(),
            "arm C: an unchanged corpus must upsert nothing, got {} upserts",
            delta.upserts.len()
        );
        c_durations.push(elapsed);
    }
    for (i, d) in c_durations.iter().enumerate() {
        println!(
            "C{i}  incremental scan, unchanged corpus    : {:>9.1} ms",
            ms(*d)
        );
    }

    // ---- Arm E (added M14): the real second start-up, index round-tripped through disk. ----
    //
    // Arms C and D hand `incremental_scan` a prior `Index` that never left memory, which measures
    // incrementality but not *persistence* -- and FR-LIB-030's sentence is "the library index
    // shall be **persisted between sessions** and updated incrementally, so that startup does not
    // require a full rescan", verified by "second start-up with an unchanged 10 000-file library".
    // A second start-up begins with an `IndexStore::open` off a file, not with an index a previous
    // scan happened to leave in this process's heap. So arm E reproduces the whole sequence a real
    // second start-up runs, and times all of it:
    //
    //     IndexStore::open(index.json)  ->  Scanner::new(roots, &loaded)  ->  run_to_completion
    //
    // The first start-up it is compared against is arm B, which is the same sequence with no index
    // file to find: `IndexStore::open` on a missing path returns an empty index, which is exactly
    // what `full_scan` already hands `Scanner::new`. So `E vs B` is literally "second start-up vs
    // first start-up", which is what the requirement's `*Verify:*` line asks to be compared.
    //
    // Ordered before arm D, deliberately: arm D leaves 100 files modified, and this arm's own
    // condition is "an unchanged 10 000-file library". The `save_atomic` is outside the timed
    // window -- writing the index is the *first* session's shutdown cost, not the second session's
    // start-up cost.
    let index_path = sibling_index_path(&work_dir);
    let _ = std::fs::remove_file(&index_path);
    let (store, empty, warnings) = IndexStore::open(index_path.clone());
    assert!(
        warnings.is_empty() && empty.is_empty(),
        "the index file should not exist yet: {warnings:?}"
    );
    store
        .save_atomic(&prior_index)
        .expect("write the first session's index");

    let mut e_durations = Vec::with_capacity(REPETITIONS);
    for _ in 0..REPETITIONS {
        let started = Instant::now();
        let (_store, loaded, warnings) = IndexStore::open(index_path.clone());
        let delta = Scanner::new(roots.clone(), &loaded).run_to_completion(&StdFs);
        let elapsed = started.elapsed();
        assert!(
            warnings.is_empty(),
            "arm E: the index written moments ago must reload cleanly, got {warnings:?}"
        );
        assert_eq!(
            loaded.len(),
            corpus.entries.len(),
            "arm E: the reloaded index must hold the whole corpus, or the scan below is not \
             incremental for the reason the requirement means"
        );
        assert!(
            delta.upserts.is_empty(),
            "arm E: an unchanged corpus must upsert nothing after a reload, got {} upserts",
            delta.upserts.len()
        );
        e_durations.push(elapsed);
    }
    for (i, d) in e_durations.iter().enumerate() {
        println!(
            "E{i}  second start-up: load index + rescan : {:>9.1} ms",
            ms(*d)
        );
    }

    // Arm D: incremental scan, 1% of files modified -- the correctness guard. Not repeated (its
    // point is correctness, not a percentile), and not gated on a timing SLA of its own, though
    // its measured time is printed for context.
    let modified_paths = modify_files(&prior_index, MODIFIED_FRACTION_COUNT);
    let (d_elapsed, d_delta) = incremental_scan(roots.clone(), &prior_index);
    println!(
        "D   incremental scan, 1% modified         : {:>9.1} ms",
        ms(d_elapsed)
    );

    assert_eq!(
        d_delta.upserts.len(),
        modified_paths.len(),
        "arm D: exactly the {} modified files should be upserted, got {}",
        modified_paths.len(),
        d_delta.upserts.len()
    );
    for entry in &d_delta.upserts {
        let prior_hash = prior_index
            .get(&entry.path)
            .and_then(|e| e.hash)
            .expect("modified file should have had a prior hash");
        let new_hash: ContentHash = entry
            .hash
            .expect("a modified, non-oversized fixture file should still hash");
        assert_ne!(
            prior_hash,
            new_hash,
            "arm D: {} was modified but its hash did not change",
            entry.path.display()
        );
    }

    // ---- NFR-PERF-060: max over >= 5 repetitions of arm C, absolute, <= 2.0 s. ----
    let c_max = *c_durations.iter().max().unwrap();
    println!(
        "\nNFR-PERF-060: max(C) = {:.1} ms against a 2000 ms ceiling -- {}",
        ms(c_max),
        if c_max <= Duration::from_secs(2) {
            "PASS"
        } else {
            "FAIL"
        }
    );
    // trace: NFR-PERF-060
    assert!(
        c_max <= Duration::from_secs(2),
        "NFR-PERF-060: arm C's max ({c_max:?}) exceeds the 2 s ceiling"
    );

    // ---- FR-LIB-030: max(E) < min(B), no overlap between the distributions. ----
    //
    // Asserted, not printed. Until M14 this comparison ended in a `CONCLUSIVE`/`INCONCLUSIVE`
    // line for a human to read, which under D-23.1's second question is not a `Verify: B` being
    // executed: a benchmark verifies a requirement by asserting its numeric condition in-process.
    // The old wording is kept, in the panic message, for the run where it fails.
    let b_min = *b_durations.iter().min().unwrap();
    let e_max = *e_durations.iter().max().unwrap();
    println!(
        "\nFR-LIB-030: max(E) = {:.1} ms against min(B) = {:.1} ms -- {}",
        ms(e_max),
        ms(b_min),
        if e_max < b_min { "PASS" } else { "FAIL" }
    );
    // trace: FR-LIB-030
    assert!(
        e_max < b_min,
        "FR-LIB-030: INCONCLUSIVE -- a second start-up (arm E, max {:.1} ms: IndexStore::open plus \
         an incremental rescan) is not measurably faster than a first (arm B, min {:.1} ms), the \
         two ranges overlapping. D-2.4: one reading on a machine that was not verified quiet is \
         not evidence of a regression -- re-run pinned (NAMIR_PIN_CORE) >= 5 times before \
         believing this, and note that a certified figure is a reference-machine \
         (02-architecture.md section 2) figure only",
        ms(e_max),
        ms(b_min)
    );
    println!(
        "  (arm C, max {:.1} ms, is the same incremental scan without the index reload -- the \
         difference between C and E is what persistence itself costs.)",
        ms(c_max)
    );

    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_file(&index_path);
}

/// Where arm E's persisted index lives: beside the working copy rather than inside it, so the
/// index file is not itself a file the scanner walks (it would be ignored by extension, but a
/// benchmark should not depend on that).
fn sibling_index_path(work_dir: &Path) -> PathBuf {
    let mut path = work_dir.as_os_str().to_owned();
    path.push("-index.json");
    PathBuf::from(path)
}
