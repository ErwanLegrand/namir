//! FR-UI-020's single screen, assembled: input meter+trim, gate, the loaded model's name, the
//! loaded IR's name, EQ, output meter+level, and global bypass, all visible without navigation.
//! [`NamirUi`] is the one widget type both product shells construct (FR-UI-010) -- see this
//! crate's top doc comment for why a single type parameterized by *which `open_*` call wraps it*
//! satisfies FR-UI-010 rather than two separate UIs.

use egui::{CentralPanel, Panel, ScrollArea, Ui};
use namir_params::global::{GLOBAL_BYPASS, OUTPUT_CEILING_DB};
use namir_state::ParamValues;

use crate::brand;
use crate::controls::param_control;
use crate::host::{UiHost, UiSnapshot};
use crate::library_view::{self, LibraryViewState};
use crate::notices;
use crate::{UiIntent, meter};

/// Per-window state carried across frames -- everything that is *this crate's own* UI state
/// (never sent to a host, never part of a [`UiSnapshot`]), as opposed to engine/library/preset
/// state, which only ever arrives through a snapshot. Kept separate from [`NamirUi`] itself so
/// [`render`] (the actual FR-UI-020 layout) can be called directly from a test without going
/// through a real `UiHost` or an `egui-baseview` window.
#[derive(Default)]
pub struct ViewState {
    library: LibraryViewState,
    /// What the user has typed into the preset-name box, between the frame they type it and the
    /// frame they press Save. This crate's own transient state -- a half-typed preset name is not
    /// engine state and never reaches a host, which is exactly why it lives here rather than in a
    /// [`UiSnapshot`] field the host would have to echo back every frame.
    preset_name: String,
    /// The brand mark's uploaded texture, `None` until the first frame draws it. Cached here
    /// rather than re-uploaded per frame -- see `brand`'s module doc comment for why this crate's
    /// own view state is the right owner for it.
    brand: Option<egui::TextureHandle>,
}

/// Renders FR-UI-020's whole screen into `ui` (the root `Ui` `egui::Context::run_ui`/
/// `egui-baseview`'s update closure hands over -- panels nest inside it, per this `egui` version's
/// `Panel`/`CentralPanel::show(ui, ...)` API), from `snapshot`, appending every [`UiIntent`] the
/// user triggered this frame to `intents`. Free function (not a method) so it can be driven
/// directly by a test with a bare [`UiSnapshot`], with no [`UiHost`] or window involved.
pub fn render(
    ui: &mut Ui,
    view: &mut ViewState,
    snapshot: &UiSnapshot,
    intents: &mut Vec<UiIntent>,
) {
    Panel::top("namir_ui_top").show(ui, |ui| {
        ui.horizontal(|ui| {
            // FR-UI-110's brand mark, replacing `ui.heading("Namir")`. Drawn at twice a heading
            // row (see `brand::MARK_HEIGHT_IN_HEADINGS`), so the top panel is taller than it was.
            brand::render(ui, &mut view.brand);
            if let Some(mode) = &snapshot.audio_mode {
                ui.label(audio_mode_label(mode)).on_hover_text(
                    "The share mode the audio device is actually open in. Exclusive mode gives \
                     this application sole use of the device; shared mode lets other applications \
                     use it at the same time.",
                );
            }
            if snapshot.unsaved_changes {
                ui.label("* unsaved changes").on_hover_text(
                    "The current settings differ from the last saved/recalled state.",
                );
            }
        });
        preset_controls(ui, snapshot, &mut view.preset_name, intents);
        notices::render(ui, &snapshot.notices, intents);
    });

    Panel::left("namir_ui_library")
        .resizable(true)
        .default_size(300.0)
        .show(ui, |ui| {
            library_view::render(ui, &mut view.library, &snapshot.library, intents);
        });

    CentralPanel::default().show(ui, |ui| {
        ScrollArea::vertical()
            .id_salt("namir_ui_main_scroll")
            .show(ui, |ui| {
                meter::render(ui, "Input", snapshot.input_meter);
                param_section(ui, "Input Trim", "trim.", &snapshot.params, intents);

                param_section(ui, "Gate", "gate.", &snapshot.params, intents);

                ui.heading("Model");
                ui.label(
                    snapshot
                        .loaded_model_name
                        .as_deref()
                        .unwrap_or("(no model loaded)"),
                );
                param_section(ui, "NAM", "nam.", &snapshot.params, intents);

                ui.heading("Impulse Response");
                ui.label(
                    snapshot
                        .loaded_ir_name
                        .as_deref()
                        .unwrap_or("(no IR loaded)"),
                );
                // Heading already drawn above the IR-name label this section belongs to -- see
                // `param_controls`' doc comment (issue #103).
                param_controls(ui, "ir.", &snapshot.params, intents);

                param_section(ui, "EQ", "eq.", &snapshot.params, intents);

                // No `ui.heading("Output")` here: the meter row below is labelled "Output" and
                // both controls under it are named "Output ...", so a heading would be the third
                // "Output" on four consecutive rows -- the same duplication issue #103 reports,
                // with the meter as the element in between. Mirrors the input side, where the
                // "Input" meter likewise stands as its own row above its controls.
                meter::render(ui, "Output", snapshot.output_meter);
                param_controls(ui, "out.", &snapshot.params, intents);
                render_single(ui, &OUTPUT_CEILING_DB, &snapshot.params, intents);

                ui.separator();
                render_single(ui, &GLOBAL_BYPASS, &snapshot.params, intents);
            });
    });
}

/// FR-STATE-030's two controls: name a preset and save it, or pick one the host listed and
/// recall it. Appends [`UiIntent::SavePreset`] / [`UiIntent::RecallPreset`] for whichever the user
/// operated this frame.
///
/// # Why here, and why these two shapes (issue #100)
///
/// FR-STATE-030 is a Must and this crate is the only GUI, so a save and a recall gesture have to
/// exist here or they exist nowhere. Before this row, `UiSnapshot::unsaved_changes` was rendered
/// two labels to the left as "* unsaved changes" and `UiIntent` had no variant that could resolve
/// it: the screen stated a problem and offered no control for it. The row is placed beside that
/// indicator for exactly that reason.
///
/// **Save takes a name; recall takes a path.** Neither is a file dialog, and that asymmetry is
/// D-5.1's, not a shortcut. This crate may not depend on `namir-platform`, so it cannot know where
/// a preset directory is; it therefore hands the host a *name* to place, and can only offer for
/// recall the paths the host itself listed in [`UiSnapshot::presets`]. A host that wants a real
/// file picker can still open one when it receives either intent -- which is also where
/// NFR-PORT-030's "no blocking dialog on an audio-affecting path" has to be honoured, since only
/// the host knows what its own dialog would block.
///
/// The save button is disabled while the box is empty rather than emitting an empty name: "a
/// **named** preset" is the requirement's own wording, and a host handed an empty name could only
/// invent a filename or refuse.
fn preset_controls(
    ui: &mut egui::Ui,
    snapshot: &UiSnapshot,
    preset_name: &mut String,
    intents: &mut Vec<UiIntent>,
) {
    ui.horizontal(|ui| {
        let label = ui
            .add(egui::Label::new("Preset").sense(egui::Sense::hover()))
            .on_hover_text(
                "Save the current settings under a name, or recall one you saved earlier. \
                 Presets are interchangeable between the standalone application and the plugin.",
            );
        let entry = ui
            .add(
                egui::TextEdit::singleline(preset_name)
                    .hint_text("Preset name")
                    .desired_width(160.0),
            )
            .labelled_by(label.id);

        let name = preset_name.trim().to_string();
        // Enter inside the box saves too -- the same gesture the button is, for a user whose
        // hands are already on the keyboard (FR-UI-030's "operable by keyboard").
        let entered = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let save = ui
            .add_enabled(!name.is_empty(), egui::Button::new("Save preset"))
            .on_hover_text("Save the current settings as a preset under the name to the left.");
        if !name.is_empty() && (save.clicked() || entered) {
            intents.push(UiIntent::SavePreset { name });
        }

        let mut recalled: Option<std::path::PathBuf> = None;
        let has_presets = !snapshot.presets.is_empty();
        ui.add_enabled_ui(has_presets, |ui| {
            egui::ComboBox::from_id_salt("namir_ui_preset_recall")
                .selected_text(if has_presets {
                    "Recall preset"
                } else {
                    "No saved presets"
                })
                .show_ui(ui, |ui| {
                    for preset in &snapshot.presets {
                        // `false`: this is a menu of actions, not a selection that persists --
                        // nothing in a snapshot says which preset is "current", and claiming one
                        // was would be the same class of lie `audio_mode_label` refuses to tell.
                        if ui.selectable_label(false, &preset.name).clicked() {
                            recalled = Some(preset.path.clone());
                        }
                    }
                });
        });
        if let Some(path) = recalled {
            intents.push(UiIntent::RecallPreset { path });
        }
    });
}

/// FR-IO-020's mode indicator as one line of text. Deliberately **not** routed through
/// [`param_section`]: a share mode is not a `namir_params::REGISTRY` entry, and
/// `every_registry_key_is_covered_by_a_section_prefix_or_a_named_single_control` (below) pins every
/// registry key to a section prefix — a non-parameter status string belongs beside the
/// "* unsaved changes" label, which is this crate's existing precedent for exactly that.
///
/// Names the granted mode unconditionally, including the ordinary shared case: the roadmap's §18
/// rule is that the indicator must not lie, and an indicator that appears only when exclusive mode
/// engaged would leave "shared" and "this build has no mode indicator" looking identical.
fn audio_mode_label(mode: &crate::host::AudioModeStatus) -> String {
    let name = match mode.share_mode {
        crate::host::AudioShareMode::Shared => "Shared",
        crate::host::AudioShareMode::Exclusive => "Exclusive",
    };
    format!("{name} mode — {}", mode.device_name)
}

/// A heading, then [`param_controls`] for `prefix` -- the ordinary section, for the four stages
/// whose heading has nothing between it and its own controls.
fn param_section(
    ui: &mut egui::Ui,
    title: &str,
    prefix: &str,
    params: &ParamValues,
    intents: &mut Vec<UiIntent>,
) {
    ui.heading(title);
    param_controls(ui, prefix, params, intents);
}

/// One [`param_control`] for every `REGISTRY` entry whose key starts with `prefix`, with **no**
/// heading of its own -- reads the live registry rather than a hand-maintained per-section list,
/// so a parameter added to a stage's descriptor module (`namir-params/src/stages/*.rs`) appears
/// here automatically.
///
/// Split out of [`param_section`] for issue #103. Two sections put something between their
/// heading and their controls -- the IR name under "Impulse Response", the output meter under
/// "Output" -- so both drew the heading themselves *and* called `param_section` with the same
/// title, and the shipped screen carried each of those two headings twice, separated only by the
/// element in between. The heading is the caller's to draw whenever anything comes between it and
/// the controls; `param_section` stays the shorthand for when nothing does.
fn param_controls(
    ui: &mut egui::Ui,
    prefix: &str,
    params: &ParamValues,
    intents: &mut Vec<UiIntent>,
) {
    for (descriptor, value) in params.iter().filter(|(d, _)| d.key.starts_with(prefix)) {
        param_control(ui, descriptor, value, intents);
    }
}

/// Renders exactly one `REGISTRY` entry by descriptor, for the two chain-level (D-10.4) controls
/// that don't belong to any per-stage section: `global.output_ceiling_db` (grouped visually with
/// Output) and `global.bypass` (FR-UI-020 lists it as its own top-level item, not folded into a
/// generic "Global" section).
fn render_single(
    ui: &mut egui::Ui,
    descriptor: &'static namir_params::ParamDescriptor,
    params: &ParamValues,
    intents: &mut Vec<UiIntent>,
) {
    if let Some(value) = params.get(descriptor.key) {
        param_control(ui, descriptor, value, intents);
    }
}

/// The widget/window type both `namir-app` (standalone, via [`open_blocking`]) and `namir-clap`
/// (embedded, via [`open_parented`]) construct -- FR-UI-010's "a single graphical user interface
/// implementation". Owns nothing from `namir-engine`/`namir-worker`/`namir-platform`: only `H`
/// (the host) and this crate's own transient view state.
// trace: FR-UI-010
pub struct NamirUi<H: UiHost> {
    host: H,
    view: ViewState,
}

impl<H: UiHost> NamirUi<H> {
    /// Wraps `host` in a fresh window state.
    pub fn new(host: H) -> Self {
        Self {
            host,
            view: ViewState::default(),
        }
    }

    /// One frame: fetch a snapshot, render FR-UI-020's screen from it, dispatch every intent that
    /// interaction produced, then request another repaint (meters and scan progress both need
    /// continuous updates even with no user input).
    fn frame(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.host.snapshot();
        let mut intents = Vec::new();
        render(ui, &mut self.view, &snapshot, &mut intents);
        for intent in intents {
            self.host.dispatch(intent);
        }
        // Meters and scan progress both need to keep animating with no user input at all.
        ui.ctx().request_repaint();
    }
}

fn default_window_size() -> baseview::dpi::Size {
    // FR-UI-080 (Should): usable on a window as small as 800x600 logical pixels. This is the
    // *default* opening size, comfortably above that floor, not the floor itself -- the window
    // remains user-resizable (baseview's default `WindowOpenOptions` behaviour).
    baseview::dpi::Size::Logical(baseview::dpi::LogicalSize::new(960.0, 640.0))
}

/// Opens `host` in a standalone, blocking window -- `namir-app`'s use of FR-UI-010's one shared
/// UI implementation. Blocks the calling thread until the window is closed (matching
/// `egui_baseview::EguiWindow::open_blocking`'s own contract); `namir-app` is expected to call
/// this from whatever thread it dedicates to the GUI.
pub fn open_blocking<H>(title: impl Into<String>, host: H)
where
    H: UiHost + 'static,
{
    let settings = egui_baseview::EguiWindowSettings {
        title: title.into(),
        size: default_window_size(),
        ..Default::default()
    };
    egui_baseview::EguiWindow::open_blocking(
        settings,
        NamirUi::new(host),
        |_ctx, _cmds, _state: &mut NamirUi<H>| {},
        |_output, _viewport, _state: &mut NamirUi<H>| {},
        |ui, _cmds, state: &mut NamirUi<H>| state.frame(ui),
    );
}

/// Opens `host` embedded in `parent`'s window -- `namir-clap`'s use of FR-UI-010's one shared UI
/// implementation (`spikes/s4-clack-clap` shows the shape a CLAP host's `set_parent` call
/// eventually supplies `parent` from; wiring a real CLAP plugin to this function is `namir-clap`'s
/// job, not this crate's). Returns immediately with a handle the caller closes when the host asks
/// the plugin to destroy its editor.
pub fn open_parented<H, P>(parent: &P, title: impl Into<String>, host: H) -> baseview::WindowHandle
where
    H: UiHost + 'static,
    P: raw_window_handle::HasWindowHandle,
{
    let settings = egui_baseview::EguiWindowSettings {
        title: title.into(),
        size: default_window_size(),
        ..Default::default()
    };
    egui_baseview::EguiWindow::open_parented(
        parent,
        settings,
        NamirUi::new(host),
        |_ctx, _cmds, _state: &mut NamirUi<H>| {},
        |_output, _viewport, _state: &mut NamirUi<H>| {},
        |ui, _cmds, state: &mut NamirUi<H>| state.frame(ui),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RecordingHost;
    use namir_params::REGISTRY;
    use namir_params::stages::gate;

    fn headless_frame(view: &mut ViewState, snapshot: &UiSnapshot, intents: &mut Vec<UiIntent>) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 640.0),
                )),
                ..Default::default()
            },
            |ui| {
                render(ui, view, snapshot, intents);
            },
        );
    }

    /// Smoke test: the whole FR-UI-020 screen renders from a default snapshot without panicking,
    /// and every screen element the requirement lists is exercised (meters, trim, gate, model/IR
    /// name placeholders, EQ, output, global bypass) since [`render`] unconditionally builds all
    /// of them every frame.
    #[test]
    fn rendering_the_full_screen_from_a_default_snapshot_does_not_panic() {
        let mut view = ViewState::default();
        let snapshot = UiSnapshot::default();
        let mut intents = Vec::new();
        headless_frame(&mut view, &snapshot, &mut intents);
        // A fresh default snapshot triggers no interaction on its own.
        assert!(intents.is_empty());
    }

    /// Every `REGISTRY` entry is reachable from *some* section: `param_section`'s prefix-based
    /// grouping plus the two `render_single` calls must together cover every key, or a future
    /// parameter added to `namir-params` would silently never appear on screen. Checked here as a
    /// property over the same prefixes `render` uses, rather than trusting the visual layout.
    #[test]
    fn every_registry_key_is_covered_by_a_section_prefix_or_a_named_single_control() {
        let prefixes = ["trim.", "gate.", "nam.", "ir.", "eq.", "out."];
        let named_singles = [GLOBAL_BYPASS.key, OUTPUT_CEILING_DB.key];
        for descriptor in REGISTRY {
            let covered = prefixes.iter().any(|p| descriptor.key.starts_with(p))
                || named_singles.contains(&descriptor.key);
            assert!(covered, "{} is not rendered by any section", descriptor.key);
        }
    }

    /// FR-IO-020's indicator states the mode *and* the device, and says "Shared" out loud rather
    /// than falling silent — see [`audio_mode_label`]'s own doc comment for why the ordinary case
    /// is still labelled.
    #[test]
    fn the_audio_mode_label_names_both_the_granted_mode_and_the_device() {
        let exclusive = audio_mode_label(&crate::host::AudioModeStatus {
            share_mode: crate::host::AudioShareMode::Exclusive,
            device_name: "Scarlett 2i2".to_string(),
        });
        assert!(exclusive.contains("Exclusive"), "{exclusive}");
        assert!(exclusive.contains("Scarlett 2i2"), "{exclusive}");

        let shared = audio_mode_label(&crate::host::AudioModeStatus {
            share_mode: crate::host::AudioShareMode::Shared,
            device_name: "Scarlett 2i2".to_string(),
        });
        assert!(shared.contains("Shared"), "{shared}");
        assert!(!shared.contains("Exclusive"), "{shared}");
    }

    /// The top panel renders with an indicator present as well as absent -- the `Option` arm added
    /// for FR-IO-020 is reached by a real frame, not only by [`audio_mode_label`] directly.
    #[test]
    fn rendering_with_an_audio_mode_indicator_present_does_not_panic() {
        let mut view = ViewState::default();
        let snapshot = UiSnapshot {
            audio_mode: Some(crate::host::AudioModeStatus {
                share_mode: crate::host::AudioShareMode::Exclusive,
                device_name: "Scarlett 2i2".to_string(),
            }),
            ..UiSnapshot::default()
        };
        let mut intents = Vec::new();
        headless_frame(&mut view, &snapshot, &mut intents);
        assert!(intents.is_empty());
    }

    /// A minimal [`UiHost`] that counts `snapshot` calls (`Arc<AtomicU32>` rather than
    /// `Rc<Cell<_>>` since `UiHost: Send`) -- used to verify `NamirUi::frame`'s own glue code
    /// (fetch a snapshot, render, dispatch, request another repaint), the one piece of this
    /// crate `RecordingHost`'s simpler design doesn't exercise.
    struct CountingHost {
        snapshot_calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
        base: UiSnapshot,
    }

    impl UiHost for CountingHost {
        fn snapshot(&mut self) -> UiSnapshot {
            self.snapshot_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.base.clone()
        }

        fn dispatch(&mut self, _intent: UiIntent) {}
    }

    /// `NamirUi::frame` must fetch exactly one snapshot per frame (never zero -- the screen would
    /// go stale -- and never more than one, which would mean rendering against two different
    /// instants of host state in the same frame) and must always request another repaint, since
    /// meters and scan progress both need to keep animating with no user input at all.
    #[test]
    fn frame_fetches_exactly_one_snapshot_and_requests_a_repaint() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let host = CountingHost {
            snapshot_calls: std::sync::Arc::clone(&calls),
            base: UiSnapshot::default(),
        };
        let mut namir_ui = NamirUi::new(host);

        let ctx = egui::Context::default();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 640.0),
                )),
                ..Default::default()
            },
            |ui| namir_ui.frame(ui),
        );

        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let viewport = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("root viewport output present");
        assert!(
            viewport.repaint_delay.is_zero(),
            "frame() must request a repaint so meters keep animating"
        );
    }

    /// The `RawInput` a headless frame runs on, `events` swapped in per call -- the same shape
    /// [`headless_frame`] builds, exposed separately because driving an interaction needs several
    /// consecutive frames against one shared [`egui::Context`] rather than one throwaway frame.
    /// `time` advances so `egui`'s own click/drag timing logic sees successive instants.
    fn frame_input(time: f64, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            time: Some(time),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(960.0, 640.0),
            )),
            events,
            ..Default::default()
        }
    }

    /// Every text `render` painted this frame, with the rect it occupies on screen -- how a test
    /// finds a *real* control to interact with without this module exposing a widget id or a
    /// layout constant to it. `egui` emits one `Shape::Text` per painted galley (nested inside
    /// `Shape::Vec` for a panel's contents), so a control's own name and its formatted value are
    /// both locatable by the exact string the user reads on screen.
    fn painted_texts(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push((text.galley.text().to_string(), text.visual_bounding_rect()));
                }
                egui::Shape::Vec(shapes) => {
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

    /// The rect of the one shape whose painted text is exactly `needle`. Asserts uniqueness: a
    /// string that appears twice on screen (a section heading that repeats a control's name, say)
    /// would otherwise silently pick whichever came first and make the interaction below land
    /// somewhere other than the control this test means to drive.
    fn unique_text_rect(output: &egui::FullOutput, needle: &str) -> egui::Rect {
        let matches: Vec<egui::Rect> = painted_texts(output)
            .into_iter()
            .filter(|(text, _)| text == needle)
            .map(|(_, rect)| rect)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one control painting {needle:?} this frame, found {} -- if a \
             default value changed so two controls now read the same text, drive a different \
             control rather than relaxing this",
            matches.len()
        );
        matches[0]
    }

    /// **The glue `NamirUi::frame` is: snapshot → [`render`] → collect intents → `UiHost::dispatch`.**
    /// Driven end to end here by dragging a real control in a real frame, rather than by calling
    /// `dispatch` directly -- which is what this test used to do, and which only re-tested
    /// `RecordingHost`'s own `Vec::push` while leaving the one path it claimed to cover untested
    /// (issue #102).
    ///
    /// The control is located by the text it actually paints (`unique_text_rect`), so nothing here
    /// depends on a layout constant or a widget id this module would have to expose: the pointer
    /// lands wherever `render` really put the Gate Threshold `DragValue` this frame. Three frames,
    /// because that is what a drag is: press, move (the frame the value changes on), release.
    #[test]
    fn dispatched_intents_from_a_frame_reach_the_host() {
        let snapshot = UiSnapshot::default();
        let before = snapshot
            .params
            .get(gate::THRESHOLD_DB.key)
            .expect("gate.threshold_db is a REGISTRY entry");
        let host = RecordingHost {
            snapshot,
            dispatched: Vec::new(),
        };
        let mut namir_ui = NamirUi::new(host);
        let ctx = egui::Context::default();

        // Frame 0, no input: find where `render` put the control's value, by its own painted text.
        let output = ctx.run_ui(frame_input(0.0, Vec::new()), |ui| namir_ui.frame(ui));
        let value_rect = unique_text_rect(&output, &gate::THRESHOLD_DB.format_value(before));
        let pos = value_rect.center();

        // Frame 1: press on it. Pressing alone changes nothing, so nothing may reach the host yet.
        let _ = ctx.run_ui(
            frame_input(
                0.2,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            ),
            |ui| namir_ui.frame(ui),
        );
        assert!(
            namir_ui.host.dispatched.is_empty(),
            "a press with no movement must not dispatch anything, got {:?}",
            namir_ui.host.dispatched
        );

        // Frame 2: drag right. This is the frame `DragValue::changed()` fires on, so this is the
        // frame `render` appends a `SetParam` and `frame` hands it to the host.
        let moved = pos + egui::vec2(40.0, 0.0);
        let _ = ctx.run_ui(
            frame_input(0.2, vec![egui::Event::PointerMoved(moved)]),
            |ui| namir_ui.frame(ui),
        );
        let dispatched = namir_ui.host.dispatched.clone();
        assert_eq!(
            dispatched.len(),
            1,
            "the drag frame must dispatch exactly one intent, got {dispatched:?}"
        );
        let UiIntent::SetParam { key, value } = dispatched[0] else {
            panic!("dragging a control must dispatch SetParam, got {dispatched:?}");
        };
        assert_eq!(key, gate::THRESHOLD_DB.key);
        // Deliberately not an exact figure: how many dB a 40-pixel drag is worth is `egui`'s own
        // mapping of `DragValue::speed`, not a property of this crate. What this crate owns is
        // that a rightward drag raises *this* control's value and reports it to the host.
        assert!(
            value > before,
            "a rightward drag must raise the value: {value} vs {before}"
        );

        // Frame 3: release. The intent already dispatched stands; nothing new is invented on the
        // way out of the gesture.
        let _ = ctx.run_ui(
            frame_input(
                0.3,
                vec![egui::Event::PointerButton {
                    pos: moved,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ),
            |ui| namir_ui.frame(ui),
        );
        assert_eq!(namir_ui.host.dispatched, dispatched);
    }

    /// One press-and-release of the primary button at `pos`, as one frame's events.
    fn click_at(pos: egui::Pos2) -> Vec<egui::Event> {
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

    /// The rect of the first shape whose painted text is exactly `needle`, or `None`.
    fn find_text(output: &egui::FullOutput, needle: &str) -> Option<egui::Rect> {
        painted_texts(output)
            .into_iter()
            .find(|(text, _)| text == needle)
            .map(|(_, rect)| rect)
    }

    /// A [`NamirUi`] over a [`RecordingHost`] plus its own [`egui::Context`], with a running
    /// clock -- the shape every multi-frame interaction test below drives.
    struct Driver {
        ui: NamirUi<RecordingHost>,
        ctx: egui::Context,
        time: f64,
    }

    impl Driver {
        fn new(snapshot: UiSnapshot) -> Self {
            Self {
                ui: NamirUi::new(RecordingHost {
                    snapshot,
                    dispatched: Vec::new(),
                }),
                ctx: egui::Context::default(),
                time: 0.0,
            }
        }

        /// One whole frame through `NamirUi::frame` -- snapshot, render, dispatch -- carrying
        /// `events`, returning what it painted.
        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.time += 0.1;
            let time = self.time;
            let ui = &mut self.ui;
            self.ctx.run_ui(frame_input(time, events), |u| ui.frame(u))
        }

        /// Where the control painting exactly `needle` ended up, once the layout has settled.
        ///
        /// **Two idle frames, and the second is the one measured.** A panel is sized from what it
        /// measured the frame before, so the first frame after any change paints a partial row:
        /// with the preset row, frame 0 paints the name box but neither the "Preset" label nor the
        /// buttons, which puts the box at an x it will not keep. A rect taken from that frame
        /// sends the click below to where the control *was*, and the interaction lands on nothing.
        fn locate(&mut self, needle: &str) -> egui::Rect {
            self.frame(Vec::new());
            let output = self.frame(Vec::new());
            find_text(&output, needle).unwrap_or_else(|| {
                panic!(
                    "nothing painting {needle:?} is on screen; painted: {:?}",
                    painted_texts(&output)
                        .into_iter()
                        .map(|(text, _)| text)
                        .collect::<Vec<_>>()
                )
            })
        }

        /// Types `text` into whichever text box paints `hint` while empty: locate it, click into
        /// it, then send the text.
        fn type_into(&mut self, hint: &str, text: &str) {
            let rect = self.locate(hint);
            self.frame(click_at(rect.center()));
            self.frame(vec![egui::Event::Text(text.to_string())]);
        }

        /// Clicks whichever control paints `needle`.
        fn click_text(&mut self, needle: &str) {
            let rect = self.locate(needle);
            self.frame(click_at(rect.center()));
        }
    }

    /// **Issue #100, the save half, driven end to end.** FR-STATE-030 is a Must and `namir-ui` is
    /// the only GUI: before this, `UiSnapshot::unsaved_changes` was rendered as "* unsaved
    /// changes" and `UiIntent` had no variant that could resolve it, so the screen showed the user
    /// a dirty flag and no control that could act on it.
    ///
    /// Typed into the real box and clicked on the real button, both located by the text `render`
    /// painted, so nothing here depends on a layout constant this module would have to expose.
    #[test]
    fn naming_a_preset_and_pressing_save_dispatches_a_save_intent() {
        let mut driver = Driver::new(UiSnapshot {
            unsaved_changes: true,
            ..UiSnapshot::default()
        });
        driver.type_into("Preset name", "Crunch");
        assert!(
            driver.ui.host.dispatched.is_empty(),
            "typing a name is not yet a save: {:?}",
            driver.ui.host.dispatched
        );

        driver.click_text("Save preset");
        assert_eq!(
            driver.ui.host.dispatched,
            vec![UiIntent::SavePreset {
                name: "Crunch".to_string()
            }]
        );
    }

    /// The dirty flag and the control that resolves it are on screen together -- the exact
    /// complaint issue #100 opens with. Asserted on one frame's paint output, so a save control
    /// that existed only on some other screen or behind a menu would not satisfy it.
    #[test]
    fn the_unsaved_changes_flag_is_shown_beside_a_control_that_can_resolve_it() {
        let mut driver = Driver::new(UiSnapshot {
            unsaved_changes: true,
            ..UiSnapshot::default()
        });
        driver.frame(Vec::new());
        let output = driver.frame(Vec::new());
        assert!(
            find_text(&output, "* unsaved changes").is_some(),
            "the dirty flag is shown"
        );
        assert!(
            find_text(&output, "Save preset").is_some(),
            "and a save control is shown on the same screen"
        );
    }

    /// An empty name is not a preset: FR-STATE-030 says "a **named** preset", and a host handed an
    /// empty name would have to invent a filename or refuse. The control refuses first.
    #[test]
    fn saving_with_an_empty_name_dispatches_nothing() {
        let mut driver = Driver::new(UiSnapshot::default());
        driver.click_text("Save preset");
        assert!(
            driver.ui.host.dispatched.is_empty(),
            "an unnamed save must not reach the host: {:?}",
            driver.ui.host.dispatched
        );
    }

    /// FR-UI-030's "operable by keyboard", for the one gesture this row adds: a user whose hands
    /// are already in the name box presses Enter rather than reaching for the button.
    #[test]
    fn pressing_enter_in_the_name_box_saves_under_that_name() {
        let mut driver = Driver::new(UiSnapshot::default());
        driver.type_into("Preset name", "Crunch");
        driver.frame(vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(
            driver.ui.host.dispatched,
            vec![UiIntent::SavePreset {
                name: "Crunch".to_string()
            }]
        );
    }

    /// A host that has listed no presets gets a control that says so and does nothing, rather than
    /// an empty menu or no control at all -- "there is nothing to recall" and "this build cannot
    /// recall" must not look the same, the same rule `audio_mode_label` follows for a share mode.
    #[test]
    fn with_no_presets_listed_the_recall_control_says_so_and_dispatches_nothing() {
        let mut driver = Driver::new(UiSnapshot::default());
        driver.click_text("No saved presets");
        assert!(
            driver.ui.host.dispatched.is_empty(),
            "{:?}",
            driver.ui.host.dispatched
        );
    }

    /// **Issue #100, the recall half.** The user picks from what the host listed, and the intent
    /// carries that entry's own path -- not its name, and not a path this crate built: a host can
    /// only ever be asked to recall something it itself put in [`UiSnapshot::presets`].
    ///
    /// The second preset is chosen deliberately, so an implementation that always reported the
    /// first cannot pass.
    #[test]
    fn recalling_a_preset_dispatches_that_presets_own_path() {
        let mut driver = Driver::new(UiSnapshot {
            presets: vec![
                crate::host::PresetSummary {
                    name: "Clean".to_string(),
                    path: std::path::PathBuf::from("/presets/clean.namirpreset"),
                },
                crate::host::PresetSummary {
                    name: "Crunch".to_string(),
                    path: std::path::PathBuf::from("/presets/crunch.namirpreset"),
                },
            ],
            ..UiSnapshot::default()
        });

        driver.click_text("Recall preset");
        driver.click_text("Crunch");

        assert_eq!(
            driver.ui.host.dispatched,
            vec![UiIntent::RecallPreset {
                path: std::path::PathBuf::from("/presets/crunch.namirpreset")
            }]
        );
    }

    /// **Issue #103.** Every section heading `render` paints must be painted once. Two were
    /// painted twice: `ui.heading("Impulse Response")` was immediately followed by a
    /// `param_section` whose own title was also `"Impulse Response"`, separated on screen only by
    /// the IR-name label, and `"Output"` had the identical shape with the output meter between the
    /// two copies. The smoke test above only asserts that rendering does not panic, so nothing
    /// caught it.
    ///
    /// Driven at a window tall enough that the central panel's `ScrollArea` has no content below
    /// the fold: `egui` culls a widget whose rectangle is not visible, so a heading scrolled out
    /// of view is never painted at all, and a duplicate-count assertion at 960x640 would be
    /// counting what fits rather than what is drawn.
    ///
    /// `"Input Trim"` is deliberately **not** on this list and is not a defect: it is painted
    /// twice because `trim.gain_db`'s own `ParamDescriptor::name` is also "Input Trim", so the
    /// second painting is a control's name, not a repeated heading.
    ///
    /// `"Output"` had a **third** painting the issue's own diagnosis does not name: the output
    /// meter's label. `ui.heading("Output")`, a meter labelled "Output" and a `param_section`
    /// titled "Output" put the word on three consecutive rows. Removing the section title alone
    /// left two, and this test is what said so -- which is why `"Input"` (the input meter's label,
    /// with no heading above it) is on the list too, as the shape the output side now matches.
    #[test]
    fn each_section_heading_is_painted_exactly_once() {
        const TALL: egui::Rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1200.0, 2400.0));
        let mut view = ViewState::default();
        let snapshot = UiSnapshot::default();
        let mut intents = Vec::new();

        // Two frames: a panel is sized from what it measured the frame before.
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(TALL),
            ..Default::default()
        };
        let _ = ctx.run_ui(input(), |ui| {
            render(ui, &mut view, &snapshot, &mut intents);
        });
        let output = ctx.run_ui(input(), |ui| {
            render(ui, &mut view, &snapshot, &mut intents);
        });
        let painted = painted_texts(&output);

        for heading in [
            "Library",
            "Input",
            "Gate",
            "Model",
            "NAM",
            "Impulse Response",
            "EQ",
            "Output",
        ] {
            let count = painted.iter().filter(|(text, _)| text == heading).count();
            assert_eq!(
                count, 1,
                "the heading {heading:?} was painted {count} times, not once"
            );
        }
    }

    /// **Issue #42's other axis, at the layer that owns the container.** The horizontal half of
    /// that issue — a long notice pushing `Dismiss` past the right edge — is fixed and asserted in
    /// `notices`' own tests. The vertical half is a property of *this* module, because it is here
    /// that the notice list is given the top panel to live in: a full `MAX_NOTICES` list, each row
    /// two lines tall since FR-UI-070's remedy line was added beneath the message, in a CLAP
    /// editor fixed at 960x640 with `can_resize() == false`.
    ///
    /// Before `notices::render` bounded the list, that measured ~736 px of rows in a 640 px
    /// window: three `Dismiss` buttons were clipped away entirely — undismissable, in a window
    /// that cannot be widened, from a list nothing else removes — and the top panel had swallowed
    /// the screen so completely that **not one FR-UI-020 control was painted**. Both halves are
    /// asserted, by the text `render` really painted rather than by a layout constant.
    #[test]
    fn a_full_notice_list_leaves_the_rest_of_the_screen_on_a_960x640_editor() {
        const EDITOR: egui::Rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(960.0, 640.0));
        const CODE: namir_core::ErrorCode = namir_core::ErrorCode::new(
            "ui.example.file_missing",
            namir_core::Severity::Error,
            "The file could not be found ({detail}).",
            "Check the file is still where the library lists it, then rescan.",
        );

        let snapshot = UiSnapshot {
            notices: (0..crate::MAX_NOTICES as u64)
                .map(|i| crate::host::UiNotice {
                    id: i,
                    code: CODE,
                    detail: format!(
                        "C:/Users/somebody/Documents/Namir/Library/marshall/plexi-1959-bright-\
                         channel-take-{i}.nam: the file could not be read (os error 2)"
                    ),
                })
                .collect(),
            ..UiSnapshot::default()
        };
        let mut view = ViewState::default();
        let mut intents = Vec::new();

        // Two frames: `egui` sizes a panel from what it measured the frame before, so nothing
        // inside the top panel is painted on the first one.
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(frame_input(0.0, Vec::new()), |ui| {
            render(ui, &mut view, &snapshot, &mut intents);
        });
        let output = ctx.run_ui(frame_input(0.1, Vec::new()), |ui| {
            render(ui, &mut view, &snapshot, &mut intents);
        });
        let painted = painted_texts(&output);

        for (text, rect) in &painted {
            if text == "Dismiss" {
                assert!(
                    EDITOR.contains_rect(*rect),
                    "a Dismiss button at {rect:?} falls outside a {EDITOR:?} editor that cannot \
                     be resized -- that notice can never be removed"
                );
            }
        }
        assert!(
            painted.iter().any(|(text, _)| text == "Dismiss"),
            "the notices are on screen at all"
        );

        // The screen the notices share. One element from each of the other two panels, so a top
        // panel that has taken the window cannot pass this.
        for element in ["Library", "Input Trim"] {
            let rect = painted
                .iter()
                .find(|(text, _)| text == element)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| {
                    panic!("a full notice list left no room to paint {element:?} at all")
                });
            assert!(
                EDITOR.contains_rect(rect),
                "{element:?} was pushed to {rect:?}, outside a {EDITOR:?} editor"
            );
        }
    }
}
