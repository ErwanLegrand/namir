//! FR-IO-060: "The application shall detect and report audio dropouts (xruns), showing a running
//! count for the session, resettable by the user."
//!
//! Two independent sources feed one counter: `cpal`'s own `ErrorKind::Xrun` (not every backend
//! reports it — WASAPI notably does not surface a dedicated xrun signal through `cpal`'s error
//! callback the way JACK does) and this crate's own [`crate::bridge`] ring underrun, detected
//! whenever the output callback needs more frames than the input side has produced. The two are
//! not double-counted against each other in the sense of correcting one from the other — they are
//! genuinely different events (a backend-reported glitch vs. this crate's own buffer running dry)
//! — so both simply increment the same session total, which is what FR-IO-060 asks for ("a
//! running count for the session").

use std::sync::atomic::{AtomicU64, Ordering};

/// A session's xrun count. `Send + Sync` (plain atomics) so the audio callback thread(s) and the
/// UI thread share one counter without a lock — incrementing must be usable from an audio
/// callback (NFR-RT-010/020: no blocking, no allocation), and `AtomicU64::fetch_add` is exactly
/// that.
#[derive(Default)]
pub struct XrunCounter {
    count: AtomicU64,
}

impl XrunCounter {
    /// A fresh counter at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one xrun. RT-safe: a single relaxed atomic increment, callable from an audio
    /// callback.
    pub fn record(&self) {
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// The running total for this session.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// FR-IO-060's "resettable by the user".
    pub fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn starts_at_zero() {
        assert_eq!(XrunCounter::new().count(), 0);
    }

    #[test]
    fn record_increments_by_one() {
        let counter = XrunCounter::new();
        counter.record();
        counter.record();
        assert_eq!(counter.count(), 2);
    }

    #[test]
    fn reset_returns_to_zero() {
        let counter = XrunCounter::new();
        counter.record();
        counter.record();
        counter.reset();
        assert_eq!(counter.count(), 0);
    }

    /// FR-IO-060's "resettable by the user" must not stop future xruns from counting again.
    #[test]
    fn counting_resumes_after_a_reset() {
        let counter = XrunCounter::new();
        counter.record();
        counter.reset();
        counter.record();
        assert_eq!(counter.count(), 1);
    }

    /// The counter is meant to be shared between an audio callback thread and the UI thread —
    /// pin that concurrent-increment property directly rather than trusting `AtomicU64` by
    /// reputation alone.
    #[test]
    fn concurrent_records_from_multiple_threads_are_not_lost() {
        let counter = Arc::new(XrunCounter::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let counter = Arc::clone(&counter);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        counter.record();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(counter.count(), 800);
    }
}
