//! D-7.4: puts the FPU into flush-to-zero / denormals-are-zero mode for the duration of an audio
//! callback and restores the previous mode on drop, so NFR-RT-030 ("denormal floating-point
//! numbers shall not cause a measurable CPU spike") is met by a CPU flag rather than, per D-7.4's
//! *Rejected* alternative, a DC-offset workaround smeared across every DSP module.
//!
//! Confined to this one module per D-5.3 / NFR-QUAL-070 — see this crate's `Cargo.toml` for why
//! the crate as a whole cannot carry `#![forbid(unsafe_code)]` the way everything at or below
//! `namir-engine` does.

#![allow(unsafe_code)]

/// MXCSR bit 15 (FTZ, flush-to-zero: a subnormal *result* is replaced by zero) and bit 6 (DAZ,
/// denormals-are-zero: a subnormal *input* is treated as zero before the operation runs). Both
/// are needed together: FTZ alone still takes the slow microcode path for subnormal inputs
/// arriving from elsewhere (e.g. another plugin's filter tail feeding into this one).
#[cfg(target_arch = "x86_64")]
const FTZ_DAZ_MASK: u32 = (1 << 15) | (1 << 6);

/// FPCR bit 24 (FZ, flush-to-zero for normal-precision arithmetic). AArch64 does not split this
/// into separate input/output bits the way x86_64's MXCSR does — FZ alone governs both directions
/// for the normal-precision case relevant to audio processing here.
#[cfg(target_arch = "aarch64")]
const FZ_MASK: u64 = 1 << 24;

/// Puts the FPU into flush-to-zero / denormals-are-zero mode for as long as it's alive, restoring
/// the exact prior mode on drop. `Drop` runs unconditionally — on an early return or a
/// panic-driven unwind through the guard's scope — which is D-7.4's whole point: the mode "cannot
/// leak even on an early return."
///
/// On an architecture this crate doesn't have a denormal-control implementation for, the guard
/// still constructs and drops cleanly; it just doesn't change anything (NFR-PORT-030 must not be
/// precluded by this guard failing to compile on, say, a 32-bit ARM target).
#[cfg(target_arch = "x86_64")]
pub struct DenormalGuard {
    previous_mxcsr: u32,
}

/// Puts the FPU into flush-to-zero / denormals-are-zero mode for as long as it's alive, restoring
/// the exact prior mode on drop. `Drop` runs unconditionally — on an early return or a
/// panic-driven unwind through the guard's scope — which is D-7.4's whole point: the mode "cannot
/// leak even on an early return."
///
/// On an architecture this crate doesn't have a denormal-control implementation for, the guard
/// still constructs and drops cleanly; it just doesn't change anything (NFR-PORT-030 must not be
/// precluded by this guard failing to compile on, say, a 32-bit ARM target).
#[cfg(target_arch = "aarch64")]
pub struct DenormalGuard {
    previous_fpcr: u64,
}

/// Puts the FPU into flush-to-zero / denormals-are-zero mode for as long as it's alive, restoring
/// the exact prior mode on drop. `Drop` runs unconditionally — on an early return or a
/// panic-driven unwind through the guard's scope — which is D-7.4's whole point: the mode "cannot
/// leak even on an early return."
///
/// On an architecture this crate doesn't have a denormal-control implementation for, the guard
/// still constructs and drops cleanly; it just doesn't change anything (NFR-PORT-030 must not be
/// precluded by this guard failing to compile on, say, a 32-bit ARM target).
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub struct DenormalGuard;

/// `core::arch::x86_64::_mm_getcsr`/`_mm_setcsr` exist but are deprecated as of this workspace's
/// toolchain: the intrinsic form carries no memory clobber, so the compiler is free to reorder
/// floating-point loads/stores across it in a way that can observe the wrong MXCSR mode — exactly
/// the kind of bug a denormal guard must not have. `stmxcsr`/`ldmxcsr` issued by hand through
/// `asm!`, taking an explicit memory operand, is the replacement the deprecation notice itself
/// points to.
#[cfg(target_arch = "x86_64")]
fn read_mxcsr() -> u32 {
    let mut value: u32 = 0;
    // SAFETY: `stmxcsr` stores the 4-byte MXCSR value to the address given by `{0}`, which is
    // `&mut value as *mut u32` — a valid, properly aligned, writable place for the instruction's
    // whole duration, not observed through any other reference while this call runs. No memory
    // besides that one `u32` is touched, and the register being read (MXCSR) is FPU control
    // state, not addressable memory, so this cannot violate Rust's memory-safety guarantees.
    unsafe {
        core::arch::asm!(
            "stmxcsr [{0}]",
            in(reg) &mut value as *mut u32,
            options(nostack, preserves_flags),
        );
    }
    value
}

#[cfg(target_arch = "x86_64")]
fn write_mxcsr(value: u32) {
    // SAFETY: `ldmxcsr` loads the 4-byte MXCSR value from the address given by `{0}`, which is
    // `&value as *const u32` — a valid, aligned, readable place for the instruction's duration.
    // The instruction only ever reads memory here (never writes it), matching the `readonly`
    // option below, and the register it loads (MXCSR) is FPU control state, not memory, so this
    // cannot violate Rust's memory-safety guarantees.
    unsafe {
        core::arch::asm!(
            "ldmxcsr [{0}]",
            in(reg) &value as *const u32,
            options(nostack, preserves_flags, readonly),
        );
    }
}

#[cfg(target_arch = "x86_64")]
impl DenormalGuard {
    /// Engages FTZ/DAZ immediately, capturing the current MXCSR so `Drop` can restore it exactly.
    pub fn new() -> Self {
        let previous_mxcsr = read_mxcsr();
        write_mxcsr(previous_mxcsr | FTZ_DAZ_MASK);
        Self { previous_mxcsr }
    }
}

#[cfg(target_arch = "aarch64")]
impl DenormalGuard {
    /// Engages FZ immediately, capturing the current FPCR so `Drop` can restore it exactly.
    pub fn new() -> Self {
        let previous_fpcr: u64;
        // SAFETY: `mrs`/`msr` against `fpcr` read/write a CPU control register directly, the same
        // way the x86_64 MXCSR intrinsics do — the instruction touches no memory, so it cannot
        // violate memory safety. The only output is a plain 64-bit integer with no aliasing
        // implications, and `fpcr` writes affect neither the stack pointer nor the NZCV
        // condition flags, so `nomem, nostack, preserves_flags` accurately describe this asm
        // block's effects.
        unsafe {
            core::arch::asm!(
                "mrs {0}, fpcr",
                out(reg) previous_fpcr,
                options(nomem, nostack, preserves_flags),
            );
        }
        let new_fpcr = previous_fpcr | FZ_MASK;
        // SAFETY: same argument as the read above.
        unsafe {
            core::arch::asm!(
                "msr fpcr, {0}",
                in(reg) new_fpcr,
                options(nomem, nostack, preserves_flags),
            );
        }
        Self { previous_fpcr }
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
impl DenormalGuard {
    /// No-op on an architecture this crate has no denormal-control implementation for; see this
    /// struct's doc comment for why that must not preclude building here at all.
    pub fn new() -> Self {
        Self
    }
}

impl Default for DenormalGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "x86_64")]
impl Drop for DenormalGuard {
    fn drop(&mut self) {
        // Restores a value this exact guard captured from a real prior `read_mxcsr()` call in
        // `new` — not an arbitrary bit pattern — so this writes back the hardware's own previous
        // state. See `write_mxcsr`'s own SAFETY comment for the argument covering the `unsafe`
        // inside it.
        write_mxcsr(self.previous_mxcsr);
    }
}

#[cfg(target_arch = "aarch64")]
impl Drop for DenormalGuard {
    fn drop(&mut self) {
        let previous_fpcr = self.previous_fpcr;
        // SAFETY: restores a value this exact guard captured from a real prior `mrs` read in
        // `new`. As above, touches no memory and cannot violate memory safety.
        unsafe {
            core::arch::asm!(
                "msr fpcr, {0}",
                in(reg) previous_fpcr,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_and_drops_cleanly() {
        let guard = DenormalGuard::new();
        drop(guard);
    }

    #[cfg(target_arch = "x86_64")]
    mod x86_64_tests {
        use super::*;

        // Reuses the crate's own `read_mxcsr` rather than calling an intrinsic directly, so the
        // test observes exactly what the guard observes/restores.
        use super::read_mxcsr as mxcsr;

        #[test]
        fn engaging_sets_ftz_and_daz_bits() {
            let before = mxcsr();
            let guard = DenormalGuard::new();
            let during = mxcsr();
            assert_eq!(
                during,
                before | FTZ_DAZ_MASK,
                "engaging should set exactly FTZ+DAZ on top of whatever was already there"
            );
            drop(guard);
        }

        #[test]
        fn dropping_restores_exact_prior_mxcsr() {
            let before = mxcsr();
            let guard = DenormalGuard::new();
            assert_ne!(
                mxcsr(),
                before,
                "guard should have changed something to restore"
            );
            drop(guard);
            assert_eq!(
                mxcsr(),
                before,
                "drop should restore the exact prior bit pattern"
            );
        }

        #[test]
        fn sequential_cycles_do_not_leak_bits() {
            let baseline = mxcsr();
            for _ in 0..5 {
                let guard = DenormalGuard::new();
                assert_eq!(mxcsr() & FTZ_DAZ_MASK, FTZ_DAZ_MASK);
                drop(guard);
                assert_eq!(mxcsr(), baseline, "a prior cycle leaked bits into this one");
            }
        }

        #[test]
        fn nested_guards_restore_in_reverse_order_without_leaking() {
            let baseline = mxcsr();
            let outer = DenormalGuard::new();
            let engaged = mxcsr();
            assert_eq!(engaged, baseline | FTZ_DAZ_MASK);

            let inner = DenormalGuard::new();
            assert_eq!(
                mxcsr(),
                engaged,
                "re-engaging while already engaged is idempotent"
            );

            drop(inner);
            assert_eq!(
                mxcsr(),
                engaged,
                "dropping the inner guard must restore the outer's engaged state, not leak past it"
            );

            drop(outer);
            assert_eq!(
                mxcsr(),
                baseline,
                "dropping the outer guard must restore the true baseline"
            );
        }

        #[test]
        fn subnormal_product_is_flushed_only_while_engaged() {
            // Chosen so the mathematically exact product (1e-40) falls in f32's subnormal range
            // (below ~1.1755e-38, above 0) rather than underflowing to true zero on its own.
            let a: f32 = std::hint::black_box(1e-20_f32);
            let b: f32 = std::hint::black_box(1e-20_f32);

            let unflushed = std::hint::black_box(a) * std::hint::black_box(b);
            assert!(
                unflushed != 0.0,
                "expected a genuine nonzero subnormal, got exact zero"
            );
            assert!(
                unflushed.is_subnormal(),
                "expected {unflushed} to be subnormal"
            );

            let guard = DenormalGuard::new();
            let flushed = std::hint::black_box(a) * std::hint::black_box(b);
            assert_eq!(
                flushed, 0.0,
                "with FTZ/DAZ engaged the subnormal product must flush to exact zero"
            );
            drop(guard);

            let restored = std::hint::black_box(a) * std::hint::black_box(b);
            assert_eq!(
                restored, unflushed,
                "dropping the guard must restore ordinary subnormal handling"
            );
        }
    }
}
