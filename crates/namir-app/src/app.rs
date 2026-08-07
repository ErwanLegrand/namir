//! Top-level wiring: [`run`] is `main`'s actual body, factored out so `main.rs` stays a one-line
//! entry point. This module is deliberately thin glue over already-tested pieces
//! ([`crate::device_state`]'s selection logic, [`crate::settings`]'s persistence,
//! [`crate::instance::SharedInstance`], [`crate::worker`]'s background thread,
//! [`crate::host::AppHost`]'s `UiHost` bridge, [`crate::stream`]'s duplex path) — real device I/O
//! and window creation cannot be meaningfully unit-tested (this crate's own final report explains
//! why, and `docs/manual-tests/` records what to check by hand instead), so this module's job is
//! to compose pieces that already have their own tests, not to introduce new untested logic of its
//! own.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use namir_core::{ChannelConfig, SampleRate};
use namir_engine::{PrepareContext, build_default_engine};
use namir_state::State;
use namir_worker::pool::ThreadPool;
use namir_worker::{EngineConfig, Instance, ResourceCache};

use crate::audio_io::{AudioBackend, CpalBackend, DeviceInfo, HostInfo, StreamParams};
use crate::host::AppHost;
use crate::instance::SharedInstance;
use crate::settings::{self, AppSettings};
use crate::stream::{self, StreamSetup};
use crate::worker::{AppEvent, WorkerContext, WorkerHandle};
use crate::xrun::XrunCounter;

/// Falls back to a working default if [`namir_platform::config_dir`] returns `None` (an
/// unrecognised environment — see that function's own doc comment). A session with no persistent
/// config directory still runs; it just doesn't remember anything across restarts, which is a
/// strictly worse but still-functional degradation (P8), not a reason to refuse to start.
fn resolve_config_dir() -> Option<PathBuf> {
    namir_platform::config_dir()
}

/// FR-IO-010/040: enumerates and negotiates one direction (input or output). Returns the selected
/// device and the sample-rate/buffer/channel choice space needed for the *other* direction's
/// negotiation to check against (`crate::device_state::negotiate_shared_sample_rate`) — kept
/// separate from applying the choice so the caller can negotiate the shared sample rate before
/// picking a final buffer size per direction.
struct DirectionSetup {
    device: DeviceInfo,
    fell_back_from: Option<String>,
    configs: Vec<crate::audio_io::SupportedConfigRange>,
}

fn setup_direction(
    backend: &dyn AudioBackend,
    host: &HostInfo,
    devices: Result<Vec<DeviceInfo>, crate::audio_io::AudioIoError>,
    remembered_device: Option<&str>,
    configs_of: impl Fn(
        &HostInfo,
        &DeviceInfo,
    ) -> Result<
        Vec<crate::audio_io::SupportedConfigRange>,
        crate::audio_io::AudioIoError,
    >,
) -> Option<DirectionSetup> {
    let devices = devices.ok()?;
    let selection = crate::device_state::select_device(&devices, remembered_device)?;
    let configs = configs_of(host, &selection.device).unwrap_or_default();
    let _ = backend; // reserved for a future multi-host UI; kept as a parameter for that seam.
    Some(DirectionSetup {
        device: selection.device,
        fell_back_from: selection.fell_back_from,
        configs,
    })
}

/// `main`'s real body. Blocks until the window is closed.
pub fn run() {
    let config_dir = resolve_config_dir();

    let (mut settings, settings_warning) = match &config_dir {
        Some(dir) => settings::load(&settings::settings_path(dir)),
        None => (AppSettings::default(), None),
    };

    let backend = CpalBackend::new();
    let host_info = match &settings.host_name {
        Some(name) => backend
            .hosts()
            .into_iter()
            .find(|h| &h.name == name)
            .unwrap_or_else(|| backend.default_host()),
        None => backend.default_host(),
    };

    let input = setup_direction(
        &backend,
        &host_info,
        backend.input_devices(&host_info),
        settings.input_device_name.as_deref(),
        |h, d| backend.input_configs(h, d),
    );
    let output = setup_direction(
        &backend,
        &host_info,
        backend.output_devices(&host_info),
        settings.output_device_name.as_deref(),
        |h, d| backend.output_configs(h, d),
    );

    let (input, output) = match (input, output) {
        (Some(i), Some(o)) => (i, o),
        _ => {
            eprintln!(
                "namir: no usable input/output audio device found on host \"{}\"; the window \
                 will still open, but no audio will process. See \
                 docs/manual-tests/fr-io-070-device-removal.md.",
                host_info.name
            );
            open_window_without_audio(config_dir);
            return;
        }
    };

    let sample_rate_hz = crate::device_state::negotiate_shared_sample_rate(
        &input.configs,
        &output.configs,
        settings.sample_rate_hz,
    )
    .unwrap_or(48_000);
    let buffer_frames = crate::device_state::negotiate_buffer_size(
        &input.configs,
        sample_rate_hz,
        settings.buffer_size_frames,
    );
    let input_channels =
        crate::device_state::negotiate_channels(&input.configs, sample_rate_hz, 1).unwrap_or(1);
    let output_channels =
        crate::device_state::negotiate_channels(&output.configs, sample_rate_hz, 2).unwrap_or(1);

    let max_block_size = buffer_frames.unwrap_or(512).max(1) as usize;
    let channel_config = if output_channels >= 2 {
        ChannelConfig::MonoToStereo
    } else {
        ChannelConfig::Mono
    };

    let Some(sample_rate) = SampleRate::new(sample_rate_hz) else {
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
    let (library, library_warnings) = namir_worker::library::LibraryService::open_at(&library_dir);
    let library_roots = library.roots().to_vec();
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
    let stream_event_sender = worker.event_sender();

    let mut host = AppHost::new(instance, worker, telemetry, library, state);
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

    let xruns = Arc::new(XrunCounter::new());
    let stream_setup = StreamSetup {
        backend: &backend,
        input_host: host_info.clone(),
        input_device: input.device.clone(),
        input_params: StreamParams {
            sample_rate_hz,
            buffer_frames,
            channels: input_channels,
        },
        output_host: host_info.clone(),
        output_device: output.device.clone(),
        output_params: StreamParams {
            sample_rate_hz,
            buffer_frames,
            channels: output_channels,
        },
        channel_config,
        input_channel_index: settings.channel_mapping.input_channel.unwrap_or(0),
        output_channel_left: settings.channel_mapping.output_channel_left.unwrap_or(0),
        output_channel_right: settings.channel_mapping.output_channel_right.unwrap_or(1),
        max_block_size,
    };

    let xruns_for_failure = Arc::clone(&xruns);
    let running = stream::open(
        stream_setup,
        engine,
        Arc::clone(&xruns),
        move |direction, failure| match failure {
            crate::audio_io::StreamFailure::Xrun => xruns_for_failure.record(),
            other => {
                let _ = stream_event_sender.send(AppEvent::StreamFailure {
                    direction,
                    detail: format!("{other:?}"),
                });
            }
        },
    );

    let _running = match running {
        Ok(running) => match running.play() {
            Ok(()) => {
                eprintln!("namir: audio stream started");
                Some(running)
            }
            Err(e) => {
                host.report(crate::error_codes::DEVICE_OPEN_FAILED, e.to_string());
                None
            }
        },
        Err(e) => {
            host.report(crate::error_codes::DEVICE_OPEN_FAILED, e.to_string());
            None
        }
    };

    // FR-IO-050/060: no device-settings surface exists in the shared `namir-ui` window (that
    // crate's scope is FR-UI-020's amp/cab screen; FR-IO is standalone-only and has no UI owner
    // yet -- recorded in this crate's own final report). Until one exists, this is reported
    // through a low-rate log line rather than not at all: still off the audio thread (D-16.2), a
    // plain background poll rather than anything the callback itself does.
    if let Some(latency) = crate::latency::estimate_round_trip(
        max_block_size as u32,
        max_block_size as u32,
        sample_rate_hz,
    ) {
        eprintln!(
            "namir: {} Hz, {max_block_size}-frame buffer, ~{:.1} ms estimated round-trip latency \
             (in: \"{}\", out: \"{}\")",
            sample_rate_hz, latency.milliseconds, input.device.name, output.device.name
        );
    }
    let xrun_log = spawn_xrun_logger(Arc::clone(&xruns));

    namir_ui::open_blocking("Namir", host);

    xrun_log.stop();

    // FR-IO-080: persist whatever was actually negotiated -- including a fallback -- so the next
    // launch starts from what worked this time.
    if let Some(dir) = &config_dir {
        settings.host_name = Some(host_info.name.clone());
        settings.input_device_name = Some(input.device.name.clone());
        settings.output_device_name = Some(output.device.name.clone());
        settings.sample_rate_hz = Some(sample_rate_hz);
        settings.buffer_size_frames = buffer_frames;
        let _ = settings::save(&settings::settings_path(dir), &settings);
    }
}

/// Opens the shared window with no live engine behind it — used only when device negotiation
/// fails outright (FR-IO-070's "shall not crash or hang": a window the user can at least see and
/// close is strictly better than a silent process exit with no explanation).
fn open_window_without_audio(config_dir: Option<PathBuf>) {
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

    let library_dir = config_dir.unwrap_or_else(|| std::env::temp_dir().join("namir-session-only"));
    let (library, _warnings) = namir_worker::library::LibraryService::open_at(&library_dir);
    let library_roots = library.roots().to_vec();
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
    let mut host = AppHost::new(instance, worker, telemetry, library, state);
    host.report(
        crate::error_codes::NO_SUPPORTED_CONFIG,
        "no audio device could be opened; parameters can still be edited but nothing will be \
         processed",
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
