//! D-7.1's worker pool: "one pool sized to `min(2, cores-1)`", may allocate, may block, and
//! deliberately not an async runtime.
//!
//! Std `Mutex` + `Condvar` over a `VecDeque`, which is about a hundred lines and no dependency.
//! D-7.1 rejects tokio and friends explicitly ("a scheduler, a large dependency surface and no
//! benefit for a workload that is a handful of long CPU/IO tasks"); the same argument disposes of
//! `rayon` (a work-stealing runtime) and of a channel crate (this pool has to *inspect* its queue
//! to supersede stale jobs, which a `Receiver` does not permit).

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// Live worker threads across every [`ThreadPool`] in this process — see [`live_worker_threads`].
static LIVE_WORKER_THREADS: AtomicUsize = AtomicUsize::new(0);

/// How many pool worker threads are alive in this process right now, summed over every
/// [`ThreadPool`].
///
/// **Test observability**, for one assertion that cannot be made any other way: that a *host-driven*
/// teardown has joined the threads the torn-down thing started, before the call that tore it down
/// returned. `clap_plugin.destroy`'s caller is entitled to `FreeLibrary` the plugin the instant
/// destroy returns, so "did destroy join its workers?" is a correctness question, not a tidiness
/// one — and from outside the plugin (an in-process CLAP host harness, which is all
/// `crates/namir-clap/tests/clap_host_teardown.rs` gets) there is no handle on that instance's pool
/// to ask. Answered process-globally instead.
///
/// Two atomic read-modify-writes per **thread lifetime** — never per job, never per sample, and
/// nowhere near the audio thread. A reader that shares the process with unrelated pools sees their
/// threads too, so compare against a baseline taken in the same process rather than against zero,
/// and only in a test binary that is not building pools concurrently on other threads.
pub fn live_worker_threads() -> usize {
    LIVE_WORKER_THREADS.load(Ordering::Acquire)
}

/// Decrements [`LIVE_WORKER_THREADS`] however the worker thread ends — including an unwind that
/// escapes [`worker_loop`] itself (a job's own panic is already caught inside it, per D-16.3).
struct LiveThreadGuard;

impl Drop for LiveThreadGuard {
    fn drop(&mut self) {
        LIVE_WORKER_THREADS.fetch_sub(1, Ordering::Release);
    }
}

/// D-7.1's sizing, with a floor.
///
/// **Decision:** the literal `min(2, cores - 1)` is clamped to at least 1.
///
/// **Rationale:** on a single-core machine `min(2, 1 - 1)` is **0**, and a zero-thread pool never
/// runs a job — every model load would hang forever. That is a total failure of FR-NAM-070, not
/// P8's "degrades". The formula's evident intent is "at most two, and leave a core for audio"; on a
/// one-core machine there is no core to leave, and a worker may block anyway (D-7.1), so it yields
/// to the audio thread naturally.
///
/// **Consequence:** NFR-PORT-030's "no assumption that the process can spawn unlimited threads" is
/// satisfied more strongly than the formula requires — the pool is at most two threads, created
/// once at construction and never grown.
///
/// **Rejected:** running jobs inline on the caller's thread when `cores == 1`. The caller is
/// sometimes the UI thread, and a 500 ms inline load freezes it (FR-UI-060).
pub fn pool_size(cores: usize) -> usize {
    cores.saturating_sub(1).clamp(1, 2)
}

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    ready: Condvar,
    shutdown: AtomicBool,
}

/// A fixed-size pool of worker threads.
///
/// # Shutdown is a method, not only a `Drop` (added M9b)
///
/// [`Self::shutdown`] exists because an owner cannot always rely on its own `Drop` running when it
/// needs the threads gone. `namir-clap` is the case that forced this: its `SharedInner` *owns* the
/// pool, every worker job holds an `Arc<SharedInner>` to reach it, and so the last `Arc` — the one
/// whose drop would run `Drop for ThreadPool` — belongs to whichever job finishes last, not to the
/// host thread calling `clap_plugin.destroy`. See that impl's own doc comment
/// (`crates/namir-clap/src/shared.rs`) for what that cost.
///
/// `threads` is therefore behind a `Mutex` rather than being a plain `Vec`: shutdown has to be
/// callable through a `&self` reached from inside an `Arc`.
pub struct ThreadPool {
    shared: Arc<Shared>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl ThreadPool {
    /// Builds a pool sized per [`pool_size`] from `available_parallelism` (treating an error as
    /// one core, the conservative reading).
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self::with_threads(pool_size(cores))
    }

    /// A pool of exactly `threads` threads. Tests use this so job ordering is total.
    pub fn with_threads(threads: usize) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let threads = (0..threads.max(1))
            .map(|_| {
                let shared = Arc::clone(&shared);
                // Incremented on *this* thread, before the spawn, so the count is already exact by
                // the time this constructor returns; the guard inside decrements it however the
                // worker ends.
                LIVE_WORKER_THREADS.fetch_add(1, Ordering::Release);
                std::thread::spawn(move || {
                    let _live = LiveThreadGuard;
                    worker_loop(&shared)
                })
            })
            .collect();
        Self {
            shared,
            threads: Mutex::new(threads),
        }
    }

    /// How many threads this pool still has to join — its full size until [`Self::shutdown`] runs,
    /// zero after.
    pub fn threads(&self) -> usize {
        lock(&self.threads).len()
    }

    /// Queues `job`. It runs on a worker thread, isolated per D-16.3 — see [`worker_loop`].
    ///
    /// After [`Self::shutdown`] this **drops `job` instead of queueing it**, and does so under the
    /// queue lock (which is also where `shutdown` publishes the flag) so the two cannot straddle
    /// each other. Queueing onto a pool with no threads left would not merely lose the job: the job
    /// owns whatever it captured, so it would keep an owner alive with nothing left to release it.
    /// Jobs queued *before* shutdown still run — see [`Self::shutdown`].
    pub fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        {
            let mut queue = lock(&self.shared.queue);
            if self.shared.shutdown.load(Ordering::Acquire) {
                return;
            }
            queue.push_back(Box::new(job));
        }
        self.shared.ready.notify_one();
    }

    /// Stops accepting new jobs, lets every already-queued one run, and joins every worker thread
    /// **on the calling thread** — returning only once none of this pool's threads is running any
    /// more. Idempotent, and what [`Drop`] does.
    ///
    /// # Why an owner may need to call this rather than just dropping the pool
    ///
    /// See this type's own doc comment: an owner reachable from inside its own jobs cannot be
    /// dropped by the thread that wants the threads gone. Calling `shutdown` directly is how such
    /// an owner gets the join to happen on *its* thread, at the point it chooses.
    ///
    /// # Two deadlocks this deliberately does not have
    ///
    /// The handles are **taken out of the mutex and joined with the lock released**. Holding it
    /// across a join would deadlock the moment a job's own drop glue re-entered `shutdown` (which
    /// is exactly what happens when a job holds the last reference to the pool's owner): the joiner
    /// would wait for a thread that was waiting for the joiner's lock. Draining first makes a
    /// re-entrant call find an empty list and return at once.
    ///
    /// A handle belonging to the **calling** thread is skipped rather than joined. `JoinHandle::
    /// join` on one's own thread blocks forever on Windows (`WaitForSingleObject` against the
    /// caller's own handle) and returns `EDEADLK` on Linux; either way it is never what the caller
    /// meant. Skipping detaches that one thread — it still observes the shutdown flag and exits on
    /// its own — which is a leak of one handle in a path that should not be reachable at all, and
    /// strictly better than wedging the thread that asked for the shutdown.
    pub fn shutdown(&self) {
        {
            // Published under the queue lock, so `spawn`'s check of it cannot straddle the store.
            let _queue = lock(&self.shared.queue);
            self.shared.shutdown.store(true, Ordering::Release);
        }
        self.shared.ready.notify_all();

        let handles = std::mem::take(&mut *lock(&self.threads));
        let current = std::thread::current().id();
        for handle in handles {
            if handle.thread().id() == current {
                continue;
            }
            let _ = handle.join();
        }
    }

    /// Queued-but-not-yet-started jobs. Test observability.
    pub fn queued(&self) -> usize {
        lock(&self.shared.queue).len()
    }
}

impl Default for ThreadPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ThreadPool {
    /// Signals shutdown and joins every thread, via [`ThreadPool::shutdown`] — a no-op if an owner
    /// already called that itself, which is the point of it being idempotent.
    ///
    /// A `join()` that returns `Err` — a thread that unwound *outside* a job body — is deliberately
    /// swallowed rather than propagated: a `Drop` impl that panics during an unwind aborts the
    /// process, which would be precisely the FR-ERR-040 failure this design exists to prevent.
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(shared: &Arc<Shared>) {
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if let Some(job) = queue.pop_front() {
                    break Some(job);
                }
                if shared.shutdown.load(Ordering::Acquire) {
                    break None;
                }
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(job) = job else { return };

        // D-16.3: "Worker jobs are isolated such that a panic in one is caught at the job
        // boundary, recorded, and does not unwind into the host (FR-ERR-040)."
        //
        // `AssertUnwindSafe` is needed because a job captures shared state behind mutexes, which is
        // exactly what `UnwindSafe` warns about. That warning is *handled*, not waved away: a panic
        // while a cache lock is held poisons it, and `cache::lock` recovers from poisoning rather
        // than propagating it, for the P8 reason documented there. Without that, one panicking job
        // would disable every subsequent load for the process's lifetime.
        //
        // The "recorded" half of that sentence was missing until M9b: the outcome was discarded
        // with `let _ =`, so `error_codes::JOB_PANICKED` existed and was emitted by nothing, and a
        // contained panic left no trace anywhere. It is now one FR-ERR-010 record at `Fault`
        // severity — the one record in this system a bug report most needs, since a contained
        // panic is by construction invisible to the user (the pool keeps serving and only this
        // job's result is lost). A pool thread is never an audio thread, so the record's
        // formatting and its writer's lock are both permitted here (D-16.2/FR-ERR-030).
        if let Err(payload) = std::panic::catch_unwind(AssertUnwindSafe(job)) {
            namir_platform::logging::record(
                crate::error_codes::JOB_PANICKED,
                panic_message(payload.as_ref()),
            );
        }
    }
}

/// The printable message out of a panic payload.
///
/// `panic!` boxes its payload as `&'static str` for a literal and as `String` for a formatted
/// message; those two cover every panic this workspace raises and every one `std` raises. Anything
/// else has no printable form at all — `Any` exposes no `Display` — so it is named as such rather
/// than dropping the record, which would put the pool back to swallowing the fault silently.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "a worker job panicked with a payload of an unprintable type"
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    /// D-7.1's sizing rule, including the clamp that keeps a single-core machine working at all.
    #[test]
    fn pool_size_is_never_zero_and_never_exceeds_two() {
        assert_eq!(pool_size(0), 1, "a zero-thread pool could never run a job");
        assert_eq!(pool_size(1), 1);
        assert_eq!(pool_size(2), 1);
        assert_eq!(pool_size(3), 2);
        assert_eq!(pool_size(4), 2);
        assert_eq!(pool_size(32), 2);
        for cores in 0..1024 {
            let n = pool_size(cores);
            assert!((1..=2).contains(&n), "pool_size({cores}) = {n}");
        }
    }

    #[test]
    fn every_queued_job_runs_exactly_once() {
        let pool = ThreadPool::with_threads(2);
        let count = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel();
        for _ in 0..64 {
            let count = Arc::clone(&count);
            let tx = tx.clone();
            pool.spawn(move || {
                count.fetch_add(1, Ordering::SeqCst);
                tx.send(()).unwrap();
            });
        }
        drop(tx);
        for _ in 0..64 {
            rx.recv().expect("every job should report in");
        }
        assert_eq!(count.load(Ordering::SeqCst), 64);
    }

    /// **D-16.3 / FR-ERR-040:** a panicking job is contained at the job boundary and the pool keeps
    /// serving. The panic hook is silenced for the duration so the suite's output stays readable —
    /// noted because otherwise it looks like a test is failing.
    ///
    /// **This is D-16.3's unit test, and since M14 it is no longer FR-ERR-040's tag site.** That
    /// requirement's method is "inject a fault into **each** non-audio subsystem", and this test
    /// reaches one of them and runs no audio at all — which is what its `trace-partial` said from
    /// M9a until the gap was closed rather than the tag promoted.
    /// `tests/fault_injection.rs` injects a fault into every non-audio subsystem this crate can
    /// see, with a live `AudioEngine` running beside it, and carries the tag (still partial: see
    /// its own `// uncovered:` field for the GUI thread and the plugin configuration).
    /// `namir-app/tests/settings_faults.rs` covers the one subsystem that lives in the other crate.
    #[test]
    fn a_panicking_job_is_contained_and_the_pool_keeps_serving() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let pool = ThreadPool::with_threads(1);
        let (tx, rx) = mpsc::channel();
        pool.spawn(|| panic!("a job failing on purpose"));
        pool.spawn(move || tx.send(42u32).unwrap());

        let got = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the pool must keep serving after a job panics");
        std::panic::set_hook(previous);
        assert_eq!(got, 42);
    }

    /// D-16.3's **"recorded"** half, at the only seam a unit test can observe it.
    ///
    /// The record itself goes to the process-global logger, which `namir_platform::logging::init`
    /// installs once per process against the real per-user sink — nothing a test may install,
    /// redirect or read back (that is deliberate; see that module's doc comment). What is pinned
    /// here is therefore the detail the record carries: the panic's own message, for both payload
    /// types `panic!` actually produces, and a named fallback rather than a dropped record for the
    /// third case. Getting this wrong would leave `worker.job.panicked` lines in a user's log with
    /// no indication of what panicked.
    #[test]
    fn a_contained_panic_renders_its_own_message_for_the_log_record() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let literal = std::panic::catch_unwind(|| panic!("a literal payload")).unwrap_err();
        let formatted =
            std::panic::catch_unwind(|| panic!("a formatted payload: {}", 7)).unwrap_err();
        std::panic::set_hook(previous);

        assert_eq!(panic_message(literal.as_ref()), "a literal payload");
        assert_eq!(panic_message(formatted.as_ref()), "a formatted payload: 7");

        let opaque: Box<dyn std::any::Any + Send> = Box::new(42u8);
        assert_eq!(
            panic_message(opaque.as_ref()),
            "a worker job panicked with a payload of an unprintable type"
        );
    }

    #[test]
    fn shutdown_joins_every_thread() {
        let pool = ThreadPool::with_threads(2);
        let count = Arc::new(AtomicUsize::new(0));
        for _ in 0..8 {
            let count = Arc::clone(&count);
            pool.spawn(move || {
                count.fetch_add(1, Ordering::SeqCst);
            });
        }
        drop(pool); // joins
        assert_eq!(count.load(Ordering::SeqCst), 8, "drop must drain the queue");
    }

    /// The property `namir-clap`'s `clap_plugin.destroy` needs and could not get from `Drop` alone:
    /// `shutdown` returns only once every worker thread is gone, and it is the *caller's* thread
    /// that waits.
    #[test]
    fn shutdown_returns_only_after_an_in_flight_job_has_finished() {
        let pool = ThreadPool::with_threads(2);
        let (started_tx, started_rx) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));

        let job_finished = Arc::clone(&finished);
        pool.spawn(move || {
            started_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
            job_finished.store(true, Ordering::Release);
        });

        started_rx.recv().expect("the job must start");
        // A 200 ms margin against a thread that has only just announced itself: the point is that
        // the job is genuinely still in flight when shutdown is called, not that it is fast.
        assert!(!finished.load(Ordering::Acquire));

        pool.shutdown();
        assert!(
            finished.load(Ordering::Acquire),
            "shutdown must not return while a job is still running"
        );
        assert_eq!(pool.threads(), 0, "every thread must have been joined");

        pool.shutdown(); // idempotent; must not hang or panic on an already-drained pool
    }

    /// A job queued after shutdown is dropped rather than left in a queue no thread will ever
    /// drain — otherwise it would hold whatever it captured (in `namir-clap`, an `Arc` to the very
    /// thing being torn down) alive with nothing left to release it.
    #[test]
    fn a_job_spawned_after_shutdown_is_dropped_rather_than_queued() {
        let pool = ThreadPool::with_threads(1);
        pool.shutdown();

        let captured = Arc::new(AtomicUsize::new(0));
        let job_captured = Arc::clone(&captured);
        pool.spawn(move || {
            job_captured.fetch_add(1, Ordering::SeqCst);
        });

        assert_eq!(pool.queued(), 0, "the job must not be queued");
        assert_eq!(
            Arc::strong_count(&captured),
            1,
            "the dropped job must have released what it captured"
        );
        assert_eq!(captured.load(Ordering::SeqCst), 0, "and never have run");
    }

    /// The self-join guard. A job that happens to hold the last reference to whatever owns the pool
    /// runs the pool's own `Drop` **on a pool thread**, so the drained handle list contains that
    /// thread's own handle. Joining it blocks forever on Windows; this asserts the drop returns.
    ///
    /// The completion signal is a `Sender` declared *after* the pool, so it is dropped — closing
    /// the channel — only once the pool's own drop has returned. A self-join would wedge that
    /// thread mid-drop, the sender would never drop, and this reports `Timeout` rather than
    /// hanging the whole test binary.
    #[test]
    fn dropping_the_pool_from_inside_one_of_its_own_jobs_does_not_self_join() {
        struct Owner {
            pool: ThreadPool,
            /// Field order is the assertion: dropped after `pool` has finished dropping.
            _done: mpsc::Sender<()>,
        }

        let (done_tx, done_rx) = mpsc::channel::<()>();
        let owner = Arc::new(Owner {
            pool: ThreadPool::with_threads(2),
            _done: done_tx,
        });

        let job_owner = Arc::clone(&owner);
        owner.pool.spawn(move || {
            // Long enough that the reference below is gone first, so this job holds the *last*
            // one and `Drop for ThreadPool` therefore runs here, on a pool thread.
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(job_owner);
        });
        drop(owner);

        match done_rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("the pool self-joined: its drop never returned")
            }
            Ok(()) => unreachable!("nothing is ever sent on this channel"),
        }
    }
}
