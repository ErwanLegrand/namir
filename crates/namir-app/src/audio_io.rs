//! D-13.1: "Audio I/O for the standalone app uses `cpal` ... behind a Namir-owned trait so the
//! engine and UI never see cpal types." [`AudioBackend`]/[`AudioStream`] are that trait: every
//! other module in this crate ([`crate::device_state`], [`crate::host`], [`crate::worker`],
//! [`crate::stream`]) is written against these two traits and the plain data types below, never
//! against a `cpal::*` type directly. This module is the one place `cpal` is named.
//!
//! # FR-IO-020's exclusive mode, and the forked dependency that closes it
//!
//! FR-IO-020 (Must) requires WASAPI **shared and exclusive** mode. Upstream `cpal` 0.18.1 cannot
//! provide the second half. Reading its own WASAPI backend source
//! (`cpal-0.18.1/src/host/wasapi/device.rs`, both `build_input_stream_raw_inner` and
//! `build_output_stream_raw_inner`) showed the share mode hardcoded to `AUDCLNT_SHAREMODE_SHARED`
//! with **no public API to request `AUDCLNT_SHAREMODE_EXCLUSIVE`** — absent from the dependency
//! itself, not a gap in this crate's usage of it. That M6 verification still stands; what changed
//! at **M11** is which `cpal` this crate depends on.
//!
//! D-13.4's Namir-maintained `cpal` fork — pinned by commit hash in this crate's `Cargo.toml`, with
//! its own narrow `[sources]` allowance in the workspace `deny.toml` — adds
//! `cpal::platform::{ShareMode, WasapiStreamOptions, WasapiDeviceExt}`: a share-mode-aware mirror of
//! `DeviceTrait`'s configuration queries and stream builders. [`CpalBackend::supports_exclusive`]
//! now asks the device through that trait instead of answering from a constant, and both stream
//! builders carry [`StreamParams::share_mode`] into the open.
//!
//! Those names carry no conditional compilation of their own — deliberately, on the fork's side:
//! the types and the trait are compiled on every platform and `WasapiDeviceExt` is implemented for
//! the platform-dispatch `cpal::Device` everywhere, refusing exclusive mode *at runtime* wherever
//! there is no WASAPI endpoint behind the device (any non-Windows build, and any Windows host that
//! is not WASAPI). Its configuration queries refuse rather than quietly answering for shared mode,
//! which is exactly what a pre-flight probe needs. That is what lets this module stay free of
//! platform attributes — which D-5.1 requires of it in any case, `xtask layering` confining those
//! to `namir-platform` — and it is why every path below, the exclusive probe included, is reachable
//! from a headless Linux test run.
//!
//! # Why device/config data crosses this boundary as plain structs, not `cpal` references
//!
//! [`DeviceInfo`] is identified by **name** (`cpal::traits::DeviceTrait`'s `Display` impl), not by
//! a live `cpal::Device` handle. [`crate::settings::AppSettings`] must persist a device identity
//! across process restarts (FR-IO-080), and a `cpal::Device` cannot outlive the process that
//! enumerated it — a name is the only thing that can be written to disk and looked up again next
//! session. This does mean two identically-named devices are indistinguishable (accepted: `cpal`
//! itself offers no more stable a public identifier that survives a restart on every backend this
//! targets either, since `DeviceId`'s stability is backend-dependent).

use std::time::Duration;

/// One audio host API (WASAPI, ALSA, CoreAudio, ...), by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    /// The host's display name, e.g. `"WASAPI"`.
    pub name: String,
}

/// One audio device, by name — see this module's doc comment for why a name, not a live handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The device's display name.
    pub name: String,
    /// Whether this was the host's reported default at enumeration time.
    pub is_default: bool,
}

/// A device's supported buffer-size range, mirroring `cpal::SupportedBufferSize` in this crate's
/// own vocabulary (D-13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSizeRange {
    /// The device supports any frame count in `min..=max`.
    Range {
        /// Smallest buffer size, in frames, the device will accept.
        min: u32,
        /// Largest buffer size, in frames, the device will accept.
        max: u32,
    },
    /// The platform provides no way to learn the supported range before opening a stream.
    Unknown,
}

/// One `(channel count, sample-rate range, buffer-size range)` combination a device reports as
/// supported for 32-bit float samples — FR-IO-040's "sample rate and buffer size from those the
/// selected device reports as supported". Non-f32 formats are out of scope for this build; see
/// `docs/manual-tests/fr-io-040-sample-rate-buffer-size.md` for why that is a documented gap
/// rather than a silent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportedConfigRange {
    /// Number of interleaved channels this range covers.
    pub channels: u16,
    /// Lowest sample rate, in Hz, this range covers.
    pub min_sample_rate_hz: u32,
    /// Highest sample rate, in Hz, this range covers.
    pub max_sample_rate_hz: u32,
    /// The buffer sizes this range covers.
    pub buffer_size: BufferSizeRange,
}

impl SupportedConfigRange {
    /// Whether this range covers `hz` — its endpoints are inclusive, as `cpal`'s own
    /// `SupportedStreamConfigRange` defines them.
    ///
    /// Sited on the type rather than beside either caller because both
    /// [`crate::device_state`]'s FR-IO-040 negotiation and [`CpalBackend::supports_exclusive`]'s
    /// FR-IO-020 probe have to ask it, and two copies of an inclusive-bounds test is exactly the
    /// kind of pair that drifts apart at one endpoint.
    pub fn covers_rate(&self, hz: u32) -> bool {
        hz >= self.min_sample_rate_hz && hz <= self.max_sample_rate_hz
    }
}

/// What to open a stream with, already negotiated by [`crate::device_state`] against a device's
/// [`SupportedConfigRange`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    /// The sample rate to open at, in Hz.
    pub sample_rate_hz: u32,
    /// The buffer size to request, in frames. `None` requests the backend's own default.
    pub buffer_frames: Option<u32>,
    /// The interleaved channel count to open the stream with — the *device's* channel count
    /// (often more than the engine's own mono/stereo core), per this module's own doc comment on
    /// why the caller, not this trait, picks which of those channels the engine actually reads or
    /// writes.
    pub channels: u16,
    /// FR-IO-020's share mode, already settled for the whole session by
    /// [`crate::app`] — never a request this trait is expected to renegotiate. See
    /// [`AudioBackend::supports_exclusive`] for why the negotiation is a separate, earlier query
    /// rather than a failed open followed by a retry.
    pub share_mode: ShareMode,
}

/// FR-IO-020's two WASAPI share modes. An enum rather than a `bool` so a call site reads as
/// `ShareMode::Exclusive` rather than `true`, matching this module's habit ([`BufferSizeRange`],
/// [`crate::stream::Direction`], [`crate::settings::ChannelMapping`]) of naming a choice instead of
/// encoding it.
///
/// Meaningful on WASAPI only. Every other host API this build enumerates (ALSA, CoreAudio, ...) has
/// no equivalent concept, which is not a special case this crate has to spell out: a backend that
/// cannot provide exclusive mode answers [`ExclusiveModeOutcome::Unsupported`] to
/// [`AudioBackend::supports_exclusive`] and the session settles on [`Shared`](Self::Shared), the
/// same path a Windows device that refuses exclusive mode takes. That is also why nothing here is
/// conditionally compiled per platform — D-5.1 confines platform `cfg` attributes to
/// `namir-platform` and `xtask layering` enforces it, so this seam is runtime-dispatched by
/// construction rather than by preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShareMode {
    /// The device is shared with every other application on the system — the working default
    /// FR-IO-080's spirit asks for, and what a session that never asked for anything else gets.
    #[default]
    Shared,
    /// This process holds the device exclusively for the lifetime of the stream.
    Exclusive,
}

/// FR-IO-020's exclusive-mode request outcome. Since M11 both variants are reachable from the real
/// backend: [`CpalBackend::supports_exclusive`] asks the device through D-13.4's fork rather than
/// answering from a constant, so [`Unsupported`](Self::Unsupported) now means "not this device, at
/// this rate and channel count, in this build" rather than "this dependency cannot ask".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusiveModeOutcome {
    /// The stream was opened in exclusive mode.
    Engaged,
    /// Exclusive mode was requested but is not available through this build's audio backend; the
    /// stream is opened in shared mode instead (a working default, per FR-IO-080's "degrade
    /// gracefully" spirit applied to a capability rather than a device).
    Unsupported,
}

/// A Namir-owned classification of a stream failure, replacing `cpal::ErrorKind` at this crate's
/// boundary (D-13.1). `Xrun` is `cpal`'s own detected dropout (not every backend reports it — see
/// [`crate::xrun`] for the ring-underrun-based detector this crate also runs, which does not
/// depend on backend support). `DeviceLost` is FR-IO-070's device-removal case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamFailure {
    /// The device was disconnected or otherwise stopped being reachable (`cpal`'s
    /// `ErrorKind::DeviceNotAvailable`/`HostUnavailable`).
    DeviceLost,
    /// `cpal` itself detected a buffer underrun/overrun (`ErrorKind::Xrun`).
    Xrun,
    /// Anything else, carrying `cpal`'s own message for diagnostics (FR-ERR-050).
    Other(String),
}

/// Why an [`AudioBackend`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioIoError {
    /// The named host is not available on this system.
    HostUnavailable(String),
    /// The named device could not be found under the named host.
    DeviceNotFound(String),
    /// FR-IO-040: the device reported no usable configuration at all, or none matching what was
    /// requested.
    NoSupportedConfig,
    /// The stream failed to open — FR-IO-070's "a device failing to open ... shall be handled
    /// without crashing". Carries the backend's own message.
    OpenFailed(String),
    /// FR-IO-020: [`ShareMode::Exclusive`] was asked for and refused. Deliberately **not** an
    /// [`OpenFailed`](Self::OpenFailed): an exclusive refusal is a *degradation* the session
    /// recovers from by opening shared (`docs/02-architecture.md` D-13.4's "the settings path must
    /// degrade to shared rather than leave the app with no audio"), whereas `OpenFailed` means
    /// there is no audio at all. Collapsing the two would make
    /// [`crate::error_codes::EXCLUSIVE_MODE_UNAVAILABLE`]'s `Warning` severity indistinguishable
    /// from [`crate::error_codes::DEVICE_OPEN_FAILED`]'s `Error`. Carries the reason, which is all
    /// the caller can say — [`ExclusiveModeOutcome::Unsupported`] carries no diagnostic of its own.
    ExclusiveModeUnavailable(String),
}

impl std::fmt::Display for AudioIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostUnavailable(h) => write!(f, "audio host \"{h}\" is not available"),
            Self::DeviceNotFound(d) => write!(f, "audio device \"{d}\" was not found"),
            Self::NoSupportedConfig => write!(f, "no usable audio configuration was reported"),
            Self::OpenFailed(msg) => write!(f, "failed to open audio stream: {msg}"),
            Self::ExclusiveModeUnavailable(reason) => {
                write!(f, "exclusive mode is unavailable: {reason}")
            }
        }
    }
}

impl std::error::Error for AudioIoError {}

/// A running (or paused) audio stream, boxed so [`AudioBackend`]'s methods can return one without
/// naming a concrete `cpal` type. Dropping this stops the stream — matching `cpal::Stream`'s own
/// `Drop`-stops contract, which every real implementation of this trait relies on rather than
/// re-implements.
pub trait AudioStream: Send {
    /// Starts (or resumes) the stream. Streams are built paused, matching `cpal`'s own contract
    /// ("callbacks do not fire until `play` is called").
    fn play(&self) -> Result<(), AudioIoError>;
    /// Pauses the stream without closing it.
    fn pause(&self) -> Result<(), AudioIoError>;
}

/// D-13.1's Namir-owned trait over `cpal`. Every method is deliberately synchronous and may
/// block briefly (device enumeration and stream construction are not RT-safe operations and are
/// never called from the audio thread) — see [`crate::worker`] for where these calls actually run.
pub trait AudioBackend: Send {
    /// Every host API this build was compiled with support for, in no particular order.
    fn hosts(&self) -> Vec<HostInfo>;
    /// The host `cpal` itself would pick with no configuration at all — FR-IO-080's "working
    /// default" starting point.
    fn default_host(&self) -> HostInfo;
    /// Every input-capable device under `host`.
    fn input_devices(&self, host: &HostInfo) -> Result<Vec<DeviceInfo>, AudioIoError>;
    /// Every output-capable device under `host`.
    fn output_devices(&self, host: &HostInfo) -> Result<Vec<DeviceInfo>, AudioIoError>;
    /// `device`'s supported input configurations restricted to 32-bit float samples (see this
    /// module's doc comment on `SupportedConfigRange` for why).
    fn input_configs(
        &self,
        host: &HostInfo,
        device: &DeviceInfo,
    ) -> Result<Vec<SupportedConfigRange>, AudioIoError>;
    /// `device`'s supported output configurations, f32 only.
    fn output_configs(
        &self,
        host: &HostInfo,
        device: &DeviceInfo,
    ) -> Result<Vec<SupportedConfigRange>, AudioIoError>;

    /// FR-IO-020: would `device` open in [`ShareMode::Exclusive`] at `params`? Answered **before**
    /// any stream is built, and this ordering is load-bearing rather than stylistic.
    ///
    /// The obvious alternative — open exclusive, and fall back to shared if it fails — cannot be
    /// written against this crate's own wiring. [`crate::stream::open`] takes its
    /// [`namir_engine::AudioEngine`] **by value** and moves it into the output callback before the
    /// backend is called at all, so a failed open consumes the engine; and the engine cannot simply
    /// be rebuilt for the retry, because `namir_engine::build_default_engine` yields an engine and
    /// a `WorkerEndpoint` together and that endpoint has by then already been consumed by
    /// `namir_worker::Instance::new` (see [`crate::app::run`]). Asking first costs one query per
    /// direction and keeps the whole retry problem from arising.
    ///
    /// `params.share_mode` is ignored by implementations of this method — the question *is* whether
    /// exclusive mode is possible, so the caller ([`crate::app`]) passes the rest of the
    /// configuration (rate, buffer, channels) that an exclusive open would have to satisfy natively.
    fn supports_exclusive(
        &self,
        host: &HostInfo,
        device: &DeviceInfo,
        params: StreamParams,
    ) -> ExclusiveModeOutcome;

    /// Opens an input stream. `on_data` receives interleaved f32 samples, `channels` per frame per
    /// `params`; `on_error` receives every post-open failure until the stream is dropped.
    /// Returned paused — the caller must call [`AudioStream::play`].
    #[allow(clippy::type_complexity)]
    fn build_input_stream(
        &self,
        host: &HostInfo,
        device: &DeviceInfo,
        params: StreamParams,
        on_data: Box<dyn FnMut(&[f32]) + Send>,
        on_error: Box<dyn FnMut(StreamFailure) + Send>,
        activation_timeout: Duration,
    ) -> Result<Box<dyn AudioStream>, AudioIoError>;

    /// Opens an output stream. `on_data` fills the interleaved f32 buffer it is handed (`channels`
    /// per frame per `params`) every callback. Returned paused.
    #[allow(clippy::type_complexity)]
    fn build_output_stream(
        &self,
        host: &HostInfo,
        device: &DeviceInfo,
        params: StreamParams,
        on_data: Box<dyn FnMut(&mut [f32]) + Send>,
        on_error: Box<dyn FnMut(StreamFailure) + Send>,
        activation_timeout: Duration,
    ) -> Result<Box<dyn AudioStream>, AudioIoError>;
}

/// The real backend, over D-13.4's `cpal` fork (0.18.1 plus WASAPI share-mode support), pinned by
/// commit hash in this crate's `Cargo.toml`.
pub struct CpalBackend;

impl CpalBackend {
    /// Builds the real backend. Cheap — `cpal` enumerates hosts/devices lazily, per call, not at
    /// construction.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self::new()
    }
}

mod cpal_impl {
    use std::time::Duration;

    use cpal::platform::{ShareMode as CpalShareMode, WasapiDeviceExt, WasapiStreamOptions};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    use super::{
        AudioBackend, AudioIoError, AudioStream, BufferSizeRange, CpalBackend, DeviceInfo,
        ExclusiveModeOutcome, HostInfo, ShareMode, StreamFailure, StreamParams,
        SupportedConfigRange,
    };

    /// Resolves `host`'s name to a live `cpal::Host`. `cpal::available_hosts`/`host_from_id`
    /// re-enumerate every call rather than caching anything, matching this trait's own "not
    /// RT-safe, may do real work" contract.
    fn resolve_host(host: &HostInfo) -> Result<cpal::Host, AudioIoError> {
        cpal::available_hosts()
            .into_iter()
            .find(|id| id.name() == host.name)
            .and_then(|id| cpal::host_from_id(id).ok())
            .ok_or_else(|| AudioIoError::HostUnavailable(host.name.clone()))
    }

    fn device_name(device: &cpal::Device) -> String {
        device.to_string()
    }

    fn resolve_device(
        devices: impl Iterator<Item = cpal::Device>,
        name: &str,
    ) -> Result<cpal::Device, AudioIoError> {
        devices
            .into_iter()
            .find(|d| device_name(d) == name)
            .ok_or_else(|| AudioIoError::DeviceNotFound(name.to_string()))
    }

    fn buffer_size_range(range: &cpal::SupportedBufferSize) -> BufferSizeRange {
        match range {
            cpal::SupportedBufferSize::Range { min, max } => BufferSizeRange::Range {
                min: *min,
                max: *max,
            },
            cpal::SupportedBufferSize::Unknown => BufferSizeRange::Unknown,
        }
    }

    /// f32-only, per this module's own doc comment on [`SupportedConfigRange`].
    ///
    /// # This filter is load-bearing for exclusive mode, and narrows it
    ///
    /// Under `AUDCLNT_SHAREMODE_SHARED` the Windows audio engine converts for a stream, so the f32
    /// filter costs a device nothing it could otherwise have offered. Exclusive mode has no engine
    /// to convert for it and D-13.4's fork does not pretend otherwise: it drops
    /// `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` and `SRC_DEFAULT_QUALITY` there, WASAPI rejecting both
    /// under exclusive mode, so only a format the device accepts **natively** will open. Namir's
    /// engine is f32 end to end and this crate has no sample-format conversion layer, so a device
    /// that natively accepts only S16 or S24 is one Namir cannot feed in exclusive mode at all.
    ///
    /// The honest consequence, stated rather than left to be discovered: **exclusive mode engages
    /// only on devices that natively accept f32 in exclusive mode; an S16- or S24-only device falls
    /// back to shared.** That is the graceful degradation D-13.4 asks for, not a silent failure —
    /// [`crate::app`] says so in a notice. Widening this filter to make exclusive mode engage more
    /// often would only move the failure later: [`CpalBackend::supports_exclusive`] would answer
    /// `Engaged` for a format the build then cannot open. Supporting those devices needs a
    /// conversion layer and is out of M11's scope.
    fn to_f32_configs(
        configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    ) -> Vec<SupportedConfigRange> {
        configs
            .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
            .map(|c| SupportedConfigRange {
                channels: c.channels(),
                min_sample_rate_hz: c.min_sample_rate(),
                max_sample_rate_hz: c.max_sample_rate(),
                buffer_size: buffer_size_range(c.buffer_size()),
            })
            .collect()
    }

    fn to_stream_failure(error: cpal::Error) -> StreamFailure {
        match error.kind() {
            cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable => {
                StreamFailure::DeviceLost
            }
            cpal::ErrorKind::Xrun => StreamFailure::Xrun,
            _ => StreamFailure::Other(error.to_string()),
        }
    }

    /// One half of what an open needs: the rate/buffer/channel-count `cpal::StreamConfig`.
    ///
    /// [`StreamParams::share_mode`] is the *other* half and deliberately does not appear here.
    /// `cpal::StreamConfig` has no share-mode member in the fork either, because the share mode is
    /// meaningless on every backend but WASAPI; it travels beside the config as a
    /// [`WasapiStreamOptions`] instead — see [`wasapi_options`], which every caller of this
    /// function pairs it with.
    fn stream_config(params: StreamParams) -> cpal::StreamConfig {
        cpal::StreamConfig {
            channels: params.channels,
            sample_rate: params.sample_rate_hz,
            buffer_size: match params.buffer_frames {
                Some(frames) => cpal::BufferSize::Fixed(frames),
                None => cpal::BufferSize::Default,
            },
        }
    }

    /// The other half: [`StreamParams::share_mode`] in the fork's own vocabulary.
    ///
    /// `WasapiStreamOptions` is `#[non_exhaustive]`, so it is built from `default()` (which is
    /// `Shared`, i.e. exactly what the cross-platform API already does) and adjusted, never
    /// field-by-field — a later addition to it must not break this call site.
    pub(super) fn wasapi_options(share_mode: ShareMode) -> WasapiStreamOptions {
        WasapiStreamOptions::default().with_share_mode(match share_mode {
            ShareMode::Shared => CpalShareMode::Shared,
            ShareMode::Exclusive => CpalShareMode::Exclusive,
        })
    }

    /// The two stream directions, for the one query that has to try both — see
    /// [`CpalBackend::supports_exclusive`] for why it does.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Direction {
        Input,
        Output,
    }

    /// `name`'s **exclusive-mode** f32 configurations in `direction`, as that device reports them.
    ///
    /// `None` means the host enumerates no device of that name in that direction at all. That is a
    /// different fact from a device that answered and refused, and collapsing the two would make an
    /// output-only device look like an input device that says no.
    fn exclusive_configs(
        cpal_host: &cpal::Host,
        name: &str,
        direction: Direction,
    ) -> Option<Result<Vec<SupportedConfigRange>, AudioIoError>> {
        let devices = match direction {
            Direction::Input => cpal_host.input_devices(),
            Direction::Output => cpal_host.output_devices(),
        }
        .ok()?;
        let device = resolve_device(devices, name).ok()?;
        let options = wasapi_options(ShareMode::Exclusive);
        // The fork's queries answer for the share mode they are handed, and refuse exclusive mode
        // outright where there is no WASAPI endpoint -- which is the whole reason this is a query
        // and not an open-and-see (see `AudioBackend::supports_exclusive`'s own doc comment).
        let configs = match direction {
            Direction::Input => device.supported_input_configs_with(options),
            Direction::Output => device.supported_output_configs_with(options),
        };
        Some(
            configs
                .map(to_f32_configs)
                .map_err(|e| AudioIoError::ExclusiveModeUnavailable(e.to_string())),
        )
    }

    /// The rule [`CpalBackend::supports_exclusive`] applies to one direction's answer, split out
    /// from the device access above so the decision is testable with no device present.
    ///
    /// [`ExclusiveModeOutcome::Engaged`] needs a **positive** answer: an exclusive-mode range that
    /// actually covers the channel count and sample rate the caller has already settled on. Every
    /// other shape of answer is [`ExclusiveModeOutcome::Unsupported`] and the session runs shared —
    /// an `Err` (`ErrorKind::UnsupportedOperation` from a device with no WASAPI endpoint, or the
    /// device's own refusal), an empty set, and a set whose ranges are all for some other rate or
    /// channel count. Erring towards `Unsupported` is the direction that cannot produce a mode
    /// indicator that lies.
    pub(super) fn exclusive_outcome(
        probed: Result<Vec<SupportedConfigRange>, AudioIoError>,
        params: StreamParams,
    ) -> ExclusiveModeOutcome {
        let covered = probed.is_ok_and(|configs| {
            configs
                .iter()
                .any(|c| c.channels == params.channels && c.covers_rate(params.sample_rate_hz))
        });
        if covered {
            ExclusiveModeOutcome::Engaged
        } else {
            ExclusiveModeOutcome::Unsupported
        }
    }

    /// Wraps a real `cpal::Stream`. Play/pause map straight through; `Drop`ping this drops the
    /// `cpal::Stream`, which is what actually stops the callback (`cpal`'s own contract).
    struct CpalStream(cpal::Stream);

    impl AudioStream for CpalStream {
        fn play(&self) -> Result<(), AudioIoError> {
            self.0
                .play()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))
        }

        fn pause(&self) -> Result<(), AudioIoError> {
            self.0
                .pause()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))
        }
    }

    impl AudioBackend for CpalBackend {
        fn hosts(&self) -> Vec<HostInfo> {
            cpal::available_hosts()
                .into_iter()
                .map(|id| HostInfo {
                    name: id.name().to_string(),
                })
                .collect()
        }

        fn default_host(&self) -> HostInfo {
            HostInfo {
                name: cpal::default_host().id().name().to_string(),
            }
        }

        fn input_devices(&self, host: &HostInfo) -> Result<Vec<DeviceInfo>, AudioIoError> {
            let cpal_host = resolve_host(host)?;
            let default_name = cpal_host.default_input_device().map(|d| device_name(&d));
            let devices = cpal_host
                .input_devices()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            Ok(devices
                .map(|d| {
                    let name = device_name(&d);
                    let is_default = default_name.as_deref() == Some(name.as_str());
                    DeviceInfo { name, is_default }
                })
                .collect())
        }

        fn output_devices(&self, host: &HostInfo) -> Result<Vec<DeviceInfo>, AudioIoError> {
            let cpal_host = resolve_host(host)?;
            let default_name = cpal_host.default_output_device().map(|d| device_name(&d));
            let devices = cpal_host
                .output_devices()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            Ok(devices
                .map(|d| {
                    let name = device_name(&d);
                    let is_default = default_name.as_deref() == Some(name.as_str());
                    DeviceInfo { name, is_default }
                })
                .collect())
        }

        fn input_configs(
            &self,
            host: &HostInfo,
            device: &DeviceInfo,
        ) -> Result<Vec<SupportedConfigRange>, AudioIoError> {
            let cpal_host = resolve_host(host)?;
            let devices = cpal_host
                .input_devices()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            let cpal_device = resolve_device(devices, &device.name)?;
            let configs = cpal_device
                .supported_input_configs()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            Ok(to_f32_configs(configs))
        }

        fn output_configs(
            &self,
            host: &HostInfo,
            device: &DeviceInfo,
        ) -> Result<Vec<SupportedConfigRange>, AudioIoError> {
            let cpal_host = resolve_host(host)?;
            let devices = cpal_host
                .output_devices()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            let cpal_device = resolve_device(devices, &device.name)?;
            let configs = cpal_device
                .supported_output_configs()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            Ok(to_f32_configs(configs))
        }

        /// Asks the device, through D-13.4's fork, whether it reports an exclusive-mode f32
        /// configuration covering `params`' sample rate and channel count
        /// (`IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, ...)` underneath, on Windows). Until
        /// M11 this answered [`ExclusiveModeOutcome::Unsupported`] from a constant, because
        /// upstream `cpal` 0.18.1 exposes no way to request `AUDCLNT_SHAREMODE_EXCLUSIVE` at all —
        /// see this module's doc comment and `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`
        /// for that verification, which is now history rather than current behaviour.
        ///
        /// # Why it probes both directions
        ///
        /// [`AudioBackend::supports_exclusive`] is asked per *device* and carries no direction; the
        /// caller ([`crate::app::run`]) asks once for the input device and once for the output one.
        /// On WASAPI — the only backend where the question means anything — a device *is* a single
        /// endpoint, capture or render, so a name resolves in exactly one of the two lists and only
        /// that direction is probed. A host where one name really is both directions (ALSA's
        /// `default`) gets both probed and the answers ANDed, which is the conservative reading:
        /// answering `Engaged` off the one direction that happened to say yes would be the mode
        /// indicator lying about the other.
        ///
        /// A name in neither list is `Unsupported`, not an error — this method has no error channel
        /// and, per its trait doc comment, exists precisely so a caller never has to recover from a
        /// failed open.
        ///
        /// # What it probes against
        ///
        /// `params` is the configuration [`crate::device_state`] already negotiated against the
        /// device's **shared-mode** config set (FR-IO-040 runs before FR-IO-020's share mode is
        /// settled, since the rate and buffer size are what the user picks and persists). A device
        /// whose exclusive-mode format list does not happen to include that settled rate therefore
        /// answers `Unsupported` and the session runs shared, even though some *other* rate would
        /// have opened exclusively. Re-negotiating rate and buffer per share mode is a larger change
        /// to the settings path than M11 takes on; recorded here so it is not mistaken for a bug in
        /// the probe.
        fn supports_exclusive(
            &self,
            host: &HostInfo,
            device: &DeviceInfo,
            params: StreamParams,
        ) -> ExclusiveModeOutcome {
            let Ok(cpal_host) = resolve_host(host) else {
                return ExclusiveModeOutcome::Unsupported;
            };
            let mut answered = false;
            for direction in [Direction::Input, Direction::Output] {
                let Some(probed) = exclusive_configs(&cpal_host, &device.name, direction) else {
                    continue;
                };
                if exclusive_outcome(probed, params) != ExclusiveModeOutcome::Engaged {
                    return ExclusiveModeOutcome::Unsupported;
                }
                answered = true;
            }
            if answered {
                ExclusiveModeOutcome::Engaged
            } else {
                ExclusiveModeOutcome::Unsupported
            }
        }

        fn build_input_stream(
            &self,
            host: &HostInfo,
            device: &DeviceInfo,
            params: StreamParams,
            mut on_data: Box<dyn FnMut(&[f32]) + Send>,
            mut on_error: Box<dyn FnMut(StreamFailure) + Send>,
            activation_timeout: Duration,
        ) -> Result<Box<dyn AudioStream>, AudioIoError> {
            let cpal_host = resolve_host(host)?;
            let devices = cpal_host
                .input_devices()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            let cpal_device = resolve_device(devices, &device.name)?;
            let stream = cpal_device
                .build_input_stream_with::<f32, _, _>(
                    stream_config(params),
                    wasapi_options(params.share_mode),
                    move |data: &[f32], _info| on_data(data),
                    move |err| on_error(to_stream_failure(err)),
                    Some(activation_timeout),
                )
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            Ok(Box::new(CpalStream(stream)))
        }

        fn build_output_stream(
            &self,
            host: &HostInfo,
            device: &DeviceInfo,
            params: StreamParams,
            mut on_data: Box<dyn FnMut(&mut [f32]) + Send>,
            mut on_error: Box<dyn FnMut(StreamFailure) + Send>,
            activation_timeout: Duration,
        ) -> Result<Box<dyn AudioStream>, AudioIoError> {
            let cpal_host = resolve_host(host)?;
            let devices = cpal_host
                .output_devices()
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            let cpal_device = resolve_device(devices, &device.name)?;
            let stream = cpal_device
                .build_output_stream_with::<f32, _, _>(
                    stream_config(params),
                    wasapi_options(params.share_mode),
                    move |data: &mut [f32], _info| on_data(data),
                    move |err| on_error(to_stream_failure(err)),
                    Some(activation_timeout),
                )
                .map_err(|e| AudioIoError::OpenFailed(e.to_string()))?;
            Ok(Box::new(CpalStream(stream)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cpal_impl::{exclusive_outcome, wasapi_options};
    use super::*;

    /// The one host API on which FR-IO-020's exclusive mode exists at all, spelled as
    /// `cpal::HostId::name()` spells it. Compared at **runtime**, never compiled against: D-5.1
    /// keeps platform attributes out of every crate but `namir-platform` (`xtask layering` enforces
    /// it), and D-13.4's fork is deliberately free of them for the same reason.
    const WASAPI_HOST_NAME: &str = "WASAPI";

    fn params() -> StreamParams {
        StreamParams {
            sample_rate_hz: 48_000,
            buffer_frames: Some(128),
            channels: 2,
            share_mode: ShareMode::Exclusive,
        }
    }

    /// One exclusive-mode range as a device might report it, for the probe-rule tests below.
    fn range(channels: u16, min_hz: u32, max_hz: u32) -> SupportedConfigRange {
        SupportedConfigRange {
            channels,
            min_sample_rate_hz: min_hz,
            max_sample_rate_hz: max_hz,
            buffer_size: BufferSizeRange::Unknown,
        }
    }

    /// Every `(host, device)` pair this machine actually enumerates, both directions. Empty on a
    /// machine with no audio hardware — which is the normal case in CI's containers, and the reason
    /// no test below asserts that it found any.
    fn enumerated_devices(backend: &CpalBackend) -> Vec<(HostInfo, DeviceInfo)> {
        backend
            .hosts()
            .into_iter()
            .flat_map(|host| {
                let devices = backend
                    .input_devices(&host)
                    .unwrap_or_default()
                    .into_iter()
                    .chain(backend.output_devices(&host).unwrap_or_default());
                devices
                    .map(move |device| (host.clone(), device))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// FR-IO-020/FR-IO-080: a session that never asked for anything gets shared mode. Pinned on
    /// [`ShareMode`]'s own `Default` rather than only on `AppSettings::exclusive_mode`'s `false`,
    /// so the "shared unless asked" rule survives a future caller that builds a [`StreamParams`]
    /// from something other than the settings file.
    #[test]
    fn the_default_share_mode_is_shared_not_exclusive() {
        assert_eq!(ShareMode::default(), ShareMode::Shared);
    }

    /// **The honest-pin test, rewritten at M11 rather than deleted.**
    ///
    /// It was written to go red the day D-13.4's fork was pinned, and its name said so. It did not
    /// go red, and would not have: the fork refuses exclusive mode wherever there is no WASAPI
    /// endpoint behind the device, so on CI's Linux and macOS legs the real backend still answers
    /// `Unsupported` — for the same underlying reason it always did, no WASAPI. Keeping the old
    /// name would have left a passing test asserting something untrue about why it passes, so what
    /// it pins now is the property that *is* true and worth pinning: **a device with no WASAPI
    /// endpoint behind it never reports exclusive mode engaged.**
    ///
    /// This is the one test that drives the whole real probe path — `resolve_host`,
    /// `exclusive_configs`, the fork's `supported_{input,output}_configs_with`, `exclusive_outcome`
    /// — against real `cpal::Device`s rather than a fake, which is possible on a headless Linux
    /// runner precisely because the fork's share-mode API is compiled on every platform.
    ///
    /// WASAPI is skipped **by host name at runtime**, not by conditional compilation. On a Windows
    /// machine with a real interface a WASAPI endpoint may legitimately answer `Engaged`, and a
    /// test that failed there would be asserting the opposite of what FR-IO-020 asks for. On a
    /// machine with no audio devices at all the enumeration loop is empty, which is why the two
    /// hardware-free cases are asserted unconditionally first.
    #[test]
    fn a_device_with_no_wasapi_endpoint_never_reports_exclusive_mode_engaged() {
        let backend = CpalBackend::new();

        // No hardware needed for either of these: a host that resolves to nothing, and a device
        // name no host enumerates, are both "no WASAPI endpoint behind this device".
        assert_eq!(
            backend.supports_exclusive(
                &HostInfo {
                    name: "no such host".to_string(),
                },
                &DeviceInfo {
                    name: "no such device".to_string(),
                    is_default: true,
                },
                params(),
            ),
            ExclusiveModeOutcome::Unsupported,
        );
        assert_eq!(
            backend.supports_exclusive(
                &backend.default_host(),
                &DeviceInfo {
                    name: "no such device".to_string(),
                    is_default: true,
                },
                params(),
            ),
            ExclusiveModeOutcome::Unsupported,
        );

        for (host, device) in enumerated_devices(&backend) {
            if host.name == WASAPI_HOST_NAME {
                continue;
            }
            assert_eq!(
                backend.supports_exclusive(&host, &device, params()),
                ExclusiveModeOutcome::Unsupported,
                "host {:?}, device {:?}",
                host.name,
                device.name,
            );
        }
    }

    /// The query answers from what the device reports, not from what it was handed — a caller
    /// cannot talk the real backend into exclusive mode by passing `ShareMode::Exclusive` in the
    /// params, and cannot be told "unsupported" merely for having passed `Shared`. Asserted as an
    /// equality between two answers rather than against a constant, so it holds on a real WASAPI
    /// endpoint that genuinely does support exclusive mode, and every enumerated device is
    /// included for that reason.
    #[test]
    fn the_real_backends_answer_does_not_depend_on_the_share_mode_it_was_handed() {
        let backend = CpalBackend::new();
        let mut pairs = enumerated_devices(&backend);
        pairs.push((
            HostInfo {
                name: "no such host".to_string(),
            },
            DeviceInfo {
                name: "no such device".to_string(),
                is_default: true,
            },
        ));
        for (host, device) in pairs {
            let asked_shared = backend.supports_exclusive(
                &host,
                &device,
                StreamParams {
                    share_mode: ShareMode::Shared,
                    ..params()
                },
            );
            assert_eq!(
                asked_shared,
                backend.supports_exclusive(&host, &device, params()),
                "host {:?}, device {:?}",
                host.name,
                device.name,
            );
        }
    }

    /// The probe's decision rule, exercised without a device: a reported exclusive-mode range that
    /// covers both the settled channel count and the settled rate is what `Engaged` means.
    #[test]
    fn the_exclusive_probe_engages_only_on_a_range_covering_the_settled_rate_and_channels() {
        assert_eq!(
            exclusive_outcome(Ok(vec![range(2, 44_100, 96_000)]), params()),
            ExclusiveModeOutcome::Engaged,
        );
        // A range's endpoints are inclusive, and a device reporting one discrete rate reports it as
        // min == max -- the shape WASAPI's exclusive-mode format probe actually produces.
        assert_eq!(
            exclusive_outcome(Ok(vec![range(2, 48_000, 48_000)]), params()),
            ExclusiveModeOutcome::Engaged,
        );
        // Only one of several reported ranges has to cover.
        assert_eq!(
            exclusive_outcome(
                Ok(vec![range(2, 96_000, 192_000), range(2, 48_000, 48_000)]),
                params(),
            ),
            ExclusiveModeOutcome::Engaged,
        );
    }

    /// Every other shape of answer is `Unsupported` and the session runs shared. The channel-count
    /// case is the one worth spelling out: exclusive mode does no channel or format conversion, so
    /// a range at the right rate but the wrong channel count is not a configuration Namir can open.
    #[test]
    fn the_exclusive_probe_refuses_anything_short_of_a_covering_range() {
        for (case, probed) in [
            ("rate below the range", Ok(vec![range(2, 88_200, 192_000)])),
            ("rate above the range", Ok(vec![range(2, 22_050, 44_100)])),
            ("wrong channel count", Ok(vec![range(1, 48_000, 48_000)])),
            ("no ranges at all", Ok(vec![])),
            (
                "the device refused, or has no WASAPI endpoint",
                Err(AudioIoError::ExclusiveModeUnavailable(
                    "Exclusive mode requires a WASAPI device".to_string(),
                )),
            ),
        ] {
            assert_eq!(
                exclusive_outcome(probed, params()),
                ExclusiveModeOutcome::Unsupported,
                "{case}",
            );
        }
    }

    /// [`SupportedConfigRange::covers_rate`]'s endpoints are inclusive at both ends — pinned
    /// because two callers now depend on it (FR-IO-040's rate negotiation and FR-IO-020's
    /// exclusive probe) and an off-by-one at either end silently changes both.
    #[test]
    fn a_supported_config_ranges_endpoints_are_both_inclusive() {
        let r = range(2, 44_100, 48_000);
        assert!(r.covers_rate(44_100));
        assert!(r.covers_rate(48_000));
        assert!(r.covers_rate(46_000));
        assert!(!r.covers_rate(44_099));
        assert!(!r.covers_rate(48_001));
    }

    /// The one-line translation into D-13.4's fork's own vocabulary, pinned because inverting it
    /// would open every stream in the mode the session did *not* settle on, with nothing else in
    /// this crate able to notice: [`crate::app`]'s mode indicator reports the negotiated decision,
    /// not what the backend then did with it.
    #[test]
    fn the_share_mode_handed_to_the_fork_is_the_one_the_session_settled_on() {
        use cpal::platform::{ShareMode as CpalShareMode, WasapiStreamOptions};

        assert_eq!(
            wasapi_options(ShareMode::Exclusive).share_mode,
            CpalShareMode::Exclusive
        );
        assert_eq!(
            wasapi_options(ShareMode::Shared).share_mode,
            CpalShareMode::Shared
        );
        // `WasapiStreamOptions::default()` is the fork's own "behaves exactly as the plain
        // DeviceTrait method" case, and Namir's default share mode has to agree with it or a
        // session that asked for nothing would take a different path through the fork.
        assert_eq!(
            wasapi_options(ShareMode::default()),
            WasapiStreamOptions::default()
        );
    }

    /// FR-ERR-050's "carries the backend's own message" applied to the exclusive-mode refusal: the
    /// reason survives into the rendered text rather than being flattened to a bare category, and
    /// the message is distinguishable from [`AudioIoError::OpenFailed`]'s, which is the whole point
    /// of it being a separate variant.
    #[test]
    fn an_exclusive_mode_refusal_displays_its_reason_and_reads_differently_from_an_open_failure() {
        let refusal =
            AudioIoError::ExclusiveModeUnavailable("the device is already held exclusively".into());
        let text = refusal.to_string();
        assert!(text.contains("exclusive mode"), "{text}");
        assert!(
            text.contains("the device is already held exclusively"),
            "{text}"
        );
        assert_ne!(
            text,
            AudioIoError::OpenFailed("the device is already held exclusively".into()).to_string()
        );
    }
}
