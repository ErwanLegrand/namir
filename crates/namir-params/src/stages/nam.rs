//! NAM model stage descriptors. D-10.1: declared once, here, per stage.
//!
//! Model selection/loading itself is not a `ParamDescriptor` (it is a file/resource load, per
//! D-8.1, not an automatable continuous/stepped value). FR-NAM-090/100 (loudness normalisation
//! and calibration) are out of scope for M2 per `03-implementation-roadmap.md` §6 — the current
//! `.nam` schema `namir-nam` reads carries no loudness metadata to normalise against yet.

use crate::SmoothingCategory;
use crate::descriptor::{ParamDescriptor, ParamKind, StepIndex, Unit, ValueFormat};

/// The FR-CHAIN-020 per-stage bypass for the NAM stage.
pub const ENABLED: ParamDescriptor = ParamDescriptor::new(
    "nam.enabled",
    "NAM Enabled",
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
    fn enabled_defaults_on() {
        assert_eq!(
            ENABLED.kind,
            ParamKind::Stepped {
                values: &["Off", "On"],
                default_index: StepIndex(1),
            }
        );
    }
}
