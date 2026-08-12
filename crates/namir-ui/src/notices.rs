//! FR-UI-070: "errors shall be surfaced non-modally and shall never interrupt audio... [and] shall
//! state what failed, which file or device it concerned, and what the user can do."
//!
//! **"Never interrupt audio" is true by construction here, not by discipline**: this crate has no
//! dependency on `namir-engine` at all (D-5.1), so nothing in this module -- or anywhere else in
//! `namir-ui` -- has a code path that could reach the audio thread even by mistake.
//!
//! **Non-modal** means exactly what it says: [`render`] draws each [`UiNotice`] inline, in normal
//! layout flow, with its own dismiss button -- never an `egui::Window`/modal overlay that blocks
//! interaction with the rest of the screen until acknowledged.
//!
//! The message text itself (`{code.id}: {message_template} ({detail})`) matches
//! `namir_library::LibraryError`/`namir_state::StateError`'s own `Display` format exactly, so a
//! host that forwards one of those errors' `(code, detail)` pair into a [`UiNotice`] produces the
//! same text a developer would see logging that error directly (FR-ERR-020's "documented,
//! searched and tested" catalogue identifier stays visible either way).

use egui::Ui;

use crate::UiIntent;
use crate::host::UiNotice;

/// Renders every notice in `notices`, each as its own non-modal, dismissible line. Appends
/// [`UiIntent::DismissNotice`] to `intents` for whichever notice's dismiss button was clicked
/// this frame (at most one per frame, since a click can only land on one button).
pub fn render(ui: &mut Ui, notices: &[UiNotice], intents: &mut Vec<UiIntent>) {
    for notice in notices {
        ui.horizontal(|ui| {
            ui.label(notice_text(notice));
            if ui
                .button("Dismiss")
                .on_hover_text("Dismiss this notice. It does not affect audio.")
                .clicked()
            {
                intents.push(UiIntent::DismissNotice { id: notice.id });
            }
        });
    }
}

/// `{code.id}: {message_template} ({detail})` -- see this module's doc comment for why this
/// exact shape was chosen. Pure and separately testable from the widget it feeds.
fn notice_text(notice: &UiNotice) -> String {
    format!(
        "{}: {} ({})",
        notice.code.id, notice.code.message_template, notice.detail
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::{ErrorCode, Severity};

    const SAMPLE: ErrorCode = ErrorCode::new(
        "ui.example.file_missing",
        Severity::Error,
        "The file could not be found.",
    );

    #[test]
    fn notice_text_includes_code_id_template_and_detail() {
        let notice = UiNotice {
            id: 1,
            code: SAMPLE,
            detail: "C:/models/plexi.nam".to_string(),
        };
        let text = notice_text(&notice);
        assert!(text.contains("ui.example.file_missing"));
        assert!(text.contains("The file could not be found."));
        assert!(text.contains("C:/models/plexi.nam"));
    }

    #[test]
    fn dismissing_a_notice_emits_its_own_id_not_anothers() {
        let ctx = egui::Context::default();
        let notices = vec![
            UiNotice {
                id: 7,
                code: SAMPLE,
                detail: "first".to_string(),
            },
            UiNotice {
                id: 9,
                code: SAMPLE,
                detail: "second".to_string(),
            },
        ];

        // Discover the second notice's dismiss-button position by reproducing `render`'s exact
        // widget sequence (both rows, in order) -- the second row's vertical position depends on
        // the first row already having been laid out above it.
        let mut button_pos = None;
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(500.0, 300.0),
                )),
                ..Default::default()
            },
            |ui| {
                for (i, notice) in notices.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(notice_text(notice));
                        let response = ui.button("Dismiss");
                        if i == 1 {
                            button_pos = Some(response.rect.center());
                        }
                    });
                }
            },
        );
        let pos = button_pos.expect("dismiss button laid out");

        let mut intents = Vec::new();
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(500.0, 300.0),
                )),
                events: vec![
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
                ],
                ..Default::default()
            },
            |ui| {
                render(ui, &notices, &mut intents);
            },
        );

        assert_eq!(intents, vec![UiIntent::DismissNotice { id: 9 }]);
    }

    #[test]
    fn rendering_no_notices_emits_no_intents() {
        let ctx = egui::Context::default();
        let mut intents = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            render(ui, &[], &mut intents);
        });
        assert!(intents.is_empty());
    }
}
