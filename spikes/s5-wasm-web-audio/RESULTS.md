# S-5 measurement log

## Verdict — 2026-09-05

**Gate 1 (compute): PASS, conditional on `simd128`.** A1 Standard p99.9 **32.4–33.6%** of the
block period sustained on a steady signal, A2 Lite **15.4–17.3%**, against a <=50% bar. The
**scalar** artefact FAILS A1 (74–86%), and there is no runtime fallback by design, so a browser
build of A1 Standard is gated on WebAssembly SIMD. Under a subnormal tail — silence after
signal, the most ordinary thing a guitar input does — A1 reaches p99.9 **44.25–58.13%**, one rep
of five above the bar; wasm mandates no flush-to-zero and `DenormalGuard` has no equivalent
there, so that cost is structural and A1's real margin is a hairline, not 1.5x.
**Gate 2 (scheduling): PASS.** Zero underruns in every steady-state second of every run, both
signal regimes, over 60 s and 300 s, on real hardware. The literal "every run zero" reading is
missed: about a third of runs drop one device callback in the *first second* of the stream's
life — proven by a three-arm 36-run experiment to be Chromium/WASAPI stream start-up, occurring
at the same rate and the same second with the chain not running at all.
**Gate 3 (latency): API figures only, and they are not encouraging.** **62 ms** best-case
API-reported total (10 ms base + 42 ms output + a *declared* 10 ms input constant), roughly twice
the ~30 ms soft reference before measuring anything the API does not account for;
`--enable-exclusive-audio`, which spec §6 assumed would be the low-latency condition, is a
**2.6x regression** (output 42 -> 128 ms). The physical loopback measurement is **PENDING RUN** —
no cable on this machine — and it can only be larger than 62 ms, never smaller.

**Recommendation: phase (b) is worth doing, as a file-playback-first demo, and only as that.**
The compute and scheduling questions the spike was built to answer both came back yes, with the
denormal condition attached. The latency question came back no for live guitar input in a
browser on this path, which is the answer that decides the demo's *shape* rather than its
existence — and file-playback-first was already the plan. What phase (b) must not do is assume
the numbers below transfer: **every browser figure here is headless Microsoft Edge 152 on one
machine.** Chrome and Firefox were never installed; SpiderMonkey is a different wasm compiler and
is entirely unmeasured. **No figure in this file is certified** in `docs/02-architecture.md`
§2's sense, and a browser figure cannot be — it passes through a JIT, a browser process model
and an OS audio stack the project does not control.

The record below is the **corrected** one, and it contains retractions kept deliberately in
place: A1 "doesn't fit in the browser" (a scalar-build artefact, Task 3 -> Task 4); the amp-decay
penalty being *not* a denormal effect (measurement says it mostly is, Task 5); the first-second
underrun being this spike's own handover guard (disproved by experiment, Task 6 fix round); and
a per-block resampler delta (retracted as session-order contamination, Task 8 — and the honest
reason is that no clean second measurement was ever obtained, not that a clean comparison came
back negative).


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

**Relabelled and partly re-adjudicated at Task 5 (2026-09-05).** This signal is now
`Signal::AmplitudeDecay`, logged as `amp-decay`: what it sweeps is amplitude, and the
figures above are measurements of that. The paragraph immediately above is right that the
*input* never leaves f32's normal range and wrong about the consequence — Task 5's census
measures **47.6% of blocks carrying subnormal output samples** (down to 1e-45) and MXCSR
raising its denormal-operand flag on **52.3%**, and installing FTZ/DAZ collapses the whole
~44% p50 rise to ~2%. So these figures were, in the main, a denormal measurement after all,
taken with an instrument that confounds amplitude and subnormality. The figures stand; the
name and the explanation are corrected in Task 5's section, which also adds
`Signal::SubnormalTail` — the probe that holds amplitude fixed.

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
   here; only the steady signal was measured in the browser. **(Task 5 made it: see that
   section. The signal named here is now `amp-decay`; the denormal probe is
   `subnormal-tail`, and the browser penalty is 1.35x p50 / 1.57x p99.9 on A1 Standard.)**
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

| Browser | Build | Model | p50 % | p99.9 % | estimator % | reps retained |
|---|---|---|---|---|---|---|
| Edge headless (V8) | scalar | A1 Standard | 38.63–39.00 | **74.44–85.88** | 50.25–50.44 | 5/5 |
| Edge headless (V8) | simd128 | A1 Standard | 11.44–11.81 | **28.88–30.94** (**32.44–33.56 sustained**) | 18.00–18.37 | 5/5 |
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
| **simd128** | **PASS** — **p99.9 32.4–33.6% sustained** (29–31% at the 20 000-block screening length) | **PASS** — p99.9 14.8–16.3% sustained (15–17% screening) |
| simd128 + revectorize | **PASS** (p99.9 29–32%) | **PASS** (p99.9 14–17%) |

**Condition added at Task 5 (2026-09-05) — read this before quoting the row above.**
Every figure in this table was measured on a **steady** signal. Under the subnormal-tail
signal Task 5 added (signal, then silence — what a guitar input does between notes), the
same A1 Standard / simd128 configuration reaches **p99.9 44.25–58.13%**, with one of five
reps *above* this gate's ≤50% bar; A2 Lite goes to 19.41%. **The verdict below is not
retracted** — the median rep (47.44%) is inside the bar and kill criterion 2 is nowhere
near firing — but A1 Standard's real margin is a hairline, not the ~1.5× this row implies,
and **Task 6 must budget A1 against ~50%, not 33.6%**. There is no wasm equivalent of
`DenormalGuard`, so that cost is structural. See "Task 5 — the denormal sub-experiment,
rebuilt".

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
3. **The simd128 margin, measured at the sustained length, is ~1.5× on A1 and ~3× on
   A2** (33.6% and 16.3% p99.9 against the 50% bar). That is real headroom rather than a
   hairline pass, and it is headroom Tasks 5–6 will spend on the AudioWorklet's own
   scheduling rather than on the DSP — but see the growth caveat below: A1's is the
   smaller margin *and* the one that moved with run length.
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
accumulates more scheduler and GC events in the same code.

**The verdict is unchanged — 33.6% is still comfortably inside the 50% bar — but the
growth itself is an open question, and it is not closed here.** Two readings fit these
four reps equally well, and nothing measured distinguishes them:

- **Asymptotic.** A longer sample simply catches more of a fixed-rate tail, so the p99.9
  converges somewhere near 34% and stays there. This is the likelier reading and it is
  the one the flat estimator supports.
- **Drifting.** Something accumulates with session length — heap growth, code-cache
  churn, fragmentation — and the p99.9 keeps climbing. At the observed +14% per 5× of
  run length, A1 would reach the 50% bar on the order of 10^7 blocks — hours, not
  minutes, of continuous play, which is not an absurd session for a guitar amp left
  running. (Order of magnitude only: extrapolating a two-point trend three decades is
  not a measurement, which is the point.)

**Two 100 000-block reps cannot tell those apart, and no longer run was made.** This
matters specifically for **Task 6**, whose AudioWorklet runs indefinitely rather than for
a bounded block count: A1 Standard is both the smaller margin and the only figure that
moved with run length. A single multi-hour run, or the same run at 10^6 blocks, would
settle it; until then Task 6 should budget against **33.6%, not 29%**, and should watch
its own p99.9 over session time rather than assume it is stationary. The 20 000-block
figures remain quoted in the matrix table alongside the sustained ones rather than being
folded in silently.

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
models, and every other column overlaps.

#### Why this is "engaged and did nothing", not "silently ignored"

**A retracted claim first.** An earlier revision of this section argued the flag was
accepted because *V8 prints `Error: unrecognized flag` for a flag it does not know, and
did not here*. **That evidence is worthless and the claim is withdrawn**: on this machine
`msedge --headless=new --js-flags=--totally-bogus-flag` also exits 0 with empty stderr,
so the tell never fires either way. Chromium does not surface the renderer's V8 stderr
here. Recorded rather than quietly replaced.

The question was then settled properly, with two checks that do discriminate.

**1. `--js-flags` demonstrably reaches V8 in headless Edge.** Run the bench page under
`--js-flags=--jitless` and the worker fails with

    ReferenceError: WebAssembly is not defined
        at loadNamir (http://127.0.0.1:8080/web/namir.js:4:24)

i.e. the flag reached V8 and removed the `WebAssembly` global. A flag string that is
ignored cannot do that, so the plumbing is proved, not assumed.

**2. The revectorizer provably runs on this artefact, and provably revectorizes nothing.**
V8 ships `--trace-wasm-revectorize` alongside the feature flag, which is a direct
observation of the pass rather than an inference from timing. Node v24.19.0
(V8 **13.6.233.17**) takes V8 flags on the command line:

    S5_WASM=web/build/simd128.wasm       node --experimental-wasm-revectorize --trace-wasm-revectorize web/parity-node.mjs

    Begin revec function _RNvMs1_...namir_ir9convolver...PreparedIr...
    store seeds:
    { #4888 Store *(#4850 + #4887) = #4885 [raw, protected, Simd128, NoWriteBarrier]
      #4893 Store *(#4850 + 16 + #4892) = #4890 [raw, protected, Simd128, ...] }
    Revec: BuildTreeRec 1052: Added a vector of stores.
    Revec: NewPackNode 295: PackNode Store(#4888, #4893)
    Revec: Run 1430: Build tree failed!
    ...

Over the whole module the pass visits **16 wasm functions** — `namir-ir`'s convolver and
`rustfft`'s `wasm_simd` radix-4 and butterfly kernels among them — and **succeeds on
none**: 14 `Build tree failed!` and 9 `Empty seed`, and no successful pack anywhere in
the trace. **Negative control:** `--trace-wasm-revectorize` *without*
`--experimental-wasm-revectorize` prints zero `Begin revec function` lines, so the trace
is the feature's own output and not noise. The flag is also a real, current V8 flag —
`node --v8-options` lists `--experimental-wasm-revectorize (enable 128 to 256 bit
revectorization for Webassembly SIMD (experimental))` — whereas a bogus flag is rejected
outright (`node: bad option: --totally-bogus-flag`), which is the discriminator Edge does
not give.

**Caveat, stated rather than papered over:** check 2 was made under Node's V8 13.6, not
under Edge 152's V8, and no equivalent trace was captured from inside Edge (the renderer's
stdout is not reachable through the dev-server beacon). What check 2 establishes is a
property of the *artefact* — this wasm module contains no store trees the revectorizer can
pack — which is determined by the code rustc and rustfft emit, not by which V8 loads it.
Combined with check 1 (Edge does honour `--js-flags`), that is a solid explanation for the
null timing result rather than an unexplained one.

**Conclusion, unchanged by all of the above:** there is nothing here to build on. **The
SIMD win comes entirely from the `+simd128` build, not from V8's revectorizer**, and
downstream tasks should treat revectorize as a non-lever — now for a known reason: the
pass runs and finds nothing to widen.

**Why it finds nothing to widen was investigated separately and is a closed question:
see `REVECTORIZE.md`.** In short — `wide::f32x8` *is* two adjacent `v128` ops on wasm, so
the right instruction pairs are being handed to the pass; V8's seeder needs the two stores
to share one address local differing only by a folded `offset=` immediate, and LLVM's
strength reduction gives our loops a recomputed base (`i32.const 16; i32.add`) instead.
Four rewrites of the kernel all failed to seed. Not a `wide` problem, not a `rustfft`
problem, not a V8 limitation — and not one we have a source-level lever over.

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

**The rule actually used is a substitution, and it is mine — not D-2.4's, and not
anything the project has previously agreed.** It is stated here in full so that a later
reader can disagree with it rather than inherit it silently, and so that Tasks 5 and 6,
which will need the same substitution, use the same numbers rather than reinventing them.

> **S-5 browser contamination rule (Task 4).** Machine load moves the *typical* block
> cost, so it shows in `p50` while the per-residue estimator — a periodic
> worst-case-block figure, insensitive to a uniform slowdown of the common case — stays
> put. Discard a repetition when **both** hold:
>
> 1. its `p50` departs from its own configuration's modal `p50` by **more than 10%**
>    (relative), **and**
> 2. its estimator departs from that configuration's modal estimator by **less than 5%**
>    (relative).
>
> Clause 2 carries a number for the same reason clause 1 does: without one, "the
> estimator did not move with it" is a judgement call, which makes the rule
> unreproducible and — worse — tunable per-run to make a figure pass. Both clauses are
> ratios, so the rule is scale-free across models and builds.

Exactly one repetition in thirty meets both clauses:

| Discarded | Reading | Why |
|---|---|---|
| `a1_standard`, simd128 + revectorize, **rep 5** | clause 1: p50 **16.87%** against that configuration's modal 11.44% = **+47.5%** (bar: >10%). clause 2: estimator **18.56%** against modal 18.19% = **+2.0%** (bar: <5%) | Machine load during the rep, not a cost of the code — the flat estimator is the tell, since a real +47% cost increase would move it too. Nothing was deliberately started, but this is a shared desktop and this session's own agent and browser-teardown processes run on it; that is the honest account of what else was running. Its p99.9 (32.06%) is in line with the other four and would not have changed the verdict either way. |

The other 29 are quoted as measured. Across their five reps, `p50` is stable to ±0.37 pp
(A1 scalar), ±0.37 pp (A1 simd128), ±0.18 pp (A2 scalar) and ±0.19 pp (A2 simd128), and
the per-residue estimator is stable to ±0.2 pp in every configuration — which is what a
clean set of repetitions looks like.

**A systematic first-rep `max` outlier, reported rather than discarded.** In **five of the
six** configurations the largest single-block `max` in the set falls in **rep 1** —
102.38% (A1 scalar), 95.44% (A2 scalar), 86.63% (A2 simd128), 76.31% (A2 revectorize) and
36.94% (A1 simd128, where the whole set is tight enough that the pattern is easy to miss).
Only A1 revectorize breaks it, and that is the configuration whose rep 5 was discarded for
load. In all five, rep 1's `p50`, `p99.9` and estimator sit with the others. That is V8 tiering the freshly-instantiated module up, plus the first major GC
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

`measured=100000` below is the spec's full `MEASURED_BLOCKS`, asked for deliberately:
this section's own Edge figures are a 20 000-block screening plus a 100 000-block
confirmation, and A1 Standard's p99.9 differs by ~14% between the two, so a Chrome or
Firefox run at 20 000 would not be comparable to the number Gate 1 is actually decided
on. If the full length is impractical, run 20 000 for the sweep **and** 100 000 for at
least `simd128 x a1_standard`, and label which is which.

    # then, for each of the four (build x model) combinations:
    "C:\Program Files\Google\Chrome\Application\chrome.exe" \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=100000&wasm=scalar&model=a1_standard"
    #   ... &wasm=simd128&model=a1_standard
    #   ... &wasm=scalar&model=a2_lite
    #   ... &wasm=simd128&model=a2_lite
    # and the revectorize configuration, Chrome only, launched fresh:
    "C:\Program Files\Google\Chrome\Application\chrome.exe" \
      --js-flags=--experimental-wasm-revectorize \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=100000&wasm=simd128&model=a1_standard"

    # Firefox has no equivalent flag; run the four build x model combinations only:
    "C:\Program Files\Mozilla Firefox\firefox.exe" \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=100000&wasm=simd128&model=a1_standard"

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

**Proceed to Task 5.** Five things carried forward:

1. **Gate 1 PASSES on the simd128 artefact for both models** — A1 Standard p99.9
   **32.44–33.56% sustained** over 100 000 blocks (28.88–30.94% at the 20 000-block
   screening length), A2 Lite p99.9 14.81–16.31% sustained, against a ≤50% bar. Kill
   criterion 2 did not fire anywhere.
2. **Gate 1 FAILS for A1 Standard on the scalar artefact** (p99.9 74–86%), so browser
   support for A1 Standard is *conditional on WebAssembly SIMD*, with no fallback by
   design. Task 5 onward must serve the simd128 build and must fail loudly where
   simd128 is absent.
3. **A1 Standard's cost grew ~14% between the 20 000- and 100 000-block runs, and whether
   that growth is asymptotic is UNRESOLVED.** Two 100 000-block reps cannot separate "a
   longer sample catches more of a fixed-rate tail" from "something accumulates with
   session length"; the flat estimator favours the first, nothing rules out the second,
   and no longer run was made. **This is Task 6's problem specifically**, because an
   AudioWorklet runs indefinitely rather than for a bounded block count: budget against
   **33.6%, not 29%**, watch p99.9 over session time rather than assuming it is
   stationary, and settle it with one multi-hour or 10^6-block run. A1 is both the
   smaller margin and the only figure that moved with run length.
4. **Revectorize is a non-lever, for a now-known reason.** V8's revectorizer provably
   runs on this artefact under `--trace-wasm-revectorize` and succeeds on none of the 16
   functions it visits (14 `Build tree failed!`, 9 `Empty seed`). The entire SIMD win is
   in the `+simd128` build.
5. **Chrome, Firefox and the laptop axis are open**, with the exact commands and the
   reasons recorded above. Nothing in this section is a certified figure.

## Task 5 — the denormal sub-experiment, rebuilt, 2026-09-05

**No figure in this section is certified.** Same caveat as Tasks 2–4: no `namir-platform`,
no core pinning in the browser, D-2.1/D-2.2 apply.

### The sub-experiment as briefed could not have measured what it claimed

The brief pointed kill criterion 3 (">2x penalty on decaying signal") at `Signal::Decaying`,
on the premise that that signal drives the chain into f32 subnormals. **It does not drive
the *input* anywhere near them.** The implemented decay is `amp *= 0.999_5` per block, so
over `MEASURED_BLOCKS` = 100 000 blocks the driving amplitude bottoms out at
`0.999_5^100_000` ~ **1.9e-22**. The spec's own stated target of ~1e-30 would not have
sufficed either: f32's smallest **normal** is `f32::MIN_POSITIVE` = **1.175 494 35e-38**.
Sixteen orders of magnitude short.

Task 5 therefore does four things: adds a signal that provably reaches subnormals,
**instruments the chain so the premise is measured rather than assumed**, relabels the old
mode as the amplitude test it is, and prices what FTZ/DAZ would have bought.

### Relabelling: `Decaying` -> `AmplitudeDecay`

`Signal::Decaying` is now `Signal::AmplitudeDecay`, labelled `amp-decay` in every log line,
and its doc comment states what it is. **Its figures are not deleted and not retracted** —
Task 2's `a1_standard decaying` numbers are valid measurements of the chain's cost under a
decaying-amplitude input, and Task 5 re-measured them (below) to within noise. What changed
is only the claim about *why* they are what they are. Everywhere below, `amp-decay` is the
old `decaying`.

### The new mode: `Signal::SubnormalTail`

Not a slowly shrinking input — the classic audio denormal shape: **signal, then silence.**
16 blocks of full-scale noise, then **exact zero** for the remaining 496 blocks of a
512-block cycle, repeating (`TAIL_BURST_BLOCKS` / `TAIL_PERIOD_BLOCKS` in `harness.rs`).
96.9% of measured blocks are driven by nothing at all, leaving the chain's own IIR state —
EQ biquads, gate envelope, gain ramps, the output stage — and the convolution tail to decay
under their own poles. Warmup runs the measured signal too, so the chain's state is already
in the burst-and-silence regime when measurement begins. (Corrected at fix round 1: an
earlier revision said the measured window "opens mid-cycle". It does not — `fill_block`'s
block index restarts at 0 for the measured loop, so the window opens on a burst. The part
that matters is the warm *state*, not the phase.)

### Proving subnormals actually occur — two witnesses, and a control

`Harness::census` re-runs the identical warmup/measured window **untimed** (a separate pass,
so nothing it does can land inside a timed span) and reports what was numerically present:

1. **Output-side count** (portable — runs natively and in the browser): subnormal f32s in
   the stereo output buffer, per block and per sample, plus the smallest non-zero |sample|.
2. **MXCSR status bits** (native x86-64 only): `_mm_getcsr` is cleared before each
   `process_block` and read after, counting blocks in which the CPU itself raised the
   **Denormal-operand (DE)** or **Underflow (UE)** flag. This is the strong witness — it
   sees subnormal arithmetic on *internal* state the harness cannot read back through the
   output buffer.
3. **The control for the census itself**: the whole census is repeated with **FTZ/DAZ
   installed**. Every count that is a real subnormal must collapse to zero; anything that
   does not was never a subnormal effect.

`set_flush_to_zero()` in `harness.rs` installs FTZ|DAZ with the same two MXCSR bits
`namir-platform/src/denormal.rs` sets. **The spike still does not depend on
`namir-platform`, and nothing under `crates/` was touched** — the guard is priced with the
two bits it comes down to, applied from the spike's own code, which is also the only way to
keep the native and wasm sides running the same crate graph.

**Census, `a1_standard` and `a2_lite`, 20 000 measured blocks after 5 000 warmup:**

| model | signal | subnormal out blocks | subnormal out samples | MXCSR **DE** blocks | UE blocks | min abs |
|---|---|---|---|---|---|---|
| a1_standard | steady | 0 (0.0%) | 0 | 0 (0.0%) | 0 | 9.83e-7 |
| a1_standard | amp-decay | **9 523 (47.6%)** | 2 436 470 | **10 458 (52.3%)** | 10 468 | **1e-45** |
| a1_standard | **subnormal** | 0 (0.0%) | 0 | **17 088 (85.4%)** | 18 221 | 1.26e-8 |
| a2_lite | steady | 0 (0.0%) | 0 | 0 (0.0%) | 0 | 3.05e-5 |
| a2_lite | amp-decay | 9 525 (47.6%) | 2 437 011 | 10 376 (51.9%) | 10 382 | **1e-45** |
| a2_lite | **subnormal** | 0 (0.0%) | 0 | **15 291 (76.5%)** | 15 282 | 6.56e-7 |

**The same census with FTZ/DAZ installed** — the control:

| model | signal | subnormal out blocks | MXCSR DE blocks | UE blocks | min abs |
|---|---|---|---|---|---|
| a1_standard | steady | 0 | 0 | 0 | 9.83e-7 |
| a1_standard | amp-decay | **0** | **0** | 3 893 | **1.179 718e-38** (just above `MIN_POSITIVE`) |
| a1_standard | subnormal | 0 | **0** | 8 859 | 1.26e-8 |
| a2_lite | steady | 0 | 0 | 0 | 3.05e-5 |
| a2_lite | amp-decay | **0** | **0** | 3 817 | **1.177 281e-38** |
| a2_lite | subnormal | 0 | **0** | 6 079 | 6.56e-7 |

Every DE count goes to zero and `amp-decay`'s smallest output moves from 1e-45 to a value
**immediately above** `f32::MIN_POSITIVE` — i.e. flushed. The counts were real subnormals,
not an artefact of how they were counted.

**One column does not collapse, and it is not supposed to: UE.** Underflow survives at
3 893 / 8 859 / 3 817 / 6 079 blocks. That is not a hole in the control, it is FTZ working:
DAZ makes a subnormal *operand* read as zero, so DE stops being raised; FTZ makes a
subnormal *result* be replaced by zero, and the SSE definition of that substitution is to
raise **UE** (with PE) as it happens. So UE rising where DE vanishes is the guard reporting
each flush, and a run with FTZ on and UE at zero would mean nothing had needed flushing.
The rule stated above — "every count that is a real subnormal must collapse to zero" —
applies to DE and to the output-side counts, which are counts of subnormals; UE is a count
of *flushes*, and is stated here rather than omitted precisely because it moves the other
way. (Recorded at fix round 1: the first revision of this table simply dropped the column,
which reads as fitting the evidence to the rule even though the explanation is benign.)

**What this establishes, and what it does not.**

- **`SubnormalTail` does what it was built to do**: on `a1_standard` the CPU reports
  denormal-operand arithmetic inside `process_block` in **85.4% of measured blocks**, with
  the output staying in normal range (min abs 1.26e-8 — a small DC-ish residue, not a
  subnormal). That is exactly the shape the ruling asked for: subnormals flowing through
  *internal* state across most of the measured window. **An output-side census alone would
  have reported 0.0% and been wrong**, which is why the MXCSR witness was built.
- **The wasm side has no MXCSR** — there is no such register on wasm32, which is the whole
  premise of this sub-experiment — so the browser's direct witness is the output-side count
  only. Run in Edge on the simd128 artefact, `a1_standard`, 20 000 blocks:

      edge simd128 a1_standard census amp-decay: blocks 20000 | sub-out blocks 9521 (47.6%) | sub-out samples 2436620 | min |x| 1.401298e-45
      edge simd128 a1_standard census subnormal: blocks 20000 | sub-out blocks 0 (0.0%) | sub-out samples 0 | min |x| 2.514571e-08

  The `amp-decay` census is a **direct in-browser measurement that subnormals flow through
  the wasm chain**: 9 521 blocks against native's 9 523 (0.02% apart), 2 436 620 samples
  against 2 436 470, and the identical smallest value 1.401 298e-45, the smallest f32
  subnormal. For `SubnormalTail` the browser census reads 0.0% for the same reason native's
  does — the subnormals are internal. **That half is an inference, and it is labelled as
  one**: the same source, the same signal generator and the same fixtures produce output
  agreeing to −81.69 dB against the native reference, native's MXCSR reports 85.4% of blocks
  doing denormal arithmetic, and WebAssembly mandates full IEEE-754 subnormal support with
  no flush mode, so the same intermediates necessarily arise. Nothing measured *inside* the
  browser observes them directly, and nothing here should be read as if it did.

### Timing — native, 100 000 measured blocks, 5 reps, reference machine

Raw log: `native_bench_task5.txt` (committed). Medians of the five reps.

| model | signal | FTZ/DAZ | p50 % | p99.9 % | estimator % |
|---|---|---|---|---|---|
| a1_standard | steady | off | **6.42** | 13.51 | 10.05 |
| a1_standard | amp-decay | off | **9.23** | 16.73 | 10.23 |
| a1_standard | **subnormal** | off | **8.93** | 16.36 | 10.09 |
| a1_standard | steady | **on** | 6.48 | 13.65 | 10.12 |
| a1_standard | amp-decay | **on** | **6.59** | 13.63 | 10.21 |
| a1_standard | **subnormal** | **on** | **6.46** | 13.60 | 10.11 |
| a2_lite | steady | off | 2.04 | 7.66 | 5.63 |
| a2_lite | amp-decay | off | 2.15 | 7.48 | 5.69 |
| a2_lite | **subnormal** | off | 2.13 | **9.14** | 5.75 |
| a2_lite | steady | on | 2.04 | 7.55 | 5.67 |
| a2_lite | amp-decay | on | 2.04 | 7.50 | 5.69 |
| a2_lite | **subnormal** | on | 2.00 | 7.51 | 5.70 |

`a1_standard`'s `amp-decay` and `subnormal` reps carry `is_quotable()` CONTAMINATED flags,
for the same reason Task 2 recorded and did not discard them: the per-residue estimator is
insensitive to a cost rise that moves the whole distribution, so `p999 − estimator` widens
without any machine load being present. All five reps of each set agree to ±0.4 pp, and the
FTZ-on counterparts of the same configurations are all quotable — which is the tell.

### Timing — Edge headless, simd128, 100 000 measured blocks, 5 reps

Runtime: Microsoft Edge (Chromium/V8), `--headless=new --disable-gpu --no-sandbox`,
`crossOriginIsolated: true`, server `python web/serve.py`, each configuration launched
alone and sequentially. Raw log: `edge_task5.txt` (committed). The parity gate ran and
**PASSED before every one of the six runs** (residual −81.6906 dB, control −82.7158 dB,
margin 1.0252 dB); nothing about the gate, its degenerate-control assertion or the
`assert_resources_loaded` / `fault_count() == 0` guards was changed, weakened or bypassed.

| model | signal | p50 % | p99.9 % | estimator % | reps retained |
|---|---|---|---|---|---|
| a1_standard | steady | 12.75–13.13 (med **12.94**) | 30.00–31.31 (med **30.19**) | 19.50–19.88 | 5/5 |
| a1_standard | **subnormal** | 17.44–17.81 (med **17.44**) | **44.25–58.13** (med **47.44**) | 19.50 | 5/5 |
| a2_lite | steady | 3.19 (all five) | 14.63–17.44 (med **15.94**) | 9.75–10.31 | 5/5 |
| a2_lite | **subnormal** | 3.38–3.75 (med **3.75**) | 18.56–20.81 (med **19.41**) | 10.12–10.31 | 4/5 |

**Discarded repetition, under Task 4's rule, cited as that section states it** ("discard a
rep whose p50 is >10% above the modal p50 **while** its estimator is <5% above the modal
estimator" — the implementer's own substitution for `is_quotable()`, which flags every
browser rep and so cannot discriminate): `a2_lite subnormal rep 3`, p50 **4.13%** against a
modal 3.75% = **+10.13%** (bar >10%), estimator 10.31% against modal 10.12% = **+1.88%**
(bar <5%). Both clauses met, marginally on the first. **It would not change the verdict**:
including it moves the a2 median p99.9 from 19.41% to 19.50%, and the penalty ratio not at
all to two decimals. Recorded because the rule was applied, not because the number mattered.

### The ratios, and kill criterion 3

Kill criterion 3 now applies to **`SubnormalTail`**, per the ruling, not to `amp-decay`.

| Runtime | Build | Model | steady | subnormal | **penalty** |
|---|---|---|---|---|---|
| native | x86-64-v3, **no FTZ** | A1 Standard | p50 6.42 / p99.9 13.51 | p50 8.93 / p99.9 16.36 | **1.39x** p50, **1.21x** p99.9 |
| native | x86-64-v3, **FTZ/DAZ on** | A1 Standard | p50 6.48 / p99.9 13.65 | p50 6.46 / p99.9 13.60 | **1.00x** p50, **1.00x** p99.9 |
| **Edge (V8)** | **simd128** | A1 Standard | p50 12.94 / p99.9 30.19 | p50 17.44 / p99.9 47.44 | **1.35x** p50, **1.57x** p99.9 |
| native | x86-64-v3, no FTZ | A2 Lite | p50 2.04 / p99.9 7.66 | p50 2.13 / p99.9 9.14 | 1.04x p50, 1.19x p99.9 |
| native | x86-64-v3, FTZ/DAZ on | A2 Lite | p50 2.04 / p99.9 7.55 | p50 2.00 / p99.9 7.51 | 0.98x p50, 0.99x p99.9 |
| **Edge (V8)** | **simd128** | A2 Lite | p50 3.19 / p99.9 15.94 | p50 3.75 / p99.9 19.41 | 1.18x p50, 1.22x p99.9 |
| **Chrome** | simd128 | both | — | — | **PENDING RUN** |
| **Firefox** | simd128 | both | — | — | **PENDING RUN** |

**Kill criterion 3 (>2x penalty on the subnormal mode): NOT FIRED.** The largest penalty
anywhere is **1.57x** (A1 Standard p99.9, Edge/simd128). Every other cell is between 1.00x
and 1.39x (0.98x on one guard-on cell, i.e. at noise). **Proceed to Task 6.**

### What FTZ actually buys, and what it does not

The headline comparison the spike can make is **wasm vs native-without-FTZ**, because
neither side of the ratio has a guard installed (`namir-platform` is out of the crate graph
on purpose). On that comparison, **wasm's subnormal penalty is not categorically worse than
x86's without FTZ** — 1.35x against 1.39x on p50 — so the browser's mandated IEEE-754
subnormal handling is not a qualitatively different beast from an x86 core running with the
guard off. On p99.9 wasm is worse (1.57x against 1.21x), and that difference is the finding.

Task 5 additionally priced the guard, which the brief did not ask for and which changes the
reading. **On native, FTZ/DAZ removes essentially the entire penalty of both modes**:
A1 Standard's subnormal p50 goes 8.93% -> 6.46% against a 6.48% steady baseline (1.00x), A2
Lite's 2.13% -> 2.00% against 2.04% (0.98x), and
its **amp-decay** p50 goes 9.23% -> 6.59% (1.02x). **There is no FTZ on wasm**, so that
saving is not available in a browser at any price — it is not a tuning knob that was left
unturned, it is a mode the platform does not have.

**A correction the coordinator's own ruling did not anticipate, stated plainly.** The ruling
said "the ~42% A1 decaying penalty already recorded natively in Task 2 is **not** a denormal
effect." **Measurement says it mostly is.** The reasoning behind the ruling was sound as far
as it went — the *input* never leaves normal range — but the chain's own state does: the
census finds 47.6% of `amp-decay` blocks carrying subnormal *output* samples down to 1e-45
and MXCSR raising DE on 52.3% of them, and installing FTZ/DAZ collapses the 1.44x p50 cost
to 1.02x. So Task 2's `decaying` figures were measuring a denormal effect after all, through
a signal that happens to produce one as a side effect of its amplitude sweep. The mode is
still the wrong instrument — amplitude and subnormality are confounded in it, and
`SubnormalTail` is the one that holds amplitude fixed — but the relabelling is about
*naming what the signal is*, not about disowning what it measured.

### The Gate 1 consequence — the real finding here, and it is not the ratio

The ratio passes comfortably. The **absolute** number does not, quite:

> **A1 Standard on simd128, under the subnormal-tail signal, reaches p99.9 = 44.25–58.13%
> of the block period, with one of five reps at 58.13% — above Gate 1's ≤50% bar.**

Task 4 certified Gate 1 for A1 Standard at p99.9 **32.44–33.56%** sustained, against ≤50%.
That was measured on a steady signal. Silence after signal — the single most ordinary thing
a guitar amp's input does, between notes and between takes — costs **+17.25 pp** of the
block budget on this model and takes the headroom from ~1.5x to roughly break-even. Kill
criterion 2 (>100%, sustained) is nowhere near firing, and the median rep (47.44%) is still
inside the bar, so **Gate 1's verdict is not retracted**; but it is now conditional on a
signal condition Task 4 did not exercise, and **the honest statement is that A1 Standard's
browser margin under realistic silence is a hairline, not the 1.5x Task 4 recorded**.

This compounds Task 4's carried-forward **UNRESOLVED** item (A1's ~14% cost growth from
20 000 to 100 000 blocks). **These runs shed no light on it**: they are all at 100 000
blocks, so there is no second run length to compare, and no longer run was made. What they
add is that the quantity that would have to stay stationary is now a p99.9 sitting at
44–58% rather than at 29–34%. If the growth turns out to be drifting rather than asymptotic,
this is the configuration where it bites first.

### What Task 6 should do with this

1. **Budget A1 Standard against ~50%, not 33.6%.** The worklet must survive silence, which
   is most of a session.
2. **A2 Lite is unaffected in practice** — 19.41% p99.9 under the same signal, ~2.5x
   headroom.
3. There is **no wasm equivalent of `DenormalGuard`**. If the browser build ever needs the
   17 pp back, the only lever left is inside the DSP (a tiny anti-denormal dither, or
   flushing small state to zero in the stages themselves) — and that is a `crates/` change,
   out of scope here and out of scope for this whole spike.

### Reproducing this section

    cd spikes/s5-wasm-web-audio
    ./run-matrix.sh
    cargo run --release --bin native_bench            # census + all 60 native timing reps
    python web/serve.py                               # from the spike root, in another shell
    # census, in-browser (signal: 0 steady, 1 amp-decay, 2 subnormal-tail):
    msedge --headless=new --disable-gpu --no-sandbox \
      "http://127.0.0.1:8080/web/bench.html?auto=1&census=1&measured=20000&wasm=simd128&model=a1_standard&signal=1"
    # timing:
    msedge --headless=new --disable-gpu --no-sandbox \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=100000&wasm=simd128&model=a1_standard&signal=2"

Node runs the same census and bench without a browser (V8, but a nanosecond clock and no
renderer — informational only, never quoted as a browser figure):

    S5_WASM=web/build/simd128.wasm node web/parity-node.mjs --bench --census --measured 20000 --signal 1

### PENDING RUN — Chrome and Firefox

Neither is installed on this machine and nothing was installed; **no Chrome or Firefox
number appears above.** Firefox especially matters here: SpiderMonkey is a different wasm
compiler, and subnormal handling on the wasm32 target is a codegen property. Exact commands,
on a machine that has them (server started from the spike root, artefacts built first,
`crossOriginIsolated: true` confirmed on the page before recording anything):

    "C:\Program Files\Google\Chrome\Application\chrome.exe" \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=100000&wasm=simd128&model=a1_standard&signal=0"
    #   ... &signal=2   (subnormal-tail; the pair above is the penalty ratio)
    #   ... &model=a2_lite, both signals
    #   ... &census=1&measured=20000&signal=1   (the in-browser subnormal witness)
    "C:\Program Files\Mozilla Firefox\firefox.exe" \
      "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=100000&wasm=simd128&model=a1_standard&signal=2"

### Task 5 verdict

**Kill criterion 3 does NOT fire (max 1.57x, bar 2x). Proceed to Task 6**, carrying:

1. The sub-experiment as briefed was measuring amplitude decay, not denormals; it now
   measures both, separately, and proves which is which rather than assuming.
2. `SubnormalTail` is proved to run subnormal arithmetic in **85.4%** of measured blocks by
   the CPU's own denormal-operand flag, with an FTZ/DAZ control that collapses every count
   to zero. In the browser the direct witness is the output-side census (47.6% of
   `amp-decay` blocks, reproducing native to 0.02%); the internal half is a stated
   inference, not a browser measurement.
3. **A1 Standard's Gate 1 margin under silence is a hairline** — p99.9 44.25–58.13% against
   a ≤50% bar, one rep over. Not a retraction of Gate 1; a condition on it.
4. `DenormalGuard` would remove the whole cost natively (1.39x -> 1.00x). Wasm has no
   equivalent, so this cost is structural in a browser.
5. Task 4's 20 000-vs-100 000-block growth question is **still UNRESOLVED** — nothing here
   bears on it, all six runs are at one length.

## Task 6 — AudioWorklet underrun gate, 2026-09-05

**Files added:** `web/worklet.html`, `web/namir-processor.js`. **Modified:** `RESULTS.md`.
Nothing under `crates/`, `docs/`, `.github/` or `xtask/` was touched.

**No figure in this section is certified.** As in Tasks 2–5, and doubly so here: a browser
render thread cannot be core-pinned, carries no `DenormalGuard`, and runs at whatever
priority Chromium gives it. Everything below is informational.

### The audio backend is real hardware — and that was checked, not assumed

A zero-underrun result is worthless if the sink is a null/dummy device: such a sink is
paced by a software timer and has no deadline in it. Three independent findings, most
direct first.

1. **Windows sees our stream on the endpoint, at the amplitude we sent.** This is the
   direct proof and it leads. A CoreAudio probe (`IMMDeviceEnumerator` ->
   `IAudioMeterInformation` / `IAudioSessionManager2` on the default render endpoint,
   `coreaudio_probe.ps1`) sampled once a second across a headless Edge run shows one
   **additional active session appear on the endpoint for exactly the duration of the run**
   and the endpoint meter read **`peak = 0.0100000`** throughout it, then return to zero.
   `0.01` is the `gain` query parameter that run was launched with: the chain's own output
   is arriving at the WASAPI endpoint at exactly the amplitude the page chose. A positive
   control a few seconds earlier — `System.Media.SoundPlayer` on
   `C:\Windows\Media\Alarm01.wav` — moves the same meter to ~0.2-0.3, so the probe is
   known to work and its zero readings are real zeroes. **Evidence:
   `coreaudio_task6_rerun.txt`** — see the provenance note below; that file is a *later
   re-run* of this check, because the original probe's console output was not preserved.
   The two runs agree on the transition that matters: in both, the session count goes
   **3 -> 4 with one active** for the duration of our Edge run. (The re-run's earlier
   `2 -> 3` step is the `SoundPlayer` positive control opening its own session, not ours.)
   Session totals are still only ever "one additional active session" as a claim, since the
   count includes whatever else the machine happens to have open — but nothing here
   diverged: one new active session and `peak = 0.01` reproduced exactly.
2. **The endpoint exists and is the one being opened.** The machine's default render
   endpoint is a **PreSonus AudioBox 22VSL** (USB interface), registry mix format
   `WAVE_FORMAT_EXTENSIBLE, 2 ch, 48 000 Hz, 32-bit float` — read from
   `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\...\Properties`,
   value `{f19f064d-...},0`, offset 8. `AudioContext.outputLatency` reads 0 ms before the
   graph starts and **40 ms** once it is running, i.e. the context binds to a device
   stream rather than reporting a fixed nominal figure. Evidence: the `backend:` line at
   the head of every run in `edge_task6.txt`.
3. **The device clock is not the system clock.** Over the 300 s run the graph clock ends
   **-8.1 ms** against `performance.now()`, i.e. **~ -27 ppm** taken endpoint-to-endpoint. A
   timer-paced null sink is driven by the *same* clock the page reads and cannot drift at
   all; a crystal in a USB interface can and does. **Ranked last, and deliberately: the
   witness is noisy, and an earlier draft over-claimed on it.** That draft said the drift
   was "smoothly and monotonically" negative. It is not: the preserved per-10-second series
   in `edge_task6.txt` reverses direction 10 times, by up to +1.4 ms. What the series does
   show is a clear linear trend under about 1 ms of jitter — a least-squares fit over its 31
   samples gives **-19.3 ppm with a residual sd of 0.82 ms, a slope 11.7x its own standard
   error**. So the conclusion holds comfortably (a null sink's slope would be 0), the
   endpoint-to-endpoint -27 ppm is the cruder of the two estimates and is left as published,
   and the correct description of the shape is "a clear linear trend under ~1 ms jitter",
   not "smooth and monotone". Corroboration for finding 1, not a replacement for it.

So the numbers below are measured against a hardware deadline. **Headless is not the
problem here; the earlier `--disable-gpu` attempts that logged "The AudioContext
encountered an error from the audio device" were a page bug (below), not a dead sink.**

### Two deviations from the brief, both forced

**1. `postMessage` of a compiled `WebAssembly.Module` into an `AudioWorkletGlobalScope` is
silently dropped in Edge 152.** The brief compiles in the page and posts the `Module`. On
this runtime that message never arrives: no `DataCloneError` on the sending side, no
`onmessage` on the receiving side, and a processor that stays un-ready forever while
`process()` keeps being called — which reads exactly like a dead audio backend and cost
most of this task's debugging. Isolated with a three-line echo processor (a plain object
round-trips fine, and `{kind:"boot"}` posted from the processor constructor always
arrives) and fixed by posting the **bytes** and calling `new WebAssembly.Module(wasm)`
inside `setup`. Synchronous compilation is legal off the main thread and happens once,
before the first render callback is answered.

**2. The processor generates its input; it does not read `inputs[0][0]`.** There is no
microphone in a headless run, so the brief's page would have driven the chain with silence
— the cheap half of the workload — and Task 5's whole finding is that *silence is the
expensive regime*. The processor therefore runs `harness.rs`'s own xorshift32 generator
(same seed, `0x2545F491`) in two regimes and counts them separately: **steady** full-scale
noise for the first half of the run, then **subnormal-tail** — 16 signal blocks of every
512, the rest exact silence — which is `Signal::SubnormalTail` reproduced in JS. Output is
attenuated by `outGain` (default 1e-4) on the way into `outputs`; the DSP is untouched.

### Two underrun witnesses, because one of them might have been blind

- **`currentFrame` gap** (the brief's): the processor's own block count against the graph
  clock. It was not obvious this could ever fire — an engine that renders every quantum
  late rather than dropping quanta would never trip it. It **does** fire on this engine:
  four of the runs below caught a gap, always of exactly 3–4 quanta, i.e. one 480-frame
  device callback. The witness is live, not decorative.
- **Graph clock vs wall clock**, sampled on the main thread. If the render thread cannot
  keep up with a device that keeps consuming, `ctx.currentTime` falls behind
  `performance.now()` and the lag accumulates. Reported per run as max lag.

`AudioContext.renderCapacity` was feature-detected and is **not exposed** by Edge 152, with
or without `--enable-blink-features=AudioContextRenderCapacity`. The page keeps the probe;
it costs nothing and would have been the only in-browser view of render load.

### Gate 2 — worklet scheduling, 2026-09-05

Runtime: Microsoft Edge **152.0.4191.62**, `--headless=new --no-sandbox
--autoplay-policy=no-user-gesture-required`. Machine: AMD Ryzen 9 5950X / Windows 11 Pro
26200, default endpoint AudioBox 22VSL @ 48 000 Hz, `baseLatency` 10.00 ms,
`outputLatency` 40.00 ms (32.00 ms in one run). Each run is 60 s = 22 500 blocks, half
steady and half subnormal-tail, run alone and sequentially. **This page does not need
cross-origin isolation** (no `SharedArrayBuffer`, no clock); `serve.py` sends COOP/COEP
anyway, and the demo path would not ship them.

| Browser | Build | Model | Run | Blocks | Underruns steady | Underruns tail | Missed quanta | max clock lag ms |
|---|---|---|---|---|---|---|---|---|
| Edge 152 headless | simd128 | a1_standard | 1 | 22 500 | 0 | 0 | 0 | 2.0 |
| Edge 152 headless | simd128 | a1_standard | 2 | 22 500 | 0 | 0 | 0 | 1.7 |
| Edge 152 headless | simd128 | a1_standard | 3 (CONTAMINATED) | 22 500 | 1 | 0 | 4 | 3.1 |
| Edge 152 headless | simd128 | a1_standard | 3b | 22 500 | 0 | 0 | 0 | 1.9 |
| Edge 152 headless | simd128 | a1_standard | growth (300 s) | 112 500 | 0 | 0 | 0 | 0.7 |
| Edge 152 headless | simd128 | a2_lite | 1 | 22 500 | 1 | 0 | 4 | 0.1 |
| Edge 152 headless | simd128 | a2_lite | 2 | 22 500 | 0 | 0 | 0 | 0.2 |
| Edge 152 headless | simd128 | a2_lite | 3 | 22 500 | 0 | 0 | 0 | 0.4 |
| Edge 152 headless | scalar | a1_standard | 1 | 22 500 | 0 | 0 | 0 | 2.3 |
| Edge 152 headless | scalar | a1_standard | 2 | 22 500 | 1 | 0 | 3 | 3.3 |
| Edge 152 headless | scalar | a1_standard | 3 | 22 500 | 1 | 0 | 4 | 3.8 |
| Chrome | simd128 | a1_standard | — | — | **PENDING RUN** | **PENDING RUN** | — | — |
| Firefox | simd128 | a1_standard | — | — | **PENDING RUN** | **PENDING RUN** | — | — |

**Raw logs.** Every run in this table is preserved verbatim in **`edge_task6.txt`** — the
beacon transcript exactly as the dev server logged it, one section per run, including the
contaminated one and the 300 s growth run's full per-10-second `clockLag` series. The fix
round's 36 attribution runs are in **`edge_task6_attribution.txt`**, one section per rep.
The CoreAudio endpoint probe is `coreaudio_probe.ps1` with its output in
`coreaudio_task6_rerun.txt`; that file is a *later re-run* of the check rather than the
original console capture, and says so in its own header. Chrome and Firefox are PENDING RUN
rows above: no figure exists for them because neither browser is installed here.

Run 3 of simd128/a1_standard is **contaminated and is reported, not used**: a second Edge
process was launched over it by mistake, which is exactly the contamination AGENTS.md's
benchmark section warns about on this machine. Run 3b is its clean replacement. It is kept
in the table because it is also the first evidence that the `currentFrame` witness fires at
all.

**Gate 2 (zero underruns over 60 s): PASS in steady state, with one honest exception.**
*(Settled by the fix round below: that exception is a Chromium/WASAPI stream-start
artefact, not this chain, so the verdict is a plain **PASS**. Note also that `outputLatency`
was 40.00 ms in every run but scalar rep 2, which negotiated 32.00 ms -- the rows are not
all measured against the same effective deadline.)*

- **Every underrun in the whole matrix — 4 of 11 runs, exactly one event each, 3–4 quanta —
  happened inside the first 10-second window**, and never again for the rest of that run or
  of the 300-second run. They are a start-up transient, not a deadline the chain cannot
  hold. The likely mechanism is in this spike's own code: `wasm_abi.rs`'s
  `HANDOVER_GUARD_BLOCK` fires `assert_resources_loaded()` on block 256 (682 ms in), which
  drains the telemetry ring into a 2 KB stack buffer on the audio thread — a one-shot cost
  that lands squarely in that window. Not proved; it is the first thing to check if this
  matters. **-- RETRACTED. Tested and disproved; see "Task 6 fix round" below. The
  event is neither the guard nor the chain: it occurs at the same rate with the guard
  moved 10x later and with the chain not running at all.**
- **Zero underruns in the subnormal-tail half of every single run**, 123 750 tail blocks in
  total. Task 5's warning — A1 Standard at p99.9 44.25–58.13% of the block period under
  silence — did **not** translate into a missed callback. Roughly a 2x margin is enough
  here, which is the useful part of this result.
- **The strict reading of the criterion — every run zero — is not met**: 7 of 10 clean runs
  are zero, 3 carry one start-up event. Taken literally the gate is red. Taken as "does the
  chain hold the deadline in steady state", it is green with margin, on all three cells
  measured (simd128/a1_standard, simd128/a2_lite, scalar/a1_standard).
- The **scalar** build also passes, which was not expected — Task 4 measured it ~3.3x slower
  than simd128 on p50, and it still holds a 128-frame quantum. The 50% budget Gate 1 argued
  over is not the binding constraint at this buffer size; the 40 ms device buffer absorbs a
  great deal of per-quantum jitter.

### The 20 000-vs-100 000-block growth question — partial evidence, not a resolution

Task 4 left this **UNRESOLVED**: A1's cost grew ~14% between 20 000 and 100 000 measured
blocks with a flat estimator. The 300-second run is 112 500 blocks, 5x the 60-second gate
and above Task 4's upper point, and it recorded **zero underruns and a max clock lag of
0.7 ms — the *lowest* of any run in the matrix**, with the per-10-second lag series showing
only the smooth -27 ppm hardware drift and no accumulating backlog.

What that does and does not establish, stated carefully: this is a **deadline detector, not
a timer**. There is no clock in an `AudioWorkletGlobalScope`, so no p99.9 over session time
was measured here and none is claimed. It rules out growth large enough to consume A1
simd128's remaining headroom within 112 500 blocks; it cannot distinguish "no growth" from
"14% growth that then flattens", because both stay under the deadline. **The question
remains open**, and the instrument that would close it is still a timed run — Task 4's
`bench.html` at a much larger `measured`, not this page.

### Reproducing this section

    cd spikes/s5-wasm-web-audio
    ./run-matrix.sh                                   # builds web/build/{scalar,simd128}.wasm
    python web/serve.py                               # from the spike root, in another shell
    msedge --headless=new --no-sandbox --autoplay-policy=no-user-gesture-required \
      "http://127.0.0.1:8080/web/worklet.html?auto=1&secs=60&split=0.5&wasm=simd128&model=a1_standard"
    #   &secs=300                 the growth run
    #   &split=1                  steady only;  &split=0  subnormal-tail only
    #   &gain=0.01                louder output, for the CoreAudio meter check
    #   &wasm=scalar &model=a2_lite   the other cells

Under `?auto=1` each line is beaconed to `/__s5?...`, which the dev server 404s and logs;
that log is the transcript, and `edge_task6.txt` / `edge_task6_attribution.txt` are exactly
that log, URL-decoded, for every run reported here.

### PENDING RUN — Chrome and Firefox

Neither is installed on this machine and nothing was installed, so **no Chrome or Firefox
underrun count appears above**. Firefox matters most: SpiderMonkey is a different wasm
compiler *and* a different audio backend (cubeb), and both halves of this gate depend on
which. Exact commands, on a machine that has them, server started from the spike root and
artefacts built first:

    "C:\Program Files\Google\Chrome\Application\chrome.exe" \
      "http://127.0.0.1:8080/web/worklet.html?auto=1&secs=60&split=0.5&wasm=simd128&model=a1_standard"
    "C:\Program Files\Mozilla Firefox\firefox.exe" \
      "http://127.0.0.1:8080/web/worklet.html?auto=1&secs=60&split=0.5&wasm=simd128&model=a1_standard"
    #   three reps each, plus &wasm=scalar, &model=a2_lite, and &secs=300

Also **PENDING RUN**: any run at a smaller device buffer. Everything above sits behind a
40 ms `outputLatency`, which is a generous cushion; a 10 ms or 5 ms endpoint would be the
real test of the tail regime, and neither Web Audio nor Chromium exposes a way to ask for
one from the page.

### Task 6 verdict

**Gate 2 passes in steady state on a real audio device, on all three cells measured, with
zero underruns in 123 750 subnormal-tail blocks — the regime Task 5 flagged as the risk.**
The literal "every run zero over 60 s" bar is missed by three runs, each by a single
start-up event in the first 10 seconds, plausibly caused by this spike's own one-shot
`assert_resources_loaded()` on block 256. **That attribution was tested in the fix round
below and is retracted: the event is the runtime's stream start, not this spike's code, and
the Gate 2 verdict is a plain PASS.** Carried forward:

1. The tail regime cost Task 5 measured (44.25–58.13% of the block period) does **not**
   produce underruns at a 40 ms device buffer. It has not been tested at a smaller one.
2. Task 4's growth question is **still UNRESOLVED**; 112 500 blocks produced no scheduling
   consequence, which bounds the effect without measuring it.
3. This page proves *scheduling*, not *fidelity*. `process()`'s `fault_count() == 0` and
   `assert_resources_loaded()` guards held in every run (no `onprocessorerror` fired), so
   the chain really was loaded and computing — but nothing here re-runs Task 3's parity
   check, and a worklet cannot.

## Task 6 fix round — the start-up underrun, attributed by experiment, 2026-09-05

Task 6 above guessed that the first-second underrun came from this spike's own
`HANDOVER_GUARD_BLOCK` — `wasm_abi.rs` firing `assert_resources_loaded()` once on block
256, which drains the telemetry ring into a 2 KB stack buffer on the audio thread, 682 ms
in. **That guess is wrong, and the experiment that disproves it is below.** The bullet
above is left as written and marked retracted, per this project's practice of keeping
corrected findings on the record rather than tidying them away.

### The experiment

Three arms, **12 reps each, 30 s per rep, steady signal only** (`&split=1`), scalar build
on `a1_standard` — scalar because it had the highest event rate in the Gate 2 matrix, and
the phenomenon appeared in all three cells. Runs are sequential and alone. The page now
beacons **one line per second** rather than every ten, so the event can be located to the
second; the guard fires at 682 ms in arm A and at 6.83 s in arm B.

| Arm | `HANDOVER_GUARD_BLOCK` | chain driven? | runs with an underrun | when |
|---|---|---|---|---|
| A — baseline | 256 (682 ms) | yes, from block 0 | **4 / 12** | all at second 1 |
| B — guard moved 10x later | 2560 (6.83 s) | yes, from block 0 | **3 / 12** | all at second 1 |
| C — chain not driven for 2 s | 256 | **no** — `preroll=750`, silence, `process()` never called | **5 / 12** | all at second 1 |

Arm B is a rebuild of the same source with the one constant changed (`web/build/scalar-guard2560.wasm`,
`sed`-edit `src/wasm_abi.rs`, `cargo build --release --target wasm32-unknown-unknown --lib`,
copy, revert, rebuild — the committed constant is 256). Arm C is the new `&preroll=N` query
parameter: the processor renders N blocks of silence *without calling into the chain at
all*, so for the first two seconds nothing of Namir runs.

All 36 runs are preserved verbatim in **`edge_task6_attribution.txt`**, one section per rep,
so the per-arm counts and the second at which each event lands are re-derivable from the log
rather than only stated here.

### What it establishes

1. **It is not the guard.** Moving `assert_resources_loaded()` from 682 ms to 6.83 s did
   not move the event: arm B still fires it at second 1 and never at second 7. If the guard
   were the cause the event would have tracked it. 4/12 vs 3/12 is **no detectable
   difference** — with n=12 an arm cannot exclude a moderate effect, and it is the *timing*
   that carries the argument, not the rate: the event never once appeared at second 7.
2. **It is not the chain.** Arm C does not run a single block of Namir DSP for the first
   two seconds — the worklet emits silence and returns — and it produces the event at the
   *same* rate and the *same* second, 5/12. Whatever drops the callback does so while the
   render thread's only work is `fill(0)` and a copy.
3. **So it is Chromium's or WASAPI's own audio-stream start-up.** Every event is a single
   3–4 quantum gap, i.e. one 480-frame device callback, in the first second of the stream's
   life, at an overall incidence of **12 of 36 runs (33%)** across the three arms — a rate
   indistinguishable between them. That is a property of the runtime, not of the code under
   test. What it is *specifically* — the first device callback after `AudioContext` start,
   V8 tiering the worklet's own JS, the audio service's first-buffer path — is not resolved
   here and would need Chromium-internal instrumentation this spike cannot reach.

### The consequence for the Gate 2 verdict

**Gate 2: PASS.** The gate asks whether the six-stage chain, compiled to wasm, holds a Web
Audio deadline for 60 seconds. It does: zero underruns in every steady-state second of
every run, in both regimes, across three cells, over 60 s and over 300 s — and the only
events in the whole matrix are proven to occur equally when the chain is not running at
all.

The literal "every run zero underruns over 60 s" reading is still not met, and that is
worth stating plainly: **about a third of the time, starting an `AudioContext` in Edge 152
on this machine costs one dropped device callback in the first second, whatever the graph
is doing.** A demo would hear a click at start-up and would have to live with it or hide it
(don't connect the node until the stream is warm, ramp in a gain). It is a real property of
the platform. It is not a property of Namir's DSP, and Gate 2 is not the gate that should
fail for it.

### Finding, in its own right: `WebAssembly.Module` over `postMessage` into an AudioWorklet is silently dropped

Recorded here as a platform finding rather than only as a debugging note, because it cost
most of a task and the failure mode is maximally misleading.

The widely-published AudioWorklet pattern is: `WebAssembly.compile()` in the page, then
`node.port.postMessage({module})` and `new WebAssembly.Instance(module)` in the processor.
**On Edge 152 that message never arrives.** There is no `DataCloneError` on the sending
side, no exception anywhere, and no `onmessage` in the worklet — the processor simply stays
un-ready while `process()` keeps being called on schedule. The observable result is a graph
that renders silence forever, which is indistinguishable from a dead or dummy audio
backend, and sends you looking in exactly the wrong place.

Isolated with a three-line echo processor: a plain object round-trips fine, a message
posted *from* the processor constructor always arrives, and the identical message carrying
a `Module` never does. The fix is to post the **bytes** and call `new
WebAssembly.Module(wasm)` inside the processor — synchronous compilation is legal off the
main thread and happens once, before the first render callback is answered.
`web/namir-processor.js` also posts a `{kind:"boot"}` from its constructor and keeps it
permanently: seeing `boot` but never `ready` localises the fault to inbound message
delivery rather than to the DSP, which is the distinction that took the longest to make.

### The deadline is not constant across rows of the Gate 2 table

`outputLatency` was **40.00 ms** in most runs and **32.00 ms** in one (scalar rep 2). The
device buffer Chromium negotiates is not fixed run to run, so rows of that table are not
all measured against the same effective deadline. Nothing in the results turns on it — the
steady-state count is zero either way — but a reader comparing rows should know the
denominator moved.

### Reproducing the fix round

    cd spikes/s5-wasm-web-audio
    python web/serve.py                                  # from the spike root
    # arm A (baseline, the committed build):
    msedge --headless=new --no-sandbox --autoplay-policy=no-user-gesture-required \
      "http://127.0.0.1:8080/web/worklet.html?auto=1&secs=30&split=1&wasm=scalar&model=a1_standard"
    # arm C (chain not driven for the first 2 s):
    #   ...&preroll=750
    # arm B needs a rebuild with the constant moved:
    sed -i 's/HANDOVER_GUARD_BLOCK: u32 = 256/HANDOVER_GUARD_BLOCK: u32 = 2560/' src/wasm_abi.rs
    cargo build --release --target wasm32-unknown-unknown --lib
    cp target/wasm32-unknown-unknown/release/s5_wasm_web_audio.wasm web/build/scalar-guard2560.wasm
    git checkout src/wasm_abi.rs && ./run-matrix.sh    # put the tree and web/build/ back
    #   ...&wasm=scalar-guard2560
    rm web/build/scalar-guard2560.wasm                 # deliberately NOT left behind: it is an
    #   experiment artefact and would be indistinguishable from a gate artefact to anyone
    #   re-running run-matrix.sh, which builds only scalar.wasm and simd128.wasm.

Twelve reps per arm is the minimum that separates these rates: at the ~33% incidence
observed, six reps per arm would have produced 3/6 vs 0/6 by chance alone (Fisher one-sided
p = 0.09), which is exactly what the first half of arm B looked like before the second half
was run. Six reps would have "confirmed" the wrong conclusion.

---

## Task 7 — Gate 3, round-trip latency on Windows, 2026-09-05

**Gate 3 has no pass/fail.** The soft reference is that ~30 ms round-trip is playable. A bad
number does not kill S-5; it means a demo ships file-playback-first, which was already
decided. What follows is what was measured.

**Read the browser line first.** This is **Microsoft Edge 152 (Chromium)**, not Chrome.
Chrome is not installed on this machine and nothing was installed to run this. Edge is
Chromium and drives the same Chromium audio service over the same Windows WASAPI path, so
these figures are informative about Chrome — but they are not a Chrome measurement, and this
gate exists precisely because Firefox figures were once over-generalised to all browsers. Do
not repeat that with Edge figures.

**No figure here is certified.** These are dev-machine readings under a browser that cannot
be core-pinned.

### The measurement page

`web/latency.html` + `web/capture-processor.js`.

The brief specified a `ScriptProcessorNode(4096)` and accepted that its buffering inflates
the absolute figure by an unknown constant, on the grounds that the constant is the same in
every condition. That trade is not available for this gate: Gate 3's entire value is an
**absolute** number, so it may not be measured through a node that corrupts absolute
numbers. Task 6 established that AudioWorklets run reliably here, so the capture path is an
`AudioWorkletNode`.

The upgrade buys more than "less buffering". A `ScriptProcessorNode` hands you a buffer with
no reliable statement of *when*, so the brief's page had to rebuild the timeline by
concatenating callbacks and counting samples from an assumed origin. Inside an
`AudioWorkletGlobalScope`, `currentFrame` is the absolute frame index of the quantum being
rendered, on the same clock as `AudioContext.currentTime` — the clock the click was
scheduled against. The round trip becomes the subtraction of two frame numbers on one clock:
no concatenation, no assumed origin, no accumulation.

Two further departures from the brief, both for correctness rather than taste:

- **Onset, not peak.** The brief takes the largest captured sample. Latency is a property of
  the edge, and the largest sample of a burst can be hundreds of microseconds later — or,
  through an AC-coupled line input that differentiates a rectangular pulse, an unpredictable
  amount later. The processor reports the first sample crossing a threshold derived from a
  *measured* noise floor, because "well above noise, not clipping" is set by a human turning
  a physical knob and cannot be assumed.
- **A negative control (`&control=1`).** Arms the detector and emits no click. This turned
  out to be load-bearing; see below.

### What was measured without a cable

Three reps of each cell, Edge 152 `--headless=new --no-sandbox
--autoplay-policy=no-user-gesture-required --use-fake-ui-for-media-stream`; AudioBox 22VSL
at 48 kHz, which is the only endpoint Chromium enumerates on this machine.

| Browser flags | `latencyHint` | baseLatency | outputLatency | API output total | input latency (`getSettings().latency`) |
|---|---|---|---|---|---|
| none | interactive | 10.000 ms | 42.000 ms | 52.000 ms | 10.000 ms |
| none | balanced | 10.000 ms | 42.000 ms | 52.000 ms | 10.000 ms |
| none | playback | 20.000 ms | 52.000 ms | 72.000 ms | 10.000 ms |
| `--enable-exclusive-audio` | interactive | 5.333 ms | 128.000 ms | **133.333 ms** | 10.000 ms |
| `--enable-exclusive-audio` | balanced | 5.333 ms | 128.000 ms | **133.333 ms** | 10.000 ms |
| `--enable-exclusive-audio` | playback | 21.333 ms | 128.000 ms | **149.333 ms** | 10.000 ms |

Every cell was identical across its three reps, and `outputLatency` sampled ten times within
each run never moved. Raw transcript: `edge_task7.txt`.

Three findings in that table.

1. **`--enable-exclusive-audio` makes it much worse, not better** — spec §6 listed it as the
   unmeasured low-latency condition and the expectation was the opposite. It does exactly
   what it says to the render quantum: `baseLatency` falls from 10 ms (480 frames) to
   5.333 ms (256 frames). But the device buffer balloons from 42 ms to **128 ms**, so the
   API output total goes from 52 ms to 133 ms. Whatever Chromium negotiates in exclusive
   mode on this USB interface, it is not a small buffer. On this machine the flag is a 2.6x
   latency regression and should not be part of a demo story.
2. **`interactive` and `balanced` are the same thing here.** Only `playback` moves, and it
   adds 20 ms.
3. **Opening a microphone does not change the output buffer.** Task 6 recorded
   `outputLatency` 40 ms with an output-only graph; this page reads 42 ms with a capture
   stream open, and the obvious hypothesis was that `getUserMedia` renegotiates the device
   period. It does not: `&noinput=1` (no capture stream at all, an oscillator into a muted
   gain to keep the graph running) reads **42.000 ms** in all three reps. The 40-vs-42
   difference is between-session device state, not the input stream.

**The input side.** There is no standard input-latency accessor, which is why spec §6 wants
the loopback figure reported beside the API ones. What Chromium does expose is
`MediaStreamTrack.getSettings().latency`, and it reports **0.01 s = 10 ms** in every cell —
including both exclusive-audio cells, where the output side changed by 86 ms.
`getCapabilities().latency` is `{min: 0.01, max: 0.01}`, i.e. Chromium declares the input
latency to be a fixed constant the page cannot influence. Full settings as reported:

    {"autoGainControl":false,"channelCount":2,"deviceId":"default","echoCancellation":false,
     "latency":0.01,"noiseSuppression":false,"sampleRate":48000,"sampleSize":16,
     "voiceIsolation":false}

Note `channelCount: 2` despite `channelCount: 1` being requested — the constraint was not
honoured, which is why the capture processor reads channel 0 rather than assuming mono.

So the **API-reported** total, input plus output, is **62 ms** in the best available
configuration (10 input + 10 base + 42 output, `latencyHint: interactive`, no flags). That is
already twice the ~30 ms soft reference, and it is a lower bound: it is what the browser
admits to, before any part of the path it does not account for.

### The loopback half: PENDING RUN, and demonstrably so

There is no loopback cable on this machine, and this is established rather than assumed.

The AudioBox 22VSL line input is open and not digitally silent (noise-floor rms 0.00025 to
0.00246 across runs, i.e. a live preamp). So "no capture" alone would have been weak
evidence. The control settles it:

| Condition | windows | detections above threshold | peak range |
|---|---|---|---|
| control (armed, **no click emitted**) | 70 | 2 | 0.00085 – 0.0308 |
| click, 64 frames @ 0.5 | 10 | 0 | 0.00106 – 0.00652 |
| click, 480 frames @ 1.0 (7.5x longer, 2x louder) | 20 | 0 | 0.00080 – 0.00397 |

Click windows are indistinguishable from windows in which **no click was emitted**, and if
anything the control has the larger excursions. Emitting a 10 ms full-scale burst instead of
a 1.3 ms half-scale one changed nothing. There is no path from the output to the input.

The two control detections are the more useful half of that table. One reported **−1.06 ms**,
which is physically impossible, and the other 448.54 ms. Both are ambient noise on an open
line input landing inside a 700 ms arming window. Had the run been done without a control, a
run that caught one of these and nothing else would have reported a confident round-trip
figure that was pure room noise. Two detections in seventy control windows is a **2.9%**
false-positive rate per window. (The operator bars below were calibrated on the first fifty
windows, i.e. 4%; the twenty added afterwards to verify the count display were both clean, so
the bars are if anything slightly conservative. Neither number is known to better than about
a factor of three — two events give a 95% CI of roughly 0.5%–10%.)

The page now labels control detections `SPURIOUS`, prints a detection count at the end of a
control run, flags any result below 0 ms or above 200 ms as `IMPLAUSIBLE`, and excludes both
from the median. The original mislabelled transcript is kept in `edge_task7.txt` rather than
re-run away — it is the evidence that the detector produces false positives.

**Two conditions from the brief could not be run at all, for the same reason.** Chromium
enumerates exactly one input and one output device here, both the AudioBox (plus its
`default`/`communications` aliases). The Realtek onboard endpoints are `NOTPRESENT` or
`UNPLUGGED` in the Windows endpoint registry and Chromium does not offer them, so the brief's
"onboard/consumer output" condition has no device to run on. The same enumeration rules out
the one software-loopback route that would not have needed a cable: Realtek "Mixage stéréo"
(Stereo Mix) exists in the registry but is not exposed to Chromium, so there is no
`setSinkId` + Stereo Mix pairing to measure. Nothing was installed to create one, per the
spike's constraints.

### Gate 3 result

| Device | Browser | Config | Median RTT ms | baseLatency ms | outputLatency ms | input latency ms | Unaccounted ms |
|---|---|---|---|---|---|---|---|
| AudioBox 22VSL | Edge 152 | interactive, default | **PENDING RUN** (no cable) | 10.000 | 42.000 | 10.000 | — |
| AudioBox 22VSL | Edge 152 | balanced, default | **PENDING RUN** (no cable) | 10.000 | 42.000 | 10.000 | — |
| AudioBox 22VSL | Edge 152 | playback, default | **PENDING RUN** (no cable) | 20.000 | 52.000 | 10.000 | — |
| AudioBox 22VSL | Edge 152 | interactive, `--enable-exclusive-audio` | **PENDING RUN** (no cable) | 5.333 | 128.000 | 10.000 | — |
| onboard/consumer output | Edge 152 | any | **NOT RUNNABLE** — no such endpoint is exposed to Chromium on this machine | — | — | — | — |
| any | Chrome | any | **PENDING RUN** — Chrome is not installed | — | — | — | — |
| any | Firefox | any | **PENDING RUN** — Firefox is not installed; this is the browser the ~70–100 ms cubeb bugs are actually about | — | — | — | — |

**What can be said now:** on this machine, in Chromium, the browser's own accounting for a
guitar-shaped signal path is **62 ms** at best (`interactive`, no flags), and
`--enable-exclusive-audio` raises it to 143 ms rather than lowering it. 62 ms is roughly twice
the ~30 ms soft reference *before* measuring anything the API does not account for, and a
physical loopback can only be larger than the API figure, never smaller. That is the honest
reading available without the cable, and it points the same way the file-playback-first demo
decision already did.

**What cannot be said:** the absolute round trip, and therefore the size of the gap between it
and the 62 ms the API admits to. That gap is the novel number this gate was created to
produce, and it is still missing.

### PENDING RUN — how to run the loopback half

Written for someone who has not read this spike. It needs one audio cable and about ten
minutes.

**What you need.** The PreSonus AudioBox 22VSL that is already connected, and one cable with
a 1/4-inch TS or TRS jack on **both** ends (a standard instrument/guitar lead is fine).

**1. Cable it.** On the back of the AudioBox, take **LINE OUTPUT 1 (left)**. On the front,
plug the other end into **INPUT 1**. You are connecting the interface's own output back into
its own input. Nothing else needs to move.

**2. Set the knobs.** On the front panel:

- Set **INPUT 1's gain knob** to about 9 o'clock — low. It is a line-level signal going into
  a preamp input, so it needs very little gain, and too much will clip.
- Set the **MIXER** knob fully to **PLAYBACK** (away from INPUT). This stops the interface
  monitoring its own input back to the output, which would otherwise create a feedback loop.
- Set the **MAIN** output knob to about 12 o'clock.
- Leave **48V phantom power OFF**. It is not needed and a line output does not want it.

**3. Start the server.** In a terminal, from the repository root:

        cd spikes/s5-wasm-web-audio
        python web/serve.py

Leave it running. It prints a URL for a different page; ignore that.

**4. Run the control first.** This is not optional — see the false-positive table above. Open
a browser (Edge is at `C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe`) and go
to:

        http://127.0.0.1:8080/web/latency.html?control=1&n=20

Press **Measure** and grant microphone access when asked. Wait about 20 seconds.

The page prints a summary line at the end: `control: N detection(s) in 20 windows`.

**Judge it against the measured false-positive rate, not against zero.** Two of the seventy
control windows recorded above tripped the detector on ambient noise alone — about 3–4%, on a
rig with nothing wrong with it. Over twenty windows that makes **one `SPURIOUS` line roughly
as likely as none** (a better-than-even chance of at least one at the 4% figure the bars below
are set from, and still ~44% at 2.9%). So:

- **0 or 1 `SPURIOUS` — expected. Proceed.** Do not go looking for a fault; there probably
  isn't one.
- **2 — borderline.** Run the control again. Two runs in a row at 2 or more is a real signal;
  a single one is not (2-or-more happens about 19% of the time on a good rig).
- **3 or more — fix the room.** Something is making noise, or something is plugged into
  INPUT 2. This is about a 4% event on a good rig, so it is worth acting on.

Also look at the peaks, which discriminate better than the count. A `SPURIOUS` line whose
peak sits just over the threshold (0.02–0.03, as both of the observed ones did) is a noise
excursion. A peak well above that is a real sound source in the room, and one line is enough
to go and find it.

That rate is itself two events out of seventy, so it is known only to about a factor of three
(95% CI roughly 0.5%–10%). The bars above are deliberately loose because of that. What they
are protecting against is a stranger spending an hour re-cabling a rig that was already
correct — the failure a cabling fault actually produces is `no capture` on **every** line of
the *measurement* run in step 5, which is unambiguous and needs no statistics.

**5. Run the measurement.** Same page, reload without the `control` parameter:

        http://127.0.0.1:8080/web/latency.html?n=20

Press **Measure**. It emits 20 clicks over about 15 seconds. You may hear faint ticks.

**A good run looks like this:** every line reads `click N: <number> ms`, the numbers are
tightly clustered (a spread of more than a few milliseconds means something is wrong), the
reported `peak` is between roughly 0.05 and 0.9 (below 0.05 the gain is too low; at or above
1.0 it is clipping — turn INPUT 1's gain knob down and re-run), and no line says
`IMPLAUSIBLE`. The last lines print the median round trip and the "unaccounted" figure, which
is the median minus what the API claims. **That unaccounted number is the result this gate
wants.**

**A bad run looks like this:** every line says `no capture`. Check the cable is in LINE OUTPUT
1 and not the headphone socket, that INPUT 1's gain is not fully counter-clockwise, and that
the MAIN knob is up. If a few lines say `IMPLAUSIBLE` and the rest are clustered, the
clustered ones are the real figure and the control run above is your evidence for saying so.

**6. Repeat for the other conditions**, re-running the control before each:

        http://127.0.0.1:8080/web/latency.html?n=20&hint=playback
        # and, launching the browser with the flag:
        "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" ^
          --enable-exclusive-audio "http://127.0.0.1:8080/web/latency.html?n=20"

**7. Record it** in the Gate 3 table above: replace each `PENDING RUN` cell with the median,
and keep the `baseLatency`/`outputLatency` columns from the same run rather than from this
table, since they are what the unaccounted figure is computed against.

The same page and the same instructions apply unchanged if Chrome or Firefox is ever
installed; add `&tag=chrome` or `&tag=firefox` so the transcript says which.

### Reproducing the no-cable half

    cd spikes/s5-wasm-web-audio
    python web/serve.py
    msedge --headless=new --no-sandbox --autoplay-policy=no-user-gesture-required \
      --use-fake-ui-for-media-stream \
      "http://127.0.0.1:8080/web/latency.html?auto=probe&hint=interactive&tag=x"
    #   ...&hint=balanced | &hint=playback      the latencyHint conditions
    #   ...&noinput=1                           output-only, no capture stream
    #   --enable-exclusive-audio                the exclusive-mode condition
    #   ...&auto=measure&control=1&n=10         the negative control
    #   ...&auto=measure&n=10&amp=1.0&len=480   the loud/long click

`--use-fake-ui-for-media-stream` auto-grants microphone permission; it does **not** substitute
a fake device (that would be `--use-fake-device-for-media-stream`, which must not be used here
— it would measure a synthetic capturer rather than the AudioBox). The transcript is recovered
from `serve.py`'s 404 log lines, the same beacon trick Task 6 used.

## Task 8 — optional extras: resampler cost, render quantum, 2026-09-05

Both extras per the coordinator's ruling: Extra 1 gets the real measurement effort (it can
produce a genuine number); Extra 2 is a one-shot availability check that stops the moment the
API is confirmed absent, rather than engineering around it. **Nothing here is certified** —
same caveat as every other figure in this file, and doubly so: browser benchmarks are not
core-pinned (Task 3's note) and this task's own Extra 1 finding is a demonstration of exactly
how much that costs.

### Extra 1: resampler cost

`bench.html` gained an IR selector (`ir_48k` default / `ir_44k1`) and an opt-in `irtiming`
checkbox / `&irtiming=1` query flag. `bench-worker.js` now times `load_ir` alone
(`performance.now()` wrapped tightly around the single export call, nothing else) and reports
each timing as its own `irload` message, kept structurally separate from the per-block `stats`
messages the existing reps loop produces — so a reader of the page output cannot conflate a
one-off load number with a per-block steady-state one.

**The parity gate does not follow the IR selector.** `bench-worker.js` gained a `PARITY_IR =
"../fixtures/ir_48k.wav"` constant: the parity render always loads that IR regardless of what
the timed reps ask for. The reference renders (`reference_render_f32le.bin` /
`reference_control_f32le.bin`) were made against `ir_48k.wav`; pointing the parity render at
`ir_44k1.wav` instead would fail it on a real signal difference (resampling changes the taps),
not a port defect — the same lesson `PARITY_MODEL` already encodes for the model axis. Every
run below still reports parity **PASS** at the usual figures.

**`ir_44k1.wav` forces the resample.** Both fixtures are 2.0 s, 16-bit PCM, stereo
(`ir_48k.wav`: 48 000 Hz / 96 000 frames; `ir_44k1.wav`: 44 100 Hz / 88 200 frames — confirmed
by parsing both WAV headers directly). Loading `ir_44k1.wav` into a 48 000 Hz engine takes
`PreparedIr::from_wav_bytes` (`crates/namir-ir/src/convolver.rs:730`) through `resample_mono`,
which is `rubato::FftFixedInOut` — scalar/NEON only, no `simd128` path, so it runs scalar on
**every** wasm artefact regardless of build. Read, not run: `resample_mono`'s output length is
`round(88200 * 48000 / 44100) = 96000` exactly (`88200 * 160 / 147 = 96000`, no rounding),
so the resampled tap count is bit-identical to `ir_48k.wav`'s native length — the convolution
partition schedule (`namir_ir::build_schedule`) is therefore the same shape either way. That
fact matters for the per-block result below.

Command (`web/serve.py` running from the spike root):

    msedge --headless=new --disable-gpu --no-sandbox \
      "http://127.0.0.1:8080/web/bench.html?auto=1&wasm=simd128&model=a1_standard&ir=ir_44k1&irtiming=1&signal=0&reps=5&measured=20000"
    # &ir=ir_48k for the control

Three same-session runs, Edge **152.0.4191.62** headless (`--disable-gpu --no-sandbox`),
simd128 build, A1 Standard, steady signal, 20 000 measured blocks, `crossOriginIsolated: true`
throughout. Parity every run: residual **−81.6906 dB**, control **−82.7158 dB**, margin
**1.0252 dB**, **PASS**.

| Run (order) | IR | `load_ir` ms, 5 reps | `load_ir` steady-state mean (reps 2–5) | per-block p99.9 %, 5 reps (contamination marked per rep) |
|---|---|---|---|---|
| 1 (1st launch) | ir_44k1 | 9.150, 5.905, 6.165, 6.020, 5.800 | 5.97 ms | rep 1 **CONTAMINATED** 23.81; reps 2–5 quotable: 20.25, 20.25, 20.44, 20.63 |
| 2 (2nd launch) | ir_48k | 3.940, 3.920, 3.920, 3.755, 3.715 | 3.83 ms | **all 5 reps CONTAMINATED**: 33.56, 30.75, 30.56, 31.31, 30.75 |
| 3 (3rd launch, ir_44k1 repeated) | ir_44k1 | 9.130, 6.020, 6.060, 5.860, 5.875 | 5.95 ms | **all 5 reps CONTAMINATED**: 33.37, 30.75, 30.75, 30.94, 30.94 |

Contamination read off each rep's own `quotable = p999 - estimator <= 5.0` flag in the raw
JSON (`edge_task8.txt`), per rep — not assumed uniform across a run. **Only Run 1 produced any
quotable per-block figure at all; Runs 2 and 3 are contaminated in every rep.** `load_ir`'s
steady-state mean is a separate metric — there is no contamination flag for a one-off load
timing — and only excludes rep 1 as the first-call JIT-warmup outlier discussed below. Each
fresh Edge launch used its own `--user-data-dir` so no on-disk profile state carried over
between runs.

**Load-time verdict: this IS a one-off load cost, and it reproduces.** `load_ir` alone costs
~**3.7–3.9 ms** with no resample (ir_48k) and ~**5.8–6.2 ms** with the 44.1→48 kHz resample
(ir_44k1) — a delta of roughly **+2.0–2.4 ms, about +55–60%**, consistent across both ir_44k1
runs (5.97 ms and 5.95 ms steady-state mean, 9.13–9.15 ms first call both times) despite one
being the very first Edge launch of the session and the other the third. That per-IR
consistency, against the per-block figure's inconsistency (next paragraph), is the basis for
calling this one real. It happens **once, at `load_ir`**, per `namir-engine`'s D-8.1 handover
protocol and this codebase's RT-safety rule (AGENTS.md: "file/network I/O... runs on
namir-worker's pool"), never per block — so in a real build this cost lands on the load/worker
path, not the audio thread's 2 666.67 µs budget, and ~2 ms extra there is immaterial to
real-time safety. It would matter to a *demo's* perceived load latency if IR loading were ever
moved onto a path a user waits on synchronously, which is a UX question, not an RT one.

**Per-block verdict: unanswerable from these runs — not measured, not "measured no effect."**
Only Run 1 (ir_44k1, first launch) produced any quotable per-block reps at all (rep 1
CONTAMINATED, reps 2–5 ≈ 20.25–20.63%). Run 2 (ir_48k, second launch, ≈ 30.56–33.56%) and Run 3
(ir_44k1 again, third launch, ≈ 30.75–33.37%) are contaminated in **every** rep — none of their
five reps clears the harness's own `quotable` rule. So "Run 3 reproduced Run 2's number" is a
contaminated-vs-contaminated agreement, not a clean comparison, and there is no clean ir_48k
(or clean ir_44k1 repeat) figure to set against Run 1's one clean result. That leaves no pair
of clean measurements to compare — which is a different, and weaker, thing to have than "we
measured X and found no per-IR difference." The retraction stands (there is no basis here for
claiming a per-block resampler cost), but the honest reason is that a second clean measurement
was never obtained, not that a clean comparison came back negative. The schedule-identity fact
above is corroborating colour for why no per-block difference would be expected even if a clean
comparison existed — `resample_mono`'s output is bit-identical in length to `ir_48k.wav`'s, so
`namir_ir::build_schedule` shapes the same partition structure either way — but it is an
argument from the code, not a substitute for the missing clean data, and it is not what makes
the retraction correct. The contamination pattern itself (every rep, both post-Run-1 launches)
is consistent with this repo's own documented "shared desktop contamination... 2–3× swings"
(repeated headless Chromium launches each pay their own SmartScreen DNS timeout and
extension-verification overhead, visible in the raw stdout logs), but that is offered as a
plausible cause, not a proven one. A firmer answer would need enough quotable reps in at least
two configurations to actually compare — many more interleaved reps (`ir_48k, ir_44k1, ir_48k,
ir_44k1, ...`) on the pinned reference machine, core-pinned if that affordance is ever extended
to a browser target — out of scope for this task's budget.

### Extra 2: render quantum sizes — PENDING RUN, confirmed unavailable here

`worklet.html` gained a `&checkRenderSizeHint=N` (default 256) cheap-availability probe: it
constructs a throwaway `AudioContext({ sampleRate: 48000, latencyHint: "interactive",
renderSizeHint: N })`, reads back `ctx.renderQuantumSize`, reports whether it was **honoured**
(equals `N`), **ignored** (some other numeric default), or **threw** (`NotSupportedError`, the
option rejected outright) — then closes the context and returns without starting the worklet.
No wasm rebuild, no `BLOCK_SIZE`/`IR_PERIOD_BLOCKS` change: per the ruling, that work is only
worth doing once the hint is confirmed present.

Command:

    msedge --headless=new --disable-gpu --no-sandbox --autoplay-policy=no-user-gesture-required \
      "http://127.0.0.1:8080/web/worklet.html?auto=1&checkRenderSizeHint=256"

Result on Edge **152.0.4191.62** (Chromium/V8; this machine has no Chrome or Firefox install):

    renderSizeHint 256: NOT honoured -- ctx.renderQuantumSize undefined ("hardware"/default;
    the option was accepted but ignored)

`renderQuantumSize` is not merely defaulting to 128 here, it is `undefined` — the whole
property is absent from this runtime's `AudioContext`, not just the option being silently
ignored. `renderSizeHint` shipped in Chrome 153; this machine's Edge is Chromium-based but
pinned to 152.0.4191.62 (confirmed via `msedge --version`), one release behind. Nothing else
in this extra was attempted — no `BLOCK_SIZE` change, no rebuild, no 256/512 measurement — per
the ruling to stop at the availability check rather than spend effort proving a negative.
**PENDING RUN, exact command above (plus, once it passes, testing `renderSizeHint: 256` and
`512` and rebuilding `harness::BLOCK_SIZE`/`IR_PERIOD_BLOCKS` to match per the task brief) on
Chrome 153+ or a newer Edge.**

### Task 8 verdict

**Extra 1 (kept, real number):** rubato's scalar-only resample of a 44.1 kHz IR into a 48 kHz
context costs roughly **+2.0–2.4 ms (+55–60%) at `load_ir`, once, off the audio thread** —
immaterial to real-time safety, plausibly noticeable to a demo's load-time UX. **The per-block
question is unanswered, not answered negatively**: only one of three runs produced any quotable
per-block rep, so there was never a clean pair of measurements to compare — the investigation
that would have claimed a clean "no effect" finding turned out to have no clean data on either
side of the comparison it was about to make. Worth keeping on the record per this project's own
stated practice of retracting a finding honestly rather than quietly dropping it.
**Extra 2 (stopped early, per the ruling):** confirmed absent on this machine's only available
browser (Edge 152.0.4191.62); `PENDING RUN` on Chrome 153+/newer Edge, exact command above.
