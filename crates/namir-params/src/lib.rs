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
//! # [`REGISTRY`] is empty by design, today
//!
//! None of the six 1.0 stages (Trim/Gate/Nam/Ir/Eq/Out) exist yet — that is M2's work per the
//! roadmap's own M0 audit (`03-implementation-roadmap.md` §2). So [`REGISTRY`] is `&[]`: this
//! crate delivers the descriptor type, the id derivation, and the manifest mechanism; M2 is what
//! populates it with real entries as each stage lands. Every piece of that system is still
//! exercised thoroughly by this crate's own tests, using local example descriptors defined in
//! each test module — not part of [`REGISTRY`], which stays honestly empty until real parameters
//! exist to put in it.

mod descriptor;
mod error_codes;
mod id;
mod manifest;

pub use descriptor::{ParamDescriptor, ParamKind, SmoothingCategory, StepIndex, Unit, ValueFormat};
pub use error_codes::ManifestViolation;
pub use id::ParamId;
pub use manifest::{FORMAT_VERSION, check_manifest, render_manifest};

/// The full set of parameters this build knows about (D-10.1). Empty at M1 — see the crate doc
/// comment's "`REGISTRY` is empty by design" section. `params.lock` at the repository root is
/// exactly [`render_manifest`]`(REGISTRY)`'s current output.
pub const REGISTRY: &[ParamDescriptor] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "one-shot generator, not part of the regular suite"]
    fn generate_params_lock() {
        let repo_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../params.lock");
        std::fs::write(repo_root, render_manifest(REGISTRY)).unwrap();
    }

    #[test]
    fn registry_is_empty_at_m1() {
        assert!(REGISTRY.is_empty());
    }

    #[test]
    fn params_lock_matches_render_manifest_of_registry() {
        let expected = render_manifest(REGISTRY);
        let repo_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../params.lock");
        let actual = std::fs::read_to_string(repo_root)
            .unwrap_or_else(|e| panic!("failed to read {repo_root}: {e}"));
        assert_eq!(
            actual, expected,
            "params.lock is stale -- regenerate it with render_manifest(REGISTRY)"
        );
    }
}
