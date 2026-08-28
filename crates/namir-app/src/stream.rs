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
//! - Counts **both** of FR-IO-060's bridge dropouts directly: the output callback's underrun, via
//!   [`crate::bridge::BridgeConsumer::pull_into`]'s own return value, and — since issue #85 — the
//!   input callback's overrun, via [`crate::bridge::BridgeProducer::push_captured`]'s. `cpal`'s own
//!   `StreamFailure::Xrun` reports arrive through the same `on_failure` callback every other
//!   stream error does; classifying it into the same [`crate::xrun::XrunCounter`] (rather than
//!   surfacing it as a one-off notice the way `StreamFailure::DeviceLost`/`Other` are) is
//!   [`crate::app`]'s job, since that is also where the counter this module increments for
//!   bridge under- and overruns lives.
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
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::time::Duration;

use namir_core::ChannelConfig;
use namir_engine::{AudioEngine, StageIo};
use namir_platform::{DenormalGuard, ThreadPriorityOutcome, elevate_current_thread_priority};

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

/// `state` values for [`ThreadPriorityReport`]. Plain `u8`s rather than a `#[repr(u8)]` enum
/// because they live in an `AtomicU8` and the decode is a `match` either way.
const PRIORITY_PENDING: u8 = 0;
const PRIORITY_ELEVATED: u8 = 1;
const PRIORITY_DENIED: u8 = 2;
const PRIORITY_OS_ERROR: u8 = 3;
const PRIORITY_UNSUPPORTED: u8 = 4;
const PRIORITY_CONSUMED: u8 = 5;

/// Where the output callback leaves D-13.2's thread-elevation outcome for a non-audio thread to
/// read and report (issue #76).
///
/// # Why the outcome cannot simply be logged where it happens
///
/// A thread can only raise *its own* priority, and `cpal` offers no pre-callback hook, so
/// [`namir_platform::elevate_current_thread_priority`] has to be called from inside the first
/// output callback — see this module's own doc comment. That is the audio thread, where FR-ERR-030
/// forbids logging and formatting for logging, where `xtask rt-logging` fails the build if this
/// module so much as names the logger, and where D-7.5's harness fails on a `format!`. The
/// outcome is nevertheless worth having: `ThreadPriorityOutcome` is `#[must_use]` precisely
/// because "expected and non-fatal" is not "ignorable", and a user reporting xruns on Linux
/// deserves to be told their process never got the priority it asked for rather than to guess.
///
/// So the outcome travels instead of being reported: it is `Copy` and eight bytes, and this type
/// is the "an atomic ... is enough" carrier `ThreadPriorityOutcome::diagnostic`'s own doc comment
/// nominates. Posting is two atomic stores; [`crate::host::AppHost`] takes it on a later frame and
/// writes the FR-ERR-010 record from the UI thread.
///
/// **This is what `let _ = elevate_current_thread_priority();` used to be.** That discarded the
/// distinction between "elevated" and "the OS refused", which is the only distinction the value
/// carries.
#[derive(Debug)]
pub struct ThreadPriorityReport {
    /// One of the `PRIORITY_*` constants above.
    state: AtomicU8,
    /// The raw OS code behind [`PRIORITY_OS_ERROR`]; meaningless for every other state.
    os_error: AtomicI64,
}

impl Default for ThreadPriorityReport {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadPriorityReport {
    /// A report nothing has been posted to yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(PRIORITY_PENDING),
            os_error: AtomicI64::new(0),
        }
    }

    /// Records `outcome`. **RT-safe:** two atomic stores, no allocation, no lock, no formatting.
    /// Called once, from the output callback's first invocation.
    ///
    /// `pub(crate)` rather than private so [`crate::host`]'s own tests can post an outcome this
    /// machine does not produce — a `PermissionDenied` on a box that grants the elevation, say.
    /// Not `pub`: the only legitimate producer is this module's output callback.
    pub(crate) fn post(&self, outcome: ThreadPriorityOutcome) {
        let (state, os_error) = match outcome {
            ThreadPriorityOutcome::Elevated => (PRIORITY_ELEVATED, 0),
            ThreadPriorityOutcome::PermissionDenied => (PRIORITY_DENIED, 0),
            ThreadPriorityOutcome::OsError(code) => (PRIORITY_OS_ERROR, code),
            ThreadPriorityOutcome::Unsupported => (PRIORITY_UNSUPPORTED, 0),
        };
        self.os_error.store(os_error, Ordering::Relaxed);
        // Release, paired with the `Acquire` in `take`, so a reader that sees `PRIORITY_OS_ERROR`
        // also sees the code stored just above it.
        self.state.store(state, Ordering::Release);
    }

    /// The outcome the audio thread posted, **once**: a second call returns `None` unless the
    /// audio thread has posted again, so a caller polling every frame reports one notice rather
    /// than one per frame. `None` also while the first output callback has not yet run — a stream
    /// that never starts never elevates anything, and has nothing to say about it.
    #[must_use]
    pub fn take(&self) -> Option<ThreadPriorityOutcome> {
        match self.state.swap(PRIORITY_CONSUMED, Ordering::AcqRel) {
            PRIORITY_ELEVATED => Some(ThreadPriorityOutcome::Elevated),
            PRIORITY_DENIED => Some(ThreadPriorityOutcome::PermissionDenied),
            PRIORITY_OS_ERROR => Some(ThreadPriorityOutcome::OsError(
                self.os_error.load(Ordering::Relaxed),
            )),
            PRIORITY_UNSUPPORTED => Some(ThreadPriorityOutcome::Unsupported),
            // `PRIORITY_PENDING` (nothing posted yet) and `PRIORITY_CONSUMED` (already reported)
            // are both "nothing to say"; writing `CONSUMED` over `PENDING` is harmless because
            // `post` stores unconditionally, so a later post is still seen by a later `take`.
            _ => None,
        }
    }
}

/// The running duplex path. Dropping this stops both streams (`AudioStream`'s own drop-stops
/// contract, per `crate::audio_io`'s doc comment).
pub struct RunningStreams {
    _input: Box<dyn AudioStream>,
    _output: Box<dyn AudioStream>,
    thread_priority: Arc<ThreadPriorityReport>,
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

    /// D-13.2's elevation outcome, for a non-audio thread to report (issue #76). Handed to
    /// [`crate::host::AppHost`] by [`crate::app::run`]; see [`ThreadPriorityReport`] for why the
    /// outcome travels rather than being reported where it is produced.
    #[must_use]
    pub fn thread_priority(&self) -> Arc<ThreadPriorityReport> {
        Arc::clone(&self.thread_priority)
    }
}

/// Opens the duplex path described by `setup`, running `engine` from the output callback.
///
/// `on_input_failure`/`on_output_failure` are called (from whichever callback thread detected it,
/// per FR-IO-070's "shall not crash or hang") with why that side failed; the caller is expected to
/// stop using the returned [`RunningStreams`] and report the condition to the user (FR-IO-070's
/// own wording) — this function does not itself decide when the stream is unrecoverable, since
/// that judgement belongs to whatever owns retry/reselection policy ([`crate::app`]).
///
/// **One callback per direction, `FnMut`, rather than one shared `Fn` (issue #88).** These run on
/// `cpal`'s error-callback threads, which are the streams' own threads — so they are audio-thread
/// code, and the caller has to be able to put a *pre-allocated, single-producer* sink in each one
/// rather than format a message and send it down an `mpsc` channel. A single `Fn + Sync` shared
/// between both directions cannot hold one: two threads would be writing one producer. Splitting
/// the parameter is what lets [`crate::app::stream_failure_sink`] own an `rtrb::Producer` per
/// direction, which is what makes the whole path allocation-free.
pub fn open(
    setup: StreamSetup<'_>,
    engine: AudioEngine,
    xruns: Arc<XrunCounter>,
    on_input_failure: impl FnMut(StreamFailure) + Send + 'static,
    on_output_failure: impl FnMut(StreamFailure) + Send + 'static,
) -> Result<RunningStreams, crate::audio_io::AudioIoError> {
    let capacity = (setup.max_block_size.max(1) * 8).next_power_of_two();
    let (producer, consumer) = bridge(capacity);

    let input_channel_index = setup.input_channel_index as usize;
    let input_channels = setup.input_params.channels as usize;

    let input_stream = build_input(
        &setup,
        producer,
        input_channel_index,
        input_channels,
        Arc::clone(&xruns),
        Box::new(on_input_failure),
    )?;
    let thread_priority = Arc::new(ThreadPriorityReport::new());
    let output_stream = match build_output(
        &setup,
        engine,
        consumer,
        Arc::clone(&xruns),
        Arc::clone(&thread_priority),
        Box::new(on_output_failure),
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
        thread_priority,
    })
}

fn build_input(
    setup: &StreamSetup<'_>,
    mut producer: BridgeProducer,
    channel_index: usize,
    channel_count: usize,
    xruns: Arc<XrunCounter>,
    on_error: Box<dyn FnMut(StreamFailure) + Send>,
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
            // FR-IO-060's *other* dropout, and until issue #85 it was thrown away: this return
            // value is how many captured samples did not fit because the ring was full, which is
            // a real dropout of exactly the class `crate::bridge` exists to detect. Discarding it
            // did not merely lose detail — it made the session count under-report, which is the
            // worst direction for a diagnostic, because a user watching a zero while their audio
            // glitches concludes the counter works and the glitch is elsewhere. Counted the same
            // way `build_output` counts an underrun below: one xrun per callback chunk that lost
            // anything, not one per lost sample, so the two sources are commensurable.
            if producer.push_captured(&mono_scratch) > 0 {
                xruns.record();
            }
        }
    });
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
    thread_priority: Arc<ThreadPriorityReport>,
    on_error: Box<dyn FnMut(StreamFailure) + Send>,
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
            // happen. A denial is expected and non-fatal (that module's own doc comment), so
            // nothing here reacts to it -- but it is no longer *discarded* (issue #76): the
            // `#[must_use]` outcome is posted, in two atomic stores, for `crate::host` to turn
            // into an FR-ERR-010 record from the UI thread. This module may not name the logger
            // (`xtask rt-logging`) and may not `format!` (D-7.5), which is exactly why the value
            // travels instead of being reported here.
            thread_priority.post(elevate_current_thread_priority());
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
    /// The *error* callbacks each direction was opened with. Captured since issue #88, because
    /// they are audio-thread code too — `cpal` invokes them on the stream's own thread — and until
    /// then this fake dropped them on the floor, so nothing in this crate had ever run one.
    pub(crate) input_error: std::sync::Mutex<Option<ErrorCallback>>,
    /// As [`FakeBackend::input_error`], for the playback direction.
    pub(crate) output_error: std::sync::Mutex<Option<ErrorCallback>>,
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
            input_error: std::sync::Mutex::new(None),
            output_error: std::sync::Mutex::new(None),
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
pub(crate) type ErrorCallback = Box<dyn FnMut(StreamFailure) + Send>;

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
        on_error: Box<dyn FnMut(StreamFailure) + Send>,
        _timeout: Duration,
    ) -> Result<Box<dyn AudioStream>, AudioIoError> {
        self.asked_share_modes
            .lock()
            .unwrap()
            .push((Direction::Input, params.share_mode));
        *self.input_data.lock().unwrap() = Some(on_data);
        *self.input_error.lock().unwrap() = Some(on_error);
        Ok(Box::new(FakeStream))
    }
    fn build_output_stream(
        &self,
        _host: &HostInfo,
        _device: &DeviceInfo,
        params: StreamParams,
        on_data: Box<dyn FnMut(&mut [f32]) + Send>,
        on_error: Box<dyn FnMut(StreamFailure) + Send>,
        _timeout: Duration,
    ) -> Result<Box<dyn AudioStream>, AudioIoError> {
        self.asked_share_modes
            .lock()
            .unwrap()
            .push((Direction::Output, params.share_mode));
        *self.output_data.lock().unwrap() = Some(on_data);
        *self.output_error.lock().unwrap() = Some(on_error);
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
            {
                let failures = Arc::clone(&failures_clone);
                move |_f| {
                    failures.fetch_add(1, Ordering::SeqCst);
                }
            },
            move |_f| {
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
            |_| {},
            |_| {},
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
            |_| {},
            |_| {},
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
    ///
    /// **The warm-up drives the exact-size pair only, and that is load-bearing (issue #87).** It
    /// used to drive the oversized pair as well, which grew `build_input`'s `mono_scratch` to the
    /// oversized length *before* the harness was armed — so the very regression this test is cited
    /// as catching, an unchunked `extend` past the reservation, passed it. Re-planting the
    /// unchunked form with the old warm-up in place is green; with this one it fails. Nothing on
    /// the output side needs the oversized warm-up: its three buffers are sized at
    /// `max_block_size` and its chunking loop keeps every write inside them.
    #[test]
    fn the_audio_callbacks_this_module_builds_allocate_nothing() {
        const MAX_BLOCK: usize = 64;
        let backend = FakeBackend::new();
        let xruns = Arc::new(XrunCounter::new());
        let _streams = open(
            setup(&backend, MAX_BLOCK),
            engine(MAX_BLOCK),
            Arc::clone(&xruns),
            |_| {},
            |_| {},
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

        // Warm-up, un-asserted, and deliberately *only* the exact-size pair: see this test's own
        // doc comment for why warming up with the oversized pair blinded it to issue #87.
        input_cb(&exact_in);
        output_cb(&mut exact_out);

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

    /// **FR-IO-060's capture-side dropout (issue #85).** `BridgeProducer::push_captured` returns
    /// how many samples did not fit because the ring was full, and [`build_input`] used to discard
    /// it — so whenever capture outran the output callback the samples were dropped and the
    /// session count stayed at zero. Under-reporting is the worst direction for a diagnostic: a
    /// user watching a stuck zero while their audio glitches concludes the counter works and looks
    /// elsewhere.
    ///
    /// Driven by pushing input with nothing ever pulling: the ring holds
    /// `(max_block * 8).next_power_of_two()` samples, so the first few callbacks fit and must
    /// count nothing, and the ones past that overrun and must.
    // trace-partial: FR-IO-060
    // uncovered: FR-IO-060 — the "resettable by the user" clause has no path to exercise:
    // uncovered: XrunCounter::reset has no caller outside its own two unit tests and no UiIntent
    // uncovered: reaches it, and the running count surfaces only through an eprintln! rather than
    // uncovered: anywhere in the window; closes M8
    #[test]
    fn input_capture_that_outruns_the_output_callback_counts_an_xrun() {
        const MAX_BLOCK: usize = 64;
        let backend = FakeBackend::new();
        let xruns = Arc::new(XrunCounter::new());
        let _streams = open(
            setup(&backend, MAX_BLOCK),
            engine(MAX_BLOCK),
            Arc::clone(&xruns),
            |_| {},
            |_| {},
        )
        .unwrap();
        let mut input_cb = backend.input_data.lock().unwrap().take().unwrap();

        // Comfortably inside the ring's capacity: nothing is lost, so nothing may be counted.
        for _ in 0..4 {
            input_cb(&[0.1f32; MAX_BLOCK]);
        }
        assert_eq!(
            xruns.count(),
            0,
            "capture that fits in the ring is not a dropout"
        );

        // Far past it, still with no output callback draining anything.
        for _ in 0..32 {
            input_cb(&[0.1f32; MAX_BLOCK]);
        }
        assert!(
            xruns.count() > 0,
            "capture that overran the bridge ring must reach the session's xrun count"
        );
    }

    /// FR-IO-070, and the wiring half of issue #88: each direction's `cpal` error callback reaches
    /// **that direction's** sink and no other. The two are now separate `FnMut`s rather than one
    /// shared `Fn` taking a [`Direction`], so a crossed pair would report an input fault as an
    /// output one — and would be invisible, since neither closure is handed a direction any more.
    ///
    /// [`FakeBackend`] captures both error callbacks for this test; before issue #88 it dropped
    /// them, so nothing in this crate had ever driven one.
    #[test]
    fn each_directions_error_callback_reaches_only_that_directions_sink() {
        let backend = FakeBackend::new();
        let seen: Arc<std::sync::Mutex<Vec<(Direction, StreamFailure)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let input_seen = Arc::clone(&seen);
        let output_seen = Arc::clone(&seen);
        let _streams = open(
            setup(&backend, 64),
            engine(64),
            Arc::new(XrunCounter::new()),
            move |f| input_seen.lock().unwrap().push((Direction::Input, f)),
            move |f| output_seen.lock().unwrap().push((Direction::Output, f)),
        )
        .unwrap();

        let mut input_err = backend.input_error.lock().unwrap().take().unwrap();
        let mut output_err = backend.output_error.lock().unwrap().take().unwrap();
        let driver_fault = StreamFailure::Other(crate::audio_io::InlineDetail::from("OS Error -1"));
        output_err(driver_fault);
        input_err(StreamFailure::DeviceLost);

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (Direction::Output, driver_fault),
                (Direction::Input, StreamFailure::DeviceLost),
            ]
        );
    }

    /// **Issue #76: D-13.2's elevation outcome is carried off the audio thread, not discarded.**
    /// `elevate_current_thread_priority` returns a `#[must_use]` outcome and this callback used to
    /// answer it with `let _ = ...`, throwing away the one distinction it carries — "elevated" vs.
    /// "the OS refused" — which is exactly the diagnostic a user reporting xruns on a Linux box
    /// with no `rtprio` allowance needs.
    ///
    /// Whatever this machine's OS answers is a property of the machine, not of this code, so what
    /// is asserted is the mechanism: nothing is readable before the first callback; something
    /// definite is readable after it; and it reads **once**, so a host polling every frame reports
    /// one notice rather than one per frame.
    #[test]
    fn the_first_output_callback_posts_its_elevation_outcome_for_a_non_audio_thread() {
        let backend = FakeBackend::new();
        let streams = open(
            setup(&backend, 64),
            engine(64),
            Arc::new(XrunCounter::new()),
            |_| {},
            |_| {},
        )
        .unwrap();
        let report = streams.thread_priority();
        assert!(
            report.take().is_none(),
            "nothing has run yet, so there is nothing to report"
        );

        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();
        let mut out = [0.0f32; 128];
        output_cb(&mut out);

        assert!(
            report.take().is_some(),
            "the first output callback must post an outcome, whatever this OS answered"
        );
        assert!(
            report.take().is_none(),
            "a posted outcome is reported once, not once per frame"
        );

        // Later callbacks do not elevate again (the one-shot flag), so nothing more appears.
        output_cb(&mut out);
        assert!(report.take().is_none());
    }

    /// The carrier itself, over every outcome `namir-platform` can produce -- including the one
    /// this machine does not produce. `OsError`'s payload has to survive, since FR-ERR-050's
    /// bundle is the intended consumer of that number.
    #[test]
    fn every_elevation_outcome_survives_the_atomic_round_trip() {
        for outcome in [
            ThreadPriorityOutcome::Elevated,
            ThreadPriorityOutcome::PermissionDenied,
            ThreadPriorityOutcome::OsError(-2_147_024_882),
            ThreadPriorityOutcome::Unsupported,
        ] {
            let report = ThreadPriorityReport::new();
            report.post(outcome);
            assert_eq!(report.take(), Some(outcome));
            assert_eq!(report.take(), None);
        }
    }

    /// Posting is what the audio callback does, so it must allocate nothing -- two atomic stores
    /// and no formatting, which is the whole reason the outcome travels rather than being logged
    /// where it is produced.
    #[test]
    fn posting_an_elevation_outcome_allocates_nothing() {
        let report = ThreadPriorityReport::new();
        crate::rt_harness::audio_section(|| {
            report.post(ThreadPriorityOutcome::OsError(5));
            report.post(ThreadPriorityOutcome::Elevated);
        });
        assert_eq!(report.take(), Some(ThreadPriorityOutcome::Elevated));
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
                |_| {},
                |_| {},
            )
            .unwrap();
            assert_eq!(backend.share_mode_asked_for(Direction::Input), Some(mode));
            assert_eq!(backend.share_mode_asked_for(Direction::Output), Some(mode));
        }
    }
}
