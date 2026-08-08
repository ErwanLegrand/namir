# FR-UI-040 manual test: numeric value display and typed entry against a real control

**Requirement (literal):** every control shall display its current value numerically on demand, and
shall accept a typed numeric value.

**Verify: M.** The *parsing* half of this requirement is fully covered by automated test:
`crates/namir-ui/src/format.rs`'s `#[cfg(test)]` module exercises `parse_value` directly (pure,
`egui`-free) against a `Continuous` descriptor (`continuous_parses_a_plain_number`) and a `Stepped`
one (`stepped_parses_a_raw_step_index`), including clamping above/below range, rejecting non-numeric
text, and rejecting `NaN`/`inf` spellings. What those tests cannot cover: `parse_value` is only
wired in as `egui::DragValue`'s `custom_parser` hook (`crates/namir-ui/src/controls.rs`'s
`param_control`) — nothing in the automated suite drives an actual OS text-entry event through a
real `DragValue` widget's click-to-edit interaction, confirms the *display* half (`format_value`,
owned by `namir-params`, rendered via `custom_formatter`) actually shows on screen, or confirms
`DragValue`'s own edit-mode UX (click to enter, what the text field looks like, Enter to commit,
focus loss behaviour) works as expected against Namir's actual controls rather than against
`egui::DragValue`'s own upstream test suite. This script closes that gap.

## Script

Run this against a real, visible `namir-ui` window (see
`docs/manual-tests/fr-ui-010-standalone-window-renders.md` for how to get one — the
`manual_window_smoke` example, with its auto-close block commented out, or a real `namir-app`/
`namir-clap` build).

1. **Numeric display on demand.** For a continuous control (e.g. Input Trim) and a stepped one
   (e.g. Gate Enabled), confirm the control shows its current value as text next to/inside the
   control at all times, not only while being interacted with — no click or hover should be
   required to see the current value.
2. **Typed entry, continuous control.** Click Input Trim's value to enter edit mode. Confirm a text
   cursor appears and the field is editable. Type `6.0` and press Enter. Confirm the displayed value
   updates to `6.0` (or its formatted equivalent, e.g. `6.0 dB`) and the control's audible/visual
   effect (if monitoring audio) matches a +6 dB trim.
3. **Typed entry, stepped control.** Click Gate Enabled's value to enter edit mode. Type `off` and
   press Enter. Confirm it resolves to the "Off" state (case-insensitive name matching). Repeat,
   typing a raw index (`1`) instead of a name; confirm it resolves to the corresponding named state
   ("On").
4. **Out-of-range typed value.** Enter edit mode on a continuous control with a bounded range (e.g.
   Input Trim, ±24 dB) and type a value outside that range (e.g. `999`). Confirm on commit (Enter)
   the value is clamped to the control's max/min rather than accepted verbatim or rejected outright
   — matching `continuous_clamps_above_range`/`continuous_clamps_below_range`'s automated behaviour,
   now confirmed through the real widget.
5. **Non-numeric typed input.** Enter edit mode on a continuous control and type non-numeric text
   (e.g. `loud`). Confirm on commit the edit is rejected — the value reverts to what it was before
   the edit began, rather than committing garbage or crashing the control.
6. **Escape/focus-loss behaviour.** Enter edit mode, type a new value, then press Escape (or click
   elsewhere without pressing Enter, per whatever `DragValue`'s actual behaviour turns out to be).
   Confirm the in-progress edit is discarded rather than silently committed — record what actually
   happens if it differs from this expectation, since this is `DragValue`'s own upstream behaviour
   and has not previously been observed against a real Namir control.

## Executed run (this session)

**Not executed.** This agent session has no way to interact with a real window (click, type, read
back a rendered value) — only to run processes and read stdout/exit codes (see
`fr-ui-010-standalone-window-renders.md`'s and `fr-ui-030-accessibility-script.md`'s own notes on
the same limitation). What *is* verified by automated test, and stands in for the parsing logic
underlying steps 3–5 here: `crates/namir-ui/src/format.rs`'s `parse_value` tests prove named-value
matching, raw-index parsing, range clamping, and rejection of non-numeric/NaN/infinity text all
function correctly as pure functions. What those tests do not and cannot cover: the display half
actually painting on screen, `DragValue`'s click-to-edit/Enter-to-commit/Escape-to-cancel UX against
a real control, and whether a real OS text-input event reaches `custom_parser` the same way a
synthetic call does.

**Result: NOT EXECUTED this session — script above is ready to run by a person with a display and
keyboard against a real `namir-ui` window.**
