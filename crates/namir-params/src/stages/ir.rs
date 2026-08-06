//! Impulse response stage descriptors (FR-IR-070). D-10.1: declared once, here, per stage.

use crate::descriptor::{
    ParamDescriptor, ParamKind, SmoothingCategory, StepIndex, Unit, ValueFormat,
};

/// The FR-CHAIN-020 per-stage bypass for the IR stage.
pub const ENABLED: ParamDescriptor = ParamDescriptor::new(
    "ir.enabled",
    "IR Enabled",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(1),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

/// FR-IR-070: -24..24 dB, default 0.
pub const LEVEL_DB: ParamDescriptor = ParamDescriptor::new(
    "ir.level_db",
    "IR Level",
    Unit::Decibels,
    ParamKind::Continuous {
        min: -24.0,
        max: 24.0,
        default: 0.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-IR-070: low cut, off or 20..500 Hz, default off.
pub const LOW_CUT_ENABLED: ParamDescriptor = ParamDescriptor::new(
    "ir.low_cut_enabled",
    "IR Low Cut Enabled",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(0),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

/// FR-IR-070: low cut frequency, 20..500 Hz, default 80.
pub const LOW_CUT_FREQ_HZ: ParamDescriptor = ParamDescriptor::new(
    "ir.low_cut_freq_hz",
    "IR Low Cut",
    Unit::Hertz,
    ParamKind::Continuous {
        min: 20.0,
        max: 500.0,
        default: 80.0,
    },
    ValueFormat::FixedDecimals(0),
    SmoothingCategory::FrequencyLike,
);

/// FR-IR-070: high cut, off or 1..20 kHz, default off.
pub const HIGH_CUT_ENABLED: ParamDescriptor = ParamDescriptor::new(
    "ir.high_cut_enabled",
    "IR High Cut Enabled",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(0),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

/// FR-IR-070: high cut frequency, 1000..20000 Hz, default 8000.
pub const HIGH_CUT_FREQ_HZ: ParamDescriptor = ParamDescriptor::new(
    "ir.high_cut_freq_hz",
    "IR High Cut",
    Unit::Hertz,
    ParamKind::Continuous {
        min: 1_000.0,
        max: 20_000.0,
        default: 8_000.0,
    },
    ValueFormat::FixedDecimals(0),
    SmoothingCategory::FrequencyLike,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_have_distinct_keys() {
        let keys = [
            ENABLED.key,
            LEVEL_DB.key,
            LOW_CUT_ENABLED.key,
            LOW_CUT_FREQ_HZ.key,
            HIGH_CUT_ENABLED.key,
            HIGH_CUT_FREQ_HZ.key,
        ];
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
