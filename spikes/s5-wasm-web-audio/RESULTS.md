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
