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
//! buffer's worth plus the bridge prefill buffer's worth — the three buffers a rendered block
//! traverses, flowing from the input callback into the bridge ring (which `crate::stream`'s `open`
//! prefills with one block of silence, see there) and out the output callback — the minimum round
//! trip the configured buffer sizes imply, before any OS/driver-internal buffering `cpal` does not
//! expose a portable way to query. [`LatencyReport`]
//! says which kind of figure it is holding, so a caller/UI never confuses the two.

/// One latency figure, tagged with which of FR-IO-050's two clauses produced it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencyReport {
    /// Round-trip latency in samples (input buffer frames + output buffer frames + bridge prefill
    /// frames).
    pub samples: u32,
    /// The same figure in milliseconds, at the configured sample rate.
    pub milliseconds: f64,
    /// Whether this is a true measured figure (always `false` today — see this module's doc
    /// comment) or the buffer-based estimate.
    pub measured: bool,
    /// Whether [`Self::samples`] includes the output stream's own buffer. `false` when the output
    /// buffer is the device's own choice rather than a size Namir requested (issue #166 —
    /// `audio_io::output_buffer_request`), which `cpal` gives no portable way to query. The figure
    /// is then a **lower bound**: everything Namir itself buffers, and nothing for the device.
    ///
    /// Reported rather than papered over with the requested size, which would be a number that
    /// reads as authoritative and is not: under a shared-mode `Default` request the device chose
    /// 144 frames where the old code would have reported 480.
    pub includes_output_buffer: bool,
}

/// Computes the buffer-based estimate: `input_buffer_frames + output_buffer_frames +
/// bridge_prefill_frames`, with `output_buffer_frames` `None` when the device chose its own buffer
/// (the sum then omits that term and [`LatencyReport::includes_output_buffer`] says so), converted to milliseconds at `sample_rate_hz`. Returns `None` if
/// `sample_rate_hz` is zero (nothing meaningful to report — the caller has a configuration error
/// to surface separately, not a latency figure).
pub fn estimate_round_trip(
    input_buffer_frames: u32,
    output_buffer_frames: Option<u32>,
    bridge_prefill_frames: u32,
    sample_rate_hz: u32,
) -> Option<LatencyReport> {
    if sample_rate_hz == 0 {
        return None;
    }
    let samples = input_buffer_frames
        .saturating_add(output_buffer_frames.unwrap_or(0))
        .saturating_add(bridge_prefill_frames);
    let milliseconds = samples as f64 * 1000.0 / sample_rate_hz as f64;
    Some(LatencyReport {
        samples,
        milliseconds,
        measured: false,
        includes_output_buffer: output_buffer_frames.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #166: a shared-mode output stream takes the device's own buffer, which `cpal` gives
    /// no portable way to query. The estimate then omits that term and says so, rather than
    /// substituting the size Namir happened to request — which is not the size in use.
    #[test]
    fn a_device_chosen_output_buffer_is_excluded_and_declared() {
        let report = estimate_round_trip(480, None, 480, 48_000).unwrap();
        assert_eq!(
            report.samples, 960,
            "only the two buffers Namir itself sizes"
        );
        assert!(
            !report.includes_output_buffer,
            "the report must not claim to cover a buffer it never saw"
        );

        let known = estimate_round_trip(480, Some(480), 480, 48_000).unwrap();
        assert!(known.includes_output_buffer);
        assert!(
            known.samples > report.samples,
            "the known-output figure is the larger one, so the unknown case is a lower bound"
        );
    }

    /// FR-IO-050's literal arithmetic: at 48 kHz with 128-frame buffers on each side and a
    /// 128-frame bridge prefill block, the round trip is 3 × 128 = 384 samples, which is
    /// 384/48000 s = 8.0 ms.
    #[test]
    fn computes_samples_and_milliseconds_at_48khz() {
        let report = estimate_round_trip(128, Some(128), 128, 48_000).unwrap();
        assert_eq!(report.samples, 384);
        assert!((report.milliseconds - 8.0).abs() < 1e-3);
        assert!(!report.measured);
        assert!(report.includes_output_buffer);
    }

    #[test]
    fn asymmetric_input_and_output_buffers_sum() {
        let report = estimate_round_trip(64, Some(256), 32, 48_000).unwrap();
        assert_eq!(report.samples, 352);
    }

    #[test]
    fn zero_sample_rate_yields_no_report() {
        assert!(estimate_round_trip(128, Some(128), 128, 0).is_none());
    }

    /// A degenerate but not impossible case (all three sides report a zero buffer): the
    /// arithmetic still produces a defined, non-panicking answer of zero rather than dividing
    /// incorrectly.
    #[test]
    fn zero_buffers_yield_zero_latency() {
        let report = estimate_round_trip(0, Some(0), 0, 48_000).unwrap();
        assert_eq!(report.samples, 0);
        assert_eq!(report.milliseconds, 0.0);
    }

    /// The bridge prefill block is the third term of the estimate (`crate::stream`'s `open`
    /// prefills the ring with one `max_block_size` block of silence); it must add to the sum, not
    /// be ignored.
    #[test]
    fn bridge_prefill_block_adds_to_the_sum() {
        let without = estimate_round_trip(128, Some(128), 0, 48_000).unwrap();
        let with = estimate_round_trip(128, Some(128), 256, 48_000).unwrap();
        assert_eq!(without.samples, 256);
        assert_eq!(with.samples, 512);
    }

    /// Never reports `measured: true` -- see this module's doc comment for why that would be a
    /// false claim without a real loopback measurement.
    #[test]
    fn never_claims_to_be_measured() {
        assert!(
            !estimate_round_trip(128, Some(128), 128, 44_100)
                .unwrap()
                .measured
        );
    }
}
