//! FR-IO-010/040 manual-verification aid: enumerates every host, every input/output device under
//! it, and every f32 configuration each device reports, using the real `cpal`-backed
//! [`namir_app::audio_io::CpalBackend`] — not a fake. Run with `cargo run --example list_devices -p
//! namir-app`. No window, no audio processing; this only exercises device enumeration
//! ([`namir_app::audio_io::AudioBackend::hosts`]/`input_devices`/`output_devices`/`input_configs`/
//! `output_configs`), which is real device I/O this crate's own automated tests cannot exercise
//! (see `docs/manual-tests/fr-io-010-device-enumeration.md`).

use namir_app::audio_io::{AudioBackend, BufferSizeRange, CpalBackend};

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
                        }
                        Err(e) => println!("      configs error: {e}"),
                    }
                }
            }
            Err(e) => println!("  output_devices error: {e}"),
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
