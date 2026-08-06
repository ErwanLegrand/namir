//! Input trim stage descriptors (FR-IN-010, FR-IN-040). D-10.1: declared once, here, per stage.

use crate::descriptor::{
    ParamDescriptor, ParamKind, SmoothingCategory, StepIndex, Unit, ValueFormat,
};

/// FR-IN-010: input trim, -24..24 dB, default 0, resolution no coarser than 0.1 dB.
pub const GAIN_DB: ParamDescriptor = ParamDescriptor::new(
    "trim.gain_db",
    "Input Trim",
    Unit::Decibels,
    ParamKind::Continuous {
        min: -24.0,
        max: 24.0,
        default: 0.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-IN-040 (Should): a DC-blocking high-pass filter, enabled by default.
pub const DC_BLOCKER_ENABLED: ParamDescriptor = ParamDescriptor::new(
    "trim.dc_blocker_enabled",
    "DC Blocker",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(1),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_have_distinct_keys() {
        assert_ne!(GAIN_DB.key, DC_BLOCKER_ENABLED.key);
    }
}
