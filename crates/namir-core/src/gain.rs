//! dB/linear conversion. `linear_to_db` of zero is mathematically undefined (silence has no dB
//! figure) — see the test below for the floor this picks instead of `-inf`, which matters because
//! meters (FR-IN-020 etc.) read this every UI frame and must never format a NaN or an infinity.
//!
//! # The never-format-a-NaN contract is by construction, not by arithmetic (issue #129)
//!
//! That contract used to hold only as a side effect of `f32::max`: `NaN.abs()` is NaN, so a NaN
//! amplitude reached `(20.0 * NaN.log10()).max(MIN_DB)`, and `max` returning *the other operand*
//! when one is NaN rescued it to `MIN_DB`. Nothing said so and no test covered it, so rewriting
//! that line as the equivalent-looking `if x < MIN_DB { MIN_DB } else { x }` would have leaked a
//! NaN to the UI while passing every test in the file. The non-finite cases are now handled
//! before any arithmetic runs, and pinned by their own tests.
//!
//! The floor is not the answer for every non-finite input, though. A NaN amplitude is a broken
//! signal with no level to report, so it reads as silence; an *infinite* one has blown up, and
//! reporting that as -600 dB would be a wrong reading the user cannot see — the same
//! silently-wrong reading issue #129 objects to in `namir-dsp`'s `Meter::peak`. So an infinity
//! clamps to `MAX_DB` at the top instead, keeping the reading monotonic in the magnitude.

/// Converts a decibel value to a linear amplitude multiplier (0 dB -> 1.0).
pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// The reading for an amplitude with no dB figure of its own: a magnitude at or below
/// `f32::MIN_POSITIVE` (silence, either sign) and a `NaN`. Chosen over `-inf`/`NaN` because this
/// feeds meters that must always format to something readable.
const MIN_DB: f32 = -600.0;

/// The ceiling, reported for an infinite magnitude. The counterpart to `MIN_DB` at the top end:
/// see this module's doc comment for why an infinity is not folded into the floor.
const MAX_DB: f32 = 600.0;

/// Converts a linear amplitude to decibels, taking its **magnitude** — `linear_to_db(-5.0)` is
/// `linear_to_db(5.0)`, about +14 dB, not the floor. A single negative sample is a signal at that
/// level, not silence (issue #128).
///
/// The result is always finite and always within `MIN_DB..=MAX_DB`: a magnitude at or below
/// `f32::MIN_POSITIVE`, and a `NaN`, read as `MIN_DB`; an infinite magnitude reads as `MAX_DB`.
/// See this module's doc comment for why that is a construction rather than a coincidence.
pub fn linear_to_db(linear: f32) -> f32 {
    let magnitude = linear.abs();
    // Ordered so that no non-finite value reaches the arithmetic below, and so that neither
    // branch relies on a NaN-propagation rule to be correct. `is_nan` is checked first because a
    // NaN compares false against every bound, and would otherwise fall through to `log10`.
    if magnitude.is_nan() || magnitude <= f32::MIN_POSITIVE {
        return MIN_DB;
    }
    if magnitude.is_infinite() {
        return MAX_DB;
    }
    (20.0 * magnitude.log10()).clamp(MIN_DB, MAX_DB)
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

    /// Issue #128: the doc comment used to claim a negative `linear` floors to `MIN_DB`. It does
    /// not and should not — the magnitude is taken, so a lone negative sample reads at its own
    /// level rather than as silence. Pinned by equality, not by `is_finite`, which passed either
    /// way and is what let the documentation drift from the code unnoticed.
    #[test]
    fn a_negative_linear_reads_as_its_magnitude_not_as_silence() {
        assert_eq!(linear_to_db(-5.0), linear_to_db(5.0));
        assert!(
            linear_to_db(-5.0) > 0.0,
            "-5.0 is a magnitude of 5, which is a positive dB figure: {}",
            linear_to_db(-5.0)
        );
    }

    /// Issue #129: the module's "must never format a NaN" contract, asserted directly rather than
    /// left to `f32::max`'s NaN-returns-the-other-operand semantics. Every one of these inputs
    /// reaches a meter's `format` in the UI thread if a stage ever emits one.
    #[test]
    fn no_amplitude_at_all_produces_a_non_finite_reading() {
        for linear in [
            f32::NAN,
            -f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.0,
            -0.0,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            1e-45, // sub-normal
            f32::MAX,
            f32::MIN,
            -5.0,
        ] {
            let db = linear_to_db(linear);
            assert!(db.is_finite(), "linear_to_db({linear}) = {db}");
            assert!(
                (MIN_DB..=MAX_DB).contains(&db),
                "linear_to_db({linear}) = {db}"
            );
        }
    }

    /// The half of issue #129 that held only by accident: a NaN amplitude is a broken signal, and
    /// the reading a meter shows for it is the silence floor rather than a NaN the UI would have
    /// to special-case.
    #[test]
    fn a_nan_amplitude_reads_as_the_silence_floor() {
        assert_eq!(linear_to_db(f32::NAN), MIN_DB);
        assert_eq!(linear_to_db(-f32::NAN), MIN_DB);
    }

    /// The other half: an infinite amplitude is clamped at the *top*, not folded into the silence
    /// floor. A meter reading -600 dB for a signal that has blown up would be a wrong reading a
    /// user cannot see, which is the failure mode issue #129 objects to downstream in `Meter`.
    /// The ceiling is a clamp, so a large finite magnitude reaches it too — what is pinned here is
    /// that "louder than anything a meter will ever show" and "silent" stay on opposite ends.
    #[test]
    fn an_infinite_amplitude_reads_at_the_ceiling_not_at_the_floor() {
        assert_eq!(linear_to_db(f32::INFINITY), MAX_DB);
        assert_eq!(linear_to_db(f32::NEG_INFINITY), MAX_DB);
        assert!(linear_to_db(f32::INFINITY) > linear_to_db(1000.0));
        assert!(linear_to_db(f32::INFINITY) > linear_to_db(0.0));
    }

    #[test]
    fn silence_floor_is_very_low() {
        // Not a specific number by contract, just "clearly silent" for a meter to display.
        assert!(linear_to_db(0.0) < -300.0);
    }
}
