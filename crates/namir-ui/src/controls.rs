//! [`param_control`]: the one widget every `namir_params::REGISTRY` entry is rendered through,
//! whatever its [`ParamKind`] -- FR-UI-030/040/050 are implemented once, here, rather than once
//! per stage's section, so every control gets them uniformly.
//!
//! # Why one widget for both [`ParamKind::Continuous`] and [`ParamKind::Stepped`]
//!
//! Both kinds are rendered as an `egui::DragValue`: a `Continuous` descriptor's own `min..=max`
//! range, or a `Stepped` descriptor's `0..=values.len() - 1` step-index range. `DragValue`'s
//! `custom_formatter`/`custom_parser` hooks are handed `ParamDescriptor::format_value` and
//! [`crate::format::parse_value`] respectively, so a stepped control still *displays* its named
//! value ("On", not "1") while still satisfying FR-UI-040's "accept a typed numeric value" (typing
//! a name or a raw index both work -- see `format.rs`). This also means every control gets
//! `DragValue`'s own click-to-edit and drag-to-adjust behaviour for free, rather than this crate
//! reimplementing a text field plus a slider plus a stepper for each kind separately.
//!
//! # FR-UI-050's two documented gestures
//!
//! - **Reset to default:** double-click the control's *name label* (not the value itself, so it
//!   never conflicts with `DragValue`'s own single-click-to-edit / double-click-to-select-all
//!   behaviour on the value). The name label and the value control share one `egui::Id` via
//!   [`egui::Response::labelled_by`], which is also what gives the control its FR-UI-030
//!   accessible name -- the label *is* the control's name, not decoration next to it.
//! - **Fine adjustment:** hold Shift while dragging the value. This is `egui::DragValue`'s own
//!   built-in behaviour (it reduces the drag speed and increases the displayed precision while
//!   Shift is held) -- deliberately not reimplemented here, just relied upon and documented.
//!
//! Both gestures are also stated in the control's hover tooltip, so they are discoverable from
//! inside the running application and not only from this doc comment (FR-UI-050 requires them
//! "documented", which this crate reads as: written down somewhere a user or tester can actually
//! find, not merely present in source).
//!
//! # FR-UI-030: accessible name and keyboard operation
//!
//! `response.labelled_by(label_id)` sets the value control's accesskit label to the name label's
//! text, which is the mechanism `egui` itself uses for FR-UI-030-style association; `DragValue` is
//! natively keyboard-operable once focused (arrow keys step the value, typing enters edit mode).
//! **Honest gap:** `egui-baseview` 0.6 (the version this crate is pinned to, matching
//! `spikes/s3-egui-baseview`'s own `Cargo.lock`) does not itself wire `egui`'s accesskit tree to a
//! platform screen reader -- the accessible name is real at the `egui`/accesskit level (a future
//! platform adapter would see it correctly) but not yet forwarded to Windows' actual accessibility
//! API through this dependency stack. Recorded in `docs/manual-tests/fr-ui-030-accessibility-script.md`
//! rather than glossed over.

use egui::{DragValue, Label, Response, Sense, Ui};
use namir_params::{ParamDescriptor, ParamKind};

use crate::UiIntent;
use crate::format::parse_value;

/// Renders one control for `descriptor` at `current`, appending at most one [`UiIntent`] to
/// `intents` if the user changed or reset it this frame. See this module's doc comment for the
/// gestures and accessibility mechanism every call gets.
pub fn param_control(
    ui: &mut Ui,
    descriptor: &'static ParamDescriptor,
    current: f32,
    intents: &mut Vec<UiIntent>,
) {
    let (min, max, speed, default) = control_range(descriptor);
    let default_text = descriptor.format_value(default as f32);

    ui.horizontal(|ui| {
        let label = add_name_label(ui, descriptor.name, &default_text);
        if label.double_clicked() {
            intents.push(UiIntent::ResetParamToDefault {
                key: descriptor.key,
            });
        }

        let mut value = f64::from(current);
        let response: Response = ui
            .add(
                DragValue::new(&mut value)
                    .range(min..=max)
                    .speed(speed)
                    .custom_formatter(move |v, _| descriptor.format_value(v as f32))
                    .custom_parser(move |text| parse_value(descriptor, text)),
            )
            .labelled_by(label.id);

        if response.changed() {
            intents.push(UiIntent::SetParam {
                key: descriptor.key,
                value: value as f32,
            });
        }
    });
}

/// Adds a control's name label, sensing clicks so FR-UI-050's reset gesture (double-click) can be
/// detected on it, and carrying the hover text that documents both FR-UI-050 gestures in-app plus
/// `default_text` (the control's default value, already formatted per its own
/// `ParamDescriptor::format_value`). Split out from [`param_control`] so a test can drive exactly
/// this interaction in isolation, without needing to know the rest of the row's layout.
fn add_name_label(ui: &mut Ui, name: &str, default_text: &str) -> Response {
    ui.add(Label::new(name).sense(Sense::click()))
        .on_hover_text(format!(
            "Default: {default_text}. Double-click to reset to it. Hold Shift while dragging the \
         value for fine adjustment."
        ))
}

/// `(min, max, drag speed, default)`, all in the control's own display space (a step index for
/// [`ParamKind::Stepped`], the physical value for [`ParamKind::Continuous`]). Speed is chosen so a
/// full-range drag takes a comfortable few hundred pixels for a continuous control, and exactly
/// one pixel-equivalent per step for a stepped one (an on/off control shouldn't need much drag
/// distance to flip).
fn control_range(descriptor: &ParamDescriptor) -> (f64, f64, f64, f64) {
    match descriptor.kind {
        ParamKind::Continuous { min, max, default } => {
            let span = (max - min) as f64;
            let speed = (span / 200.0).max(f64::EPSILON);
            (min as f64, max as f64, speed, default as f64)
        }
        ParamKind::Stepped {
            values,
            default_index,
        } => {
            let max_index = values.len().saturating_sub(1) as f64;
            (0.0, max_index, 1.0, default_index.0 as f64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UiIntent;
    use namir_params::stages::{gate, trim};

    /// A fixed-size headless frame's `RawInput`, `events` swapped in per call. No window, no GPU
    /// -- `Context::run_ui` is the same headless entry point `egui-baseview` itself calls every
    /// frame (`egui-baseview-0.6.0/src/window.rs`), so this exercises real `egui`
    /// widget/interaction logic, not a mock of it.
    fn frame_input(time: f64, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            time: Some(time),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn click_events(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    /// FR-UI-050's reset gesture, exercised through the exact widget [`param_control`] uses
    /// ([`add_name_label`]): a first click (frame 1) followed by a second click at the same point
    /// well within `egui`'s default double-click window (frame 2) must report
    /// `double_clicked() == true` only on the second frame -- the same condition
    /// [`param_control`] gates `UiIntent::ResetParamToDefault` on.
    #[test]
    fn double_clicking_the_name_label_is_detected_only_on_the_second_click() {
        let ctx = egui::Context::default();

        // Frame 0: discover exactly where the label lands, with no pointer interaction yet -- a
        // freshly created `Context`'s root `Ui` starts laying out at a fixed, deterministic
        // position, so this position is stable across every subsequent frame that runs the same
        // widget code first.
        let mut label_rect = None;
        let _ = ctx.run_ui(frame_input(0.0, Vec::new()), |ui| {
            label_rect = Some(add_name_label(ui, "Input Trim", "0.0").rect);
        });
        let pos = label_rect.expect("label laid out in frame 0").center();

        // Frame 1: first click. Not yet a double-click.
        let mut first_double_clicked = None;
        let _ = ctx.run_ui(frame_input(0.0, click_events(pos)), |ui| {
            first_double_clicked = Some(add_name_label(ui, "Input Trim", "0.0").double_clicked());
        });
        assert_eq!(
            first_double_clicked,
            Some(false),
            "a single click must not register as a double-click"
        );

        // Frame 2: second click, well inside egui's default 0.3s double-click window.
        let mut second_double_clicked = None;
        let _ = ctx.run_ui(frame_input(0.05, click_events(pos)), |ui| {
            second_double_clicked = Some(add_name_label(ui, "Input Trim", "0.0").double_clicked());
        });
        assert_eq!(
            second_double_clicked,
            Some(true),
            "a second click within the double-click window must register as a double-click"
        );
    }

    /// The same gesture, now asserted through the full [`param_control`] widget: a double-click
    /// on the label must be the only path to `ResetParamToDefault`, and it must not also emit a
    /// spurious `SetParam` for the same frame.
    // trace: FR-UI-050
    #[test]
    fn double_clicking_param_controls_label_emits_exactly_a_reset_intent() {
        let ctx = egui::Context::default();

        // `param_control` lays out its label via `add_name_label` as the first thing it does in
        // a fresh horizontal row starting at the outer `Ui`'s current cursor -- the same starting
        // position `add_name_label` itself lands at when called directly at the top of a fresh
        // frame, so discovering the position this way finds the exact rect `param_control`'s own
        // label occupies.
        let mut label_rect = None;
        let _ = ctx.run_ui(frame_input(0.0, Vec::new()), |ui| {
            label_rect = Some(add_name_label(ui, trim::GAIN_DB.name, "0.0").rect);
        });
        let pos = label_rect.expect("label laid out").center();

        // First click: primes the double-click timer, must not itself reset anything.
        let mut intents = Vec::new();
        let _ = ctx.run_ui(frame_input(0.0, click_events(pos)), |ui| {
            param_control(ui, &trim::GAIN_DB, 6.0, &mut intents);
        });
        assert!(
            intents.is_empty(),
            "a single click must not emit any intent, got {intents:?}"
        );

        // Second click: the double-click.
        let mut intents = Vec::new();
        let _ = ctx.run_ui(frame_input(0.05, click_events(pos)), |ui| {
            param_control(ui, &trim::GAIN_DB, 6.0, &mut intents);
        });
        assert_eq!(
            intents,
            vec![UiIntent::ResetParamToDefault {
                key: trim::GAIN_DB.key
            }],
            "a double-click on the label must emit exactly one ResetParamToDefault intent"
        );
    }

    #[test]
    fn control_range_for_continuous_matches_its_descriptor() {
        let (min, max, _speed, default) = control_range(&trim::GAIN_DB);
        assert_eq!(min, -24.0);
        assert_eq!(max, 24.0);
        assert_eq!(default, 0.0);
    }

    #[test]
    fn control_range_for_stepped_spans_its_step_indices() {
        let (min, max, speed, default) = control_range(&gate::ENABLED);
        assert_eq!(min, 0.0);
        assert_eq!(max, 1.0); // "Off"/"On" -- two values, indices 0..=1.
        assert_eq!(speed, 1.0);
        assert_eq!(default, 1.0); // gate.enabled defaults on.
    }
}
