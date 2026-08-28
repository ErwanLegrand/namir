//! D-7.2's producer side: "The worker is the sole producer; a mutex on the *producer side only*
//! serialises UI and worker submissions."
//!
//! The engine hands out exactly one [`RingProducer`] per instance — that *is* the SPSC guarantee.
//! What is left to decide is how several worker threads and the UI share it, and D-7.2 answers
//! that: an ordinary mutex, on this side only, because this side is not the audio thread and may
//! block.
//!
//! # "waits and retries; it never drops a command silently"
//!
//! **Decision:** the retry is bounded — a short spin, then sleeps, then a deadline, after which the
//! command is handed *back* to the caller as [`SubmitError::Timeout`].
//!
//! **Rationale:** a literal unbounded retry is a liveness hazard D-7.2 does not discuss. A host
//! that deactivates a plugin stops calling `process` entirely, so the ring is never drained again;
//! an unbounded retry would wedge a pool thread permanently, and with a two-thread pool (D-7.1)
//! two such submissions wedge the whole worker. The operative word in D-7.2 is **silently**: a
//! command here is dropped only after a bounded wait, by an explicit decision, with a catalogued
//! error ([`crate::error_codes::NOT_DELIVERED`]) and a reported outcome — and the value itself
//! comes back to the caller, so even the drop happens on the worker's thread where it is legal.
//!
//! **Consequence:** `submit` can fail, and callers must decide what to do with the returned
//! command rather than assume delivery.
//!
//! The sleep interval is chosen against the consumer's real cadence rather than picked round: the
//! audio callback drains once per block, which is 1.333 ms at NFR-PERF-010's own condition, so
//! sub-block granularity is what a retry needs, and sleeping rather than spinning avoids burning a
//! core the audio thread may want.

use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use namir_engine::{Command, RingProducer};

/// A bounded spin before parking, for the common case where the audio thread is mid-block and
/// about to drain.
const SPIN_ATTEMPTS: u32 = 64;

/// Sub-block granularity against a 1.333 ms block period — see this module's doc comment.
const RETRY_BACKOFF: Duration = Duration::from_micros(500);

/// How long a blocking submit keeps trying before giving the command back.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(2);

/// Why a submission did not land. **Carries the command back** in every case — nothing is dropped
/// inside `submit`.
pub enum SubmitError {
    /// The command did not land within the attempt's deadline. For [`CommandSubmitter::submit`]
    /// that means the audio thread did not drain the ring in time; for
    /// [`CommandSubmitter::try_submit`], whose deadline is zero, it also covers a producer another
    /// submitter held at that instant (issue #106). Both are "not now, try again", which is why
    /// they share one variant.
    Timeout(Command),
    /// The audio side is gone (its consumer was dropped), so nothing will ever drain.
    Abandoned(Command),
}

impl std::fmt::Debug for SubmitError {
    /// Hand-written because [`Command`] has no `Debug` — it owns prepared slots, and deriving
    /// `Debug` through them would drag it onto types that have no use for it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout(_) => f.write_str("Timeout(<command>)"),
            Self::Abandoned(_) => f.write_str("Abandoned(<command>)"),
        }
    }
}

/// The producer-side mutex D-7.2 specifies, one per engine instance.
pub struct CommandSubmitter {
    producer: Mutex<RingProducer<Command>>,
}

impl CommandSubmitter {
    /// Wraps one instance's command producer.
    pub fn new(producer: RingProducer<Command>) -> Self {
        Self {
            producer: Mutex::new(producer),
        }
    }

    /// One attempt, never blocks. **This is what the UI thread uses** — D-15.3 says the UI never
    /// blocks on the worker, and a parameter change that misses one block is not worth stalling a
    /// frame for.
    ///
    /// # Why the mutex is *tried*, not taken (issue #106)
    ///
    /// There are two ways this can fail to land, and only one of them is the ring. The other is
    /// [`Self::submit_with_deadline`], which holds this same mutex across its entire wait: a
    /// worker mid-`Instance::load` against a host that has deactivated the plugin holds it for
    /// [`DEFAULT_DEADLINE`]. A plain `lock()` here would therefore park the GUI thread for up to
    /// two seconds inside a call documented never to block — the exact D-15.3 / FR-UI-060
    /// violation `namir-clap`'s `audio.rs` reasons about for the audio thread, arriving on the
    /// thread that draws frames.
    ///
    /// So contention is treated as the momentary miss it is: the command comes back as
    /// [`SubmitError::Timeout`], the same outcome and the same caller response (retry on a later
    /// frame) as a ring that happened to be full. Nothing is dropped, and the promise the callers
    /// in `namir-clap/src/ui_host.rs` and `namir-app/src/host.rs` cite is one this method keeps
    /// against a contended producer as well as a full ring.
    pub fn try_submit(&self, command: Command) -> Result<(), SubmitError> {
        let Some(mut producer) = self.try_lock() else {
            return Err(SubmitError::Timeout(command));
        };
        if producer.is_abandoned() {
            return Err(SubmitError::Abandoned(command));
        }
        producer.try_push(command).map_err(SubmitError::Timeout)
    }

    /// Blocks until the audio thread makes room, or the default deadline expires. **Worker threads
    /// only** — never call this from the UI thread.
    pub fn submit(&self, command: Command) -> Result<(), SubmitError> {
        self.submit_with_deadline(command, DEFAULT_DEADLINE)
    }

    /// As [`Self::submit`], with an explicit deadline.
    ///
    /// The mutex is held across the wait, which is what makes the producer side single at any
    /// instant. Two worker threads submitting to a full ring therefore form a bounded convoy: one
    /// sleeps against the ring, the other against the mutex. That is acceptable because submitters
    /// are per-instance (unrelated instances never contend), the wait is deadline-bounded, and it
    /// sleeps rather than spins. [`Self::try_submit`] is deliberately **not** part of that convoy
    /// — see its own doc comment for why the caller it serves may not join one.
    ///
    /// **The one hard rule for callers: never hold the resource cache's lock across this call.**
    /// A full ring on one instance would otherwise stall every other instance's cache lookup, which
    /// would undermine D-8.2's whole "nothing contends" argument on the worker side instead of the
    /// audio side. `handover.rs` is structured so the cache guard is always released first.
    pub fn submit_with_deadline(
        &self,
        command: Command,
        deadline: Duration,
    ) -> Result<(), SubmitError> {
        let started = Instant::now();
        let mut producer = self.lock();
        let mut command = command;

        for _ in 0..SPIN_ATTEMPTS {
            if producer.is_abandoned() {
                return Err(SubmitError::Abandoned(command));
            }
            match producer.try_push(command) {
                Ok(()) => return Ok(()),
                Err(back) => command = back,
            }
            std::hint::spin_loop();
        }

        loop {
            if producer.is_abandoned() {
                return Err(SubmitError::Abandoned(command));
            }
            match producer.try_push(command) {
                Ok(()) => return Ok(()),
                Err(back) => command = back,
            }
            if started.elapsed() >= deadline {
                return Err(SubmitError::Timeout(command));
            }
            std::thread::sleep(RETRY_BACKOFF);
        }
    }

    /// Recovers from poisoning rather than propagating it, for the same P8 reason
    /// `cache::lock` documents: a submitter that failed forever after one unrelated panic would be
    /// a total failure, not degradation. The producer's own invariants are `rtrb`'s, and a panic
    /// cannot leave it half-pushed — `try_push` either moves the value in or hands it back.
    fn lock(&self) -> MutexGuard<'_, RingProducer<Command>> {
        self.producer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// [`Self::lock`]'s non-blocking form, for [`Self::try_submit`]. A poisoned-but-free mutex is
    /// recovered exactly as `lock` recovers it, for the same reason; `WouldBlock` — another
    /// submitter holding it — is the only case that yields `None`.
    fn try_lock(&self) -> Option<MutexGuard<'_, RingProducer<Command>>> {
        match self.producer.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_engine::{ParamChange, ParamId, ring};

    fn param(i: u32) -> Command {
        Command::Param(ParamChange {
            id: ParamId(i),
            value: 0.0,
        })
    }

    /// **D-7.2: "it never drops a command silently."** A full ring must hand the command back, not
    /// swallow it.
    #[test]
    fn a_full_ring_returns_the_command_rather_than_dropping_it() {
        let (tx, _rx) = ring::<Command>(2);
        let submitter = CommandSubmitter::new(tx);
        assert!(submitter.try_submit(param(1)).is_ok());
        assert!(submitter.try_submit(param(2)).is_ok());
        match submitter.try_submit(param(3)) {
            Err(SubmitError::Timeout(returned)) => {
                // The command survived and can be retried.
                assert_eq!(returned.kind(), namir_engine::CommandKind::Param);
            }
            _ => panic!("a full ring must hand the command back"),
        }
    }

    /// D-7.2's "the producer waits and retries": a submit blocked on a full ring lands as soon as
    /// the consumer makes room.
    #[test]
    fn submit_blocks_until_the_consumer_drains_then_delivers() {
        let (tx, mut rx) = ring::<Command>(1);
        let submitter = CommandSubmitter::new(tx);
        submitter.try_submit(param(1)).expect("first fits");

        let drainer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            rx.try_pop().expect("there is one queued");
            rx
        });

        submitter
            .submit_with_deadline(param(2), Duration::from_secs(5))
            .expect("should land once the consumer drains");
        let _rx = drainer.join().unwrap();
    }

    /// The deadline exists so a deactivated plugin cannot wedge a pool thread forever. Expiry must
    /// still return the command rather than drop it.
    #[test]
    fn submit_returns_the_command_when_the_deadline_expires() {
        let (tx, _rx) = ring::<Command>(1);
        let submitter = CommandSubmitter::new(tx);
        submitter.try_submit(param(1)).expect("first fits");
        let started = Instant::now();
        match submitter.submit_with_deadline(param(2), Duration::from_millis(50)) {
            Err(SubmitError::Timeout(_)) => {}
            _ => panic!("a never-drained ring must time out rather than block forever"),
        }
        assert!(started.elapsed() >= Duration::from_millis(50));
    }

    /// **Issue #106.** `try_submit`'s "one attempt, never blocks" is a promise made to the *GUI*
    /// thread (D-15.3, FR-UI-060), and a full ring is only one of the two ways it can fail to
    /// land: the other is a worker thread already inside [`CommandSubmitter::submit`], holding the
    /// producer mutex across its whole deadline. Before the fix this call waited on that mutex for
    /// up to `DEFAULT_DEADLINE`, so the one caller forbidden to block was the one that blocked
    /// longest.
    #[test]
    fn try_submit_does_not_wait_for_a_worker_already_inside_the_deadline() {
        let (tx, rx) = ring::<Command>(1);
        let submitter = std::sync::Arc::new(CommandSubmitter::new(tx));
        submitter.try_submit(param(1)).expect("first fits"); // the ring is now full

        let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = {
            let submitter = std::sync::Arc::clone(&submitter);
            let entered = std::sync::Arc::clone(&entered);
            std::thread::spawn(move || {
                entered.store(true, std::sync::atomic::Ordering::Release);
                // Deliberately the full default deadline: the pre-fix failure is that the call
                // below waits out *this* thread's deadline, so a short one would hide it.
                submitter.submit_with_deadline(param(2), DEFAULT_DEADLINE)
            })
        };
        while !entered.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::yield_now();
        }
        // The flag is set just before the lock is taken, so settle briefly to make the contended
        // case the one actually measured. Overshooting only costs a false pass, never a false
        // failure.
        std::thread::sleep(Duration::from_millis(100));

        let started = Instant::now();
        let result = submitter.try_submit(param(3));
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(250),
            "try_submit waited {elapsed:?} on a contended producer -- it is documented never to \
             block, and the UI thread calls it"
        );
        assert!(
            matches!(result, Err(SubmitError::Timeout(_))),
            "a contended producer must hand the command back rather than dropping it"
        );

        // Release the worker rather than waiting out its deadline: an abandoned ring is reported
        // at once, so this costs one retry interval instead of two seconds.
        drop(rx);
        let _ = worker.join().unwrap();
    }

    /// If the audio side is gone entirely, say so distinctly rather than waiting out the deadline.
    #[test]
    fn an_abandoned_ring_is_reported_immediately() {
        let (tx, rx) = ring::<Command>(4);
        drop(rx);
        let submitter = CommandSubmitter::new(tx);
        match submitter.submit_with_deadline(param(1), Duration::from_secs(30)) {
            Err(SubmitError::Abandoned(_)) => {}
            _ => panic!("an abandoned ring should be reported, not waited on"),
        }
    }
}
