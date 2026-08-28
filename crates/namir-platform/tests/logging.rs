//! FR-ERR-010 (`Verify: I`): "Namir shall write a log to a per-user location, with a configurable
//! verbosity, rotated so that it cannot grow without bound."
//!
//! This is `namir-platform`'s first `tests/` directory; D-16.5 nominates this exact file for the
//! requirement and enumerates the six clauses it has to span, which are the first six `clause_*`
//! functions below. They are called by one covering `#[test]` rather than being six `#[test]`s of
//! their own, because D-23.1 makes a tag an assertion about *one* annotated artifact and what it
//! verifies of the *whole* requirement — six independent tests would leave no single artifact to
//! carry that assertion, and splitting a tag six ways is not something the annotation grammar
//! expresses. Each clause is named, so a panic still says which one failed.
//!
//! **The tag was `trace-partial:` until M14, and clause seven is what promoted it.** The first six
//! clauses span the requirement's verbosity and boundedness clauses; its *per-user location*
//! clause was asserted by nothing, because nothing here called `logging::init`, the only code that
//! binds the sink to `crate::log_file_path`. `clause_7_the_per_user_location_through_the_real_init`
//! does exactly that, in a child process with the per-user environment variables redirected — see
//! its own section comment for why a `OnceLock` global resolved from the real environment cannot
//! be driven in-process without `unsafe`.
//!
//! Apart from clause seven, the logger is driven against a caller-supplied temporary path
//! throughout, never the process-global one: the same "pure logic, wired to the real world only at
//! the edge" split `paths.rs`'s `config_dir_from` uses. Nothing in *this* process touches the real
//! `NAMIR_LOG` either — `std::env::set_var` is `unsafe` in this edition and this crate denies
//! `unsafe`, which is precisely why D-16.5 requires the level parser to be a pure function over
//! `Option<&OsStr>` (clause six), and why clause seven reaches the real variable through
//! `Command::env` on a child rather than through this process's own environment.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use namir_core::{ErrorCode, Severity};
use namir_platform::logging::{
    self, EnvLevel, LOG_MAX_BYTES, LevelChoice, LogLevel, Logger, parse_env_level, resolve_level,
};

// ---------------------------------------------------------------------------------------------
// Fixtures: four catalogue entries, one per severity, plus a scratch directory that cleans up
// after itself. Deliberately no `tempfile` dependency -- `std::env::temp_dir()` plus a pid and a
// counter is what every other temp-dir-using test in this workspace does (`namir-library`'s
// `fs.rs`, `namir-app`'s `settings.rs`), and D-16.5 rules out adding a crate for this module.
// ---------------------------------------------------------------------------------------------

const INFO: ErrorCode = ErrorCode::new("platform.test.info", Severity::Info, "", "n/a");
const WARNING: ErrorCode = ErrorCode::new("platform.test.warning", Severity::Warning, "", "n/a");
const ERROR: ErrorCode = ErrorCode::new("platform.test.error", Severity::Error, "", "n/a");
const FAULT: ErrorCode = ErrorCode::new("platform.test.fault", Severity::Fault, "", "n/a");

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "namir-fr-err-010-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch directory");
        Scratch { dir }
    }

    /// The sink path, one directory deeper than the scratch root so the writer's own
    /// `create_dir_all` is exercised the way `log_file_path`'s `logs/` subdirectory will exercise
    /// it in production.
    fn sink(&self) -> PathBuf {
        self.dir.join("logs").join("namir.log")
    }

    fn logs_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(text) => text.lines().map(str::to_owned).collect(),
        Err(_) => Vec::new(),
    }
}

/// The `<code-id>` field of a record line -- the fifth of the six space-separated fields.
fn code_of(line: &str) -> &str {
    line.split(' ').nth(4).unwrap_or("")
}

/// Every code id present in `path`, in file order.
fn codes_in(path: &Path) -> Vec<String> {
    read_lines(path)
        .iter()
        .map(|line| code_of(line).to_owned())
        .collect()
}

/// Asserts a line has the six fields D-16.5 specifies, with fields one to five free of spaces, and
/// returns its `<detail>` -- everything after the fifth space, which is what makes the format need
/// no quoting scheme and no parser.
fn assert_well_formed(line: &str) -> &str {
    let mut fields = line.splitn(6, ' ');
    let timestamp = fields.next().unwrap_or_default();
    let level = fields.next().unwrap_or_default();
    let pid = fields.next().unwrap_or_default();
    let thread = fields.next().unwrap_or_default();
    let code = fields.next().unwrap_or_default();
    let detail = fields.next().unwrap_or_default();

    assert_eq!(timestamp.len(), 24, "timestamp width in {line:?}");
    assert!(timestamp.ends_with('Z'), "timestamp not UTC in {line:?}");
    assert!(
        matches!(level, "INFO" | "WARN" | "ERROR" | "FAULT"),
        "unknown LEVEL {level:?} in {line:?}"
    );
    assert!(
        pid.chars().all(|c| c.is_ascii_digit()) && !pid.is_empty(),
        "pid field {pid:?} in {line:?}"
    );
    assert!(
        !thread.is_empty() && !thread.contains(char::is_whitespace),
        "thread field {thread:?} in {line:?}"
    );
    assert!(code.contains('.'), "code id {code:?} in {line:?}");
    detail
}

// ---------------------------------------------------------------------------------------------
// Clause 1 -- level filtering per severity.
// ---------------------------------------------------------------------------------------------

fn clause_1_level_filtering_per_severity() {
    // `off` never opens or creates the file at all -- not an empty file, no file.
    let scratch = Scratch::new("level-off");
    let logger = Logger::new(Some(scratch.sink()), LevelChoice::at(LogLevel::Off));
    submit_one_of_each(&logger);
    assert!(
        !scratch.sink().exists(),
        "`off` must never open or create the log file"
    );
    assert!(
        !scratch.logs_dir().exists(),
        "`off` must not even create the log directory"
    );

    // `error` admits Error and Fault only -- so not even the Info session-started record, which is
    // why the file exists at all only once the first Error record arrives.
    let scratch = Scratch::new("level-error");
    let logger = Logger::new(Some(scratch.sink()), LevelChoice::at(LogLevel::Error));
    submit_one_of_each(&logger);
    assert_eq!(
        codes_in(&scratch.sink()),
        vec![ERROR.id.to_owned(), FAULT.id.to_owned()],
        "`error` admits Severity::Error and Severity::Fault, and nothing else"
    );

    // `info` (the default) adds Warning and Info -- including the lifecycle record -- but still
    // drops anything submitted through record_verbose.
    let scratch = Scratch::new("level-info");
    let logger = Logger::new(Some(scratch.sink()), LevelChoice::at(LogLevel::Info));
    submit_one_of_each(&logger);
    assert_eq!(
        codes_in(&scratch.sink()),
        vec![
            "platform.log.session_started".to_owned(),
            INFO.id.to_owned(),
            WARNING.id.to_owned(),
            ERROR.id.to_owned(),
            FAULT.id.to_owned(),
        ],
        "`info` admits all four severities and no verbose record"
    );

    // `verbose` adds the second entry point, and only it.
    let scratch = Scratch::new("level-verbose");
    let logger = Logger::new(Some(scratch.sink()), LevelChoice::at(LogLevel::Verbose));
    submit_one_of_each(&logger);
    assert_eq!(
        codes_in(&scratch.sink()),
        vec![
            "platform.log.session_started".to_owned(),
            INFO.id.to_owned(),
            WARNING.id.to_owned(),
            ERROR.id.to_owned(),
            FAULT.id.to_owned(),
            "platform.test.verbose".to_owned(),
        ],
        "`verbose` admits everything `info` does plus record_verbose"
    );

    // The level is configurable at runtime, not only at construction (FR-ERR-010's "configurable
    // verbosity"): a logger opened at `error` and later raised starts admitting Info records.
    let scratch = Scratch::new("level-raised");
    let logger = Logger::new(Some(scratch.sink()), LevelChoice::at(LogLevel::Error));
    logger.record(INFO, "dropped");
    assert_eq!(logger.level(), LogLevel::Error);
    logger.set_level(LogLevel::Info);
    logger.record(INFO, "admitted");
    assert_eq!(
        codes_in(&scratch.sink()),
        vec![INFO.id.to_owned()],
        "raising the level must admit records the previous level dropped, and only those"
    );
}

fn submit_one_of_each(logger: &Logger) {
    logger.record(INFO, "info-record");
    logger.record(WARNING, "warning-record");
    logger.record(ERROR, "error-record");
    logger.record(FAULT, "fault-record");
    logger.record_verbose(
        ErrorCode::new("platform.test.verbose", Severity::Info, "", "n/a"),
        "verbose-record",
    );
}

// ---------------------------------------------------------------------------------------------
// Clause 2 -- rotation at the byte cap, with content preserved.
// ---------------------------------------------------------------------------------------------

/// A detail long enough that the cap is crossed in tens of records rather than tens of thousands:
/// 64 KiB per record means ~64 records per generation, so one rotation costs ~4 MiB of I/O
/// instead of ~4 MiB spread over 40 000 syscalls.
const BULK_DETAIL_BYTES: usize = 64 * 1024;

fn bulk_detail(marker: usize) -> String {
    let prefix = format!("marker={marker};");
    let mut detail = String::with_capacity(BULK_DETAIL_BYTES);
    detail.push_str(&prefix);
    detail.extend(std::iter::repeat_n('x', BULK_DETAIL_BYTES - prefix.len()));
    detail
}

fn clause_2_rotation_at_the_byte_cap_preserves_content() {
    let scratch = Scratch::new("rotation");
    let logger = Logger::new(Some(scratch.sink()), LevelChoice::at(LogLevel::Info));

    // 80 records x ~64 KiB = ~5.1 MiB: past one 4 MiB cap and nowhere near two, so exactly one
    // rotation must have happened.
    let records = 80;
    for marker in 0..records {
        logger.record(INFO, &bulk_detail(marker));
    }

    let gen1 = with_suffix(&scratch.sink(), ".1");
    let gen2 = with_suffix(&scratch.sink(), ".2");
    assert!(gen1.exists(), "the cap was crossed but nothing rotated");
    assert!(
        !gen2.exists(),
        "one cap crossing must produce one rotation, not two"
    );

    // Neither generation exceeds the cap...
    for path in [scratch.sink(), gen1.clone()] {
        let len = fs::metadata(&path).expect("metadata").len();
        assert!(
            len <= LOG_MAX_BYTES,
            "{} is {len} bytes, past the {LOG_MAX_BYTES}-byte cap",
            path.display()
        );
    }

    // ...and rotation preserved content rather than truncating it: every marker survives exactly
    // once across the two generations, the rotated-out records are the older ones, and the new
    // generation opens with the catalogue-backed rotation notice.
    let older = read_lines(&gen1);
    let newer = read_lines(&scratch.sink());
    assert!(!older.is_empty() && !newer.is_empty());
    assert_eq!(
        code_of(&newer[0]),
        "platform.log.rotated",
        "the new generation must open with the rotation record: {:?}",
        newer[0]
    );
    assert!(
        newer[0].contains("namir.log.1 replaced"),
        "rotation detail must name the generation it replaced: {:?}",
        newer[0]
    );
    assert_eq!(
        code_of(&older[0]),
        "platform.log.session_started",
        "the session's first record must have rotated out with its generation"
    );

    let mut seen = Vec::new();
    for line in older.iter().chain(newer.iter()) {
        let detail = assert_well_formed(line);
        if let Some(marker) = detail.strip_prefix("marker=") {
            let marker: usize = marker
                .split(';')
                .next()
                .expect("marker field")
                .parse()
                .expect("marker is numeric");
            seen.push(marker);
        }
    }
    assert_eq!(
        seen,
        (0..records).collect::<Vec<_>>(),
        "every record must survive rotation, once, in order"
    );
    assert_eq!(
        older.len() + newer.len(),
        records + 2,
        "records plus the session-started and rotated lifecycle records"
    );
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

// ---------------------------------------------------------------------------------------------
// Clause 3 -- the retention bound holds across many rotations; never a fourth file.
// ---------------------------------------------------------------------------------------------

fn clause_3_retention_bound_never_produces_a_fourth_file() {
    let scratch = Scratch::new("retention");
    let logger = Logger::new(Some(scratch.sink()), LevelChoice::at(LogLevel::Info));

    // ~21 MiB written through a 12 MiB ceiling: five cap crossings, so the two-generation bound is
    // exercised repeatedly rather than once.
    let records = 320;
    for marker in 0..records {
        logger.record(INFO, &bulk_detail(marker));
    }

    let mut names: Vec<String> = fs::read_dir(scratch.logs_dir())
        .expect("read log directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "namir.log".to_owned(),
            "namir.log.1".to_owned(),
            "namir.log.2".to_owned()
        ],
        "exactly three generations may ever exist -- a fourth file is unbounded growth"
    );

    // FR-ERR-010's actual property: bounded. Each generation is capped, so the directory is too.
    let total: u64 = names
        .iter()
        .map(|name| {
            fs::metadata(scratch.logs_dir().join(name))
                .expect("metadata")
                .len()
        })
        .sum();
    assert!(
        total <= 3 * LOG_MAX_BYTES,
        "the log directory holds {total} bytes, past the {} byte ceiling",
        3 * LOG_MAX_BYTES
    );
    // ...and the newest generation really is the newest: the most recent marker is in namir.log.
    let newest = read_lines(&scratch.sink());
    assert!(
        newest
            .iter()
            .any(|line| line.contains(&format!("marker={};", records - 1))),
        "the last record written must be in the live generation"
    );
}

// ---------------------------------------------------------------------------------------------
// Clause 4 -- one intact line per record under eight concurrent threads.
// ---------------------------------------------------------------------------------------------

fn clause_4_eight_concurrent_threads_never_tear_a_line() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 250;

    let scratch = Scratch::new("concurrent");
    let logger = Arc::new(Logger::new(
        Some(scratch.sink()),
        LevelChoice::at(LogLevel::Info),
    ));

    let mut handles = Vec::new();
    for thread in 0..THREADS {
        let logger = Arc::clone(&logger);
        handles.push(
            std::thread::Builder::new()
                // A name with whitespace in it, so the sanitisation that keeps field four
                // space-free is exercised by the concurrent path rather than only in a unit test.
                .name(format!("namir worker {thread}"))
                .spawn(move || {
                    for n in 0..PER_THREAD {
                        // Embedded CR/LF in every record: if the escaping were wrong, or if two
                        // threads could interleave inside one line, the line count below would
                        // not match the record count.
                        logger.record(
                            if n % 2 == 0 { INFO } else { ERROR },
                            &format!("t={thread}\r\nn={n}"),
                        );
                    }
                })
                .expect("spawn"),
        );
    }
    for handle in handles {
        handle.join().expect("join");
    }

    let lines = read_lines(&scratch.sink());
    assert_eq!(
        lines.len(),
        THREADS * PER_THREAD + 1,
        "one line per record, plus the session-started record"
    );

    let mut details = BTreeSet::new();
    for line in &lines {
        let detail = assert_well_formed(line);
        if line.contains("platform.log.session_started") {
            continue;
        }
        assert!(
            detail.starts_with("t=") && detail.contains("\\r\\nn="),
            "detail was torn or unescaped: {line:?}"
        );
        assert!(details.insert(detail.to_owned()), "duplicate: {line:?}");
    }
    assert_eq!(
        details.len(),
        THREADS * PER_THREAD,
        "every record from every thread must appear exactly once"
    );
    for thread in 0..THREADS {
        for n in 0..PER_THREAD {
            assert!(
                details.contains(&format!("t={thread}\\r\\nn={n}")),
                "missing record {n} from thread {thread}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Clause 5 -- the `None`-path no-op sink.
// ---------------------------------------------------------------------------------------------

fn clause_5_none_path_is_a_silent_no_op() {
    // `log_file_path()` returns None on Android and iOS, where `config_dir_from` has no branch.
    // The contract is: level check still runs, every record is dropped, no file is created, and no
    // error is raised.
    let scratch = Scratch::new("no-op");
    let logger = Logger::new(None, LevelChoice::at(LogLevel::Verbose));
    assert_eq!(logger.sink_path(), None);
    assert_eq!(logger.level(), LogLevel::Verbose);
    submit_one_of_each(&logger);
    logger.record(FAULT, "still nothing");
    logger.set_level(LogLevel::Off);
    logger.record(FAULT, "still nothing");

    let entries: Vec<_> = fs::read_dir(&scratch.dir)
        .expect("read scratch directory")
        .collect();
    assert!(
        entries.is_empty(),
        "a no-op sink must create nothing on disk"
    );

    // The same for a logger whose level resolution rejected a NAMIR_LOG value: the bad-level
    // record has nowhere to go and must not panic on the way there.
    let logger = Logger::new(
        None,
        resolve_level(Some(OsStr::new("shout")), Some(LogLevel::Verbose)),
    );
    logger.record(ERROR, "dropped too");
}

// ---------------------------------------------------------------------------------------------
// Clause 6 -- the NAMIR_LOG value parser.
// ---------------------------------------------------------------------------------------------

fn clause_6_the_namir_log_parser() {
    // A pure function over Option<&OsStr>: no process environment is read or mutated here, which
    // is the whole reason D-16.5 specifies it this way (std::env::set_var is `unsafe`).
    assert_eq!(parse_env_level(None), EnvLevel::Unset);
    assert_eq!(parse_env_level(Some(OsStr::new(""))), EnvLevel::Unset);
    assert_eq!(parse_env_level(Some(OsStr::new("   "))), EnvLevel::Unset);

    for (raw, expected) in [
        ("off", LogLevel::Off),
        ("error", LogLevel::Error),
        ("errors", LogLevel::Error), // D-16.4's own prose spelling
        ("info", LogLevel::Info),
        ("verbose", LogLevel::Verbose),
        ("VERBOSE", LogLevel::Verbose),
        ("  Info  ", LogLevel::Info),
    ] {
        assert_eq!(
            parse_env_level(Some(OsStr::new(raw))),
            EnvLevel::Set(expected),
            "NAMIR_LOG={raw:?}"
        );
    }

    for raw in ["loud", "trace", "debug", "2", "warn"] {
        assert_eq!(
            parse_env_level(Some(OsStr::new(raw))),
            EnvLevel::Unparseable(raw.to_owned()),
            "NAMIR_LOG={raw:?} must be rejected, not silently reinterpreted"
        );
    }

    // Resolution order: NAMIR_LOG if set and valid, else the persisted setting, else info.
    assert_eq!(
        resolve_level(Some(OsStr::new("verbose")), Some(LogLevel::Error)),
        LevelChoice::at(LogLevel::Verbose),
        "a valid NAMIR_LOG outranks the persisted setting"
    );
    assert_eq!(
        resolve_level(None, Some(LogLevel::Error)),
        LevelChoice::at(LogLevel::Error),
        "an unset NAMIR_LOG defers to the persisted setting"
    );
    assert_eq!(
        resolve_level(None, None),
        LevelChoice::at(LogLevel::Info),
        "with neither, the default is info"
    );

    // An unparseable value falls back the same way -- never silently off -- and is reported.
    assert_eq!(
        resolve_level(Some(OsStr::new("loud")), None),
        LevelChoice {
            level: LogLevel::Info,
            rejected: Some("loud".to_owned()),
        }
    );
    assert_eq!(
        resolve_level(Some(OsStr::new("loud")), Some(LogLevel::Error)).level,
        LogLevel::Error,
        "a rejected NAMIR_LOG must not disable the persisted setting either"
    );

    // ...and the rejection reaches the log as one WARN record naming the value.
    let scratch = Scratch::new("bad-level");
    let logger = Logger::new(
        Some(scratch.sink()),
        resolve_level(Some(OsStr::new("loud")), None),
    );
    let lines = read_lines(&scratch.sink());
    assert_eq!(
        codes_in(&scratch.sink()),
        vec![
            "platform.log.session_started".to_owned(),
            "platform.log.bad_level".to_owned()
        ],
        "exactly one bad-level record, after the session record"
    );
    let bad = &lines[1];
    assert_eq!(
        bad.split(' ').nth(1),
        Some("WARN"),
        "the bad-level record must be WARN: {bad:?}"
    );
    assert!(
        bad.contains("NAMIR_LOG=loud"),
        "the bad-level record must name the rejected value: {bad:?}"
    );
    assert_eq!(logger.level(), LogLevel::Info);

    // ...and it reaches the log at `error` too, where the level check would otherwise drop it
    // (issue #79). LOG_BAD_LEVEL is a WARN and `error` admits only ERROR and FAULT, so a user who
    // had chosen a quiet log *and* mistyped NAMIR_LOG used to be told nothing at all -- the one
    // combination where the record matters most. The session record, an INFO, is correctly absent
    // here: it is the bad-level record specifically that bypasses admission.
    let scratch = Scratch::new("bad-level-at-error");
    let logger = Logger::new(
        Some(scratch.sink()),
        resolve_level(Some(OsStr::new("shout")), Some(LogLevel::Error)),
    );
    assert_eq!(logger.level(), LogLevel::Error);
    assert_eq!(
        codes_in(&scratch.sink()),
        vec!["platform.log.bad_level".to_owned()],
        "a mistyped NAMIR_LOG must be reported even at a level that does not admit WARN"
    );
    assert!(
        read_lines(&scratch.sink())[0].contains("NAMIR_LOG=shout"),
        "the forced record must still name the rejected value"
    );

    // The one level that keeps its silence: `off` promises the file is never opened or created,
    // and a forced record would create a log the user switched off.
    let scratch = Scratch::new("bad-level-at-off");
    let logger = Logger::new(
        Some(scratch.sink()),
        resolve_level(Some(OsStr::new("shout")), Some(LogLevel::Off)),
    );
    assert_eq!(logger.level(), LogLevel::Off);
    assert!(
        !scratch.logs_dir().exists(),
        "`off` must still create nothing on disk, bad-level record or not"
    );

    // The module-level entry points exist and are safe to call before `init` has run -- a record
    // submitted during static initialisation must be a no-op, not a panic and not a logger
    // installed at the wrong level behind the shell's back.
    assert!(logging::logger().is_none());
    logging::record(INFO, "no global logger yet");
    logging::record_verbose(INFO, "no global logger yet");
}

// ---------------------------------------------------------------------------------------------
// Clause seven (added M14): the per-user location, through the real `logging::init`.
//
// This is the clause the tag below used to name as uncovered, and it is the one clause that
// cannot be driven in-process. `logging::init` is a `OnceLock`, so it can be called once per
// process and never again; it resolves its level from the real `NAMIR_LOG` and its sink from the
// real `crate::log_file_path()`, which reads the real per-user environment variables. A test that
// wanted to steer either would have to mutate the process environment, and `std::env::set_var` is
// `unsafe` in this edition — which this crate may not use outside its two designated modules, and
// which would be process-global besides, so it would corrupt the six in-process clauses above
// running beside it.
//
// So the clause is driven where a per-process global belongs: in its own process. The parent
// re-invokes this test binary with `APPDATA`, `HOME` and `XDG_CONFIG_HOME` all redirected into a
// scratch directory — one variable per platform convention `paths.rs` documents, all three set so
// the same child works on all three, and `Command::env` needs no `unsafe` because it configures a
// child's environment rather than mutating this one's. The child then calls the real `init`, and
// asserts that the sink it chose is the real `log_file_path()`, that the path lands under the
// redirected per-user root, and that a record submitted through the module-level `logging::record`
// entry point actually reaches that file at the level the real `NAMIR_LOG` selected. The parent
// independently finds the file by walking the scratch directory, so the location is confirmed from
// outside the code that computed it as well as from inside.
//
// This is the same in-process/child-process split `namir-worker`'s `cross_process_restore.rs`
// already uses, for the same reason: one `#[test]` function that is both parent and child,
// distinguished by an environment variable the parent sets.
// ---------------------------------------------------------------------------------------------

/// Set by the parent on the child it spawns; its presence is how the one test function below
/// tells which role it is playing.
const CHILD_ENV_VAR: &str = "NAMIR_FR_ERR_010_CHILD";

/// The level the parent puts in the child's `NAMIR_LOG`. Deliberately not the `info` default, so
/// the child's assertion that `init` resolved to it is evidence that the real environment was
/// read rather than evidence that a default happened to match.
const CHILD_LEVEL: &str = "error";

/// Every file named `namir.log` anywhere under `root`. Used by the parent to confirm the child's
/// sink from outside — the parent cannot compute the expected path itself without a
/// `#[cfg(target_os)]` ladder duplicating `paths.rs`'s own.
fn find_log_files(root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_log_files(&path, found);
        } else if path.file_name() == Some(OsStr::new("namir.log")) {
            found.push(path);
        }
    }
}

/// The child half: the only code in this suite that calls the process-global [`logging::init`].
fn run_as_per_user_location_child() {
    let root = PathBuf::from(
        std::env::var_os(CHILD_ENV_VAR).expect("the parent sets this on the child it spawns"),
    );

    // The path `init` is about to bind its sink to, computed by the same function `init` calls.
    let expected = namir_platform::log_file_path()
        .expect("the parent redirected this platform's per-user variable, so this resolves");
    assert!(
        expected.starts_with(&root),
        "log_file_path() resolved to {expected:?}, which is not under the redirected per-user \
         root {root:?} -- the sink is not per-user"
    );
    assert_eq!(
        expected.file_name(),
        Some(OsStr::new("namir.log")),
        "{expected:?}"
    );

    assert!(
        logging::logger().is_none(),
        "nothing in this process may have installed the global logger before init"
    );
    let logger = logging::init(None);
    assert!(logging::logger().is_some(), "init must install the global");
    assert_eq!(
        logger.level(),
        LogLevel::Error,
        "init must resolve its level from the real NAMIR_LOG ({CHILD_LEVEL})"
    );

    // Through the module-level entry points, not through the returned handle: those are what the
    // rest of the workspace calls, and they are the path that would silently no-op if `init` had
    // not bound a sink.
    logging::record(ERROR, "per-user-location probe");
    logging::record(INFO, "must be filtered out at error level");

    assert!(
        expected.is_file(),
        "init bound its sink to {expected:?}, but no file is there after a record was submitted"
    );
    let codes = codes_in(&expected);
    assert!(
        codes.contains(&"platform.test.error".to_owned()),
        "the ERROR record must have reached the per-user log: {codes:?}"
    );
    assert!(
        !codes.contains(&"platform.test.info".to_owned()),
        "an INFO record must be filtered out at NAMIR_LOG={CHILD_LEVEL}: {codes:?}"
    );

    println!("child wrote {}", expected.display());
}

/// The parent half. Spawns the child above with a redirected per-user environment, then confirms
/// the log landed under that root independently of the child's own assertions.
fn clause_7_the_per_user_location_through_the_real_init() {
    let scratch = Scratch::new("per-user");
    let root = scratch.dir.clone();

    let exe = std::env::current_exe().expect("a test binary knows its own path");
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "the_diagnostic_log_is_configurable_and_bounded",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV_VAR, &root)
        .env(logging::LEVEL_ENV_VAR, CHILD_LEVEL)
        // One per platform convention `paths.rs` documents: Windows reads APPDATA, macOS reads
        // HOME, other Unix prefers XDG_CONFIG_HOME and falls back to HOME. Setting all three
        // keeps this child correct on every supported platform without a cfg ladder here.
        .env("APPDATA", root.join("appdata"))
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .output()
        .expect("spawn the per-user-location child");

    assert!(
        output.status.success(),
        "the per-user-location child failed ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut found = Vec::new();
    find_log_files(&root, &mut found);
    assert_eq!(
        found.len(),
        1,
        "exactly one namir.log should exist under the redirected per-user root, found {found:?}"
    );
    assert_eq!(
        found[0].parent().and_then(Path::file_name),
        Some(OsStr::new("logs")),
        "the sink must live in the logs/ subdirectory log_file_path names: {:?}",
        found[0]
    );
}

// ---------------------------------------------------------------------------------------------
// The covering test.
// ---------------------------------------------------------------------------------------------

// trace: FR-ERR-010
#[test]
fn the_diagnostic_log_is_configurable_and_bounded() {
    if std::env::var_os(CHILD_ENV_VAR).is_some() {
        run_as_per_user_location_child();
        return;
    }
    clause_1_level_filtering_per_severity();
    clause_2_rotation_at_the_byte_cap_preserves_content();
    clause_3_retention_bound_never_produces_a_fourth_file();
    clause_4_eight_concurrent_threads_never_tear_a_line();
    clause_5_none_path_is_a_silent_no_op();
    clause_6_the_namir_log_parser();
    clause_7_the_per_user_location_through_the_real_init();
}
