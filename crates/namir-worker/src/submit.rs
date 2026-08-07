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

use std::sync::{Mutex, MutexGuard, PoisonError};
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
    /// The ring was not drained within the deadline.
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
    pub fn try_submit(&self, command: Command) -> Result<(), SubmitError> {
        let mut producer = self.lock();
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
    /// sleeps rather than spins.
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
