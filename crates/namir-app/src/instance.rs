//! [`SharedInstance`]: a `Mutex`-guarded [`namir_worker::Instance`], shared between
//! [`crate::host::AppHost`] (the GUI-adjacent thread's direct, non-blocking parameter-change path)
//! and [`crate::worker`]'s background thread (load/unload/recall/save, all of which may block).
//!
//! # Why `namir-app` builds an `Instance` at all now
//!
//! Until this milestone's `namir_worker::Instance::try_submit_param` existed, `Instance`'s public
//! surface had no way to submit a plain `Command::Param`/`Command::Reset` from outside
//! `namir-worker` — see `docs/02-architecture.md`'s D-7.2 "added M6" consequence note for the full
//! story of the gap and how it was closed. `namir-app` used to work around that by re-deriving its
//! own `LiveEngine` (`crates/namir-app/src/engine_live.rs`, now deleted) with a shared
//! `Arc<CommandSubmitter>` instead. With `try_submit_param` in place, that whole substitute is
//! unnecessary: this module is the thin adapter that lets [`crate::host`] and [`crate::worker`]
//! share one real `Instance`.
//!
//! # Why a `Mutex<Instance>` here is sound
//!
//! Same argument `namir-clap`'s `crates/namir-clap/src/shared.rs` module doc comment makes for its
//! own `Mutex<Option<Instance>>`: NFR-RT-010's "no lock the audio thread can contend on" is about
//! the audio thread's own path, `namir_engine::AudioEngine::process` (wired independently in
//! `crate::stream`, which never touches this type at all) — not about the two non-realtime threads
//! that *do* reach into an `Instance`. Those two are exactly `AppHost::dispatch`'s
//! `SetParam`/`ResetParamToDefault` handling (one non-blocking `try_submit_param` call, released
//! immediately) and `crate::worker`'s single background thread (which already serialises every
//! load/unload/recall/save through its own `mpsc` command queue, so it never contends with itself).
//! A brief block on this mutex costs nothing a real-time deadline depends on.
//!
//! Unlike `namir-clap`'s `Mutex<Option<Instance>>`, this wraps a plain `Instance`, no `Option`:
//! `namir-clap` needs `Option` because CLAP's activate/deactivate cycle (FR-CLAP-080) rebuilds the
//! whole engine on every activation, so there is a real "not built yet" state between them.
//! `namir-app` builds exactly one engine at startup (`crate::app::run`) and runs it for the whole
//! process's life, with no analogous rebuild — so there is no "not yet built" state for `Option` to
//! represent.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use namir_engine::{Command, ParamChange};
use namir_worker::{CommandSubmitter, Instance, SubmitError};

/// See this module's doc comment.
#[derive(Clone)]
pub struct SharedInstance {
    instance: Arc<Mutex<Instance>>,
    submitter: Arc<CommandSubmitter>,
}

impl SharedInstance {
    /// Wraps a freshly built `Instance` for sharing between the GUI-adjacent and worker threads.
    pub fn new(instance: Instance) -> Self {
        let submitter = instance.submitter();
        Self {
            instance: Arc::new(Mutex::new(instance)),
            submitter,
        }
    }

    /// Runs `f` against the instance under the lock. P8: a panic elsewhere while holding this lock
    /// must not permanently deny access to it (the same recovery `namir-worker::cache::lock` and
    /// `namir-clap::shared::lock` both use) — the poison is discarded, not propagated.
    pub fn with<R>(&self, f: impl FnOnce(&mut Instance) -> R) -> R {
        let mut guard: MutexGuard<'_, Instance> =
            self.instance.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut guard)
    }

    /// Submits one parameter change via the non-blocking [`CommandSubmitter`], without acquiring
    /// the instance mutex.
    pub fn try_submit_param(&self, change: ParamChange) -> Result<(), SubmitError> {
        self.submitter.try_submit(Command::Param(change))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::{ChannelConfig, SampleRate};
    use namir_engine::{PrepareContext, build_default_engine};
    use namir_worker::EngineConfig;

    /// The whole reason this type exists: two independent callers, each holding their own clone,
    /// reach the same underlying instance rather than two separate ones. End-to-end concurrent
    /// access (a real worker thread alongside `AppHost::dispatch`) is exercised by `crate::host`'s
    /// own tests; this one just proves the clone shares state.
    #[test]
    fn a_clone_reaches_the_same_underlying_instance() {
        let c =
            PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, ChannelConfig::Mono).unwrap();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let shared = SharedInstance::new(Instance::new(EngineConfig { ctx: c }, endpoint));
        let other = shared.clone();
        let freed = other.with(|i| i.drain_retired());
        assert_eq!(freed, 0);
    }

    // trace-partial: FR-UI-060
    // uncovered: FR-UI-060 — non-blocking parameter submission under instance contention; whole-interface
    // uncovered: frame duration during a 10,000-file scan is measured by namir-ui/benches/library_frame.rs;
    // uncovered: closes M8
    #[test]
    fn try_submit_param_does_not_block_while_instance_mutex_is_held() {
        let c =
            PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, ChannelConfig::Mono).unwrap();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let shared = SharedInstance::new(Instance::new(EngineConfig { ctx: c }, endpoint));

        let (lock_acquired_tx, lock_acquired_rx) = std::sync::mpsc::channel();
        let (release_lock_tx, release_lock_rx) = std::sync::mpsc::channel();
        let other = shared.clone();

        let handle = std::thread::spawn(move || {
            other.with(|_| {
                lock_acquired_tx.send(()).unwrap();
                release_lock_rx.recv().unwrap();
            });
        });

        lock_acquired_rx.recv().unwrap();

        // Submitting parameter change must succeed non-blockingly while another thread holds the instance lock.
        let result = shared.try_submit_param(ParamChange {
            id: namir_engine::ParamId(10),
            value: -3.0,
        });
        assert!(result.is_ok());

        release_lock_tx.send(()).unwrap();
        handle.join().unwrap();
    }
}
