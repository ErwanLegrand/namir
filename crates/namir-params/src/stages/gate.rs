//! Noise gate stage descriptors (FR-GATE-010). D-10.1: declared once, here, per stage.

use crate::descriptor::{
    ParamDescriptor, ParamKind, SmoothingCategory, StepIndex, Unit, ValueFormat,
};

/// FR-GATE-010's "Enabled" control; also the FR-CHAIN-020 per-stage bypass for this stage.
pub const ENABLED: ParamDescriptor = ParamDescriptor::new(
    "gate.enabled",
    "Gate Enabled",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(1),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

/// FR-GATE-010: -100..0 dBFS, default -70.
pub const THRESHOLD_DB: ParamDescriptor = ParamDescriptor::new(
    "gate.threshold_db",
    "Gate Threshold",
    Unit::Decibels,
    ParamKind::Continuous {
        min: -100.0,
        max: 0.0,
        default: -70.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-GATE-010: 0.1..50 ms, default 1.
pub const ATTACK_MS: ParamDescriptor = ParamDescriptor::new(
    "gate.attack_ms",
    "Gate Attack",
    Unit::Milliseconds,
    ParamKind::Continuous {
        min: 0.1,
        max: 50.0,
        default: 1.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-GATE-010: 0..500 ms, default 30.
pub const HOLD_MS: ParamDescriptor = ParamDescriptor::new(
    "gate.hold_ms",
    "Gate Hold",
    Unit::Milliseconds,
    ParamKind::Continuous {
        min: 0.0,
        max: 500.0,
        default: 30.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-GATE-010: 1..2000 ms, default 100.
pub const RELEASE_MS: ParamDescriptor = ParamDescriptor::new(
    "gate.release_ms",
    "Gate Release",
    Unit::Milliseconds,
    ParamKind::Continuous {
        min: 1.0,
        max: 2000.0,
        default: 100.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_have_distinct_keys() {
        let keys = [
            ENABLED.key,
            THRESHOLD_DB.key,
            ATTACK_MS.key,
            HOLD_MS.key,
            RELEASE_MS.key,
        ];
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
