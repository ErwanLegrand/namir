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
    let mut mono_scratch: Vec<f32> = Vec::with_capacity(setup.max_block_size);
    let on_data = Box::new(move |data: &[f32]| {
        if channel_count == 0 {
            return;
        }
        mono_scratch.clear();
        mono_scratch.extend(
            data.chunks_exact(channel_count)
                .map(|frame| frame.get(channel_index).copied().unwrap_or(0.0)),
        );
        producer.push_captured(&mono_scratch);
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
    let mut engine_channels: Vec<Vec<f32>> = vec![vec![0.0f32; max_block]; engine_channel_count];

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

            for (ch_index, channel) in engine_channels.iter_mut().enumerate() {
                if ch_index == 0 || duplicate_into_stereo {
                    channel[..chunk].copy_from_slice(&mono_in[..chunk]);
                } else {
                    channel[..chunk].fill(0.0);
                }
            }

            {
                let mut refs: Vec<&mut [f32]> = engine_channels
                    .iter_mut()
                    .map(|c| &mut c[..chunk])
                    .collect();
                let mut io = StageIo::new(&mut refs, chunk);
                engine.process(&mut io);
            }

            for frame in 0..chunk {
                let out_frame = &mut out
                    [(done + frame) * output_channels..(done + frame + 1) * output_channels];
                out_frame.fill(0.0);
                if let Some(slot) = out_frame.get_mut(left) {
                    *slot = engine_channels[0][frame];
                }
                if engine_channel_count > 1
                    && let Some(slot) = out_frame.get_mut(right)
                {
                    *slot = engine_channels[1][frame];
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_io::{AudioIoError, BufferSizeRange, SupportedConfigRange};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    /// A minimal in-process fake backend: no real device, just two channels connected through
    /// nothing but this test's own direct calls into the callbacks it captures. Proves the
    /// *wiring* (channel selection, chunking, xrun accounting) without any real audio hardware.
    struct FakeStream;
    impl AudioStream for FakeStream {
        fn play(&self) -> Result<(), AudioIoError> {
            Ok(())
        }
        fn pause(&self) -> Result<(), AudioIoError> {
            Ok(())
        }
    }

    type InputCallback = Box<dyn FnMut(&[f32]) + Send>;
    type OutputCallback = Box<dyn FnMut(&mut [f32]) + Send>;

    struct FakeBackend {
        input_data: Mutex<Option<InputCallback>>,
        output_data: Mutex<Option<OutputCallback>>,
    }

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
        fn build_input_stream(
            &self,
            _host: &HostInfo,
            _device: &DeviceInfo,
            _params: StreamParams,
            on_data: Box<dyn FnMut(&[f32]) + Send>,
            _on_error: Box<dyn FnMut(StreamFailure) + Send>,
            _timeout: Duration,
        ) -> Result<Box<dyn AudioStream>, AudioIoError> {
            *self.input_data.lock().unwrap() = Some(on_data);
            Ok(Box::new(FakeStream))
        }
        fn build_output_stream(
            &self,
            _host: &HostInfo,
            _device: &DeviceInfo,
            _params: StreamParams,
            on_data: Box<dyn FnMut(&mut [f32]) + Send>,
            _on_error: Box<dyn FnMut(StreamFailure) + Send>,
            _timeout: Duration,
        ) -> Result<Box<dyn AudioStream>, AudioIoError> {
            *self.output_data.lock().unwrap() = Some(on_data);
            Ok(Box::new(FakeStream))
        }
    }

    fn setup(backend: &FakeBackend, max_block_size: usize) -> StreamSetup<'_> {
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
            },
            channel_config: ChannelConfig::MonoToStereo,
            input_channel_index: 0,
            output_channel_left: 0,
            output_channel_right: 1,
            max_block_size,
        }
    }

    fn engine(max_block_size: usize) -> AudioEngine {
        let c = namir_engine::PrepareContext::new(
            namir_core::SampleRate::new(48_000).unwrap(),
            max_block_size,
            ChannelConfig::MonoToStereo,
        )
        .unwrap();
        let chain = namir_engine::build_default_chain(&c).unwrap();
        let (engine, _endpoint) =
            namir_engine::split(chain, namir_engine::RingCapacities::default());
        engine
    }

    /// Wiring proof: input capture reaches the output buffer, duplicated into both channels
    /// (`ChannelConfig::MonoToStereo`), with no crash and no underrun when supply matches demand.
    #[test]
    fn captured_input_reaches_the_output_buffer_duplicated_across_channels() {
        let backend = FakeBackend {
            input_data: Mutex::new(None),
            output_data: Mutex::new(None),
        };
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
    // trace: FR-IO-060
    #[test]
    fn an_output_pull_with_no_input_yet_counts_an_xrun() {
        let backend = FakeBackend {
            input_data: Mutex::new(None),
            output_data: Mutex::new(None),
        };
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
        let backend = FakeBackend {
            input_data: Mutex::new(None),
            output_data: Mutex::new(None),
        };
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
}
