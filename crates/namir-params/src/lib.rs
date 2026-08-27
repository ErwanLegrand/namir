//! D-5.1's role for this crate: "Parameter identity, ranges, formatting, smoothing, automation
//! intake." This crate is the *system*: the stable id derivation FR-PARAM-020 requires, the
//! descriptor shape FR-PARAM-010/050 require, the smoothing-category declarations D-10.3
//! assigns, and the checked-in manifest mechanism D-10.1 requires. It is not itself parameters —
//! see [`REGISTRY`] below.
//!
//! # Scope
//!
//! In scope, and closed by this crate at M1 (`03-implementation-roadmap.md` §5):
//! - FR-PARAM-010 — [`ParamDescriptor`]: key, [`ParamId`], name, [`Unit`], range/default (via
//!   [`ParamKind::Continuous`]), and a [`ValueFormat`].
//! - FR-PARAM-020 — [`ParamId::from_key`]'s permanent FNV-1a derivation (see `id.rs`) plus
//!   [`render_manifest`]/[`check_manifest`]'s enforcement that an existing entry's identifier or
//!   kind never changes and a retired entry is tombstoned, never silently dropped.
//! - FR-PARAM-050 — [`ParamKind::Stepped`], with named values and a [`descriptor::StepIndex`]
//!   value-representation type, instead of forcing discrete choices through a continuous range.
//! - D-10.2 — the `stage_instance` field on every descriptor, present and zeroed now so RD-2's
//!   future dynamic chain can grow without renumbering existing parameters.
//! - D-10.3 — [`SmoothingCategory`], declaring which `namir-dsp` primitive a future stage reaches
//!   for, without this crate depending on `namir-dsp` or performing any smoothing itself.
//!
//! Out of scope, deliberately, for this crate:
//! - FR-PARAM-030 (accepting changes from UI/CLAP automation/preset loading) and FR-PARAM-040
//!   (actually smoothing a value stream) — both are `namir-engine`'s job once real stages exist
//!   at M2. `namir-engine`'s existing `ParamId`/`ParamChange` (`crates/namir-engine/src/
//!   param.rs`) are a separate, deliberately bare RT-path type; wiring them to
//!   [`ParamDescriptor`]/[`ParamKind::Stepped`] is explicitly M2's work, not this crate's.
//! - FR-PARAM-060 (a modulation/automation-appropriateness flag) — not yet designed; left for
//!   whichever milestone first needs to distinguish per-sample-automatable parameters from
//!   configuration-like ones.
//!
//! # [`REGISTRY`], M2 onward
//!
//! M2 (`03-implementation-roadmap.md` §6) lands the six 1.0 product stages (Trim/Gate/Nam/Ir/Eq/
//! Out) in `namir-engine`, and [`REGISTRY`] is populated here with each stage's descriptor set —
//! see the [`stages`] module, one submodule per stage per D-10.1's "declared in one place per
//! stage". `namir-engine`'s stage implementations reference these same `const`s (by id) rather
//! than re-declaring ranges/defaults, so there is exactly one source of truth per parameter.
//!
//! - D-10.4 (added M6) — the [`global`] module: two descriptors, `global.bypass` and
//!   `global.output_ceiling_db`, for chain-level state (FR-CHAIN-030/090) that isn't owned by any
//!   one of the six stages. See that module's doc comment for why these were missing from
//!   [`REGISTRY`] until now.

mod descriptor;
mod error_codes;
pub mod global;
mod id;
mod manifest;
pub mod stages;

pub use descriptor::{ParamDescriptor, ParamKind, SmoothingCategory, StepIndex, Unit, ValueFormat};
pub use error_codes::ManifestViolation;
pub use id::ParamId;
pub use manifest::{FORMAT_VERSION, check_manifest, merge_manifest, render_manifest};

/// The full set of parameters this build knows about (D-10.1). `params.lock` at the repository
/// root is exactly [`merge_manifest`]`(params.lock, REGISTRY)`'s current output — this list's
/// `live` lines plus whatever tombstones the file already carries; regenerate it with
/// `cargo run -p xtask -- params-lock --write` after changing this list.
pub const REGISTRY: &[ParamDescriptor] = &[
    stages::trim::GAIN_DB,
    stages::trim::DC_BLOCKER_ENABLED,
    stages::gate::ENABLED,
    stages::gate::THRESHOLD_DB,
    stages::gate::ATTACK_MS,
    stages::gate::HOLD_MS,
    stages::gate::RELEASE_MS,
    stages::nam::ENABLED,
    stages::nam::NORMALIZE_ENABLED,
    stages::nam::NORMALIZE_OFFSET_DB,
    stages::ir::ENABLED,
    stages::ir::LEVEL_DB,
    stages::ir::LOW_CUT_ENABLED,
    stages::ir::LOW_CUT_FREQ_HZ,
    stages::ir::HIGH_CUT_ENABLED,
    stages::ir::HIGH_CUT_FREQ_HZ,
    stages::eq::ENABLED,
    stages::eq::LOW_SHELF_FREQ_HZ,
    stages::eq::LOW_SHELF_GAIN_DB,
    stages::eq::MID_FREQ_HZ,
    stages::eq::MID_GAIN_DB,
    stages::eq::MID_Q,
    stages::eq::HIGH_SHELF_FREQ_HZ,
    stages::eq::HIGH_SHELF_GAIN_DB,
    stages::eq::HIGH_PASS_ENABLED,
    stages::eq::HIGH_PASS_FREQ_HZ,
    stages::eq::LOW_PASS_ENABLED,
    stages::eq::LOW_PASS_FREQ_HZ,
    stages::out::GAIN_DB,
    // D-10.4: chain-level, not owned by any one of the six stages -- see `global`'s module doc
    // comment for why these live outside `stages` and why they were missing from REGISTRY until
    // now.
    global::GLOBAL_BYPASS,
    global::OUTPUT_CEILING_DB,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// M14: goes through `merge_manifest` rather than `render_manifest`, so this path no longer
    /// deletes a tombstone the file already carries (FR-PARAM-020, issue #31). Prefer `cargo run
    /// -p xtask -- params-lock --write`, which additionally runs `check_manifest` before writing;
    /// this generator is kept because AGENTS.md and this crate's own doc comments have pointed at
    /// it since M1, and a regeneration command that silently drops tombstones is exactly the trap
    /// being removed.
    #[test]
    #[ignore = "one-shot generator, not part of the regular suite"]
    fn generate_params_lock() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../params.lock");
        let old = std::fs::read_to_string(path).unwrap_or_default();
        std::fs::write(path, merge_manifest(&old, REGISTRY)).unwrap();
    }

    #[test]
    fn registry_has_no_duplicate_keys_or_ids() {
        use std::collections::HashSet;
        let mut keys = HashSet::new();
        let mut ids = HashSet::new();
        for d in REGISTRY {
            assert!(keys.insert(d.key), "duplicate key: {}", d.key);
            assert!(ids.insert(d.id), "duplicate id for key: {}", d.key);
        }
    }

    /// M14: compares against `merge_manifest(the file, REGISTRY)`, not `render_manifest(REGISTRY)`.
    /// The old form was the third of the three live-only comparisons that made a committed
    /// tombstone fail the gate permanently (FR-PARAM-020, issue #31); with no tombstone in the file
    /// the two are byte-identical, so this assertion is unchanged in strength.
    #[test]
    fn params_lock_matches_the_merged_manifest_of_registry() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../params.lock");
        let actual =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
        assert_eq!(
            actual,
            merge_manifest(&actual, REGISTRY),
            "params.lock is stale -- regenerate it with `cargo run -p xtask -- params-lock --write`"
        );
    }

    /// The other half of the same property, and the one the assertion above cannot make: the
    /// checked-in file must also *pass* `check_manifest`, which is what detects a reused tombstone
    /// or a changed id. `merge_manifest` deliberately does not adjudicate those (see its doc
    /// comment), so without this the merged comparison above would go green on a file the manifest
    /// rules reject.
    #[test]
    fn params_lock_satisfies_check_manifest_against_the_registry() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../params.lock");
        let actual =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
        if let Err(violations) = check_manifest(&actual, REGISTRY) {
            let rendered: Vec<String> = violations.iter().map(ToString::to_string).collect();
            panic!("params.lock violates D-10.1:\n  {}", rendered.join("\n  "));
        }
    }
}
