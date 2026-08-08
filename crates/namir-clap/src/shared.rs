//! [`NamirShared`]: the CLAP `[thread-safe]` half of a plugin instance (`clack_plugin::plugin::
//! PluginShared`) — the one place data genuinely needs to be reachable from the audio thread, the
//! main thread, the GUI thread, and this crate's own worker-pool jobs all at once.
//!
//! # Why the data lives behind an `Arc<SharedInner>`, not directly in `NamirShared<'a>`
//!
//! [`namir_worker::pool::ThreadPool::spawn`] requires `impl FnOnce() + Send + 'static` — a
//! background job cannot capture anything tied to the plugin's own `'a` lifetime, only `'static`
//! data. `NamirShared<'a>` itself is only ever reachable through a `&'a` reference (clack hands it
//! out that way to the main thread and the audio processor), so it cannot be the thing a spawned
//! closure captures. [`SharedInner`] carries every field that has no reason to know about `'a` —
//! everything except the [`clack_plugin::host::HostSharedHandle`] a caller needs to notify the
//! host of a latency change (`namir-clap`'s only genuine `'a`-tied need) — behind an `Arc`, so a
//! worker job captures a cheap `Arc::clone` and is `'static` by construction. [`ClapUiHost`]
//! (`crate::ui_host`) holds only an `Arc<SharedInner>` clone for the same reason: the GUI's own
//! window/thread lifetime is unrelated to `'a` too.
//!
//! # What this holds, and why each piece is here rather than somewhere per-thread
//!
//! - [`crate::param_mirror::ParamMirror`] — see that module's doc comment.
//! - `cache: Arc<ResourceCache>` — **`ResourceCache::shared()`**, not `ResourceCache::new()`. This
//!   is FR-CLAP-090's entire mechanism: every `namir-clap` instance in one host process calls
//!   `new_shared` once, and every one of those calls resolves the same process-global `Arc` (a
//!   `OnceLock` behind `ResourceCache::shared()`), so two instances loading the same file converge
//!   on one set of weights rather than each parsing and holding its own copy.
//! - `instance: Mutex<Option<Instance>>` — **not present until the first `activate()`**, and
//!   replaced (not mutated) by every subsequent one, because a `namir_worker::Instance` owns one
//!   engine's `WorkerEndpoint`, and CLAP rebuilds the whole engine on every activation (mid-session
//!   sample-rate/block-size changes go through a deactivate-then-reactivate cycle — FR-CLAP-080).
//!   A `Mutex` here is sound specifically because **the audio thread never touches this field** —
//!   only worker-pool jobs and the GUI/main thread reach into it (NFR-RT-010's "no lock the audio
//!   thread can contend on" is about the audio thread's *own* path, which is
//!   `AudioEngine::process`/`apply_param_direct`, neither of which is behind this lock).
//! - `nam_ref`/`ir_ref: Mutex<Option<FileRef>>` — the "what the user asked to have loaded" half of
//!   a [`namir_state::State`], kept independently of whatever the worker has actually finished
//!   loading (which can lag behind by however long a file read/parse takes) so that a save
//!   started immediately after a load request still records the right reference.
//! - `doc: Mutex<Document>` — the last state document this instance loaded from (or
//!   `Document::empty()` if none yet), so a save can go through `State::write_onto` and preserve
//!   whatever a host-specific or future-version section this build doesn't understand (D-11.2).
//! - `library: Mutex<Option<LibraryService>>` — opened via
//!   [`namir_worker::library::LibraryService::open_default`], `None` only under the same
//!   conditions that itself degrades to `None` for (no per-user config directory on this system).
//!   **An earlier version of this crate opened with an empty root list instead**, on the theory
//!   that "no UI to configure one yet" meant "leave it empty until there is one." That reasoning
//!   was wrong in a way that isn't merely inert: `namir_library::scan`'s own rule is that a
//!   *complete* walk concludes every path it didn't see is removed, and a walk over zero roots
//!   completes trivially — so clicking "Rescan library" inside the plugin didn't just fail to
//!   find new files, it wiped every entry `namir-app` had already indexed, since both products
//!   read and write the identical `library-index.json` under the same config directory.
//!   `open_default` is now the one function both product shells call, specifically so this
//!   default can't drift between them a second time. No `namir-ui::UiIntent` yet lets a user add
//!   a *second* root — that gap is real and still open — but a single, correct default needed no
//!   new UI to fix, only for this crate to stop assuming "unconfigured" was a harmless state to
//!   leave a destructive scan operation pointed at.
//! - `latency_samples`/`latency_dirty` — see `crate::audio`'s module doc comment for the full
//!   FR-CLAP-040 story; these are the audio-thread-writable, main-thread-readable channel between
//!   the two halves of it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use clack_plugin::plugin::PluginShared;
use namir_core::ErrorCode;
use namir_engine::TelemetryReader;
use namir_library::ScanProgress;
use namir_state::{Document, FileRef, State};
use namir_ui::UiNotice;
use namir_worker::library::{LibraryService, ScanHandle};
use namir_worker::pool::ThreadPool;
use namir_worker::{Instance, ResourceCache};

use crate::param_mirror::ParamMirror;

/// Everything this instance needs that has no reason to be tied to the plugin's `'a` lifetime.
/// See this module's doc comment.
pub(crate) struct SharedInner {
    pub(crate) params: ParamMirror,
    pub(crate) cache: Arc<ResourceCache>,
    pub(crate) pool: ThreadPool,
    pub(crate) instance: Mutex<Option<Instance>>,
    nam_ref: Mutex<Option<FileRef>>,
    ir_ref: Mutex<Option<FileRef>>,
    doc: Mutex<Document>,
    notices: Mutex<Vec<UiNotice>>,
    next_notice_id: AtomicU64,
    unsaved_changes: AtomicBool,
    /// Set in `activate()`, cleared in `deactivate()` — see `crate::latency_ext`'s use of it.
    pub(crate) active: AtomicBool,
    /// The chain's own last-measured `latency_samples()`, published by the audio thread every
    /// block (see `crate::audio`).
    pub(crate) latency_samples: AtomicU32,
    /// Set by the audio thread when `latency_samples` changed since it was last reported to the
    /// host; cleared once `on_main_thread` has acted on it. See `crate::audio`'s module doc
    /// comment for the full FR-CLAP-040 sequencing.
    pub(crate) latency_dirty: AtomicBool,
    library: Mutex<Option<LibraryService>>,
    scan_progress: Mutex<Option<ScanProgress>>,
    scan_handle: Mutex<Option<ScanHandle>>,
    /// A clone of the live engine's `namir_engine::TelemetryReader`, set fresh by every
    /// `activate()` and cleared by `deactivate()` — see `crate::ui_host`'s module doc comment for
    /// why the GUI keeps its own clone rather than reading through here directly (each clone
    /// tracks an independent cursor, so the GUI's drain cadence never affects anyone else's).
    telemetry: Mutex<Option<TelemetryReader>>,
}

impl SharedInner {
    pub(crate) fn new() -> Self {
        let library = LibraryService::open_default().map(|(service, warnings)| {
            for w in warnings {
                log_worker_warning(&w);
            }
            service
        });

        Self {
            params: ParamMirror::new(),
            cache: ResourceCache::shared(),
            pool: ThreadPool::new(),
            instance: Mutex::new(None),
            nam_ref: Mutex::new(None),
            ir_ref: Mutex::new(None),
            doc: Mutex::new(Document::empty()),
            notices: Mutex::new(Vec::new()),
            next_notice_id: AtomicU64::new(0),
            unsaved_changes: AtomicBool::new(false),
            active: AtomicBool::new(false),
            latency_samples: AtomicU32::new(0),
            latency_dirty: AtomicBool::new(false),
            library: Mutex::new(library),
            scan_progress: Mutex::new(None),
            scan_handle: Mutex::new(None),
            telemetry: Mutex::new(None),
        }
    }

    pub(crate) fn telemetry_reader(&self) -> Option<TelemetryReader> {
        lock(&self.telemetry).clone()
    }

    pub(crate) fn set_telemetry_reader(&self, reader: Option<TelemetryReader>) {
        *lock(&self.telemetry) = reader;
    }

    pub(crate) fn nam_ref(&self) -> Option<FileRef> {
        lock(&self.nam_ref).clone()
    }

    pub(crate) fn ir_ref(&self) -> Option<FileRef> {
        lock(&self.ir_ref).clone()
    }

    pub(crate) fn set_nam_ref(&self, r: Option<FileRef>) {
        *lock(&self.nam_ref) = r;
    }

    pub(crate) fn set_ir_ref(&self, r: Option<FileRef>) {
        *lock(&self.ir_ref) = r;
    }

    /// The `namir_state::State` this instance currently stands for — the payload every save, and
    /// every replay onto a freshly (re)activated engine, is built from.
    pub(crate) fn snapshot_state(&self) -> State {
        State {
            params: self.params.snapshot(),
            nam: self.nam_ref(),
            ir: self.ir_ref(),
        }
    }

    /// Applies a freshly loaded/restored state's non-resource half to the mirror, and records the
    /// resource references — **does not** itself touch a live engine; the caller (host `state`
    /// load, or `activate()`'s replay) is responsible for that via `Instance::recall`, which is
    /// not RT-safe and belongs on a worker thread.
    pub(crate) fn adopt_state(&self, state: &State) {
        self.params.load(&state.params);
        self.set_nam_ref(state.nam.clone());
        self.set_ir_ref(state.ir.clone());
    }

    pub(crate) fn last_document(&self) -> Document {
        lock(&self.doc).clone()
    }

    pub(crate) fn set_last_document(&self, doc: Document) {
        *lock(&self.doc) = doc;
    }

    pub(crate) fn mark_dirty(&self) {
        self.unsaved_changes.store(true, Ordering::Relaxed);
    }

    pub(crate) fn mark_clean(&self) {
        self.unsaved_changes.store(false, Ordering::Relaxed);
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.unsaved_changes.load(Ordering::Relaxed)
    }

    pub(crate) fn push_notice(&self, code: ErrorCode, detail: impl Into<String>) {
        let id = self.next_notice_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.notices).push(UiNotice {
            id,
            code,
            detail: detail.into(),
        });
    }

    pub(crate) fn dismiss_notice(&self, id: u64) {
        lock(&self.notices).retain(|n| n.id != id);
    }

    pub(crate) fn notices(&self) -> Vec<UiNotice> {
        lock(&self.notices).clone()
    }

    pub(crate) fn library_snapshot(&self) -> namir_ui::LibrarySnapshot {
        let index = lock(&self.library)
            .as_ref()
            .map(|s| s.snapshot())
            .unwrap_or_else(|| Arc::new(namir_library::Index::empty()));
        let scan = *lock(&self.scan_progress);
        namir_ui::LibrarySnapshot { index, scan }
    }

    pub(crate) fn start_library_scan(self: &Arc<Self>) {
        let library_guard = lock(&self.library);
        let Some(service) = library_guard.as_ref() else {
            drop(library_guard);
            self.push_notice(
                crate::error_codes::LIBRARY_UNAVAILABLE,
                "no per-user configuration directory is available on this system",
            );
            return;
        };
        let this_progress = Arc::clone(self);
        let this_complete = Arc::clone(self);
        let handle = service.start_scan(
            &self.pool,
            move |progress| {
                *lock(&this_progress.scan_progress) = Some(progress);
            },
            move |outcome| {
                *lock(&this_complete.scan_progress) = None;
                for w in &outcome.warnings {
                    log_worker_warning(w);
                }
                if let Some(e) = &outcome.save_error {
                    log_worker_warning(e);
                }
            },
        );
        drop(library_guard);
        *lock(&self.scan_handle) = handle;
    }

    pub(crate) fn cancel_library_scan(&self) {
        if let Some(handle) = lock(&self.scan_handle).take() {
            handle.cancel();
        }
    }

    fn lock_instance(&self) -> MutexGuard<'_, Option<Instance>> {
        lock(&self.instance)
    }

    pub(crate) fn with_instance<R>(&self, f: impl FnOnce(&mut Instance) -> R) -> Option<R> {
        self.lock_instance().as_mut().map(f)
    }

    /// Installs a freshly built `Instance`, replacing whatever was there — every `activate()`
    /// calls this exactly once (see `crate::audio`'s module doc comment for why the whole engine
    /// is rebuilt on every activation rather than mutated in place).
    pub(crate) fn install_instance(&self, instance: Instance) {
        *self.lock_instance() = Some(instance);
    }

    /// Drops the current `Instance` — called by `deactivate()`. The `WorkerEndpoint` (and thus
    /// the SPSC rings) it owned goes with it; any worker-pool job still holding an `Arc` to this
    /// `SharedInner` and racing a submit against it degrades exactly as D-8.1 describes for an
    /// abandoned ring (`SubmitError::Abandoned`), not a panic.
    pub(crate) fn clear_instance(&self) {
        *self.lock_instance() = None;
    }
}

/// A worker/library warning this crate has nowhere richer to send yet — surfaced as an FR-UI-070
/// notice via the caller, not here (this only exists for warnings a callback receives with no
/// `Arc<SharedInner>` in scope, i.e. genuinely unreachable paths); kept as a named function so a
/// future real diagnostic sink (FR-ERR-050) has one call site to redirect.
fn log_worker_warning(_w: &namir_worker::WorkerError) {
    // Deliberately a no-op today: `namir-platform`'s log sink (D-13.2's `log_file_path`) computes
    // a path but this crate has not wired an actual writer to it (out of this round's scope, see
    // this crate's top-level doc comment). Named and called from every site that produces a
    // worker warning so wiring a real sink later is a one-function change, not a grep-and-patch.
}

/// This instance's CLAP `[thread-safe]` half (`clack_plugin::plugin::PluginShared`). See this
/// module's doc comment for why the real data lives in `Arc<SharedInner>` rather than here.
///
/// Must be `pub`, not `pub(crate)`: `clack_plugin::plugin::Plugin::Shared` is an associated type
/// of a trait implemented on the crate's `pub struct NamirClapPlugin` (`lib.rs`), and Rust
/// requires an associated type reachable from a public trait impl to be at least as visible as
/// that impl — nothing outside this crate can actually construct or name a useful value of this
/// type, since every field and every constructor stays `pub(crate)`.
///
/// No `HostSharedHandle` field: every site that needs one already has its own (`crate::audio`'s
/// `NamirAudioProcessor::host`, `crate::main_thread`'s `NamirMainThread::host`), obtained
/// straight from `activate`/`new_main_thread`'s own parameters — so `'a` is carried only via
/// `PhantomData` here, not by storing a redundant third handle nothing reads.
pub struct NamirShared<'a> {
    pub(crate) inner: Arc<SharedInner>,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> PluginShared<'a> for NamirShared<'a> {}

impl<'a> NamirShared<'a> {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(SharedInner::new()),
            _lifetime: std::marker::PhantomData,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // P8, matching `namir-worker::cache::lock`'s identical recovery: a panic elsewhere in this
    // instance must not permanently disable its own state.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_params::stages::trim;

    /// **FR-CLAP-090's mechanism, proven at the point this crate actually calls it**: two
    /// independently constructed `SharedInner`s (standing in for two plugin instances in one host
    /// process) resolve to the *same* `Arc<ResourceCache>` because both go through
    /// `ResourceCache::shared()`, not `ResourceCache::new()`.
    // trace: FR-CLAP-090
    #[test]
    fn two_shared_inners_resolve_to_the_same_process_global_cache() {
        let a = SharedInner::new();
        let b = SharedInner::new();
        assert!(
            Arc::ptr_eq(&a.cache, &b.cache),
            "two instances must share one process-global ResourceCache (FR-CLAP-090)"
        );
    }

    #[test]
    fn snapshot_state_round_trips_through_adopt_state() {
        let inner = SharedInner::new();
        inner.params.set_by_key(trim::GAIN_DB.key, 5.0);
        inner.set_nam_ref(Some(FileRef {
            hash: namir_core::ContentHash::of(b"x"),
            library_relative: None,
            absolute: None,
            display_name: "x.nam".to_string(),
            embedded: None,
        }));
        let state = inner.snapshot_state();

        let other = SharedInner::new();
        other.adopt_state(&state);
        assert_eq!(other.params.snapshot().get(trim::GAIN_DB.key), Some(5.0));
        assert_eq!(
            other.nam_ref().map(|r| r.display_name),
            Some("x.nam".to_string())
        );
    }

    #[test]
    fn dirty_flag_starts_clean_and_tracks_mark_calls() {
        let inner = SharedInner::new();
        assert!(!inner.is_dirty());
        inner.mark_dirty();
        assert!(inner.is_dirty());
        inner.mark_clean();
        assert!(!inner.is_dirty());
    }

    #[test]
    fn notices_can_be_pushed_and_dismissed() {
        let inner = SharedInner::new();
        inner.push_notice(crate::error_codes::LIBRARY_UNAVAILABLE, "detail");
        assert_eq!(inner.notices().len(), 1);
        let id = inner.notices()[0].id;
        inner.dismiss_notice(id);
        assert!(inner.notices().is_empty());
    }
}
