# FR-UI-050 manual test: reset-to-default and fine-adjustment gestures against a real control

**Requirement (literal):** a control shall reset to its default on a documented gesture
(double-click or equivalent), and shall support fine adjustment on a documented modifier.

**Verify: M.** The *reset* gesture's dispatch logic is fully covered by automated test:
`crates/namir-ui/src/controls.rs`'s `double_clicking_param_controls_label_emits_exactly_a_reset_intent`
drives two synthetic click frames through `egui::Context::run_ui` at the exact position
`param_control`'s name label lays out at, and asserts a double-click emits exactly one
`UiIntent::ResetParamToDefault` and no spurious `SetParam` — real `egui` widget/interaction logic,
not a mock, and its neighbour
`double_clicking_the_name_label_is_detected_only_on_the_second_click` additionally proves a single
click does not falsely register. What that test cannot cover: whether an actual mouse double-click
from a human, through a real OS and a real `egui-baseview` window, lands on the label the same way
the synthetic events do, and whether the reset is visually and audibly confirmed. The
*fine-adjustment* gesture (Shift+drag) has **no automated coverage at all** — per
`crates/namir-ui/src/controls.rs`'s own module doc comment, it is "`egui::DragValue`'s own built-in
behaviour ... deliberately not reimplemented here, just relied upon and documented," so nothing in
this crate's test suite exercises it, and it has never been independently verified against Namir's
actual controls (as opposed to `egui::DragValue`'s own upstream test suite, which this project does
not re-run). This script covers both gaps.

## Script

Run this against a real, visible `namir-ui` window (see
`docs/manual-tests/fr-ui-010-standalone-window-renders.md` for how to get one — the
`manual_window_smoke` example, with its auto-close block commented out, or a real `namir-app`/
`namir-clap` build).

1. **Reset gesture, continuous control.** Change Input Trim away from its default (drag it or type
   a value — see `fr-ui-040-numeric-value-entry.md`). Double-click the control's *name label* (not
   its value). Confirm the value snaps back to its default (0.0 dB) immediately, and that a single
   click on the label does nothing (no reset, no other side effect).
2. **Reset gesture, stepped control.** Change Gate Enabled away from its default. Double-click its
   name label. Confirm it returns to its documented default state.
3. **Reset gesture does not fire on the value itself.** Double-click the *value* (not the name
   label) of a control that has been changed from default. Confirm this does **not** reset it —
   per the module doc comment, double-click-to-reset is scoped to the name label specifically so it
   never conflicts with `DragValue`'s own double-click-to-select-all behaviour on the value.
4. **Fine adjustment, continuous control.** Click and hold on Input Trim's value to begin a normal
   drag; note roughly how far the mouse must move to change the value by 1 dB. Release, then repeat
   the drag while holding Shift. Confirm the value changes more slowly per pixel of mouse movement
   (finer control) than the unmodified drag, and that the displayed precision increases while Shift
   is held (per `DragValue`'s documented Shift behaviour).
5. **Fine adjustment, stepped control.** Repeat step 4 against a stepped control if one with more
   than two states is available (e.g. a multi-position control, if present). Confirm Shift+drag
   still functions sensibly (fine-grained stepping) rather than erroring or jumping erratically.
6. **Gestures are discoverable in-app.** Hover the mouse over a control's name label without
   clicking. Confirm a tooltip appears stating the control's default value, that double-clicking
   resets it, and that Shift+drag gives fine adjustment — matching `add_name_label`'s hover text in
   `crates/namir-ui/src/controls.rs`. This is FR-UI-050's "documented" clause: both gestures must be
   discoverable from inside the running application, not only from source comments or external
   documentation.

## Executed run (this session)

**Not executed.** This agent session has no way to interact with a real window (double-click, drag,
hold a modifier key) — only to run processes and read stdout/exit codes (see
`fr-ui-010-standalone-window-renders.md`'s and `fr-ui-030-accessibility-script.md`'s own notes on
the same limitation). What *is* verified by automated test, and stands in for part of steps 1–3
here: `controls.rs`'s headless tests prove the double-click-on-label-only-resets dispatch logic is
correct against real `egui` widget/interaction logic driven by synthetic pointer events, through the
same `Context::run_ui` entry point `egui-baseview` itself calls per frame. What those tests do not
and cannot cover, and what remains genuinely unverified pending this script actually being run: an
actual human double-click through a real window landing correctly, the visual/audible confirmation
of a reset, and — most notably, since it has zero automated coverage of any kind — whether
Shift+drag fine adjustment (steps 4–5) actually works against Namir's real controls at all, as
opposed to being merely assumed correct because `egui::DragValue` documents the behaviour upstream.

**Result: NOT EXECUTED this session — script above is ready to run by a person with a display,
keyboard, and mouse against a real `namir-ui` window.** The fine-adjustment gesture (steps 4–5) is
the higher-priority half to run first once a human is available: it is the only part of FR-UI-050
with no automated coverage of any kind today.
