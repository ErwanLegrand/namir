# S-5 measurement log

## Task 1 — wasm32-unknown-unknown portability probe, 2026-09-05

Command:

    rustup target add wasm32-unknown-unknown
    cargo build --target wasm32-unknown-unknown --release \
      -p namir-core -p namir-params -p namir-dsp -p namir-nam -p namir-ir -p namir-engine

Toolchain: `rustc 1.98.0 (88d9e12ae 2026-08-18)`

Result: **PASS**

Build finished in 11.50s (`Finished `release` profile [optimized] target(s) in 11.50s`),
zero warnings, zero errors. All six requested crates (`namir-core`, `namir-params`,
`namir-dsp`, `namir-nam`, `namir-ir`, `namir-engine`) and their transitive dependency
graph compiled for `wasm32-unknown-unknown` with no edits to anything under `crates/`.

The known hazard named in the task brief —
`crates/namir-engine/src/telemetry_ring.rs:38`'s
`const _: () = assert!(cfg!(target_has_atomic = "64"), …)` — did not fire.
`target_has_atomic = "64"` holds on `wasm32-unknown-unknown` as predicted (LLVM
legalises 64-bit atomics to plain loads/stores on this single-threaded target).

Full transitive dependency set compiled (39 crates total, alphabetical by
first appearance): `autocfg`, `find-msvc-tools`, `shlex`, `proc-macro2`, `cfg-if`,
`constant_time_eq`, `arrayref`, `unicode-ident`, `arrayvec`, `quote`, `serde_core`,
`strength_reduce`, `zmij`, `bytemuck`, `serde_json`, `serde`, `itoa`, `memchr`,
`hound`, `rtrb`, `wide`, `cc`, `num-traits`, `blake3`, `syn`, `num-integer`,
`num-complex`, `transpose`, `primal-check`, `namir-core`, `rustfft`, `namir-dsp`,
`namir-params`, `realfft`, `rubato`, `serde_derive`, `namir-ir`, `namir-nam`,
`namir-engine`.

Nothing surprising beyond one minor note: `cc` and `find-msvc-tools` appear in
the graph (almost certainly pulled in by `blake3`'s optional native-intrinsics
build script for content hashing in `namir-core`). These only run as host-side
build-script tooling to decide whether to use a C backend; they did not block
or alter the wasm target output, and no MSVC toolchain issue surfaced. No other
platform-specific transitive dependency (no `cpal`, no `clack`, nothing naming
CLAP or a native audio backend) leaked into this crate subset — consistent with
D-5.1's layering table, since `namir-platform`/`namir-worker`/`namir-app`/
`namir-clap` were correctly excluded from the build.

**Verdict: kill criterion 1 does NOT trigger.** The spike may continue to Task 2.

## Task 2 — spike crate, shared harness, native reference figures, 2026-09-05

Files added: `Cargo.toml`, `src/harness.rs`, `src/lib.rs`, `src/bin/native_bench.rs`,
`src/wasm_abi.rs` (see below), `.gitignore`. Transcribed from
`.superpowers/sdd/plan-s5-wasm-web-audio/task-2-brief.md` with two type-level
corrections the brief's code did not compile as written (`namir_core::SampleRate::new`
returns `Option`, not `Result`; `RingConsumer::try_pop` returns `Option`, not a
`Result` with a `.pop()` method) — both fixed in `harness.rs` without touching
anything under `crates/`. `src/wasm_abi.rs` is a placeholder empty module: `lib.rs`
(per the brief) declares `#[cfg(target_arch = "wasm32")] mod wasm_abi;`, and Task 3
owns its contents, but Task 2 itself must produce a passing `wasm32-unknown-unknown`
lib build, which a missing module file would fail for a reason entirely internal to
this crate.

**Command:**

    cd spikes/s5-wasm-web-audio
    cargo build --release
    cargo run --release --bin native_bench

**Toolchain:** `rustc 1.98.0 (88d9e12ae 2026-08-18)` (same as Task 1).

**Machine:** AMD Ryzen 9 5950X, 32 logical CPUs — `docs/02-architecture.md` §2's
reference machine. `NAMIR_PIN_CORE` was left at its default (core 4).

**Was anything else running?** Yes — this session's own agent/tooling process was
active (issuing shell commands) during part of the run, specifically overlapping the
`a1_standard decaying` configuration; see the CONTAMINATED discussion below. Per
AGENTS.md's own benchmark-methodology section, this is a known, previously-documented
contamination source on this machine, not a first observation.

**Both builds:**
- Native `cargo build --release`: clean, no warnings.
- `cargo build --target wasm32-unknown-unknown --release --lib` (from inside the spike
  directory): clean, no warnings. `namir-fixtures` and `core_affinity` do not enter the
  wasm dependency graph (both are `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`
  in `Cargo.toml`), so no `getrandom`/host-only crate leaks into the wasm build.

**No figure below is a certified NFR-PERF-010 figure.** This spike does not link
`namir-platform`, so none of D-2.1/D-2.2/D-2.4's certified-benchmark apparatus applies
here beyond the borrowed methodology (single-core pin, warmup/measured counts,
percentile + estimator reduction) — every number below is informational only, for the
wasm-vs-native ratio Task 3 will compute, never a Namir performance claim on its own.

### Full results (20 lines: 2 models x 2 signals x 5 reps)

```
native a1_standard steady   rep 1/5: p50 6.39% | p99 10.51% | p99.9 13.93% | max 23.54% | estimator  9.95% | quotable
native a1_standard steady   rep 2/5: p50 6.30% | p99 10.38% | p99.9 13.60% | max 20.73% | estimator  9.92% | quotable
native a1_standard steady   rep 3/5: p50 6.65% | p99 10.91% | p99.9 14.32% | max 20.01% | estimator 10.25% | quotable
native a1_standard steady   rep 4/5: p50 6.59% | p99 11.24% | p99.9 14.69% | max 24.06% | estimator 10.16% | quotable
native a1_standard steady   rep 5/5: p50 6.44% | p99 10.92% | p99.9 14.07% | max 30.73% | estimator 10.37% | quotable
native a1_standard decaying rep 1/5: p50 9.00% | p99 15.41% | p99.9 18.53% | max 47.97% | estimator  9.92% | CONTAMINATED*
native a1_standard decaying rep 2/5: p50 9.08% | p99 14.34% | p99.9 16.59% | max 22.60% | estimator 10.17% | CONTAMINATED*
native a1_standard decaying rep 3/5: p50 9.17% | p99 14.46% | p99.9 16.77% | max 20.45% | estimator 10.23% | CONTAMINATED*
native a1_standard decaying rep 4/5: p50 9.08% | p99 14.37% | p99.9 16.66% | max 29.03% | estimator  9.96% | CONTAMINATED*
native a1_standard decaying rep 5/5: p50 9.17% | p99 14.36% | p99.9 16.32% | max 18.93% | estimator 10.20% | CONTAMINATED*
native a2_lite     steady   rep 1/5: p50 2.05% | p99  6.11% | p99.9  8.21% | max 12.84% | estimator  5.64% | quotable
native a2_lite     steady   rep 2/5: p50 1.93% | p99  6.05% | p99.9  8.03% | max 12.29% | estimator  5.61% | quotable
native a2_lite     steady   rep 3/5: p50 1.83% | p99  5.96% | p99.9  7.94% | max 10.20% | estimator  5.64% | quotable
native a2_lite     steady   rep 4/5: p50 2.06% | p99  6.16% | p99.9  8.13% | max 12.74% | estimator  5.59% | quotable
native a2_lite     steady   rep 5/5: p50 1.92% | p99  6.23% | p99.9  8.41% | max  9.68% | estimator  5.38% | quotable
native a2_lite     decaying rep 1/5: p50 2.18% | p99  6.38% | p99.9  8.35% | max 11.98% | estimator  5.60% | quotable
native a2_lite     decaying rep 2/5: p50 2.05% | p99  6.02% | p99.9  8.10% | max 10.22% | estimator  5.40% | quotable
native a2_lite     decaying rep 3/5: p50 2.13% | p99  6.09% | p99.9  8.07% | max 15.32% | estimator  5.60% | quotable
native a2_lite     decaying rep 4/5: p50 1.97% | p99  5.95% | p99.9  8.04% | max 21.27% | estimator  5.35% | quotable
native a2_lite     decaying rep 5/5: p50 2.16% | p99  6.38% | p99.9  8.28% | max 10.76% | estimator  5.71% | quotable
```

**\* `a1_standard decaying`'s five CONTAMINATED flags are not machine noise — read them
as signal-dependent cost, not as discardable.** The asymmetry rules out contamination
as the cause: all five `a2_lite decaying` reps, measured on the same machine in the
same run, are quotable; only `a1_standard`'s decaying reps flag. Across the five
`a1_standard` reps, `p50` rises from steady's ~6.4-6.6% to decaying's ~9.0-9.2% while
the per-residue estimator stays flat at ~9.9-10.4% in both signals — i.e. the *typical*
per-block cost gets measurably more expensive as the driving signal's amplitude decays.
**Correction (fix round 1):** an earlier revision of this note, and of
`harness.rs`'s `Signal::Decaying` doc comment, claimed the signal reaches ~1e-30 and
drives the chain into subnormals. Neither half holds: `0.999_5^100_000` (the actual
per-block decay applied over `MEASURED_BLOCKS`) is ~2e-22, not ~1e-30, and 1e-30 is
itself still well above f32's own subnormal threshold (`f32::MIN_POSITIVE` ~1.18e-38).
The driving *signal* therefore never leaves f32's normal range at all. The cost rise
is real and still worth explaining, but it is attributable to *internal* chain state
(e.g. the IR convolver's or EQ's own running state decaying into subnormal magnitudes
faster than the input itself, since a convolution accumulator's magnitude can fall
below its input's), not to the harness's input signal going subnormal. The estimator
(a periodic worst-case-block figure, insensitive to a slow amplitude ramp) does not
move as this happens. `is_quotable()`'s `p999 - estimator <= 5.0` rule was built to
catch *background* interference inflating the tail against an unmoving baseline
(D-2.4); here the baseline itself doesn't reflect the real ongoing cost increase, so
the same test fires for a different reason than it was designed to catch. These five
reps are reported as-is: **not** discarded as contaminated, and **not** quoted as if
`is_quotable()` had passed — the number is real, and the reason the flag fired is
stated here rather than left implicit.

**No `DenormalGuard` is active on this native run.** The spike depends on
`namir-core`/`namir-params`/`namir-dsp`/`namir-nam`/`namir-ir`/`namir-engine` and
deliberately excludes `namir-platform` (the only crate that installs FTZ/DAZ), so
nothing here enables the denormal guard shipped Namir installs at startup. That is the
right shape for this experiment — it isolates the wasm-vs-native comparison Task 3
needs rather than conflating it with a guard-vs-no-guard comparison — but it means the
`decaying` figures above (native's likely-subnormal-heavy branch cost) are **not**
representative of what shipped native Namir measures with its guard installed; expect
shipped Namir's decaying-signal cost to be flatter than this spike's.

### `native_bench_output.txt`

Committed deliberately as raw evidence (not gitignored), rather than only transcribing
the figures into this file by hand — it's the unedited console capture the numbers
above were copied from, and this project's own convention (see
`crates/namir-engine/benches/six_stage_chain.rs`'s M9b log, which keeps a disqualified
measurement set on the record rather than deleting it) favours keeping the primary
artifact rather than only its transcription.

### Fixtures written (gitignored, not committed)

`fixtures/a1_standard.nam`, `fixtures/a2_lite.nam`, `fixtures/ir_48k.wav`,
`fixtures/ir_44k1.wav`, `fixtures/reference_render_f32le.bin` — the last is the
deterministic reference render Task 3's wasm-vs-native output-parity check will
compare against.

**Verdict: proceed to Task 3.**

### Not directly comparable to `six_stage_chain.rs`'s own figures

This harness's methodology (single-core pin, warmup/measured counts, percentile +
per-residue estimator) is borrowed from
`crates/namir-engine/benches/six_stage_chain.rs`, but **the two time different spans**
and their absolute percentages must not be read side by side as a same-thing
comparison. `six_stage_chain.rs` times `chain.process(&mut io)` alone. This harness's
`Harness::run` times `Harness::process_block`, which additionally covers: copying
`input` into two owned channel buffers, `StageIo::new`'s own setup, `AudioEngine::
process`'s command-ring drain, retire-collection and telemetry publish that wrap
`chain.process` (plus, as of fix round 1 below, one `TelemetryReader::drain` call per
`run` for the resource-loaded check — off the hot per-block path, so it does not add
per-block cost, but worth naming), and this harness's own retire-ring drain loop after
`engine.process` returns. So this spike's ~10% estimator/~14% p99.9 for `a1_standard`
is not evidence that the real six-stage chain got faster or slower than
`six_stage_chain.rs`'s own ~14.56%/~16.39% (see that file's own module doc comment) —
different things are being timed. The wasm-vs-native **ratio** this spike exists to
produce is unaffected by this, because both sides of that ratio go through the
identical `Harness::process_block` span; only an absolute cross-reference to
`six_stage_chain.rs`'s own certified figures would be affected, and none is intended.

## Fix round 1 (review findings), 2026-09-05

Two Important findings from code review, both fixed in `harness.rs` without touching
`crates/`:

**1. No fault-count assertion.** `harness.rs`'s `run` never checked
`chain.fault_count()`, unlike `six_stage_chain.rs:594-598`'s `assert_eq!(chain.
fault_count(), 0, ...)` — meaning a run that had silently hit FR-CHAIN-080's NaN/Inf
fault path could have been quoted as a clean timing figure. Added the identical
assertion at the end of `run`, after the measured loop, before `reduce()`:

    assert_eq!(
        self.engine.chain().fault_count(),
        0,
        "the measured run must not have hit FR-CHAIN-080's NaN/Inf fault path"
    );

`AudioEngine::chain()` (already public) exposes the read-only accessor this needs.

**2. Handover-completion check.** `try_push`ing the `Load` command only proves it
entered the ring, not that a stage took the offer — a silently-empty NAM stage would
have produced a plausible-looking but wrong (too-fast) figure, with the a1-vs-a2 p50
gap as only after-the-fact evidence, not a guard. **A public way to confirm this does
exist, with no `crates/` change needed:** `stages/nam.rs`/`stages/ir.rs` each publish a
`telemetry.{nam,ir}.loaded` reading (`1.0` only once `self.slots[self.active].
is_some()`), reachable through `WorkerEndpoint::telemetry` (`TelemetryReader::drain`,
already public). Added `Harness::assert_resources_loaded`, called once per `run` right
after the warmup loop (thousands of blocks — far more than one `HANDOVER_CROSSFADE_MS`
crossfade needs) and before the measured loop starts: it drains the telemetry ring and
asserts both `telemetry.nam.loaded` and `telemetry.ir.loaded` read `1.0`.

**Re-run to confirm both, on the same machine/toolchain as the original run:**

    cd spikes/s5-wasm-web-audio
    cargo build --release
    cargo run --release --bin native_bench

Exit code **0** across all 20 configurations — meaning **both new assertions held for
every one of them**, including all five `a1_standard decaying` reps (the ones already
flagged CONTAMINATED above). That is a real, checked result, not merely an absence of
prior checking:

- **Fault count: 0 for every configuration**, `a1_standard decaying` included. The
  anomalous A1-decaying figures are confirmed to be genuine timing, not a run that
  silently took the NaN/Inf path. Because `Harness::new`'s xorshift32 RNG is
  re-seeded identically (`0x2545_F491`) for every `Harness` and the block sequence is
  otherwise deterministic (same seed, same fixture bytes, same block counts), this
  fault-checked re-run exercises the same computed sample sequence as the run that
  produced the figures already recorded above — only wall-clock timings differ
  between the two runs — so this result also corroborates that the originally-recorded
  figures were fault-free, not merely that a later run happened to be.
- **Resource-loaded check: passed for every configuration** — both `telemetry.nam.
  loaded` and `telemetry.ir.loaded` read `1.0` before every measured run began, for
  both models and both signals.

Re-run output (`native_bench_output_fault_check_rerun.txt`, committed as further raw
evidence): figures are consistent with the original run within ordinary rep-to-rep
variance (e.g. `a1_standard steady` estimator 10.00-10.26% here vs. 9.92-10.37%
originally; `a1_standard decaying` still 5/5 CONTAMINATED with estimator 9.92-10.18%).
The original run's committed figures above are left as the recorded figures; this
re-run's role is the fault/handover confirmation, not a replacement measurement.

Both builds (`cargo build --release` and `cargo build --target wasm32-unknown-unknown
--release --lib`) were re-verified clean after these changes.

## Task 3 — wasm ABI, JS glue, output-parity check, 2026-09-05

**Files added:** `src/wasm_abi.rs` (real contents, replacing Task 2's placeholder),
`.cargo/config.toml`, `web/serve.py`, `web/namir.js`, `web/bench-worker.js`,
`web/bench.html`, `web/parity-node.mjs`. **Files modified:** `src/harness.rs`,
`RESULTS.md`. Nothing under `crates/`, `docs/`, `.github/` or `xtask/` was touched —
kill criterion 1 did not trigger.

**No figure in this section is a certified figure.** As in Task 2: this spike does not
link `namir-platform`, none of D-2.1/D-2.2/D-2.4's certified-benchmark apparatus
applies, and every timing number below is informational only.

### Two corrections to the Task 3 brief

**1. `--import-undefined` is required; the brief predicted otherwise.** The brief's
Step 2 says an unresolved `now_us` import at link time "is expected and correct — it is
satisfied at instantiation". It is not: `rust-lld` fails the link outright.

    rust-lld: error: ...s5_wasm_web_audio...rcgu.o: undefined symbol: now_us

Fixed with `.cargo/config.toml`, scoped to `wasm32-unknown-unknown` only so the native
bench and the workspace build are untouched:

    [target.wasm32-unknown-unknown]
    rustflags = ["-C", "link-arg=--import-undefined"]

**2. The build command must be `--lib`.** `cargo build --release --target
wasm32-unknown-unknown` (the brief's Step 2, no `--lib`) also tries to build
`src/bin/native_bench.rs`, which uses `namir-fixtures` and `core_affinity` — both
`cfg(not(target_arch = "wasm32"))` dependencies — and fails with four `E0433`s. That
is the `Cargo.toml` shape working as designed (Task 2 kept the fixture generator out of
the wasm dependency graph), not a defect; only the command in the brief is wrong.

### Build

    cd spikes/s5-wasm-web-audio
    cargo build --release --lib --target wasm32-unknown-unknown

Output: `Finished` release profile, clean, no warnings. Artefact:
`target/wasm32-unknown-unknown/release/s5_wasm_web_audio.wasm`, **859 330 bytes**. Its
module interface, read back with `WebAssembly.Module.imports/exports`:

    imports [ { module: 'env', name: 'now_us', kind: 'function' } ]
    exports memory alloc bench init io_ptr load_ir load_nam process render stats_ptr

— exactly the ABI the brief specifies: one import, nine exports plus `memory`.
`namir-fixtures` does not enter the wasm dependency graph (re-verified: the `--lib`
build is clean and the only import is `env.now_us`).

### Two `harness.rs` changes, both arithmetically inert (proved, below)

**1. The deferred short-final-chunk bug.** `process_block` built `StageIo` with a hard
`BLOCK_SIZE` while copying only `input.len()` samples, so a short final chunk processed
the *previous* block's samples in its tail. Fixed by zero-padding rather than by
shortening `StageIo`'s `frames`: `StageIo::new` would accept a smaller `frames`, but
the IR convolver's partition schedule is built for a fixed `BLOCK_SIZE`, so
pad-with-silence is the honest short-block semantics for this chain. Web Audio's render
quantum is exactly 128 = `BLOCK_SIZE`, so this only ever bites `render` on a
non-multiple length; `PARITY_SAMPLES` is a multiple, so no parity figure here depends
on it.

**2. `render` bypassed `run`'s two guards.** `Harness::render` is the parity path and
does not go through `Harness::run`, so it carried neither `assert_resources_loaded` nor
the `fault_count() == 0` check. A parity render from a chain whose NAM/IR never landed
would have been compared against a *native* reference produced the same broken way —
and would have "passed". Both guards added at the end of `render`, which fixes the
native and wasm sides at once rather than only the new wasm path. Also added
`Harness::output_left`, so the `process` export can be genuinely in-place (below).

Both changes are arithmetically inert, and that was *checked*, not assumed: reverting
them and rebuilding `native_bench` reproduces Task 2's
`fixtures/reference_render_f32le.bin` **byte-for-byte** (md5
`6b147f09eb4d929400f4d11b532ed850`), and restoring them reproduces it byte-for-byte
again. The reference render used for every parity figure below is that same md5.

### The `process` export is in-place — the brief's version was not

The brief's `process()` reads `io_ptr`'s buffer into the chain and never writes the
result back. Task 6's AudioWorklet would then emit exactly what it was handed, i.e. a
plausible-looking but fully bypassed plugin — the same class of silent failure the
parity check exists to catch. `process()` now copies `Harness::output_left` back over
the `io_ptr` buffer, and `web/parity-node.mjs` smoke-tests it:

    process() in-place: 128/128 samples written back, out energy 1.10e+2 -- OK

### The native-vs-native reproducibility floor: **−82.72 dB**

Before the parity verdict, the number it is measured against — because nothing in this
project had measured it before, and it is arguably the most reusable thing this spike
has produced.

**Take the same source, the same compiler, the same machine, and change only host
codegen. The six-stage chain's rendered output differs by −82.72 dB.** That is this
chain's f32 reproducibility limit, and it means no bit-exactness claim and no absolute
output-parity bar tighter than about −80 dB can hold for it across builds.

How it was obtained — reproducible, two commands:

    # default codegen
    cargo run --release --bin native_bench -- \
      --render-only fixtures/reference_render_f32le.bin

    # control: same source, AVX/AVX2/FMA off, isolated target dir so cargo cannot
    # reuse fingerprints from the build above
    CARGO_TARGET_DIR=../../target-s5-control \
      RUSTFLAGS="-C target-cpu=x86-64 -C target-feature=-avx,-avx2,-fma" \
      cargo run --release --bin native_bench -- \
        --render-only fixtures/reference_control_f32le.bin

`--render-only <path>` was added to `native_bench` for exactly this: it writes the
deterministic reference render and stops, instead of also sitting through the
20-configuration bench.

Three things make this a real measurement rather than an artefact:

- The `.nam` and `.wav` fixtures the two builds generate are **byte-identical** (md5
  compared), so the inputs are the same and only the rendering differs.
- The isolated `CARGO_TARGET_DIR` is load-bearing. A first attempt in the shared target
  directory produced a stale-fingerprint mix — the "default" build linked
  control-codegen rlibs — which briefly made this look like a source-level difference.
  It is not.
- `--render-only` reproduces the default reference **byte-for-byte** (md5
  `6b147f09eb4d929400f4d11b532ed850`) against the full `native_bench` run, so the flag
  changes nothing about what is rendered.

Why a nonlinear amp model amplifies rounding this far is not investigated here; the
per-block diagnostic below shows the divergence is bounded and non-accumulating, so the
mechanism is reassociation in a chaotic system, not drift.

### Parity: **PASS** — residual −81.48 dB against a −82.72 dB control, margin 1.24 dB

**The criterion changed in fix round 1, on a coordinator ruling.** The Task 3 brief's
bar was an absolute **−100 dB**, inherited from the spec. The floor above shows that bar
is unreachable by *any* build of this chain, native included, so it tested nothing
achievable — it would have failed a correct port and could not have been passed by a
correct one. The criterion is now **relative**: the wasm-vs-native residual must sit
within **3 dB** of the native-vs-native control, and the control is computed from the
two native renders **in the same run** rather than hardcoded (`parity()` in
`web/namir.js`; `CONTROL_MARGIN_DB = 3.0`). The old absolute bar is gone from the code,
and so is the manual opt-in that used to be needed to benchmark past it.

Measured two ways, agreeing to the last printed digit:

| Runtime | `crossOriginIsolated` | residual | control | margin | verdict |
|---|---|---|---|---|---|
| Node v24.19.0 (V8/TurboFan), `web/parity-node.mjs` | n/a | **−81.48 dB** | −82.72 dB | **+1.24 dB** | **PASS** |
| Microsoft Edge 152.0.4191.62 headless (Chromium/V8), full module-worker + `fetch` + COOP/COEP path | `true` | **−81.48 dB** | −82.72 dB | **+1.24 dB** | **PASS** |

Supporting comparisons:

| Comparison | dB |
|---|---|
| **silence** vs the native reference | **0.00** |
| wasm32 vs native (default codegen) — the residual | −81.48 |
| wasm32 vs native (control codegen) | −83.33 |

**If this ruling is wrong, the spike is accepting a real numerical defect as codegen
noise.** The diagnostics that argue against that, stated so a later reader can weigh
them rather than take the verdict on trust:

- **Silence scores 0.00 dB.** A chain rendering nothing is 81 dB away from passing, and
  `parity-node.mjs` asserts this in-process on every run — the metric is not one that
  passes trivially.
- **No delay and no gain error.** Best-fit gain got/want = `0.999999726`. Shifting the
  wasm output by ±1 sample collapses the figure to −35 dB and by ±2 to −29 dB, so the
  two renders are sample-aligned.
- **Bounded, non-accumulating error.** Max absolute error stays ~1e-4 across the whole
  render (block 4: 7.4e-5, block 40: 2.2e-4, block 255: 0) against a reference RMS of
  ~0.9-1.0. It does not grow with block index, which a genuine defect in a stateful
  chain would.
- **Several blocks are bit-exact** (max absolute error exactly `0.0`) — every one of
  them a block whose reference RMS is exactly 1.0, i.e. a fully railed block where the
  nonlinearity saturates both implementations to the same value. A structurally
  different chain would not produce bit-exact blocks.
- **The wasm module is *closer* to the no-AVX native build (−83.33 dB) than to the AVX
  one (−81.48 dB)**, which is what a scalar target should look like if the difference is
  vectorised reassociation.

A residual more than 3 dB worse than the control still refuses to benchmark, with no
bypass.

### Timing figures

**Chrome and Firefox are not installed on this machine and nothing was installed to
change that.** Edge and Node are both V8/TurboFan, which is what this ABI and the parity
check actually exercise, so those were run instead and are labelled as such throughout.
Every figure here is informational only — this spike does not link `namir-platform`, so
none of these can ever become a certified figure — and every one is **CONTAMINATED** by
`Stats::is_quotable`'s own rule (`p999 - estimator <= 5.0`).

Runtimes: Microsoft Edge **152.0.4191.62** (`--headless=new --disable-gpu
--no-sandbox`), Node **v24.19.0**. Toolchain `rustc 1.98.0 (88d9e12ae 2026-08-18)`.
Machine: AMD Ryzen 9 5950X / Windows 11 Pro 26200. **Benchmarks in the browser are not
core-pinned** — `NAMIR_PIN_CORE` is a native-only affordance and there is no browser
equivalent, which is one more reason none of this is certifiable.

#### Edge headless — steady signal, 20 000 measured blocks, 5 reps each

Command (`web/serve.py` running from the spike root):

    msedge --headless=new --disable-gpu --no-sandbox \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=20000"
    # and again with &model=a2_lite

`crossOriginIsolated: true` in both runs. Parity, reported by the page before it
benchmarks: residual **−81.4759 dB**, control **−82.7158 dB**, margin **1.2399 dB**,
**PASS**. (The a2_lite run reports the same parity figures because the parity render
always uses `a1_standard` — see the note under the a2 table.)

**A1 Standard — does not fit.**

```
edge a1_standard steady rep 1/5: p50 39.19% | p99 67.88% | p99.9 86.44% | max 104.44% | estimator 50.62% | CONTAMINATED
edge a1_standard steady rep 2/5: p50 39.38% | p99 69.37% | p99.9 89.25% | max  93.19% | estimator 50.81% | CONTAMINATED
edge a1_standard steady rep 3/5: p50 39.19% | p99 65.44% | p99.9 84.38% | max  92.44% | estimator 50.81% | CONTAMINATED
edge a1_standard steady rep 4/5: p50 39.19% | p99 64.69% | p99.9 85.50% | max  97.13% | estimator 50.63% | CONTAMINATED
edge a1_standard steady rep 5/5: p50 39.19% | p99 65.63% | p99.9 86.44% | max  93.38% | estimator 50.81% | CONTAMINATED
```

**A2 Lite — fits, with room.**

```
edge a2_lite     steady rep 1/5: p50  7.12% | p99 29.81% | p99.9 34.50% | max  56.81% | estimator 18.94% | CONTAMINATED
edge a2_lite     steady rep 2/5: p50  6.94% | p99 20.25% | p99.9 29.25% | max  35.25% | estimator 18.94% | CONTAMINATED
edge a2_lite     steady rep 3/5: p50  6.94% | p99 20.06% | p99.9 28.87% | max  38.06% | estimator 18.94% | CONTAMINATED
edge a2_lite     steady rep 4/5: p50  6.94% | p99 20.06% | p99.9 29.63% | max  36.19% | estimator 18.94% | CONTAMINATED
edge a2_lite     steady rep 5/5: p50  6.94% | p99 20.63% | p99.9 31.13% | max  34.88% | estimator 18.94% | CONTAMINATED
```

**The parity render always uses `a1_standard`, whatever model is benchmarked**, because
the two native reference renders are made from `a1_standard`. This was found the direct
way: the first a2_lite attempt pointed the parity check at a2_lite and scored **+1.15
dB** — "completely different signal" — and the gate refused to benchmark. That is the
relative criterion working exactly as intended on a genuine mismatch, and it is the
strongest evidence in this document that the check is not a formality. Parity is a
fidelity check on the port; the benchmarked model is a separate axis.

**20 000 measured blocks, not the 100 000 the native run used.** A 100 000-block
`a1_standard` rep takes ~11 minutes under headless Edge on this machine (Edge is roughly
5× slower per rep than Node for the same work — a browser-scheduling artefact, not a
wasm one), so five of them was not a practical wait. 20 000 blocks still puts 20 samples
above p99.9 and does not affect the per-residue estimator at all. Two earlier five-rep
attempts at 100 000 blocks are **discarded and not quoted**: the first had a second
headless Edge instance still alive and beaconing into the same log, and both showed the
`p50` spread (39% → 75%) that concurrent load produces. The runs above were each made
with nothing else running; `p50` is stable to ±0.19 pp (A1) and ±0.19 pp (A2) across
five reps, and A2's estimator is identical to the last digit in all five.

#### Node v24.19.0 cross-check — `a1_standard`, steady, 100 000 measured blocks, 1 rep

    node web/parity-node.mjs --bench --reps 1

```
node a1_standard steady rep 1/1: p50 44.82% | p99 65.71% | p99.9 83.90% | max 99.73% | estimator 55.61% | CONTAMINATED
```

Same V8, no browser scheduler, `process.hrtime.bigint()` instead of
`performance.now()`, full 100 000 blocks — and it lands within a few points of the Edge
figures, which is the cross-check's whole job. Wall time 2 m 12 s for the rep, i.e.
~1.26 ms of wall per 128-frame block against a 1.21 ms measured p50: the harness's own
per-block overhead outside the timed span is small, so the measured span is not hiding
the cost. It is also why the 5 µs clock quantum is not a problem — see caveat 1.

#### The numbers this task exists to produce

**A1 Standard** — native (Task 2, 5 reps) vs Edge headless (5 reps):

| | native | Edge | ratio |
|---|---|---|---|
| p50 | 6.30-6.65% | 39.19-39.38% | **≈6.1×** |
| p99.9 | 13.60-14.69% | 84.38-89.25% | **≈6.1×** |
| estimator (contamination-immune) | 9.92-10.37% | 50.62-50.81% | **≈5.0×** |

**A2 Lite** — native (Task 2, 5 reps) vs Edge headless (5 reps):

| | native | Edge | ratio |
|---|---|---|---|
| p50 | 1.83-2.06% | 6.94-7.12% | **≈3.6×** |
| p99.9 | 7.94-8.41% | 28.87-34.50% | **≈3.8×** |
| estimator (contamination-immune) | 5.38-5.64% | 18.94% | **≈3.4×** |

**A1 Standard does not fit in the browser on this machine.** p99.9 at 84-89% of the
block period, and `max` crossed 100% (104.44%) in one rep of five. Kill criterion 2
(>100% of realtime) has **not** fired — that reads on the sustained figure, and the p50
and the estimator are both comfortably under — so the spike continues; but A1's tail has
no headroom whatsoever, and a single missed deadline is an audible glitch, not a
statistic. This is the headline result and is not to be softened.

**A2 Lite fits, with real room**: p99.9 at 29-35%, `max` at worst 57%, estimator 19%.
The wasm penalty is also smaller for A2 (≈3.4-3.8×) than for A1 (≈5.0-6.1×), so the two
architectures are not a fixed multiple apart — whatever A1's expense is in wasm, it is
not uniform across the chain.

Together these two are the number Tasks 4-6 have to work against: **the browser can run
this chain today at A2 Lite, and cannot reliably run it at A1 Standard.**

#### Three caveats that bound how far these figures can be pushed

1. **Timer resolution is fine, and per-block timing must be kept.** Every Edge
   percentage above is an exact multiple of 0.1875% of the block period — which is
   exactly **5 µs**, the `performance.now()` quantum a cross-origin-isolated page gets.
   So headless Edge coarsened its clock precisely as the shipping isolated browser
   does, and D-S5.5's stated 0.19% quantisation error is what was observed. Against a
   wasm block that costs ~1000 µs (below), a 5 µs quantum is ~0.5% of the measured
   quantity. **Task 4 must keep the per-block `now_us()` pair; do not batch N blocks and
   divide** — the per-block distribution (p99.9 and the per-residue estimator) is the
   whole measurement, and batching destroys exactly the information the gate reads. The
   case that would need care is a *non*-isolated page, where the quantum is 100 µs
   (3.75%); the bench page ships COOP/COEP for this reason and reports
   `crossOriginIsolated` so a run without it is visible.

   *(Correction, fix round 1: an earlier revision of this caveat read "5 ns", "~187% of
   the block period", and recommended batching. All three were the same arithmetic slip
   — the block period is 2666.67 µs, not 2666.67 ns — and the recommendation they led
   to was backwards. Recorded rather than silently rewritten.)*
2. **No `DenormalGuard` on either side**, same as Task 2 — and wasm32 has no FTZ/DAZ at
   all and cannot get one, so the decaying-signal condition should be expected to be
   *worse* in the browser than in shipped native Namir. That comparison is not made
   here; only the steady signal was measured in the browser.
3. **No core pinning in the browser**, and these are not certified figures.

#### PENDING RUN — Step 8's Chrome and Firefox figures

Neither browser is installed on this machine. Both remain to be run by a human on a
machine that has them; the page takes all its settings from the query string, so no code
change is needed:

    cd spikes/s5-wasm-web-audio
    cargo build --release --lib --target wasm32-unknown-unknown
    # both native renders -- see "The native-vs-native reproducibility floor" for the
    # second command's flags; the parity check needs both files
    cargo run --release --bin native_bench -- --render-only fixtures/reference_render_f32le.bin
    CARGO_TARGET_DIR=../../target-s5-control       RUSTFLAGS="-C target-cpu=x86-64 -C target-feature=-avx,-avx2,-fma"       cargo run --release --bin native_bench -- --render-only fixtures/reference_control_f32le.bin
    cargo run --release --bin native_bench    # fixtures/ + the native figures
    python web/serve.py
    # then, in each browser, open:
    #   http://127.0.0.1:8080/web/bench.html
    # confirm "crossOriginIsolated: true", pick the model, click Run. Parity must report
    # PASS before it will benchmark; there is no bypass.
    # Record the browser version and the crossOriginIsolated state with the figures.

Interactive Chrome and Firefox should be expected to land near the Edge headless
numbers above: the clock quantum is the same 5 µs under cross-origin isolation, so any
difference should be engine and scheduler, not timer resolution. Confirm
`crossOriginIsolated: true` on the page before recording anything — without it the
quantum is 100 µs (3.75% of the block period) and the figures are not comparable.

### Task 3 verdict

**Proceed to Task 4.** Three things carried forward:

1. **The port is correct.** Parity **PASSES** the relative criterion — residual −81.48
   dB against a −82.72 dB native-vs-native control, margin 1.24 dB, bar 3 dB — measured
   identically under Node and headless Edge. Silence scores 0.00 dB, the residual is
   sample-aligned at unit gain, bounded at ~1e-4, and bit-exact on railed blocks. The
   spec's absolute −100 dB bar was unreachable by any build of this chain and has been
   replaced (fix round 1, coordinator ruling).
2. **A1 Standard does not fit; A2 Lite does.** A1: p99.9 84-89% of the block period,
   `max` 104% in one rep of five, ≈5.0-6.1× native. A2: p99.9 29-35%, `max` ≤57%,
   ≈3.4-3.8× native. Kill criterion 2 has not fired, but A1 has no tail headroom.
3. **Keep per-block timing.** The 5 µs cross-origin-isolated clock quantum is ~0.5% of a
   ~1000 µs wasm block. Batching would destroy the p99.9 and the estimator, which are
   the measurement.

## Fix round 2 (review findings), 2026-09-05

Two guards that were missing from paths that needed them. Neither changes any figure
recorded above; both change what the figures can be trusted to mean.

**1. The parity check trusted its control blindly.** `parity()` used the
native-vs-native control as its bar without checking the control was sane. A degenerate
control — silent, truncated, or accidentally a copy of the reference — reads ~0 dB, at
which point the *relative* bar collapses to an *absolute* 3 dB, and a **silent wasm
chain scoring 0.00 dB would PASS**. That inverts the one check this whole spike rests
on, and both reference fixtures are gitignored and regenerated by hand, so it was
reachable in practice rather than only in theory. `web/parity-node.mjs` happened to
guard it; `web/bench-worker.js` — the browser path that produced every Edge figure above
— did not.

The assertion now lives inside `parity()` in `web/namir.js`, so both runners and any
future caller inherit it, and it asserts the property directly rather than a proxy:
*a silent render must not pass.* Verified by zeroing
`fixtures/reference_control_f32le.bin` and re-running both paths — Node refuses, and
headless Edge refuses **and does not benchmark**, reporting `degenerate control render:
0.00 dB against the reference -- a silent chain (0.00 dB) would pass the 3 dB bar`.
Restoring the fixture restores the PASS. `parity-node.mjs` carries this as a permanent
negative check.

**2. `process()` carried none of `run`/`render`'s guards.** `bench` and the parity
render both reach `assert_resources_loaded` and `fault_count() == 0`; the `process`
export — Task 6's entire RT path — reached neither, so a worklet driven against a chain
whose NAM/IR never landed would have emitted silence quietly and reported zero
underruns, for exactly the wrong reason. Same failure mode as the missing `io_ptr`
write-back, one layer down.

`process()` now checks `fault_count()` every block (a plain counter read) and runs
`assert_resources_loaded()` once, on its 256th call since `init` — not per block,
because that call drains the telemetry ring into a 2 KB stack buffer. 256 blocks is
682 ms, ~34× D-8.1's `HANDOVER_CROSSFADE_MS` (20 ms = 7.5 blocks): late enough never to
false-positive on a handover still in flight, early enough to trap inside the first
second. Verified both ways — the smoke test now drives 300 calls so the good path
crosses the guard, and a negative check inits without loading anything and confirms the
trap.

**Deferred by the reviewer, recorded here so they are not lost:** `load_nam`/`load_ir`
ignore `_ptr` and read `SCRATCH` unconditionally; `--render-only` with no argument
silently writes the default path and then runs the full bench; `bench.html`'s title
`setInterval` is never cleared.

## Task 4 — build matrix and the Gate 1 verdict, 2026-09-05

**No figure in this section is certified.** This spike does not link `namir-platform`, so
no denormal guard and no thread priority is installed, and browser runs cannot be
core-pinned at all (`NAMIR_PIN_CORE` has no browser equivalent). Everything here is
informational, per D-2.1/D-2.2 and the same caveat Tasks 2 and 3 carry.

### Build — two artefacts, three configurations

`run-matrix.sh` builds two `.wasm` artefacts; the third configuration is the *same*
simd128 binary run under a V8 flag, not a third build (spec §7's Build axis lists a
flag, not a build).

    ./spikes/s5-wasm-web-audio/run-matrix.sh

| artefact | build | size |
|---|---|---|
| `web/build/scalar.wasm` | `cargo build --release --target wasm32-unknown-unknown --lib` | **859 549 B** |
| `web/build/simd128.wasm` | `RUSTFLAGS="-C link-arg=--import-undefined -C target-feature=+simd128" cargo build --release --target wasm32-unknown-unknown --lib --features wasm-simd` | **1 646 256 B** |

Toolchain `rustc 1.98.0 (88d9e12ae 2026-08-18)`. `web/build/` is gitignored — both
artefacts are reproducible from the script in ~40 s and neither belongs in git.

**One correction to the brief's build script.** An *environment* `RUSTFLAGS` replaces
`.cargo/config.toml`'s `target.wasm32-unknown-unknown.rustflags` wholesale rather than
appending to it, so the brief's simd128 command as written drops the
`-C link-arg=--import-undefined` that resolves the `env.now_us` import and fails to
link. The flag is repeated inside the `RUSTFLAGS` string, with a comment saying why.

### `wide`/simd128 demonstrably engaged — three independent signs

The brief asked for this to be checked prominently, because every downstream conclusion
depends on it. The two artefacts are not the same code:

1. **Size**: 859 549 B → 1 646 256 B, a 1.9× increase.
2. **Numerics**: the parity residual moves, **−81.4759 dB** (scalar) → **−81.6906 dB**
   (simd128), against the same **−82.7158 dB** native-vs-native control. Both **PASS**
   the relative criterion (margins 1.2399 dB and 1.0252 dB, bar 3 dB). A different f32
   accumulation order is exactly what a vectorised dot product produces, and it moved
   the residual slightly *towards* the control, not away from it.
3. **Speed**: 3.3× on `a1_standard`'s `p50` (below).

### Matrix — headless Edge, steady signal, 20 000 measured blocks, 5 reps each

Runtime: Microsoft Edge **152.0.4191.62** (`--headless=new --disable-gpu --no-sandbox`),
`crossOriginIsolated: true` in every run, V8/TurboFan. Machine: AMD Ryzen 9 5950X /
Windows 11 Pro 26200. Server: `python web/serve.py` from the spike root. Each
configuration was run alone and sequentially.

    msedge --headless=new --disable-gpu --no-sandbox \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=20000&wasm=<scalar|simd128>&model=<a1_standard|a2_lite>"
    # third configuration: the simd128 URL above, plus
    #   --js-flags=--experimental-wasm-revectorize

`WARMUP_BLOCKS = 5 000` throughout. **20 000 measured blocks, not 100 000** — the same
deviation Task 3 made and for the same reason (a 100 000-block A1 rep is minutes of wall
clock and thirty of them was not a practical wait on a machine that also has to stay
quiet). 20 000 blocks still puts 20 samples above p99.9 and does not touch the
per-residue estimator at all, and the deviation is closed by the 100 000-block
confirmation runs below, which reproduce the 20 000-block figures. Per-block timing is
retained everywhere; nothing is batched.

All percentages are of the 2 666.67 µs block period (128 frames at 48 000 Hz).

| Browser | Build | Model | p50 % | p99.9 % | estimator % | quotable reps |
|---|---|---|---|---|---|---|
| Edge headless (V8) | scalar | A1 Standard | 38.63–39.00 | **74.44–85.88** | 50.25–50.44 | 5/5 |
| Edge headless (V8) | simd128 | A1 Standard | 11.44–11.81 | **28.88–30.94** | 18.00–18.37 | 5/5 |
| Edge headless (V8) | simd128 + revectorize | A1 Standard | 11.44–12.19 | **29.06–31.88** | 18.00–18.19 | 4/5 (rep 5 discarded) |
| Edge headless (V8) | scalar | A2 Lite | 6.94–7.12 | **29.25–34.13** | 18.75–18.94 | 5/5 |
| Edge headless (V8) | simd128 | A2 Lite | 3.00–3.19 | **15.37–17.25** | 9.75–9.94 | 5/5 |
| Edge headless (V8) | simd128 + revectorize | A2 Lite | 3.19–3.56 | **14.44–17.44** | 9.94–10.13 | 5/5 |
| **Chrome** | scalar / simd128 / revectorize | both | — | — | — | **PENDING RUN** |
| **Firefox** | scalar / simd128 | both | — | — | — | **PENDING RUN** |

Native reference (same machine, same fixtures, 128-frame block, Task 2, steady, 5 reps):
A1 Standard p50 **6.30–6.65%**, p99.9 **13.60–14.69%**; A2 Lite p50 **1.83–2.06%**,
p99.9 **7.94–8.41%**.

wasm/native ratio, **simd128**, `p50`: A1 Standard **≈1.8×**, A2 Lite **≈1.6×**.
wasm/native ratio, **scalar**, `p50`: A1 Standard **≈6.0×**, A2 Lite **≈3.5×**.

## Gate 1 — compute, 2026-09-05, reference machine

**Gate 1 (≤50% of realtime on the reference machine):**

| Build | A1 Standard | A2 Lite |
|---|---|---|
| scalar | **FAIL** (p99.9 74–86%) | **PASS** (p99.9 29–34%) |
| **simd128** | **PASS** (p99.9 29–31%) | **PASS** (p99.9 15–17%) |
| simd128 + revectorize | **PASS** (p99.9 29–32%) | **PASS** (p99.9 14–17%) |

**Verdict: Gate 1 PASSES for both models on the simd128 artefact, and FAILS for A1
Standard on the scalar artefact.** The scalar/simd128 split is the whole result: it is
not a tuning margin, it is 3.3× on `p50` and 2.6× on `p99.9` for A1.

**Kill criterion 2 (>100% of realtime, sustained) has NOT fired** in any configuration.
The only reading above 100% anywhere in the matrix is a single-block `max` of 102.38% in
`a1-scalar` rep 1 — one block out of 20 000, on the configuration that already fails
Gate 1. No p99.9 in any configuration exceeds 86%.

### The reasoning behind the verdict

1. **The gate is read off `p99.9`, not `p50` and not `max`.** A block that misses its
   deadline is an underrun regardless of how many blocks made it, so the median is not
   the quantity of interest; but a single-block `max` over a 20 000-block run is one
   scheduler event and is not a property of the code. p99.9 is the figure the brief's
   table asks for and the one the harness's estimator is built to cross-check.
2. **The simd128 figures are sustained, not a short-run artefact.** See the 100 000-block
   confirmation below: 4½ minutes of continuous audio per rep reproduces the verdict.
3. **Both simd128 configurations pass by a factor of roughly 1.6× on A1 and 3× on A2.**
   That is real headroom, not a hairline pass, and it is headroom Tasks 5–6 will spend
   on the AudioWorklet's own scheduling rather than on the DSP.
4. **The pass is conditional on simd128 being available.** `rustfft`'s `wasm_simd`
   feature has no runtime detection, so the simd128 artefact traps immediately on a
   runtime without simd128 — by design, they are different artefacts, not one with a
   fallback. Since the scalar artefact FAILS Gate 1 for A1 Standard, **there is no
   working A1 configuration on a simd128-less runtime**, and A1 Standard in a browser is
   therefore gated on WebAssembly SIMD. Every shipping browser has had simd128 on by
   default since 2021 (Chrome/Edge 91, Firefox 89, Safari 16.4), so this is a stated
   floor rather than a live risk, but it is a floor: whatever ships must serve the
   simd128 build and must fail loudly, not silently, where simd128 is absent.

### The browser penalty is not a fixed multiple — and Task 3's reading of why was incomplete

Task 3 measured the scalar artefact only and reported ≈6.1× for A1 against ≈3.8× for A2,
concluding that the browser penalty scales with the model. The matrix **confirms that
observation on the scalar artefact and refutes the explanation it suggested.** With
simd128 the penalty collapses to ≈1.8× (A1) and ≈1.6× (A2) — nearly uniform. So the
model-dependent part of the scalar penalty was not "the browser is worse at bigger
models"; it was that A1 Standard's cost is dominated by dot products the native build
auto-vectorises and the scalar wasm build does not. Once wasm gets the same vector
width, the two models sit within 0.2× of each other and the residual ~1.7× is the
ordinary wasm-vs-native gap (bounds checks, no FMA contraction, no `target-cpu` tuning).

### Sustained-load confirmation — 100 000 measured blocks, simd128, 2 reps each

Each rep is 100 000 × 128 frames at 48 kHz = **4 m 27 s of continuous audio**.

```
edge a1_standard simd128 100k rep 1/2: p50 13.31% | p99 28.13% | p99.9 33.56% | max 42.19% | estimator 18.19%
edge a1_standard simd128 100k rep 2/2: p50 13.12% | p99 28.31% | p99.9 32.44% | max 40.87% | estimator 18.19%
edge a2_lite     simd128 100k rep 1/2: p50  3.00% | p99 12.37% | p99.9 16.31% | max 86.06% | estimator  9.75%
edge a2_lite     simd128 100k rep 2/2: p50  3.00% | p99 10.31% | p99.9 14.81% | max 22.12% | estimator  9.56%
```

A2 Lite reproduces the 20 000-block figures to the digit. A1 Standard runs **~14% more
expensive** over the longer window (p50 13.1–13.3% against 11.4–11.8%, p99.9 32.4–33.6%
against 28.9–30.9%) while its estimator is unchanged at 18.19% — i.e. a longer run
accumulates more scheduler and GC events in the same code, which is what a five-times
longer sample should do. **The verdict is unchanged**: 33.6% is still comfortably inside
the 50% bar, and the 20 000-block figures are quoted in the matrix table with this
correction stated rather than folded in silently.

### `--experimental-wasm-revectorize` changed nothing measurable

| Model | simd128 | simd128 + revectorize |
|---|---|---|
| A1 Standard, p50 | 11.44–11.81% | 11.44–12.19% |
| A1 Standard, p99.9 | 28.88–30.94% | 29.06–31.88% |
| A1 Standard, estimator | 18.00–18.37% | 18.00–18.19% |
| A2 Lite, p50 | 3.00–3.19% | 3.19–3.56% |
| A2 Lite, p99.9 | 15.37–17.25% | 14.44–17.44% |
| A2 Lite, estimator | 9.75–9.94% | 9.94–10.13% |

The estimator — the contamination-immune figure — is identical to within 0.2 pp in both
models, and every other column overlaps. **The flag was accepted** (V8 prints
`Error: unrecognized flag` for a flag it does not know, and did not; the run also loaded
and parity-checked the module normally), so this is "engaged and made no difference"
rather than "silently ignored" — though those two cannot be told apart from timing alone,
and no attempt was made to dump V8's generated code to distinguish them. Either way there
is nothing here to build on: **the SIMD win comes entirely from the `+simd128` build, not
from V8's revectorizer.** Downstream tasks should treat revectorize as a non-lever.

### Full per-rep results (30 matrix reps)

```
edge a1_standard scalar    rep 1/5: p50 39.00% | p99 60.19% | p99.9 77.81% | max 102.38% | estimator 50.44%
edge a1_standard scalar    rep 2/5: p50 38.63% | p99 57.75% | p99.9 76.13% | max  91.50% | estimator 50.44%
edge a1_standard scalar    rep 3/5: p50 38.81% | p99 59.44% | p99.9 77.62% | max  93.00% | estimator 50.44%
edge a1_standard scalar    rep 4/5: p50 38.81% | p99 66.00% | p99.9 85.88% | max  91.13% | estimator 50.44%
edge a1_standard scalar    rep 5/5: p50 38.81% | p99 57.56% | p99.9 74.44% | max  87.37% | estimator 50.25%

edge a1_standard simd128   rep 1/5: p50 11.81% | p99 26.25% | p99.9 30.94% | max  36.94% | estimator 18.00%
edge a1_standard simd128   rep 2/5: p50 11.63% | p99 20.81% | p99.9 29.06% | max  36.75% | estimator 18.00%
edge a1_standard simd128   rep 3/5: p50 11.44% | p99 20.63% | p99.9 29.06% | max  30.94% | estimator 18.19%
edge a1_standard simd128   rep 4/5: p50 11.44% | p99 19.88% | p99.9 28.88% | max  31.31% | estimator 18.19%
edge a1_standard simd128   rep 5/5: p50 11.62% | p99 21.19% | p99.9 29.44% | max  33.56% | estimator 18.37%

edge a1_standard revector. rep 1/5: p50 12.19% | p99 27.56% | p99.9 31.87% | max  37.87% | estimator 18.00%
edge a1_standard revector. rep 2/5: p50 11.44% | p99 20.44% | p99.9 29.25% | max  42.56% | estimator 18.19%
edge a1_standard revector. rep 3/5: p50 11.44% | p99 20.81% | p99.9 29.06% | max  47.25% | estimator 18.19%
edge a1_standard revector. rep 4/5: p50 12.00% | p99 29.63% | p99.9 31.88% | max  42.56% | estimator 18.19%
edge a1_standard revector. rep 5/5: p50 16.87% | p99 30.56% | p99.9 32.06% | max  39.38% | estimator 18.56%  <-- DISCARDED

edge a2_lite     scalar    rep 1/5: p50  7.12% | p99 27.94% | p99.9 34.13% | max  95.44% | estimator 18.94%
edge a2_lite     scalar    rep 2/5: p50  6.94% | p99 20.62% | p99.9 30.19% | max  50.63% | estimator 18.75%
edge a2_lite     scalar    rep 3/5: p50  6.94% | p99 28.69% | p99.9 33.37% | max  35.25% | estimator 18.94%
edge a2_lite     scalar    rep 4/5: p50  6.94% | p99 20.06% | p99.9 29.25% | max  33.94% | estimator 18.94%
edge a2_lite     scalar    rep 5/5: p50  6.94% | p99 20.06% | p99.9 29.44% | max  34.13% | estimator 18.75%

edge a2_lite     simd128   rep 1/5: p50  3.19% | p99 14.25% | p99.9 17.06% | max  86.63% | estimator  9.75%
edge a2_lite     simd128   rep 2/5: p50  3.00% | p99 14.81% | p99.9 17.25% | max  18.56% | estimator  9.94%
edge a2_lite     simd128   rep 3/5: p50  3.00% | p99 12.75% | p99.9 16.69% | max  19.69% | estimator  9.94%
edge a2_lite     simd128   rep 4/5: p50  3.00% | p99 11.06% | p99.9 15.56% | max  18.00% | estimator  9.75%
edge a2_lite     simd128   rep 5/5: p50  3.00% | p99 10.50% | p99.9 15.37% | max  33.75% | estimator  9.75%

edge a2_lite     revector. rep 1/5: p50  3.19% | p99 11.44% | p99.9 14.44% | max  76.31% | estimator  9.94%
edge a2_lite     revector. rep 2/5: p50  3.56% | p99 14.25% | p99.9 17.44% | max  23.44% | estimator  9.94%
edge a2_lite     revector. rep 3/5: p50  3.56% | p99 13.88% | p99.9 17.44% | max  19.87% | estimator 10.13%
edge a2_lite     revector. rep 4/5: p50  3.38% | p99 12.94% | p99.9 16.50% | max  31.69% | estimator 10.12%
edge a2_lite     revector. rep 5/5: p50  3.56% | p99 12.94% | p99.9 16.50% | max  25.31% | estimator  9.94%
```

Parity, reported by the page before it would benchmark, in every one of the eight runs:
scalar **−81.4759 dB**, simd128 (and revectorize) **−81.6906 dB**, control
**−82.7158 dB** — **PASS** throughout. The gate is live and there is no bypass; the
figures above exist because the port was proved correct first.

### Discarded repetitions, and why `is_quotable()` cannot be the rule in a browser

**`Stats::is_quotable()`'s rule (`p999 - estimator <= 5.0`, D-2.4) flags all 30 browser
reps, including every one quoted above** — as it flagged all ten of Task 3's. That is not
thirty contaminated runs; it is the rule measuring something it was not built for.
`is_quotable` was designed to catch *background machine load* inflating a tail against an
otherwise-flat per-residue baseline. In a browser the tail is structurally fat — GC,
tier-up and the renderer's own scheduler all land inside the timed span — so
`p999 − estimator` is 5–35 pp in *every* browser configuration, including the calmest.
Applying the brief's Step 4 literally would discard the entire matrix and leave no
verdict, which is plainly not what it is for.

**The rule actually used.** Machine load moves the *typical* block cost, so it shows in
`p50` while the per-residue estimator (a periodic worst-case-block figure) stays put. A
repetition is discarded when its `p50` departs from its own configuration's modal `p50`
by more than 10% while the estimator does not move with it. Exactly one repetition in
thirty meets that:

| Discarded | Reading | Why |
|---|---|---|
| `a1_standard`, simd128 + revectorize, **rep 5** | p50 **16.87%** against that configuration's own modal 11.44% (**+47%**); estimator 18.56%, flat against the other four reps' 18.00–18.19% | Machine load during the rep, not a cost of the code — the flat estimator is the tell, since a real +47% cost increase would move it too. Nothing was deliberately started, but this is a shared desktop and this session's own agent and browser-teardown processes run on it; that is the honest account of what else was running. Its p99.9 (32.06%) is in line with the other four and would not have changed the verdict either way. |

The other 29 are quoted as measured. Across their five reps, `p50` is stable to ±0.37 pp
(A1 scalar), ±0.37 pp (A1 simd128), ±0.18 pp (A2 scalar) and ±0.19 pp (A2 simd128), and
the per-residue estimator is stable to ±0.2 pp in every configuration — which is what a
clean set of repetitions looks like.

**A systematic first-rep `max` outlier, reported rather than discarded.** In four of six
configurations the largest single-block `max` in the whole set falls in **rep 1**
(102.38%, 95.44%, 86.63%, 76.31%) while rep 1's `p50`, `p99.9` and estimator sit with the
others. That is V8 tiering the freshly-instantiated module up, plus the first major GC
after page load, both inside the first repetition. A shipping worklet would pay the same
cost on its first blocks, so it is recorded, not discarded — and it is one more reason
the verdict is read off `p99.9` rather than `max`.

### PENDING RUN — Chrome and Firefox

**Neither Chrome nor Firefox is installed on this machine, and nothing was installed.**
The matrix was run under headless Microsoft Edge, which is Chromium/V8 — the same engine
family as Chrome, which is also why the V8 revectorize flag applies to it. **No Chrome or
Firefox number appears anywhere above; the rows are empty, not estimated.** Firefox in
particular is SpiderMonkey/Ion, a different wasm compiler with a different vectoriser,
and nothing here should be read as covering Gecko.

To run them, on a machine that has them (server started from the spike root, artefacts
built first):

    cd spikes/s5-wasm-web-audio
    ./run-matrix.sh
    # both native renders, needed by the parity gate before it will benchmark:
    cargo run --release --bin native_bench -- --render-only fixtures/reference_render_f32le.bin
    CARGO_TARGET_DIR=../../target-s5-control \
      RUSTFLAGS="-C target-cpu=x86-64 -C target-feature=-avx,-avx2,-fma" \
      cargo run --release --bin native_bench -- --render-only fixtures/reference_control_f32le.bin
    cargo run --release --bin native_bench    # the native reference figures
    python web/serve.py

    # then, for each of the four (build x model) combinations:
    "C:\Program Files\Google\Chrome\Application\chrome.exe" \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=20000&wasm=scalar&model=a1_standard"
    #   ... &wasm=simd128&model=a1_standard
    #   ... &wasm=scalar&model=a2_lite
    #   ... &wasm=simd128&model=a2_lite
    # and the revectorize configuration, Chrome only, launched fresh:
    "C:\Program Files\Google\Chrome\Application\chrome.exe" \
      --js-flags=--experimental-wasm-revectorize \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=20000&wasm=simd128&model=a1_standard"

    # Firefox has no equivalent flag; run the four build x model combinations only:
    "C:\Program Files\Mozilla Firefox\firefox.exe" \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=20000&wasm=simd128&model=a1_standard"

Confirm **`crossOriginIsolated: true`** on the page before recording anything — without
it the `performance.now()` quantum is 100 µs (3.75% of the block period) instead of 5 µs
(0.1875%) and the figures are not comparable. Record the browser version alongside. Under
`?auto=1` each result line is also beaconed to the dev server's log, which is the
transcript for a headless run; interactively, read them off the page.

### NOT RUN — the laptop axis

Spec §7's Machine axis ("Reference | one laptop") and the brief's Step 6 (same procedure,
own section, ≤100% threshold) were **not run**. **Reason: no second machine is reachable
from this session.** This is recorded as not-run, not as a pass and not as omitted. Spec
§11 makes the second machine individually droppable without invalidating the gates, so
the reference-machine verdict stands on its own — but the ≤100% laptop bar is untested,
and a mid-range laptop is exactly where the A1-Standard-on-simd128 headroom (33.6% p99.9
here) would be consumed. Anyone re-running this on a second machine should start there.

### Task 4 verdict

**Proceed to Task 5.** Four things carried forward:

1. **Gate 1 PASSES on the simd128 artefact for both models** — A1 Standard p99.9
   28.88–30.94% (33.56% sustained over 100 000 blocks), A2 Lite p99.9 15.37–17.25%,
   against a ≤50% bar. Kill criterion 2 did not fire anywhere.
2. **Gate 1 FAILS for A1 Standard on the scalar artefact** (p99.9 74–86%), so browser
   support for A1 Standard is *conditional on WebAssembly SIMD*, with no fallback by
   design. Task 5 onward must serve the simd128 build and must fail loudly where
   simd128 is absent.
3. **Revectorize is a non-lever.** No measurable change in either model; the entire SIMD
   win is in the `+simd128` build.
4. **Chrome, Firefox and the laptop axis are open**, with the exact commands and the
   reasons recorded above. Nothing in this section is a certified figure.
