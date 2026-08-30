//! FR-IO-010/040 manual-verification aid: enumerates every host, every input/output device under
//! it, and asks each device whether WASAPI **exclusive** mode is available — using the real
//! `cpal`-backed [`namir_app::audio_io::CpalBackend`], not a fake. No window, no audio processing;
//! this only exercises device enumeration
//! ([`namir_app::audio_io::AudioBackend::hosts`]/`input_devices`/`output_devices`/`input_configs`/
//! `output_configs`) and [`namir_app::audio_io::AudioBackend::supports_exclusive`], which is real
//! device I/O this crate's own automated tests cannot exercise (see
//! `docs/manual-tests/fr-io-010-device-enumeration.md`).
//!
//! ```text
//! cargo run -p namir-app --example list_devices [-- [--verbose] [<sample-rate-hz>]]
//! ```
//!
//! * **Default** — one line per device: the endpoint name, whether it is the host default, and
//!   whether it would grant exclusive mode at `<sample-rate-hz>` (48000 unless given), **at the
//!   channel count `namir_app::app::run` would actually open that endpoint with** (see
//!   [`app_channel_count`]: 1 for a mono capture endpoint, not the 2 this listing hard-coded until
//!   that function existed). This is the form the FR-UI-070 and FR-IO-020 scripts want, because
//!   both need a device *name* and one needs a device that **refuses** exclusive mode.
//! * **`--verbose`** — additionally prints every configuration the device reports and sweeps
//!   [`PROBE_RATES_HZ`] for exclusive-mode support at every channel count reported. Run this on
//!   the reference machine before `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`: it
//!   predicts what that script will find without opening a stream, and distinguishes "this device
//!   cannot do exclusive mode" from "not at the rate we picked". See [`print_exclusive_sweep`] for
//!   how to read it.
//!
//! # Why the names matter more than they look
//!
//! `audio-settings.json` is matched against the *endpoint* name the backend reports, which on
//! Windows is localised and is usually not the product name on the box — `Ligne (AudioBox 22VSL)`,
//! not `AudioBox 22VSL`. A near-miss is not a warning, it is
//! `app.audio_io.remembered_device_unavailable` and a silent fallback to another device. So copy a
//! name from this output verbatim, quotes excluded, into `input_device_name`/`output_device_name`.
//!
//! # One example, not two (issue #92)
//!
//! There used to be a second, near-duplicate `list-devices.rs` beside this one: same backend, same
//! enumeration, same `supports_exclusive` probe, differing only in output detail and in which
//! manual-test script it happened to be written for. Cargo built both, and this crate has already
//! paid once for two copies of one computation drifting apart (`LibraryService`'s bootstrap — see
//! `namir_worker::library::LibraryService::open_default`). The two output shapes are now the two
//! settings of `--verbose`, and the hyphenated file is gone.

use namir_app::audio_io::{
    AudioBackend, BufferSizeRange, CpalBackend, DeviceInfo, ExclusiveModeOutcome, HostInfo,
    ShareMode, StreamParams, SupportedConfigRange,
};

/// Rates to ask each device about under `--verbose`. Exclusive mode negotiates against the
/// device's own native format rather than the engine's mix format, so a device can perfectly well
/// support exclusive mode at one rate and refuse it at another -- and Namir settles its rate from
/// the *shared*-mode config set before the share mode is negotiated. Sweeping tells you whether a
/// refusal is "this device cannot do exclusive mode" or "not at the rate we happened to pick".
const PROBE_RATES_HZ: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

/// The rate the concise listing probes at, unless one is given on the command line. What
/// `namir-app` itself would ask for on a device offering it.
const DEFAULT_PROBE_RATE_HZ: u32 = 48_000;

fn main() {
    let mut verbose = false;
    let mut rate = DEFAULT_PROBE_RATE_HZ;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
            other => match other.parse::<u32>() {
                Ok(hz) => rate = hz,
                Err(_) => {
                    eprintln!(
                        "usage: cargo run -p namir-app --example list_devices \
                         [-- [--verbose] [<sample-rate-hz>]]"
                    );
                    return;
                }
            },
        }
    }

    let backend = CpalBackend::new();
    let hosts = backend.hosts();
    let default_host = backend.default_host();
    println!("default host: {}", default_host.name);
    println!(
        "hosts ({}): {:?}\n",
        hosts.len(),
        hosts.iter().map(|h| &h.name).collect::<Vec<_>>()
    );

    for host in &hosts {
        let marker = if host.name == default_host.name {
            " (default)"
        } else {
            ""
        };
        println!("== host: {}{marker} ==", host.name);

        match backend.input_devices(host) {
            Ok(devices) => report(&backend, host, "input", &devices, rate, verbose),
            Err(e) => println!("  input devices unavailable: {e}"),
        }
        match backend.output_devices(host) {
            Ok(devices) => report(&backend, host, "output", &devices, rate, verbose),
            Err(e) => println!("  output devices unavailable: {e}"),
        }
        println!();
    }

    println!(
        "Copy a name verbatim, quotes excluded, into audio-settings.json's input_device_name or \
         output_device_name."
    );
}

/// One direction of one host.
fn report(
    backend: &CpalBackend,
    host: &HostInfo,
    direction: &str,
    devices: &[DeviceInfo],
    rate: u32,
    verbose: bool,
) {
    if devices.is_empty() {
        println!("  no {direction} devices");
        return;
    }
    println!("  {direction} devices ({}):", devices.len());
    for device in devices {
        let default = if device.is_default { " [default]" } else { "" };
        // Enumerated for every device now, not only under `--verbose`: the concise line's probe
        // needs a channel count, and the only honest one is the count `namir_app::app::run` would
        // negotiate for this endpoint (below).
        let configs = match direction {
            "input" => backend.input_configs(host, device),
            _ => backend.output_configs(host, device),
        };
        let configs = match configs {
            Ok(configs) => configs,
            Err(e) => {
                // Exactly what `app::run` does with this failure -- it calls
                // `configs_of(..).unwrap_or_default()` -- so the negotiation below falls through to
                // the same one-channel answer the app would open with. The error is still
                // printed: a device whose formats cannot be read is worth seeing in a manual run.
                println!("      configs error: {e}");
                Vec::new()
            }
        };
        let channels = app_channel_count(direction, &configs, rate);
        let exclusive = match backend.supports_exclusive(host, device, probe_params(rate, channels))
        {
            ExclusiveModeOutcome::Engaged => "exclusive ok",
            ExclusiveModeOutcome::Unsupported => "shared-only",
        };
        println!(
            "    \"{}\"{default}  -- {exclusive} at {rate} Hz, {channels} ch",
            device.name
        );

        if !verbose {
            continue;
        }
        for c in &configs {
            print_config(c);
        }
        print_exclusive_sweep(backend, host, device, &configs);
    }
}

/// The channel count [`namir_app::app::run`] would open this endpoint with at `rate` — the whole
/// point of the concise listing's probe, which asked with a hard-coded `2` until this pass.
///
/// **Why that was wrong and not merely approximate.** `app::run` negotiates each direction's
/// channel count from that device's own reported configurations
/// ([`namir_app::device_state::negotiate_channels`]), asking for the smallest count that meets the
/// engine's minimum: **1** for the capture side, **2** for playback. A mono capture endpoint — an
/// instrument input, which is the device this product is for — is therefore opened at one channel
/// by the application and was probed at two by this example, and a device that grants exclusive
/// mode at one channel and refuses it at two was reported `shared-only`. That output is what
/// `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md` tells its reader to trust when choosing
/// a device, so the error propagated into a manual run's conclusions.
///
/// Calls `negotiate_channels` rather than restating its rule, and mirrors `app::run`'s own
/// `.unwrap_or(1)` for the case where no reported configuration covers `rate` — the two must agree
/// by construction, since agreeing is the only property this function has.
fn app_channel_count(direction: &str, configs: &[SupportedConfigRange], rate: u32) -> u16 {
    // `app::run`'s two literals: 1 for the input's mono capture read, 2 for the stereo output
    // write. Named here rather than inlined so the asymmetry is legible at the call site.
    let minimum = if direction == "input" { 1 } else { 2 };
    namir_app::device_state::negotiate_channels(configs, rate, minimum).unwrap_or(1)
}

/// What the probe asks with: the backend's own default buffer size, and `ShareMode::Exclusive`
/// (ignored by the probe -- the question *is* whether exclusive mode is possible).
fn probe_params(sample_rate_hz: u32, channels: u16) -> StreamParams {
    StreamParams {
        sample_rate_hz,
        buffer_frames: None,
        channels,
        share_mode: ShareMode::Exclusive,
    }
}

/// FR-IO-020, `--verbose` only: ask the device itself whether exclusive mode is available, at every
/// channel count it reported and a sweep of [`PROBE_RATES_HZ`]. Three outcomes are worth telling
/// apart:
///
/// * `engaged` at the rate the app would pick -- exclusive mode will be used.
/// * `unsupported` everywhere -- the endpoint offers no format Namir can feed it. Namir accepts
///   F32, I32 and I24 (24 valid bits in a 32-bit container) in exclusive mode, and deliberately
///   not I16. Note that cpal cannot express *packed* 24-bit (a 3-byte container) at all, so a
///   device offering only that shape reports unsupported here and falls back to shared.
/// * `engaged` at some rates only -- the device does exclusive mode, but not at the rate the
///   shared-mode negotiation settled on. That is a known limitation, recorded on
///   `AudioBackend::supports_exclusive`.
fn print_exclusive_sweep(
    backend: &CpalBackend,
    host: &HostInfo,
    device: &DeviceInfo,
    configs: &[SupportedConfigRange],
) {
    let mut channel_counts: Vec<u16> = configs.iter().map(|c| c.channels).collect();
    channel_counts.sort_unstable();
    channel_counts.dedup();
    if channel_counts.is_empty() {
        println!("      exclusive mode: no configs reported, nothing to probe");
        return;
    }

    for channels in channel_counts {
        let engaged: Vec<u32> = PROBE_RATES_HZ
            .iter()
            .copied()
            .filter(|&sample_rate_hz| {
                backend.supports_exclusive(host, device, probe_params(sample_rate_hz, channels))
                    == ExclusiveModeOutcome::Engaged
            })
            .collect();
        if engaged.is_empty() {
            println!("      exclusive mode, {channels} ch: unsupported at any probed rate");
        } else {
            println!("      exclusive mode, {channels} ch: engaged at {engaged:?} Hz");
        }
    }
}

fn print_config(c: &SupportedConfigRange) {
    let buf = match c.buffer_size {
        BufferSizeRange::Range { min, max } => format!("{min}..={max} frames"),
        BufferSizeRange::Unknown => "unknown".to_string(),
    };
    println!(
        "      f32, {} ch, {}..={} Hz, buffer {}",
        c.channels, c.min_sample_rate_hz, c.max_sample_rate_hz, buf
    );
}
