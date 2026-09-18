//! Standing regression probe for the JACK stream-drop race: opens a JACK duplex pair and drops
//! it, repeatedly, timing both sides of each drop. Before cpal fork commit `fabe84d`
//! (`fix(jack): never block stream drop on deactivation`), `jack_deactivate` could block the
//! caller forever on Jack2/Windows — one 4-minute hang in 40 rounds — leaving the sibling
//! client streaming into a dead consumer (namir's window-close hang; see D-13.4's M15
//! follow-up note). The fork now deactivates on a detached thread.
//!
//! The rounds (40) match the sample the pre-fix evidence was gathered at; a drop stuck past
//! 5 s is the bug returning. Exit status is the file's contract: **0** — every drop clean
//! across all rounds; **1** — one or more drops exceeded the ceiling, the regression is back;
//! **2** — the environment cannot run the probe (no JACK host or default device compiled in,
//! an enumeration, open or play failure — a machine with no server running lands here), which
//! a caller that only reads the status must not mistake for the race.
//!
//! Run whenever the cpal pin is bumped (R-10's highest-risk operation). Requires a running
//! JACK server.
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
        eprintln!("SKIP: no JACK host compiled; nothing to probe");
        std::process::exit(2);
    };
    let Ok(inputs) = backend.input_devices(&host) else {
        eprintln!("SKIP: input enumeration failed");
        std::process::exit(2);
    };
    let Ok(outputs) = backend.output_devices(&host) else {
        eprintln!("SKIP: output enumeration failed");
        std::process::exit(2);
    };
    let Some(input) = inputs.into_iter().find(|d| d.is_default) else {
        eprintln!("SKIP: no default JACK input device");
        std::process::exit(2);
    };
    let Some(output) = outputs.into_iter().find(|d| d.is_default) else {
        eprintln!("SKIP: no default JACK output device");
        std::process::exit(2);
    };
    let params = StreamParams {
        sample_rate_hz: 48_000,
        buffer_frames: Some(128),
        channels: 2,
        share_mode: ShareMode::Shared,
    };
    let mut hung = 0usize;
    let mut attempted = 0usize;
    for round in 0..40 {
        attempted += 1;
        let Ok(input_stream) = backend.build_input_stream(
            &host,
            &input,
            params,
            Box::new(move |_data: &[f32], _status: namir_app::audio_io::CallbackStatus| {}),
            Box::new(move |_failure: namir_app::audio_io::StreamFailure| {}),
            Duration::from_secs(2),
        ) else {
            eprintln!("SKIP: round {round}: input open failed");
            std::process::exit(2);
        };
        let Ok(output_stream) = backend.build_output_stream(
            &host,
            &output,
            params,
            Box::new(move |_data: &mut [f32], _status: namir_app::audio_io::CallbackStatus| {}),
            Box::new(move |_failure: namir_app::audio_io::StreamFailure| {}),
            Duration::from_secs(2),
        ) else {
            eprintln!("SKIP: round {round}: output open failed");
            std::process::exit(2);
        };
        let Ok(_) = input_stream.play() else {
            eprintln!("SKIP: round {round}: input play failed");
            std::process::exit(2);
        };
        let Ok(_) = output_stream.play() else {
            eprintln!("SKIP: round {round}: output play failed");
            std::process::exit(2);
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
    if hung > 0 {
        return Err(format!(
            "{hung} drops exceeded the 5 s ceiling in {attempted} rounds — the jack_deactivate \
             race is back (see D-13.4's M15 follow-up note)"
        ));
    }
    eprintln!("done, hung drops: {hung} in {attempted} rounds");
    Ok(())
}
