//! D-10.2: "a stable u32 derived from a namespaced string ... hosts see the u32".
//! Re-exports [`namir_params::ParamId`] as the RT boundary identifier.

pub use namir_params::ParamId;
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
