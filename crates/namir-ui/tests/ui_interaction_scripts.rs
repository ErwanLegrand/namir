// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Erwan Patrick Legrand

//! Automated headless execution of manual UI interaction scripts:
//! - FR-UI-040: Numeric value display, typed entry (continuous and stepped), range clamping,
//!   non-numeric rejection, and cancel/escape behavior.
//! - FR-UI-050: Reset to default on double-click gesture (label only), fine adjustment
//!   on Shift+drag, and gesture discoverability.
//! - FR-UI-030: Keyboard operability and accessible name associations.
//! - FR-UI-020 / FR-UI-070: Single screen layout and non-modal notice dismissal.

use egui::{Event, FullOutput, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Shape, vec2};
use namir_core::{ErrorCode, Severity};
use namir_params::stages::{gate, trim};
use namir_state::ParamValues;
use namir_ui::{NamirUi, UiHost, UiIntent, UiNotice, UiSnapshot};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Default, Clone)]
struct TestHostState {
    snapshot: UiSnapshot,
    dispatched: Vec<UiIntent>,
}

struct TestHost {
    state: Arc<Mutex<TestHostState>>,
}

impl TestHost {
    fn new(snapshot: UiSnapshot) -> (Self, Arc<Mutex<TestHostState>>) {
        let state = Arc::new(Mutex::new(TestHostState {
            snapshot,
            dispatched: Vec::new(),
        }));
        (
            Self {
                state: Arc::clone(&state),
            },
            state,
        )
    }
}

impl UiHost for TestHost {
    fn snapshot(&mut self) -> UiSnapshot {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .clone()
    }

    fn dispatch(&mut self, intent: UiIntent) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &intent {
            UiIntent::SetParam { key, value } => {
                let _ = state.snapshot.params.set(key, *value);
            }
            UiIntent::ResetParamToDefault { key } => {
                let defaults = ParamValues::defaults();
                if let Some(def) = defaults.get(key) {
                    let _ = state.snapshot.params.set(key, def);
                }
            }
            UiIntent::DismissNotice { id } => {
                state.snapshot.notices.retain(|n| n.id != *id);
            }
            _ => {}
        }
        state.dispatched.push(intent);
    }
}

struct HeadlessUiDriver {
    ui: NamirUi<TestHost>,
    state: Arc<Mutex<TestHostState>>,
    ctx: egui::Context,
    time: f64,
}

impl HeadlessUiDriver {
    fn new(snapshot: UiSnapshot) -> Self {
        let (host, state) = TestHost::new(snapshot);
        Self {
            ui: NamirUi::new(host),
            state,
            ctx: egui::Context::default(),
            time: 0.0,
        }
    }

    fn frame(&mut self, events: Vec<Event>) -> FullOutput {
        self.frame_with_modifiers(events, Modifiers::NONE)
    }

    fn frame_with_modifiers(&mut self, mut events: Vec<Event>, modifiers: Modifiers) -> FullOutput {
        self.time += 0.1;
        let time = self.time;
        let ui = &mut self.ui;
        events.insert(0, Event::ModifiersChanged(modifiers));
        let mut output = self.ctx.run_ui(
            RawInput {
                time: Some(time),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(960.0, 640.0))),
                events,
                ..Default::default()
            },
            |u| ui.frame(u),
        );
        output.textures_delta.clear();
        output
    }

    /// One frame whose first pass calls `Context::request_discard`, so `egui`'s multi-pass loop
    /// (`Context::run_dyn`) re-runs the entire UI a second time *within the same frame*.
    fn frame_with_discard(&mut self, events: Vec<Event>) -> FullOutput {
        self.time += 0.1;
        let time = self.time;
        let ui = &mut self.ui;
        let mut pass = 0_u32;
        let mut output = self.ctx.run_ui(
            RawInput {
                time: Some(time),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(960.0, 640.0))),
                events,
                ..Default::default()
            },
            |u| {
                pass += 1;
                if pass == 1 {
                    u.ctx().request_discard("test: force a second pass");
                }
                ui.frame(u);
            },
        );
        assert_eq!(pass, 2, "request_discard must have produced a second pass");
        output.textures_delta.clear();
        output
    }

    fn painted_texts(output: &FullOutput) -> Vec<(String, Rect)> {
        fn walk(shape: &Shape, out: &mut Vec<(String, Rect)>) {
            match shape {
                Shape::Text(text) => {
                    out.push((text.galley.text().to_string(), text.visual_bounding_rect()));
                }
                Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }
        texts
    }

    fn find_text(output: &FullOutput, needle: &str) -> Option<Rect> {
        Self::painted_texts(output)
            .into_iter()
            .find(|(text, _)| text == needle)
            .map(|(_, rect)| rect)
    }

    fn locate(&mut self, needle: &str) -> Rect {
        self.frame(Vec::new());
        let output = self.frame(Vec::new());
        Self::find_text(&output, needle).unwrap_or_else(|| {
            panic!(
                "nothing painting {needle:?} is on screen; painted: {:?}",
                Self::painted_texts(&output)
                    .into_iter()
                    .map(|(text, _)| text)
                    .collect::<Vec<_>>()
            )
        })
    }

    fn locate_label(&mut self, label_name: &str) -> Rect {
        self.frame(Vec::new());
        let output = self.frame(Vec::new());
        let texts = Self::painted_texts(&output);
        let matching_labels: Vec<_> = texts
            .iter()
            .filter(|(text, _)| text == label_name)
            .collect();
        for (_, label_rect) in matching_labels {
            let has_adjacent_control = texts.iter().any(|(text, rect)| {
                rect.min.x >= label_rect.max.x
                    && (rect.center().y - label_rect.center().y).abs() < 15.0
                    && text.as_str() != label_name
            });
            if has_adjacent_control {
                return *label_rect;
            }
        }
        panic!("label {label_name:?} with adjacent control not found");
    }

    fn locate_value_for_control(&mut self, label_name: &str) -> (String, Rect) {
        self.frame(Vec::new());
        let output = self.frame(Vec::new());
        let texts = Self::painted_texts(&output);
        let matching_labels: Vec<_> = texts
            .iter()
            .filter(|(text, _)| text == label_name)
            .collect();
        for (_, label_rect) in matching_labels {
            let candidate = texts
                .iter()
                .filter(|(text, rect)| {
                    rect.min.x >= label_rect.max.x
                        && (rect.center().y - label_rect.center().y).abs() < 15.0
                        && text.as_str() != label_name
                })
                .min_by(|(_, a), (_, b)| a.min.x.partial_cmp(&b.min.x).unwrap());
            if let Some((v_text, v_rect)) = candidate {
                return (v_text.clone(), *v_rect);
            }
        }
        panic!("value for label {label_name:?} not found among texts: {texts:?}");
    }

    fn click_at(&mut self, pos: Pos2) {
        self.frame(vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ]);
    }

    fn double_click_at(&mut self, pos: Pos2) {
        self.frame(vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ]);
        self.time += 0.05;
        let time = self.time;
        let ui = &mut self.ui;
        let _ = self.ctx.run_ui(
            RawInput {
                time: Some(time),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(960.0, 640.0))),
                events: vec![
                    Event::PointerMoved(pos),
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |u| ui.frame(u),
        );
    }

    fn drag_at(&mut self, pos: Pos2, delta: egui::Vec2, modifiers: Modifiers) {
        self.frame_with_modifiers(
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers,
                },
            ],
            modifiers,
        );
        let moved = pos + delta;
        self.frame_with_modifiers(vec![Event::PointerMoved(moved)], modifiers);
        self.frame_with_modifiers(
            vec![Event::PointerButton {
                pos: moved,
                button: PointerButton::Primary,
                pressed: false,
                modifiers,
            }],
            modifiers,
        );
    }

    fn type_into_control_value(&mut self, label_name: &str, new_text: &str) {
        let (_, rect) = self.locate_value_for_control(label_name);
        self.click_at(rect.center());
        // Frame 1: Select all and replace with new_text
        self.frame(vec![
            Event::Key {
                key: Key::A,
                pressed: true,
                modifiers: Modifiers::COMMAND,
                repeat: false,
                physical_key: None,
            },
            Event::Text(new_text.to_string()),
        ]);
        // Frame 2: Press Enter to commit edit
        self.frame(vec![Event::Key {
            key: Key::Enter,
            pressed: true,
            modifiers: Modifiers::NONE,
            repeat: false,
            physical_key: None,
        }]);
        // Frame 3: Empty frame to settle post-focus-loss state
        self.frame(vec![]);
    }

    fn type_and_escape_control_value(&mut self, label_name: &str, new_text: &str) {
        let (_, rect) = self.locate_value_for_control(label_name);
        self.click_at(rect.center());
        // Frame 1: Select all and replace with new_text
        self.frame(vec![
            Event::Key {
                key: Key::A,
                pressed: true,
                modifiers: Modifiers::COMMAND,
                repeat: false,
                physical_key: None,
            },
            Event::Text(new_text.to_string()),
        ]);
        // Frame 2: Press Escape to cancel edit
        self.frame(vec![Event::Key {
            key: Key::Escape,
            pressed: true,
            modifiers: Modifiers::NONE,
            repeat: false,
            physical_key: None,
        }]);
        // Frame 3: Empty frame to verify post-focus-loss state
        self.frame(vec![]);
    }

    fn current_param(&self, key: &'static str) -> f32 {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .params
            .get(key)
            .expect("valid param key")
    }

    fn dispatched_intents(&self) -> Vec<UiIntent> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dispatched
            .clone()
    }

    fn clear_dispatched(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dispatched
            .clear();
    }
}

// ---------------------------------------------------------------------------
// FR-UI-040: Numeric value display and typed entry
// ---------------------------------------------------------------------------

#[test]
fn numeric_display_shows_formatted_values_without_interaction() {
    let mut driver = HeadlessUiDriver::new(UiSnapshot::default());
    // Continuous control displays value text without interaction
    let (trim_text, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(trim_text, "0.0", "Input Level displays 0.0 by default");

    let (gate_thresh_text, _) = driver.locate_value_for_control("Gate Threshold");
    assert_eq!(
        gate_thresh_text, "-70.0",
        "Gate Threshold displays -70.0 by default"
    );

    // Stepped control displays named value text without interaction
    let (gate_en_text, _) = driver.locate_value_for_control("Gate Enabled");
    assert_eq!(gate_en_text, "On", "Gate Enabled displays On by default");

    let (dc_block_text, _) = driver.locate_value_for_control("DC Blocker");
    assert_eq!(dc_block_text, "On", "DC Blocker displays On by default");
}

#[test]
fn numeric_value_entry_via_keyboard_updates_continuous_parameter() {
    let mut driver = HeadlessUiDriver::new(UiSnapshot::default());
    driver.type_into_control_value("Input Level", "6.0");
    let intents = driver.dispatched_intents();
    assert_eq!(
        intents,
        vec![UiIntent::SetParam {
            key: trim::GAIN_DB.key,
            value: 6.0,
        }],
        "typing 6.0 into Input Level must dispatch SetParam 6.0"
    );

    let (updated_text, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(updated_text, "6.0");
}

#[test]
fn numeric_value_entry_via_keyboard_updates_stepped_parameter() {
    let mut driver = HeadlessUiDriver::new(UiSnapshot::default());

    // Type name "off"
    driver.type_into_control_value("Gate Enabled", "off");
    assert_eq!(
        driver.dispatched_intents(),
        vec![UiIntent::SetParam {
            key: gate::ENABLED.key,
            value: 0.0,
        }],
        "typing 'off' into Gate Enabled must resolve to Off (0.0)"
    );
    let (text, _) = driver.locate_value_for_control("Gate Enabled");
    assert_eq!(text, "Off");

    driver.clear_dispatched();

    // Type raw index "1"
    driver.type_into_control_value("Gate Enabled", "1");
    assert_eq!(
        driver.dispatched_intents(),
        vec![UiIntent::SetParam {
            key: gate::ENABLED.key,
            value: 1.0,
        }],
        "typing '1' into Gate Enabled must resolve to On (1.0)"
    );
    let (text, _) = driver.locate_value_for_control("Gate Enabled");
    assert_eq!(text, "On");
}

#[test]
fn numeric_value_entry_clamps_out_of_range_inputs() {
    let mut driver = HeadlessUiDriver::new(UiSnapshot::default());

    // Input trim range is -24.0..=+24.0. Type 999.0 -> clamped to 24.0
    driver.type_into_control_value("Input Level", "999");
    assert_eq!(
        driver.dispatched_intents(),
        vec![UiIntent::SetParam {
            key: trim::GAIN_DB.key,
            value: 24.0,
        }],
        "typing 999 must clamp to maximum 24.0"
    );
    let (text, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(text, "24.0");

    driver.clear_dispatched();

    // Type -999.0 -> clamped to -24.0
    driver.type_into_control_value("Input Level", "-999");
    assert_eq!(
        driver.dispatched_intents(),
        vec![UiIntent::SetParam {
            key: trim::GAIN_DB.key,
            value: -24.0,
        }],
        "typing -999 must clamp to minimum -24.0"
    );
    let (text, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(text, "-24.0");
}

#[test]
fn numeric_value_entry_rejects_non_numeric_input() {
    let mut params = ParamValues::defaults();
    params.set(trim::GAIN_DB.key, 6.0).unwrap();
    let mut driver = HeadlessUiDriver::new(UiSnapshot {
        params,
        ..Default::default()
    });

    let (initial_text, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(initial_text, "6.0");

    driver.type_into_control_value("Input Level", "loud");
    // Value remains 6.0 and was not modified to invalid state
    assert_eq!(driver.current_param(trim::GAIN_DB.key), 6.0);

    let (text, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(text, "6.0", "value must remain at previous value 6.0");
}

#[test]
fn numeric_value_entry_escape_key_cancels_in_progress_edit() {
    let mut params = ParamValues::defaults();
    params.set(trim::GAIN_DB.key, 6.0).unwrap();
    let mut driver = HeadlessUiDriver::new(UiSnapshot {
        params,
        ..Default::default()
    });

    driver.type_and_escape_control_value("Input Level", "12.0");
    assert_eq!(
        driver.current_param(trim::GAIN_DB.key),
        6.0,
        "parameter value must remain at pre-edit value immediately after Escape"
    );

    let (text, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(text, "6.0");

    // Advance additional frames to verify no late commit occurs on lost focus (e.g. frame after Escape)
    driver.frame(vec![]);
    driver.frame(vec![]);

    assert_eq!(
        driver.current_param(trim::GAIN_DB.key),
        6.0,
        "parameter value must remain at pre-edit value across subsequent frames"
    );

    let (text_after, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(text_after, "6.0");

    let intents = driver.dispatched_intents();
    assert!(
        intents.is_empty(),
        "no SetParam intent with aborted value must be dispatched, got: {intents:?}"
    );
}

/// The Escape frame is allowed to run more than one `egui` pass (anything in the UI may call
/// `Context::request_discard`), and `param_control`'s cancellation stamp is scoped to a *pass*,
/// not a frame. This pins the observed pairing that makes that correct: with two passes on the
/// Escape frame, `DragValue`'s focus-loss re-commit lands on pass N+1 of the *same* frame, which
/// is exactly the pass the stamp suppresses. A frame-scoped stamp would miss it and dispatch the
/// cancelled 12.0.
#[test]
fn escape_cancellation_survives_a_multi_pass_frame() {
    let mut params = ParamValues::defaults();
    params.set(trim::GAIN_DB.key, 6.0).unwrap();
    let mut driver = HeadlessUiDriver::new(UiSnapshot {
        params,
        ..Default::default()
    });

    let (_, rect) = driver.locate_value_for_control("Input Level");
    driver.click_at(rect.center());
    driver.frame(vec![
        Event::Key {
            key: Key::A,
            pressed: true,
            modifiers: Modifiers::COMMAND,
            repeat: false,
            physical_key: None,
        },
        Event::Text("12.0".to_string()),
    ]);

    // Escape, on a frame that egui re-runs a second time.
    driver.frame_with_discard(vec![Event::Key {
        key: Key::Escape,
        pressed: true,
        modifiers: Modifiers::NONE,
        repeat: false,
        physical_key: None,
    }]);
    driver.frame(vec![]);
    driver.frame(vec![]);

    assert_eq!(
        driver.current_param(trim::GAIN_DB.key),
        6.0,
        "a discarded pass on the Escape frame must not let the cancelled edit commit"
    );
    let intents = driver.dispatched_intents();
    assert!(
        intents.is_empty(),
        "no intent must be dispatched for a cancelled edit, got: {intents:?}"
    );
}

#[test]
fn escape_cancellation_in_one_control_preserves_staged_edit_in_another() {
    let mut params = ParamValues::defaults();
    params.set(trim::GAIN_DB.key, 6.0).unwrap();
    params
        .set(namir_params::stages::gate::THRESHOLD_DB.key, -70.0)
        .unwrap();
    let mut driver = HeadlessUiDriver::new(UiSnapshot {
        params,
        ..Default::default()
    });

    // Begin edit in Control A ("Input Level"): focus and type staged value without Enter
    let (_, rect_a) = driver.locate_value_for_control("Input Level");
    driver.click_at(rect_a.center());
    driver.frame(vec![
        Event::Key {
            key: Key::A,
            pressed: true,
            modifiers: Modifiers::COMMAND,
            repeat: false,
            physical_key: None,
        },
        Event::Text("12.0".to_string()),
    ]);

    // Click into Control B ("Gate Threshold"), type and cancel with Escape
    driver.type_and_escape_control_value("Gate Threshold", "-50.0");

    // Gate Threshold was escaped: its value must remain -70.0, not -50.0
    assert_eq!(
        driver.current_param(namir_params::stages::gate::THRESHOLD_DB.key),
        -70.0,
        "escaped control must remain at its initial value"
    );

    // Control A was not cancelled: clicking back into it and pressing Enter should commit its 12.0 edit
    let (_, rect_a_again) = driver.locate_value_for_control("Input Level");
    driver.click_at(rect_a_again.center());
    driver.frame(vec![Event::Key {
        key: Key::Enter,
        pressed: true,
        modifiers: Modifiers::NONE,
        repeat: false,
        physical_key: None,
    }]);
    driver.frame(vec![]);

    assert_eq!(
        driver.current_param(trim::GAIN_DB.key),
        12.0,
        "control A edit must not have been wiped by escape in control B"
    );
}

// ---------------------------------------------------------------------------
// FR-UI-050: Reset and fine adjust gestures
// ---------------------------------------------------------------------------

#[test]
fn reset_gesture_double_clicking_label_restores_continuous_default() {
    let mut params = ParamValues::defaults();
    params.set(trim::GAIN_DB.key, 12.0).unwrap();
    let mut driver = HeadlessUiDriver::new(UiSnapshot {
        params,
        ..Default::default()
    });

    let (initial_text, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(initial_text, "12.0");

    let label_rect = driver.locate_label("Input Level");

    // Single click does not reset
    driver.click_at(label_rect.center());
    assert!(
        driver.dispatched_intents().is_empty(),
        "single click on label must not reset"
    );

    // Double click resets to default
    driver.double_click_at(label_rect.center());
    assert_eq!(
        driver.dispatched_intents(),
        vec![UiIntent::ResetParamToDefault {
            key: trim::GAIN_DB.key,
        }],
        "double clicking label must emit ResetParamToDefault"
    );
    let (text_after, _) = driver.locate_value_for_control("Input Level");
    assert_eq!(text_after, "0.0");
}

#[test]
fn reset_gesture_double_clicking_label_restores_stepped_default() {
    let mut params = ParamValues::defaults();
    // Default is 1.0 (On), set to 0.0 (Off)
    params.set(gate::ENABLED.key, 0.0).unwrap();
    let mut driver = HeadlessUiDriver::new(UiSnapshot {
        params,
        ..Default::default()
    });

    let (initial_text, _) = driver.locate_value_for_control("Gate Enabled");
    assert_eq!(initial_text, "Off");

    let label_rect = driver.locate_label("Gate Enabled");
    driver.double_click_at(label_rect.center());
    assert_eq!(
        driver.dispatched_intents(),
        vec![UiIntent::ResetParamToDefault {
            key: gate::ENABLED.key,
        }],
        "double clicking Gate Enabled label must emit ResetParamToDefault"
    );
    let (text_after, _) = driver.locate_value_for_control("Gate Enabled");
    assert_eq!(text_after, "On");
}

#[test]
fn reset_gesture_double_clicking_value_does_not_reset_parameter() {
    let mut params = ParamValues::defaults();
    params.set(trim::GAIN_DB.key, 12.0).unwrap();
    let mut driver = HeadlessUiDriver::new(UiSnapshot {
        params,
        ..Default::default()
    });

    let (_, val_rect) = driver.locate_value_for_control("Input Level");
    driver.double_click_at(val_rect.center());
    let reset_intents: Vec<_> = driver
        .dispatched_intents()
        .into_iter()
        .filter(|i| matches!(i, UiIntent::ResetParamToDefault { .. }))
        .collect();
    assert!(
        reset_intents.is_empty(),
        "double clicking on value itself must not trigger ResetParamToDefault"
    );
}

#[test]
fn reset_and_fine_adjust_shift_drag_scales_increments() {
    // Standard drag without Shift
    let mut driver1 = HeadlessUiDriver::new(UiSnapshot::default());
    let (_, rect1) = driver1.locate_value_for_control("Input Level");
    driver1.drag_at(rect1.center(), vec2(50.0, 0.0), Modifiers::NONE);
    let normal_dispatched = driver1.dispatched_intents();
    assert_eq!(normal_dispatched.len(), 1);
    let UiIntent::SetParam {
        value: normal_val, ..
    } = normal_dispatched[0]
    else {
        panic!("expected SetParam");
    };

    // Drag with Shift modifier
    let mut driver2 = HeadlessUiDriver::new(UiSnapshot::default());
    let (_, rect2) = driver2.locate_value_for_control("Input Level");
    driver2.drag_at(rect2.center(), vec2(50.0, 0.0), Modifiers::SHIFT);
    let shift_dispatched = driver2.dispatched_intents();
    assert_eq!(shift_dispatched.len(), 1);
    let UiIntent::SetParam {
        value: shift_val, ..
    } = shift_dispatched[0]
    else {
        panic!("expected SetParam");
    };

    assert!(
        shift_val > 0.0 && shift_val < normal_val,
        "Shift+drag must produce finer adjustment than standard drag: {shift_val} vs {normal_val}"
    );
}

// ---------------------------------------------------------------------------
// FR-UI-030: Keyboard Operability
// ---------------------------------------------------------------------------

#[test]
fn keyboard_arrow_keys_on_focused_control_adjust_value() {
    let mut driver = HeadlessUiDriver::new(UiSnapshot::default());

    // Focus Input Level by clicking its value control
    let (_, rect) = driver.locate_value_for_control("Input Level");
    driver.click_at(rect.center());
    driver.clear_dispatched();

    // Send ArrowUp key to step its value
    driver.frame(vec![Event::Key {
        key: Key::ArrowUp,
        pressed: true,
        modifiers: Modifiers::NONE,
        repeat: false,
        physical_key: None,
    }]);

    let intents = driver.dispatched_intents();
    assert_eq!(
        intents.len(),
        1,
        "arrow key on focused control must dispatch exactly one SetParam intent"
    );
    let UiIntent::SetParam { key, value } = intents[0] else {
        panic!("expected SetParam intent, got {:?}", intents[0]);
    };
    assert_eq!(key, trim::GAIN_DB.key);
    assert!(
        value > 0.0,
        "ArrowUp on focused control must increment parameter value above default 0.0 (got {value})"
    );
}

// ---------------------------------------------------------------------------
// FR-UI-020 & FR-UI-070: Layout and Non-modal notices
// ---------------------------------------------------------------------------

#[test]
fn single_screen_layout_paints_all_major_sections() {
    let mut driver = HeadlessUiDriver::new(UiSnapshot::default());
    // Verify all major controls and section headers exist on one screen
    let sections = [
        "Input Level",
        "Output Level",
        "Gate",
        "Model",
        "Impulse Response",
        "EQ",
        "Preset",
        "Library",
    ];
    for sec in sections {
        assert!(
            driver.locate(sec).width() > 0.0,
            "section {sec} must be present on the single screen layout"
        );
    }
}

const SAMPLE_NOTICE: ErrorCode = ErrorCode::new(
    "ui.test.notice",
    Severity::Warning,
    "Sample notice: {detail}",
    "Check input file",
);

#[test]
fn notice_dismiss_button_dispatches_dismiss_intent() {
    let snapshot = UiSnapshot {
        notices: vec![UiNotice {
            id: 42,
            code: SAMPLE_NOTICE,
            detail: "detail text".to_string(),
        }],
        ..Default::default()
    };
    let mut driver = HeadlessUiDriver::new(snapshot);
    let dismiss_rect = driver.locate("Dismiss");
    driver.click_at(dismiss_rect.center());

    assert_eq!(
        driver.dispatched_intents(),
        vec![UiIntent::DismissNotice { id: 42 }],
        "clicking Dismiss on notice must dispatch DismissNotice with its id"
    );
}
