//! FR-IO-020's sample-format conversion: the integer <-> `f32` bridge the audio callback runs when
//! a device will not accept `f32` natively.
//!
//! # Why this exists at all
//!
//! Under `AUDCLNT_SHAREMODE_SHARED` the Windows audio engine converts for a stream, so
//! [`crate::audio_io`] could ask for `f32` and always get it. Exclusive mode has no engine to
//! convert for it — D-13.4's fork drops `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` and
//! `SRC_DEFAULT_QUALITY` there, WASAPI rejecting both — so only a format the device accepts
//! **natively** will open, and most real hardware (onboard HDA codecs, USB class-compliant
//! interfaces, HDMI) exposes integer PCM only. `f32` looks universal on Windows only because
//! `GetMixFormat` reports the *engine's* mix format. Without this module, FR-IO-020's exclusive
//! mode would be a path that essentially never engages on real hardware.
//!
//! # What is converted, and what deliberately is not
//!
//! [`crate::audio_io`]'s `acceptable_formats` names the whole set: `F32` (no conversion at all),
//! `I32` and `I24`. **`I16` is deliberately excluded** — see that function for the reasoning, which
//! is about dither, not about effort.
//!
//! # The arithmetic, and why it is right at the boundaries
//!
//! Writing `N` significant bits, full scale is `2^(N-1)` steps either side of silence and the
//! representable codes are `-2^(N-1) ..= 2^(N-1) - 1` — an asymmetric range, which is the whole
//! difficulty.
//!
//! **Engine -> device** ([`IntegerFormat::from_engine`], via [`to_scaled_int`]): clamp the `f32` to
//! `[-1.0, 1.0]`, multiply by `2^(N-1)`, truncate toward zero into an `i64`, then clamp *that* to
//! the format's representable range.
//!
//! - `-1.0` maps to `-2^(N-1)`, exactly the most-negative code. The clamp never fires on this side.
//! - `+1.0` maps to `+2^(N-1)`, which is **one past** the most-positive code, and the integer clamp
//!   is what pins it to `2^(N-1) - 1`. This is the case worth being explicit about: without that
//!   clamp, `I24` would carry an inner value of `0x0080_0000`, whose low 24 bits are the 24-bit
//!   two's-complement pattern for `-2^23`. A full-scale *positive* sample would leave the device as
//!   full-scale *negative* — the worst possible failure on an audio output, and an audible one on
//!   every peak. (For `i32` Rust's float-to-integer `as` cast happens to saturate rather than wrap,
//!   so that format would have survived without the clamp; `I24` would not, and one rule written
//!   once for both is better than a rule that is only load-bearing in one of two places.)
//! - Nothing legitimately in range is pushed over by rounding. The largest `f32` strictly below
//!   `1.0` is `1 - 2^-24`; times `2^(N-1)` that is `2^(N-1) - 2^(N-25)`, exactly representable as an
//!   `f32` for both supported widths, and it truncates to `2^23 - 1` (`I24`) and `2^31 - 128`
//!   (`i32`). So the clamp only ever fires at exactly `±1.0` and beyond.
//! - `NaN` survives `f32::clamp` (which propagates it) and Rust's `as` cast maps it to `0` —
//!   silence, not a wrapped extreme. `±inf` is pinned by the clamp before the multiply.
//!
//! **Device -> engine** ([`IntegerFormat::to_engine`]): `cpal`'s own `Sample`/`FromSample`
//! conversion, used unchanged rather than hand-rolled. `dasp_sample`'s integer-to-float direction
//! is `v as f32 / 2^(N-1)`: total, with no precondition to violate, mapping `-2^(N-1)` to exactly
//! `-1.0` and `2^(N-1) - 1` to just under `+1.0` (`1 - 2^-23` for `I24`; for `i32` the exact
//! quotient `1 - 2^-31` is not representable in `f32` and rounds to `1.0`, which is in range and
//! round-trips back to `i32::MAX` through the outbound half above).
//!
//! The float-to-integer direction is the one place this module does **not** defer to `cpal`, and
//! the reason is specific: `dasp_sample`'s own source says of it "the following conversions assume
//! `-1.0 <= s < 1.0` (note that +1.0 is excluded) and will overflow otherwise", and its `I24` arm
//! builds the result with `I24::new_unchecked`. It documents the precondition and does not enforce
//! it. Enforcing it — on both sides, for both formats — is exactly what [`to_scaled_int`] is.
//!
//! # Real-time safety (NFR-RT-010)
//!
//! Both converters run *inside* the audio callback. Each owns one `f32` scratch buffer sized once
//! at stream-build time from the negotiated block size and channel count, and neither ever grows
//! it: a callback larger than the scratch is processed in successive chunks, the same shape
//! [`crate::stream`]'s output callback already uses for `max_block_size`. Chunk boundaries stay on
//! frame boundaries because the scratch length is a whole multiple of the channel count and so is
//! every callback length, which keeps the interleave phase intact across chunks. Nothing here
//! allocates, locks, or does I/O; `convert::tests` asserts the allocation half of that directly
//! through [`crate::rt_harness`].

use cpal::{I24, Sample, SizedSample};

/// One integer sample format the audio callback can convert to and from.
///
/// Implemented for exactly the two formats `crate::audio_io::acceptable_formats` names — `i32` and
/// `I24`. `f32` is deliberately **not** an implementor: a device that accepts `f32` natively is
/// opened through `cpal`'s typed builder with no converter in the path at all, which is both the
/// fastest and the least surprising thing to do.
pub(super) trait IntegerFormat: SizedSample + Copy + Send + 'static {
    /// `2^(N-1)` for this format's `N` significant bits — the multiplier taking `1.0` to one step
    /// past the most-positive code.
    const FULL_SCALE: f32;
    /// The most-negative representable code, `-2^(N-1)`.
    const MIN_CODE: i64;
    /// The most-positive representable code, `2^(N-1) - 1`.
    const MAX_CODE: i64;

    /// Engine `f32` -> device code. See this module's doc comment for the boundary argument.
    fn from_engine(sample: f32) -> Self;

    /// Device code -> engine `f32`, through `cpal`'s own conversion.
    fn to_engine(self) -> f32;
}

/// The shared float-to-integer half: clamp the float, scale, truncate, clamp the integer.
///
/// Split out of [`IntegerFormat::from_engine`] rather than written twice, because the second clamp
/// is the entire correctness argument and two copies of it is exactly the pair that drifts apart at
/// one endpoint (the same reasoning `SupportedConfigRange::covers_rate` records for its own
/// inclusive-bounds test).
fn to_scaled_int(sample: f32, full_scale: f32, min_code: i64, max_code: i64) -> i64 {
    // `f32::clamp` propagates NaN; the `as` cast then maps NaN to 0. Both are load-bearing -- see
    // this module's doc comment.
    ((sample.clamp(-1.0, 1.0) * full_scale) as i64).clamp(min_code, max_code)
}

impl IntegerFormat for i32 {
    const FULL_SCALE: f32 = 2_147_483_648.0; // 2^31
    const MIN_CODE: i64 = i32::MIN as i64;
    const MAX_CODE: i64 = i32::MAX as i64;

    fn from_engine(sample: f32) -> Self {
        to_scaled_int(sample, Self::FULL_SCALE, Self::MIN_CODE, Self::MAX_CODE) as i32
    }

    fn to_engine(self) -> f32 {
        f32::from_sample(self)
    }
}

impl IntegerFormat for I24 {
    const FULL_SCALE: f32 = 8_388_608.0; // 2^23
    const MIN_CODE: i64 = -8_388_608;
    const MAX_CODE: i64 = 8_388_607;

    fn from_engine(sample: f32) -> Self {
        let code = to_scaled_int(sample, Self::FULL_SCALE, Self::MIN_CODE, Self::MAX_CODE) as i32;
        // `to_scaled_int` has already pinned `code` inside `I24`'s range, so `new` cannot answer
        // `None`. The fallback is spelled out rather than `unwrap`ped anyway: this runs in an audio
        // callback, where a panic takes the whole process down, and one silent sample of silence is
        // the right way for an impossible branch to fail if it ever stops being impossible.
        I24::new(code).unwrap_or_default()
    }

    fn to_engine(self) -> f32 {
        f32::from_sample(self)
    }
}

/// The engine-side playback callback: fills an interleaved `f32` buffer. Exactly the closure
/// [`crate::audio_io::AudioBackend::build_output_stream`] takes, named so the converter's
/// signatures stay readable — and so `clippy::type_complexity` has a definition to point at rather
/// than an `allow` at every mention, which is how the trait's own methods deal with it.
pub(super) type OutputCallback = Box<dyn FnMut(&mut [f32]) + Send>;

/// The engine-side capture callback, as [`OutputCallback`].
pub(super) type InputCallback = Box<dyn FnMut(&[f32]) + Send>;

/// The output callback's converting body: run the engine into an `f32` scratch buffer, then write
/// that scratch out as device codes.
///
/// Holds the same `on_data` closure the `f32` path hands straight to `cpal`, so the engine side of
/// the callback is bit-identical between the two paths — the only difference is what happens to the
/// samples after `on_data` returns.
pub(super) struct OutputConverter {
    scratch: Vec<f32>,
    on_data: OutputCallback,
}

impl OutputConverter {
    /// Pre-sizes the scratch to `samples` interleaved `f32`s — see
    /// `crate::audio_io::scratch_samples` for where that number comes from. `samples` is floored at
    /// 1 so [`Self::fill`]'s chunk length is never zero.
    pub(super) fn new(on_data: OutputCallback, samples: usize) -> Self {
        Self {
            scratch: vec![0.0; samples.max(1)],
            on_data,
        }
    }

    /// Fills one device callback buffer, in chunks of at most the pre-sized scratch length.
    ///
    /// The scratch is handed to `on_data` exactly as `cpal`'s typed builder would hand it the
    /// device buffer, stale contents and all — it is not cleared first, because the `f32` path does
    /// not clear the device buffer either and [`crate::stream`]'s callback writes every sample it
    /// is given. (It is zeroed once at construction, so even a first callback starts from silence
    /// rather than from whatever the device buffer held.)
    pub(super) fn fill<T: IntegerFormat>(&mut self, out: &mut [T]) {
        let chunk_len = self.scratch.len();
        for chunk in out.chunks_mut(chunk_len) {
            let engine = &mut self.scratch[..chunk.len()];
            (self.on_data)(engine);
            for (code, sample) in chunk.iter_mut().zip(engine.iter()) {
                *code = T::from_engine(*sample);
            }
        }
    }
}

/// The input callback's converting body: read device codes into an `f32` scratch buffer, then hand
/// that scratch to the same `on_data` closure the `f32` path uses.
pub(super) struct InputConverter {
    scratch: Vec<f32>,
    on_data: InputCallback,
}

impl InputConverter {
    /// Pre-sizes the scratch to `samples` interleaved `f32`s, floored at 1 as for
    /// [`OutputConverter::new`].
    pub(super) fn new(on_data: InputCallback, samples: usize) -> Self {
        Self {
            scratch: vec![0.0; samples.max(1)],
            on_data,
        }
    }

    /// Converts one device callback buffer, in chunks of at most the pre-sized scratch length.
    pub(super) fn drain<T: IntegerFormat>(&mut self, data: &[T]) {
        let chunk_len = self.scratch.len();
        for chunk in data.chunks(chunk_len) {
            let engine = &mut self.scratch[..chunk.len()];
            for (sample, code) in engine.iter_mut().zip(chunk.iter()) {
                *sample = code.to_engine();
            }
            (self.on_data)(engine);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;

    /// Where a round trip through an integer format is allowed to land: one quantisation step of
    /// the *narrower* of the two supported widths, which is `I24`'s `2^-23`. Stated once so the two
    /// round-trip tests below cannot drift apart on tolerance.
    const ONE_I24_STEP: f32 = 1.0 / 8_388_608.0;

    /// A spread of engine-side values covering silence, both polarities, small signals and the
    /// in-range extremes, for the tests that want "every ordinary sample" rather than a boundary.
    const IN_RANGE_SAMPLES: [f32; 11] = [
        0.0, 1e-6, -1e-6, 0.1, -0.1, 0.5, -0.5, 0.75, -0.75, 0.999_999, -0.999_999,
    ];

    #[test]
    fn a_sample_round_trips_through_i32_within_one_quantisation_step() {
        for sample in IN_RANGE_SAMPLES {
            let back = i32::from_engine(sample).to_engine();
            assert!(
                (back - sample).abs() <= ONE_I24_STEP,
                "{sample} round-tripped through i32 to {back}"
            );
        }
    }

    #[test]
    fn a_sample_round_trips_through_i24_within_one_quantisation_step() {
        for sample in IN_RANGE_SAMPLES {
            let back = I24::from_engine(sample).to_engine();
            assert!(
                (back - sample).abs() <= ONE_I24_STEP,
                "{sample} round-tripped through I24 to {back}"
            );
        }
    }

    /// The failure this module exists to prevent, asserted directly rather than inferred from a
    /// round trip: a full-scale positive sample must stay positive. `dasp_sample`'s own
    /// `f32 -> I24` conversion returns `I24::new_unchecked(8_388_608)` here, whose low 24 bits are
    /// full-scale *negative*.
    #[test]
    fn positive_full_scale_does_not_wrap_to_negative_in_either_format() {
        assert_eq!(i32::from_engine(1.0), i32::MAX);
        assert_eq!(I24::from_engine(1.0).inner(), 8_388_607);
        assert!(i32::from_engine(1.0) > 0);
        assert!(I24::from_engine(1.0).inner() > 0);
    }

    /// Negative full scale is the *representable* endpoint, so it is not clamped away: it must land
    /// on the most-negative code exactly, not one step short of it.
    #[test]
    fn negative_full_scale_lands_on_the_most_negative_code_exactly() {
        assert_eq!(i32::from_engine(-1.0), i32::MIN);
        assert_eq!(I24::from_engine(-1.0).inner(), -8_388_608);
    }

    /// Beyond full scale in either direction the result is pinned, never wrapped — including the
    /// infinities, which `f32::clamp` pins before the multiply can produce anything at all.
    #[test]
    fn a_sample_beyond_full_scale_is_clamped_rather_than_wrapped() {
        for over in [1.000_001_f32, 1.5, 2.0, 1e9, f32::INFINITY, f32::MAX] {
            assert_eq!(i32::from_engine(over), i32::MAX, "+{over}");
            assert_eq!(I24::from_engine(over).inner(), 8_388_607, "+{over}");
        }
        for under in [
            -1.000_001_f32,
            -1.5,
            -2.0,
            -1e9,
            f32::NEG_INFINITY,
            f32::MIN,
        ] {
            assert_eq!(i32::from_engine(under), i32::MIN, "{under}");
            assert_eq!(I24::from_engine(under).inner(), -8_388_608, "{under}");
        }
    }

    /// Silence maps to the zero code in both directions and both formats. Worth its own test
    /// because an off-by-one in the scaling (a `2^N` multiplier, or an unsigned origin) would show
    /// up here first and nowhere else in a listening test.
    #[test]
    fn silence_maps_to_zero_in_both_directions() {
        assert_eq!(i32::from_engine(0.0), 0);
        assert_eq!(I24::from_engine(0.0).inner(), 0);
        assert_eq!(0i32.to_engine(), 0.0);
        assert_eq!(I24::new(0).unwrap().to_engine(), 0.0);
    }

    /// A `NaN` reaching the output is a bug upstream, but it must not become full-scale noise here.
    /// Rust's float-to-integer cast maps `NaN` to zero, and this pins that we depend on it.
    #[test]
    fn a_nan_sample_becomes_silence_rather_than_a_wrapped_extreme() {
        assert_eq!(i32::from_engine(f32::NAN), 0);
        assert_eq!(I24::from_engine(f32::NAN).inner(), 0);
    }

    /// The inbound direction's own endpoints, which `cpal`'s conversion owns rather than this
    /// module: the widest negative code is exactly `-1.0`, and nothing lands outside `[-1.0, 1.0]`.
    #[test]
    fn the_widest_device_codes_arrive_inside_the_engines_range() {
        assert_eq!(i32::MIN.to_engine(), -1.0);
        assert_eq!(I24::new(-8_388_608).unwrap().to_engine(), -1.0);
        for code in [i32::MAX, i32::MIN, 0, 1, -1] {
            let sample = code.to_engine();
            assert!((-1.0..=1.0).contains(&sample), "i32 {code} -> {sample}");
        }
        for code in [8_388_607, -8_388_608, 0, 1, -1] {
            let sample = I24::new(code).unwrap().to_engine();
            assert!((-1.0..=1.0).contains(&sample), "I24 {code} -> {sample}");
        }
    }

    /// A callback larger than the pre-sized scratch is chunked, and the chunking is invisible: the
    /// samples come out in order, once each, with no gap at a chunk boundary.
    #[test]
    fn an_output_callback_larger_than_the_scratch_is_chunked_without_losing_a_frame() {
        let mut next = 0.0f32;
        let mut converter = OutputConverter::new(
            Box::new(move |buf: &mut [f32]| {
                for slot in buf.iter_mut() {
                    *slot = next;
                    next += ONE_I24_STEP;
                }
            }),
            4,
        );
        let mut out = [0i32; 10];
        converter.fill(&mut out);
        for (index, code) in out.iter().enumerate() {
            let expected = index as f32 * ONE_I24_STEP;
            let got = code.to_engine();
            assert!(
                (got - expected).abs() <= ONE_I24_STEP,
                "sample {index}: expected {expected}, got {got}"
            );
        }
    }

    /// The same property inbound: every device code reaches `on_data` exactly once, in order, even
    /// when the callback is several scratch-lengths long.
    #[test]
    fn an_input_callback_larger_than_the_scratch_is_chunked_without_losing_a_frame() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&seen);
        let mut converter = InputConverter::new(
            Box::new(move |buf: &[f32]| recorder.lock().unwrap().extend_from_slice(buf)),
            4,
        );
        let codes: Vec<I24> = (0..10)
            .map(|i| I24::new(i * 100_000).unwrap())
            .collect::<Vec<_>>();
        converter.drain(&codes);
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), codes.len());
        for (index, (sample, code)) in got.iter().zip(codes.iter()).enumerate() {
            assert_eq!(*sample, code.to_engine(), "sample {index}");
        }
    }

    /// NFR-RT-010 for the conversion arithmetic itself: neither converter allocates once built, in
    /// either format, including on a callback several times the pre-sized scratch length (the path
    /// that would have to grow a buffer if the chunking were wrong).
    ///
    /// **The closures held here are stand-ins, not the ones [`crate::stream`] installs** — they
    /// write into or read from the buffer they are handed and nothing else, so what this test
    /// isolates is the converter's own chunking and arithmetic. (This doc comment used to claim
    /// they *were* the shape `crate::stream` installs, which was never something this test checked:
    /// issue #89.) The real pair is driven, converters and all, by
    /// [`the_real_stream_callbacks_run_allocation_free_inside_both_converters`] below.
    #[test]
    fn neither_converter_allocates_once_the_stream_is_built() {
        let mut phase = 0.0f32;
        let mut output = OutputConverter::new(
            Box::new(move |buf: &mut [f32]| {
                for slot in buf.iter_mut() {
                    // Deliberately includes out-of-range values: the clamp is on the hot path and
                    // has to be allocation-free too.
                    *slot = (phase * 0.37).sin() * 1.5;
                    phase += 1.0;
                }
            }),
            8,
        );
        // An `AtomicUsize` behind an `Arc` rather than a captured local: the count has to be
        // readable after the audio section to prove the closure ran, and neither the clone (made
        // here, outside the section) nor the increment allocates.
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&seen);
        let mut input = InputConverter::new(
            Box::new(move |buf: &[f32]| {
                counter.fetch_add(buf.len(), std::sync::atomic::Ordering::Relaxed);
            }),
            8,
        );

        let mut out_i32 = [0i32; 32];
        let mut out_i24 = [I24::new(0).unwrap(); 32];
        let in_i32 = [123_456_789i32; 32];
        let in_i24 = [I24::new(1_234_567).unwrap(); 32];

        audio_section(|| {
            output.fill(&mut out_i32);
            output.fill(&mut out_i24);
            input.drain(&in_i32);
            input.drain(&in_i24);
        });

        // The buffers really were written and read -- an allocation-free no-op would pass the
        // harness too.
        assert!(out_i32.iter().any(|c| *c != 0));
        assert!(out_i24.iter().any(|c| c.inner() != 0));
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::Relaxed),
            in_i32.len() + in_i24.len()
        );
    }

    /// **The callbacks a real `cpal` stream actually invokes, run through the real converters,
    /// under D-7.5's allocation harness.** Issue #89: `audio_section` had exactly one caller in
    /// this crate, and the closures it wrapped were the stand-ins above; M14 put
    /// [`crate::stream`]'s own callbacks under the harness *bare*, and this closes the last gap
    /// between the two — the integer-format path, where the callback the device calls is a
    /// converter wrapping the very closure `crate::stream::open` built.
    ///
    /// Composed exactly as `crate::audio_io::cpal_impl::build_converting_input`/
    /// `build_converting_output` compose it: the callback [`crate::stream::open`] handed the
    /// backend, moved into [`InputConverter`]/[`OutputConverter`] with the same
    /// `cpal_impl::scratch_samples` length a real open would pre-size (a whole 512-frame default
    /// block, deliberately larger than the engine's own `max_block_size`, so both chunking loops —
    /// the converter's and the stream callback's — run more than once per callback).
    ///
    /// The first callback of each direction is driven *outside* the harness: `build_output`'s first
    /// invocation elevates the thread's priority once (D-13.2), which a real stream also pays once.
    #[test]
    fn the_real_stream_callbacks_run_allocation_free_inside_both_converters() {
        const MAX_BLOCK: usize = 64;
        let backend = crate::stream::FakeBackend::new();
        let xruns = std::sync::Arc::new(crate::xrun::XrunCounter::new());
        let _streams = crate::stream::open(
            crate::stream::fake_duplex_setup(&backend, MAX_BLOCK),
            crate::stream::default_test_engine(MAX_BLOCK),
            std::sync::Arc::clone(&xruns),
            |_| {},
            |_| {},
        )
        .unwrap();

        let input_cb = backend.input_data.lock().unwrap().take().unwrap();
        let output_cb = backend.output_data.lock().unwrap().take().unwrap();

        // One mono capture channel, two playback channels -- `fake_duplex_setup`'s own params,
        // with `buffer_frames: None`, which is what makes `block_frames` answer its default.
        let input_scratch = crate::audio_io::block_frames(None);
        let output_scratch = crate::audio_io::block_frames(None) * 2;
        let mut input = InputConverter::new(input_cb, input_scratch);
        let mut output = OutputConverter::new(output_cb, output_scratch);

        // Device buffers longer than the scratch, so the converters chunk too. Both lengths stay a
        // whole number of frames (the output side is even), which is the invariant chunking has to
        // preserve for the interleave phase to survive a chunk boundary.
        let in_i32 = [123_456_789i32; 1400];
        let in_i24 = [I24::new(1_234_567).unwrap(); 1400];
        let mut out_i32 = [0i32; 2800];
        let mut out_i24 = [I24::new(0).unwrap(); 2800];

        // Warm-up, un-asserted: see this test's own doc comment.
        input.drain(&in_i32);
        output.fill(&mut out_i32);

        let mut saw_output = false;
        for _ in 0..8 {
            audio_section(|| input.drain(&in_i32));
            audio_section(|| output.fill(&mut out_i32));
            saw_output |= out_i32.iter().any(|c| *c != 0);
            audio_section(|| input.drain(&in_i24));
            audio_section(|| output.fill(&mut out_i24));
            saw_output |= out_i24.iter().any(|c| c.inner() != 0);
        }

        // The run has to have produced real audio somewhere, or every assertion above would have
        // held over callbacks that returned early -- the same guard `crate::stream`'s own harness
        // test uses, and for the same reason.
        assert!(
            saw_output,
            "every output callback produced silence -- nothing above was actually exercised"
        );
    }
}
