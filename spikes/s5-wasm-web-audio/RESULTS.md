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

### Parity: −81.48 dB, above the brief's −100 dB bar. **A bar problem, not a chain problem.**

Measured two ways, agreeing to the last printed digit:

| Runtime | `crossOriginIsolated` | parity vs native reference |
|---|---|---|
| Node v24.19.0 (V8/TurboFan), `web/parity-node.mjs` | n/a | **−81.4759 dB** |
| Microsoft Edge headless (Chromium/V8), full module-worker + `fetch` + COOP/COEP path | `true` | **−81.4759 dB** |

Controls, same machine, same reference render:

| Comparison | dB |
|---|---|
| **CONTROL** — silence vs the native reference | **0.00** |
| **CONTROL** — native (default codegen) vs native (`-C target-cpu=x86-64 -C target-feature=-avx,-avx2,-fma`) | **−82.72** |
| wasm32 vs native (default codegen) | −81.48 |
| wasm32 vs native (`-avx,-avx2,-fma`) | −83.33 |

Both native builds are the *same source*, the same compiler, on the same machine, and
produce byte-identical `.nam`/`.wav` fixtures — only the host codegen differs, and they
still disagree by **−82.72 dB**. (The isolated `CARGO_TARGET_DIR` matters here: an
earlier attempt in the shared target directory produced a stale-fingerprint mix that
briefly made this look like a source-level difference. It is not.)

So **the −100 dB bar is unreachable by any build of this chain, native included.** The
wasm figure sits 1.24 dB from that native-vs-native floor. Read with the 0.00 dB
silence control, the diagnostic profile below, and the fact that the wasm module is
*closer* to the no-AVX native build than to the AVX one, the conclusion is that the
wasm chain computes the same thing and the divergence is float reassociation amplified
by a nonlinear amp model.

Per-block diagnostic on the wasm-vs-native residual (`a1_standard`, 256 blocks):

- **No delay and no gain error.** Best-fit gain got/want = `0.999999726`. Shifting the
  wasm output by ±1 sample collapses the figure to −35 dB and by ±2 to −29 dB, so the
  two renders are sample-aligned; a structural difference would not look like this.
- **Bounded, non-accumulating error.** Max absolute error stays ~1e-4 across the whole
  render (block 4: 7.4e-5, block 40: 2.2e-4, block 255: 0) against a reference RMS of
  ~0.9-1.0. It does not grow with block index.
- **Several blocks are bit-exact** (max absolute error exactly `0.0`) — every one of
  them a block whose reference RMS is exactly 1.0, i.e. a fully railed block where the
  nonlinearity saturates both implementations to the same value. A structurally
  different chain would not produce bit-exact blocks.

**Verdict: NOT a kill.** The chain ports correctly. What is wrong is the −100 dB bar,
which was specified without a native-vs-native control. **The threshold is deliberately
left at −100 dB in both `web/bench-worker.js` and `web/parity-node.mjs`, and both still
report FAIL** — tuning it to pass was explicitly out of bounds, and the calibration
evidence for whatever the bar should become belongs to whoever owns the plan, not to
this task. Both runners refuse to benchmark on a parity failure unless an explicit
opt-in is given (`?anyway=1` in the page, `--allow-parity-fail` in the Node runner);
that opt-in is the human act saying the investigation above was read.

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

#### Edge headless — `a1_standard`, steady, 20 000 measured blocks, 5 reps

Command (`web/serve.py` running from the spike root):

    msedge --headless=new --disable-gpu --no-sandbox \
      "http://127.0.0.1:8080/web/bench.html?auto=1&anyway=1&reps=5&measured=20000"

`crossOriginIsolated: true`. Parity, reported by the page before it would benchmark:
**−81.4759 dB** (the `anyway=1` opt-in is why it benchmarked at all — see Parity above).

```
edge a1_standard steady rep 1/5: p50 39.19% | p99 67.88% | p99.9 86.44% | max 104.44% | estimator 50.62% | CONTAMINATED
edge a1_standard steady rep 2/5: p50 39.38% | p99 69.37% | p99.9 89.25% | max  93.19% | estimator 50.81% | CONTAMINATED
edge a1_standard steady rep 3/5: p50 39.19% | p99 65.44% | p99.9 84.38% | max  92.44% | estimator 50.81% | CONTAMINATED
edge a1_standard steady rep 4/5: p50 39.19% | p99 64.69% | p99.9 85.50% | max  97.13% | estimator 50.63% | CONTAMINATED
edge a1_standard steady rep 5/5: p50 39.19% | p99 65.63% | p99.9 86.44% | max  93.38% | estimator 50.81% | CONTAMINATED
```

**20 000 measured blocks, not the 100 000 the native run used.** A 100 000-block rep
takes ~11 minutes under headless Edge on this machine (Edge is roughly 5× slower per
rep than Node for the same work, which is a browser-scheduling artefact, not a wasm
one), so five of them was not a practical wait. 20 000 blocks still puts 20 samples
above p99.9 and does not affect the per-residue estimator at all. Two earlier
five-rep attempts at 100 000 blocks are **discarded and not quoted**: the first had a
second headless Edge instance still alive and beaconing into the same log, and both
showed the `p50` spread (39% → 75%) that concurrent load produces. The run above was
made with nothing else running, and its `p50` is stable to ±0.19 percentage points
across five reps.

#### Node v24.19.0 cross-check — `a1_standard`, steady, 100 000 measured blocks, 1 rep

    node web/parity-node.mjs --bench --allow-parity-fail --reps 1

```
node a1_standard steady rep 1/1: p50 44.82% | p99 65.71% | p99.9 83.90% | max 99.73% | estimator 55.61% | CONTAMINATED
```

Same V8, no browser scheduler, `process.hrtime.bigint()` instead of
`performance.now()`, full 100 000 blocks — and it lands within a few points of the Edge
figures, which is the cross-check's whole job. Wall time 2 m 12 s for the rep, i.e.
~1.26 ms of wall per 128-frame block against a 1.21 ms measured p50: the harness's own
per-block overhead outside the timed span is small, so the measured span is not hiding
the cost.

#### The number this task exists to produce

| | native (Task 2, 5 reps) | Edge headless (5 reps) | ratio |
|---|---|---|---|
| p50 | 6.30-6.65% | 39.19-39.38% | **≈6.1×** |
| p99.9 | 13.60-14.69% | 84.38-89.25% | **≈6.1×** |
| estimator (contamination-immune) | 9.92-10.37% | 50.62-50.81% | **≈5.0×** |

**wasm32 is ~5-6× slower than native for the same chain**, and at 128 frames / 48 kHz
that puts `a1_standard`'s p99.9 at ~86% of the block period with `max` crossing 100% in
one rep of five. That is not a pass and not a clean fail; it is the number Tasks 4-6
have to work against. `a2_lite` was not measured in the browser — at ~5.6× its native
5.4-5.7% estimator it would land near 30%, which is comfortable, but that is an
extrapolation and is not recorded here as a measurement.

#### Three caveats that bound how far these figures can be pushed

1. **Headless timer resolution is not the shipping browser's — the most important
   methodological finding here.** Every Edge percentage above is an exact multiple of
   0.1875% of the block period, i.e. **5 ns**. Headless Edge is evidently not applying
   the 5 µs `performance.now()` coarsening a cross-origin-isolated *interactive* page
   gets (let alone the 100 µs a non-isolated one gets). An interactive Chrome run will
   quantize each per-block sample to 5 µs — ~187% of the block period — so per-block
   percentiles there will be nearly meaningless. **Task 4 needs a batched timing
   strategy (time N blocks, divide) rather than the per-block `now_us()` pair this
   harness uses**, or its browser figures will be quantization artefacts.
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
    cargo run --release --bin native_bench    # regenerates fixtures/ if absent
    python web/serve.py
    # then, in each browser, open:
    #   http://127.0.0.1:8080/web/bench.html
    # confirm "crossOriginIsolated: true", tick "bench anyway" (the parity check fails
    # against the -100 dB bar by design -- read the Parity section first), click Run.
    # Record the browser version and the crossOriginIsolated state with the figures.

Expect interactive Chrome and Firefox figures to differ from the Edge headless numbers
above for the timer-resolution reason in caveat 1, not only for engine reasons.

### Task 3 verdict

**Proceed to Task 4, with two things carried forward.** The wasm chain is functionally
correct (parity −81.48 dB against a −82.72 dB native-vs-native floor, 0.00 dB against
silence, sample-aligned, unit gain, bit-exact on railed blocks). The −100 dB parity bar
is wrong and needs re-deciding with the native-vs-native control in hand — that decision
is not this task's to make, and the threshold is left failing in both runners. The
timing headline is ~5-6× native, with `a1_standard`'s tail at ~86% of the block period.
