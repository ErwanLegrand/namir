# FR-UI-010 manual test: `namir-ui`'s screen actually renders in a real window

**Requirement (literal):** a single graphical user interface implementation shall serve both
product configurations, differing only in the presence of the audio-device panel.

**Verify: S** (structural — namir-ui's `open_blocking`/`open_parented` share the one `render`
function; see `crates/namir-ui/src/app.rs`'s module doc comment). This manual test covers the part
S can't: `cargo test`'s coverage of this crate (`crates/namir-ui/src/*.rs`'s `#[cfg(test)]`
modules) is deliberately headless throughout — every test drives `egui::Context::run_ui` directly,
the same entry point `egui-baseview` itself calls per frame, but none of them opens a real OS
window, a real GPU/graphics context, or a real font atlas. That proves the *widget logic* (layout,
gestures, intent dispatch) is correct without needing a display attached to the test runner, but it
does not by itself prove the crate actually paints pixels when driven by a real
`egui_baseview::EguiWindow`. This script closes that specific gap.

## Script

1. Run `crates/namir-ui/examples/manual_window_smoke.rs`, which opens FR-UI-020's actual screen
   (`namir_ui::render` — the same function `NamirUi`/`open_blocking` call every real frame) via a
   real `egui_baseview::EguiWindow::open_blocking` call, against a canned `UiSnapshot` carrying
   representative content (two library entries, a loaded model/IR name, non-silent meters, one
   FR-UI-070 notice), for 90 frames, then closes itself automatically (unattended, the same pattern
   `spikes/s3-egui-baseview`'s own smoke test uses):
   ```
   cargo run --example manual_window_smoke -p namir-ui
   ```
2. Confirm the process exits 0 with no panic, and prints a frame count and a clean-close message.
3. Separately (interactively, not scripted): comment out the `send_viewport_cmd(Close)` block in
   the example, run it again, and actually look at the window. Confirm FR-UI-020's screen elements
   are visible and laid out sensibly: a top notice bar, a left library panel (search box plus the
   two sample entries), and a central area with the input meter, Input Trim, Gate, Model name, IR
   name, EQ, Output (meter + level), and Global Bypass, all without switching tabs.

## Executed run (this session)

Step 1–2, unattended:

```
$ cargo run --example manual_window_smoke -p namir-ui
   Compiling namir-ui v0.1.0 (.../crates/namir-ui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.55s
     Running `target\debug\examples\manual_window_smoke.exe`
build: egui context created
rendered 90 frames; closing
window closed cleanly
```

A real `baseview` window was created (Win32, this session's platform), a real OpenGL context was
initialised (`egui-baseview`'s `Renderer::new`, which panics on failure — see
`egui-baseview-0.6.0/src/window.rs`'s `EguiWindow::new`), `namir_ui::render`'s full FR-UI-020
layout was built and painted for 90 consecutive frames without panicking, and the window closed
itself cleanly on request. This is the strongest evidence available in this session that the crate
does not merely lay out correctly in a headless test harness but actually renders through the real
`egui-baseview`/`baseview`/OpenGL stack this platform provides.

**Step 3 (interactive visual confirmation) was not executed in this session** — this agent session
has no way to view or screenshot a native Win32 window, only to run processes and read their
stdout/exit code. Step 1–2's unattended run is real, executed evidence that the window opens and
renders without error; step 3's visual layout check (does it *look* right, are labels legible, is
nothing overlapping) still needs a human to actually look at the screen once. Left as the one
honestly-unexecuted part of this script, per this project's manual-test convention of recording
what was and wasn't actually run rather than asserting a result nobody observed.

**Result: PARTIAL.** PASS for steps 1–2 (executed). Step 3 requires a human with a display —
not executed this session. (Verdict token corrected to `PARTIAL` at M15: the sentence was always
this one, and `PASS` was never what it recorded — see `docs/manual-tests/README.md`.)
