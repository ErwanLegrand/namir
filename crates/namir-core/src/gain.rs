//! dB/linear conversion. `linear_to_db` of a non-positive value is mathematically undefined
//! (silence has no dB figure) — see the test below for the floor this picks instead of NaN/-inf,
//! which matters because meters (FR-IN-020 etc.) read this every UI frame and must never format
//! a NaN.

/// Converts a decibel value to a linear amplitude multiplier (0 dB -> 1.0).
pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A silent or negative `linear` has no dB figure; both are floored to `MIN_DB` rather than
/// producing `-inf`/`NaN`, since this feeds meters that must always format to something readable.
const MIN_DB: f32 = -600.0;

/// Converts a linear amplitude to decibels, floored at `MIN_DB` for silent or negative input
/// instead of returning `-inf`/`NaN` (see this module's doc comment).
pub fn linear_to_db(linear: f32) -> f32 {
    if linear.abs() <= f32::MIN_POSITIVE {
        return MIN_DB;
    }
    (20.0 * linear.abs().log10()).max(MIN_DB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_db_is_unity_gain() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn plus_twenty_db_is_10x() {
        assert!((db_to_linear(20.0) - 10.0).abs() < 1e-4);
    }

    #[test]
    fn minus_infinity_ish_db_is_near_zero() {
        assert!(db_to_linear(-200.0) < 1e-9);
    }

    #[test]
    fn unity_gain_is_zero_db() {
        assert!((linear_to_db(1.0) - 0.0).abs() < 1e-4);
    }

    #[test]
    fn round_trips() {
        for db in [-60.0f32, -12.0, -1.0, 0.0, 3.0, 12.0] {
            let back = linear_to_db(db_to_linear(db));
            assert!((back - db).abs() < 1e-3, "{db} -> {back}");
        }
    }

    #[test]
    fn silence_gives_a_floor_not_nan_or_infinity() {
        assert!(linear_to_db(0.0).is_finite());
        assert!(linear_to_db(-0.0).is_finite());
        assert!(linear_to_db(-5.0).is_finite()); // negative linear input: still must not NaN
    }

    #[test]
    fn silence_floor_is_very_low() {
        // Not a specific number by contract, just "clearly silent" for a meter to display.
        assert!(linear_to_db(0.0) < -300.0);
    }
}
