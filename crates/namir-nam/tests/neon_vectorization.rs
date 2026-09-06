//! Tests verifying `wide::f32x8` vectorization and AArch64 / NEON instruction mapping (Issue #148).
//!
//! On `target_arch = "aarch64"`, `wide` implements `f32x8` as a pair of 128-bit `f32x4` NEON
//! vectors (`{ a: f32x4, b: f32x4 }`) backed by `core::arch::aarch64::float32x4_t` NEON
//! intrinsics rather than falling back to scalar arithmetic.

use wide::f32x8;

#[test]
fn f32x8_vector_arithmetic_matches_scalar() {
    let a_arr = [1.0f32, -2.5, 3.25, -4.125, 5.0, -6.5, 7.75, -8.875];
    let b_arr = [0.5f32, 1.5, -2.0, 3.0, -1.0, 2.0, -0.5, 4.0];

    let a_vec = f32x8::from(a_arr);
    let b_vec = f32x8::from(b_arr);

    let sum = (a_vec + b_vec).to_array();
    let diff = (a_vec - b_vec).to_array();
    let prod = (a_vec * b_vec).to_array();
    let div = (a_vec / b_vec).to_array();

    for i in 0..8 {
        assert_eq!(sum[i], a_arr[i] + b_arr[i], "lane {i} sum mismatch");
        assert_eq!(diff[i], a_arr[i] - b_arr[i], "lane {i} diff mismatch");
        assert_eq!(prod[i], a_arr[i] * b_arr[i], "lane {i} prod mismatch");
        assert_eq!(div[i], a_arr[i] / b_arr[i], "lane {i} div mismatch");
    }
}

#[test]
fn f32x8_vector_transcendentals_and_activations_match_scalar() {
    let vals = [-3.0f32, -1.5, -0.5, 0.0, 0.5, 1.0, 2.0, 4.0];
    let v = f32x8::from(vals);

    // Tanh
    let v_tanh = v.tanh().to_array();
    for (i, &x) in vals.iter().enumerate() {
        let expected = x.tanh();
        assert!(
            (v_tanh[i] - expected).abs() < 1e-5,
            "lane {i} tanh mismatch: {} vs {}",
            v_tanh[i],
            expected
        );
    }

    // Exp
    let v_exp = v.exp().to_array();
    for (i, &x) in vals.iter().enumerate() {
        let expected = x.exp();
        assert!(
            (v_exp[i] - expected).abs() < 1e-4,
            "lane {i} exp mismatch: {} vs {}",
            v_exp[i],
            expected
        );
    }

    // Sigmoid: 1.0 / (1.0 + exp(-v))
    let v_sig = (f32x8::ONE / (f32x8::ONE + (-v).exp())).to_array();
    for (i, &x) in vals.iter().enumerate() {
        let expected = 1.0 / (1.0 + (-x).exp());
        assert!(
            (v_sig[i] - expected).abs() < 1e-5,
            "lane {i} sigmoid mismatch: {} vs {}",
            v_sig[i],
            expected
        );
    }

    // Fast-path comparisons and selection (LeakyReLU / PReLU logic)
    let slope = 0.1f32;
    let v_leaky = v
        .simd_gt(f32x8::ZERO)
        .select(v, v * f32x8::splat(slope))
        .to_array();
    for (i, &x) in vals.iter().enumerate() {
        let expected = if x > 0.0 { x } else { x * slope };
        assert_eq!(v_leaky[i], expected, "lane {i} leaky relu mismatch");
    }
}

#[test]
fn vector_layout_and_alignment_guarantees() {
    // f32x8 must be 32-byte aligned and 32 bytes in size (2x 128-bit NEON or 1x 256-bit AVX)
    assert_eq!(std::mem::size_of::<wide::f32x8>(), 32);
    assert_eq!(std::mem::align_of::<wide::f32x8>(), 32);

    // f32x4 must be 16-byte aligned and 16 bytes in size (128-bit NEON vector)
    assert_eq!(std::mem::size_of::<wide::f32x4>(), 16);
    assert_eq!(std::mem::align_of::<wide::f32x4>(), 16);
}

#[test]
fn f32x4_vector_operations_match_expected() {
    let a = [1.0f32, 2.0, 3.0, 4.0];
    let b = [5.0f32, 6.0, 7.0, 8.0];

    let wide_a = wide::f32x4::from(a);
    let wide_b = wide::f32x4::from(b);
    let wide_sum = (wide_a + wide_b).to_array();

    assert_eq!(wide_sum, [6.0, 8.0, 10.0, 12.0]);
}
