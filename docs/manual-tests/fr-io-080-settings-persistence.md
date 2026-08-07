# FR-IO-080 manual test: audio settings persist and degrade gracefully across sessions

**Requirement (literal):** "Audio device selection, sample rate, buffer size and channel mapping
shall persist between sessions, and the application shall degrade gracefully to a working default
if the remembered device is unavailable at start-up."

**Verify: I.** The persistence *mechanism* (`crates/namir-app/src/settings.rs`) and the
degrade-gracefully *selection logic* (`crates/namir-app/src/device_state.rs::select_device`) are
both fully covered by automated tests. What those tests cannot cover is the real integration:
does `crate::app::run` actually write the negotiated choice to the real
`namir_platform::config_dir()` location, and does it actually read a stale entry back and fall back
correctly on the next real launch. This script covers that.

## Script

1. Delete (or note the contents of) `%APPDATA%\Namir\audio-settings.json` (Windows) /
   `~/.config/namir/audio-settings.json` (Linux) / `~/Library/Application Support/Namir/audio-settings.json`
   (macOS), if present.
2. Run `namir`, let it negotiate devices, close it (via the window's close control, not a task-kill
   — FR-IO-080's save happens after the window closes, per `crate::app::run`'s own structure).
3. Inspect the settings file: confirm it now names the real device(s)/rate/buffer this session
   negotiated.
4. Hand-edit the settings file to name a device that does not exist (e.g. append `" XYZ"` to a
   device name). Run `namir` again.
5. Confirm: the application still starts (no crash, no hang — FR-IO-080's own "degrade gracefully"
   wording), and negotiates a real, working device in place of the missing one.
6. Close `namir` again (window close, not kill) and re-inspect the settings file: confirm it has
   self-healed back to the device that actually worked, not the stale/broken name from step 4.

## Executed run (this session, Windows 11, WASAPI)

Step 2, first launch (no prior settings file):

```
$ cargo run --bin namir
namir: audio stream started
namir: 48000 Hz, 480-frame buffer, ~20.0 ms estimated round-trip latency (in: "Microphone (Trust 1080p HD Webcam)", out: "Haut-parleurs (AudioBox 22VSL)")
```

A real Win32 window titled "Namir" was confirmed open (`Get-Process | ... MainWindowTitle`) while
this ran, then closed gracefully via `CloseMainWindow()` (the programmatic equivalent of clicking
the window's close button).

Step 3, `%APPDATA%\Namir\audio-settings.json` after that close:

```json
{
  "host_name": "WASAPI",
  "input_device_name": "Microphone (Trust 1080p HD Webcam)",
  "output_device_name": "Haut-parleurs (AudioBox 22VSL)",
  "exclusive_mode": false,
  "sample_rate_hz": 48000,
  "buffer_size_frames": 480,
  "channel_mapping": {
    "input_channel": null,
    "output_channel_left": null,
    "output_channel_right": null
  }
}
```

Exactly the real negotiated device names/rate/buffer — `crate::settings::save` wrote real data to
the real `namir_platform::config_dir()` location.

Step 4, `output_device_name` hand-edited to `"Nonexistent Device XYZ"`, then relaunched:

```
$ cargo run --bin namir
namir: audio stream started
namir: 48000 Hz, 480-frame buffer, ~20.0 ms estimated round-trip latency (in: "Microphone (Trust 1080p HD Webcam)", out: "Haut-parleurs (AudioBox 22VSL)")
```

Step 5: the application started normally — no crash, no hang, no error dialog blocking startup —
and the log shows it fell back to the real `"Haut-parleurs (AudioBox 22VSL)"` output device (the
system default), exactly `crate::device_state::select_device`'s documented fallback order
(remembered, else default, else first). The window opened and was confirmed present via
`Get-Process`, then closed gracefully.

Step 6, settings file after that second close:

```json
{
  "output_device_name": "Haut-parleurs (AudioBox 22VSL)",
  ...
}
```

Self-healed: the stale `"Nonexistent Device XYZ"` entry is gone, replaced by the device that
actually worked. A third launch would now use it directly, with no fallback needed.

**Result: PASS.** All six steps executed for real, against a real config directory and real
hardware, in this session.

**One gap recorded rather than silently passed over:** step 5's fallback is confirmed via the
negotiated device *actually used* (the log line and the self-healed settings file both prove it).
What was **not** independently confirmed in this run is that
`crate::error_codes::REMEMBERED_DEVICE_UNAVAILABLE`'s user-facing notice actually appeared in the
FR-UI-020 window during step 5 — this agent session has no way to read pixels off a native Win32
window, only process state and log output. The code path that pushes that notice
(`crate::app::run`'s `host.report(...)` calls for `input.fell_back_from`/`output.fell_back_from`)
is straight-line, unconditional code reached by the exact same fallback branch
`device_state::select_device`'s own unit tests already exercise (`fell_back_from` is `Some` in
exactly this scenario), so it is very likely correct, but "very likely" is not "confirmed by
looking at the screen" — a human should glance at the window during step 5 to close this one
remaining gap.
