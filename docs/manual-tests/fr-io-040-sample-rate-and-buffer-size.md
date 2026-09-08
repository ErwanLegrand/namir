# FR-IO-040 manual test: sample rate and buffer size selection

**Requirement (literal):** FR-IO-040 — "the user shall be able to select sample rate and
buffer size from those the selected device reports as supported, and the current values shall
always be displayed."

**Verify: M** per platform.

## Script

1. `cargo run --bin namir` — start the standalone application.
2. Click the "Audio Settings" button in the top bar to open the overlay panel.
3. Verify that "Sample Rate" and "Buffer Size" combo boxes display the currently active sample rate (in Hz) and buffer size (in frames).
4. Select a different sample rate from the supported rates list.
5. Select a different buffer size from the supported buffer sizes list.
6. Confirm that the selected sample rate and buffer size update in the UI, audio actually plays through cleanly and runs at the selected rate and buffer size, and the values persist across restarts.
7. Close the audio settings overlay and close the application.
8. **Declined buffer size notice (issue #167, `app.audio_io.buffer_size_declined`):**
   - In `audio-settings.json`, set `"buffer_size_frames"` to a value not supported by the current device (e.g. `960` on a device supporting only power-of-two buffer sizes).
   - Launch the application: confirm audio plays using a supported fallback buffer size (or device default).
   - Confirm that a `Warning` notice `app.audio_io.buffer_size_declined` is displayed non-modally in the top bar (`"The requested buffer size is not supported by the audio device (requested 960 frames, using <actual> frames)"` or `"using the device default"`).
   - Verify that `audio-settings.json` still retains `"buffer_size_frames": 960` rather than overwriting it with the fallback value.

## Executed run (this session)

**Result: NOT EXECUTED this session — script above is ready to run by a person with a display and real audio devices.**
