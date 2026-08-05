//! D-10.1: "Parameters are declared in one place per stage as static descriptors, and the full
//! set is emitted at build time into a checked-in parameter manifest (`params.lock`)... The
//! manifest is diffed in CI: adding a parameter is allowed, changing or removing an existing
//! entry's identifier or type fails the build, and retiring one requires an explicit tombstone
//! entry."
//!
//! # Format
//!
//! Plain text, not JSON — a lockfile in `Cargo.lock`'s sense (line-oriented, diffable, never
//! spuriously reordered), not a data-interchange document. A header comment block, a
//! `format_version` line, then one line per parameter, sorted by key, each
//! `<key> <id> <kind> <live|tombstoned>`. Tombstoned lines are never deleted — that is the entire
//! point of a tombstone (D-10.1).
//!
//! [`render_manifest`] only ever emits `live` lines: it renders the current, in-source descriptor
//! set, which by construction contains no tombstones (a tombstoned parameter has no descriptor
//! left to render). Tombstone lines enter `params.lock` when a parameter is retired — its
//! descriptor is deleted from source and its manifest line is hand-flipped from `live` to
//! `tombstoned` in the same change, rather than deleted. [`check_manifest`] is what enforces that
//! going forward: a key that is `live` in `old` and absent from `new` is a build failure unless
//! `old` already marked it `tombstoned` (see that function's doc comment for the full rule set).
//! Wiring an automated regeneration tool that merges old tombstones with a new render is left to
//! the CI tooling milestone (`03-implementation-roadmap.md` §5's own note); today, with
//! [`crate::REGISTRY`] empty, there is no tombstone history to preserve yet.

use std::collections::BTreeMap;

use crate::descriptor::{ParamDescriptor, ParamKind};
use crate::error_codes::{
    DROPPED, DUPLICATE_ID, DUPLICATE_KEY, ID_CHANGED, KIND_CHANGED, MALFORMED_LINE,
    ManifestViolation, TOMBSTONE_REUSED,
};

/// The `params.lock` schema version, written as the manifest's `format_version` line. Bump this
/// if the line format itself ever changes shape; it is not a per-parameter version.
pub const FORMAT_VERSION: u32 = 1;

const HEADER: &str = "\
# namir-params manifest (params.lock) -- machine-generated, do not hand-edit except to flip a
# retired parameter's line from \"live\" to \"tombstoned\" (D-10.1). Regenerate the \"live\" lines
# with `cargo test -p namir-params --lib -- --ignored generate_params_lock`, which calls
# render_manifest(REGISTRY) (see crates/namir-params/src/manifest.rs) and writes this file.
#
# Columns: key id kind live|tombstoned. One line per parameter, sorted by key. Tombstoned lines
# are retained forever -- a parameter is retired here, never deleted (FR-PARAM-020).
";

fn kind_tag(kind: &ParamKind) -> &'static str {
    match kind {
        ParamKind::Continuous { .. } => "continuous",
        ParamKind::Stepped { .. } => "stepped",
    }
}

/// Renders `descriptors` as `params.lock` text (D-10.1). Deterministic: sorted by key with a
/// stable sort, so the file never spuriously reorders itself between otherwise-identical builds.
/// Every rendered line is `live` — see the module doc comment for why tombstones don't come from
/// here.
pub fn render_manifest(descriptors: &[ParamDescriptor]) -> String {
    let mut sorted: Vec<&ParamDescriptor> = descriptors.iter().collect();
    sorted.sort_by_key(|d| d.key);

    let mut out = String::from(HEADER);
    out.push_str(&format!("format_version {FORMAT_VERSION}\n"));
    for d in sorted {
        out.push_str(&format!(
            "{} {} {} live\n",
            d.key,
            d.id.0,
            kind_tag(&d.kind)
        ));
    }
    out
}

struct OldEntry {
    id: u32,
    kind: String,
    tombstoned: bool,
}

fn parse_manifest(text: &str) -> (BTreeMap<String, OldEntry>, Vec<ManifestViolation>) {
    let mut entries = BTreeMap::new();
    let mut violations = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("format_version") {
            continue;
        }

        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        let parsed = match fields.as_slice() {
            [key, id, kind, tombstone] => {
                let id = id.parse::<u32>().ok();
                let tombstoned = match *tombstone {
                    "live" => Some(false),
                    "tombstoned" => Some(true),
                    _ => None,
                };
                match (id, tombstoned) {
                    (Some(id), Some(tombstoned)) => Some((*key, id, *kind, tombstoned)),
                    _ => None,
                }
            }
            _ => None,
        };

        match parsed {
            Some((key, id, kind, tombstoned)) => {
                entries.insert(
                    key.to_string(),
                    OldEntry {
                        id,
                        kind: kind.to_string(),
                        tombstoned,
                    },
                );
            }
            None => violations.push(ManifestViolation {
                code: MALFORMED_LINE,
                detail: format!("'{line}'"),
            }),
        }
    }

    (entries, violations)
}

/// Checks a new descriptor set against the previously checked-in manifest text, per D-10.1/
/// FR-PARAM-020. Catches:
/// - a key that was `live` in `old` and now derives a different id than its old entry recorded;
/// - a key or an id that `old` already marked `tombstoned` appearing live in `new` (covers both a
///   retired key coming back and, in principle, two different keys colliding on one `u32`);
/// - a key that stayed live across `old` and `new` but changed kind shape (continuous/stepped) in
///   place, instead of being tombstoned and replaced under a new key;
/// - duplicate ids or duplicate keys within `new` itself;
/// - a key that was `live` in `old` and is simply absent from `new` without a tombstone.
///
/// A key present in `new` but absent from `old` is always fine (that's how a parameter is added).
/// Returns every violation found, not just the first, so a CI run can report the whole diff at
/// once.
pub fn check_manifest(old: &str, new: &[ParamDescriptor]) -> Result<(), Vec<ManifestViolation>> {
    let (old_entries, mut violations) = parse_manifest(old);

    let tombstoned_ids: BTreeMap<u32, &String> = old_entries
        .iter()
        .filter(|(_, e)| e.tombstoned)
        .map(|(k, e)| (e.id, k))
        .collect();

    let mut seen_keys: BTreeMap<&str, ()> = BTreeMap::new();
    let mut seen_ids: BTreeMap<u32, &str> = BTreeMap::new();
    for d in new {
        if seen_keys.insert(d.key, ()).is_some() {
            violations.push(ManifestViolation {
                code: DUPLICATE_KEY,
                detail: format!("key '{}'", d.key),
            });
        }
        if let Some(other_key) = seen_ids.insert(d.id.0, d.key)
            && other_key != d.key
        {
            violations.push(ManifestViolation {
                code: DUPLICATE_ID,
                detail: format!("'{}' and '{}' both derive id {}", other_key, d.key, d.id.0),
            });
        }
    }

    for d in new {
        if tombstoned_ids.contains_key(&d.id.0) {
            violations.push(ManifestViolation {
                code: TOMBSTONE_REUSED,
                detail: format!("key '{}', id {}", d.key, d.id.0),
            });
            continue;
        }

        if let Some(old_entry) = old_entries.get(d.key) {
            if old_entry.id != d.id.0 {
                violations.push(ManifestViolation {
                    code: ID_CHANGED,
                    detail: format!(
                        "key '{}', old id {}, new id {}",
                        d.key, old_entry.id, d.id.0
                    ),
                });
            }
            let new_kind = kind_tag(&d.kind);
            if old_entry.kind != new_kind {
                violations.push(ManifestViolation {
                    code: KIND_CHANGED,
                    detail: format!(
                        "key '{}', old kind '{}', new kind '{}'",
                        d.key, old_entry.kind, new_kind
                    ),
                });
            }
        }
    }

    let new_keys: BTreeMap<&str, ()> = new.iter().map(|d| (d.key, ())).collect();
    for (key, entry) in &old_entries {
        if !entry.tombstoned && !new_keys.contains_key(key.as_str()) {
            violations.push(ManifestViolation {
                code: DROPPED,
                detail: format!("key '{key}'"),
            });
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{SmoothingCategory, StepIndex, Unit, ValueFormat};

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

    const GATE_THRESHOLD: ParamDescriptor = ParamDescriptor::new(
        "gate.threshold",
        "Gate Threshold",
        Unit::Decibels,
        ParamKind::Continuous {
            min: -80.0,
            max: 0.0,
            default: -50.0,
        },
        ValueFormat::FixedDecimals(1),
        SmoothingCategory::GainLike,
    );

    const CHANNEL_MODE: ParamDescriptor = ParamDescriptor::new(
        "out.channel_mode",
        "Channel Mode",
        Unit::None,
        ParamKind::Stepped {
            values: &["Mono", "Stereo"],
            default_index: StepIndex(0),
        },
        ValueFormat::Named,
        SmoothingCategory::Stepped,
    );

    #[test]
    fn render_manifest_is_sorted_by_key_and_has_a_header() {
        let text = render_manifest(&[CHANNEL_MODE, TRIM, GATE_THRESHOLD]);
        assert!(text.starts_with("# namir-params manifest"));
        assert!(text.contains("format_version 1\n"));

        let gate_pos = text.find("gate.threshold").unwrap();
        let out_pos = text.find("out.channel_mode").unwrap();
        let trim_pos = text.find("trim.gain_db").unwrap();
        assert!(gate_pos < out_pos && out_pos < trim_pos);
    }

    #[test]
    fn render_manifest_of_empty_registry_has_no_data_lines() {
        let text = render_manifest(&[]);
        let data_lines: Vec<&str> = text
            .lines()
            .filter(|l| {
                !l.trim().is_empty() && !l.starts_with('#') && !l.starts_with("format_version")
            })
            .collect();
        assert!(
            data_lines.is_empty(),
            "unexpected data lines: {data_lines:?}"
        );
    }

    #[test]
    fn render_manifest_is_deterministic_across_calls() {
        let a = render_manifest(&[TRIM, GATE_THRESHOLD]);
        let b = render_manifest(&[TRIM, GATE_THRESHOLD]);
        assert_eq!(a, b);
    }

    #[test]
    fn round_trips_through_render_and_check_with_no_changes() {
        let old = render_manifest(&[TRIM, GATE_THRESHOLD]);
        assert!(check_manifest(&old, &[TRIM, GATE_THRESHOLD]).is_ok());
    }

    #[test]
    fn happy_path_add_a_key_and_tombstone_another() {
        let old = render_manifest(&[TRIM, GATE_THRESHOLD]);
        // Retire GATE_THRESHOLD (tombstoned in old) and add CHANNEL_MODE as new.
        let old_with_tombstone = old.replace(
            &format!("gate.threshold {} continuous live", GATE_THRESHOLD.id.0),
            &format!(
                "gate.threshold {} continuous tombstoned",
                GATE_THRESHOLD.id.0
            ),
        );
        assert!(check_manifest(&old_with_tombstone, &[TRIM, CHANNEL_MODE]).is_ok());
    }

    #[test]
    fn id_changed_for_a_live_key_is_rejected() {
        let old = render_manifest(&[TRIM]);
        let mutated_old = old.replace(&TRIM.id.0.to_string(), "999999999");
        let result = check_manifest(&mutated_old, &[TRIM]);
        let violations = result.expect_err("id change must be rejected");
        assert!(violations.iter().any(|v| v.code.id == ID_CHANGED.id));
    }

    #[test]
    fn reusing_a_tombstoned_key_is_rejected() {
        let old = render_manifest(&[TRIM]);
        let tombstoned = old.replace(
            &format!("trim.gain_db {} continuous live", TRIM.id.0),
            &format!("trim.gain_db {} continuous tombstoned", TRIM.id.0),
        );
        let result = check_manifest(&tombstoned, &[TRIM]);
        let violations = result.expect_err("tombstone reuse must be rejected");
        assert!(violations.iter().any(|v| v.code.id == TOMBSTONE_REUSED.id));
    }

    #[test]
    fn changing_kind_shape_in_place_is_rejected() {
        let old = render_manifest(&[TRIM]);
        // Same key, same id, but now declared Stepped instead of Continuous.
        const TRIM_AS_STEPPED: ParamDescriptor = ParamDescriptor::new(
            "trim.gain_db",
            "Input Trim",
            Unit::None,
            ParamKind::Stepped {
                values: &["Off", "On"],
                default_index: StepIndex(0),
            },
            ValueFormat::Named,
            SmoothingCategory::Stepped,
        );
        let result = check_manifest(&old, &[TRIM_AS_STEPPED]);
        let violations = result.expect_err("kind change in place must be rejected");
        assert!(violations.iter().any(|v| v.code.id == KIND_CHANGED.id));
    }

    #[test]
    fn duplicate_keys_within_new_set_are_rejected() {
        let result = check_manifest("", &[TRIM, TRIM]);
        let violations = result.expect_err("duplicate key must be rejected");
        assert!(violations.iter().any(|v| v.code.id == DUPLICATE_KEY.id));
    }

    #[test]
    fn duplicate_ids_within_new_set_are_rejected() {
        // Two distinct keys that (deliberately, via a direct struct literal rather than
        // `ParamDescriptor::new`) derive the same id -- e.g. a real FNV-1a collision between two
        // otherwise-unrelated keys. Constructed by hand here since a genuine 32-bit collision
        // between two short, meaningful strings isn't guaranteed to exist; the point under test
        // is the detector, not FNV-1a's collision rate.
        const OTHER_KEY_SAME_ID: ParamDescriptor = ParamDescriptor {
            key: "other.unrelated_key",
            id: TRIM.id,
            stage_instance: 0,
            name: "Unrelated",
            unit: Unit::None,
            kind: ParamKind::Continuous {
                min: 0.0,
                max: 1.0,
                default: 0.0,
            },
            format: ValueFormat::FixedDecimals(1),
            smoothing: SmoothingCategory::GainLike,
        };
        let result = check_manifest("", &[TRIM, OTHER_KEY_SAME_ID]);
        let violations = result.expect_err("duplicate id must be rejected");
        assert!(violations.iter().any(|v| v.code.id == DUPLICATE_ID.id));
        assert!(!violations.iter().any(|v| v.code.id == DUPLICATE_KEY.id));
    }

    #[test]
    fn dropping_a_live_key_without_tombstoning_is_rejected() {
        let old = render_manifest(&[TRIM, GATE_THRESHOLD]);
        let result = check_manifest(&old, &[TRIM]);
        let violations = result.expect_err("silent drop must be rejected");
        assert!(violations.iter().any(|v| v.code.id == DROPPED.id));
    }

    #[test]
    fn adding_a_brand_new_key_is_fine() {
        let old = render_manifest(&[TRIM]);
        assert!(check_manifest(&old, &[TRIM, GATE_THRESHOLD]).is_ok());
    }

    #[test]
    fn malformed_old_lines_are_reported_but_do_not_panic() {
        let old = "not a valid manifest line at all\n";
        let result = check_manifest(old, &[TRIM]);
        let violations = result.expect_err("malformed line must be reported");
        assert!(violations.iter().any(|v| v.code.id == MALFORMED_LINE.id));
    }
}
