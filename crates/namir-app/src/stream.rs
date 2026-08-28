//! Builds and owns the real `cpal` streams (through [`crate::audio_io::AudioBackend`]), and is
//! the one place in this crate that:
//!
//! - Acquires [`namir_platform::DenormalGuard`] once per audio callback (D-7.4, "once per audio
//!   callback" — engaged for exactly the duration of [`namir_engine::AudioEngine::process`], the
//!   only place in this callback that does real floating-point DSP work).
//! - Calls [`namir_platform::elevate_current_thread_priority`] exactly once, lazily, on the output
//!   callback thread's first invocation (D-13.2's own module doc comment: "once, at stream start
//!   ... from the thread being elevated" — `cpal` gives no pre-callback hook to call this from, so
//!   "first call inside the callback, gated by a one-shot flag" is the only way to satisfy both
//!   halves of that constraint at once).
//! - Runs [`namir_engine::AudioEngine::process`] itself.
//! - Counts FR-IO-060's bridge-underrun dropouts directly, via
//!   [`crate::bridge::BridgeConsumer::pull_into`]'s own return value. `cpal`'s own
//!   `StreamFailure::Xrun` reports arrive through the same `on_failure` callback every other
//!   stream error does; classifying it into the same [`crate::xrun::XrunCounter`] (rather than
//!   surfacing it as a one-off notice the way `StreamFailure::DeviceLost`/`Other` are) is
//!   [`crate::app`]'s job, since that is also where the counter this module increments for
//!   bridge underruns lives.
//!
//! # Why the engine runs in the *output* callback, not the input one
//!
//! **Decision:** [`namir_engine::AudioEngine::process`] is called from the output stream's data
//! callback; the input stream's callback only pushes captured samples into
//! [`crate::bridge::BridgeProducer`].
//!
//! **Rationale:** the output callback is the side with a hard deadline the OS actually enforces
//! (an empty output buffer is an audible dropout; a late *input* read is merely absorbed by the
//! bridge ring's own depth). Driving the engine from the callback whose timing already has to be
//! respected means there is exactly one place per block where `process` runs, at a cadence `cpal`
//! itself paces — no second timer, no possibility of the two callbacks racing to call `process`
//! twice for one block.
//!
//! # Channel handling, and what is deliberately not built here
//!
//! FR-CHAIN-060/`stages/trim.rs`'s own doc comment: for `ChannelConfig::MonoToStereo`, the caller
//! (this module) is responsible for duplicating the mono capture into both `StageIo` channels
//! *before* `Chain::process` runs — Trim's own -6dB-both-terms law only performs a real downmix
//! when the two channels already differ, which is the genuine-stereo-input case. This module
//! implements `Mono` and `MonoToStereo` (a single physical input channel, chosen by
//! `crate::settings::ChannelMapping::input_channel`, duplicated when the engine wants two
//! channels). It does **not** implement `ChannelConfig::Stereo` (two independently captured
//! physical input channels) — that needs reading a second channel index out of the same
//! interleaved input buffer and is left for a future pass; see
//! `docs/manual-tests/fr-io-090-channel-mapping.md`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use namir_core::ChannelConfig;
use namir_engine::{AudioEngine, StageIo};
use namir_platform::{DenormalGuard, elevate_current_thread_priority};

use crate::audio_io::{
    AudioBackend, AudioStream, DeviceInfo, HostInfo, StreamFailure, StreamParams,
};
#[cfg(test)]
use crate::audio_io::{
    AudioIoError, BufferSizeRange, ExclusiveModeOutcome, ShareMode, SupportedConfigRange,
};
use crate::bridge::{BridgeConsumer, BridgeProducer, bridge};
use crate::xrun::XrunCounter;

/// How long `cpal`'s own stream construction waits before giving up — FR-IO-070's "a device
/// failing to open ... shall be handled" needs a bound, not an indefinite hang.
const STREAM_ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Which side of the duplex path a [`StreamFailure`] came from — FR-IO-070's report needs to say
/// which device was lost, and the input/output callbacks share the same failure type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The input (capture) stream.
    Input,
    /// The output (playback) stream.
    Output,
}

/// Everything needed to open the duplex path.
pub struct StreamSetup<'a> {
    /// The audio backend to build streams through.
    pub backend: &'a dyn AudioBackend,
    /// The input host/device/params.
    pub input_host: HostInfo,
    /// See [`Self::input_host`].
    pub input_device: DeviceInfo,
    /// See [`Self::input_host`].
    pub input_params: StreamParams,
    /// The output host/device/params.
    pub output_host: HostInfo,
    /// See [`Self::output_host`].
    pub output_device: DeviceInfo,
    /// See [`Self::output_host`].
    pub output_params: StreamParams,
    /// The engine's own channel configuration — see this module's doc comment for which values
    /// are actually implemented.
    pub channel_config: ChannelConfig,
    /// Which physical input channel (0-indexed within the input stream's own interleaved layout)
    /// feeds the engine.
    pub input_channel_index: u16,
    /// Which physical output channel receives the engine's left/mono output.
    pub output_channel_left: u16,
    /// Which physical output channel receives the engine's right output (ignored for `Mono`).
    pub output_channel_right: u16,
    /// The engine's own declared maximum block size (`PrepareContext::max_block_size`) — output
    /// callbacks are processed in chunks of at most this many frames.
    pub max_block_size: usize,
}

/// The running duplex path. Dropping this stops both streams (`AudioStream`'s own drop-stops
/// contract, per `crate::audio_io`'s doc comment).
pub struct RunningStreams {
    _input: Box<dyn AudioStream>,
    _output: Box<dyn AudioStream>,
}

impl RunningStreams {
    /// Starts both streams. Built paused by `cpal`'s own contract; this is the one call that
    /// actually makes audio flow.
    pub fn play(&self) -> Result<(), crate::audio_io::AudioIoError> {
        self._input.play()?;
        self._output.play()
    }

    /// Pauses both streams without closing them.
    pub fn pause(&self) -> Result<(), crate::audio_io::AudioIoError> {
        self._input.pause()?;
        self._output.pause()
    }
}

/// Opens the duplex path described by `setup`, running `engine` from the output callback.
/// `on_failure` is called (from whichever callback thread detected it, per FR-IO-070's "shall not
/// crash or hang") with which side failed and why; the caller is expected to stop using the
/// returned [`RunningStreams`] and report the condition to the user (FR-IO-070's own wording) —
/// this function does not itself decide when the stream is unrecoverable, since that judgement
/// belongs to whatever owns retry/reselection policy ([`crate::app`]).
pub fn open(
    setup: StreamSetup<'_>,
    engine: AudioEngine,
    xruns: Arc<XrunCounter>,
    on_failure: impl Fn(Direction, StreamFailure) + Send + Sync + 'static,
) -> Result<RunningStreams, crate::audio_io::AudioIoError> {
    let capacity = (setup.max_block_size.max(1) * 8).next_power_of_two();
    let (producer, consumer) = bridge(capacity);

    let input_channel_index = setup.input_channel_index as usize;
    let input_channels = setup.input_params.channels as usize;
    let on_failure: Arc<dyn Fn(Direction, StreamFailure) + Send + Sync> = Arc::new(on_failure);

    let input_stream = build_input(
        &setup,
        producer,
        input_channel_index,
        input_channels,
        Arc::clone(&on_failure),
    )?;
    let output_stream = match build_output(
        &setup,
        engine,
        consumer,
        Arc::clone(&xruns),
        Arc::clone(&on_failure),
    ) {
        Ok(s) => s,
        Err(e) => {
            drop(input_stream);
            return Err(e);
        }
    };

    Ok(RunningStreams {
        _input: input_stream,
        _output: output_stream,
    })
}

fn build_input(
    setup: &StreamSetup<'_>,
    mut producer: BridgeProducer,
    channel_index: usize,
    channel_count: usize,
    on_failure: Arc<dyn Fn(Direction, StreamFailure) + Send + Sync>,
) -> Result<Box<dyn AudioStream>, crate::audio_io::AudioIoError> {
    let max_block = setup.max_block_size.max(1);
    let mut mono_scratch: Vec<f32> = Vec::with_capacity(max_block);
    let on_data = Box::new(move |data: &[f32]| {
        if channel_count == 0 {
            return;
        }
        // Chunked at `max_block` frames rather than extending over the whole callback in one go
        // (NFR-RT-010, found by `the_audio_callbacks_this_module_builds_allocate_nothing` at M14):
        // `mono_scratch` is reserved for exactly `max_block` samples, so a host that hands this
        // callback a buffer larger than the block size the engine was prepared for — which is
        // legal, and is why `build_output` below already chunks — would grow the `Vec` from inside
        // the audio callback. Chunking keeps every `extend` inside the reservation.
        for frames in data.chunks(channel_count * max_block) {
            mono_scratch.clear();
            mono_scratch.extend(
                frames
                    .chunks_exact(channel_count)
                    .map(|frame| frame.get(channel_index).copied().unwrap_or(0.0)),
            );
            producer.push_captured(&mono_scratch);
        }
    });
    let on_error = {
        let on_failure = Arc::clone(&on_failure);
        Box::new(move |failure: StreamFailure| on_failure(Direction::Input, failure))
    };

    setup.backend.build_input_stream(
        &setup.input_host,
        &setup.input_device,
        setup.input_params,
        on_data,
        on_error,
        STREAM_ACTIVATION_TIMEOUT,
    )
}

fn build_output(
    setup: &StreamSetup<'_>,
    mut engine: AudioEngine,
    mut consumer: BridgeConsumer,
    xruns: Arc<XrunCounter>,
    on_failure: Arc<dyn Fn(Direction, StreamFailure) + Send + Sync>,
) -> Result<Box<dyn AudioStream>, crate::audio_io::AudioIoError> {
    let output_channels = setup.output_params.channels as usize;
    let left = setup.output_channel_left as usize;
    let right = setup.output_channel_right as usize;
    let duplicate_into_stereo = matches!(setup.channel_config, ChannelConfig::MonoToStereo);
    let engine_channel_count = setup.channel_config.output_channels() as usize;
    let max_block = setup.max_block_size.max(1);

    let priority_elevated = AtomicBool::new(false);
    let mut mono_in = vec![0.0f32; max_block];
    // Two named buffers rather than a `Vec<Vec<f32>>`, because `StageIo::new` wants a
    // `&mut [&mut [f32]]` and building that from a `Vec<Vec<f32>>` means collecting a fresh
    // `Vec<&mut [f32]>` — an allocation, on the audio thread, once per chunk. `ChannelConfig`
    // has exactly two output-channel counts (1 and 2), so the two cases below are exhaustive and
    // the borrow can be a stack array instead. Found by
    // `the_audio_callbacks_this_module_builds_allocate_nothing` at M14, which is the first thing
    // in this crate to run these callbacks under D-7.5's harness (NFR-RT-010).
    let mut engine_left = vec![0.0f32; max_block];
    let mut engine_right = vec![0.0f32; max_block];

    let on_data = Box::new(move |out: &mut [f32]| {
        if !priority_elevated.swap(true, Ordering::AcqRel) {
            // D-13.2: once, lazily, from this callback thread itself -- see this module's doc
            // comment for why "first call inside the callback" is the only place cpal lets this
            // happen. A denial is expected and non-fatal (that module's own doc comment); nothing
            // here needs to react to the outcome beyond having attempted it.
            let _ = elevate_current_thread_priority();
        }
        // D-7.4: engaged for the whole callback, not just the `engine.process` call, since this
        // callback's bridge-pull/write-back arithmetic is also floating point and denormal-prone
        // once fed by a decaying tail.
        let _guard = DenormalGuard::new();

        if output_channels == 0 {
            out.fill(0.0);
            return;
        }
        let frames = out.len() / output_channels;
        let mut done = 0usize;
        while done < frames {
            let chunk = (frames - done).min(max_block);

            let padded = consumer.pull_into(&mut mono_in[..chunk], 0.0);
            if padded > 0 {
                xruns.record();
            }

            engine_left[..chunk].copy_from_slice(&mono_in[..chunk]);
            if engine_channel_count > 1 {
                if duplicate_into_stereo {
                    engine_right[..chunk].copy_from_slice(&mono_in[..chunk]);
                } else {
                    engine_right[..chunk].fill(0.0);
                }
            }

            if engine_channel_count > 1 {
                let mut refs: [&mut [f32]; 2] =
                    [&mut engine_left[..chunk], &mut engine_right[..chunk]];
                let mut io = StageIo::new(&mut refs, chunk);
                engine.process(&mut io);
            } else {
                let mut refs: [&mut [f32]; 1] = [&mut engine_left[..chunk]];
                let mut io = StageIo::new(&mut refs, chunk);
                engine.process(&mut io);
            }

            for frame in 0..chunk {
                let out_frame = &mut out
                    [(done + frame) * output_channels..(done + frame + 1) * output_channels];
                out_frame.fill(0.0);
                if let Some(slot) = out_frame.get_mut(left) {
                    *slot = engine_left[frame];
                }
                if engine_channel_count > 1
                    && let Some(slot) = out_frame.get_mut(right)
                {
                    *slot = engine_right[frame];
                }
            }

            done += chunk;
        }
    });

    let on_error = {
        let on_failure = Arc::clone(&on_failure);
        Box::new(move |failure: StreamFailure| on_failure(Direction::Output, failure))
    };

    setup.backend.build_output_stream(
        &setup.output_host,
        &setup.output_device,
        setup.output_params,
        on_data,
        on_error,
        STREAM_ACTIVATION_TIMEOUT,
    )
}

/// A minimal in-process fake backend: no real device, just two channels connected through nothing
/// but a test's own direct calls into the callbacks it captures. Proves the *wiring* (channel
/// selection, chunking, xrun accounting, and since M11 the share mode each direction was opened
/// with) without any real audio hardware — so every test built on it runs on a headless Linux CI
/// runner exactly as it does on Windows.
///
/// Declared at module level rather than nested inside this module's own `mod tests`, and
/// `pub(crate)`, for the same reason `namir_ui::host::RecordingHost` is: [`crate::app`]'s tests
/// need an [`AudioBackend`] too, and a second, separately-drifting fake is worse than one shared
/// one.
#[cfg(test)]
pub(crate) struct FakeBackend {
    /// The input callback the last `build_input_stream` captured, for a test to drive directly.
    pub(crate) input_data: std::sync::Mutex<Option<InputCallback>>,
    /// The output callback the last `build_output_stream` captured.
    pub(crate) output_data: std::sync::Mutex<Option<OutputCallback>>,
    /// Which device names answer [`ExclusiveModeOutcome::Engaged`] to
    /// `supports_exclusive`. Every other name answers `Unsupported` — what the real
    /// [`crate::audio_io::CpalBackend`] answers for any device with no exclusive-capable WASAPI
    /// endpoint behind it, so a test that says nothing about exclusive mode gets the conservative
    /// answer rather than an optimistic one.
    exclusive_devices: Vec<String>,
    /// The [`ShareMode`] each direction's `build_*_stream` was actually handed —
    /// the observable that distinguishes "the session settled on exclusive" from "the session
    /// settled on exclusive and then opened shared anyway".
    asked_share_modes: std::sync::Mutex<Vec<(Direction, ShareMode)>>,
}

#[cfg(test)]
impl FakeBackend {
    /// A backend that refuses exclusive mode on every device — the interim real-world answer.
    pub(crate) fn new() -> Self {
        Self {
            input_data: std::sync::Mutex::new(None),
            output_data: std::sync::Mutex::new(None),
            exclusive_devices: Vec::new(),
            asked_share_modes: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Makes `device_name` answer `Engaged` to `supports_exclusive`. Per device, not per backend,
    /// so a test can grant exclusive mode to one direction and refuse it on the other.
    pub(crate) fn granting_exclusive_to(mut self, device_name: &str) -> Self {
        self.exclusive_devices.push(device_name.to_string());
        self
    }

    /// The share mode `direction`'s stream was actually opened with, or `None` if that direction
    /// was never opened.
    pub(crate) fn share_mode_asked_for(&self, direction: Direction) -> Option<ShareMode> {
        self.asked_share_modes
            .lock()
            .unwrap()
            .iter()
            .find(|(d, _)| *d == direction)
            .map(|(_, mode)| *mode)
    }
}

#[cfg(test)]
struct FakeStream;

#[cfg(test)]
impl AudioStream for FakeStream {
    fn play(&self) -> Result<(), AudioIoError> {
        Ok(())
    }
    fn pause(&self) -> Result<(), AudioIoError> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) type InputCallback = Box<dyn FnMut(&[f32]) + Send>;
#[cfg(test)]
pub(crate) type OutputCallback = Box<dyn FnMut(&mut [f32]) + Send>;

#[cfg(test)]
impl AudioBackend for FakeBackend {
    fn hosts(&self) -> Vec<HostInfo> {
        vec![]
    }
    fn default_host(&self) -> HostInfo {
        HostInfo {
            name: "fake".to_string(),
        }
    }
    fn input_devices(&self, _host: &HostInfo) -> Result<Vec<DeviceInfo>, AudioIoError> {
        Ok(vec![])
    }
    fn output_devices(&self, _host: &HostInfo) -> Result<Vec<DeviceInfo>, AudioIoError> {
        Ok(vec![])
    }
    fn input_configs(
        &self,
        _h: &HostInfo,
        _d: &DeviceInfo,
    ) -> Result<Vec<SupportedConfigRange>, AudioIoError> {
        Ok(vec![SupportedConfigRange {
            channels: 1,
            min_sample_rate_hz: 48_000,
            max_sample_rate_hz: 48_000,
            buffer_size: BufferSizeRange::Unknown,
        }])
    }
    fn output_configs(
        &self,
        _h: &HostInfo,
        _d: &DeviceInfo,
    ) -> Result<Vec<SupportedConfigRange>, AudioIoError> {
        Ok(vec![SupportedConfigRange {
            channels: 2,
            min_sample_rate_hz: 48_000,
            max_sample_rate_hz: 48_000,
            buffer_size: BufferSizeRange::Unknown,
        }])
    }
    fn supports_exclusive(
        &self,
        _host: &HostInfo,
        device: &DeviceInfo,
        _params: StreamParams,
    ) -> ExclusiveModeOutcome {
        if self.exclusive_devices.contains(&device.name) {
            ExclusiveModeOutcome::Engaged
        } else {
            ExclusiveModeOutcome::Unsupported
        }
    }
    fn build_input_stream(
        &self,
        _host: &HostInfo,
        _device: &DeviceInfo,
        params: StreamParams,
        on_data: Box<dyn FnMut(&[f32]) + Send>,
        _on_error: Box<dyn FnMut(StreamFailure) + Send>,
        _timeout: Duration,
    ) -> Result<Box<dyn AudioStream>, AudioIoError> {
        self.asked_share_modes
            .lock()
            .unwrap()
            .push((Direction::Input, params.share_mode));
        *self.input_data.lock().unwrap() = Some(on_data);
        Ok(Box::new(FakeStream))
    }
    fn build_output_stream(
        &self,
        _host: &HostInfo,
        _device: &DeviceInfo,
        params: StreamParams,
        on_data: Box<dyn FnMut(&mut [f32]) + Send>,
        _on_error: Box<dyn FnMut(StreamFailure) + Send>,
        _timeout: Duration,
    ) -> Result<Box<dyn AudioStream>, AudioIoError> {
        self.asked_share_modes
            .lock()
            .unwrap()
            .push((Direction::Output, params.share_mode));
        *self.output_data.lock().unwrap() = Some(on_data);
        Ok(Box::new(FakeStream))
    }
}

/// A duplex [`StreamSetup`] over `backend`: one mono input channel, two output channels, 48 kHz,
/// shared mode. `pub(crate)` for the same reason [`FakeBackend`] itself is — the tests in
/// `crate::audio_io::convert` drive the very callbacks this setup produces, and a second copy of
/// the setup would be free to drift away from the one every other test uses.
#[cfg(test)]
pub(crate) fn fake_duplex_setup(backend: &FakeBackend, max_block_size: usize) -> StreamSetup<'_> {
    fake_duplex_setup_with_share_mode(backend, max_block_size, ShareMode::Shared)
}

/// As [`fake_duplex_setup`], with the share mode both directions are opened with chosen by the
/// caller.
#[cfg(test)]
pub(crate) fn fake_duplex_setup_with_share_mode(
    backend: &FakeBackend,
    max_block_size: usize,
    share_mode: ShareMode,
) -> StreamSetup<'_> {
    StreamSetup {
        backend,
        input_host: HostInfo {
            name: "fake".to_string(),
        },
        input_device: DeviceInfo {
            name: "in".to_string(),
            is_default: true,
        },
        input_params: StreamParams {
            sample_rate_hz: 48_000,
            buffer_frames: None,
            channels: 1,
            share_mode,
        },
        output_host: HostInfo {
            name: "fake".to_string(),
        },
        output_device: DeviceInfo {
            name: "out".to_string(),
            is_default: true,
        },
        output_params: StreamParams {
            sample_rate_hz: 48_000,
            buffer_frames: None,
            channels: 2,
            share_mode,
        },
        channel_config: ChannelConfig::MonoToStereo,
        input_channel_index: 0,
        output_channel_left: 0,
        output_channel_right: 1,
        max_block_size,
    }
}

/// A real default chain, split into the [`AudioEngine`] half [`open`] runs from the output
/// callback. `pub(crate)` for the same reason [`fake_duplex_setup`] is.
#[cfg(test)]
pub(crate) fn default_test_engine(max_block_size: usize) -> AudioEngine {
    let c = namir_engine::PrepareContext::new(
        namir_core::SampleRate::new(48_000).unwrap(),
        max_block_size,
        ChannelConfig::MonoToStereo,
    )
    .unwrap();
    let chain = namir_engine::build_default_chain(&c).unwrap();
    let (engine, _endpoint) = namir_engine::split(chain, namir_engine::RingCapacities::default());
    engine
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    // The three helpers below live at module level (and `pub(crate)`) so `audio_io::convert`'s
    // tests can build the same duplex path this module's own tests do; aliased back to their
    // original names here so every test body below reads as it always has.
    use super::{
        default_test_engine as engine, fake_duplex_setup as setup,
        fake_duplex_setup_with_share_mode as setup_with_share_mode,
    };

    /// Wiring proof: input capture reaches the output buffer, duplicated into both channels
    /// (`ChannelConfig::MonoToStereo`), with no crash and no underrun when supply matches demand.
    #[test]
    fn captured_input_reaches_the_output_buffer_duplicated_across_channels() {
        let backend = FakeBackend::new();
        let xruns = Arc::new(XrunCounter::new());
        let failures = Arc::new(AtomicUsize::new(0));
        let failures_clone = Arc::clone(&failures);

        let _streams = open(
            setup(&backend, 64),
            engine(64),
            Arc::clone(&xruns),
            move |_dir, _f| {
                failures_clone.fetch_add(1, Ordering::SeqCst);
            },
        )
        .unwrap();

        let mut input_cb = backend.input_data.lock().unwrap().take().unwrap();
        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();

        input_cb(&[0.1f32; 64]);
        let mut out = [0.0f32; 128]; // 64 frames * 2 channels
        output_cb(&mut out);

        assert_eq!(
            xruns.count(),
            0,
            "supply matched demand: no underrun expected"
        );
        assert_eq!(failures.load(Ordering::SeqCst), 0);
        // Both channels should carry non-silent output once the gate/trim ramp has something to
        // pass through -- a bypassed, unloaded chain still passes the dry signal through Trim.
        assert!(out.iter().any(|s| s.abs() > 1e-6));
    }

    /// FR-IO-060's bridge-underrun path: pulling with nothing pushed yet counts an xrun rather
    /// than panicking or silently producing garbage.
    // trace-partial: FR-IO-060
    // uncovered: FR-IO-060 — the "resettable by the user" clause has no path to exercise:
    // uncovered: XrunCounter::reset has no caller outside its own two unit tests and no UiIntent
    // uncovered: reaches it, and the running count surfaces only through an eprintln! rather than
    // uncovered: anywhere in the window; closes M8
    #[test]
    fn an_output_pull_with_no_input_yet_counts_an_xrun() {
        let backend = FakeBackend::new();
        let xruns = Arc::new(XrunCounter::new());
        let _streams = open(
            setup(&backend, 64),
            engine(64),
            Arc::clone(&xruns),
            |_, _| {},
        )
        .unwrap();

        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();
        let mut out = [0.0f32; 128];
        output_cb(&mut out); // no input_cb call first -- the ring is empty.

        assert!(xruns.count() > 0);
    }

    /// A callback asking for more frames than `max_block_size` is processed in more than one
    /// internal chunk rather than panicking (`StageIo::new`'s own assertion would trip otherwise).
    #[test]
    fn an_output_request_larger_than_max_block_size_is_chunked() {
        let backend = FakeBackend::new();
        let xruns = Arc::new(XrunCounter::new());
        let _streams = open(
            setup(&backend, 32),
            engine(32),
            Arc::clone(&xruns),
            |_, _| {},
        )
        .unwrap();

        let mut input_cb = backend.input_data.lock().unwrap().take().unwrap();
        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();

        input_cb(&[0.1f32; 100]);
        let mut out = [0.0f32; 200]; // 100 frames, over max_block_size (32)
        output_cb(&mut out); // must not panic
    }

    /// **NFR-RT-010 for this crate's own audio callbacks.** D-7.5's `assert_no_alloc` harness has
    /// been installed in this crate since M11 (`crate::rt_harness`), but until M14 the only thing
    /// it wrapped was `audio_io::convert`'s sample-format arithmetic — so the two closures
    /// [`build_input`] and [`build_output`] construct, which are the whole of `namir-app`'s
    /// audio-thread code, ran under no allocation assertion anywhere. These are the callbacks a
    /// real `cpal` stream invokes; [`FakeBackend`] hands them back verbatim, so driving them here
    /// runs the same code a device would, minus the device.
    ///
    /// **It found two allocations on the first run, both now fixed** and both commented at their
    /// sites: `build_output` collected a fresh `Vec<&mut [f32]>` for `StageIo::new` once per
    /// internal chunk, on every single callback; and `build_input`'s `mono_scratch` grew past its
    /// reservation whenever the host delivered more frames than the negotiated block size.
    ///
    /// Both buffer sizes are driven here — one callback at exactly `max_block_size`, and one
    /// larger than it so `build_output`'s chunking loop runs more than once and `build_input`'s
    /// new chunking loop does too. The first callback pair is deliberately *outside* the harness:
    /// `build_output`'s first invocation elevates the thread's priority once (D-13.2), a one-time
    /// OS call rather than per-callback work, and a real stream pays it once as well.
    #[test]
    fn the_audio_callbacks_this_module_builds_allocate_nothing() {
        const MAX_BLOCK: usize = 64;
        let backend = FakeBackend::new();
        let xruns = Arc::new(XrunCounter::new());
        let _streams = open(
            setup(&backend, MAX_BLOCK),
            engine(MAX_BLOCK),
            Arc::clone(&xruns),
            |_, _| {},
        )
        .unwrap();

        let mut input_cb = backend.input_data.lock().unwrap().take().unwrap();
        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();

        let exact_in = [0.1f32; MAX_BLOCK];
        let mut exact_out = [0.0f32; MAX_BLOCK * 2];
        // Deliberately not a multiple of MAX_BLOCK, so the final chunk of each callback is a
        // partial one -- the shape most likely to be got wrong by a fixed-size buffer.
        let big_in = [0.1f32; 200];
        let mut big_out = [0.0f32; 400];

        // Warm-up, un-asserted: see this test's own doc comment.
        input_cb(&exact_in);
        output_cb(&mut exact_out);
        input_cb(&big_in);
        output_cb(&mut big_out);

        let mut saw_output = false;
        for _ in 0..32 {
            crate::rt_harness::audio_section(|| input_cb(&exact_in));
            crate::rt_harness::audio_section(|| output_cb(&mut exact_out));
            saw_output |= exact_out.iter().any(|s| s.abs() > 1e-6);
            crate::rt_harness::audio_section(|| input_cb(&big_in));
            crate::rt_harness::audio_section(|| output_cb(&mut big_out));
            saw_output |= big_out.iter().any(|s| s.abs() > 1e-6);
        }

        // The run has to have produced real audio somewhere, or the assertions above would hold
        // over callbacks that all returned early. Deliberately "somewhere across the run" rather
        // than "on the last callback": the oversized pair pushes more frames than the bridge ring
        // holds, so individual pulls legitimately underrun and pad with silence (that is what
        // `xruns` counts, and FR-IO-060's own test asserts it).
        assert!(
            saw_output,
            "every output callback produced silence -- nothing above was actually exercised"
        );
    }

    /// FR-IO-020: whatever share mode [`crate::app`] settled on reaches **both** backend opens
    /// unchanged. This module does not renegotiate, downgrade or second-guess it — the whole
    /// all-or-nothing rule (`crate::app::negotiate_share_mode`) would be undone by one direction
    /// quietly opening shared.
    #[test]
    fn the_settled_share_mode_reaches_both_stream_opens_unchanged() {
        for mode in [ShareMode::Shared, ShareMode::Exclusive] {
            let backend = FakeBackend::new();
            let _streams = open(
                setup_with_share_mode(&backend, 64, mode),
                engine(64),
                Arc::new(XrunCounter::new()),
                |_, _| {},
            )
            .unwrap();
            assert_eq!(backend.share_mode_asked_for(Direction::Input), Some(mode));
            assert_eq!(backend.share_mode_asked_for(Direction::Output), Some(mode));
        }
    }
}
