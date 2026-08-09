//! D-13.1: "Audio I/O for the standalone app uses `cpal` ... behind a Namir-owned trait so the
//! engine and UI never see cpal types." [`AudioBackend`]/[`AudioStream`] are that trait: every
//! other module in this crate ([`crate::device_state`], [`crate::host`], [`crate::worker`],
//! [`crate::stream`]) is written against these two traits and the plain data types below, never
//! against a `cpal::*` type directly. This module is the one place `cpal` is named.
//!
//! # A verified, load-bearing limitation of D-13.1's chosen dependency
//!
//! FR-IO-020 (Must) requires WASAPI **shared and exclusive** mode. Reading `cpal` 0.18.1's own
//! WASAPI backend source (`cpal-0.18.1/src/host/wasapi/device.rs`, both
//! `build_input_stream_raw_inner` and `build_output_stream_raw_inner`) shows the share mode is
//! hardcoded to `AUDCLNT_SHAREMODE_SHARED` with **no public API to request
//! `AUDCLNT_SHAREMODE_EXCLUSIVE`** — this is not a gap in this crate's usage of `cpal`, it is
//! absent from the dependency itself as pinned by D-13.1. [`ExclusiveModeOutcome::Unsupported`]
//! is what [`CpalBackend`] reports whenever exclusive mode is requested; see this crate's final
//! report for the full analysis and options (a `namir-platform`-owned raw WASAPI path — this crate
//! is not on D-5.3's unsafe carve-out list, so it cannot write one itself — or an upstream/forked
//! `cpal` change).
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

/// FR-IO-020's exclusive-mode request outcome — see this module's doc comment for why
/// [`Unsupported`](Self::Unsupported) is, today, the only outcome this crate's `cpal` version can
/// produce, regardless of platform.
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

/// The real backend, over `cpal` 0.18.1.
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

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    use super::{
        AudioBackend, AudioIoError, AudioStream, BufferSizeRange, CpalBackend, DeviceInfo,
        ExclusiveModeOutcome, HostInfo, StreamFailure, StreamParams, SupportedConfigRange,
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

    /// The single choke point where [`StreamParams::share_mode`] will eventually reach `cpal` —
    /// both `build_input_stream` and `build_output_stream` below go through here.
    ///
    /// **It currently ignores that field**, and there is nowhere for it to go: `cpal::StreamConfig`
    /// (0.18.1) has no share-mode member, because the share mode is a hardcoded
    /// `AUDCLNT_SHAREMODE_SHARED` local inside cpal's own WASAPI stream construction — this
    /// module's doc comment records the verification. D-13.4's forked `cpal` is what gives this
    /// function a field to set; until then dropping it here is the honest behaviour, and
    /// [`CpalBackend::supports_exclusive`] is what makes sure nobody was told otherwise.
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

        /// Always [`ExclusiveModeOutcome::Unsupported`], on every platform, for every device — the
        /// interim behaviour this module's doc comment promises, restated here because this is the
        /// method that makes the promise observable.
        ///
        /// This is not a "not implemented yet" stub for something already reachable: `cpal` 0.18.1
        /// exposes no way to request `AUDCLNT_SHAREMODE_EXCLUSIVE` at all (verified against its
        /// vendored source at M6; see the module doc comment and
        /// `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`). **D-13.4's Namir-maintained
        /// `cpal` fork, pinned by commit, is the single change that flips this**: once
        /// `crates/namir-app/Cargo.toml`'s `cpal` line points at it, this method asks the device
        /// through the fork's own share-mode-aware format negotiation
        /// (`IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, ...)`) instead of answering from a
        /// constant, and `stream_config` above gains a field to carry `params.share_mode` into.
        /// Nothing else in this crate changes: the seam, the negotiation in [`crate::app`] and the
        /// mode indicator are all already written against the answer, not against the constant.
        fn supports_exclusive(
            &self,
            _host: &HostInfo,
            _device: &DeviceInfo,
            _params: StreamParams,
        ) -> ExclusiveModeOutcome {
            ExclusiveModeOutcome::Unsupported
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
                .build_input_stream::<f32, _, _>(
                    stream_config(params),
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
                .build_output_stream::<f32, _, _>(
                    stream_config(params),
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
    use super::*;

    fn params() -> StreamParams {
        StreamParams {
            sample_rate_hz: 48_000,
            buffer_frames: Some(128),
            channels: 2,
            share_mode: ShareMode::Exclusive,
        }
    }

    /// FR-IO-020/FR-IO-080: a session that never asked for anything gets shared mode. Pinned on
    /// [`ShareMode`]'s own `Default` rather than only on `AppSettings::exclusive_mode`'s `false`,
    /// so the "shared unless asked" rule survives a future caller that builds a [`StreamParams`]
    /// from something other than the settings file.
    #[test]
    fn the_default_share_mode_is_shared_not_exclusive() {
        assert_eq!(ShareMode::default(), ShareMode::Shared);
    }

    /// **An honest pin on interim behaviour, which SHOULD go red the day D-13.4's fork is pinned.**
    /// `cpal` 0.18.1 cannot request `AUDCLNT_SHAREMODE_EXCLUSIVE` at all, so the real backend
    /// answers `Unsupported` for every device on every platform — including Windows, which is why
    /// this test needs no hardware, no device and no platform-conditional compilation to run
    /// everywhere CI runs.
    /// When the fork lands and a real device answers `Engaged`, this assertion fails, and that
    /// failure is the intended signal to delete it rather than a regression to fix.
    #[test]
    fn the_real_cpal_backend_reports_exclusive_mode_unsupported_until_the_fork_lands() {
        let backend = CpalBackend::new();
        let host = HostInfo {
            name: "any".to_string(),
        };
        let device = DeviceInfo {
            name: "any".to_string(),
            is_default: true,
        };
        assert_eq!(
            backend.supports_exclusive(&host, &device, params()),
            ExclusiveModeOutcome::Unsupported,
        );
    }

    /// The query answers from the build's own capability, not from what it was handed — a caller
    /// cannot talk the real backend into exclusive mode by passing `ShareMode::Exclusive` in the
    /// params, and cannot be told "unsupported" merely for having passed `Shared`.
    #[test]
    fn the_real_backends_answer_does_not_depend_on_the_share_mode_it_was_handed() {
        let backend = CpalBackend::new();
        let host = HostInfo {
            name: "any".to_string(),
        };
        let device = DeviceInfo {
            name: "any".to_string(),
            is_default: true,
        };
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
            backend.supports_exclusive(&host, &device, params())
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
