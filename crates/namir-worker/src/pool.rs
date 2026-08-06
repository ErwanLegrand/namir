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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

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
pub struct ThreadPool {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
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
                std::thread::spawn(move || worker_loop(&shared))
            })
            .collect();
        Self { shared, threads }
    }

    /// How many threads this pool actually runs.
    pub fn threads(&self) -> usize {
        self.threads.len()
    }

    /// Queues `job`. It runs on a worker thread, isolated per D-16.3 — see [`worker_loop`].
    pub fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        lock(&self.shared.queue).push_back(Box::new(job));
        self.shared.ready.notify_one();
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
    /// Signals shutdown and joins every thread.
    ///
    /// A `join()` that returns `Err` — a thread that unwound *outside* a job body — is deliberately
    /// swallowed rather than propagated: a `Drop` impl that panics during an unwind aborts the
    /// process, which would be precisely the FR-ERR-040 failure this design exists to prevent.
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.ready.notify_all();
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
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
        let _ = std::panic::catch_unwind(AssertUnwindSafe(job));
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
}
