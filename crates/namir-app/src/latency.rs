//! FR-IO-050: "The application shall display the measured round-trip latency, or the
//! driver-reported latency where measurement is not possible, in both samples and milliseconds."
//!
//! **What this crate actually provides, stated plainly rather than overclaimed:** a true
//! *measured* round trip needs a loopback signal — play a known impulse out and time its arrival
//! back on the input — which needs a physical (or virtual) cable connecting an output to an input
//! and is inherently a real-hardware procedure; see
//! `docs/manual-tests/fr-io-050-latency-measurement.md`. What this module computes instead is the
//! **buffer-based estimate** FR-IO-050's own second clause anticipates ("driver-reported latency
//! where measurement is not possible"): one input buffer's worth of samples plus one output
//! buffer's worth, the minimum round trip the configured buffer sizes imply, before any
//! OS/driver-internal buffering `cpal` does not expose a portable way to query. [`LatencyReport`]
//! says which kind of figure it is holding, so a caller/UI never confuses the two.

/// One latency figure, tagged with which of FR-IO-050's two clauses produced it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencyReport {
    /// Round-trip latency in samples (input buffer frames + output buffer frames).
    pub samples: u32,
    /// The same figure in milliseconds, at the configured sample rate.
    pub milliseconds: f64,
    /// Whether this is a true measured figure (always `false` today — see this module's doc
    /// comment) or the buffer-based estimate.
    pub measured: bool,
}

/// Computes the buffer-based estimate: `input_buffer_frames + output_buffer_frames`, converted to
/// milliseconds at `sample_rate_hz`. Returns `None` if `sample_rate_hz` is zero (nothing
/// meaningful to report — the caller has a configuration error to surface separately, not a
/// latency figure).
pub fn estimate_round_trip(
    input_buffer_frames: u32,
    output_buffer_frames: u32,
    sample_rate_hz: u32,
) -> Option<LatencyReport> {
    if sample_rate_hz == 0 {
        return None;
    }
    let samples = input_buffer_frames.saturating_add(output_buffer_frames);
    let milliseconds = samples as f64 * 1000.0 / sample_rate_hz as f64;
    Some(LatencyReport {
        samples,
        milliseconds,
        measured: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-IO-050's literal arithmetic: at 48 kHz with 128-frame buffers on each side, the round
    /// trip is 256 samples, which is 256/48000 s = 5.333... ms.
    #[test]
    fn computes_samples_and_milliseconds_at_48khz() {
        let report = estimate_round_trip(128, 128, 48_000).unwrap();
        assert_eq!(report.samples, 256);
        assert!((report.milliseconds - 5.3333).abs() < 1e-3);
        assert!(!report.measured);
    }

    #[test]
    fn asymmetric_input_and_output_buffers_sum() {
        let report = estimate_round_trip(64, 256, 48_000).unwrap();
        assert_eq!(report.samples, 320);
    }

    #[test]
    fn zero_sample_rate_yields_no_report() {
        assert!(estimate_round_trip(128, 128, 0).is_none());
    }

    /// A degenerate but not impossible case (both sides report a zero buffer): the arithmetic
    /// still produces a defined, non-panicking answer of zero rather than dividing incorrectly.
    #[test]
    fn zero_buffers_yield_zero_latency() {
        let report = estimate_round_trip(0, 0, 48_000).unwrap();
        assert_eq!(report.samples, 0);
        assert_eq!(report.milliseconds, 0.0);
    }

    /// Never reports `measured: true` -- see this module's doc comment for why that would be a
    /// false claim without a real loopback measurement.
    #[test]
    fn never_claims_to_be_measured() {
        assert!(!estimate_round_trip(128, 128, 44_100).unwrap().measured);
    }
}
