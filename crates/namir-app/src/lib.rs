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
//!   scanning, [`namir_worker::ResourceCache`] for D-8.2's process-wide model/IR sharing, and
//!   [`namir_worker::Instance`] itself (via [`instance::SharedInstance`]) for load/unload/recall
//!   and, since this milestone's `Instance::try_submit_param`, ordinary parameter changes too — see
//!   [`instance`]'s module doc comment for why this crate used to build a substitute instead, and
//!   `docs/02-architecture.md`'s D-7.2 "added M6" consequence note for the closed API gap.
//! - **`namir_ui`** — [`host::AppUiHost`] implements [`namir_ui::UiHost`]; [`namir_ui::open_blocking`]
//!   opens the window.
//!
//! # Module map
//!
//! - [`error_codes`] — this crate's own `ErrorCode` catalogue (D-16.1), for the FR-IO failure modes
//!   no existing crate's catalogue names (device open failure, xrun, stream loss).
//! - [`diagnostics`] — the only module in this crate outside [`host`] that names `namir-platform`'s
//!   logger, so that [`app`] and [`audio_io`] can go on FR-ERR-030's audio-thread list. The same
//!   `audio.rs` -> `shared.rs` split `namir-clap` already uses; see its own doc comment.
//! - [`audio_io`] — D-13.1's trait plus the real `cpal` implementation.
//! - [`device_state`] — FR-IO-010/040/080's pure selection logic: which device, sample rate and
//!   buffer size to use, given what the system reports and what was remembered.
//! - [`settings`] — FR-IO-080's persistence.
//! - [`xrun`] — FR-IO-060's dropout counter.
//! - [`latency`] — FR-IO-050's round-trip figure.
//! - [`presets`] — FR-STATE-030's named-preset locations, naming rule and listing. **Its
//!   `preset_dir_under` belongs in `namir-platform`** beside `config_dir`, shared with
//!   `namir-clap`'s identical `crates/namir-clap/src/presets.rs`; see that module's own doc
//!   comment for why it is duplicated today and what hoisting it costs.
//! - [`bridge`] — the input->output ring buffer and its own xrun detection.
//! - [`instance`] — [`instance::SharedInstance`], the `Mutex`-guarded `namir_worker::Instance`
//!   shared between [`host`] and [`worker`] (see that module's doc comment).
//! - The default library location is `namir_worker::library::LibraryService::open_at`/
//!   `open_default` directly — this crate no longer has its own bootstrap module for it. It used
//!   to (`library_service.rs`); that logic moved to `namir-worker` in the same M6 session that
//!   found `namir-clap` had independently duplicated (and gotten wrong) the exact same
//!   computation, so both product shells now call one function instead of two that could drift
//!   apart. See `namir_worker::library::LibraryService::open_default`'s own doc comment for the
//!   full story.
//! - [`host`] — [`host::AppUiHost`], the [`namir_ui::UiHost`] bridge.
//! - [`worker`] — the dedicated background thread every blocking operation (load, scan, save) runs
//!   on, so the UI thread and the audio thread are never blocked by one.
//! - [`stream`] — builds and owns the real `cpal` streams.
//! - [`startup_probe`] — NFR-PERF-030's measurement seam: one environment variable that makes
//!   [`app::run`] mark the instant it becomes audible and then exit instead of opening the window,
//!   so `benches/startup_to_audible.rs` can time a real launch. Inert in every ordinary launch.
//! - [`app`] — top-level wiring: `main`'s actual body, factored out so it is unit-testable without
//!   a real window.

pub mod app;
pub mod audio_io;
pub mod bridge;
pub mod device_state;
pub mod diagnostics;
pub mod error_codes;
pub mod host;
pub mod instance;
pub mod latency;
pub mod presets;
#[cfg(test)]
mod rt_harness;
pub mod settings;
pub mod startup_probe;
pub mod stream;
pub mod worker;
pub mod xrun;
