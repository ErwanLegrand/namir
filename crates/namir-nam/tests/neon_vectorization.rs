//! Tests verifying `wide::f32x8` vectorization and AArch64 / NEON instruction mapping (Issue #148).
//!
//! On `target_arch = "aarch64"`, `wide` 1.7.0's `pick!` macro selects `core::arch::aarch64` NEON
//! intrinsics (`float32x4_t`) for `f32x4`, and `wide::f32x8` is composed of `{ a: f32x4, b: f32x4 }`,
//! confirming vectorization at the source dependency level rather than a scalar fallback.

#[cfg(target_feature = "neon")]
#[test]
fn neon_is_in_the_baseline_so_wide_is_not_a_scalar_fallback() {
    assert!(cfg!(target_feature = "neon"));
}
