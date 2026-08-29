//! The two direct `namir_platform::logging` calls [`crate::app`] used to make, moved here so that
//! module can go on FR-ERR-030's audio-thread list.
//!
//! # Why this module exists
//!
//! `xtask rt-logging` forbids a module that carries audio-thread code from *naming*
//! `namir-platform`'s logger, and applies the ban at **file** granularity — see
//! `xtask/src/rt_logging.rs`'s own module doc for why a line-based scanner cannot do better and why
//! the resulting over-approximation is the honest direction to err in. Three modules in this crate
//! were outside that list while carrying callback code:
//!
//! - [`crate::app`] owns `stream_failure_sink`, the closure `cpal` invokes on the stream's own
//!   error-callback thread (issue #88 is what made that closure allocation-free; nothing made the
//!   file *covered*).
//! - [`crate::audio_io`] wraps every `cpal` callback and classifies a failure inside the error one
//!   (`to_stream_failure`).
//! - `crate::audio_io::convert` converts sample formats inside the two data callbacks.
//!
//! The last two name nothing forbidden and went on the list unchanged. [`crate::app`] could not:
//! it makes two entirely legitimate **main-thread** logger calls — installing the process logger as
//! the first statement of `run`, and recording a settings save that failed *after* the window has
//! closed, where there is no FR-UI-070 notice list left to push onto. Listing the file without
//! moving them would have failed the gate on two calls that break nothing.
//!
//! # The escape hatch, which is the house pattern rather than a workaround
//!
//! `rt_logging.rs`'s module doc names it: `namir-clap`'s `audio.rs` is on the list and its
//! `activate()` — CLAP's `[main-thread]` — reports through `shared.rs`'s `push_notice`, so it is
//! `shared.rs`, not `audio.rs`, that names the logger. This module is that `shared.rs` for the
//! standalone.
//!
//! **It is not a thread-safety mechanism and must not be read as one.** Calling into here from an
//! audio callback would be exactly as illegal as calling the logger directly, and the static check
//! could not see it (`rt_logging.rs`'s residual blind spot 1: the ban is on naming the logger, not
//! on reaching it). What this file buys is that the *name* lives somewhere the callbacks do not, so
//! a future logger call added to [`crate::app`] is a build failure rather than a silent
//! RT-violation. Every function below is main-thread-only, and both of today's callers are on the
//! main thread before the window opens or after it has closed.

use namir_core::ErrorCode;

/// FR-ERR-010: installs the process-global logger, once, before anything can report.
///
/// Called as the first statement of [`crate::app::run`] — see that call site for why the persisted
/// level is `None` and why this happens before the configuration directory is even resolved.
/// Idempotent (`namir_platform::logging::init`'s own contract), so a second call is harmless.
///
/// **Main thread only.** See this module's doc comment.
pub fn install() {
    namir_platform::logging::init(None);
}

/// FR-ERR-010: writes one log record for a condition that has no notice to carry it.
///
/// The ordinary path for a diagnostic in this crate is [`crate::host::AppHost`]'s `push_notice`,
/// which writes the record *and* queues the FR-UI-070 notice from one function so the two cannot
/// drift apart. This exists for the one condition that reaches neither: a settings save that fails
/// at the foot of [`crate::app::run`], with the window already closed and the host already dropped.
///
/// **Main thread only.** See this module's doc comment.
pub fn record(code: ErrorCode, detail: &str) {
    namir_platform::logging::record(code, detail);
}
