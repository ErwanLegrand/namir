//! NAM model stage descriptors. D-10.1: declared once, here, per stage.
//!
//! Model selection/loading itself is not a `ParamDescriptor` (it is a file/resource load, per
//! D-8.1, not an automatable continuous/stepped value).
//!
//! FR-NAM-090/100 (loudness normalisation and calibration) were out of scope through M9 per
//! `03-implementation-roadmap.md` §6 — the `.nam` schema `namir-nam` read carried no loudness
//! metadata to normalise against. D-9.12's *Consequence — FR-NAM-090/100 stop being blocked*
//! (`docs/02-architecture.md` §9) removed that blocker: A2-era files carry `metadata.loudness`
//! (an integrated-loudness figure in LUFS). M10 closes FR-NAM-090 (below): [`NORMALIZE_ENABLED`]
//! and [`NORMALIZE_OFFSET_DB`], plus [`TARGET_LOUDNESS_LUFS`], the reference every model's
//! declared loudness is normalised toward. **FR-NAM-100 (dBu-calibrated operating levels and
//! interface sensitivity) stays out of scope** — a separate, Should-priority requirement reading
//! `input_level_dbu`/`output_level_dbu`, neither of which anything in this crate or
//! `namir-engine`'s Nam stage reads.

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

/// FR-NAM-090: "The user shall be able to disable this normalisation" — independent of [`ENABLED`]
/// (FR-CHAIN-020's stage bypass, which turns the model itself off): disabling normalisation still
/// runs the loaded model, it only stops applying the corrective gain [`TARGET_LOUDNESS_LUFS`]
/// implies. Defaults on, per the requirement's own framing ("shall apply... so that models... are
/// perceived at comparable level when swapped" is the ordinary behavior; disabling it is the
/// user's opt-out, not the default).
pub const NORMALIZE_ENABLED: ParamDescriptor = ParamDescriptor::new(
    "nam.normalize_enabled",
    "Loudness Normalize",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(1),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

/// FR-NAM-090: "The user shall be able to... offset it" — a trim added to the computed
/// normalisation gain, so a user who finds the automatic correction slightly off in either
/// direction can nudge it without disabling normalisation outright. `Unit::Decibels` rather than
/// a distinct "LU" unit: this crate declares no such unit (see [`Unit`]'s own variant list), and a
/// *relative* loudness offset in LU is numerically identical to a dB gain offset (ITU-R BS.1770's
/// loudness unit is defined as one dB of level difference on the integrated-loudness scale) — so
/// this reuses `Unit::Decibels` for consistency with every other gain-shaped parameter in this
/// crate (`trim.gain_db`, `out.gain_db`, `ir.level_db`) rather than inventing a unit that would
/// mean exactly the same thing. Range and default mirror `trim.gain_db`'s shape: symmetric around
/// zero, `0.0` default (no offset until the user asks for one).
pub const NORMALIZE_OFFSET_DB: ParamDescriptor = ParamDescriptor::new(
    "nam.normalize_offset_db",
    "Loudness Offset",
    Unit::Decibels,
    ParamKind::Continuous {
        min: -12.0,
        max: 12.0,
        default: 0.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// FR-NAM-090's reference/target loudness: every loaded model's declared `metadata.loudness`
/// (LUFS, `namir_nam::NamMetadata::loudness`) is normalised toward this figure, so that two models
/// of differing declared loudness end up at comparable perceived level when swapped, per the
/// requirement's own wording. `-18` LUFS is a commonly used production/broadcast reference point —
/// roughly midway between EBU R128's broadcast target (-23 LUFS) and typical streaming-platform
/// targets (around -14 LUFS) — chosen because Namir has no loudness convention of its own to
/// anchor to and this sits centrally in the range a real `.nam` export's declared `loudness` is
/// likely to fall in. Not itself an FRS figure; this project's own documented choice, the way
/// `out::SILENCE_FLOOR_DB` is. Shared here so `namir-engine`'s Nam stage and this crate's own
/// tests agree on one number rather than each hand-copying `-18.0`.
pub const TARGET_LOUDNESS_LUFS: f32 = -18.0;

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

    #[test]
    fn normalize_enabled_defaults_on() {
        assert_eq!(
            NORMALIZE_ENABLED.kind,
            ParamKind::Stepped {
                values: &["Off", "On"],
                default_index: StepIndex(1),
            }
        );
    }

    #[test]
    fn normalize_offset_defaults_to_zero() {
        let ParamKind::Continuous { default, .. } = NORMALIZE_OFFSET_DB.kind else {
            panic!("expected Continuous");
        };
        assert_eq!(default, 0.0);
    }

    #[test]
    fn descriptors_have_distinct_keys() {
        let keys = [ENABLED.key, NORMALIZE_ENABLED.key, NORMALIZE_OFFSET_DB.key];
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
