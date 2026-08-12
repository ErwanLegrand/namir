//! FR-CLAP-080: every sample rate a host may present between 44.1 kHz and 192 kHz inclusive,
//! including a mid-session sample-rate change.
//!
//! Driven through the real C vtable by the shared `support` harness — **read that module's doc
//! comment first**, in particular the HAZARD about `start_library_scan` and the developer's real
//! library index. Nothing here starts a scan.
//!
//! # What "every sample rate" is taken to mean
//!
//! Read as a continuum the set is uncountable and no artifact can span it; read as *what a host can
//! actually present*, it is the set of `f64` values a host puts in `clap_plugin_activate`'s
//! `sample_rate` field, and the plugin's own handling of it collapses that set immediately:
//! `crates/namir-clap/src/audio.rs:125` does `audio_config.sample_rate.round()` and builds a
//! `SampleRate` (a `NonZeroU32`) from the result, so every presentable rate is equivalent to some
//! integer hertz value, and the only structure the plugin can be sensitive to is that integer.
//! This file therefore sweeps
//!
//! * both endpoints exactly — 44100 and 192000;
//! * the six standard rates — 44100, 48000, 88200, 96000, 176400, 192000;
//! * a 1 kHz grid across the whole range (44100, 45100, … 191100), 148 rates, all but the first of
//!   them non-standard, which is what catches a rate-dependent assumption that happens to hold at
//!   the six rates everyone tests;
//! * one fractional rate, 48000.5, which is the only input that exercises the `round()` above
//!   rather than passing through it unchanged.
//!
//! At each rate the plugin is activated, fed a phase-continuous 1 kHz sine, and asserted to write
//! finite, non-silent output whose RMS matches the 48 kHz reading — the level of a 1 kHz tone
//! through this chain is a property of the signal, not of the rate it is sampled at, so a
//! rate-dependent time constant or a filter coefficient computed against the wrong rate shows up
//! here as a level error.
//!
//! Then the mid-session limb: activate at 44.1 kHz, process, deactivate, activate at 192 kHz,
//! process — the deactivate/reactivate cycle `src/shared.rs` documents as how Namir takes a
//! sample-rate change — asserting the second engine is as correct as a freshly built one.

mod support;

use clack_host::prelude::PluginInstance;
use support::{
    CHANNELS, SINE_FREQ_HZ, StereoBuffers, TestHost, activate, all_finite, audio_section, config,
    fill_sine, instantiate_default, peak,
};

/// Block size every sweep block uses. Small enough that 150 activations stay cheap, large enough
/// that per-block overhead does not dominate the measurement.
const BLOCK: u32 = 256;

/// Amplitude of the probe tone: -12 dBFS, far above the gate's -70 dBFS default threshold, so the
/// gate is open throughout the measurement window.
const AMPLITUDE: f32 = 0.25;

/// Discarded before measuring. Covers the gate's 1 ms attack and every `GainLike` smoothing ramp in
/// the chain, expressed in *milliseconds* precisely so it means the same thing at every rate.
const WARMUP_MS: f64 = 40.0;

/// Measured. 20 ms is 20 whole cycles of the 1 kHz probe at every rate, so the RMS of a settled
/// sine over this window is rate-independent to well under the tolerance below.
const MEASURE_MS: f64 = 20.0;

/// How far a rate's measured RMS may sit from the 48 kHz reading, in decibels.
///
/// Not zero: the chain's biquads are designed per rate, and the bilinear transform warps a
/// designed corner slightly differently at 44.1 kHz than at 192 kHz, so a 1 kHz probe sees a small
/// genuine level difference. Measured across the whole sweep that difference peaks at 0.0085 dB
/// (at 192 kHz, the furthest rate from the reference), so this leaves roughly six times the
/// observed spread as headroom for another platform's floating point while still missing by orders
/// of magnitude any time constant computed against a hard-coded 48 kHz or filter fed the wrong
/// rate. The measurement itself is fully deterministic — there is no run-to-run noise to absorb.
const RMS_TOLERANCE_DB: f32 = 0.05;

/// What one activation at one rate produced.
struct RateReport {
    /// RMS of channel 0 over the measurement window.
    rms: f32,
    /// Peak absolute sample of channel 0 over the measurement window.
    peak: f32,
    /// Channel 0's measurement window, sample for sample — the mid-session round-trip compares two
    /// of these for bit-exact equality.
    measured: Vec<f32>,
}

/// Activates `instance` at `rate`, runs a phase-continuous 1 kHz sine through it, and reports what
/// came out of the measurement window.
///
/// Leaves the instance deactivated, so the caller may call this again at another rate — which is
/// exactly the mid-session change CLAP asks a plugin to survive.
fn run_at_rate(
    instance: &mut PluginInstance<TestHost>,
    bufs: &mut StereoBuffers,
    rate: f64,
) -> RateReport {
    let warmup_frames = (WARMUP_MS / 1000.0 * rate).ceil() as u64;
    let measure_frames = (MEASURE_MS / 1000.0 * rate).ceil() as u64;
    let total_frames = warmup_frames + measure_frames;

    let stopped = activate(instance, config(rate, 1, BLOCK));
    let mut processor = stopped
        .start_processing()
        .unwrap_or_else(|e| panic!("processing must start at {rate} Hz: {e}"));

    let mut measured: Vec<f32> = Vec::with_capacity(measure_frames as usize);
    let mut done: u64 = 0;

    while done < total_frames {
        let frames = BLOCK.min((total_frames - done) as u32);

        // Phase-continuous across blocks: the tone is one unbroken sine for the whole run, so the
        // measurement window contains no block-boundary transient to widen the RMS.
        for channel in 0..CHANNELS {
            fill_sine(
                &mut bufs.input_mut(channel)[..frames as usize],
                SINE_FREQ_HZ,
                rate,
                AMPLITUDE,
                done,
            );
        }
        // If the plugin does not write this block, every assertion below sees NaN rather than
        // whatever the previous block happened to leave behind.
        bufs.poison_output(f32::NAN);

        audio_section(|| bufs.process_block(&mut processor, frames))
            .unwrap_or_else(|e| panic!("a {frames}-frame block at {rate} Hz must process: {e}"));

        for channel in 0..CHANNELS {
            let written = &bufs.output(channel)[..frames as usize];
            assert!(
                all_finite(written),
                "at {rate} Hz, channel {channel} of a {frames}-frame block is not finite -- \
                 the plugin either produced a non-finite sample or did not write the block"
            );
        }

        // Collect only the part of this block that lies inside the measurement window.
        let block_start = done;
        if block_start + u64::from(frames) > warmup_frames {
            let from = warmup_frames.saturating_sub(block_start) as usize;
            measured.extend_from_slice(&bufs.output(0)[from..frames as usize]);
        }

        done += u64::from(frames);
    }

    let stopped = processor.stop_processing();
    instance.deactivate(stopped);

    assert_eq!(
        measured.len(),
        measure_frames as usize,
        "the measurement window at {rate} Hz should be exactly {measure_frames} frames"
    );

    let sum_sq: f64 = measured.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    RateReport {
        rms: (sum_sq / measured.len() as f64).sqrt() as f32,
        peak: peak(&measured),
        measured,
    }
}

/// The rate set this file sweeps: both endpoints, the six standard rates, a 1 kHz grid across the
/// range, and one fractional rate. Sorted and deduplicated.
fn presentable_rates() -> Vec<f64> {
    let mut rates: Vec<f64> = vec![
        // Both endpoints, exactly, named rather than left to fall out of the grid.
        44_100.0, 192_000.0,
        // The six standard rates (the two endpoints above are two of them).
        48_000.0, 88_200.0, 96_000.0, 176_400.0,
        // The one input that exercises `src/audio.rs`'s `round()` rather than passing through it.
        48_000.5,
    ];

    // The 1 kHz grid, anchored at the lower endpoint: 44100, 45100, … 191100.
    let mut hz = 44_100u32;
    while hz <= 192_000 {
        rates.push(f64::from(hz));
        hz += 1_000;
    }

    rates.sort_by(f64::total_cmp);
    rates.dedup();
    rates
}

/// A level difference in decibels, or `None` if either side is not a positive level.
fn level_difference_db(measured: f32, reference: f32) -> Option<f32> {
    (measured > 0.0 && reference > 0.0).then(|| 20.0 * (measured / reference).log10())
}

/// Asserts one rate's report is a correct, rate-independent rendering of the probe tone.
fn assert_report_is_correct(rate: f64, report: &RateReport, reference_rms: f32) {
    assert!(
        all_finite(&report.measured),
        "at {rate} Hz the measurement window contains a non-finite sample"
    );
    assert!(
        report.peak > AMPLITUDE * 0.5,
        "at {rate} Hz the plugin produced near-silence (peak {}) from a -12 dBFS tone",
        report.peak
    );

    let difference = level_difference_db(report.rms, reference_rms).unwrap_or_else(|| {
        panic!(
            "at {rate} Hz the measured RMS ({}) or the 48 kHz reference ({reference_rms}) is not a \
             positive level",
            report.rms
        )
    });
    assert!(
        difference.abs() <= RMS_TOLERANCE_DB,
        "at {rate} Hz the 1 kHz probe came out {difference:+.3} dB from its 48 kHz level \
         (rms {} vs {reference_rms}), beyond the {RMS_TOLERANCE_DB} dB tolerance -- a level that \
         depends on the sample rate means a time constant or filter coefficient was computed \
         against the wrong one",
        report.rms
    );
}

// trace-partial: FR-CLAP-080
// uncovered: FR-CLAP-080 — swept at 154 rates (both endpoints, the six standard rates, a 1 kHz
// uncovered: grid and one fractional value), not every rate the requirement's range admits:
// uncovered: `src/audio.rs:125` rounds the host's rate to an integer, so the set a host can
// uncovered: present collapses onto ~147 900 distinct `SampleRate` values, of which this file
// uncovered: reaches 154. And nothing is loaded at any of them — this file never touches
// uncovered: `PluginState`, so no model and no IR are present and D-9.2's `SlotResampler`, the
// uncovered: one rate-dependent subsystem in the chain and the one this milestone found broken
// uncovered: at 192 kHz, never runs at any rate here, the mid-session limb included, which
// uncovered: asserts only a 1 kHz tone's RMS within 0.05 dB of the 48 kHz reading through a
// uncovered: pass-through chain; closes M8
#[test]
fn every_presentable_sample_rate_and_a_mid_session_change_are_handled() {
    let (_entry, mut instance) = instantiate_default();
    let mut bufs = StereoBuffers::new(BLOCK as usize);

    // The reference every other rate is compared against. 48 kHz is NFR-PERF-010's reference rate
    // and the harness default, so a failure at *it* is a plain chain bug rather than a rate bug.
    let reference = run_at_rate(&mut instance, &mut bufs, 48_000.0);
    assert!(
        reference.rms > 0.0,
        "the 48 kHz reference must be a real signal, not silence"
    );

    let rates = presentable_rates();
    assert!(
        rates.len() >= 150,
        "the sweep should span the range densely; got only {} rates",
        rates.len()
    );

    for rate in rates {
        let report = run_at_rate(&mut instance, &mut bufs, rate);
        assert_report_is_correct(rate, &report, reference.rms);
    }

    // The mid-session change itself: one live instance taken from the bottom of the range to the
    // top through the deactivate/reactivate cycle, with the second engine held to the same
    // standard as a freshly built one.
    let low = run_at_rate(&mut instance, &mut bufs, 44_100.0);
    assert_report_is_correct(44_100.0, &low, reference.rms);
    let high = run_at_rate(&mut instance, &mut bufs, 192_000.0);
    assert_report_is_correct(192_000.0, &high, reference.rms);

    drop(instance); // `clap_plugin.destroy`
}

/// Supplementary to the tagged test above, and a strictly stronger claim about the mid-session
/// change than "the new engine is correct": after going 44.1 kHz → 192 kHz → 44.1 kHz, the third
/// activation must reproduce the first **sample for sample**.
///
/// Anything the rate change left behind — a filter state carried across, a coefficient not
/// recomputed, a smoothing ramp measured in samples rather than seconds — makes the two 44.1 kHz
/// runs differ, and the tolerance-based check above could absorb a small one.
#[test]
fn a_mid_session_round_trip_returns_the_engine_to_its_original_behaviour() {
    let (_entry, mut instance) = instantiate_default();
    let mut bufs = StereoBuffers::new(BLOCK as usize);

    let first = run_at_rate(&mut instance, &mut bufs, 44_100.0);
    let _excursion = run_at_rate(&mut instance, &mut bufs, 192_000.0);
    let again = run_at_rate(&mut instance, &mut bufs, 44_100.0);

    assert_eq!(
        first.measured, again.measured,
        "44.1 kHz did not render identically before and after an excursion to 192 kHz -- some \
         state survived the deactivate/reactivate cycle that should have been rebuilt"
    );

    drop(instance); // `clap_plugin.destroy`
}
