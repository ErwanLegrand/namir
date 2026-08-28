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
//! # What M14 changed here, and why the four links are one defect
//!
//! A human ran `docs/manual-tests/fr-ui-070-non-modal-error-notices.md` on 2026-08-27 and this
//! module's row was the last link in a chain of four defensible decisions that compose into an
//! unusable state (step 14, issue #42). Taken in the order they compound:
//!
//! 1. **The text was too long**, because the code and template were rendered twice (issue #39,
//!    fixed in `namir-worker` and `namir-app`), and because the template's own `{placeholder}`
//!    tokens reached the screen unsubstituted (issue #15, fixed by `namir_core::ErrorCode::render`,
//!    which [`notice_text`] now calls instead of printing `message_template` raw).
//! 2. **The row did not wrap.** An `egui` horizontal layout does not wrap, so a long label pushed
//!    the `Dismiss` button off the right edge.
//! 3. **The editor could not be widened.** The CLAP editor is fixed at 960x640 with
//!    `can_resize() == false`, so the standalone's escape hatch does not exist there.
//! 4. **Notices never expired**, and `Dismiss` was the only removal path.
//!
//! The result was a notice that could never be removed, occupying part of a screen that
//! FR-UI-020's own run shows already cannot display every element at once. [`render`] now draws
//! the control **first**, in a right-to-left layout, so it is placed against the panel's right
//! edge before the label is given any room at all -- the button's position stops depending on the
//! text's length, which is what makes it reachable at *any* geometry rather than at generous ones.
//! The label then wraps inside whatever is left.
//! `a_long_notice_keeps_its_dismiss_button_reachable_in_a_960x640_editor` asserts that against the
//! plugin's real geometry, because a test that passes only in a wide default window is the defect
//! rather than the check.
//!
//! **The same defect had a second axis, and M14's own fix is what put it there** -- the remedy line
//! below doubled every row's height and the cap of sixteen bounded the list's length without
//! bounding the space it takes, so a full list clipped its last rows off the bottom of the same
//! editor. [`render`]'s doc comment carries that half.
//!
//! **The remedy line costs vertical space in the top panel**, and FR-UI-020's own manual run
//! records that the 960x640 editor already cannot show every element at once. That cost is
//! accepted rather than overlooked: it is paid only while a notice is showing, the notice is
//! dismissible again, and FR-UI-070's third clause is not satisfiable by text nobody can see.
//!
//! [`push_deduplicated`] is the shared list-side half (issues #43 and #42's fourth link). It lives
//! here, in the one crate both shells already depend on, rather than as a copy in each -- this
//! project has had a real bug from duplicating shell logic once already.

use egui::Ui;

use crate::UiIntent;
use crate::host::UiNotice;

/// How many notices a shell keeps on screen at once (see [`push_deduplicated`]).
pub const MAX_NOTICES: usize = 16;

/// The largest share of the window's height the notice list may occupy before it starts to
/// scroll. See [`render`] for why the list needs a bound at all and why the bound is a fraction of
/// the window rather than a number of rows.
pub const MAX_NOTICE_AREA_FRACTION: f32 = 1.0 / 3.0;

/// Renders every notice in `notices`, each as its own non-modal, dismissible line. Appends
/// [`UiIntent::DismissNotice`] to `intents` for whichever notice's dismiss button was clicked
/// this frame (at most one per frame, since a click can only land on one button).
///
/// # The list is bounded on screen, not only in memory (issue #42, vertical axis)
///
/// Issue #42 is "some notices can never be dismissed in the CLAP plugin", and M14 fixed the axis
/// it was reported on: a *long* notice pushed `Dismiss` past the right edge of an editor fixed at
/// 960x640 that `can_resize() == false`. The same pass added FR-UI-070's remedy line beneath every
/// message and capped the list at [`MAX_NOTICES`] — which together put the identical defect on the
/// other axis, and measurably so. Sixteen notices at ~46 px a row is ~736 px of content in a
/// 640 px editor: driving the real `namir_ui::render` at exactly that geometry drew **thirteen**
/// `Dismiss` buttons and clipped three away, and the top panel had by then swallowed the whole
/// window, so not one FR-UI-020 control was painted either. A notice nobody can reach is a notice
/// nobody can dismiss, and the plugin's escape hatch — widen the window — still does not exist.
///
/// So the list gets the same treatment its length already had: it is bounded, and the overflow
/// stays reachable. The notices live in a vertical [`egui::ScrollArea`] capped at
/// [`MAX_NOTICE_AREA_FRACTION`] of the window height, which shrinks to its content while the list
/// is short (one notice still costs one row, not a third of the screen) and scrolls once it is
/// not.
///
/// **A fraction of the window, not a row count.** A row's height depends on how far its text
/// wraps, which depends on the width the shell gives it, so no constant number of rows is safe at
/// every geometry — a bound in rows would be the same class of mistake as a `Dismiss` button whose
/// position depends on the length of the label beside it. The fraction also leaves the rest of the
/// screen its majority share by construction, which is the property FR-UI-020's single-screen
/// layout actually needs.
pub fn render(ui: &mut Ui, notices: &[UiNotice], intents: &mut Vec<UiIntent>) {
    if notices.is_empty() {
        return;
    }
    let max_height = ui.ctx().content_rect().height() * MAX_NOTICE_AREA_FRACTION;
    egui::ScrollArea::vertical()
        .id_salt("namir_ui_notices")
        .max_height(max_height)
        // Never shrink horizontally: the row is laid out right-to-left, so the dismiss button is
        // placed against this area's right edge, and an area narrower than the panel would move it
        // back inside the text's reach -- the very coupling this row's layout exists to break.
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for notice in notices {
                if render_one(ui, notice).clicked() {
                    intents.push(UiIntent::DismissNotice { id: notice.id });
                }
            }
        });
}

/// Draws one notice's row and returns its dismiss button's `Response`.
///
/// Split out of [`render`] so a test can find the button's rectangle **through the real layout
/// code** rather than by reproducing it. That distinction is not cosmetic here: the defect this
/// row's layout was changed to fix (issue #42) is entirely about where the button ends up, so a
/// test that measured a hand-copied second version of the layout would be measuring the wrong
/// thing the moment the two drifted apart — and the first draft of
/// `a_long_notice_keeps_its_dismiss_button_reachable_in_a_960x640_editor` did exactly that, found
/// a plausible-looking rectangle belonging to a row `render` had never drawn, and its synthetic
/// click landed on nothing.
fn render_one(ui: &mut Ui, notice: &UiNotice) -> egui::Response {
    // Right-to-left: the button is laid out against the right edge *before* the text is measured,
    // so its position cannot depend on how long the text is. See this module's doc comment for the
    // 960x640 case that made the previous left-to-right row unusable.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
        let dismiss = ui
            .button("Dismiss")
            .on_hover_text("Dismiss this notice. It does not affect audio.");
        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
            ui.add(egui::Label::new(notice_text(notice)).wrap());
            // FR-UI-070's third clause, in its own weaker style: what failed reads as the notice,
            // what to do about it reads as advice. That separation is the whole argument for
            // `remedy` being a field rather than a sentence inside the template -- see
            // `namir_core::ErrorCode`'s doc comment.
            ui.add(egui::Label::new(egui::RichText::new(notice.code.remedy).weak().small()).wrap());
        });
        dismiss
    })
    .inner
}

/// Appends `notice` to `notices` unless an identical one is already showing, and keeps the list to
/// [`MAX_NOTICES`] by dropping the oldest.
///
/// # Deduplication (issue #43)
///
/// One event produced several identical notices in both shells. `worker_jobs::spawn_recall` has two
/// deliberate triggers -- a host `state` load and an activation -- and the comments at both
/// anticipate them both running; the replay itself is idempotent and its *reporting* was not, so a
/// single deleted `.nam` produced two indistinguishable `state.reference.not_found` lines. The
/// device-lost pair was the same shape from the other shell.
///
/// Identity is `(code.id, detail)`: two notices a user cannot tell apart are one notice. The
/// **existing** entry is kept rather than replaced, so a notice's id -- and therefore whatever the
/// user is about to click -- stays stable while it is on screen.
///
/// # The cap, and why it is a cap and not a timer
///
/// Notices never expired and the list was unbounded. A timed expiry was considered and rejected:
/// a notice that vanishes on its own is exactly the "I never saw what went wrong" failure
/// FR-UI-070's second sentence exists to prevent, and severity is no guide either -- the warning a
/// user most needs to read (a truncated IR, a settings file about to be overwritten) is the one
/// whose severity would earn it the shortest timeout. What was actually broken was unboundedness,
/// so what is bounded is the list. Nothing is lost from the *record* when the oldest is dropped:
/// both shells write an FR-ERR-010 log record from the same function that pushes the notice.
pub fn push_deduplicated(notices: &mut Vec<UiNotice>, notice: UiNotice) {
    if notices
        .iter()
        .any(|n| n.code.id == notice.code.id && n.detail == notice.detail)
    {
        return;
    }
    notices.push(notice);
    if notices.len() > MAX_NOTICES {
        notices.remove(0);
    }
}

/// `{code.id}: {rendered message}` -- the code id stays visible so FR-ERR-020's "documented,
/// searched and tested" identifier is the thing a user quotes in a bug report, and the message is
/// [`namir_core::ErrorCode::render`]'s output rather than the raw template (issue #15). Pure and
/// separately testable from the widget it feeds.
fn notice_text(notice: &UiNotice) -> String {
    format!("{}: {}", notice.code.id, notice.code.render(&notice.detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::{ErrorCode, Severity};

    const SAMPLE: ErrorCode = ErrorCode::new(
        "ui.example.file_missing",
        Severity::Error,
        "The file could not be found ({detail}).",
        "Check the file is still where the library lists it, then rescan.",
    );

    const SELF_CONTAINED: ErrorCode = ErrorCode::new(
        "ui.example.self_contained",
        Severity::Warning,
        "This model's architecture is not supported by this build of Namir.",
        "Load a WaveNet or LSTM model instead.",
    );

    fn notice(id: u64, code: ErrorCode, detail: &str) -> UiNotice {
        UiNotice {
            id,
            code,
            detail: detail.to_string(),
        }
    }

    #[test]
    fn notice_text_includes_code_id_template_and_detail() {
        let text = notice_text(&notice(1, SAMPLE, "C:/models/plexi.nam"));
        assert!(text.contains("ui.example.file_missing"));
        assert!(text.contains("The file could not be found"));
        assert!(text.contains("C:/models/plexi.nam"));
    }

    /// Issue #15, at the layer a user actually reads: the eight literal `{placeholder}` tokens a
    /// human transcribed off a real window in the 2026-08-27 run came from this function printing
    /// `message_template` raw. Nothing rendered here may contain a brace.
    #[test]
    fn no_placeholder_token_survives_into_the_rendered_line() {
        let text = notice_text(&notice(1, SAMPLE, "C:/models/plexi.nam"));
        assert_eq!(
            text,
            "ui.example.file_missing: The file could not be found (C:/models/plexi.nam)."
        );
        assert!(!text.contains('{'), "{text}");
        assert!(!text.contains('}'), "{text}");
    }

    /// A template with no placeholder keeps the pre-M14 shape, detail appended in parentheses --
    /// the contract this module's doc comment has always described, now with one renderer.
    #[test]
    fn a_self_contained_template_still_gets_its_detail_appended() {
        let text = notice_text(&notice(1, SELF_CONTAINED, "architecture=Transformer"));
        assert!(text.ends_with("(architecture=Transformer)"), "{text}");
    }

    /// Issue #43: two triggers, one event, one notice.
    #[test]
    fn an_identical_notice_is_not_shown_twice() {
        let mut notices = Vec::new();
        push_deduplicated(&mut notices, notice(1, SAMPLE, "plexi.nam"));
        push_deduplicated(&mut notices, notice(2, SAMPLE, "plexi.nam"));
        assert_eq!(notices.len(), 1);
        // The *first* notice survives, so an id already on screen stays clickable.
        assert_eq!(notices[0].id, 1);
    }

    /// Deduplication must not merge two different failures. The device-lost pair of step 8 was
    /// indistinguishable only because `{direction}` was never substituted; once the details differ,
    /// they are two facts and both belong on screen.
    #[test]
    fn notices_differing_only_in_detail_are_both_kept() {
        let mut notices = Vec::new();
        push_deduplicated(&mut notices, notice(1, SAMPLE, "input device \"A\""));
        push_deduplicated(&mut notices, notice(2, SAMPLE, "output device \"B\""));
        assert_eq!(notices.len(), 2);
    }

    /// The list is bounded, and the bound drops the oldest rather than refusing the newest.
    #[test]
    fn the_notice_list_is_capped() {
        let mut notices = Vec::new();
        for i in 0..(MAX_NOTICES as u64 + 5) {
            push_deduplicated(&mut notices, notice(i, SAMPLE, &format!("file{i}.nam")));
        }
        assert_eq!(notices.len(), MAX_NOTICES);
        assert_eq!(notices[0].detail, "file5.nam");
    }

    /// **Step 14's defect, at the geometry that produced it.**
    ///
    /// The CLAP editor is fixed at 960x640 with `can_resize() == false`
    /// (`crates/namir-clap/src/gui.rs`), so a notice whose `Dismiss` button lands past x=960 can
    /// never be dismissed and never expires. This drives `render` at exactly that size with a
    /// notice longer than the row, and asserts both halves of "reachable": the button's rectangle
    /// is inside the window, and a real synthetic click on it emits that notice's intent.
    ///
    /// The detail is deliberately far longer than anything the catalogue produces -- the point is
    /// that the button's position does not depend on the text at all.
    ///
    /// The rectangle comes from what [`render`] itself *painted*, not from a second call to
    /// `render_one`: since the list acquired a bounding scroll area (see `render`'s doc comment),
    /// a row drawn outside that area is not the row a user can click, and a test that measured one
    /// would be back to measuring a layout `render` never drew.
    #[test]
    fn a_long_notice_keeps_its_dismiss_button_reachable_in_a_960x640_editor() {
        let long_detail = "C:/Users/somebody/Documents/Namir/Library/marshall/\
                           a-very-long-model-name-of-the-kind-a-capture-session-produces-\
                           plexi-1959-bright-channel-treble-boosted-take-3.nam: \
                           the file could not be read (os error 2)";
        let notices = vec![notice(7, SAMPLE, long_detail)];

        let ctx = egui::Context::default();
        let rects = dismiss_button_rects(&ctx, &notices, EDITOR, Vec::new());
        assert_eq!(rects.len(), 1, "one notice, one dismiss button");
        assert!(
            EDITOR.contains_rect(rects[0]),
            "Dismiss button at {:?} is outside a {EDITOR:?} editor",
            rects[0]
        );

        let mut intents = Vec::new();
        let _ = ctx.run_ui(frame(EDITOR, click_at(rects[0].center())), |ui| {
            render(ui, &notices, &mut intents);
        });
        assert_eq!(intents, vec![UiIntent::DismissNotice { id: 7 }]);
    }

    /// **Issue #42 on its other axis.** A full [`MAX_NOTICES`] list is ~736 px of rows in a 640 px
    /// editor that cannot be resized, so before `render` bounded the list it simply clipped the
    /// last three notices away -- and, having taken the whole window for the top panel, painted no
    /// FR-UI-020 control at all. Both halves are asserted here: nothing is drawn outside the
    /// editor, and the notices stop short of owning the screen.
    #[test]
    fn a_full_notice_list_does_not_take_the_whole_editor() {
        let notices: Vec<UiNotice> = (0..MAX_NOTICES as u64)
            .map(|i| notice(i, SAMPLE, &format!("C:/Namir/Library/model-{i}.nam")))
            .collect();

        let ctx = egui::Context::default();
        let rects = dismiss_button_rects(&ctx, &notices, EDITOR, Vec::new());
        assert!(!rects.is_empty(), "a bounded list still shows notices");
        for rect in &rects {
            assert!(
                EDITOR.contains_rect(*rect),
                "a Dismiss button at {rect:?} falls outside a {EDITOR:?} editor that cannot be \
                 resized -- that notice can never be removed"
            );
        }
        let lowest = rects.iter().map(|r| r.max.y).fold(f32::MIN, f32::max);
        let bound = EDITOR.height() * MAX_NOTICE_AREA_FRACTION;
        assert!(
            lowest <= bound,
            "the notice list reaches {lowest} px in a {EDITOR:?} editor, past its {bound} px bound"
        );
    }

    /// The overflow a bound creates has to stay reachable, or the bound has merely moved the
    /// undismissable notice rather than removed it. Scrolls the notice area to its end and clicks
    /// what is then the lowest button, which must be the **last** notice in the list.
    ///
    /// Unlike the two tests above this one does *not* reproduce issue #42 — an unbounded list is
    /// trivially "scrolled to its end" — it guards the failure mode the **fix** could introduce,
    /// which is a bound that hides notices instead of clipping them.
    #[test]
    fn the_last_notice_of_a_full_list_is_reachable_by_scrolling() {
        let notices: Vec<UiNotice> = (0..MAX_NOTICES as u64)
            .map(|i| notice(i, SAMPLE, &format!("C:/Namir/Library/model-{i}.nam")))
            .collect();

        let ctx = egui::Context::default();
        // A wheel event applies to whatever the pointer is over, so the pointer is put inside the
        // notice area first; the delta is far larger than the list is tall, and `egui` clamps.
        let over_notices = egui::pos2(EDITOR.width() / 2.0, 20.0);
        let scroll = vec![
            egui::Event::PointerMoved(over_notices),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -4000.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            },
        ];
        let rects = dismiss_button_rects(&ctx, &notices, EDITOR, scroll);
        let lowest = rects
            .iter()
            .copied()
            .max_by(|a, b| a.center().y.total_cmp(&b.center().y))
            .expect("a Dismiss button was drawn");
        assert!(EDITOR.contains_rect(lowest), "{lowest:?}");

        let mut intents = Vec::new();
        let _ = ctx.run_ui(frame(EDITOR, click_at(lowest.center())), |ui| {
            render(ui, &notices, &mut intents);
        });
        assert_eq!(
            intents,
            vec![UiIntent::DismissNotice {
                id: MAX_NOTICES as u64 - 1
            }],
            "scrolled to the end, the lowest button must belong to the last notice"
        );
    }

    #[test]
    fn dismissing_a_notice_emits_its_own_id_not_anothers() {
        const WINDOW: egui::Rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(500.0, 300.0));
        let notices = vec![notice(7, SAMPLE, "first"), notice(9, SAMPLE, "second")];

        let ctx = egui::Context::default();
        let rects = dismiss_button_rects(&ctx, &notices, WINDOW, Vec::new());
        assert_eq!(rects.len(), 2, "two notices, two dismiss buttons");
        // The second row is the lower one; its button is the one this test means to click.
        let second = rects
            .iter()
            .copied()
            .max_by(|a, b| a.center().y.total_cmp(&b.center().y))
            .expect("two buttons");

        let mut intents = Vec::new();
        let _ = ctx.run_ui(frame(WINDOW, click_at(second.center())), |ui| {
            render(ui, &notices, &mut intents);
        });

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

    /// The CLAP editor's real geometry -- fixed, and `can_resize() == false`
    /// (`crates/namir-clap/src/gui.rs`). Every layout assertion in this module is made at it,
    /// because a check that passes only in a generous standalone window is the defect rather than
    /// the check.
    const EDITOR: egui::Rect =
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(960.0, 640.0));

    /// One frame's input at `window`, carrying `events`.
    fn frame(window: egui::Rect, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(window),
            events,
            ..Default::default()
        }
    }

    /// Where [`render`] actually put each `Dismiss` button, read off the shapes it painted.
    ///
    /// Several frames, because two `egui` behaviours make a single one unrepresentative: a scroll
    /// area is sized from what it measured the frame *before*, so the first frame's clip rectangle
    /// is a placeholder and nothing inside it is drawn yet; and a wheel delta is applied smoothly
    /// over the frames that follow it rather than all at once. So `events` are delivered on the
    /// second frame, once there is a real area under the pointer to receive them, and the
    /// measurement is taken after the scroll has come to rest -- which is also the state the
    /// caller's own click frame will be in.
    ///
    /// Going through the paint output rather than through a second call to `render_one` is the
    /// same rule this module's `render_one` doc comment records: measure the layout that was
    /// drawn, never a copy of it.
    fn dismiss_button_rects(
        ctx: &egui::Context,
        notices: &[UiNotice],
        window: egui::Rect,
        events: Vec<egui::Event>,
    ) -> Vec<egui::Rect> {
        let mut discard = Vec::new();
        let _ = ctx.run_ui(frame(window, Vec::new()), |ui| {
            render(ui, notices, &mut discard);
        });
        let mut output = ctx.run_ui(frame(window, events), |ui| {
            render(ui, notices, &mut discard);
        });
        for _ in 0..16 {
            output = ctx.run_ui(frame(window, Vec::new()), |ui| {
                render(ui, notices, &mut discard);
            });
        }

        fn walk(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Text(text) if text.galley.text() == "Dismiss" => {
                    out.push(text.visual_bounding_rect());
                }
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut rects = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut rects);
        }
        rects
    }

    /// One press-and-release of the primary button at `pos`.
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
}
