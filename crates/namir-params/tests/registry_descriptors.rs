//! FR-PARAM-010 and FR-PARAM-050 against the **shipped** [`namir_params::REGISTRY`].
//!
//! Both requirements had a covering test in `descriptor.rs`'s own `mod tests` from M1 until M14,
//! and neither test touched a shipped parameter: each made two `format_value` calls against a
//! `const` fabricated inside the test module, one of them (`out.channel_mode`) keyed to a
//! parameter that exists in no registry. Those tests are still there and still worth having —
//! they pin `format_value`'s rounding and clamping behaviour, which is unit-level behaviour of the
//! *function* — but they are not what either requirement asks for. FR-PARAM-010's method says
//! "enumerate parameters and assert the completeness of each descriptor", and the parameters to
//! enumerate are the ones the product ships.
//!
//! Two directions are needed and this file runs both, because either alone is passable by a
//! registry that is wrong:
//!
//! 1. **Specification → registry.** Every control the FRS §5 names must be present, with the range
//!    and default the FRS states. A test that reads only `REGISTRY` cannot notice a control that
//!    was never added, and "every continuous control identified in Section 5" is the half of
//!    FR-PARAM-010 that quantifies. The FRS rows are transcribed below as data, with the
//!    requirement id that states each one, so a reader can check them against the FRS by eye.
//! 2. **Registry → completeness.** Every entry in `REGISTRY`, including any added after this file
//!    was written, must carry all seven properties FR-PARAM-010 enumerates (or, for a stepped
//!    entry, FR-PARAM-050's named-values shape). This is what catches a *new* parameter landing
//!    with an empty name or a default outside its own range.

use namir_params::{
    ParamDescriptor, ParamId, ParamKind, REGISTRY, SmoothingCategory, Unit, ValueFormat,
};

// ------------------------------------------------------------------------------------------
// The FRS's own §5 tables, transcribed.
// ------------------------------------------------------------------------------------------

/// One continuous control FRS §5 names, and the figures the FRS states for it.
///
/// `min`/`max` are asserted as *coverage*, not equality — FR-IN-010 says "a range of **at least**
/// −24 dB to +24 dB", so a descriptor may be wider than the FRS row but never narrower. `default`
/// is asserted exactly. `None` means the FRS states no figure for that column and the descriptor's
/// own choice is unconstrained by it (FR-NAM-090 states no range for its offset; FR-CHAIN-090
/// states a default ceiling but no range; FR-IR-070's cut frequencies state a range but leave the
/// stored frequency's default to the implementation, the control itself defaulting to "off").
struct Section5Continuous {
    key: &'static str,
    min: Option<f32>,
    max: Option<f32>,
    default: Option<f32>,
    /// The requirement stating this row, quoted in the assertion message.
    source: &'static str,
}

const fn row(
    key: &'static str,
    min: Option<f32>,
    max: Option<f32>,
    default: Option<f32>,
    source: &'static str,
) -> Section5Continuous {
    Section5Continuous {
        key,
        min,
        max,
        default,
        source,
    }
}

/// Every continuous control FRS §5 identifies. Twenty rows, against `REGISTRY`'s twenty
/// `Continuous` entries — the two counts are asserted equal below, so a control added to one and
/// not the other fails rather than passing quietly.
const SECTION_5_CONTINUOUS: &[Section5Continuous] = &[
    row(
        "trim.gain_db",
        Some(-24.0),
        Some(24.0),
        Some(0.0),
        "FR-IN-010",
    ),
    row(
        "gate.threshold_db",
        Some(-100.0),
        Some(0.0),
        Some(-70.0),
        "FR-GATE-010",
    ),
    row(
        "gate.attack_ms",
        Some(0.1),
        Some(50.0),
        Some(1.0),
        "FR-GATE-010",
    ),
    row(
        "gate.hold_ms",
        Some(0.0),
        Some(500.0),
        Some(30.0),
        "FR-GATE-010",
    ),
    row(
        "gate.release_ms",
        Some(1.0),
        Some(2000.0),
        Some(100.0),
        "FR-GATE-010",
    ),
    row(
        "nam.normalize_offset_db",
        None,
        None,
        Some(0.0),
        "FR-NAM-090",
    ),
    row(
        "ir.level_db",
        Some(-24.0),
        Some(24.0),
        Some(0.0),
        "FR-IR-070",
    ),
    row(
        "ir.low_cut_freq_hz",
        Some(20.0),
        Some(500.0),
        None,
        "FR-IR-070",
    ),
    row(
        "ir.high_cut_freq_hz",
        Some(1_000.0),
        Some(20_000.0),
        None,
        "FR-IR-070",
    ),
    row(
        "eq.low_shelf_freq_hz",
        Some(40.0),
        Some(500.0),
        None,
        "FR-EQ-010",
    ),
    row(
        "eq.low_shelf_gain_db",
        Some(-15.0),
        Some(15.0),
        Some(0.0),
        "FR-EQ-010",
    ),
    row(
        "eq.mid_freq_hz",
        Some(200.0),
        Some(5_000.0),
        None,
        "FR-EQ-010",
    ),
    row(
        "eq.mid_gain_db",
        Some(-15.0),
        Some(15.0),
        Some(0.0),
        "FR-EQ-010",
    ),
    row("eq.mid_q", Some(0.2), Some(5.0), None, "FR-EQ-010"),
    row(
        "eq.high_shelf_freq_hz",
        Some(1_000.0),
        Some(12_000.0),
        None,
        "FR-EQ-010",
    ),
    row(
        "eq.high_shelf_gain_db",
        Some(-15.0),
        Some(15.0),
        Some(0.0),
        "FR-EQ-010",
    ),
    // FR-EQ-010's "plus a defeatable high-pass and low-pass filter as in FR-IR-070" — so these two
    // take FR-IR-070's cut ranges.
    row(
        "eq.high_pass_freq_hz",
        Some(20.0),
        Some(500.0),
        None,
        "FR-EQ-010 via FR-IR-070",
    ),
    row(
        "eq.low_pass_freq_hz",
        Some(1_000.0),
        Some(20_000.0),
        None,
        "FR-EQ-010 via FR-IR-070",
    ),
    row(
        "out.gain_db",
        Some(-60.0),
        Some(12.0),
        Some(0.0),
        "FR-OUT-010",
    ),
    row(
        "global.output_ceiling_db",
        None,
        None,
        Some(0.0),
        "FR-CHAIN-090",
    ),
];

/// Every discrete choice FRS §5 identifies, and the requirement stating it. FR-PARAM-050's
/// parenthetical names three categories — "enabled/disabled, filter type, channel mode" — and only
/// the first has any shipped control: the EQ's band shapes are fixed by FR-EQ-010's table rather
/// than user-selected, and channel mode is not a user control in 1.0 (FR-CHAIN-060 makes it a
/// property of the host's port layout, and FR-CHAIN-070's Should — the only requirement that would
/// have given the user a chooser — was dropped for 1.0 at M14's Phase 0). So the two remaining
/// categories are vacuous rather than unspanned, and the check that keeps them honest is
/// `no_discrete_looking_parameter_is_modelled_as_a_continuous_range` below.
const SECTION_5_DISCRETE: &[(&str, &str)] = &[
    ("trim.dc_blocker_enabled", "FR-IN-040"),
    ("gate.enabled", "FR-GATE-010"),
    ("nam.enabled", "FR-CHAIN-020"),
    ("nam.normalize_enabled", "FR-NAM-090"),
    ("ir.enabled", "FR-IR-070"),
    ("ir.low_cut_enabled", "FR-IR-070"),
    ("ir.high_cut_enabled", "FR-IR-070"),
    ("eq.enabled", "FR-CHAIN-020"),
    ("eq.high_pass_enabled", "FR-EQ-010"),
    ("eq.low_pass_enabled", "FR-EQ-010"),
    ("global.bypass", "FR-CHAIN-030"),
];

fn find(key: &str) -> &'static ParamDescriptor {
    REGISTRY
        .iter()
        .find(|d| d.key == key)
        .unwrap_or_else(|| panic!("REGISTRY has no parameter keyed {key:?}"))
}

fn continuous(d: &ParamDescriptor) -> Option<(f32, f32, f32)> {
    match d.kind {
        ParamKind::Continuous { min, max, default } => Some((min, max, default)),
        ParamKind::Stepped { .. } => None,
    }
}

// ------------------------------------------------------------------------------------------
// FR-PARAM-010.
// ------------------------------------------------------------------------------------------

// trace: FR-PARAM-010
#[test]
fn every_section_5_continuous_control_is_a_registry_parameter_with_its_frs_range_and_default() {
    let registry_continuous = REGISTRY.iter().filter(|d| continuous(d).is_some()).count();
    assert_eq!(
        registry_continuous,
        SECTION_5_CONTINUOUS.len(),
        "REGISTRY holds {registry_continuous} continuous parameters but FRS §5 identifies {} — \
         one of the two gained a control the other did not",
        SECTION_5_CONTINUOUS.len()
    );

    for spec in SECTION_5_CONTINUOUS {
        let d = find(spec.key);
        let (min, max, default) = continuous(d).unwrap_or_else(|| {
            panic!(
                "{} ({}) is a continuous control but is declared Stepped",
                spec.key, spec.source
            )
        });

        // "at least", per FR-IN-010's wording: wider than the FRS row is fine, narrower is not.
        if let Some(want) = spec.min {
            assert!(
                min <= want,
                "{} ({}): minimum {min} does not reach the FRS's {want}",
                spec.key,
                spec.source
            );
        }
        if let Some(want) = spec.max {
            assert!(
                max >= want,
                "{} ({}): maximum {max} does not reach the FRS's {want}",
                spec.key,
                spec.source
            );
        }
        if let Some(want) = spec.default {
            assert_eq!(
                default, want,
                "{} ({}): default {default} is not the FRS's {want}",
                spec.key, spec.source
            );
        }
    }
}

// trace: FR-PARAM-010
#[test]
fn every_shipped_continuous_descriptor_carries_all_seven_required_properties() {
    for d in REGISTRY {
        let Some((min, max, default)) = continuous(d) else {
            continue;
        };
        let key = d.key;

        // 1. A stable identifier, derived from the key and never set independently (D-10.2).
        assert_eq!(
            d.id,
            ParamId::from_key(key),
            "{key}: id is not the derivation of its own key"
        );
        assert_ne!(d.id.0, 0, "{key}: id is zero");

        // The key itself must be the namespaced `<stage>.<control>` shape D-10.1 requires, since
        // it is what the identifier is derived *from*.
        let (namespace, control) = key
            .split_once('.')
            .unwrap_or_else(|| panic!("{key}: not a namespaced <stage>.<control> key"));
        assert!(!namespace.is_empty() && !control.is_empty(), "{key}");
        assert!(
            !control.contains('.'),
            "{key}: more than one namespace separator"
        );
        assert!(
            key.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.'),
            "{key}: keys are lowercase ascii, digits, underscore and one dot"
        );

        // 2. A human-readable name — which is to say, not the key, and not empty.
        assert!(!d.name.is_empty(), "{key}: empty name");
        assert_ne!(d.name, key, "{key}: name is just the key");
        assert!(
            !d.name.contains('.') && !d.name.contains('_'),
            "{key}: name {:?} still reads as an identifier",
            d.name
        );
        assert!(
            d.name.starts_with(|c: char| c.is_ascii_uppercase()),
            "{key}: name {:?} is not capitalised for display",
            d.name
        );

        // 3. A unit — and, where the key states the physical quantity in its own suffix, the one
        // the suffix names. A `_db` parameter declared `Unit::Hertz` is exactly the kind of
        // copy-paste slip an enumerate-and-check test exists to catch.
        let expected_unit = match () {
            _ if key.ends_with("_db") => Some(Unit::Decibels),
            _ if key.ends_with("_hz") => Some(Unit::Hertz),
            _ if key.ends_with("_ms") => Some(Unit::Milliseconds),
            _ => None,
        };
        if let Some(expected) = expected_unit {
            assert_eq!(
                d.unit, expected,
                "{key}: unit does not match the key suffix"
            );
        }

        // 4/5/6. A minimum, a maximum and a default, all finite and mutually consistent.
        assert!(
            min.is_finite() && max.is_finite(),
            "{key}: non-finite range"
        );
        assert!(min < max, "{key}: minimum {min} is not below maximum {max}");
        assert!(default.is_finite(), "{key}: non-finite default");
        assert!(
            (min..=max).contains(&default),
            "{key}: default {default} lies outside {min}..={max}"
        );

        // 7. A value-to-text formatting rule, exercised at all three stated points rather than
        // merely declared. `Named` on a continuous parameter is the mismatch `format_value`'s own
        // doc comment says it tolerates at runtime and this crate's tests are meant to prevent.
        let ValueFormat::FixedDecimals(places) = d.format else {
            panic!("{key}: a continuous parameter formatted as Named");
        };
        for value in [min, default, max] {
            let text = d.format_value(value);
            assert!(!text.is_empty(), "{key}: empty rendering of {value}");
            let parsed: f32 = text
                .parse()
                .unwrap_or_else(|e| panic!("{key}: {text:?} does not parse back as a number: {e}"));
            // The rendering must round-trip to within its own declared resolution, so a format
            // that silently truncated the value would fail rather than merely look tidy.
            let resolution = 10f32.powi(-(places as i32));
            assert!(
                (parsed - value).abs() <= resolution,
                "{key}: {value} renders as {text:?}, which is further than one unit of its own \
                 {places}-decimal resolution away"
            );
        }

        // D-10.3: a declared smoothing category, and never `Stepped` on a continuous range.
        assert_ne!(
            d.smoothing,
            SmoothingCategory::Stepped,
            "{key}: a continuous parameter declaring stepped smoothing"
        );
    }
}

// ------------------------------------------------------------------------------------------
// FR-PARAM-050.
// ------------------------------------------------------------------------------------------

// trace: FR-PARAM-050
#[test]
fn every_section_5_discrete_choice_is_a_stepped_parameter_with_named_values() {
    let registry_stepped = REGISTRY.iter().filter(|d| continuous(d).is_none()).count();
    assert_eq!(
        registry_stepped,
        SECTION_5_DISCRETE.len(),
        "REGISTRY holds {registry_stepped} stepped parameters but FRS §5 identifies {} discrete \
         choices",
        SECTION_5_DISCRETE.len()
    );

    for (key, source) in SECTION_5_DISCRETE {
        let d = find(key);
        let ParamKind::Stepped {
            values,
            default_index,
        } = d.kind
        else {
            panic!("{key} ({source}): a discrete choice exposed as a continuous range");
        };

        assert!(
            values.len() >= 2,
            "{key} ({source}): a choice needs at least two options, has {}",
            values.len()
        );
        for (i, v) in values.iter().enumerate() {
            assert!(!v.is_empty(), "{key}: option {i} has no name");
            // "with named values": a name, not a number wearing one.
            assert!(
                v.parse::<f64>().is_err(),
                "{key}: option {i} is named {v:?}, which is a number, not a name"
            );
        }
        for (i, v) in values.iter().enumerate() {
            for w in &values[i + 1..] {
                assert_ne!(v, w, "{key}: two options share the name {v:?}");
            }
        }
        assert!(
            (default_index.0 as usize) < values.len(),
            "{key}: default index {} is outside 0..{}",
            default_index.0,
            values.len()
        );

        // The formatting rule that makes the values *named* in practice: every index must render
        // as its own option's name, not as a number.
        assert_eq!(
            d.format,
            ValueFormat::Named,
            "{key}: stepped, but formatted as a number"
        );
        for (i, want) in values.iter().enumerate() {
            assert_eq!(
                &d.format_value(i as f32),
                want,
                "{key}: index {i} does not render as its own name"
            );
        }

        assert_eq!(
            d.smoothing,
            SmoothingCategory::Stepped,
            "{key}: D-10.3 gives a stepped parameter stepped smoothing"
        );
        assert_eq!(d.id, ParamId::from_key(key), "{key}: id is not derived");
    }
}

/// FR-PARAM-050's actual prohibition — "not as continuous ranges" — read against the registry
/// rather than against the list above, so a *future* on/off control that lands as a 0..1
/// `Continuous` fails here even though nobody remembered to add it to `SECTION_5_DISCRETE`.
#[test]
fn no_discrete_looking_parameter_is_modelled_as_a_continuous_range() {
    for d in REGISTRY {
        let looks_discrete = d.key.ends_with("_enabled") || d.key.ends_with(".bypass");
        if looks_discrete {
            assert!(
                continuous(d).is_none(),
                "{}: an on/off control pressed into a continuous range",
                d.key
            );
        }
    }
}
