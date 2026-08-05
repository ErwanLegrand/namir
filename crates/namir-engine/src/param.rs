//! D-10.2: "a stable u32 derived from a namespaced string ... hosts see the u32". This is only
//! the id type the RT path needs to carry. The string-to-u32 derivation, the checked-in manifest
//! (`params.lock`, D-10.1) and the stage-instance index D-10.2 reserves for RD-2's dynamic chain
//! are all out of scope here — none of them are needed to define what `Stage::apply` receives.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParamId(pub u32);

/// A single parameter update, as delivered to `Stage::apply` (D-6.1). Carries no smoothing
/// information: D-10.3 assigns smoothing to a parameter *descriptor*, which doesn't exist at
/// this layer yet — a stage that needs to avoid a zipper on this value ramps internally.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamChange {
    pub id: ParamId,
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
