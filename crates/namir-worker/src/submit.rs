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
//!
//! # How long the mutex is held (revised after issue #106's fix)
//!
//! **The mutex is held for one push, never across a wait.** [`CommandSubmitter::submit`] used to
//! take it once and keep it for its whole deadline, sleeping under it; the retry loop now takes it
//! per attempt and drops it before each sleep. The difference is not a micro-optimisation, it is
//! what makes [`CommandSubmitter::try_submit`]'s non-blocking answer honest: with the old hold,
//! "the producer is busy" meant "a worker is somewhere inside a two-second deadline", which is a
//! *state*, and refusing a command for the duration of a state silently drops every parameter
//! change made during it. With the hold narrowed to a single `try_push`, "the producer is busy"
//! means "another thread is between two instructions", which is the momentary miss `try_submit`'s
//! contract was always written against.
//!
//! What is given up is the strict first-come ordering the old convoy gave two *concurrent*
//! blocking submitters on a full ring: whichever thread wins the lock when room appears goes
//! first. No caller depends on that — an `Instance` owns its submitter and is reached through
//! `&mut self`, so both shells already serialise every submitter access through their own
//! `Mutex<Instance>` — and the commands in question (a parameter change, a resource offer) carry
//! no ordering relation to each other. Serialised *access*, which is what D-7.2 asks for, is
//! unchanged: every push still happens under the mutex.

use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use namir_engine::{Command, RingProducer};

/// A bounded spin before parking, for the common case where the audio thread is mid-block and
/// about to drain.
const SPIN_ATTEMPTS: u32 = 64;

/// Sub-block granularity against a 1.333 ms block period — see this module's doc comment.
const RETRY_BACKOFF: Duration = Duration::from_micros(500);

/// How many times [`CommandSubmitter::try_submit`] re-tries the *mutex* before giving the command
/// back. Deliberately an iteration count and not a duration: the wait it bounds is then bounded by
/// this many `pause` instructions **whatever any other thread does**, which is the property the GUI
/// thread needs and a `lock()` cannot offer. It is sized against the longest hold that now exists —
/// one `is_abandoned` plus one `try_push`, tens of nanoseconds — with enough headroom to ride out a
/// worker in its own `SPIN_ATTEMPTS` phase, which re-takes the mutex rapidly for a few microseconds.
const LOCK_SPIN_ATTEMPTS: u32 = 256;

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
    ///
    /// # Why one `try_lock` was not enough (the #106 fix's own regression)
    ///
    /// "Not now, try again" is only a fair answer if the callers *do* try again, and they do not:
    /// both cite this method precisely so they can discard the result with `let _ =`, having
    /// already written the new value into their own snapshot state. A refusal is therefore a
    /// silently lost gesture — the knob moves on screen and the audio does not follow. That is
    /// tolerable for the miss this method was designed around (a ring the audio thread has not
    /// drained *this* block, which the next gesture or the next block resolves) and not tolerable
    /// for a mutex another thread holds across a two-second deadline, which is what a single
    /// `try_lock` was actually reporting before [`Self::submit_with_deadline`] stopped holding it
    /// that way.
    ///
    /// Both halves of that are fixed, and the order matters. The hold is narrowed first (see this
    /// module's doc comment), so the longest contention that can exist is one `try_push`; this
    /// method then re-tries the mutex [`LOCK_SPIN_ATTEMPTS`] times against that, which is bounded
    /// by a fixed number of `pause` instructions and by nothing another thread can extend. A
    /// [`SubmitError::Timeout`] from here once again means what the callers assume it means: the
    /// ring itself had no room.
    pub fn try_submit(&self, command: Command) -> Result<(), SubmitError> {
        for _ in 0..LOCK_SPIN_ATTEMPTS {
            let Some(mut producer) = self.try_lock() else {
                std::hint::spin_loop();
                continue;
            };
            if producer.is_abandoned() {
                return Err(SubmitError::Abandoned(command));
            }
            return producer.try_push(command).map_err(SubmitError::Timeout);
        }
        Err(SubmitError::Timeout(command))
    }

    /// Blocks until the audio thread makes room, or the default deadline expires. **Worker threads
    /// only** — never call this from the UI thread.
    pub fn submit(&self, command: Command) -> Result<(), SubmitError> {
        self.submit_with_deadline(command, DEFAULT_DEADLINE)
    }

    /// As [`Self::submit`], with an explicit deadline.
    ///
    /// **The mutex is taken per attempt and released before every sleep** — it is *not* held
    /// across the wait. It was, until the #106 fix's own regression made the cost of that plain:
    /// see this module's "How long the mutex is held" section for the argument, and
    /// [`Self::try_submit`] for the caller it was costing. What the mutex still guarantees is the
    /// one thing D-7.2 asks of it — that no two threads touch the producer at once — because every
    /// push happens under it.
    ///
    /// Two worker threads submitting to a full ring therefore interleave rather than convoy, and
    /// whichever wins the mutex when room appears goes first. Submitters are per-instance
    /// (unrelated instances never contend) and an `Instance` is reached through `&mut self`, so in
    /// both shells that case does not arise at all; where it could, the two commands carry no
    /// ordering relation.
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
        let mut command = command;

        for _ in 0..SPIN_ATTEMPTS {
            match self.attempt(command) {
                Ok(()) => return Ok(()),
                Err(SubmitError::Abandoned(back)) => return Err(SubmitError::Abandoned(back)),
                Err(SubmitError::Timeout(back)) => command = back,
            }
            std::hint::spin_loop();
        }

        loop {
            match self.attempt(command) {
                Ok(()) => return Ok(()),
                Err(SubmitError::Abandoned(back)) => return Err(SubmitError::Abandoned(back)),
                Err(SubmitError::Timeout(back)) => command = back,
            }
            if started.elapsed() >= deadline {
                return Err(SubmitError::Timeout(command));
            }
            std::thread::sleep(RETRY_BACKOFF);
        }
    }

    /// One locked attempt, guard taken and dropped inside. The whole retry policy lives in the
    /// callers precisely so that no wait — no sleep, no spin, no deadline test — happens under the
    /// mutex.
    fn attempt(&self, command: Command) -> Result<(), SubmitError> {
        let mut producer = self.lock();
        if producer.is_abandoned() {
            return Err(SubmitError::Abandoned(command));
        }
        producer.try_push(command).map_err(SubmitError::Timeout)
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

    /// **The regression `try_submit`'s #106 fix introduced.** A `try_lock` that gives up on first
    /// contention is only harmless if contention is brief, and before this test's fix it was not:
    /// [`CommandSubmitter::submit_with_deadline`] held the producer mutex across its *entire*
    /// sleep loop, so for as long as a worker was inside it — up to [`DEFAULT_DEADLINE`] — every
    /// `try_submit` returned `Timeout`, whatever the ring's actual state. The production callers
    /// (`namir-clap/src/ui_host.rs`, `namir-app/src/host.rs`) discard that with `let _ =` after
    /// having already written the new value into their own snapshot state, so the user sees the
    /// knob move and hears nothing change.
    ///
    /// The ring here has **four free slots** at the moment of the attempt and a live consumer, so
    /// nothing about the ring justifies refusing the command: the only thing in the way is the
    /// other thread's mutex, and that is not a reason to drop a user's gesture.
    #[test]
    fn a_change_the_ring_had_room_for_lands_even_while_a_worker_is_mid_deadline() {
        let (tx, mut rx) = ring::<Command>(8);
        let submitter = std::sync::Arc::new(CommandSubmitter::new(tx));
        for i in 0..8 {
            submitter
                .try_submit(param(i))
                .expect("the ring starts empty");
        }

        let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = {
            let submitter = std::sync::Arc::clone(&submitter);
            let entered = std::sync::Arc::clone(&entered);
            std::thread::spawn(move || {
                entered.store(true, std::sync::atomic::Ordering::Release);
                submitter.submit_with_deadline(param(100), DEFAULT_DEADLINE)
            })
        };
        while !entered.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::yield_now();
        }
        // Long enough for the worker to be past its spin phase and settled into the sleep loop,
        // which is where the pre-fix version parked holding the mutex.
        std::thread::sleep(Duration::from_millis(50));

        // Room appears. The worker will claim one slot at its next wake (<= RETRY_BACKOFF), which
        // still leaves three, so this is not a contest for the last slot.
        for _ in 0..4 {
            rx.try_pop().expect("eight were queued");
        }

        let started = Instant::now();
        let result = submitter.try_submit(param(42));
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(250),
            "try_submit waited {elapsed:?} -- #106's property must survive this fix"
        );
        assert!(
            result.is_ok(),
            "a knob turn was refused while four slots were free: {result:?}. Mere producer-mutex \
             contention is not a full ring, and every production caller discards this error"
        );

        worker
            .join()
            .unwrap()
            .expect("the worker's own command had room too");
        let mut ids = Vec::new();
        while let Some(command) = rx.try_pop() {
            if let Command::Param(change) = command {
                ids.push(change.id.0);
            }
        }
        assert!(
            ids.contains(&42),
            "`try_submit` said Ok, so the change must be queued for the audio thread; the ring \
             held {ids:?}"
        );
    }

    /// **What is left of issue #106 once the hold is narrowed.** The test above asserts the right
    /// property but, since `submit_with_deadline` stopped holding the mutex across its deadline,
    /// no longer *discriminates*: with the longest hold reduced to one `try_push`, a `try_submit`
    /// written back as a blocking `lock()` acquires in nanoseconds, finds the same full ring, and
    /// returns the same `Timeout` well inside that test's 250 ms bound. Checked rather than
    /// assumed — reverting `try_submit` to `lock()` leaves the whole module's suite green.
    ///
    /// So the contract is pinned here instead, against the only thing that can ever make it false:
    /// *something* holding the producer for a long time. The holder is this test, so the guarantee
    /// no longer depends on any other method's retry policy — which is what let the property quietly
    /// lose its guard in the first place.
    #[test]
    fn try_submit_does_not_wait_for_whoever_holds_the_producer() {
        let (tx, _rx) = ring::<Command>(4);
        let submitter = std::sync::Arc::new(CommandSubmitter::new(tx));

        // The hold is released on a timer rather than on a flag this thread sets *after* the call
        // below: a regression must fail this test, not hang it, and a `lock()` here would never
        // reach a flag it is itself blocking.
        let held = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let holder = {
            let submitter = std::sync::Arc::clone(&submitter);
            let held = std::sync::Arc::clone(&held);
            std::thread::spawn(move || {
                let _guard = submitter.lock();
                held.store(true, std::sync::atomic::Ordering::Release);
                std::thread::sleep(Duration::from_millis(400));
            })
        };
        while !held.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::yield_now();
        }

        let started = Instant::now();
        let result = submitter.try_submit(param(1));
        let elapsed = started.elapsed();
        holder.join().unwrap();

        assert!(
            elapsed < Duration::from_millis(250),
            "try_submit waited {elapsed:?} for a held producer -- it is documented never to \
             block, and the UI thread calls it"
        );
        assert!(
            matches!(result, Err(SubmitError::Timeout(_))),
            "a contended producer must hand the command back rather than dropping it"
        );
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
