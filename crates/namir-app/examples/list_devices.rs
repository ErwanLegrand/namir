//! FR-IO-010/040 manual-verification aid: enumerates every host, every input/output device under
//! it, and every f32 configuration each device reports, using the real `cpal`-backed
//! [`namir_app::audio_io::CpalBackend`] — not a fake. Run with `cargo run --example list_devices -p
//! namir-app`. No window, no audio processing; this only exercises device enumeration
//! ([`namir_app::audio_io::AudioBackend::hosts`]/`input_devices`/`output_devices`/`input_configs`/
//! `output_configs`), which is real device I/O this crate's own automated tests cannot exercise
//! (see `docs/manual-tests/fr-io-010-device-enumeration.md`).
//!
//! M11 added a second job: for each device it also asks
//! [`namir_app::audio_io::AudioBackend::supports_exclusive`] whether WASAPI **exclusive** mode is
//! available, at every channel count the device reported and a sweep of common sample rates. Run
//! this on the reference machine before `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`:
//! it predicts what that script will find, without opening a stream, and distinguishes "this
//! device cannot do exclusive mode" from "not at the rate we picked". See
//! [`print_exclusive_support`] for how to read the output.

use namir_app::audio_io::{
    AudioBackend, BufferSizeRange, CpalBackend, DeviceInfo, ExclusiveModeOutcome, HostInfo,
    ShareMode, StreamParams, SupportedConfigRange,
};

/// Rates to ask each device about in exclusive mode. Exclusive mode negotiates against the
/// device's own native format rather than the engine's mix format, so a device can perfectly well
/// support exclusive mode at one rate and refuse it at another -- and Namir settles its rate from
/// the *shared*-mode config set before the share mode is negotiated. Sweeping tells you whether a
/// refusal is "this device cannot do exclusive mode" or "not at the rate we happened to pick".
const PROBE_RATES_HZ: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

fn main() {
    let backend = CpalBackend::new();
    let hosts = backend.hosts();
    println!(
        "hosts ({}): {:?}",
        hosts.len(),
        hosts.iter().map(|h| &h.name).collect::<Vec<_>>()
    );
    println!("default host: {:?}", backend.default_host().name);

    for host in &hosts {
        println!("\n== host: {} ==", host.name);
        match backend.input_devices(host) {
            Ok(devices) => {
                println!("  input devices ({}):", devices.len());
                for device in &devices {
                    println!("    - {} (default: {})", device.name, device.is_default);
                    match backend.input_configs(host, device) {
                        Ok(configs) => {
                            for c in &configs {
                                print_config(c);
                            }
                            print_exclusive_support(&backend, host, device, &configs);
                        }
                        Err(e) => println!("      configs error: {e}"),
                    }
                }
            }
            Err(e) => println!("  input_devices error: {e}"),
        }

        match backend.output_devices(host) {
            Ok(devices) => {
                println!("  output devices ({}):", devices.len());
                for device in &devices {
                    println!("    - {} (default: {})", device.name, device.is_default);
                    match backend.output_configs(host, device) {
                        Ok(configs) => {
                            for c in &configs {
                                print_config(c);
                            }
                            print_exclusive_support(&backend, host, device, &configs);
                        }
                        Err(e) => println!("      configs error: {e}"),
                    }
                }
            }
            Err(e) => println!("  output_devices error: {e}"),
        }
    }
}

/// FR-IO-020: ask the device itself whether exclusive mode is available, at every channel count it
/// reported and a sweep of common rates.
///
/// This is the check to run on the reference machine *before* executing
/// `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`, because it predicts what that script
/// will find without opening a stream. Three outcomes are worth telling apart:
///
/// * `engaged` at the rate the app would pick -- exclusive mode will be used.
/// * `unsupported` everywhere -- the endpoint offers no format Namir can feed it. Namir accepts
///   F32, I32 and I24 (24 valid bits in a 32-bit container) in exclusive mode, and deliberately
///   not I16. Note that cpal cannot express *packed* 24-bit (a 3-byte container) at all, so a
///   device offering only that shape reports unsupported here and falls back to shared.
/// * `engaged` at some rates only -- the device does exclusive mode, but not at the rate the
///   shared-mode negotiation settled on. That is a known limitation, recorded on
///   `AudioBackend::supports_exclusive`.
fn print_exclusive_support(
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
                backend.supports_exclusive(
                    host,
                    device,
                    StreamParams {
                        sample_rate_hz,
                        buffer_frames: None,
                        channels,
                        share_mode: ShareMode::Exclusive,
                    },
                ) == ExclusiveModeOutcome::Engaged
            })
            .collect();
        if engaged.is_empty() {
            println!("      exclusive mode, {channels} ch: unsupported at any probed rate");
        } else {
            println!("      exclusive mode, {channels} ch: engaged at {engaged:?} Hz");
        }
    }
}

fn print_config(c: &namir_app::audio_io::SupportedConfigRange) {
    let buf = match c.buffer_size {
        BufferSizeRange::Range { min, max } => format!("{min}..={max} frames"),
        BufferSizeRange::Unknown => "unknown".to_string(),
    };
    println!(
        "      f32, {} ch, {}..={} Hz, buffer {}",
        c.channels, c.min_sample_rate_hz, c.max_sample_rate_hz, buf
    );
}
