//! FR-PARAM-010: "Every continuous control ... shall be exposed as an automatable parameter with
//! a stable identifier, a human-readable name, a unit, a minimum, a maximum, a default and a
//! value-to-text formatting rule." FR-PARAM-050 adds the stepped/discrete shape. D-10.3 assigns
//! each descriptor a smoothing category rather than leaving smoothing open-coded per stage.

use crate::id::ParamId;

/// The physical unit a continuous parameter's value is expressed in. Deliberately a plain
/// enumeration with no conversion logic attached — unit *display* is this crate's job (FR-PARAM-
/// 010's "value-to-text formatting rule"); unit *conversion* (e.g. semitones to a frequency
/// ratio) belongs to whichever stage's DSP actually needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Decibels.
    Decibels,
    /// Hertz.
    Hertz,
    /// Milliseconds.
    Milliseconds,
    /// A dimensionless ratio (e.g. a mix or blend control), distinct from [`Unit::Percent`]
    /// purely in how it's printed (`0.50` vs `50%`), not in range or meaning.
    Ratio,
    /// A percentage.
    Percent,
    /// Semitones.
    Semitones,
    /// No unit — used by parameters whose value is inherently unitless (e.g. a filter's Q).
    None,
}

/// How a parameter's value renders as text, per FR-PARAM-010. Intentionally a closed set of the
/// handful of shapes this product's parameters actually need, not a generic format-string or
/// closure engine — there is exactly one product to serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueFormat {
    /// Render a continuous value with a fixed number of decimal places (e.g. `FixedDecimals(1)`
    /// renders `-6.0` for a dB value).
    FixedDecimals(u8),
    /// Render a stepped value by looking up its named value in [`ParamKind::Stepped`]'s `values`
    /// slice, rather than printing a number at all.
    Named,
}

/// A newtype over a stepped parameter's selected index (FR-PARAM-050's "named values" carrier).
/// Exists so a future consumer has a value-representation type for a chosen stepped option that
/// isn't `f32` — `namir_engine::ParamChange.value` is `f32`-only today precisely because this
/// type didn't exist yet (see this crate's top doc comment for that scope boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StepIndex(pub u32);

/// The shape a parameter's value space takes. FR-PARAM-050: discrete choices are `Stepped`, not
/// a `Continuous` range pressed into service with rounding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamKind {
    /// A continuous range, per FR-PARAM-010.
    Continuous {
        /// Lowest value the parameter can take.
        min: f32,
        /// Highest value the parameter can take.
        max: f32,
        /// Value the parameter starts at.
        default: f32,
    },
    /// A discrete set of named choices, per FR-PARAM-050.
    Stepped {
        /// Named values in index order; index 0 is `values[0]`, etc. `&'static` so descriptors
        /// stay `const`-constructible data, matching `namir_core::ErrorCode`'s style.
        values: &'static [&'static str],
        /// Index into `values` the parameter starts at.
        default_index: StepIndex,
    },
}

/// D-10.3: smoothing is declared per parameter, not open-coded per stage. Each variant names the
/// `namir-dsp` primitive a future stage implementation reaches for; this crate only declares the
/// category; it does not depend on `namir-dsp` or perform any smoothing itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothingCategory {
    /// A gain-like parameter (e.g. input trim, output level): a future stage smooths it with a
    /// one-pole ramp, `namir_dsp::GainRamp`.
    GainLike,
    /// A frequency-like parameter (e.g. a filter cutoff or Q): a future stage interpolates
    /// `namir_dsp::Biquad` coefficients per block rather than stepping them instantaneously.
    FrequencyLike,
    /// A stepped parameter (FR-PARAM-050): a future stage crossfades between states or defers the
    /// change to a click-free switch point (e.g. a zero crossing), never jumps mid-block.
    Stepped,
}

/// A parameter's full identity (FR-PARAM-010/050, D-10.1/D-10.2/D-10.3). One `ParamDescriptor`
/// per control; the full set a build knows about is [`crate::REGISTRY`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamDescriptor {
    /// The namespaced source-of-truth string (e.g. `"gate.threshold"`). `id` is derived from
    /// this and kept in sync by [`ParamDescriptor::new`] — never set independently.
    pub key: &'static str,
    /// The stable `u32` identifier hosts see (D-10.2), derived from `key` by
    /// [`ParamDescriptor::new`].
    pub id: ParamId,
    /// D-10.2's consequence for RD-2: reserved for the future dynamic chain, where more than one
    /// instance of the same stage kind can exist and each instance's parameters need to stay
    /// distinct without renumbering every other parameter. Always `0` until RD-2 lands; present
    /// now so that landing it later is additive, not a renumbering of every existing id.
    pub stage_instance: u32,
    /// Human-readable display name (FR-PARAM-010).
    pub name: &'static str,
    /// The physical unit `format_value` and any future UI should present this parameter in.
    pub unit: Unit,
    /// The parameter's value space; see [`ParamKind`].
    pub kind: ParamKind,
    /// How `format_value` renders this parameter's value as text.
    pub format: ValueFormat,
    /// Which `namir-dsp` smoothing strategy a future stage should apply to this parameter
    /// (D-10.3); see [`SmoothingCategory`].
    pub smoothing: SmoothingCategory,
}

impl ParamDescriptor {
    /// Builds a descriptor, deriving `id` from `key` (D-10.2) and zeroing `stage_instance`
    /// (D-10.2's consequence for RD-2). `const fn` so descriptors can be declared as `const`s the
    /// way `namir_core::ErrorCode` consts are.
    pub const fn new(
        key: &'static str,
        name: &'static str,
        unit: Unit,
        kind: ParamKind,
        format: ValueFormat,
        smoothing: SmoothingCategory,
    ) -> ParamDescriptor {
        ParamDescriptor {
            key,
            id: ParamId::from_key(key),
            stage_instance: 0,
            name,
            unit,
            kind,
            format,
            smoothing,
        }
    }

    /// Renders `value` as text per this descriptor's [`ValueFormat`] (FR-PARAM-010). `value` is
    /// in the parameter's own space: the raw `f32` for `Continuous`, or a step index (as `f32`,
    /// rounded and clamped) for `Stepped`. A format/kind pairing this crate didn't intend (e.g.
    /// `Named` on a `Continuous` kind) falls back to a plain numeric render rather than panicking
    /// — this is a display convenience, not a validity gate; [`crate::manifest::check_manifest`]
    /// and this crate's own tests are what actually keep descriptors internally consistent.
    pub fn format_value(&self, value: f32) -> String {
        match (self.format, self.kind) {
            (ValueFormat::FixedDecimals(places), _) => format!("{:.*}", places as usize, value),
            (ValueFormat::Named, ParamKind::Stepped { values, .. }) if !values.is_empty() => {
                let max_index = (values.len() - 1) as f32;
                let index = value.round().clamp(0.0, max_index) as usize;
                values[index].to_string()
            }
            (ValueFormat::Named, _) => format!("{value}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIM: ParamDescriptor = ParamDescriptor::new(
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

    const CHANNEL_MODE: ParamDescriptor = ParamDescriptor::new(
        "out.channel_mode",
        "Channel Mode",
        Unit::None,
        ParamKind::Stepped {
            values: &["Mono", "Stereo", "Dual Mono"],
            default_index: StepIndex(0),
        },
        ValueFormat::Named,
        SmoothingCategory::Stepped,
    );

    #[test]
    fn id_is_derived_from_key_not_set_independently() {
        assert_eq!(TRIM.id, ParamId::from_key("trim.gain_db"));
    }

    #[test]
    fn stage_instance_is_zero() {
        assert_eq!(TRIM.stage_instance, 0);
        assert_eq!(CHANNEL_MODE.stage_instance, 0);
    }

    // The two consts above are deliberately fabricated, and the tests below are deliberately about
    // `format_value` itself — its rounding, its clamping, its fallbacks — rather than about any
    // shipped parameter. FR-PARAM-010's and FR-PARAM-050's tags used to live here, which was the
    // over-claim M14 removed: neither requirement is about this function, both are about the
    // descriptors the product ships. Those tags are now on
    // `crates/namir-params/tests/registry_descriptors.rs`, which enumerates `REGISTRY`.

    #[test]
    fn continuous_formats_with_fixed_decimals() {
        assert_eq!(TRIM.format_value(-6.0), "-6.0");
        assert_eq!(TRIM.format_value(0.049), "0.0");
    }

    #[test]
    fn stepped_formats_via_named_values() {
        assert_eq!(CHANNEL_MODE.format_value(0.0), "Mono");
        assert_eq!(CHANNEL_MODE.format_value(2.0), "Dual Mono");
    }

    #[test]
    fn stepped_formatting_clamps_out_of_range_indices() {
        assert_eq!(CHANNEL_MODE.format_value(-5.0), "Mono");
        assert_eq!(CHANNEL_MODE.format_value(99.0), "Dual Mono");
    }

    #[test]
    fn stepped_formatting_rounds_to_nearest_index() {
        assert_eq!(CHANNEL_MODE.format_value(1.4), "Stereo");
        assert_eq!(CHANNEL_MODE.format_value(1.6), "Dual Mono");
    }
}
