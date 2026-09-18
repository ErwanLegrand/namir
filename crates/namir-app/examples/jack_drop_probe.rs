//! Throwaway diagnostic (issue: JACK window-close hang): opens a JACK duplex pair and drops it,
//! repeatedly, timing both sides of the drop. The suspected race is `jack_deactivate` (the jack
//! crate's `AsyncClient::drop`) blocking indefinitely on Jack2/Windows; a drop stuck past the
//! 5 s ceiling is the bug. Requires a running JACK server.
//!
//! ```text
//! cargo run -p namir-app --example jack_drop_probe
//! ```

use std::time::{Duration, Instant};

use namir_app::audio_io::{AudioBackend, CpalBackend, ShareMode, StreamParams};

const MAX_WAIT: Duration = Duration::from_secs(5);

fn main() -> Result<(), String> {
    let backend = CpalBackend::new();
    let Some(host) = backend.hosts().into_iter().find(|h| &h.name == "JACK") else {
        return Err("no JACK host compiled; nothing to probe".to_string());
    };
    let Ok(inputs) = backend.input_devices(&host) else {
        return Err("input enumeration failed".to_string());
    };
    let Ok(outputs) = backend.output_devices(&host) else {
        return Err("output enumeration failed".to_string());
    };
    let Some(input) = inputs.into_iter().find(|d| d.is_default) else {
        return Err("no default JACK input device".to_string());
    };
    let Some(output) = outputs.into_iter().find(|d| d.is_default) else {
        return Err("no default JACK output device".to_string());
    };
    let params = StreamParams {
        sample_rate_hz: 48_000,
        buffer_frames: Some(128),
        channels: 2,
        share_mode: ShareMode::Shared,
    };
    let mut hung = 0usize;
    for round in 0..=20 {
        let Ok(input_stream) = backend.build_input_stream(
            &host,
            &input,
            params,
            Box::new(move |_data: &[f32], _status: namir_app::audio_io::CallbackStatus| {}),
            Box::new(move |_failure: namir_app::audio_io::StreamFailure| {}),
            Duration::from_secs(2),
        ) else {
            return Err(format!("round {round}: input open failed"));
        };
        let Ok(output_stream) = backend.build_output_stream(
            &host,
            &output,
            params,
            Box::new(move |_data: &mut [f32], _status: namir_app::audio_io::CallbackStatus| {}),
            Box::new(move |_failure: namir_app::audio_io::StreamFailure| {}),
            Duration::from_secs(2),
        ) else {
            return Err(format!("round {round}: output open failed"));
        };
        let Ok(_) = input_stream.play() else {
            return Err(format!("round {round}: input play failed"));
        };
        let Ok(_) = output_stream.play() else {
            return Err(format!("round {round}: output play failed"));
        };
        std::thread::sleep(Duration::from_millis(150));

        let t0 = Instant::now();
        drop(output_stream);
        let output_drop = Instant::now() - t0;
        let t0 = Instant::now();
        drop(input_stream);
        let input_drop = Instant::now() - t0;
        let slow = output_drop > MAX_WAIT || input_drop > MAX_WAIT;
        if slow {
            hung += 1;
        }
        eprintln!(
            "round {:>2}: output drop {:?}, input drop {:?}{}",
            round,
            output_drop,
            input_drop,
            if slow { "  <-- hung" } else { "" }
        );
        if hung >= 3 {
            break;
        }
    }
    eprintln!("done, hung drops: {hung}");
    Ok(())
}
