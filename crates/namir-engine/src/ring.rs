//! D-7.2's SPSC command ring and D-8.1's return ring: the two wait-free queues that carry
//! everything across the audio-thread boundary.
//!
//! Both directions of the handover protocol use the same structure, so this module defines it
//! once, generically, and the two rings differ only in their element type — [`crate::Command`]
//! inbound, [`crate::Retired`] outbound. D-7.3's telemetry ring is deliberately *not* here: it has
//! different semantics (overwrite-oldest rather than back-pressure, many readers' worth of loss
//! tolerance) and needs no dependency at all, so it lives in `telemetry.rs` built on plain
//! atomics.
//!
//! # Why this composes `rtrb` rather than hand-rolling the queue
//!
//! **Decision:** build both rings on the `rtrb` crate's `Producer`/`Consumer` pair rather than
//! writing the lock-free queue in this crate.
//!
//! **Rationale:** D-7.2 requires a "single-producer, single-consumer, wait-free ring buffer of
//! fixed-size command records, pre-allocated at preparation", and D-8.1's return ring must carry
//! an `Arc<PreparedNam>`/`Arc<PreparedIr>` — a non-`Copy` value with a destructor. A queue of
//! non-`Copy` values with wait-free concurrent access cannot be written in safe Rust: the slot
//! storage needs `UnsafeCell`, and every read out of it is an `unsafe` move. This crate declares
//! `unsafe_code = "forbid"` (D-5.3, via the workspace lint in the root `Cargo.toml`), and a
//! `forbid`-level lint cannot be downgraded by a nested `#[allow]` the way `namir-platform`'s
//! `deny` can. So the unsafe has to live somewhere else, and a dependency is where.
//!
//! This is the same move `rt_harness.rs` already documents for `assert_no_alloc`, and for the same
//! reason — see that module's doc comment, which states the `forbid`-vs-`deny` distinction in
//! full. `rtrb` was checked against this project's own constraints before adoption rather than
//! trusted: MIT OR Apache-2.0 (NFR-LIC-020; already on `deny.toml`'s allow-list, so that file
//! needed no edit), **zero** transitive dependencies, no build script, `no_std`-capable pure Rust
//! (so NFR-PORT-030's `aarch64-linux-android`/`aarch64-apple-ios` cross-builds are unaffected),
//! and its own `rust-version = "1.38"` sits far below this workspace's 1.97 MSRV (NFR-PORT-010).
//!
//! **Consequence:** `RingBuffer::new` allocates the whole buffer once, up front, and neither
//! `push` nor `pop` allocates thereafter — which is what makes this legal on the audio thread at
//! all. Ring capacity is therefore a preparation-time decision (P1/D-6.1), not something that can
//! grow under load.
//!
//! **Consequence:** `rtrb::PushError::Full(T)` hands the rejected value *back* to the caller
//! rather than dropping it. That is not a convenience here, it is load-bearing: D-8.1 step 4 says
//! the audio thread "never drops" a retired `Arc`, so a full return ring must give the value back
//! so the stage can hold it and retry. [`RingProducer::try_push`]'s `Err(T)` return is that
//! guarantee, restated in this crate's own vocabulary.
//!
//! **Alternatives rejected:** `crossbeam-queue`'s `ArrayQueue` — bounded and preallocated, but
//! MPMC and lock-free rather than SPSC and wait-free, so it would satisfy NFR-RT-020's letter less
//! well than the structure D-7.2 actually names, for a larger dependency. Amending D-5.3 to let
//! this one module carry `unsafe` — rejected because hand-written lock-free code is the highest-risk
//! thing in this milestone and D-5.3's confinement list is a decision this project made
//! deliberately; reopening it to avoid a zero-dependency, audio-domain-standard crate is a bad
//! trade. `std::sync::mpsc::sync_channel` — its receive path can deallocate, which is a P1
//! violation on the consumer side, and it is not wait-free.

use rtrb::{Consumer, Producer, PushError, RingBuffer};

/// The producing end of a wait-free SPSC ring.
///
/// Which thread owns this depends on the ring: the *worker* owns the command ring's producer
/// (D-7.2: "the worker is the sole producer"), while the *audio thread* owns the return ring's
/// producer (D-8.1 step 4). Both ends are `Send` and neither is `Sync`, which is exactly the SPSC
/// contract — one producer, one consumer, each pinned to its own thread.
pub struct RingProducer<T> {
    inner: Producer<T>,
}

/// The consuming end of a wait-free SPSC ring. See [`RingProducer`] for which thread owns which
/// end of which ring.
pub struct RingConsumer<T> {
    inner: Consumer<T>,
}

/// Allocates a ring holding at least `capacity` elements and splits it into its two ends.
///
/// **Not RT-safe** — this is the one allocation, made once at preparation time (D-6.1/D-7.2:
/// "pre-allocated at preparation"). Everything either end does afterwards is allocation-free.
pub fn ring<T>(capacity: usize) -> (RingProducer<T>, RingConsumer<T>) {
    let (producer, consumer) = RingBuffer::<T>::new(capacity);
    (
        RingProducer { inner: producer },
        RingConsumer { inner: consumer },
    )
}

impl<T> RingProducer<T> {
    /// Pushes `value`, returning it back as `Err(value)` if the ring is full.
    ///
    /// Wait-free and allocation-free. **Never drops `value`** — that is the whole point on the
    /// return-ring side, where a drop on the audio thread is the P1 violation D-8.1 step 4 exists
    /// to prevent. A caller that gets `Err` must keep holding the value and retry, not discard it.
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
        match self.inner.push(value) {
            Ok(()) => Ok(()),
            Err(PushError::Full(value)) => Err(value),
        }
    }

    /// Whether the consuming end has been dropped. D-8.1's degradation case: "If the worker dies,
    /// the ring fills and memory is retained but audio continues. Degradation, not failure (P8)."
    /// Callers use this to report the condition, never to change what the audio thread does.
    pub fn is_abandoned(&self) -> bool {
        self.inner.is_abandoned()
    }
}

impl<T> RingConsumer<T> {
    /// Pops the oldest element, or `None` if the ring is empty. Wait-free and allocation-free.
    pub fn try_pop(&mut self) -> Option<T> {
        self.inner.pop().ok()
    }

    /// Whether the producing end has been dropped. See [`RingProducer::is_abandoned`].
    pub fn is_abandoned(&self) -> bool {
        self.inner.is_abandoned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;

    /// D-7.2: the ring carries values across in order, first in first out.
    #[test]
    fn values_come_out_in_the_order_they_went_in() {
        let (mut tx, mut rx) = ring::<u32>(4);
        assert!(tx.try_push(1).is_ok());
        assert!(tx.try_push(2).is_ok());
        assert!(tx.try_push(3).is_ok());
        assert_eq!(rx.try_pop(), Some(1));
        assert_eq!(rx.try_pop(), Some(2));
        assert_eq!(rx.try_pop(), Some(3));
        assert_eq!(rx.try_pop(), None);
    }

    /// D-8.1 step 4's load-bearing property: a full ring hands the value *back* rather than
    /// dropping it, so the audio thread can retain a retired resource and retry. If this ever
    /// regressed to dropping, the P1 violation M4 exists to close would silently return.
    #[test]
    fn a_full_ring_returns_the_value_rather_than_dropping_it() {
        let (mut tx, mut rx) = ring::<u32>(2);
        assert!(tx.try_push(10).is_ok());
        assert!(tx.try_push(20).is_ok());
        assert_eq!(
            tx.try_push(30),
            Err(30),
            "a full ring must hand the value back"
        );
        // Draining one slot makes room, and the previously-rejected value still exists to retry.
        assert_eq!(rx.try_pop(), Some(10));
        assert!(tx.try_push(30).is_ok());
        assert_eq!(rx.try_pop(), Some(20));
        assert_eq!(rx.try_pop(), Some(30));
    }

    /// NFR-RT-010/NFR-RT-020: neither end allocates. This is the property that makes the ring
    /// legal on the audio thread at all, so it is asserted by the D-7.5 harness rather than by
    /// reading the dependency's source.
    #[test]
    fn neither_end_allocates() {
        let (mut tx, mut rx) = ring::<u64>(8);
        audio_section(|| {
            for i in 0..8u64 {
                assert!(tx.try_push(i).is_ok());
            }
            // Full: the rejecting path must not allocate either.
            assert_eq!(tx.try_push(99), Err(99));
            for i in 0..8u64 {
                assert_eq!(rx.try_pop(), Some(i));
            }
            assert_eq!(rx.try_pop(), None);
        });
    }

    /// The same, for an element type that owns a heap allocation and has a real destructor —
    /// which is what both rings actually carry. Moving an `Arc` through the ring must not
    /// allocate, and (critically) must not *drop* one on the consuming side while inside the
    /// audio section.
    #[test]
    fn moving_an_arc_through_the_ring_does_not_allocate() {
        use std::sync::Arc;
        let payload = Arc::new(vec![0u8; 1024]);
        let (mut tx, mut rx) = ring::<Arc<Vec<u8>>>(4);
        let popped = audio_section(|| {
            tx.try_push(Arc::clone(&payload)).unwrap();
            rx.try_pop().expect("pushed one, expected one back")
        });
        // Dropped here, outside the audio section, exactly as the worker would.
        assert!(Arc::ptr_eq(&popped, &payload));
        drop(popped);
        assert_eq!(Arc::strong_count(&payload), 1);
    }

    /// D-8.1's "if the worker dies" case (P8: degradation, not failure) is observable rather than
    /// silent, so a caller can report it.
    #[test]
    fn a_dropped_consumer_is_visible_to_the_producer() {
        let (tx, rx) = ring::<u32>(2);
        assert!(!tx.is_abandoned());
        drop(rx);
        assert!(tx.is_abandoned());
    }

    /// Both ends must be `Send` so the two threads can own one each, and neither may be `Sync`
    /// (that would permit a second producer or consumer, breaking the SPSC contract D-7.2 names).
    #[test]
    fn ends_are_send_but_not_sync() {
        fn assert_send<T: Send>() {}
        assert_send::<RingProducer<u32>>();
        assert_send::<RingConsumer<u32>>();
        // Not asserting `!Sync` mechanically — negative trait bounds are not stable — but the
        // absence of `unsafe impl Sync` in `rtrb` is what this test's name is claiming; see this
        // module's doc comment.
    }
}
