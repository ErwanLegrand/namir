# Namir — Implementation Roadmap

| | |
|---|---|
| **Project** | Namir |
| **Document** | 03 — Implementation Roadmap |
| **Version** | 0.1 (draft) |
| **Date** | 2026-08-05 |
| **Status** | Draft — awaiting review |
| **Author / Copyright holder** | Erwan Patrick Legrand |
| **Licence** | MIT OR Apache-2.0 |
| **Governing documents** | `01-functional-requirements.md` v0.2, `02-architecture.md` v0.8 |

---

## 1. Purpose and standing

This document sequences the work `02-architecture.md` decided but has not yet built. It does not
redecide anything: where a milestone below conflicts with the FRS or the architecture document,
those win and this document is a defect, exactly as `02-architecture.md` §1 holds itself
subordinate to the FRS.

What this document adds that the other two don't: **order**. The FRS says what; the architecture
document says how; neither says which piece to build before which other piece, or what "done"
looks like at each waypoint. That's the gap this fills.

**No calendar estimates appear anywhere in this document.** The project's own methodology
(D-2.1/D-2.2, OQ-2) refuses placeholder numbers dressed up as measurements — "a real number, not
a guess." There is no velocity data for this team yet, so a date would be exactly that kind of
guess. Instead, each milestone is sized relatively (S/M/L/XL, roughly: days / one-to-two weeks /
several weeks / a month-plus of focused work for one person) and, more importantly, sequenced by
genuine dependency, not by estimated duration.

---

## 2. Current state (M0) — as of this document's date

Verified by a fresh audit of the repository, not assumed from the architecture document's own
account of itself: `02-architecture.md`'s own §24 changelog stops at version 0.8, predating this
session's `namir-dsp`/`namir-nam` work.

**Built and tested: 130 tests, all passing, zero compiler/clippy warnings, zero `unsafe` code
anywhere in the workspace.**

| Crate | State |
|---|---|
| `namir-core` | Complete for its own narrow brief (D-5.1: "no logic"). `SampleRate`, `ChannelConfig`, `ContentHash`, `db_to_linear`/`linear_to_db`, the `ErrorCode`/`Severity` catalogue pattern. Nothing missing here. |
| `namir-dsp` | Complete and product-grade: `Biquad`/`BiquadCoeffs` (TDF-II, RBJ designs, f64 coefficient math), `NoiseGate` (hysteresis, sample-accurate ramps), `Meter` (peak/average/peak-hold/clip), `GainRamp`, `DcBlocker`. All allocation-free and RT-harness-proven. No stage/chain/parameter awareness by design — that's `namir-engine`'s job. |
| `namir-nam` | The single most roadmap-ready subsystem. `.nam` JSON parsing with a full catalogued-error taxonomy, NFR-SEC-020-grade dimension-ceiling validation *before* any arithmetic, allocation-free RT-path WaveNet inference, and a cross-implementation numeric-parity test against `namir-fixtures`' independent reference (bit-exact agreement measured). Explicitly and deliberately **not** in scope: LSTM, resampling, crossfaded handover, loudness calibration, cost reporting — see its own crate doc for the exact boundary. |
| `namir-fixtures` | Complete for its own scope: seeded, generated (never captured) `.nam` fixtures, IR-correctness signals (delta/delayed-delta/decaying-noise/minimum-phase), and fuzz-corpus mutation. Will grow in M3 (an LSTM fixture generator) but needs no rework. |
| `namir-engine` | **Scaffolding only.** `Stage`/`StagePrep` (the trait split), `Chain` (real, tested — including the non-trivial max-not-sum tail arithmetic), `StageIo`, `PrepareContext`/`PrepareError` are all real and tested. `ParamId`/`ParamChange` exist but are deliberately bare (id + raw `f32`; no descriptor, no manifest). `TelemetrySink` is explicitly a single-threaded trait-shape placeholder for D-7.3's real lock-free cross-thread ring, not the ring itself. **Zero real `Stage` implementations exist anywhere in the crate** — the only things implementing `Stage` are `#[cfg(test)]`-only fakes (`FixedGainStage`, `ConstantTail`, `AllocatingStage`) used to exercise `Chain` and the RT harness. None of the six 1.0 product stages (Trim/Gate/Nam/Ir/Eq/Out) exist. |
| `namir-params`, `namir-ir`, `namir-state`, `namir-library`, `namir-platform`, `namir-worker`, `namir-ui`, `namir-app`, `namir-clap` | **Do not exist.** |
| CI / tooling | **Nothing exists.** No `.github/`, no `deny.toml`, no fuzz harness, no `benches/`, no MSRV pin, no `params.lock`, no traceability tooling. Every NFR whose *Verify* method is **S** currently holds only "by omission" (e.g. no network dependency happens to be linked; nothing enforces it stays that way). |
| Dependencies resolved so far | Only `rustfft`/`realfft` (via `namir-fixtures`), plus `serde`/`serde_json`/`rand`/`rand_pcg`/`blake3`/`assert_no_alloc`. **Not yet added to the product workspace:** `rubato` (resampling), `hound` (WAV), `cpal` (audio I/O), `egui`/`baseview`/`egui-baseview` (UI), `clack`/`clack-extensions` (CLAP). These are first-use additions at the milestones that need them, not something to pre-integrate speculatively. |
| Spikes (`spikes/*`, excluded from the workspace) | All four already executed and their findings recorded in `02-architecture.md` §19 — **s1** (WaveNet inference, already ported into `namir-nam`), **s2** (partitioned convolution — the source `namir-ir` will be built from, including the R-8 lockstep-partition defect it found), **s3** (egui-in-baseview, validated D-15.1/D-15.2 — a future `namir-ui` ports its wiring pattern), **s4** (clack against real hosts, validated D-14.2, including a working `PluginGuiImpl`/`open_parented` embed — a future `namir-clap` ports its skeleton). s3/s4 don't carry their own `README.md` the way s1/s2 do; their results live centrally in the architecture document's §19 instead. That's a difference in *where* the record lives, not evidence the spikes are unfinished — the architecture doc's write-up for both is detailed enough (frame counts, validator pass/fail tallies, install-path findings) to be genuine execution evidence, and this roadmap treats R-1 and R-2 as retired, matching `02-architecture.md`. |

---

## 3. Sequencing principles

Three things drive the order below, in this priority:

1. **The layering table in `02-architecture.md` §5 (D-5.1) is a hard dependency graph, not a
   preference.** A crate can only be built once everything in its "may depend on" column exists.
   This alone fixes most of the ordering: `namir-params`/`namir-ir` before the product stages;
   the product stages before `namir-worker`; `namir-worker` before `namir-library`/`namir-state`
   consumers that need it; everything above before `namir-app`/`namir-clap`.
2. **P1/P2 (`02-architecture.md` §3): nothing expensive, fallible, or platform-specific belongs on the audio thread,
   and worker-thread machinery should exist before anything depends on it being real.** This is
   why the SPSC/telemetry rings and the four-step handover protocol are sequenced *after* the six
   stages exist, not before: a stage can be built and correctness-tested via direct
   `StagePrep::prepare`/`Chain::apply` calls without a real cross-thread ring underneath it, and
   building the ring before there's a real worker to drive it or a real stage to receive from it
   would mean testing it against fakes twice.
3. **Known, already-measured risk beats convenience.** R-4 and R-8 (`02-architecture.md` §19/§22) are not
   theoretical — S-1 and S-2 already measured that NAM (41%) and IR (56–94% at NFR-PERF-010's own
   condition) *individually* consume more than the entire 25% budget between them, before gate or
   EQ are counted, and R-8 specifically is a scheduling defect that's much cheaper to design out
   of `namir-ir` from the start than to retrofit after a convolution engine ships. Performance
   closure is therefore its own explicit, benchmark-gated milestone (M3), not a footnote.

A caution that applies across M2 and M4: FR-NAM-070/FR-IR-060 require a stage to run **two live
resources simultaneously** during a crossfade. M2 should build the Nam and Ir stages with that
dual-resource capacity from day one — even though nothing exercises it until M4's worker and
handover protocol exist — because retrofitting "hold two of these instead of one" into an
already-shipped stage shape is exactly the kind of rework D-6.1's whole prepare/process split
exists to make unnecessary. Build it right the first time; M4 only has to *wire* it.

```
M0 (done) ─┬─→ M1 (CI, params, denormal guard) ─→ M2 (six stages + namir-ir) ─→ M3 (perf + LSTM)
           │                                              │                          │
           │                                              ▼                          ▼
           │                                     M4 (rings, worker, handover) ←──────┘
           │                                              │
           │                                              ▼
           │                                     M5 (state, library)
           │                                              │
           │                                              ▼
           └──────────────────────────────────→  M6 (platform, app, ui, clap)
                                                           │
                                                           ▼
                                                  M7 (compliance closure)
                                                           │
                                                           ▼
                                                  M8 (1.0 exit)
```

---

## 4. Standing gates — hold at the end of every milestone, not just once in CI

Two things worth pinning down explicitly before M1 starts: gate configuration drifting out of
date once work is split across many milestones, and CI being the *only* enforcement layer when a
faster local one would catch most problems before they're even pushed.

**Every milestone's exit, M1 through M8, includes — in addition to whatever that milestone's own
acceptance criteria list — these three, non-negotiably:**

- `cargo fmt --check` (workspace-wide: `cargo fmt --all -- --check`) passes with no diff.
- `cargo clippy --workspace --all-targets` passes with zero warnings under the project's
  configured lint set (NFR-QUAL-060). This is already named as an M1 CI deliverable below; the
  reason to restate it here is that "the CI job exists" and "the CI job is green at every
  milestone boundary" are different claims, and only the second one actually satisfies
  NFR-QUAL-060 on an ongoing basis rather than once. A milestone is not done while either check is
  red — this sits alongside that milestone's functional acceptance criteria, not below it.
- `cargo test --workspace` passes (already implied by every milestone's own acceptance criteria;
  restated here so all three read as one standing bar rather than three concerns of different
  weight).

**Local enforcement, complementing CI, not replacing it:** a checked-in pre-commit hook
(`.githooks/pre-commit`, enabled per clone via `git config core.hooksPath .githooks`) runs
`cargo fmt --all -- --check` and `cargo check --workspace --all-targets` before every commit —
fast checks only. Clippy and the full test suite are deliberately **not** in the hook: both are
slow enough that putting them on the commit-time path would make `--no-verify` tempting, which is
a worse outcome for quality than clippy running one step later, in CI, on push. The hook is a
faster feedback loop for the two cheapest, most common failure modes (unformatted code, plain
compile errors) — it is not the authoritative gate. Hooks can be bypassed (`--no-verify`) or
simply never installed by a contributor who hasn't run the one-time `git config` step, so CI
remains what D-18.1 actually relies on.

**Test coverage is tracked, not gated.** `cargo-llvm-cov` runs in CI (set up in M1, below) and
reports line/branch coverage on every push, but it does not block a milestone or a merge — neither
the FRS nor the architecture document sets a numeric coverage target anywhere, and this project's
actual quality bar is NFR-QUAL-010's per-requirement traceability (every Must requirement mapped
to a test), not a line-coverage percentage. A percentage can be high while real requirements go
unverified, or read low on code that's fully covered by requirement-driven tests but happens to
exercise few lines — it's a diagnostic signal for reviewers, not a substitute for traceability.
Deliberately decided this way rather than picking a threshold now: an unbacked number today would
be exactly the kind of placeholder-dressed-as-a-decision this project's own methodology (D-2.1,
OQ-2) refuses elsewhere.

---

## 5. M1 — Foundations for real stages

**Size: M.** **Depends on:** M0 only. **Blocks:** M2.

Nothing here touches DSP or the audio path. The point of M1 is that every stage built in M2
should be built *against* a real parameter system and a real CI gate from its first commit,
rather than retrofitted onto one later — the exact mistake FR-PARAM-020 (permanent identifiers)
exists to make expensive.

**Deliverables:**

- **`namir-params`** — parameter descriptors (FR-PARAM-010: name, unit, min, max, default,
  formatting rule), the string→stable-`u32` identifier derivation with the FR-PARAM-020 manifest
  (`params.lock`, checked in, D-10.1), the stage-instance-index field D-10.2 requires be present
  and zeroed now (so RD-2's future dynamic chain never has to renumber), and the smoothing-category
  declarations D-10.3 assigns to a descriptor rather than open-coding per stage (gain-like →
  `namir-dsp::GainRamp`, frequency-like → `Biquad` coefficient interpolation, stepped → a
  crossfade/click-free switch point). Stepped/discrete parameters (FR-PARAM-050) get their own
  type here too — today `ParamChange.value` is always `f32`, which cannot represent them.
- **`namir-platform`, minimal slice** — just D-7.4's denormal-suppression guard type (FTZ/DAZ set
  for the callback's duration, restored on drop via `Drop`, `unsafe` with a written safety
  argument per D-5.3 — this crate's whole reason for existing is to be the one place besides
  `namir-clap` allowed to carry that). Everything else D-5.1 assigns to `namir-platform`
  (filesystem paths, CLAP install paths, thread priority) waits for M6, when something actually
  consumes it.
- **CI skeleton** (`.github/workflows/`, or equivalent) covering everything that can be gated
  *today*, so every subsequent milestone lands under it instead of accumulating debt to clean up
  at M7: build + test on Windows/Linux/macOS; formatting and clippy as errors (NFR-QUAL-060,
  remembering S-4's finding that MSVC's `linker_messages` on `cdylib` builds must be explicitly
  allowed, not trained-around, once `namir-clap` exists); `cargo-deny` license audit
  (NFR-LIC-020); the layering dependency-graph lint rejecting any edge not in D-5.1's table
  (D-5.2); the `params.lock` diff check (FR-PARAM-020); the D-7.5 RT-allocation harness run as a
  gate, not just a local test; an MSRV pin in the workspace manifest, enforced (NFR-PORT-010); a
  `cargo-llvm-cov` coverage report generated on every push, informational only, no blocking
  threshold (see §4's reasoning). The no-C++-toolchain container build (NFR-PORT-040) and mobile
  cross-compilation gates (NFR-PORT-030) are added incrementally as each "builds for mobile" crate
  lands — most of the workspace already qualifies today, so this can start immediately and grow.
- **Quick wins, no dependencies, worth doing now rather than at M7:** a `cargo-fuzz` target for
  `namir_nam::load`, seeded from the mutation corpus already in `namir-fixtures::mutate`
  (NFR-QUAL-040 currently only has one-shot corpus testing, not continuous fuzzing — this closes
  that gap for the one parser that exists today, and the pattern repeats for each new parser
  M2/M5 add); a `#![warn(missing_docs)]`-equivalent enforced per crate (NFR-DOC-020).
- **Pre-commit hook** (§4) — already implemented, ahead of the rest of this milestone:
  `.githooks/pre-commit` plus the one-time `git config core.hooksPath .githooks` to enable it per
  clone. Cheap, dependency-free, and blocks on nothing else, so it shipped immediately rather than
  waiting for the rest of M1 to land.

**Open decision to record before M2 starts** (surfaced by this milestone's own audit, not
resolved by it — see §15): whether NFR-QUAL-030's "golden reference audio held in the repository"
is satisfied by the project's actual, deliberate strategy of generated fixtures plus analytic or
cross-implementation verification (D-19.1's decision already points this way) or whether it needs
its own restatement. This costs nothing to resolve now and is a paragraph in
`02-architecture.md`, not new code.

**Acceptance:** FR-PARAM-010/020/050 closed; NFR-PORT-010, NFR-LIC-020, NFR-QUAL-060 (partial —
mobile/no-C++ gates still growing), D-5.2's layering lint all live and green in CI.

---

## 6. M2 — The fixed six-stage chain

**Size: XL — by far the largest milestone.** **Depends on:** M1. **Blocks:** M3, M4, M5.

This is where `namir-engine` stops being scaffolding. Trim, Gate, and Eq depend only on
`namir-dsp` (already done) and `namir-params` (M1) — they can start immediately and in parallel.
Nam and Ir each depend on one new piece of integration work first.

**Deliverables:**

- **`namir-ir`** (new crate; may depend on core, dsp per D-5.1) — WAV decoding via `hound`
  widened to FR-IR-010's full format matrix (16/24/32-bit int, 32-bit float, mono/stereo,
  8–192 kHz); `rubato`-based resampling to the engine rate meeting FR-NAM-060's quality bar
  (≥100 dB stopband, ≤0.1 dB ripple — verified by direct measurement per D-9.3, not trusted from
  the library's defaults); D-9.4–9.6's non-uniform partitioned convolution, ported from
  `spikes/s2-ir-convolution` — **built from the start with R-8's fix** (staggering same-size
  partitions' trigger phases, or equivalently amortizing each large FFT across the several block
  calls its slack provides) rather than the spike's synchronous scheme, since that defect is
  R-8's whole finding: schedule tuning alone cannot fix it. D-9.5's direct time-domain reference
  convolution is ported alongside and kept permanently for correctness verification, per its own
  mandate.
- **The six real `Stage`/`StagePrep` pairs**, added to `namir-engine`, each backed by an existing
  or newly-built primitive crate, plus a `build_default_chain()` assembling them in FR-CHAIN-010's
  fixed order:
  - **Trim** — `namir-dsp::GainRamp` + `DcBlocker` (FR-IN-010/040).
  - **Gate** — `namir-dsp::NoiseGate` (FR-GATE-010/020/030/040), placed *before* Trim in the
    actual chain per D-9.8 — this is `namir-dsp`'s own stated boundary (the primitive is
    trim-agnostic; ordering is `namir-engine`'s job, exercised here for the first time).
  - **Nam** — `namir-nam::PreparedNam`/`NamState` wrapped with a `rubato` resampler per D-9.2/9.3
    (resampling around the NAM stage only, bypassed at 48 kHz for zero added cost/latency), built
    with the crossfade-capable dual-resource shape §3 calls out. WaveNet only until M3; a
    `.nam` file naming `LSTM` is rejected the same way any unsupported architecture is, exactly
    as `namir-nam` already does today.
  - **Ir** — `namir-ir`'s convolver + `namir-dsp::Biquad` HighPass/LowPass for FR-IR-070's cuts,
    same dual-resource crossfade shape.
  - **Eq** — three `namir-dsp::Biquad` bands (low shelf / mid peaking / high shelf per
    FR-EQ-010's table) plus HP/LP, each already proven stable across the full parameter/rate
    sweep and click-free under coefficient interpolation at the primitive level (M2's job is
    wiring parameter descriptors to it, not re-proving the DSP).
  - **Out** — `namir-dsp::GainRamp` with FR-OUT-010's exact-silence-at-−60 dB-floor semantics
    (not just an asymptotic ramp) + an optional brickwall limiter (Should, FR-OUT-030).
  - Cross-cutting, at the `Chain`/stage level rather than any one stage: FR-CHAIN-020 (per-stage
    bypass, click-free), FR-CHAIN-030 (global bypass), FR-CHAIN-040 (empty-stage passthrough —
    Nam/Ir with nothing loaded behave as bypassed, not silent, not erroring), FR-CHAIN-080 (NaN/Inf
    → silence + fault flag, not propagation), FR-CHAIN-090 (output ceiling), FR-CHAIN-060/070
    (the three channel configurations and stereo-input channel selection, using
    `namir-core::ChannelConfig`'s already-tested arithmetic).

**Acceptance:** every Must in FRS §5.1 (CHAIN), §5.2 (IN), §5.3 (GATE), most of §5.4 (NAM, minus
LSTM/resampling-quality/loudness), §5.5 (IR, minus AIFF/FLAC), §5.6 (EQ), §5.7 (OUT), and
FR-PARAM-030/040 goes from Partial/Not-started to Done. This is the milestone that turns nearly
every "Partial: primitive exists, not integrated" row from the gap analysis into "Done."

---

## 7. M3 — Performance closure and LSTM

**Size: L.** **Depends on:** M2 (needs a real chain to benchmark end-to-end). **Blocks:** M4 only
loosely (M4 doesn't need M3's results, but shipping a worker/handover on top of a chain that's
already known to blow its CPU budget is a bad sequencing choice regardless).

This milestone exists because S-1 and S-2 already proved it's needed, not because it's
speculative hardening. NAM alone measured 41% of one core against a 25% budget; IR alone measured
56–94% at NFR-PERF-010's own literal condition and up to several hundred percent at small block
sizes. The two together, before gate or EQ, already exceed the entire budget.

**Deliverables:**

- **R-4** — vectorize `namir-nam`'s WaveNet inner loops (the dilated and 1×1 convolutions).
  Re-measure against the *assembled* chain from M2, not the isolated loop the S-1 spike measured,
  since that's the number NFR-PERF-010 actually gates on.
- **R-8** — verify and tune `namir-ir`'s phase-staggering (built into M2 per §6's note, not
  retrofitted here) against the same grid S-2 swept: IR lengths 0.1–10 s, block sizes 32–2048,
  rates 44.1–192 kHz. If M2's implementation didn't fully close the gap, this is where it gets
  fixed for real, with the benchmark as the exit gate rather than a design review.
- **LSTM** in `namir-nam` (FR-NAM-020's other Must architecture) — needs a matching generated
  fixture (D-19.1: generated, never captured) added to `namir-fixtures`, and a parity test the
  same shape as WaveNet's existing one.
- **Exit criterion, and the reason this milestone is gated rather than advisory**: the full
  six-stage chain, assembled for real, meets NFR-PERF-010's literal condition (48 kHz, 64-sample
  block, standard WaveNet, 2 s stereo IR, gate + EQ active) at the 99.9th percentile per
  D-2.1/D-2.2's methodology, measured on the reference machine (`02-architecture.md` §2). This is the first point in
  the roadmap where that benchmark can be run against something real instead of a spike's
  isolated loop.

> ⚠️ **Read "M3 close-out: the tail was never Namir's" at the end of this section before trusting
> any performance figure in the three subsections that follow.** Every `p99.9` number below was
> measured with the benchmark pinned to logical CPU 0, which on the reference machine absorbs the
> GPU driver's 128–512 µs interrupts (~165/second, zero on all other cores). Those figures are
> therefore substantially measuring `dxgkrnl.sys`, not Namir, and the conclusions drawn from them —
> including two separate "NFR-PERF-010 does not close" verdicts — do not survive. They are retained
> unedited as an honest record of the investigation, not as current findings.

**Status as of the prior session (2026-08-06) — honest accounting, not the original "Acceptance"
text below, which this milestone had not yet earned in full. Superseded twice over: first by the
reference-machine pass recorded immediately below, then wholesale by the close-out at the end of
this section:**

- **LSTM: done.** `namir-nam/src/lstm.rs` ports `NeuralAmpModelerCore`'s `LSTMCell`/`LSTM` from
  its C++ source directly (module doc comment cites the exact fields/order read), unified behind
  the same `PreparedNam`/`NamState` surface WaveNet already used (`model.rs`'s enum wrapper) —
  `namir-engine`'s `Nam` stage needed **zero** changes to support it, confirming that surface was
  genuinely architecture-agnostic. `namir-fixtures` gained a matching generated LSTM corpus
  (three shapes) and `namir-nam/tests/lstm_fixtures.rs` parity-tests it against an independent
  from-scratch reference (`namir-fixtures`' own `lstm_infer`), the same cross-implementation-
  agreement approach S-1 used for WaveNet. FR-NAM-020 (both Must architectures) closes.
- **R-4: real, measured, but not sufficient alone — and the exact magnitude is genuinely
  uncertain on this sandbox.** `wavenet.rs`'s `axpy` now vectorizes every AXPY-shaped inner loop
  with `wide::f32x8` (see that file's own Decision/Rationale note). Its own
  `benches/wavenet_inner_loops.rs` has now been measured three times across this milestone's
  review passes, with growing rigor: an initial best-of-several read of ~42–53% p99.9; an
  intermediate re-measurement that reported an unreproduced 330–345% p99.9 spike on every run;
  and this close-out pass's own interleaved scalar-vs-vector A/B, run under a load average
  explicitly confirmed quiet throughout (9-11 runs), landing back near the first estimate: no
  reproducible p50 win (scalar mean 26.58%, vector mean 26.80%), p99.9 44.3–54.8% scalar vs.
  43.7–48.6% vector — overlapping ranges that don't cleanly separate from run-to-run noise. See
  `wavenet.rs`'s own Decision-note for the full numbers and the most likely explanation for the
  intermediate reading's outlier (probably uncontrolled sandbox contention, per the same
  phenomenon R-8's own re-verification documented — not confirmed, flagged as the best available
  account). **Even at the more favourable ~45% p99.9 reading, this alone, in isolation, already
  exceeds the 25% budget on this sandbox** — whichever exact number is closest to true,
  vectorization has not been shown to close the gap S-1 found by itself. Downgraded further
  evidence, not retired.
- **R-8: real, measured, largely closed at the stage level.** `convolver.rs`'s stagger fix
  (per-*size*, block-aligned, replacing M2's per-*group* scheme) is described and measured in that
  file's own module doc comment: at NFR-PERF-010's own literal condition (48 kHz, 64-sample block,
  2 s IR), IR-stage-alone p99.9/max fell from 337.7%/602.5% to **16.8%/41.3%** on this sandbox — a
  15–20x improvement, taking that condition from several multiples of a full core's budget to
  comfortably under half of one core. Two gaps remain, recorded rather than glossed over in that
  same doc comment: 2048-sample blocks at 192 kHz/10 s IRs stay just over budget (117.8% p99.9),
  and 32-sample blocks at 192 kHz show an elevated but likely-noise `max`. The scheduling *defect*
  R-8 names is closed; the milestone-risk closure below is a separate question.
- **Exit-criterion benchmark: built and run; result is FAIL on this sandbox.**
  `namir-engine/benches/six_stage_chain.rs` assembles the real six stages (gate → trim → nam → ir
  → eq → out) via the same `StagePrep::prepare` calls `build_default_chain` makes, loads a real
  generated standard WaveNet model through `namir_nam::load` and a real generated 2 s stereo IR
  through `PreparedIr::from_wav_bytes`, and engages gate (non-default threshold) and EQ (non-default
  low-shelf gain) — then measures `Chain::process` end to end, single-core-pinned, 5,000 warmup +
  100,000 measured 64-sample blocks. **On this sandbox: p50 21–24%, p99.9 61–76% across four runs
  (noisy tail, same jitter signature R-4's own bench notes), max spiking as high as 500%+ on the
  noisiest run.** p99.9 is 2.5–3x the 25% budget even accounting for this sandbox's weaker
  single-core performance than the reference machine. R-4 alone (~44–55% in isolation, see the R-4
  bullet above) already explains most of this; R-8's stage-level fix does not fully offset it once
  gate/trim/eq/out's own
  (unmeasured-in-isolation, but nonzero) per-block cost is added on top. **Not verified:** what this
  figure is on the actual §2 reference machine — that run has not happened, and per D-2.1 nothing
  short of it can be the certified figure.

**Acceptance — not met this session.** FR-NAM-020 (LSTM) closes. NFR-PERF-010 does **not** close:
the real assembled chain measured FAIL against its own literal condition, on this sandbox, at the
99.9th percentile. R-4 and R-8 both stay **downgraded, not retired** — R-8's own scheduling defect
is closed and R-4 measured a real if partial improvement, but neither the isolated NAM figure nor
the assembled-chain figure is under budget yet, and the certified reference-machine run that could
retire either risk has not been performed. Closing NFR-PERF-010 for real needs further engine-level
cost reduction (candidates: `namir-nam`'s own remaining scalar-loop cost outside `axpy`, per-stage
overhead in gate/trim/eq/out, or the R-8 doc comment's still-open 2048-block/192 kHz gap) plus a
confirmatory run on the pinned reference machine — both left open, not silently deferred.

**Certified reference-machine pass (this session, 2026-08-06).** This session ran directly on
`docs/02-architecture.md` §2's pinned machine — confirmed, not assumed: `wmic`/`Get-CimInstance`
on this host report an AMD Ryzen 9 5950X (16C/32T, 3401 MHz base) and 64 GB RAM, matching §2
exactly, running Windows 11 Pro build 26200. Every figure below is therefore the certified
NFR-PERF-010 number the prior session's own numbers explicitly said they were not.

*Gates first:* `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (314 tests), `xtask layering`, `xtask params-lock`, and `cargo
deny check` all pass clean on this machine before any benchmark was trusted.

*Baseline, before this session's changes (measured here first, to have a real reference-machine
"before" rather than trusting the sandbox numbers above as a proxy):*

| Benchmark | p50 | p99.9 |
|---|---|---|
| `wavenet_inner_loops` (NAM alone, standard shape) | ~12.1% | ~33.4–34.3% |
| `namir-ir/examples/perf_bench 96000 64` (IR alone, 48 kHz condition, mono) | — | ~25.4–25.8% |
| `six_stage_chain` (real assembled chain, stereo IR, gate+EQ active) | ~12.7% | ~52.2% |

Already a clean, reproducible FAIL against the 25% budget — the isolated NAM figure alone (~33%)
exceeds it, exactly as S-1's spike found, now confirmed on the actual reference machine rather than
inferred from a weaker sandbox.

*Two fixes landed this session, both root-caused by reading the hot loops directly rather than
guessing:*

1. **`namir-nam/src/wavenet.rs`: vectorized `Tanh`/`Sigmoid`, not just `axpy`.** R-4's original
   vectorization (prior sessions) covered every multiply-accumulate loop but left the file's only
   transcendental-heavy loop scalar: the standard WaveNet shape's two ten-layer arrays call
   `f32::tanh()` roughly 15,360 times per 64-sample block, none of it going through `axpy` (`axpy`
   only handles multiply-accumulate, not a nonlinearity). Fixed via a new `vectorize_unary` helper
   plus `wide::f32x8`'s own `tanh`/`exp` methods — genuinely vectorized (not a per-lane scalar
   fallback; `wide`'s source documents sub-ULP accuracy via range-reduced polynomial/`exp_m1`),
   confirmed safe against the -100 dB numeric-parity test (`tests/fixtures.rs`), which reads
   **-130.8 dB** after the change (was passing before too, but this is the actual post-change
   margin, with plenty of headroom). The same treatment was applied to `Layer::apply_into`'s
   head-sum and residual accumulation (`axpy(w=1.0)` instead of a hand-rolled `+=` loop) for
   consistency; measured to make no further difference (LLVM already auto-vectorized those simple
   loops), so it's a robustness/consistency change, not a counted performance win.
2. **`namir-ir/src/convolver.rs`: vectorized the head partition's direct-convolution loop.** The
   original loop computed `y[i] = sum_k head[k] * head_history[(t-k) % head_len]` per *sample*,
   with a modulo *per tap* — `namir-nam`'s `Conv1D::apply_into` had already solved exactly this
   shape (a causal FIR) with a history-plus-input `padded` buffer and a per-tap vectorized `axpy`
   over the whole block; this session ported that technique here, replacing the ring-buffer/modulo
   scheme with a linear history buffer (`ChannelState.head_history`) and a reused `head_scratch`
   padded buffer, sized once per channel from a newly-stored `PreparedChannel::block_size` (the
   caller's declared max per-call `input.len()`, not previously kept). Added `wide` as a direct
   dependency of `namir-ir` for this (same crate/version/rationale `namir-nam` already uses it
   with; `cargo deny check` and `xtask layering` both stay clean). All 39 `namir-ir` tests pass
   unchanged, including `partitioned_matches_direct_across_fixtures_block_sizes_and_ir_lengths`
   (the -100 dB check against `direct_convolve`, D-9.5's permanent reference).

*Measured impact — a genuine, if partial, win, established by a controlled A/B on this machine
(not a before/after across separate sessions, which this same milestone's history shows can be
confounded by the reference machine's own run-to-run variance — see the note below):*

| Benchmark | p50 before → after | p99.9 before → after |
|---|---|---|
| `wavenet_inner_loops` (NAM alone) | ~12.1% → ~8.8–9.2% (real, reproducible ~25% relative cut) | ~33–34% → ~30–35% (unchanged within run-to-run noise) |
| `perf_bench 96000 64` (IR alone, 48 kHz) | not separately tracked | ~25.4–25.8% → ~24.8–25.0% (no measurable change) |
| `six_stage_chain` (real assembled chain) | ~12.7% → ~10.0–10.1% (stable across 5 repeats) | ~52.2% → ~49.2–49.5% (stable across 5 repeats) |

**Why the head-conv fix barely moved either isolated-IR or chain-level p99.9, despite being a real
compute-cost reduction (verified: same test suite, same numeric parity, fewer scalar ops):**
`perf_bench.rs`'s own module doc comment already explains the mechanism — a partition's FFT
triggers *periodically* (every `size` samples), so the IR benchmark's p99.9 event is a recurring
large-FFT-trigger block, not an average block. The head partition's per-sample cost, vectorized or
not, is small next to a triggering partition's FFT/spectral-multiply/overlap-add cost, so cutting
it reduces the *typical* block's cost (which would show in a p50 if this benchmark tracked one)
without moving the *worst* block's cost at all. The same asymmetry explains why `wavenet_inner_loops`'s
own p99.9 didn't move even though its p50 clearly did: whatever drives that file's tail (cache
effects, branch misprediction on the `w == 0.0` skip checks, or plain OS scheduling jitter on a
general-purpose desktop with no elevated thread priority — thread-priority elevation is explicitly
an M6 `namir-platform` deliverable, not yet built) is apparently insensitive to the steady-state
per-sample compute cost this session's fixes reduced. The chain-level benchmark's own p99.9 *did*
move by a real, reproducible ~3 percentage points (52.2% → 49.2–49.5%, confirmed via a clean
stash/pop A/B on identical hardware in the same session, not a cross-session comparison) — smaller
than the p50 win would suggest, consistent with the tail being only partly, not wholly, explained
by the per-sample costs this session's fixes touched.

**A methodological finding worth recording for future certified runs on this machine:** this
session's first read of the post-fix IR-alone benchmark showed a dramatic ~9.5% p99.9 (vs. the
~25% reported in the table above) — before a controlled, same-session A/B (stash the fix, rebuild,
remeasure; pop it back, rebuild, remeasure) showed the true, reproducible effect was near zero.
The most likely explanation, not confirmed at the time: this is a general-purpose desktop, not a
quieted benchmarking rig — `Get-Process` during this session showed ordinary background load (a
browser, Discord, a stray leftover `find` process from this session's own earlier exploration) and
CPU clock speed readings at or near base (no sustained boost observed), so an isolated run can land
in a favourable clock/scheduling window that a same-day repeat does not reproduce.

**Follow-up, same session: tested that explanation directly, rather than leaving it a guess.** The
operator closed every non-essential background application (Firefox, Discord, WacomCenterUI,
MuseHub — the processes `Get-Process` had flagged) and switched this machine's Windows power mode
from Balanced to Best Performance, then asked for a clean re-run. Result, 5-6 repeats per
benchmark:

| Benchmark | p99.9 (background load, Balanced) | p99.9 (quiet, Best Performance) |
|---|---|---|
| `wavenet_inner_loops` (NAM alone) | ~30–35% (visibly spread across repeats) | **~29.8–30.1%** (tight, all 5 runs within 0.3 pp) |
| `perf_bench 96000 64` (IR alone, 48 kHz) | ~24.8–25.0% | **~24.6–25.0%** (unchanged) |
| `six_stage_chain` (real assembled chain) | ~49.2–49.5% | **~48.2–48.7%** (tight, all 6 runs within 0.5 pp) |

**The headline finding: quieting the machine made every number far more *reproducible* (run-to-run
spread collapsed from several points to a few tenths of a point) but barely changed the *level* of
any of them** — the chain's p99.9 moved by about half a point, nowhere near enough to explain the
2x-over-budget gap, and NAM/IR alone moved even less. This is a real answer, not a non-result: it
means the tail this benchmark measures is **inherent to the workload itself, not an artifact of
this being a shared desktop with other software running**. For the IR-alone case this is exactly
what `perf_bench.rs`'s own doc comment already predicted (the p99.9 event is a periodically
recurring large-FFT-partition trigger, a property of the schedule, not the OS); for NAM and the
full chain it now rules out "background contention" as the explanation for the earlier session's
spread, leaving cache effects, branch prediction, or genuine compute-cost variance across different
regions of the WaveNet's own weight-dependent control flow (`if w == 0.0 { continue; }` in `axpy`'s
callers) as the more likely remaining candidates — still unconfirmed, but now narrowed by ruling
out the OS-level explanation with a real experiment rather than a guess. Practically: **the earlier
"background load" figures (~49.2–49.5%, ~30–35%) were already close enough to trustworthy that this
session's Acceptance verdict does not change** — the quiet-machine numbers below supersede them as
the more precise reading, but not as a different verdict.

**Acceptance — still not met, now on certified, quiet-machine reference numbers.** FR-NAM-020
(LSTM) stays closed (unaffected by this session). NFR-PERF-010 does **not** close: the real
assembled chain measures **p99.9 ≈ 48.2–48.7%** of one core against a 25% budget on
`docs/02-architecture.md` §2's own pinned machine, quiet and at Best Performance — roughly 2x over,
a real but small improvement over this session's own measured ~52.2% pre-fix baseline (itself also
now confirmed not to be an artifact of background load) but nowhere near closing the gap. R-4 and
R-8 both stay **downgraded, not retired**: R-4's vectorization now provably covers every hot loop
this session could find in `wavenet.rs` (not just `axpy`), and R-8's scheduling defect is unaffected
by this session (untouched); neither the isolated NAM figure (~29.8-30.1% p99.9, still exceeds
budget alone) nor the assembled-chain figure is under budget. Closing NFR-PERF-010 for real now
needs a **structural** reduction, not further micro-vectorization or environment tuning — both
levers this session had available are now spent and both moved the needle only modestly. Candidates
worth trying next: reducing NAM's own tanh call count (e.g. batching activations across layers
rather than per-layer, or accepting a coarser/cheaper approximation — there is `-130.8 dB` of
headroom against the `-100 dB` parity bar to spend before that's even a risk), reducing per-block
overhead in gate/trim/eq/out (never individually measured in isolation this session), or revisiting
the IR schedule's own partition-size/growth-factor defaults per-condition rather than only its
stagger phase (R-8's fix; the trigger-cost magnitude itself is untouched by staggering). M6's
thread-priority elevation is no longer the leading candidate for closing this gap, now that a real
experiment has shown the tail survives a quiet, performance-mode machine — still worth building for
other reasons, but not expected to be what retires NFR-PERF-010.

### M3 continuation: the AVX2 finding, and a hypothesis this milestone got wrong

Three further results, recorded in the order they were established because the third corrects the
first two.

**1. The x86-64 baseline was never set, and that was the single largest cost in the milestone.**
No `target-cpu` existed anywhere in the repository, so the workspace compiled to bare x86-64 —
SSE2, no AVX, no FMA — and every `wide::f32x8` operation became two 4-lane SSE ops rather than one
8-wide AVX one. `wavenet.rs`'s own R-4 note had assumed otherwise and nothing had checked. Setting
`x86-64-v3` (now **D-2.3**, `.cargo/config.toml`, scoped to `cfg(target_arch = "x86_64")` so
aarch64 is unaffected) measured, on the §2 reference machine:

| | p50 | p99.9 |
|---|---|---|
| NAM alone, SSE2 | 8.77% | 30.3% |
| NAM alone, AVX2+FMA | ~6.5% | **~10.5%** |
| Assembled chain, SSE2 | 10.34% | 43–49% |
| Assembled chain, AVX2+FMA | **~7.9%** | ~38.9% |

Numeric parity re-verified under FMA at **-130.8 dB**, unchanged. Note the asymmetry: NAM's own
p99.9 fell 3x, but the chain's fell far less — because the chain's tail is now **IR-dominated**,
which reorders the remaining work.

**2. Per-stage attribution closed the "unmeasured-in-isolation" gap.** `per_stage_cost.rs` measures
each stage's own `process` cost. gate/trim/eq/out together are **~0.2% p50 / ~0.5% p99.9** of the
block period — there is no recoverable budget there, and that candidate from the paragraph above is
now closed rather than speculative. NAM is 89% of a typical block. The six isolated p50 figures sum
to 9.80% against the chain's own ~9.9% p50, which validates the attribution method.

**3. The tail is *not* environmental — this milestone's own earlier hypothesis, refuted by
experiment.** The paragraphs above lean toward "cache effects, branch prediction, or genuine
compute variance… still unconfirmed", after a quiet-machine test had already ruled out background
load. A sharper experiment (`tail_structure.rs`, which retains per-block durations in *acquisition
order* rather than sorting them immediately as every other benchmark here does) settles it against
the environmental reading on three independent discriminators at once:

- **Contiguous runs: 1005 slow blocks, mean run length 1.00, longest 1.** Not one pair of adjacent
  slow blocks in 100,000. Contention episodes last milliseconds — tens to thousands of consecutive
  64-sample blocks — so this alone essentially excludes them.
- **Lag-1 autocorrelation: 0.0945.** Blocks are independent.
- **Residues mod the IR schedule period (128 blocks): chi2 = 1950** against ~128 for uniform.
  Strongly periodic, locked to the convolution schedule — which nothing in the OS knows about.

The duration histogram confirms it by shape: discrete modes (63,932 blocks at 120–140 µs; 6,288 at
260–280 µs; 1,642 at 400–420 µs), which is what a fixed partition schedule produces and not what
scheduler noise imitates. **So the tail is code-driven, periodic, and therefore genuinely
optimisable** — a better outcome than the environmental reading, which would have meant the metric
was partly unfixable.

**The contradiction this leaves open, recorded rather than resolved.** A schedule-periodic tail
ought to respond to the schedule's own parameters, and it does not: sweeping `max_partition` from
8192 through 4096 to 2048 left the IR stage's p99.9 flat (23.42 / 23.47 / 23.43%), and
`build_schedule`'s cross-size decorrelation fix — which provably cut the worst block's *modelled*
FFT load from 11.893x the mean to 6.793x, verified by its own permanent regression test — moved it
only ~24.8% → ~23.4%. Two interventions the model says should have worked, didn't. The
`2P·log2(2P)` cost model behind both predictions is therefore incomplete; the likeliest omissions
are real FFT constant factors at small sizes and the per-sample, per-partition bookkeeping in
`PreparedChannel::process_block`, which scales with partition *count* (and so grows as
`max_partition` falls, potentially cancelling the smaller spike). **Resolving that model is
prerequisite to further IR tail work** — the next optimisation must not be designed against a model
already known to mispredict.

### M3 close-out: the tail was never Namir's

The contradiction recorded immediately above is resolved, and the resolution invalidates most of
the performance numbers in the three sections preceding this one. They are left in place as an
honest record of how the milestone actually proceeded; **this section supersedes them.**

**What the tail actually was.** An elevated `xperf -on Latency` trace on the §2 reference machine
found `dxgkrnl.sys` — the DirectX/GPU kernel driver — issuing **6,494 interrupts of 128–512 µs over
39.4 seconds (~165/second)**, with its ISR time landing on **CPU 0 exclusively**: 1,670,068 µs on
CPU 0 and exactly 0 µs on all 31 other logical CPUs, a steady ~4.2% of that core's wall clock.
Every benchmark in this workspace pinned to `get_core_ids().next()` — CPU 0. ISRs execute at DIRQL,
above every thread priority, which is why raising the process to Windows `High` priority had
changed nothing when that was tried. The same trace showed CPU 2 carries the heaviest kernel DPC
load (50,151 µs of `ntoskrnl.exe`), so it is a poor second choice; benchmarks now default to core 4
and expose `NAMIR_PIN_CORE`.

Interleaved, same session, same binary:

| | core 0 | core 4 | core 8 |
|---|---|---|---|
| IR stage alone, p99.9 | 258 µs | **55 µs** | **56 µs** |
| assembled chain, p99.9 | 34.8% | **19.4%** | 24.5% |

On a clean core the IR stage's p99 (51.6 µs) and p99.9 (55.0 µs) converge — the tight
schedule-bounded distribution the cost model predicted throughout. **The model was right; the
measurement was contaminated.** Two corroborating results from the same close-out: a complete
8192-point FFT trigger costs only ~31 µs (so the FFT was never ~265 µs of anything), and its
cold/warm cache ratio is 1.00–1.06x at every partition size, which independently killed a
memory-bound account that had looked quantitatively convincing to within 10%.

**Why raw p99.9 cannot certify this requirement on this machine.** Even after moving off CPU 0,
ten consecutive runs of the chain benchmark with nothing changed between them measured p99.9
anywhere from **17% to 52%** of the block period while p50 stayed pinned near 7.8%. A statistic
that moves 3x with ambient machine state cannot gate a requirement.

**The figure that does hold.** `namir-engine/benches/tail_structure.rs` now reports a
contamination-immune estimator: interference is additive and aperiodic, while the IR partition
schedule is periodic with period `largest_partition / block_size` (128 blocks at this condition),
so each residue recurs ~781 times per run and its *cheapest* occurrence is the one no interrupt,
preemption or frequency excursion landed on. Nothing can make a block finish faster than its own
arithmetic allows, so the per-residue minimum is a tight lower bound, and the maximum over residues
is the schedule's true worst-case block. Measured four times while the machine was visibly busy
(p50 14–19%, raw p99.9 47–49%): **15.33 / 15.38 / 15.23 / 15.11%** of the block period — stable to
±0.14 points across runs whose raw figures spanned several points, and reproducing at 15.2–15.5%
across six different cores, odd and even alike.

**So the assembled six-stage chain's own worst-case block costs ~15.2% of the block period against
NFR-PERF-010's 25% budget.**

**The apparent choice between two statistics was a false dilemma, and measuring properly dissolved
it.** This section originally left NFR-PERF-010 open on the grounds that Namir's own worst-case
block (~15.2%, reproducible) and the literal p99.9 (17–52%, unstable) disagreed about whether the
requirement passed, and that picking the passing one would be choosing the flattering measurement.

The instability turned out to be almost entirely **this project's own tooling**. The later
measurement sessions ran concurrently with background analysis agents and repeated `cargo` builds;
with those gone, and nothing else changed, the same benchmark on the same machine reports:

| | during concurrent tooling | machine actually quiet |
|---|---|---|
| p50 | 14–19% | **7.75–7.91%** |
| p99 | 40–44% | **15.28–15.47%** |
| **raw p99.9 (D-2.2's own metric)** | 47–52% | **16.45–17.08%** |
| per-residue-minimum estimator | 15.1–15.4% | 14.94–15.10% |

So the literal metric passes the 25% budget with room to spare, and now agrees with the
contamination-immune estimator to within ~1.8 points. There was never a need to choose: p99.9 was
unstable because of *how* it was measured, not because it was the wrong quantity.

**Decision recorded as D-2.4** (`02-architecture.md` §2): D-2.2's p99.9 gate is kept exactly as
written; what is added is the measurement conditions under which it is valid (pin away from
device-ISR cores, verify the machine is actually quiet rather than assuming it, at least five
repetitions with the spread reported) plus a mandatory validity check — run the estimator alongside
and **discard any run whose raw p99.9 substantially exceeds it**, because that run was contaminated.
The estimator is promoted to a permanent part of the methodology as the instrument that tells you
whether a p99.9 reading means anything, not as a replacement for it. This also keeps the
requirement verifiable exactly as the FRS specifies ("*Verify:* B, as a CI regression gate"), which
a hand-computed estimator would not be.

**Acceptance — FR-NAM-020 and NFR-PERF-010 both close.** The assembled six-stage chain measures
**p99.9 = 16.45–17.08% of one core against a 25% budget**, on `docs/02-architecture.md` §2's pinned
reference machine, across five repetitions under D-2.4's conditions, cross-checked against an
estimator that reads 14.94–15.10% on the same runs. M3's exit criterion is met.

R-4 **retires**: vectorization's benefit is now directly measured rather than inferred — D-2.3's
AVX2/FMA baseline took the NAM stage from p99.9 30.3% to ~10.5%, with numeric parity re-verified at
−130.8 dB. R-8 **retires as a scheduling defect**: `build_schedule`'s cross-size phase alignment is
fixed with a permanent quantitative regression test (worst-block modelled FFT load 11.893x → 6.793x
the mean, against a 6.507x floor), and the residual tail it was suspected of causing is now
attributed to the GPU driver instead.

**A methodological note this milestone earned the hard way.** Across this close-out, four separate
conclusions were announced from single unreplicated measurements and each had to be retracted: an
IR figure that did not reproduce, an "environmental hypothesis refuted" verdict that was an
over-reading of a periodicity test, a cache-locality account whose arithmetic matched the observed
figure to within 10% and was still wrong, and a chain PASS at ~17% that failed to reproduce at ~24%
an hour later. The standing rule that follows: **on this machine, no performance claim from a
single run, and prefer a statistic that is immune to contamination over a quiet-machine ritual that
cannot be guaranteed.**

---

## 8. M4 — Resource handover, worker, and cross-instance sharing

**Size: L.** **Depends on:** M2 (needs real stages with the dual-resource shape already built in).
**Blocks:** M5.

> ⚠️ **Read both status sections at the end of this section before trusting anything here about
> R-7.** The "Acceptance" paragraph below predicts R-7 retires; the first status section records
> that the measurement did *not* support that; the close-out records that it does, once the
> mitigation the measurement pointed at was built. The original text is left unedited as the record
> of what the milestone expected before it measured anything.

**Deliverables:**

- `namir-engine`'s real **D-7.2 SPSC command ring** (wait-free from the audio thread's side,
  fixed-size records, no owned heap data) and **D-7.3 lock-free telemetry ring** (replacing
  today's single-threaded trait-shape placeholder with the real cross-thread structure it was
  always meant to be).
- **`namir-worker`** implementing **D-8.1's four-step handover** (prepare → offer → crossfade →
  retire) exactly as specified, including the return ring for the audio thread to push retired
  `Arc`s into without ever dropping the last reference itself.
- **D-8.2's process-global resource cache** — `content hash → Weak<Prepared*>`, worker-only,
  mutex-guarded, never touched by the audio thread.

**Acceptance:** FR-NAM-070, FR-IR-060 (crossfaded, glitch-free model/IR swap) close. FR-CLAP-090's
cross-instance weight sharing becomes achievable (though it isn't exercised for real until M6's
`namir-clap`). **R-7 retires**: the crossfade's transient 2× cost is now something the benchmark
harness can actually measure, not a stated concern.
---

### M4 status — this session, 2026-08-06

Appended rather than rewriting the Deliverables and Acceptance text above, following §7's own
convention. Where this section and the text above disagree, this section is what happened.

**All three deliverables landed.** `namir-engine` gained D-7.2's SPSC command ring and D-8.1's
return ring (`ring.rs`, on the new `rtrb` dependency — `02-architecture.md` §17), D-7.3's real
lock-free telemetry ring (`telemetry_ring.rs`, plain `AtomicU64`s and no dependency at all), and
`AudioEngine`, which owns the rings so `Chain` stays the pure DSP object `six_stage_chain.rs`
measures. `namir-worker` exists, with D-7.1's pool, D-8.2's cache, D-16.3's per-job panic isolation,
and the worker halves of D-8.1 — and **no third-party dependency of its own**.

**The codebase's one known P1 violation is closed, and a second one nobody had documented with it.**
M2's `nam.rs`/`ir.rs` dropped the outgoing slot on the audio thread the instant a handover
completed; both files admitted it in their module docs. It is now a move into a one-slot retire pen
that the return ring drains. The second site turned up while wiring the first: an offer arriving
mid-fade *replaced* the slot still fading in, and replacing it dropped it — harmless while the
loader was documented non-RT and only tests called it, a real violation once offers install on the
audio thread. Both are closed the same way.

The evidence is not an assertion. The tightened tests were committed **first**, failing with 21 and
7 deallocations respectively, and the fix committed second — NFR-QUAL-020's "evidenced by commit
order", honoured literally. Both stages now drive a complete real-to-real handover to completion
*inside* `rt_harness::audio_section`, which they demonstrably could not before.

**Two decisions needed correcting, not merely implementing** — both recorded as
`*Consequence (added M4)*` notes in `02-architecture.md`:

- **D-8.1 understates step 1.** "The stage is prepared with capacity for two live resources
  precisely so this needs no allocation" is true of the slot *array* and false of the slot
  *contents*: installing a bare `Arc<PreparedNam>` still builds this instance's `NamState` and, at a
  mismatched rate, a whole `rubato` resampler pair. So a command carries a built, boxed slot, which
  also makes D-7.2's "carries a pointer, never the model" literally true rather than an argument.
- **D-8.2's cache key is insufficient as written.** `PreparedIr::from_wav_bytes` bakes in both the
  engine rate and the block size, and `process_block` **asserts** the block size its schedule was
  built for — so a hit keyed on content hash alone could hand one instance an IR prepared for
  another's smaller block, and the failure mode is a *panic on the audio thread*, not a wrong sound.
  Keyed `(hash, rate, block_size)` instead.

**Acceptance — FR-NAM-070 and FR-IR-060 close; R-7 does not retire.**

FR-NAM-070 and FR-IR-060 are each verified by their own literal *Verify: I* method — swapped under a
continuous sine, asserting no discontinuity beyond a stated threshold and no dropout, with the whole
run inside the D-7.5 harness. FR-CLAP-090's sharing mechanism is built and tested at the worker
level and remains, as the Acceptance text above already said, *achievable* rather than exercised
until M6.

**R-7 was measured and stays open, which is the one place this milestone's original Acceptance text
was wrong.** It predicted retirement on the grounds that the transient would become measurable. It
is now measured, and the measurement does not support retiring it — though it does narrow it
sharply. On the §2 reference machine under D-2.4's conditions, **six retained repetitions of ten**
(four discarded on D-2.4's own estimator check):

| Arm | p99.9 across retained runs | vs. the 25% budget |
|---|---|---|
| A — steady, no handover | **16.25–16.51%** (estimator 14.60–15.29%) | pass; reproduces M3's certified figure |
| B — NAM handover, all four rates | **21.32–24.31%** | **pass** |
| C — IR handover, all four rates | **17.97–24.63%** | **pass**, with little margin |
| D — NAM **and** IR simultaneously | **25.06–31.49%** | **fail at every rate** |

So R-7's own wording — "crossfade doubles NAM cost transiently, eating the NFR-PERF-010 budget" — is
**half right, and the half that is wrong is the half it names.** A NAM handover alone stays inside
budget even at a 94%-duty swap rate faster than any human audition. What exceeds the budget is two
stages crossfading at once, which R-7 does not mention. Arm D's spread is under 0.6 points across
six runs — tighter than arm A's — so this is a property of the workload, not noise.

The mitigation is named in §22 and deliberately **not** built here: a worker-side rule that a NAM
and an IR handover are never in flight simultaneously would eliminate arm D by construction, for one
state bit per instance. The operator's call was to certify the measurement first rather than fix and
measure in the same pass. Shortening the crossfade toward FR-NAM-070's 5 ms floor is *not* an
alternative — it reduces the transient's duty cycle, not its 2× peak.

**What M4 does not close, stated rather than left to inference:**

- **NFR-RT-010 is Partial, not Done.** M4 closed the one known audio-thread allocation and proves a
  complete handover allocation-free, but the requirement's *Verify* clause also demands a stress
  test "with concurrent model loading, preset recall and library scanning". Preset recall is M5's
  `namir-state` and scanning is M5's `namir-library`; neither exists, so only the model-loading axis
  is covered.
- **NFR-PERF-050** (500 ms for a 50 MB load) now has a worker to measure but no benchmark yet.
- **The mobile cross-builds are unverified for `namir-worker`.** `-p namir-worker` is added to both
  CI jobs, and nothing in the crate should block either target, but those jobs run on
  ubuntu/macOS-hosted runners which the concurrent GitHub incident left stuck — so this is claimed
  by inspection, not by a green run.

**Two methodology notes this milestone earned.**

First, D-2.4's estimator has a limit worth recording: it is **not** a valid validity check for the
IR-swapping arms. It assumes cost is periodic in block index, and recycled IR slots each carry their
own stream position, so the expensive partition triggers land on varying residues. The tell was
unmistakable — arm C's estimator reads *below* arm A's, which is impossible for a lower bound on the
same schedule doing strictly more work. Arms C and D are therefore validity-checked against arm A's
estimator, measured in the same run.

Second, and more embarrassing: **two of the four discarded repetitions were contaminated by this
session's own polling** — a shell command every few seconds to check whether the run had finished.
That is exactly the phenomenon M3's close-out recorded ("the later measurement sessions ran
concurrently with background analysis agents and repeated `cargo` builds"), reproduced by someone
who had just finished reading the warning about it. D-2.4's "no unrelated load on the machine,
verified rather than assumed" includes the tooling watching the benchmark.

---

### M4 close-out: R-7 mitigated, measured again, and retired

The status section above records R-7 as open with a quantified cause. That cause has since been
removed, and this section supersedes that verdict — but not the measurement it rests on, which
stands and is what identified the fix.

**What was built.** `namir-worker`'s `Instance` now serialises cross-target handovers: before
offering one for a target, it waits out any handover it recently offered for the *other*. The wait
happens on a worker thread, which D-7.1 explicitly permits workers to do, and sits immediately
before the offer rather than at the top of `load()` — preparation has already consumed real time and
that time counts toward the other stage's fade, so waiting first would charge the delay twice.

It is a **timer**, not telemetry feedback, and that is a real constraint rather than a preference.
The closed-loop signal exists (`telemetry.*.handover_active`), but it cannot stand alone: the *first*
load into an empty stage retires nothing, and between submission and the audio thread's next block
it reports no fade in flight — so a purely feedback-driven rule races and lets both through anyway.
A timer needs no feedback, cannot deadlock, and if the audio thread stalls it expires regardless,
which is the right failure mode.

`HANDOVER_CROSSFADE_MS` was promoted to a single public constant in `namir-engine` in the same pass.
It had been privately duplicated in `nam.rs` and `ir.rs` — two copies of a figure the two stages must
agree on — and the worker needed a third, which would have compounded the problem rather than
inherited it.

**What it measures.** Arms D (unserialised) and E (serialised) run **interleaved in the same
process**, which is the only comparison form this machine supports reliably. Six retained
repetitions of nine, D-2.4 conditions, arm A reading 16.04–16.84% throughout:

| Handover rate | D — both at once | E — serialised | measured overlap, D → E |
|---|---|---|---|
| every 32 blocks | 30.08–31.26% | **23.20–24.63%** | 43.8% → **0%** |
| every 64 blocks | 29.66–30.25% | **22.20–23.31%** | 21.9% → **0%** |
| every 128 blocks | 28.77–29.47% | **22.44–23.49%** | 10.9% → **0%** |

A 6–7 point reduction, and every rate lands inside NFR-PERF-010's 25% budget. The `overlap` column
is the check that the rule is actually in force rather than assumed — it is measured from the
stages' own telemetry, not from the bench's intent.

**Acceptance — R-7 retires.** Every condition the system can actually produce is within budget, and
`02-architecture.md` §22's row is updated accordingly.

**Two residuals recorded rather than glossed over.**

1. **The margin is thin: about 0.4 points** at the worst achievable condition (24.63% against 25%).
   This is now the path any future increase in NAM or IR per-block cost will breach first, which is
   the reason `handover_crossfade.rs` is a permanent target rather than a one-off measurement.
2. **Arm E at `period 16` still reads 26.99–31.89% with 75% overlap, and that is not a failure of
   the mitigation.** `namir-engine` may not depend on `namir-worker` (D-5.1), so arm E reproduces
   the rule's *effect* with a fixed half-period offset rather than calling the rule. At period 16
   half a period is 8 blocks against a 15-block fade, so the simulation cannot serialise there. The
   real rule does not offset, it **waits** — at least 25 ms, about 19 blocks, which exceeds the fade
   — so the overlapping condition that row depicts is one the worker cannot produce. The row is kept
   in the output rather than suppressed, because a benchmark that quietly hid the case where its own
   approximation breaks down would be worse than one that shows it.

**A distinction worth keeping straight, since one run does not establish both:** arm E measures what
serialising costs the *audio thread*; `namir-worker`'s three unit tests measure that the rule
*holds* (cross-target handovers separated by at least the crossfade, same-target reloads not delayed,
a failed load not arming the timer). Neither piece of evidence substitutes for the other.


---

## 9. M5 — State, presets, and library

**Size: L.** **Depends on:** M2 (real stage parameters to serialize) and M4 (the resource cache
resolves file references off the audio thread). **Blocks:** M6 loosely (the CLAP adapter needs
state save/load; the app doesn't strictly need the library to be minimally functional, but
FR-LIB-* is Must for 1.0 regardless).

**Deliverables:**

- **`namir-state`** — D-11.1's JSON preset/state format (pretty-printed, stable key ordering,
  `format_version`), D-11.2's tolerant/versioned deserialization (unknown fields preserved and
  written back, missing fields default), D-11.3's three-way file reference (library-relative
  path, absolute path, `ContentHash`) with FR-STATE-070's resolution order and its
  locate-manually fallback.
- **`namir-library`** — D-12.1's on-disk index (path/size/mtime/hash/metadata,
  incrementally updated), D-12.2's cancellable off-thread scanning, and **AQ-3's decision**
  (embedded index store, constrained to no copyleft/no C-or-C++ dependency, corruption degrades
  to full rescan rather than crash) needs to actually be made here — it's the one open item in
  this milestone's path that isn't already answered by the architecture document.

**Acceptance:** FRS §5.9 (STATE) and §5.10 (LIB) close in full — these were 100%/Not-started at
M0's audit, the largest single block of untouched Must requirements outside the product stages
themselves.

---

### M5 status — this session, 2026-08-07

Appended rather than rewriting the Deliverables and Acceptance text above, following §7's own
convention (M4's own status sections are the precedent this one follows).

**Both deliverables landed, plus the composition layer the original text under-scoped.** The
Deliverables above name `namir-state` and `namir-library` but not what actually turns them into
FR-STATE-030/050/070's live behaviour: `namir-engine` gained `Command::Unload` (FR-STATE-070's "the
state shall load with that stage empty" had no engine mechanism until this milestone — the
crossfade machinery already treated a `None` slot as dry passthrough, so this needed no new DSP,
only new plumbing), and `namir-worker` gained `Instance::unload`, `Instance::recall`
(FR-STATE-030/050) and `library::LibraryService` (D-12.2's "cancellable worker job" finally whole —
the mechanism in `namir-library`, the thread in `namir-worker`, exactly as the M2 seam analysis
below predicted). None of that was a separate milestone deliverable in the original scope; it
turned out to be necessary to close the requirements the original Acceptance line claims.

**AQ-3 decided:** a single pretty-printed JSON document, atomic replace (temp file, `sync_all`,
`rename`). `docs/02-architecture.md` D-12.3 records the full Decision/Rationale/Rejected form; §15
below is struck through accordingly.

**Should-scope closed too:** FR-STATE-080 (embedded model/IR data, `base64` — the one new
third-party dependency this milestone added, `namir-core`/`namir-library` add none), FR-LIB-050
(favourites, keyed by content hash so a mark survives a file moving), FR-LIB-060 (ordered
next/previous over a caller-supplied search result).

**Six corrections to the governing documents, each recorded as a `*Consequence (added M5)*` note
in `docs/02-architecture.md` at the section the correction belongs to, not collected in one place**
(the same style the twelve pre-existing M4-and-earlier notes already use): D-5.1's `namir-library`
row contradicted itself (Platform code? said "via `namir-platform`", May-depend-on omitted it,
`xtask layering` enforces the latter — corrected to "No"); FR-LIB-070 and D-12.1 as originally
written contradicted each other (a same-length edit landing within one mtime tick was invisible to
the literal "compare size and mtime" rule — closed by the mtime-settling-window fix, R3 below);
D-2.1's "never wall-clock" rule is scoped to audio-thread per-block budgets by new decision D-2.5,
since NFR-PERF-030/040/050/060 are wall-clock by their own FRS wording and this milestone is the
first to actually hit that gap; `.gitattributes`'s `* text=auto eol=lf` would have silently repaired
a CRLF-emitting serialiser bug in the checked-in NFR-PORT-050 corpus, closed by marking
`*.namirpreset binary`; FR-STATE-070 was silent on which of several configured library roots a
relative path resolves against and on what a hash-mismatched path hit means, both resolved (no root
identity is stored; a mismatch falls through, never substitutes); and global bypass/output ceiling
have no `ParamDescriptor` home, flagged (not solved) as a decision M6's CLAP adapter needs to make.

**`docs/04-state-and-preset-format.md` is new** — NFR-DOC-010's format reference, written to the
level a third party can implement a reader from alone. Both of NFR-DOC-010's and FR-STATE-040's
manual tests (`docs/manual-tests/`) were executed, not merely scripted: a Python reader written
using *only* that document (no Rust source) correctly extracted every parameter and both file
references from a document `xtask preset` (new subcommand, since no product shell exists yet to
produce one) generated; a hand-edited copy diffed to exactly one changed line and reloaded with the
edit in effect and zero warnings.

**Five red-first pairs, three genuinely so.** R1 (unknown-field preservation via the `Document`
carrier, not `#[serde(flatten)]`) and R3 (D-12.1's mtime-settling window) were both real: the
naive implementation the requirement's literal wording suggests is provably wrong, demonstrated by
a failing commit before the fix. `Command::Unload` (R2 in the build order, not one of the plan's
five lettered pairs) was likewise genuine — `NamStage`/`IrStage::unload` stubbed as no-ops first,
proven to fail the crossfade-to-dry assertion, then implemented. **R4 (preset recall serialisation)
and R5 (the three-axis stress test) were not manufactured reds, and said so at the time rather than
faked for form:** `Instance::recall` is one function with no thread spawned inside it, so the
tempting parallel-submission bypass R-7's rule warns against isn't merely avoided by discipline,
it isn't expressible without restructuring `Instance` first — forcing an artificial failing version
of that shape would have been exactly the "manufacturing a red where the first implementation would
have passed" trap the plan itself warns against. Both landed green on arrival, with the regression
test included as real evidence regardless.

**NFR-RT-010 moves Partial → Done.** `crates/namir-worker/tests/rt_stress.rs` runs model loading,
preset recall and library scanning concurrently against a live `AudioEngine` inside D-7.5's
`audio_section` for two seconds — the two axes M4 recorded as the reason it could not close. Zero
allocations, zero dropout, zero panics, every produced error catalogue-coded, no block over 200x its
period, and counters proving all three axes genuinely ran (>=3 loads, >=3 recalls, >=1 completed
scan) — stable across four repeated local runs, debug and release.

**NFR-PERF-050 and NFR-PERF-060 measured, both comfortably inside budget.**
`namir-worker/benches/resource_load.rs`: a standard model p50 ~700 us / p99 ~950 us; a 2 s stereo IR
p50 ~4.4 ms / p99 ~5 ms; a ~50 MB uncalibrated worst-case fixture (not a shape the NAM ecosystem
actually produces, measured only because the requirement states its ceiling in file-size terms) p50
~125 ms / p99 ~128 ms — roughly 4x headroom against the 500 ms ceiling even on the pathological
case. `namir-library/benches/library_scan.rs`: arm C (incremental, unchanged, 5 repetitions)
22.1–25.8 ms against the 2 s ceiling; arm D (1% modified) 34–36 ms with all 100 hash-change
assertions passing; FR-LIB-030 conclusive both full local runs (`max(C) < min(B)`, no overlap).
Arm A (full scan, first touch) read ~53–55 s both runs — consistent with this workspace's own
documented Defender-contamination pattern on a burst of just-written files, reported per that arm's
own "not gated" status rather than investigated further.

**R-7 re-run, and the reason it needed to be:** preset recall is a new, more frequent way to reach
R-7's worst condition than a human changing two controls by hand. Five repetitions on this session's
own sandbox (not the §2 reference machine). Two were contaminated by the benchmark's own stated
check — not just arm A's raw/estimator gap, which one contaminated run passed while still showing a
23-point gap on arm C, a stronger signal arm A alone does not catch — and were discarded. Across the
three clean runs, arm E (serialised) at periods 32/64/128 read 22.16–24.35%, matching the original
22.20–24.63% figure closely; period 16 read 26.43–32.36%, matching the already-documented
26.99–31.89% benchmark-simulation artifact from M4's own close-out (not a real-world condition — see
`docs/02-architecture.md` §22's R-7 row). **No evidence of regression; the risk remains retired.**
M5 added no code to the crossfade/chain path this benchmark exercises, so a regression was never
mechanically plausible; this was a check against a new *access pattern*, not a suspected code
change.

**NFR-QUAL-040's second fuzz target, landed in M5 rather than deferred to M7** (the plan's own
decision #3: D-11.1 chose JSON specifically so there would be one parser to harden across every
consumer, and `namir-fixtures::mutate` is already format-agnostic). `crates/namir-state/fuzz`
mirrors `namir-nam/fuzz` exactly. Locally executed for 60 s under nightly + `cargo-fuzz`: this
machine's rustup nightly install bundles no Windows ASan runtime at all (confirmed the *existing*
`load_nam` target hits the identical `STATUS_DLL_NOT_FOUND` here, so this is a pre-existing
environment gap, not new); adding MSVC's own bundled `clang_rt.asan_dynamic-x86_64.dll` to `PATH`
resolved it — 955,695–972,242 executions across two runs, zero crashes. `.github/workflows/fuzz.yml`
gains the matching CI leg (`fuzz-smoke-state`), which is what actually validates execution going
forward.

**CI:** both mobile cross-build jobs' `-p` lists gain `namir-state`/`namir-library`, per D-5.1's
"builds for mobile: yes" and exactly what the pre-existing comment above those jobs predicted this
milestone would do. The no-cxx-toolchain job's dependency audit gains a note: `base64` carries no
build script, so it was never a C++-toolchain risk. Both changes validated by YAML parse only —
this session has no GitHub Actions runner, and the GitHub Actions incident this whole milestone
worked around (branching from `claude/milestone-m4-workflow-handover` rather than `trunk`, since PR
#3/M4 has not merged) blocks a real CI run regardless — claimed by inspection, matching this
project's established convention for CI changes that can't be locally executed.

**What M5 does not close, stated rather than left to inference — the honest restatement of the
Acceptance line above, which over-claims "close in full":**

Of §5.9/§5.10's twelve Must requirements, **seven close in full**, each by its own literal *Verify*
method: FR-STATE-010, FR-STATE-040, FR-STATE-050, FR-LIB-010, FR-LIB-030, FR-LIB-040, FR-LIB-070.
**Five close only their M5-resolvable half**, completed elsewhere: FR-STATE-030 (recall exists and
is tested at the worker level; a host/plugin-instance UI to trigger it is M6), FR-STATE-060
(cross-process, bit-identical restore is the M5-resolvable half of "restart the host" — a literal
host restart needs M6's product shells), FR-STATE-070 (resolution, the locate-manually *data*, and
the embedded fallback all exist; the "offer to locate it manually" *affordance* is M6 UI), FR-LIB-020
(the scan mechanism, cancellation and the pool-driven job are all built and tested; scan-progress
*visibility* is M6 UI). FR-STATE-020 closes its own mechanism (the corpus test harness and its
release gate) but cannot close in full until a first version actually ships — its corpus is defined
over released versions, and none exist yet. Of the four Shoulds, FR-STATE-080 and FR-LIB-050 close;
FR-STATE-090 (factory presets, gated on AQ-4, scheduled M7) and FR-LIB-060's *Verify: M* against a
real UI (the mechanism itself already closes, see above) remain M6/M7.

**NFR-PORT-050 and NFR-PORT-030 close on their Windows leg** (this session's own platform) **and are
claimed by inspection on Linux/macOS**, following M4's identical convention, since the GitHub
incident leaves those runners stuck. NFR-RT-010, NFR-PERF-050, NFR-PERF-060, NFR-SEC-020,
NFR-DOC-010 and NFR-QUAL-040 all close, the last four not named in the original Acceptance line at
all.

---

## 10. M6 — Product shells: platform, app, UI, CLAP

**Size: XL.** **Depends on:** M2 (real chain), M5 (state/library for save/load and browsing).
**Internally parallel** once `namir-ui` exists, since both `namir-app` and `namir-clap` embed it.

**Deliverables:**

- **`namir-platform`, full scope** — D-13.2's filesystem/config-dir/log-sink paths, D-13.3's
  CLAP-specific install paths (per-user default, confirmed empirically in S-4 that Reaper
  silently ignores the naive `%APPDATA%` location), thread-priority elevation.
- **Wire in `DenormalGuard` (NFR-RT-030, D-7.4) — carried over from M1, which built the type but
  never engaged it.** An M3 audit found `namir-platform`'s `DenormalGuard` is referenced nowhere
  outside `namir-platform` itself: the guard is real and unit-tested, but no audio path has ever
  acquired it, so NFR-RT-030 ("denormals shall not cause a measurable CPU spike in any stage")
  currently holds only because nothing measured happens to drive values subnormal — not by
  construction. This is the milestone that closes it, because this is the first milestone in which
  a real audio callback exists to acquire it in.

  **Where:** once per callback, in `namir-app`'s `cpal` stream callback and in `namir-clap`'s
  `process()` — matching D-7.4's own "once per audio callback" wording. **Not** in
  `namir-engine::Chain::process`: D-5.1's layering table (enforced by `xtask layering`) does not
  permit `namir-engine` to depend on `namir-platform` at all, so the engine cannot acquire it even
  if that seemed tidier. Both product shells may, and do, depend on `namir-platform`.

  **Plus the verification that has never existed:** NFR-RT-030's *Verify* method is **B** — drive
  each stage with a signal decaying into the denormal range and assert per-block processing time
  stays within 10% of nominal. No such benchmark exists yet. Note that M3's existing benchmarks
  call `Chain::process` directly and so run with FTZ/DAZ *off*; their numbers are valid for what
  they measure but are not evidence about NFR-RT-030 in either direction.
- **`namir-ui`** — egui, porting `spikes/s3-egui-baseview`'s validated wiring, implementing
  FR-UI-010 through 070 (the single-screen layout, keyboard/mouse operability, accessible names,
  numeric value entry, reset/fine-adjust gestures, responsiveness during a 10k-file library scan,
  non-modal error surfacing). Built once, shared by both `namir-app` and `namir-clap` per
  FR-UI-010's own requirement.
- **`namir-app`** — `cpal`-based standalone audio I/O (FR-IO-010 through 080: device selection,
  WASAPI shared/exclusive, sample-rate/buffer negotiation, latency reporting, xrun counting,
  **device-removal handling tested against a real failable device, not just the happy path** —
  this is R-5, called out explicitly because "works in the common case" is the failure mode this
  requirement exists to catch).
- **`namir-clap`** — porting `spikes/s4-clack-clap`'s validated skeleton (entry point, descriptor,
  audio processing, GUI embedding via `open_parented`) into a real adapter wired to the actual
  `Chain`, `namir-worker`, `namir-params`, and `namir-state` built in the milestones above:
  params/state/audio-ports/latency/tail extensions, host-driven bypass, arbitrary/varying block
  sizes down to one sample, mid-session sample-rate changes, multi-instance coexistence with
  shared weights (exercising M4's D-8.2 cache for real for the first time). **Needs its own
  written unsafe-code safety argument** (D-5.3) for the raw-window-handle bridge the spike used
  unchecked — this and `namir-platform` are the only two crates besides a future SIMD kernel
  module allowed to carry `unsafe` at all.

**Acceptance:** FRS §5.11 (IO), §5.12 (CLAP), §5.13 (UI) close — three entire sections that were
100% Not-started at M0.

**Note (added M6, `namir-app` built, `namir-clap` built in parallel by a separate session):**
FR-IO's Musts close with two documented exceptions rather than fully, found while building
against D-13.1's pinned `cpal` v0.18.1 rather than assumed: **FR-IO-020's WASAPI exclusive mode is
architecturally absent from that dependency** (verified against its vendored source, not
inferred — see `docs/02-architecture.md`'s D-13.1 consequence note and
`docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`), and **R-5's own literal ask — a real
failable device to test FR-IO-070 against — was not available this session**
(`docs/manual-tests/fr-io-070-device-removal.md`). Both are recorded as open, not silently folded
into "Acceptance" above. Device enumeration, sample-rate/buffer negotiation, settings persistence
with graceful fallback, and a real opened/playing stream were all verified against real WASAPI
hardware in this session (`docs/manual-tests/fr-io-010-device-enumeration.md` and
`fr-io-080-settings-persistence.md`'s executed runs). Separately, building `namir-app` surfaced
that `namir_worker::Instance` (M4) has no public way to submit an ordinary parameter change,
despite D-7.2's own module doc comment describing exactly that shared-submitter architecture —
see `docs/02-architecture.md`'s D-7.2 consequence note for the full finding and the additive fix
it proposes; `namir-clap` needs the identical workaround or fix.

---

## 11. M7 — Compliance and hardening closure

**Size: M.** **Depends on:** M2 through M6 (this milestone closes out gates whose targets didn't
exist until those landed — it is not "the CI milestone," M1 was that; this is what's left once
there's a whole product to check).

**Deliverables:**

- Remaining CI gates from M1's list that needed a real target first: full multi-OS build/test now
  covering every crate, mobile cross-compilation for every "builds for mobile: yes" crate
  including the ones M2–M5 added, the no-C++-toolchain container build now meaningful against a
  complete workspace, fuzz targets for every parser M2/M5 introduced (IR/WAV, state/preset —
  following the pattern M1 established for `.nam`), the network-free build configuration as a
  permanent target (D-18.2/FR-ERR-070 — deliberately **not** built pre-emptively before this,
  since there's no network feature yet for a feature flag to gate; building that infrastructure
  now, once, is cheaper than guessing at its shape earlier).
- **NFR-QUAL-010's full traceability check** (§23 of the architecture document): every Must
  requirement mapped to the component that satisfies it, to a test identifier, and to the test
  source itself, checked in CI. This is the mechanical proof that every row this roadmap marked
  "Done" across M1–M6 actually is.
- NFR-LIC-030's machine-generated attribution file, produced by the build.
- NFR-DOC-030's user guide (installation, audio setup, signal chain, troubleshooting).
- **AQ-4's resolution** (licence of NAM's standardized capture input signal) — this blocks
  shipping factory presets specifically, not the rest of 1.0, so it can run in parallel with
  everything else in this milestone and only needs to land before M8.

**Acceptance:** every remaining **S**-verified NFR that held "by omission" per the M0 gap analysis
now holds by mechanical enforcement instead.

---

## 12. M8 — 1.0 exit

**Size: S**, if M1–M7 were done honestly; this milestone is a gate, not a construction phase.

**Exit checklist:**

- Every row in this document's Must-requirement status snapshot (§14 below) reads **Done**, for every requirement marked
  **Must**.
- NFR-QUAL-010's CI traceability check is green.
- Cross-platform release binaries: Windows (primary, fully supported), Linux/macOS (secondary,
  best-effort) per FRS §1.4.
- Factory presets ship (post AQ-4).
- User guide ships.
- FR-CFG-020's bit-identical-output check passes across both product configurations on the same
  golden vectors — this is a good final integration test precisely because it only passes if
  everything upstream was actually shared correctly, not merely built twice.

---

## 13. Explicit non-goals for this roadmap (restated, not re-decided)

Everything in FRS §7.1 (Won't for 1.0) and §7.2 (RD-1 through RD-4, post-1.0 direction) stays
exactly as scoped there. Nothing above should be read as sneaking any of it in early. Specifically
worth restating because M1–M7 touch adjacent territory:

- **RD-1 (Tone3000/online acquisition)** — `namir-library`'s `origin` field (D-12.4) is built with
  room for it in M5, but no network code, API client, or download flow is part of this roadmap.
- **RD-2 (multiple NAM/IR stages, reorderable chain)** — M1's parameter-ID scheme reserves the
  stage-instance-index field per D-10.2, and M2's `Chain` is already `Vec<Box<dyn Stage>>` (D-6.1),
  but M2 populates it with exactly the fixed six stages FR-CHAIN-010 specifies, in that order,
  non-reorderable.
- **RD-3 (hosting foreign CLAP plugins)** — not part of M6's `namir-clap`; that crate hosts
  Namir's own chain in a host, it does not host other plugins itself.
- **Should/Could requirements** (ASIO, PipeWire/JACK, AIFF/FLAC, dual IR slots with blend, IR
  normalization, output limiter, MIDI program-change preset selection, host-resize GUI, themes,
  etc.) are not tabled milestone-by-milestone above the way Musts are. Pick them up opportunistically
  within whichever milestone owns their area (e.g. the output limiter fits naturally into M2's Out
  stage) if the size budget allows; none of them gate M8.

---

## 14. Appendix: Must-requirement status snapshot (M0)

Full detail (every FRS subsection, every Must ID) lives in the audit this roadmap was built from;
reproduced here in summary so this document doesn't silently drift out of sync with re-reading
the FRS. **Done** / **Partial** (primitive or scaffolding exists, not integrated into a real
stage/product) / **Not started**.

| FRS area | Must count | Done | Partial | Not started |
|---|---|---|---|---|
| 5.1 CHAIN | 7 | 0 | 2 | 5 |
| 5.2 IN | 3 | 0 | 3 | 0 |
| 5.3 GATE | 3 | 0 | 3 | 0 |
| 5.4 NAM | 11 | 3 | 4 | 4 |
| 5.5 IR | 7 | 1 | 0 | 6 |
| 5.6 EQ | 3 | 0 | 3 | 0 |
| 5.7 OUT | 2 | 0 | 1 | 1 |
| 5.8 PARAM | 5 | 0 | 1 | 4 |
| 5.9 STATE | 7 | 0 | 0 | 7 |
| 5.10 LIB | 5 | 0 | 0 | 5 |
| 5.11 IO | 8 | 0 | 0 | 8 |
| 5.12 CLAP | 10 | 0 | 0 | 10 |
| 5.13 UI | 7 | 0 | 0 | 7 |
| 5.14 ERR | 6 | 0 | 4 | 2 |
| 6.1 RT | 4 | 1 | 2 | 1 |
| 6.2 PERF | 6 | 1 | 0 | 5 |
| 6.3 PORT | 5 | 0 | 4 | 1 |
| 6.4 QUAL | 6 | 0 | 4 | 2 |
| 6.5 LIC | 5 | 3 | 0 | 2 |
| 6.6 SEC | 3 | 0 | 3 | 0 |
| 6.7 BUILD | 2 | 1 | 0 | 1 |
| 6.8 DOC | 2 | 0 | 2 | 0 |

M2 alone converts most of 5.1/5.2/5.3/5.6/5.7's Partial rows to Done. M3 converts most of 5.4.
M5 converts 5.9/5.10 wholesale from Not-started. M6 converts 5.11/5.12/5.13 wholesale. M1+M7
between them convert nearly all of 5.8 and every 6.x row.

**One live update to this otherwise-frozen M0 snapshot, made in this session:** 5.4 NAM's Done
count moved 1 → 2. FR-NAM-020 ("Namir shall support, at minimum, the `WaveNet` and `LSTM`
architectures") was Partial at M0 (WaveNet-only); §7's LSTM deliverable closes it — both Must
architectures now load, run, and parity-test against an independent reference behind the same
`PreparedNam`/`NamState` surface, with zero `namir-engine` changes required. The rest of 5.4
(resampling quality, crossfade, loudness calibration, cost reporting) is unaffected and stays as
audited; this is the one cell §7's evidence directly justifies moving, not a re-audit of the row.

**A second live update, also made in this session:** 6.2 PERF's Done count moves 0 -> 1.
NFR-PERF-010 ("no more than 25% of one core at the 99.9th percentile", under its own literal
condition) closes: the assembled six-stage chain measures **p99.9 = 16.45-17.08%** on the §2
reference machine across five repetitions under D-2.4's measurement conditions, cross-checked
against the contamination-immune estimator at 14.94-15.10% on the same runs.

This cell was briefly held open on the grounds that the two candidate statistics disagreed. They
do not: the disagreement was an artifact of measuring while this project's own tooling saturated
the machine, and D-2.4 records the conditions that prevent a repeat. The other five Musts in 6.2
(NFR-PERF-020 latency, 030-060) are untouched by M3 and stay Not-started -- only the one cell M3's
evidence directly justifies moves, matching how 5.4 NAM was handled above.

**Three further live updates, made in the M4 session**, following the same rule — only cells this
milestone's own evidence directly justifies, each named:

- **5.4 NAM Done 2 -> 3, Partial 5 -> 4.** FR-NAM-070 (glitch-free crossfaded model swap) closes,
  verified by its own literal *Verify: I* method in
  `namir-engine/src/engine.rs`'s `fr_nam_070_swapping_models_under_a_sine_has_no_discontinuity_or_dropout`.
- **5.5 IR Done 0 -> 1, Not-started 7 -> 6.** FR-IR-060 closes, same method, same file.
- **6.1 RT Done 0 -> 1, Partial 1 -> 2, Not-started 3 -> 1.** NFR-RT-020 (wait-free from the audio
  thread's side) closes: there is now a real ring to verify rather than an absence, and its
  *Verify: S plus code review* is met by the type-level SPSC split plus the D-7.5 harness covering
  every ring operation. **NFR-RT-010 moves Not-started -> Partial, deliberately not Done** — M4
  closed the one known audio-thread allocation and drives a complete handover inside the harness,
  but the requirement's *Verify* clause also demands a stress test "with concurrent model loading,
  preset recall and library scanning", and preset recall is M5's `namir-state` while scanning is
  M5's `namir-library`. Neither exists, so only the model-loading axis is covered. Recorded as
  Partial rather than claimed.

**Not moved, and worth stating so nobody moves it:** 5.12 CLAP stays 0 Done. FR-CLAP-090's
cross-instance sharing mechanism is built and tested at the worker level, but §8's own acceptance
text says it "isn't exercised for real until M6's `namir-clap`", and that remains true.

**Six further live updates, made in the M5 session**, following the same rule — only cells this
milestone's own evidence directly justifies, each named. See this section's own §9 M5-status
addendum above for the full per-requirement accounting; only the cell movements themselves are
recorded here.

- **5.9 STATE Done 0 -> 3, Not-started 7 -> 0, Partial 0 -> 4.** FR-STATE-010, -040, -050 close in
  full. FR-STATE-020, -030, -060, -070 each close only their M5-resolvable half (a released-version
  corpus, a host/plugin UI to trigger recall, a literal host restart, and the locate-manually UI
  affordance respectively are M6+ or, for -020, the first release itself) — Partial, not Done,
  deliberately.
- **5.10 LIB Done 0 -> 4, Not-started 5 -> 0, Partial 0 -> 1.** FR-LIB-010, -030, -040, -070 close in
  full. FR-LIB-020's scan mechanism, cancellation and pool-driven job are all built and tested;
  scan-progress *visibility* is M6 UI — Partial.
- **6.1 RT Done 1 -> 2, Partial 2 -> 1.** NFR-RT-010 moves Partial -> Done:
  `crates/namir-worker/tests/rt_stress.rs` supplies the two axes (preset recall, library scanning)
  M4's close-out recorded as the reason it could not close, run concurrently with model loading
  against a live `AudioEngine` inside the D-7.5 harness.
- **6.2 PERF Done 1 -> 3, Not-started 5 -> 3.** NFR-PERF-050 (50 MB load within 500 ms) and
  NFR-PERF-060 (10 000-file incremental rescan within 2 s) both close, measured with margin —
  see this section's own M5-status addendum for the figures.
- **6.4 QUAL Done 0 -> 1, Partial 4 -> 3.** NFR-QUAL-040 closes: "the preset and state readers"
  now have a `cargo-fuzz` target of their own (`crates/namir-state/fuzz`), landed in M5 rather than
  deferred to M7 per the plan's own decision that D-11.1's one-parser-per-format choice forfeits
  its reason otherwise.
- **6.6 SEC Done 0 -> 1, Partial 3 -> 2.** NFR-SEC-020 closes: the byte-count ceiling
  (`namir_core::MAX_FILE_BYTES`, moved there this milestone specifically so `namir-library` could
  reach it without depending on `namir-worker`) is now enforced at every point untrusted bytes
  enter this codebase — `namir-worker`'s file loads, `namir-library`'s scanner, and
  `namir-state`'s embedded-data decode.
- **6.8 DOC Done 0 -> 1, Partial 2 -> 1.** NFR-DOC-010 closes:
  `docs/04-state-and-preset-format.md`, with both its own manual test and FR-STATE-040's executed
  and recorded in `docs/manual-tests/` rather than left as an unrun script.

---

## 15. Appendix: open decisions to make, not build

Small, cheap to resolve, but genuinely unresolved by the architecture document as written —
flagged here so they're made deliberately rather than defaulted into by whoever writes the code
that happens to depend on them first.

1. ~~**NFR-QUAL-030's wording vs. the project's actual verification strategy.** D-19.1 already
   commits to generated-not-captured fixtures verified analytically or by independent
   cross-implementation, which is arguably a stronger standard than "golden reference audio" but
   is literally different wording. Resolve during M1 with a short addendum to
   `02-architecture.md`, not new code.~~ **Resolved:** `02-architecture.md` D-9.11 (§9.4).
2. ~~**AQ-3** (embedded index store for `namir-library`, D-12.3) — due at M5, constrained but not
   decided.~~ **Resolved at M5, 2026-08-07:** `02-architecture.md` D-12.3 — a single
   pretty-printed JSON document, written whole and replaced atomically. No new dependency.
3. **AQ-4** (licence of NAM's standardized capture input signal) — due before M8, blocks factory
   presets only.
4. **Whether `namir-nam`'s FR-NAM-030 parity claim should be re-anchored against
   `NeuralAmpModelerCore` from inside the product workspace**, rather than relying on the
   already-excluded `spikes/s1-nam-inference`'s one-time -131 dB measurement. The cross-implementation
   parity test added to `namir-nam` in this session is strong evidence on its own, but it validates
   internal consistency between two from-scratch Rust ports, not agreement with the external
   reference implementation FR-NAM-030 actually names. Worth a decision at M3 (when LSTM parity
   needs the same treatment anyway): commit a small, licence-clean reference-output fixture into
   the repo, or accept the spike's result as sufficient historical evidence and say so explicitly.
5. ~~**What NFR-PERF-010 actually gates on (D-2.2).** Raised by M3's close-out, which measured both
   candidate quantities on the §2 reference machine and found they disagree about whether the
   requirement passes:~~ **Resolved: `02-architecture.md` D-2.4.** The disagreement was an artifact
   of measuring while this project's own tooling loaded the machine. Measured quiet, the literal
   p99.9 reads 16.45-17.08% and agrees with the estimator to ~1.8 points, so D-2.2's gate is kept
   as written and D-2.4 adds the measurement conditions plus a mandatory contamination check. The
   original framing is preserved below because the reasoning about *why* the choice mattered
   remains the reason D-2.4 exists:
   - *Namir's own worst-case block* — the per-residue-minimum estimator in
     `namir-engine/benches/tail_structure.rs`: **~15.2%** of the block period, reproducible to
     ±0.15 points across cores and across machine loads. **Passes** the 25% budget.
   - *End-to-end observed per-block latency* — raw p99.9 as D-2.2 literally specifies: **17–52%**
     across ten identical runs, dominated on this machine by GPU-driver ISRs and background load
     rather than by Namir. Neither passes nor fails stably; it is not a reproducible statistic on
     a general-purpose desktop.

   The case for the first is that it measures the thing the project can actually engineer, and it
   is reproducible. The case for the second is that a dropout is a dropout — a user whose audio
   glitches does not care that a GPU driver caused it, and NFR-RT-040 is a statement about worst
   case. A defensible resolution may be to keep p99.9 as the gate but specify the measurement
   conditions properly (core selection away from device ISRs, minimum repetitions, and a
   reproducibility criterion the figure must meet before it counts), which would make the gate both
   literal and achievable. **Due before M8**, since 6.2 PERF cannot be marked Done without it, and
   worth deciding early because M6's `namir-platform` thread-affinity work is the product-side
   half of the same problem.
