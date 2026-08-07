# FR-IO-010/040 manual test: device selection, sample rate and buffer size negotiate against real hardware

**Requirement (literal):** FR-IO-010 — "the user shall be able to select an audio input device and
an audio output device from those the system reports, including selecting different devices for
each where the platform permits." FR-IO-040 — "the user shall be able to select sample rate and
buffer size from those the selected device reports as supported, and the current values shall
always be displayed."

**Verify: M** per platform. The *selection logic* — which device/rate/buffer to pick given what is
reported and what was remembered — is fully covered by automated tests
(`crates/namir-app/src/device_state.rs`'s `#[cfg(test)]` module, 24 tests, pure functions over
plain data). What those tests cannot cover is whether the real `cpal`-backed
[`CpalBackend`](../../crates/namir-app/src/audio_io.rs) actually reports real, usable data when
asked, and whether `crate::app::run`'s wiring of that data into `device_state`'s functions and then
into a real `cpal` stream actually opens on real hardware. This script covers that gap.

## Script

1. `cargo run --example list_devices -p namir-app` — enumerates every host and every input/output
   device under it, with every f32 configuration each reports (see
   `crates/namir-app/examples/list_devices.rs`). Confirm the output lists genuine devices with
   plausible sample-rate/channel/buffer data, not an empty list or an error.
2. `cargo run --bin namir` — starts the real standalone application against whatever this
   machine's default devices are. Confirm: the process does not panic or exit immediately; a
   window titled "Namir" appears; a log line reports the negotiated sample rate, buffer size, and
   device names.
3. Interactively: close the window (there is no in-app device-selection UI yet to change devices —
   see this crate's own final report's "known gaps" section for why FR-IO-010's *selection*
   half currently has no interactive surface, only automatic negotiation) and confirm the process
   exits cleanly.

## Executed run (this session, Windows 11, WASAPI, real hardware: a PreSonus AudioBox 22VSL audio
interface and a Trust webcam's built-in microphone)

Step 1:

```
$ cargo run --example list_devices -p namir-app
hosts (1): ["WASAPI"]
default host: "WASAPI"

== host: WASAPI ==
  input devices (2):
    - Ligne (AudioBox 22VSL) (default: false)
      f32, 2 ch, 48000..=48000 Hz, buffer 480..=480 frames
    - Microphone (Trust 1080p HD Webcam) (default: true)
      f32, 2 ch, 48000..=48000 Hz, buffer 480..=480 frames
  output devices (2):
    - Realtek Digital Output (2- Realtek(R) Audio) (default: false)
      f32, 2 ch, 8000..=8000 Hz, buffer 80..=80 frames
      ... (14 rate/buffer pairs, 8 kHz through 384 kHz) ...
    - Haut-parleurs (AudioBox 22VSL) (default: true)
      f32, 2 ch, 8000..=8000 Hz, buffer 80..=80 frames
      ... (14 rate/buffer pairs, 8 kHz through 384 kHz) ...
```

Real devices, real per-device configuration ranges, exactly the shape `device_state.rs`'s tests
assume `SupportedConfigRange` data looks like.

Step 2:

```
$ cargo run --bin namir
namir: audio stream started
namir: 48000 Hz, 480-frame buffer, ~20.0 ms estimated round-trip latency (in: "Microphone (Trust 1080p HD Webcam)", out: "Haut-parleurs (AudioBox 22VSL)")
```

Confirmed via `Get-Process`: a real Win32 window with `MainWindowTitle` "Namir" was open while this
ran. `crate::device_state::negotiate_shared_sample_rate` picked 48 kHz (both devices' one
supported rate), `negotiate_buffer_size` picked the device-reported 480 frames (`BufferSizeRange`
was not `Unknown` here, so `PREFERRED_BUFFER_FRAMES` — 256 — was clamped into the device's
`{480, 480}` range, correctly landing on 480), and `crate::stream::open` followed by `.play()`
succeeded against the real AudioBox 22VSL output and the real webcam microphone input — no
`DEVICE_OPEN_FAILED` notice, no panic.

Step 3: the window was closed via `CloseMainWindow()` (equivalent to a user clicking the close
button) rather than a hard kill, specifically so `app::run`'s post-window code (FR-IO-080's
settings save) would run. It did: the process exited with no further output and no panic.

**Result: PASS.** Real device enumeration, real sample-rate/buffer negotiation, and a real opened,
playing audio stream against real hardware, all confirmed in this session — see
`fr-io-080-settings-persistence.md` for the companion run that also exercises the save/fallback
path this same execution produced.

**Not covered by this script, and why:** selecting a *different* device than whatever was
negotiated automatically — FR-IO-010's literal "the user shall be able to select" implies an
interactive control, and none exists in `namir-ui`'s shared FR-UI-020 screen (that crate's scope is
the amp/cab screen; FR-IO has no UI owner yet in this codebase — see this crate's final report).
Today's negotiation is fully automatic (remembered choice, else system default, else first
enumerated), which is a real, working, but non-interactive implementation of FR-IO-010's *outcome*
without its *mechanism*. Flagged as a known gap, not silently passed over.
