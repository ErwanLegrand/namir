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
//! `<key> <id> <kind> <live|tombstoned> <shape>`. Tombstoned lines are never deleted — that is the
//! entire point of a tombstone (D-10.1).
//!
//! # The shape columns, and the format version that carries them (issue #121)
//!
//! Format version 1 recorded `<key> <id> <kind> <live|tombstoned>` and nothing else, and
//! [`kind_tag`] reduces a kind to the bare word `continuous` or `stepped`. So a change to a
//! parameter's `min`, `max` or `default`, or to a [`ParamKind::Stepped`]'s `values` list, moved no
//! byte of `params.lock` — while silently reinterpreting every saved preset and every
//! host-normalised automation value carrying that id. D-10.1's guarantee is that the manifest is
//! *diffed* in CI; a change nothing in the file records is a change nothing can diff.
//!
//! Version 2 appends a per-kind shape (see [`shape_tag`]):
//!
//! - continuous: `min=<f32> max=<f32> default=<f32>`, each rendered by `f32`'s shortest
//!   round-tripping `Debug` form so the file is byte-stable across builds;
//! - stepped: `steps=<count> default=<index> values=<hex>`, the last an FNV-1a fingerprint of the
//!   value labels — which catches a *reordering* of the labels, the change that actually re-points
//!   a stored index at a different option, and which a bare count would miss.
//!
//! **A shape change is a diff, not a violation.** D-10.1 makes a changed *identifier or type* a
//! build failure; a widened range is a legitimate edit (FRS §5 states several ranges as "at
//! least"), so recording it makes it visible and reviewable rather than forbidden. What used to
//! pass invisibly now fails `xtask params-lock` as a stale file whose regeneration a reviewer sees.
//!
//! An internally inconsistent descriptor — a `default` outside its own range, a `default_index`
//! past the end of `values` — would be written into that record as fact, so [`check_manifest`]
//! also runs [`ParamDescriptor::validate`] over every descriptor it is given (issue #119).
//!
//! [`render_manifest`] only ever emits `live` lines: it renders the current, in-source descriptor
//! set, which by construction contains no tombstones (a tombstoned parameter has no descriptor
//! left to render). Tombstone lines enter `params.lock` when a parameter is retired — its
//! descriptor is deleted from source and its manifest line is hand-flipped from `live` to
//! `tombstoned` in the same change, rather than deleted. [`check_manifest`] is what enforces that
//! going forward: a key that is `live` in `old` and absent from `new` is a build failure unless
//! `old` already marked it `tombstoned` (see that function's doc comment for the full rule set).
//!
//! # The regeneration tool, added M14
//!
//! The paragraph that stood here said wiring "an automated regeneration tool that merges old
//! tombstones with a new render" was left to the CI tooling milestone, and that with
//! [`crate::REGISTRY`] empty there was no tombstone history to preserve yet. `REGISTRY` stopped
//! being empty at M2 and the tool was never wired, which left the tombstone mechanism **inoperable
//! rather than merely unbuilt** (FR-PARAM-020, issue #31): every regeneration path in the tree —
//! `xtask params-lock --write`, `generate_params_lock`, and the byte-equality check both are read
//! against — compared or wrote [`render_manifest`]'s *live-only* output, so a committed tombstone
//! line failed the gate permanently and `--write` deleted it. A retirement mechanism that cannot
//! survive its own regeneration command is not a mechanism, and 1.0 is the version D-10.1's
//! stability promise is measured from.
//!
//! [`merge_manifest`] is that tool. It is what every regeneration and comparison path now goes
//! through; [`render_manifest`] is kept as the live-only primitive it always was, and is called
//! only by [`merge_manifest`] and by tests of it.

use std::collections::BTreeMap;

use crate::descriptor::{ParamDescriptor, ParamKind};
use crate::error_codes::{
    DROPPED, DUPLICATE_ID, DUPLICATE_KEY, FORMAT_VERSION_UNSUPPORTED, ID_CHANGED,
    INVALID_DESCRIPTOR, KIND_CHANGED, MALFORMED_LINE, ManifestViolation, TOMBSTONE_REUSED,
};

/// The `params.lock` schema version, written as the manifest's `format_version` line. Bump this
/// if the line format itself ever changes shape; it is not a per-parameter version.
///
/// - **1** — `<key> <id> <kind> <live|tombstoned>`.
/// - **2** — the same, plus the per-kind shape columns [`shape_tag`] renders (issue #121).
///
/// [`check_manifest`] reads a file declaring a version *newer* than this one as a single
/// `FORMAT_VERSION_UNSUPPORTED` violation rather than parsing it under this build's rules (issue
/// #122). An *older* one is deliberately not a violation: it is a staleness, and the whole of
/// migrating it is `cargo run -p xtask -- params-lock --write`. Making it an error instead would
/// leave the file in a state the documented regeneration command refuses to fix, which is the trap
/// issue #117 named.
pub const FORMAT_VERSION: u32 = 2;

const HEADER: &str = "\
# namir-params manifest (params.lock) -- machine-generated, do not hand-edit except to flip a
# retired parameter's line from \"live\" to \"tombstoned\" (D-10.1), leaving the rest of that line
# alone. Regenerate with `cargo run -p xtask -- params-lock --write`, which calls
# merge_manifest(this file, REGISTRY) (see crates/namir-params/src/manifest.rs): the \"live\" lines
# are re-rendered from REGISTRY and every \"tombstoned\" line already here is carried through
# unchanged, so that hand edit is one the gate accepts rather than one it refuses forever.
#
# Columns: key id kind live|tombstoned <shape>. One line per parameter, sorted by key. Tombstoned
# lines are retained forever -- a parameter is retired here, never deleted (FR-PARAM-020).
#
# The shape columns record what the kind tag alone does not, so that a changed range, default or
# set of stepped values shows up as a diff here instead of silently reinterpreting every saved
# preset and every host-normalised automation value carrying that id:
#   continuous  min=<f32> max=<f32> default=<f32>
#   stepped     steps=<count> default=<index> values=<FNV-1a of the value labels, hex>
";

fn kind_tag(kind: &ParamKind) -> &'static str {
    match kind {
        ParamKind::Continuous { .. } => "continuous",
        ParamKind::Stepped { .. } => "stepped",
    }
}

/// The `<shape>` columns of a manifest line: what the parameter's value space actually *is*, as
/// opposed to which of the two shapes it has (issue #121). See the module doc comment for the
/// column vocabulary and for why a change here is a diff rather than a violation.
///
/// Floats are rendered with `f32`'s `Debug` form, which is the shortest decimal that round-trips
/// — deterministic, so the file never changes spuriously, and never a truncation that would let
/// two different ranges render the same columns.
fn shape_tag(kind: &ParamKind) -> String {
    match kind {
        ParamKind::Continuous { min, max, default } => {
            format!("min={min:?} max={max:?} default={default:?}")
        }
        ParamKind::Stepped {
            values,
            default_index,
        } => format!(
            "steps={} default={} values={:08x}",
            values.len(),
            default_index.0,
            values_digest(values)
        ),
    }
}

/// FNV-1a over a stepped parameter's labels, joined by the US separator (0x1f), which no
/// display label contains.
/// A fingerprint rather than the labels themselves because a label may contain whitespace
/// ("Dual Mono"), and this file's grammar is whitespace-separated columns.
fn values_digest(values: &[&str]) -> u32 {
    crate::id::fnv1a_32(values.join("\u{1f}").as_bytes())
}

/// One manifest line for `d` in the given `live`/`tombstoned` state, without its newline.
fn manifest_line(d: &ParamDescriptor, state: &str) -> String {
    format!(
        "{} {} {} {} {}",
        d.key,
        d.id.0,
        kind_tag(&d.kind),
        state,
        shape_tag(&d.kind)
    )
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
        out.push_str(&manifest_line(d, "live"));
        out.push('\n');
    }
    out
}

/// The manifest text a fresh `params.lock` should have, given the one checked in today: every
/// `live` line [`render_manifest`] derives from `new`, plus every `tombstoned` line `old` already
/// carried, merged into one key-sorted list.
///
/// This is the function that makes D-10.1's tombstone a real mechanism rather than a documented
/// intention (see the module doc's *The regeneration tool* section). Three properties are
/// load-bearing:
///
/// - **It is a no-op when `old` carries no tombstone.** The output is then byte-identical to
///   `render_manifest(new)`, which is what lets it replace that call everywhere without touching
///   the checked-in `params.lock`.
/// - **A tombstoned key that has come back live is not duplicated.** Its `live` line wins in the
///   rendered text — but that state is exactly [`check_manifest`]'s `TOMBSTONE_REUSED`, which every
///   caller of this function is required to run first, so the merge never has to adjudicate it.
///   Silently emitting both lines would produce a file whose own re-parse disagrees with itself.
/// - **A malformed `old` line is dropped, not propagated.** `old` is hand-edited by definition (a
///   tombstone is a hand-flip of `live` to `tombstoned`), so a typo is the expected failure. It is
///   [`check_manifest`]'s `MALFORMED_LINE` that reports it with the offending text; carrying the
///   bad line forward here would make `--write` cement a typo into the checked-in file.
pub fn merge_manifest(old: &str, new: &[ParamDescriptor]) -> String {
    let parsed = parse_manifest(old);

    let live_keys: BTreeMap<&str, ()> = new.iter().map(|d| (d.key, ())).collect();
    let mut lines: Vec<(&str, String)> = new
        .iter()
        .map(|d| (d.key, manifest_line(d, "live")))
        .collect();
    for (key, entry) in &parsed.entries {
        if entry.tombstoned && !live_keys.contains_key(key.as_str()) {
            // The recorded shape is carried through verbatim, exactly like the id and the kind: a
            // retired parameter has no descriptor left to re-render one from, and what its range
            // or its named values *were* is the historical fact the tombstone exists to keep. A
            // line written under format version 1 has none, and is carried through without one.
            let shape = match &entry.shape {
                Some(shape) => format!(" {shape}"),
                None => String::new(),
            };
            lines.push((
                key.as_str(),
                format!("{} {} {} tombstoned{}", key, entry.id, entry.kind, shape),
            ));
        }
    }
    // Same stable sort by key `render_manifest` uses, so a tombstone lands where its key belongs
    // rather than at the end -- the file stays diffable and never spuriously reorders.
    lines.sort_by_key(|(key, _)| *key);

    let mut out = String::from(HEADER);
    out.push_str(&format!("format_version {FORMAT_VERSION}\n"));
    for (_, line) in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

struct OldEntry {
    id: u32,
    kind: String,
    tombstoned: bool,
    /// The line's `<shape>` columns, joined by single spaces, or `None` for a line written under
    /// format version 1 (which had none). Never re-derived: see [`merge_manifest`].
    shape: Option<String>,
}

/// What a manifest's `format_version` line said. Distinguishing *absent* from *unreadable* matters:
/// an absent version is a version-1 file, which regeneration migrates, while an unreadable one is a
/// file this build cannot claim to understand at all.
enum DeclaredVersion {
    Absent,
    Value(u32),
    Unreadable(String),
}

struct ParsedManifest {
    entries: BTreeMap<String, OldEntry>,
    violations: Vec<ManifestViolation>,
    version: DeclaredVersion,
}

fn parse_manifest(text: &str) -> ParsedManifest {
    let mut entries = BTreeMap::new();
    let mut violations = Vec::new();
    let mut version = DeclaredVersion::Absent;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let fields: Vec<&str> = trimmed.split_whitespace().collect();

        // The version line is recognised by its key **exactly**, not by prefix: `starts_with` here
        // used to swallow any line beginning `format_version`, so `format_version_2 7` was skipped
        // in silence rather than reported (issue #122). And the value is now read rather than
        // discarded -- see `check_manifest`.
        if fields[0] == "format_version" {
            match fields.as_slice() {
                [_, value] if matches!(version, DeclaredVersion::Absent) => {
                    version = match value.parse::<u32>() {
                        Ok(v) => DeclaredVersion::Value(v),
                        Err(_) => DeclaredVersion::Unreadable((*value).to_string()),
                    };
                }
                _ => violations.push(ManifestViolation {
                    code: MALFORMED_LINE,
                    detail: format!("'{line}'"),
                }),
            }
            continue;
        }

        let parsed = match fields.as_slice() {
            [key, id, kind, tombstone, shape @ ..] => {
                let id = id.parse::<u32>().ok();
                let tombstoned = match *tombstone {
                    "live" => Some(false),
                    "tombstoned" => Some(true),
                    _ => None,
                };
                // Every shape column is a `<name>=<value>` pair. Checked rather than assumed, so a
                // typo in the one hand edit D-10.1 permits is reported against its own line instead
                // of being carried forward by `merge_manifest` as if it were data.
                let shape_ok = shape.iter().all(|token| {
                    let mut halves = token.splitn(2, '=');
                    matches!(
                        (halves.next(), halves.next()),
                        (Some(name), Some(value)) if !name.is_empty() && !value.is_empty()
                    )
                });
                match (id, tombstoned, shape_ok) {
                    (Some(id), Some(tombstoned), true) => Some((
                        *key,
                        id,
                        *kind,
                        tombstoned,
                        (!shape.is_empty()).then(|| shape.join(" ")),
                    )),
                    _ => None,
                }
            }
            _ => None,
        };

        match parsed {
            Some((key, id, kind, tombstoned, shape)) => {
                entries.insert(
                    key.to_string(),
                    OldEntry {
                        id,
                        kind: kind.to_string(),
                        tombstoned,
                        shape,
                    },
                );
            }
            None => violations.push(ManifestViolation {
                code: MALFORMED_LINE,
                detail: format!("'{line}'"),
            }),
        }
    }

    ParsedManifest {
        entries,
        violations,
        version,
    }
}

/// Checks a new descriptor set against the previously checked-in manifest text, per D-10.1/
/// FR-PARAM-020. Catches:
/// - a key that was `live` in `old` and now derives a different id than its old entry recorded;
/// - a key or an id that `old` already marked `tombstoned` appearing live in `new` (covers both a
///   retired key coming back and, in principle, two different keys colliding on one `u32`);
/// - a key that stayed live across `old` and `new` but changed kind shape (continuous/stepped) in
///   place, instead of being tombstoned and replaced under a new key;
/// - duplicate ids or duplicate keys within `new` itself;
/// - a key that was `live` in `old` and is simply absent from `new` without a tombstone;
/// - a descriptor in `new` that contradicts itself — a default outside its own range, a stepped
///   default index past the end of its values ([`ParamDescriptor::validate`], issue #119);
/// - `old` declaring a `format_version` this build cannot read (issue #122).
///
/// A key present in `new` but absent from `old` is always fine (that's how a parameter is added).
/// So is a *shape* change — a widened range, a new default, a different set of stepped values:
/// version 2 records those (see the module doc comment) so that they surface as a diff a reviewer
/// sees, which is what D-10.1 asks of the manifest, rather than as a build failure D-10.1 reserves
/// for a changed identifier or type.
///
/// Returns every violation found, not just the first, so a CI run can report the whole diff at
/// once — except for an unreadable `format_version`, which returns alone: under a line grammar this
/// build does not know, every other finding is a guess. Reporting a future file as a pile of
/// `MALFORMED_LINE`s is exactly what issue #122 named.
pub fn check_manifest(old: &str, new: &[ParamDescriptor]) -> Result<(), Vec<ManifestViolation>> {
    let ParsedManifest {
        entries: old_entries,
        mut violations,
        version,
    } = parse_manifest(old);

    // A version *newer* than this build's, or one that is not a number at all. Not a staleness:
    // there is no regeneration that fixes it, and `--write`ing over it would destroy a file written
    // by tooling that knows more than this build does. An *older* version is deliberately absent
    // from this rule -- see `FORMAT_VERSION`'s own doc comment.
    match version {
        DeclaredVersion::Value(found) if found > FORMAT_VERSION => {
            return Err(vec![ManifestViolation {
                code: FORMAT_VERSION_UNSUPPORTED,
                detail: format!(
                    "the file declares format_version {found}; this build writes and reads \
                     {FORMAT_VERSION}"
                ),
            }]);
        }
        DeclaredVersion::Unreadable(found) => {
            return Err(vec![ManifestViolation {
                code: FORMAT_VERSION_UNSUPPORTED,
                detail: format!("format_version '{found}' is not a number"),
            }]);
        }
        DeclaredVersion::Value(_) | DeclaredVersion::Absent => {}
    }

    // Before any comparison against `old`: a descriptor that contradicts itself would otherwise be
    // recorded in the manifest's shape columns as fact (issue #119).
    for d in new {
        if let Err(problem) = d.validate() {
            violations.push(ManifestViolation {
                code: INVALID_DESCRIPTOR,
                detail: format!("key '{}': {problem}", d.key),
            });
        }
    }

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
        assert!(text.contains(&format!("format_version {FORMAT_VERSION}\n")));

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

    // --- merge_manifest (M14, FR-PARAM-020 / issue #31) ----------------------------------------

    /// `old` with `key` retired: its line hand-flipped from `live` to `tombstoned`, which is the
    /// only hand edit D-10.1 permits to this file.
    fn with_tombstone(old: &str, key: &str) -> String {
        let flipped: Vec<String> = old
            .lines()
            .map(|line| {
                if line.starts_with(&format!("{key} ")) {
                    line.replace(" live", " tombstoned")
                } else {
                    line.to_string()
                }
            })
            .collect();
        format!("{}\n", flipped.join("\n"))
    }

    #[test]
    fn merging_into_a_manifest_with_no_tombstone_reproduces_render_manifest_byte_for_byte() {
        // What lets `merge_manifest` replace `render_manifest` at every regeneration and
        // comparison site without moving a byte of the checked-in params.lock.
        let old = render_manifest(&[TRIM, GATE_THRESHOLD]);
        assert_eq!(
            merge_manifest(&old, &[TRIM, GATE_THRESHOLD]),
            render_manifest(&[TRIM, GATE_THRESHOLD])
        );
        // And from nothing at all -- the first-generation case.
        assert_eq!(
            merge_manifest("", &[TRIM, GATE_THRESHOLD]),
            render_manifest(&[TRIM, GATE_THRESHOLD])
        );
    }

    #[test]
    fn a_tombstoned_line_survives_regeneration() {
        // The defect issue #31 names, in one assertion: retire GATE_THRESHOLD, drop its descriptor
        // from the live set (which is what retiring it means), and regenerate. Under
        // `render_manifest` the line vanished and the id became reusable; under `merge_manifest`
        // it is still there, verbatim.
        let old = with_tombstone(&render_manifest(&[TRIM, GATE_THRESHOLD]), "gate.threshold");
        let merged = merge_manifest(&old, &[TRIM, CHANNEL_MODE]);

        let tombstone = format!(
            "gate.threshold {} continuous tombstoned",
            GATE_THRESHOLD.id.0
        );
        assert!(merged.contains(&tombstone), "{merged}");
        assert!(merged.contains(&format!("trim.gain_db {} continuous live", TRIM.id.0)));
        assert!(merged.contains(&format!(
            "out.channel_mode {} stepped live",
            CHANNEL_MODE.id.0
        )));

        // Idempotent: regenerating twice is a fixed point, so a `--write` in a pull request that
        // changed nothing produces no diff.
        assert_eq!(merge_manifest(&merged, &[TRIM, CHANNEL_MODE]), merged);
        // And the result is a manifest the checker accepts.
        assert!(check_manifest(&merged, &[TRIM, CHANNEL_MODE]).is_ok());
    }

    #[test]
    fn a_tombstone_is_placed_at_its_key_position_not_appended() {
        // Diffability (D-10.1's "never spuriously reordered"): a retired `gate.threshold` must stay
        // between `out.channel_mode`'s predecessors and `trim.gain_db`, exactly where its key
        // sorts, rather than being tacked onto the end.
        let old = with_tombstone(&render_manifest(&[TRIM, GATE_THRESHOLD]), "gate.threshold");
        let merged = merge_manifest(&old, &[TRIM, CHANNEL_MODE]);
        let gate = merged.find("gate.threshold").unwrap();
        let out = merged.find("out.channel_mode").unwrap();
        let trim = merged.find("trim.gain_db").unwrap();
        assert!(gate < out && out < trim, "{merged}");
    }

    #[test]
    fn a_key_that_is_both_tombstoned_and_live_renders_exactly_one_line() {
        // The state `check_manifest` reports as TOMBSTONE_REUSED. The merge must not emit two
        // lines for one key -- a file whose own re-parse disagrees with itself would be worse than
        // the violation it is hiding, and the violation is reported by the checker every caller
        // runs first.
        let old = with_tombstone(&render_manifest(&[TRIM]), "trim.gain_db");
        let merged = merge_manifest(&old, &[TRIM]);
        assert_eq!(merged.matches("trim.gain_db ").count(), 1, "{merged}");
        assert!(merged.contains(&format!("trim.gain_db {} continuous live", TRIM.id.0)));
        // ...and the state itself is still a violation, reported against the file that carries it.
        // `merged` no longer does -- which is exactly why the merge may not be trusted to
        // adjudicate this and every caller runs the checker against the *checked-in* text first.
        assert!(check_manifest(&old, &[TRIM]).is_err());
    }

    #[test]
    fn a_malformed_old_line_is_dropped_rather_than_carried_forward() {
        // `old` is hand-edited by definition, so a typo is the expected failure mode. It is
        // `check_manifest`'s MALFORMED_LINE that reports it; `--write` must not cement it in.
        let old = format!(
            "{}oops this is not a manifest line\n",
            render_manifest(&[TRIM])
        );
        let merged = merge_manifest(&old, &[TRIM]);
        assert!(!merged.contains("oops"), "{merged}");
    }

    // --- the shape columns (issue #121) --------------------------------------------------------

    /// `TRIM` with a different `ParamKind`, keeping its key, id and every other field. This is the
    /// edit issue #121 is about: same identifier, same type, a different value space.
    fn trim_with(kind: ParamKind) -> ParamDescriptor {
        ParamDescriptor { kind, ..TRIM }
    }

    #[test]
    fn a_changed_range_moves_the_manifest() {
        let old = render_manifest(&[TRIM]);
        for widened in [
            ParamKind::Continuous {
                min: -30.0,
                max: 24.0,
                default: 0.0,
            },
            ParamKind::Continuous {
                min: -24.0,
                max: 36.0,
                default: 0.0,
            },
        ] {
            let new = trim_with(widened);
            assert_ne!(
                render_manifest(&[new]),
                old,
                "a changed range must move params.lock, or nothing can diff it"
            );
            // ...and the file is therefore reported stale rather than silently accepted, while
            // still being a legitimate edit rather than a violation.
            assert!(check_manifest(&old, &[new]).is_ok());
            assert_ne!(merge_manifest(&old, &[new]), old);
        }
    }

    #[test]
    fn a_changed_default_moves_the_manifest() {
        let old = render_manifest(&[TRIM]);
        let new = trim_with(ParamKind::Continuous {
            min: -24.0,
            max: 24.0,
            default: -6.0,
        });
        assert_ne!(render_manifest(&[new]), old);
        assert!(old.contains("default=0.0"), "{old}");
        assert!(render_manifest(&[new]).contains("default=-6.0"));
    }

    #[test]
    fn a_changed_stepped_values_list_moves_the_manifest() {
        let old = render_manifest(&[CHANNEL_MODE]);

        // One more option: the count column alone would catch this one.
        let added = ParamDescriptor {
            kind: ParamKind::Stepped {
                values: &["Mono", "Stereo", "Dual Mono"],
                default_index: StepIndex(0),
            },
            ..CHANNEL_MODE
        };
        assert_ne!(render_manifest(&[added]), old);

        // The same two options, reordered: the count is unchanged, and every preset that stored
        // index 0 now means the other option. This is what the fingerprint column is for.
        let reordered = ParamDescriptor {
            kind: ParamKind::Stepped {
                values: &["Stereo", "Mono"],
                default_index: StepIndex(0),
            },
            ..CHANNEL_MODE
        };
        assert_ne!(
            render_manifest(&[reordered]),
            old,
            "a reordered values list must move params.lock"
        );

        // And a changed default index, with the same list.
        let other_default = ParamDescriptor {
            kind: ParamKind::Stepped {
                values: &["Mono", "Stereo"],
                default_index: StepIndex(1),
            },
            ..CHANNEL_MODE
        };
        assert_ne!(render_manifest(&[other_default]), old);
    }

    #[test]
    fn a_tombstones_shape_columns_survive_regeneration() {
        // A retired parameter has no descriptor left to re-render its range from, so what it *was*
        // is only in the file. `merge_manifest` must carry it verbatim, like the id and the kind.
        let old = with_tombstone(&render_manifest(&[TRIM, GATE_THRESHOLD]), "gate.threshold");
        let merged = merge_manifest(&old, &[TRIM]);
        assert!(
            merged.contains(&format!(
                "gate.threshold {} continuous tombstoned min=-80.0 max=0.0 default=-50.0",
                GATE_THRESHOLD.id.0
            )),
            "{merged}"
        );
        assert_eq!(merge_manifest(&merged, &[TRIM]), merged);
    }

    #[test]
    fn a_shape_column_that_is_not_a_name_value_pair_is_malformed() {
        let old = format!(
            "{}zz.retired 42 continuous tombstoned min=-1.0 oops\n",
            render_manifest(&[TRIM])
        );
        let violations = check_manifest(&old, &[TRIM]).expect_err("a bad shape column must fail");
        assert!(violations.iter().any(|v| v.code.id == MALFORMED_LINE.id));
    }

    // --- format_version (issue #122) -------------------------------------------------------------

    #[test]
    fn a_future_format_version_is_reported_as_a_version_mismatch_not_a_pile_of_malformed_lines() {
        let future = format!(
            "{}\nsome.key 7 continuous live shape-this-build-cannot-read\n",
            render_manifest(&[TRIM]).replace(
                &format!("format_version {FORMAT_VERSION}"),
                &format!("format_version {}", FORMAT_VERSION + 1)
            )
        );
        let violations = check_manifest(&future, &[TRIM]).expect_err("a future version must fail");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].code.id, FORMAT_VERSION_UNSUPPORTED.id);
        assert!(
            violations[0]
                .detail
                .contains(&(FORMAT_VERSION + 1).to_string()),
            "{:?}",
            violations[0]
        );
    }

    #[test]
    fn an_unreadable_format_version_is_reported_as_such() {
        let text = render_manifest(&[TRIM]).replace(
            &format!("format_version {FORMAT_VERSION}"),
            "format_version two",
        );
        let violations = check_manifest(&text, &[TRIM]).expect_err("must fail");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].code.id, FORMAT_VERSION_UNSUPPORTED.id);
    }

    #[test]
    fn a_key_merely_beginning_format_version_is_not_skipped_in_silence() {
        // The prefix test this replaced swallowed any such line without reading it.
        let text = format!("{}format_version_2 7\n", render_manifest(&[TRIM]));
        let violations = check_manifest(&text, &[TRIM]).expect_err("must be reported");
        assert!(violations.iter().any(|v| v.code.id == MALFORMED_LINE.id));

        // A second, well-formed version line is not data either.
        let two = format!("{}format_version 1\n", render_manifest(&[TRIM]));
        let violations = check_manifest(&two, &[TRIM]).expect_err("must be reported");
        assert!(violations.iter().any(|v| v.code.id == MALFORMED_LINE.id));
    }

    #[test]
    fn an_older_format_version_is_a_staleness_a_regeneration_fixes_not_a_violation() {
        // A version-1 file: four columns, no shape. It must still *pass* the identifier rules --
        // making it an error would leave it in a state `params-lock --write` refuses to fix, which
        // is issue #117's trap -- and regenerating it must migrate it to this build's format.
        let v1 = format!(
            "# namir-params manifest\nformat_version 1\ntrim.gain_db {} continuous live\n",
            TRIM.id.0
        );
        assert!(check_manifest(&v1, &[TRIM]).is_ok());

        let migrated = merge_manifest(&v1, &[TRIM]);
        assert!(migrated.contains(&format!("format_version {FORMAT_VERSION}\n")));
        assert_eq!(migrated, render_manifest(&[TRIM]));

        // A version-1 *tombstone* survives the migration too, shapeless as it is.
        let v1_tombstone = format!("{v1}zz.retired_example 4242424242 continuous tombstoned\n");
        assert!(check_manifest(&v1_tombstone, &[TRIM]).is_ok());
        let migrated = merge_manifest(&v1_tombstone, &[TRIM]);
        assert!(
            migrated.contains("zz.retired_example 4242424242 continuous tombstoned\n"),
            "{migrated}"
        );
        assert_eq!(merge_manifest(&migrated, &[TRIM]), migrated);
    }

    // --- descriptor invariants (issue #119) ------------------------------------------------------

    #[test]
    fn a_descriptor_that_contradicts_itself_is_rejected() {
        let bad_index = ParamDescriptor {
            kind: ParamKind::Stepped {
                values: &["Mono", "Stereo"],
                default_index: StepIndex(5),
            },
            ..CHANNEL_MODE
        };
        let violations = check_manifest("", &[bad_index]).expect_err("must be rejected");
        assert!(
            violations
                .iter()
                .any(|v| v.code.id == INVALID_DESCRIPTOR.id)
        );

        let default_outside_range = trim_with(ParamKind::Continuous {
            min: -24.0,
            max: 24.0,
            default: 96.0,
        });
        let violations =
            check_manifest("", &[default_outside_range]).expect_err("must be rejected");
        assert!(
            violations
                .iter()
                .any(|v| v.code.id == INVALID_DESCRIPTOR.id && v.detail.contains("trim.gain_db"))
        );
    }

    #[test]
    fn malformed_old_lines_are_reported_but_do_not_panic() {
        let old = "not a valid manifest line at all\n";
        let result = check_manifest(old, &[TRIM]);
        let violations = result.expect_err("malformed line must be reported");
        assert!(violations.iter().any(|v| v.code.id == MALFORMED_LINE.id));
    }
}
