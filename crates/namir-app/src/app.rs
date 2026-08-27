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
    StreamParams,
};
use crate::host::AppHost;
use crate::instance::SharedInstance;
use crate::settings::{self, AppSettings};
use crate::startup_probe;
use crate::stream::{self, StreamSetup};
use crate::worker::{AppEvent, WorkerContext, WorkerHandle};
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

/// FR-IO-020's settled answer for one session: the share mode both streams open with, and — when
/// exclusive mode was asked for and not granted — the notice detail explaining why the session is
/// running shared instead.
struct ShareModeDecision {
    mode: ShareMode,
    /// `None` whenever the answer needs no explanation: exclusive was never requested, or it was
    /// requested and granted.
    refusal_detail: Option<String>,
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
fn negotiate_share_mode(
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
    // there for the day a settings field arrives: `logging::init` takes the level as a parameter
    // precisely so `namir-platform` need not know what `AppSettings` is.
    //
    // Before `resolve_config_dir` because the log's own location is `namir_platform::
    // log_file_path`, which is independent of the app's config directory and of
    // `startup_probe`'s override of it — a probed launch logs to the same place a real one does.
    namir_platform::logging::init(None);

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

    // FR-IO-020, and the first read of `AppSettings::exclusive_mode` since M6 added the field: the
    // share mode is settled here, once, before anything is opened -- both stream literals below and
    // the mode indicator handed to `AppHost` all take their value from this one decision.
    let mut input_params = StreamParams {
        sample_rate_hz,
        buffer_frames,
        channels: input_channels,
        share_mode: ShareMode::Shared,
    };
    let mut output_params = StreamParams {
        sample_rate_hz,
        buffer_frames,
        channels: output_channels,
        share_mode: ShareMode::Shared,
    };
    let share_mode = negotiate_share_mode(
        &backend,
        &host_info,
        &input.device,
        input_params,
        &output.device,
        output_params,
        settings.exclusive_mode,
    );
    input_params.share_mode = share_mode.mode;
    output_params.share_mode = share_mode.mode;

    let max_block_size = crate::audio_io::block_frames(buffer_frames);
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
    let (library, _) = namir_worker::library::LibraryService::open_at(&library_dir);
    // M14 (§22 R-18): `open_at` no longer reads the index file, and **the standalone asks for it
    // anyway, here, deliberately.** The deferral exists for the *plugin*, where a host instantiates
    // one instance per track and NFR-PERF-040's 200 ms is a per-instance budget the index parse was
    // eating whole. This process launches once, has a user waiting in front of one window, and
    // measures itself as "start-up to audible **with a warm library index**" (NFR-PERF-030) — a
    // launch that reported an empty library and filled it in later would not be that measurement,
    // and `startup_probe::audible` below would report an index of zero entries.
    library.ensure_loaded();
    let library_warnings = library.take_load_warnings();
    let library_roots = library.roots().to_vec();
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
        library_roots,
        state: Arc::clone(&state),
    };
    let worker = WorkerHandle::spawn(worker_ctx);
    let stream_event_sender = worker.event_sender();

    // FR-IO-020's mode indicator: the mode actually granted, never the one requested. The output
    // device names it -- see `namir_ui::AudioModeStatus::device_name` for why one name is enough
    // when the mode is settled across both directions.
    let audio_mode = Some(namir_ui::AudioModeStatus {
        share_mode: share_mode.mode.into(),
        device_name: output.device.name.clone(),
    });
    let mut host = AppHost::new(instance, worker, telemetry, library, state, audio_mode);
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

    let xruns = Arc::new(XrunCounter::new());
    let stream_setup = StreamSetup {
        backend: &backend,
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
                // NFR-PERF-030's marking event, emitted before the log line below so the measured
                // interval ends where the requirement says it does: `RunningStreams::play`
                // returning `Ok(())` is, in its own doc comment's words, "the one call that
                // actually makes audio flow". A no-op outside a measurement run.
                startup_probe::audible(library_index_entries, default_state_params);
                eprintln!("namir: audio stream started");
                Some(running)
            }
            Err(e) => {
                // The detail is carried on the marker, not left to the notice alone: a probed
                // launch opens no window, so `host.report` below has no reader.
                startup_probe::not_audible(
                    startup_probe::REASON_STREAM_NOT_STARTED,
                    &e.to_string(),
                );
                host.report(crate::error_codes::DEVICE_OPEN_FAILED, e.to_string());
                None
            }
        },
        Err(e) => {
            startup_probe::not_audible(startup_probe::REASON_STREAM_NOT_STARTED, &e.to_string());
            host.report(crate::error_codes::DEVICE_OPEN_FAILED, e.to_string());
            None
        }
    };

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
        // The one report in this function that cannot become a notice: the window is already
        // closed, so there is no FR-UI-070 list left to push onto. It was `let _ =` — a settings
        // file that silently failed to save is precisely the "why did it forget my device again?"
        // report a log exists to answer — and is now the record it always should have been.
        if let Err(w) = settings::save(&settings::settings_path(dir), &settings) {
            namir_platform::logging::record(w.code, &w.detail);
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
    // No device was opened at all on this path, so there is no share mode to indicate -- `None`
    // rather than a truthful-looking "Shared", which would claim a device this window does not have.
    let mut host = AppHost::new(instance, worker, telemetry, library, state, None);
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
    use crate::stream::FakeBackend;

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
}
