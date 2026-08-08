//! FR-UI-040's "accept a typed numeric value" side, for both a [`ParamKind::Continuous`] and a
//! [`ParamKind::Stepped`] descriptor -- the display half (`ParamDescriptor::format_value`) already
//! lives in `namir-params`; this module is only the *parse* direction, which that crate has no
//! reason to own (parsing free-typed text is a UI concern, not a parameter-identity one).
//!
//! Pure, `egui`-free functions so they're unit-testable without a `Ui`/`Context` at all --
//! [`crate::controls::param_control`] is a thin wrapper handing these to `egui::DragValue`'s own
//! `custom_parser`/`custom_formatter` hooks.

use namir_params::{ParamDescriptor, ParamKind};

/// Parses `text` into a value valid for `descriptor`, or `None` if `text` names neither a number
/// nor (for a [`ParamKind::Stepped`] descriptor) one of its named values.
///
/// - [`ParamKind::Continuous`]: a plain decimal number, clamped to `min..=max`.
/// - [`ParamKind::Stepped`]: either one of `values` (case-insensitively, so a user can type "on"
///   for a control whose canonical spelling is "On"), or a plain step index, clamped to
///   `0..=values.len() - 1` and rounded to the nearest whole step.
///
/// Returning `f64` (not `f32`) matches `egui::DragValue::custom_parser`'s own signature, which is
/// the only caller this function has.
pub fn parse_value(descriptor: &ParamDescriptor, text: &str) -> Option<f64> {
    let trimmed = text.trim();
    match descriptor.kind {
        ParamKind::Continuous { min, max, .. } => trimmed
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(min as f64, max as f64)),
        ParamKind::Stepped { values, .. } => {
            if let Some(index) = values.iter().position(|v| v.eq_ignore_ascii_case(trimmed)) {
                return Some(index as f64);
            }
            let max_index = values.len().saturating_sub(1) as f64;
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .map(|v| v.round().clamp(0.0, max_index))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_params::stages::{gate, trim};

    // trace: FR-UI-040
    #[test]
    fn continuous_parses_a_plain_number() {
        assert_eq!(parse_value(&trim::GAIN_DB, "6.0"), Some(6.0));
    }

    #[test]
    fn continuous_clamps_above_range() {
        assert_eq!(parse_value(&trim::GAIN_DB, "999"), Some(24.0));
    }

    #[test]
    fn continuous_clamps_below_range() {
        assert_eq!(parse_value(&trim::GAIN_DB, "-999"), Some(-24.0));
    }

    #[test]
    fn continuous_rejects_non_numeric_text() {
        assert_eq!(parse_value(&trim::GAIN_DB, "loud"), None);
    }

    #[test]
    fn continuous_rejects_nan_and_infinity_spellings() {
        assert_eq!(parse_value(&trim::GAIN_DB, "NaN"), None);
        assert_eq!(parse_value(&trim::GAIN_DB, "inf"), None);
    }

    #[test]
    fn continuous_trims_surrounding_whitespace() {
        assert_eq!(parse_value(&trim::GAIN_DB, "  3.5  "), Some(3.5));
    }

    #[test]
    fn stepped_parses_an_exact_named_value() {
        assert_eq!(parse_value(&gate::ENABLED, "On"), Some(1.0));
        assert_eq!(parse_value(&gate::ENABLED, "Off"), Some(0.0));
    }

    #[test]
    fn stepped_parses_a_named_value_case_insensitively() {
        assert_eq!(parse_value(&gate::ENABLED, "on"), Some(1.0));
        assert_eq!(parse_value(&gate::ENABLED, "OFF"), Some(0.0));
    }

    // trace: FR-UI-040
    #[test]
    fn stepped_parses_a_raw_step_index() {
        assert_eq!(parse_value(&gate::ENABLED, "1"), Some(1.0));
        assert_eq!(parse_value(&gate::ENABLED, "0"), Some(0.0));
    }

    #[test]
    fn stepped_clamps_an_out_of_range_index() {
        assert_eq!(parse_value(&gate::ENABLED, "99"), Some(1.0));
        assert_eq!(parse_value(&gate::ENABLED, "-5"), Some(0.0));
    }

    #[test]
    fn stepped_rejects_an_unrecognised_word() {
        assert_eq!(parse_value(&gate::ENABLED, "sideways"), None);
    }

    #[test]
    fn stepped_rounds_a_fractional_index_to_the_nearest_step() {
        assert_eq!(parse_value(&gate::ENABLED, "0.6"), Some(1.0));
        assert_eq!(parse_value(&gate::ENABLED, "0.4"), Some(0.0));
    }
}
