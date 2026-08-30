//! Bit-reproducible elementary functions, for the parts of this crate whose *output bytes* are a
//! fixture other machines have to reproduce.
//!
//! # Why this module exists
//!
//! D-19.1's premise is that a fixture is regenerable from its `(shape, seed)`: the file is the
//! artifact, the generator is the recipe, and anyone can re-run the recipe and get the file back.
//! `std`'s `f32::tanh`, `f32::sin` and `f64::exp` quietly break that premise. Rust delegates all
//! three to the platform's libm, and libms do not agree bit for bit — the C standard requires
//! neither correct rounding nor a shared implementation for any of them. Everything else in these
//! generators is exact: `rand_pcg` produces the same bits everywhere, `+ - * /` and `sqrt` are
//! IEEE-754 correctly rounded, Rust performs no FMA contraction or reassociation without an
//! explicit request, and `serde_json` prints floats through `ryu`, which is exact.
//!
//! This is not a theoretical hazard; it has already cost this project a CI round. M14's
//! `the_a2_golden_models_match_their_generator` was a byte comparison, and it failed on all three
//! CI platforms while passing on the machine that wrote the goldens — **two bytes out of 205 986**,
//! `config.head_scale` and the trailing weight that mirrors it (`0.15790403` locally,
//! `0.15790401` on every runner). Every one of the ~50 000 RNG-derived weights matched exactly.
//! `head_scale` is calibrated as `base * (target_rms / measured_rms)`, and `measure_output_rms`
//! runs a whole inference pass, so one differing libm result anywhere in it lands in that one
//! float. The A2 generator's *only* transcendental is `calibration_probe`'s `sin` — A2 inference
//! itself is LeakyReLU, pure arithmetic — which is as direct a fingerprint of libm as the evidence
//! could offer. That test was relaxed to a relative tolerance rather than fixed; this module fixes
//! the cause.
//!
//! # What is guaranteed here, and what is not
//!
//! Every function below is built exclusively from IEEE-754 `f64` arithmetic — add, subtract,
//! multiply, divide, comparison, and bit-level scaling by powers of two — with no library call and
//! no table lookup. Each of those operations is correctly rounded and fully specified by the
//! standard, so **the same input produces the same bits on every platform this project targets**,
//! independent of the C library, the compiler version and the instruction set. (The one
//! environment that would break the claim is a target evaluating `f64` in x87 extended precision —
//! 32-bit x86 without SSE2 — which this project does not build for.)
//!
//! Accuracy is a *separate* claim and a weaker one: these are faithful, not proven
//! correctly-rounded. Each is evaluated in `f64` and rounded once to `f32` at the end, so a
//! result carrying ~1e-16 of relative error in `f64` lands on the correctly-rounded `f32` for all
//! but inputs sitting within that distance of an `f32` midpoint. The module's own tests sweep each
//! function against `std`'s **double**-precision counterpart, rounded once, and assert agreement
//! within one `f32` ULP — with over 99% of samples agreeing exactly. They deliberately do not
//! compare against `std`'s *single*-precision functions, which on this sandbox are themselves up to
//! two ULP out (a worked example is in the tests); that error is a property of one platform's libm,
//! which is the thing this module exists to keep out of a fixture.
//!
//! # Scope
//!
//! Only the `nam` generation and reference-inference path uses this, because only that path's
//! output is a checked-in fixture: `crates/namir-nam/tests/golden/*.nam` and
//! `crates/namir-nam/fuzz/corpus/load_nam/valid_nano.json`. [`crate::ir`]'s designed filters still
//! call `std` — their output is never committed, only compared under a tolerance — and
//! [`crate::resample_response`] likewise.

/// `ln(2)` split so that `k * LN2_HI` is *exact* for every `k` a range reduction can produce:
/// `LN2_HI` carries only the top 33 significant bits, leaving room for a 20-bit integer factor in
/// a 53-bit mantissa. The pair sums to `ln(2)` to about 1e-27, so the reduced argument keeps full
/// precision instead of inheriting the rounding error of a single-`f64` `ln(2)`.
const LN2_HI: f64 = 6.931_471_803_691_238e-1;
const LN2_LO: f64 = 1.908_214_929_270_587_7e-10;
const LOG2_E: f64 = std::f64::consts::LOG2_E;

/// `pi/2` split three ways, for the same reason [`LN2_HI`]/[`LN2_LO`] are split two ways: each
/// part is truncated to 33 significant bits or fewer, so `n * PIO2_i` is exact for `|n| < 2^20`
/// and the subtractions below lose nothing. Three parts rather than two because a sine argument
/// can be far larger, relative to `pi/2`, than an exponent argument is relative to `ln(2)`.
const PIO2_1: f64 = 1.570_796_326_734_125_6;
const PIO2_2: f64 = 6.077_100_506_506_192e-11;
const PIO2_3: f64 = 2.022_266_248_711_166_5e-21;

/// The largest `|x|` [`sin_f32`]'s exact argument reduction covers (`2^20 * pi/2`). Above it the
/// reduction falls back to plain `f64` arithmetic: still bit-identical everywhere — that is this
/// module's whole point and it holds for every input — but no longer accurate, because the
/// three-part constant above runs out of bits. Nothing in this crate comes close: the calibration
/// probe's largest argument is about 230 radians.
const SIN_EXACT_REDUCTION_LIMIT: f64 = 1_647_099.0;

/// `2^k` as an `f64`, for `-1022 <= k <= 1023`, by writing the exponent field directly. Exact, and
/// deliberately not `2.0f64.powi(k)` — `powi` is a compiler intrinsic whose lowering is not
/// something this module wants to depend on.
fn pow2(k: i32) -> f64 {
    debug_assert!((-1022..=1023).contains(&k));
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// `e^x`, to within about one `f64` ULP over the whole finite range, using only arithmetic.
///
/// Cody-Waite range reduction (`x = k*ln2 + r`, `|r| <= ln2/2`) followed by the Taylor series for
/// `e^r`, whose terms fall off as `0.347^n / n!`: truncating after `r^16/16!` leaves a remainder
/// around 1e-23 relative, far inside `f64`'s own resolution. Horner's form keeps it to sixteen
/// multiply-divide-adds.
pub fn exp_f64(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 709.9 {
        return f64::INFINITY;
    }
    if x < -745.2 {
        return 0.0;
    }

    let k = (x * LOG2_E).round();
    let k_i = k as i32;
    // Both products are exact (see LN2_HI), so `r` carries no reduction error of its own.
    let r = (x - k * LN2_HI) - k * LN2_LO;

    // Horner on `e^r = 1 + r/1 (1 + r/2 (1 + ... r/16))`, innermost term first.
    let mut sum = 1.0f64;
    for n in (1..=16u32).rev() {
        sum = sum * r / f64::from(n) + 1.0;
    }

    // Split the scaling in two so a result that is subnormal or near the top of the range still
    // goes through two in-range `pow2` factors rather than one out-of-range one.
    let half = k_i.clamp(-1000, 1000) / 2;
    sum * pow2(half) * pow2(k_i - half)
}

/// `tanh(x)` for an `f32`, computed in `f64` and rounded once.
///
/// `(e^{2x} - 1) / (e^{2x} + 1)`, with the tail cut off at `|x| > 20` (where the true value is
/// within 1e-17 of ±1, far inside `f32`'s resolution, and where `e^{2x}` would otherwise reach
/// infinity and turn the quotient into a NaN). The cancellation in `e^{2x} - 1` as `x` approaches
/// zero is harmless at this width: at `x = 1e-3` it costs about three decimal digits of a
/// sixteen-digit `f64` intermediate, leaving ten more than the `f32` result can express.
pub fn tanh_f32(x: f32) -> f32 {
    let xd = f64::from(x);
    if xd.is_nan() {
        return x;
    }
    if xd > 20.0 {
        return 1.0;
    }
    if xd < -20.0 {
        return -1.0;
    }
    let e = exp_f64(2.0 * xd);
    ((e - 1.0) / (e + 1.0)) as f32
}

/// The logistic sigmoid `1 / (1 + e^{-x})` for an `f32`, computed in `f64` and rounded once — the
/// LSTM gate nonlinearity, and the second place `std`'s `exp` used to enter a generated fixture.
pub fn sigmoid_f32(x: f32) -> f32 {
    let xd = f64::from(x);
    if xd.is_nan() {
        return x;
    }
    (1.0 / (1.0 + exp_f64(-xd))) as f32
}

/// `sin(r)` for `|r| <= pi/4`, by Taylor series. Terms fall off as `0.7854^n / n!`; stopping after
/// `r^19/19!` leaves under 1e-19 absolute, below `f64`'s resolution at this magnitude.
fn sin_kernel(r: f64) -> f64 {
    let r2 = r * r;
    let mut sum = -1.0 / 121_645_100_408_832_000.0; // -1/19!
    for (denom, sign) in [
        (355_687_428_096_000.0f64, 1.0f64), // 17!
        (1_307_674_368_000.0, -1.0),        // 15!
        (6_227_020_800.0, 1.0),             // 13!
        (39_916_800.0, -1.0),               // 11!
        (362_880.0, 1.0),                   // 9!
        (5_040.0, -1.0),                    // 7!
        (120.0, 1.0),                       // 5!
        (6.0, -1.0),                        // 3!
    ] {
        sum = sum * r2 + sign / denom;
    }
    r + r * r2 * sum
}

/// `cos(r)` for `|r| <= pi/4`, by Taylor series, truncated after `r^20/20!` on the same argument
/// as [`sin_kernel`]'s.
fn cos_kernel(r: f64) -> f64 {
    let r2 = r * r;
    let mut sum = 1.0 / 2_432_902_008_176_640_000.0; // 1/20!
    for (denom, sign) in [
        (6_402_373_705_728_000.0f64, -1.0f64), // 18!
        (20_922_789_888_000.0, 1.0),           // 16!
        (87_178_291_200.0, -1.0),              // 14!
        (479_001_600.0, 1.0),                  // 12!
        (3_628_800.0, -1.0),                   // 10!
        (40_320.0, 1.0),                       // 8!
        (720.0, -1.0),                         // 6!
        (24.0, 1.0),                           // 4!
        (2.0, -1.0),                           // 2!
    ] {
        sum = sum * r2 + sign / denom;
    }
    1.0 + r2 * sum
}

/// `sin(x)` for an `f32`, computed in `f64` and rounded once.
///
/// Cody-Waite reduction to `|r| <= pi/4` plus a quadrant index, then [`sin_kernel`]/[`cos_kernel`].
/// See [`SIN_EXACT_REDUCTION_LIMIT`] for the (unreachable, in this crate) argument magnitude at
/// which the reduction stops being exact — determinism is unaffected there, only accuracy.
pub fn sin_f32(x: f32) -> f32 {
    let xd = f64::from(x);
    if !xd.is_finite() {
        return f32::NAN;
    }

    let n = (xd * std::f64::consts::FRAC_2_PI).round();
    let r = if xd.abs() <= SIN_EXACT_REDUCTION_LIMIT {
        ((xd - n * PIO2_1) - n * PIO2_2) - n * PIO2_3
    } else {
        xd - n * std::f64::consts::FRAC_PI_2
    };

    // `n mod 4` decides which of ±sin, ±cos the reduced argument feeds.
    let quadrant = (n as i64).rem_euclid(4);
    let y = match quadrant {
        0 => sin_kernel(r),
        1 => cos_kernel(r),
        2 => -sin_kernel(r),
        _ => -cos_kernel(r),
    };
    y as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole module exists for cannot be tested from one machine — "these bytes
    /// are the same on Windows, Linux and macOS" needs three machines. What *is* testable here is
    /// the premise that makes it true: every one of these functions is built from IEEE-754
    /// arithmetic alone, which is specified to the bit. So these tests check the other half — that
    /// the deterministic implementations are *also* accurate enough to stand in for `std`'s —
    /// leaving the determinism claim resting on the operations used, not on a measurement.
    ///
    /// One ULP of `f32` is the bar, measured against the **`f64`** function of the same name —
    /// `f64::from(x).tanh() as f32`, not `x.tanh()`. That is deliberate, and the reason is the
    /// issue itself: this sandbox's single-precision libm is not correctly rounded, so comparing
    /// against it would measure its error rather than this module's. A concrete case found while
    /// writing these tests: for `x = -0.544f32` the true `tanh` is `-0.4960098653132381`, whose
    /// nearest `f32` is `-0.49600986` — what this module returns — while `x.tanh()` returns
    /// `-0.49600992`, two ULP away. Rounding the `f64` result once is the accurate reference, and
    /// the fact that `f32`'s libm misses it by two ULP *here* is exactly the platform-to-platform
    /// variation the module removes from generated fixtures.
    ///
    /// At one ULP a substituted value moves a calibrated `head_scale` by ~6e-8 relative, orders of
    /// magnitude below every tolerance the golden tests assert.
    fn assert_within_one_ulp(ours: f32, theirs: f32, label: &str) {
        if ours == theirs {
            return;
        }
        assert!(
            ours.is_finite() && theirs.is_finite(),
            "{label}: {ours} vs std's {theirs}"
        );
        let ulps = (ours.to_bits() as i64 - theirs.to_bits() as i64).abs();
        assert!(
            ulps <= 1,
            "{label}: {ours} vs std's {theirs} ({ulps} ULP apart)"
        );
    }

    #[test]
    fn tanh_agrees_with_std_to_within_one_ulp() {
        let mut exact = 0u32;
        let mut total = 0u32;
        for i in -40_000i32..40_000 {
            let x = i as f32 / 1_000.0;
            let ours = tanh_f32(x);
            let theirs = f64::from(x).tanh() as f32;
            assert_within_one_ulp(ours, theirs, &format!("tanh({x})"));
            exact += u32::from(ours == theirs);
            total += 1;
        }
        assert!(
            exact * 100 >= total * 99,
            "only {exact}/{total} tanh samples matched std exactly"
        );
    }

    #[test]
    fn sigmoid_agrees_with_the_std_expression_to_within_one_ulp() {
        for i in -40_000i32..40_000 {
            let x = i as f32 / 1_000.0;
            assert_within_one_ulp(
                sigmoid_f32(x),
                (1.0 / (1.0 + (-f64::from(x)).exp())) as f32,
                &format!("sigmoid({x})"),
            );
        }
    }

    #[test]
    fn sin_agrees_with_std_to_within_one_ulp_across_the_probes_range() {
        // The calibration probe's arguments run to about 230 radians; sweep well past that.
        let mut exact = 0u32;
        let mut total = 0u32;
        for i in -100_000i32..100_000 {
            let x = i as f32 / 200.0;
            let ours = sin_f32(x);
            let theirs = f64::from(x).sin() as f32;
            assert_within_one_ulp(ours, theirs, &format!("sin({x})"));
            exact += u32::from(ours == theirs);
            total += 1;
        }
        assert!(
            exact * 100 >= total * 99,
            "only {exact}/{total} sin samples matched std exactly"
        );
    }

    #[test]
    fn exp_agrees_with_std_across_a_wide_range() {
        for i in -70_000i32..70_000 {
            let x = f64::from(i) / 100.0;
            let ours = exp_f64(x);
            let theirs = x.exp();
            let ulps = (ours.to_bits() as i128 - theirs.to_bits() as i128).abs();
            assert!(ulps <= 2, "exp({x}): {ours} vs std's {theirs} ({ulps} ULP)");
        }
    }

    #[test]
    fn the_edges_behave() {
        assert_eq!(tanh_f32(0.0), 0.0);
        assert_eq!(tanh_f32(100.0), 1.0);
        assert_eq!(tanh_f32(-100.0), -1.0);
        assert!(tanh_f32(f32::NAN).is_nan());
        assert_eq!(sigmoid_f32(0.0), 0.5);
        assert_eq!(sigmoid_f32(-1_000.0), 0.0);
        assert_eq!(sigmoid_f32(1_000.0), 1.0);
        assert_eq!(sin_f32(0.0), 0.0);
        assert!(sin_f32(f32::INFINITY).is_nan());
        assert_eq!(exp_f64(0.0), 1.0);
        assert_eq!(exp_f64(1_000.0), f64::INFINITY);
        assert_eq!(exp_f64(-1_000.0), 0.0);
        assert!(exp_f64(f64::NAN).is_nan());
    }

    /// The static half of the guarantee, and the test that was red before this module existed:
    /// **no file in the `.nam` generation path may call a platform transcendental**. Accuracy
    /// tests cannot catch a regression here — a reintroduced `f32::tanh` would agree with
    /// `detmath::tanh_f32` to a ULP on this machine and still make the generated fixture
    /// platform-dependent, which is the whole defect. What makes a fixture reproducible is *which
    /// operations* produced it, so that is what this checks.
    ///
    /// Scoped to `src/nam/`, the only path whose output is a checked-in artifact. `sqrt` is
    /// deliberately absent from the list: IEEE-754 requires it to be correctly rounded, so it is
    /// as reproducible as multiplication.
    #[test]
    fn the_nam_generator_calls_no_platform_transcendental() {
        const FORBIDDEN: [&str; 10] = [
            ".tanh()", ".sin()", ".cos()", ".exp()", ".exp2()", ".ln()", ".log10()", ".log2()",
            ".powf(", ".powi(",
        ];
        let nam_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/nam");
        let mut scanned = 0;
        let mut offences = Vec::new();
        for entry in std::fs::read_dir(&nam_dir).expect("src/nam is readable") {
            let path = entry.expect("a readable directory entry").path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("a readable source file");
            scanned += 1;
            for (n, line) in source.lines().enumerate() {
                // Comments name these functions constantly (this file included); only code counts.
                let code = line.split("//").next().unwrap_or("");
                for needle in FORBIDDEN {
                    if code.contains(needle) {
                        offences.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            scanned >= 4,
            "expected to scan the whole nam module, saw {scanned} files"
        );
        assert!(
            offences.is_empty(),
            "the .nam generator must go through `crate::detmath`, not the platform libm, or its \
             output bytes stop being reproducible across platforms:\n{}",
            offences.join("\n")
        );
    }

    /// Sanity on the reduction: `sin` of an exact multiple of pi must be tiny, and the quadrant
    /// walk must produce the right signs.
    #[test]
    fn sin_reduces_correctly_over_many_periods() {
        for k in 0..200 {
            let x = (k as f32) * std::f32::consts::PI;
            assert!(
                sin_f32(x).abs() < 1e-3,
                "sin({k}pi) = {} is not near zero",
                sin_f32(x)
            );
            let peak = ((k as f32) + 0.5) * std::f32::consts::PI;
            let expected = if k % 2 == 0 { 1.0 } else { -1.0 };
            assert!(
                (sin_f32(peak) - expected).abs() < 1e-3,
                "sin(({k}+0.5)pi) = {} should be {expected}",
                sin_f32(peak)
            );
        }
    }
}
