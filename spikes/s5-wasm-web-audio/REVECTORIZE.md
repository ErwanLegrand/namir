# S-5 sub-investigation — V8's wasm revectorizer vs. Namir's DSP: why nothing packs

**Closed question, not a backlog item.** This is S-5's answer to "would 256-bit SIMD help in a
browser". It would not, we cannot reach it from Rust, and no later phase should re-open it
without a reason this file does not already cover.

Task 4 (`RESULTS.md`) established the *fact*: under `--experimental-wasm-revectorize` the pass
visits 16 wasm functions in the simd128 artefact and succeeds on none, with no measurable timing
change. This file establishes the *cause*, which Task 4 did not.

Investigated 2026-09-05, read-only on the repo. The experiments ran in a throwaway probe crate
outside the tree (`wide = "=1.6.1"` — 1.6.0 is yanked on crates.io; 1.6.1 has a byte-identical
`f32x8` definition) which is **not preserved**: everything load-bearing is transcribed below, and
the probe shapes are described precisely enough to rebuild in an hour if anyone ever needs to.
Node v24.19.0 / V8 13.6, flags `--experimental-wasm-revectorize --trace-wasm-revectorize
--no-liftoff` (`--no-liftoff` is needed or most functions never reach the TurboFan/Turboshaft
pipeline the pass lives in).

## The question

Does V8's 128->256-bit revectorizer pack nothing on our module because of what rustc/`wide`/
`rustfft` emit (upstream limitation), or because of the shape of the code we hand them
(something we could change)?

## H1 — "`wide::f32x8` has no simd128 backend and degrades to scalar": FALSE

Source evidence, `wide-1.6.1/src/f32x8_.rs:13-23`:

```rust
if #[cfg(target_feature="avx")] { pub struct f32x8 { avx: m256 } }
else                            { pub struct f32x8 { a: f32x4, b: f32x4 } }
```

and `f32x4_.rs:14-24` has an explicit `else if #[cfg(target_feature="simd128")]` arm holding a
`core::arch::wasm32::v128`. So on `wasm32-unknown-unknown +simd128`, `f32x8` is exactly two
`v128`s and every op is two `f32x4.*` instructions. Confirmed in emitted code: compiling
`namir-ir`'s `axpy` verbatim (`cargo rustc -- --emit asm`, LLVM's wasm text output) gives a loop
of paired `v128.load` / `f32x4.mul` / `f32x4.add` / `v128.store`. **The revectorizer is being fed
exactly the adjacent-128-bit-pair idiom it wants.** H1 is dead.

## H2 — "the pass is store-seeded and our kernels don't give it seeds": TRUE, and it decides this

The trace vocabulary maps directly onto V8's algorithm: collect *store seeds* (two `Simd128`
stores at adjacent addresses), then build a pack tree upward from them.

Measured, on toy functions built for the purpose:

| probe | shape | result |
|---|---|---|
| `straight` / `canonical` | v128 pairs against three module-level `static mut` arrays | `Empty seed` |
| `base_off`, `base_off_aligned` | one base local, `offset=64`/`offset=80` immediates, straight line | seeds found -> `IsSideEffectFree: break side effect` -> `Build tree failed!` |
| `one_load_two_store` | two adjacent stores, no intervening memory op | tree built; `Save: 1, cost: 1` -> not worth it |
| `loop_indexed`, `loop_unaligned` | loop, invariant base + induction index | **`Decide to vectorize, 6 revectorizable nodes`** |
| `loop_norolled` | same, unrolling inhibited with `black_box` | **`Decide to vectorize, 6 revectorizable nodes`** in the hot loop |
| `axpy_real`/`axpy_entry` (verbatim from `crates/namir-ir/src/convolver.rs:157`) | | `Empty seed` |
| `axpy_out` (out-of-place, 3 distinct slices) | | `Empty seed` |
| `axpy_idx` (real semantics, hand-written invariant-base + index) | | `Empty seed` |

**The toy packs.** So the pass works in this build and our code is the variable.

Two gates, both visible in the trace:

1. **Seeding needs the two stores to share one address local and differ only by the static
   `offset=` immediate.** `loop_norolled`'s hot loop emits `v128.store 16` and `v128.store 0` off
   `local 5`; V8 prints them as `*(#24 + #47)` and `*(#24 + 16 + #43)` and seeds. Every `axpy`
   variant instead gets LLVM's strength-reduced pointer induction — `local.get 0; i32.const 16;
   i32.add; ... v128.store 0` — so the second store's *base node* is a different SSA value, V8 sees
   no adjacency, and reports `Empty seed`. Alignment is **not** the variable: `loop_unaligned`
   (`p2align=0`) and `loop_indexed` (`p2align=4`) both pack.
2. **Even with seeds, no memory op may sit between the two stores in the effect chain.**
   `base_off` seeds and then fails `IsSideEffectFree ... break side effect`, because LLVM scheduled
   `store, load, store`. Namir's in-place `out[i] += w*in[i]` naturally produces exactly that
   interleave.

Caveat worth recording: in the *unrolled* toys (`loop_indexed`, `loop_unaligned`) the single seed
pair V8 found is in the **scalar-remainder block**, not the hot loop — the `v128.store 16` /
`v128.store 0` pair with folded immediates only survives there. Only `loop_norolled`, with
unrolling suppressed, packed inside the loop. So even the successes are narrower than the
headline "the toy packs" suggests.

## H3 — "rustc/LLVM's wasm backend won't emit the idiom": TRUE in a narrow, load-bearing sense

Not an absolute limitation — LLVM *does* emit the seedable form (`loop_norolled` proves it). But
which form you get is decided by LLVM's loop-strength-reduction and address folding, and every
natural way of writing the kernel in Rust (iterator + `as_chunks`, raw pointer + index, in-place
or out-of-place, 2 or 3 slices) landed on the unseedable pointer-bumping form. Four rewrites, all
failing the same way. The only way the seedable form was reached at all was
`core::hint::black_box` on the induction variable to defeat unrolling — which is not a change
anyone should make to a hot DSP kernel.

So the real chain is: H2 is the mechanism, H3 is why we're stuck on the wrong side of it, and we
have no reliable source-level lever over it.

## H4 — the premise is worth little anyway: also TRUE

`--experimental-wasm-revectorize` is off by default, is not exposed to web content, exists only
in V8 (no Firefox/Safari equivalent), and pattern-matches a narrow idiom. Even a full success
buys ~2x on the SIMD ops of one kernel, on one engine, behind a flag nobody can turn on in a
browser. Gate 1 already passed at 3.3x over scalar with plain simd128.

## Verdict

- Not an upstream `wide` problem, not a `rustfft` problem, not "V8 can't do it".
- The blocker is the address-arithmetic form LLVM chooses for our loops, plus store/load
  interleaving in the read-modify-write kernels. Both are LLVM scheduling decisions we can only
  nudge, not specify, from Rust.
- Making a real kernel packable would mean hand-writing it in `core::arch::wasm32` intrinsics with
  an addressing pattern chosen to survive LLVM (single base local, static offsets, both loads
  before both stores), then re-checking the trace after every compiler bump — a fragile,
  unverifiable-by-CI dependency on an off-by-default experimental flag.
- **Recommendation: don't.** 256-bit on wasm is a V8 lowering detail we cannot reach from Rust
  today. The wasm target's performance story is `+simd128`, which already works and is what
  Gate 1 passed on.
