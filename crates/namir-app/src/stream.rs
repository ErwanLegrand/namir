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
//! - Counts **all three** of FR-IO-060's dropout sources directly: the output callback's bridge
//!   underrun, via [`crate::bridge::BridgeConsumer::pull_into`]'s own return value; the input
//!   callback's bridge overrun, via [`crate::bridge::BridgeProducer::push_captured`]'s (issue
//!   #85); and — since issue #200 item 6 — the backend's own per-callback report, which `cpal`
//!   0.19 delivers through `CallbackInfo::xrun()` and which reaches this module as
//!   [`crate::audio_io::CallbackStatus::xrun`]. All three increment the same
//!   [`crate::xrun::XrunCounter`] at the same granularity — one xrun per callback that lost
//!   anything, never one per lost sample — so the counts stay commensurable. The backend's
//!   report is counted unconditionally; only the bridge pads are gated on the settling window
//!   below, because only they can be an artefact of activation.
//!   Since issue #189 the output side's pads are gated on this `open`'s pair having settled: the
//!   capture side must have delivered a callback, and the ring must have had a bounded settling
//!   window to fill. An output device takes a few hundred milliseconds to start running, and a
//!   callback's demand need not divide into the captured block size, so the pads before then are
//!   activation rather than dropouts — they used to open every session, and every settings
//!   reopen, at a non-zero count.
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
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::time::Duration;

use namir_core::ChannelConfig;
use namir_engine::{AudioEngine, StageIo};
use namir_platform::{DenormalGuard, ThreadPriorityOutcome, elevate_current_thread_priority};

use crate::audio_io::{
    AudioBackend, AudioStream, CallbackStatus, DeviceInfo, HostInfo, StreamFailure, StreamParams,
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
/// contract, per `crate::audio_io`'s doc comment) — **output side first**, see this type's
/// [`Drop`] impl, where that order is the whole of issue #194's fix.
pub struct RunningStreams {
    /// `Option` only so [`Drop`] can stop the two sides in a deliberate order instead of the
    /// order they happen to be declared in; `Some` for the whole observable life of the value.
    input: Option<Box<dyn AudioStream>>,
    output: Option<Box<dyn AudioStream>>,
    thread_priority: Arc<ThreadPriorityReport>,
}

impl Drop for RunningStreams {
    /// **Output side first, input side second, and the order is load-bearing (issue #194).**
    ///
    /// Stopping a stream *is* dropping it here, and a stream not yet dropped is still being
    /// serviced by the driver. Stop the input side first and every output callback in the
    /// teardown window pulls a bridge nobody is feeding any more:
    /// [`crate::bridge::BridgeConsumer::pull_into`] pads, `build_output` calls
    /// `XrunCounter::record`, and FR-IO-060's session count — the number a user reads as "my
    /// audio glitched" — gains a dropout the shutdown itself invented. In this order no such
    /// pull happens at all; the input side then stops into a bridge nobody reads, and pushes
    /// nothing drains count nothing until the ring fills. That headroom is a margin, not a
    /// proof, and it scales with the block: capacity is `(max_block * 8).next_power_of_two()`
    /// less the one-block prefill, so ~75 ms at a 480-frame block but ~4.7 ms at the 32-frame
    /// minimum `STANDARD_BUFFER_SIZES` offers. An overrun there is not silent either —
    /// `build_input` records one xrun per losing chunk — so at the small end this trades an
    /// output-side pad for an input-side overrun on a stop that takes more than a few
    /// milliseconds, rather than eliminating the possibility. Better than the old order by seven
    /// blocks of slack rather than one, at every block size — the old order's only slack against
    /// an output pull was that same one-block prefill. It is worse only if closing the *output*
    /// side takes some seven blocks longer than closing the input side (4.7 ms at the 32-frame
    /// minimum), which no observed teardown does; that precondition, not block size, is what the
    /// claim rests on.
    ///
    /// Written out here rather than obtained by declaring the two fields the other way round,
    /// so a later field reorder cannot silently reintroduce the counted teardown pad.
    fn drop(&mut self) {
        drop(self.output.take());
        drop(self.input.take());
    }
}

/// Both stream sides are `Some` until `Drop` takes them; see [`RunningStreams::play`].
const BOTH_SIDES_UNTIL_DROP: &str = "both stream sides are Some until Drop takes them";

impl RunningStreams {
    /// Starts both streams. Built paused by `cpal`'s own contract; this is the one call that
    /// actually makes audio flow.
    ///
    /// **Input first, deliberately the mirror image of the stop order.** Starting is the one
    /// point where the capture side has to lead: the bridge must already be filling when the
    /// output callback starts pulling it, or the first pulls pad — the same reason [`open`]
    /// prefills a block.
    ///
    /// Both fields are `Some` for the whole observable life of a `RunningStreams` — they are
    /// private and [`open`] is the only constructor, `Drop` being the only thing that takes them
    /// — so a `None` here is a bug in a later refactor, not a state to tolerate. `expect` rather
    /// than a silent no-op because the symptom of tolerating it is "reports success, no audio
    /// flows", which this crate's callback plumbing gives no other signal for.
    pub fn play(&self) -> Result<(), crate::audio_io::AudioIoError> {
        self.input.as_ref().expect(BOTH_SIDES_UNTIL_DROP).play()?;
        self.output.as_ref().expect(BOTH_SIDES_UNTIL_DROP).play()
    }

    /// Pauses both streams without closing them.
    ///
    /// No production caller today — the app stops by dropping, never by pausing — so this is the
    /// order for whenever one appears rather than a cost anything currently pays. It matches the
    /// stop order for the same reason: an input side paused under a still-running output side
    /// would be exactly the unfed-bridge window [`Drop`] avoids, and would charge FR-IO-060's
    /// counter for a pause.
    ///
    /// The one path the ordering fix does not cover: `?` on the output side leaves the input side
    /// running, which is the mirror of the bug being fixed — the capture side then pushes into a
    /// bridge nobody drains. [`play`](Self::play)'s `?` has the same one-sided-failure shape.
    /// Both are unreachable while nothing calls `pause`, and a caller that appears has to decide
    /// what a half-paused pair means before this is worth building out.
    pub fn pause(&self) -> Result<(), crate::audio_io::AudioIoError> {
        self.output.as_ref().expect(BOTH_SIDES_UNTIL_DROP).pause()?;
        self.input.as_ref().expect(BOTH_SIDES_UNTIL_DROP).pause()
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
    let (mut producer, consumer) = bridge(capacity);
    // One block of silence ahead of the first pull. The two callbacks are independently scheduled
    // (see `crate::bridge`), so with an empty ring the steady-state occupancy sits at zero and any
    // output callback that happens to run before its matching input one pads and counts an xrun --
    // perpetually, not just at startup. Prefilling gives the pull one block of slack to absorb that
    // jitter. The cost is one block of added latency, which `crate::latency::estimate_round_trip`
    // accounts for as its `bridge_prefill_frames` term.
    let dropped = producer.push_captured(&vec![0.0; setup.max_block_size.max(1)]);
    debug_assert_eq!(
        dropped, 0,
        "fresh bridge with capacity >= 8 * max_block_size cannot drop prefill"
    );

    // Issue #189: the output stream's device activation is not a dropout. On a healthy session
    // the output callback fires once, then the device takes a few hundred milliseconds to start
    // running (measured on §2's interface: one callback, a ~524 ms gap, then an exact 10.000 ms
    // cadence for the rest of the run). The bridge is pulled across that gap and legitimately has
    // nothing beyond the prefill above, so `pull_into` pads and every healthy session used to
    // open at `xrun count is now 2` before the user played a note. This latch says whether the
    // capture side has ever delivered a callback; it is what anchors [`build_output`]'s settling
    // window, so an activation gap of any length is covered rather than a fixed number of
    // callbacks from here. It is created per `open`, not per session — `crate::host`'s reopen
    // path re-enters this function for every device/rate/buffer change, and each reopen pays the
    // same transient.
    let capture_started = Arc::new(AtomicBool::new(false));

    let input_channel_index = setup.input_channel_index as usize;
    let input_channels = setup.input_params.channels as usize;

    let input_stream = build_input(
        &setup,
        producer,
        input_channel_index,
        input_channels,
        Arc::clone(&xruns),
        Arc::clone(&capture_started),
        Box::new(on_input_failure),
    )?;
    let thread_priority = Arc::new(ThreadPriorityReport::new());
    let output_stream = match build_output(
        &setup,
        engine,
        consumer,
        Arc::clone(&xruns),
        Arc::clone(&capture_started),
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
        input: Some(input_stream),
        output: Some(output_stream),
        thread_priority,
    })
}

fn build_input(
    setup: &StreamSetup<'_>,
    mut producer: BridgeProducer,
    channel_index: usize,
    channel_count: usize,
    xruns: Arc<XrunCounter>,
    capture_started: Arc<AtomicBool>,
    on_error: Box<dyn FnMut(StreamFailure) + Send>,
) -> Result<Box<dyn AudioStream>, crate::audio_io::AudioIoError> {
    let max_block = setup.max_block_size.max(1);
    let mut mono_scratch: Vec<f32> = Vec::with_capacity(max_block);
    let on_data = Box::new(move |data: &[f32], status: CallbackStatus| {
        // Issue #200 item 6: `cpal` 0.19 reports a device-side dropout per callback, and this is
        // the only place it can be counted. One relaxed atomic increment, before the early return
        // below, because a lost callback is a lost callback whatever this stream then does with
        // it. Same granularity as the bridge detector: one xrun per reporting callback.
        if status.xrun {
            xruns.record();
        }
        if channel_count == 0 {
            return;
        }
        // Issue #189: the capture side is running, so from here on an output pad is a real
        // dropout rather than the output device's activation transient. A relaxed store per
        // callback rather than a compare-and-swap: this is a monotonic latch, and the pull side
        // only needs to see it eventually, within a callback or two of the first real audio.
        capture_started.store(true, Ordering::Relaxed);
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
    capture_started: Arc<AtomicBool>,
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
    // Issue #189: a freshly opened pair does not reach its steady-state bridge occupancy
    // immediately, and the silence it pads in on the way there is its activation rather than
    // FR-IO-060's "audio dropout". Two things happen at once. The output device takes a few
    // hundred milliseconds to start running (measured on §2's interface: one callback, a ~524 ms
    // gap, then an exact cadence for the rest of the run), which `capture_started` in `open`
    // covers; and once both sides are running, the ring still has to fill, because a callback's
    // demand need not divide into the capture side's block — measured here, WASAPI shared asks
    // 480 frames per output callback against 256-frame captured blocks, so the pull chunked at
    // 256 + 224 starves once more before occupancy builds. Counting is therefore suppressed for
    // a settling window after the capture side's first callback.
    //
    // Both budgets are wall-clock, not pull counts (PR #206 review): a pull is at most
    // `max_block` frames, and `STANDARD_BUFFER_SIZES` offers 32..=2048, so a fixed pull count
    // would swing 64x in real time — at 32 frames a 512-pull ceiling is ~341 ms, *below* the
    // ~524 ms activation it exists to outlast, and issue #189 would come back at exactly the
    // buffer sizes a latency-sensitive user picks. So the settling window is `SETTLING_MS`
    // (100 ms, at or above the ~85 ms the 256-frame reference machine was measured with) and the
    // ceiling is `ACTIVATION_MS` (3 s, comfortably past the measured ~524 ms activation at every
    // offered size and rate). Both are converted to pulls here, once per open, outside the
    // callback — the RT path keeps its two counters, one relaxed load and one comparison, and no
    // division.
    //
    // The ceiling bounds the *other* half. Waiting on `capture_started` alone means an input
    // device that opens and then silently delivers nothing — which raises no `StreamFailure`,
    // since there is no error to report — suppresses every pad for the life of the stream, so
    // the one failure FR-IO-060 matters most for would read a clean 0. Finite, so a dead capture
    // side starts counting.
    //
    // A genuine dropout inside the settling window is not counted, which is the deliberate
    // trade — the alternative is every session and every settings change opening at a non-zero
    // count, which is what a user reads as "my audio glitched" before they have played a note.
    //
    // Both pieces of state are local to this closure, so both are per-open by construction:
    // `crate::host::apply_audio_reopen` builds a new one for every device, rate or buffer change,
    // and each reopen pays the same transient.
    const SETTLING_MS: u64 = 100;
    const ACTIVATION_MS: u64 = 3_000;
    let pulls_per_second = setup.output_params.sample_rate_hz as u64 / max_block as u64;
    let settling_pulls = (pulls_per_second * SETTLING_MS / 1_000).max(1) as u32;
    let activation_pulls_max = (pulls_per_second * ACTIVATION_MS / 1_000).max(1) as u32;
    let mut pulls_since_capture: u32 = 0;
    let mut pulls: u32 = 0;

    let on_data = Box::new(move |out: &mut [f32], status: CallbackStatus| {
        // Issue #200 item 6: the backend's own dropout report, counted unconditionally — unlike
        // the bridge pads below it is not an artefact of this pair's activation transient, so the
        // settling window does not apply to it.
        if status.xrun {
            xruns.record();
        }
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
            pulls = pulls.saturating_add(1);
            if capture_started.load(Ordering::Relaxed) {
                pulls_since_capture = pulls_since_capture.saturating_add(1);
            }
            let settled = pulls_since_capture > settling_pulls || pulls > activation_pulls_max;
            if padded > 0 && settled {
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
///
/// # It is also FR-IO-070's failable virtual device (issue #24, §22 **R-5**)
///
/// FR-IO-070's stated method is *"I with a virtual device that can be made to fail on demand"*, and
/// for two milestones no such device existed — the requirement's own apparatus was missing, which
/// is what issue #24 is about. It is this type, because this is where D-13.1's Namir-owned trait
/// already puts the seam: making a fake backend fail needs no OS device manipulation, no
/// `#[cfg(target_os)]` (which D-5.1 forbids outside `namir-platform` anyway) and no hardware, so
/// the method runs on a headless CI container exactly as it would on the reference machine.
///
/// Two kinds of failure, matching the two halves of the requirement's first sentence:
///
/// - **A device that fails to open** — [`FakeBackend::failing_to_open`], which makes that
///   direction's `build_*_stream` return [`AudioIoError::OpenFailed`].
/// - **A device that fails while in use** — [`FakeBackend::input_error`]/
///   [`FakeBackend::output_error`], the error callbacks captured since issue #88, fired by the
///   test at whatever point in the stream's life it chooses.
///
/// What a stream then *did* is readable from [`FakeBackend::input_stream`]/
/// [`FakeBackend::output_stream`]: FR-IO-070's "stop the stream cleanly" is not observable from a
/// backend that only records the callbacks it was handed.
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
    /// What the capture direction's stream was told to do, and whether it has been stopped —
    /// FR-IO-070's "stop the stream cleanly" needs an observable, and the callbacks above are not
    /// one. Shared with the [`FakeStream`] handed back by `build_input_stream`, so it survives that
    /// stream being dropped, which is precisely the event it has to record.
    pub(crate) input_stream: Arc<FakeStreamLog>,
    /// As [`FakeBackend::input_stream`], for the playback direction.
    pub(crate) output_stream: Arc<FakeStreamLog>,
    /// Directions whose `build_*_stream` fails outright rather than returning a stream — the
    /// open-failure half of FR-IO-070's fault injection. See [`FakeBackend::failing_to_open`].
    open_failures: Vec<Direction>,
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
    /// What this backend reports when asked for **exclusive** configs, per direction. `None`
    /// means "the same ranges as shared", which is what a backend with no WASAPI endpoint behind
    /// it does. Per direction rather than per backend for the same reason
    /// [`FakeBackend::granting_exclusive_to`] is per device: a single shared answer cannot catch
    /// a direction mix-up in the code it exercises, and picking the wrong direction is precisely
    /// the class of bug issue #190 was.
    exclusive_input_configs: Option<Vec<SupportedConfigRange>>,
    exclusive_output_configs: Option<Vec<SupportedConfigRange>>,
    /// Every `(direction, share_mode)` a config query was made with, in call order — the
    /// observable for *which mode was enumerated*, and the only way to see issue #190's two-pass
    /// sequence. [`FakeBackend::asked_share_modes`] is its counterpart for the stream open.
    enumerated_share_modes: std::sync::Mutex<Vec<(Direction, ShareMode)>>,
    input_devices: Vec<DeviceInfo>,
    output_devices: Vec<DeviceInfo>,
}

#[cfg(test)]
impl FakeBackend {
    /// A backend that refuses exclusive mode on every device — the interim real-world answer.
    pub(crate) fn new() -> Self {
        let (input_stream, output_stream) = FakeStreamLog::pair();
        Self {
            input_data: std::sync::Mutex::new(None),
            output_data: std::sync::Mutex::new(None),
            input_error: std::sync::Mutex::new(None),
            output_error: std::sync::Mutex::new(None),
            input_stream,
            exclusive_input_configs: None,
            exclusive_output_configs: None,
            enumerated_share_modes: std::sync::Mutex::new(Vec::new()),
            output_stream,
            open_failures: Vec::new(),
            exclusive_devices: Vec::new(),
            asked_share_modes: std::sync::Mutex::new(Vec::new()),
            input_devices: Vec::new(),
            output_devices: Vec::new(),
        }
    }

    /// Makes the exclusive-mode config query answer with these ranges instead of the shared ones —
    /// the WASAPI shape, where the two modes describe different devices (issue #190).
    ///
    /// Per direction, and `Option` per direction, for two reasons. A single answer for both
    /// directions cannot catch a `Direction` mix-up in the code it exercises, which is the class of
    /// bug issue #190 itself was. And `None` on one side only is a real hardware shape — a capture
    /// endpoint with a reachable WASAPI exclusive endpoint beside a render device without one — so
    /// it is what makes `negotiate_audio`'s "either direction answered exclusive" gate testable.
    ///
    /// An **empty** `Some` answers shared, exactly as [`crate::audio_io::CpalBackend`] does: its
    /// `exclusive_configs_when_asked` maps an empty exclusive answer to the shared query, so a fake
    /// that reported `share_mode: Exclusive` with no ranges would claim a state the real backend
    /// never produces.
    pub(crate) fn reporting_exclusive_configs(
        mut self,
        input: Option<Vec<SupportedConfigRange>>,
        output: Option<Vec<SupportedConfigRange>>,
    ) -> Self {
        self.exclusive_input_configs = input.filter(|ranges| !ranges.is_empty());
        self.exclusive_output_configs = output.filter(|ranges| !ranges.is_empty());
        self
    }

    /// Makes `device_name` answer `Engaged` to `supports_exclusive`. Per device, not per backend,
    /// so a test can grant exclusive mode to one direction and refuse it on the other.
    pub(crate) fn granting_exclusive_to(mut self, device_name: &str) -> Self {
        self.exclusive_devices.push(device_name.to_string());
        self
    }

    /// Configures the input and output devices reported by this backend.
    pub(crate) fn with_devices(
        mut self,
        input_devices: Vec<DeviceInfo>,
        output_devices: Vec<DeviceInfo>,
    ) -> Self {
        self.input_devices = input_devices;
        self.output_devices = output_devices;
        self
    }

    /// Makes `direction`'s `build_*_stream` fail rather than hand back a stream — FR-IO-070's
    /// "a device failing to open". The message is the shape a real backend's is: something the
    /// user can be shown, carried on [`AudioIoError::OpenFailed`].
    pub(crate) fn failing_to_open(mut self, direction: Direction) -> Self {
        self.open_failures.push(direction);
        self
    }

    /// The log for one direction, so a test can name the direction it is asserting about rather
    /// than remembering which field is which.
    pub(crate) fn stream_log(&self, direction: Direction) -> &Arc<FakeStreamLog> {
        match direction {
            Direction::Input => &self.input_stream,
            Direction::Output => &self.output_stream,
        }
    }

    /// `Err` when this direction was told to fail its open, `Ok(())` otherwise.
    fn open_outcome(&self, direction: Direction) -> Result<(), AudioIoError> {
        if self.open_failures.contains(&direction) {
            let side = match direction {
                Direction::Input => "input",
                Direction::Output => "output",
            };
            return Err(AudioIoError::OpenFailed(format!(
                "the fake {side} device was told to fail on demand"
            )));
        }
        Ok(())
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

    /// Every config query this backend answered, in call order — issue #190's two-pass
    /// re-enumeration is a sequence, not a final state, so a test needs the whole list.
    pub(crate) fn enumerations(&self) -> Vec<(Direction, ShareMode)> {
        self.enumerated_share_modes.lock().unwrap().clone()
    }
}

/// What one [`FakeStream`] was told to do, in what order relative to the other direction, and
/// whether it has been stopped.
///
/// `stops` counts **drops**, not `pause` calls, because dropping is what stopping a stream *is* in
/// this crate: [`AudioStream`]'s own doc comment makes "dropping this stops the stream" the
/// contract every real implementation relies on rather than re-implements, and
/// [`RunningStreams`]'s drop is the mechanism [`crate::host::AppHost`] uses to honour FR-IO-070's
/// "stop the stream cleanly". Counting rather than flagging so a double stop is visible as a
/// count of 2 rather than indistinguishable from a single one.
///
/// The two `*_tick` fields answer the question issue #194 turns on, which no per-direction count
/// can: *which side went first*. Both directions' logs share one monotonic clock (see
/// [`FakeStreamLog::pair`]), so their ticks are comparable.
#[cfg(test)]
// Deliberately no `#[derive(Default)]`: `pair` is the only constructor, because two logs with
// unshared clocks would produce ticks that look comparable and are not.
pub(crate) struct FakeStreamLog {
    plays: AtomicUsize,
    pauses: AtomicUsize,
    stops: AtomicUsize,
    pause_tick: AtomicUsize,
    stop_tick: AtomicUsize,
    /// The clock shared with the other direction's log. Ticks are 1-based, so 0 in the two fields
    /// above reads as "this never happened".
    clock: Arc<AtomicUsize>,
}

#[cfg(test)]
impl FakeStreamLog {
    /// The input and output logs, sharing one clock — the only way they are built, since a tick
    /// from a clock the other direction is not also using would compare against nothing.
    fn pair() -> (Arc<Self>, Arc<Self>) {
        let clock = Arc::new(AtomicUsize::new(0));
        let side = |clock| {
            Arc::new(Self {
                plays: AtomicUsize::new(0),
                pauses: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
                pause_tick: AtomicUsize::new(0),
                stop_tick: AtomicUsize::new(0),
                clock,
            })
        };
        (side(Arc::clone(&clock)), side(clock))
    }

    fn tick(&self) -> usize {
        self.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn plays(&self) -> usize {
        self.plays.load(Ordering::Relaxed)
    }
    pub(crate) fn pauses(&self) -> usize {
        self.pauses.load(Ordering::Relaxed)
    }
    pub(crate) fn stops(&self) -> usize {
        self.stops.load(Ordering::Relaxed)
    }

    /// When this direction was last paused, on the clock it shares with the other direction:
    /// `Some(1)` for whichever side was paused first, `None` for a side never paused.
    pub(crate) fn pause_tick(&self) -> Option<usize> {
        Some(self.pause_tick.load(Ordering::Relaxed)).filter(|t| *t > 0)
    }

    /// As [`FakeStreamLog::pause_tick`], for the stop (i.e. the drop).
    pub(crate) fn stop_tick(&self) -> Option<usize> {
        Some(self.stop_tick.load(Ordering::Relaxed)).filter(|t| *t > 0)
    }
}

#[cfg(test)]
struct FakeStream {
    log: Arc<FakeStreamLog>,
}

#[cfg(test)]
impl AudioStream for FakeStream {
    fn play(&self) -> Result<(), AudioIoError> {
        self.log.plays.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn pause(&self) -> Result<(), AudioIoError> {
        self.log.pauses.fetch_add(1, Ordering::Relaxed);
        let tick = self.log.tick();
        self.log.pause_tick.store(tick, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
impl Drop for FakeStream {
    fn drop(&mut self) {
        self.log.stops.fetch_add(1, Ordering::Relaxed);
        let tick = self.log.tick();
        self.log.stop_tick.store(tick, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) type InputCallback = Box<dyn FnMut(&[f32], CallbackStatus) + Send>;
#[cfg(test)]
pub(crate) type OutputCallback = Box<dyn FnMut(&mut [f32], CallbackStatus) + Send>;
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
        Ok(self.input_devices.clone())
    }
    fn output_devices(&self, _host: &HostInfo) -> Result<Vec<DeviceInfo>, AudioIoError> {
        Ok(self.output_devices.clone())
    }
    fn input_configs(
        &self,
        _h: &HostInfo,
        _d: &DeviceInfo,
        share_mode: ShareMode,
    ) -> Result<crate::audio_io::EnumeratedConfigs, AudioIoError> {
        self.enumerated_share_modes
            .lock()
            .unwrap()
            .push((Direction::Input, share_mode));
        if let (ShareMode::Exclusive, Some(ranges)) = (share_mode, &self.exclusive_input_configs) {
            return Ok(crate::audio_io::EnumeratedConfigs {
                share_mode: ShareMode::Exclusive,
                ranges: ranges.clone(),
            });
        }
        Ok(crate::audio_io::EnumeratedConfigs {
            share_mode: ShareMode::Shared,
            ranges: vec![SupportedConfigRange {
                channels: 1,
                min_sample_rate_hz: 48_000,
                max_sample_rate_hz: 48_000,
                buffer_size: BufferSizeRange::Unknown,
            }],
        })
    }
    fn output_configs(
        &self,
        _h: &HostInfo,
        _d: &DeviceInfo,
        share_mode: ShareMode,
    ) -> Result<crate::audio_io::EnumeratedConfigs, AudioIoError> {
        self.enumerated_share_modes
            .lock()
            .unwrap()
            .push((Direction::Output, share_mode));
        if let (ShareMode::Exclusive, Some(ranges)) = (share_mode, &self.exclusive_output_configs) {
            return Ok(crate::audio_io::EnumeratedConfigs {
                share_mode: ShareMode::Exclusive,
                ranges: ranges.clone(),
            });
        }
        Ok(crate::audio_io::EnumeratedConfigs {
            share_mode: ShareMode::Shared,
            ranges: vec![SupportedConfigRange {
                channels: 2,
                min_sample_rate_hz: 48_000,
                max_sample_rate_hz: 48_000,
                buffer_size: BufferSizeRange::Unknown,
            }],
        })
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
        on_data: Box<dyn FnMut(&[f32], CallbackStatus) + Send>,
        on_error: Box<dyn FnMut(StreamFailure) + Send>,
        _timeout: Duration,
    ) -> Result<Box<dyn AudioStream>, AudioIoError> {
        self.asked_share_modes
            .lock()
            .unwrap()
            .push((Direction::Input, params.share_mode));
        // Before the callbacks are stored: a real backend that refuses the open never received
        // them either, and a test asserting the teardown must not find a live callback behind a
        // failed open.
        self.open_outcome(Direction::Input)?;
        *self.input_data.lock().unwrap() = Some(on_data);
        *self.input_error.lock().unwrap() = Some(on_error);
        Ok(Box::new(FakeStream {
            log: Arc::clone(&self.input_stream),
        }))
    }
    fn build_output_stream(
        &self,
        _host: &HostInfo,
        _device: &DeviceInfo,
        params: StreamParams,
        on_data: Box<dyn FnMut(&mut [f32], CallbackStatus) + Send>,
        on_error: Box<dyn FnMut(StreamFailure) + Send>,
        _timeout: Duration,
    ) -> Result<Box<dyn AudioStream>, AudioIoError> {
        self.asked_share_modes
            .lock()
            .unwrap()
            .push((Direction::Output, params.share_mode));
        self.open_outcome(Direction::Output)?;
        *self.output_data.lock().unwrap() = Some(on_data);
        *self.output_error.lock().unwrap() = Some(on_error);
        Ok(Box::new(FakeStream {
            log: Arc::clone(&self.output_stream),
        }))
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

        input_cb(&[0.1f32; 64], CallbackStatus::default());
        let mut out = [0.0f32; 128]; // 64 frames * 2 channels
        // Drains `open`'s one-block prefill of silence.
        output_cb(&mut out, CallbackStatus::default());
        input_cb(&[0.1f32; 64], CallbackStatus::default());
        output_cb(&mut out, CallbackStatus::default()); // and now the captured signal

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

    /// FR-IO-060's bridge-underrun path, and issue #189's activation transient, which are the
    /// same code path seen from either side. A pull with nothing pushed pads, and padding counts
    /// an xrun rather than panicking or silently producing garbage — but **only once the capture
    /// side has actually run**. Before that, every pad belongs to the output device's own
    /// activation: on §2's interface the first output callback fires, the device then takes
    /// ~524 ms to start running, and the two blocks pulled across that gap opened every healthy
    /// session at `xrun count is now 2` before the user played anything. `open`'s one-block
    /// prefill absorbs the first of them; the flag this test drives absorbs the rest of the gap,
    /// however long the device takes.
    ///
    /// Red before the gate: the second `output_cb` below counted, with no input callback ever
    /// having run.
    // trace-partial: FR-IO-060
    // uncovered: FR-IO-060 — the "resettable by the user" clause has no path to exercise:
    // uncovered: XrunCounter::reset has no caller outside its own two unit tests and no UiIntent
    // uncovered: reaches it, and the running count surfaces only through an eprintln! rather than
    // uncovered: anywhere in the window; closes M8
    #[test]
    fn output_pads_during_the_activation_transient_are_not_counted() {
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

        let mut input_cb = backend.input_data.lock().unwrap().take().unwrap();
        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();
        let mut out = [0.0f32; 128];

        // 64 frames, exactly the prefill -- no input_cb call needed.
        output_cb(&mut out, CallbackStatus::default());
        assert_eq!(xruns.count(), 0, "the prefill absorbs the first pull");

        // The prefill is spent, the device is still activating, and the capture side has not run:
        // these pads are the transient, however many callbacks it lasts (up to the activation
        // ceiling, below). The suppression is not a fixed number of callbacks from the *open* —
        // it is anchored on the capture side's first callback, so an activation gap of any
        // realistic length is covered.
        for _ in 0..64 {
            output_cb(&mut out, CallbackStatus::default());
        }
        assert_eq!(
            xruns.count(),
            0,
            "pads before the first input callback are the output device's activation, not dropouts"
        );

        // Capture is live from here. The settling window (100 ms of pulls: one pull per callback
        // at a 64-frame `max_block`, so 48_000 / 64 / 10) still absorbs the pads while the ring
        // fills; past it, a starved pull is a real dropout again.
        let settling_pulls = 48_000 / 64 / 10;
        input_cb(&[0.1f32; 64], CallbackStatus::default());
        for _ in 0..settling_pulls {
            output_cb(&mut out, CallbackStatus::default());
        }
        assert_eq!(
            xruns.count(),
            0,
            "pads inside the settling window after capture starts are still the transient"
        );

        for _ in 0..4 {
            output_cb(&mut out, CallbackStatus::default());
        }
        assert!(
            xruns.count() > 0,
            "once the stream has settled, an underrun must reach the session's xrun count"
        );
    }

    /// A capture side that opens and then delivers nothing raises no `StreamFailure` — there is no
    /// error for the driver to report — so without a ceiling on the pre-capture suppression the
    /// session would pad silence forever at a clean `xrun count` of 0. That is the one failure
    /// FR-IO-060 matters most for, so the suppression is bounded (PR #206 review).
    #[test]
    fn a_capture_side_that_never_runs_eventually_counts_dropouts() {
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

        // One pull per callback here (64 frames against a 64-frame `max_block`), so the ceiling
        // is the 3 s activation budget in pulls — the first absorbed by `open`'s prefill, the
        // rest padded — and no input callback ever fires.
        let activation_pulls_max = 48_000 / 64 * 3;
        for _ in 0..activation_pulls_max {
            output_cb(&mut out, CallbackStatus::default());
        }
        assert_eq!(
            xruns.count(),
            0,
            "the activation ceiling has not been passed yet"
        );

        output_cb(&mut out, CallbackStatus::default());
        assert!(
            xruns.count() > 0,
            "past the ceiling, a silent capture side's pads are dropouts the user can see"
        );
    }

    /// The same silent-capture scenario at the smallest buffer size `STANDARD_BUFFER_SIZES`
    /// offers. The ceiling is a wall-clock budget, so it must still outlast the measured ~524 ms
    /// device activation at 32 frames — a fixed 512-pull ceiling would be ~341 ms there and would
    /// start counting an activation that is still in progress, which is issue #189 resurfacing at
    /// exactly the buffer sizes a latency-sensitive user picks (PR #206 review).
    #[test]
    fn the_activation_ceiling_outlasts_device_activation_at_the_smallest_buffer_size() {
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

        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();
        let mut out = [0.0f32; 64]; // 32 frames: one pull per callback.

        // 600 ms of pulls at 32 frames / 48 kHz — past the ~524 ms activation, and well short of
        // the 3 s budget. A 512-pull ceiling (~341 ms here) counts dropouts before this point.
        for _ in 0..(48_000 * 600 / 1_000 / 32) {
            output_cb(&mut out, CallbackStatus::default());
        }
        assert_eq!(
            xruns.count(),
            0,
            "at 32 frames the ceiling must still be past the measured ~524 ms activation"
        );
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

        input_cb(&[0.1f32; 100], CallbackStatus::default());
        let mut out = [0.0f32; 200]; // 100 frames, over max_block_size (32)
        output_cb(&mut out, CallbackStatus::default()); // must not panic
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
        input_cb(&exact_in, CallbackStatus::default());
        output_cb(&mut exact_out, CallbackStatus::default());

        let mut saw_output = false;
        for iteration in 0..32 {
            // One iteration carries a backend-reported xrun, so the `xruns.record()` branch these
            // callbacks gained with issue #200 item 6 runs *inside* the harness rather than being
            // the branch never taken. A relaxed `fetch_add` cannot allocate, which is exactly the
            // kind of claim this harness exists to check rather than assert by reasoning.
            let status = CallbackStatus {
                xrun: iteration == 7,
            };
            crate::rt_harness::audio_section(|| input_cb(&exact_in, status));
            crate::rt_harness::audio_section(|| output_cb(&mut exact_out, status));
            saw_output |= exact_out.iter().any(|s| s.abs() > 1e-6);
            crate::rt_harness::audio_section(|| input_cb(&big_in, CallbackStatus::default()));
            crate::rt_harness::audio_section(|| output_cb(&mut big_out, CallbackStatus::default()));
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
            input_cb(&[0.1f32; MAX_BLOCK], CallbackStatus::default());
        }
        assert_eq!(
            xruns.count(),
            0,
            "capture that fits in the ring is not a dropout"
        );

        // Far past it, still with no output callback draining anything.
        for _ in 0..32 {
            input_cb(&[0.1f32; MAX_BLOCK], CallbackStatus::default());
        }
        assert!(
            xruns.count() > 0,
            "capture that overran the bridge ring must reach the session's xrun count"
        );
    }

    /// FR-IO-060's **other** source, live again since issue #200 item 6: a dropout the *backend*
    /// detected. `cpal` 0.19 reports it per data callback through `CallbackInfo::xrun()`, which
    /// reaches this crate as [`CallbackStatus::xrun`]; before the seam carried it, a device that
    /// lost samples of its own was counted nowhere, and a session whose bridge kept up read a
    /// clean zero while the user heard crackling.
    ///
    /// Both directions, because each callback is built separately and a fix applied to one of
    /// them is exactly the half-wiring this would otherwise miss. Counted unconditionally:
    /// unlike a bridge pad, a backend report is not an artefact of the pair's activation
    /// transient, so the settling window does not gate it — which is why this asserts from the
    /// very first callback of each stream.
    // trace-partial: FR-IO-060
    // uncovered: FR-IO-060 — the "resettable by the user" clause has no path to exercise:
    // uncovered: XrunCounter::reset has no caller outside its own two unit tests and no UiIntent
    // uncovered: reaches it, and the running count surfaces only through an eprintln! rather than
    // uncovered: anywhere in the window; closes M8
    #[test]
    fn a_backend_reported_xrun_reaches_the_session_count_from_either_direction() {
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
        let mut input_cb = backend.input_data.lock().unwrap().take().unwrap();
        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();
        let mut out = [0.0f32; 128];

        let clean = CallbackStatus::default();
        let glitched = CallbackStatus { xrun: true };

        input_cb(&[0.1f32; 64], clean);
        output_cb(&mut out, clean);
        assert_eq!(
            xruns.count(),
            0,
            "a callback the backend reported nothing about is not a dropout"
        );

        input_cb(&[0.1f32; 64], glitched);
        assert_eq!(
            xruns.count(),
            1,
            "a capture callback the device dropped samples on must be counted"
        );

        output_cb(&mut out, glitched);
        assert_eq!(
            xruns.count(),
            2,
            "a render callback the device dropped samples on must be counted too"
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
        output_cb(&mut out, CallbackStatus::default());

        assert!(
            report.take().is_some(),
            "the first output callback must post an outcome, whatever this OS answered"
        );
        assert!(
            report.take().is_none(),
            "a posted outcome is reported once, not once per frame"
        );

        // Later callbacks do not elevate again (the one-shot flag), so nothing more appears.
        output_cb(&mut out, CallbackStatus::default());
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

    /// **Issue #194: the stop order is the fix, so the stop order is what gets pinned.** Stopping
    /// the pair is dropping it, and the output side has to go first — while it is still live the
    /// driver keeps calling its callback, and a callback with the capture side already gone pulls
    /// a bridge nobody is feeding, pads, and charges FR-IO-060's counter for the shutdown.
    ///
    /// Asserted through the two logs' shared clock rather than through field order, because the
    /// mechanism under test is `RunningStreams`' own `Drop`: a reorder of its fields must not be
    /// able to change the answer without this failing.
    #[test]
    fn dropping_the_pair_stops_the_output_side_before_the_input_side() {
        let backend = FakeBackend::new();
        let streams = open(
            setup(&backend, 64),
            engine(64),
            Arc::new(XrunCounter::new()),
            |_| {},
            |_| {},
        )
        .unwrap();
        assert_eq!(
            backend.stream_log(Direction::Output).stop_tick(),
            None,
            "nothing has been stopped yet"
        );

        drop(streams);

        // Relative, not absolute: the clock lives on the `FakeBackend` and spans everything it
        // does, so pinning `(Some(1), Some(2))` would fail spuriously the moment this test paused
        // first or opened a second pair. `o < i` still fails under the old order (`o > i`) and
        // still fails if either side never stops.
        let (output, input) = (
            backend.stream_log(Direction::Output).stop_tick(),
            backend.stream_log(Direction::Input).stop_tick(),
        );
        assert!(
            matches!((output, input), (Some(o), Some(i)) if o < i),
            "the output side stops first, the input side second; got {output:?} then {input:?}"
        );
    }

    /// `pause` orders the two sides too, and orders them the same way the stop does and for the
    /// same reason: an input side paused under a still-running output side is the unfed-bridge
    /// window, one `pause` call wide. (`play` is the deliberate exception — see its doc comment.)
    #[test]
    fn pausing_the_pair_pauses_the_output_side_before_the_input_side() {
        let backend = FakeBackend::new();
        let streams = open(
            setup(&backend, 64),
            engine(64),
            Arc::new(XrunCounter::new()),
            |_| {},
            |_| {},
        )
        .unwrap();

        streams.pause().unwrap();

        // Relative for the same reason as the stop test above.
        let (output, input) = (
            backend.stream_log(Direction::Output).pause_tick(),
            backend.stream_log(Direction::Input).pause_tick(),
        );
        assert!(
            matches!((output, input), (Some(o), Some(i)) if o < i),
            "the output side pauses first, the input side second; got {output:?} then {input:?}"
        );
    }

    /// **Issue #194 at the counter it was read on.** A session that ran cleanly and was then
    /// stopped must leave FR-IO-060's count exactly where the run left it: ending a session is
    /// not a dropout, and a user who quits should not be handed a number that reads as glitching
    /// audio.
    ///
    /// A stop is not instantaneous, and the driver keeps servicing whichever side is still live
    /// for as long as the other side's close takes — several blocks at any real buffer size. That
    /// is the window this test replays, on whichever side outlived the other. Under the old field
    /// order that side was the output one, and its pulls padded (the bridge holds one block of
    /// prefill slack, so the first pull absorbs and the rest count). With the output side stopped
    /// first the survivor is the capture side, whose pushes go into a bridge nobody reads —
    /// silent, and countable only by overrunning the ring. `TEARDOWN_CALLBACKS` is 4 because that
    /// is a teardown window a real close plausibly spans while staying inside the ~7 blocks of
    /// headroom past the prefill; the headroom is a margin in time, not a guarantee (~75 ms at a
    /// 480-frame block, ~4.7 ms at the 32-frame minimum), so a deliberately longer replay would
    /// count — for a true reason, and it is not what this test is about.
    ///
    /// **The backend's own report is deliberately *not* suppressed here (issue #200 item 6).**
    /// The teardown replay below ends with a callback carrying `CallbackStatus { xrun: true }`,
    /// and that one does count. The asymmetry is the point: a bridge pad during teardown is an
    /// artefact Namir manufactured — it stopped draining one side and then pulled from the ring
    /// it stopped feeding — whereas a backend report is the *device's* claim that it lost
    /// samples, made by the layer that alone can know. Suppressing it would mean deciding, from
    /// this side of the seam, that a driver is wrong about its own dropout, and the suppression
    /// would have to be a second stopping flag read on the audio thread. Not verified against
    /// real hardware: whether a driver actually raises `CallbackInfo::xrun()` on the callbacks
    /// bracketing a close is unknown here (see
    /// `docs/manual-tests/fr-io-060-xrun-induction.md`), so if a real interface turns out to
    /// report one per stop, this is the test and the argument to revisit — and #189/#194's
    /// gating is the shape to copy.
    #[test]
    fn stopping_a_clean_session_leaves_the_xrun_count_where_the_run_left_it() {
        /// Callbacks the still-live side takes while the other side is closing.
        const TEARDOWN_CALLBACKS: usize = 4;

        let backend = FakeBackend::new();
        let xruns = Arc::new(XrunCounter::new());
        let streams = open(
            setup(&backend, 64),
            engine(64),
            Arc::clone(&xruns),
            |_| {},
            |_| {},
        )
        .unwrap();
        let mut input_cb = backend.input_data.lock().unwrap().take().unwrap();
        let mut output_cb = backend.output_data.lock().unwrap().take().unwrap();

        let mut out = [0.0f32; 128];
        for _ in 0..8 {
            input_cb(&[0.1f32; 64], CallbackStatus::default());
            output_cb(&mut out, CallbackStatus::default());
        }
        assert_eq!(
            xruns.count(),
            0,
            "supply matched demand: the run itself has to be clean or this proves nothing"
        );

        drop(streams);

        let input_outlived_output = backend.stream_log(Direction::Input).stop_tick()
            > backend.stream_log(Direction::Output).stop_tick();
        for _ in 0..TEARDOWN_CALLBACKS {
            if input_outlived_output {
                input_cb(&[0.1f32; 64], CallbackStatus::default());
            } else {
                output_cb(&mut out, CallbackStatus::default());
            }
        }

        assert_eq!(
            xruns.count(),
            0,
            "the teardown counted a dropout of its own -- the session's own stop is not an xrun"
        );

        // And now the other half of the rule: the *device* reporting a dropout across the same
        // window is counted, because it is the device's claim rather than the teardown's own
        // artefact. See this test's doc comment for why that asymmetry is deliberate.
        let glitched = CallbackStatus { xrun: true };
        if input_outlived_output {
            input_cb(&[0.1f32; 64], glitched);
        } else {
            output_cb(&mut out, glitched);
        }
        assert_eq!(
            xruns.count(),
            1,
            "a backend-reported dropout is not suppressed by the teardown window"
        );
    }
}
