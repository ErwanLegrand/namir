//! FR-IO-060: running xrun count for the session, resettable by the user.

use std::sync::atomic::{AtomicU64, Ordering};

/// A session's xrun count.
#[derive(Default, Debug)]
pub struct XrunCounter(AtomicU64);

impl XrunCounter {
    /// A fresh counter at zero.
    pub fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Records one xrun.
    #[inline]
    pub fn record(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// The running total for this session.
    #[inline]
    pub fn count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Resets the counter to zero.
    #[inline]
    pub fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrun_counter_increments_and_resets() {
        let counter = XrunCounter::new();
        assert_eq!(counter.count(), 0);
        counter.record();
        counter.record();
        assert_eq!(counter.count(), 2);
        counter.reset();
        assert_eq!(counter.count(), 0);
        counter.record();
        assert_eq!(counter.count(), 1);
    }
}
