//! The erased-lifetime [`clack_plugin::host::HostSharedHandle`] a worker-pool thread can hold, so
//! a preset recall that happens off the main thread can still ask the host for the
//! [`on_main_thread`](crate::main_thread::NamirMainThread::on_main_thread) callback that alone
//! may call [`HostParams::rescan`](clack_extensions::params::HostParamsImplMainThread::rescan).
//!
//! # Why this module exists (issue #94)
//!
//! `spawn_recall_preset` (`crate::worker_jobs`) runs on `namir_worker`'s pool and, after adopting
//! a `.namirpreset` document, sets `SharedInner::params_rescan_pending` so the next main-thread
//! callback tells the host its cached parameter values are stale. But nothing *woke* the main
//! thread: [`HostParams::rescan`] is `[main-thread]`, the pool thread cannot call it, and with no
//! latency/priority event in flight no main-thread callback was ever scheduled — so the host's
//! cached values stayed stale until the user happened to trigger one. The fix is the CLAP
//! mechanism [`HostSharedHandle::request_callback`], which asks the host to schedule the very
//! callback that services the pending flag.
//!
//! The difficulty is the handle's lifetime. `clack_plugin` hands `new_shared` a
//! `HostSharedHandle<'a>` tied to the plugin instance's `'a`, while a pool-job closure must
//! capture only `'static` data. This module erases that lifetime, storing a
//! [`HostSharedHandle<'static>`] and documenting exactly why the underlying `clap_host` pointer
//! stays valid for as long as any thread could still touch it.
//!
//! Confined to this one module per D-5.3/NFR-QUAL-070 — `#![allow(unsafe_code)]` below opts only
//! this file back into this crate's `[lints.rust] unsafe_code = "deny"`, the same
//! "`deny`, not `forbid`, so only a *designated* module can opt back in" shape the crate's
//! `gui.rs` already uses and `namir-platform`'s `denormal.rs` and `thread_priority.rs` use there.
//! This is the fourth designated `unsafe` module in the workspace (`docs/02-architecture.md`
//! D-5.3's *Consequence (added M15, 2026-09-07, issue #94)*).
//!
//! # D-5.3's written safety argument for [`HostWake::from_shared`]'s `unsafe` block
//!
//! **What the `unsafe` block does.** `HostWake::from_shared` reinterprets a
//! `HostSharedHandle<'_>` — obtained straight from `DefaultPluginFactory::new_shared`'s own
//! parameter, the same `clap_host` pointer every safe CLAP call this plugin makes already goes
//! through — as a `HostSharedHandle<'static>`, by taking its raw `clap_host` pointer
//! (`NonNull::from(host.as_raw())`) and rebuilding a handle from it with
//! [`HostSharedHandle::from_raw`]. The `'static` is a lie about the *type* only; every use of the
//! erased handle is restricted to [`HostWake::request_callback`], which invokes exactly
//! `request_callback(&self)`, and the safety argument below establishes that this one call is
//! still in the pointer's valid lifetime. This is precisely the "use host callbacks in a container
//! that does not let you use proper lifetimes, but you can still guarantee that host callbacks
//! won't be called after the host's `'a` lifetime" case `with_arbitrary_lifetime` names — this
//! module implements the same contract with `from_raw` so the pointer is obtained in one place.
//!
//! **1. `clap_host` pointer validity is guaranteed by the CLAP spec to outlive the plugin
//! instance until `plugin->destroy` returns.** The `clap_host` a plugin receives is the host
//! process's own handle, passed to `clap_plugin_factory.create` / `new_shared` and valid for the
//! whole of the plugin's lifecycle (`clack-plugin` `src/host.rs:341`'s `from_raw` doc: "Pointer
//! must be valid for the duration of the `'a` lifetime" — and `'a` spans the plugin instance). A
//! host that freed it before `destroy` returned would already have broken the one C ABI contract
//! every CLAP plugin is written against. So the pointer this module stores is valid from the
//! moment `new_shared` receives it until `clap_plugin.destroy` returns.
//!
//! **2. `NamirShared::drop` (invoked on `plugin->destroy`) calls `self.pool.shutdown()`, which
//! joins all worker threads before plugin destruction completes, ensuring no worker thread can
//! touch the handle after `destroy`.** The handle is captured by pool-job closures, so the only
//! threads that can call [`HostWake::request_callback`] are `namir_worker`'s pool threads.
//! `impl Drop for NamirShared` calls `SharedInner::shutdown_workers`, which cancels the
//! library scan and then `pool.shutdown()` — `ThreadPool::shutdown` joins every worker thread and
//! returns only once they are all finished (`crate::shared`'s drop impl documents that this is the
//! sole mechanism preventing the M9a `0xc0000005` teardown crash). `destroy` therefore cannot
//! return — and the host cannot drop its `clap_host` pointer — while any worker thread that could
//! still call `request_callback` is alive. The pointer's guaranteed lifetime (argument 1) extends
//! at least to the end of `destroy`, so every `request_callback` issued by a joined-completed pool
//! thread lands strictly inside it.
//!
//! **3. `request_callback` is explicitly thread-safe per the CLAP specification and clack
//! documentation.** `clack-plugin`'s own `HostSharedHandle` declares `unsafe impl Send/Sync` with
//! the comment "this type only safely exposes the thread-safe operations of `clap_host`"
//! (`src/host.rs:47-52`), and `request_callback` is one of those operations (its entry in the
//! `clap_host` struct is the `request_callback` C pointer, callable from any thread the plugin
//! designates — here, a worker thread, which is a supported CLAP usage for host *requests*). The
//! call resolves and invokes a raw C function pointer against the host's own handle; it is not a
//! call into this crate's code, so no Namir data (and no `SharedInner`) is on the other side of
//! the erased lifetime. The race this could theoretically present — a `request_callback` landing
//! after `destroy` — is closed by argument 2.
//!
//! **Why sound rather than a gap.** The erased handle stores *no* `SharedInner` data and exposes
//! *one* method, `request_callback`, whose safety the three arguments pin to the CLAP contract
//! that the pointer outlives the instance and the pool-join guarantee that no worker thread
//! survives `destroy`. There is no other field, no dereference of any `'a`-tied reference, and no
//! way for the `'static` to leak into any other reasoning about the plugin. This is the same class
//! of foreign-ABI trust `gui.rs`'s `set_parent` argument and `thread_priority.rs`'s
//! `SetThreadPriority` argument already document — a documented platform/protocol contract,
//! trusted because the alternative (a fatal worker-job panic, or no host rescan after a preset
//! recall) is not an acceptable trade, and because the join ordering is enforced in
//! `impl Drop for NamirShared` rather than assumed.

#![allow(unsafe_code)]

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use std::ptr::NonNull;

use clack_plugin::host::HostSharedHandle;

/// An erased-lifetime [`HostSharedHandle`] a worker-pool thread may hold, for the single purpose
/// of requesting a main-thread callback. See this module's doc comment for the full D-5.3 safety
/// argument.
pub(crate) struct HostWake {
    handle: Option<HostSharedHandle<'static>>,
    /// Test-only observation of how many times [`request_callback`](Self::request_callback) has
    /// been invoked, so `crate::worker_jobs`'s unit test can assert the wake is requested (the
    /// call is a no-op without a host, and only a sparse shared-build test has a real host to
    /// observe the C ABI through). Absent from production builds; `Ordering::Relaxed` is fine for
    /// a counter. See [`requested_callbacks`](Self::requested_callbacks).
    #[cfg(test)]
    callbacks_requested: AtomicUsize,
}

impl HostWake {
    /// An empty wake slot, for the `SharedInner`s that have no host — the bare instances built
    /// by this crate's unit tests and benches (`SharedInner::new`/`new_at`, which construct no
    /// plugin and receive no `clap_host`). [`request_callback`](Self::request_callback) is a
    /// no-op on it; the CLAP host path is only ever populated by [`from_shared`](Self::from_shared).
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            handle: None,
            #[cfg(test)]
            callbacks_requested: AtomicUsize::new(0),
        }
    }

    /// Stores `host`'s raw `clap_host` pointer behind an erased (`'static`) lifetime, so a pool
    /// job can request a main-thread callback later. **Unsafe — read the module doc comment's
    /// safety argument before touching this.** The argument pins that the pointer stays valid
    /// until `plugin->destroy` returns (1), that `NamirShared::drop` joins every worker thread
    /// before that point (2), and that `request_callback` itself is thread-safe (3).
    pub(crate) fn from_shared(host: &HostSharedHandle<'_>) -> Self {
        // SAFETY: see D-5.3's written argument in this module's doc comment. `host` is the live
        // plugin instance's `clap_host` (valid until `destroy` returns, CLAP's own contract); the
        // only thing the erased handle ever does is `request_callback`, which is explicitly
        // thread-safe; and this crate's drop joins all worker threads before `destroy` returns,
        // so no thread that holds this handle can call it after the pointer dies.
        let raw = NonNull::from(host.as_raw());
        let handle = unsafe { HostSharedHandle::from_raw(raw) };
        Self {
            handle: Some(handle),
            #[cfg(test)]
            callbacks_requested: AtomicUsize::new(0),
        }
    }

    /// Asks the host to schedule an `on_main_thread` callback — the wake a preset recall off the
    /// pool needs, so `SharedInner::params_rescan_pending` is actually serviced (issue #94). A
    /// no-op when this instance was built without a host ([`empty`](Self::empty)).
    pub(crate) fn request_callback(&self) {
        #[cfg(test)]
        self.callbacks_requested.fetch_add(1, Ordering::Relaxed);
        if let Some(handle) = self.handle {
            handle.request_callback();
        }
    }

    /// How many times [`request_callback`](Self::request_callback) has been invoked. Test-only —
    /// production builds never read it; `crate::worker_jobs`'s recall test does.
    #[cfg(test)]
    pub(crate) fn requested_callbacks(&self) -> usize {
        self.callbacks_requested.load(Ordering::Relaxed)
    }
}
