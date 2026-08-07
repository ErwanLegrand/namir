# FR-UI-030 manual test: every control operable by mouse and keyboard, with an accessible name

**Requirement (literal):** every control shall be operable by mouse and by keyboard, and every
control shall have an accessible name.

**Verify: M** (manual, against a written accessibility script — this document is that script).

## What's mechanically true today, and what isn't

`crates/namir-ui/src/controls.rs`'s `param_control` (every `namir_params::REGISTRY`-driven
control, including the global bypass) pairs its value control with a name label via
`egui::Response::labelled_by`, which is `egui`'s own mechanism for attaching an accessible name to
a widget's `accesskit` node — real, and exercised by this crate's own headless tests
(`controls::tests::double_clicking_the_name_label_is_detected_only_on_the_second_click` and
neighbours prove the label/value pairing and interaction both work). `egui::DragValue` (the one
widget type every control in this crate is built from — see `controls.rs`'s module doc comment for
why) is natively keyboard-operable once focused: Tab cycles focus, Enter/click enters edit mode,
arrow keys step the value, typing replaces it.

**What is not yet true, and is worth stating plainly rather than leaving implicit:**
`egui-baseview` 0.6.0 (the version this crate is pinned to, matching `spikes/s3-egui-baseview`'s
own `Cargo.lock`) does not itself forward `egui`'s `accesskit` tree to a platform screen reader —
there is no `accesskit`-to-Windows-UI-Automation adapter wired into the `baseview` window this
crate opens. So the accessible name is real at the `egui`/`accesskit` *data* level (a future
platform adapter, or a different windowing backend with one wired in, would see it correctly) but a
real screen reader (NVDA, Narrator) running against a `namir-ui` window today would not currently
announce it. This is a gap in the M6 dependency stack, not a gap in how `namir-ui` uses it —
closing it is future work (wiring an `accesskit` platform adapter into `namir-app`/`namir-clap`'s
window, or a `baseview` version that does this itself), out of scope for this crate alone.

## Script

Run this against a real, visible `namir-ui` window (see
`docs/manual-tests/fr-ui-010-standalone-window-renders.md` for how to get one — the
`manual_window_smoke` example, with its auto-close block commented out).

1. **Mouse operation.** For each control type present (a continuous `DragValue` like Input Trim,
   and a stepped one like Gate Enabled): click and drag to change the value; click once to enter
   text-edit mode and type a value, then press Enter; double-click the control's *name* to reset it
   to its default. Confirm all three work and the displayed value updates each time.
2. **Keyboard operation, mouse-free.** Tab through the screen. Confirm focus visibly lands on each
   control in turn (a focus outline). With a control focused: press Up/Down arrow to step its
   value; type a number directly (the control should enter edit mode); press Escape to cancel an
   in-progress edit without committing it.
3. **Accessible name, at the `egui`/`accesskit` data level (not via a screen reader — see the gap
   noted above).** This needs a small probe, since there is no wired screen reader to listen with
   this session. `egui::Context::run_ui`'s `FullOutput` carries an `accesskit_update` field
   (crate's `egui-0.35.0/src/context.rs`) when the `accesskit` feature path is active; a developer
   with a debugger or a quick `dbg!()` on that field, set on a build with a control focused, can
   confirm the focused node's `label`/`name` matches the control's name label text. This is the
   honest substitute for "run a screen reader and listen" available without a wired platform
   adapter — recorded as a data-level check, not represented as equivalent to an actual assistive-
   technology pass.

## Executed run (this session)

**Not executed.** This agent session has no way to interact with a real window (click, drag, type,
Tab) or run a screen reader — only to run processes and read stdout/exit codes (see
`fr-ui-010-standalone-window-renders.md`'s own note on the same limitation). What *is* verified by
automated test, and stands in for part of step 1/2 here: `controls.rs`'s headless tests prove the
double-click-reset gesture and the label/value `Id` pairing both function correctly against real
`egui` widget/interaction logic (not a mock), via synthetic `egui::RawInput` pointer events run
through `egui::Context::run_ui`. What those tests do **not** and cannot cover: an actual mouse drag
changing a `DragValue`'s value (covered instead by `egui::DragValue`'s own upstream test suite,
not re-verified here), Tab-key focus traversal across this crate's specific screen layout, and
anything screen-reader-observable.

**Result: NOT EXECUTED this session — script above is ready to run by a person with a display,
keyboard, and mouse against a real `namir-ui` window.** The one substantive finding worth acting on
before this is run for real: **wiring a real `accesskit` platform adapter is still open work**,
tracked here rather than silently assumed done because `Response::labelled_by` is called correctly.
