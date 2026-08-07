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
    last_seen_latency: u32,
}

impl<'a> NamirAudioProcessor<'a> {
    /// Applies every `ParamValue` automation event in this block, before running the chain — see
    /// this module's doc comment for why this goes through `apply_param_direct`, not the ring.
    fn apply_automation(&mut self, events: &Events) {
        for event in events.input.iter() {
            if let Some(CoreEventSpace::ParamValue(ev)) = event.as_core_event()
                && let Some(id) = ev.param_id()
            {
                self.apply_direct_and_mirror(ParamId(id.get()), ev.value() as f32);
            }
        }
    }

    /// One direct-applied change plus its `ParamMirror` update — the one piece of logic both
    /// `process()`'s own automation loop and `crate::params_ext`'s `PluginAudioProcessorParams::
    /// flush` (called when active but `process()` was not, per `clack_extensions::params`'s own
    /// doc comment) share, kept in one place so the two paths cannot silently drift apart.
    pub(crate) fn apply_direct_and_mirror(&mut self, id: ParamId, value: f32) {
        self.engine.apply_param_direct(ParamChange { id, value });
        self.shared.inner.params.set_by_id(id.0, value);
    }

    /// Publishes this block's latency reading and, if it changed, wakes the main thread — see
    /// this module's doc comment for the full FR-CLAP-040 sequence.
    fn publish_latency(&mut self) {
        let latency = self.engine.chain().latency_samples();
        self.shared
            .inner
            .latency_samples
            .store(latency, Ordering::Relaxed);
        if latency != self.last_seen_latency {
            self.last_seen_latency = latency;
            self.shared
                .inner
                .latency_dirty
                .store(true, Ordering::Relaxed);
            self.host.shared().request_callback();
        }
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

        let latency = engine.chain().latency_samples();
        shared
            .inner
            .latency_samples
            .store(latency, Ordering::Relaxed);
        // Permitted here unconditionally per `clack_extensions::latency::HostLatency::changed`'s
        // own doc comment ("allowed to change only during the activate callback") — see this
        // module's doc comment for the full sequence.
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
            last_seen_latency: latency,
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
            let _ = namir_platform::elevate_current_thread_priority();
            self.priority_elevated = true;
        }

        self.apply_automation(&events);

        for mut port_pair in &mut audio {
            let Ok(channels) = port_pair.channels() else {
                continue;
            };
            let Some(mut channels) = channels.into_f32() else {
                continue;
            };
            process_port_pair(&mut self.engine, &mut channels);
        }

        self.publish_latency();

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

/// Builds the up-to-two channel mutable slices a `StageIo` needs from one port pair, and runs the
/// engine over them. Declared free (not a method) so its generic-lifetime signature stays simple;
/// see this module's doc comment section on channel handling for the FR-CLAP-030 stereo-only
/// scope this round declares.
fn process_port_pair(engine: &mut AudioEngine, channels: &mut PairedChannels<'_, f32>) {
    let frames = channels.frames_count() as usize;
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
            *slot = prepare_channel(pair);
        }
    }
    let [Some(a), Some(b)] = bufs else {
        return;
    };
    let mut slices: [&mut [f32]; 2] = [a, b];
    let mut io = StageIo::new(&mut slices, frames);
    engine.process(&mut io);
}

/// One channel's mutable "to be processed in place" buffer, from whatever shape the host handed
/// this pair — see `clack_plugin::process::Audio`'s own doc comment for the four `ChannelPair`
/// cases. `InputOutput` is copied dry-into-wet first (the chain processes in place, exactly as
/// `AudioEngine::process`'s existing tests already exercise via `StageIo`); `OutputOnly` is
/// silenced first, matching `spikes/s4-clack-clap`'s own rule that the host must never read
/// uninitialised memory.
fn prepare_channel<'a>(pair: ChannelPair<'a, f32>) -> Option<&'a mut [f32]> {
    match pair {
        ChannelPair::InputOutput(input, output) => {
            output.copy_from_slice(input);
            Some(output)
        }
        ChannelPair::InPlace(buf) => Some(buf),
        ChannelPair::OutputOnly(buf) => {
            buf.fill(0.0);
            Some(buf)
        }
        ChannelPair::InputOnly(_) => None,
    }
}
