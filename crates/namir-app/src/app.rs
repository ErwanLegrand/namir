//! Top-level wiring: [`run`] is `main`'s actual body, factored out so `main.rs` stays a one-line
//! entry point. This module is deliberately thin glue over already-tested pieces
//! ([`crate::device_state`]'s selection logic, [`crate::settings`]'s persistence,
//! [`crate::instance::SharedInstance`], [`crate::worker`]'s background thread,
//! [`crate::host::AppHost`]'s `UiHost` bridge, [`crate::stream`]'s duplex path) — real device I/O
//! and window creation cannot be meaningfully unit-tested (this crate's own final report explains
//! why, and `docs/manual-tests/` records what to check by hand instead), so this module's job is
//! to compose pieces that already have their own tests, not to introduce new untested logic of its
//! own.
//!
//! One exception, added at M11 and kept honest by the second half of that sentence:
//! [`negotiate_share_mode`] is real decision logic (FR-IO-020's all-or-nothing exclusive-mode rule)
//! that has no lower-level home — [`crate::device_state`] is deliberately pure and takes no
//! [`crate::audio_io::AudioBackend`], and this decision has to query one. It is therefore a
//! separate, backend-generic function with its own unit tests at the foot of this file, driven by
//! [`crate::stream::FakeBackend`], rather than logic inlined into [`run`] where nothing could reach
//! it.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::{PrepareContext, build_default_engine};
use namir_state::State;
use namir_worker::pool::ThreadPool;
use namir_worker::{EngineConfig, Instance, ResourceCache};

use crate::audio_io::{
    AudioBackend, AudioIoError, CpalBackend, DeviceInfo, ExclusiveModeOutcome, HostInfo, ShareMode,
    StreamFailure, StreamParams,
};
use crate::host::AppHost;
use crate::instance::SharedInstance;
use crate::settings::{self, AppSettings};
use crate::startup_probe;
use crate::stream::{self, StreamSetup};
use crate::worker::{WorkerContext, WorkerHandle};
use crate::xrun::XrunCounter;

/// Falls back to a working default if [`namir_platform::config_dir`] returns `None` (an
/// unrecognised environment — see that function's own doc comment). A session with no persistent
/// config directory still runs; it just doesn't remember anything across restarts, which is a
/// strictly worse but still-functional degradation (P8), not a reason to refuse to start.
///
/// [`crate::startup_probe`]'s override takes precedence when set, so an NFR-PERF-030 measurement
/// runs against a configuration directory the harness owns rather than this machine's real one.
/// Unset in every ordinary launch, which is every launch that is not a benchmark.
fn resolve_config_dir() -> Option<PathBuf> {
    startup_probe::config_dir_override().or_else(namir_platform::config_dir)
}

/// FR-IO-010/040: enumerates and negotiates one direction (input or output). Returns the selected
/// device and the sample-rate/buffer/channel choice space needed for the *other* direction's
/// negotiation to check against (`crate::device_state::negotiate_shared_sample_rate`) — kept
/// separate from applying the choice so the caller can negotiate the shared sample rate before
/// picking a final buffer size per direction.
pub(crate) struct DirectionSetup {
    pub(crate) device: DeviceInfo,
    pub(crate) fell_back_from: Option<String>,
    pub(crate) configs: Vec<crate::audio_io::SupportedConfigRange>,
    /// The mode `configs` actually describe, which is not necessarily the one that was asked for
    /// — see [`crate::audio_io::EnumeratedConfigs`]. `negotiate_audio` reads it to decide whether
    /// a refused exclusive request has anything to re-enumerate.
    pub(crate) enumerated_share_mode: crate::audio_io::ShareMode,
}

pub(crate) fn setup_direction(
    backend: &dyn AudioBackend,
    host: &HostInfo,
    devices: Result<Vec<DeviceInfo>, crate::audio_io::AudioIoError>,
    remembered_device: Option<&str>,
    configs_of: impl Fn(
        &HostInfo,
        &DeviceInfo,
    )
        -> Result<crate::audio_io::EnumeratedConfigs, crate::audio_io::AudioIoError>,
) -> Option<DirectionSetup> {
    let devices = devices.ok()?;
    let selection = crate::device_state::select_device(&devices, remembered_device)?;
    // A direction that could not be enumerated at all has no ranges and describes no mode: shared
    // is the conservative answer, and the one that makes `negotiate_audio` skip a second pass it
    // has no new information for.
    let enumerated =
        configs_of(host, &selection.device).unwrap_or(crate::audio_io::EnumeratedConfigs {
            share_mode: crate::audio_io::ShareMode::Shared,
            ranges: Vec::new(),
        });
    let _ = backend; // reserved for a future multi-host UI; kept as a parameter for that seam.
    Some(DirectionSetup {
        device: selection.device,
        fell_back_from: selection.fell_back_from,
        configs: enumerated.ranges,
        enumerated_share_mode: enumerated.share_mode,
    })
}

/// What the user asked for, as [`negotiate_audio`] needs it: the remembered device names, rate and
/// buffer size, plus FR-IO-020's exclusive-mode request. A struct rather than five positional
/// arguments because [`crate::host`] fills it from its own `current_*` fields, not from
/// [`crate::settings::AppSettings`] directly.
pub(crate) struct AudioPreferences<'a> {
    pub(crate) input_device: Option<&'a str>,
    pub(crate) output_device: Option<&'a str>,
    pub(crate) sample_rate_hz: Option<u32>,
    pub(crate) buffer_size_frames: Option<u32>,
    pub(crate) exclusive_mode: bool,
}

/// One settled audio configuration: the two devices, what both sides agreed on, and the share mode
/// the streams will open in.
pub(crate) struct AudioNegotiation {
    pub(crate) input: DirectionSetup,
    pub(crate) output: DirectionSetup,
    pub(crate) sample_rate_hz: u32,
    pub(crate) buffer_frames: Option<u32>,
    pub(crate) input_channels: u16,
    pub(crate) output_channels: u16,
    pub(crate) share_mode: ShareModeDecision,
}

/// Enumerates both directions and negotiates rate, buffer, channels and share mode — the sequence
/// [`run`] and [`crate::host::AppHost::initiate_audio_reopen`] both need, in one place because
/// issue #190 requires it to run **twice** in one case and two copies would drift.
///
/// # Why enumeration is inside the negotiation (issue #190)
///
/// Configs are enumerated in the share mode the session is *asking* for, because the two modes
/// describe different devices ([`AudioBackend::input_configs`]). But whether exclusive mode is
/// granted is only known after [`negotiate_share_mode`] has asked both devices, which needs a rate
/// and channel count, which come from the configs. So when an exclusive request is refused, the
/// first pass negotiated against ranges that do not apply to the shared session that will actually
/// run — and the whole sequence is repeated against the shared ranges. The refused decision itself
/// is kept: it was settled by the devices, not by the ranges, and re-asking would give the same
/// answer.
///
/// **The second pass is skipped when the first one never got exclusive ranges.** A device with no
/// reachable exclusive endpoint answers an exclusive request with its *shared* ranges
/// ([`crate::audio_io::EnumeratedConfigs`] records which it gave), and then refuses the mode — so
/// the gate would otherwise fire on every non-WASAPI host with `exclusive_mode: true` in its
/// settings file and repeat a query whose answer is already in hand. On Windows that repeat is COM
/// device enumeration on the start-up path NFR-PERF-030 measures.
pub(crate) fn negotiate_audio(
    backend: &dyn AudioBackend,
    host_info: &HostInfo,
    input_devices: Result<Vec<DeviceInfo>, crate::audio_io::AudioIoError>,
    output_devices: Result<Vec<DeviceInfo>, crate::audio_io::AudioIoError>,
    prefs: &AudioPreferences<'_>,
) -> Option<AudioNegotiation> {
    let input_devices = input_devices.ok()?;
    let output_devices = output_devices.ok()?;
    let enumerate = |mode: ShareMode| {
        let input = setup_direction(
            backend,
            host_info,
            Ok(input_devices.clone()),
            prefs.input_device,
            |h, d| backend.input_configs(h, d, mode),
        )?;
        let output = setup_direction(
            backend,
            host_info,
            Ok(output_devices.clone()),
            prefs.output_device,
            |h, d| backend.output_configs(h, d, mode),
        )?;
        Some((input, output))
    };

    let requested = if prefs.exclusive_mode {
        ShareMode::Exclusive
    } else {
        ShareMode::Shared
    };
    let (input, output) = enumerate(requested)?;
    let settled = settle(&input, &output, prefs);

    let share_mode = negotiate_share_mode(
        backend,
        host_info,
        &input.device,
        StreamParams {
            sample_rate_hz: settled.0,
            buffer_frames: settled.1,
            channels: settled.2,
            share_mode: requested,
        },
        &output.device,
        StreamParams {
            sample_rate_hz: settled.0,
            buffer_frames: settled.1,
            channels: settled.3,
            share_mode: requested,
        },
        prefs.exclusive_mode,
    );

    // Either direction having answered in exclusive mode is enough: `settle` reads both sides'
    // ranges, so one exclusive list is enough to have skewed the result.
    let enumerated_exclusive = input.enumerated_share_mode == ShareMode::Exclusive
        || output.enumerated_share_mode == ShareMode::Exclusive;
    let (input, output, settled) = if enumerated_exclusive && share_mode.mode == ShareMode::Shared {
        let (input, output) = enumerate(ShareMode::Shared)?;
        let settled = settle(&input, &output, prefs);
        (input, output, settled)
    } else {
        (input, output, settled)
    };

    Some(AudioNegotiation {
        input,
        output,
        sample_rate_hz: settled.0,
        buffer_frames: settled.1,
        input_channels: settled.2,
        output_channels: settled.3,
        share_mode,
    })
}

/// Everything both call sites need between [`negotiate_audio`] and [`crate::stream::open`]: the
/// input for a [`StreamSetup`], plus the two lists FR-IO-040's selectors show.
pub(crate) struct AssembledAudioConfig {
    pub input_params: StreamParams,
    pub output_params: StreamParams,
    pub max_block_size: usize,
    pub channel_config: ChannelConfig,
    /// `None` when the negotiated rate is zero. Each caller decides what to do about it: start-up
    /// opens a windowless-audio session, the reopen path keeps the running stream and posts a
    /// notice — deliberately different, so this function does not choose.
    pub sample_rate: Option<SampleRate>,
    pub supported_sample_rates: Vec<u32>,
    pub supported_buffer_sizes: Vec<u32>,
}

/// Assembles one settled [`AudioNegotiation`] into the values a stream opens with — in one place
/// because [`run`] and [`crate::host::AppHost::initiate_audio_reopen`] both need all of them
/// (issue #192).
///
/// # Why this is shared rather than written twice
///
/// A fix applied to one copy and not the other is **silent**: the reopen path only runs when a
/// user changes a device or a rate in the settings panel, so a start-up-only fix looks correct in
/// every test and every launch. That has already happened twice — D-13.3's
/// [`crate::audio_io::output_buffer_request`] line (issue #166) was added to [`run`] first and to
/// [`crate::host`] separately, and issue #190's re-enumeration likewise.
///
/// The callers' *failure handling* stays theirs (see `sample_rate`): those differences are
/// intended, and are the reason this is not simply folded into [`negotiate_audio`].
///
/// # Where the line falls
///
/// This owns the values a *stream opens with*, not the negotiation result as a whole. Both
/// callers still destructure the [`AudioNegotiation`] afterwards for `input`, `output`,
/// `share_mode` and `buffer_frames`, because their remaining uses are genuinely call-site
/// specific — notice text, device names, the FR-IO-020 mode indicator, `buffer_decline_detail`.
/// A newly derived value belongs here if both open paths need it and out there if one does.
///
/// # FR-IO-020: one share-mode decision
///
/// The share mode was settled inside [`negotiate_audio`], once, before anything is opened: both
/// stream literals below and the mode indicator handed to [`crate::host::AppHost`] read their
/// value from that single decision, never from a second query.
pub(crate) fn assemble_stream_config(negotiated: &AudioNegotiation) -> AssembledAudioConfig {
    let AudioNegotiation {
        input,
        output,
        sample_rate_hz,
        buffer_frames,
        input_channels,
        output_channels,
        share_mode,
    } = negotiated;
    let (sample_rate_hz, buffer_frames) = (*sample_rate_hz, *buffer_frames);

    let input_params = StreamParams {
        sample_rate_hz,
        buffer_frames,
        channels: *input_channels,
        share_mode: share_mode.mode,
    };
    // Issue #166 (D-13.3): the output stream asks the device for its own buffer, so the render
    // path keeps a reserve instead of being drained every callback. The engine's block size still
    // comes from `buffer_frames` -- see `audio_io::output_buffer_request`, whose rule is
    // mode-independent.
    let output_params = StreamParams {
        sample_rate_hz,
        buffer_frames: crate::audio_io::output_buffer_request(),
        channels: *output_channels,
        share_mode: share_mode.mode,
    };

    AssembledAudioConfig {
        input_params,
        output_params,
        max_block_size: crate::audio_io::block_frames(buffer_frames),
        channel_config: if *output_channels >= 2 {
            ChannelConfig::MonoToStereo
        } else {
            ChannelConfig::Mono
        },
        sample_rate: SampleRate::new(sample_rate_hz),
        supported_sample_rates: crate::device_state::supported_sample_rates(
            &input.configs,
            &output.configs,
        ),
        supported_buffer_sizes: crate::device_state::supported_buffer_sizes(
            &input.configs,
            &output.configs,
            sample_rate_hz,
        ),
    }
}

/// `(sample_rate_hz, buffer_frames, input_channels, output_channels)` for one pair of enumerated
/// directions. Both sides' ranges, never one side's alone (issue #86).
fn settle(
    input: &DirectionSetup,
    output: &DirectionSetup,
    prefs: &AudioPreferences<'_>,
) -> (u32, Option<u32>, u16, u16) {
    let sample_rate_hz = crate::device_state::negotiate_shared_sample_rate(
        &input.configs,
        &output.configs,
        prefs.sample_rate_hz,
    )
    .unwrap_or(48_000);
    let buffer_frames = crate::device_state::negotiate_shared_buffer_size(
        &input.configs,
        &output.configs,
        sample_rate_hz,
        prefs.buffer_size_frames,
    );
    (
        sample_rate_hz,
        buffer_frames,
        crate::device_state::negotiate_channels(&input.configs, sample_rate_hz, 1).unwrap_or(1),
        crate::device_state::negotiate_channels(&output.configs, sample_rate_hz, 2).unwrap_or(1),
    )
}

/// FR-IO-020's settled answer for one session: the share mode both streams open with, and — when
/// exclusive mode was asked for and not granted — the notice detail explaining why the session is
/// running shared instead.
pub(crate) struct ShareModeDecision {
    pub(crate) mode: ShareMode,
    /// `None` whenever the answer needs no explanation: exclusive was never requested, or it was
    /// requested and granted.
    pub(crate) refusal_detail: Option<String>,
}

/// FR-IO-020: asks both devices whether they can provide exclusive mode and **ANDs the answers**,
/// so a session runs exclusive on both directions or on neither.
///
/// The all-or-nothing rule is deliberate, not a simplification. `docs/03-implementation-roadmap.md`
/// §18 rules out "a mode indicator that lies", and there is exactly one indicator
/// ([`namir_ui::AudioModeStatus`]) for a duplex path: if the output engaged exclusive and the input
/// did not, no single-valued indicator can be truthful, and a user who asked for exclusive mode to
/// stop other applications sharing their interface has still not got it. Degrading both to shared
/// is also what `docs/02-architecture.md` D-13.4 asks for — "degrade to shared rather than leave
/// the app with no audio".
///
/// Asked before any stream is opened; see [`AudioBackend::supports_exclusive`] for why a pre-flight
/// query rather than an open-and-retry.
pub(crate) fn negotiate_share_mode(
    backend: &dyn AudioBackend,
    host: &HostInfo,
    input_device: &DeviceInfo,
    input_params: StreamParams,
    output_device: &DeviceInfo,
    output_params: StreamParams,
    requested: bool,
) -> ShareModeDecision {
    if !requested {
        // The device is never asked when nothing was requested: an untouched settings file
        // (`AppSettings::default().exclusive_mode == false`) must change nothing about start-up,
        // including making a query it has no use for.
        return ShareModeDecision {
            mode: ShareMode::Shared,
            refusal_detail: None,
        };
    }

    let ask = |device: &DeviceInfo, params: StreamParams| {
        backend.supports_exclusive(
            host,
            device,
            StreamParams {
                share_mode: ShareMode::Exclusive,
                ..params
            },
        )
    };
    let input = ask(input_device, input_params);
    let output = ask(output_device, output_params);

    if input == ExclusiveModeOutcome::Engaged && output == ExclusiveModeOutcome::Engaged {
        return ShareModeDecision {
            mode: ShareMode::Exclusive,
            refusal_detail: None,
        };
    }

    let mut refused = Vec::new();
    if input != ExclusiveModeOutcome::Engaged {
        refused.push(format!("input \"{}\"", input_device.name));
    }
    if output != ExclusiveModeOutcome::Engaged {
        refused.push(format!("output \"{}\"", output_device.name));
    }
    // `ExclusiveModeOutcome::Unsupported` carries no diagnostic of its own, so this is as specific
    // a reason as the seam can honestly give -- said once, here, rather than paraphrased at each
    // call site.
    let reason = AudioIoError::ExclusiveModeUnavailable(
        "the audio backend reports no exclusive-mode support for this device and format"
            .to_string(),
    );
    ShareModeDecision {
        mode: ShareMode::Shared,
        refusal_detail: Some(format!(
            "{}; {reason}; continuing in shared mode",
            refused.join(", ")
        )),
    }
}

/// How many stream failures each direction's ring holds before it starts dropping them.
///
/// Sized for "several reports arriving between two GUI frames", not for a backlog: a stream that
/// is failing repeatedly needs one notice, not sixteen, and [`crate::host::AppHost`] drains this
/// every frame. Small enough that both rings together are a few kilobytes allocated once, at
/// stream open, and never again.
pub(crate) const STREAM_FAILURE_RING_SLOTS: usize = 16;

/// Builds one direction's `cpal` error callback (FR-IO-070), and the reason it is a function with
/// its own tests rather than a closure inlined into [`run`].
///
/// # What it must not do, and used to (issue #88)
///
/// `cpal` invokes an error callback on the stream's **own** thread — `crate::worker`'s
/// `AppEvent::StreamFailure` doc says so in as many words — so NFR-RT-010 and FR-ERR-030 apply to
/// it exactly as they apply to the data callback beside it. The closure this replaces did three
/// allocating things there: `format!` to build the notice detail, an
/// `mpsc::Sender::send` (which allocates a queue node), and — one layer down, in
/// `crate::audio_io`'s `to_stream_failure` — `cpal::Error::to_string()`.
///
/// What it does instead is what D-7.3's telemetry path already does in the other direction: push a
/// pre-allocated, `Copy`, heap-free value into a bounded ring sized at stream open, and let the UI
/// thread do the formatting. A full ring drops the report rather than blocking or growing, which
/// is the only RT-legal answer and costs nothing real: [`crate::host::AppHost`] deduplicates
/// identical notices anyway.
///
/// Every failure is pushed; nothing is classified here. Until issue #200 item 6 this closure also
/// counted `StreamFailure::Xrun`, a variant `cpal` 0.19 stopped producing — dropouts now arrive
/// per data callback as [`crate::audio_io::CallbackStatus::xrun`] and are counted in
/// [`crate::stream`], beside the bridge under/overruns they have to stay commensurable with.
pub(crate) fn stream_failure_sink(
    mut failures: rtrb::Producer<StreamFailure>,
) -> impl FnMut(StreamFailure) + Send + 'static {
    move |failure| {
        // `StreamFailure` is `Copy` and owns no heap, so the value handed back by a full ring is
        // dropped without a deallocation -- which is why the payload had to stop being a `String`.
        let _ = failures.push(failure);
    }
}

/// `main`'s real body. Blocks until the window is closed.
pub fn run() {
    startup_probe::entered();

    // FR-ERR-010, first thing and once per process: everything below this line — a settings file
    // that failed to parse, a device that could not be opened, a share mode that was refused — is
    // reported through `AppHost::push_notice`, which writes a log record only if a logger has been
    // installed. Installed before anything can report, so a launch that fails early is exactly the
    // launch whose log a bug report will have.
    //
    // `None` for the persisted level, deliberately: `AppSettings` (FR-IO-080's record) has no
    // verbosity field, and M9b does not add one — the plugin is environment-variable-only by
    // decision (roadmap §15 item 8) and giving the app a second, divergent control was ruled out of
    // this round. `NAMIR_LOG` therefore governs both products identically. The seam is already
    // there for the day a settings field arrives: the platform initialiser takes the level as a
    // parameter precisely so `namir-platform` need not know what `AppSettings` is.
    //
    // Before `resolve_config_dir` because the log's own location is `namir_platform::
    // log_file_path`, which is independent of the app's config directory and of
    // `startup_probe`'s override of it — a probed launch logs to the same place a real one does.
    //
    // Through `crate::diagnostics` rather than `namir-platform` directly: this file is on
    // FR-ERR-030's audio-thread list (it owns `stream_failure_sink`), so it may not name the
    // logger even for a main-thread call. See that module's doc comment.
    crate::diagnostics::install();

    let config_dir = resolve_config_dir();

    let (settings, settings_warning) = match &config_dir {
        Some(dir) => settings::load(&settings::settings_path(dir)),
        None => (AppSettings::default(), None),
    };

    let backend = Arc::new(CpalBackend::new());
    let host_info = match &settings.host_name {
        Some(name) => backend
            .hosts()
            .into_iter()
            .find(|h| &h.name == name)
            .unwrap_or_else(|| backend.default_host()),
        None => backend.default_host(),
    };

    // FR-IO-020/040: enumerate in the share mode the session asks for, negotiate, and — when an
    // exclusive request is refused — do both again against the shared ranges (issue #190).
    let negotiated = negotiate_audio(
        backend.as_ref(),
        &host_info,
        backend.input_devices(&host_info),
        backend.output_devices(&host_info),
        &AudioPreferences {
            input_device: settings.input_device_name.as_deref(),
            output_device: settings.output_device_name.as_deref(),
            sample_rate_hz: settings.sample_rate_hz,
            buffer_size_frames: settings.buffer_size_frames,
            exclusive_mode: settings.exclusive_mode,
        },
    );

    let Some(negotiated) = negotiated else {
        eprintln!(
            "namir: no usable input/output audio device found on host \"{}\"; the window \
             will still open, but no audio will process. See \
             docs/manual-tests/fr-io-070-device-removal.md.",
            host_info.name
        );
        open_window_without_audio(config_dir);
        return;
    };
    // Every value between the negotiation and the open comes from one shared function, so this
    // path and the reopen path in `crate::host` cannot drift (issue #192). Only the failure
    // handling below is this call site's own.
    let AssembledAudioConfig {
        input_params,
        output_params,
        max_block_size,
        channel_config,
        sample_rate,
        supported_sample_rates,
        supported_buffer_sizes,
    } = assemble_stream_config(&negotiated);
    let AudioNegotiation {
        input,
        output,
        sample_rate_hz,
        buffer_frames,
        share_mode,
        ..
    } = negotiated;

    let Some(sample_rate) = sample_rate else {
        eprintln!(
            "namir: negotiated an invalid sample rate ({sample_rate_hz} Hz); refusing to open a stream."
        );
        open_window_without_audio(config_dir);
        return;
    };
    let ctx = match PrepareContext::new(sample_rate, max_block_size, channel_config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("namir: could not prepare the engine: {e:?}");
            open_window_without_audio(config_dir);
            return;
        }
    };

    let (engine, endpoint) = match build_default_engine(&ctx) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("namir: could not build the engine: {e:?}");
            open_window_without_audio(config_dir);
            return;
        }
    };

    let cache = ResourceCache::shared();
    // `TelemetryReader` is `Clone` (D-7.3), cloned before `Instance::new` consumes the rest of
    // `endpoint` -- see `crate::instance`'s module doc comment, matching `namir-clap::audio`'s
    // `activate` (`crates/namir-clap/src/audio.rs`).
    let telemetry = endpoint.telemetry.clone();
    let instance = SharedInstance::new(Instance::new(EngineConfig { ctx }, endpoint));

    let library_dir = config_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("namir-session-only"));
    let (library, _) = namir_worker::library::LibraryService::open_at_with_roots(
        &library_dir,
        settings.library_roots.clone(),
    );
    // M14 (§22 R-18): `open_at` no longer reads the index file, and **the standalone asks for it
    // anyway, here, deliberately.** The deferral exists for the *plugin*, where a host instantiates
    // one instance per track and NFR-PERF-040's 200 ms is a per-instance budget the index parse was
    // eating whole. This process launches once, has a user waiting in front of one window, and
    // measures itself as "start-up to audible **with a warm library index**" (NFR-PERF-030) — a
    // launch that reported an empty library and filled it in later would not be that measurement,
    // and `startup_probe::audible` below would report an index of zero entries.
    library.ensure_loaded();
    let library_warnings = library.take_load_warnings();
    let library_roots = (*library.roots()).clone();
    let library = Arc::new(library);
    // NFR-PERF-030's "with a warm library index": captured here, where it is true, so the startup
    // probe's marker reports the size of the index this launch actually read rather than leaving a
    // harness to assume one. An `Arc` clone and a `len()`.
    let library_index_entries = library.snapshot().len();

    let default_state = State::defaults();
    // NFR-PERF-030's "default state loaded": that half of the requirement has no event of its own
    // — it is satisfied implicitly, here and at `build_default_engine` above — so rather than
    // invent one, the probe reports what was actually built and the benchmark checks it against
    // `namir_params::REGISTRY`. See `crate::startup_probe`'s module doc comment.
    let default_state_params = default_state.params.iter().count();
    let state = Arc::new(Mutex::new(default_state));
    let worker_ctx = WorkerContext {
        instance: instance.clone(),
        cache: Arc::clone(&cache),
        library: Arc::clone(&library),
        pool: ThreadPool::new(),
        library_roots: library_roots.clone(),
        state: Arc::clone(&state),
    };
    let worker = WorkerHandle::spawn(worker_ctx);

    let xruns = Arc::new(XrunCounter::new());

    // FR-IO-020's mode indicator: the mode actually granted, never the one requested. The output
    // device names it -- see `namir_ui::AudioModeStatus::device_name` for why one name is enough
    // when the mode is settled across both directions.
    let audio_mode = Some(namir_ui::AudioModeStatus {
        share_mode: share_mode.mode.into(),
        device_name: output.device.name.clone(),
    });
    let mut host = AppHost::new(
        instance,
        worker,
        telemetry,
        Arc::clone(&library),
        state,
        audio_mode,
    );
    let reopen_ctx = crate::host::AudioReopenContext {
        backend: Arc::clone(&backend) as Arc<dyn AudioBackend>,
        host_info: host_info.clone(),
        xruns: Arc::clone(&xruns),
    };
    host.enable_audio_reopen(reopen_ctx);
    let input_device_names: Vec<String> = backend
        .input_devices(&host_info)
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.name)
        .collect();
    let output_device_names: Vec<String> = backend
        .output_devices(&host_info)
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.name)
        .collect();
    host.configure_audio_devices(
        config_dir.clone(),
        settings.clone(),
        input_device_names,
        output_device_names,
        Some(input.device.name.clone()),
        Some(output.device.name.clone()),
        supported_sample_rates,
        sample_rate_hz,
        supported_buffer_sizes,
        buffer_frames.unwrap_or(256),
    );
    // FR-STATE-030: `<config_dir>/Presets` (`namir_platform::presets` owns preset location and
    // naming rules). `resolve_config_dir`'s answer, not `namir_platform::config_dir`'s directly,
    // so a NFR-PERF-030 measurement run stays inside the directory its harness owns.
    if let Some(dir) = &config_dir {
        host.watch_presets(crate::presets::preset_dir_under(dir));
        host.watch_config_dir(dir.clone());
    }
    if let Some(w) = settings_warning {
        host.report(w.code, w.detail);
    }
    for w in library_warnings {
        host.report(w.code, w.detail);
    }
    if let Some(from) = &input.fell_back_from {
        host.report(
            crate::error_codes::REMEMBERED_DEVICE_UNAVAILABLE,
            format!("input \"{from}\", using \"{}\"", input.device.name),
        );
    }
    if let Some(from) = &output.fell_back_from {
        host.report(
            crate::error_codes::REMEMBERED_DEVICE_UNAVAILABLE,
            format!("output \"{from}\", using \"{}\"", output.device.name),
        );
    }
    if let Some(detail) = share_mode.refusal_detail {
        host.report(crate::error_codes::EXCLUSIVE_MODE_UNAVAILABLE, detail);
    }
    if let Some(requested) = settings.buffer_size_frames
        && let Some(detail) = crate::audio_io::buffer_decline_detail(requested, buffer_frames)
    {
        host.report(crate::error_codes::BUFFER_SIZE_DECLINED, detail);
    }

    let stream_setup = StreamSetup {
        backend: backend.as_ref(),
        input_host: host_info.clone(),
        input_device: input.device.clone(),
        input_params,
        output_host: host_info.clone(),
        output_device: output.device.clone(),
        output_params,
        channel_config,
        input_channel_index: settings.channel_mapping.input_channel.unwrap_or(0),
        output_channel_left: settings.channel_mapping.output_channel_left.unwrap_or(0),
        output_channel_right: settings.channel_mapping.output_channel_right.unwrap_or(1),
        max_block_size,
    };

    // The two device names the failure notice needs, captured before `stream_setup` is consumed.
    // Issue #44's smallest half: the app knew which device and which direction had failed and
    // dropped both, so the notice a human read on 2026-08-27 named neither. They are handed to
    // `AppHost` rather than into the callbacks (issue #88), because that is where the notice is
    // now built -- on the UI thread, where formatting a string is allowed.
    let failed_input_name = input.device.name.clone();
    let failed_output_name = output.device.name.clone();
    let (input_failure_tx, input_failure_rx) = rtrb::RingBuffer::new(STREAM_FAILURE_RING_SLOTS);
    let (output_failure_tx, output_failure_rx) = rtrb::RingBuffer::new(STREAM_FAILURE_RING_SLOTS);
    let running = stream::open(
        stream_setup,
        engine,
        Arc::clone(&xruns),
        stream_failure_sink(input_failure_tx),
        stream_failure_sink(output_failure_tx),
    );
    host.watch_stream_failures(crate::host::StreamFailureWatch::new(
        input_failure_rx,
        output_failure_rx,
        failed_input_name,
        failed_output_name,
    ));

    // Handed to `AppHost` rather than kept in a local (issue #24): FR-IO-070 requires the stream
    // to be stopped cleanly when a device is lost, and this function is about to block inside
    // `namir_ui::open_blocking` for the whole life of the window. The UI thread is the only one
    // that both learns of the loss (it drains the failure rings) and may act on it -- see
    // `AppHost::hold_streams`. The host drops the path when the window closes, which is where
    // this local used to drop it.
    match running {
        Ok(running) => {
            // Issue #76: D-13.2's elevation outcome is produced inside the first output callback
            // and cannot be reported from there (see `stream::ThreadPriorityReport`), so the
            // report is handed to the host, which polls it and writes the record from the UI
            // thread. Before `play()`, because that is what makes the first callback run.
            host.watch_thread_priority(running.thread_priority());
            match running.play() {
                Ok(()) => {
                    // NFR-PERF-030's marking event, emitted before the log line below so the
                    // measured interval ends where the requirement says it does:
                    // `RunningStreams::play` returning `Ok(())` is, in its own doc comment's
                    // words, "the one call that actually makes audio flow". A no-op outside a
                    // measurement run.
                    startup_probe::audible(library_index_entries, default_state_params);
                    eprintln!("namir: audio stream started");
                    host.hold_streams(running);
                    // FR-IO-080: persist the negotiated device/rate/channel configuration
                    // immediately so the next launch starts from what worked. The buffer size is
                    // not among them since issue #167 — see `AppHost::persist_negotiated_audio`.
                    host.persist_negotiated_audio(
                        &host_info.name,
                        &input.device.name,
                        &output.device.name,
                        sample_rate_hz,
                    );
                }
                Err(e) => {
                    // The detail is carried on the marker, not left to the notice alone: a probed
                    // launch opens no window, so `host.report` below has no reader.
                    startup_probe::not_audible(
                        startup_probe::REASON_STREAM_NOT_STARTED,
                        &e.to_string(),
                    );
                    host.report(crate::error_codes::DEVICE_OPEN_FAILED, e.to_string());
                    // Not held: a path that never started is dropped here, which stops the half
                    // of it that did open (FR-IO-070's "stop the stream cleanly" for the
                    // failed-to-start case, and `RunningStreams`' own drop contract).
                }
            }
        }
        Err(e) => {
            startup_probe::not_audible(startup_probe::REASON_STREAM_NOT_STARTED, &e.to_string());
            host.report(crate::error_codes::DEVICE_OPEN_FAILED, e.to_string());
        }
    }

    // NFR-PERF-030: a measurement run has nothing left to do — its marker is out — and returning
    // here is what makes the process exit instead of blocking in `open_blocking` below. Before the
    // `settings::save` at the foot of this function too, so a measurement never writes to the
    // directory it was pointed at.
    if startup_probe::enabled() {
        return;
    }

    // FR-IO-050/060: no device-settings surface exists in the shared `namir-ui` window (that
    // crate's scope is FR-UI-020's amp/cab screen; FR-IO is standalone-only and has no UI owner
    // yet -- recorded in this crate's own final report). Until one exists, this is reported
    // through a low-rate log line rather than not at all: still off the audio thread (D-16.2), a
    // plain background poll rather than anything the callback itself does.
    if let Some(latency) = crate::latency::estimate_round_trip(
        max_block_size as u32,
        output_params.buffer_frames,
        max_block_size as u32,
        sample_rate_hz,
    ) {
        // "at least", not "~", when the device chose its own output buffer (issue #166): the
        // figure is then a lower bound covering only what Namir itself buffers. Saying which it
        // is costs one word and is the difference between a figure and a guess.
        let qualifier = if latency.includes_output_buffer {
            "~"
        } else {
            "at least ~"
        };
        eprintln!(
            "namir: {} Hz, {max_block_size}-frame block, {qualifier}{:.1} ms estimated round-trip \
             latency (in: \"{}\", out: \"{}\")",
            sample_rate_hz, latency.milliseconds, input.device.name, output.device.name
        );
    }
    let xrun_log = spawn_xrun_logger(Arc::clone(&xruns));

    namir_ui::open_blocking("Namir", host);

    xrun_log.stop();

    // FR-IO-080: device/rate/channels are persisted at the point of negotiation (see
    // `host.persist_negotiated_audio` called right after `play()` above, and
    // `apply_audio_reopen`); a buffer size is written only when something requested one, by
    // `AppHost::persist_settings`. Only library_roots needs updating here: it tracks mid-session
    // changes (add/remove via panel) that `persist_negotiated_audio` does not touch.
    if let Some(dir) = &config_dir {
        let settings_path = settings::settings_path(dir);
        let (mut final_settings, _) = settings::load(&settings_path);
        final_settings.library_roots = (*library.roots()).clone();
        // The one report in this function that cannot become a notice: the window is already
        // closed, so there is no FR-UI-070 list left to push onto.
        if let Err(w) = settings::save(&settings_path, &final_settings) {
            crate::diagnostics::record(w.code, &w.detail);
        }
    }
}

/// Opens the shared window with no live engine behind it — used only when device negotiation
/// fails outright (FR-IO-070's "shall not crash or hang": a window the user can at least see and
/// close is strictly better than a silent process exit with no explanation).
fn open_window_without_audio(config_dir: Option<PathBuf>) {
    // NFR-PERF-030: every one of this function's four call sites is a launch that will never become
    // audible, which is a different outcome from a slow one and must not be measured as a timeout.
    // Checked here rather than at the four call sites so a fifth can never be added without it, and
    // returning before anything is built because a measurement run has no window to open. Each call
    // site has already printed on stderr which of the four conditions it was.
    if startup_probe::enabled() {
        startup_probe::not_audible(startup_probe::REASON_NO_AUDIO_DEVICE, "");
        return;
    }

    let c = PrepareContext::new(
        SampleRate::new(48_000).unwrap(),
        512,
        ChannelConfig::MonoToStereo,
    )
    .expect("a fixed, always-valid fallback context");
    let (_engine, endpoint) = build_default_engine(&c).expect("the default chain always prepares");
    let cache = ResourceCache::shared();
    let telemetry = endpoint.telemetry.clone();
    let instance = SharedInstance::new(Instance::new(EngineConfig { ctx: c }, endpoint));

    let preset_dir = config_dir.as_deref().map(crate::presets::preset_dir_under);
    let library_dir = config_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("namir-session-only"));
    let (library, _warnings) = namir_worker::library::LibraryService::open_at(&library_dir);
    let library_roots = (*library.roots()).clone();
    let library = Arc::new(library);

    let state = Arc::new(Mutex::new(State::defaults()));
    let worker_ctx = WorkerContext {
        instance: instance.clone(),
        cache: Arc::clone(&cache),
        library: Arc::clone(&library),
        pool: ThreadPool::new(),
        library_roots,
        state: Arc::clone(&state),
    };
    let worker = WorkerHandle::spawn(worker_ctx);
    // No device was opened at all on this path, so there is no share mode to indicate -- `None`
    // rather than a truthful-looking "Shared", which would claim a device this window does not
    // have.
    let mut host = AppHost::new(instance, worker, telemetry, library, state, None);
    let (settings, _) = match &config_dir {
        Some(dir) => settings::load(&settings::settings_path(dir)),
        None => (AppSettings::default(), None),
    };
    host.configure_audio_devices(
        config_dir.clone(),
        settings,
        Vec::new(),
        Vec::new(),
        None,
        None,
        Vec::new(),
        48_000,
        Vec::new(),
        256,
    );
    // recall presets, and refusing to would be a second degradation the missing device does not
    // imply.
    if let Some(dir) = preset_dir {
        host.watch_presets(dir);
    }
    if let Some(dir) = config_dir {
        host.watch_config_dir(dir);
    }
    // `NO_AUDIO_DEVICE`, not `NO_SUPPORTED_CONFIG` (issue #40): FR-IO-040's entry says none of the
    // rates *a device* reports could be negotiated, and on this path there is no device to be the
    // subject of that sentence. The same judgement two lines up passes `None` for the share-mode
    // indicator rather than a truthful-looking "Shared".
    host.report(
        crate::error_codes::NO_AUDIO_DEVICE,
        "no audio device was found or could be opened",
    );
    namir_ui::open_blocking("Namir", host);
}

/// A background thread logging the xrun count's *changes* at a low, bounded rate — never from the
/// audio callback itself (D-16.2). Stopped explicitly by [`XrunLog::stop`] rather than relying on
/// `Drop` alone, so `run`'s own shutdown ordering (log stopped before the function returns) is
/// explicit.
struct XrunLog {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl XrunLog {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn spawn_xrun_logger(counter: Arc<XrunCounter>) -> XrunLog {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        let mut last = counter.count();
        while !stop_clone.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
            let now = counter.count();
            if now != last {
                eprintln!("namir: xrun count is now {now} (session total, FR-IO-060)");
                last = now;
            }
        }
    });
    XrunLog {
        stop,
        thread: Some(thread),
    }
}

/// The one piece of real logic this module owns rather than composes: FR-IO-020's share-mode
/// negotiation. Everything else here is glue over already-tested pieces (see the module doc
/// comment), so these tests deliberately cover [`negotiate_share_mode`] alone — `run` itself still
/// needs a real window and real devices and is still verified by hand
/// (`docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`).
///
/// Every test here runs with no audio device of any kind, through [`crate::stream::FakeBackend`].
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{Direction, FakeBackend};

    const IN: &str = "fake in";
    const OUT: &str = "fake out";

    fn host() -> HostInfo {
        HostInfo {
            name: "fake".to_string(),
        }
    }

    fn device(name: &str) -> DeviceInfo {
        DeviceInfo {
            name: name.to_string(),
            is_default: true,
        }
    }

    fn params(channels: u16) -> StreamParams {
        StreamParams {
            sample_rate_hz: 48_000,
            buffer_frames: Some(128),
            channels,
            share_mode: ShareMode::Shared,
        }
    }

    fn negotiate(backend: &FakeBackend, requested: bool) -> ShareModeDecision {
        negotiate_share_mode(
            backend,
            &host(),
            &device(IN),
            params(1),
            &device(OUT),
            params(2),
            requested,
        )
    }

    /// A backend whose exclusive-mode ranges differ from its shared ones — the WASAPI shape
    /// (issue #190). Shared reports `BufferSizeRange::Unknown`, so no buffer size can be picked;
    /// exclusive reports a real range, out of which `PREFERRED_BUFFER_FRAMES` (256) is chosen.
    ///
    /// The two directions get **different** exclusive ranges — the input keeps its one channel,
    /// the output its two, as the shared answers already do — so a test can tell a direction
    /// mix-up in the enumeration from correct wiring. A single shared answer could not.
    fn backend_with_two_faces() -> FakeBackend {
        let exclusive = |channels: u16| {
            vec![crate::audio_io::SupportedConfigRange {
                channels,
                min_sample_rate_hz: 48_000,
                max_sample_rate_hz: 48_000,
                buffer_size: crate::audio_io::BufferSizeRange::Range {
                    min: 144,
                    max: 240_000,
                },
            }]
        };
        FakeBackend::new()
            .with_devices(vec![device(IN)], vec![device(OUT)])
            .reporting_exclusive_configs(Some(exclusive(1)), Some(exclusive(2)))
    }

    fn negotiated(backend: &FakeBackend, exclusive_mode: bool) -> AudioNegotiation {
        negotiate_audio(
            backend,
            &host(),
            backend.input_devices(&host()),
            backend.output_devices(&host()),
            &AudioPreferences {
                input_device: Some(IN),
                output_device: Some(OUT),
                sample_rate_hz: None,
                buffer_size_frames: None,
                exclusive_mode,
            },
        )
        .expect("both directions have a device")
    }

    /// **Issue #190.** An exclusive session negotiates against the device's *exclusive* ranges.
    /// Before this, enumeration was hardcoded to shared, and the shared answer here — a device
    /// that reports no usable buffer range at all — is what reached FR-IO-040's list.
    #[test]
    fn an_exclusive_session_negotiates_against_the_exclusive_ranges() {
        let backend = backend_with_two_faces()
            .granting_exclusive_to(IN)
            .granting_exclusive_to(OUT);
        let negotiated = negotiated(&backend, true);

        assert_eq!(negotiated.share_mode.mode, ShareMode::Exclusive);
        assert_eq!(
            negotiated.buffer_frames,
            Some(256),
            "the buffer comes from the exclusive range, not the shared one"
        );
    }

    /// The degrade path: exclusive was asked for, the devices refused, so the session runs shared
    /// — and every negotiated value has to come from the *shared* ranges. Negotiating against
    /// exclusive ranges and then opening shared is the same class of bug in the other direction.
    #[test]
    fn a_refused_exclusive_request_renegotiates_against_the_shared_ranges() {
        let backend = backend_with_two_faces();
        let negotiated = negotiated(&backend, true);

        assert_eq!(negotiated.share_mode.mode, ShareMode::Shared);
        assert!(
            negotiated.share_mode.refusal_detail.is_some(),
            "a refused request still explains itself"
        );
        assert_eq!(
            negotiated.buffer_frames, None,
            "the exclusive range must not survive into a shared session"
        );
    }

    /// A session that never asked for exclusive mode never sees the exclusive ranges, even on a
    /// device that would report them.
    #[test]
    fn a_shared_session_negotiates_against_the_shared_ranges() {
        let backend = backend_with_two_faces()
            .granting_exclusive_to(IN)
            .granting_exclusive_to(OUT);
        let negotiated = negotiated(&backend, false);

        assert_eq!(negotiated.share_mode.mode, ShareMode::Shared);
        assert_eq!(negotiated.buffer_frames, None);
    }

    /// The exclusive query is made **per direction**, and each direction gets its own answer. A
    /// swap — input enumerated as output, or both enumerated as one — is the same class of bug
    /// issue #190 fixed one level up, so it is asserted rather than assumed.
    #[test]
    fn each_direction_is_enumerated_in_its_own_right() {
        let backend = backend_with_two_faces()
            .granting_exclusive_to(IN)
            .granting_exclusive_to(OUT);
        let negotiated = negotiated(&backend, true);

        assert_eq!(
            negotiated.input_channels, 1,
            "the input's channel count comes from the input's own exclusive ranges"
        );
        assert_eq!(
            negotiated.output_channels, 2,
            "and the output's from the output's"
        );
        assert_eq!(
            backend.enumerations(),
            vec![
                (Direction::Input, ShareMode::Exclusive),
                (Direction::Output, ShareMode::Exclusive),
            ],
            "one exclusive query per direction, and no second pass when the mode is granted"
        );
    }

    /// **The second pass runs only when there is something to re-enumerate.** Here the device
    /// answered the exclusive query for real and *then* refused the mode, so the first pass'
    /// ranges do not apply to the shared session that will run.
    #[test]
    fn a_refusal_after_a_real_exclusive_answer_enumerates_a_second_time() {
        let backend = backend_with_two_faces();
        let _ = negotiated(&backend, true);

        assert_eq!(
            backend.enumerations(),
            vec![
                (Direction::Input, ShareMode::Exclusive),
                (Direction::Output, ShareMode::Exclusive),
                (Direction::Input, ShareMode::Shared),
                (Direction::Output, ShareMode::Shared),
            ]
        );
    }

    /// The other way to reach a refused exclusive request: the device could not answer the
    /// exclusive query at all, so the first pass already returned the shared ranges. Re-running it
    /// would repeat a query whose answer is in hand — on Windows, COM device enumeration on the
    /// start-up path NFR-PERF-030 measures — so it is skipped. This is every non-WASAPI host with
    /// `exclusive_mode: true` in its settings file.
    #[test]
    fn a_device_that_cannot_answer_the_exclusive_query_is_not_enumerated_twice() {
        let backend = FakeBackend::new().with_devices(vec![device(IN)], vec![device(OUT)]);
        let negotiated = negotiated(&backend, true);

        assert_eq!(negotiated.share_mode.mode, ShareMode::Shared);
        assert_eq!(
            backend.enumerations(),
            vec![
                (Direction::Input, ShareMode::Exclusive),
                (Direction::Output, ShareMode::Exclusive),
            ],
            "the exclusive request is made once; its shared-range answer is kept"
        );
    }

    /// **One endpoint answers the exclusive query and the other does not** — a capture device with
    /// a reachable WASAPI exclusive endpoint beside a render device without one. The first pass
    /// still negotiated one direction's ranges from the exclusive answer, and `settle` reads both
    /// sides, so the refusal has to re-enumerate: this is what the gate's `||` is for. Verified
    /// discriminating — with `&&` the second pass never runs and this test fails.
    #[test]
    fn one_direction_answering_exclusive_is_enough_to_force_the_second_pass() {
        let exclusive = vec![crate::audio_io::SupportedConfigRange {
            channels: 1,
            min_sample_rate_hz: 48_000,
            max_sample_rate_hz: 48_000,
            buffer_size: crate::audio_io::BufferSizeRange::Range {
                min: 144,
                max: 240_000,
            },
        }];
        let backend = FakeBackend::new()
            .with_devices(vec![device(IN)], vec![device(OUT)])
            .reporting_exclusive_configs(Some(exclusive), None);
        let negotiated = negotiated(&backend, true);

        assert_eq!(
            backend.enumerations(),
            vec![
                (Direction::Input, ShareMode::Exclusive),
                (Direction::Output, ShareMode::Exclusive),
                (Direction::Input, ShareMode::Shared),
                (Direction::Output, ShareMode::Shared),
            ],
            "one exclusive answer is enough to make the first pass unusable"
        );
        assert_eq!(
            negotiated.buffer_frames, None,
            "and the shared ranges, which offer no buffer size, are what the session runs on"
        );
    }

    /// **Issue #88: the `cpal` error callback allocates nothing.** This is the closure a real
    /// stream invokes on its own thread when a device is lost or a driver faults, and it used to
    /// `format!` a notice detail and `mpsc::Sender::send` it — two heap allocations on an audio
    /// thread, which NFR-RT-010 and FR-ERR-030 both forbid.
    ///
    /// Driven under D-7.5's `assert_no_alloc` harness with a real backend message, which is the
    /// only shape left: `cpal` 0.19 stopped producing xruns through this path (issue #200 item 6).
    /// The failure is built *outside* the section, because building it is `crate::audio_io`'s job
    /// and has its own test above.
    #[test]
    fn the_stream_failure_sink_allocates_nothing_on_the_callback_thread() {
        let (producer, mut consumer) = rtrb::RingBuffer::new(STREAM_FAILURE_RING_SLOTS);
        let mut sink = stream_failure_sink(producer);

        let lost = StreamFailure::Other(crate::audio_io::InlineDetail::from(
            "OS Error -2004287450 (FormatMessageW() returned error 317)",
        ));
        crate::rt_harness::audio_section(|| {
            sink(lost);
        });

        assert_eq!(
            consumer.pop().ok(),
            Some(lost),
            "a failure reaches the ring intact"
        );
    }

    /// A ring that has filled up must drop the report, not block, grow, or free anything: the
    /// value `rtrb` hands back on a full push is dropped right there on the callback thread, which
    /// is only legal because `StreamFailure` owns no heap. Deliberately pushed well past capacity
    /// inside the harness.
    #[test]
    fn a_full_stream_failure_ring_drops_reports_rather_than_allocating() {
        let (producer, mut consumer) = rtrb::RingBuffer::new(STREAM_FAILURE_RING_SLOTS);
        let mut sink = stream_failure_sink(producer);

        let failure = StreamFailure::DeviceLost;
        crate::rt_harness::audio_section(|| {
            for _ in 0..(STREAM_FAILURE_RING_SLOTS * 4) {
                sink(failure);
            }
        });

        let mut drained = 0;
        while consumer.pop().is_ok() {
            drained += 1;
        }
        assert_eq!(
            drained, STREAM_FAILURE_RING_SLOTS,
            "the ring holds its capacity and drops the rest"
        );
    }

    /// The untouched-settings case: `AppSettings::default().exclusive_mode` is `false`, so a first
    /// run — or any run by a user who never asked for exclusive mode — settles on shared with
    /// nothing to report, even on a backend that would have granted exclusive mode.
    #[test]
    fn a_session_that_never_asked_for_exclusive_mode_settles_on_shared_with_no_notice() {
        let backend = FakeBackend::new()
            .granting_exclusive_to(IN)
            .granting_exclusive_to(OUT);
        let decision = negotiate(&backend, false);
        assert_eq!(decision.mode, ShareMode::Shared);
        assert!(decision.refusal_detail.is_none());
    }

    /// The interim real-world case, and the one every non-Windows platform is in permanently: the
    /// request is refused outright, so the session runs shared and says so.
    #[test]
    fn an_exclusive_request_the_backend_refuses_settles_the_session_on_shared() {
        let backend = FakeBackend::new();
        let decision = negotiate(&backend, true);
        assert_eq!(decision.mode, ShareMode::Shared);
        let detail = decision
            .refusal_detail
            .expect("a refused request must be explained, not settled silently");
        assert!(detail.contains(IN), "{detail}");
        assert!(detail.contains(OUT), "{detail}");
    }

    /// **The all-or-nothing rule.** One direction granting exclusive mode is not enough: the
    /// session settles on shared for *both*, because a single mode indicator cannot truthfully
    /// describe a half-exclusive duplex path (roadmap §18). Run in both directions so a future
    /// short-circuit that only checks one side fails here.
    #[test]
    fn exclusive_granted_on_only_one_device_settles_both_on_shared() {
        for granted in [IN, OUT] {
            let backend = FakeBackend::new().granting_exclusive_to(granted);
            let decision = negotiate(&backend, true);
            assert_eq!(
                decision.mode,
                ShareMode::Shared,
                "exclusive granted only on {granted} must not engage the session"
            );
            let detail = decision
                .refusal_detail
                .expect("a partial grant is a refusal");
            let refusing = if granted == IN { OUT } else { IN };
            assert!(detail.contains(refusing), "{detail}");
        }
    }

    /// The path D-13.4's fork exists to reach: both devices grant it, so the session runs
    /// exclusive and there is nothing to warn about.
    #[test]
    fn exclusive_granted_on_both_devices_settles_the_session_on_exclusive() {
        let backend = FakeBackend::new()
            .granting_exclusive_to(IN)
            .granting_exclusive_to(OUT);
        let decision = negotiate(&backend, true);
        assert_eq!(decision.mode, ShareMode::Exclusive);
        assert!(decision.refusal_detail.is_none());
    }

    /// The refusal detail is what `EXCLUSIVE_MODE_UNAVAILABLE`'s `{reason}` placeholder stands for,
    /// so it must actually carry a reason and say what happened instead — FR-UI-070 wants a notice
    /// to state what failed, which device it concerned, and where that leaves the user.
    #[test]
    fn the_refusal_detail_names_the_device_the_reason_and_the_fallback() {
        let backend = FakeBackend::new();
        let detail = negotiate(&backend, true).refusal_detail.unwrap();
        assert!(detail.contains("exclusive mode is unavailable"), "{detail}");
        assert!(detail.contains("shared mode"), "{detail}");
    }

    /// **Issue #192.** [`run`] and [`crate::host::AppHost::initiate_audio_reopen`] both open with
    /// whatever this function derives, so these are the values a stream actually opens with. The
    /// cross-path claim — that the reopen path really routes through here rather than building
    /// them by hand — is asserted in
    /// `crate::host::tests::a_reopen_assembles_its_stream_config_through_the_shared_function`,
    /// which drives the real reopen; it cannot be tested here, where there is one pure function.
    ///
    /// The `output_buffer_request` assertion is the D-13.3 line the issue names: the output
    /// stream must *not* inherit the negotiated 256-frame request, while the engine's block size
    /// still comes from it.
    #[test]
    fn assemble_stream_config_produces_the_values_a_stream_opens_with() {
        let backend = backend_with_two_faces()
            .granting_exclusive_to(IN)
            .granting_exclusive_to(OUT);

        let assembled = assemble_stream_config(&negotiated(&backend, true));

        assert_eq!(
            assembled.input_params.buffer_frames,
            Some(256),
            "the input stream opens at the negotiated buffer size"
        );
        assert_eq!(
            assembled.output_params.buffer_frames,
            crate::audio_io::output_buffer_request(),
            "D-13.3: the output stream asks the device for its own buffer"
        );
        assert_eq!(
            assembled.max_block_size, 256,
            "the engine's block size still comes from the negotiated buffer"
        );
        assert_eq!(assembled.channel_config, ChannelConfig::MonoToStereo);
        assert_eq!(assembled.sample_rate, SampleRate::new(48_000));
    }
}
