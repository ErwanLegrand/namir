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
// uncovered: `src/audio.rs` rounds the host's rate to an integer, so the set a host can present
// uncovered: collapses onto ~147 900 distinct `SampleRate` values, of which this file reaches
// uncovered: 154. The resource-loaded limb `loaded` adds — where D-9.2's `SlotResampler`, the one
// uncovered: rate-dependent subsystem in the chain, is actually engaged — is narrower still: 8 of
// uncovered: those 154, the two endpoints, the six standard rates and two off-grid values, since
// uncovered: a rate there costs a model load and real inference rather than a pass-through
// uncovered: block; closes M8
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

/// The limb of FR-CLAP-080 that reaches the chain's one rate-dependent subsystem: the same sweep,
/// with a real `.nam` model loaded, so D-9.2's `SlotResampler` is constructed and run at every rate
/// rather than left as `None`.
///
/// # Why this exists as its own module (M14)
///
/// The sweep above loads nothing at any of its 154 rates. Every stage in a resource-free chain is
/// either rate-independent or a biquad whose coefficients the existing RMS check does exercise —
/// but `NamStage`'s resampler, the piece that only exists when a model's declared rate differs from
/// the engine's, and **the one M9b found broken at 192 kHz**, was reached by nothing in this file.
/// A rate sweep that cannot see the rate-dependent code is the shape of gap M9b's own close-out
/// warns about.
///
/// # The model's declared rate is chosen per session rate, deliberately
///
/// `NamSlot::resample` is `None` when the two rates match, which is exactly the configuration this
/// module is not interested in. So the model is re-declared at 44.1 kHz for every session rate
/// except 44.1 kHz itself, where it is re-declared at 48 kHz — the resampler is therefore engaged
/// at *every* swept rate, and the plugin's own reported latency, which is non-zero only when it is,
/// is a sound install detector at all of them. `namir-fixtures` always stamps 48 kHz on a generated
/// fixture; overwriting the one field is what turns "a model is loaded" into "the resampler is
/// running", the same device `clap_host_block_sizes.rs`'s loaded limb uses.
///
/// # Why the rate set is 8 and not 154
///
/// Each rate here costs an activation, a document load, a handover and real Nano inference; the
/// resource-free sweep costs a pass-through block. Both endpoints, all six standard rates and two
/// off-grid values is what buys the most structure per second — and the `uncovered:` field on the
/// sweep above says so rather than implying this limb spans the range.
#[cfg(feature = "host-ext-tests")]
mod loaded {
    use std::time::Duration;

    use clack_extensions::latency::PluginLatency;
    use clack_extensions::state::PluginState;
    use clack_host::prelude::{PluginInstance, StartedPluginAudioProcessor};
    use namir_core::ContentHash;
    use namir_fixtures::nam::{WaveNetShape, generate};
    use namir_state::{Document, EmbeddedRef, FileRef, State};

    use super::support::{
        CHANNELS, SINE_FREQ_HZ, StereoBuffers, TestHost, activate, all_finite, audio_section,
        config, fill_sine, instantiate_default, main_thread_handle, peak, require_plugin_extension,
    };
    use super::{AMPLITUDE, BLOCK, level_difference_db};

    /// Seeds the generated model. Fixed, so a failure reproduces exactly (D-19.1).
    const MODEL_SEED: u64 = 0x0C1A_9080_4A11_0000;

    /// The rate the model declares at every session rate but one. See this module's doc comment.
    const MODEL_RATE_HZ: u32 = 44_100;

    /// The rate it declares when the session is already at [`MODEL_RATE_HZ`].
    const ALTERNATE_MODEL_RATE_HZ: u32 = 48_000;

    /// Discarded before measuring. Long enough to cover the handover crossfade's tail, the
    /// resampler's FIFO fill and every `GainLike` ramp; expressed in milliseconds so it means the
    /// same thing at 44.1 kHz and at 192 kHz.
    const WARMUP_MS: f64 = 40.0;

    /// Measured. Ten whole cycles of the 1 kHz probe at every rate.
    const MEASURE_MS: f64 = 10.0;

    /// How far a rate's measured RMS may sit from the 48 kHz reading, in decibels.
    ///
    /// Far looser than the resource-free sweep's 0.05 dB, and for a real reason rather than
    /// caution: the signal the model sees has been resampled to its own declared rate by a
    /// polyphase filter whose passband ripple and transition band genuinely differ between a
    /// 192→44.1 kHz ratio and a 48→44.1 kHz one, and the model is a *nonlinearity*, so a small
    /// input-level difference does not come out as the same small output-level difference. What
    /// the bound has to be tight enough to catch is a coefficient or time constant computed
    /// against the wrong rate, which moves the level by whole decibels or produces silence.
    ///
    /// **Measured rather than guessed**: across this rate set the spread peaks at **0.52 dB**, at
    /// 45.1 kHz — the rate whose ratio to the model's 44.1 kHz is closest to, but not, unity — with
    /// 44.1 kHz itself (where the model is re-declared at 48 kHz) at 0.11 dB and both endpoints
    /// well inside that. The bound is a little under three times the observed worst case. Bisected
    /// by running this test with the constant lowered until it failed, so the figure is this
    /// machine's own reading and not an inherited one.
    const RMS_TOLERANCE_DB: f32 = 1.5;

    /// Blocks pumped waiting for the handover to complete before the warm-up starts. At 512 frames
    /// and a 20 ms crossfade this is reached in two or three; the rest is headroom for a loaded
    /// machine. Exhausting it means the model never arrived, which the panic says.
    const LANDING_LIMIT: usize = 64;

    /// Slept between landing polls, so the worker actually gets to run on a small box.
    const LANDING_POLL: Duration = Duration::from_millis(10);

    /// Audio time a run of steady blocks must span before the chain counts as settled, in
    /// milliseconds. **Rate-independent by construction, and that is the point.** A crossfade
    /// changes the level monotonically across its own 20 ms, so at 192 kHz it spans some fifteen
    /// 256-frame blocks and consecutive blocks inside it differ by only a few percent -- a
    /// block-against-previous-block test would call that steady. A run required to span three
    /// crossfades' worth of audio cannot sit inside one, whatever the rate, because it necessarily
    /// contains the whole excursion.
    const SETTLE_SPAN_MS: f64 = 60.0;

    /// Ceiling on the settle gate, in blocks. Reaching it means the output never stopped moving,
    /// which the panic says.
    const SETTLE_LIMIT: usize = 512;

    /// How far the loudest and quietest block in a candidate run may differ, as a fraction, and
    /// still count as steady.
    ///
    /// **The gate reads each block's peak, not its RMS, and that choice is what makes this number
    /// meaningful.** A block is not a whole number of 1 kHz cycles at any of these rates, and at
    /// the top of [`RATES`] it is barely more than one -- 256 frames at 191 100 Hz is 1.34 cycles
    /// -- so a *settled* tone's per-block RMS still swings between 0.1453 and 0.1606, i.e. **10.5%**,
    /// purely from where the window happens to cut the waveform. That is a quarter of the +3 dB
    /// (41%) excursion this gate exists to catch, which leaves no honest threshold between them.
    /// Peak has no such term: every block at every rate here spans at least one full period of the
    /// chain's output, which is periodic at the probe frequency however hard the model distorts it,
    /// so a settled peak repeats to within the sampling grid's own ~0.01%. 5% is far above that and
    /// far below 41%.
    const SETTLE_TOLERANCE: f64 = 0.05;

    /// The rate set. Both endpoints, the six standard rates, and two values off every grid.
    const RATES: [f64; 8] = [
        44_100.0, 45_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 191_100.0, 192_000.0,
    ];

    /// A generated Nano WaveNet re-declared at `rate_hz`, as the `.nam` bytes a real one arrives
    /// as. Everything numeric about it — topology, weights, the RMS calibration that keeps its
    /// output neither silent nor exploding — is the generator's, untouched.
    fn model_bytes(rate_hz: u32) -> Vec<u8> {
        let mut model =
            generate(WaveNetShape::Nano, MODEL_SEED).expect("the WaveNet fixture must generate");
        model.sample_rate = rate_hz;
        model.to_json_bytes()
    }

    /// A document naming that model by FR-STATE-080's embedded form alone: no absolute path and no
    /// library-relative candidate, so nothing is read from or written to disk and the developer's
    /// library is neither consulted nor modified.
    fn document_bytes(rate_hz: u32) -> Vec<u8> {
        let data = model_bytes(rate_hz);
        let mut state = State::defaults();
        state.nam = Some(FileRef {
            hash: ContentHash::of(&data),
            library_relative: None,
            absolute: None,
            display_name: format!("fr-clap-080-{rate_hz}.nam"),
            embedded: Some(EmbeddedRef {
                media_type: "application/vnd.namir.nam+json".to_string(),
                data,
            }),
        });
        state.write_onto(&Document::empty()).to_pretty_bytes()
    }

    /// What one loaded activation at one rate produced.
    struct LoadedReport {
        /// RMS of channel 0 over the measurement window.
        rms: f32,
        /// Peak absolute sample of channel 0 over the measurement window.
        peak: f32,
        /// The latency the plugin reported once the model was installed and warm.
        latency: u32,
    }

    /// Pumps the probe tone until the chain's own output stops moving, so the measurement window
    /// that follows contains no handover.
    ///
    /// **Why the latency poll above is not enough (issue #145's finding 7).** Every activation
    /// after the first dispatches *two* `spawn_recall` jobs -- `crate::audio`'s activate-time
    /// replay and `state_ext`'s own at the end of `load` -- and the landing poll breaks on the
    /// first nonzero latency, which only proves that *one* of them finished. The second installs
    /// the same model at the same latency, so no latency reading can tell it apart from the first;
    /// the only thing that changes when it lands is the audio. On a slow runner it landed inside
    /// the warm-up or the measurement window and FR-NAM-070's equal-power crossfade averaged into
    /// the reading: -1.76 dB when it straddled the window's start (the macOS CI failure, RMS
    /// 0.12507376), and up to +3 dB when it sat wholly inside, since the two sides of that fade
    /// are the *same* model. No engine change can bring either inside [`RMS_TOLERANCE_DB`] --
    /// equal-power is what FR-NAM-070 mandates -- so the gate is what has to get stricter.
    ///
    /// Blocks are separated by a [`LANDING_POLL`] so the worker pool actually gets scheduled
    /// between them on a small box; the crossfade itself only advances as blocks are processed.
    fn settle(
        processor: &mut StartedPluginAudioProcessor<TestHost>,
        bufs: &mut StereoBuffers,
        rate: f64,
    ) {
        let span_blocks = ((SETTLE_SPAN_MS / 1000.0 * rate) / f64::from(BLOCK)).ceil() as usize;
        let mut run: Vec<f64> = Vec::with_capacity(span_blocks + 1);

        for index in 0..SETTLE_LIMIT {
            for channel in 0..CHANNELS {
                fill_sine(
                    &mut bufs.input_mut(channel)[..BLOCK as usize],
                    SINE_FREQ_HZ,
                    rate,
                    AMPLITUDE,
                    index as u64 * u64::from(BLOCK),
                );
            }
            audio_section(|| bufs.process_block(processor, BLOCK))
                .unwrap_or_else(|e| panic!("a settling block at {rate} Hz must process: {e}"));

            let block_peak = f64::from(peak(&bufs.output(0)[..BLOCK as usize]));

            run.push(block_peak);
            let low = run.iter().copied().fold(f64::INFINITY, f64::min);
            let high = run.iter().copied().fold(0.0_f64, f64::max);
            if low <= 0.0 || high - low > low * SETTLE_TOLERANCE {
                // This block does not belong to the run the earlier ones were forming. Restart
                // from it rather than from nothing -- it is itself a candidate first block.
                run.clear();
                run.push(block_peak);
            }
            if run.len() >= span_blocks {
                return;
            }
            std::thread::sleep(LANDING_POLL);
        }

        panic!(
            "at {rate} Hz the chain never held a steady level for {SETTLE_SPAN_MS} ms within \
             {SETTLE_LIMIT} blocks -- a handover is still running, so any measurement taken now \
             would average a crossfade rather than the settled chain"
        );
    }

    /// Activates `instance` at `rate`, loads a model declared at a rate that is *not* `rate`, waits
    /// for the handover, then measures a 1 kHz tone through the loaded chain.
    ///
    /// Leaves the instance deactivated, so the caller may call this again at another rate.
    fn run_loaded_at_rate(
        instance: &mut PluginInstance<TestHost>,
        bufs: &mut StereoBuffers,
        rate: f64,
    ) -> LoadedReport {
        let model_rate = if rate.round() as u32 == MODEL_RATE_HZ {
            ALTERNATE_MODEL_RATE_HZ
        } else {
            MODEL_RATE_HZ
        };
        let state_ext = require_plugin_extension::<PluginState>(instance);
        let latency_ext = require_plugin_extension::<PluginLatency>(instance);

        let stopped = activate(instance, config(rate, 1, BLOCK));
        let mut processor = stopped
            .start_processing()
            .unwrap_or_else(|e| panic!("processing must start at {rate} Hz: {e}"));

        let document = document_bytes(model_rate);
        let mut reader = &document[..];
        state_ext
            .load(&mut main_thread_handle(instance), &mut reader)
            .unwrap_or_else(|e| {
                panic!("the host-driven state load must succeed at {rate} Hz: {e}")
            });

        // Wait for the install by watching the figure that only moves when the resampler exists.
        bufs.silence_input();
        let mut landed = false;
        for _ in 0..LANDING_LIMIT {
            audio_section(|| bufs.process_block(&mut processor, BLOCK))
                .unwrap_or_else(|e| panic!("a landing block at {rate} Hz must process: {e}"));
            if latency_ext.get(&mut main_thread_handle(instance)) != 0 {
                landed = true;
                break;
            }
            std::thread::sleep(LANDING_POLL);
        }
        assert!(
            landed,
            "at {rate} Hz the model declared at {model_rate} Hz never reached the engine -- the \
             plugin still reports zero latency after {LANDING_LIMIT} blocks, so D-9.2's \
             SlotResampler was never built and this rate proves nothing"
        );

        settle(&mut processor, bufs, rate);

        let warmup_frames = (WARMUP_MS / 1000.0 * rate).ceil() as u64;
        let measure_frames = (MEASURE_MS / 1000.0 * rate).ceil() as u64;
        let total_frames = warmup_frames + measure_frames;

        let mut measured: Vec<f32> = Vec::with_capacity(measure_frames as usize);
        let mut done: u64 = 0;
        while done < total_frames {
            let frames = BLOCK.min((total_frames - done) as u32);
            for channel in 0..CHANNELS {
                fill_sine(
                    &mut bufs.input_mut(channel)[..frames as usize],
                    SINE_FREQ_HZ,
                    rate,
                    AMPLITUDE,
                    done,
                );
            }
            bufs.poison_output(f32::NAN);
            audio_section(|| bufs.process_block(&mut processor, frames)).unwrap_or_else(|e| {
                panic!("a {frames}-frame block at {rate} Hz must process: {e}")
            });

            for channel in 0..CHANNELS {
                let written = &bufs.output(channel)[..frames as usize];
                assert!(
                    all_finite(written),
                    "at {rate} Hz with a model loaded, channel {channel} of a {frames}-frame \
                     block is not finite"
                );
            }

            let block_start = done;
            if block_start + u64::from(frames) > warmup_frames {
                let from = warmup_frames.saturating_sub(block_start) as usize;
                measured.extend_from_slice(&bufs.output(0)[from..frames as usize]);
            }
            done += u64::from(frames);
        }

        let latency = latency_ext.get(&mut main_thread_handle(instance));
        let stopped = processor.stop_processing();
        instance.deactivate(stopped);

        let sum_sq: f64 = measured.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
        LoadedReport {
            rms: (sum_sq / measured.len() as f64).sqrt() as f32,
            peak: peak(&measured),
            latency,
        }
    }

    /// The loaded sweep itself, across both endpoints, the six standard rates and two off-grid
    /// values, one live instance taken through all of them in turn — so the last rate is also a
    /// mid-session change from the first, with a fresh model install at each.
    ///
    /// Carries no tag of its own: FR-CLAP-080's `trace-partial:` lives on the resource-free sweep
    /// above, whose `uncovered:` field names both this limb's rate count and the sweep's own.
    #[test]
    fn a_loaded_chain_resamples_correctly_at_every_swept_rate() {
        let (_entry, mut instance) = instantiate_default();
        let mut bufs = StereoBuffers::new(BLOCK as usize);

        let reference = run_loaded_at_rate(&mut instance, &mut bufs, 48_000.0);
        assert!(
            reference.rms > 0.0,
            "the 48 kHz loaded reference must be a real signal, not silence"
        );
        assert!(
            reference.latency > 0,
            "the 48 kHz reference must have the resampler engaged (a 44.1 kHz model)"
        );

        for rate in RATES {
            let report = run_loaded_at_rate(&mut instance, &mut bufs, rate);

            assert!(
                report.latency > 0,
                "at {rate} Hz the plugin reports zero latency with a model declared at another \
                 rate loaded -- D-9.2's resampler is not in the chain"
            );
            assert!(
                report.peak > 0.0,
                "at {rate} Hz the loaded chain produced digital silence from a -12 dBFS tone"
            );

            let difference = level_difference_db(report.rms, reference.rms).unwrap_or_else(|| {
                panic!(
                    "at {rate} Hz the loaded RMS ({}) or the 48 kHz reference ({}) is not a \
                     positive level",
                    report.rms, reference.rms
                )
            });
            assert!(
                difference.abs() <= RMS_TOLERANCE_DB,
                "at {rate} Hz the 1 kHz probe came out {difference:+.3} dB from its 48 kHz level \
                 through the *loaded* chain (rms {} vs {}), beyond the {RMS_TOLERANCE_DB} dB \
                 tolerance -- the resampler or a rate-derived coefficient is wrong at this rate",
                report.rms,
                reference.rms
            );
        }

        drop(instance); // `clap_plugin.destroy`
    }
}
