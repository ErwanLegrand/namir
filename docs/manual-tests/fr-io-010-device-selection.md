# FR-IO-010 manual test: interactive audio input and output device selection

**Requirement (literal):** FR-IO-010 — "the user shall be able to select an audio input device and
an audio output device from those the system reports, including selecting different devices for
each where the platform permits."

**Verify: M** per platform.

## Script

1. `cargo run --bin namir` — start the standalone application.
2. Confirm the top panel renders the "Audio Settings" button.
3. Click "Audio Settings" to open the audio settings overlay window.
4. Verify the "Input Device" and "Output Device" dropdown combo boxes list all available system audio devices.
5. Select a different input device and output device from the dropdowns.
6. Verify that the selected devices are reflected in the UI and persisted to `audio-settings.json`.
7. Close the audio settings overlay and the application window.

## Executed run (this session)

**Result: NOT EXECUTED this session — script above is ready to run by a person with a display and real audio devices.**
