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

1. **Numeric display on demand.** For a continuous control (e.g. Input Level) and a stepped one
   (e.g. Gate Enabled), confirm the control shows its current value as text next to/inside the
   control at all times, not only while being interacted with — no click or hover should be
   required to see the current value.
2. **Typed entry, continuous control.** Click Input Level's value to enter edit mode. Confirm a text
   cursor appears and the field is editable. Type `6.0` and press Enter. Confirm the displayed value
   updates to `6.0` (or its formatted equivalent, e.g. `6.0 dB`) and the control's audible/visual
   effect (if monitoring audio) matches a +6 dB trim.
3. **Typed entry, stepped control.** Click Gate Enabled's value to enter edit mode. Type `off` and
   press Enter. Confirm it resolves to the "Off" state (case-insensitive name matching). Repeat,
   typing a raw index (`1`) instead of a name; confirm it resolves to the corresponding named state
   ("On").
4. **Out-of-range typed value.** Enter edit mode on a continuous control with a bounded range (e.g.
   Input Level, ±24 dB) and type a value outside that range (e.g. `999`). Confirm on commit (Enter)
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

## Executed run

**Executed 2026-09-08** by a human on the §2 reference machine (AMD Ryzen 9 5950X, Windows 11 Pro
build 26200) with a display and keyboard, against a live `namir-app` standalone window with an
AudioBox 22VSL audio interface. **All six steps pass.**

| Step | Description | Verdict |
|---|---|---|
| 1 | Numeric display on demand (Input Level, Gate Enabled) | PASS — current numeric values visible at all times without click/hover |
| 2 | Typed entry, continuous control (Input Level `6.0`) | PASS — updates to `+6.0 dB`, applying +6 dB gain to incoming signal |
| 3 | Typed entry, stepped control (Gate Enabled `off` / `1`) | PASS — resolves case-insensitively to "Off", and index `1` to "On" |
| 4 | Out-of-range typed value (Input Level `999`) | PASS — clamped cleanly to max range (`+24.0 dB`) |
| 5 | Non-numeric typed input (Input Level `loud`) | PASS — rejected; value reverted to previous setting |
| 6 | Escape/focus-loss cancellation (Input Level `12.0` + Escape) | PASS — in-progress edit discarded and reverted to previous value |

**Result: PASS, 2026-09-08.** All six steps executed against real controls in a visible window.

### Supplementary headless driver coverage (2026-09-06, issue #143)

Supplementary automated headless tests in `crates/namir-ui/tests/ui_interaction_scripts.rs` drive
real widget layout and interaction via synthetic `RawInput` events through `egui::Context::run_ui`:
- `numeric_display_shows_formatted_values_without_interaction`: verifies continuous and stepped
  controls display their formatted values numerically without requiring click or hover.
- `numeric_value_entry_via_keyboard_updates_continuous_parameter`: exercises click-to-edit, typing
  `6.0`, pressing Enter, and verifies `SetParam` dispatch.
- `numeric_value_entry_via_keyboard_updates_stepped_parameter`: exercises typing state names ("off")
  and raw indices ("1") on stepped controls.
- `numeric_value_entry_clamps_out_of_range_inputs`: exercises typing out-of-range values (e.g. `999`)
  and verifies clamping at the widget boundary.
- `numeric_value_entry_rejects_non_numeric_input`: exercises typing non-numeric text (`loud`) and
  verifies the parameter value is preserved.
- `numeric_value_entry_escape_key_cancels_in_progress_edit`: exercises pressing Escape during text
  editing and verifies the in-progress edit is discarded.
