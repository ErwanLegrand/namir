//! D-10.2: "a stable u32 derived from a namespaced string ... hosts see the u32". This is only
//! the id type the RT path needs to carry. The string-to-u32 derivation, the checked-in manifest
//! (`params.lock`, D-10.1) and the stage-instance index D-10.2 reserves for RD-2's dynamic chain
//! are all out of scope here — none of them are needed to define what `Stage::apply` receives.

/// D-10.2's stable `u32` parameter identifier, as carried across the RT boundary. The
/// string-namespace-to-`u32` derivation itself lives outside this crate (see this module's doc
/// comment); this is just the id type `Stage::apply` receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParamId(pub u32);

/// A single parameter update, as delivered to `Stage::apply` (D-6.1). Carries no smoothing
/// information: D-10.3 assigns smoothing to a parameter *descriptor*, which doesn't exist at
/// this layer yet — a stage that needs to avoid a zipper on this value ramps internally.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamChange {
    /// Which parameter changed.
    pub id: ParamId,
    /// The new value, always `f32` at this layer (see this struct's doc comment for the
    /// stepped/discrete-parameter gap that leaves open).
    pub value: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_compare_by_value() {
        assert_eq!(ParamId(7), ParamId(7));
        assert_ne!(ParamId(7), ParamId(8));
    }
}
