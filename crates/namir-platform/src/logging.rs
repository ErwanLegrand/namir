//! FR-ERR-010's diagnostic log: "a log to a per-user location, with a configurable verbosity,
//! rotated so that it cannot grow without bound." D-16.5 settles every parameter this module
//! implements; read it (`docs/02-architecture.md` §16) before changing any of them, because the
//! numbers below are the decision's, not this file's.
//!
//! | Parameter | Value |
//! |---|---|
//! | Maximum file size before rotation | 4 MiB ([`LOG_MAX_BYTES`]) |
//! | Retained generations | 2 — `namir.log`, `namir.log.1`, `namir.log.2`; a 12 MiB ceiling |
//! | Record format | one UTF-8 line: `<timestamp> <LEVEL> <pid> <thread> <code-id> <detail>` |
//! | Verbosity environment variable | `NAMIR_LOG` — `off` / `error` / `info` / `verbose` |
//! | Default level | `info` |
//! | Thread model | synchronous: one process-global writer behind a `Mutex`, no logger thread |
//!
//! **Why a mutex in a logger is acceptable here, and why this module lives in *this* crate.**
//! D-5.1's table gives `namir-engine` `core, params, dsp, nam, ir` and nothing else, and `cargo
//! run -p xtask -- layering` checks that edge on every merge — so no *production* code on the
//! audio thread can so much as *name* this module. The lint is what makes the lock safe; siting
//! the writer in `namir-platform` is therefore load-bearing rather than incidental. Two things the
//! lint does not cover, stated rather than assumed:
//!
//! 1. **The edge check exempts dev-dependencies**, and `crates/namir-engine/Cargo.toml` carries
//!    `namir-platform` as one (for D-7.4's `DenormalGuard` in `benches/denormal_guard.rs` and
//!    `benches/rt_invariance.rs`). So `namir-engine`'s own benches and integration tests —
//!    including the RT harnesses that drive `AudioEngine::process` — *can* name this module, and
//!    the "cannot so much as name it" guarantee holds for the shipped build, not for the test
//!    build. Nothing shipped is affected, and no test target does name it today; the carve-out is
//!    recorded because a guarantee stated without its exception is the kind that gets relied on
//!    where it does not hold.
//! 2. **`namir-app` and `namir-clap` depend on everything and own the audio callbacks**, so those
//!    two crates *could* call in from `cpal`'s callback or from `process()`. `xtask rt-logging`
//!    (M9b, FR-ERR-030's static half) is what now stops them, per-module: it fails the build if
//!    any file on its `AUDIO_THREAD_MODULES` list names `logging`, `Logger`, `LogLevel` or
//!    `record_verbose`. Beyond the names it reads, the rule that no record is emitted from an
//!    audio callback or a per-frame UI path is held by review plus `namir-worker`'s
//!    `assert_no_alloc` stress harness, which fails on the allocation a record's formatting
//!    performs.
//!
//! **What this module deliberately does not have.** No `BufWriter`: a half-flushed buffer loses
//! precisely the records written in the moments a crash makes interesting, so a record is exactly
//! one `write_all` of a complete line. No open handle and no byte counter held between records
//! either — see "Two writers, one file" below, which is the reason. No logger thread:
//! NFR-PORT-030 forbids assuming a process can spawn unlimited threads, and a thread parked
//! inside a `.clap` the host may unload would need a shutdown handshake the synchronous design
//! needs not at all. No `#[cfg(target_os)]`:
//! every platform difference is absorbed by [`crate::log_file_path`], which yields `None` on
//! Android and iOS, and on `None` this module builds a **no-op sink** — the level check still
//! runs, every record is dropped, no file is created, no error is raised. No dependency either:
//! the timestamp is the standard days-from-civil arithmetic over `SystemTime`'s epoch offset, not
//! a date library. And no `unsafe`, so this crate's `unsafe_code = "deny"` is satisfied without a
//! third designated module beside `denormal.rs` and `thread_priority.rs`.
//!
//! **Two writers, one file.** The standalone application and a DAW hosting the plugin both write
//! `namir.log`, and a user running both at once is ordinary rather than exotic. Every plugin
//! instance inside one DAW is covered by the process-global mutex; two processes are not, and no
//! lock here can cover them. Records stay attributable because each carries its pid. What makes
//! the *file* survive the arrangement is that this writer keeps no state about it: an admitted
//! record opens the path, reads the real length from the file it opened, rotates if this line
//! would carry that length past [`LOG_MAX_BYTES`], writes exactly one line, and closes. So the cap
//! is measured against every writer's bytes rather than against one writer's tally of its own, and
//! each record lands in the file that bears the name at the moment it is written.
//!
//! That is not a refinement of an earlier design, it is the repair of a real data-loss path (#145,
//! finding 11). A writer that seeds a counter at open and increments it by its own writes
//! rotates late by exactly the other writer's contribution — measured at roughly double the 4 MiB
//! cap — and a writer that keeps its handle goes on appending into whatever generation another
//! process's rotation renamed its file into, until its own counter fires and it renames the live
//! file over the one it was itself writing. Two writers alternating 400 records through the
//! previous design left **85** of them in the three files, with holes in the middle of the
//! history; the same run through this one leaves 148, contiguous, newest-first, inside the ceiling.
//!
//! What is *not* claimed. Rotation re-measures the file immediately before its renames and gives
//! up when another writer has already rotated, but the window between that measurement and the
//! rename is not closed: two writers crossing the cap within microseconds of each other can still
//! rotate twice in a row, which retires one generation early — history is shortened, nothing is
//! destroyed. Closing that would take an advisory lock on a file whose name never changes, which
//! means a fourth file in a directory D-16.5 specifies as three, so it is not taken here. A record
//! whose write is in flight when another process renames the file lands in the renamed
//! generation — one record, still on disk. A failed `fs::rename` is an ordinary outcome, never an
//! `unwrap`: no rotation happens for that record and the size check runs again on the next one.
//! And the 12 MiB ceiling holds for any number of writers up to the one oversized record each may
//! have in flight, since each file is measured as it is written rather than remembered.

use std::ffi::OsStr;
use std::fmt;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use namir_core::{ErrorCode, Severity};

use crate::error_codes::{LOG_BAD_LEVEL, LOG_ROTATED, LOG_SESSION_STARTED};

/// D-16.5's rotation threshold: 4 MiB. At the ~100-byte line this format produces that is roughly
/// forty thousand records — several full sessions at a level whose records are per user action.
/// With the two retained generations the whole `logs` directory stays under 12 MiB, which is what
/// keeps it an ordinary issue or email attachment.
pub const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The environment variable D-16.5 assigns to the log's verbosity. Deliberately **not**
/// `RUST_LOG`: that name belongs to `env_logger`'s per-module filter grammar, and D-16.4 installs
/// no logging facade, so borrowing the name would promise a syntax this writer does not implement.
pub const LEVEL_ENV_VAR: &str = "NAMIR_LOG";

/// How much the diagnostic log admits. Ordered ascending, and stored in the [`Logger`] as an
/// `AtomicU8` so a below-threshold record costs one relaxed load and returns without touching the
/// sink's lock.
///
/// This ladder and `namir_core::Severity`'s ladder are the same ladder in three places out of
/// four; [`LogLevel::Verbose`] is the exception, and is expressed by a second entry point
/// ([`Logger::record_verbose`]) rather than by a fifth severity. D-16.5 records why: adding a
/// "trace" severity would change a type every crate's catalogue and the UI's severity mapping
/// share, for a distinction only the log makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum LogLevel {
    /// Nothing is admitted; the file is never opened or created.
    Off = 0,
    /// [`Severity::Error`] and [`Severity::Fault`].
    Error = 1,
    /// The above, plus [`Severity::Warning`] and [`Severity::Info`]. The default.
    Info = 2,
    /// The above, plus records submitted through [`Logger::record_verbose`], which is a no-op at
    /// every other level.
    Verbose = 3,
}

impl LogLevel {
    /// The lowercase spelling this level parses from and prints as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LogLevel::Off => "off",
            LogLevel::Error => "error",
            LogLevel::Info => "info",
            LogLevel::Verbose => "verbose",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What [`parse_env_level`] made of a raw `NAMIR_LOG` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvLevel {
    /// The variable is unset, empty, or whitespace only — indistinguishable outcomes, and treated
    /// as "the user did not choose" rather than as an error, because `NAMIR_LOG=` is a common way
    /// to clear a variable in a shell.
    Unset,
    /// The variable named a level.
    Set(LogLevel),
    /// The variable was set to something unrecognised, rendered lossily for the
    /// [`LOG_BAD_LEVEL`] record that names it back to the user.
    Unparseable(String),
}

/// A resolved verbosity plus, if `NAMIR_LOG` had to be rejected to get there, the value that was
/// rejected. See [`resolve_level`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelChoice {
    /// The level the logger will run at.
    pub level: LogLevel,
    /// The unrecognised `NAMIR_LOG` value, if there was one. A logger built from this choice
    /// writes exactly one [`LOG_BAD_LEVEL`] record naming it.
    pub rejected: Option<String>,
}

impl LevelChoice {
    /// A choice of `level` with nothing rejected — what a caller that is not consulting the
    /// environment at all (a test, or a shell setting the level from its own settings only) wants.
    #[must_use]
    pub const fn at(level: LogLevel) -> Self {
        LevelChoice {
            level,
            rejected: None,
        }
    }
}

/// Parses one raw `NAMIR_LOG` value.
///
/// A pure function over `Option<&OsStr>` for a hard reason rather than a stylistic one:
/// `std::env::set_var` is `unsafe` as of this workspace's edition and this crate denies `unsafe`
/// outside its two carve-out modules, so a test that mutates the real environment cannot be
/// written here at all. The same "pure logic, wired to the real world only at the edge" split
/// `paths.rs`'s `config_dir_from` uses.
///
/// Recognised spellings are the four level names, ASCII-case-insensitively and with surrounding
/// whitespace trimmed, plus `errors` as a synonym for `error` — D-16.4's own prose spells it that
/// way, so an instruction copied out of the decision still works.
#[must_use]
pub fn parse_env_level(raw: Option<&OsStr>) -> EnvLevel {
    let Some(raw) = raw else {
        return EnvLevel::Unset;
    };
    let Some(text) = raw.to_str() else {
        return EnvLevel::Unparseable(raw.to_string_lossy().into_owned());
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return EnvLevel::Unset;
    }
    let level = if trimmed.eq_ignore_ascii_case("off") {
        LogLevel::Off
    } else if trimmed.eq_ignore_ascii_case("error") || trimmed.eq_ignore_ascii_case("errors") {
        LogLevel::Error
    } else if trimmed.eq_ignore_ascii_case("info") {
        LogLevel::Info
    } else if trimmed.eq_ignore_ascii_case("verbose") {
        LogLevel::Verbose
    } else {
        return EnvLevel::Unparseable(trimmed.to_owned());
    };
    EnvLevel::Set(level)
}

/// D-16.5's level resolution, whole: `NAMIR_LOG` if set and valid, else the persisted setting
/// where one exists, else `info`.
///
/// `persisted` is a parameter rather than a lookup because `namir-app`'s `AppSettings` is where
/// that setting lives and D-5.1 forbids this crate from depending on `namir-app` (or on anything
/// but `namir-core`). `namir-clap` has no persisted setting to pass — roadmap §15 item 8 — so it
/// passes `None` and `NAMIR_LOG` is the plugin's only verbosity control in 1.0.
///
/// An unparseable value falls back exactly as an unset one does and is returned in
/// [`LevelChoice::rejected`] — never silently off, the same degrade-rather-than-assume posture
/// `paths.rs` applies per NFR-PORT-030.
#[must_use]
pub fn resolve_level(raw: Option<&OsStr>, persisted: Option<LogLevel>) -> LevelChoice {
    match parse_env_level(raw) {
        EnvLevel::Set(level) => LevelChoice::at(level),
        EnvLevel::Unset => LevelChoice::at(persisted.unwrap_or(LogLevel::Info)),
        EnvLevel::Unparseable(value) => LevelChoice {
            level: persisted.unwrap_or(LogLevel::Info),
            rejected: Some(value),
        },
    }
}

/// Whether a record of `severity` is admitted at the level held in `bits`. Operates on the raw
/// atomic value so the hot path never reconstructs a [`LogLevel`].
fn admits(bits: u8, severity: Severity) -> bool {
    if bits == LogLevel::Off as u8 {
        false
    } else if bits == LogLevel::Error as u8 {
        severity >= Severity::Error
    } else {
        true
    }
}

/// The `LEVEL` field: the record's `namir_core::Severity`, so the printed level and the catalogue
/// severity are one fact rather than two that can disagree.
fn level_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "INFO",
        Severity::Warning => "WARN",
        Severity::Error => "ERROR",
        Severity::Fault => "FAULT",
    }
}

/// D-16.5's writer: an `AtomicU8` level and a `Mutex`-guarded sink.
///
/// Construct one with [`Logger::new`] against a caller-supplied path (what
/// `crates/namir-platform/tests/logging.rs` does), or take the process-global one with
/// [`init`]/[`logger`].
pub struct Logger {
    level: AtomicU8,
    sink: Mutex<SinkState>,
}

impl Logger {
    /// Builds a logger writing to `path`, or a no-op sink if `path` is `None`.
    ///
    /// `None` is the Android/iOS case — [`crate::log_file_path`] has no convention for them — and
    /// is not an error: the level check still runs, every record is dropped, and no file is
    /// created. This is exactly the caller behaviour `paths.rs`'s own doc comment specifies.
    ///
    /// No file is opened here even when `path` is `Some`. The sink opens lazily on the first
    /// admitted record, which is what makes `off` mean "the file is never opened or created"
    /// without a second code path, and what keeps a level later raised through [`set_level`]
    /// working.
    ///
    /// Writes the session's [`LOG_SESSION_STARTED`] record, and — if `choice` carries a rejected
    /// `NAMIR_LOG` value — one [`LOG_BAD_LEVEL`] record naming it.
    #[must_use]
    pub fn new(path: Option<PathBuf>, choice: LevelChoice) -> Self {
        let logger = Logger {
            level: AtomicU8::new(choice.level as u8),
            sink: Mutex::new(SinkState {
                target: path.map(FileTarget::new),
                scratch: String::new(),
            }),
        };
        let detail = match logger.sink_path() {
            Some(path) => format!("level={}; path={}", choice.level, path.display()),
            None => format!(
                "level={}; no per-user log location on this platform",
                choice.level
            ),
        };
        logger.record(LOG_SESSION_STARTED, &detail);
        if let Some(value) = choice.rejected {
            // Emitted through `write_locked`, deliberately bypassing `record`'s severity
            // admission. [`LOG_BAD_LEVEL`] is a `Severity::Warning` (that const's doc comment
            // argues why it may not be anything else), and `admits` does not pass a `Warning` at
            // `LogLevel::Error` -- so a user running at `error` who mistyped `NAMIR_LOG` was told
            // nothing at all by the record whose entire purpose is to tell them. The one check
            // kept is `Off`, because that level's contract is that the file is never opened or
            // created; a record forced past *that* would create a log the user switched off.
            let bits = choice.level as u8;
            if bits != LogLevel::Off as u8 {
                logger.write_locked(
                    bits,
                    LOG_BAD_LEVEL,
                    &format!(
                        "{LEVEL_ENV_VAR}={value} is not one of off/error/info/verbose; using {}",
                        choice.level
                    ),
                );
            }
        }
        logger
    }

    /// The level this logger currently admits at.
    #[must_use]
    pub fn level(&self) -> LogLevel {
        match self.level.load(Ordering::Relaxed) {
            0 => LogLevel::Off,
            1 => LogLevel::Error,
            3 => LogLevel::Verbose,
            _ => LogLevel::Info,
        }
    }

    /// Changes the level a running logger admits at — for a settings change taking effect without
    /// a restart. Lowering to [`LogLevel::Off`] does not close the file; it stops admitting.
    pub fn set_level(&self, level: LogLevel) {
        self.level.store(level as u8, Ordering::Relaxed);
    }

    /// Submits one catalogue-backed record. `detail` is the already-materialised sixth field —
    /// `message_template` is never written, because D-16.2 puts template formatting on the UI side
    /// and the log carries the id plus the detail.
    ///
    /// Below-threshold records cost one relaxed atomic load and return without touching the lock.
    /// Above it, the lock is held across formatting into the sink's reused scratch `String` and
    /// exactly one `write_all` of the complete line, then released — so records are totally
    /// ordered within a process and no line is ever torn.
    pub fn record(&self, code: ErrorCode, detail: &str) {
        let bits = self.level.load(Ordering::Relaxed);
        if !admits(bits, code.severity) {
            return;
        }
        self.write_locked(bits, code, detail);
    }

    /// Submits one record that only [`LogLevel::Verbose`] admits, whatever its severity. A no-op
    /// at every other level, including `info`.
    pub fn record_verbose(&self, code: ErrorCode, detail: &str) {
        let bits = self.level.load(Ordering::Relaxed);
        if bits != LogLevel::Verbose as u8 {
            return;
        }
        self.write_locked(bits, code, detail);
    }

    /// The path this logger writes to, or `None` for a no-op sink.
    #[must_use]
    pub fn sink_path(&self) -> Option<PathBuf> {
        let state = self.sink.lock().ok()?;
        state.target.as_ref().map(|t| t.path.clone())
    }

    fn write_locked(&self, bits: u8, code: ErrorCode, detail: &str) {
        // A poisoned mutex means some other thread panicked mid-record. Dropping this record is
        // the right answer: the alternative is propagating a panic out of a diagnostic call site,
        // which would turn a logging fault into an application fault.
        let Ok(mut state) = self.sink.lock() else {
            return;
        };
        let SinkState { target, scratch } = &mut *state;
        let Some(target) = target.as_ref() else {
            return;
        };
        format_record(
            scratch,
            SystemTime::now(),
            std::process::id(),
            &current_thread_label(),
            code,
            detail,
        );
        target.write_line(bits, scratch);
    }
}

/// Everything the mutex guards: the sink itself and the scratch buffer records are formatted into,
/// reused rather than reallocated per record.
struct SinkState {
    target: Option<FileTarget>,
    scratch: String,
}

/// The file sink and its two retained generations.
///
/// Deliberately holds **no** open handle and **no** byte counter between records: see
/// [`FileTarget::write_line`]. The whole struct is three paths, so a second writer — in this
/// process or another — cannot make anything it caches wrong, because it caches nothing.
struct FileTarget {
    path: PathBuf,
    generation_1: PathBuf,
    generation_2: PathBuf,
}

impl FileTarget {
    fn new(path: PathBuf) -> Self {
        FileTarget {
            generation_1: generation(&path, 1),
            generation_2: generation(&path, 2),
            path,
        }
    }

    /// Opens whatever file currently bears the sink's name, for appending, creating it — and its
    /// directory, if the first attempt failed for want of one — when it is not there.
    ///
    /// The directory is created only on the failure path rather than checked every record: the
    /// common case is one `open`, and `create_dir_all` on an existing directory is a syscall this
    /// module would otherwise make per record for nothing.
    ///
    /// A failure to create the directory or to open the file is not raised anywhere — there is, by
    /// construction, nowhere to report it *to* — it simply drops this record and is retried on the
    /// next one, so a disk that filled up and was then freed starts logging again on its own.
    fn open(&self) -> Option<File> {
        let opened = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path);
        if let Ok(file) = opened {
            return Some(file);
        }
        let parent = self.path.parent()?;
        if parent.as_os_str().is_empty() || fs::create_dir_all(parent).is_err() {
            return None;
        }
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
            .ok()
    }

    /// Writes one complete line, rotating first if it would carry the file past
    /// [`LOG_MAX_BYTES`].
    ///
    /// **The file is opened, measured, written and closed inside this one call, every record.**
    /// That is what makes the size check and the rotation correct when a second process is writing
    /// the same path (#145, finding 11), and it costs one `open` plus one `metadata` per
    /// admitted record — paid by a writer whose records are per user action, and never by the
    /// audio thread, which cannot reach this module at all.
    ///
    /// Two properties follow, and neither is available to a writer that keeps a handle. The length
    /// is read from the file itself, so it counts *every* writer's bytes rather than this one's
    /// tally of its own — a per-writer counter fires late by exactly the other writer's
    /// contribution, which measured at roughly double the cap. And each record goes to the file
    /// that bears the name *now*, so a writer whose file was renamed away by another process's
    /// rotation cannot go on appending into a retired generation — which is what previously let
    /// the next rotation rename a live session over the file the other writer was in.
    ///
    /// The `len > 0` guard means a single record larger than the whole cap is written rather than
    /// rotated around forever; that is bounded by the record, not unbounded growth, and rotating
    /// an empty file would not help.
    fn write_line(&self, bits: u8, line: &str) {
        let Some(mut file) = self.open() else {
            return;
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or_default();
        let mut rotated = None;
        if len > 0 && len + line.len() as u64 > LOG_MAX_BYTES {
            // Closed before the renames: a rename over a file the *same* process holds open is
            // the case most likely to fail.
            drop(file);
            rotated = self.rotate(len);
            let Some(reopened) = self.open() else {
                return;
            };
            file = reopened;
        }
        if let Some(previous) = rotated
            && admits(bits, LOG_ROTATED.severity)
        {
            let mut notice = String::new();
            format_record(
                &mut notice,
                SystemTime::now(),
                std::process::id(),
                &current_thread_label(),
                LOG_ROTATED,
                &format!(
                    "{} reached {previous} bytes; {} replaced",
                    file_label(&self.path),
                    file_label(&self.generation_1),
                ),
            );
            append(&mut file, &notice);
        }
        append(&mut file, line);
    }

    /// Rolls `namir.log.1` to `namir.log.2` and `namir.log` to `namir.log.1`, returning the
    /// rotated file's length when this call is the one that rotated.
    ///
    /// `observed` is the length that decided the rotation. The file at the name is re-measured
    /// here, immediately before the renames, and the rotation is abandoned when it has since
    /// *shrunk*: that means another writer rotated in between, and rotating again would retire a
    /// generation that is one record old and discard the one behind it for nothing. The window
    /// this leaves — between this measurement and the rename below — is not closed, and cannot be
    /// without a lock file this module deliberately does not create; see the module doc.
    ///
    /// A failure to rename is an ordinary outcome, never an `unwrap`: it means no rotation this
    /// record, the line is written to the file as it stands, and the size check runs again on the
    /// next record. Never a fourth generation: the first rename overwrites `namir.log.2`, so at
    /// most three files ever exist.
    fn rotate(&self, observed: u64) -> Option<u64> {
        let current = fs::metadata(&self.path)
            .map(|m| m.len())
            .unwrap_or_default();
        if current < observed {
            return None;
        }
        // May legitimately fail because .1 does not exist yet; the outcome that matters is the
        // second rename.
        let _ = fs::rename(&self.generation_1, &self.generation_2);
        if fs::rename(&self.path, &self.generation_1).is_err() {
            return None;
        }
        Some(current)
    }
}

/// One `write_all` of one complete line. A failed write drops the record: the handle is closed at
/// the end of the record either way, so there is no latched failure state to clear.
fn append(file: &mut File, line: &str) {
    let _ = file.write_all(line.as_bytes());
}

/// `namir.log` + `.1` / `.2`. Appends to the whole file name rather than replacing an extension,
/// so `namir.log.1` is produced and not `namir.1`.
fn generation(path: &Path, index: u8) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// The bare file name for a rotation notice's detail, falling back to the whole path if there is
/// somehow no final component.
fn file_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The `<thread>` field for the calling thread: its name with whitespace collapsed to `_`, or its
/// [`std::thread::ThreadId`] when it has no name. Never empty and never containing a space, so
/// field five stays unambiguous.
fn current_thread_label() -> String {
    let current = std::thread::current();
    match current.name() {
        Some(name) => {
            let sanitised: String = name
                .chars()
                .map(|c| if c.is_whitespace() { '_' } else { c })
                .collect();
            if sanitised.is_empty() {
                "_".to_owned()
            } else {
                sanitised
            }
        }
        None => format!("{:?}", current.id()),
    }
}

/// Formats one whole record — including its trailing newline — into `out`, replacing whatever was
/// there.
///
/// Pure over its inputs (the clock, the pid and the thread label are supplied, not read) so the
/// format can be asserted exactly without a filesystem or a fixed process. Fields one to five
/// never contain a space, so `detail` is unambiguously everything after the fifth space and the
/// format needs no quoting scheme and no parser — which is why CR and LF inside `detail` become
/// the two-character sequences `\n` and `\r` here: one record is one line unconditionally, so a
/// panic payload with embedded newlines cannot break a `grep`.
fn format_record(
    out: &mut String,
    at: SystemTime,
    pid: u32,
    thread: &str,
    code: ErrorCode,
    detail: &str,
) {
    out.clear();
    push_timestamp(out, at);
    out.push(' ');
    out.push_str(level_word(code.severity));
    let _ = write!(out, " {pid} {thread} {} ", code.id);
    for c in detail.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('\n');
}

/// `2026-08-09T14:03:57.412Z` — UTC with milliseconds.
///
/// UTC only, and labelled `Z`: `std` carries no timezone database, so local time is unavailable
/// without the dependency D-16.4 declined, and a mislabelled local time would be worse than a
/// correctly labelled foreign one. A clock reading before the Unix epoch (which
/// `duration_since` reports as an error) formats as the epoch rather than failing the record.
fn push_timestamp(out: &mut String, at: SystemTime) {
    let since = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since.as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let seconds_of_day = secs % 86_400;
    let _ = write!(
        out,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
        since.subsec_millis(),
    );
}

/// Howard Hinnant's `civil_from_days`: the standard days-from-civil arithmetic, taking days since
/// 1970-01-01 to a proleptic-Gregorian `(year, month, day)`. Verbatim integer arithmetic, which is
/// the whole reason D-16.5 could rule out taking on a date crate for one timestamp.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let shifted_month = (5 * day_of_year + 2) / 153; // [0, 11], March-based
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

static GLOBAL: OnceLock<Logger> = OnceLock::new();

/// Installs the process-global logger, or returns the one already installed.
///
/// The level is resolved by [`resolve_level`] from the real `NAMIR_LOG` and the `persisted`
/// setting the calling shell owns (`namir-app`'s `AppSettings`; `namir-clap` passes `None`), and
/// the sink is [`crate::log_file_path`] — `None` there yields a no-op sink rather than an error.
///
/// Idempotent, and cheap to call defensively: a second call with a different `persisted` value
/// does **not** re-resolve, so whichever shell initialises first wins. Use
/// [`Logger::set_level`] on the returned logger to change level afterwards.
pub fn init(persisted: Option<LogLevel>) -> &'static Logger {
    GLOBAL.get_or_init(|| {
        let raw = std::env::var_os(LEVEL_ENV_VAR);
        let choice = resolve_level(raw.as_deref(), persisted);
        Logger::new(crate::log_file_path(), choice)
    })
}

/// The process-global logger, if [`init`] has run. `None` before it has — deliberately, so a
/// record submitted during static initialisation cannot silently install a logger at the default
/// level before the shell has had a chance to apply its persisted setting.
#[must_use]
pub fn logger() -> Option<&'static Logger> {
    GLOBAL.get()
}

/// [`Logger::record`] against the process-global logger; a no-op before [`init`].
pub fn record(code: ErrorCode, detail: &str) {
    if let Some(logger) = GLOBAL.get() {
        logger.record(code, detail);
    }
}

/// [`Logger::record_verbose`] against the process-global logger; a no-op before [`init`].
pub fn record_verbose(code: ErrorCode, detail: &str) {
    if let Some(logger) = GLOBAL.get() {
        logger.record_verbose(code, detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const INFO_CODE: ErrorCode = ErrorCode::new("platform.test.info", Severity::Info, "", "n/a");

    fn at(seconds: u64, millis: u32) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::new(seconds, millis * 1_000_000)
    }

    /// D-16.5's own worked example, to the byte: `2026-08-09T14:03:57.412Z` is epoch second
    /// 1 786 284 237.
    #[test]
    fn the_record_format_is_six_space_separated_fields() {
        let mut out = String::new();
        format_record(
            &mut out,
            at(1_786_284_237, 412),
            18244,
            "main",
            INFO_CODE,
            "a=1",
        );
        assert_eq!(
            out,
            "2026-08-09T14:03:57.412Z INFO 18244 main platform.test.info a=1\n"
        );
    }

    #[test]
    fn newlines_in_a_detail_become_two_character_escapes() {
        let mut out = String::new();
        format_record(&mut out, at(0, 0), 1, "t", INFO_CODE, "one\r\ntwo\nthree");
        assert_eq!(
            out.matches('\n').count(),
            1,
            "record must be one line: {out}"
        );
        assert!(
            out.ends_with(" platform.test.info one\\r\\ntwo\\nthree\n"),
            "{out}"
        );
    }

    #[test]
    fn the_epoch_and_a_leap_day_both_render_correctly() {
        let mut out = String::new();
        push_timestamp(&mut out, at(0, 0));
        assert_eq!(out, "1970-01-01T00:00:00.000Z");

        // 2024-02-29T23:59:59.999Z — a leap day in a leap century-cycle year.
        out.clear();
        push_timestamp(&mut out, at(1_709_251_199, 999));
        assert_eq!(out, "2024-02-29T23:59:59.999Z");

        // 2000-03-01T00:00:00.000Z — the day after the leap day of the century leap year.
        out.clear();
        push_timestamp(&mut out, at(951_868_800, 0));
        assert_eq!(out, "2000-03-01T00:00:00.000Z");
    }

    #[test]
    fn a_thread_label_never_contains_whitespace() {
        let label = std::thread::Builder::new()
            .name("namir worker\t0".to_owned())
            .spawn(current_thread_label)
            .expect("spawn")
            .join()
            .expect("join");
        assert_eq!(label, "namir_worker_0");
    }

    #[test]
    fn generations_append_to_the_whole_file_name() {
        let base = PathBuf::from("/var/log/namir.log");
        assert_eq!(generation(&base, 1), PathBuf::from("/var/log/namir.log.1"));
        assert_eq!(generation(&base, 2), PathBuf::from("/var/log/namir.log.2"));
    }

    #[test]
    fn admission_follows_the_level_ladder() {
        for severity in [
            Severity::Info,
            Severity::Warning,
            Severity::Error,
            Severity::Fault,
        ] {
            assert!(!admits(LogLevel::Off as u8, severity));
            assert!(admits(LogLevel::Info as u8, severity));
            assert!(admits(LogLevel::Verbose as u8, severity));
        }
        assert!(!admits(LogLevel::Error as u8, Severity::Info));
        assert!(!admits(LogLevel::Error as u8, Severity::Warning));
        assert!(admits(LogLevel::Error as u8, Severity::Error));
        assert!(admits(LogLevel::Error as u8, Severity::Fault));
    }
}
