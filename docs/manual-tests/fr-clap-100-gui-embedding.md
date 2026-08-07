# FR-CLAP-100 manual test: embedded GUI, real host, GUI-declined case

**Requirement (literal):** the plugin shall provide an embedded graphical editor via the CLAP GUI
extension, supporting the host embedding it, and shall function correctly if the host declines to
show a GUI at all.
*Verify: I.*

## What's mechanically true today

`crates/namir-clap/src/gui.rs` implements `PluginGuiImpl`, restricted to `GuiApiType::WIN32`,
non-floating (matching `spikes/s4-clack-clap`'s own S-4-validated shape, D-14.2). `set_parent`
calls `namir_ui::app::open_parented` with a real `crate::ui_host::ClapUiHost` (not the spike's
placeholder egui window), bridging to this instance's live `ParamMirror`, meters, loaded model/IR
names, library snapshot and notices (`crates/namir-clap/src/ui_host.rs`). The one `unsafe` block
this needs (`window.borrow_handle_unchecked()`) carries a full written safety argument in that
module's own doc comment, per D-5.3.

**The "host declines to show a GUI" half is true by construction, not merely by intent:** nothing
in `crates/namir-clap/src/audio.rs` (the audio-thread path) or `crates/namir-clap/src/main_thread.rs`
(state/params/latency handling) references `NamirMainThread::window` or calls anything in
`crates/namir-clap/src/gui.rs` — the GUI extension is entirely optional infrastructure the host
opts into by calling `create`/`set_parent`/`show`. `clap-validator`'s own suite ran the plugin
through its full parameter/state/process fuzzing without the GUI extension ever being invoked (the
validator does not exercise `gui` at all in this version), and every one of those tests passed —
direct evidence the plugin is fully functional with the GUI never shown.

## Why this needs a real host and can't be fully automated

Embedding is specifically about the host's own window-management code correctly receiving,
sizing, and parenting a foreign HWND — `clap-validator` has no GUI-driving test at all (confirmed:
none of its 44 tests reference the `gui` extension), and there is no way to observe "does this
render inside Reaper's own plugin-editor window frame, with correct focus/input/DPI behaviour"
without an actual host process and a screen.

## Script

1. Build `namir_clap.dll` in release mode; copy/rename it to `namir.clap` and place it at
   `namir_platform::clap_paths::clap_install_dir(ClapInstallScope::PerUser)`'s reported path
   (`%LOCALAPPDATA%\Programs\Common\CLAP` on Windows — **not** `%APPDATA%\...`, per D-13.3's own
   S-4 finding that Reaper silently ignores the latter).
2. Open Reaper, insert Namir on a track from the FX browser. Confirm the plugin appears under its
   declared name ("Namir") and vendor.
3. Open the plugin's editor (double-click the FX, or the host's own "show UI" control). Confirm:
   - The editor renders embedded in Reaper's own plugin window frame (not a separate floating
     window), matching S-4's own confirmed shape.
   - FR-UI-020's screen (input meter+trim, gate, loaded model/IR name, EQ, output meter+level,
     global bypass, library panel) is visible and responds to interaction — turning a knob updates
     both the GUI display and (confirmed via Reaper's own automation lane) the host-visible
     parameter value.
   - Closing the editor and reopening it does not crash or leak (open/close a few times).
4. Confirm the GUI-declined case: in a host or host mode that never opens the editor (or by simply
   never opening it in Reaper), confirm the plugin still processes audio correctly and responds to
   host automation — this is implicitly covered by every other manual/automated test in this round
   that never touches the GUI at all (e.g. `fr-clap-020`'s `clap-validator` run, which never
   invokes `gui`).

## Executed run (this session)

**Not executed** (steps 2–3, requiring a visible window and mouse/keyboard interaction). This
agent session has no way to interact with a real window — see
`docs/manual-tests/fr-ui-010-standalone-window-renders.md`'s identical limitation note, which
`spikes/s4-clack-clap`'s own S-4 record already established the same embedding mechanism works for
(Reaper on Windows 11, confirmed frame-counter-advancing render). What *is* verified this session:
`clap-validator`'s full suite (32 passed, 0 failed, 0 warnings, exit code 0) with the plugin loaded
and processing throughout, the GUI extension never invoked — direct evidence of step 4's
GUI-declined functional requirement. The embedding half (steps 2–3) is ready to run by a person
with Reaper installed.
