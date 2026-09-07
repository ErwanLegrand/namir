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

use std::sync::OnceLock;
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

/// **Temporary diagnostic, `NAMIR_CALLBACK_STATS=1`.** Splits "the output callback is too slow"
/// from "the input side never produced the frames" — the two hypotheses a bare xrun count cannot
/// tell apart, since `XrunCounter::record` fires once per padded pull regardless of cause or of
/// whether one frame or a whole block was padded.
///
/// RT-safe by the same argument as [`XrunCounter`]: relaxed atomics only, no allocation, no
/// formatting, no logger named in the callback's own module.
#[derive(Default)]
pub struct CallbackStats {
    calls: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    over_budget: AtomicU64,
    padded_frames: AtomicU64,
    pulled_frames: AtomicU64,
    input_calls: AtomicU64,
    input_frames: AtomicU64,
    input_dropped: AtomicU64,
    backend_xruns: AtomicU64,
    out_max_gap_ns: AtomicU64,
    out_late_wakeups: AtomicU64,
    in_max_gap_ns: AtomicU64,
    in_late_wakeups: AtomicU64,
    out_gap_sum_ns: AtomicU64,
    out_gap_count: AtomicU64,
    in_gap_sum_ns: AtomicU64,
    in_gap_count: AtomicU64,
}

impl CallbackStats {
    /// A fresh, zeroed set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one output callback: how long the whole callback took, how many frames it pulled,
    /// and how many of those it had to pad with silence. `budget_ns` is the callback's own period.
    pub fn record(&self, elapsed_ns: u64, budget_ns: u64, pulled: u64, padded: u64) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.total_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        self.max_ns.fetch_max(elapsed_ns, Ordering::Relaxed);
        if elapsed_ns > budget_ns {
            self.over_budget.fetch_add(1, Ordering::Relaxed);
        }
        self.pulled_frames.fetch_add(pulled, Ordering::Relaxed);
        self.padded_frames.fetch_add(padded, Ordering::Relaxed);
    }

    /// Records one input callback chunk: frames offered to the ring, and frames the ring refused
    /// because it was full (`BridgeProducer::push_captured`'s return — FR-IO-060's *other*
    /// dropout).
    pub fn record_input(&self, offered: u64, dropped: u64) {
        self.input_calls.fetch_add(1, Ordering::Relaxed);
        self.input_frames.fetch_add(offered, Ordering::Relaxed);
        self.input_dropped.fetch_add(dropped, Ordering::Relaxed);
    }

    /// Records the wall-clock gap since the previous callback on one side. `late` counts gaps
    /// beyond 1.5 periods — a missed wakeup, which is invisible to a per-call duration.
    pub fn record_gap(&self, output_side: bool, gap_ns: u64, budget_ns: u64) {
        let (max, late) = if output_side {
            (&self.out_max_gap_ns, &self.out_late_wakeups)
        } else {
            (&self.in_max_gap_ns, &self.in_late_wakeups)
        };
        let (sum, count) = if output_side {
            (&self.out_gap_sum_ns, &self.out_gap_count)
        } else {
            (&self.in_gap_sum_ns, &self.in_gap_count)
        };
        sum.fetch_add(gap_ns, Ordering::Relaxed);
        count.fetch_add(1, Ordering::Relaxed);
        max.fetch_max(gap_ns, Ordering::Relaxed);
        if gap_ns * 2 > budget_ns * 3 {
            late.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `(out_max_gap_ns, out_late, in_max_gap_ns, in_late)`.
    pub fn read_gaps(&self) -> (u64, u64, u64, u64) {
        (
            self.out_max_gap_ns.load(Ordering::Relaxed),
            self.out_late_wakeups.load(Ordering::Relaxed),
            self.in_max_gap_ns.load(Ordering::Relaxed),
            self.in_late_wakeups.load(Ordering::Relaxed),
        )
    }

    /// Mean inter-callback period per side, in ns — `(out, in)`. Since the last [`Self::take_means`]
    /// call, so a stretch that begins mid-session is not diluted by the healthy start.
    pub fn take_means(&self) -> (u64, u64) {
        let out_sum = self.out_gap_sum_ns.swap(0, Ordering::Relaxed);
        let out_n = self.out_gap_count.swap(0, Ordering::Relaxed);
        let in_sum = self.in_gap_sum_ns.swap(0, Ordering::Relaxed);
        let in_n = self.in_gap_count.swap(0, Ordering::Relaxed);
        (
            out_sum.checked_div(out_n).unwrap_or(0),
            in_sum.checked_div(in_n).unwrap_or(0),
        )
    }

    /// Records one `StreamFailure::Xrun` reported by the backend itself.
    pub fn record_backend_xrun(&self) {
        self.backend_xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// `(input_calls, input_frames, input_dropped, backend_xruns)`.
    pub fn read_input(&self) -> (u64, u64, u64, u64) {
        (
            self.input_calls.load(Ordering::Relaxed),
            self.input_frames.load(Ordering::Relaxed),
            self.input_dropped.load(Ordering::Relaxed),
            self.backend_xruns.load(Ordering::Relaxed),
        )
    }

    /// `(calls, mean_ns, max_ns, over_budget, pulled_frames, padded_frames)`.
    pub fn read(&self) -> (u64, u64, u64, u64, u64, u64) {
        let calls = self.calls.load(Ordering::Relaxed);
        let total = self.total_ns.load(Ordering::Relaxed);
        (
            calls,
            total.checked_div(calls).unwrap_or(0),
            self.max_ns.load(Ordering::Relaxed),
            self.over_budget.load(Ordering::Relaxed),
            self.pulled_frames.load(Ordering::Relaxed),
            self.padded_frames.load(Ordering::Relaxed),
        )
    }

    /// Whether the diagnostic is switched on. Read once at stream setup, never in a callback.
    pub fn enabled() -> bool {
        std::env::var("NAMIR_CALLBACK_STATS").is_ok_and(|v| v != "0")
    }

    /// The one process-wide set. A global rather than another `Arc` threaded through
    /// `stream::open`'s signature and its dozen test call sites, deliberately: this is a
    /// diagnostic that exists to answer one question and then leave, and a signature change would
    /// outlive it.
    pub fn global() -> &'static Self {
        static STATS: OnceLock<CallbackStats> = OnceLock::new();
        STATS.get_or_init(CallbackStats::new)
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
