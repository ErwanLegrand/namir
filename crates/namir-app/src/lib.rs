//! D-5.1's role for this crate: "Standalone application: audio device I/O, window, settings." May
//! depend on everything (`xtask layering`'s `LAYERING_TABLE` already carries this row). This is the
//! product shell FRS §5.11 (FR-IO) is written against, and the second of `namir-ui`'s two
//! [`namir_ui::UiHost`] implementors — `namir-clap` (built in parallel, a different crate, no file
//! overlap with this one) is the first.
//!
//! # What this crate wires together
//!
//! - **`namir_engine::build_default_engine`** — the real fixed six-stage chain, split into an
//!   audio-thread half ([`namir_engine::AudioEngine`]) and a worker-thread half
//!   ([`namir_engine::WorkerEndpoint`]).
//! - **`cpal`**, behind [`audio_io::AudioBackend`] (D-13.1: "a Namir-owned trait so the engine and
//!   UI never see cpal types") — device enumeration, stream construction, and the input-callback ->
//!   output-callback bridge ([`bridge`]) that turns two independently-scheduled OS callbacks into
//!   one full-duplex signal path for [`namir_engine::AudioEngine::process`].
//! - **`namir_platform`** — [`namir_platform::DenormalGuard`] acquired once per audio callback
//!   ([`stream`], D-7.4) and [`namir_platform::elevate_current_thread_priority`] acquired once, on
//!   the callback thread's first invocation (D-13.2), plus [`namir_platform::config_dir`] for where
//!   [`settings::AppSettings`] and the library index live on disk.
//! - **`namir_worker`** — [`namir_worker::library::LibraryService`] for FR-LIB-020's off-thread
//!   scanning and [`namir_worker::ResourceCache`] for D-8.2's process-wide model/IR sharing.
//!   **Not** `namir_worker::Instance` — see [`engine_live`]'s module doc comment for why, and this
//!   crate's own final report for the API gap that forced the substitution.
//! - **`namir_ui`** — [`host::AppUiHost`] implements [`namir_ui::UiHost`]; [`namir_ui::open_blocking`]
//!   opens the window.
//!
//! # Module map
//!
//! - [`error_codes`] — this crate's own `ErrorCode` catalogue (D-16.1), for the FR-IO failure modes
//!   no existing crate's catalogue names (device open failure, xrun, stream loss).
//! - [`audio_io`] — D-13.1's trait plus the real `cpal` implementation.
//! - [`device_state`] — FR-IO-010/040/080's pure selection logic: which device, sample rate and
//!   buffer size to use, given what the system reports and what was remembered.
//! - [`settings`] — FR-IO-080's persistence.
//! - [`xrun`] — FR-IO-060's dropout counter.
//! - [`latency`] — FR-IO-050's round-trip figure.
//! - [`bridge`] — the input->output ring buffer and its own xrun detection.
//! - [`engine_live`] — the live command-submission and load/unload/recall orchestration this
//!   crate drives instead of `namir_worker::Instance` (see that module's doc comment).
//! - [`library_service`] — thin bootstrap over `namir_worker::library::LibraryService`.
//! - [`host`] — [`host::AppUiHost`], the [`namir_ui::UiHost`] bridge.
//! - [`worker`] — the dedicated background thread every blocking operation (load, scan, save) runs
//!   on, so the UI thread and the audio thread are never blocked by one.
//! - [`stream`] — builds and owns the real `cpal` streams.
//! - [`app`] — top-level wiring: `main`'s actual body, factored out so it is unit-testable without
//!   a real window.

pub mod app;
pub mod audio_io;
pub mod bridge;
pub mod device_state;
pub mod engine_live;
pub mod error_codes;
pub mod host;
pub mod latency;
pub mod library_service;
pub mod settings;
pub mod stream;
pub mod worker;
pub mod xrun;
