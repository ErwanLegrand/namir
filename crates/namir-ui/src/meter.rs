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

/// Renders one labelled meter bar for `reading`.
pub fn render(ui: &mut Ui, label: &str, reading: MeterReading) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(
            ProgressBar::new(normalize_db(reading.peak_db))
                .text(format!("{:.1} dBFS", reading.peak_db))
                .desired_width(180.0),
        )
        .on_hover_text(format!(
            "Peak {:.1} dBFS, RMS {:.1} dBFS",
            reading.peak_db, reading.rms_db
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
