//! [`NamirAudioProcessor`]: CLAP's `[audio-thread]` half (`PluginAudioProcessor`). This is
//! Namir's actual audio thread for this plugin instance — the one place in the whole crate where
//! NFR-RT-010 ("no allocation, no lock the audio thread can contend on, no unbounded loop") is a
//! hard requirement rather than a nicety, and where D-7.4's [`namir_platform::DenormalGuard`] and
//! D-13.2's [`namir_platform::elevate_current_thread_priority`] — both "built but not yet called"
//! as of M6's `namir-platform` round — get their first real caller.
//!
//! # Two parameter-delivery paths, and why they must stay separate
//!
//! Host automation ([`clack_common::events::event_types::ParamValueEvent`]s in this call's
//! `events.input`) is applied via
//! [`namir_engine::AudioEngine::apply_param_direct`] — **not**
//! [`namir_worker::Instance::try_submit_param`], which `crate::ui_host` uses from the GUI thread.
//! The two exist for the same reason `namir_engine::engine`'s own module doc comment gives for
//! its ring: `try_submit_param` takes a producer-side `Mutex` that a worker-pool job can hold for
//! up to `CommandSubmitter::DEFAULT_DEADLINE` (2 seconds) while `Instance::load` waits out a full
//! ring. If `process()` — the audio thread — contended that same mutex, a resource load in
//! flight on the worker could stall an audio callback for up to two seconds: exactly the
//! FR-CLAP-130 violation ("never block the audio thread ... under any user action including
//! model loading") this split exists to prevent. `apply_param_direct` instead calls straight
//! through to `Chain::apply` — sound specifically because `process()` already holds `&mut
//! AudioEngine` exclusively *from the audio thread itself*, so there is no cross-thread boundary
//! for a ring or a lock to mediate (see that method's own doc comment in `namir-engine` for the
//! full argument). Every automated change is also mirrored into
//! [`crate::param_mirror::ParamMirror`] so the GUI reflects host automation, not only its own
//! dispatches.
//!
//! # FR-CLAP-060: the block is split at each event's own frame (M14, issue #30)
//!
//! Every `clap_event_param_value` carries a `header().time()`: the frame, relative to the start of
//! this `process()` call, at which the host means the change to take effect. Until M14 this module
//! never read it. It applied the whole event list once, before running the chain, so every
//! automation point in a block landed on that block's *first* frame — **~10.7 ms of quantisation at
//! 48 kHz/512 and ~85 ms at 4096**, on a plugin whose primary user class automates a bypass and a
//! gain in time with a performance.
//!
//! [`NamirAudioProcessor::process`] now walks the event list once, keeping a `cursor` frame:
//! everything from `cursor` up to the next event's time is rendered
//! ([`NamirAudioProcessor::process_segment`]) before that event is applied, and the remainder of
//! the block is rendered after the last one. A block with no automation in it therefore takes
//! exactly the path it always did — one segment, the whole block — and one with *k* events takes at
//! most *k + 1*. Nothing about it allocates or loops unboundedly (NFR-RT-010): the segment count is
//! bounded by the host's own event list, and each segment reuses the same buffers, sub-sliced.
//!
//! The per-segment dry-into-wet copy and output-silencing live in [`prepare_channel`], which takes
//! the segment's range for exactly this reason — doing either over the whole block would have a
//! later segment overwrite what an earlier one produced.
//!
//! **The click-free half of FR-CLAP-060 was not this module's to close, and issue #142 closed it
//! elsewhere.** `namir_engine::Chain`'s global bypass used to be a `bool` flip with no crossfade,
//! where FR-CHAIN-020's *per-stage* bypass faded over 15 ms; sample-accurate delivery is what makes
//! a change land where the host asked, not what smooths it. `Chain` now runs the same 15 ms blend
//! for its global bypass, so what this module delivers sample-accurately is the fade's *start*, and
//! `tests/clap_host_automation.rs` measures both halves — the frame the transition begins on, and
//! the trajectory it takes from there.
//!
//! # FR-CLAP-040: latency reporting, and the restart CLAP's own contract requires
//!
//! Every block, this processor reads `AudioEngine::chain().latency_samples()` and publishes it to
//! `SharedInner::latency_samples` (a plain atomic store — cheap, wait-free). When that value
//! differs from what this processor itself last saw, it flags `SharedInner::latency_dirty` and
//! calls `host.shared().request_callback()` — documented by `clack_extensions::latency` as
//! thread-safe, so calling it from here is sound. The host then calls `on_main_thread`
//! (`crate::main_thread`), which — per `clack_extensions::latency::HostLatency::changed`'s own
//! doc comment, *"The latency is allowed to change only during the activate callback... If the
//! plugin is active, you should request a restart first"* — calls `request_restart()` rather than
//! `changed()` directly while active. The actual `changed()` notification happens inside the
//! *next* `activate()` (this module), which the CLAP-mandated deactivate/reactivate cycle brings
//! about. **Honest limitation, not glossed over:** this means a model swap that genuinely changes
//! the engine's resampler-induced latency (D-9.2 — only when the new model's declared rate
//! differs from the session rate) costs a brief restart cycle, which is a CLAP protocol
//! requirement Namir has no way around, not an engine defect; FR-NAM-070's glitch-free crossfade
//! still holds for every model swap that does *not* change latency, which is the common case. See
//! `docs/manual-tests/fr-clap-040-latency-restart.md`.
//!
//! ## The figure an activation carries is a prediction, and predictions get checked (issue #145)
//!
//! `SharedInner::carried_latency` (issue #93) lets `activate` keep announcing the figure the host
//! already has instead of the zero its freshly built engine reports, because the replay it is about
//! to dispatch is expected to put the same model — and the same latency — straight back. That is a
//! prediction about work that has not happened yet, and it can be wrong: the model may have been
//! deleted or replaced with a session-rate one while the plugin was inactive, in which case the
//! replay converges on **zero**, which is exactly what the fresh engine already reported. "Differs
//! from the last block" is then false forever, and the host is left compensating for a delay the
//! chain does not have — for the rest of the session, since nothing else will ever wake the main
//! thread about it.
//!
//! So a carried figure is tracked as an outstanding claim ([`CarriedLatency`]) until it is
//! discharged one of two ways: the engine's own reading moves (the prediction came true, or came
//! true differently, and the ordinary change path handles it), or the replay finishes with the
//! engine and the reading still disagrees with what the host was *told*, in which case the figure
//! is corrected downwards and the same restart machinery renegotiates it. "The replay finished" is
//! not something the engine can report — a replay that loads nothing leaves no trace on it — so it
//! comes from `SharedInner::worker_instance_epoch`, with `CARRY_SETTLE_MS` covering the bounded
//! remainder between a command being submitted and the handover crossfade completing.

use clack_plugin::events::Event;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::host::HostAudioProcessorHandle;
use clack_plugin::plugin::{PluginAudioProcessor, PluginError};
use clack_plugin::process::audio::{ChannelPair, PairedChannels};
use clack_plugin::process::{Audio, Events, PluginAudioConfiguration, Process, ProcessStatus};
use namir_core::{ChannelConfig, SampleRate};
use namir_engine::{
    AudioEngine, ParamChange, ParamId, PrepareContext, StageIo, build_default_engine,
};
use namir_platform::DenormalGuard;
use namir_worker::{EngineConfig, Instance};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::main_thread::NamirMainThread;
use crate::shared::NamirShared;

// `pub`, not `pub(crate)`: an associated type of `clack_plugin::plugin::Plugin::AudioProcessor`,
// implemented on this crate's public `NamirClapPlugin` — see `crate::shared::NamirShared`'s own
// doc comment for the full reason this visibility bump is required and harmless.
pub struct NamirAudioProcessor<'a> {
    engine: AudioEngine,
    shared: &'a NamirShared<'a>,
    host: HostAudioProcessorHandle<'a>,
    priority_elevated: bool,
    /// The engine's *own* last `latency_samples()` reading — not the figure the host is being
    /// told, which `SharedInner::latency_samples` holds and which can legitimately differ from
    /// this while an activation's replay is still in flight (issue #93; see
    /// `SharedInner::carried_latency`).
    last_seen_latency: u32,
    /// The unconfirmed half of that difference, while there is one — see [`CarriedLatency`] and
    /// [`Self::publish_latency`]. `None` for an activation that adopted the engine's own reading,
    /// which is the common case and needs no bookkeeping at all.
    carried: Option<CarriedLatency>,
    /// This activation's sample rate, kept so [`Self::publish_latency`] can record which rate the
    /// figure it publishes was measured at without asking the engine again.
    sample_rate_hz: u32,
}

/// A latency figure `activate` carried across an activation on the *prediction* that this
/// activation's replay will reproduce it — issue #93's mechanism, and issue #145's finding 8 about
/// what happens when the prediction is wrong.
///
/// See [`NamirAudioProcessor::publish_latency`] for how this is discharged.
struct CarriedLatency {
    /// The figure `activate` published and `notify_latency_changed` announced: what the host is
    /// currently compensating for, and what the engine's own reading is measured against once the
    /// replay has had its turn.
    announced: u32,
    /// `SharedInner::worker_instance_epoch` as `activate` read it, immediately before dispatching
    /// the replay. While it is unchanged the replay has not finished touching the engine, so the
    /// engine's reading is a transient rather than an answer.
    epoch: u32,
    /// Frames still to be processed before the prediction is judged, or `None` while the epoch has
    /// not moved. See [`CARRY_SETTLE_MS`].
    settle_frames: Option<u32>,
}

/// How much audio must pass after the replay has finished with the engine before an unconfirmed
/// carried figure is judged against the engine's own reading.
///
/// **Not a guess at how long a replay takes** — that is unbounded (a file read, a parse, a worker
/// thread the OS may schedule whenever it likes) and is what `SharedInner::worker_instance_epoch`
/// answers instead. This covers only the strictly bounded remainder: the replay's resource command
/// is already in the SPSC ring by the time the epoch moves, so what is left is one `process()` call
/// to drain it plus D-8.1 step 3's handover crossfade (`HANDOVER_CROSSFADE_MS`, 20 ms) before the
/// new slot becomes active and the chain's reported latency moves. Half a second is that with more
/// than an order of magnitude of margin, and it is half a second of *processed audio*, so it is
/// half a second of wall clock in any host that is actually running.
const CARRY_SETTLE_MS: u32 = 500;

impl<'a> NamirAudioProcessor<'a> {
    /// Runs `audio`'s frames `[start, end)` through the engine — see [`process_port_pair`] for the
    /// per-port channel plumbing. Called once per segment of [`Self::process`]'s event split, so a
    /// block with no automation in it reaches this exactly once, with the whole block.
    fn process_segment(&mut self, audio: &mut Audio, start: u32, end: u32) {
        for mut port_pair in &mut *audio {
            let Ok(channels) = port_pair.channels() else {
                continue;
            };
            let Some(mut channels) = channels.into_f32() else {
                continue;
            };
            process_port_pair(
                &mut self.engine,
                &mut channels,
                start as usize,
                end as usize,
            );
        }
    }

    /// This instance's shared state — `pub(crate)` accessor rather than a `pub(crate)` field, so
    /// `crate::params_ext`'s `flush` can reach the parameter mirror without every field of this
    /// audio-thread type becoming reachable from outside the module that owns the audio thread.
    pub(crate) fn shared(&self) -> &'a NamirShared<'a> {
        self.shared
    }

    /// One direct-applied change plus its `ParamMirror` update — the one piece of logic both
    /// `process()`'s own automation loop and `crate::params_ext`'s `PluginAudioProcessorParams::
    /// flush` (called when active but `process()` was not, per `clack_extensions::params`'s own
    /// doc comment) share, kept in one place so the two paths cannot silently drift apart.
    ///
    /// **Non-finite values are refused here, not passed on** (issue #145's finding 6). A host is
    /// free to hand us any `f64` it likes and both callers narrow it with `as f32`, so a `NaN` or
    /// an infinity is reachable from outside. `namir_engine`'s `Chain::apply` rejects one too --
    /// it has to, being reachable from the ring as well -- but the engine's guard cannot protect
    /// the *mirror*: without this check the bad value would still be stored, shown in the editor
    /// and written back into the instance's state on the next save, which outlives the session
    /// the bad event arrived in. Refused rather than clamped, for the reason D-16.3 gives and
    /// `Chain::apply` repeats: there is no sensible clamp for "NaN dB", so the last valid value
    /// stays in force. Silent, per FR-ERR-030 -- this is the audio thread.
    pub(crate) fn apply_direct_and_mirror(&mut self, id: ParamId, value: f32) {
        if !value.is_finite() {
            return;
        }
        self.engine.apply_param_direct(ParamChange { id, value });
        self.shared.inner.params.set_by_id(id.0, value);
    }

    /// Publishes this block's latency reading and, if it changed, wakes the main thread — see
    /// this module's doc comment for the full FR-CLAP-040 sequence, and [`CarriedLatency`] for the
    /// one case in which "changed" is not the same question as "differs from last block".
    ///
    /// `frames` is this block's own frame count, which only the settle countdown reads.
    fn publish_latency(&mut self, frames: u32) {
        let latency = self.engine.chain().latency_samples();
        if latency != self.last_seen_latency {
            // The engine has spoken, which settles any outstanding prediction along with it.
            self.last_seen_latency = latency;
            self.carried = None;
            self.announce_latency(latency);
            return;
        }

        // The engine's reading has not moved. Ordinarily that is the whole story -- and it must
        // stay the whole story while a replay is in flight, where the engine still reports 0 and
        // `SharedInner::latency_samples` is deliberately holding the figure that replay is
        // expected to converge on. Republishing the transient zero there re-opens issue #93's loop
        // from the other side, which is why this compares against the engine's own last reading
        // rather than against the published one.
        //
        // What that comparison alone cannot see is a replay that has *finished* and converged
        // somewhere else -- most sharply, back on the fresh engine's own zero, where "differs from
        // last block" is false forever and the host is left compensating for a delay the chain
        // does not have (issue #145's finding 8). So a carried figure is a prediction with an
        // outstanding verdict, and this is where it is discharged: once the replay has finished
        // with the engine (`SharedInner::worker_instance_epoch`) and the bounded remainder of the
        // handover has had time to land (`CARRY_SETTLE_MS`), the engine's reading is compared
        // against what the host was actually *told*, and a disagreement is published like any
        // other change.
        let Some(carried) = self.carried.as_mut() else {
            return;
        };
        match carried.settle_frames {
            None => {
                if self.shared.inner.worker_instance_epoch() != carried.epoch {
                    carried.settle_frames = Some(settle_frames(self.sample_rate_hz));
                }
            }
            Some(remaining) => {
                let remaining = remaining.saturating_sub(frames);
                carried.settle_frames = Some(remaining);
                if remaining == 0 {
                    let announced = carried.announced;
                    self.carried = None;
                    if latency != announced {
                        self.announce_latency(latency);
                    }
                }
            }
        }
    }

    /// Publishes `latency` as this instance's reported figure and wakes the main thread to act on
    /// it — the tail both of [`Self::publish_latency`]'s paths share.
    ///
    /// Wait-free: two relaxed atomic stores and `request_callback`, which
    /// `clack_extensions::latency` documents as thread-safe and which returns without waiting.
    fn announce_latency(&mut self, latency: u32) {
        self.shared
            .inner
            .publish_latency(latency, self.sample_rate_hz);
        self.shared
            .inner
            .latency_dirty
            .store(true, Ordering::Relaxed);
        self.host.shared().request_callback();
    }
}

impl<'a> PluginAudioProcessor<'a, NamirShared<'a>, NamirMainThread<'a>>
    for NamirAudioProcessor<'a>
{
    fn activate(
        host: HostAudioProcessorHandle<'a>,
        main_thread: &mut NamirMainThread<'a>,
        shared: &'a NamirShared<'a>,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sample_rate_hz = audio_config.sample_rate.round();
        let usable_rate = sample_rate_hz.is_finite()
            && sample_rate_hz >= 1.0
            && sample_rate_hz <= u32::MAX as f64;
        let sample_rate = usable_rate
            .then(|| SampleRate::new(sample_rate_hz as u32))
            .flatten();
        let Some(sample_rate) = sample_rate else {
            shared.inner.push_notice(
                crate::error_codes::INVALID_SAMPLE_RATE,
                format!("{sample_rate_hz}"),
            );
            return Err(PluginError::Message(
                "host presented an unusable sample rate",
            ));
        };

        // FR-CLAP-030: fixed stereo I/O (see `crate::audio_ports_ext`'s own doc comment for why
        // this is the one configuration this round declares).
        let ctx = PrepareContext::new(
            sample_rate,
            audio_config.max_frames_count as usize,
            ChannelConfig::Stereo,
        )
        .map_err(|_| PluginError::Message("failed to prepare the engine"))?;

        let (engine, endpoint) = build_default_engine(&ctx)
            .map_err(|_| PluginError::Message("failed to build the engine"))?;
        // `TelemetryReader` is `Clone` (D-7.3) specifically so a UI-side consumer can keep one
        // independent of whatever `Instance::new` does with the rest of `endpoint` — see
        // `crate::ui_host`'s module doc comment.
        let telemetry_reader = endpoint.telemetry.clone();
        let instance = Instance::new(EngineConfig { ctx }, endpoint);

        shared.inner.set_telemetry_reader(Some(telemetry_reader));
        shared.inner.install_instance(instance);
        shared.inner.active.store(true, Ordering::Relaxed);

        // The engine this activation just built is a *default* one, so this is 0 -- and
        // publishing that zero on every activation, while `spawn_recall` below is about to put
        // the model (and its latency) back, is exactly what made FR-CLAP-040's restart
        // unbounded (issue #93). `carried_latency` is what decides whether the figure the host
        // already has survives this activation; see its doc comment for both of its conditions.
        let engine_latency = engine.chain().latency_samples();
        let sample_rate_hz = sample_rate.hz();
        let reported = shared
            .inner
            .carried_latency(sample_rate_hz)
            .unwrap_or(engine_latency);
        shared.inner.publish_latency(reported, sample_rate_hz);
        // Read *before* the replay is dispatched below, or the very job whose completion this is
        // waiting for could finish between the read and the dispatch and go unnoticed.
        let epoch = shared.inner.worker_instance_epoch();
        // Permitted here unconditionally per `clack_extensions::latency::HostLatency::changed`'s
        // own doc comment ("allowed to change only during the activate callback") — see this
        // module's doc comment for the full sequence. It is also what records `reported` as the
        // figure the host has been given, which `on_main_thread` then compares against.
        main_thread.notify_latency_changed();

        // FR-STATE-030/050's replay: whatever this instance's `ParamMirror`/resource references
        // already stand for (from a prior activation, a host `state` load, or a GUI load request
        // that arrived before any engine existed) is pushed onto the freshly built engine. See
        // `crate::worker_jobs::spawn_recall`'s own doc comment for why this is dispatched to the
        // pool rather than run inline here.
        crate::worker_jobs::spawn_recall(Arc::clone(&shared.inner));

        Ok(Self {
            engine,
            shared,
            host,
            priority_elevated: false,
            last_seen_latency: engine_latency,
            // Only when the two actually disagree is there a prediction outstanding: a carried
            // figure that already matches the fresh engine's reading predicts nothing.
            carried: (reported != engine_latency).then_some(CarriedLatency {
                announced: reported,
                epoch,
                settle_frames: None,
            }),
            sample_rate_hz,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // D-7.4: engaged for the whole callback, restored on drop (including an early return).
        let _denormal = DenormalGuard::new();

        // D-13.2: once, at first `process()` activation — see `namir_platform::thread_priority`'s
        // own module doc comment for why this cadence (not once per callback) is correct.
        if !self.priority_elevated {
            // The outcome is `#[must_use]` and is carried *off* this thread rather than reported
            // here: FR-ERR-030 forbids logging and logging-formatting on the audio thread, and
            // `xtask rt-logging` forbids this module from so much as naming the logger. Two atomic
            // stores, and `on_main_thread` turns them into a notice -- see
            // `SharedInner::record_thread_priority_outcome`.
            let outcome = namir_platform::elevate_current_thread_priority();
            if self.shared.inner.record_thread_priority_outcome(outcome) {
                // Only when there is something to say, and only once per instance -- see that
                // method's own doc comment. A successful elevation wakes nobody.
                self.host.shared().request_callback();
            }
            self.priority_elevated = true;
        }

        // FR-CLAP-060's sample-accuracy limb: split the block at each automation event's own
        // `header().time()` rather than applying the whole event list before it. See this module's
        // doc comment for the full argument; `tests/clap_host_automation.rs` is what asserts it.
        //
        // Allocation-free and bounded: one pass over the event list, and at most one
        // `process_segment` call per event plus one for the tail.
        let frames = audio.frames_count();
        let mut cursor: u32 = 0;
        for event in events.input.iter() {
            if let Some(CoreEventSpace::ParamValue(ev)) = event.as_core_event()
                && let Some(id) = ev.param_id()
            {
                // Clamped twice, and both clamps are about a host that does not hold up its end.
                // CLAP requires `time < frames_count` and requires the list to be sorted by time;
                // an event past the end is taken as belonging to the last frame, and one that
                // would move the cursor backwards is applied where the cursor already is. Neither
                // can produce an empty or reversed range, which is what `StageIo::new` would
                // panic on.
                let at = ev.header().time().min(frames).max(cursor);
                if at > cursor {
                    self.process_segment(&mut audio, cursor, at);
                    cursor = at;
                }
                self.apply_direct_and_mirror(ParamId(id.get()), ev.value() as f32);
            }
        }
        if cursor < frames {
            self.process_segment(&mut audio, cursor, frames);
        }

        self.publish_latency(frames);

        // FR-PARAM-030's other direction (issue #94): a knob the user turned in *this* plugin's
        // editor is reported back to the host as automation, wrapped in a gesture, so the host can
        // record it and keep its own generic UI in step. See `crate::params_ext`'s
        // `emit_gui_param_changes` for why this is allocation-free and why host-originated changes
        // are never echoed back through it.
        crate::params_ext::emit_gui_param_changes(
            &self.shared.inner.params,
            &self.shared.inner.gestures,
            events.output,
        );

        Ok(ProcessStatus::Continue)
    }

    fn deactivate(self, _main_thread: &mut NamirMainThread<'a>) {
        self.shared.inner.active.store(false, Ordering::Relaxed);
        self.shared.inner.clear_instance();
        self.shared.inner.set_telemetry_reader(None);
    }

    fn reset(&mut self) {
        // Clears every stage's internal state (envelopes, filter history, resampler FIFOs) on
        // transport stop/reposition, without a full re-`activate`. `AudioEngine::reset_direct`
        // reaches `Chain::reset` directly, for the same "already on the audio thread, no ring
        // needed" reason `apply_param_direct` documents in `namir-engine`.
        self.engine.reset_direct();
    }
}

/// [`CARRY_SETTLE_MS`] as a frame count at `sample_rate_hz`, saturating rather than wrapping on a
/// rate no sane host presents. At least one frame, so the countdown always terminates.
fn settle_frames(sample_rate_hz: u32) -> u32 {
    let frames = u64::from(sample_rate_hz) * u64::from(CARRY_SETTLE_MS) / 1_000;
    u32::try_from(frames).unwrap_or(u32::MAX).max(1)
}

/// Builds the up-to-two channel mutable slices a `StageIo` needs from one port pair's frames
/// `[start, end)`, and runs the engine over them. Declared free (not a method) so its
/// generic-lifetime signature stays simple; see this module's doc comment section on channel
/// handling for the FR-CLAP-030 stereo-only scope this round declares.
///
/// `end` is clamped to this port's own frame count, so a host whose ports disagree with
/// `Audio::frames_count` degrades to "process what this port actually has" rather than panicking
/// (P8). An empty range after that clamp is a no-op — in particular, nothing is copied dry-to-wet
/// and nothing is silenced, which matters because a later segment of the same block will do both
/// for its own range.
fn process_port_pair(
    engine: &mut AudioEngine,
    channels: &mut PairedChannels<'_, f32>,
    start: usize,
    end: usize,
) {
    let end = end.min(channels.frames_count() as usize);
    if start >= end {
        return;
    }
    let frames = end - start;
    let count = channels.channel_pair_count();
    if count == 0 {
        return;
    }
    // Fixed at two: this round's declared port configuration is always stereo (FR-CLAP-030's
    // documented scope reduction — see `crate::audio_ports_ext`). A host that somehow negotiates
    // a different channel count degrades to "no processing for the channels beyond two, and
    // nothing at all if there are fewer than two" rather than panicking (P8).
    let mut bufs: [Option<&mut [f32]>; 2] = [None, None];
    for (i, slot) in bufs.iter_mut().enumerate().take(count.min(2)) {
        if let Some(pair) = channels.channel_pair(i) {
            *slot = prepare_channel(pair, start, end);
        }
    }
    let [Some(a), Some(b)] = bufs else {
        return;
    };
    let mut slices: [&mut [f32]; 2] = [a, b];
    let mut io = StageIo::new(&mut slices, frames);
    engine.process(&mut io);
}

/// One channel's mutable "to be processed in place" buffer for frames `[start, end)`, from
/// whatever shape the host handed this pair — see `clack_plugin::process::Audio`'s own doc comment
/// for the four `ChannelPair` cases. `InputOutput` is copied dry-into-wet first (the chain
/// processes in place, exactly as `AudioEngine::process`'s existing tests already exercise via
/// `StageIo`); `OutputOnly` is silenced first, matching `spikes/s4-clack-clap`'s own rule that the
/// host must never read uninitialised memory.
///
/// Both of those are done **per segment**, over `[start, end)` alone, which is what makes
/// [`NamirAudioProcessor::process`]'s event split safe to run over the same buffers repeatedly: a
/// later segment can neither re-copy nor re-silence a range an earlier one already processed.
///
/// Returns `None` if either side is shorter than `end` — a host contradicting its own
/// `frames_count` — rather than slicing out of bounds; the caller then leaves this port alone (P8).
fn prepare_channel<'a>(
    pair: ChannelPair<'a, f32>,
    start: usize,
    end: usize,
) -> Option<&'a mut [f32]> {
    match pair {
        ChannelPair::InputOutput(input, output) => {
            let input = input.get(start..end)?;
            let output = output.get_mut(start..end)?;
            output.copy_from_slice(input);
            Some(output)
        }
        ChannelPair::InPlace(buf) => buf.get_mut(start..end),
        ChannelPair::OutputOnly(buf) => {
            let buf = buf.get_mut(start..end)?;
            buf.fill(0.0);
            Some(buf)
        }
        ChannelPair::InputOnly(_) => None,
    }
}
