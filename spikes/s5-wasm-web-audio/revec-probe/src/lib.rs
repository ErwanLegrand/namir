use core::arch::wasm32::*;

const N: usize = 1024;
static mut A: [f32; N] = [0.0; N];
static mut B: [f32; N] = [0.0; N];
static mut C: [f32; N] = [0.0; N];

/// Canonical revectorizer seed: two adjacent 128-bit loads, op, two adjacent 128-bit stores.
#[no_mangle]
pub unsafe extern "C" fn canonical(n: usize) {
    let a = core::ptr::addr_of_mut!(A) as *mut u8;
    let b = core::ptr::addr_of_mut!(B) as *mut u8;
    let c = core::ptr::addr_of_mut!(C) as *mut u8;
    let mut i = 0usize;
    while i < n {
        let off = i * 4;
        let a0 = v128_load(a.add(off) as *const v128);
        let a1 = v128_load(a.add(off + 16) as *const v128);
        let b0 = v128_load(b.add(off) as *const v128);
        let b1 = v128_load(b.add(off + 16) as *const v128);
        v128_store(c.add(off) as *mut v128, f32x4_add(a0, b0));
        v128_store(c.add(off + 16) as *mut v128, f32x4_add(a1, b1));
        i += 8;
    }
}

/// Same shape, but expressed through wide::f32x8.
#[no_mangle]
pub unsafe extern "C" fn via_wide(n: usize) {
    use wide::f32x8;
    let a = core::ptr::addr_of_mut!(A) as *mut f32;
    let b = core::ptr::addr_of_mut!(B) as *mut f32;
    let c = core::ptr::addr_of_mut!(C) as *mut f32;
    let mut i = 0usize;
    while i < n {
        let mut xa = [0f32; 8];
        let mut xb = [0f32; 8];
        core::ptr::copy_nonoverlapping(a.add(i), xa.as_mut_ptr(), 8);
        core::ptr::copy_nonoverlapping(b.add(i), xb.as_mut_ptr(), 8);
        let r = f32x8::from(xa) + f32x8::from(xb);
        core::ptr::copy_nonoverlapping(r.as_array().as_ptr(), c.add(i), 8);
        i += 8;
    }
}

/// Reduction / accumulate-in-locals shape (like a NAM dot-product kernel): no adjacent stores.
#[no_mangle]
pub unsafe extern "C" fn accumulate(n: usize) -> f32 {
    use wide::f32x8;
    let a = core::ptr::addr_of_mut!(A) as *const f32;
    let b = core::ptr::addr_of_mut!(B) as *const f32;
    let mut acc = f32x8::splat(0.0);
    let mut i = 0usize;
    while i < n {
        let mut xa = [0f32; 8];
        let mut xb = [0f32; 8];
        core::ptr::copy_nonoverlapping(a.add(i), xa.as_mut_ptr(), 8);
        core::ptr::copy_nonoverlapping(b.add(i), xb.as_mut_ptr(), 8);
        acc += f32x8::from(xa) * f32x8::from(xb);
        i += 8;
    }
    acc.reduce_add()
}

#[no_mangle]
pub unsafe extern "C" fn seed(v: f32) {
    let mut i = 0;
    while i < N { A[i] = v + i as f32; B[i] = v - i as f32; C[i] = 0.0; i += 1; }
}

#[no_mangle]
pub unsafe extern "C" fn checksum() -> f32 {
    let mut s = 0.0f32;
    let mut i = 0;
    while i < N { s += C[i]; i += 1; }
    s
}

/// Straight-line, no loop: the absolute simplest seed pair.
#[no_mangle]
pub unsafe extern "C" fn straight(off: usize) {
    let a = core::ptr::addr_of_mut!(A) as *mut u8;
    let c = core::ptr::addr_of_mut!(C) as *mut u8;
    let a0 = v128_load(a.add(off) as *const v128);
    let a1 = v128_load(a.add(off + 16) as *const v128);
    v128_store(c.add(off) as *mut v128, f32x4_mul(a0, a0));
    v128_store(c.add(off + 16) as *mut v128, f32x4_mul(a1, a1));
}

/// Same base local, distinct static offsets, unaligned intrinsic loads/stores.
#[no_mangle]
pub unsafe extern "C" fn base_off(p: *mut u8) {
    let a0 = v128_load(p.add(0) as *const v128);
    let a1 = v128_load(p.add(16) as *const v128);
    v128_store(p.add(64) as *mut v128, f32x4_mul(a0, a0));
    v128_store(p.add(80) as *mut v128, f32x4_mul(a1, a1));
}

/// Same, but naturally-aligned reads/writes (p2align=4).
#[no_mangle]
pub unsafe extern "C" fn base_off_aligned(p: *mut v128) {
    let a0 = core::ptr::read(p.add(0));
    let a1 = core::ptr::read(p.add(1));
    core::ptr::write(p.add(4), f32x4_mul(a0, a0));
    core::ptr::write(p.add(5), f32x4_mul(a1, a1));
}

/// Loop over a caller-supplied buffer, aligned, adjacent stores.
#[no_mangle]
pub unsafe extern "C" fn loop_buf(src: *const v128, dst: *mut v128, pairs: usize) {
    let mut i = 0usize;
    while i < pairs {
        let a0 = core::ptr::read(src.add(2 * i));
        let a1 = core::ptr::read(src.add(2 * i + 1));
        core::ptr::write(dst.add(2 * i), f32x4_add(a0, a0));
        core::ptr::write(dst.add(2 * i + 1), f32x4_add(a1, a1));
        i += 1;
    }
}

/// Distinct src/dst, both loads before both stores.
#[no_mangle]
pub unsafe extern "C" fn two_ptr(src: *const v128, dst: *mut v128) {
    let a0 = core::ptr::read(src.add(0));
    let a1 = core::ptr::read(src.add(1));
    let r0 = f32x4_add(a0, a0);
    let r1 = f32x4_mul(a1, a1);
    core::ptr::write(dst.add(0), r0);
    core::ptr::write(dst.add(1), r1);
}

/// Same-value duplicated: only one load, two adjacent stores (no intervening memory op).
#[no_mangle]
pub unsafe extern "C" fn one_load_two_store(src: *const v128, dst: *mut v128) {
    let a0 = core::ptr::read(src.add(0));
    let r = f32x4_add(a0, a0);
    core::ptr::write(dst.add(0), r);
    core::ptr::write(dst.add(1), r);
}

/// Loop, but indexed off one base local with constant offsets (no pointer bumping).
#[no_mangle]
pub unsafe extern "C" fn loop_indexed(src: *const f32, dst: *mut f32, pairs: usize) {
    let mut i = 0usize;
    while i < pairs {
        let s = src.add(i * 8) as *const v128;
        let d = dst.add(i * 8) as *mut v128;
        let a0 = core::ptr::read(s);
        let a1 = core::ptr::read(s.add(1));
        let r0 = f32x4_add(a0, a0);
        let r1 = f32x4_add(a1, a1);
        core::ptr::write(d, r0);
        core::ptr::write(d.add(1), r1);
        i += 1;
    }
}

/// Verbatim copy of namir's axpy kernel (namir-ir/src/convolver.rs, namir-nam/src/wavenet.rs).
#[no_mangle]
pub extern "C" fn axpy_real(out: &mut [f32], in_: &[f32], w: f32) {
    use wide::f32x8;
    let n = out.len();
    let lanes = n - n % 8;
    let (out_vec_part, out_rem) = out.split_at_mut(lanes);
    let (in_vec_part, in_rem) = in_.split_at(lanes);
    let w_vec = f32x8::splat(w);
    for (o, i) in out_vec_part
        .as_chunks_mut::<8>()
        .0
        .iter_mut()
        .zip(in_vec_part.as_chunks::<8>().0)
    {
        let sum = f32x8::from(*o) + w_vec * f32x8::from(*i);
        o.copy_from_slice(&sum.to_array());
    }
    for (o, &i) in out_rem.iter_mut().zip(in_rem.iter()) {
        *o += w * i;
    }
}

#[no_mangle]
pub unsafe extern "C" fn axpy_entry(outp: *mut f32, inp: *const f32, n: usize, w: f32) {
    axpy_real(
        core::slice::from_raw_parts_mut(outp, n),
        core::slice::from_raw_parts(inp, n),
        w,
    );
}

/// Out-of-place variant of the same kernel: dst = acc + w*in, three distinct slices.
#[no_mangle]
pub unsafe extern "C" fn axpy_out(dst: *mut f32, acc: *const f32, inp: *const f32, n: usize, w: f32) {
    use wide::f32x8;
    let d = core::slice::from_raw_parts_mut(dst, n);
    let a = core::slice::from_raw_parts(acc, n);
    let i2 = core::slice::from_raw_parts(inp, n);
    let w_vec = f32x8::splat(w);
    for ((o, x), y) in d
        .as_chunks_mut::<8>().0.iter_mut()
        .zip(a.as_chunks::<8>().0)
        .zip(i2.as_chunks::<8>().0)
    {
        let sum = f32x8::from(*x) + w_vec * f32x8::from(*y);
        o.copy_from_slice(&sum.to_array());
    }
}

/// loop_indexed, but with unaligned (p2align=0) loads/stores — isolates the alignment variable.
#[no_mangle]
pub unsafe extern "C" fn loop_unaligned(src: *const u8, dst: *mut u8, pairs: usize) {
    let mut i = 0usize;
    while i < pairs {
        let s = src.add(i * 32);
        let d = dst.add(i * 32);
        let a0 = v128_load(s as *const v128);
        let a1 = v128_load(s.add(16) as *const v128);
        v128_store(d as *mut v128, f32x4_add(a0, a0));
        v128_store(d.add(16) as *mut v128, f32x4_add(a1, a1));
        i += 1;
    }
}

/// Real axpy semantics (out += w*in, in place) but written with an invariant base + index,
/// which is the addressing idiom the packing toys produced.
#[no_mangle]
pub unsafe extern "C" fn axpy_idx(outp: *mut f32, inp: *const f32, n: usize, w: f32) {
    use wide::f32x8;
    let w_vec = f32x8::splat(w);
    let pairs = n / 8;
    let mut k = 0usize;
    while k < pairs {
        let i = k * 8;
        let mut o = [0f32; 8];
        let mut x = [0f32; 8];
        core::ptr::copy_nonoverlapping(outp.add(i), o.as_mut_ptr(), 8);
        core::ptr::copy_nonoverlapping(inp.add(i), x.as_mut_ptr(), 8);
        let sum = f32x8::from(o) + w_vec * f32x8::from(x);
        core::ptr::copy_nonoverlapping(sum.as_array().as_ptr(), outp.add(i), 8);
        k += 1;
    }
}

/// Same as loop_unaligned but unrolling inhibited, so there is no straight-line remainder block.
#[no_mangle]
pub unsafe extern "C" fn loop_norolled(src: *const u8, dst: *mut u8, pairs: usize) {
    let mut i = 0usize;
    while i < pairs {
        let s = src.add(i * 32);
        let d = dst.add(i * 32);
        let a0 = v128_load(s as *const v128);
        let a1 = v128_load(s.add(16) as *const v128);
        v128_store(d as *mut v128, f32x4_add(a0, a0));
        v128_store(d.add(16) as *mut v128, f32x4_add(a1, a1));
        i = core::hint::black_box(i + 1);
    }
}
