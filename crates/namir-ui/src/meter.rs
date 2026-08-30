//! FR-UI-020's input/output meters: a peak-reading bar. `namir-ui` never computes a meter value
//! itself (it never sees an audio sample, per D-5.1) -- this module only maps an already-computed
//! [`MeterReading`] onto something drawable.

use egui::{ProgressBar, Ui};

use crate::host::MeterReading;

/// The dB floor a meter reads as empty at -- matches this workspace's own silence-floor
/// convention (`namir_params::stages::out::SILENCE_FLOOR_DB`, FR-OUT-010's "-60 dB or below is
/// exact silence"), so the meter's empty end lines up with the point the output stage itself
/// treats as silence, not an arbitrary different number.
pub const METER_FLOOR_DB: f32 = namir_params::stages::out::SILENCE_FLOOR_DB;

/// Maps `db` (dBFS; `f32::NEG_INFINITY` for silence) onto `0.0..=1.0` for display: `0.0` at
/// [`METER_FLOOR_DB`] or below, `1.0` at `0.0` dBFS or above. Pure and separately testable from
/// the widget it feeds.
pub fn normalize_db(db: f32) -> f32 {
    if db.is_nan() {
        // f32::clamp documents that it propagates a NaN `self` rather than treating it as
        // "below the minimum" -- a NaN reading is already a host-side bug, but this is the
        // display layer, so it degrades to silence rather than handing `egui::ProgressBar` a
        // NaN fraction.
        return 0.0;
    }
    ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
}

/// One meter reading as the text printed beside the bar.
///
/// Above [`METER_FLOOR_DB`] this is the figure, to one decimal, in dBFS. **At or below the floor
/// it is stated as a bound, `<= -60.0 dBFS` (with a real U+2264), and not as a number**
/// (issue #105).
///
/// Two reasons, one of which is the reported defect and one of which is the more general case:
///
/// - [`MeterReading::SILENT`] is `f32::NEG_INFINITY` on both readings, and `{:.1}` renders that as
///   the literal `-inf dBFS`. That is correct arithmetic -- the dB value of an amplitude of
///   exactly zero is unbounded below -- and it is the label the screen carries before any audio
///   has arrived at all, so `-inf` is the *first* thing a user ever reads off this interface. No
///   physical meter shows it; it reads as a fault, or as a unit nobody recognises, rather than as
///   "silence".
/// - Below the floor the bar beside the text is already pinned empty by [`normalize_db`], so any
///   figure printed there claims a precision the meter is not displaying. Bounding the text at the
///   same number the bar bottoms out at keeps the two halves of one widget telling one story.
///
/// The floor is spelled out rather than replaced by a dash or by the word "silent": the reader
/// keeps the unit and the scale, so the label above the floor and the label below it stay visibly
/// the same kind of value. A NaN reading -- already a host-side bug -- degrades to the same bound
/// [`normalize_db`] already degrades it to, rather than painting `NaN dBFS`.
pub fn format_db(db: f32) -> String {
    if db.is_nan() || db <= METER_FLOOR_DB {
        format!("\u{2264} {METER_FLOOR_DB:.1} dBFS")
    } else {
        format!("{db:.1} dBFS")
    }
}

/// Renders one labelled meter bar for `reading`.
pub fn render(ui: &mut Ui, label: &str, reading: MeterReading) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            ProgressBar::new(normalize_db(reading.peak_db))
                .text(format_db(reading.peak_db))
                .desired_width(180.0),
        )
        .on_hover_text(format!(
            "Peak {}, RMS {}",
            format_db(reading.peak_db),
            format_db(reading.rms_db)
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every text `render` painted this frame at `window`, read off the shapes it actually
    /// produced -- the same technique `app`'s and `notices`' tests use, for the same reason: what
    /// a user reads is what was painted, not what a second copy of the formatting logic would say.
    fn painted_texts(reading: MeterReading, window: egui::Rect) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(window),
                ..Default::default()
            },
            |ui| render(ui, "Input", reading),
        );
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut texts);
        }
        texts
    }

    /// **Issue #105, through the real widget.** `MeterReading::SILENT` is `f32::NEG_INFINITY` on
    /// both readings, and `{:.1}` renders that as the literal `-inf` -- which is the label the
    /// screen carries before any audio has arrived at all, i.e. the first thing a user ever reads
    /// off this interface. Asserted on what `render` painted rather than on [`format_db`] alone,
    /// because the defect was in the bar's own `.text(..)`, not in a helper.
    #[test]
    fn a_silent_meter_never_paints_the_word_inf() {
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 100.0));
        let texts = painted_texts(MeterReading::SILENT, window);
        for text in &texts {
            assert!(
                !text.contains("inf") && !text.contains("NaN"),
                "a silent meter painted {text:?}"
            );
        }
        assert!(
            texts.iter().any(|t| t == &format_db(f32::NEG_INFINITY)),
            "a silent meter must still state its floor, painted: {texts:?}"
        );
    }

    /// The floor reads as a bound, not as a measurement: at or below [`METER_FLOOR_DB`] the bar is
    /// already pinned empty (`normalize_db`), so a figure beside it would claim a precision the
    /// meter is not showing.
    #[test]
    fn at_or_below_the_floor_the_reading_is_stated_as_a_bound() {
        let bound = format_db(METER_FLOOR_DB);
        assert_eq!(bound, "\u{2264} -60.0 dBFS");
        assert_eq!(format_db(f32::NEG_INFINITY), bound);
        assert_eq!(format_db(METER_FLOOR_DB - 12.0), bound);
        assert_eq!(format_db(f32::NAN), bound, "matches normalize_db's NaN arm");
    }

    /// Above the floor nothing changes: a real reading is still a number, to one decimal, in dBFS.
    #[test]
    fn above_the_floor_a_real_reading_is_unchanged() {
        assert_eq!(format_db(-12.34), "-12.3 dBFS");
        assert_eq!(format_db(0.0), "0.0 dBFS");
        assert_eq!(format_db(3.0), "3.0 dBFS");
    }

    #[test]
    fn silence_normalizes_to_zero() {
        assert_eq!(normalize_db(f32::NEG_INFINITY), 0.0);
        assert_eq!(normalize_db(METER_FLOOR_DB), 0.0);
    }

    #[test]
    fn zero_dbfs_normalizes_to_one() {
        assert_eq!(normalize_db(0.0), 1.0);
    }

    #[test]
    fn above_zero_dbfs_clamps_to_one() {
        assert_eq!(normalize_db(12.0), 1.0);
    }

    #[test]
    fn below_floor_clamps_to_zero() {
        assert_eq!(normalize_db(METER_FLOOR_DB - 20.0), 0.0);
    }

    #[test]
    fn midpoint_is_roughly_half() {
        let mid = METER_FLOOR_DB / 2.0; // halfway between the floor and 0 dBFS.
        assert!((normalize_db(mid) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn nan_is_treated_as_silence_not_a_panic() {
        assert_eq!(normalize_db(f32::NAN), 0.0);
    }
}
