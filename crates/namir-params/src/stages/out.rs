//! Output stage descriptors (FR-OUT-010). D-10.1: declared once, here, per stage.
//!
//! No FR-CHAIN-020 bypass here: FR-CHAIN-020 lists only the gate, NAM, IR and EQ stages as
//! individually bypassable — Trim and Out are not in that list.

use crate::SmoothingCategory;
use crate::descriptor::{ParamDescriptor, ParamKind, Unit, ValueFormat};

/// FR-OUT-010: -60..+12 dB, default 0, with -60 dB or below being exact silence.
pub const GAIN_DB: ParamDescriptor = ParamDescriptor::new(
    "out.gain_db",
    "Output Level",
    Unit::Decibels,
    ParamKind::Continuous {
        min: -60.0,
        max: 12.0,
        default: 0.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-OUT-010's literal floor: at or below this value, output is exact silence, not merely a
/// very quiet asymptotic approach. Shared here so `namir-engine`'s Out stage and this crate's own
/// tests agree on one number rather than each hand-copying `-60.0`.
pub const SILENCE_FLOOR_DB: f32 = -60.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_floor_matches_the_range_minimum() {
        let ParamKind::Continuous { min, .. } = GAIN_DB.kind else {
            panic!("expected Continuous");
        };
        assert_eq!(min, SILENCE_FLOOR_DB);
    }
}
