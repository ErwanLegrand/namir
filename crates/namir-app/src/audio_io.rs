//! D-13.1: "Audio I/O for the standalone app uses `cpal` ... behind a Namir-owned trait so the
//! engine and UI never see cpal types." [`AudioBackend`]/[`AudioStream`] are that trait: every
//! other module in this crate ([`crate::device_state`], [`crate::host`], [`crate::worker`],
//! [`crate::stream`]) is written against these two traits and the plain data types below, never
//! against a `cpal::*` type directly. This module — counting its `convert` submodule, which is
//! part of the same boundary — is the one place `cpal` is named.
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
//! # The other half of exclusive mode: sample formats (added M11)
//!
//! Asking for exclusive mode is not enough to get it. Exclusive mode does no format conversion —
//! the fork drops `AUTOCONVERTPCM`/`SRC_DEFAULT_QUALITY` there because WASAPI rejects both — so
//! only a format the device accepts **natively** will open, and most real hardware (onboard HDA
//! codecs, USB class-compliant interfaces, HDMI) exposes integer PCM only. `f32` feels universal on
//! Windows only because shared mode's `GetMixFormat` reports the *engine's* mix format and the APO
//! chain converts. A build that could open exclusive streams in `f32` alone would therefore have
//! closed FR-IO-020 on a path that essentially never activates on real hardware.
//!
//! So `cpal_impl::acceptable_formats` names a small set of formats per share mode, and
//! `cpal_impl`'s `convert` submodule does the arithmetic for the integer ones inside the audio
//! callback. The probe ([`CpalBackend::supports_exclusive`]) and the open
//! ([`AudioBackend::build_output_stream`]) read that same set through the same predicate, so they
//! cannot disagree about whether a device is openable — which is what FR-IO-020's all-or-nothing
//! rule (`crate::app::negotiate_share_mode`) depends on.
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

mod convert;

/// The block size, in frames, a session runs at when the device reported no buffer-size preference
/// of its own and the settings file named none either.
///
/// Read by [`crate::app`] for [`namir_engine::PrepareContext`]'s `max_block_size`, and — since M11
/// — by `cpal_impl` for how much sample-format conversion scratch a stream pre-sizes; one constant
/// rather than two literals so those two can never disagree about how big a block is.
pub const DEFAULT_BLOCK_FRAMES: u32 = 512;

/// The block size, in frames, for a negotiated `buffer_frames` — [`DEFAULT_BLOCK_FRAMES`] when the
/// negotiation produced nothing, and never zero.
pub fn block_frames(buffer_frames: Option<u32>) -> usize {
    buffer_frames.unwrap_or(DEFAULT_BLOCK_FRAMES).max(1) as usize
}

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
/// supported in some sample format Namir can open — FR-IO-040's "sample rate and buffer size from
/// those the selected device reports as supported".
///
/// Which formats those are depends on the share mode and is `cpal_impl::acceptable_formats`'s
/// business, not this type's: a range carries no sample format of its own, because nothing
/// downstream of here chooses one. [`crate::device_state`]'s FR-IO-040 negotiation sees only the
/// **shared-mode** set, which is `f32` and nothing else — see
/// `docs/manual-tests/fr-io-040-sample-rate-buffer-size.md` for that documented gap. The
/// exclusive-mode probe sees the wider set the converting stream path can actually open.
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

/// `Display`, added M14 (issue #44), because the `Debug` rendering was reaching a user's screen:
/// `crate::app`'s stream-error callback built its notice detail with `format!("{other:?}")`, so a
/// human running `docs/manual-tests/fr-ui-070-non-modal-error-notices.md` on 2026-08-27
/// transcribed `Other("OS Error -2004287450 (FormatMessageW() returned error 317) ...")` — the
/// Rust variant name and all — off a real window.
impl std::fmt::Display for StreamFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceLost => f.write_str("the device is no longer available"),
            Self::Xrun => f.write_str("the audio buffer under- or overran"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

/// Message fragments a backend uses for "this endpoint has gone away", for a failure that reached
/// [`StreamFailure::Other`] because `cpal` did not classify it.
///
/// # Why this exists at all — and it is a live **R-5** data point, not a tidy-up
///
/// `to_stream_failure` maps `cpal::ErrorKind::DeviceNotAvailable`/`HostUnavailable` onto
/// [`StreamFailure::DeviceLost`], and that mapping is **known incomplete against a real observed
/// case**. On 2026-08-27 an interface was physically unplugged while audio was flowing on the
/// reference machine, and the failure arrived as `Other` carrying an unmapped OS error — the
/// WASAPI `AUDCLNT_E_RESOURCES_INVALIDATED` (`0x88890026`, which prints as the signed decimal
/// `-2004287450`), the code WASAPI raises for exactly "the endpoint device has been unplugged, or
/// the hardware has been reconfigured, disabled or removed". Namir reported a device loss anyway,
/// because [`crate::host`]'s code was chosen from the stream's *direction* and never from its
/// classification: right by accident, and equally willing to call an unrelated driver fault a
/// device loss.
///
/// So the classification was wrong, not merely its rendering. This recovers the device-loss cases
/// that reach `Other`, matching on the backend's own message because that is the only thing
/// `cpal::Error` exposes once its `ErrorKind` has come back as something else. §22's **R-5** says
/// device-removal handling is weak in any cross-platform audio library and that the happy path is
/// not the thing to test; this is that risk arriving, with a transcript.
///
/// **What it does not claim.** Matching on message text is a recovery, not a classification
/// scheme: a backend that reworded its errors would silently fall back to
/// [`crate::error_codes::STREAM_FAILED`], which is the safe direction (a truthful "the stream
/// stopped" rather than an invented "your device was unplugged"). The real fix is upstream, in
/// `cpal`'s own `ErrorKind`, and stays R-5's business.
const DEVICE_LOSS_MARKERS: &[&str] = &[
    // WASAPI, as the fork surfaces them: AUDCLNT_E_DEVICE_INVALIDATED (0x88890004) and
    // AUDCLNT_E_RESOURCES_INVALIDATED (0x88890026), in both the hex and signed-decimal spellings
    // an `OS Error` message can carry.
    "-2004287484",
    "0x88890004",
    "-2004287450",
    "0x88890026",
    // Backend-agnostic phrasings.
    "device was disconnected",
    "device is no longer",
    "device not available",
    "devicenotavailable",
    "no longer available",
    "unplugged",
    "invalidated",
    "disconnected",
];

/// Whether a [`StreamFailure::Other`] message names a device that has gone away. See
/// [`DEVICE_LOSS_MARKERS`] for the observed case that made this necessary.
#[must_use]
pub fn classifies_as_device_loss(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    DEVICE_LOSS_MARKERS.iter().any(|m| lowered.contains(m))
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
    /// `device`'s supported input configurations, restricted to the formats a **shared-mode**
    /// stream can be opened in — 32-bit float and nothing else (see [`SupportedConfigRange`]).
    fn input_configs(
        &self,
        host: &HostInfo,
        device: &DeviceInfo,
    ) -> Result<Vec<SupportedConfigRange>, AudioIoError>;
    /// `device`'s supported output configurations, shared-mode formats only — as
    /// [`Self::input_configs`].
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

    use super::convert;
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

    /// The sample formats a stream may be opened in under `share_mode`, **most preferred first**.
    ///
    /// # Why the set depends on the share mode
    ///
    /// Under `AUDCLNT_SHAREMODE_SHARED` the Windows audio engine converts for a stream, so asking
    /// for f32 costs a device nothing it could otherwise have offered — and asking for anything
    /// else would gain nothing, since the engine is already converting. Shared mode is therefore
    /// exactly what it was before M11: f32, through `cpal`'s typed builder, no conversion layer in
    /// the path at all.
    ///
    /// Exclusive mode has no engine to convert for it, and D-13.4's fork does not pretend
    /// otherwise: it drops `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` and `SRC_DEFAULT_QUALITY` there,
    /// WASAPI rejecting both, so only a format the device accepts **natively** will open. Most real
    /// hardware exposes integer PCM only, so Namir converts — see [`super::convert`] for the
    /// arithmetic and its boundary argument.
    ///
    /// # The order
    ///
    /// F32 first because it needs no conversion at all; then I32, then I24. Between the two integer
    /// widths the wider one is preferred: the engine is f32 end to end, so its samples carry 24
    /// bits of mantissa either way, and a device offering both is one whose driver will do
    /// something with the extra headroom rather than nothing.
    ///
    /// # Why I16 is not on this list
    ///
    /// A deliberate exclusion, not an unfinished one. Converting f32 to 16 bits by truncation —
    /// which is what an undithered conversion is — puts the quantisation error in correlation with
    /// the signal, and at 16 bits that is audible as distortion on quiet passages and on a decaying
    /// reverb or amp tail rather than as a noise floor. Doing it properly needs a dither generator
    /// (and, to be worth having, noise shaping), which is a DSP component with its own design,
    /// tests and CPU cost. Shipping undithered 16-bit truncation in an audio product is worse than
    /// not offering the format: it would silently degrade the output of a device that would
    /// otherwise have run perfectly well in shared mode. Essentially every exclusive-capable audio
    /// interface runs at 24 bits or better, so the coverage this costs is close to nil. A device
    /// that offers nothing wider than I16 in exclusive mode is answered `Unsupported` by the probe
    /// and runs shared, which is the graceful degradation D-13.4 asks for and which
    /// [`crate::app`] reports in a notice.
    pub(super) fn acceptable_formats(share_mode: ShareMode) -> &'static [cpal::SampleFormat] {
        match share_mode {
            ShareMode::Shared => &[cpal::SampleFormat::F32],
            ShareMode::Exclusive => &[
                cpal::SampleFormat::F32,
                cpal::SampleFormat::I32,
                cpal::SampleFormat::I24,
            ],
        }
    }

    /// One `cpal`-reported range in this crate's own vocabulary, dropping the sample format.
    fn to_range(c: &cpal::SupportedStreamConfigRange) -> SupportedConfigRange {
        SupportedConfigRange {
            channels: c.channels(),
            min_sample_rate_hz: c.min_sample_rate(),
            max_sample_rate_hz: c.max_sample_rate(),
            buffer_size: buffer_size_range(c.buffer_size()),
        }
    }

    /// `configs` reduced to the ranges Namir could open under `share_mode`, in this crate's own
    /// vocabulary. Ranges in a format [`acceptable_formats`] does not name are dropped; the rest
    /// keep their channel count, rate span and buffer-size range unchanged.
    ///
    /// Duplicates are not removed. Two reported ranges differing only in sample format collapse to
    /// two identical entries here, and every consumer asks "does *any* range cover this?" rather
    /// than counting them.
    pub(super) fn to_supported_configs(
        configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
        share_mode: ShareMode,
    ) -> Vec<SupportedConfigRange> {
        let acceptable = acceptable_formats(share_mode);
        configs
            .filter(|c| acceptable.contains(&c.sample_format()))
            .map(|c| to_range(&c))
            .collect()
    }

    /// The format an open should ask for: the first entry of [`acceptable_formats`] that `configs`
    /// offers at `params`' channel count and sample rate, or `None` if it offers none of them.
    ///
    /// This is the *same* question [`exclusive_outcome`] answers, asked for a different purpose —
    /// the probe wants a yes/no, the open wants the winning format. Keeping both on one predicate
    /// (an acceptable format, `params.channels` exactly, and [`SupportedConfigRange::covers_rate`])
    /// is what stops the probe promising exclusive mode for a configuration the open then refuses;
    /// `the_probe_and_the_open_agree_on_every_reported_format_set` pins the agreement directly.
    pub(super) fn preferred_format(
        configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
        share_mode: ShareMode,
        params: StreamParams,
    ) -> Option<cpal::SampleFormat> {
        let offered: Vec<cpal::SampleFormat> = configs
            .filter(|c| {
                let range = to_range(c);
                range.channels == params.channels && range.covers_rate(params.sample_rate_hz)
            })
            .map(|c| c.sample_format())
            .collect();
        acceptable_formats(share_mode)
            .iter()
            .copied()
            .find(|format| offered.contains(format))
    }

    /// How many interleaved `f32`s of conversion scratch a stream pre-sizes, from the block size
    /// and channel count already settled in `params`.
    ///
    /// A whole number of frames by construction, which [`super::convert`] relies on to keep its
    /// chunk boundaries on frame boundaries. It is a working size rather than a hard bound: a
    /// callback larger than this is converted in successive chunks, never by growing the buffer.
    pub(super) fn scratch_samples(params: StreamParams) -> usize {
        super::block_frames(params.buffer_frames) * params.channels.max(1) as usize
    }

    /// Classifies one `cpal::Error` into this crate's own vocabulary (D-13.1).
    ///
    /// **The `_` arm is not a fall-through any more (issue #44).** `cpal`'s own `ErrorKind` is
    /// known incomplete against a real observed case: the physical unplug of 2026-08-27 arrived
    /// here as neither `DeviceNotAvailable` nor `HostUnavailable`, but as an unclassified error
    /// carrying WASAPI's `AUDCLNT_E_RESOURCES_INVALIDATED`. So the message is read before giving
    /// up on it — see [`super::classifies_as_device_loss`] for the marker list, the transcript it
    /// came from, and what this recovery does and does not claim.
    fn to_stream_failure(error: cpal::Error) -> StreamFailure {
        match error.kind() {
            cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable => {
                StreamFailure::DeviceLost
            }
            cpal::ErrorKind::Xrun => StreamFailure::Xrun,
            _ => {
                let message = error.to_string();
                if super::classifies_as_device_loss(&message) {
                    StreamFailure::DeviceLost
                } else {
                    StreamFailure::Other(message)
                }
            }
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

    /// `name`'s **exclusive-mode** configurations in `direction`, as that device reports them,
    /// narrowed to the formats [`acceptable_formats`] names for exclusive mode.
    ///
    /// `None` means there was no device of that name in that direction to ask — the host
    /// enumerated none by that name, or the enumeration itself failed. That is a different fact
    /// from a device that answered and refused, and collapsing the two would make an output-only
    /// device look like an input device that says no.
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
                .map(|c| to_supported_configs(c, ShareMode::Exclusive))
                .map_err(|e| AudioIoError::ExclusiveModeUnavailable(e.to_string())),
        )
    }

    /// The sample format `direction`'s stream on `device` should be opened in for `params`.
    ///
    /// Shared mode answers `F32` immediately, **without asking the device anything**: the Windows
    /// audio engine converts, f32 is what this crate has always opened with, and a query made here
    /// would be a behaviour change to a path M11 has no business touching.
    ///
    /// Exclusive mode asks the device — through the same fork query the probe used, with the same
    /// options — and applies [`preferred_format`]. A `None` answer means the device's exclusive
    /// format list does not cover this configuration in any format Namir can open. By the time an
    /// open happens [`CpalBackend::supports_exclusive`] has already said otherwise, so reaching
    /// this is a device whose state changed between the probe and the open; it is reported as
    /// [`AudioIoError::ExclusiveModeUnavailable`] rather than silently downgraded to shared,
    /// because [`crate::app`] has by then already told the user which mode the session is in.
    fn chosen_format(
        device: &cpal::Device,
        direction: Direction,
        params: StreamParams,
    ) -> Result<cpal::SampleFormat, AudioIoError> {
        if params.share_mode == ShareMode::Shared {
            return Ok(cpal::SampleFormat::F32);
        }
        let options = wasapi_options(params.share_mode);
        let configs = match direction {
            Direction::Input => device.supported_input_configs_with(options),
            Direction::Output => device.supported_output_configs_with(options),
        }
        .map_err(|e| AudioIoError::ExclusiveModeUnavailable(e.to_string()))?;
        preferred_format(configs, params.share_mode, params).ok_or_else(|| {
            AudioIoError::ExclusiveModeUnavailable(format!(
                "the device reports no exclusive-mode format Namir can open at {} Hz, {} channels",
                params.sample_rate_hz, params.channels,
            ))
        })
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
            Ok(to_supported_configs(configs, ShareMode::Shared))
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
            Ok(to_supported_configs(configs, ShareMode::Shared))
        }

        /// Asks the device, through D-13.4's fork, whether it reports an exclusive-mode
        /// configuration covering `params`' sample rate and channel count in a format
        /// [`acceptable_formats`] names
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

        /// Opens in whichever format `chosen_format` settles on. Shared mode always takes the
        /// `f32` branch, unchanged since M6; only an exclusive-mode open on an integer-only device
        /// reaches `build_converting_input`.
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
            let format = chosen_format(&cpal_device, Direction::Input, params)?;
            let stream = match format {
                cpal::SampleFormat::F32 => cpal_device.build_input_stream_with::<f32, _, _>(
                    stream_config(params),
                    wasapi_options(params.share_mode),
                    move |data: &[f32], _info| on_data(data),
                    move |err| on_error(to_stream_failure(err)),
                    Some(activation_timeout),
                ),
                cpal::SampleFormat::I32 => build_converting_input::<i32>(
                    &cpal_device,
                    params,
                    on_data,
                    on_error,
                    activation_timeout,
                ),
                cpal::SampleFormat::I24 => build_converting_input::<cpal::I24>(
                    &cpal_device,
                    params,
                    on_data,
                    on_error,
                    activation_timeout,
                ),
                other => return Err(unconvertible_format(other)),
            };
            Ok(Box::new(CpalStream(
                stream.map_err(|e| AudioIoError::OpenFailed(e.to_string()))?,
            )))
        }

        /// As [`AudioBackend::build_input_stream`], for the playback direction.
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
            let format = chosen_format(&cpal_device, Direction::Output, params)?;
            let stream = match format {
                cpal::SampleFormat::F32 => cpal_device.build_output_stream_with::<f32, _, _>(
                    stream_config(params),
                    wasapi_options(params.share_mode),
                    move |data: &mut [f32], _info| on_data(data),
                    move |err| on_error(to_stream_failure(err)),
                    Some(activation_timeout),
                ),
                cpal::SampleFormat::I32 => build_converting_output::<i32>(
                    &cpal_device,
                    params,
                    on_data,
                    on_error,
                    activation_timeout,
                ),
                cpal::SampleFormat::I24 => build_converting_output::<cpal::I24>(
                    &cpal_device,
                    params,
                    on_data,
                    on_error,
                    activation_timeout,
                ),
                other => return Err(unconvertible_format(other)),
            };
            Ok(Box::new(CpalStream(
                stream.map_err(|e| AudioIoError::OpenFailed(e.to_string()))?,
            )))
        }
    }

    /// The `match` arm that cannot be reached: [`chosen_format`] only ever returns an entry of
    /// [`acceptable_formats`], and every one of those has a branch above. `cpal::SampleFormat` is
    /// `#[non_exhaustive]`, so the arm has to exist. Making it an error rather than a `panic!` or a
    /// silent f32 open means that a future widening of `acceptable_formats` which forgets to add a
    /// branch gets a failed open with a readable message, rather than a crash or — worse — a stream
    /// opened in a format nothing in the callback path converts.
    fn unconvertible_format(format: cpal::SampleFormat) -> AudioIoError {
        AudioIoError::OpenFailed(format!(
            "sample format {format} was selected but this build cannot convert it"
        ))
    }

    /// Opens a capture stream in integer format `T`, converting each callback to `f32` before
    /// handing it to `on_data`.
    ///
    /// Generic over the format rather than matching inside the callback: the format is settled once
    /// at build time, so the audio thread does no dispatch at all.
    fn build_converting_input<T: convert::IntegerFormat>(
        device: &cpal::Device,
        params: StreamParams,
        on_data: convert::InputCallback,
        mut on_error: Box<dyn FnMut(StreamFailure) + Send>,
        activation_timeout: Duration,
    ) -> Result<cpal::Stream, cpal::Error> {
        let mut converter = convert::InputConverter::new(on_data, scratch_samples(params));
        device.build_input_stream_raw_with(
            stream_config(params),
            T::FORMAT,
            wasapi_options(params.share_mode),
            move |data: &cpal::Data, _info| {
                if let Some(codes) = data.as_slice::<T>() {
                    converter.drain(codes);
                }
                // `cpal`'s own typed builder `expect()`s on the `None` here. A host handing back a
                // different format than it was asked for is a bug, but an audio callback is the
                // worst place in the process to panic from, so this drops the block instead: the
                // stream stays alive, the bridge underruns, and FR-IO-060's xrun counter says so.
            },
            move |err| on_error(to_stream_failure(err)),
            Some(activation_timeout),
        )
    }

    /// Opens a playback stream in integer format `T`, running `on_data` into an `f32` scratch and
    /// converting the result. As [`build_converting_input`], in the other direction.
    fn build_converting_output<T: convert::IntegerFormat>(
        device: &cpal::Device,
        params: StreamParams,
        on_data: convert::OutputCallback,
        mut on_error: Box<dyn FnMut(StreamFailure) + Send>,
        activation_timeout: Duration,
    ) -> Result<cpal::Stream, cpal::Error> {
        let mut converter = convert::OutputConverter::new(on_data, scratch_samples(params));
        device.build_output_stream_raw_with(
            stream_config(params),
            T::FORMAT,
            wasapi_options(params.share_mode),
            move |data: &mut cpal::Data, _info| match data.as_slice_mut::<T>() {
                Some(codes) => converter.fill(codes),
                // As `build_converting_input`, except that an output callback must leave *something*
                // in the buffer: an all-zero byte pattern is silence in every signed integer and
                // IEEE float format `cpal` can hand back here, so this is silence rather than
                // whatever the device buffer happened to hold.
                None => data.bytes_mut().fill(0),
            },
            move |err| on_error(to_stream_failure(err)),
            Some(activation_timeout),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::cpal_impl::{
        acceptable_formats, exclusive_outcome, preferred_format, scratch_samples,
        to_supported_configs, wasapi_options,
    };
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

    /// **The transcript from the 2026-08-27 manual run, verbatim.** A physical unplug of a
    /// PreSonus AudioBox 22VSL, while audio was flowing, reached
    /// `docs/manual-tests/fr-ui-070-non-modal-error-notices.md`'s step 8 in this shape: `cpal`
    /// classified it as neither `DeviceNotAvailable` nor `HostUnavailable`, so it arrived as a
    /// message rather than as a `DeviceLost`. That the *classification* was wrong — not merely
    /// its rendering — is issue #44's larger half, and this is the case it was found on.
    #[test]
    fn the_unplug_observed_on_2026_08_27_classifies_as_a_device_loss() {
        let observed = "OS Error -2004287450 (FormatMessageW() returned error 317)";
        assert!(classifies_as_device_loss(observed), "{observed}");
    }

    /// The device-invalidated codes in every spelling an `OS Error` line can carry, plus the
    /// backend-agnostic phrasings.
    #[test]
    fn the_device_loss_markers_cover_both_wasapi_codes_and_plain_english() {
        for message in [
            "OS Error -2004287484 (AUDCLNT_E_DEVICE_INVALIDATED)",
            "0x88890004",
            "0x88890026",
            "The device was disconnected",
            "audio endpoint unplugged",
        ] {
            assert!(classifies_as_device_loss(message), "{message}");
        }
    }

    /// The safe direction: an error that says nothing about a device stays unclassified, so it is
    /// reported as a stream failure rather than as an invented device removal. This is exactly
    /// what the pre-M14 code could not do, since it chose from the stream's direction alone.
    #[test]
    fn an_unrelated_error_is_not_promoted_to_a_device_loss() {
        for message in [
            "the requested buffer size is not supported",
            "OS Error -2004287465 (AUDCLNT_E_UNSUPPORTED_FORMAT)",
            "",
        ] {
            assert!(!classifies_as_device_loss(message), "{message:?}");
        }
    }

    /// Issue #44's rendering half: no `Debug` shape may reach a user-facing string. `Other`'s
    /// message is written through, without the variant name and quotes `format!("{:?}")` adds.
    #[test]
    fn stream_failure_displays_without_its_debug_variant_name() {
        let failure = StreamFailure::Other("OS Error -1 (something)".to_string());
        assert_eq!(failure.to_string(), "OS Error -1 (something)");
        assert!(!failure.to_string().contains("Other("));
        assert_eq!(
            StreamFailure::DeviceLost.to_string(),
            "the device is no longer available"
        );
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

    /// One reported range as `cpal` hands it over, for the format-selection tests below. All of
    /// them are at [`params`]' own channel count and rate unless a case deliberately says
    /// otherwise, so the *format* is the only thing under test.
    fn reported(
        format: cpal::SampleFormat,
        channels: u16,
        min_hz: u32,
        max_hz: u32,
    ) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(
            channels,
            min_hz,
            max_hz,
            cpal::SupportedBufferSize::Unknown,
            format,
        )
    }

    /// A range covering exactly what [`params`] asks for, in `format`.
    fn covering(format: cpal::SampleFormat) -> cpal::SupportedStreamConfigRange {
        reported(format, 2, 48_000, 48_000)
    }

    /// The preference order, stated as an order rather than inferred from the two pairwise tests
    /// below: F32 first because it needs no conversion at all, then the wider integer format.
    #[test]
    fn the_exclusive_format_preference_runs_f32_then_i32_then_i24() {
        assert_eq!(
            acceptable_formats(ShareMode::Exclusive),
            &[
                cpal::SampleFormat::F32,
                cpal::SampleFormat::I32,
                cpal::SampleFormat::I24
            ],
        );
    }

    /// F32 wins whenever the device offers it, whatever else is on the list and whatever order the
    /// device reported them in — the reported order must not decide this, because a device is free
    /// to enumerate in any order it likes.
    #[test]
    fn f32_is_chosen_whenever_the_device_offers_it_at_the_settled_configuration() {
        for offered in [
            vec![covering(cpal::SampleFormat::F32)],
            vec![
                covering(cpal::SampleFormat::I24),
                covering(cpal::SampleFormat::I32),
                covering(cpal::SampleFormat::F32),
            ],
            vec![
                covering(cpal::SampleFormat::F32),
                covering(cpal::SampleFormat::I32),
            ],
        ] {
            assert_eq!(
                preferred_format(offered.into_iter(), ShareMode::Exclusive, params()),
                Some(cpal::SampleFormat::F32),
            );
        }
    }

    /// With no F32 on offer, the wider integer format wins over the narrower one.
    #[test]
    fn i32_is_chosen_over_i24_when_the_device_offers_both_but_not_f32() {
        assert_eq!(
            preferred_format(
                vec![
                    covering(cpal::SampleFormat::I24),
                    covering(cpal::SampleFormat::I32)
                ]
                .into_iter(),
                ShareMode::Exclusive,
                params(),
            ),
            Some(cpal::SampleFormat::I32),
        );
        assert_eq!(
            preferred_format(
                vec![covering(cpal::SampleFormat::I24)].into_iter(),
                ShareMode::Exclusive,
                params(),
            ),
            Some(cpal::SampleFormat::I24),
        );
    }

    /// **The deliberate I16 exclusion, asserted rather than only written down.** A device whose
    /// exclusive-mode format list holds nothing wider than 16 bits is offered no format at all, so
    /// [`CpalBackend::supports_exclusive`]'s rule answers `Unsupported` and the session runs
    /// shared. See `cpal_impl::acceptable_formats` for why 16-bit output is refused rather than
    /// truncated: it wants dither, and undithered truncation is worse than not offering the format.
    #[test]
    fn a_device_offering_only_sixteen_bit_formats_is_not_offered_exclusive_mode() {
        let i16_only = || {
            vec![
                covering(cpal::SampleFormat::I16),
                covering(cpal::SampleFormat::U16),
            ]
        };
        assert_eq!(
            preferred_format(i16_only().into_iter(), ShareMode::Exclusive, params()),
            None,
        );
        assert!(to_supported_configs(i16_only().into_iter(), ShareMode::Exclusive).is_empty());
        assert_eq!(
            exclusive_outcome(
                Ok(to_supported_configs(
                    i16_only().into_iter(),
                    ShareMode::Exclusive
                )),
                params(),
            ),
            ExclusiveModeOutcome::Unsupported,
        );
    }

    /// A format on the acceptable list is still no use at the wrong rate or the wrong channel
    /// count — the format filter and the configuration filter are ANDed, not ORed.
    #[test]
    fn an_acceptable_format_at_the_wrong_rate_or_channel_count_is_not_chosen() {
        for (case, offered) in [
            (
                "rate outside the range",
                vec![reported(cpal::SampleFormat::I32, 2, 88_200, 192_000)],
            ),
            (
                "wrong channel count",
                vec![reported(cpal::SampleFormat::I32, 1, 48_000, 48_000)],
            ),
            ("nothing reported at all", vec![]),
        ] {
            assert_eq!(
                preferred_format(offered.into_iter(), ShareMode::Exclusive, params()),
                None,
                "{case}",
            );
        }
    }

    /// **The probe and the open must never disagree**, since FR-IO-020's all-or-nothing rule
    /// settles the share mode from the probe's answer and [`crate::app`] then tells the user which
    /// mode the session is in. Asserted as an equivalence over a table rather than inferred from
    /// the two sides being written against the same helper, so a later change to either side that
    /// breaks the agreement fails here.
    #[test]
    fn the_probe_and_the_open_agree_on_every_reported_format_set() {
        for offered in [
            vec![],
            vec![covering(cpal::SampleFormat::F32)],
            vec![covering(cpal::SampleFormat::I32)],
            vec![covering(cpal::SampleFormat::I24)],
            vec![covering(cpal::SampleFormat::I16)],
            vec![
                covering(cpal::SampleFormat::I16),
                covering(cpal::SampleFormat::I24),
            ],
            vec![reported(cpal::SampleFormat::I32, 2, 88_200, 192_000)],
            vec![reported(cpal::SampleFormat::F32, 1, 48_000, 48_000)],
            vec![
                reported(cpal::SampleFormat::F32, 1, 48_000, 48_000),
                reported(cpal::SampleFormat::I24, 2, 44_100, 96_000),
            ],
        ] {
            let probe = exclusive_outcome(
                Ok(to_supported_configs(
                    offered.clone().into_iter(),
                    ShareMode::Exclusive,
                )),
                params(),
            );
            let open =
                preferred_format(offered.clone().into_iter(), ShareMode::Exclusive, params());
            assert_eq!(
                probe == ExclusiveModeOutcome::Engaged,
                open.is_some(),
                "probe said {probe:?} but the open would choose {open:?} for {offered:?}",
            );
        }
    }

    /// **Shared mode is untouched.** Its acceptable set is exactly what the old f32-only filter
    /// was, so a device reporting integer formats offers `crate::device_state`'s FR-IO-040
    /// negotiation nothing it did not offer before M11, and no shared-mode stream can ever be
    /// opened through the converting path.
    #[test]
    fn shared_mode_still_accepts_nothing_but_f32() {
        assert_eq!(
            acceptable_formats(ShareMode::Shared),
            &[cpal::SampleFormat::F32]
        );
        let mixed = || {
            vec![
                covering(cpal::SampleFormat::I32),
                covering(cpal::SampleFormat::I24),
                covering(cpal::SampleFormat::I16),
                covering(cpal::SampleFormat::F32),
            ]
        };
        assert_eq!(
            preferred_format(mixed().into_iter(), ShareMode::Shared, params()),
            Some(cpal::SampleFormat::F32),
        );
        // One entry out of four: the three integer ranges are dropped, exactly as the pre-M11
        // `to_f32_configs` dropped them.
        assert_eq!(
            to_supported_configs(mixed().into_iter(), ShareMode::Shared),
            vec![range(2, 48_000, 48_000)],
        );
        // ... and an integer-only device reports no shared-mode configuration at all, which is
        // also what it did before M11.
        assert!(
            to_supported_configs(
                vec![covering(cpal::SampleFormat::I32)].into_iter(),
                ShareMode::Shared
            )
            .is_empty()
        );
    }

    /// The conversion scratch is a whole number of frames — [`super::convert`]'s chunking keeps its
    /// boundaries on frame boundaries only because of that — and never zero, which would make its
    /// chunk length zero and its loop diverge.
    #[test]
    fn the_conversion_scratch_is_a_whole_number_of_frames_and_never_zero() {
        for buffer_frames in [None, Some(0), Some(1), Some(64), Some(4096)] {
            for channels in [0u16, 1, 2, 8] {
                let samples = scratch_samples(StreamParams {
                    buffer_frames,
                    channels,
                    ..params()
                });
                assert!(samples > 0, "{buffer_frames:?}, {channels} channels");
                assert_eq!(
                    samples % channels.max(1) as usize,
                    0,
                    "{buffer_frames:?}, {channels} channels",
                );
            }
        }
        assert_eq!(
            scratch_samples(StreamParams {
                buffer_frames: Some(128),
                channels: 2,
                ..params()
            }),
            256,
        );
    }

    /// The block size a session runs at is one number, not two: [`crate::app`]'s engine
    /// `max_block_size` and (from M11) the sample-format conversion scratch both come from here, so
    /// they cannot disagree.
    #[test]
    fn the_default_block_size_is_used_whenever_the_negotiation_produced_none() {
        assert_eq!(block_frames(None), DEFAULT_BLOCK_FRAMES as usize);
        assert_eq!(block_frames(Some(0)), 1);
        assert_eq!(block_frames(Some(256)), 256);
    }
}
