//! Prints every audio device this build can see, under the name `audio-settings.json` must use,
//! and asks each one whether it would grant exclusive mode.
//!
//! Written during FR-UI-070's manual run, where two steps need a device *name* and one needs a
//! device that **refuses** exclusive mode:
//!
//! - `docs/manual-tests/fr-ui-070-non-modal-error-notices.md` step 11 sets `input_device_name` to
//!   a device that does not exist, and step 12 sets `"exclusive_mode": true` against one that
//!   cannot grant it.
//! - `docs/manual-tests/fr-io-010-device-enumeration.md` and `fr-io-020-wasapi-exclusive-mode.md`
//!   ask the same two questions of a person at a machine.
//!
//! The names matter more than they look: `audio-settings.json` is matched against the *endpoint*
//! name the backend reports, which on Windows is localised and is usually not the product name on
//! the box — `Ligne (AudioBox 22VSL)`, not `AudioBox 22VSL`. A near-miss is not a warning, it is
//! `app.audio_io.remembered_device_unavailable` and a silent fallback to another device.
//!
//! ```text
//! cargo run -p namir-app --example list-devices [-- <sample-rate-hz>]
//! ```
//!
//! The exclusive-mode probe is asked at `<sample-rate-hz>` (default 48000) with the backend's own
//! default buffer size and two channels, which is what `namir-app` itself would ask at those
//! settings. A device answering `shared-only` there is the one step 12 wants.

use namir_app::audio_io::{
    AudioBackend, CpalBackend, DeviceInfo, ExclusiveModeOutcome, HostInfo, ShareMode, StreamParams,
};

fn main() {
    let rate: u32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(48_000);

    let backend = CpalBackend;
    let default_host = backend.default_host();
    println!("default host: {}\n", default_host.name);

    for host in backend.hosts() {
        let marker = if host.name == default_host.name {
            " (default)"
        } else {
            ""
        };
        println!("host {}{marker}", host.name);

        match backend.input_devices(&host) {
            Ok(devices) => report(&backend, &host, "input", &devices, rate),
            Err(e) => println!("  input devices unavailable: {e}"),
        }
        match backend.output_devices(&host) {
            Ok(devices) => report(&backend, &host, "output", &devices, rate),
            Err(e) => println!("  output devices unavailable: {e}"),
        }
        println!();
    }

    println!(
        "Copy a name verbatim, quotes excluded, into audio-settings.json's input_device_name or \
         output_device_name."
    );
}

fn report(
    backend: &CpalBackend,
    host: &HostInfo,
    direction: &str,
    devices: &[DeviceInfo],
    rate: u32,
) {
    if devices.is_empty() {
        println!("  no {direction} devices");
        return;
    }
    println!("  {direction}:");
    for device in devices {
        let params = StreamParams {
            sample_rate_hz: rate,
            buffer_frames: None,
            channels: 2,
            // Ignored by the probe — the question *is* whether exclusive mode is possible.
            share_mode: ShareMode::Exclusive,
        };
        let exclusive = match backend.supports_exclusive(host, device, params) {
            ExclusiveModeOutcome::Engaged => "exclusive ok",
            ExclusiveModeOutcome::Unsupported => "shared-only",
        };
        let default = if device.is_default { " [default]" } else { "" };
        println!(
            "    \"{}\"{default}  -- {exclusive} at {rate} Hz",
            device.name
        );
    }
}
