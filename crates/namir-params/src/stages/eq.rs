//! Equaliser stage descriptors (FR-EQ-010). D-10.1: declared once, here, per stage.

use crate::descriptor::{
    ParamDescriptor, ParamKind, SmoothingCategory, StepIndex, Unit, ValueFormat,
};

/// The FR-CHAIN-020 per-stage bypass for the EQ stage.
pub const ENABLED: ParamDescriptor = ParamDescriptor::new(
    "eq.enabled",
    "EQ Enabled",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(1),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

/// FR-EQ-010 low band: shelf, 40..500 Hz.
pub const LOW_SHELF_FREQ_HZ: ParamDescriptor = ParamDescriptor::new(
    "eq.low_shelf_freq_hz",
    "EQ Low Shelf Freq",
    Unit::Hertz,
    ParamKind::Continuous {
        min: 40.0,
        max: 500.0,
        default: 100.0,
    },
    ValueFormat::FixedDecimals(0),
    SmoothingCategory::FrequencyLike,
);

/// FR-EQ-010 low band: +-15 dB.
pub const LOW_SHELF_GAIN_DB: ParamDescriptor = ParamDescriptor::new(
    "eq.low_shelf_gain_db",
    "EQ Low Shelf Gain",
    Unit::Decibels,
    ParamKind::Continuous {
        min: -15.0,
        max: 15.0,
        default: 0.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-EQ-010 mid band: peaking, 200 Hz..5 kHz.
pub const MID_FREQ_HZ: ParamDescriptor = ParamDescriptor::new(
    "eq.mid_freq_hz",
    "EQ Mid Freq",
    Unit::Hertz,
    ParamKind::Continuous {
        min: 200.0,
        max: 5_000.0,
        default: 1_000.0,
    },
    ValueFormat::FixedDecimals(0),
    SmoothingCategory::FrequencyLike,
);

/// FR-EQ-010 mid band: +-15 dB.
pub const MID_GAIN_DB: ParamDescriptor = ParamDescriptor::new(
    "eq.mid_gain_db",
    "EQ Mid Gain",
    Unit::Decibels,
    ParamKind::Continuous {
        min: -15.0,
        max: 15.0,
        default: 0.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-EQ-010 mid band: adjustable Q, 0.2..5.0.
pub const MID_Q: ParamDescriptor = ParamDescriptor::new(
    "eq.mid_q",
    "EQ Mid Q",
    Unit::None,
    ParamKind::Continuous {
        min: 0.2,
        max: 5.0,
        default: 0.707,
    },
    ValueFormat::FixedDecimals(2),
    SmoothingCategory::FrequencyLike,
);

/// FR-EQ-010 high band: shelf, 1..12 kHz.
pub const HIGH_SHELF_FREQ_HZ: ParamDescriptor = ParamDescriptor::new(
    "eq.high_shelf_freq_hz",
    "EQ High Shelf Freq",
    Unit::Hertz,
    ParamKind::Continuous {
        min: 1_000.0,
        max: 12_000.0,
        default: 3_000.0,
    },
    ValueFormat::FixedDecimals(0),
    SmoothingCategory::FrequencyLike,
);

/// FR-EQ-010 high band: +-15 dB.
pub const HIGH_SHELF_GAIN_DB: ParamDescriptor = ParamDescriptor::new(
    "eq.high_shelf_gain_db",
    "EQ High Shelf Gain",
    Unit::Decibels,
    ParamKind::Continuous {
        min: -15.0,
        max: 15.0,
        default: 0.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-EQ-010's "plus a defeatable high-pass ... filter as in FR-IR-070": off or 20..500 Hz.
pub const HIGH_PASS_ENABLED: ParamDescriptor = ParamDescriptor::new(
    "eq.high_pass_enabled",
    "EQ High-pass Enabled",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(0),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

/// FR-EQ-010's high-pass corner, 20..500 Hz, default 80 (as in FR-IR-070's low-cut range).
pub const HIGH_PASS_FREQ_HZ: ParamDescriptor = ParamDescriptor::new(
    "eq.high_pass_freq_hz",
    "EQ High-pass Freq",
    Unit::Hertz,
    ParamKind::Continuous {
        min: 20.0,
        max: 500.0,
        default: 80.0,
    },
    ValueFormat::FixedDecimals(0),
    SmoothingCategory::FrequencyLike,
);

/// FR-EQ-010's "plus a defeatable ... low-pass filter as in FR-IR-070": off or 1..20 kHz.
pub const LOW_PASS_ENABLED: ParamDescriptor = ParamDescriptor::new(
    "eq.low_pass_enabled",
    "EQ Low-pass Enabled",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(0),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

/// FR-EQ-010's low-pass corner, 1..20 kHz, default 18000 (as in FR-IR-070's high-cut range).
pub const LOW_PASS_FREQ_HZ: ParamDescriptor = ParamDescriptor::new(
    "eq.low_pass_freq_hz",
    "EQ Low-pass Freq",
    Unit::Hertz,
    ParamKind::Continuous {
        min: 1_000.0,
        max: 20_000.0,
        default: 18_000.0,
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
            LOW_SHELF_FREQ_HZ.key,
            LOW_SHELF_GAIN_DB.key,
            MID_FREQ_HZ.key,
            MID_GAIN_DB.key,
            MID_Q.key,
            HIGH_SHELF_FREQ_HZ.key,
            HIGH_SHELF_GAIN_DB.key,
            HIGH_PASS_ENABLED.key,
            HIGH_PASS_FREQ_HZ.key,
            LOW_PASS_ENABLED.key,
            LOW_PASS_FREQ_HZ.key,
        ];
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
