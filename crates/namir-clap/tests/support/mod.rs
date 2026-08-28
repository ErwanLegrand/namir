// Every test binary that includes this module compiles the *whole* module, and CI runs clippy with
// `-D warnings`; a helper only one of the six downstream test files uses would otherwise fail the
// build for the other five. This attribute is mandatory, not tidying -- do not remove it.
#![allow(dead_code)]

//! Shared in-process CLAP **host** harness for M9b's `namir-clap` tests.
//!
//! # ⚠ HAZARD — READ BEFORE WRITING A TEST AGAINST THIS ⚠
//!
//! **No test may call `SharedInner::start_library_scan`, or drive any UI intent that reaches it
//! (`namir_ui::UiIntent::RescanLibrary` and anything that dispatches it).**
//!
//! Instantiating the plugin runs `SharedInner::new()`, which calls
//! `namir_worker::library::LibraryService::open_default()` against **the developer's real
//! per-user configuration directory** — the same `library-index.json` `namir-app` writes. That by
//! itself is read-only and harmless. A *scan* is not: `crates/namir-clap/src/shared.rs`'s own
//! record (the `library` field's doc comment) is that `namir_library::scan` concludes every path a
//! complete walk did not see has been removed, so a scan started from a test — where nothing has
//! configured a root, and where the fixture directories a test cares about are certainly not
//! roots — can **erase the developer's real library index**. There is no sandbox around this and
//! nothing in the harness can add one, because `open_default` deliberately resolves the real
//! path. The only protection is that no test asks for a scan.
//!
//! Everything else this harness exposes is safe to use freely.
//!
//! # What this is
//!
//! [`clap_host_teardown.rs`](../clap_host_teardown.rs) proved the shape at M9a: a real
//! `NamirClapPlugin` driven through the real C vtable by `clack-host`, with **no `dlopen` and no
//! `unsafe` of ours** — `PluginEntry::load_from_clack` is the only way to load a CLAP plugin using
//! safe code alone, and D-5.3 permits exactly one `#![allow(unsafe_code)]` file in this crate
//! (`src/gui.rs`), which neither that harness nor this module is. This module generalises it so
//! six independent test binaries share one instantiation path, one buffer-plumbing story and one
//! allocation probe.
//!
//! `clap_host_teardown.rs` deliberately **does not** use this module and must not be changed to:
//! it asserts an exact equality on the process-global `namir_worker::pool::live_worker_threads()`
//! counter, which is only meaningful in a binary where nothing else instantiates a plugin.
//!
//! # Feature gating (D-18.7)
//!
//! Anything that reads a CLAP *extension* — the plugin's `audio-ports`/`params`/`state`/`latency`
//! surfaces, or a host-side callback the plugin invokes on them — lives behind
//! `#[cfg(feature = "host-ext-tests")]`, because `clack-extensions`' host halves only exist under
//! its own `clack-host` feature. The module compiles and runs both ways: with the feature off you
//! get instantiate/activate/process/destroy, with it on you additionally get extension access and
//! the `HostLatency`/`HostParams` callback counters. See `Cargo.toml`'s `[features]` comment for
//! why the feature must stay non-default and why `--all-features` is forbidden.
//!
//! # Determinism
//!
//! No `rand` dependency (D-19.1's spirit: fixtures are generated from a seed, never captured).
//! [`Lcg`] is a plain seeded linear congruential generator and [`fill_sine`] is closed-form, so a
//! failing assertion reproduces exactly.

use std::sync::atomic::{AtomicUsize, Ordering};

use assert_no_alloc::AllocDisabler;
use clack_host::prelude::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, AudioProcessorHandler, EventBuffer,
    HostHandlers, HostInfo, InitializedPluginHandle, InputChannel, InputEvents, MainThreadHandler,
    PluginAudioConfiguration, PluginEntry, PluginInstance, PluginInstanceError, ProcessStatus,
    SharedHandler, StartedPluginAudioProcessor, StoppedPluginAudioProcessor,
};
use clack_plugin::prelude::{DefaultPluginFactory, SinglePluginEntry};
use namir_clap::NamirClapPlugin;

#[cfg(feature = "host-ext-tests")]
use clack_extensions::latency::{HostLatency, HostLatencyImpl};
#[cfg(feature = "host-ext-tests")]
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
#[cfg(feature = "host-ext-tests")]
use clack_host::extensions::{Extension, PluginExtensionSide};
#[cfg(feature = "host-ext-tests")]
use clack_host::prelude::{ClapId, HostExtensions, PluginMainThreadHandle};

// ---------------------------------------------------------------------------------------------
// D-7.5's allocation probe, restated for this crate.
// ---------------------------------------------------------------------------------------------

/// The process-global allocator every test binary that includes this module installs.
///
/// `namir-engine`'s equivalent (`crates/namir-engine/src/rt_harness.rs`) is `#[cfg(test)]`-private
/// to that crate, so it cannot be imported from here — and a `#[global_allocator]` is one per
/// binary anyway, so each binary needs its own declaration regardless. This is a safe `static`
/// plus an attribute: the `unsafe impl GlobalAlloc` lives in `assert_no_alloc`, which is why this
/// composes rather than hand-rolls (D-5.3 forbids `unsafe` outside `src/gui.rs` in this crate).
#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

/// Runs `f` inside D-7.5's "audio section" marker and panics if anything allocated inside it.
///
/// `assert_no_alloc`'s `warn_debug`/`warn_release` features (both on, see `Cargo.toml`) make a
/// violation increment a counter rather than abort the process; turning that count back into a
/// `panic!` here is what makes it observable by `#[should_panic]` and by an ordinary test failure.
///
/// Wrap **only** the `process()` call (or `StereoBuffers::process_block`, which is allocation-free
/// by construction — see its doc comment). Do not wrap activation, deactivation or any host
/// callback: those are `[main-thread]` operations that are *supposed* to allocate.
pub fn audio_section<T>(f: impl FnOnce() -> T) -> T {
    assert_no_alloc::reset_violation_count();
    let result = assert_no_alloc::assert_no_alloc(f);
    assert_eq!(
        assert_no_alloc::violation_count(),
        0,
        "D-7.5/NFR-RT-010: allocation occurred inside an audio section"
    );
    result
}

// ---------------------------------------------------------------------------------------------
// The host handlers.
// ---------------------------------------------------------------------------------------------

/// The `[thread-safe]` half of the test host — and, unlike `clap_host_teardown.rs`'s `()`, one
/// that **records** what the plugin asked for instead of swallowing it.
///
/// `()` implements `SharedHandler` with three empty bodies, so a plugin's `request_restart()` /
/// `request_callback()` / `request_process()` vanish without trace. FR-CLAP-040's whole mechanism
/// is the plugin asking the host for a callback and then, from `on_main_thread`, asking for a
/// restart (`src/audio.rs` and `src/main_thread.rs`), so a test of it has to be able to see both.
#[derive(Debug, Default)]
pub struct TestHostShared {
    restart_requests: AtomicUsize,
    process_requests: AtomicUsize,
    callback_requests: AtomicUsize,
    flush_requests: AtomicUsize,
}

impl TestHostShared {
    /// How many times the plugin has called `request_restart()` since the last
    /// [`reset_request_counts`](Self::reset_request_counts).
    pub fn restart_requests(&self) -> usize {
        self.restart_requests.load(Ordering::SeqCst)
    }

    /// How many times the plugin has called `request_process()`.
    pub fn process_requests(&self) -> usize {
        self.process_requests.load(Ordering::SeqCst)
    }

    /// How many times the plugin has called `request_callback()` — the audio thread's half of
    /// FR-CLAP-040's latency-change sequence.
    pub fn callback_requests(&self) -> usize {
        self.callback_requests.load(Ordering::SeqCst)
    }

    /// How many times the plugin has called the `params` extension's `request_flush()`.
    ///
    /// Always zero unless the `host-ext-tests` feature is on, since without it the host never
    /// declares `HostParams` and the plugin's `get_extension::<HostParams>()` returns `None`.
    pub fn flush_requests(&self) -> usize {
        self.flush_requests.load(Ordering::SeqCst)
    }

    /// Zeroes all four counters. Useful to separate "what instantiation did" from "what the block
    /// under test did".
    pub fn reset_request_counts(&self) {
        self.restart_requests.store(0, Ordering::SeqCst);
        self.process_requests.store(0, Ordering::SeqCst);
        self.callback_requests.store(0, Ordering::SeqCst);
        self.flush_requests.store(0, Ordering::SeqCst);
    }
}

impl SharedHandler<'_> for TestHostShared {
    fn request_restart(&self) {
        self.restart_requests.fetch_add(1, Ordering::SeqCst);
    }

    fn request_process(&self) {
        self.process_requests.fetch_add(1, Ordering::SeqCst);
    }

    fn request_callback(&self) {
        self.callback_requests.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(feature = "host-ext-tests")]
impl HostParamsImplShared for TestHostShared {
    fn request_flush(&self) {
        self.flush_requests.fetch_add(1, Ordering::SeqCst);
    }
}

/// The `[main-thread]` half of the test host.
///
/// Holds the [`InitializedPluginHandle`] clack hands over once instantiation completes, plus
/// counters for the two host-side extension callbacks this plugin actually invokes
/// (`src/main_thread.rs` queries exactly `HostLatency` and `HostParams` and nothing else).
pub struct TestHostMainThread<'a> {
    shared: &'a TestHostShared,
    plugin: Option<InitializedPluginHandle<'a>>,
    latency_changes: usize,
    param_rescans: usize,
    param_clears: usize,
    #[cfg(feature = "host-ext-tests")]
    last_rescan_flags: Option<ParamRescanFlags>,
}

impl<'a> TestHostMainThread<'a> {
    fn new(shared: &'a TestHostShared) -> Self {
        Self {
            shared,
            plugin: None,
            latency_changes: 0,
            param_rescans: 0,
            param_clears: 0,
            #[cfg(feature = "host-ext-tests")]
            last_rescan_flags: None,
        }
    }

    /// The `[thread-safe]` handler this main thread was built against.
    pub fn shared(&self) -> &'a TestHostShared {
        self.shared
    }

    /// The plugin handle clack provided at `initialized()`, or `None` if instantiation has not
    /// reached that point yet.
    pub fn plugin(&self) -> Option<&InitializedPluginHandle<'a>> {
        self.plugin.as_ref()
    }

    /// How many times the plugin called `HostLatency::changed()`. Only ever non-zero under the
    /// `host-ext-tests` feature — see [`TestHostShared::flush_requests`] for why.
    pub fn latency_changes(&self) -> usize {
        self.latency_changes
    }

    /// How many times the plugin called `HostParams::rescan()`.
    pub fn param_rescans(&self) -> usize {
        self.param_rescans
    }

    /// How many times the plugin called `HostParams::clear()`.
    pub fn param_clears(&self) -> usize {
        self.param_clears
    }

    /// The flags carried by the most recent `HostParams::rescan()` call, if any.
    #[cfg(feature = "host-ext-tests")]
    pub fn last_rescan_flags(&self) -> Option<ParamRescanFlags> {
        self.last_rescan_flags
    }

    /// Zeroes the extension-callback counters.
    pub fn reset_callback_counts(&mut self) {
        self.latency_changes = 0;
        self.param_rescans = 0;
        self.param_clears = 0;
        #[cfg(feature = "host-ext-tests")]
        {
            self.last_rescan_flags = None;
        }
    }
}

impl<'a> MainThreadHandler<'a> for TestHostMainThread<'a> {
    fn initialized(&mut self, instance: InitializedPluginHandle<'a>) {
        self.plugin = Some(instance);
    }
}

#[cfg(feature = "host-ext-tests")]
impl HostLatencyImpl for TestHostMainThread<'_> {
    fn changed(&mut self) {
        self.latency_changes += 1;
    }
}

#[cfg(feature = "host-ext-tests")]
impl HostParamsImplMainThread for TestHostMainThread<'_> {
    fn rescan(&mut self, flags: ParamRescanFlags) {
        self.param_rescans += 1;
        self.last_rescan_flags = Some(flags);
    }

    fn clear(&mut self, _param_id: ClapId, _flags: ParamClearFlags) {
        self.param_clears += 1;
    }
}

/// The `[audio-thread]` half of the test host.
///
/// Carries a reference to [`TestHostShared`] so a test that only holds the audio processor can
/// still read the request counters (`StartedPluginAudioProcessor::access_handler`).
pub struct TestHostAudioProcessor<'a> {
    shared: &'a TestHostShared,
}

impl<'a> TestHostAudioProcessor<'a> {
    /// Builds the audio-thread handler. [`activate`] already does this; a test only needs it
    /// directly when it calls `PluginInstance::activate` itself (e.g. to assert on an activation
    /// failure).
    pub fn new(shared: &'a TestHostShared) -> Self {
        Self { shared }
    }

    /// The `[thread-safe]` handler, reachable from the audio-processing context.
    pub fn shared(&self) -> &'a TestHostShared {
        self.shared
    }
}

impl<'a> AudioProcessorHandler<'a> for TestHostAudioProcessor<'a> {}

/// The test host itself — the type that ties the three handlers together.
///
/// Use it as `PluginInstance::<TestHost>` everywhere; [`instantiate`] already does.
pub struct TestHost;

impl HostHandlers for TestHost {
    type Shared<'a> = TestHostShared;
    type MainThread<'a> = TestHostMainThread<'a>;
    type AudioProcessor<'a> = TestHostAudioProcessor<'a>;

    /// Declares the two host-side extensions `src/main_thread.rs` actually queries. Without this
    /// the plugin's `host.shared().get_extension::<HostLatency>()` returns `None` and its
    /// latency/param notifications become silent no-ops — which is a legitimate host
    /// configuration, and precisely the wrong one for a test of FR-CLAP-040 or of the params
    /// rescan path.
    #[cfg(feature = "host-ext-tests")]
    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder.register::<HostLatency>().register::<HostParams>();
    }
}

// ---------------------------------------------------------------------------------------------
// Instantiation — one place, so six test files cannot drift.
// ---------------------------------------------------------------------------------------------

/// NFR-PERF-010's reference sample rate, and what every M9b CLAP test should activate at unless
/// it is specifically testing rate changes.
pub const DEFAULT_SAMPLE_RATE: f64 = 48_000.0;

/// The largest block this harness's default configuration accepts.
pub const DEFAULT_MAX_BLOCK: u32 = 512;

/// The `HostInfo` every instance in this harness is created with.
pub fn host_info() -> HostInfo {
    HostInfo::new(
        "Namir test host",
        "Namir",
        "https://example.invalid",
        env!("CARGO_PKG_VERSION"),
    )
    .expect("host info must be constructible from static strings")
}

/// Loads the in-process CLAP entry for `NamirClapPlugin`.
///
/// `clack-host` caches loaded entries by entry pointer (`clack_host::entry::cache`), so calling
/// this repeatedly in one binary returns handles onto the same loaded entry rather than
/// re-initialising it — a test may hold one per instance without thinking about it.
pub fn entry() -> PluginEntry {
    PluginEntry::load_from_clack::<SinglePluginEntry<NamirClapPlugin>>(c"")
        .expect("the in-process entry must load")
}

/// Creates one live plugin instance against [`TestHost`].
///
/// The plugin id is read from the plugin's **own** descriptor rather than restated as a literal,
/// so this harness cannot drift from what the entry advertises (FR-CLAP-010's id).
///
/// The returned instance must be dropped on the thread that created it — that drop is
/// `clap_plugin.destroy`, and `impl Drop for NamirShared` joins the instance's worker pool inside
/// it. Any [`StartedPluginAudioProcessor`] must be stopped and passed to
/// `PluginInstance::deactivate` **before** the instance is dropped, or the instance leaks (a
/// `clack-host` contract, not a Namir one).
pub fn instantiate(entry: &PluginEntry) -> PluginInstance<TestHost> {
    let descriptor = <NamirClapPlugin as DefaultPluginFactory>::get_descriptor();
    let plugin_id = descriptor.id().expect("the descriptor must carry an id");

    PluginInstance::<TestHost>::new(
        |_| TestHostShared::default(),
        |shared| TestHostMainThread::new(shared),
        entry,
        plugin_id,
        &host_info(),
    )
    .expect("the plugin must instantiate")
}

/// [`entry`] + [`instantiate`] in one call, for the common case of one instance per test.
///
/// Returns the entry alongside the instance because dropping the entry handle while an instance is
/// alive is pointless churn — bind both (`let (_entry, mut instance) = ...`).
pub fn instantiate_default() -> (PluginEntry, PluginInstance<TestHost>) {
    let entry = entry();
    let instance = instantiate(&entry);
    (entry, instance)
}

/// A `PluginAudioConfiguration`. Panics inside `activate` if `min` or `max` is 0 or `min > max`
/// (clack validates it), so pass sane values.
pub fn config(rate: f64, min: u32, max: u32) -> PluginAudioConfiguration {
    PluginAudioConfiguration {
        sample_rate: rate,
        min_frames_count: min,
        max_frames_count: max,
    }
}

/// [`config`] at [`DEFAULT_SAMPLE_RATE`], accepting 1..=[`DEFAULT_MAX_BLOCK`] frames.
pub fn default_config() -> PluginAudioConfiguration {
    config(DEFAULT_SAMPLE_RATE, 1, DEFAULT_MAX_BLOCK)
}

/// Activates `instance`, returning the audio processor in its `stopped` state.
///
/// Call `.start_processing()` on the result to get a [`StartedPluginAudioProcessor`], which is what
/// [`StereoBuffers::process_block`] takes. The processor must be stopped and passed back to
/// `PluginInstance::deactivate` before the instance is dropped.
pub fn activate(
    instance: &mut PluginInstance<TestHost>,
    configuration: PluginAudioConfiguration,
) -> StoppedPluginAudioProcessor<TestHost> {
    instance
        .activate(
            |shared, _main_thread| TestHostAudioProcessor::new(shared),
            configuration,
        )
        .expect("the plugin must activate")
}

/// [`activate`] at [`default_config`].
pub fn activate_default(
    instance: &mut PluginInstance<TestHost>,
) -> StoppedPluginAudioProcessor<TestHost> {
    activate(instance, default_config())
}

/// Fetches a plugin-side extension handle (`PluginAudioPorts`, `PluginParams`, `PluginState`,
/// `PluginLatency`, `PluginGui`, …) from a live instance.
///
/// Returns `None` if the plugin does not implement it. The extension *methods* only exist under
/// the `host-ext-tests` feature, which is why this helper is gated too.
#[cfg(feature = "host-ext-tests")]
pub fn plugin_extension<E>(instance: &mut PluginInstance<TestHost>) -> Option<E>
where
    E: Extension<ExtensionSide = PluginExtensionSide>,
{
    instance.plugin_handle().get_extension::<E>()
}

/// Like [`plugin_extension`] but panics with the extension's CLAP identifier when it is absent —
/// the right behaviour when a test's whole premise is that Namir implements it.
#[cfg(feature = "host-ext-tests")]
pub fn require_plugin_extension<E>(instance: &mut PluginInstance<TestHost>) -> E
where
    E: Extension<ExtensionSide = PluginExtensionSide>,
{
    plugin_extension::<E>(instance).unwrap_or_else(|| {
        panic!(
            "the plugin must implement the {:?} extension",
            E::IDENTIFIERS
        )
    })
}

/// A `[main-thread]` handle onto a live instance — what every `clack-extensions` host-side method
/// takes as its first argument.
#[cfg(feature = "host-ext-tests")]
pub fn main_thread_handle(instance: &mut PluginInstance<TestHost>) -> PluginMainThreadHandle<'_> {
    instance.plugin_handle()
}

// ---------------------------------------------------------------------------------------------
// Stereo buffer plumbing.
// ---------------------------------------------------------------------------------------------

/// The channel count `src/audio_ports_ext.rs` declares, on both the input and the output port.
pub const CHANNELS: usize = 2;

/// One stereo input port and one stereo output port, plus everything `process()` needs around
/// them, allocated once at construction and reused for every block.
///
/// # Why block size can vary without reallocating
///
/// The channel `Vec`s are sized to `max_frames` once. Each [`process_block`](Self::process_block)
/// rebuilds the `InputAudioBuffers`/`OutputAudioBuffers` views over those same `Vec`s and then
/// calls `InputAudioBuffers::truncate` / `OutputAudioBuffers::truncate`, which shortens only the
/// frame count exposed to the plugin — it does not touch the underlying storage. `AudioPorts` was
/// built with `with_capacity(CHANNELS, 1)`, so the pointer list it refills each call never grows
/// past its capacity either. The whole path is therefore allocation-free and safe to run inside
/// [`audio_section`]; that is the property NFR-RT-010's CLAP-side check depends on.
///
/// Ports are *not* aliased in place even though `audio_ports_ext.rs` declares an `in_place_pair`:
/// separate input and output storage makes "did the plugin actually write the output" an
/// answerable question, which a shared buffer would destroy.
pub struct StereoBuffers {
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    input: [Vec<f32>; CHANNELS],
    output: [Vec<f32>; CHANNELS],
    out_events: EventBuffer,
    max_frames: usize,
    steady_time: u64,
}

impl StereoBuffers {
    /// Allocates for blocks of up to `max_frames` frames per channel.
    pub fn new(max_frames: usize) -> Self {
        assert!(max_frames > 0, "max_frames must be non-zero");
        Self {
            input_ports: AudioPorts::with_capacity(CHANNELS, 1),
            output_ports: AudioPorts::with_capacity(CHANNELS, 1),
            input: [vec![0.0; max_frames], vec![0.0; max_frames]],
            output: [vec![0.0; max_frames], vec![0.0; max_frames]],
            // Standard-event slots up front so a plugin that emits output events does not
            // allocate mid-block and trip `audio_section`. Namir *does* emit them since issue
            // #94: a parameter the user moved in the plugin's own editor comes out as a
            // gesture-wrapped automation point (three events), so the worst case is three times
            // `namir_params::REGISTRY`'s length in one block -- 96 today. Sized past that, since
            // an `EventBuffer` that has to grow allocates, and this buffer is written from inside
            // an `audio_section`.
            out_events: EventBuffer::with_capacity(256),
            max_frames,
            steady_time: 0,
        }
    }

    /// [`new`](Self::new) at [`DEFAULT_MAX_BLOCK`].
    pub fn default_size() -> Self {
        Self::new(DEFAULT_MAX_BLOCK as usize)
    }

    /// The per-channel capacity this was built with.
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Read access to one input channel's full backing storage.
    pub fn input(&self, channel: usize) -> &[f32] {
        &self.input[channel]
    }

    /// Write access to one input channel's full backing storage.
    pub fn input_mut(&mut self, channel: usize) -> &mut [f32] {
        &mut self.input[channel]
    }

    /// Read access to one output channel's full backing storage. Only the first `frames` samples
    /// were written by the last [`process_block`](Self::process_block).
    pub fn output(&self, channel: usize) -> &[f32] {
        &self.output[channel]
    }

    /// Write access to one output channel's full backing storage — for pre-poisoning it with a
    /// sentinel so "the plugin wrote nothing" is distinguishable from "the plugin wrote silence".
    pub fn output_mut(&mut self, channel: usize) -> &mut [f32] {
        &mut self.output[channel]
    }

    /// Fills every input sample from `f(channel, frame)`.
    pub fn fill_input(&mut self, mut f: impl FnMut(usize, usize) -> f32) {
        for (channel, buf) in self.input.iter_mut().enumerate() {
            for (frame, sample) in buf.iter_mut().enumerate() {
                *sample = f(channel, frame);
            }
        }
    }

    /// Zeroes every input sample.
    pub fn silence_input(&mut self) {
        self.fill_input(|_, _| 0.0);
    }

    /// Sets every output sample to `value` — call before a block to make an unwritten output
    /// detectable.
    pub fn poison_output(&mut self, value: f32) {
        for buf in &mut self.output {
            buf.fill(value);
        }
    }

    /// The events the plugin emitted during the last block.
    pub fn output_events(&self) -> &EventBuffer {
        &self.out_events
    }

    /// The running sample counter handed to `process()` as `steady_time`.
    pub fn steady_time(&self) -> u64 {
        self.steady_time
    }

    /// Rewinds the `steady_time` counter — legal only immediately after
    /// `StartedPluginAudioProcessor::reset`, per CLAP's own contract.
    pub fn reset_steady_time(&mut self) {
        self.steady_time = 0;
    }

    /// Runs one block of `frames` frames with no input events.
    ///
    /// Allocation-free (see this type's doc comment), so `audio_section(|| bufs.process_block(&mut
    /// p, 64))` is the NFR-RT-010 check.
    pub fn process_block(
        &mut self,
        processor: &mut StartedPluginAudioProcessor<TestHost>,
        frames: u32,
    ) -> Result<ProcessStatus, PluginInstanceError> {
        self.process_block_with_events(processor, frames, &InputEvents::empty())
    }

    /// Runs one block of `frames` frames, delivering `input_events` to the plugin.
    ///
    /// Build the list with a `clack_host::prelude::EventBuffer` and `as_input()`. Note that
    /// pushing into that buffer *does* allocate, so populate it outside any [`audio_section`].
    pub fn process_block_with_events(
        &mut self,
        processor: &mut StartedPluginAudioProcessor<TestHost>,
        frames: u32,
        input_events: &InputEvents,
    ) -> Result<ProcessStatus, PluginInstanceError> {
        assert!(
            frames as usize <= self.max_frames,
            "block of {frames} frames exceeds this StereoBuffers' capacity of {}",
            self.max_frames
        );
        self.out_events.clear();

        let Self {
            input_ports,
            output_ports,
            input,
            output,
            out_events,
            steady_time,
            ..
        } = self;

        let mut inputs = input_ports.with_input_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_input_only(
                input
                    .iter_mut()
                    .map(|c| InputChannel::variable(c.as_mut_slice())),
            ),
        }]);
        let mut outputs = output_ports.with_output_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only(
                output.iter_mut().map(|c| c.as_mut_slice()),
            ),
        }]);

        // The whole point: the storage stays `max_frames` long, only the exposed frame count moves.
        inputs.truncate(frames);
        outputs.truncate(frames);

        let status = processor.process(
            &inputs,
            &mut outputs,
            input_events,
            &mut out_events.as_output(),
            Some(*steady_time),
            None,
        );

        *steady_time += u64::from(frames);
        status
    }
}

// ---------------------------------------------------------------------------------------------
// Deterministic signal generators. No `rand` dependency, by design.
// ---------------------------------------------------------------------------------------------

/// The tone frequency M9b's CLAP tests use when they need a single, unambiguous partial.
pub const SINE_FREQ_HZ: f64 = 1_000.0;

/// A seeded linear congruential generator — Numerical Recipes' 64-bit constants.
///
/// Deliberately not a `rand` dependency: this workspace generates every fixture from a seed
/// (D-19.1), and a five-line LCG is enough for "a signal that is not silence and reproduces
/// exactly". It is not a good PRNG and must not be used where statistical quality matters.
#[derive(Debug, Clone)]
pub struct Lcg {
    state: u64,
}

impl Lcg {
    /// Seeds the generator. Any seed is valid; the same seed always yields the same sequence.
    pub fn new(seed: u64) -> Self {
        Self {
            // Odd-ify so a zero seed still advances.
            state: seed.wrapping_mul(2).wrapping_add(1),
        }
    }

    /// The next raw 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    /// The next sample, uniform in `[-1.0, 1.0)`.
    pub fn next_f32(&mut self) -> f32 {
        // Top 24 bits -> [0, 1), exactly representable in f32.
        let bits = (self.next_u64() >> 40) as u32;
        (bits as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
    }
}

/// Fills `dst` with seeded noise in `[-amplitude, amplitude)`.
pub fn fill_noise(dst: &mut [f32], seed: u64, amplitude: f32) {
    let mut lcg = Lcg::new(seed);
    for sample in dst.iter_mut() {
        *sample = lcg.next_f32() * amplitude;
    }
}

/// A fresh `Vec` of seeded noise in `[-amplitude, amplitude)`.
pub fn noise(len: usize, seed: u64, amplitude: f32) -> Vec<f32> {
    let mut v = vec![0.0; len];
    fill_noise(&mut v, seed, amplitude);
    v
}

/// Fills `dst` with a sine at `freq_hz`, sampled at `sample_rate`, starting at sample index
/// `start_frame` — so consecutive blocks stay phase-continuous by passing a running frame count.
pub fn fill_sine(
    dst: &mut [f32],
    freq_hz: f64,
    sample_rate: f64,
    amplitude: f32,
    start_frame: u64,
) {
    let step = std::f64::consts::TAU * freq_hz / sample_rate;
    for (i, sample) in dst.iter_mut().enumerate() {
        let phase = step * (start_frame + i as u64) as f64;
        *sample = (phase.sin() as f32) * amplitude;
    }
}

/// A fresh `Vec` holding a [`SINE_FREQ_HZ`] tone at `sample_rate`, starting at phase zero.
pub fn sine_1k(len: usize, sample_rate: f64, amplitude: f32) -> Vec<f32> {
    let mut v = vec![0.0; len];
    fill_sine(&mut v, SINE_FREQ_HZ, sample_rate, amplitude, 0);
    v
}

/// Peak absolute sample value of `buf` — the cheapest "did anything come out" assertion.
pub fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
}

/// `true` if every sample in `buf` is finite (no `NaN`, no infinity).
pub fn all_finite(buf: &[f32]) -> bool {
    buf.iter().all(|s| s.is_finite())
}
