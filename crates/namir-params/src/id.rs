//! D-10.2: "A parameter identifier is a stable `u32` derived from a namespaced string
//! (`"gate.threshold"`), with the string retained in the manifest. Hosts see the `u32`; humans
//! see the string."
//!
//! # The hash algorithm is a permanent commitment
//!
//! [`ParamId::from_key`] uses FNV-1a, 32-bit, offset basis `2166136261`, prime `16777619`, over
//! the key's UTF-8 bytes. FNV-1a was picked for being small enough to hand-verify and implement
//! without a dependency, not for any cryptographic property — collision resistance doesn't matter
//! here, `check_manifest` (see `manifest.rs`) is what actually catches an id collision, not the
//! hash function's quality.
//!
//! What *does* matter: once any real parameter ships, this exact algorithm can never change.
//! `ParamId::from_key("gate.threshold")` must produce the same `u32` in every future build,
//! forever — changing the constants, the byte encoding, or the algorithm itself would silently
//! reassign every existing identifier, which is precisely what FR-PARAM-020 ("Parameter
//! identifiers shall be stable across versions") forbids. Nothing in the type system enforces
//! that permanence; [`crate::render_manifest`]/[`crate::check_manifest`] and the checked-in
//! `params.lock` (D-10.1) are the actual guard rail — this comment is the other half of it.

/// A stable numeric parameter identifier, derived from (and always paired with, in a
/// [`crate::ParamDescriptor`]) a namespaced key string. Hosts (CLAP automation, saved projects)
/// only ever see this `u32`; the key string is what humans and source code work with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParamId(pub u32);

const FNV_OFFSET_BASIS: u32 = 2166136261;
const FNV_PRIME: u32 = 16777619;

impl ParamId {
    /// Derives a [`ParamId`] from a namespaced key such as `"gate.threshold"`. Pure and
    /// deterministic: the same key always yields the same id, in this build and every future one
    /// (see the module doc comment).
    pub const fn from_key(key: &str) -> ParamId {
        ParamId(fnv1a_32(key.as_bytes()))
    }
}

/// FNV-1a, 32-bit, over `bytes`. `pub(crate)` so [`crate::manifest`] can fingerprint a stepped
/// parameter's value labels with the same hash this crate already commits to for keys; nothing
/// outside this crate may depend on the algorithm.
pub(crate) const fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut hash = FNV_OFFSET_BASIS;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    // Locked down by hand from the algorithm above so a future accidental change to the
    // constants or byte encoding is caught here, not just by a "changed shape" observation.
    const GATE_THRESHOLD_ID: u32 = 0xa6fa_c247;

    #[test]
    fn known_key_derives_the_documented_id() {
        assert_eq!(
            ParamId::from_key("gate.threshold"),
            ParamId(GATE_THRESHOLD_ID)
        );
    }

    #[test]
    fn derivation_is_deterministic() {
        assert_eq!(
            ParamId::from_key("nam.input_trim"),
            ParamId::from_key("nam.input_trim")
        );
    }

    #[test]
    fn different_keys_derive_different_ids() {
        assert_ne!(
            ParamId::from_key("gate.threshold"),
            ParamId::from_key("gate.release_ms")
        );
    }

    #[test]
    fn empty_key_is_the_offset_basis() {
        assert_eq!(ParamId::from_key(""), ParamId(FNV_OFFSET_BASIS));
    }

    #[test]
    fn ids_are_orderable_and_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ParamId::from_key("a"));
        set.insert(ParamId::from_key("b"));
        assert_eq!(set.len(), 2);
        assert!(ParamId(1) < ParamId(2));
    }
}
