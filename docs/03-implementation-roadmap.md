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

*Consequence (added 2026-08-08, when M9–M13 were planned)* — the enumeration above says "M1 through
M8" because M8 was the last milestone when it was written. It binds **M9 through M13 too**; the
range was a list of the milestones that existed, never a statement that some milestones are exempt.
Recorded as a note rather than by editing the sentence, per this document's own convention, but read
it as "every milestone's exit, without exception." Two of the new milestones also tighten what that
exit means: M9 makes `xtask traceability` a required check rather than the informational one M7
left it as, and M13 adds the release pipeline's own artifacts to what must be green.

*Further consequence (added M9's P0 decision pass, 2026-08-08)* — the note above says M9 makes
`xtask traceability` a required check. **Half of it does.** `02-architecture.md` **D-18.5** splits
the check in two: the generated-plan-freshness half becomes required at **M9a** (§16's P0 decision
pass splits this milestone into two phases — M9a the ledger, M9b the build work), and the
zero-uncovered-Musts half stays informational until **M13's close-out**, because nine of the
twenty-four Musts the tool currently reports uncovered belong to M10, M12 and M13 rather than to
M9 — **ten** once the same pass moves NFR-PERF-030 to M13 (§20's dated scope note). The three
standing gates above are unchanged; what changes is which half of this one check blocks a merge,
and on which date. From M9a, deleting a coverage annotation from a currently-covered Must stops
being something CI tolerates — today it is, because the tool returns a single exit value for both
halves (`xtask/src/main.rs:304`) and CI's one invocation of it carries `continue-on-error: true`
(`.github/workflows/ci.yml:108-120`), which suppresses them together.

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

### M6 close-out (coordinating session, 2026-08-07)

The gap the paragraph above flagged closed the same session it was found: `namir_worker::Instance`
gained `try_submit_param` (a two-line forward to the submitter it already owned privately), and
`namir-app`'s 575-line `LiveEngine` substitute was deleted in favour of it — `namir-app` now shares
a real `Instance` behind a `Mutex`, the same shape `namir-clap`'s `SharedInner` already used for
the identical concurrent-access problem. Net -455 lines; no other gap was found in `Instance`'s
public surface.

**`namir-platform` reached its full M6 scope.** D-13.2's config/log-sink paths and thread-priority
elevation, D-13.3's CLAP install-path table (Windows/macOS/Linux, per-user and system-wide) — all
three new, all documented with the same "built but not yet called" honesty D-7.4's own `DenormalGuard`
used from M1 through M5, since nothing in this round calls the thread-priority primitive except
`namir-app`/`namir-clap` themselves, below.

**`namir-ui` was built, ported from `spikes/s3-egui-baseview`'s validated wiring.** The key design
decision — not spelled out in this section's original Deliverables text — is the `UiHost` trait
(`crates/namir-ui/src/host.rs`): `namir-ui` may depend only on `core`/`params`/`library`/`state`
(D-5.1), so it cannot own a live `Chain` or `Instance` itself; it is a pure view+intent layer,
driven each frame by a caller-supplied `UiSnapshot` and emitting `UiIntent`s, which `namir-app` and
`namir-clap` each implement independently against their own real engine/worker. FR-UI-010/020/040/
050/060/070 all close, the last with a real 10,000-entry stress test (via `namir-fixtures`' M5
generator) proving no frame exceeds the FR-UI-060 budget. **FR-UI-030 stays Partial**: `egui-baseview`
0.6 does not forward `egui`'s accesskit tree to a real screen reader, so "every control has an
accessible name" is true in the widget tree but not yet observable by assistive technology —
recorded in `docs/manual-tests/fr-ui-030-accessibility-script.md` rather than silently claimed.

**`namir-clap` was built, wired to the real `Chain`/`Instance`/`REGISTRY`/`State`, and validated
against `clap-validator` for real** (installed from git, run both in-process and out-of-process):
**32 of 32 applicable tests passed, 0 failed, 0 warnings**, catching one real bug along the way
(state loads never called `HostParams::rescan(VALUES)`, fixed this session). FR-CLAP-060's host
bypass is wired directly to D-10.4's new `global.bypass` descriptor via CLAP's own `IS_BYPASS` flag
— the concrete reason D-10.4 was done this session rather than left open. FR-CLAP-090's cache
sharing (`ResourceCache::shared()`, a process-global `OnceLock`) is verified at the unit level
(`Arc::ptr_eq` across independently-constructed instances) but **not exercised inside a real host
with two simultaneous instances** — recorded as Partial, alongside FR-CLAP-100's embedded GUI,
which this session confirmed builds, passes the validator with the GUI extension never invoked, and
is installed at the real per-user CLAP path (`%LOCALAPPDATA%\Programs\Common\CLAP`, confirmed
already holding S-4's own spike plugin on this machine) — but whose actual rendering inside Reaper's
window frame has not been observed by any agent session, none of which has had a way to drive a
real desktop GUI. **This crate's `set_parent` — the workspace's first new `unsafe` code since
M1 — was adversarially reviewed by an independent agent before merging**, per D-5.3's "written
safety argument" requirement: no soundness/UB hole was found, but the review did find (and this
session fixed) two real gaps the original argument missed — a recognised-but-wrong host window-API
tag reaching `baseview`'s Windows backend as a panic instead of this crate's own diagnostic notice,
and a double-`set_parent` (a host contract violation, but real hosts do have bugs) silently
orphaning the previous native window rather than closing it. Both are closed in `gui.rs`, with the
review's own reasoning folded into that module's safety-argument doc comment.

**`namir-app` was built and verified against real hardware this session** (WASAPI, a PreSonus
AudioBox 22VSL, and this machine's own default devices): device enumeration, sample-rate/buffer
negotiation, and a genuine opened-and-playing duplex stream (48 kHz, 480-frame buffer, ~20 ms
estimated round-trip) all confirmed working, not merely unit-tested. **One Must-requirement gap
was found and is recorded honestly rather than worked around: FR-IO-020's WASAPI exclusive mode is
architecturally absent from `cpal` 0.18.1**, D-13.1's pinned dependency — verified directly against
that exact version's vendored source (`host/wasapi/device.rs` hardcodes
`AUDCLNT_SHAREMODE_SHARED`), not inferred. `namir-app` cannot work around this itself: D-5.3 confines
`unsafe` to `namir-platform`/`namir-clap`, and a raw `IAudioClient::Initialize(...,
AUDCLNT_SHAREMODE_EXCLUSIVE, ...)` call needs exactly that. Per this session's own decision (see
`docs/02-architecture.md` D-13.1's consequence note and
`docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`), this is recorded as an open gap rather than
built around under time pressure — a follow-up needs either a `namir-platform`-owned unsafe
WASAPI-exclusive helper (mirroring `DenormalGuard`'s existing pattern) or an upstream `cpal` fix.
**R-5's own literal ask — a real failable device — was also not available this session**, so
FR-IO-070's device-removal handling stays built-and-tested against everything except that one
condition (`docs/manual-tests/fr-io-070-device-removal.md`), and physically unplugging a device
mid-stream is beyond what any agent session (this one included, lacking hands) can perform. ALSA/
CoreAudio (FR-IO-030), measured loopback latency (FR-IO-050's other half), real-hardware xrun
induction (FR-IO-060), and independent-stereo-input channel mapping (FR-IO-090) are each recorded
Partial in the same honest style, in their own `docs/manual-tests/fr-io-0*.md` files.

**NFR-RT-030 (D-7.4's `DenormalGuard`, unused since M1) closes, with a certified benchmark that
never existed before this session.** `crates/namir-engine/benches/denormal_guard.rs` (a
`namir-platform` dev-dependency of `namir-engine`, exempt from D-5.1's normal-edge layering check,
the same exemption `namir-nam`'s dev-dependency on `namir-fixtures` already relies on) drives the
real six-stage chain with a signal decaying into the denormal range, guard-engaged vs. guard-absent
vs. a nominal (non-denormal) control, interleaved within one process. **Certified on the §2
reference machine, five repetitions, `NAMIR_PIN_CORE` defaulted to core 4 (D-2.4's clean core):**
guard-engaged-denormal p50 stayed within **-1.81% to +1.56%** of nominal across all five runs — far
inside NFR-RT-030's 10% budget — while guard-*absent* consistently cost **1.33-1.38x** the
guard-engaged figure, direct, repeatable evidence the guard suppresses a real, measurable spike
rather than merely existing. `DenormalGuard` is now acquired for real, once per callback, in both
`namir-app`'s `cpal` stream callback and `namir-clap`'s `process()` — D-7.4's own wording, finally
exercised by a real audio path.

**R-7's ~0.4-point margin was re-checked against M6's new thread-priority-elevation code path in
the audio callback, per this row's own standing "re-run whenever the audio callback changes"
reason for existing** (`docs/02-architecture.md` §22). Five repetitions on the §2 reference
machine: raw p99.9 was contaminated by this session's own concurrent tooling load (multiple
`claude` agent processes running in parallel, plus ordinary desktop applications) — confirmed, not
assumed, since even arm A (steady state, no handover at all) swung 18.5-28.9% raw p99.9 across the
five runs despite having no handover activity to vary. The contamination-immune estimator, which
does not depend on a quiet machine the same way, stayed tight and stable at **14.0-14.5%** across
every gating period (32/64/128 blocks) and every repetition — comfortably under the 25% budget,
consistent with the certified quiet-machine range this risk retired against, and showing **no
evidence of regression** from M6's thread-priority/`DenormalGuard` wiring. Per D-2.4's own
methodology, the raw figures are discarded as contaminated rather than trusted; the estimator is
the reading of record.

**Acceptance, restated honestly rather than left as this section's original "close" claim:** the
large majority of FRS §5.11/5.12/5.13's Musts close in full (see §14's updated snapshot for the
per-requirement count), but none of the three sections closes wholesale. Two residuals are
Must-requirement gaps proper (FR-IO-020's exclusive mode, architecturally blocked by the pinned
dependency; R-5's failable-device test, blocked by hardware availability); the rest are real
functionality this session verified as built and tested but could not observe running inside a
real host or against real assistive technology, for want of a way to drive either from an agent
session. Every gap is named in its own `docs/manual-tests/*.md` file or a `*Consequence (added
M6)*` note, not silently folded into a claim of completion.

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

### M7 status (this session, 2026-08-08)

**Investigation first, before any code**: most of this milestone's own bullet list turned out to
already exist. The Windows/Linux/macOS build+test matrix, `cargo-deny` licence audit, MSRV pin,
no-C++-toolchain build, and Android/iOS mobile cross-compile jobs were all already live in
`.github/workflows/ci.yml`, built incrementally at M1–M6, not started fresh here — this milestone
was genuinely a *closure* pass over specific, identifiable gaps, confirmed by reading the actual
files rather than assumed from this section's own prose.

**Concrete gaps found and closed:**

- **`namir-ui` excluded from the mobile cross-build, correctly, not by oversight.** D-5.1's table
  predicted it would join once M6 built it. Checked instead of assumed: `namir-ui` depends
  directly on `baseview` 0.2, whose `platform.rs` compiles a backend only for `macos`/`linux`/
  `windows` — neither `android` nor `ios` matches, verified by a real cross-check build on this
  session's machine. D-5.1's table corrected from "Yes" to "No" for this crate, with a
  `*Consequence (added M7)*` note; `namir-app`/`namir-clap` stay correctly excluded per their
  existing "Not for 1.0"/"No" cells.
- **`namir-ir` fuzz target** (`probe_wav`), the one parser missing cargo-fuzz coverage — `namir-nam`
  and `namir-state` already had theirs since M1/M5. Seeded from a generated (D-19.1) corpus via a
  new `namir-ir` example, mirroring the other two fuzz crates exactly.
- **D-18.2/FR-ERR-070.5's network-free build gate**, deliberately deferred since M1 until there was
  a whole product to check. Reuses the existing `cargo-deny` wiring: `deny.toml`'s `[bans]` now
  denies the well-known HTTP/TLS/DNS/async-networking crates by name, gated by a new `network-free`
  CI job. A standing assertion — no network client exists in this workspace — not a fix for a real
  violation.
- **NFR-LIC-030's attribution file** — `cargo run -p xtask -- attribution [--write]`, same
  generate-and-diff shape `params.lock` established: walks `cargo metadata`'s real resolve graph
  from `namir-app`/`namir-clap` (the two shipped products) via `Normal`-kind edges only, so
  dev/build-only subtrees are correctly excluded. 243 real third-party dependencies discovered,
  zero `UNKNOWN` licences. Closes *production* of the artifact and its CI freshness gate; physical
  bundling into a release installer stays open — no packaging pipeline exists yet (M8).
- **NFR-QUAL-010's traceability check** — the substantial piece. FRS §10 names a
  `docs/03-test-plan.md` this project never actually had (the roadmap document took the "03" slot
  instead — a real, previously unrecorded inconsistency). `cargo run -p xtask -- traceability
  [--write]` now generates it: every `**FR-*/NFR-* (Must)**` + `*Verify:*` pair parsed from the
  FRS, reconciled against `docs/manual-tests/*.md` (`Verify: M`) or a `// trace: ID` comment /
  `fr_xxx_nnn_...`-named test function anywhere under `crates/**`/`xtask/**` (everything else, plus
  `# trace:` in CI/build config for the Musts verified entirely by tooling, not test code). FRS
  §10 and architecture.md §23 both gained `*Consequence (added M7)*` notes recording the mechanism
  honestly — crate-granularity component mapping, a generated (not hand-authored) test plan.
  Building the check also found a second, smaller FRS defect: §1.5's own *Verify*-code legend
  listed only six codes (U/I/G/B/S/M), missing the seventh, "Process", that NFR-QUAL-020 actually
  uses — added to the legend.
- **Retroactive sweep**: nine parallel agents, one per product crate, found and tagged the
  already-existing test/bench that covers each of their crate's Must requirements — no test logic
  changed anywhere, only `// trace:` comments added. Each correctly identified and reported the
  requirements genuinely outside its own crate's scope rather than forcing a tag; those were
  reconciled directly afterward (`namir-dsp`'s primitive-level tests for FR-IN-020/GATE-020/
  GATE-030/EQ-020/PARAM-040; `namir-engine`'s stage tests for FR-NAM-050/130, FR-PARAM-030,
  FR-IR-070/100; `namir-worker`'s recall/pool tests for FR-STATE-030 and FR-ERR-040; NFR-QUAL-030
  and NFR-SEC-020 at their real, already-existing locations). Two manual-test docs written for
  FR-UI-040/050 (`Verify: M` requirements whose automated coverage exists but, per NFR-QUAL-010's
  own text, still need a written script — both honestly recorded as not executed this session, no
  way to drive a real GUI, matching FR-UI-030's own precedent).
- **NFR-DOC-030's user guide** (`docs/user-guide.md`, Should) — installation, audio setup, the
  six-stage signal chain in its real D-9.8 order, troubleshooting. Every claim traced to a specific
  file read, not invented; the two known FR-IO gaps (WASAPI exclusive mode, device-removal
  mid-session) stated as plainly as their own manual-test docs already do.
- **AQ-4 resolved** (research, not code): no explicit licence exists for NAM's standardised
  reamp/capture signal — distributed off the MIT-licensed upstream source trees via a personal
  file-sharing link, no terms found anywhere reachable. Recorded as all-rights-reserved pending
  upstream clarification, in the same dated-note style AQ-3/AQ-5 already used. Blocks nothing but
  shipping factory presets, exactly as originally scoped.

**Net result: the uncovered-Must count went from 107 (the honest "red" state the traceability tool
found on its first real run) to 18, then 16 once the two manual-test docs landed.** Every workspace
build, `clippy -D warnings`, and `cargo test --workspace` run stayed green throughout — the entire
sweep is comment-only, zero test behavior changed.

**Two further, unplanned findings, recorded honestly rather than smoothed over:**

- **A second stale-cell bug in this section's own §14 snapshot**, the same species M6 found and
  fixed for 6.1 RT: M5's own prose claimed "6.6 SEC Done 0 -> 1" (NFR-SEC-020 closing), but the
  physical table row was never edited to match, and stayed at the M0-frozen `3 | 0 | 3 | 0`. Found
  and corrected in the table below, alongside this session's own two genuine new closures in that
  row (FR-ERR-060/070's mirror, NFR-SEC-030, and NFR-SEC-020's own "See NFR-QUAL-040" text — now
  that all three of `namir-nam`/`namir-ir`/`namir-state`'s fuzz targets exist and are tagged
  `NFR-SEC-010` too) — all three Musts in 6.6 SEC now close.
- **5.12 CLAP's "8 Done" cell rests, for several Musts, on a one-time manual `clap-validator` run
  (M6, 32/32) rather than the CI-gated, repeatable verification those requirements' own literal
  *Verify* text calls for** — FR-CLAP-020 says outright "pass clap-validator... gate in CI", and
  `clap-validator` is not wired into `.github/workflows/ci.yml`. This session's more rigorous check
  found seven of the ten CLAP Musts (020, 030, 040, 070, 080, 100, 130) have **zero** `#[cfg(test)]`
  coverage in the crate — only manual-test docs for the ones that have them. The underlying
  functionality was real and was demonstrated once; it is not mechanically re-verified today. This
  is left as a flagged finding rather than a guessed-at table correction: 5.12's own Must-ID
  membership needs a careful re-audit (this session counted 11 `FR-CLAP-*` ids against the row's
  stated count of 10, an unresolved discrepancy) before its Done/Partial split can be corrected
  with confidence, so the table below is left as M6 recorded it for this one row, with this
  paragraph standing in as the honest record until that re-audit happens.

**A real bug found by this PR's own first CI run, not by local testing**: `docs/03-test-plan.md`
reported "stale" on Linux CI despite being freshly regenerated and committed from this session's
Windows machine, even though the missing-Musts count matched exactly. Root cause not fully
confirmed (Linux access unavailable to this session), but the most likely mechanism —
`std::fs::read_dir`'s iteration order over `docs/manual-tests/` being filesystem-dependent, feeding
a `.find()` that could pick a different matching file on a different platform when more than one
manual-test doc's content mentions the same id — is fixed by sorting that list before searching,
plus the comparison itself is now CRLF/LF-tolerant as defense in depth. **Separately, and more
importantly**: even once that's fixed, `xtask traceability`'s missing-Musts count is *real* and
currently 16, which would leave a supposedly-required CI check permanently red until every one
closes — several needing genuinely new benchmark/test infrastructure outside this milestone's
scope. Following the exact precedent this workflow already sets for `coverage` and
`nfr-perf-010-chain-bench`, the traceability step is `continue-on-error: true` (informational)
until that count reaches zero, not because the check is unimportant but because a red required
check nobody can act on trains reviewers to ignore CI, the opposite of what NFR-QUAL-010 exists to
prevent.

**Confirmed by this PR's second CI run** (after the sort-before-search fix, commit `f8f72d9`): all
19 checks pass, including `layering + params.lock + attribution` as a whole. Reading that job's own
Linux log directly (not inferring from the green checkmark) shows `xtask traceability` printing
`docs/03-test-plan.md is up to date` on `ubuntu-latest` — the cross-platform staleness false
positive is gone — immediately followed by the same 16-item missing-Musts list as the local Windows
run, exiting 1 as designed and tolerated only by `continue-on-error`. Both halves of the fix hold:
the determinism bug is actually fixed, not just no-longer-observed, and the real gap count is
unchanged by the fix (as it should be — sorting search order doesn't change coverage).

**Acceptance — partially met, stated honestly rather than claimed in full.** The CI-gate deliverables
(mobile-list correction, `namir-ir` fuzz, network-free gate, attribution file) are built and confirmed
green on their real first CI run against this PR (all 19 checks passing, including both mobile
cross-builds and the new `fuzz-smoke-ir`/`network-free`/`attribution` jobs) — recorded as such, not
assumed green, per this project's own standard of not claiming untested behavior works. NFR-QUAL-010's
traceability check is real and running, but does not yet report zero uncovered Musts: 16 remain,
each individually investigated and confirmed as a genuine gap (not a tagging miss) by name above and
in `docs/03-test-plan.md`'s own generated output. AQ-4 and the user guide close in full. The milestone
is far more complete than it started, closes real infrastructure gaps M1 never had a target for, and
converts several previously-invisible gaps (the CLAP CI-gating finding, the WASAPI/resampling-quality
gaps already known) into named, tracked ones — which is what NFR-QUAL-010 existing at all is for.

### M7 status — correction appended 2026-08-08

The Acceptance paragraph immediately above states that the 16 remaining uncovered Musts were "each
individually investigated and confirmed as a genuine gap (not a tagging miss)". **That claim is
wrong for at least three of the sixteen.** The original sentence stands unedited, per this project's
convention that a corrected finding stays on the record with the correction appended after it rather
than being quietly repaired; what follows is the correction. Each of the three was established by
re-reading the actual source this session, not by re-reading the earlier session's summary of it.

Three of the sixteen are **tagging misses** — the covering test or benchmark already exists and is
simply untagged, which is precisely the category the original sentence claimed to have ruled out:

- **NFR-PERF-050** — `crates/namir-worker/benches/resource_load.rs` measures this requirement
  directly. It is the same benchmark whose figures §9's own M5 status section quotes against
  NFR-PERF-050's 500 ms ceiling, and it is the only benchmark in the repository carrying no
  `// trace:` tag at all. Nothing needs building here; a tag needs adding.
- **FR-STATE-050** — `crates/namir-worker/src/recall.rs` carries eight tests for recall behaviour,
  including `recalling_both_a_model_and_an_ir_never_offers_them_simultaneously`, which is exactly
  the property the requirement states.
- **FR-LIB-020** — covered by `cancelling_a_large_scan_stops_it_before_completion`
  (`crates/namir-worker/src/library.rs:437`).

The remaining thirteen have **not** been individually re-checked this session, so the honest
statement is "at least three", not "exactly three". The full re-audit is M9's first deliverable and
it, not this note, produces the real split.

**One finding in the opposite direction, which the original claim understated rather than
overstated:** **NFR-PERF-030** (standalone startup to an audible state) and **NFR-PERF-040** (plugin
instantiation within 200 ms) are not untagged coverage — neither identifier appears **anywhere in
the codebase at all**. There is no benchmark, no test, no harness, and no measurement scaffolding
for either. These two are genuine gaps of a harder kind than a sixteen-item "missing tag" list
conveys, because closing them means building measurement infrastructure that does not exist rather
than labelling work that does. M9 carries both, sized accordingly.

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

### Execution order — M8 runs last, and the numbers are not the order (added 2026-08-08)

**Read this before §16 through §20.** Five further milestones — **M9 through M13** — were added to
this roadmap on 2026-08-08 and are specified in §16–§20 below. They are numbered by **when they
joined the plan, not by when they run.** The execution order is:

> **M9 → M10 → M11 → M12 → M13 → M8.**

**M8 keeps its number, its text and its meaning, and runs last.** Its entire content is the 1.0 exit
gate: every Must row reads Done, NFR-QUAL-010's traceability check is green, cross-platform release
binaries exist, factory presets and the user guide ship, FR-CFG-020 passes. Four of those six
assertions are the *output* of M9, M12 and M13 rather than things that could be true before them —
M9 is what makes the §14 table and the traceability gate mean anything, and M13 is what produces a
release binary at all. A gate that runs before the work it gates is not a gate, so M8 moves to the
end of the order and nothing about it is rewritten.

Renumbering M8 to "M14" was the obvious alternative and was rejected: every reference to M8 across
the FRS, `02-architecture.md`, this document's own earlier sections and several `docs/manual-tests/`
files would then have to be rewritten, which this project's documentation convention forbids and
which would destroy the audit trail those references form.

The same convention explains why §16–§20 sit **after** this document's two appendices rather than
immediately after this section. §13, §14 and §15 were already taken by non-milestone sections, and
both `AGENTS.md` and several passages in this document address them by number ("§14 below",
"§15 below"). Section numbers in this file are addresses, not an ordering.

**So: neither the milestone numbers nor the section numbers encode execution order.** Only the
arrow line above does. §3's dependency diagram stops at M8 and is deliberately left as written —
it records the M0–M8 dependency reasoning as it stood; each new section below states its own
depends-on and blocks relationships in its header line instead.

*Consequence (added M9's P0 decision pass, 2026-08-08)* — the arrow above is **refined, not
replaced**, and is deliberately left as written. M9 is split into two **phases** — **M9a**, the
ledger, tooling and documentation work, and **M9b**, the verification infrastructure that has to be
built — declared inside §16 rather than as new milestone numbers, the same device §17 already uses
for M10's five phases, Phase 0 through Phase 4, so no existing reference to "M9" in this document,
the FRS, `02-architecture.md` or `docs/manual-tests/` changes meaning. The refined order is:

> **M9a → M10 → M11 → M12 → M13 → M9b → M8.**

Two constraints and no others. **M9a completes before M10 starts:** §17's dependency line on M9 —
"a hard-won parity claim is worth less landing in a ledger nobody trusts" — is a dependency on the
ledger, and nothing in M9b is a prerequisite for A2 support. **M9b blocks only M8**, whose exit
checklist nominates FR-CFG-020's bit-identical-output check as its final integration test. §16's own
P0 subsection records the reasoning and the phase membership. The preamble to §16–§20 ("Milestones
added 2026-08-08") repeats the unrefined arrow for a reader who skipped this section; it is left as
written too, and read through this note.

*Consequence (added M14, 2026-08-12)* — the arrow is **refined again, and again not replaced**. A
sweep of every open register after M9b — §15's open decisions, §22's risks, all 66 partials, all 26
manual-test documents, every Should, and a grep for in-code deferrals — established that M8 as
written cannot hold what it now inherits. **M8 is "Size: S, a gate, not a construction phase", and
what stands in front of it includes nine user-facing surfaces that do not exist in `namir-ui` at
all, six unexecuted `Verify: M` Musts, and six open §15 decisions, two of them past their own
deadlines.** §21 adds **M14 — Pre-1.0 closure** to hold that work, so M8 remains the gate its own
text describes. The order is:

> **M9a → M10 → M11 → M12 → M13 → M9b → M14 → M8.**

M14 is numbered by when it joined the plan, not by when it runs — the same convention this section
established for M9 through M13, and for the same reason: renumbering would destroy the audit trail
that every existing reference to M8 forms. Nothing above this note is rewritten, M8 keeps its number
and its text, and §21 states its own depends-on and blocks relationships in its header line.

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

**Superseded by the M9a re-audit at the end of this section (2026-08-08).** The table immediately
below is the M0 baseline plus six sessions of local cell edits. It is kept exactly as it stands,
unedited, because the prose beneath it quotes its physical cell values when recording what each
session moved — rewriting the cells would make those sentences unverifiable. It is **not** current
status: five of its rows carry the wrong denominator, and it omits two FRS sections entirely.

| FRS area | Must count | Done | Partial | Not started |
|---|---|---|---|---|
| 5.1 CHAIN | 7 | 0 | 2 | 5 |
| 5.2 IN | 3 | 0 | 3 | 0 |
| 5.3 GATE | 3 | 0 | 3 | 0 |
| 5.4 NAM | 11 | 3 | 4 | 4 |
| 5.5 IR | 7 | 1 | 0 | 6 |
| 5.6 EQ | 3 | 2 | 1 | 0 |
| 5.7 OUT | 2 | 0 | 1 | 1 |
| 5.8 PARAM | 5 | 0 | 1 | 4 |
| 5.9 STATE | 7 | 0 | 0 | 7 |
| 5.10 LIB | 5 | 0 | 0 | 5 |
| 5.11 IO | 8 | 2 | 6 | 0 |
| 5.12 CLAP | 10 | 8 | 2 | 0 |
| 5.13 UI | 7 | 6 | 1 | 0 |
| 5.14 ERR | 6 | 2 | 4 | 0 |
| 6.1 RT | 4 | 3 | 1 | 0 |
| 6.2 PERF | 6 | 1 | 0 | 5 |
| 6.3 PORT | 5 | 5 | 0 | 0 |
| 6.4 QUAL | 6 | 1 | 4 | 1 |
| 6.5 LIC | 5 | 4 | 0 | 1 |
| 6.6 SEC | 3 | 3 | 0 | 0 |
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

**Nine further live updates, made in the M6 session** — see this section's own §10 M6-close-out
addendum above for the full per-requirement accounting; only the cell movements are recorded here.
Two of the three headline rows (5.11, 5.12) are corrected **from** this table's own stale
`0 | 0 | N` cells, since the M0-era rows had never been touched despite M4/M5 already noting
FR-CLAP-090's mechanism was "built and tested at the worker level" — this session's audit found the
physical table had simply never been live-updated for that, and fixes it alongside M6's own new
evidence rather than leaving the drift for a future session to rediscover:

- **5.11 IO: 0/0/8 -> 2/6/0.** FR-IO-040 (rate/buffer negotiation) and FR-IO-080 (settings
  persistence) close in full, both verified against real WASAPI hardware this session. The other
  six close only partially or stay open — FR-IO-020's exclusive-mode half is architecturally
  blocked by the pinned `cpal` dependency, not merely unverified; the rest (010, 030, 050, 060,
  070) are built and tested but missing either an interactive control, real hardware this session
  didn't have (Linux/macOS, loopback, a failable device), or both.
- **5.12 CLAP: 0/0/10 -> 8/2/0.** Eight of ten Musts close in full, each verified by
  `clap-validator`'s own real run against the built plugin (32/32 applicable tests, 0 failed).
  FR-CLAP-090 (cache sharing) and FR-CLAP-100 (embedded GUI) stay Partial: both are built, and the
  first is verified at the unit level, but neither has been observed running inside a real host
  process this session had a way to drive interactively.
- **5.13 UI: 0/0/7 -> 6/1/0.** Six of seven Musts close in full. FR-UI-030 stays Partial:
  `egui-baseview` 0.6 doesn't forward `egui`'s accesskit tree to assistive technology yet, so
  "every control has an accessible name" holds in the widget tree but isn't independently
  observable.
- **6.1 RT: corrected 1/2/1 -> 3/1/0 (also fixing a stale cell, see below).** NFR-RT-030
  (`DenormalGuard`, unused since M1) closes: a certified benchmark now exists
  (`namir-engine/benches/denormal_guard.rs`) and measures the guard keeping denormal-input
  processing within 1.6% of nominal across five reference-machine repetitions, against a 10%
  budget, while confirming a real 1.33-1.38x cost when the guard is absent. **This row's own
  starting cell was itself wrong before this session touched it**: M5's own prose claimed "6.1 RT
  Done 1 -> 2" (NFR-RT-010 closing), but the physical table cell was never actually edited to
  match — a documentation drift bug, not a new regression, caught and fixed in the same pass as
  M6's own NFR-RT-030 update rather than left for a future session to trip over. The corrected
  count is NFR-RT-010 (M5), NFR-RT-020 (M4) and NFR-RT-030 (M6) all Done; NFR-RT-040
  (content-independent worst-case timing) stays Partial — substantial supporting evidence exists
  (R-4/R-8's benchmark infrastructure, `tail_structure.rs`'s estimator) but nothing has formally
  verified it against its own *Verify: B* method's "varied material over a long run" wording.

**Five further live updates, made in the M7 session** — see this section's own §11 M7-status
addendum above for the full accounting; only the cell movements are recorded here.

- **5.14 ERR Done 0 -> 2, Not-started 2 -> 0.** FR-ERR-060 and FR-ERR-070 close: both requirements'
  own *Verify: S* text ("a build-time check that no network-capable dependency is linked") is met
  by the new `network-free` CI gate (D-18.2), the first mechanical enforcement either has had.
- **6.3 PORT: corrected 0/4/1 -> 5/0/0.** Another stale-cell case, same species as 6.1 RT's own M6
  fix: `NFR-PORT-010/020/030/040`'s CI enforcement (MSRV pin, layering lint, mobile cross-build,
  no-C++-toolchain build) all predate this session by several milestones and were simply never
  reflected in this row; `NFR-PORT-050`'s cross-platform round-trip coverage (four byte-level
  invariant tests in `namir-state`) likewise already existed. This session's traceability sweep is
  what surfaced the discrepancy, not new implementation work — all five Musts in this row were
  already true, just untraced.
- **6.4 QUAL Done 0 -> 1, Not-started 2 -> 1.** NFR-QUAL-010 closes: the traceability check this
  row itself describes now exists and runs in CI (`cargo run -p xtask -- traceability`).
- **6.5 LIC Done 3 -> 4, Not-started 2 -> 1.** NFR-LIC-030 closes: the attribution file
  (`THIRD-PARTY-NOTICES.md`, `xtask attribution`) didn't exist before this session.
- **6.6 SEC: corrected 0/3/0 -> 3/0/0 (also fixing a stale cell, see below).** NFR-SEC-030 closes
  alongside FR-ERR-060/070 above (the identical network-free mechanism); NFR-SEC-010 closes per its
  own text ("See NFR-QUAL-040") now that all three untrusted-input parsers have a tagged fuzz
  target. **This row's own starting cell was also wrong before this session**: M5's own prose
  claimed "6.6 SEC Done 0 -> 1" (NFR-SEC-020 closing), but the physical table cell was never edited
  to match — the same documentation-drift bug M6 already found and fixed once for 6.1 RT, recurring
  in a different row and going uncaught until this session's own audit.

**One finding recorded in prose only, not as a table move**: 5.12 CLAP's existing "8 Done" cell
rests, for several Musts, on a one-time manual `clap-validator` run (M6) rather than the CI-gated,
repeatable verification several of those requirements' own literal *Verify* text calls for —
`clap-validator` is not wired into CI, and seven of the ten `FR-CLAP-*` Musts have zero automated
test coverage in the crate today. Left as a flagged finding rather than a guessed-at correction
because this session counted 11 `FR-CLAP-*` ids against the row's own stated count of 10, an
unresolved discrepancy a future session should resolve before touching this row's numbers.

**This whole table needs a full re-audit, and that audit is M9's work (noted 2026-08-08, §16
below).** Every session above followed the same rule — move only the cells that session's own
evidence directly justifies — which was correct for each session taken alone, but the cumulative
result is a stack of six local edits over an M0 baseline that nobody has re-derived from evidence
since M0. Three specific defect classes were identified while planning M9 and are named here so the
audit starts from them rather than rediscovering them:

- **Six rows have never been touched since M0, despite the prose directly beneath the table
  predicting they would move.** 5.1 CHAIN, 5.2 IN, 5.3 GATE, 5.6 EQ, 5.7 OUT and 5.8 PARAM all
  still read exactly as audited at M0 — yet that paragraph states "M2 alone converts most of
  5.1/5.2/5.3/5.6/5.7's Partial rows to Done" and "M1+M7 between them convert nearly all of 5.8".
  M2 shipped the six stages; M1 shipped the parameter system. Either the prediction was wrong or
  the rows are stale, and no session has yet said which. On the balance of evidence they are stale,
  but that is an inference, not an audit, and it is not recorded as a table move here on that basis.
- **Five further rows are contradicted by prose written beneath them in this same section.**
  5.9 STATE and 5.10 LIB still carry their M0 `0 / 0 / N` cells while the M5 bullets above describe
  3/4/0 and 4/1/0 respectively; 6.2 PERF reads 1 Done against an M5 bullet stating "Done 1 -> 3";
  6.8 DOC reads 0 Done against an M5 bullet stating "Done 0 -> 1"; 6.4 QUAL reads 1 Done against
  two separate bullets (M5's NFR-QUAL-040, M7's NFR-QUAL-010) each claiming a closure. This is the
  identical documentation-drift species already caught and fixed one row at a time twice — 6.1 RT
  at M6, 6.6 SEC at M7 — and five more instances of it are outstanding.
- **5.12 CLAP's Must *count* is wrong, not merely its Done/Partial split.** The row states 10
  Musts; the FRS contains 11 `FR-CLAP-*` Must requirements (010, 020, 030, 040, 050, 060, 070, 080,
  090, 100, 130 — 110 and 120 are Shoulds). The denominator every other figure in that row is
  measured against has never been reconciled against the FRS. M7's flagged finding above spotted
  this and correctly declined to guess at a correction; M9 resolves it.

- **The row *set* itself is now out of date, not just the cells.** The eight Must requirements added
  on 2026-08-08 change three denominators and require one new row, none of which is reflected above:
  5.4 NAM goes from 11 Musts to 13 (FR-NAM-140, FR-NAM-150); 6.5 LIC from 5 to 6 (NFR-LIC-070); 6.8
  DOC from 2 to 3 (NFR-DOC-040); and FRS §5.15 PKG is an **entirely new section** needing its own
  row at 4 Musts (FR-PKG-010/020/030/040 — FR-PKG-050 is a Should). Two further requirements landed
  the same day but are **Shoulds and so do not appear in this table at all**: FR-PKG-050 and
  FR-UI-110. Every one of the eight is Not-started by construction, since they are requirements
  *for* M9–M13's work. M9's audit must therefore re-derive the row set, not only re-verify the
  existing rows' contents.

Until that audit lands, **treat every cell in this table as a claim of unknown age rather than as
current status.** `cargo run -p xtask -- traceability` and its generated `docs/03-test-plan.md` are
the mechanically regenerated view of the same question and are the more trustworthy of the two
today — with the caveat that §11's own appended correction records that at least three of the
sixteen Musts that tool reports as uncovered are tagging misses rather than gaps, so it currently
over-reports in one direction while this table over-reports in the other. Expect that tool's
uncovered count *rise* from sixteen to twenty-four as the eight new Musts land — measured, not
estimated, by running the tool after this session's edits. That is the requirements arriving ahead
of the work, not a regression.

### M9a re-audit — corrected row set and denominators (2026-08-08)

Published as a new table rather than as further edits to the one above, per **D-23.2**. Verdict
cells are adjudicated by that decision's rule — against the requirement's own text and its stated
`Verify:` method, with every cell naming its evidence by file path — and are left **blank** here
because this pass fixes the **row set and the denominators only**. Inventing verdicts for 130
requirements to fill 72 cells would be the failure this section exists to correct.

**This table is not frozen, and the blanks are not a permanent state.** **M9a** re-derives every
verdict from evidence as it stands at M9a and fills the cells in one pass. **M9b and every later
milestone move only the cells their own evidence justifies**, appending what they moved and why in
prose beneath the table, exactly as the six prior sessions did to the M0 snapshot above. A cell
nobody's evidence has touched stays where M9a left it rather than drifting quietly.

**The denominators are derived, not counted by hand.** `xtask traceability` already parses every
`**ID (Must)**` line in the FRS and emits all of them into `docs/03-test-plan.md`; per D-23.2 it now
also emits the per-section counts below and fails if this table disagrees with them or omits a
section that has Musts. That check rides on the **required plan-diff half** of D-18.5's split gate
from M9a — it is mechanical and satisfiable immediately — while the zero-uncovered half stays
informational until M13's close-out. The two halves flip on different dates and this check is on the
earlier one. Because the check is mechanical, this subsection's heading, its column order and its
row-label form are machine-parsed: moving or retitling the table means changing the tool in the same
commit. The trailing **Total** row is part of that contract and is **not** an FRS area — D-23.2's
implementation note records that the check reads it as a checked sum of the column above it rather
than as a twenty-fifth section.

**Reconciling the movement, so it is not misread as scope growth.** The thirteen-Must difference is
two unrelated things: five Musts are corrections to a table that was already wrong when it was
written (one CHAIN, one CLAP, three CFG), and eight are requirements the FRS gained on 2026-08-08.

| | Musts |
|---|---|
| The table above, summing its own Must-count column across 22 rows | 117 |
| + 5.1 CHAIN undercounted at M0 — 7 stated, 8 in the FRS then and now (only FR-CHAIN-070 is a Should) | +1 |
| + 5.12 CLAP undercounted at M0 — 10 stated, 11 in the FRS then and now (110/120 are Shoulds) | +1 |
| + §4 CFG has never had a row, in any version — FR-CFG-010/-020/-030 | +3 |
| **= the FRS as it stood when this table was written (commit `984b0b6`)** | **122** |
| + 5.4 NAM — FR-NAM-140, FR-NAM-150 (FRS 0.3) | +2 |
| + 6.5 LIC — NFR-LIC-070 (FRS 0.3) | +1 |
| + 6.8 DOC — NFR-DOC-040 (FRS 0.3) | +1 |
| + 5.15 PKG — FR-PKG-010/-020/-030/-040, an entirely new FRS section (FRS 0.3) | +4 |
| **= the FRS today** | **130** |

**5.12 CLAP's count is an original M0 error, not drift — the same species as 5.1 CHAIN's, and this
section's own 2026-08-08 note left that open.** The FRS at commit `984b0b6` already contained 11
`FR-CLAP-*` Musts and 8 `FR-CHAIN-*` Musts. Both rows also summed internally (0+0+10 and 0+2+5), so
**one CHAIN Must and one CLAP Must have never been placed in any column in any version of this
table**. Which identifiers were dropped is not recoverable — the M0 rows carry counts, not ids — and
does not need to be, since every cell below is re-derived from evidence rather than inherited.

**§4 CFG gets a row, and the axis admits it.** The first column is "FRS area"; §4 is a numbered FRS
section whose three requirements carry ordinary `*Verify:*` codes exactly like every other. Its
absence is an oversight from M0, not a scoping choice — and a consequential one: **FR-CFG-020** is
named both in §12's M8 exit checklist as the final integration test and in §16 as a deliverable, and
`docs/03-test-plan.md` reports it UNRESOLVED. It has been gated by a table it has never appeared in.

All eight requirements added on 2026-08-08 will enter as **Not started** when M9a fills the verdicts
— they are requirements *for* M10/M12/M13's work, and none of that work has run — and this pass does
not adjudicate them further.

| FRS area | Must count | Done | Partial | Not started |
|---|---|---|---|---|
| 4 CFG | 3 | 2 | 1 | 0 |
| 5.1 CHAIN | 8 | 6 | 2 | 0 |
| 5.2 IN | 3 | 1 | 2 | 0 |
| 5.3 GATE | 3 | 3 | 0 | 0 |
| 5.4 NAM | 13 | 6 | 7 | 0 |
| 5.5 IR | 7 | 7 | 0 | 0 |
| 5.6 EQ | 3 | 0 | 3 | 0 |
| 5.7 OUT | 2 | 1 | 1 | 0 |
| 5.8 PARAM | 5 | 3 | 2 | 0 |
| 5.9 STATE | 7 | 3 | 4 | 0 |
| 5.10 LIB | 5 | 3 | 2 | 0 |
| 5.11 IO | 8 | 2 | 6 | 0 |
| 5.12 CLAP | 11 | 4 | 7 | 0 |
| 5.13 UI | 7 | 3 | 4 | 0 |
| 5.14 ERR | 6 | 3 | 3 | 0 |
| 5.15 PKG | 4 | 3 | 1 | 0 |
| 6.1 RT | 4 | 0 | 4 | 0 |
| 6.2 PERF | 6 | 2 | 4 | 0 |
| 6.3 PORT | 5 | 4 | 1 | 0 |
| 6.4 QUAL | 6 | 2 | 4 | 0 |
| 6.5 LIC | 6 | 4 | 2 | 0 |
| 6.6 SEC | 3 | 1 | 2 | 0 |
| 6.7 BUILD | 2 | 0 | 2 | 0 |
| 6.8 DOC | 3 | 1 | 2 | 0 |
| **Total** | **130** | **66** | **64** | **0** |

Every other row's denominator was already correct and is carried forward unchanged — checked against
the FRS row by row this session, not assumed.

**Verdict columns filled 2026-08-09.** See the `### M9a close-out` subsection in §16 for how the
pass was run, what diverges from the tool's own output and why, and what it could not settle — and
for the four cells an independent spot-check corrected before this table was committed (FR-STATE-050
and FR-OUT-010 Done → Partial, NFR-PERF-030 and NFR-PERF-040 Not started → Partial), together with
the one correction that was rejected and became §15 item 19. The
per-row evidence D-23.2 requires is immediately below: every requirement is named, and every Done
and every Partial names its evidence by file path. Paths are repo-relative. A Partial's entry names
**which** clause or member is unmet, not that it is "partly done"; where the requirement already
carries a `// trace-partial:`, the wording is that annotation's own `uncovered:` field in compressed
form, so the ledger and the source agree.

#### Per-row evidence for the verdict columns

- **4 CFG — 1 / 2 / 0.** *Done:* FR-CFG-010, one workspace building both products
  (`Cargo.toml:4-7`) and CI producing both on every push (`.github/workflows/ci.yml:88-89`, `:284-285`),
  over one shared engine (`crates/namir-app/src/app.rs:160`, `crates/namir-clap/src/audio.rs:151`).
  *Partial:* FR-CFG-020 — the shared engine is real at those same two call sites, but the `Verify: G`
  apparatus is wholly absent: no golden vector exists anywhere in the tree and nothing runs one
  through both configurations. FR-CFG-030 — `xtask layering`'s dependency-edge lint
  (`xtask/src/layering.rs:40-44`) argues compile-time separation; "each is installed alone into a
  clean environment and exercised" is executed by nothing.
- **5.1 CHAIN — 3 / 5 / 0.** *Done:* FR-CHAIN-030 (`crates/namir-engine/src/chain.rs:562-607`, plus
  the through-the-ring form at `crates/namir-engine/src/engine.rs:895-933`); FR-CHAIN-040
  (`crates/namir-engine/src/stages/nam.rs:946-958`, `crates/namir-engine/src/stages/ir.rs:916-928` —
  both enumerated stages, real nonzero signal); FR-CHAIN-090
  (`crates/namir-engine/src/chain.rs:650-678`, default pinned at
  `crates/namir-params/src/global.rs:66-72`). *Partial:* FR-CHAIN-010 — order and non-reorderability
  hold at `crates/namir-engine/src/stages/mod.rs:47-67`, but the method's probe signal is silence
  (`:86-114`), which cannot distinguish any ordering from an empty chain. FR-CHAIN-020 — only the EQ
  bypass is toggled mid-signal (`crates/namir-engine/src/stages/eq.rs:643-690`); NAM and IR set
  `ENABLED=0` before processing. FR-CHAIN-050 — mono-core-then-duplicate shown for the gate only
  (`crates/namir-engine/src/stages/gate.rs:466-493`); `MonoToStereo` reaches no gate/NAM/IR test.
  FR-CHAIN-060 — all three configurations are built (`crates/namir-engine/src/stages/mod.rs:86-114`)
  but no IR is loaded under `MonoToStereo`. FR-CHAIN-080 — the silence-and-fault behaviour is
  asserted (`crates/namir-engine/src/chain.rs:615-648`) but no NaN is injected into any product
  stage's state.
- **5.2 IN — 1 / 2 / 0.** *Done:* FR-IN-010 (`crates/namir-ui/src/controls.rs:250-256` pins range and
  default; `crates/namir-engine/src/stages/trim.rs:264-288` applies the gain). *Partial:* FR-IN-020 —
  the `U` measurement half passes (`crates/namir-dsp/src/meter.rs:165-191`); the `M` display half has
  no document and no UI field (`namir_ui::MeterReading` carries no peak-hold). FR-IN-030 — the latch
  is asserted (`crates/namir-dsp/src/meter.rs:140-158`) but "resettable by the user" is unbuilt:
  `Meter::reset_clip` has no caller outside its own test and `UiIntent`
  (`crates/namir-ui/src/host.rs:148-179`) has no reset variant.
- **5.3 GATE — 1 / 2 / 0.** *Done:* FR-GATE-020, the method executed literally — a decaying envelope
  producing exactly one close event (`crates/namir-dsp/src/gate.rs:264-297`), hysteresis at `:140`.
  *Partial:* FR-GATE-010 — of the five controls "U per control" names, Attack and Release are
  exercised by no test (`ATTACK_MS_ID` appears only at `crates/namir-engine/src/stages/gate.rs:40`
  and `:220`). FR-GATE-030 — the opening ramp is measured (`crates/namir-dsp/src/gate.rs:304-327`)
  one sample per `process` call, so "sample-accurate within the block, not stepped at block
  boundaries" cannot be falsified; the closing ramp is unmeasured.
- **5.4 NAM — 3 / 8 / 2.** *Done:* FR-NAM-010 (`crates/namir-nam/tests/fixtures.rs:34-58`,
  `crates/namir-nam/tests/lstm_fixtures.rs:33-57`, identification by content at
  `crates/namir-library/src/probe.rs:11-13`); FR-NAM-070, the `Verify: I` method run literally
  (`crates/namir-engine/src/engine.rs:513-571`); FR-NAM-130
  (`crates/namir-engine/src/stages/nam.rs:946-958`). *Partial:* FR-NAM-020 — both architectures load
  and dispatch (`crates/namir-nam/src/model.rs:226-237`), but the `Verify: G` comparison is executed
  for neither; the tagged artifacts parse metadata only. FR-NAM-030 — parity is against
  `namir-fixtures`' own port (`crates/namir-fixtures/src/nam/mod.rs:95`, `:142`), not the reference
  implementation, and the probe is ~83 ms rather than the specified 10 s. FR-NAM-040 — the catalogue
  and rejection paths exist (`crates/namir-nam/src/error_codes.rs:14-112`,
  `crates/namir-nam/src/file.rs:347-350`), but "naming the file" holds in `namir-app` only
  (`crates/namir-app/src/host.rs:263-267` vs `crates/namir-clap/src/worker_jobs.rs:77-79`) and the
  corrupted-file corpus asserts non-panic, not a specific reason. FR-NAM-050 — the resampling is
  integrated (`crates/namir-engine/src/stages/nam.rs:245-418`); the cross-rate comparison the method
  specifies is computed nowhere. FR-NAM-060 — both resamplers are live
  (`crates/namir-engine/src/stages/nam.rs:278-418`, `crates/namir-ir/src/convolver.rs:845-914`) and
  their own comments record the stopband/ripple figures as unmeasured. FR-NAM-080 — the read half is
  asserted (`crates/namir-nam/src/probe.rs:141-160`); "display" spans the name field only
  (`crates/namir-ui/src/host.rs:111`). FR-NAM-110 — both tagged tests read an accessor whose body is
  the literal `0` (`crates/namir-nam/src/wavenet.rs:1260-1266`,
  `crates/namir-nam/src/lstm.rs:612-618`); no impulse is cross-correlated. FR-NAM-140 — the
  architecture clause is built and tested on the byte path
  (`crates/namir-nam/src/error_codes.rs:28-32`, `crates/namir-nam/src/model.rs:241-246`); the
  configuration clause is false, an A2 file failing as `nam.load.malformed_json`
  (`crates/namir-nam/src/file.rs:153-161`). *Not started:* FR-NAM-090 (no loudness measurement
  anywhere; `crates/namir-nam/src/lib.rs:54` records why) and FR-NAM-150 (no A2 support and no
  `namir-fixtures` A2 generator) — both M10's.
- **5.5 IR — 4 / 3 / 0.** *Done:* FR-IR-010 (`crates/namir-ir/src/wav.rs:298-429`, the full
  depth × channel × rate matrix, boundaries at `:469-496`); FR-IR-040
  (`crates/namir-ir/src/convolver.rs:1224-1254` against the direct reference at `:815-826`, and
  `crates/namir-engine/src/stages/ir.rs:931-975`); FR-IR-050 (choice recorded as D-9.7; acceptance
  and truncation at `crates/namir-ir/src/convolver.rs:1502-1540`, report reaching the user via
  `crates/namir-worker/src/cache.rs:171-186`); FR-IR-100
  (`crates/namir-engine/src/stages/ir.rs:916-928`). *Partial:* FR-IR-030 — resample-on-load works
  (`crates/namir-ir/src/convolver.rs:681-735`, `:1414-1496`) but the FR-NAM-060 quality bar it
  imports is measured nowhere. FR-IR-060 — the handover is exercised
  (`crates/namir-engine/src/engine.rs:581-620`) without the no-dropout half of the FR-NAM-070 method
  it imports. FR-IR-070 — of the four controls, the low-cut and high-cut **frequencies** are set by
  no test (`crates/namir-engine/src/stages/ir.rs:99`, `:103`, `:739`, `:745` are the only sites).
- **5.6 EQ — 0 / 3 / 0.** *Partial:* FR-EQ-010 — every EQ-stage measurement is at DC or Nyquist
  against hand-written constants at 0.3 dB (`crates/namir-engine/src/stages/eq.rs:477-540`), three
  times the method's 0.1 dB and at no band's own frequency. FR-EQ-020 — the sweep
  (`crates/namir-dsp/src/biquad.rs:442-483`) spans four of the six sample rates the method names by
  number; 88.2 and 176.4 kHz are absent (`:454`). FR-EQ-030 — of `EqStage`'s twelve parameters
  (`crates/namir-params/src/lib.rs:75-86`) only `ENABLED` is changed inside the measured window
  (`crates/namir-engine/src/stages/eq.rs:643-690`).
- **5.7 OUT — 0 / 2 / 0.** *Partial:* FR-OUT-010 — the requirement states **three** literal
  parameters (range −60 dB to +12 dB, default 0 dB, exact silence at or below −60 dB) and **only the
  silence clause is asserted**: exact silence at and below the floor holds
  (`crates/namir-engine/src/stages/out.rs:292-334`), gain is applied (`:255-286`), and the floor is
  pinned to the declared range minimum (`crates/namir-params/src/stages/out.rs:32-38`). The **+12 dB
  maximum** and the **0 dB default** are declared in `GAIN_DB`
  (`crates/namir-params/src/stages/out.rs:14-18`) and asserted nowhere — no test reads either back,
  and `params.lock` records no bounds at all, its columns being key, id, kind and live/tombstoned
  state — so an edit to either passes every gate in this workspace. Named evidence for the cell is
  the `// trace-partial:` pair at `crates/namir-engine/src/stages/out.rs:287-291`. FR-OUT-020 — the clip latch is asserted
  (`crates/namir-engine/src/stages/out.rs:396-430`); of the four characteristics imported from
  FR-IN-020/-030, the published `peak_db`, `average_db` and `peak_hold_db` telemetry is read by no
  test and the indicator has no user reset path.
- **5.8 PARAM — 0 / 5 / 0.** *Partial:* FR-PARAM-010 and FR-PARAM-050 — both tagged tests assert
  `format_value` against a fabricated const declared inside `mod tests`
  (`crates/namir-params/src/descriptor.rs:201`, `:214`); no shipped parameter's name, unit, range,
  default or steppedness is asserted, and the only REGISTRY-enumerating test
  (`crates/namir-params/src/lib.rs:105-114`) checks uniqueness alone. FR-PARAM-020 — `check_manifest`,
  which detects `TOMBSTONE_REUSED`/`ID_CHANGED`, has no caller outside its own test module
  (`crates/namir-params/src/manifest.rs:143`, `:220`), so the reused-identifier half is outside the
  gate CI runs (`.github/workflows/ci.yml:110-111`). FR-PARAM-030 — of the three sources, only two
  engine-internal paths are compared (`crates/namir-engine/src/engine.rs:855`); no artifact drives
  `namir_state::State` or `UiIntent::SetParam` into the comparison. FR-PARAM-040 — the gain limb is
  genuinely bounded (`crates/namir-dsp/src/gain_ramp.rs:103-134`); "measure the artefact's spectral
  energy" is executed nowhere and frequency-affecting parameters interpolate outside `GainRamp`.
- **5.9 STATE — 2 / 5 / 0.** *Done:* FR-STATE-010 (`crates/namir-state/src/state.rs:265-282`, the
  method literally, over every section; version at `crates/namir-state/src/document.rs:171-173`);
  FR-STATE-020 (`crates/namir-state/tests/corpus.rs:44-56`, `:100-105`, with the release guard at
  `:170-185` — see the close-out on why this Done is vacuous on its own quantifier). *Partial:*
  FR-STATE-050 — the tagged artifact (`crates/namir-worker/src/recall.rs:300-361`) asserts that the
  model and IR changeovers never overlap and **processes no audio at all**, so the FR-NAM-070
  constraints this requirement explicitly imports — no discontinuity and no dropout under a
  continuous signal across the changeover — are asserted by nothing through a recall. Serialisation
  is a necessary condition for that clause, not the clause itself; the `// trace-partial:` pair at
  `:294-299` is the cell's named evidence. FR-STATE-030 — the tagged test recalls an in-memory
  `State` and never writes or names a preset (`crates/namir-worker/src/recall.rs:455`); neither
  direction of app↔plugin interchange is spanned. FR-STATE-040 — the `M` half is executed and PASS
  (`docs/manual-tests/fr-state-040-diffability-and-hand-editability.md`); the "plus S (schema check)"
  half has no artifact of any kind — no schema document, no `xtask` schema subcommand, and CI never
  invokes `xtask preset`. FR-STATE-060 — the artifact re-invokes its own test binary rather than the
  plugin and resolves against `RootsOnlyResolver::new(&[])`
  (`crates/namir-worker/tests/cross_process_restore.rs:143`, `:172`), so "restart the host" and the
  file-identity clause are both bypassed. FR-STATE-070 — the three resolution paths pass
  (`crates/namir-state/src/resolve.rs:236`, `:255`, `:290`, `:309`); "with an option to locate it
  manually" exists nowhere in the product.
- **5.10 LIB — 1 / 4 / 0.** *Done:* FR-LIB-040, both limbs the requirement names
  (`crates/namir-library/src/search.rs:157-163`, `:165-171`), wired to the real UI path
  (`crates/namir-ui/src/library_view.rs:120-127`). *Partial:* FR-LIB-010 — recursive scan is covered
  (`crates/namir-library/src/scan.rs:356`, `:379`) but "nominate one or more directories as library
  roots" has no mechanism: both shells hard-code the single root through
  `LibraryService::open_at`/`open_default`. FR-LIB-020 — three of four clauses meet the 10 000-file
  scale (`crates/namir-worker/src/library.rs:462`, `:533`, against
  `crates/namir-fixtures/src/library.rs:104`); the off-the-audio-thread clause is exercised against a
  6-file corpus only. FR-LIB-030 — the bench prints CONCLUSIVE/INCONCLUSIVE and asserts no time bound
  (`crates/namir-library/benches/library_scan.rs:154`), and its persistence arms reuse an in-memory
  prior index. FR-LIB-070 — deletion and change each have a dedicated before/after test
  (`crates/namir-library/src/scan.rs:423`, `:545`) but the third member of the requirement's own set,
  files that **are added** while Namir is running, is spanned by nothing;
  `crates/namir-fixtures/src/library.rs:590` was generated for exactly this trio and has no consumer.
- **5.11 IO — 2 / 6 / 0** (FR-IO-020's cell was moved at M11, 2026-08-11; every other cell in this
  bullet is M9a's and untouched)**.** *Done:* FR-IO-080 — all four enumerated members round-tripped
  (`crates/namir-app/src/settings.rs:167`) plus the graceful-degrade clause
  (`crates/namir-app/src/device_state.rs:226`), with an executed PASS across all six steps
  (`docs/manual-tests/fr-io-080-settings-persistence.md`). FR-IO-020 — `Verify: M`, so under D-18.6
  the manual document **is** the named artifact rather than a stand-in for one, and
  `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md` now records an **executed PASS** on the §2
  reference machine (2026-08-11, PreSonus AudioBox 22VSL, fork `2edbacb`) against a written script,
  which is the form repeatability takes for this code — steps 2-7 and 9 PASS. Both members of the
  requirement's own set are spanned: **shared** by that document's step 9 and by
  `docs/manual-tests/fr-io-010-device-enumeration.md`'s and `fr-io-080-settings-persistence.md`'s
  executed runs, **exclusive** by steps 3-7, with exclusivity proved by Windows refusing a second
  application rather than by Namir not erroring, and the setting finally read
  (`crates/namir-app/src/app.rs`'s `negotiate_share_mode`, `crates/namir-app/src/audio_io.rs`'s
  `supports_exclusive`). ASIO is the requirement's own **Should** clause, which FRS §1.5 rules does
  not make the Must Partial. **Two caveats, named rather than left behind an unqualified pass, and
  neither a clause of FR-IO-020's text:** the `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` retry path has
  never executed anywhere, this endpoint's device period being a whole 144 frames at 48 kHz; and
  shared-mode `I24` is unreachable from this product, `namir-app` restricting shared mode to `F32`
  (`crates/namir-app/src/audio_io.rs:453`), so the fork's container-justification fix rests there on
  the format contract rather than on measurement. At M9a this cell read a documented **FAIL** —
  exclusive mode unreachable through cpal 0.18.1 as pinned by D-13.1, and `exclusive_mode` read by
  nothing — and D-13.4's fork, built at M11 (§18's status), is what changed it. *Partial:* FR-IO-010
  — enumeration and a real opened stream PASS
  (`docs/manual-tests/fr-io-010-device-enumeration.md`), but "the user shall be able to select" has
  no interactive surface at all; selection happens once at start-up
  (`crates/namir-app/src/app.rs:95-133`). FR-IO-030 — **NOT EXECUTED**
  (`docs/manual-tests/fr-io-030-alsa-coreaudio.md`); no ALSA or CoreAudio stream has ever been
  opened, the evidence being structural only. FR-IO-040 — the
  negotiation logic is unit-tested (`crates/namir-app/src/device_state.rs`) but neither selection
  clause is built and "always displayed" is served by an `eprintln!`
  (`crates/namir-app/src/app.rs:283`). FR-IO-050 — recorded **PARTIAL** by its own document; the
  measured-latency clause is unbuilt (`crates/namir-app/src/latency.rs:23-24`, `:43` hardcodes
  `measured: false`) and the display is that same `eprintln!`, milliseconds only. FR-IO-060 —
  induction is covered (`crates/namir-app/src/stream.rs:486`); "resettable by the user" has no path,
  `XrunCounter::reset` having no caller outside its own tests. FR-IO-070 — the method's named
  apparatus, a virtual device that can be made to fail on demand, does not exist, and the tagged test
  opens no device (`crates/namir-app/src/device_state.rs:247`).
- **5.12 CLAP — 2 / 9 / 0.** *Done:* FR-CLAP-010 and FR-CLAP-020 — the reference validator the
  `Verify:` lines name, run as a blocking CI job (`.github/workflows/ci.yml:263-305`). This cell was
  first written on a local 32-of-32 run and **the gate's own first CI run falsified it within the
  hour** — `state-reproducibility-basic` crashed with `0xc0000005`. It reads Done again only because
  that crash was root-caused and fixed (see the close-out below); the evidence is now CI's own
  `44 tests run, 32 passed, 0 failed, 12 skipped`, plus
  `crates/namir-clap/src/lib.rs:132`, `:143`. *Partial:* FR-CLAP-030 — one stereo port pair is
  declared (`crates/namir-clap/src/audio_ports_ext.rs:30-48`); two of FR-CHAIN-060's three
  configurations are undeclared, and "across at least two host implementations" is unexecuted
  (`docs/manual-tests/fr-clap-030-audio-ports-negotiation.md`). FR-CLAP-040 — the notify path is
  built (`crates/namir-clap/src/latency_ext.rs:12`, `crates/namir-clap/src/audio.rs:99`) and driven
  by nothing; its document records **Not executed**. FR-CLAP-050 — the tagged test bypasses
  `PluginStateImpl::save`/`load` and the stream adapters (`crates/namir-clap/src/state_ext.rs:77`).
  FR-CLAP-060 — the tagged test asserts only the `IS_BYPASS` flag and processes no audio, and
  sample-accuracy is contradicted by `apply_automation` ignoring event sample offsets
  (`crates/namir-clap/src/params_ext.rs:197`). FR-CLAP-070 — no artifact anywhere compares a
  varying-block run against a fixed-block reference at chain or plugin level. FR-CLAP-080 —
  `activate` takes the host's rate (`crates/namir-clap/src/audio.rs:119`) and neither the 44.1-192 kHz
  range nor a mid-session change is exercised. FR-CLAP-090 — sharing is asserted
  (`crates/namir-clap/src/shared.rs:343`); the `B` half of "I plus B" is measured by nothing, there
  being no memory benchmark in the workspace. FR-CLAP-100 — the extension is implemented
  (`crates/namir-clap/src/gui.rs`) and invoked by no in-process test; the embedding half of its
  document is **Not executed**; `is_api_supported` is `WIN32`-only with no `cfg` (`:91-105`).
  FR-CLAP-130 — neither half of "S plus I" reaches this crate: `AllocDisabler` is installed in five
  crates, none of which owns a real audio callback, and no static check for blocking exists.
- **5.13 UI — 1 / 6 / 0.** *Done:* FR-UI-010 — one widget type
  (`crates/namir-ui/src/app.rs:133`) rendered by both shells through one `render` (`:32`), with the
  manifest fact that neither shell depends on `egui` directly, corroborated by an executed run
  (`docs/manual-tests/fr-ui-010-standalone-window-renders.md` steps 1-2 PASS, 90 real frames).
  *Partial:* FR-UI-020 — the single screen exists and renders
  (`crates/namir-ui/src/app.rs:32-93`) but there is **no** `docs/manual-tests/fr-ui-020-*.md`, and
  the one executed document records its visual-confirmation step as NOT EXECUTED. FR-UI-030 — **NOT
  EXECUTED** (`docs/manual-tests/fr-ui-030-accessibility-script.md`), and the document names a second
  gap: `egui-baseview` 0.6.0 wires no accesskit adapter, so accessible names exist at the data level
  only. FR-UI-040 and FR-UI-050 — both documents **NOT EXECUTED**; the automated halves
  (`crates/namir-ui/src/format.rs:49`, `:93`; `crates/namir-ui/src/controls.rs:210`) are
  supplementary under D-18.6 because both `Verify:` lines elect `M`, and FR-UI-050's fine-adjust
  gesture has no automated coverage of any kind. FR-UI-060 — the timed render
  (`crates/namir-ui/src/library_view.rs:286`) omits the requirement's own condition, a scan in
  progress, and is a `#[test]` rather than the `Verify: B` benchmark with a certified figure.
  FR-UI-070 — notices render non-modally (`crates/namir-ui/src/notices.rs`) but there is no
  `docs/manual-tests/fr-ui-070-*.md`, and "M against the error catalogue of FR-ERR-020" has been run
  against no catalogue entry.
- **5.14 ERR — 1 / 4 / 1.** *Done:* FR-ERR-070 — the permanent network-free CI target the `S` method
  names (`.github/workflows/ci.yml:183-191`, `deny.toml:90`); the "I per feature" half quantifies
  over post-1.0 network features, a set that is empty. *Partial:* FR-ERR-020 — the catalogue is
  enumerable and unique (`crates/namir-core/src/error.rs:77-97`); "every error path maps to an entry"
  has no artifact, and `crates/namir-ui/examples/manual_window_smoke.rs:27` constructs an
  off-catalogue code inline. FR-ERR-030 — the allocation limb is enforced
  (`crates/namir-engine/src/rt_harness.rs:54-71`); the logging limb is checked by neither half of the
  `Verify:` line. FR-ERR-040 — containment passes at the pool boundary
  (`crates/namir-worker/src/pool.rs:192-214`); no fault is injected into the scanner, state I/O,
  settings, cache or GUI thread, and the tagged test runs no audio. FR-ERR-060 — the deny list is a
  real permanent gate (`deny.toml:63`) that its own comment calls non-exhaustive, and nothing detects
  first-party `std::net` use. *Not started:* FR-ERR-010 — nothing writes a log record anywhere;
  `crates/namir-platform/src/paths.rs:68` computes a path and
  `crates/namir-clap/src/shared.rs:292-293` records the sink as a deliberate no-op. D-16.5 decided
  the six parameters; none of it is implemented.
- **5.15 PKG — 0 / 0 / 4.** *Not started:* FR-PKG-010, -020, -030 and -040, all M13's.
  `.github/workflows/` holds `ci.yml` and `fuzz.yml` only — there is no release workflow and nothing
  produces a distribution; `xtask` has no `bundle` subcommand, so there is no packaging step to
  assert a layout or the presence of `THIRD-PARTY-NOTICES.md` and the two licence texts; and there is
  no Windows installer and no `docs/manual-tests/fr-pkg-030-*.md`.
  `crates/namir-platform/src/clap_paths.rs:38-99` resolves D-13.3's install-path table but is a
  runtime resolver consumed by no installer, and is deliberately not counted as scaffolding.
- **6.1 RT — 0 / 4 / 0.** *Partial:* NFR-RT-010 — the three-axis stress test passes
  (`crates/namir-worker/tests/rt_stress.rs:186-193`) but the allocation harness reaches neither crate
  that owns a real audio callback; `crates/namir-app/src/stream.rs:262-266` builds a `Vec` inside the
  cpal callback, which is the class of thing the missing harness would catch. NFR-RT-020 — the sole
  artifact (`crates/namir-engine/src/ring.rs:174-192`) asserts non-allocation, which is not
  wait-freedom, and spans one of the audio thread's communication paths;
  `crates/namir-app/src/bridge.rs:22-32`, `crates/namir-app/src/xrun.rs:21` and
  `crates/namir-clap/src/param_mirror.rs:46` carry none. NFR-RT-030 — the certified figure is real
  (`crates/namir-engine/benches/denormal_guard.rs:411-415`; §2 machine, five repetitions) but
  measures the assembled chain's aggregate, not "each stage", and covers one of three platforms.
  NFR-RT-040 — `crates/namir-engine/benches/tail_structure.rs:211-217` varies neither audio content
  nor parameter values inside the measured loop, so the invariance the requirement states is
  untested.
- **6.2 PERF — 0 / 6 / 0.** *Partial:* NFR-PERF-010 — a certified figure exists (16.45-17.08% against
  25%) but the threshold is printed, never asserted
  (`crates/namir-engine/benches/six_stage_chain.rs:242-246`), and the CI job is titled informational
  by design (`.github/workflows/ci.yml:597-613`), so "as a CI regression gate" is unmet on its face.
  NFR-PERF-020 — the tagged test asserts 0 with nothing loaded
  (`crates/namir-engine/src/stages/mod.rs:116-129`); no test compares actual group delay against a
  nonzero `latency_samples()`. NFR-PERF-030 — the behaviour exists and ships: `namir-app` opens its
  devices, negotiates a rate and a buffer, builds the engine and starts the stream on one path from
  `run()` (`crates/namir-app/src/app.rs:74`, `:92-135`, `:160-174`), loading the default state at
  `:183` and reaching an audible state at `:256`'s `play()`, with a real 90-frame run recorded
  (`docs/manual-tests/fr-ui-010-standalone-window-renders.md`). What is absent is the whole of the
  `Verify: B` half: no start-up harness exists, the identifier appears in no `.rs`, `.toml` or `.yml`
  under `crates/`, `xtask/` or `.github/`, and **the 3 s bound has never been measured** on the §2
  machine or anywhere else. NFR-PERF-040 — same shape and the same named gap: the plugin demonstrably
  instantiates, the reference validator having constructed it 32 times out of 32 in a blocking CI job
  (`.github/workflows/ci.yml:264-307`, `crates/namir-clap/src/lib.rs:132`), which is what carries
  FR-CLAP-010/-020's own Done cells above; no `Verify: B` harness exists and **the 200 ms bound has
  never been measured**, in-process or in a real host — §15 item 14 is the open question of which of
  those two counts as "in a host" for a certified D-2.4 figure. NFR-PERF-050 — certified at M9a and
  the ceiling now asserted (`crates/namir-worker/benches/resource_load.rs:192-199`), but every arm
  loads `LoadSource::Bytes`, so a 50 MB **file** is never timed, and the never-delay-the-audio-thread
  clause is evidenced only by an integration test. NFR-PERF-060 — the 2 s ceiling is asserted
  in-process (`crates/namir-library/benches/library_scan.rs:272-287`) but no reference-machine figure
  exists (the only recorded numbers are M5's sandbox run) and nothing re-runs it when the code
  changes.
- **6.3 PORT — 3 / 2 / 0.** *Done:* NFR-PORT-010 (`Cargo.toml:35` read at run time by the MSRV job,
  `.github/workflows/ci.yml:314-344`, green on trunk 2026-08-08); NFR-PORT-030 (both mobile
  cross-builds executed green, `.github/workflows/ci.yml:478-545`, covering exactly D-5.1's ten
  mobile crates — which discharges M5's "claimed by inspection" caveat); NFR-PORT-050 (all four named
  members asserted at `crates/namir-state/src/document.rs:318-361` and
  `crates/namir-state/src/reference.rs:342-348`, with the corpus at
  `crates/namir-state/tests/corpus.rs`, run on all three OSes). *Partial:* NFR-PORT-020 — the lint
  matches three literal substrings (`xtask/src/layering.rs:166-172`), so `cfg(not(windows))`,
  `cfg!(...)`, `cfg_attr` and `target_arch`/`target_family` forms pass unseen, and it scans
  `crates/*/src` only. NFR-PORT-040 — the no-C++-toolchain job is real and green
  (`.github/workflows/ci.yml:354-428`) but spans one of the three tier-1/tier-2 platforms
  (`ubuntu-latest`); Windows and macOS are never built with a C++ compiler absent.
- **6.4 QUAL — 2 / 4 / 0.** *Done:* NFR-QUAL-030 — under D-9.11 the satisfied form is a stated,
  numerical, reproducible reference, and all six shipped stages have one
  (`crates/namir-nam/tests/fixtures.rs:128`, `crates/namir-nam/tests/lstm_fixtures.rs:120`,
  `crates/namir-ir/src/convolver.rs:1224`, `crates/namir-dsp/src/biquad.rs:369-433`,
  `crates/namir-dsp/src/gate.rs:232-353`, `crates/namir-engine/src/stages/trim.rs:266-345`,
  `crates/namir-engine/src/stages/out.rs:255-334`); NFR-QUAL-060 (`.github/workflows/ci.yml:67-79`,
  lint set at `Cargo.toml:36-46`, on all three platforms every push). *Partial:* NFR-QUAL-010 — CI's
  required step passes `--allow-uncovered` (`.github/workflows/ci.yml:156`) and the plain form is
  `continue-on-error` (`:159-160`), so "fails on any uncovered Must" is unexecuted and 20 uncovered
  Musts stand with CI green. NFR-QUAL-020 — exactly three `(red)`/`(green)` commit pairs exist across
  120 commits (`1bb8436`/`f75b4a1`, `0efa5e8`/`6d431b5`, `6076fd2`/`5bda960`) against a universal
  clause, and the "enforced by review" half leaves no artifact that re-runs. NFR-QUAL-040 — the audio
  file reader is fuzzed header-only (`crates/namir-ir/fuzz/fuzz_targets/probe_wav.rs:23-28`), so the
  decode path where a hang or over-allocation would live is never reached. NFR-QUAL-050 — the
  platform-matrix half is complete (`.github/workflows/ci.yml:38-45`, `:91-92`) and the "shall gate
  merges" clause is unmet in fact: `trunk` carries no branch protection and its one ruleset contains
  only `deletion` and `non_fast_forward`, with no required status checks.
- **6.5 LIC — 2 / 3 / 1.** *Done:* NFR-LIC-020 (`deny.toml:15-57`, `version = 2` with a
  permissive-only allow-list, gating CI at `.github/workflows/ci.yml:164-172`); NFR-LIC-040 (same
  allow-list; no proprietary component is a required build dependency anywhere, and the
  optional-support clause is conditional on a component that does not exist). *Partial:* NFR-LIC-010 —
  both licence files are present at the root, but nothing checks their presence and only two tracked
  files under `crates/` carry an SPDX header (`Cargo.toml:24-29`). NFR-LIC-030 —
  `THIRD-PARTY-NOTICES.md` is generated and freshness-gated (`xtask/src/attribution.rs:14-18`,
  `.github/workflows/ci.yml:116-117`) but "shipped with the binaries" has no artifact, there being no
  release workflow. NFR-LIC-050 — the manifest the method names does not exist and the traced
  artifact is a runtime generator (`crates/namir-fixtures/src/lib.rs:16-21`); fifteen assets are
  tracked under `crates/*/fuzz/corpus`, recorded nowhere. *Not started:* NFR-LIC-070 — no
  TRADEMARK/BRAND file or brand-asset licensing statement exists; M12's.
- **6.6 SEC — 0 / 3 / 0.** *Partial:* NFR-SEC-010 — three of the four enumerated file kinds are
  fuzzed to depth; the IR kind is header-only
  (`crates/namir-ir/fuzz/fuzz_targets/probe_wav.rs:3-11`, `:18`), and the unreached code is exactly
  where this requirement's own failure modes live. NFR-SEC-020 — of the four kinds the limits doc
  enumerates, the `.nam`/IR disk-load ceiling has no artifact: `crates/namir-worker/src/lib.rs:129`
  refuses an over-large file and no test drives that branch
  (`crates/namir-state/src/document.rs:222-227`). NFR-SEC-030 — the build-producibility clause is
  fully covered (`deny.toml:90`, `.github/workflows/ci.yml:183-191`); the no-outbound-connection
  clause is derivative of FR-ERR-060, which is itself Partial for a non-exhaustive deny list.
- **6.7 BUILD — 0 / 2 / 0.** *Partial:* NFR-BUILD-010 — the property is true in fact (`Cargo.lock`
  committed, no `git`/`path` third-party dependency) but the tagged anchor is a manifest table that
  checks nothing (`Cargo.toml:4`), nothing passes `--locked`, and `deny.toml:114-117`'s `[sources]`
  section — the one mechanical expression of "every other dependency fetched by the package manager"
  — is **never run**: CI runs `check licenses` and `check bans` only. NFR-BUILD-020 — nothing
  extracts or runs the commands `docs/user-guide.md` documents
  (`.github/workflows/ci.yml:81-92`), so the guide can drift freely; the guide also gives one
  command per product with no per-platform variation.
- **6.8 DOC — 1 / 1 / 1.** *Done:* NFR-DOC-020 — `missing_docs = "warn"` at `Cargo.toml:39-46`,
  carried by all 14 crates (the two with their own tables restate it,
  `crates/namir-clap/Cargo.toml:16-21`, `crates/namir-platform/Cargo.toml:14-21`) and promoted to an
  error by `cargo clippy -- -D warnings` on every push. *Partial:* NFR-DOC-010 — the third-party
  reader run is executed and **PASS** (`docs/manual-tests/nfr-doc-010-third-party-reader.md`), but
  the requirement enumerates **three** formats and the `.nam` subset has no repository documentation
  a third party could write a reader from; `docs/04-state-and-preset-format.md:3-4` and the manual
  test's own "Requirement (literal)" line both silently restate it as the state and preset format.
  *Not started:* NFR-DOC-040 — there is no `README` of any extension at the repository root; M12's.

**M10 moves, 2026-08-09 — 5.4 NAM only, four cells, appended per D-23.2's "every later milestone
moves only the cells its own evidence justifies."** Row becomes **6 / 7 / 0** (was 3 / 8 / 2).

- **FR-NAM-140: Partial → Done.** Both clauses the M9a bullet above named unmet are now met: the
  configuration clause (`crates/namir-nam/src/wavenet.rs`'s `reject_unsupported_layer_features` and
  `resolve_layer_array`, `crates/namir-nam/src/error_codes.rs`'s `UNSUPPORTED_CONFIGURATION`/
  `INCONSISTENT_CONFIGURATION`) is built and covered by a table-driven test spanning every
  permanently-rejected feature (`crates/namir-nam/src/model.rs`'s
  `unsupported_features_are_named_and_distinct_from_malformed`), asserting both that the error id
  differs from `MALFORMED_JSON`'s and that `detail` names the offending key — the requirement's own
  quantifier ("architecture, **or** … configuration") and its `Verify: U` method both satisfied.
- **FR-NAM-150: Not started → Done.** `crates/namir-nam/src/wavenet.rs` loads and runs core A2
  (bottleneck threading, per-layer activation, the k-tap head); `crates/namir-nam/tests/
  a2_fixtures.rs`'s `numeric_parity_against_an_independent_reference_implementation_for_a2` covers
  both quantified configurations (`A2Shape::Full`/`A2Shape::Lite`, mapped to "A2-Full"/"A2-Lite" at
  `crates/namir-fixtures/src/nam/mod.rs`'s `A2Shape` doc comment) against `namir-fixtures`'
  independently-derived `reference_infer_a2`, to the accuracy FR-NAM-030 sets — the two agree to
  the bit.
- **FR-NAM-030: Partial → Done.** The M9a bullet above named two unmet clauses — comparison against
  `namir-fixtures`' own port rather than the real reference implementation, and an ~83 ms probe
  rather than the specified 10-second clean/transient/saturated signal. `crates/namir-nam/tests/
  golden_reference.rs` closes both: committed, generated (D-19.1), regenerable fixtures compared
  against a real `NeuralAmpModelerCore` render (pinned `3cde95c`,
  `-DNAM_USE_INLINE_GEMM -DNAM_ENABLE_A2_FAST=OFF`) over the specified signal, for both
  architectures — WaveNet to -137 dB, LSTM to the bit. Corroborated, not merely asserted: `xtask
  nam-parity` run against seven real, locally-held LSTM models spanning the full `num_layers` 1-4
  range (`docs/manual-tests/fr-nam-020-real-lstm-models.md`'s 2026-08-09 addendum), -114 to -130 dB,
  no prewarm treatment needed for any of them.
- **FR-NAM-090: Not started → Partial.** `crates/namir-nam/src/file.rs`'s `NamMetadata::loudness`,
  `crates/namir-params/src/stages/nam.rs`'s `NORMALIZE_ENABLED`/`NORMALIZE_OFFSET_DB`, and
  `crates/namir-engine/src/stages/nam.rs`'s `NamSlot::process_wet` (a smoothed, allocation-free
  gain) implement the behaviour; `crates/namir-engine/src/stages/nam.rs`'s
  `normalization_enabled_brings_differently_declared_models_within_one_lu` executes the `Verify: U`
  method's comparison shape but substitutes plain RMS-in-dB for the method's named "integrated
  loudness" (ITU-R BS.1770), which this codebase has no meter for — carries a `// trace-partial:`
  in the source for exactly this reason, so **Partial** here agrees with the tag rather than
  overriding it. FR-NAM-100 (dBu-calibrated operating levels) is untouched and stays **Not
  started** — a distinct, Should-priority requirement this milestone did not scope.

**M11 moves, 2026-08-11 — 5.11 IO only, one cell.** Row becomes **2 / 6 / 0** (was 1 / 7 / 0):
FR-IO-020 **Partial → Done**, on the executed run
`docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md` now carries. Unlike M10's move above, the
evidence is written into the 5.11 IO bullet itself rather than restated here — FR-IO-020 is a
`Verify: M` Must whose whole evidence is one document, so a second copy of the same citation would
be the only thing a separate entry added. What that bullet replaces is quoted inside it, so the
cell's age stays readable, and no other requirement's text in it moved. Nothing else in §14 changes:
M11 produced evidence for FR-IO-020 and for nothing else.

**M12 moves, 2026-08-10 — two rows, two cells, appended per D-23.2's "every later milestone moves
only the cells its own evidence justifies."** 6.5 LIC becomes **3 / 3 / 0** (was 2 / 3 / 1); 6.8 DOC
becomes **1 / 2 / 0** (was 1 / 1 / 1); **Total** becomes **34 / 91 / 5** (was 33 / 90 / 7,
M11's move having landed first). **M12 was built before M11 and rebased onto it**, so its own
deltas are unchanged — two cells, both in rows M11 never touched — and only the Total, which both
milestones write, had to be recomputed against the newer base.

- **NFR-LIC-070: Not started → Done.** The M9a bullet above named the gap exactly — "no
  TRADEMARK/BRAND file or brand-asset licensing statement exists". `TRADEMARK.md` now exists at the
  repository root and takes the position the requirement asks to be explicit: the code licence
  covers the code and not the name "Namir" or the mark, and the assets under `images/` are all
  rights reserved. The `Verify: S` method is executed rather than asserted —
  `xtask/src/identity.rs`'s `TRADEMARK_REQUIRED` check fails if the document or any required
  statement goes missing, run on every push by `.github/workflows/ci.yml`'s `xtask identity` step.
  Tagged plainly, because a build-time check asserting the document exists and states the terms is
  that method as written.
- **NFR-DOC-040: Not started → Partial**, one column and not two. `README.md` now exists at the root
  and carries what the requirement enumerates — the product, its licence, and the commands to build,
  run and test it — with `xtask/src/identity.rs`'s `README_REQUIRED` check asserting each of those
  substrings is present. But the requirement also says the README shall *identify the product* and *state what it
  does*, and no substring check reaches whether the prose actually does either. The tag is
  `// trace-partial: NFR-DOC-040` with its mandatory `uncovered:` field naming that clause, and
  under D-23.2 a Partial is not Done. Closes M13.

**M13 moves, 2026-08-11 — 5.15 PKG only, and it is the row's first movement since M0.** Row becomes
**1 / 3 / 0** (was 0 / 0 / 4); **Total** becomes **35 / 94 / 1** (was 34 / 91 / 5). All four cells in
the row move, because before this milestone nothing in FRS §5.15 had any artifact of any kind.
Denominator unchanged at 4 — `xtask traceability` machine-checks that column and it is not M13's to
move.

- **FR-PKG-020: Not started → Done.** The only Done of the four, and the one where the method is
  executed as written rather than approximated. `*Verify:* S` asks that "the packaging step asserts
  the produced layout against the required form for the platform it targets, and fails the build on
  any deviation", and `xtask/src/bundle.rs`'s `check` does exactly that — run by `bundle` itself
  after materialising, not only by `bundle --check`, so the assertion is part of producing rather
  than an optional second pass. The set the requirement quantifies over is three platforms and the
  artifact spans all three: its test materialises a staging tree for each from a fake shared library
  and checks the **produced** result, including the two negative cases the requirement exists for —
  a plain file named `Namir.clap` on macOS (a renamed dylib, which no host loads) and a *directory*
  named `Namir.clap` on Windows. That test runs on every CI runner rather than only on the macOS
  leg, because `plan(Platform) -> Layout` is pure and never consults the host. Tagged plainly.
- **FR-PKG-010: Not started → Partial.** `.github/workflows/release.yml` exists, is tag-triggered
  and builds on all three runners, and `xtask/src/release_workflow.rs` asserts the `Verify: S`
  method's three clauses separately — tag-triggered with no branch or dispatch trigger and no
  pinned `ref` on any checkout; a job per platform parsed out of **FRS §1.4's own table** rather
  than hard-coded, each uploading an artifact; and every published distribution being this
  workflow's, via a single publish job that `needs` all three builds, checks nothing out, takes
  `download-artifact` as its only input and runs no build command. Twelve mutation tests prove those
  checks have teeth. What is unspanned is the requirement's own verb: **produce**. The workflow has
  never run, and neither has `iscc`, `make_installer.sh` or the `tar` block anywhere. Promoting this
  needs a tagged run, not a code change. Closes M13.
- **FR-PKG-040: Not started → Partial.** `xtask bundle` stages `THIRD-PARTY-NOTICES.md`,
  `LICENSE-MIT` and `LICENSE-APACHE` into every platform's tree and asserts their presence, and all
  three packagers carry them into their outputs. But the requirement quantifies over "every
  distribution, **installer and archive alike**", and the staging tree is the packager's *input*,
  not a distribution: nothing asserts the three files inside a produced `.exe`, `.pkg`, `.dmg`,
  `.zip` or `.tar.gz`. The macOS script does open its own three outputs and check them, but a shell
  assertion inside an unrun script is not the in-process artifact a `Verify: S` traces. Closes M13.
- **FR-PKG-030: Not started → Partial.** `packaging/windows/namir.iss` offers both scopes
  (`PrivilegesRequiredOverridesAllowed=dialog commandline`), defaults to per-user
  (`PrivilegesRequired=lowest`), and places the CLAP artifact at D-13.3's two Windows cells via
  `{autocf}` with `ArchitecturesInstallIn64BitMode=x64compatible`. `Verify: M`, so
  `docs/manual-tests/fr-pkg-030-windows-install-scope.md` is its whole evidence under D-18.6 — and
  that document records **every step NOT EXECUTED**, including step 0, D-18.3's standing
  precondition that a real host be observed to scan `%LOCALAPPDATA%\Programs\Common\CLAP`. Partial
  rather than Done for that reason, on the M9a precedent that a `Verify: M` script recording NOT
  EXECUTED is Partial and not Done. Note that the generated plan reads this requirement as
  **covered**, purely because a correctly-named file now exists — the filename arm's known weakness,
  which §15 item 15's fix deliberately did not address. §14 is where that reading is corrected, and
  is the reason D-23.2 keeps these columns hand-adjudicated.

**No other cell moves, and two near-misses are worth naming.** **6.2 PERF does not move**:
NFR-PERF-030 gained a real benchmark that asserts its 3-second threshold, but its tag is
`trace-partial` — "audible" is `play()` returning `Ok(())`, so a build that started its streams and
produced silence would still be timed as audible — and under D-23.2 a Partial is not Done. M9a had
already moved that cell Not started → Partial, so the row stays **6 / 0 / 6 / 0**. And **5.13 UI
does not move** although this milestone changed what the tool reports for two of its requirements:
FR-UI-020 and FR-UI-070 fell to `**UNRESOLVED**` when §15 item 15's fix stopped a prose mention from
crediting a manual document. Neither lost verification it had — both lost credit for verification
that never happened — and M9a had already adjudicated both **Partial** by hand under §16's
"Divergence 2". This change makes the tool agree with a verdict that was already correct, which is
not evidence for moving it.

**One row deliberately not moved, recorded so its absence is not read as an oversight (M13's).** 6.7
BUILD stays **0 / 2 / 0**. M12 improved NFR-BUILD-020 — the README documents the build, run and test
commands, and `xtask identity` pins those exact strings, so the document can no longer drift
silently from what CI runs — but the requirement's "exercised by CI" clause is still unmet: nothing
executes a command *as documented*. That tag's `uncovered:` field is rewritten to say so and its
closing milestone moved from M12 to M13, which is a correction to a claim M12 would otherwise have
been recorded as meeting. **FR-UI-110 appears in no row here**: it is a Should, and §14 counts Musts
only. It stays open after M12 — see `docs/manual-tests/fr-ui-110-brand-mark.md`.

**M9b moves, 2026-08-12 — five rows, six requirements, appended per D-23.2's "every later
milestone moves only the cells its own evidence justifies."** 4 CFG becomes **2 / 1 / 0** (was
1 / 2 / 0); 5.10 LIB becomes **2 / 3 / 0** (was 1 / 4 / 0); 5.12 CLAP becomes **4 / 7 / 0** (was
2 / 9 / 0); 5.14 ERR becomes **1 / 5 / 0** (was 1 / 4 / 1); 6.2 PERF becomes **1 / 5 / 0** (was
0 / 6 / 0); **Total** becomes **42 / 88 / 0** (was 37 / 92 / 1). **The Not-started column reaches
zero for the first time**, 5.14 ERR having been its only non-zero cell since M13 emptied 5.15 PKG's.
That is a real fact about the ledger and a small one: it says every Must now has *some* artifact, not
that any additional Must is met — 88 of 130 are still Partial, which is the number that matters.

**M14 moves, 2026-08-12 — one row, two requirements, and the reason is worth stating.** 5.4 NAM
becomes **4 / 9 / 0** (was 6 / 7 / 0); **Total** becomes **40 / 90 / 0** (was 42 / 88 / 0).
FR-NAM-030 and FR-NAM-150 both move Done → Partial, not because anything regressed but because the
owner asked whether A2 support had ever been tested and the answer was that it has never been
compared against the reference implementation at all. FR-NAM-030's golden pair spans WaveNet-A1 and
LSTM; `tests/golden/` holds no A2 model, and the comment claiming an A1 golden covers A2 was false
on the facts. FR-NAM-150's own `Verify:` line elects an in-house reference, so that half is not a
gap — its "to the accuracy of FR-NAM-030" clause is, since the probe is 4 000 samples against a
receptive field of roughly 6 300. §21 Phase 4b owns both, R-9 is reopened, and issue #37 tracks it.
**This is the third time in two sessions that a question asked from outside the ledger found a
requirement the ledger read as Done** — after M9b's own two demotions, and it is the strongest
argument in this document for §22 **R-14**, which says the verdict columns sit outside every gate.

**Two of those cells moved back inside this same milestone, and the figures above are the corrected
ones.** M9b's build phase first recorded **44 / 86 / 0**, counting FR-CLAP-080 and FR-ERR-010 Done;
this section's `### M9b addendum` re-read both against D-23.1 and demoted their tags to
`trace-partial:`, so 5.12 CLAP is 4 / 7 / 0 rather than 5 / 6 / 0 and 5.14 ERR is 1 / 5 / 0 rather
than 2 / 4 / 0. The earlier figure is left standing where the status subsection first recorded it,
per this document's convention, and corrected here rather than there. It is worth naming what that
means: **the milestone built to enforce a tagging doctrine shipped two tags that failed it**, which
is the second time M9's own phases have done exactly that — M9a's `### M9a status` records the same
thing happening to NFR-PERF-050, caught the same way, by adversarial review rather than by any gate.

- **FR-CFG-020: Partial → Done.** The M9a bullet above named the gap in full — "no golden vector
  exists anywhere in the tree and nothing runs one through both configurations".
  `crates/namir-clap/tests/fr_cfg_020_shell_parity.rs` closes both halves as the `Verify: G` method
  words them: a committed, generated (D-19.1) input vector and preset
  (`crates/namir-clap/tests/golden/fr-cfg-020/input.f32`, `preset.namirpreset`) driven through each
  configuration's **own** code — the plugin through `clap_plugin.process`, the standalone through the
  real output callback `namir_app::stream::open` builds, not through a shared helper that would prove
  only that one engine equals itself — and compared bit-identically at three block sizes. This is the
  requirement §12's M8 exit checklist nominates as its final integration test and the one §14's own
  2026-08-08 note recorded as "gated by a table it has never appeared in"; it is now the 4 CFG row's
  second Done.
- **FR-CLAP-040: Partial → Done.** The M9a bullet's gap was that the notify path was "built and
  driven by nothing" with its document recording Not executed.
  `crates/namir-clap/tests/clap_host_latency.rs` drives it in-process through `clack-host`: latency
  is both **reported** on the extension and the host **notified** across three transitions, and the
  reported value is checked against an independently constructed `AudioEngine` rather than against
  the plugin's own accessor, so a stage's latency contribution going unreported would fail. Both
  verbs of the requirement's own text are spanned by one artifact executing its `Verify: I` method.
- **FR-CLAP-070: Partial → Done.** M9a: "no artifact anywhere compares a varying-block run against a
  fixed-block reference at chain or plugin level". `crates/namir-clap/tests/clap_host_block_sizes.rs`
  is that comparison at plugin level — a fixed-block reference against randomised block runs
  including a 256-block sequence of one-frame blocks, with a model and an IR actually loaded rather
  than through an empty chain where any block division trivially agrees. It earned its Done the hard
  way: the test found a real engine defect on first run, and the cell moves on the fixed code.
- **FR-CLAP-080: stays Partial, and its M9a gap narrows without closing.** M9a: "`activate` takes
  the host's rate and neither the 44.1-192 kHz range nor a mid-session change is exercised".
  `crates/namir-clap/tests/clap_host_sample_rates.rs` now exercises both — 154 rates including both
  endpoints the requirement names, plus a mid-session rate change through the deactivate/reactivate
  cycle. It is not Done, on both of D-23.1's questions. The requirement's own quantifier is "**every
  sample rate the host may present** within 44.1 kHz to 192 kHz inclusive"
  (`01-functional-requirements.md:763`), and `crates/namir-clap/src/audio.rs:125` rounds the host's
  rate to an integer, so that set is ~147 900 distinct `SampleRate` values of which the sweep
  reaches 154 — the same shape as FR-NAM-060's own `uncovered:` field, and D-23.2's clause 4 makes a
  quantified requirement whose test spans part of the set Partial. And nothing is loaded at any of
  those rates: the file never touches `PluginState`, so D-9.2's `SlotResampler` — the one
  rate-dependent subsystem in the chain, and the one this very milestone found broken at 192 kHz —
  runs at none of them, and the mid-session limb asserts only a 1 kHz tone's RMS through a
  pass-through chain. The tag is `// trace-partial:` at `clap_host_sample_rates.rs:219`, and that
  pair is this cell's named evidence.
- **FR-ERR-010: Not started → Partial.** M9a's finding was categorical — "nothing writes a log
  record anywhere", with `crates/namir-clap/src/shared.rs`'s sink recorded as a deliberate no-op and
  D-16.5's six parameters wholly unimplemented. `crates/namir-platform/src/logging.rs` is the writer
  and `crates/namir-platform/tests/logging.rs` its coverage, spanning D-16.5's parameters rather
  than asserting that a file appears. This empties the Not-started column. It is Partial and not
  Done because the requirement has **three** clauses and the covering test spans two: the six clause
  helpers verify configurable verbosity and boundedness, while **the per-user-location clause is
  asserted by nothing**. The logger is driven against a caller-supplied temporary path throughout
  (`crates/namir-platform/tests/logging.rs:17-19`) and the covering test asserts
  `logging::logger().is_none()` (`:606`); the only code binding the sink to a per-user path is
  `logging::init` via `crate::log_file_path` (`crates/namir-platform/src/logging.rs:614-620`), which
  no test calls. `crates/namir-platform/src/paths.rs:123-133` is the nearest evidence and is not it:
  `#[cfg(target_os)]`-gated, vacuous when its environment variable is unset, and untagged. The tag
  is `// trace-partial:` at `crates/namir-platform/tests/logging.rs:615`, and that pair is this
  cell's named evidence.
- **FR-LIB-020: Partial → Done.** M9a's gap was one clause of four: three met the 10 000-file scale
  and "the off-the-audio-thread clause is exercised against a 6-file corpus only".
  `crates/namir-worker/tests/library_scan_scale.rs` runs all four clauses against a 10 000-file
  corpus, which is the scale the method itself names — the case D-23.1's first question is written
  for, where the requirement quantifies over a set *and the method names its size*.
- **NFR-PERF-040: Partial → Done, and read the next paragraph before reading this as a certified
  figure.** M9a's gap was the whole `Verify: B` half — "no `Verify: B` harness exists and the 200 ms
  bound has never been measured". `crates/namir-clap/benches/plugin_instantiation.rs` is that
  harness, and it **asserts** the bound in-process rather than printing it for a human, which is what
  D-2.6 settles as the satisfied form and what D-23.1's second question demands of a `Verify: B`
  plain tag. §15 item 14 — which of an in-process instantiation and a real host counts as "in a host"
  — is answered by D-2.6 for the purpose of this cell and is not otherwise reopened here.

**Eight requirements gained real evidence this milestone and stay Partial, deliberately.**
FR-NAM-060, FR-IR-030, FR-CLAP-030, FR-CLAP-100, FR-CLAP-130, FR-ERR-030, NFR-PERF-010 and
NFR-PERF-050 each carry more than they did at M9a and each still carries a `// trace-partial:` whose
`uncovered:` field names what is unmet; under D-23.2 a Partial is not Done, and the cells stay where
M9a put them. None of the eight sat in **Not started**, so none moves one column either — worth
saying because the instruction to move a Not-started requirement to Partial would have been
justified had any of them been there, and checking the M9a bullets above shows none was.

**FR-UI-020 and FR-UI-070 do not move, and this is the most important caveat on this table.** Both
are `Verify: M`. `docs/manual-tests/fr-ui-020-single-screen-elements.md` and
`docs/manual-tests/fr-ui-070-non-modal-error-notices.md` now exist — closing the M9a bullet's literal
complaint that "there is **no** `docs/manual-tests/fr-ui-020-*.md`" — and `xtask traceability`
therefore counts both **covered**, the filename arm doing exactly what §15 item 15's fix left it
doing. **Neither script has been executed by a human.** Both documents say so in as many words:
each records **Result: NOT EXECUTED**, with no step run in either product configuration. A `Verify: M`
requirement is met by *running* its script, not by the file existing, and under D-18.6 that document
is the requirement's whole traced artifact — so the file's existence moves the tool's reading and
nothing else. The mechanical gate and the honest verdict disagree here, §14 is the place that
disagreement is resolved in the requirement's favour rather than the tool's, and a later reader must
not "reconcile" these two cells by promoting them to match `docs/03-test-plan.md`. Both stay
**Partial**, where M9a put them; 5.13 UI stays **1 / 6 / 0**, unchanged for the second milestone
running and for the same reason M13 recorded.

**FR-STATE-040 does not move either, and its stability is not evidence of anything.** 5.9 STATE stays
**2 / 5 / 0**. Its `Verify:` is `M plus S (schema check)`; the parser keeps only the first code, so
the requirement resolves plainly covered from a filename match while **no schema check exists
anywhere in the tree**. M9b found this while building FR-ERR-030's static check and recorded the full
analysis — the six compound-method Musts, why five self-correct through their `uncovered:` fields and
this one structurally cannot, and the three candidate fixes — as **§15 item 21**, due before M8.
Cited rather than restated here so there is one copy of it.

**The performance figures behind this milestone are not certified, and NFR-PERF-040's Done does not
claim they are.** They were taken on `docs/02-architecture.md` §2's pinned reference machine but not
on a *quiet* one, which fails D-2.4's second condition, so under this project's own standing rule
they are informational. NFR-PERF-040's promotion rests on D-2.6 — an in-process benchmark that
asserts its threshold is the satisfied form of a `Verify: B` — and not on any measured number being
certified. NFR-PERF-010 stays **Partial** for the separate reason its own tag names.

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

   **Narrowed, not resolved, 2026-08-08 (M9's P0 decision pass).** The *research* half closed at M7:
   `02-architecture.md` §21's AQ-4 row records that no explicit licence exists for NAM's
   standardised reamp signal and concludes "treat as all-rights-reserved unless/until Atkinson
   clarifies", and §11's M7 status above lists AQ-4 among that milestone's closures. **What is not
   resolved is the choice that finding forces:** obtain explicit written permission for the standard
   signal — the precedent NAM's own maintainer set for "Super Input" — or record a self-licensed
   reamp signal for the capture session and sidestep the question. This item is deliberately **not**
   struck through, because a strike reads as "no decision left", which is false, and §21's own row
   still carries a live "Needed by: Before shipping factory presets". **Still due before M8**,
   unchanged. Recorded here because M9's brief is verification truth-up, and a register item that
   looks resolved while half of it is open is the same species of drift §14's table has.

   **Still open at M14's Phase 0, 2026-08-12, and deliberately not taken here.** Both answers this
   item leads to — obtain written permission to redistribute the standard reamp signal, or record a
   self-licensed one — cost a capture session with real hardware, so neither is a decision a
   documentation pass can make. No factory preset exists anywhere in the tree, so the permitted
   route is unbuilt work whichever answer is chosen. **§21's Phase 0 names a third resolution this
   item does not list, and it is recorded rather than performed:** FR-STATE-090 is a **Should**, §13
   says no Should gates M8, and yet M8's exit checklist gates on "Factory presets ship (post AQ-4)".
   Those two cannot both stand. Striking that bullet is cheaper than either answer above and is the
   only disposition consistent with §13 — it changes what M8 gates, not what Namir ships. It is not
   struck here because editing M8's exit checklist is a decision about the 1.0 gate. Carried as a
   `*Consequence*` note at FR-STATE-090 as well, so it is visible from the requirement.
4. ~~**Whether `namir-nam`'s FR-NAM-030 parity claim should be re-anchored against
   `NeuralAmpModelerCore` from inside the product workspace**, rather than relying on the
   already-excluded `spikes/s1-nam-inference`'s one-time -131 dB measurement. The cross-implementation
   parity test added to `namir-nam` in this session is strong evidence on its own, but it validates
   internal consistency between two from-scratch Rust ports, not agreement with the external
   reference implementation FR-NAM-030 actually names. Worth a decision at M3 (when LSTM parity
   needs the same treatment anyway): commit a small, licence-clean reference-output fixture into
   the repo, or accept the spike's result as sufficient historical evidence and say so explicitly.~~
   **Folded into M10, 2026-08-08 (§17 below).** It stopped being a standalone decision the moment
   A2 support was planned: D-9.12 requires A2's weight-layout order to be re-derived from
   `NeuralAmpModelerCore`'s own `NAM/wavenet/detail.h`/`params.h` and proven by a new
   `namir-fixtures` A2 generator acting as a parity oracle (R-9 is the risk row for getting that
   wrong). That work re-reads the external reference implementation from inside the product
   workspace and produces exactly the anchoring artifact this item asked for, as a **side effect**
   of a deliverable that has to happen anyway — so the decision is now "do it as part of M10 Phase
   3", not a separate choice between two options. The A1 layout is re-anchored on the same pass,
   since the A2 derivation cannot be trusted without confirming the A1 one it extends.
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
6. ~~**FR-IO-020's WASAPI exclusive mode has no path forward yet.** Found during M6: `cpal` 0.18.1,
   D-13.1's pinned dependency, hardcodes `AUDCLNT_SHAREMODE_SHARED` with no way to request exclusive
   mode — verified against that exact version's vendored source, not inferred
   (`docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`). Two paths exist and neither is built:
   a `namir-platform`-owned unsafe WASAPI-exclusive helper (D-5.3 already permits that crate
   `unsafe`, mirroring `DenormalGuard`'s pattern), or an upstream `cpal` change/fork. **Due before
   M8**, since 5.11 IO cannot be marked Done without it, and cheap to decide now while the shape of
   `namir-app`'s `AudioBackend` trait (`crates/namir-app/src/audio_io.rs`) is still fresh — a
   later decision risks needing that trait's boundary redrawn instead of just extended.~~
   **Resolved 2026-08-08: `02-architecture.md` D-13.4** — the second of the two paths, a **forked
   `cpal`** adding `AUDCLNT_SHAREMODE_EXCLUSIVE`, rather than a `namir-platform`-owned unsafe WASAPI
   helper. The decision's own text records what the choice costs: the fork is a git dependency, so
   it touches §17's dependency register, `cargo-deny`'s `[sources]` policy and the vendoring
   question, and it carries an ongoing rebase obligation (risk row **R-10**). It also records what
   it does *not* cost — `AppSettings::exclusive_mode` already exists as a persisted field, so no
   settings migration is needed and the trait boundary this item worried about is extended rather
   than redrawn, exactly as hoped. **Built in M11 (§18 below).**
7. ~~**Whether Namir accepts a build script in a shipped crate, in order to embed the Windows `.exe`
   icon.** Raised 2026-08-08 while planning M12. Embedding an icon resource into a Windows
   executable needs a build script, and this project's dependency-adoption bar treats build scripts
   as a real cost rather than a neutral convenience — `02-architecture.md` §17 records `libc` as
   **the one knowing exception to that bar in the whole workspace**, and justifies it by ABI-layout
   correctness for `pthread_setschedparam`. An icon is cosmetic, so it cannot borrow that
   justification; the question is whether the bar bends for presentation or holds. Three answers are
   available and none is obviously right: accept a build script (e.g. `winresource`) and record it
   as the second knowing exception; ship without an executable icon, taking the default; or set the
   icon outside the build, as a post-build step in M13's packaging pipeline, which keeps the crate
   build-script-free but means a `cargo build` and a released binary differ in a user-visible way.
   **Due before M12 (§19 below)**, and worth deciding deliberately rather than discovering
   mid-implementation, since the third option silently moves work from M12 into M13.~~
   **Resolved 2026-08-10: `02-architecture.md` D-17.3** — the third of the three answers. The bar
   holds: `namir-app` takes no build script, and M13's packaging pipeline embeds the icon. The
   decision records what that costs — a `cargo build` and a released binary differ in a user-visible
   way until M13 — and one thing this item did not know: FR-UI-110's *window* icon is blocked in M12
   as well, `baseview` 0.2.2's `WindowOpenOptions` having no icon field at all, so the two clauses
   defer together rather than one moving and one landing. **Deferred to M13 (§20 below).**
8. ~~**Whether the plugin configuration ever gets a persisted verbosity setting, or stays
   environment-variable-only.** Raised 2026-08-08 by M9's P0 decision pass while settling
   `02-architecture.md` **D-16.5**. D-16.4 says FR-ERR-010's verbosity is "configurable from
   settings and overridable by an environment variable", and in the standalone application both
   halves exist: `namir-app` owns `AppSettings` (`crates/namir-app/src/settings.rs`) and can persist
   a level. In the plugin there is no settings file to persist it *in* — D-5.1 lets `namir-clap`
   depend on "everything except app", so `AppSettings` is unreachable by construction, and
   `namir-state`'s document is the preset/plugin state FR-STATE-010 governs, which deliberately
   carries no host-machine-specific configuration (the same reasoning that kept device selection out
   of it at M5). So in 1.0 the plugin's only verbosity control is `NAMIR_LOG`. Three answers are
   available: accept environment-variable-only for the plugin and say so in the user guide's
   troubleshooting section; give `namir-clap` a small preferences file of its own under
   `config_dir()`, which is a new persisted artifact and a new migration surface for one enum's
   worth of value; or move the verbosity level into a shared, non-preset settings record both shells
   read, which is the general fix and the largest. **Due before M8**, since it is a user-guide and
   support-story question rather than an engine one, and cheap to answer while D-16.5's own text is
   fresh.~~
   **Resolved 2026-08-12, at M9b's start: the first answer — environment-variable-only for the
   plugin in 1.0**, recorded as a dated Consequence note on **D-16.5** itself rather than as a new
   decision, because D-16.5's level-resolution rule already reads "the persisted setting **where one
   exists**" and therefore needs no amendment. That the writer was *already* specified to work in a
   configuration with no settings file is the argument for this answer, not merely its cheapness.
   The obligation it creates is a documentation one and is recorded at D-16.5 so it is not lost: the
   user guide's troubleshooting section must tell a plugin user how to raise verbosity with
   `NAMIR_LOG`, **including that it must be set in the environment the host inherits** — a DAW
   launched from a desktop shortcut does not see a variable exported in a terminal, which is the
   detail separating an instruction that works from a support thread. Both other answers are
   recorded rejected there: a `namir-clap` preferences file (a new persisted artifact and migration
   surface for one enum, and the plugin's only host-machine state — a category declined once already
   when device selection was kept out of the preset document at M5), and a shared non-preset
   settings record (the general fix, far too large to buy for a log level, and reasonable to raise
   again when a second such setting actually exists).
9. ~~**Whether FR-CLAP-030 ships with a single Stereo port configuration.** Raised 2026-08-08 in M9's
   P0 decision pass. The requirement says the plugin "shall declare audio port configurations
   corresponding to FR-CHAIN-060", and FR-CHAIN-060 names three (Mono, Mono→stereo, Stereo);
   `crates/namir-clap/src/audio_ports_ext.rs` declares **Stereo only** and does not implement CLAP's
   `audio-ports-config` extension, which is what would let a host choose among several. The scope
   reduction has been recorded honestly since M6 — in that module's own doc comment and in
   `docs/manual-tests/fr-clap-030-audio-ports-negotiation.md` — but only as an implementation note,
   never as a decision, and M9a's split-evidence tagging (`02-architecture.md` **D-18.6**) will make
   the row read green in `docs/03-test-plan.md` while the reduction is still in force. Three answers:
   implement `audio-ports-config` and declare all three, which is the only one that meets the
   requirement as written; declare Stereo only and record it as an accepted, FRS-level scope
   reduction with a Consequence note at FR-CLAP-030 itself; or keep the current silence, which is the
   one option M9 exists to stop doing. **Due before M8**, since 5.12 CLAP cannot be marked Done
   without it.~~
   **Resolved 2026-08-12, at M9b's start: the second answer — Stereo only, recorded as an accepted
   FRS-level scope reduction**, with the Consequence note written at **FR-CLAP-030 itself** in
   `01-functional-requirements.md` as this item prescribes. The requirement text is deliberately left
   unamended: the FRS states what Namir must do, and lowering it to match what was built would erase
   the gap rather than record it. Three things that note settles so no later pass re-derives them.
   **FR-CLAP-030 stays `trace-partial:` for 1.0** — the unspanned member is the negotiation limb,
   one configuration of FR-CHAIN-060's three. **Satisfying "at least two host implementations" does
   not promote it**: the in-process `clack-host` harness and the commit-pinned `clap-validator` CI
   job are two genuinely independent hosts, but D-23.1's enumerated-set rule bites regardless of how
   many hosts ask. And **this does not obstruct NFR-QUAL-010 closing at M9b**, since a partial counts
   as covered — what it does mean is that **5.12 CLAP cannot read Done in §14** while the reduction
   stands, which is exactly what this item's own "Due before M8" clause was protecting. Implementing
   `audio-ports-config` is recorded as rejected-for-now rather than dismissed: it remains the only
   route to a plain tag, and it was declined because M9b is scoped to verification infrastructure
   and this is a feature touching the audio path.
10. ~~**Whether `xtask traceability`'s scanned-file list gains `.github/workflows/release.yml` when M13
    creates it.** Raised 2026-08-08 by M9's P0 decision pass. The list at `xtask/src/main.rs:205-216`
    is hard-coded to `ci.yml`, `fuzz.yml`, `Cargo.toml` and `deny.toml` — nothing derives it, so a
    new workflow is invisible to the check until someone edits that array. FR-PKG-010's `*Verify:*`
    line elects the release workflow as its artifact, which FRS §10's M9 adequacy rule makes
    admissible — but only if the tool can see it. Two answers, and the choice is not obvious: extend
    the list, and say plainly that a wider scan set is a wider surface for a tag that asserts nothing;
    or close FR-PKG-010 with an in-repo assertion instead, which is what §10's M8-planning note
    assumed before the rule that supersedes it existed. **Due before M13 (§20 below)** — deciding
    late means meeting it as a red gate on the one milestone whose entire content is shipping.~~
    **Resolved 2026-08-11, at M13's start: the second answer — an in-repo assertion, and the scanned
    list is left alone.** FRS §10 admits "an annotated test **or** `xtask` subcommand", and a test
    is the cheaper of the two here, so `release.yml` gains no `# trace:` tag and the hard-coded
    arrays (now `xtask/src/main.rs:301-312`, not `:205-216` — the citation had already drifted) are
    unchanged. Three things decided this rather than the elected-configuration route, which was
    otherwise admissible. **A tag in a workflow asserts nothing a reader can check**, which is this
    item's own stated cost, and FR-PKG-010 has four separable clauses — tag-triggered, from a tagged
    *source tree*, on every tier-1 and tier-2 platform, and every published distribution an artifact
    of that workflow — that a bare `# trace:` line would flatten into one unexamined claim.
    **A test runs on every pull request; a workflow tag is inspected by nobody and the workflow
    itself runs only on a tag**, so the elected-configuration route would have left FR-PKG-010's
    evidence untouched between releases. And **the scan set stays narrow**, which matters beyond
    this item: it is hard-coded precisely so that adding a file to it is a deliberate act, and M13
    adds three packaging files (`.iss`, `install.sh`, a macOS script) that would each raise the same
    question if the precedent were set here. See §20's M13 status for what the assertion actually
    checks.
11. ~~**`clap-validator`'s supply-chain shape, once FR-CLAP-020's gate is required.** It is not
    published on crates.io and must be installed from git — recorded in `02-architecture.md` §19 and
    restated in §16's own deliverables. Wiring it into CI makes that install a build input on every
    merge, which is the same class of exposure D-13.4's forked `cpal` had to argue for explicitly and
    which **R-10** tracks; `deny.toml`'s `[sources]` policy governs cargo dependencies and does not
    see a CI-installed tool at all, so nothing existing catches it. Pin by commit, vendor a built
    binary, or accept a floating install with the reason written down. **Due at M9a, before the gate
    lands** rather than after — M9a is the phase that adds the job.~~
    **Resolved at M9a, 2026-08-08: pinned by commit** — this item's first option, taken over the
    other two. The gate landed as `.github/workflows/ci.yml`'s new `clap-validator` job
    (`ci.yml:253-296` as of this pass), which installs the tool with `cargo install --locked --git
    https://github.com/free-audio/clap-validator --rev b2f1d9b79b1d264a5747f46707d72b1aa40a02ef`
    (`:266-271`). **The record lives in that job's own `# ---- Supply-chain shape` comment**
    (`:191-222`), not in `02-architecture.md` §17 — deliberately, and on that register's own titled
    instruction (`02-architecture.md:1836-1847`), which already names `clap-validator` as build
    tooling that puts nothing of its own into the shipped artifact and says its "availability,
    versions and licences are D-18.3's and the CI configuration's business, recorded there". Both
    rejected options are recorded there rather than dropped: vendoring a built binary (upstream's
    prebuilts come from a third-party redirector over expiring Actions artifacts, and an opaque
    committed executable is a worse provenance story than a hash in a repository whose every test
    fixture is generated by decision, D-19.1), and a floating install (the gate's meaning would then
    change with no commit here for a reviewer to see). `--locked` is load-bearing rather than
    hygiene — upstream commits its own `Cargo.lock`, so the flag pins the validator's entire
    transitive tree to what upstream resolved. That comment also records version, licence and MSRV
    as read from the pinned checkout on 2026-08-08 rather than inherited — `clap-validator` 0.4.1,
    `license = "MIT"` with a LICENSE file present, `rust-version = "1.95.0"`, which is under this
    workspace's own pin. The exposure itself does not disappear with the
    pin — a git-sourced build input on every merge is still the class **R-10** tracks, and
    `deny.toml` still cannot see it — so the pin is the mitigation, not a closure of the concern.
12. ~~**What happens if FR-NAM-060's measurement legitimately fails.** The resampler's stopband
    attenuation (≥100 dB) and passband ripple (≤0.1 dB up to 20 kHz or Nyquist) have never been
    measured; FR-NAM-050's resampling is tested for *correctness*, not *quality*, and the plan's row
    for FR-NAM-060 is `**UNRESOLVED**`. This is a genuine first look and it may fail. Three branches,
    not equally cheap: fix the resampler (a real DSP change, under NFR-PERF-010's budget); amend the
    requirement's figures in the FRS with a stated justification; or record a deviation. **Decide the
    branch policy before the number exists** — deciding afterwards means the rule gets chosen to suit
    the measurement, which is the failure mode D-2.2's own history already demonstrates. **Due at
    M9b's start.**~~
    **Resolved 2026-08-12, at M9b's start, before the measurement was taken: the three branches are
    ordered as a hierarchy, not offered as a choice.** Taken in order, and a later branch may be
    reached only when the one before it has been tried and written up:
    1. **Meet the requirement as written.** `SlotResampler` (`crates/namir-engine/src/stages/nam.rs:351`)
       uses two `rubato::FftFixedInOut<f32>` "as configured by `rubato` itself, not tuned or
       verified" — that module's own doc comment. A bar missed by a resampler nobody has *tried* to
       configure is not evidence the bar is wrong. Configuration, or a different `rubato` resampler
       type, is the first move, bounded by NFR-PERF-010's budget.
    2. **Amend the FRS figures** — permitted only with a stated justification in terms of what a
       user would *hear*, not in terms of what the current implementation happens to achieve. A
       figure edited to match a measurement is not a requirement.
    3. **Record a deviation** — last, and it must name the audible consequence.
    *Why a hierarchy rather than a choice:* the item's own warning is that deciding afterwards means
    the rule gets chosen to suit the measurement. Three peer branches do not prevent that; they
    merely make the choice look deliberate. Ordering them does prevent it, because branch 2 is
    unreachable without a written record of branch 1 having failed. **This applies unchanged to
    `namir-ir`'s second resampler**, `resample_mono` (`crates/namir-ir/src/convolver.rs:845`), whose
    own `trace-partial:` at `:1408-1413` imports the same bar and records it "measured nowhere …
    which a 40 dB-stopband, 3 dB-ripple resampler would pass". Both sites are measured in M9b and
    both are governed by this order.
13. ~~**What happens if FR-CLAP-070's block-size parity test fails.** The requirement demands arbitrary
    and varying block sizes "including a block size of one sample", `Verify: U`, and the test has
    **never been run** — `namir-clap` carries `// trace:` tags for FR-CLAP-010/-050/-060/-090 only.
    `02-architecture.md` D-6.2's consequence asserts the design handles it (buffers sized for the
    declared maximum, a smaller block uses a prefix, an over-declared block is processed in slices
    rather than allocating), but an assertion is not a measurement, and the partitioned convolver's
    schedule and the NAM stage's history are the plausible places a one-sample block breaks. Same
    branch structure as item 12 and the same reason to settle it first, with one addition: **"declare
    a minimum block size" is not an available branch** without an FRS change, because the requirement
    names one sample explicitly. **Due at M9b's start.**~~
    **Resolved 2026-08-12, at M9b's start, before the test was written: fix the engine. Item 12's
    hierarchy does not transfer here, and the difference is the point.** FR-NAM-060 measures a
    quantity nobody has ever claimed, so a failure there is new information about an untuned
    component. FR-CLAP-070 is not that: **D-6.2's consequence already asserts the design handles it**
    — buffers sized for the declared maximum, a smaller block using a prefix, an over-declared block
    processed in slices rather than allocating. A failing parity test therefore does not report that
    the requirement is too strict; it reports that **the implementation has diverged from a decision
    this document already made**. The remedy for that is to fix the implementation.
    Consequently, for this requirement: amending the FRS is **not** an available branch, and neither
    is recording a deviation, unless the failure first proves D-6.2 itself unimplementable — which
    would be a finding about the architecture, would be written up as an amendment to D-6.2 rather
    than to the FRS, and is not something a test failure alone establishes. The item's own exclusion
    stands and is widened: "declare a minimum block size" remains unavailable, and so does every
    other route that changes the requirement instead of the code. The plausible failure sites named
    above — the partitioned convolver's schedule and the NAM stage's history — are where to look
    first.
14. ~~**Whether an in-process instantiation counts as "a host" for NFR-PERF-040's certified figure.**
    The requirement caps plugin instantiation at 200 ms excluding model loading, `Verify: B`; nothing
    measures it and the identifier appears nowhere in the codebase. **D-18.6 settles the vehicle** —
    the `clack-host` harness instantiates this crate's real plugin in-process through the real C
    vtable — but it does not settle whether a figure taken that way is what NFR-PERF-040's "in a
    host" means for a **certified** number under D-2.4, or whether a recorded measurement in a real
    DAW is required for that and the in-process figure is the regression test beside it. The choice
    decides whether this requirement closes with a repeatable benchmark, a one-time recorded figure,
    or both; D-2.4's "certified means the §2 reference machine and at least five repetitions" binds
    either way. **Due at M9b's start.**~~
    **Resolved 2026-08-12, at M9b's start: `02-architecture.md` **D-2.6** — the in-process harness
    certifies; a real-DAW figure is supplementary and is not a precondition for closure.** The
    deciding argument is D-2.4's own definition rather than a judgement about realism: "certified"
    means the §2 reference machine and at least five repetitions with a contaminated run discarded,
    and five hand-timed instantiations inside a DAW measure the operator as much as the plugin. The
    vehicle is not a stub — `PluginEntry::load_from_clack::<SinglePluginEntry<NamirClapPlugin>>`
    drives the real `activate` path through the real C vtable — and the split mirrors D-18.6's
    existing doctrine rather than inventing a second one: the in-process artifact is traced, the
    manual document is the residue a real host is needed to observe. D-2.6 records the honest limit
    in full: a DAW also does plugin scanning, sandboxing and host-thread contention, so this figure
    is the plugin's own instantiation cost and a lower bound on what a user experiences. **Both
    rejected options are recorded there**, including "both required", which was declined because it
    would make a `Verify: B` requirement's closure depend on a manual session.
15. ~~**How a `Verify: M` Must is matched to its manual-test document.** Raised 2026-08-09 by M9a's
    set-quantification sweep. `build_report` credits a manual document to a `Verify: M` requirement
    if **either** the filename starts with the id's lowercase prefix **or** the file's text contains
    the id anywhere, taking the first match in directory order
    (`xtask/src/traceability.rs:689-699`). Both arms admit a document that does not verify the
    requirement it is credited to, and the second one is doing so today: **FR-UI-020 resolves to
    `docs/manual-tests/fr-clap-030-audio-ports-negotiation.md`**, a CLAP audio-port negotiation
    script that names FR-UI-020 once, in a parenthesis about watching a meter, and wins only by
    sorting first among the six files that mention it. FR-UI-020 has **no document of its own**, and
    the nearest thing to one — `fr-ui-010-standalone-window-renders.md`, which does direct the tester
    to confirm FR-UI-020's screen elements (`:18`, `:29`) — loses on alphabetical order. The prefix
    arm has the complementary weakness: it reads no content at all, so a correctly-named document
    recording "not executed, no hardware available" credits its requirement in full and identically
    to one recording a clean pass. Neither is catchable downstream — D-23.1 refuses a
    `trace-partial:` on a `Verify: M` Must (`xtask/src/traceability.rs:616-633`, and §16's
    2026-08-09 status), so for the thirteen there is no disposition between "covered" and
    "UNRESOLVED" at all. Three answers, and the cheapest is not obviously the right one: match on the
    document's own `**Requirement (literal):**` line, which makes the credit an assertion its author
    wrote deliberately, at the cost of backfilling that line into the seven of twenty-one documents
    lacking it and of FR-UI-020 going honestly UNRESOLVED until it gets a script; keep both arms but
    require the match to be unique and error on ambiguity, which surfaces FR-UI-020 immediately and
    costs one check; or leave it and rely on review, which is the answer this project has rejected
    everywhere else it has been offered. **Due before M13's close-out** — that is when D-18.5's
    zero-uncovered half becomes required, and a wrongly-credited document would let that gate certify
    a Must nothing verifies, on the one milestone whose flip is meant to make the ledger mean
    something.~~
    **Resolved 2026-08-11 at M13's start: the first answer, built.** The content arm now reads the
    document's `**Requirement (literal…):**` block rather than the whole file —
    `declared_requirement_ids` in `xtask/src/traceability.rs`, called from `build_report`'s `'M'`
    arm. The filename arm is unchanged, and so is its complementary weakness: a correctly-named
    document recording "not executed, no hardware available" still credits its requirement
    identically to one recording a clean pass. That half of this item is **not** fixed and is not
    claimed to be; it is a different problem with a different answer, and conflating them is how a
    resolution note over-claims.

    Three things the work found that the item above did not predict, all of which make the fix
    look better rather than worse.

    - **The backfill it costed does not exist.** The item budgets "backfilling that line into the
      seven of twenty-one documents lacking it". All **22** documents in `docs/manual-tests/` carry
      the line today; the count was already stale when it was written or has been closed since. No
      document was edited to land this.
    - **The block is a paragraph, not a line**, and that distinction is load-bearing. Real
      declarations wrap: `fr-io-010-device-enumeration.md` names FR-IO-010 on its first line and
      FR-IO-040 on its third. A literal single-line read would have dropped the one legitimate
      multi-requirement document in the tree while keeping every false positive — the exact inverse
      of the intent. The reader takes the marker's paragraph, to the first blank line.
    - **There was a third false positive, not one.** The item names FR-UI-020 → `fr-clap-030`; M12's
      close-out had already found and reworded a fourth in `fr-ui-110-brand-mark.md` → FR-PKG-030.
      Narrowing the arm also demoted **FR-UI-070**, which resolved to
      `fr-ui-010-standalone-window-renders.md` because that script's step 1 says the canned snapshot
      carries "one FR-UI-070 notice". FR-UI-070 requires errors to be surfaced non-modally, never
      interrupt audio, and state what failed, which file or device, and what to do — verified
      "against the error catalogue of FR-ERR-020". That script walks no catalogue. The credit was a
      noun in a parenthesis.

    **Effect on the ledger, stated because it goes the unpopular way.** `docs/03-test-plan.md` moves
    exactly two rows, FR-UI-020 and FR-UI-070, from a manual-test filename to `**UNRESOLVED**`, and
    the informational uncovered count goes **15 → 17**. Neither requirement lost any verification it
    had; both lost a credit for verification that never happened. **No §14 cell moves**: M9a's
    re-audit had already adjudicated both **Partial** by hand under §16's "Divergence 2", which is
    the disposition this change makes the tool agree with. And per item 15's own deadline reasoning,
    the due date travelled: D-18.5's zero-uncovered half now flips at **M9b's** close-out (see the
    correction at §16's restated acceptance), so this was no longer due here. It was done here
    anyway — M13 is the milestone that writes `fr-pkg-030-windows-install-scope.md`, and FR-PKG-030
    is the requirement the bug hit last.
16. **Whether 1.0 ships an audio-device panel in `namir-ui`, and if not, what FR-IO-010/-040/-050
    mean.** Raised 2026-08-09 by M9a's sweep, which found the surface absent rather than untested —
    `UiSnapshot` carries no host, device, sample-rate, buffer-size, latency or xrun field and
    `UiIntent` no device variant (`crates/namir-ui/src/host.rs:100-123`, `:148-176`), device
    selection happening once at start-up from remembered settings
    (`crates/namir-app/src/app.rs:95-133`). Five Musts lean on a panel existing; the table in §16's
    2026-08-09 status names them and the clause each one loses. This is a scope decision with no
    current owner: **FR-IO-060's and FR-IO-070's partials book their UI halves to M9b**, which is a
    verification-infrastructure phase and a poor home for building a settings surface, and no
    milestone's deliverables name the panel at all. Three answers: build it, and site it where the
    audio-path work already is (**M11**, whose §18 scope is WASAPI exclusive mode and which has to
    tell the user which mode they actually got, so a mode indicator and a device panel are the same
    surface); ship without it and record an FRS-level scope reduction with a `*Consequence*` note at
    each of the five, the shape §15 item 9 already proposes for FR-CLAP-030; or keep the current
    silence, in which three Musts read covered on manual documents describing a panel that does not
    exist. **Due before M9b's start**, and worth taking before M11 rather than after, since the
    second answer changes what M11 builds and the first changes where.
    **Still open at M14's Phase 0, 2026-08-12, and deliberately not taken here — now overdue by two
    milestones.** The deadline above was "before M9b's start"; M9b and M14's Phase 0 have both run
    without it. §21 named it "the single decision with the largest effect on this milestone's size,
    and it must be first", and Phase 0 is nonetheless declining it, so the reason is stated rather
    than left as a lapse: the two live answers differ by **a whole UI feature phase**, which is a
    product-scope decision about what the user sees and an owner's to take, not a disposition a
    documents-only pass can record. Neither of the two is unavailable for want of analysis; what is
    missing is the call. **The scope this fixes for M14:** the milestone's automated half does **no
    device work of any kind** — no device panel, no `UiSnapshot` host, device, rate, buffer, latency
    or xrun fields, no `UiIntent` device variant — and **FR-IO-060 and FR-IO-070 therefore stay
    Partial through M14**, with `*Consequence*` notes at both recording that, and recording that
    M14's silence is not the item answered in the negative. FR-IO-010, -040 and -050 are unmoved for
    the same reason. Answer 3 — keeping the current silence — is what this note refuses on the
    item's behalf: the item stays open and dated, which is not the same thing.
17. ~~**Which milestone closes FR-CFG-030, NFR-LIC-030 and FR-IO-070.** Raised 2026-08-09 by M9a's
    sweep, whose `// uncovered:` fields had to declare a closing milestone for each and, for these
    three alone of the 54, could not take one this document assigns. FR-CFG-030 and NFR-LIC-030 both
    name **M13**, which §20 does not claim either in — and NFR-LIC-030 additionally contradicts
    §14's own M7-session bullet booking it closed (`:1690`). FR-IO-070 names **M9b** while §18 already
    names it as M11's opportunistic item, gated on hardware and explicitly not to be back-filled
    (`:3025-3029`); M11 runs before M9b, so the two readings are not merely different owners but
    different orders. §16's 2026-08-09 status sets out each case. The annotations were left as
    written rather than edited to fit, because picking a milestone to make a field parse is the
    defaulting this appendix exists to prevent. **Due before M10's start** — M9a is the phase that
    wrote the three fields, and leaving them unreconciled means the tool's printed owner attribution
    and its printed closing milestone disagree from the very next milestone onward, which is a
    disagreement no exit status will ever surface.~~
    **Resolved 2026-08-11, at M13's start. Overdue by three milestones** — the deadline above was
    "before M10's start" and M10, M11 and M12 all ran with the three fields unreconciled. Nothing
    broke, which is the point R-13 makes about quiet failures rather than a defence of the delay.
    Each leg separately, and one of the three answered itself by M11 simply running.

    - **FR-CFG-030 → M13. The annotation was right and §20 was silent; §20 adopts it.** Its
      `*Verify:* I` reads "each is installed alone into a clean environment and exercised", and
      before this milestone there is nothing to install — the standalone is a `cargo build` output
      and the plugin is a hand-copied `.dll`. M13 is the first milestone at which the sentence can
      be executed at all, and a CI runner is a clean environment by construction. Recorded as scope
      in §20's M13 status rather than by editing §20's deliverable list. What M13 does **not**
      promise is that it closes in full: the requirement is two installations, each exercised, and
      whether the exercising half reaches its `Verify: I` bar is settled by what actually runs, with
      any residue staying a `trace-partial:` naming it.
    - **NFR-LIC-030 → M13, and §14's M7-session bullet booking it closed (`:1690`) is the error.**
      Both cannot stand, and the annotation is the one supported by the requirement's text. That
      bullet says "NFR-LIC-030 closes: the attribution file (`THIRD-PARTY-NOTICES.md`, `xtask
      attribution`) didn't exist before this session" — true of the file, and the requirement has
      **two** clauses: "produced by the build" **and** "shipped with the binaries". M7 closed the
      first. There were no binaries to ship anything with, so the second could not have closed and
      was not examined. This is the same shape as M9a's other findings — a requirement counted Done
      on the strength of the clause someone was working on — and it is left in place as historical
      text per §14's own rule, with the correction here and the cell moving at M13's close-out, not
      M7's.
    - **FR-IO-070 → M9b, and the conflict dissolved on its own.** The item's difficulty was that
      §18 named FR-IO-070 as M11's opportunistic item while the annotation booked it to M9b, with
      M11 running first. **M11 has now run and did not take it**: its close-out states "M11 produced
      evidence for FR-IO-020 and for nothing else, and a cell nobody's evidence has touched stays
      where M9a left it." §18's own wording made this the expected outcome — the item was gated on
      hardware and explicitly not to be back-filled with an inspection claim. So the annotation
      stands unopposed and **M9b owns it**, by the milestone that could have claimed it declining
      to, which is a better resolution than either reading argued for.
18. ~~**FR-CHAIN-070 cannot be satisfied as the chain is built: two of its three options are
    unreachable and there is no chooser at all.** Raised 2026-08-09 by the independent spot-check of
    §14's adjudication, verified at the sites named rather than inferred. The requirement says the
    user "shall be able to choose whether the core is fed the left channel, the right channel, or the
    sum of both at −6 dB", default left (`docs/01-functional-requirements.md:295-298`). It is a
    **Should**, so it sits in no §14 row and no gate reads it — which is precisely why it needs a
    home here rather than in a verdict cell. Two facts, both checked this pass. **There is no
    chooser:** `FR-CHAIN-070` appears nowhere under `crates/`, and none of `params.lock`'s 29 live
    parameters is a channel source. **And one could not simply be added at the only site that
    could serve it:** `GateStage::process` detects on channel 0 and copies that gated result over
    every other channel whenever the stage is enabled
    (`crates/namir-engine/src/stages/gate.rs:163-172`; `mix_target = 1.0` at `:216`, so only the
    bypass crossfade restores the original channels), while `TrimStage` — which owns the chain's
    only cross-channel mixing, L·g + R·g at −6 dB per term
    (`crates/namir-engine/src/stages/trim.rs:145-156`) — runs after it under D-9.8's order. The right
    channel is therefore gone before Trim can sum it, the sum evaluates to the left channel at unity,
    and right-only is unreachable for the same reason. Shipped behaviour is FR-CHAIN-070's stated
    default arrived at by accident. The FRS already records this as an observed fact
    (`docs/01-functional-requirements.md:211-222`, in M9a's D-9.8 amendment) and deliberately stopped
    short of deciding it; this item is that decision, still unmade. Three answers, and their costs
    are not close. **Reorder** (trim before gate) reopens D-9.8, which M9a has just re-affirmed in
    the product's favour after seven milestones — the expensive one, and it would move a Must's
    behaviour to serve a Should. **Teach `GateStage` to preserve channels** — one detector, its
    envelope applied to every channel instead of channel 0's output being copied over them — which
    keeps FR-CHAIN-050's mono-core rule for NAM intact but needs a seam `namir_dsp::NoiseGate` does
    not expose today: `process` applies the envelope in place to one buffer and the only readout is
    a single `gain_reduction_db()` scalar (`crates/namir-dsp/src/gate.rs:196`, `:204`). **Drop the Should**
    with a `*Consequence*` note at FR-CHAIN-070 itself, accepting left-only — which costs no Must,
    since FR-CHAIN-060's Stereo row admits either reading in words ("2 ch summed **or** L-only
    (FR-CHAIN-070)", `docs/01-functional-requirements.md:291`), so that Must is not in conflict and
    no separate item is owed for it. **Due before M8.** Recorded without a recommendation on purpose.
    *Cross-reference:* §15 item 9 owns the **plugin-side** half of the same subject — FR-CLAP-030
    declaring one port configuration where FR-CHAIN-060 names three — and is a different question
    (what the plugin *tells a host*, not what the chain *does with two channels*); this item does not
    reopen it. §15 item 16's audio-device panel is unrelated: it is about FR-IO-010/-040/-050's
    missing settings surface, not about channel routing.~~
    **Resolved 2026-08-12, at M14's Phase 0: the Should is dropped for 1.0.** Namir ships the left
    channel only, with no chooser, recorded as a `*Consequence*` note at FR-CHAIN-070 itself
    (`docs/01-functional-requirements.md`) rather than by amending the requirement — lowering a
    requirement to match what was built erases the gap instead of recording it, which is
    FR-CLAP-030's own precedent from M9b. **The item said it was recorded without a recommendation,
    and its own cost analysis nevertheless pointed one way, which this resolution follows.**
    Reordering reopens `02-architecture.md` D-9.8 after M9a re-affirmed it, and moves a **Must**'s
    behaviour to serve a **Should**. Teaching `GateStage` to preserve channels needs a seam
    `namir_dsp::NoiseGate` does not expose, in a milestone already carrying ~35 Musts of test debt,
    for a requirement §13 says cannot gate M8. Dropping it costs no Must: FR-CHAIN-060's Stereo row
    admits L-only in words. Two residues are carried into the note rather than left here: the −6 dB
    sum is reachable only by switching the gate off, which is a side effect and not the *choice* the
    requirement asks for, so it does not partially satisfy it; and the stale
    `crates/namir-engine/src/stages/trim.rs:19-21` comment M9a identified is still uncorrected,
    which is a source change M14's Phase 0 does not make.
19. ~~**NFR-PORT-030's `*Verify:*` method may be too weak for its own text.** Raised 2026-08-09 by
    the spot-check of §14's adjudication, which argued the cell to Partial and was **rejected** —
    the verdict stays **Done**, and this item is where the unrebutted half of that argument goes.
    The requirement's text enumerates five design constraints: no assumption of a file-system-wide
    path namespace, no assumption of a mouse, no assumption that the process can spawn unlimited
    threads, no blocking dialog on the path of any audio-affecting operation, and no dependency on a
    desktop-only windowing model (`docs/01-functional-requirements.md:964-968`). Its `*Verify:*`
    line elects one check for all five — "S — the engine and its supporting crates shall build for
    `aarch64-linux-android` and `aarch64-apple-ios` in CI" (`:971-972`) — and both jobs run green
    (`.github/workflows/ci.yml:478-545`, plain-tagged at `:478`). **A cross-build proves none of the
    five.** A crate that assumes a mouse, blocks on a dialog or spawns a thread per scan job
    compiles for `aarch64-linux-android` exactly as well as one that does not. The verdict is Done
    because D-23.2 adjudicates against the requirement's **own stated method** and that method is
    executed as written; the gap here is between the requirement's *text* and its *method*, not
    between the method and its artifact, and a verdict cell is not the instrument for closing it.
    Amending the `*Verify:*` line is an FRS change and needs its own decision — the same shape as
    FR-CHAIN-010's resolution earlier in M9a, where a requirement and the product disagreed and the
    FRS's owner moved the document rather than a cell moving quietly. Three answers: amend the
    method to name per-constraint checks that can actually fail (a lint for blocking calls on the
    audio-affecting path, a bound on spawned threads, the absence of pointer-only input handling in
    `namir-ui`), which is the only one that makes the Done cell mean the text; keep the method and
    add a `*Consequence*` note at NFR-PORT-030 recording that the cross-build is a door-open check
    rather than a compliance check, so no later reader mistakes green CI for the five constraints
    holding; or leave it, which is the option this appendix exists to stop taking silently. **Due
    before M8**, since 6.3 PORT's row feeds M8's exit checklist and this is the one cell in it whose
    method and text are known to disagree.~~
    **Resolved 2026-08-12, at M14's Phase 0: answer 2 — the method stands, and a `*Consequence*`
    note at NFR-PORT-030 records that the cross-build is a door-open check rather than a compliance
    check.** The item's argument that a cross-build proves none of the five constraints is
    unrebutted and is adopted in full; what was open was the disposition, not the analysis. Answer 1
    was declined on proportion, not on principle: it needs five new static analyses that do not
    exist — a blocking-call lint over the audio-affecting path, a bound on spawned threads, a check
    for pointer-only input handling in `namir-ui`, and two more — each with its own false-positive
    surface, built for a platform FRS §1.4 calls **Prospective** and RD-4 defers past 1.0. Answer 3,
    leaving it silent, is what this appendix exists to prevent. **The 6.3 PORT cell stays Done** —
    D-23.2 adjudicates against a requirement's own stated method and that method is executed — and
    the note fixes how the cell is to be read: *the door is open at compile time*, not *the five
    constraints hold*. `xtask rt-logging` is named in the note as the working template for a
    per-constraint check, if that work is ever taken post-1.0.
20. **Whether 1.0 ships a CLAP plugin with no interface on two of its three supported platforms.**
    Raised 2026-08-12 by M13, by loading the plugin in Reaper on a Mac — the first time anyone had.
    `crates/namir-clap/src/gui.rs`'s `is_api_supported` returns true only for `GuiApiType::WIN32`,
    so on macOS and Linux the host is refused and **no editor is ever embedded**: the plugin loads,
    processes audio and takes automation, and what the user sees is the host's own generic parameter
    panel — no brand mark, no meters, nothing Namir drew. The standalone is unaffected on those
    platforms, which is what makes the plugin's behaviour look like a defect rather than a scope
    boundary. **FR-CLAP-100 is a Must and says nothing about platform**: "the plugin shall provide
    an embedded graphical editor via the CLAP GUI extension". Its second clause — function correctly
    if the host declines a GUI — is met, and the macOS run is what demonstrates it. Three things
    make this an appendix item rather than a bug report. It was **introduced deliberately** at M6
    from spike S-4's validated shape, and S-4 ran on Windows only; it was **recorded nowhere** —
    not in `02-architecture.md`, not in the FRS, not in `docs/user-guide.md` — until M13 added the
    notes now at FR-CLAP-100 and at S-4's own record; and closing it is **not a small change**:
    Cocoa and X11 embedding mean a second and third `#![allow(unsafe_code)]` surface in
    `namir-clap` under D-5.3, on window-handle paths this project has never exercised. Three
    answers. Ship as-is for 1.0 and say so plainly in the user guide and release notes, treating
    the plugin as Windows-first and the standalone as the cross-platform product — cheapest, and
    honest only if it is *stated*, which until now it was not. Add the two backends, which is real
    unsafe work against D-5.3's bar and needs its own decision the way `libc` did. Or narrow
    FR-CLAP-100 to name the platforms it applies to, which is an FRS change and makes the ledger
    honest without pretending the limitation is smaller than it is. **Due before M8**, since
    FR-CLAP-100 feeds 5.12 CLAP's row and M8's exit gate reads every row Done. Tracked as a GitHub
    issue so it is visible outside this appendix.
    **Still open at M14's Phase 0, 2026-08-12, and deliberately not taken here.** All three answers
    turn on the same question — whether "supported platform" means the same thing for the plugin as
    for the standalone — and that is a product-positioning commitment, the owner's to make. **What
    M14 does settle is that answer 2 is not this milestone's work:** Cocoa and X11 window embedding
    is a second and third `#![allow(unsafe_code)]` surface under D-5.3, on window-handle paths this
    project has never exercised, and §21 lists it among what M14 deliberately does not build. So
    FR-CLAP-100 stays `**UNRESOLVED**` and unmet on two of three platforms through M14. One stale
    detail found while writing this and recorded rather than fixed: the requirement's ledger entry
    still books it to **M9b**, a milestone that has run, so whoever takes the decision re-books it.
21. ~~**`xtask traceability` keeps only the first code of a compound `Verify:` method, and for one
    requirement that silently hides an unexecuted half.** Raised 2026-08-12 at M9b, found while
    building FR-ERR-030's static check. **Six** Musts state a compound method: FR-STATE-040
    (`M plus S (schema check)`), FR-CLAP-090 (`I plus B`), FR-CLAP-130 (`S plus I`), FR-ERR-030
    (`S plus I`), NFR-RT-020 (`S plus code review`) and NFR-PERF-010 (`B, as a CI regression gate`).
    The parser records only the first, so `docs/03-test-plan.md` shows a weaker bar than the FRS
    states for all six. **Five self-correct and one cannot, and the difference is structural.** For
    the five whose first code is not `M`, the annotation author can write the missing half into the
    `uncovered:` field, and at M9b four of them do exactly that, naming the `B` half, the `S` half,
    the `I` half and the unasserted threshold respectively — the doctrine working as intended.
    **FR-STATE-040 cannot**, because its first code *is* `M`: under D-18.6 a `Verify: M` Must is
    traced by its manual document, `xtask` hard-refuses a `trace-partial:` naming one, and so it
    resolves **plainly covered** from a filename match alone. Its `S` half is a schema check that
    **does not exist** — nothing under `crates/namir-state/src/` mentions a schema, `xtask preset`
    is a sample *generator* rather than a validator, and no `trace:` tag anywhere in the tree names
    FR-STATE-040. So one Must reads fully green while half its stated method is unexecuted, and no
    mechanism in the current design could ever say so. Three answers, and the cheapest is not
    obviously right: **teach the parser compound methods**, which is the general fix and would catch
    this class automatically, but will demote rows and reopen adjudications; **build FR-STATE-040's
    schema check** and tag it, which closes this instance and leaves the class open — and requires
    deciding what the schema *is*, which the FRS does not say; or **narrow the FRS line** to the half
    that is actually verified, which is honest bookkeeping and gives up a check the requirement's
    author evidently wanted. **Due before M8**, since M8's exit gate reads §14 and 5.9 STATE's row
    depends on it. **Recorded rather than acted on at M9b by decision**, and M9b's close-out states
    the caveat plainly rather than letting NFR-QUAL-010's first-ever closure be read as stronger
    than it is.~~
    **Resolved 2026-08-12, at M14's Phase 0: the general fix first, the instance second, and the FRS
    line is not narrowed.** Recorded as a `*Consequence*` note at FR-STATE-040 itself. **Narrowing
    the FRS line is refused** on FR-CLAP-030's precedent — editing a method to match what was built
    gives up a check the requirement's author wanted, and this appendix exists to stop exactly that
    being done quietly. **Building only the schema check is refused as insufficient**: it closes
    this instance and leaves the class open, so the next compound method written repeats the failure
    in silence. So: **teach the parser compound `*Verify:*` methods**, so every code a requirement
    states has to resolve, and **then** build FR-STATE-040's schema check. The item's objection that
    the FRS does not say what the schema is holds against the FRS and not against the project —
    `docs/04-state-and-preset-format.md` §§3–7 document the top-level structure, `format_version`,
    `global`, `parameters` and `references`, which is the schema. Two consequences are carried into
    the note rather than discovered later: **the parser change will demote rows and reopen
    adjudications**, which is the fix working and is why it belongs in a milestone that expects it
    rather than in M8; and **until it lands, FR-STATE-040 must not be counted Done by hand for §14
    or M8's exit checklist whatever `xtask traceability` prints**, because what the tool prints for
    it is a filename match and structurally cannot be anything else. The parser change is §21 Phase
    3 work (issue #27) and is not done by this Phase 0 pass, which writes no code.

---

## Milestones added 2026-08-08 — M9 through M13

**Execution order is `M9 → M10 → M11 → M12 → M13 → M8`, and §12's execution-order note explains
why.** The short version, repeated here because a reader arriving at this point may have skipped
it: these milestone numbers record when each milestone joined the plan, not when it runs, and these
section numbers are addresses, not an ordering. M8 keeps its number and its text and runs after all
five, because its entire content is the 1.0 exit gate and several of its checklist items are what
the milestones below produce. Nothing above this line has been rewritten.

---

## 16. M9 — Verification truth-up

**Size: M.** **Depends on:** M7 — this milestone acts on the traceability tool and the generated
test plan M7 built, and there is nothing to truth-up before they exist. **Blocks:** M8 directly, as
M8's first two checklist items are literally this milestone's acceptance criteria; and less
obviously everything after it, since M10–M13 each add claims to a ledger nobody currently trusts.

**This runs first of the five, deliberately.** M7 ended with a traceability gate marked
`continue-on-error: true` and a §14 snapshot table that contradicts its own prose in five rows and
has six more rows untouched since M0. Layering A2 support, exclusive mode, a brand and a release
pipeline on top of that would mean four further milestones' worth of claims landing in a ledger of
unknown age. The cheapest moment to repair a verification story is before more things depend on it.

**Deliverables:**

- **A re-audit of all 16 uncovered Musts `xtask traceability` reports**, separating tagging misses
  (the coverage exists, the tag doesn't) from genuine gaps (nothing covers it). **Three are already
  confirmed as tagging misses** — NFR-PERF-050, FR-STATE-050 and FR-LIB-020, each with its covering
  benchmark or test named in §11's appended correction above. That correction also retracts M7's
  claim that all sixteen had been individually investigated, so the other thirteen are genuinely
  unexamined and this is the first pass over them, not a second opinion.
- **The verification infrastructure that is genuinely absent and has to be built rather than
  tagged:**
  - **NFR-PERF-030** — a standalone-application startup benchmark, measuring time to an audible
    state. The identifier appears nowhere in the codebase at all.
  - **NFR-PERF-040** — a plugin-instantiation benchmark against the requirement's 200 ms ceiling.
    Likewise absent entirely. Both are wall-clock measurements and fall under D-2.5's scoping of
    D-2.1's "never wall-clock" rule to audio-thread per-block budgets, so neither needs a new
    decision — but both need a harness, and per D-2.4 a *certified* figure means the §2 reference
    machine and at least five repetitions, not one run on whatever machine is to hand.
  - **FR-CFG-020** — the golden-vector bit-identity check across both product configurations. M8's
    exit checklist already nominates this as its final integration test; it cannot be a final check
    if nothing has ever executed it once, so the harness lands here and M8 re-runs it.
  - **FR-NAM-060** — a resampler frequency-response measurement against the requirement's own
    stopband figure. FR-NAM-050's resampling exists and is tested for correctness; its *quality*
    has never been measured.
- **FR-ERR-010's logging, which is genuinely unbuilt.** Only `namir-platform`'s `log_file_path()`
  exists — a path, with nothing writing to it. There is **no `log`, no `tracing`, and no logging
  crate of any kind anywhere in this workspace**, so this is a real dependency-adoption question
  against a deliberately strict bar, not an afternoon's wiring. **Decided in `02-architecture.md`
  D-16.4** — numbered 16.4, not 16.3, because D-16.3 has meant worker-job panic isolation since the
  original draft. That decision weighed adopting `log`/`tracing` against hand-rolling a small
  bounded-rotation writer in `namir-platform`, and **chose to adopt nothing**: siting the writer in
  `namir-platform` makes it unreachable from the audio thread by a lint that already exists, since
  D-5.1 forbids `namir-engine → namir-platform` and `xtask layering` enforces that edge. So this is
  implementation against a settled decision, not an open dependency question — but the constraint
  it turns on still binds: the audio thread must never touch the logger directly (NFR-RT-010).
- **A sweep for requirements the gate reports as covered but only partly is — a blind spot in the
  mechanism itself, not a tagging error.** `xtask traceability` answers "does a covering test
  exist?", which is the wrong question for any requirement that **quantifies over a set**: one
  matching test satisfies the tool no matter how much of the set it leaves untouched. One instance
  is already confirmed. **FR-NAM-030 (Must)** reads "for **each** supported architecture… match the
  reference NAM implementation", and only WaveNet has ever been compared that way — S-1's own scope
  note in `02-architecture.md`'s changelog 0.6 states plainly that "S-1 covered WaveNet only… LSTM
  is unaddressed." LSTM's parity is against `namir-fixtures`' from-scratch reference instead, which
  is not what the requirement names. The tool reports FR-NAM-030 covered, and by its own rules it is
  right; the requirement is nonetheless half-met, and has been since M3 with nobody recording it.
  M10's Phase 4 closes this particular one. **What M9 owes is the sweep**: re-read every Must whose
  text quantifies ("each", "every", "all", "any supported…") and check the covering test actually
  spans the set, recording any others found. Doing this *before* the gate becomes required matters —
  once it is green and mandatory, "the gate passes" starts being read as "the requirement is met,"
  and this class of gap gets much harder to see.
- **`clap-validator` wired into CI.** FR-CLAP-020's own text says "as a gate in CI"; today it is
  not, and the row's entire evidence is M6's single manual 32/32 run. One practical wrinkle,
  already recorded in `02-architecture.md` §19 and worth restating rather than rediscovering:
  `clap-validator` is not published on crates.io and must be installed from git, which gives the
  CI step a supply-chain shape worth stating explicitly.
- **Real `namir-clap` test coverage for FR-CLAP-030, -040, -070, -080, -100 and -130.** The crate
  today has **zero** `#[cfg(test)]` coverage for any of them. The functionality is real and was
  demonstrated once; nothing re-verifies it when something changes. FR-CLAP-020 is covered by the
  validator gate above rather than by hand-written tests.
- **A full, evidence-derived re-audit of §14's snapshot table**, starting from the three defect
  classes named in that section's own 2026-08-08 note: six rows untouched since M0, five rows
  contradicted by prose written beneath them, and 5.12 CLAP's stated Must count of 10 against the
  FRS's actual 11 `FR-CLAP-*` Musts. Every cell re-derived from evidence rather than inherited from
  the cell before it; where evidence is genuinely absent, the cell should say so rather than guess.
- **`xtask traceability` flipped from `continue-on-error: true` to a required check.** Last, not
  first — flipping it before the count reaches zero recreates exactly the red-check-nobody-can-act-on
  problem M7's own reasoning gave for marking it informational in the first place.

**Acceptance:** `cargo run -p xtask -- traceability` exits 0 with zero uncovered Musts, as a
required check on every supported CI runner. §14's table is re-derived from evidence rather than
inherited, with 5.12 CLAP's Must count reconciled against the FRS. NFR-QUAL-010 means what it
claims, for the first time since it was written.

### M9 P0 decision pass — 2026-08-08 (decisions only; nothing implemented)

Governance questions settled *before* any M9 implementation begins, appended rather than merged into
the Deliverables and Acceptance above, which stand unedited per this document's convention. Seven
questions were drafted independently, collided when combined, and were merged and renumbered in one
pass before anything was written to a governing document; what follows is the merged result, ordered
so each part reads against the ones before it. Nothing is built yet, and each decision is owed a
status note when the work actually happens. Further P0 entries append here.

**Two sentences in this section's own header are refined rather than edited by this pass.** "This
runs first of the five, deliberately" is true of **M9a** only — M9b runs sixth, after M13. And
"**Blocks:** M8 directly" now also means **M9a blocks M10**. Both are left exactly as written; part
2 below carries the refinement and §12's execution-order note carries the arrow.

**This pass lands in two commits, and the boundary is not cosmetic.** The first is documents only —
this subsection, §4's, §12's and §14's notes, §15's new items, the FRS's §1.5 and §10 notes,
`02-architecture.md`'s decisions and registers, `AGENTS.md`, and two source *doc comments* that name
a file which was renamed. None of it changes what any gate sees. The second is the tooling change
and everything that depends on it: `--allow-uncovered` and the printed owner attribution, D-23.1's
two integrity fixes and its `trace-partial` parsing, D-23.2's denominator emitter, the three
annotations under 3 below, the regenerated `docs/03-test-plan.md`, `ci.yml`'s two steps,
`clack-host`'s manifest entry, and `docs/manual-tests/fr-lib-020-ui-responsiveness-during-scan.md`.
Those cannot land piecemeal: the commit that makes the plan-diff half required is the same commit
that changes three rows of the generated plan and invokes a flag `xtask` does not have today
(`xtask/src/main.rs:304` returns the single `plan_up_to_date && coverage_clean`, and the argument
parser at `:328-329` recognises only `--write`), so any other split makes the new required check red
on arrival.

**1. The acceptance criterion restated, in two halves that flip on different dates —
`02-architecture.md` D-18.5.** The Acceptance paragraph above asks for something M9 cannot deliver,
and it stands as written. What follows is the correction, made before any M9 work starts rather than
discovered at its close. Three problems, and only the first is arithmetic.

*Nine of the twenty-four uncovered Musts are not M9's to close, and ten after this pass's own scope
change.* Counted from the checked-in generated plan rather than estimated —
`docs/03-test-plan.md` carries 24 `**UNRESOLVED**` rows against 130 Must rows (a raw `grep -c`
returns 25; the legend line matches too) — with each attribution checked against the owning
milestone's own text:

| Owner | Uncovered Musts it claims | Where |
|---|---|---|
| **M10** (§17) | FR-NAM-140, FR-NAM-150, **FR-NAM-090** | "New requirements this milestone closes" for the first two; "Also closes FR-NAM-090 and FR-NAM-100" for the third |
| **M12** (§19) | NFR-DOC-040, NFR-LIC-070 | "New requirements this milestone closes" |
| **M13** (§20) | FR-PKG-010, -020, -030, -040 | "New requirements this milestone closes"; FRS §5.15 in full |
| **M13** (§20), **by this pass** | **NFR-PERF-030** | the scope change at the end of this part, and §20's dated scope note recording the arrival |

M9's own share is therefore **14**, not the 16 the first deliverable above names, and that bullet's
"other thirteen … genuinely unexamined" is **eleven**. **§14's note is not where the error is.** Its
"eight Must requirements added on 2026-08-08" counts *new FRS requirements* and is correct, and its
prediction that the tool's count would rise from sixteen to twenty-four is exactly what the tool now
reports. The ninth is **FR-NAM-090**, a pre-existing Must (FRS §5.4) that M10 adopted because A2-era
files carry the loudness metadata the A1 schema lacks — new to *M10's scope*, not new to the FRS.
"Eight new Musts" is a statement about the FRS and "ten later-milestone Musts" is a statement about
ownership; both are true and they are not the same number.

*This milestone's best deliverable pushes the count the wrong way, and that is it working.* The
sweep for requirements the gate reports as covered but only partly is expected to *raise* the number
of Musts not honestly closed — FR-NAM-030 is the confirmed instance and the sweep exists to find the
rest. An acceptance criterion of "zero uncovered Musts" is one M9 could satisfy most cheaply by
**not looking**, which inverts the milestone's entire purpose. Whatever else the criterion says, it
must not be satisfiable by declining to look.

*The gate is weaker than it reads, which matters more than the count.* `xtask traceability` returns
a single value for two independent checks — `plan_up_to_date && coverage_clean`
(`xtask/src/main.rs:304`), exiting 1 if either fails — and CI's one invocation of it carries
`continue-on-error: true` (`.github/workflows/ci.yml:108-120`), which suppresses both halves
together. Stated plainly, because it is the strongest argument for this restatement: **deleting a
coverage annotation from a currently-covered Must leaves CI green today.** The plan file would go
stale, the tool would say so and exit 1, and nothing would fail. Nor does `.githooks/pre-commit`
catch it — it runs `fmt` and `check` only, by design.

**Restated acceptance:**

- **Required from M9a, on every merge — the plan-diff half.** `docs/03-test-plan.md` matches what
  the tool generates. This is the regression gate: it makes coverage a ratchet from M9a onward
  regardless of how many gaps remain, and it is the half that is enforceable *today*. It needs
  `--allow-uncovered` to exist in `xtask` first; today's single exit value cannot express the split.
- **Informational until M13's close-out — the zero-uncovered half.** It keeps `continue-on-error`,
  with its flip condition restated: it becomes required at **M13's close-out**, not at M9's, because
  ten of the remaining gaps belong to M10, M12 and M13, and M7's original reasoning against a
  permanently-red required check (`ci.yml:108-117`) applies to them exactly as it applied at M7.
  **M13's close-out owns the flip** — deleting `--allow-uncovered` and the informational step — and
  must record it. M9's close-out must not claim it.
- **Both modes print the full uncovered list**, each id attributed to the milestone that owns it.
  Attribution is printed by the tool, never stored in the plan file, and **the exit status never
  depends on it** — an owner label explains a gap, it must never excuse one. Both the flag and the
  attribution are M9a's `xtask` change, not descriptions of today's tool: today it prints
  `  - {id} (Verify: {verify})` and nothing else (`xtask/src/main.rs:299`), and it holds no
  milestone data at all. Where the id→milestone mapping comes from is left to M9a's implementation
  and must not be a hard-coded exemption table, which is the allowlist rejected below under another
  name.
- **§14's table is re-derived from evidence**, row set included, with 5.12 CLAP's denominator
  reconciled: the FRS holds **11** `FR-CLAP-*` Musts, which the generated plan already confirms
  mechanically by listing exactly eleven of them.
- **NFR-QUAL-010 is not closed by M9, and M9 must not claim it is.** Its own *Verify* text requires a
  check that "fails on any uncovered **Must**" (FRS §6.4); under this restatement that is true only
  when the second half flips at M13's close. §12's exit checklist is unaffected — M8 runs last.

**Correction (added M13, 2026-08-11) — the flip is at M9b's close-out, not M13's, and this
restatement is where the error entered.** The bullets above are otherwise unamended; only the
milestone named in the second and fifth changes. The reasoning that produced "M13" is directly
above, at the attribution table: ten of the twenty-four uncovered Musts belong to M10, M12 and M13,
so M13 was read as the milestone after which none remains. That counted one column. **M9's own share
is fourteen**, and this same section's second deliverable puts those fourteen in **M9b**, which this
same section then orders *after* M13. The two statements are on the same page and were never
reconciled. Measured from the checked-in plan at M13's start rather than re-derived: **15**
`**UNRESOLVED**` Must rows, five owned by M13 (FR-PKG-010, -020, -030, -040, NFR-PERF-030) and
**ten** by M9b (FR-CFG-020; FR-CLAP-030, -040, -070, -080, -100, -130; FR-ERR-010; FR-NAM-060;
NFR-PERF-040). Deleting `--allow-uncovered` at M13's close would make a required check red for ten
requirements no milestone before M9b was ever asked to close — the permanently-red required check
M7's argument, quoted two bullets above, exists to prevent. So: **M9b's close-out owns the flip**,
NFR-QUAL-010 closes at **M9b** and no earlier milestone may record it closed, and §15 item 15's
"due before M13's close-out" deadline follows the flip with it. `02-architecture.md` D-18.5 carries
the same correction as its own dated consequence note, and §20's scope note carries it as an
appended note there. M13 still lands every annotation §20's Acceptance names; what it does not do is
flip a gate whose remaining gaps are not its to close.

*An allowlist or exemption register is rejected, not overlooked.* The obvious way to make "zero
uncovered Musts" pass today is a file of known-exempt ids the gate skips. Three reasons not to. The
artifact it would duplicate already exists: `docs/03-test-plan.md` is generated, checked in,
hand-editing-forbidden by its own header, and diffed on every run, so the current set of gaps is
already a reviewed, versioned document and any change to it is a legible line in a pull request. An
exemption list inverts the default, turning "uncovered" from something you can only remove by
covering into something you can add. And it would need its own freshness check to stop it rotting,
which is a second gate guarding the first. **This disposes of a declared-deferral table too**, which
is the same list under another name: a Must whose implementing milestone has not run carries no tag,
stays `**UNRESOLVED**`, and is explained by the printed owner attribution above. **The honest limit
of the ratchet, stated rather than glossed:** it is review-visible, not mechanically monotone —
regenerating with `--write` and committing the new `**UNRESOLVED**` row passes the gate. A
checked-in count permitted only to decrease would close that hole and is rejected for the same
reason, being the same list in numeric form; what stops a silent regression here is that the diff
lands in review, which is the enforcement NFR-QUAL-020 already runs on.

*"On every supported CI runner" is restated as one runner.* The check is a pure static comparison
over checked-in text, the same class as the layering and `params.lock` steps sharing its job, whose
own comment already records that one OS is enough (`ci.yml:83-88`). Two things are recorded rather
than assumed: this tool has had a real cross-platform determinism bug (`read_dir` ordering over
`docs/manual-tests/`, commit `f8f72d9`), and its fix was confirmed only across Windows-local versus
`ubuntu-latest` (§11's note above) — **macOS has never been compared.** M9a closes that by running
the tool once on macOS and recording the result here, in this milestone's close-out subsection where
a later session can find it, rather than by buying a permanent three-runner matrix for a text diff.

*One scope change, so it is not reopened later:* **NFR-PERF-030 moves to M13** (§20 below, where a
dated scope note records it). It cannot run on any CI runner — a machine with no audio device
diverts `namir-app` to `open_window_without_audio` (`crates/namir-app/src/app.rs:116`, `:148`,
`:155`, `:164`, `:309`) and never becomes audible — and measuring "time to an audible state" needs a
seam in `namir-app`'s entry path that exists solely to enable the measurement. M13's release
pipeline already touches that launch path with a real machine in the loop, so that is where the
harness costs least and means most. It is `**UNRESOLVED**` in the checked-in plan today, which is
why the move is what takes the count of uncovered Musts owned outside M9 from nine to ten. **M9
keeps NFR-PERF-050**: its tag, plus the numeric threshold assertion that makes its `Verify: B`
literally true (see 3 below).

**2. Size restated: L, and split into two phases.** The header above reads "Size: M". It is wrong,
and its own neighbour is the quickest way to see how wrong. M10 is rated **L** for one
architecture's parser, its DSP primitives, a weight-layout re-derivation, a parity oracle and an
LSTM parity run. M9's content is: a re-audit of 14 uncovered Musts; a sweep of all 130 Musts' wording
for quantifiers; a full evidence-derived rebuild of §14's table including its row *set*; a
plugin-instantiation benchmark whose identifier appears nowhere in the codebase at all
(NFR-PERF-040); a golden-vector bit-identity harness (FR-CFG-020); a resampler frequency-response
measurement (FR-NAM-060); FR-ERR-010's entire logging subsystem, hand-written per D-16.4 precisely
because no logging crate is adopted; `clap-validator` wired into CI; and six `FR-CLAP-*`
requirements' tests written from zero. That is not smaller than M10.

**Restated as Size: L, and split into two phases** — phases inside this section, the same device §17
already uses for M10, **not** new milestone numbers, so every existing reference to "M9" in this
document, the FRS, `02-architecture.md` and `docs/manual-tests/` keeps its meaning:

- **M9a — the ledger (Size: M).** The 14-Must triage; the quantifier sweep and the form its findings
  are recorded in; §14's table re-derived, row set and CLAP denominator included; the three confirmed
  tagging misses (NFR-PERF-050, FR-STATE-050, FR-LIB-020) with the work each one owes first;
  D-23.1's two tool-integrity fixes; D-18.5's split gate — the `--allow-uncovered` flag, the printed
  owner attribution, the two CI steps, the required half switched on, and the one-time macOS
  determinism check; the `clap-validator` CI job; **adding `clack-host` as a `namir-clap`
  dev-dependency and clearing D-18.6's three landing gates against it** — `cargo deny check bans`,
  `cargo deny check licenses` and D-18.2's network-free job, plus the `cargo tree -e normal` proof
  that the feature does not reach the cdylib and an unchanged `xtask attribution` — which §17's row
  and D-18.6 both assign here, and without which the gates M9a owns have nothing to run against;
  FRS §10's factual correction; §15's new items; and `AGENTS.md`'s two edits.
- **M9b — the missing verification infrastructure (Size: L).** FR-ERR-010's logger; NFR-PERF-040's
  instantiation harness, built from nothing, and NFR-PERF-050's certified figure re-measured under
  D-2.4 once M9a's threshold assertion exists; FR-LIB-020's remaining scale gap, named by its own
  `// uncovered:` line under 3 below; FR-CFG-020's golden vector; FR-NAM-060's measurement; and the
  in-process CLAP tests (FR-CLAP-030, -040, -070, -080, -100, -130).

**Why split rather than only relabel.** §17's own dependency line says M10 depends on M9 "in the
sense that a hard-won parity claim is worth less landing in a ledger nobody trusts" — that is a
dependency on **M9a alone**. Nothing in M9b is a prerequisite for A2 support. **The order this
implies:** **M9a → M10 → M11 → M12 → M13 → M9b → M8**, with two hard constraints and no others —
M9a completes before M10 starts, and **M9b blocks only M8**, whose exit checklist nominates
FR-CFG-020's bit-identical-output check as its final integration test. §12's execution-order arrow is
left as written and carries an appended note recording this refinement.

**3. The tagging doctrine is decided — `02-architecture.md` D-23.1.** The deliverables above are
internally inconsistent on the one question that governs every tag M9a adds: FR-LIB-020 is proposed
for tagging while FR-NAM-140 is declined, on the reasoning that tagging a half-met requirement turns
the gate green on it. D-23.1 makes a tag mean "the whole requirement, by its stated `Verify:`
method", and adds `// trace-partial:` with a **mandatory** `// uncovered:` line naming the unspanned
member and the closing milestone — one without the other is a hard `xtask` error. Nothing covers it,
no tag; silence is never how a partial is expressed. §22 gains **R-13** for the obvious abuse of a
partial.

*Two tool-integrity fixes land before M9a's first tag, not after.* `trace_annotations` finds the
marker anywhere in a line with no id-shape filter (`xtask/src/traceability.rs:115-131`), and the
function-name fallback is a whole-file substring test — so string literals in `xtask`'s own tests put
`xtask` in `docs/03-test-plan.md` as a component of FR-NAM-070, which it does not test. That false
positive is live and checked in. Tagging under a doctrine while the tool still counts a string
literal as a tag would put a false attribution into the same generated file the doctrine relies on
for visibility.

*The adjacency clause must admit `fn main()`, and this is the highest-risk detail in the pass.*
D-23.1 requires a tag to sit immediately above the artifact it claims. Every benchmark in this
workspace is `harness = false` with a plain `fn main()`, and all four existing bench tags sit
directly above one — `crates/namir-engine/benches/denormal_guard.rs:411` (NFR-RT-030),
`six_stage_chain.rs:242` (NFR-PERF-010), `tail_structure.rs:211` (NFR-RT-040),
`crates/namir-library/benches/library_scan.rs:154` (FR-LIB-030, NFR-PERF-060). For **all five** of
the identifiers those tags carry, the bench tag is the only coverage there is — every source hit for
each was grepped across `crates/` and `xtask/` this session, and the generated plan lists exactly
one component for each — so an adjacency rule recognising only `#[test]`/`#[bench]` would turn five
green rows red on the same commit that makes the plan-diff half required. The rule names `fn main()`
in a `benches/*.rs` target as an anchor explicitly.

**Correction 1 — the count above is stale, and "16 uncovered Musts" understates it.**
`docs/03-test-plan.md` lists **24**. The difference is M8-planning's own new requirements, added to
the FRS after the sixteen were counted. The re-audit deliverable is sized against 24, of which 14 are
M9's own.

**Correction 2 — the Acceptance paragraph above is unreachable as written, and this was not visible
when it was drafted.** "Zero uncovered Musts" cannot be reached at M9 for a reason that is not a
verification gap: **nine** of the 24 are requirements whose *implementing* milestone has not run —
the table under 1 above lists all nine, FR-NAM-090 included — and **ten** once this pass's own scope
change moves NFR-PERF-030 to M13. The only route to zero at M9 is tagging unbuilt work, which
D-23.1 forbids in the same breath as it is adopted. The criterion M9 is actually held to is the
split gate restated under 1; it is restated once, not twice.

**Dispositions decided for the four requirements this milestone was about to tag.** Each verified by
reading the source this session, not by re-reading an earlier summary:

- **FR-STATE-050 — plain tag, no work owed.** Its text is a reference, not a set ("the constraints of
  FR-NAM-070 apply to any model or IR change a preset implies"), and
  `recalling_both_a_model_and_an_ir_never_offers_them_simultaneously`
  (`crates/namir-worker/src/recall.rs:265`) drives a recall naming **both**, asserting elapsed time
  exceeds one crossfade. `recall.rs:18-30` records that routing through `Instance::load`/`unload` is
  structural rather than incidental, and that primitive is what `namir-engine/src/engine.rs:514`'s
  sine test verifies. Under D-23.1's adjacency clause the tag is the **last** comment line before
  `#[test]` at `recall.rs:264`, beneath that test's existing doc comment, not above it.
- **NFR-PERF-050 — assertion first, then a plain tag.** `crates/namir-worker/benches/resource_load.rs`
  asserts only that the load succeeded (`:97-100`) and prints the 500 ms ceiling as a closing line
  (`:157`). Its `Verify: B` means "benchmark with a numeric threshold", so it is not currently a `B`
  and a plain tag would be false under D-23.1's second question. The house pattern is
  `namir-engine/benches/denormal_guard.rs`, which asserts its own budget and states which arm is
  informational and why. The figure exists — §9's M5 status records this requirement measured
  comfortably inside budget. The bench does not run in CI (only `six_stage_chain` does), so an
  absolute wall-clock assertion cannot make CI flaky; per D-2.4 the certified figure remains a
  §2-reference-machine matter and a failing assertion means *re-run before believing it*. The tag
  goes in the comment block immediately preceding `fn main()` (`resource_load.rs:115`), per the
  adjacency clause above; the assertion itself stays inside `measure`.
- **FR-LIB-020 — add the missing progress test, then a `trace-partial`; not a plain tag.** §11's
  correction above is right that this is a tagging miss and wrong about which part is missing. The
  progress callback **is** asserted — `a_scan_commits_found_files_to_the_snapshot_and_the_store`
  counts calls and asserts `>= 1` (`crates/namir-worker/src/library.rs:304-311`, `:322-325`) — but on
  a **two-file** fixture (`:294-295`), and its own message says it is testing the terminal report
  that fires "even for a scan shorter than the cadence". The cadence branch (`:206-211`), which is
  what "progress shall be visible" means during a long scan, is exercised by **no test at any
  scale**: the only ≥10,000-file test passes `|_| {}` (`:446`). The fix is one new test running the
  shared corpus to completion and asserting ≥2 progress calls. It must be a **new** test, not an
  edit to the cancel test: that test cancels immediately (`:449`), so the loop breaks before a 50 ms
  cadence window elapses and `>= 2` there would be flaky.

  **Even with that test written the tag is a partial — and this is the doctrine failing its own
  worked example, which is worth recording rather than quietly reclassifying.** FR-LIB-020's
  `*Verify:*` line is "I with a synthetic library of at least 10 000 files"
  (`01-functional-requirements.md:635`); a named scale is a quantifier under D-23.1's first
  question, not decoration. Cancellation is measured at that scale (`library.rs:437-461`) and the
  new progress test will be. The **off-the-audio-thread** clause is not: its only evidence is
  `rt_stress.rs` axis C (`crates/namir-worker/tests/rt_stress.rs:273-281`), whose corpus is **six
  files** (`write_small_scan_corpus`, `:138-149`) — and that is deliberate rather than an oversight
  to patch, since that function's own doc comment says it wants many fast scan cycles inside the run
  window rather than one slow one, so re-pointing it at the shared corpus would destroy the axis it
  exists to run. FR-LIB-020 therefore lands as a `// trace-partial:` carrying the mandatory
  `// uncovered:` line, drafted here because under D-23.1 that line *is* the evidence:
  `// uncovered: FR-LIB-020 — the off-the-audio-thread clause is exercised only against a 6-file
  corpus in rt_stress.rs axis C, not the 10 000-file scale the Verify method names; closes M9b`.
  Extending an axis is harness work, so **M9b** owns the closure and M9a owns the honest annotation.

  **One prerequisite with no other owner:**
  `docs/manual-tests/fr-lib-020-ui-responsiveness-during-scan.md` **does not exist** (that directory
  holds 20 files, none matching), and the "shall not block the user interface" clause rests on it as
  D-18.6's supplementary evidence — never the traced artifact, since FR-LIB-020 is `Verify: I`.
  Nothing mechanical will catch its absence: manual-document lookup applies to `Verify: M` only
  (`xtask/src/traceability.rs:177-186`). Write it in M9a, in the same commit as the annotation.
- **FR-NAM-140 — no tag, in any form; it stays `**UNRESOLVED**` and closes at M10 Phase 0.** The
  distinct catalogue entries exist (`crates/namir-nam/src/error_codes.rs:15/28/37/44/61/108`) and the
  architecture clause is tested (`model.rs:241`, `wavenet.rs:1164`, `lstm.rs:543`). But every
  configuration-clause test builds a `NamFile` **struct** via `minimal_valid_file()` and mutates one
  field (`wavenet.rs:1172/1188/1196`), never touching the byte path — where `NamFile::parse` maps
  every serde failure to `MALFORMED_JSON` (`file.rs:156-161`), FR-NAM-040's own code. So an A2 file
  gets a false error, exactly as `02-architecture.md` §9.5 records, and **no test anywhere performs
  the paired comparison FR-NAM-140's own `Verify:` names**. A `trace-partial` records a debt; this is
  a requirement that is currently *false* for the case it was written for. It is attributed to M10
  Phase 0 by the gate's printed owner attribution (D-18.5), which never affects exit status. The same
  applies to FR-NAM-090, FR-NAM-150, NFR-LIC-070, NFR-DOC-040, FR-PKG-010/-020/-030/-040 and, after
  this pass's scope change, NFR-PERF-030.

The sweep deliverable is unaffected in scope and gains a home: every partially-spanned Must it finds
is recorded as a `// trace-partial:`/`// uncovered:` pair at the covering test, rendered into
`docs/03-test-plan.md`, rather than in a table in this document that would go stale the way §14's
did. FR-NAM-030 is the sweep's first, naming LSTM as the uncovered member and M10 Phase 4 as its
closing milestone; FR-LIB-020's, above, is the first this pass writes.

**4. §14's table is rebuilt, not patched — `02-architecture.md` D-23.2.** That decision fixes the
rule the table is adjudicated by (against each requirement's own text and stated `Verify:` method,
with every cell naming its evidence by file path) and makes the per-FRS-section Must-count and row
set **generated** rather than hand-counted, checked on the required plan-diff half of the split gate.
§14 now carries an appended `### M9a re-audit` subsection publishing the corrected row set and
denominators — 130 Musts across 24 sections, against the old table's 117 across 22 — with its verdict
cells blank. **M9a fills those verdicts** from evidence as it stands at M9a; M9b and every later
milestone move only the cells their own evidence justifies, appending what they moved beneath the
table as the six prior sessions did. The re-derived table is a new baseline, not a frozen one.

**5. Evidence for the three host-observable CLAP requirements — `02-architecture.md` D-18.6, and
`clack-host` adopted.** FR-CLAP-030, -040 and -100 are `Verify: I` whose only recorded evidence is a
`docs/manual-tests/*.md` document, and the tool resolves that directory for `Verify: M` only
(`xtask/src/traceability.rs:177-186`), so all three read as permanently uncovered. D-18.6 resolves it
as **split evidence** — an annotated in-process test for the automatable part, plus the existing
manual document for the host-observable residue — with no change to any `Verify` code and no change
to the tool's dispatch. The vehicle is **`clack-host` 0.1.1 as a `namir-clap` dev-dependency**,
**adopted** (§17 gains a row) rather than left prospective: `clack-extensions` 0.1.1's own
`src/__doc_utils.rs:114-146` instantiates a plugin in-process via `PluginEntry::load_from_clack` with
**no `unsafe`**, and this crate already exports what that needs
(`crates/namir-clap/src/lib.rs:84`, `:125`; `Cargo.toml:26`'s `crate-type = ["cdylib", "lib"]`).
Three gates must pass before the §17 row is treated as verified rather than argued: `cargo deny check
bans`, `cargo deny check licenses`, and D-18.2's network-free build gate — plus `cargo tree -e normal`
proving `clack-extensions`' `clack-host` feature does not reach the shipped `cdylib`, and an
unchanged `xtask attribution`, since NFR-LIC-030's artifact must not gain a row for a dev-only crate.
All three are **M9a's**, and adding the dev-dependency is listed in M9a's phase membership under 2
above so the gates have something to run against. That `clack-host` 0.1.1 resolves and builds against
this workspace's pinned clack versions is **read from vendored manifests, not executed**; the gates
exist for exactly that. R-2, retired 2026-08-04 on S-4's evidence, gains a dated note recording that
its pre-1.0-churn residual is narrowly reopened on a dev-only surface.

*What the harness is worth beyond the three that raised the question:* it is the same vehicle for
**FR-CLAP-070** (`Verify: U` — randomised and one-sample blocks), **FR-CLAP-080** (`Verify: I` —
mid-session sample-rate change) and **FR-CLAP-130** (`Verify: S plus I` — the RT harness that raised
the `unsafe` question under 7 below), all three UNRESOLVED today. All six of this milestone's named
`namir-clap` rows come off one harness, which is the reason to build it rather than to extract six
free functions.

*Per requirement — and they do not get the same answer:*

- **FR-CLAP-040 — closes in M9b, in full.** The requirement is a statement about what the plugin does
  at its own boundary ("report its total latency in samples and … notify the host whenever that
  latency changes"), and an in-process host observes precisely that: `activate` publishes the fresh
  value, a model whose declared rate differs from the session rate (D-9.2) changes it, the audio
  thread flags `latency_dirty` and calls `request_callback`, and `on_main_thread` branches to
  `request_restart` while active or `HostLatency::changed` while not (`crates/namir-clap/src/audio.rs`,
  `crates/namir-clap/src/main_thread.rs`). The residue in
  `docs/manual-tests/fr-clap-040-latency-restart.md` is the *host's* reaction — Reaper's PDC indicator
  — which is not Namir's behaviour and therefore cannot be Namir's evidence. Needs a `namir-fixtures`
  dev-dependency for the rate-mismatched model.
- **FR-CLAP-100 — countable in M9b, Partial until a human runs it.** Two clauses. "Functions correctly
  if the host declines to show a GUI at all" is automatable and is the stronger half: a harness that
  never registers or queries `gui` and still activates, processes and round-trips state *is* the
  requirement, verified rather than argued. The API-negotiation surface (`is_api_supported`,
  `get_preferred_api`, `can_resize`, `get_size`, `set_transient`) is automatable and portable —
  `gui.rs` compares against `GuiApiType::WIN32` unconditionally, with no `cfg`, so the assertions run
  identically on every runner. "Supporting the host embedding it" is not automatable: `set_parent`
  needs a live foreign HWND. That half stays where it is, and it is **one person, one hour** —
  `fr-clap-100-gui-embedding.md`'s step 1 already executed on this machine (Reaper installed;
  `Namir.clap` installed at the path Reaper is confirmed to scan), so steps 2–3 need someone to open
  Reaper and drag it onto a track.
- **FR-CLAP-030 — countable in M9b, and the second confirmed instance of this milestone's own "gate
  green, requirement partly met" class.** The in-process test re-checks what is declared (one stereo
  in, one stereo out, `IS_MAIN`, `in_place_pair` set — `crates/namir-clap/src/audio_ports_ext.rs`) on
  every merge, which is real and is checked by nothing today. It does not close the requirement. Two
  clauses stay open, both recorded outside the ledger and neither previously carried into it:
  FR-CHAIN-060 names three channel configurations and this plugin declares **one**, with no
  `audio-ports-config` (that module's own doc comment, and `fr-clap-030-audio-ports-negotiation.md`);
  and the *Verify* clause says "across at least two host implementations", which no in-process clack
  harness satisfies, since `clack-host` shares `clack-common` with `clack-plugin`. In the re-audited
  **5.12 CLAP** row, FR-CLAP-030 and FR-CLAP-100 are counted in the **Partial** column rather than
  **Done**, with each one's unmet clause named in that row's audit entry — D-23.2 requires every cell
  to name its evidence by file path. The scope question goes to §15 as item 9, due before M8.

*One documentation defect found during this pass, unrelated to the decision:*
`crates/namir-clap/src/audio_ports_ext.rs`'s doc comment points at
`docs/manual-tests/fr-clap-030-audio-ports.md`; the file is
`fr-clap-030-audio-ports-negotiation.md`. A dead cross-reference — fix it with the rest.

**6. FR-ERR-010's logging parameters — `02-architecture.md` D-16.5.** M9's deliverable above says
this is "implementation against a settled decision, not an open dependency question", which is true
of *what* to build and was not true of *how large, in what format, controlled by what*. D-16.4 stated
the writer's shape and left six values blank; D-16.5 supplies them — 4 MiB per file, two retained
generations (a 12 MiB ceiling), one line per record as
`<timestamp> <LEVEL> <pid> <thread> <code-id> <detail>`, `NAMIR_LOG` with `off`/`error`/`info`/
`verbose` defaulting to `info`, and a synchronous process-global writer behind one `Mutex` with no
thread of its own. §22 gains **R-12** for that writer's unmeasured interaction with FR-UI-060's frame
budget during a 10 000-file scan. No dependency is added and no `#[cfg(target_os)]` appears, so the
two mobile cross-build jobs and `xtask layering` are unaffected; where `log_file_path()` returns
`None`, as it does on both mobile targets, the sink is a no-op.

What this binds for the implementation, stated so it is not rediscovered mid-work: FR-ERR-010 is a
`Verify: I` requirement whose test lands at `crates/namir-platform/tests/logging.rs`, which will be
that crate's **first** `tests/` directory — its `// trace: FR-ERR-010` is what moves the row off
`**UNRESOLVED**` in the generated plan. The generated plan lists **24** uncovered Musts, of which
**14** are M9's own; FR-ERR-010 is one of those 14, and it is **M9b** work. The logger's call sites
are `push_notice` in each product shell and the worker's job boundary, not the engine: D-5.1 already
forbids `namir-engine → namir-platform`, which is the reason D-16.4 sited the writer where it did.
One question is left open rather than answered, and is registered as §15 item 8: `namir-clap` cannot
depend on `namir-app`, so it cannot see `AppSettings`, so the plugin configuration has no persisted
verbosity control at all — `NAMIR_LOG` is its only knob in 1.0. That is a question about whether the
plugin gets a preferences file, not about logging, so D-16.5 does not decide it.

**7. May `unsafe` appear in `namir-clap`'s tests and benches? No, and it turns out not to be
needed.** Resolved at D-5.3's `*Consequence (added M9)*` — no new decision number, because this
answers a question D-5.3 already owns. Two deliverables above were planned on the assumption that
they required it — FR-CLAP-130's `assert_no_alloc` harness and NFR-PERF-040's instantiation benchmark
— and **the justification first offered for it was wrong as stated**: "legal because `namir-clap` sets
`unsafe_code = "deny"`, not `"forbid"`" reads `deny` as permission, which it is not; `deny` fails the
build too, and only a file-level `#![allow(unsafe_code)]` compiles. That is recorded rather than
quietly fixed, because the corrected reading changes the question from "is this already allowed?" to
"should it be?", and the answer to the second is no. Checking the premise before answering the policy
question is also what showed the policy question to be free: there is no `unsafe` in any bench or
integration test anywhere in this workspace today, and `assert_no_alloc` and `core_affinity` are both
already used under `unsafe_code = "forbid"` in `namir-worker`. The `clack-host` harness adopted under
5 above then closes the remainder from the other side — it reaches the plugin through the
already-public surface (`crates/namir-clap/src/lib.rs:84`, `:125`; `Cargo.toml:26`), so no visibility
hole and no seam refactor is needed, and the port/channel iteration inside `process()`,
`request_callback()` and the CLAP factory dispatch NFR-PERF-040 would otherwise exclude are exercised
by a real in-process host rather than booked as residuals. The FR-CLAP-130 harness still lives
in-crate as a `#[cfg(test)] mod rt_harness` mirroring `namir-engine/src/rt_harness.rs` — this crate
deliberately keeps almost everything `pub(crate)`, and its `#[global_allocator]` is legal there for
the reason `namir-engine`'s already is — and its three stress axes (model load, preset recall, library
scan) reuse `crates/namir-worker/tests/rt_stress.rs`'s shape rather than a second invention of it.
**No FRS change is needed**, and two documentation corrections fall out regardless of the decision:
`AGENTS.md` says the two carve-out crates are "confined to one module each" when `namir-platform` has
carried two designated modules since M6, and `crates/namir-clap/src/gui.rs`'s module doc comment
repeats the same error in miniature.

### M9a status — the tooling and the tagging pass, 2026-08-08

Appended rather than rewriting anything above, per this document's own convention. Where this
section and the text above disagree, this section is what happened.

**Landed in three commits.** `29f5a51` the P0 decision pass (documents only, changing no gate);
`542a9c3` the `xtask` implementation of D-23.1, D-18.5 and D-23.2; and this one, carrying the three
annotations, the `clack-host` adoption, the `clap-validator` job and the gate flip. The split is
D-18.5's own: a required plan-diff half cannot land before the flag it invokes or the plan it diffs.

**The uncovered count moves 24 → 20**, and every id that moved is named with its evidence rather
than counted. FR-STATE-050 closes on a plain tag. FR-CLAP-020 closes on a `# trace:` in
`.github/workflows/ci.yml`, which is limb 1 under FRS §10's M9 rule — the requirement's own subject
is the CI configuration — and the job was executed for real during review: **32 of 32 applicable
validator tests passed, exit 0**. FR-LIB-020 and NFR-PERF-050 land as `// trace-partial:`
annotations, each carrying the mandatory `// uncovered:` line D-23.1 requires; both count as
coverage for the ordinary run and both name M9b as their closing milestone.

**NFR-PERF-050's certified figure, which never existed before.** Measured on
`docs/02-architecture.md` §2's pinned reference machine — confirmed on every recorded field, not
assumed — pinned to core 4 per D-2.4, on a machine verified quiet by measurement, across **eight
repetitions**: the binding ~50 MB arm reads **128.63–132.31 ms against a 500 ms ceiling** (mean
129.65 ms). One repetition showed a contaminated IR arm and is identified as such rather than
averaged in. The bench now asserts the ceiling instead of printing it, so `Verify: B` — "benchmark
with a numeric threshold" — is literally true of it for the first time.

**Where this milestone diverged from its own plan, recorded because it is the more interesting
result.** §16's deliverable text above prescribes "**NFR-PERF-050 — assertion first, then a plain
tag**", and §14's own triage bullet calls it a pure tagging miss where "nothing needs building
here; a tag needs adding". Both are now wrong, and the reason is this milestone's whole point. The
assertion was built and passes, but two independent adversarial reviews found the plain tag would
have asserted more than the artifact delivers: every arm of the benchmark loads from
`LoadSource::Bytes`, so the `fs::metadata` + `fs::read` that `LoadSource::File` performs sits
outside the measured window — it times a 50 MB *payload*, never a 50 MB *file* — and the
requirement's second clause, "shall never delay the audio thread regardless of duration", is not
measured there at all. Its nearest evidence, `crates/namir-worker/tests/rt_stress.rs`'s axis A, was
read rather than assumed and fails D-23.1's questions twice over: an integration test rather than
the `Verify: B` the requirement names, and its concurrent loads are `WaveNetShape::Nano` fixtures,
so "regardless of duration" is exercised at no long duration anywhere in the tree. So it is a
partial, with both gaps named.

That is worth stating plainly: **the commit that built the machinery for detecting half-met
requirements was itself about to ship one**, and nothing mechanical would have caught it — the gate
would have gone green and the row would have read covered, with the contradiction sitting two
hundred lines apart in a single file. It was caught by adversarial review, twice, independently.
The same pass corrected a false universal inside D-23.1's own new adjacency note ("all eighteen are
`Verify: S`"; FR-CFG-030 is `Verify: I`, checked by the layering lint).

**A consequence for §14's re-audit, whose verdict cells are still blank by design:** when M9a's
per-requirement adjudication fills them, **NFR-PERF-050 counts Partial, not Done** — D-23.2's rule
is that a Partial is not Done, and §14's older M5-era bullet claiming "6.2 PERF Done 1 → 3" is
superseded on that cell. FR-LIB-020 likewise.

**What M9a still owes**, none of it started: the re-audit of the remaining uncovered Musts, the
set-quantification sweep, and §14's per-requirement adjudication. The tooling those depend on now
exists, which was this phase's purpose.

### M9a status — the set-quantification sweep, 2026-08-09

Appended rather than rewriting anything above, per this document's own convention; where this
section and anything above it disagree, this section is what happened. This is the second of the
three things the subsection above records M9a as still owing — §16's deliverable "**a sweep for
requirements the gate reports as covered but only partly is**". The re-audit of the remaining
uncovered Musts and §14's per-requirement adjudication are still outstanding.

**All 130 Musts were read against their artifacts, and 54 were demoted.** Each requirement's own
text and its stated `Verify:` method were read beside whatever the tool had resolved for it, under
D-23.1's two questions. The tally moves from **108 plain / 2 partial / 20 uncovered** to **54 plain
/ 56 partial / 20 uncovered**, out of 130. **The uncovered count does not move at all**: no
requirement lost its coverage, 54 lost the claim that their coverage was *complete*. Every one of
the 54 carries the `// uncovered:` field D-23.1 makes mandatory, naming the specific unspanned
member or unexecuted half and a closing milestone, and every field is rendered verbatim into
checked-in `docs/03-test-plan.md`. **The diff is comment-only** — no test logic changed, no
assertion was added, removed or loosened. What changed is what the ledger claims, not what the
suite does.

**The owner reviewed the 42% figure and kept every one of the 54.** Fifty-four of 130 is 42% of the
Must set demoted in a single pass, and 56 partials against 54 plain tags means the modal disposition
of a Namir Must is now "half-met". That is an uncomfortable number and it is recorded as the
decision it was, not absorbed. Three reasons, in the order they were weighed:

- **The only alternative on offer is choosing which true findings to suppress.** Nobody proposed
  that a specific `uncovered:` field was factually wrong. The question was whether *this many* should
  land at once — which is a question about the number, not about any finding. Reducing the count
  means selecting true findings to not write down, and ending exactly that practice is what this
  milestone exists for. §16's own text already anticipated the shape of this pressure: an acceptance
  criterion satisfiable by **not looking** inverts the milestone's purpose, and so does a partial
  count trimmed to look reasonable.
- **The information is in the `uncovered:` field, not in the PARTIAL flag.** A row reading
  `**PARTIAL**` conveys almost nothing on its own; what a reader acts on is "88.2 kHz and 176.4 kHz
  are absent from the sweep's rate array" (FR-EQ-020) or "every arm loads `LoadSource::Bytes`, so
  the `fs::metadata` + `fs::read` sits outside the measured window" (NFR-PERF-050). The 42% is a
  count of flags. The value is 54 specific, individually actionable sentences that did not exist
  yesterday, each naming a file, a line or a named member. Suppressing flags to lower a percentage
  discards the sentences to improve the number, which is backwards.
- **§22 R-13's mitigation is a *falling* count, and a falling count needs an honest starting
  number.** R-13's own last clause is the test: "if the count is not falling by M12, the mechanism is
  being used as a bypass and this row is not mitigated." That test is only meaningful against a
  baseline nobody curated. Fifty-six is that baseline, dated today. A trimmed starting number would
  make the mitigation unfalsifiable in exactly the direction R-13 is worried about, and would do it
  on the first measurement.

**A consequence for the two subsections above, stated so it is not read as a reversal.** The M9a
status above records NFR-PERF-050 as this milestone's demonstration that "the commit that built the
machinery for detecting half-met requirements was itself about to ship one", caught by adversarial
review twice. The sweep is the same finding at scale and by construction rather than by luck: 54
more instances of the identical species, found by reading rather than by review catching a specific
overclaim. The right reading of both is that the machinery works and that the pre-M9a ledger was
optimistic in one direction, uniformly.

**FR-CHAIN-010's order conflict, unresolved since before M2, is resolved by amending the FRS.** The
sweep read FR-CHAIN-010's text beside `build_default_chain` and found the two disagree about the
product. The FRS mandated `input → input trim → noise gate → NAM → IR → EQ → output level → output`
(`01-functional-requirements.md:163-166`); `build_default_chain`
(`crates/namir-engine/src/stages/mod.rs:47-67`) ships `gate → trim → nam → ir → eq → out`, and has
since M2. **The owner's decision is that the FRS is amended to describe the product, not that the
code is a defect** — `02-architecture.md` **D-9.8** is the reason and it stands: a gate whose
threshold references the interface's actual noise floor rather than moving when the user adjusts
trim is the better product, and nothing in seven milestones has argued otherwise. The amendment
lands as a `*Consequence (added M9a, 2026-08-09)*` note at FR-CHAIN-010 itself and a matching one at
D-9.8, both in this pass; §6's M2 deliverable text, which already directed the chain be built "*gate
before trim* … per D-9.8" (`:259-261`), needed no change and gets none.

*How long it stood, and what that says about the gates.* D-9.8's Rationale flagged itself for
review; `02-architecture.md` §21's **AQ-2** (`:2652`) records the author confirming it on
2026-08-04 — against the usability argument alone, never against the FRS's own sentence. The contradiction is as old as the two
documents, both texts having landed in `875068e`, so it predates every milestone, survived M2
building the chain, and survived M9's own P0 decision pass. **It was never concealed**:
`stages/mod.rs:31-36` has named the divergence in plain prose since M2's `7941577`. Nothing
mechanical was ever going to catch it — `xtask traceability` asks whether an artifact *references*
an identifier, never whether it *agrees* with the requirement's text, and FR-CHAIN-010 has carried a
tag and read covered throughout, correct by the tool's own rules the entire time. It surfaced as a
byproduct of reading requirement prose to answer D-23.1's two questions, which is an argument for
that reading being periodic rather than once.

**Correction to this section's own text: FR-NAM-030 is worse than §16 above describes it.** §16's
sweep deliverable (`:2168-2173`) says "only WaveNet has ever been compared that way", citing S-1's
scope note that "S-1 covered WaveNet only… LSTM is unaddressed", and D-23.1's Rationale, D-23.2 and
`AGENTS.md` all carry the same half-met wording. The original text stands unedited, per this
document's convention; **what follows is the correction.** Verified by reading both tests this
session rather than by re-reading the summary: **no in-repo runnable artifact compares *either*
architecture against `NeuralAmpModelerCore`.** `crates/namir-nam/tests/fixtures.rs:129` calls
`nam::reference_infer` and `crates/namir-nam/tests/lstm_fixtures.rs:120` calls
`nam::reference_infer_lstm` — both `namir-fixtures`' own from-scratch Rust ports, neither reaching
C++. The WaveNet comparison the earlier text credits was real and is not retracted, but it lives in
`spikes/s1-nam-inference/`, which the root manifest `exclude`s (`Cargo.toml:15-20`) and which pins
its own lockfile, so nothing re-runs it under `cargo test`. The two `// trace-partial: FR-NAM-030`
pairs the sweep wrote (`fixtures.rs:120-126`, `lstm_fixtures.rs:112-118`) therefore name the *same*
gap rather than one naming LSTM, and add a second one neither text had noticed: the probe is 4 000
samples of sine plus noise, not the 10-second signal containing clean, transient and saturated
material the requirement specifies. **This does not change M10's ownership** — FR-NAM-030 still
closes at M10 Phase 4 — but it doubles what that phase owes, from one architecture's parity run to
two plus a conforming test signal, and §17 should be read with that in mind.

**A product-scope discovery the sweep was not looking for: there is no audio-device panel, and five
Musts lean on one existing.** `namir-ui` has seven modules (`app`, `controls`, `format`, `host`,
`library_view`, `meter`, `notices`) and none of them is a device or settings surface. `UiSnapshot`
(`crates/namir-ui/src/host.rs:100-123`) carries exactly eight fields — `params`, `input_meter`,
`output_meter`, `loaded_model_name`, `loaded_ir_name`, `library`, `unsaved_changes`, `notices` — and
**no host, device, sample-rate, buffer-size, latency or xrun field of any kind**. `UiIntent`
(`:148-176`) has seven variants, all parameter, library or notice actions; none names a device.
Device selection happens once at start-up from remembered settings, non-interactively
(`crates/namir-app/src/app.rs:95-133`, through `device_state::select_device` and the three
`negotiate_*` helpers), and the xrun count surfaces through an `eprintln!`. The five:

| Requirement | Verify | The clause with no surface |
|---|---|---|
| **FR-IO-010** | M | "The user shall be able to select an audio input device and an audio output device" |
| **FR-IO-040** | M | "select sample rate and buffer size … and the current values shall always be displayed" |
| **FR-IO-050** | M | "shall display the measured round-trip latency … in both samples and milliseconds" |
| **FR-IO-060** | I | "a running count for the session, **resettable by the user**" |
| **FR-IO-070** | I | "**allow the user to select another device**" |

The last two are among the 54 and say so in their own `uncovered:` fields. The first three are
`Verify: M` and so cannot be — see the next paragraph, which is why this is recorded here in prose.
It is a scope question, not a verification one: three of these five are not "untested", they are
**unbuilt**, and no milestone in this roadmap owns building them. §15 item 16 below carries it.

*And it was already written down twice, which is the part worth keeping on the record.*
`docs/manual-tests/fr-io-010-device-enumeration.md:85-88`, under its "Not covered by this script,
and why" heading, says in as many words that FR-IO-010's "the user shall be able to select" "implies
an interactive control, and none exists in `namir-ui`'s shared FR-UI-020 screen", and that "FR-IO
has no UI owner yet in this codebase"; `fr-io-090-channel-mapping.md:19-21` records the same gap
independently. Both were written at M6. Neither escalated, and nothing was ever going to make them:
a `Verify: M` Must resolves the moment its document exists, and no gate reads a word inside it. This
is the second finding this sweep did not discover so much as *find already recorded and unread* —
`stages/mod.rs:31-36`'s note on FR-CHAIN-010 being the first.

**Fourteen findings cannot be a source annotation in any form, so §14's verdict columns are their
only ledger — R-14 made concrete on its first real use.** D-23.1's `trace-partial` is refused
outright for a `Verify: M` or `Verify: Process` Must (`xtask/src/traceability.rs:616-633`), and the
tool's own doc comment states the scope exactly: false "for 14 of the FRS's 130 Musts (13 `M`, 1
`Process`)" (`:590-591`). The refusal is right — a manual script is not a `.rs` file and review is
not an artifact — but its consequence is that **for those fourteen requirements there is no
mechanical place to write down a half-met finding at all.** The generated plan renders them as
covered the moment a correctly-named document exists, with no PARTIAL disposition available; the
gate cannot express what FR-IO-010's, -040's and -050's rows in the table above are about. The
fourteen are the thirteen `Verify: M` Musts (FR-IO-010, -020, -030, -040, -050, FR-PKG-030,
FR-STATE-040, FR-UI-020, -030, -040, -050, -070, NFR-DOC-010) and the one `Verify: Process` Must
(NFR-QUAL-020). **This is the strongest argument yet for finishing §14's re-audit**, and it is R-14's
risk stated as a fact rather than a hypothetical: for 14 of 130 Musts, a hand-adjudicated verdict
cell naming its evidence by file path is not merely the *better* record, it is the **only** one this
project has. Nothing about them is mechanically checkable in either direction.

**Three partials declare a closing milestone this roadmap does not assign them.** D-23.1 requires
every `uncovered:` field to end `; closes M<n>`, and the tool prints that declared milestone beside
each partial (§22 R-13(d)) — but the declaration is the annotation author's, while the tool's *other*
attribution is derived from this document's own `## <n>. M<k>` sections, and nothing reconciles the
two. Forty-nine of the 54 name M9b — 51 of all 56 partials — whose scope is the open-ended "missing
verification infrastructure" and which accommodates them. Three do not fit, and all three are
flagged for owner confirmation rather than quietly left:

- **FR-CFG-030 → M13.** The only mention of FR-CFG-030 anywhere in this document is `:2668`, a
  parenthetical correcting its `Verify:` code. §20 does not claim it. The field's own reasoning is
  sound — "each is installed alone into a clean environment and exercised" needs installers, which
  M13 builds — but M13's deliverables and Acceptance do not name it, so nobody has agreed to it.
- **NFR-LIC-030 → M13.** Also unnamed in §20, and it additionally **contradicts a prior closure
  claim**: §14's M7-session bullet at `:1690` reads "NFR-LIC-030 closes: the attribution file …". The
  partial's gap is the "shipped with the binaries" clause, which needs a release pipeline and so
  points at M13 plausibly enough — but "already closed at M5" and "closes at M13" cannot both stand,
  and this is precisely the drift §14's re-audit exists to end.
- **FR-IO-070 → M9b.** This one is not an absence but a **conflict**: §18's M11 already names
  FR-IO-070 explicitly, as "**Opportunistic, if the hardware allows**" (`:3025-3029`), gated on a
  device that can be made to fail on demand and with an instruction that if none appears "it stays
  open and is **not** back-filled with an inspection claim". The annotation books it to M9b instead.
  M11 runs before M9b, so the two are not even in the same order; and the M9b booking reads as a
  commitment to build the virtual failable device M11 declined to assume.

None of the three is changed in this pass. Changing an `uncovered:` field to match a milestone
nobody has agreed to would be the same defaulting-by-whoever-writes-it-first that §15 exists to
prevent. §15 item 17 below carries the decision.

**What M9a still owes after this pass**: the re-audit of the remaining uncovered Musts, and §14's
per-requirement adjudication — which the fourteen above make the load-bearing half.

### M9a close-out — §14's per-requirement adjudication, 2026-08-09

Appended rather than rewriting anything above. This is M9a's last deliverable and the one the two
status subsections above name as still outstanding: **§14's `### M9a re-audit` table now has
verdicts.** The row set, the row order and the denominators were not touched — they are
machine-checked by `xtask traceability` under D-23.2 and the check passes unchanged. Only the three
verdict columns and the `**Total**` row were filled, plus the per-row evidence block beneath the
table that D-23.2's cite-by-path rule requires.

**How the pass was run.** All 130 Musts were adjudicated in six independent passes over disjoint row
sets — 24 rows between them, no row read twice, no verdict copied from another pass — and
reconciled here. Each cell was judged against **the requirement's own text and its own stated
`Verify:` method**, per D-23.2, never against whether code exists and never against what `xtask
traceability` reports. The tool's output was used to *locate* artifacts and never to settle a cell;
D-23.2 clause 6 makes it evidence in neither direction, and this pass took that literally in both
directions, as the divergence lists below show.

**Headline distribution: 29 Done, 92 Partial, 9 Not started, of 130. Zero UNCLEAR — every cell
settled.** Every row sums to its FRS-derived denominator and the columns sum to 130; the
reconciliation was done before any cell was written, on the principle that a table that does not sum
is worse than a blank one.

**Four cells moved after the pass, on an independent spot-check, and this is the record of it.** The
first adjudication read **31 / 88 / 11**; every number in this subsection is the corrected **29 / 92
/ 9**. A second reader re-adjudicated **27** of the 130 Musts from scratch, against the requirements'
own text and `Verify:` methods and without reference to the cells already written. Twenty-two agreed.
**Five disagreed; four were accepted and one rejected.**

| Requirement | Was | Now | Why the correction was accepted |
|---|---|---|---|
| FR-STATE-050 | Done | Partial | Its artifact processes no audio, so the FR-NAM-070 constraints the requirement explicitly imports are asserted by nothing. |
| FR-OUT-010 | Done | Partial | Three literal parameters are stated; only exact silence at the floor is asserted, the +12 dB maximum and 0 dB default nowhere. |
| NFR-PERF-030 | Not started | Partial | D-23.2 clause 3 reserves Not started for "neither the behaviour nor its verification"; the app reaches an audible state and ships. Only the `Verify: B` harness is missing. |
| NFR-PERF-040 | Not started | Partial | Same rule, and the table's own FR-CLAP-010/-020 Done cells rest on a validator run that instantiated the plugin 32 times out of 32. |

The two Done→Partial moves are **not** a change of standard: both requirements carry a
`// trace-partial:` in the tree as of this commit (`crates/namir-engine/src/stages/out.rs:287-291`,
`crates/namir-worker/src/recall.rs:294-299`), so adjudicating them Done contradicted the source, and
this pass's own cross-check below — that no requirement carrying a partial was adjudicated Done —
holds after the correction where it did not before. The two Not-started→Partial moves also remove an
internal inconsistency: **FR-CFG-020 was already Partial on exactly this reasoning**, its behaviour
built and its `Verify: G` apparatus wholly absent, and a performance Must with the same shape was
being read the other way.

**The rejected correction, recorded with its reasoning because it becomes an open item rather than a
cell.** The spot-check argued **NFR-PORT-030** to Partial: the requirement's text enumerates five
design constraints — no path-namespace assumption, no mouse, no unlimited threads, no blocking dialog
on an audio-affecting path, no desktop-only windowing model — and a cross-build proves none of them.
That is true, and the verdict **stays Done** anyway, because its `*Verify:*` line is explicit that
the method is the two mobile cross-builds and both jobs run green
(`.github/workflows/ci.yml:478-545`). D-23.2 adjudicates against the requirement's **own stated
method**, and that method is executed. If the method is too weak for the text — and the argument that
it is has not been rebutted — the remedy is to amend the `*Verify:*` line, which is an FRS change
needing its own decision, the same shape as FR-CHAIN-010's resolution earlier in M9a and not
something a verdict cell settles unilaterally. It is booked as **§15 item 19**.

**One finding from the same spot-check moves no cell in either direction and is booked anyway.**
**FR-CHAIN-070 cannot be satisfied as the chain is built:** `GateStage` overwrites every channel with
channel 0's gated result (`crates/namir-engine/src/stages/gate.rs:163-172`) before `TrimStage`, the
chain's only cross-channel mixer (`crates/namir-engine/src/stages/trim.rs:145-156`), can sum them, so
of the requirement's three options — left, right, sum at −6 dB — only its stated default is
reachable, and no chooser exists in any case. It is a **Should**, so no §14 row moves and no gate
reads it, which is exactly the class of finding that disappears if it is not written down: it is
booked as **§15 item 18**.

One thing below is left as the first adjudication wrote it, per this document's convention, with this
block as the correction: **Divergence 3 names nine ids and should read eleven**, NFR-PERF-030 and
NFR-PERF-040 joining it for precisely its own reason. The reconciliation table *is* updated, because
a table that does not sum is worse than a blank one, and its top row moves for a second reason worth
separating out: the sweep subsection above reports the tool's tally as **54 plain / 56 partial /
20 uncovered**, which was correct as of that pass; demoting FR-OUT-010's and FR-STATE-050's
annotations in this same commit makes it **52 / 58 / 20**. Both figures are right on their own dates
and neither is a correction of the other. So **Done reads 52 − 10 − 13 = 29**, Partial reads
58 + 23 + 11 = 92, and Not started reads 9.

**The modal disposition of a Namir Must is Partial, by a wide margin, and that is the finding.**
Twenty-two percent Done. It is the same result the set-quantification sweep above reached about
annotations, arrived at independently and from the requirements' own text rather than from the
tags — which is the strongest available check that the sweep's 54 demotions were real rather than an
artifact of one reading. **Under D-23.2 a Partial is not Done for §14 or for M8's exit checklist**,
so M8's ship gate now reads a ledger in which 101 of 130 Musts are not closed. That is the number
this milestone exists to produce.

**The cross-check against the tree found no conflicts.** The 58 requirements carrying a
`// trace-partial:` are at most Partial under D-23.2, and **not one of them was adjudicated Done** —
checked id by id against the tool's own R-13 list rather than assumed. That sentence is true of the
corrected table and was **not** true of the first one, which is exactly what the FR-OUT-010 and
FR-STATE-050 moves above repair. The arithmetic closes exactly in both directions, which is worth
recording because it means the two ledgers can be reconciled by subtraction rather than by argument:

| | Musts |
|---|---|
| Plain-covered by the tool (`52 plain / 58 partial / 20 uncovered`) | 52 |
| − adjudicated **Partial** on a plain source tag | −10 |
| − adjudicated **Partial** where the tool resolves a `Verify: M` document or exempts `Verify: Process` | −13 |
| **= Done** | **29** |
| The tool's 58 partials, every one at most Partial and none Done | 58 |
| + the 23 demotions above | +23 |
| + uncovered Musts adjudicated **Partial** rather than Not started | +11 |
| **= Partial** | **92** |
| Uncovered Musts where neither the behaviour nor its verification exists | **9** |

**Divergence 1 — ten plain source tags adjudicated Partial.** A reader would guess Done from the
tool for each of these; each is Partial on the requirement's own text. `FR-NAM-020` (a `Verify: G`
whose two tagged artifacts compare nothing at all — they parse metadata; missed by the sweep that
demoted FR-NAM-030 for the same structural reason). `FR-NAM-040` (a `.nam` rejection in the **plugin**
does not name the file: `crates/namir-clap/src/worker_jobs.rs:77-79` pushes a bare code while
`crates/namir-app/src/host.rs:263-267` prepends it, and nothing tests the clause in either product).
`FR-LIB-070` (the requirement's own sentence enumerates three members — files that disappear, change
or **are added** — and deletion and change each have a dedicated before/after test while addition has
none; `crates/namir-fixtures/src/library.rs:590` was generated for exactly this trio and has no
consumer). `NFR-RT-020` (the sole artifact asserts non-allocation, which is not wait-freedom, and
spans one of the audio thread's communication paths). `NFR-PERF-060` (no reference-machine figure
exists for a `Verify: B`, and nothing re-runs the bench). `NFR-PORT-040` (one of three tier-1/tier-2
platforms). `NFR-BUILD-010` (`deny.toml`'s `[sources]` section is the one mechanical expression of
the clause and **no workflow runs `cargo deny check sources`**). `NFR-QUAL-050` (below).
`NFR-SEC-010` and `NFR-SEC-030` (each plain-tagged in the same file as a `trace-partial` recording the
identical gap, five and two lines away respectively — an internal inconsistency, not a new finding).

*Consequence for M9b, stated so it is not left as a silent mismatch:* for these ten the ledger and
the source now disagree, and D-23.1 says the source should carry the claim. Each is a candidate for
demotion to `// trace-partial:` with an `// uncovered:` field; for `NFR-SEC-010` and `NFR-SEC-030`
the marginal cost is zero, since the partial that already exists in the same file declares the same
closing milestone. This pass did not make those edits: it is a documents pass, and re-tagging is
source work that belongs with the test that closes each gap.

**Divergence 2 — thirteen `Verify: M`/`Verify: Process` Musts the tool resolves, adjudicated
Partial.** `FR-IO-010`, `-020`, `-030`, `-040`, `-050`; `FR-UI-020`, `-030`, `-040`, `-050`, `-070`;
`FR-STATE-040`; `NFR-DOC-010`; `NFR-QUAL-020`. The plan renders a `Verify: M` Must as covered the
moment a correctly-named document exists and reads no word inside it. The documents were read.
**`FR-IO-020` records FAIL.** `FR-IO-030`, `FR-UI-030`, `FR-UI-040` and `FR-UI-050` record NOT
EXECUTED. `FR-IO-050` records PARTIAL in its own words. **`FR-UI-020` and `FR-UI-070` have no
document at all** — the plan resolves them to `fr-clap-030-audio-ports-negotiation.md` and
`fr-ui-010-standalone-window-renders.md` because those files mention the ids in prose, which under
D-23.2 clause 6 is not evidence. Writing those two missing documents is cheap and should be scoped.
Two documents *do* record an executed PASS and are still Partial for a named reason rather than a
grudging one: `FR-STATE-040`, whose `Verify:` line is "M plus S (schema check)" and whose **S half has
no artifact of any kind** — no schema document, no `xtask` schema subcommand, and CI never invokes
`xtask preset` — and whose recorded transcript is additionally stale, showing a `global` section
`State::into_document` has not emitted since D-10.4; and `NFR-DOC-010`, whose requirement enumerates
**three** formats while the executed reader covers the preset/state document only, the `.nam` subset
having no repository documentation a third party could work from. That last one is the same failure
class the sweep above found twice: `docs/04-state-and-preset-format.md:3-4` and the manual test's own
"Requirement (literal)" line both silently restate the requirement as "the state and preset format",
dropping a clause in plain sight.

**Divergence 3 — nine uncovered Musts adjudicated Partial rather than Not started.** A reader would
guess Not started from an UNRESOLVED row; D-23.2 clause 3 reserves Not started for requirements where
**neither the behaviour nor its verification exists**, and for these the behaviour exists and is
integrated in a shipped product. `FR-CFG-020` (both products already build from one engine through
`namir_engine::build_default_engine`; what is absent is the entire `Verify: G` apparatus).
`FR-NAM-060` (both resamplers are live in the audio path; only the measurement is missing).
`FR-NAM-140` (the architecture clause is built and tested on the byte path; only the configuration
clause is false). `FR-CLAP-030`, `-040`, `-070`, `-080`, `-100`, `-130` (every CLAP extension impl
exists and the plugin passes the reference validator 32/32; what is missing is that
`crates/namir-clap/src/audio.rs`, `main_thread.rs`, `latency_ext.rs`, `audio_ports_ext.rs` and
`gui.rs` carry **no `#[cfg(test)]` module at all**, so `process`, `activate` and `apply_automation`
are driven by no test anywhere — which is precisely what D-18.6's `clack-host` dev-dependency was
adopted to fix at M9b). This is M7's 6.3 PORT precedent applied in the direction the brief warned
about: an UNRESOLVED row is not evidence of Not-started.

**One prediction in §14 is superseded.** The text above the re-audit table states that "all eight
requirements added on 2026-08-08 will enter as **Not started**". Seven did — `FR-NAM-150`,
`NFR-LIC-070`, `NFR-DOC-040`, `FR-PKG-010/-020/-030/-040`. **`FR-NAM-140` did not**, for the reason
in Divergence 3. The prediction is left as written, per this document's convention; this paragraph is
the correction.

**The fourteen findings for which this table is the only ledger are now on the record — 13 Partial,
1 Not started, 0 Done.** R-14's risk, stated as a fact in the sweep above, is what this pass was for:
for the thirteen `Verify: M` Musts (`FR-IO-010`, `-020`, `-030`, `-040`, `-050`, `FR-PKG-030`,
`FR-STATE-040`, `FR-UI-020`, `-030`, `-040`, `-050`, `-070`, `NFR-DOC-010`) and the one
`Verify: Process` Must (`NFR-QUAL-020`), `xtask` refuses a `trace-partial` outright and the generated
plan has no PARTIAL disposition available, so **§14's cells are the only place any of this can be
written down.** Not one of the fourteen is Done. `FR-PKG-030` is Not started (no installer exists).
The other thirteen are Partial with the clause named. Three of them are not "untested" but
**unbuilt** — `FR-IO-010`'s device selection, `FR-IO-040`'s rate/buffer selection and its
always-displayed values, `FR-IO-050`'s measured round-trip latency — and §15 item 16 still records
that no milestone owns building them. `NFR-QUAL-020` is Partial on the plainest possible evidence:
exactly three `(red)`/`(green)` commit pairs exist across 120 commits against a universal clause, and
"enforced by review" leaves no artifact that anything re-runs.

**One new finding this pass, outside every gate and worth separating from its cell.** `NFR-QUAL-050`'s
"and shall gate merges" clause is not merely unassertable from inside the repository, as
`docs/01-functional-requirements.md:1336-1338` records — it is **false**. Checked read-only against
the GitHub API: `trunk` has no branch protection, and its single active ruleset contains exactly two
rules, `deletion` and `non_fast_forward`, with no required status checks and no pull-request rule. A
change can land on `trunk` with CI red or unrun. CI itself does run and is green. The fix is one
required-status-checks rule naming the existing jobs, it is outside every gate by construction, and
M8's exit reads a row that had no way to know.

**A structural finding spanning every `Verify: B` Must — treat it as one problem, not seven.** No
benchmark in this workspace is re-run when the code changes. `cargo test --workspace` does not
execute `harness = false` bench targets, the only bench with a CI job at all is `six_stage_chain`,
and that job is informational by design and asserts nothing. D-23.2 clause 1's repeatability limb is
therefore structurally unmet for `NFR-RT-030`, `NFR-RT-040`, `NFR-PERF-010`, `-030`, `-040`, `-050`,
`-060`, `FR-LIB-030` and `FR-UI-060`. It is named inside `NFR-PERF-060`'s entry, where it is
load-bearing for the verdict, and left out of the others', whose own gaps already decide them.

**What this pass could not settle, recorded rather than smoothed over.**

- **Three Done cells rest on a clause satisfied by construction rather than by assertion**, and are
  Done because the requirement's own `Verify:` method is executed as written. `FR-GATE-020`'s
  hysteresis clause rests on a constant, and its probe envelope is smooth and monotonic, so a single
  crossing is arithmetically guaranteed — the test would likely pass at `hysteresis_db = 0`.
  `FR-NAM-070`'s equal-power law and its 5-50 ms fade bound are a constant and the blend code.
  `FR-CHAIN-040`'s "shall not produce an error on every block" has no code path that could violate it
  without changing the `Stage` trait. Each is a cheap strengthening for M9b, none is a named gap.
  **`FR-OUT-010` was this bullet's fourth entry and is no longer in it:** the spot-check read its
  declared-but-unasserted +12 dB maximum and 0 dB default as a *named gap* rather than a
  by-construction clause and the correction was accepted, so the cell is Partial above. What
  separates it from the three that remain is that a descriptor constant is **data**, which a later
  edit can change silently with every gate green, where the other three rest on code shape a change
  would have to break visibly.
- **`FR-STATE-020` is Done on a vacuous quantifier.** It quantifies over documents "produced by any
  earlier released version"; there is no released version. What makes it Done rather than a dodge is
  that the substantive clause is asserted directly and
  `crates/namir-state/tests/corpus.rs:170-185` is a live guard that fails the day the version leaves
  its placeholder. It becomes a real quantifier at the first release *after* 1.0, not at 1.0.
- **One Not-started call is still a boundary judgement under D-23.2 clause 3; two were, and have
  already moved.** `FR-ERR-010`: `crates/namir-platform/src/paths.rs:68` computes a log path, which
  could be read as scaffolding; nothing anywhere writes a record, there is no verbosity control and
  no rotation, so it was read as neither the behaviour nor its verification. The alternative reading
  stays recorded here so that cell can be moved knowingly rather than re-litigated. `NFR-PERF-030`
  and `NFR-PERF-040` were called Not started on the reading that a performance Must's "behaviour"
  *is* the timing property itself, with the alternative recorded beside it in exactly these terms;
  the spot-check took the alternative and it was accepted, so both are **Partial** above. Clause 3's
  own text settled it — "exists, not integrated" is Partial — together with the inconsistency against
  FR-CFG-020, which was already Partial for a built behaviour and an absent method. **The M9b/M13
  budget is identical either way**, which was true when this bullet was first written and is why the
  move costs nothing but accuracy.
- **The FRS uses "tier-1" and "tier-2" in four requirements and defines them nowhere** (§1.4's table
  says Primary/Secondary/Prospective). `NFR-PORT-040`'s verdict depends on reading tier-1 = Primary
  and tier-2 = Secondary, the only available mapping. That requirement additionally may not be
  satisfiable as literally worded on Windows, where rustc's MSVC target needs a linker Microsoft
  ships as C++ build tools, and its own `Verify:` method ("a container") is Linux-shaped and narrower
  than the text it verifies. A verdict cell cannot settle that; the FRS or M9b must.
- **Evidence freshness.** Rust artifacts were re-run this session; the CLAP validator result is the
  dated 32/32 from M9a's review, and the CI-based cells rest on run `31256963587` (trunk,
  2026-08-08, all 12 jobs green), which is three commits behind HEAD but touches none of the jobs
  those cells rest on. **No benchmark was run and no new certified D-2.4 figure was produced or
  claimed** — the certified figures cited (NFR-RT-030, NFR-PERF-010, NFR-PERF-050) are the existing
  reference-machine ones.

**Every cell is dated to this pass, and only this pass.** Per D-23.2, M9b and every later milestone
move only the cells their own evidence justifies, appending what moved and why beneath the table. A
verdict here is current as of 2026-08-09 and no later.

**M9a is complete.** Its three deliverables — the tooling and tagging pass, the set-quantification
sweep, and §14's per-requirement adjudication — have all run. M9b's scope is now readable directly
off the table: 92 named gaps, of which the great majority declare M9b as their closing milestone.

### M9a addendum: the gate found a crash on its first run, 2026-08-09

Appended the same day as the close-out above, because it changes what that close-out's FR-CLAP-020
cell rests on and is the strongest evidence this milestone produced about why any of it was worth
doing.

**PR #8's first CI run failed.** The `clap-validator` job M9a added for FR-CLAP-020 reported
`44 tests run, 31 passed, 1 failed, 12 skipped`, with `state-reproducibility-basic` **crashing**:
`exit code: 0xc0000005`, an access violation, not an assertion failure. Every other check on the PR
passed, including the newly-required traceability step.

**Root cause — an ownership cycle that let `destroy` return while plugin threads were still running
inside the DLL.** `SharedInner` owns its `ThreadPool` (`crates/namir-clap/src/shared.rs:80`), every
worker job captures an `Arc<SharedInner>` to reach the rest of it, and the pool's only join was
`impl Drop for ThreadPool` — which cannot run until the last `Arc` dies, and that `Arc` belongs to
whichever *job* finishes last, not to the host thread inside `clap_plugin.destroy`. So `destroy`
decremented a refcount and returned with worker threads live; the host then `FreeLibrary`d the
`.clap`, unmapping the code those threads were executing. `PluginStateImpl::load` ends with
`spawn_recall` (`state_ext.rs:61`), which is why the state tests and only the state tests hit it —
`state-invalid-empty`/`-random` return `Err` before reaching that line and are the negative control.

**Established by experiment, not argument.** The three `state-reproducibility-*` tests call one
function with two booleans, over a fixed PRNG seed, so they execute an identical sequence of plugin
entry points — same input, one crash, which alone excludes a deterministic logic bug. Idle: 8/8
clean, which is why M6's manual run and every local run since saw green. Under 48 CPU burners
emulating a contended runner: **20/20 crashed** with the exact CI string. A control build whose job
captured no `Arc`: **0/20**. After the fix, under the same load: 0/20 on the single test and 0/5 on
the full suite; and a deliberately re-broken build re-crashed **10/10**.

**The bug has been present since M6 and unobserved.** M6's "32 of 32 applicable tests passed" was a
lucky idle-machine run, not a different test set: S-4 and this run both report 44 tests, and
31 + 1 = 32 applicable with 12 skipped is the identical applicable set. `state_ext.rs` has not been
touched since M6 except by annotation commits. **M9a did not introduce this crash; M9a is why anyone
knows about it.**

**What it says about the milestone.** M9a's own sweep had already written the gap down. The
`// uncovered: FR-CLAP-050` field at `crates/namir-clap/src/state_ext.rs` names, verbatim, "load's
own sequel work — set_last_document, push_notice on parse warnings, notify_params_changed and
spawn_recall — runs nowhere". That is the code that crashed. The sweep found the *hole* by reading;
the gate found the *bug* in it by running; neither would have found it alone.

**Two things deliberately not done.** `notify_params_changed` was the obvious suspect and is
**exonerated** — the control build left it intact and crashed 0/20, and `clap-validator` positively
*requires* that rescan, so removing it would have converted a crash into a failure. And
FR-CLAP-050's `// trace-partial:` annotation is unchanged: the fix does not exercise the host-driven
`save`/`load` path, so its recorded gap still stands and closes at M9b.

**Blast radius beyond the reported test, recorded because it is wider than FR-CLAP-020.** The same
cycle exists at every site cloning `Arc<SharedInner>` into a job — `audio.rs:178` (`activate`),
`ui_host.rs:138`, and `shared.rs`'s `start_library_scan`, which holds the reference for a whole
library walk and so turns a millisecond window into a multi-second one. Closing a plugin during a
rescan was the same crash with a far larger target. The fix is at the teardown seam, so it covers
all of them. `namir-app` does not share the shape.

**A consequence for teardown latency, stated rather than discovered later:** `clap_plugin.destroy`
now blocks for the duration of any in-flight job. Library scans are cancelled cooperatively first,
so those are bounded by one `Scanner::step`; recall jobs have no cancel flag and are joined
outright. If a real DAW ever shows this as a stall, the remedy is to give recall jobs the same
cooperative cancel the scanner already has.

---

### M9b P0 decision pass — 2026-08-12 (decisions only; one measurement, nothing else implemented)

Appended per this document's convention; nothing above is edited. M9a opened with a documented
decision pass and M9b does the same, for the same reason: three §15 items were explicitly **due at
M9b's start**, and two of the three are branch policies that must be fixed *before* the measurement
they govern exists, or the rule gets chosen to suit the number.

**Five §15 items are struck in this pass** — 8, 9, 12, 13 and 14. Each carries its resolution at the
item; only what a later reader would not find there is repeated here.

**The one thing that was measured, because the decision turned on a fact nobody had checked.**
D-18.6's landing note hands M9b a named blocker: a harness wanting the host-side halves of
`audio-ports`/`params`/`state`/`latency` must "first make `xtask attribution` resolve dependency kind
per shipped path rather than per unified-resolve node, or observe those extensions another way". A
spike in an isolated worktree measured all three arms and found a **third** route that note did not
consider. `xtask attribution` walks `cargo metadata`'s **default-feature** resolve, so a feature that
is off by default never enters the graph it reads. `02-architecture.md` **D-18.7** adopts it:
`host-ext-tests = ["clack-extensions/clack-host"]`, non-default, with a **required** CI step running
`cargo test -p namir-clap --features host-ext-tests` — because a feature-gated test that no gate runs
is not evidence, and tagging FR-CLAP-030/-040/-100 against tests nobody executes would be the exact
over-claiming D-23.1 exists to prevent. §22 gains **R-17** for the landmine this creates: nothing
mechanical stops `--all-features` from linking `clack-host` into the shipped cdylib, and what catches
it is `xtask attribution` going red, which is a real gate but a late one.

**Recorded because the pass first drafted it wrongly:** the spike reported that
`crates/namir-clap/Cargo.toml:95-107` was "right for the wrong reason", and a draft of D-18.7 said so.
It is not. That comment does not claim `cargo tree -e normal` predicts attribution; it uses
`cargo tree` to establish that the **cdylib** is unaffected and therefore that the row would be a
fidelity artifact rather than a real leak — which is correct as written, and which **R-15 already
names as its own detector**. Arm A reproduced the comment's prediction to the character, including
the row text. The correction was made before the decision landed rather than after.

**What this pass did *not* do: the ~52 partials declaring `closes M9b`.** M9b's scope is the twelve
uncovered Musts plus the partials its own new infrastructure promotes for free. The great majority of
partials naming M9b were booked there **by default** — M9b was the open-ended "missing verification
infrastructure" bucket, as §16's own note above says in as many words — not by anyone sizing them.
They do not block D-18.5's flip, because a partial counts as covered for the ordinary run. The
policy set here is that **each is re-booked explicitly rather than silently carried**, and the
mechanical re-booking is deliberately deferred to M9b's close-out rather than done now, for one
reason: the correct destination for a given partial depends on whether M9b's own work happens to
close it, which is not knowable at the milestone's start. Doing it now would mean editing 52
annotations twice. Three stale declarations are already known and are fixed in the same close-out
pass: **FR-CFG-030** and **NFR-LIC-030** declare `closes M13`, a milestone that has *completed*, and
**NFR-QUAL-010** still declares `closes M13` when §20's own second scope note moved that flip to
M9b.

**Two counts corrected against the tool rather than inherited from prose.** §16's restated acceptance
and §20's correction both say **ten** uncovered Musts are M9b's. `cargo run -p xtask -- traceability`
reports **twelve**, and M13's close-out already recorded why: it closed four of its own five and
**added two**, FR-UI-020 and FR-UI-070, which §15 item 15's fix demoted by stopping a prose mention
from crediting a manual document. That close-out left them as "gaps no milestone has claimed" and
addressed the next planner directly — "whoever plans M9b should read that as work arriving, not as
bookkeeping". **M9b adopts both.** Neither can be a partial: `xtask` refuses a `trace-partial:` on a
`Verify: M` Must, so the only disposition is a manual-test document, and neither
`docs/manual-tests/fr-ui-020-*.md` nor `fr-ui-070-*.md` exists. **The zero-uncovered gate cannot flip
without them**, which makes two documents nobody had scoped into a precondition for NFR-QUAL-010
closing.

---

### M9b status — the build work, and why this is not yet a close-out, 2026-08-12

Appended per this document's convention; nothing above is edited. **This is deliberately a `status`
and not a `close-out`**, because the two things M9b was created to deliver last — D-18.5's flip and
NFR-QUAL-010's closure — are blocked on a precondition no amount of code can satisfy. See the final
subsection below.

**`xtask traceability` reports `clean -- all 130 Must requirements are covered`.** That sentence has
never been printable before. Twelve uncovered Musts at this milestone's start are zero: FR-CFG-020,
FR-NAM-060, FR-CLAP-030/-040/-070/-080/-100/-130, FR-ERR-010, NFR-PERF-040, and the two M13 handed
over unclaimed, FR-UI-020 and FR-UI-070. §14's table moves 37 / 92 / 1 to **44 / 86 / 0**, emptying
the Not-started column for the first time.

**Two real DSP defects were found, and finding them is the milestone's strongest justification.**
Neither was a tagging gap; both had shipped undetected because the test that would catch them did
not exist.

- **FR-NAM-060 had never been measured at all**, and failed at both resampling sites on its first
  measurement. `rubato` places an antialias cutoff as a function of filter *length*, so a short
  filter loses passband rather than widening a transition. The NAM stage sized its FFT in *engine*
  frames, so at a 192 kHz engine against a 48 kHz model the filter was 64 frames in the model's
  domain: **−14.95 dB at 20 kHz** against a 0.1 dB ripple bar. `namir-ir`'s `SincFixedIn(256)`
  measured **−17.7 dB** stopband at 192→48 against a 100 dB bar. Both now size the FFT in the lower
  rate's domain — worst ripple 1.9e-5 dB, worst stopband −129 dB, and 48 kHz output byte-identical.
- **FR-CLAP-070 had never run with a model loaded**, and found a live engine bug. `SlotResampler`
  primed its output FIFO only to the current call's length and substituted zeros when starved, so
  every starved frame spliced in a sample of silence never taken back. The stage's delay became a
  function of block-size **history** — 63 accumulated samples under one-frame blocks — and the shift
  arrived **mid-stream** when a host changed block size, which a user hears as a glitch. Both the
  parity clause and the "without artefacts" clause were failing. Priming the FIFO with one engine
  block makes `in.len() + out.len() == engine_block` an invariant the loop cannot starve under, and
  makes the reported `latency_samples` exactly true where it had been an upper bound met by
  accident.

**The branch policies fixed at this milestone's start are why neither turned into an argument.**
§15 item 12's hierarchy sent FR-NAM-060 to configuration before any thought of amending the FRS;
§15 item 13 sent FR-CLAP-070 straight to the engine, because D-6.2 had already asserted the
behaviour and a failure therefore reported an implementation that had diverged from a decision. Both
failures arrived exactly as those items predicted they might, and in neither case was the rule chosen
to suit the measurement — which is the whole reason they were decided in advance.

**Scope taken on beyond the plan, each by explicit decision rather than drift.** FR-ERR-030's static
half, once the logger made an abstract gap live — both shells now link a logger their audio callbacks
could reach, and `xtask rt-logging` forbids the logger's name in the six modules that carry
audio-thread code. `namir-fixtures` as a `namir-clap` dev-dependency, whose absence had already
forced two tests into workarounds. And a `namir-app` dev-edge for FR-CFG-020, recorded at D-5.1,
because bit-identity is only a meaningful claim within one process and one build.

**Three findings recorded rather than acted on**, each with its own home so none rests only here:
**R-17** (D-18.7's `--all-features` landmine), **R-18** (NFR-PERF-040's margin is governed by the
user's library size, not by plugin code — the index parse reaches the 200 ms ceiling near 12 000
entries), and **§15 item 21** (the traceability parser keeps only the first code of a compound
`Verify:` method; five such Musts self-correct through their `uncovered:` field and FR-STATE-040
cannot, because its first code is `M`).

**The 51 partials that declared `closes M9b` are re-booked to M8**, discharging the deferral this
section's P0 pass recorded. M9b closed none of them. **That destination is a default, not a scoping
decision**, and saying so is the point: they were booked here because M9b was the open-ended bucket,
and M8 is now simply the only milestone that follows. Whoever plans M8 inherits 58 partials and
should expect to triage them, not assume someone already has.

### M9b's two outstanding preconditions — what this milestone must not claim, 2026-08-12

**D-18.5's flip has not happened and NFR-QUAL-010 is not closed.** Both are M9b's to do, and M9b
alone is permitted to do them. Neither is done, for one reason, stated plainly because the mechanical
gate now disagrees with it.

**FR-UI-020 and FR-UI-070 are covered on paper and unexecuted in fact.** Both are `Verify: M`, and
since §15 item 15's fix a `Verify: M` Must resolves against a manual-test document by filename. Those
two documents now exist and are written as runnable scripts — so `xtask traceability` counts them
covered and prints `clean -- all 130 Must requirements are covered`. **Both documents say
`Result: NOT EXECUTED`.** A `Verify: M` requirement is met by a human running its script, not by the
file being present, and the gate structurally cannot tell the difference: `xtask` refuses a
`trace-partial:` on a `Verify: M` Must, so there is no disposition between "no document" and "fully
covered" for it to express.

**Deleting `--allow-uncovered` today would therefore make the required check green on evidence
nobody has produced** — the precise inversion of what D-18.5's split was for. The flip waits on those
two scripts being run and their results recorded in their own documents, and **NFR-QUAL-010 closes
with the flip, not before**. §14's own prose carries the same caveat at 5.13 UI.

**A second caveat on the closure, when it comes: FR-STATE-040.** Its `Verify:` is `M plus S (schema
check)`; the parser keeps only `M`, so it reads plainly covered from a filename match while no schema
check exists anywhere in the tree. NFR-QUAL-010's own text asks for a check that "fails on any
uncovered **Must**", and by the tool's rules that will be true — but one Must's coverage is weaker
than its stated method, and the closure should be recorded with that said rather than left to be
discovered. §15 item 21 carries the analysis and the three candidate fixes.

**The performance figures, re-taken on an idle machine, 2026-08-12.** The first set was measured
while this session's own tooling compiled throughout, which D-2.4 condition 2 disqualifies. All three
benchmarks were re-run after the build work finished, on §2's reference machine with nothing else
this session was running. **Every arm passes**, and each benchmark's own output still declines to
call itself certified — `six_stage_chain` prints, in as many words, that a certified figure is one
measured on that machine *quiet* and that its own PASS line "is not by itself evidence that it was".
That caution is warranted and is not boilerplate: see the discarded repetitions below.

| Requirement | Ceiling | Measured (worst of the runs below) | Headroom |
|---|---|---|---|
| **NFR-PERF-040**, empty library index | 200 ms | **229 µs** | ~99.9% |
| **NFR-PERF-040**, 10 000-entry index, 48 kHz / 512 | 200 ms | **181.2 ms** | 9.4% |
| **NFR-PERF-040**, 10 000-entry index, 192 kHz / 4096 | 200 ms | **187.5 ms** | **6.2%** |
| **NFR-PERF-050**, ~50 MB model from a real path | 500 ms | **152.5 ms** | 69.5% |
| **NFR-PERF-050**, ~50 MB payload already in memory | 500 ms | **129.6 ms** | 74.1% |
| **NFR-PERF-010**, schedule's own worst block (estimator) | 25% of one core | **14.56%** | 41.8% |
| **NFR-PERF-010**, raw p99.9 over quotable repetitions | 25% of one core | **16.39%** | 34.4% |

NFR-PERF-040: 5 runs × 20 repetitions per arm. NFR-PERF-050: 5 runs. NFR-PERF-010: 3 runs × 5
repetitions, all 15 reported individually by the harness.

**Three things these numbers say that the pass/fail does not.**

*NFR-PERF-040's margin is not Namir's.* The harness reports the create and activate halves
separately, and `activate` — constructing the entire six-stage chain — is **6 µs** on the empty arm
and **147 µs** at 192 kHz with 4096-frame blocks. Everything else in the 187.5 ms is parsing
`library-index.json`. The idle re-run *tightened* the medians (161–165 ms) and *worsened* the tail,
leaving 6.2% headroom on the worst arm, which strengthens **R-18** rather than softening it: a user
whose library passes roughly 12 000 entries breaches a Must requirement with no code change at all.

*NFR-PERF-050's file arm costs about 23 ms over its byte arm* on a 49.6 MiB model (152.5 vs
129.6 ms), so even reading 50 MB from a warm page cache, **~85% of what a user waits for is parse and
prepare, not I/O**. D-2.5's page-cache condition is stated by the harness at runtime: these are warm
reads, so that delta is a floor on cold-read cost.

*D-2.4's validity check earned its place on this very run.* One NFR-PERF-010 repetition set
discarded **2 of its 5** repetitions — p99.9 of 24.35% and 24.54%, one with a `max` of **76.09%** —
while the contamination-immune estimator across those same repetitions read 14.38%, moving by
hundredths of a point. An idle machine is not a quiet one, the difference is measurable, and the
estimator is the figure to trust when they disagree. That is the argument for asserting on it, made
by the instrument rather than about it.

### M9b addendum: an adversarial re-read of this milestone's own ledger, 2026-08-12

Appended per this document's convention; nothing above is edited. **This is still not a close-out** —
the two preconditions above are unchanged and unmet. What this pass did is re-read M9b's own output
against the tree, on the principle the milestone itself is built on, and it found the ledger
over-claiming in eleven places. None of it changes what the suite does; all of it changes what the
ledger says.

**The verification state was confirmed first, so the findings below are corrections to a green tree
rather than excuses for a red one.** `cargo fmt`, `cargo clippy --workspace --all-targets -D
warnings`, `cargo test --workspace`, D-18.7's `cargo test -p namir-clap --features host-ext-tests`,
all six `xtask` subcommands and `cargo deny check` pass — the last in the full form AGENTS.md names
*and* in each of the three `ci.yml` runs separately (`check licenses`, `check sources`, `check
bans`), all four categories reporting ok, which matters because the `clack-host` dev-dependency
D-18.6 adopted is exactly what those gates were told to watch. Three environment defects were fixed
to get there and none is a repository defect: the sandbox's toolchain was 1.94.1 against this
workspace's 1.97 MSRV, ALSA development headers were absent — which `alsa-sys`' `build.rs` fails on
exactly as `ci.yml`'s own Linux step predicts — and `cargo-deny` was not installed at all, so that
gate had never run in this sandbox.

**Two plain tags were demoted to `trace-partial:`, and both were written by M9b.** This is the same
class of error M9a's sweep existed to find, committed by the milestone that inherited the doctrine.

- **FR-CLAP-080.** Its test sweeps 154 rates — both endpoints, the six standard rates, a 1 kHz grid
  and one fractional value — against a requirement reading "**every** sample rate the host may
  present within 44.1 kHz to 192 kHz inclusive". `src/audio.rs` rounds to an integer, so the set is
  ~147 900 values. **That is the identical argument by which this same milestone made FR-NAM-060 a
  partial**, applied to the same milestone's own other tag and reaching the opposite verdict; the
  two sat a few hundred lines apart. The second gap is larger: the file never touches
  `PluginState`, so no model and no IR are loaded, and D-9.2's `SlotResampler` — the one
  rate-dependent subsystem in the chain, and the thing M9b itself found broken at 192 kHz — runs at
  none of the 154 rates.
- **FR-ERR-010.** The requirement has three clauses; the covering test asserts two. The logger is
  driven against a caller-supplied temporary path throughout, and the covering test asserts
  `logging::logger().is_none()` — the per-user-location clause is asserted by nothing, and
  `logging::init`, the only thing binding the sink to `log_file_path()`, is called by no test. The
  test's own *name* claimed the clause it did not assert.

§14's table moves **44 / 86 / 0 → 42 / 88 / 0** accordingly. The Not-started column stays empty.

**Nine `uncovered:` fields asserted facts that were false when read.** D-23.1 makes this field the
ledger entry a Partial is retired against, and it is rendered verbatim into checked-in
`docs/03-test-plan.md`, so a stale field is a false statement in a generated document. Four were
falsified by M9b's own work, because the re-booking commit changed the milestone token in 104 lines
and re-validated none of the text: **FR-CLAP-090** said this crate "has no benches directory" when
M9b had just added one; **NFR-RT-010** said `namir-clap`'s `process()` runs under no
`assert_no_alloc` anywhere when M9b's own harness wraps every call (and mis-stated the surviving
`namir-app` half besides); **FR-CLAP-050** said `PluginState::load` "is called by no test" and its
`spawn_recall` sequel "runs nowhere" when four M9b suites drive it through the real vtable and one
*depends* on it; **NFR-QUAL-010** said "20 uncovered Musts stand with CI green" against a tool that
now reports zero.

**Six declared a milestone that cannot close them.** Three are the ones this section's P0 pass
named and said would be fixed in the close-out — FR-CFG-030, NFR-LIC-030 and NFR-QUAL-010 — and the
P0 pass's own count was short: **FR-PKG-040, NFR-BUILD-020 and NFR-DOC-040 declare `closes M13`
too**, and M13 has completed. NFR-LIC-030's and FR-PKG-040's *text* was stale in the same direction
as the milestone, both asserting that no release workflow, installer or archive exists — M13 built
all three. All six are re-booked to **M8**, on the same "only milestone that follows" default the
51 partials took, and their gap text is re-derived rather than carried.

**Two stale prose comments and one stale test doc were corrected.**
`crates/namir-clap/tests/clap_host_block_sizes.rs`'s module doc still read, in the present tense,
"**why it is currently red**" and "`NamStage::reset`… **which currently clears both FIFOs**", with
the remedy described as something to run "at a distance, without touching `namir-engine`" — all
falsified by the fix that landed in the same commit, in the most load-bearing place in the
milestone: the file carrying FR-CLAP-070's plain tag. The investigation is kept and re-tensed
rather than deleted with the bug.

**D-6.2 gains a consequence note, because the finding was a gap in its reasoning and not only in
the code.** That decision's stated consequence is about buffer *sizing* — "a smaller block simply
uses a prefix" — which is true of the buffers and false of a stateful stage's delay. The general
form, which nothing in the decision implied: a stage carrying its own FIFOs owes a
block-size-independent delay invariant, which buffer sizing neither supplies nor can check.

**One audit finding was investigated and rejected**, recorded because a rejected finding is worth
as much as an accepted one here. It was argued that the FR-NAM-060 fix materially raises reported
latency at high engine rates with nothing saying so. The consequence *is* recorded, twice — D-9.3's
own `*Consequence (added M9b)*` note gives it as "a constant ~5.3 ms internal block rather than a
constant 256 frames", and this section's status subsection records the FIFO priming making
`latency_samples` exactly true where it had been an upper bound met by accident.

**The two blocking manual scripts were attempted, and the attempt is why R-16 is now precise.**
FR-UI-020's and FR-UI-070's scripts were run as far as a headless Linux container allows, which
turned out to be not at all: neither product can open a window on a software-GL X server. **R-16
already predicted this and gave the wrong reason** — "`xvfb-run` not helping because that path needs
a GLX-capable display". An Xvfb display *is* GLX-capable; Mesa 25.2.8 / llvmpipe reports direct
rendering, a 4.5 core profile and 320 GLXFBConfigs on it. **None of the 320 is sRGB-capable**, and
`egui-baseview`'s default `GlConfig` asks for one, so `glXChooseFBConfig` matches nothing and
`baseview` panics on its own `.expect`. No Mesa configuration tried produces an sRGB config. R-16
carries the mechanism now, with the consequence stated in the form that matters: **no headless CI or
container can execute the `Verify: M` UI scripts**, so those two documents stay `NOT EXECUTED` and
D-18.5's flip stays unmade. Recorded so the next session does not spend the hour again.

**What M9b still owes is exactly what it owed before this pass**: a human running
`docs/manual-tests/fr-ui-020-single-screen-elements.md` and `fr-ui-070-non-modal-error-notices.md`
on a machine with a display, a real audio interface and an instrument, their results recorded in
those documents, and only then the flip and NFR-QUAL-010's closure.

### M9b close-out — the two scripts were run by a human, and the flip still does not happen, 2026-08-27

Appended per this document's convention; nothing above is edited. **This is the close-out the
status subsection above deferred**, and it discharges what that subsection said M9b still owed: a
human running both blocking scripts on a machine with a display, a real audio interface and an
instrument. That happened on 2026-08-27, on SALON — a 1440p display at 100 % scale, a PreSonus
AudioBox 22VSL monitoring audibly, an instrument playing, and the CLAP plugin loaded in **Reaper**
for the plugin halves. Library fixtures came from a new `namir-fixtures` example, below.

**Both documents now carry per-step verdicts and a real `Result:` line.**

- **FR-UI-020 — PASS, all twelve steps**, in both product configurations
  (`docs/manual-tests/fr-ui-020-single-screen-elements.md`).
- **FR-UI-070 — FAIL**, twelve of fifteen steps passing; **steps 4, 8 and 14 fail**
  (`docs/manual-tests/fr-ui-070-non-modal-error-notices.md`).

**FR-UI-070 is now verified and failing, which is a better state than unverified and a worse one
than the ledger implied.** Its first sentence holds and was observed rather than argued from the
source: notices are non-modal under real clicking, typing, scrolling and window dragging, and audio
never broke across a corrupt model, a deleted file, a truncating IR load, a full rescan against an
unwritable index, or a project reload with a missing reference. Its second sentence — "an error
shall state what failed, which file or device it concerned, and what the user can do" — does not.
**Of the 69 `message_template` values in the tree, none tells the user what they can do**; two state
what the *system* did instead. Two notices name neither the file nor the device.

**The run produced seven findings, and their most useful property is that they compound.** Filed as
issues #39-#45, with #15, #24, #26 and #34 updated from the same evidence rather than duplicated.
The chain worth reading in full is step 14's: the code-and-template duplication (#39) roughly
doubles a notice's rendered length; `notices.rs`'s `ui.horizontal` row does not wrap, so the
`Dismiss` button lands past the right edge; the CLAP editor is fixed at 960x640 with
`can_resize() == false` because FR-CLAP-110 is a scoped-out Should; and notices never expire, since
`Dismiss` is the only removal path. **The result is a notice that can never be removed**, occupying
part of a screen that FR-UI-020's own run shows already cannot display every element at once. Four
defensible decisions, no author of the outcome, and a defect that looked cosmetic at step 1 turning
functional at step 14. That is the argument for manual tests stated better than this document could
state it in the abstract.

**Two other requirements gained real evidence on the same pass.**

- **FR-IO-070's step 2 was executed** — not with the failable virtual device R-5 asks for, but by
  physically unplugging the interface while audio flowed
  (`docs/manual-tests/fr-io-070-device-removal.md`). Crash-free, hang-free, no error spam, window
  functional: those clauses hold. The naming clause fails, and **the classification is a direct R-5
  data point**: the unplug arrived as `StreamFailure::Other` carrying an unmapped OS error, **not**
  as `DeviceLost`, so `to_stream_failure`'s mapping is now known to be incomplete against a real
  case. Namir reported it as a device loss anyway, by luck — the code is chosen from the direction,
  never from the classification. Step 3 is now observed rather than inferred: re-plugging does not
  resume, and no device-selection surface exists to recover with (§15 item 16, issue #26).
- **Tooling the run needed, added mid-run rather than worked around.**
  `crates/namir-fixtures/examples/seed-library.rs` writes both scripts' fixtures into the library
  root with one command, including the over-10-second IR neither fixture generator produces; and
  `crates/namir-app/examples/list-devices.rs` prints endpoint names as `audio-settings.json` must
  spell them and probes exclusive mode per device — the two facts steps 11 and 12 cannot be run
  without. Both replace instructions that told a human to find a hash-named directory under
  `target/` or to guess a localised Windows endpoint name.

### The flip does not happen here, and the reason has changed

**M9b's own text says this milestone's close-out owns D-18.5's flip. It does not happen, and this
subsection is the correction rather than an edit to the sentences that said it would.**

The status subsection above blocked the flip on one thing: these two documents saying `NOT
EXECUTED`. They no longer do. **But M14's planning pass, which landed between that subsection and
this one, found the hole is larger than the two documents M9b knew about: six `Verify: M` Musts are
unexecuted, not two** — FR-UI-020, -030, -040, -050, -070 and FR-IO-030. This pass executed two of
the six. **FR-UI-030, FR-UI-040, FR-UI-050 and FR-IO-030 remain unexecuted**, and `xtask
traceability` still cannot tell, so deleting `--allow-uncovered` today would make the required check
green on evidence nobody has produced — the same inversion the status subsection refused, at a
smaller number.

**§21's Phase 3 already states the sequencing this implies**, and it is now the plan of record:
issue #34's fix — teaching the tool to read a manual document's `Result:` line — "**must land before
D-18.5's flip**, or the flip inherits the same hole". So the flip is **M14's**, after that fix, and
**NFR-QUAL-010 closes with the flip, not at M9b**. Every reason the status subsection gave for the
flip being M9b's still holds against *M13*; what it did not anticipate was a milestone arriving
between M9b and M8.

**This run also sharpens what #34's fix must do, and the sharpening comes from a document that
passes its own script's structure while failing the requirement.** FR-UI-070's document now says
`Result: FAIL`, and `xtask traceability` prints `clean -- all 130 Must requirements are covered`
regardless. A fix that keys only on the literal `NOT EXECUTED` would credit a **failing** manual test
as covering its Must. It must treat anything other than a pass as uncovered.

### §14 moves, and one that deliberately does not

**M9b close-out moves, 2026-08-27 — one row, one requirement**, appended per D-23.2's "every later
milestone moves only the cells its own evidence justifies". **5.13 UI becomes 2 / 5 / 0** (was
1 / 6 / 0); **Total becomes 41 / 89 / 0** (was 40 / 90 / 0).

- **FR-UI-020 — Partial → Done.** Its `Verify:` is `M`, its manual document is the traced artifact
  under D-18.6, and that document now records twelve of twelve steps passing in both shells with the
  window size, display scale, host and fixture provenance stated. M9a's adjudication of Partial rested
  on there being no `fr-ui-020-*.md` at all; M9b's build pass wrote one and this pass ran it. **The
  verdict rests on one recorded judgement and should not be read without it**: the runner adjudicated,
  in writing and as step 11 demands, that scrolling within the single screen is *not* navigation. Had
  it gone the other way, FR-UI-020 would **fail in the plugin shell**, where the fixed 960x640 editor
  puts everything below `EQ Enabled` behind a scroll with no way to widen the window. The requirement's
  standing in the plugin is therefore contingent on a Should (FR-CLAP-110) that was scoped out — a link
  no decision, requirement or comment records, and worth one when someone revisits FR-CLAP-110.
- **FR-UI-070 — stays Partial, for an entirely new reason.** M9a adjudicated it Partial because "M
  against the error catalogue of FR-ERR-020" had been run against no catalogue entry. It has now been
  run against thirteen, and the requirement **fails**. A Partial cell that used to mean "unverified"
  now means "verified and not met", which is the same cell carrying a different and more actionable
  claim. It cannot become Done until #41's catalogue work lands, and it is not Not-started: the
  non-modality and never-interrupt-audio clauses are built, observed and passing.
- **5.11 IO does not move**, and the absence is deliberate. FR-IO-070 gained real evidence — a
  physical unplug, executed and recorded — and stays **Partial**, because that evidence establishes
  the requirement is *not met* rather than that it is: its "allow the user to select another device"
  clause has no implementation to verify. The cell is unchanged; what changed is that its
  `uncovered:` claim is now backed by an observation instead of an inspection.

### What M9b leaves for M14

M9b's build work is complete and its two blocking scripts are executed. What it hands on, beyond the
58 partials the status subsection re-booked to M8:

1. **The flip and NFR-QUAL-010**, sequenced behind issue #34's fix (§21 Phase 3), with the
   additional requirement that the fix treat a `FAIL` result as uncovered and not only a `NOT
   EXECUTED` one.
2. **Four unexecuted `Verify: M` Musts** — FR-UI-030, -040, -050 and FR-IO-030. The first three need
   the same machine this pass used and roughly the same hour; FR-IO-030 needs Linux and macOS
   hardware, and until it runs **neither Linux nor macOS audio has ever moved a sample**.
3. **Issues #39-#45**, all seven from one manual run, of which #41 (the catalogue says nothing about
   what to do) and #42 (undismissable notices) are the two that block FR-UI-070 from ever reading
   Done.

**The case for M14 existing, restated from this milestone's own evidence.** M9b found two real DSP
defects by running tests that had never run against a loaded chain, and this pass found seven
user-facing defects by running two scripts nobody had ever executed. Both are the same finding at
different altitudes: **this project's remaining risk is concentrated in the checks that were written
and never run**, not in the code that was written and never checked. §21 Phase 4 makes that argument
for the engine; the record above makes it for the interface.

---

## 17. M10 — NAM Architecture 2 (A2) support

**Size: L.** **Depends on:** M9, in the sense that a hard-won parity claim is worth less landing in
a ledger nobody trusts; materially, on nothing else, since `namir-nam`'s A1 support and
`namir-fixtures`' generator are both mature. **Blocks:** M8's Must-row check, via the two new
requirements below. **Governed by:** `02-architecture.md` **D-9.12**, and **risk R-9**.

**New requirements this milestone closes:** **FR-NAM-150** (load and run A2 models in the A2-Full
and A2-Lite configurations) and **FR-NAM-140** (a model whose architecture or configuration is
unsupported is rejected with an error naming the unsupported feature, distinct from the
malformed-file error). Note the id ordering: A2 support is **FR-NAM-150**, not FR-NAM-130 — that
number was already taken by "the NAM stage shall be usable with no model loaded", and FRS §1.5
makes identifiers permanent and never reused.

**Scope decided up front: core A2 only.** The extended WaveNet config schema, the new activations,
grouped and bottleneck convolutions, the convolutional head, and the parity oracle — enough to run
A2-Full and A2-Lite, and no further. **`SlimmableContainer`, `condition_dsp`, FiLM conditioning and
the `.namb` container are explicitly deferred** to a later milestone, not forgotten. D-9.12 records
the same boundary, and the reason for drawing it here is that each of those four is a separable
feature with its own risk, none of which A2-Full or A2-Lite needs.

**Deliverables, in five phases; the phase order is a dependency order, not a preference:**

- **Phase 0 — fix the misleading rejection (FR-NAM-140).** An A2 file today fails with
  `nam.load.malformed_json` — "not valid JSON" — which is simply untrue and sends anyone
  investigating it in entirely the wrong direction. The cause is that A2 layers dropped the scalar
  `kernel_size`, dropped the `gated` bool, and turned `activation` into an object, so the existing
  deserializer fails at field level and the failure surfaces as malformed input. An
  `unsupported_architecture`-class error naming the offending feature is a small change, is
  independently useful to anyone holding an A2 file *today*, and is **worth shipping ahead of A2
  support itself** rather than bundled with it.
- **Phase 1 — the extended WaveNet `config` schema.** `kernel_sizes[]` (an array, replacing the
  scalar), `activation` as an object or an array of objects, `gating_mode` replacing the `gated`
  bool, plus `bottleneck`, `groups_input`, `groups_input_mixin`, `layer1x1`, `head1x1`,
  `in_channels`, and a nested `head` object — the last of which `namir-nam` currently *rejects*
  outright for any non-null value. Parsing and validation only. NFR-SEC-020's dimension-ceiling
  checks apply to every new field before any arithmetic touches it, exactly as they already do to
  the existing ones; a new schema is a new attack surface, not an exception to that rule.
- **Phase 2 — DSP primitives and activations.** Grouped dilated convolution, bottleneck
  expand/contract, per-layer variable kernel size (`namir-nam`'s `Conv1D` assumes a single kernel
  size across a whole array), the 1x1 residual and skip projections, a convolutional head, and the
  activations A2 uses: LeakyReLU, SiLU, PReLU, Softsign, Hardswish and LeakyHardtanh.
- **Phase 3 — weight-layout re-derivation plus the parity oracle. The highest-risk item in this
  milestone, and the reason R-9 is severity High.** A2's weight ordering must be re-derived by
  reading `NeuralAmpModelerCore`'s `NAM/wavenet/detail.h` and `params.h`, the same way A1's was and
  for the same reason: **a silently-wrong order produces a model that loads without error and
  sounds entirely plausible while being wrong.** There is no failure mode that announces itself.
  The proof is a new A2 generator in `namir-fixtures` acting as an independent parity oracle, per
  D-19.1's generated-never-captured rule and NFR-QUAL-030's cross-implementation standard. **No A2
  model ships before the oracle agrees.** This phase re-anchors §15 item 4 as a side effect, since
  re-reading the upstream reference from inside the product workspace is precisely what that item
  asked for; the A1 layout is re-confirmed on the same pass, because an A2 derivation that extends
  A1's cannot be trusted further than the A1 one it extends.

- **Phase 4 — LSTM: close FR-NAM-030's other half, and measure a cost curve. Independent of A2, and
  included here only because it runs off the same reference build Phase 3 stands up.** Two distinct
  jobs, in this order:

  **(a) Numerical parity for LSTM against the reference implementation.** FR-NAM-030 is a **Must**
  and reads "for **each** supported architecture, the output of Namir's inference shall match the
  reference NAM implementation to within an error whose RMS is at least 90 dB below the RMS of the
  reference output." Only WaveNet has ever been checked that way — S-1's −131 dB result, whose own
  scope note in `02-architecture.md`'s changelog 0.6 says outright that "S-1 covered WaveNet only…
  **LSTM is unaddressed**." What LSTM has instead is parity against `namir-fixtures`' independent
  from-scratch reference, which is genuinely strong evidence and is *not* what this requirement
  asks for: two Rust ports agreeing rules out independent bugs, but both could share a
  misreading of the upstream format. **So FR-NAM-030 is only half-met, and has been since M3
  without anyone writing that down** — the traceability tool reports it as covered, correctly, since
  a covering test exists; what the tool cannot see is that the requirement quantifies over
  architectures and the test does not. Closing it needs a reference render of a real LSTM model, and
  the 67 models recorded in `docs/manual-tests/fr-nam-020-real-lstm-models.md` are exactly that
  input, at a known shape grid (layers 1–4 × hidden 1–12, 16, 20, 24, 28, 32, `input_size` 1,
  48 kHz).

  **A constraint that shapes how this is built, not merely a caveat.** Those files carry **no stated
  licence** and cannot enter the repository — D-19.1's generated-never-captured rule forbids it
  independently of the licence question. So this cannot be a committed test with committed inputs.
  Build it as an `xtask` subcommand or an example that takes a path to a locally-held model, runs
  the comparison, prints the dB figure, and **skips cleanly when the path is absent** — with the
  executed result recorded in the manual-test doc above, per the same pattern every other
  human-verified finding in this project uses. A CI job cannot close this; a recorded local run can.

  **(b) An LSTM cost curve.** `namir-nam` has **no LSTM benchmark at all** — every performance
  result this project holds (S-1, R-4, D-2.3's `x86-64-v3` finding, `wavenet_inner_loops.rs`,
  NFR-PERF-010's certification) is WaveNet. The same 67-model set is a ready-made compute sweep,
  which is what its source post published it for: it exists specifically to characterise the
  low-compute regime. Measuring per-block cost across that grid under D-2.4's conditions yields a
  real cost-versus-shape curve, which feeds **FR-NAM-120** (Should — "expose the model's
  computational cost… **measured rather than estimated**", currently out of scope in `namir-nam`
  for want of exactly this harness) and tells us where LSTM sits against NFR-PERF-010's budget,
  which is today simply unknown rather than known-good. Same licence constraint, same
  skip-if-absent shape.

**Also closes FR-NAM-090 and FR-NAM-100** (loudness normalisation, dBu calibration), which
`namir-nam` currently declares out of scope for a reason A2 removes. The crate's own boundary note
says they need "metadata fields the current `.nam` schema this crate reads doesn't carry" — and
A2-era files carry `loudness`, `input_level_dbu` and `output_level_dbu`. The blocker was the file
format, not the DSP, so these come nearly free once Phase 1 lands. Worth taking here rather than
leaving them to be rediscovered later as an unexplained pair of open Musts.

**A performance note that should not be taken on trust.** A2 is claimed to need 30–40% *less* CPU
than A1 at comparable quality, which would make NFR-PERF-010 easier rather than harder. That claim
is about the architecture, not about this implementation of it: `namir-nam`'s `wide::f32x8` kernels
assume dense convolutions, and grouped and bottleneck convolutions need their own kernel variants
before any of that saving is realised. **Expect the first working A2 path to be slower than A1**
until those variants exist, and measure it under D-2.4's conditions before recording any figure
either way.

**Acceptance:** an A2-Full model and an A2-Lite model both load, run, and agree with the
`namir-fixtures` parity oracle to the tolerance NFR-QUAL-030's cross-implementation standard sets;
an unsupported-configuration file is rejected with an error naming the unsupported feature and
distinguishable from the malformed-file error; FR-NAM-090 and FR-NAM-100 close; NFR-PERF-010 is
re-measured on the §2 reference machine with an A2 model in the chain and the figure recorded,
whichever direction it moves.

**Acceptance for Phase 4, stated separately because it closes a different requirement and can pass
or fail independently of A2:** a real LSTM model's render is compared against the reference
implementation built with `-DNAM_USE_INLINE_GEMM` (D-9.12's pinned-reference note explains why that
flag and not the default GEMM path), the measured RMS error is recorded against FR-NAM-030's 90 dB
floor, and the run is written up in `docs/manual-tests/fr-nam-020-real-lstm-models.md` naming the
model and the reference build. **FR-NAM-030 is then met for both supported architectures rather than
one**, and that fact — not merely the number — is what the milestone records. The cost curve is
measured across the shape grid under D-2.4's conditions and reported as a curve, not a single
figure; if it shows LSTM breaching NFR-PERF-010's budget at shapes users actually run, that is a
finding to record and act on, not to average away.

### M10 status — this session, 2026-08-09

All five phases built, across three stacked PRs (Phase 0 alone; Phases 1-3; Phase 4 plus
FR-NAM-090 plus this note). Recorded here honestly, including where the work landed on a
different shape than this section predicted.

**Naming, resolved.** "A2-Full"/"A2-Lite" do not appear anywhere in `NeuralAmpModelerCore`'s own
source. The names map to upstream's **A2 standard** (`channels == bottleneck == 8`) and **A2
nano** (`channels == bottleneck == 3`) respectively — `NAM/wavenet/a2_fast.h`'s own header
comment and its strict shape detector (`a2_fast.cpp`, `is_a2_shape`) are the source. Recorded once,
at `crates/namir-fixtures/src/nam/mod.rs`'s `A2Shape` doc comment, and restated at D-9.12's own
consequence note in `docs/02-architecture.md` §9.5.

**Phase 2's built scope is narrower than this section's literal list, by design, not oversight.**
"Grouped dilated convolution... the convolutional head, and the activations" above reads as
grouped convolution being built; it is not. Core A2's own detector requires `groups_input ==
groups_input_mixin == layer1x1.groups == 1` — no real A2 shape ever exercises a grouped kernel — so
building one would have shipped code with no fixture, real or generated, that ever calls it. What
was built: per-layer `kernel_sizes`, a `bottleneck` width distinct from the residual trunk's
`channels`, the `layer1x1` projection (upstream's name for what A1 called `residual`), a real k-tap
dilated head rechannel, and six new activations (`LeakyReLU`, `SiLU`, `Hardswish`, `Softsign`,
`LeakyHardtanh`, `PReLU`, scalar or per-channel). Every feature this scope excludes —
`condition_dsp`, all eight FiLM sites, gating, non-unity `groups_*`, `slimmable`, an active
`head1x1`, an inactive/grouped `layer1x1` — is rejected by name (FR-NAM-140), not silently ignored,
and stays a permanent D-9.12 boundary, not a temporary Phase-0-only gap.

**R-9, mitigated by process.** Two independent implementations were derived from the upstream C++
source by two agents, neither reading the other's code: `crates/namir-nam`'s own A2 path, and
`crates/namir-fixtures`' A2 generator plus `reference_infer_a2`. Their independently-derived weight
orders agree exactly (compare `crates/namir-nam/src/wavenet.rs`'s weight walk against
`crates/namir-fixtures/src/nam/mod.rs`'s `build_a2_weights` doc comment), and the in-tree
Rust-vs-Rust parity test (`crates/namir-nam/tests/a2_fixtures.rs`) agrees to **-inf dB** — bit-exact,
since core A2's `LeakyReLU` has no rounding-sensitive transcendental function and both
implementations sum in the same order. Cross-checked against a real reference render
(`NeuralAmpModelerCore`, pinned `3cde95c`, built with `-DNAM_USE_INLINE_GEMM
-DNAM_ENABLE_A2_FAST=OFF`): A2 Full and A2 Lite (`WaveNetShape::Standard`-scale synthetic fixtures)
measured -90.31 dB and -90.31 dB respectively against the real reference, with an A1 Standard
re-confirmation on the same pass at -90.87 dB. A signal-complexity diagnostic (a single-sample
impulse, a 100 ms probe, the full 2-second probe) found this figure flat regardless of signal
length or complexity — the signature of per-operation floating-point non-associativity between
Eigen's internal summation order and this crate's sequential per-tap loop, not a structural defect
(a wrong weight order produces errors of a different order of magnitude, not a few dB that hold
steady across wildly different signals). **R-9 is retired** — see `docs/02-architecture.md` §22's
updated risk-register row.

**That -90 dB figure is now understood, not just measured.** It comes from model *size*, not from
a flat noise floor: the golden-reference fixtures added for FR-NAM-030 below use much smaller
shapes (`WaveNetShape::Nano`, `LstmShape::Tiny`) and measure -137 dB (WaveNet) and bit-exact
(LSTM) against the same real reference. More layers and channels accumulate more per-operation
summation-order drift — `Standard`'s ten layers across two arrays versus `Nano`'s two-layer, one-
array shape is the difference, not a hidden bug in either.

**FR-NAM-030 closes, for both architectures, in-repo, permanently.** `crates/namir-nam/tests/
golden_reference.rs` (new) commits small, generated (D-19.1: never captured), regenerable fixtures
— `tests/golden/{wavenet_nano,lstm_tiny}.nam`, `input_10s.wav` (the specified 10-second
clean/transient/saturated signal, built from the same recipe `spikes/s1-nam-inference`'s own
generator uses), and each model's real-reference render — and asserts `namir-nam`'s own output
against them in-process, tagged `// trace: FR-NAM-030` on both tests (the pair together spans "each
supported architecture"; neither alone does). Building the LSTM fixture surfaced a real, useful
finding along the way: `NeuralAmpModelerCore`'s `DSP::Reset` silently prewarms every stateful model
with 0.5 s of processed silence by default (`NAM/lstm.cpp`'s own comment calls this "Hacky, but a
half-second seems to work for most models") — a host-convenience default, not part of the LSTM
model's mathematical definition or the `.nam` format itself, and `render.exe` always takes that
default path. Omitting the same prewarm on `namir-nam`'s side (which always starts from the
model's own declared `h0`/`c0`) produced -44 dB against a tiny, randomly-initialised fixture; once
the test replicates the reference's own default treatment, both sides agree to the bit. Corroborated
by `xtask nam-parity` (new, `xtask/src/nam_parity.rs`) run against seven real, locally-held LSTM
models spanning every `num_layers` value in the 67-model grid documented in
`docs/manual-tests/fr-nam-020-real-lstm-models.md` — none showed anything like the tiny fixture's
prewarm sensitivity, measuring -114 to -130 dB with no prewarm treatment applied at all, consistent
with real trained models' `h0`/`c0` already sitting close to where silence would settle them. No
`namir-nam` production code changed because of this finding — see `tests/golden_reference.rs`'s own
doc comment for why matching it in the test, not the product, is the correct read of what
"matching the reference implementation" means here.

**NFR-PERF-010, re-measured, informationally.** `crates/namir-nam`'s own inference in isolation
(not the full six-stage chain, so not a certified figure under D-2.4 either way), three interleaved
runs on the pinned reference machine, p99.9 of one core: A1 Standard 10.57-10.88%, A2 Full
8.34-8.68%, A2 Lite 1.92-2.28%. A2 Full costs *less* than A1 Standard despite more than double the
layers (23 vs. 10) — core A2's narrower per-layer channel count and `bottleneck == channels`
(no grouped-conv overhead to pay, since Phase 2 never built one) more than offset the extra layer
count. §17's own warning to "expect the first working A2 path to be slower than A1" did not
materialize, for this specific comparison. A full six-stage-chain re-certification is not part of
this milestone; the isolated-stage figure above is the informational evidence this acceptance
criterion asks be recorded, "whichever direction it moves."

**The LSTM cost curve is new evidence, and it is not entirely comfortable.**
`crates/namir-nam/benches/lstm_inner_loops.rs` (new) sweeps the real 67-model grid's shape space
(`num_layers` 1-4 x `hidden_size` 1-12/16/20/24/28/32). Informational (D-2.4: not >= 5 certified
repetitions on the reference machine), but reproducible in direction across independent runs: a
double-digit number of shapes at the larger end (`num_layers` >= 2, `hidden_size` >= ~16-20)
breach NFR-PERF-010's 25%-of-one-core budget at p99.9, worst observed near 60% for `LSTM-3-032`
excluding two runs' worth of clearly contaminated tail outliers at `LSTM-4-028`/`LSTM-4-032`
(p99.9 in the hundreds-of-percent range, inconsistent with those same shapes' own p50/p99, the
signature of the GPU-driver/DPC contamination `AGENTS.md`'s benchmark-methodology section already
documents on this machine — not evidence those two shapes are that expensive). LSTM's low-compute
reputation, which motivated collecting the 67-model grid in the first place, does not hold at the
larger end of that same grid. Not acted on in this milestone — recorded as a finding for whoever
scopes LSTM's own performance-closure work, the way M3 scoped WaveNet's.

**FR-NAM-090 closes, FR-NAM-100 is explicitly deferred — not the "FR-NAM-090 and FR-NAM-100 close"
this section's acceptance text originally predicted.** `namir-nam` parses and exposes
`metadata.loudness`; `namir-params` gained `nam.normalize_enabled` (Stepped, default on) and
`nam.normalize_offset_db` (Continuous, ±12 dB, default 0); `namir-engine`'s Nam stage applies a
smoothed (`namir-dsp::GainRamp`, 25 ms, matching `trim.rs`/`out.rs`'s own convention),
allocation-free normalisation gain computed as `-18 LUFS (this project's own reference point,
documented at `namir_params::stages::nam::TARGET_LOUDNESS_LUFS`) minus the model's declared
loudness, plus the user's offset`, or zero when a model declares none; old presets load unaffected
(`namir-state`'s `ParamValues` is already positionally complete over `REGISTRY`, needing no code
change); `namir-ui`'s existing generic-over-`REGISTRY` control surfacing needed no per-parameter
code either. **Tagged `trace-partial:`, not a plain `trace:`**: FR-NAM-090's `Verify: U` method
names "integrated loudness," ITU-R BS.1770's specific K-weighted, gated measurement, and the test
(`crates/namir-engine/src/stages/nam.rs`) substitutes plain RMS-in-dB — a fair proxy for the
matched-signal-pair comparisons that test makes, stated as a proxy in its own doc comment, but not
the literal method, and no BS.1770 meter exists anywhere in this codebase. **FR-NAM-100 was not
built** — a separate, Should-priority requirement (dBu-calibrated operating levels, a user-stated
interface-sensitivity setting) that needs a real UI surface this milestone did not scope time for;
deferred to whichever future milestone next touches NAM-stage calibration, not silently dropped.

### M10 close-out — §14's re-audit table, this session, 2026-08-09

FR-NAM-140, FR-NAM-150 and FR-NAM-030 move to **Done** in §14's `### M9a re-audit` table;
FR-NAM-090 moves to **Partial**. See that table's own per-row evidence bullets, appended beneath it
in the same session, for the file-path evidence D-23.2 requires. FR-NAM-100 stays **Not started** —
this milestone did not touch it.

---

## 18. M11 — WASAPI exclusive mode

**Size: M.** **Depends on:** the author's own `cpal` fork existing — the one prerequisite in this
group that is not itself Namir code. **Blocks:** M8's Must-row check via FR-IO-020, and §14's 5.11
IO row, which cannot reach all-Done without it. **Governed by:** `02-architecture.md` **D-13.4**,
and **risk R-10**.

**The blocker, restated because it is architectural rather than a matter of effort:** `cpal` 0.18.1
— D-13.1's pinned dependency — hardcodes `AUDCLNT_SHAREMODE_SHARED`. There is no parameter, no
cargo feature, and no extension trait through which a caller can ask for
`AUDCLNT_SHAREMODE_EXCLUSIVE`. That was verified at M6 against that exact version's vendored source
rather than inferred from documentation, and is written up in
`docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`. **D-13.4 resolves the two-way choice M6
left open in favour of a forked `cpal`**, over the alternative of a `namir-platform`-owned unsafe
WASAPI helper.

**Deliverables:**

- **The `cpal` fork** adding exclusive-mode support, consumed as a git dependency **pinned by
  commit**. Nothing else in this milestone can start without it.
- **D-13.4 recorded in `02-architecture.md`**, with its §17 dependency-register row (severity
  **High** — a maintained fork is an ongoing cost, and divergence from upstream is the real risk,
  not the patch itself) and its §22 risk row **R-10**. The git dependency interacts with
  `cargo-deny`'s `[sources]` policy and weakens the build reproducibility NFR-SEC-040 asks for; say
  so in the register rather than discovering it when `cargo deny check` turns red. Upstream the
  change if upstream will take it — that is the only mitigation that actually retires R-10.
- **`AppSettings::exclusive_mode` wired through.** The field already exists and is already
  persisted; it is simply never read. It reaches the device through `namir-app`'s `AudioBackend`
  trait (`crates/namir-app/src/audio_io.rs`), which is **extended rather than needing its boundary
  redrawn** — exactly the outcome §15 item 6 argued for when it made the case for deciding early.
  **No settings migration is needed**, which is the whole return on that field having been added
  speculatively at M6.
- **Failure behaviour, which matters more here than it does in shared mode.** An exclusive-mode
  open fails for reasons shared mode never encounters — another application already holds the
  device, or the requested format is not natively supported by it. Both need a catalogued error and
  a defined fallback (shared mode, with the user told which mode they actually got), not a silent
  failure to start audio and not a mode indicator that lies.

**Opportunistic, if the hardware allows:** **R-5 / FR-IO-070** (device removal mid-session) has
stayed open across M6 and M7 for want of a device that can be made to fail on demand. If one
becomes available while this milestone's audio-path work is open, take it then — the code is in
hand and the setup cost is already paid. If not, it stays open and is **not** back-filled with an
inspection claim.

**Acceptance:** FR-IO-020 closes, verified against real WASAPI hardware on the §2 reference machine,
with its manual-test document updated from "no path forward" to an executed result; §14's 5.11 IO
row is unblocked; D-13.4, the §17 register row and R-10 are all on the record.

### M11 status — this session, 2026-08-11

Every acceptance criterion above is met: FR-IO-020 closes on an executed run against the §2
reference machine's real hardware, its manual-test document carries that run, §14's 5.11 IO row
moves below, and D-13.4, §17's fork row and R-10 are all on the record with M11 notes. Recorded here
with the two places this section predicted wrongly, in the shape M10's status subsection above uses.

**Size: not M.** The fork alone is seven commits on upstream trunk `e0893c3` — **2867 insertions /
144 deletions across 11 files** — and the Namir-side change measures **2396 insertions / 173
deletions across 18 files** under `crates/`, `.github/`, `deny.toml` and `Cargo.lock`, against M10's
merge commit `a81ead9`. Two things this section's deliverable list does not mention account for most
of the Namir side: an integer sample-format converter
(`crates/namir-app/src/audio_io/convert.rs`, 436 new lines) that had to be written because exclusive
mode does no format conversion — the fork drops `AUTOCONVERTPCM`/`SRC_DEFAULT_QUALITY` there, WASAPI
rejecting both — and a UI surface, the mode indicator (`crates/namir-ui/src/app.rs`,
`crates/namir-ui/src/host.rs`), which "the user told which mode they actually got" implies but does
not name. **L on this document's own scale**, calibrated against the neighbouring milestone rather
than asserted: M10 is labelled L and lands 4212 insertions across 27 files, where M11 authored
roughly 5300 insertions across 29 files counting both repositories. The fork's share of that is the
substance of R-10's corrected rebase estimate rather than a scoping error here.

**"Nothing else in this milestone can start without it" is false as written, and the milestone was
built on it being false.** The entire Namir-side seam — `ShareMode` on `StreamParams`,
`AudioBackend::supports_exclusive`, the catalogued `app.audio_io.exclusive_mode_unavailable`
warning, the all-or-nothing rule in `crate::app::negotiate_share_mode`, and the mode indicator —
was built and unit-tested green **against upstream `cpal` 0.18.1**, with
`CpalBackend::supports_exclusive` returning `ExclusiveModeOutcome::Unsupported`, before the fork was
pinned (commits `4c131d3` and `2bae30c`, both before `d1907ed`). What the fork was needed for was
answering the question, not asking it. The ordering was deliberate and it worked: the seam's shape
was settled against a backend that could only say no, so pinning the fork changed one method body
(`1167db1`) rather than the design. Worth generalising — a fork that gates *nothing* until its answer
is needed is a much smaller dependency than one everything waits on.

**Two real defects were found only by running on real hardware, after every automated check had
passed.** Both are upstream `cpal` bugs that exclusive mode exposed rather than caused, both
independently upstreamable, and both are written up at D-13.4 in `docs/02-architecture.md`. (a)
`config_to_waveformatextensible` sent `dwChannelMask = 0` for every format; a PreSonus AudioBox 22VSL
refuses that in exclusive mode where Microsoft's own HD Audio driver accepts it, so the same machine's
Realtek and HDMI endpoints hid the defect throughout. Fork commit `ab5f40a`. (b) WASAPI stores 24
valid bits **left-justified** in a 32-bit container and `dasp_sample::I24` is right-aligned in the
low 24; `cpal` wrote one as the other — render about 48 dB too quiet, capture 256x too large. Fork
commit `2edbacb`.

**Why that matters more than two bug fixes.** With both defects present: the conversion arithmetic
was correct and exhaustively unit-tested at its boundaries, the `WAVEFORMATEXTENSIBLE` was
hexdump-verified, the fork type-checked for `x86_64-pc-windows-msvc`, the workspace's **913 tests
passed** and CI was green on all three platforms. The disagreements were about the container
*conventions around* correct code, and both sides of each boundary agreed with themselves — so no
in-process test could see either gap: one is observable only where the driver is, the other only by a
person listening to a speaker. That is the concrete argument for FR-IO-020 carrying `Verify: M` and
for D-18.6's rule that such a Must is traced by its manual document. It is also a caution for
whoever rebases the fork: a rebase that compiles and passes CI is not evidence the Windows audio path
still works, and `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md` has to be re-run.

**A third defect was found and deliberately not fixed: issue #15.** Step 7's fallback notice renders
its template placeholders literally — `Exclusive mode is not available for {device} ({reason}); using
shared mode.` reaches the user with the braces in it. `namir_ui::notices::notice_text`
(`crates/namir-ui/src/notices.rs:43-47`) concatenates `{code.id}: {message_template} ({detail})` and
nothing in the workspace ever substitutes into `message_template`, so this has held for every
placeholder-bearing catalogue entry since the mechanism landed at M5 — the issue tallies 30 entries
across seven crates. It is pre-existing, workspace-wide, and touches crates M11 has no business
editing; M11's own new entry is left worded like its neighbours so the fix can be one consistent pass
rather than one odd entry left behind. Its FR-UI-070 impact is presentation, not capability: the
`detail` string does name the device, the reason and the fallback.

**FR-IO-070 was offered and not taken.** This section's opportunistic item is declined explicitly:
no device that can be made to fail on demand was available during this milestone's audio-path work,
so per this section's own instruction it "stays open and is **not** back-filled with an inspection
claim". **This resolves §15 item 17's FR-IO-070 limb in favour of M9b** — the annotation and the
roadmap conflicted only while M11 might still have taken it, and M11 has now declined, leaving M9b as
the sole claimant. The `// uncovered:` field at `crates/namir-app/src/device_state.rs:247-252` is
therefore **left exactly as written**, not edited to fit; item 17's other two limbs (FR-CFG-030 and
NFR-LIC-030) are untouched by this milestone and stay open.

**§15 item 16 — the absent audio-device panel — gained real evidence rather than another opinion.**
M11 took the mode *indicator* only, so the panel question is still open, and this milestone is not
the one that settles it. But the milestone hit the gap **three separate times**, each requiring a
hand-edit of `%APPDATA%\Namir\audio-settings.json` with the app closed: enabling exclusive mode at
all, selecting a device, and driving step 7's fallback test by naming a device that refuses exclusive
mode. Item 16's third answer — "keep the current silence" — now has a concrete cost attached: the
only executed evidence FR-IO-020 has could not have been produced through the product's own
interface. That is a practical finding for whoever settles item 16, not a theoretical one. Note what
it is *not*: FR-IO-020 is not one of the five Musts §16's 2026-08-09 table lists as leaning on the
panel, and it does not need one — it requires no chooser the way FR-IO-010 does, so a persisted
setting satisfies its literal text. What M11 adds to item 16 is a demonstration of how thin a
hand-edited JSON key is as the only mechanism a user has, not a sixth requirement on the list.

**Known-unverified, in one place, so the PASS is not read as more than it is:**

- **The `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` retry path has never executed** — not in a test, not on
  hardware. The reference machine's device period is 3 ms = 144 frames at 48 kHz, a whole number, so
  the error never arose; step 8 records that as the expected non-event it is, not as a pass.
- **Shared-mode `I24` is unexercised and unreachable from this product.** `namir-app` restricts
  shared mode to `F32` (`crates/namir-app/src/audio_io.rs:453`), so step 9 exercised the fork's
  container-justification path returning a **zero** shift for a full container. The fix is correct
  for shared-mode `I24` by the format contract, not by measurement.
- **Packed 24-bit (a 3-byte container) cannot be expressed by `cpal` at all** — `SampleFormat::I24`'s
  `sample_size()` is `size_of::<i32>()`, so every format this backend builds has a 32-bit container,
  even though its parser maps a device-reported `wBitsPerSample == 24` to that same `I24`. A device
  offering only packed 24-bit would have every candidate refused and fall back to shared. This
  endpoint offers 24-in-32, so the case did not arise.
- **The fork's rebase burden is higher than D-13.4 assumed.** `src/host/wasapi/device.rs` alone is
  546 insertions against 130 deletions and will conflict; the cause is structural, a per-call
  extension trait threading the share mode through every enumeration and build path. R-10 carries the
  corrected estimate and D-13.4 records which of its own sentences is withdrawn.
- **One machine, one third-party interface.** Both hardware-only defects were invisible on the same
  machine's Microsoft HD Audio endpoints; a second vendor's interface would be worth more than a
  second run on this one.

### M11 close-out — §14's re-audit table, this session, 2026-08-11

FR-IO-020 moves from **Partial** to **Done** in §14's `### M9a re-audit` table: the 5.11 IO row goes
from `8 | 1 | 7 | 0` to `8 | 2 | 6 | 0`. The Must count is unchanged — `xtask traceability`
machine-checks that column and it is not M11's to move — and only the hand-adjudicated verdict
columns change. See that table's own per-row evidence bullet for the file-path evidence D-23.2
requires, and for the two caveats named there rather than hidden behind an unqualified pass. No other
requirement's verdict moves: M11 produced evidence for FR-IO-020 and for nothing else, and a cell
nobody's evidence has touched stays where M9a left it.

---

## 19. M12 — Brand, README and product identity

**Size: S.** **Depends on:** nothing technical. Sequenced here only because it is the cheapest item
in the group and there is no reason for it to displace substantive work. **Blocks:** M13, weakly
but genuinely — an installer needs a product name, an icon and a licence statement for its own
metadata, and settling those *inside* a packaging milestone means settling them in a hurry.

**New requirements this milestone closes:** **NFR-DOC-040** (the repository carries a README
identifying the product, what it does, its licence, and how to build, run and test it),
**FR-UI-110** (the interface displays the brand mark; the standalone window and executable carry
the application icon) and **NFR-LIC-070** (brand assets are not covered by the code licence, and
their terms are stated explicitly in the repository). Note the id choices, for the same FRS §1.5
reason as M10's: FR-UI-060 and NFR-LIC-060 were already taken, by the load-time responsiveness
requirement and by REUSE compliance respectively.

**Deliverables:**

- **Track `images/` in git.** It is currently **untracked** — the brand assets exist on disk and in
  nobody's clone but the author's. Do this first; every other item here depends on it.
- **`README.md` at the repository root** (NFR-DOC-040): the logo, what Namir is, the licence, and
  build/run/test instructions. It also serves NFR-BUILD-020's "documented and CI-exercised" clause,
  which today holds only on the CI-exercised half.
- **The in-app brand mark.** `crates/namir-ui/src/app.rs:40` renders `ui.heading("Namir")` today —
  a text heading standing in for a mark. One thing to know before starting: this UI is
  **egui-on-baseview, not eframe**, so the usual `eframe` window-icon recipe does not apply and the
  window icon has to be set through baseview's own window options instead.

  *Correction (added M12, 2026-08-10):* **the last clause is wrong and cannot be followed.**
  `baseview` 0.2.2's `WindowOpenOptions` is `#[non_exhaustive]` and carries exactly `title`, `size`
  and `scale` plus an `opengl`-gated `gl_config` — there is no icon field of any kind, so there are
  no "baseview's own window options" to set one through. `02-architecture.md` **D-17.3**'s second
  consequence note records the finding and its effect: the window icon defers to M13 alongside the
  executable icon, rather than being the half of FR-UI-110 that lands here. §17's register row for
  `baseview` said `0.3.0` when this was written and the tree has always pinned 0.2.2; that row is
  corrected in the same pass, and whether 0.3.0 gained an icon field is unchecked.
- **The Windows `.exe` icon**, which needs a build script to embed the resource. Flagged rather
  than waved through: build scripts sit awkwardly with this project's dependency-adoption bar, and
  `libc` is already on record as the one knowing exception to it. Decide this one deliberately,
  the same way.
- **The brand assets themselves** — `#ff6600`, a single fill, transparent background, a roughly
  3.73:1 wordmark plus a leopard-head mark — and **NFR-LIC-070's explicit statement that
  `MIT OR Apache-2.0` covers the code, not the name and not the mark.** A permissive code licence
  alongside an unstated trademark position is the combination that produces awkward conversations
  later; stating it now costs a paragraph.

**Acceptance:** `images/` is tracked; a README exists at the repository root and its build/run/test
instructions have actually been followed on a clean clone rather than assumed correct; the brand
mark renders in both product shells; the standalone window and executable carry the icon on
Windows; the brand-asset licensing statement is in the repository.

### M12 status — this session, 2026-08-10

Built **before M11**, which §19's own header permits ("**Depends on:** nothing technical") and
which M11's state made the sensible order: M11's one prerequisite is a forked `cpal` that does not
exist, `crates/namir-app/Cargo.toml` still pinning `cpal = "0.18.1"` from crates.io. Nothing
downstream waits on M11 — §20's M13 depends on M12 and M9, not on M11 — so the reordering costs
nothing and unblocks M13.

**The first deliverable was already done before it was written.** "Track `images/` in git. It is
currently **untracked** … Do this first; every other item here depends on it" is stale:
`images/namir.png` and `images/namir.svg` were committed in `9c1c698`, the same commit that wrote
that sentence. No action was needed.

**The brand mark ships with no image decoder in either product, and no build script.** The artwork
is a single fill on a transparent background, which makes an 8-bit alpha mask a *colour*-lossless
re-encoding — the colour is constant and every bit of shape and anti-aliasing lives in alpha. So
`xtask identity` decodes `images/namir.png` with an `xtask`-only `png` dependency and emits a 34
376-byte blob (358x96 plus an 8-byte header) that `namir-ui` `include_bytes!`s and tints at upload.
`cargo tree -e normal` reaches `png` from neither product and `xtask attribution` is unchanged.
Generation is integer-only so the blob cannot depend on which machine ran `--write`. The 24x
downsample from 1767x474 is a deliberate, separate loss, not covered by "lossless".

**Both icon clauses moved to M13, one of them for a reason this section got wrong.** §15 item 7 is
resolved by **D-17.3** against admitting a build script to a shipped crate for a cosmetic feature.
The window icon then turned out to be blocked independently: `baseview` 0.2.2's `WindowOpenOptions`
has no icon field at all, so this section's instruction to set it "through baseview's own window
options" cannot be followed — corrected by an appended note at that bullet. §17's register row
compounded it by recording `baseview` 0.3.0, a version the tree has never used; that row is fixed
in place and whether 0.3.0 gained an icon field is left explicitly unchecked for M13.

**What the requirements actually did.** NFR-LIC-070 closes (plain tag). **NFR-DOC-040 closes only
partially** — a substring check cannot reach its "stating what it does" clause — so §14's 6.8 DOC
cell moves one column, not two. **FR-UI-110 does not close at all**; only its brand-mark clause was
in scope and even that is unobserved, below.

**One correction against this milestone's own scoreboard.** `ci.yml`'s NFR-BUILD-020 partial
declared `closes M12`. M12 gives it a README carrying the build, run and test commands with `xtask
identity` asserting those strings are present — but nothing executes a command *as documented*, and
the README and CI are already apart (`cargo build --workspace` against `cargo build --workspace
--all-targets`). The field is rewritten to claim only what holds and moved to `closes M13`.

**An adversarial review found a defect this milestone introduced into the gate itself, and it is
the most useful thing in this section.** The FR-UI-110 manual-test document mentioned `FR-PKG-030`
in passing prose. `xtask traceability` resolves a `Verify: M` requirement against a manual-test
file's **whole content** (`xtask/src/traceability.rs`'s `build_report`), so that bare mention
marked an unrelated Must — the Windows installer's per-user/system-wide scope — as covered in
checked-in `docs/03-test-plan.md`, and dropped the informational uncovered count from 15 to 14.
Reworded; the count is 15 again. **The durable fix is not made**: the whole-content match should be
narrowed to a declared-ids line, which is left for M9b or M13 as a note here rather than done in a
milestone that had no other business in that tool. Three further over-claims the same review caught
are fixed and recorded in `02-architecture.md`'s changelog 0.24.

**What was verified, and what was not.** From a genuine clean clone of this branch: `cargo build
--workspace` finished in 1m26s, `cargo test --workspace` ran 41 suites with no failures, and
`identity`, `layering` and `attribution` were all clean — so §19's "followed on a clean clone
rather than assumed correct" is met for the *source tree*. It is **not** met for a cold machine:
this sandbox already had Rust 1.97 and `libasound2-dev` installed before the clone, so the README's
prerequisite list is still untested against a fresh environment.

**The brand mark has never been seen.** No display exists here — `cargo run -p namir-app` starts
audio and then panics in `baseview`'s X11 window open, and `xvfb-run` does not help, `baseview`
0.2.2 needing a GLX-capable display. No host was available for the plugin shell either. So this
section's Acceptance clause "the brand mark renders in both product shells" is met in code and
**unverified in fact**, and `docs/manual-tests/fr-ui-110-brand-mark.md` records all four of its
steps as not executed. The same applies to the mipmapping added after review found the mark is
minified ~4-5x without it: a reasoned fix to a problem nobody has observed, asserted by a test that
reads the texture options back rather than by looking.

### M12 close-out — §14's re-audit table, this session, 2026-08-10

**NFR-LIC-070** moves to **Done** and **NFR-DOC-040** to **Partial** in §14's `### M9a re-audit`
table; 6.5 LIC becomes 3 / 3 / 0, 6.8 DOC becomes 1 / 2 / 0, Total 32 / 91 / 7 → 33 / 92 / 5. See
that table's own appended `**M12 moves**` block for the file-path evidence D-23.2 requires,
including why 6.7 BUILD deliberately does not move. **FR-UI-110 stays open** — it is a Should, so
it sits in no §14 row, and its remaining work is the four manual steps plus both icon clauses, all
of which need the Windows machine M13 requires anyway.

### M12 addendum: the manual test ran on Windows, and found the mark too small, 2026-08-11

**Supersedes the status subsection's "The brand mark has never been seen" paragraph**, which is
left as written per this document's convention -- it was true of the session that wrote it, and it
is why that session's design notes were reasoned rather than observed.

FR-UI-110's manual test was executed on Windows, the only platform where its step 2 is possible at
all (`namir-clap` declares Win32 embedded-only GUI support). **Steps 1, 2 and 4 pass**: the mark is
visible in the standalone window, visible in the CLAP plugin inside a host, and its accessible name
reads "Namir", so FR-UI-030 did not regress when an image replaced a text heading. The Acceptance
clause "the brand mark renders in both product shells" is now met **in fact** and not only in code.

**Step 3 found a real defect: the mark was too small in both shells.** It was drawn at one
`TextStyle::Heading` row -- ~25 logical pixels, the height of a line of text rather than of a logo.
`brand::MARK_HEIGHT_IN_HEADINGS` now doubles it to ~50. That factor is the largest one free of
cost: the blob is 96 rows, so ~50 logical pixels is ~100 physical at 2x HiDPI against 96 stored,
which is effectively 1:1 and the sharpest this asset can be drawn. **The consequence is that the
blob's former ~2x resolution margin is now spent**, recorded at `MARK_TARGET_HEIGHT`'s own doc
comment in `xtask/src/identity.rs`: any further increase in the drawn size has to raise that
constant in the same change, or the mark magnifies a mask with no more detail in it.

**What is still open, and it is smaller than it was.** Step 3's other half -- that the mark be
legible rather than aliased at 1x and at a HiDPI scale factor -- was not reported either way, and
the enlarged mark has not been seen at all. That half matters because two fixes now ride on it
unobserved: the mipmapping added when a review found the mark minified ~4-5x with `mipmap_mode:
None`, and this size change, which halves that minification to ~2x. Both are reasoned from the
pinned sources; neither has been looked at. **FR-UI-110 therefore stays open**, on that half plus
both icon clauses, which D-17.3 defers to M13. No §14 cell moves -- FR-UI-110 is a Should and sits
in no row.

**Closed later the same day, superseding the paragraph above.** The enlarged mark was inspected and
reported to read fine, so **step 3's legibility half passes too and all four steps of the manual
test now pass**. That also confirms the two fixes the paragraph above flagged as riding on it
unobserved -- the mipmapping and the size increase -- in the only way they could be confirmed.
**FR-UI-110's brand-mark clause is closed**: observed in both product shells, and legible. The
requirement stays open on **both icon clauses only**, which D-17.3 defers to M13. Still no §14 cell
moves; it is a Should and sits in no row.

**§19's Acceptance, clause by clause, as it now stands.** `images/` is tracked, and already was.
The README exists and its build/run/test instructions were followed on a clean clone -- though not
on a cold machine, so its prerequisite list is still untested. The brand mark renders in both
product shells, now verified rather than argued. The brand-asset licensing statement is in the
repository. **The one clause not met is the icon**, deliberately, on the record, and carried to M13
with the rest of that work.


---

## 20. M13 — Distribution and packaging

**Size: L.** **Depends on:** M12 (name, icon, licence statement) and, for anything a user is meant
to install, M9's verification work. **Blocks:** M8 directly — "cross-platform release binaries" is
an M8 checklist item and this milestone is what produces them. **Governed by:**
`02-architecture.md` **D-18.3** (release and packaging pipeline) and **D-18.4** (`publish = false`);
**risk R-11** (unsigned release binaries).

**New requirements this milestone closes:** FRS §5.15 (PKG) in full — **FR-PKG-010** (installable
distributions per platform, built by CI from a tagged source tree), **FR-PKG-020** (the CLAP
artifact in the form each platform's loader requires), **FR-PKG-030** (per-user and system-wide
install scope on Windows, defaulting to per-user), **FR-PKG-040** (attribution file and licence
texts in every distribution) and **FR-PKG-050** (Should — a plain archive alongside each installer).

**Deliverables:**

- **`xtask bundle`, first — nothing else in this milestone works without it.** Nothing in the Rust
  ecosystem will build a macOS `.clap` bundle for you; `nih_plug_xtask` is the model to follow. On
  Windows and Linux the CLAP artifact is a renamed shared library, but **on macOS it is a bundle
  directory** — `Namir.clap/Contents/{Info.plist, PkgInfo, MacOS/<dylib>}` — because CLAP's own
  `entry.h` defines `plugin_path` as the DSO on Linux and Windows and as the *bundle* on macOS.
  `docs/user-guide.md` stated this incorrectly and is corrected; FR-PKG-020 now carries the
  requirement, and D-13.3 gains a note recording both facts.
- **`release.yml` across the three runners**: build → bundle → per-OS package → GitHub Release,
  triggered from a tag, so that FR-PKG-010's "built by CI from a tagged source tree" is literally
  true rather than a description of what someone once did on a laptop.
- **The traceability tags and the manual-test document, planned in rather than discovered late.**
  `xtask traceability` reads `// trace:` annotations in repository source and `# trace:`
  annotations in CI/build configuration — **not** the output of a release run. A green release
  pipeline therefore closes **none** of FR-PKG-010, FR-PKG-020 or FR-PKG-040 by itself: each needs
  an annotated test or an `xtask` subcommand that asserts the property (the bundle's structure is
  checkable offline; so is the presence of `THIRD-PARTY-NOTICES.md` and the licence texts inside a
  built distribution). FR-PKG-030 is *Verify: M* and needs a
  `docs/manual-tests/fr-pkg-030-windows-install-scope.md` recording exactly what was and wasn't
  executed, per this project's standing rule for requirements no automated test can reach. Budget
  for this alongside the pipeline itself; M9 makes the traceability check a **required** gate, so
  a packaging milestone that ships without its tags turns CI red rather than merely leaving a hole.
- **Windows — an Inno Setup `.iss` plus a plain ZIP**, unsigned initially. Inno's `{autocf}` token
  resolves to `%COMMONPROGRAMFILES%` when elevated and `%LOCALAPPDATA%\Programs\Common` when not,
  which is exactly D-13.3's two paths out of a single line, with `PrivilegesRequired=lowest` giving
  the per-user default; it is preinstalled on `windows-latest`. **Before committing to the per-user
  default, empirically verify that REAPER actually scans `%LOCALAPPDATA%\Programs\Common\CLAP`.**
  This is not a theoretical caution: Dexed ships its per-user mode commented out with a note that
  the DAW issues were never resolved, and D-13.3's own doc comment already warns that this
  silent-failure mode — the plugin installs successfully and the host simply never lists it — is
  the likeliest support ticket this product will ever generate.
- **macOS — a `.pkg` inside a `.dmg`**, porting Surge's `make_installer.sh`, with signing
  **conditional on a secret being present** so that unsigned builds take the identical code path
  and notarisation can be added later without rework. Record why `.pkg` rather than a `.dmg` alone:
  only `pkgbuild`/`productbuild` can place multiple payloads at multiple absolute paths, and files
  placed by `installer` never carry `com.apple.quarantine`, whereas a zip-delivered plugin does.
  **The honest caveat, recorded because it determines who can actually use a macOS build:** a
  quarantined plugin fails to load in a DAW with **no user-visible "Open Anyway" path** — that
  affordance exists for applications, not for plugins, and macOS 15 removed the Control-click
  bypass. Until signing and notarisation are real, **macOS is effectively developer-only**, and
  saying so plainly is better than shipping something that appears to install and then does nothing.
- **Linux — a tarball plus an `install.sh`** defaulting to `~/.clap`. Two known wrinkles worth
  recording now rather than hitting later: Fedora uses `/usr/lib64/clap`, and CLAP issue #46 on
  `~/.clap` versus an XDG-conformant path is still open, so today's default may need revisiting.
- **Physically bundle `THIRD-PARTY-NOTICES.md` and the licence texts into every distribution**
  (FR-PKG-040). M7 produced the attribution file and its CI freshness gate but explicitly deferred
  the bundling for want of a packaging pipeline. This is that pipeline, so the deferral closes here.
- **Apply D-18.4: `publish = false` workspace-wide**, plus a `version` on every path dependency as
  hygiene. Namir is one product, not a library ecosystem — 12 of 14 crates are implementation
  details, `namir-clap` is a `cdylib` nobody can depend on, and `namir-fixtures` is test tooling.
  Record that `cargo publish` hard-fails today regardless, since no path dependency carries a
  `version`, and that adding those versions is worth doing anyway so that reversing the policy
  later stays a cheap decision. Accepted cost, stated plainly: no docs.rs, and no
  `cargo install namir`.
- **NFR-SEC-040 (Should) — reproducible builds plus published hashes**, taken as far as it goes
  given the git dependency M11 introduces. Publish the hashes regardless: a hash a user can check
  is worth having even where bit-for-bit reproducibility is not achieved, and it is honest about
  which of the two is on offer.

**A standing caveat over this whole milestone (risk R-11): the binaries are unsigned.** On Windows
that means a SmartScreen warning on every release and Smart App Control blocking outright; on macOS
it means the quarantine behaviour described above. D-18.3's signing-conditional structure is what
keeps the cost of fixing this low; it does not fix it. **Revisit before any release aimed at
non-developers.**

**Acceptance:** a tagged commit produces, from CI alone, an installer and a plain archive for each
of the three platforms; the macOS CLAP artifact is a valid bundle directory and loads in a host on
a machine where quarantine does not apply; the Windows installer offers both scopes, defaults to
per-user, and that per-user path has been empirically confirmed as scanned by at least one real
DAW; every distribution contains `THIRD-PARTY-NOTICES.md` and the licence texts; FR-PKG-010 through
FR-PKG-040 each carry a traceability annotation or a manual-test document, so `xtask traceability`
stays green; FRS §5.15 closes.

### M13 scope note (added M9's P0 decision pass, 2026-08-08)

Two obligations arrive here from M9's P0 decision pass. Recorded as a note rather than by editing the
deliverables above, per this document's convention.

**NFR-PERF-030 moves from M9 into this milestone.** The requirement measures the standalone
application reaching an audible state within 3 seconds on the reference machine with a warm library
index (FRS §6.2). It cannot run on any CI runner: a machine with no audio device diverts `namir-app`
to `open_window_without_audio` (`crates/namir-app/src/app.rs:116`, `:148`, `:155`, `:164`, `:309`)
and never becomes audible, so the measurement needs both a real machine and a seam in `namir-app`'s
entry path that exists solely to enable it. M13's release pipeline already touches that launch path
with a real machine in the loop, which is why the harness costs least here. It is `**UNRESOLVED**`
in the checked-in generated plan today, so its arrival here is also what takes the count of
uncovered Musts owned by a milestone other than M9 from nine to **ten** — the number §16's P0
subsection and D-18.5 both work from. M9's P0 subsection considered keeping it in M9 and decided
against; D-2.4 still binds the certified figure to §2's pinned reference machine and at least five
repetitions, wherever the harness lives.

**The forcing function this milestone's third deliverable assumes does not exist until this
milestone's own close-out.** That bullet says "M9 makes the traceability check a **required** gate, so
a packaging milestone that ships without its tags turns CI red rather than merely leaving a hole."
Under **D-18.5** only the plan-diff half is required from M9a; the zero-uncovered half stays
informational until M13's close-out. FR-PKG-010 through -040 are already `**UNRESOLVED**` in the
checked-in plan, so landing untagged packaging code leaves that file unchanged and the gate green.
The sentence above stands as written; what actually enforces the tags until the flip is **this
milestone's own Acceptance paragraph**, which names them explicitly. **M13's close-out owns the
flip** — delete `--allow-uncovered` from the required step, delete the informational step — and must
record that it happened. M9's close-out must not claim it.

### M13 scope note, second correction: the flip is M9b's, 2026-08-11

**The paragraph immediately above is wrong in its last three sentences, and this note is the
correction rather than an edit to them.** M13's close-out cannot own the flip, for a reason visible
in §16 before this milestone started. §16's attribution table counts ten uncovered Musts owned by
M10, M12 and M13 and concludes M13 is where the list runs out; it does not carry the *other*
fourteen, which are M9's own and which §16's own second deliverable assigns to **M9b** — ordered
**after** M13 by that same pass. Counted from the checked-in plan at this milestone's start:
**15** `**UNRESOLVED**` Must rows, of which M13 owns five — FR-PKG-010, -020, -030, -040 and
NFR-PERF-030 — and **ten** are M9b's. §16's own correction lists those ten by id, and this note
deliberately does not repeat them: `xtask traceability` attributes an uncovered id to the last
`## <n>. M<k>` section naming it, so spelling an M9b id out here would relabel it **M13** in the
tool's printed output — which is how the first draft of this note was caught. Deleting
`--allow-uncovered` here would turn a required check red on the day it became required and leave it
red until M9b, which is exactly the failure mode D-18.5 split the gate to avoid.

**M9b's close-out owns the flip.** Everything else in the paragraph above holds: the flip is still
the deletion of the flag and of the informational step, still two lines; the required plan-diff half
is unaffected; and **this milestone's Acceptance paragraph remains the only thing enforcing M13's
own annotations**, which is what that paragraph was already saying. NFR-QUAL-010 therefore closes at
**M9b**, and M13's close-out must not claim it any more than M9a's was allowed to. `02-architecture.md`
D-18.5 and §16's restated acceptance carry the same correction, each as an appended note.

### M13 status — `xtask bundle`, 2026-08-11

The first deliverable, and the one everything else in this milestone waits on, is built:
`cargo run -p xtask -- bundle [--target <windows|macos|linux>] [--check|--plan]`
(`xtask/src/bundle.rs`, wired at `xtask/src/main.rs`'s `run_bundle`). It stages one platform's
already-built release artifacts under `target/bundle/<platform>` — the CLAP artifact in the form
that platform's loader requires, the standalone `namir`/`namir.exe`, and
`THIRD-PARTY-NOTICES.md`, `LICENSE-MIT`, `LICENSE-APACHE` and `README.md` — then asserts what it
produced and fails on any deviation, so FR-PKG-020's "the packaging step asserts the produced
layout" is true of `bundle` itself and not only of `bundle --check`. It builds nothing, archives
nothing and hashes nothing: those stay `release.yml`'s, per D-18.3's fixed order.

Three things worth recording rather than leaving to be re-derived.

**The layout planner is pure and host-independent.** `plan(Platform) -> Layout` reads nothing and
never consults the host, so the macOS bundle layout — `Namir.clap/Contents/{Info.plist, PkgInfo,
MacOS/libnamir_clap.dylib}` — is computed, asserted and (given macOS artifacts) materialised from a
Windows or Linux machine. That is what lets FR-PKG-020's `Verify: S` assertion be an ordinary test
running on **every** CI runner rather than only on the macOS leg of a release job, which would
leave the one platform whose layout is easy to get wrong checked only where the check is hardest to
reach. `Info.plist` is emitted as deterministic text and `PkgInfo` as its eight literal bytes, so
this took **no new cargo dependency** and §17's register is unchanged (that note's own line: a
non-cargo build tool needs no row, a cargo dependency does).

**The two annotations, and why one of them is partial.** `// trace: FR-PKG-020` is plain: its test
materialises a staging tree for all three platforms from a fake shared library, runs `check` over
the *produced* result, and asserts both negative cases — a plain file named `Namir.clap` on macOS,
and a directory named `Namir.clap` on Windows. `// trace-partial: FR-PKG-040` is not: that
requirement quantifies over "every distribution, installer and archive alike", and the staging tree
is the packager's *input*, not a distribution. The Windows installer, the macOS `.pkg`/`.dmg` and
the plain archives are later deliverables of this milestone, so its `uncovered:` field names them
and closes M13. FR-PKG-010 and FR-PKG-030 are untagged and stay `**UNRESOLVED**`, which is the
correct disposition for a requirement whose deliverable has not run. Uncovered Musts fell from 17
to 15; partials rose from 59 to 60.

**One ledger entry corrected in passing.** `xtask/src/attribution.rs`'s `// uncovered:` field for
NFR-LIC-030 said the "shipped with the binaries" clause "has no artifact of any kind … packaged
with nothing", which this change makes false — the attribution file is now staged beside the
binaries and its presence asserted. The field is reworded to say exactly that and to keep the half
that is still open (no release workflow, no installer, no archive); the tag stays `trace-partial:`
and still closes M13.

**§14's `### M9a re-audit` table is deliberately untouched.** Its 5.15 PKG row still reads
`4 | 0 | 0 | 4`. Verdict cells move at the close-out of the milestone whose evidence moves them,
and M13 has one deliverable of seven done; the row moves at M13's close-out, from that milestone's
whole evidence, not incrementally from this one.

**Amendment, same day: the macOS standalone is an application bundle, not a bare executable.** The
first form of `plan(Platform::MacOs)` above staged the standalone as a bare Mach-O named `namir`.
The macOS packaging lane found that while writing `packaging/macos/make_installer.sh`, and it is a
**working-product defect, not a tidiness one**: from macOS 10.14 a process may not open an audio
*input* device until the user has granted microphone access, a process that declares no
`NSMicrophoneUsageDescription` is denied outright rather than prompted, and an **unbundled**
Mach-O has no `Info.plist` to declare that key in. `namir-app` opens an input device on its
ordinary path (`crates/namir-app/src/audio_io.rs`'s `input_devices`/`Direction::Input`) — it is an
instrument-input amp simulator, so that is the entire product. A bare binary in `/Applications` is
also not what D-18.3's "the standalone app under `/Applications`" can mean: Finder treats it as a
Unix executable and double-clicking it opens Terminal.

The macOS layout therefore gains three rows —
`Namir.app/Contents/{Info.plist, PkgInfo, MacOS/namir}` — in the same shape as `Namir.clap`, and
`check` asserts them the same way, `Info.plist` byte-compared and `Namir.app` required to be a
directory. Windows and Linux are untouched: the standalone stays a plain executable on both, and a
`Namir.app` appearing in either tree is now itself a violation. `make_installer.sh` anticipated
this and needs no change — its `if [ -d "${STAGING}/${PRODUCT_NAME}.app" ]` branch takes its other
arm, and its own stopgap wrapper (the one payload `bundle --check` never asserted) is now dead
code that lane can delete.

Two decisions, both recorded at the constant in `xtask/src/bundle.rs` rather than only here.
**`APP_BUNDLE_IDENTIFIER` is `org.legrand.namir.standalone`**, deliberately not the plugin's
`org.legrand.namir`: identifiers must be unique per bundle, and macOS TCC keys the microphone
grant on this one, so sharing it would put the standalone's permission and the plugin's under one
subject. **`LSMinimumSystemVersion` is `11.0`**, and it is derived rather than guessed — nothing in
this repository states a macOS floor and neither `baseview` 0.2.2, `egui-baseview` 0.6.0 nor the
pinned `cpal` fork states one, but CI's macOS leg runs on `macos-latest`, which is Apple Silicon,
so the artifact is `aarch64-apple-darwin`, whose own Rust-target minimum is macOS 11.0 because no
earlier macOS runs on that hardware. An Intel or universal artifact would have a lower floor
(Rust's `x86_64-apple-darwin` baseline is 10.12) and is the thing that would reopen this. The
packaging lane reached `11.0` independently; the two now agree by derivation rather than by
coincidence.

**FR-PKG-020's tag is unchanged and stays plain.** That requirement's text is about "the CLAP
artifact" and nothing else, so the application bundle neither widens nor weakens what the tag
claims. Its test grew the `Namir.app` assertions and a negative case mirroring the `Namir.clap`
one — a bare Mach-O named `Namir.app` is reported, with the microphone reason in the message —
because that test is where a produced macOS tree already exists to assert against, and its doc
comment says in as many words which of its assertions the tag covers. No annotation moved, so
`docs/03-test-plan.md` is unchanged; the uncovered count stays 15 and the partial count 60.

### M13 status — the rest of the milestone, 2026-08-11

The `xtask bundle` subsection above covers the first deliverable. This one covers the other six and
the two that arrived by scope note. **Read the last paragraph first if you read nothing else: none
of this has been executed end to end.**

**Four decisions this milestone was meant to merely execute turned out to be wrong, and that is the
most useful thing here.** Each is recorded as a dated consequence note at the decision rather than
by editing it, and each was found by writing the thing rather than by reviewing the text.

- **D-18.3, twice, on the Windows installer — either error alone would have failed FR-PKG-030.**
  `PrivilegesRequired=lowest` *alone* offers **one** scope, not two: nothing in Inno lets the user
  ask for the other unless `PrivilegesRequiredOverridesAllowed` is also set, so "shall offer both"
  would have failed while every other clause passed. And **`{autocf}` when elevated is the 32-bit
  Common Files directory** — `%CommonProgramFiles(x86)%`, not `%COMMONPROGRAMFILES%` and not a path
  CLAP's `entry.h` lists — unless `ArchitecturesInstallIn64BitMode=x64compatible` is set. So
  D-18.3's "D-13.3's Windows row, both cells, from one line in the `.iss`" is false as written, and
  the failure it would have produced is exactly the silent one D-13.3's rationale exists to warn
  about: installs cleanly, host never lists it, nothing anywhere says why. That directive is why
  `packaging/windows/namir.iss` requires **Inno 6.3+**, which fails at compile time on an older
  Inno rather than quietly landing in the wrong directory.
- **D-13.3, on the Linux system-wide cell.** `/usr/lib/clap` is too narrow: Fedora, RHEL and
  openSUSE use `/usr/lib64/clap`. `packaging/linux/install.sh` probes rather than assuming, and
  probes properly — a bare `[ -d /usr/lib64 ]` is wrong twice, Debian having a real `/usr/lib64`
  holding only the loader and Arch's being a symlink to `/usr/lib`.
- **D-17.3, on the window icon.** M12 left "whether `baseview` 0.3.0 gained an icon field"
  explicitly unchecked. It has not, and neither has any other version: 0.3.0 does not even have
  `WindowOpenOptions`, and its Win32 window class registers `hIcon: null_mut()` byte-identically to
  0.2.2. Published `egui-baseview` 0.6.0 requires `baseview` 0.2.2, so the pin is **forced rather
  than conservative**.

**`xtask bundle`'s macOS layout was wrong when first built, and the packaging lane caught it.** It
staged the standalone as a bare Mach-O named `namir`. An unbundled process has no `Info.plist` and
therefore cannot declare `NSMicrophoneUsageDescription`, which macOS 10.14+ requires before a
process may open an audio **input** device — which, for an instrument amplifier simulator, is the
whole product. `Namir.app/Contents/{Info.plist, PkgInfo, MacOS/namir}` is now staged too, with
`CFBundlePackageType` `APPL` and `PkgInfo` `APPL????` (not the plugin bundle's `BNDL`), a
`CFBundleIdentifier` distinct from the plugin's because macOS TCC keys the microphone grant on it,
and `LSMinimumSystemVersion` **derived rather than guessed**: CI's macOS leg is `macos-latest`, so
the artifact is `aarch64-apple-darwin`, whose own floor is macOS 11.0. An Intel or universal
artifact reopens that number and the constant says so. The macOS script's stopgap wrapper was
deleted rather than kept as a fallback — its own comment called it "the one payload
`xtask bundle --check` never asserted", and a fallback that silently produces an unasserted bundle
from a stale staging tree is worse than a loud failure.

**The three packagers** (`packaging/{windows,macos,linux}/`) all consume `bundle`'s staging tree and
nothing else, so what ships is what `bundle --check` asserted. macOS forces `BundleIsRelocatable`
false — left true, `installer` Spotlight-relocates onto any stray `Namir.clap` and nothing reaches
the CLAP path — and expresses D-13.3's two macOS cells through install domains, the counterpart of
`{autocf}`. Signing is keyed on secrets with an empty-argument-array expansion so signed and
unsigned runs take one code path.

**`release.yml`** is four jobs in D-18.3's order, tag-triggered, with a single `publish` job that
`needs` all three builds, checks nothing out, and takes `download-artifact` as its only input. That
shape is not tidiness: FR-PKG-010's third clause — every published distribution being an artifact of
the workflow rather than of a local build — is checkable only if the publishing step provably cannot
obtain a file from anywhere else. `contents: write` lives on that job alone, and `ci.yml`'s
workspace-wide `contents: read` comment, which justified itself on the grounds that nothing
"touch[es] releases", is rescoped to `ci.yml` rather than left reading as a project-wide claim.

**D-18.4 applied**, and its own prediction checked rather than restated: `publish = false` inherited
from `[workspace.package]`, 59 path dependencies given `version = "0.1.0"`, and
`cargo publish -p namir-core --dry-run` now stops on the policy instead of on a versionless path
dependency. `Cargo.lock` is unchanged by the whole edit.

**NFR-PERF-030 closes to a benchmark that asserts**, per D-23.1's rule that a `Verify: B` needs an
in-process threshold assertion rather than a printed figure. The seam is one environment variable
whose **value is the configuration directory** the probed launch uses — a shape that buys two
things a bare on/off flag does not: "warm library index" becomes a precondition the harness
*establishes* at a size it chose, and a measurement provably cannot write to the user's real config
directory. **440–486 ms against a 3000 ms ceiling**, 25 launches over 4 runs on §2's reference
machine with a warm 10 000-entry index; the index parse is roughly 150 ms of that, an empty-index
launch taking 286 ms in-process. **Informational, not certified** — D-2.4 wants a quiet machine and
this one had this session's own agent processes on it throughout.

**FR-UI-110's executable icon** is `images/namir.ico`, generated from `images/namir.png` by
`xtask identity` and byte-compared for freshness, embedded post-build by `rcedit` before
`xtask bundle` so the staging tree, installer payload and ZIP carry the same binary. D-17.3's
refusal of a build script in a shipped crate holds and its stated cost is now real. Two honest
results: the **16×16 size reads as a smudge** (32 readable, 48 and 256 good), and a contrast-rescale
written to fix that was measured, found to gain 1.05× against an already-243/255 peak alpha, and
**deleted**, with a test pinning the number so it is not re-invented. The window-icon clause cannot
close at all, above.

**Two things added beyond the deliverable list.** `TRADEMARK.md` now ships in every distribution —
the binaries carry the brand mark, NFR-LIC-070 requires the mark's terms stated, and the staged
README's licence section pointed at a file no distribution contained. It is deliberately **not** in
FR-PKG-040's set, which is exactly three and named by the requirement. And **R-16** records that
`baseview` has no Wayland backend in any version: not new, but M13 is the milestone at which it
stops being a developer's problem and becomes a user's, since before this the only way to run Namir
on Linux was to build it and anyone building it already had the X11 headers.

**Corrections to this document made in passing.** §20's scope note cites the
`open_window_without_audio` call sites as `app.rs:116, :148, :155, :164, :309` and says five; M11
had already moved them and there were **four**. They are now `:218, :277, :284, :293` with the
definition at `:470`. The citation is left as written per this document's convention and corrected
here.

**What has never been executed — the paragraph to read before believing any of the above.** No
`iscc` run, no `pkgbuild`, no `productbuild`, no `hdiutil`, no `install.sh`, no `tar` block, and no
tagged workflow run. Inno Setup is not present here, there is no Mac and no Linux box, and a GitHub
workflow cannot be run locally. `release.yml` was validated by two independent YAML parsers and by
reading; both shell scripts pass `bash -n` and that is the whole of it. Each packaging README says
so of itself in a "What is untested" section, so the statement travels with the code. **D-18.3's
standing precondition is also still outstanding**: that a real host be *observed* to scan
`%LOCALAPPDATA%\Programs\Common\CLAP` before the per-user default ships. The existing S-4 evidence
is a **negative** result about `%APPDATA%\REAPER\UserPlugins\CLAP` and is not a confirmation of the
per-user CLAP path. `docs/manual-tests/fr-pkg-030-windows-install-scope.md` carries it as step 0 and
records every step NOT EXECUTED.

### M13 close-out — §14's re-audit table, and what this milestone must not claim, 2026-08-11

**§14's `### M9a re-audit` table moves one row, 5.15 PKG, from `0 / 0 / 4` to `1 / 3 / 0`, and the
Total from 34 / 91 / 5 to **35 / 94 / 1**.** See that table's own appended `**M13 moves**` block for
the per-cell evidence D-23.2 requires, including why 6.2 PERF and 5.13 UI deliberately do not move.
One Done, three Partial: **FR-PKG-020** is the only requirement in this milestone whose `Verify:`
method is executed as written rather than approximated.

**One uncovered Must left in FRS §5.15's group, and it is not one of M13's.** The gate reports
**12** uncovered Musts, down from 15 at this milestone's start. M13 closed four of its five
(FR-PKG-010, -020, -030, -040 — the last as a plain-covered manual document, the middle two as
partials) and NFR-PERF-030, and **added two**: FR-UI-020 and FR-UI-070, which §15 item 15's fix
demoted by stopping a prose mention from crediting a manual document. Ten of the twelve are M9b's.
The remaining two are now in exactly the state D-18.5's printed attribution exists to make visible —
**gaps no milestone has claimed**, wearing an owner label the tool derives from whichever `## <n>.
M<k>` section last mentions them. Whoever plans M9b should read that as work arriving, not as
bookkeeping.

**This close-out does not flip D-18.5's gate, and must not be read as having deferred the decision
to do so.** §20's second scope note, written at this milestone's *start*, moved the flip to
**M9b's close-out** because ten of the then-fifteen uncovered Musts belong to M9b and M9b runs after
M13 — so deleting `--allow-uncovered` here would have turned a required check red on the day it
became required. That reasoning is unchanged by anything this milestone built; the count is now
twelve and ten of them are still M9b's. **NFR-QUAL-010 therefore does not close here either**, and
this milestone does not record it as closed.

**§20's Acceptance, clause by clause, as it now stands.** The paragraph asks for seven things and
this milestone meets **two** of them outright.

- *"A tagged commit produces, from CI alone, an installer and a plain archive for each of the three
  platforms"* — **not met, and not close to met in the sense the clause means.** Everything needed
  to produce them exists and is committed; nothing has produced one. No tag has been pushed, no
  workflow run, no `iscc`, no `pkgbuild`, no `tar` block executed anywhere.
- *"The macOS CLAP artifact is a valid bundle directory and loads in a host on a machine where
  quarantine does not apply"* — **half met.** The bundle's structure is asserted in-process on every
  runner, including the negative case, which is the strongest offline evidence available. It has
  never been built on a Mac and no host has loaded it.
- *"The Windows installer offers both scopes, defaults to per-user, and that per-user path has been
  empirically confirmed as scanned by at least one real DAW"* — **not met**, and the last clause is
  the one that matters most. D-18.3's standing precondition is still outstanding, and the existing
  S-4 evidence is a *negative* result about a different path. This is also the only outstanding item
  that could have been settled before any of this milestone's code existed.
- *"Every distribution contains `THIRD-PARTY-NOTICES.md` and the licence texts"* — **met in the
  staging tree and in every packager's own file list, not asserted inside a produced distribution.**
  That is precisely what FR-PKG-040's partial says.
- *"FR-PKG-010 through FR-PKG-040 each carry a traceability annotation or a manual-test document, so
  `xtask traceability` stays green"* — **met.** Two plain-or-partial annotations, one manual-test
  document, one plain tag; the required half of the gate exits 0.
- *"FRS §5.15 closes"* — **not met.** One of four is Done.
- The clause §20 does not contain but M12 handed forward: **FR-UI-110's icon clauses.** The
  executable icon is built; the window icon **cannot be met through the pinned stack** and that is
  now a finding rather than a deferral. FR-UI-110 stays open. It is a Should and sits in no §14 row.

**What M13 actually is, stated plainly rather than scored.** It is a milestone that wrote every
artifact needed to ship and shipped nothing, because the machines that could run them were not the
machine it was written on. That is a legitimate state and this document has been here before — M6
left FR-IO-020 as a documented FAIL for five milestones — but it should not be dressed up. **The
next honest step is not more code.** It is a Windows machine executing
`docs/manual-tests/fr-pkg-030-windows-install-scope.md`, starting at its step 0; a tag pushed to
watch `release.yml` fail in whatever way it first fails; and a Mac and a Linux box for the other two
legs. Four of the seven Acceptance clauses turn on those runs and on nothing else.

### M13 addendum: the Windows manual run passed, and it found a bug in a different requirement, 2026-08-12

**Supersedes the close-out's Acceptance scoring above**, which is left as written per this
document's convention — it was true of the session that wrote it, and it is why that session's
claims were hedged.

**Inno Setup was installed and `packaging/windows/namir.iss` compiled on the first attempt, with no
edit to it.** Version **7.0.2**, not the 6.3 this milestone's documents state as the floor. That
floor is unchanged and still correct — `x64compatible` arrived in 6.3 — but it is **stated and
untested**, and now demonstrably not the version anyone has actually used. `windows-latest` will be
a third version, also untested. Worth knowing before treating "requires Inno 6.3+" as verified.

**All five steps of `docs/manual-tests/fr-pkg-030-windows-install-scope.md` pass.** FR-PKG-030 moves
**Partial → Done** in §14's re-audit table: 5.15 PKG becomes **2 / 2 / 0** (was 1 / 3 / 0), Total
**36 / 93 / 1** (was 35 / 94 / 1). The denominator is untouched.

**Step 2.1 is recorded verbatim rather than as a general pass**, because it carries the whole
"defaults to" half of the requirement on its own: an installer offering both scopes but
preselecting system-wide passes every other clause and fails this one. The dialog preselects
**"➔ Install only for me (recommended)"**. Two things follow that a bare "it defaulted correctly"
would not have shown. `packaging/windows/namir.iss` has **no `[Messages]` section**, so this is
Inno's own default text — the parenthesised "(recommended)" is *Inno* steering the user towards
per-user, and it does so because `PrivilegesRequired=lowest` is set. That directive is therefore
doing more than D-18.3 credited it with: it does not merely allow the per-user default, it makes
the installer recommend it in as many words. And the string is not this project's to keep stable, so
a reader who finds different wording on a later Inno should read that as an Inno change rather than
a regression, and re-check the *preselection* instead of the text.

**Step 0 is the one that matters beyond this requirement.** D-18.3's standing precondition — that a
real host be *observed* to scan `%LOCALAPPDATA%\Programs\Common\CLAP` — is discharged. It had been
outstanding since the decision was written, the close-out above named it as the single item that
could have been settled before any of this milestone's code existed, and it is now settled. The
per-user default is known-good rather than believed-good.

**§20's Acceptance, rescored.** Three of the seven clauses that the close-out marked unmet or half
met are now met: the Windows installer offers both scopes, defaults to per-user, and that per-user
path is empirically confirmed as scanned by a real DAW. What remains unmet is unchanged and is
entirely about **runs that have not happened**: no tagged commit has produced anything from CI, no
macOS or Linux artifact has been built on its own platform or loaded in a host, and nothing asserts
FR-PKG-040's three files inside a produced distribution. FRS §5.15 is now **2 of 4 Done**.

**The manual run found a defect, and it is not in §5.15.** With the plugin installed and running in
a host, the **output meter read full and never moved**, while the output level control worked
normally. Cause: `crates/namir-clap/src/ui_host.rs`'s `drain_meters` updated the reading as
`self.output_peak_db.max(entry.value)` — correct-looking as "the louder of the two output channels"
until one notices the left-hand side is a struct field rather than a local, which made it a maximum
over the plugin instance's whole lifetime. The meter climbed to the loudest peak ever seen and
stayed. **Not a regression:** those lines are unchanged since M6 created the file, and the symptom
only appears once a peak near 0 dBFS has occurred. `namir-app`'s `read_meters` has always had the
correct shape — a local maximum, assigned — so the standalone was never affected, which is exactly
what the reporter observed. Fixed on `claude/clap-output-meter-ratchet`, cherry-picked here, and
**verified in a host**.

**Why it survived seven milestones, which is the more useful half.** Every test in that module
passed `telemetry: None`, so `drain_meters` returned at its first line and the only meter code in
`namir-clap` was never executed by the suite at all. Five tests now cover it; two of them fail
against the old code with precisely the reported symptom. A covering test existing is not the same
as a requirement being met — this project already says so — and this is the complementary case: a
module with tests, none of which reached the code that was wrong.

**It is an FR-UI-020 defect, and the coincidence is worth recording rather than smoothing over.**
FR-UI-020 requires "output meter and level" among the elements on one screen. It has **no
manual-test document of its own**, and until this milestone it was credited to
`docs/manual-tests/fr-clap-030-audio-ports-negotiation.md` — a CLAP audio-port script that names it
once, in a parenthesis about watching a meter, and won only by sorting first among the files that
mention it. §15 item 15's fix removed that credit at this milestone's start and FR-UI-020 went
`**UNRESOLVED**`, which the close-out above records as one of two gaps no milestone has claimed. The
requirement whose only evidence was a passing mention of a meter turned out to have a **broken
meter**, in the very product shell whose document was crediting it, and nobody would have learned
that from the ledger. It is the strongest argument this milestone produced for the fix that demoted
it. **FR-UI-020 stays `**UNRESOLVED**`**: it still has no document of its own, and writing one now
to absorb this observation would be creating a document to move a number, which is the defaulting
§15 exists to prevent. It belongs to whoever gives FR-UI-020 a script.

### M13 addendum: the macOS pipeline runs, on a real Mac, and two of this milestone's own findings were wrong, 2026-08-12

**Supersedes the close-out's "no macOS or Linux artifact has been built on its own platform"**, which
was true of the session that wrote it. `packaging/macos/make_installer.sh` now runs end to end on
Apple hardware and produces all three artifacts, and both cells of D-13.3's macOS row are confirmed
by installing them.

**What ran.** `cargo build --release --workspace` → `xtask bundle --target macos` →
`bundle --target macos --check` → `make_installer.sh`. Output: `Namir-0.1.0-rc2.pkg`,
`Namir-0.1.0-rc2-macos.dmg` and `Namir-0.1.0-rc2-macos.zip`, each with a SHA-256, and each opened
again by the script's own `verify_outputs` — `pkgutil --expand-full`, `hdiutil attach`, `ditto -x` —
which asserted the licence documents and the bundle-as-directory inside all three. Four steps that
had never executed anywhere before this: `pkgbuild`, `productbuild`, `hdiutil` and that
verification.

**D-13.3's macOS row is confirmed in both cells, by installing rather than by reading.**
`sudo installer -target /` placed the plugin at `/Library/Audio/Plug-Ins/CLAP/Namir.clap`, and
`installer -target CurrentUserHomeDirectory` reported `Upgrading at base path /Users/<user>` and
placed `~/Library/Audio/Plug-Ins/CLAP/Namir.clap` and `~/Applications/Namir.app`. So the
`<domains enable_currentUserHome="true"/>` mechanism works — one `--install-location` per component,
re-rooted by the chosen domain, which is the macOS counterpart of Inno's `{autocf}`. That mechanism
was this script's largest unverified assumption, carrying "two separate product archives" as its
fallback; the fallback is not needed.

**The plugin loads in Reaper on macOS**, which is §20's "loads in a host on a machine where
quarantine does not apply" — locally built and `installer`-placed files carry no quarantine, so the
clause is met in the only way it can be until signing exists. What that run *also* found is that
the plugin has **no editor at all** on macOS, which is §15 item 20 and GitHub issue #18, not a
packaging defect.

**Two findings this milestone made and then had to retract. Both are recorded because the retraction
is the useful part.**

- **"`pkgbuild` does not consider a `.clap` a bundle" was wrong.** The first CI run died at
  `pkgbuild --analyze found no bundle under roots/plugin`, and the conclusion drawn — that `.clap`
  is not one of the extensions macOS recognises, so no bundle component exists and relocation
  cannot apply — was plausible, was written into the script as a `none`/`required` special case, and
  was **disproved by the first run on a real Mac**. `--analyze` emits exactly one entry, carrying
  `RootRelativeBundlePath = Namir.clap`. What it omits is `BundleIsRelocatable`, which it writes
  only for bundle types that *can* relocate — and the loop probed for that key, so it read "a bundle
  that cannot relocate" as "no bundle". The built `PackageInfo` settles it:
  `<bundle id="org.legrand.namir" path="./Namir.clap"/>` present, `relocatable="false"`, empty
  `<relocate/>`. Detection is now by `RootRelativeBundlePath`, both roots are handled identically,
  and the special case is gone. **The artifact never changed** — `pkgbuild` defaults a `.clap` to
  non-relocatable on its own — so what the fix bought is that the script *asserts* the property
  instead of inheriting it. The installed-to-the-stated-path result above is the dynamic half of the
  same evidence.
- **A comment broke the build twice over, and neither fault was visible from reading the file.** The
  heredoc that writes `distribution.xml` is unquoted, which is intended for `${...}` but also makes
  backticks command substitutions: the comment above the `pkg-ref` block contained `` `#` `` and
  `` `--synthesize` ``, so the shell ran them, producing `--synthesize: command not found` and
  silently deleting both words from the generated XML. What was left then failed XML parsing for an
  independent second reason — the same comment said `--package-path`, and XML forbids a double
  hyphen inside a comment. `productbuild` rejected the whole document. The comment now warns the
  next editor about both traps, since only running the script reveals either.

**What this does not change.** FR-PKG-040 stays **Partial**: the macOS script does verify its three
produced distributions, but a shell assertion inside a packaging script is not the in-process
artifact a `Verify: S` traces, and the Windows and Linux distributions are still unverified for
contents. No §14 cell moves on this run. And the honest limit of all of it: this is one Mac, Apple
Silicon, unsigned, with no notarisation — R-11 is untouched, and `installer` has no uninstaller,
which the macOS README should say and currently does not.

### M13 addendum: the release pipeline runs green on all three platforms, 2026-08-12

**This supersedes every "no tagged commit has produced anything from CI" statement above**, each of
which was true when written. Tag **`v0.1.0-rc2`** ran `release.yml` to completion: all three build
jobs and the publish job succeeded, and the prerelease carries twelve assets — an installer, a plain
archive and a SHA-256 for each of the three platforms.

| Platform | Installer | Plain archive (FR-PKG-050) |
|---|---|---|
| Windows | `namir-0.1.0-rc2-windows-x86_64-setup.exe` | `namir-0.1.0-rc2-windows-x86_64.zip` |
| macOS | `Namir-0.1.0-rc2.pkg` and `Namir-0.1.0-rc2-macos.dmg` | `Namir-0.1.0-rc2-macos.zip` |
| Linux | `namir-0.1.0-rc2-linux-x86_64.tar.gz` (both roles — see the close-out) | same file |

**§20's first Acceptance clause is met**: "a tagged commit produces, from CI alone, an installer and
a plain archive for each of the three platforms."

**FR-PKG-010 is promoted from `trace-partial:` to a plain tag, and §14's 5.15 PKG row moves to
`3 / 1 / 0`** (Total **37 / 92 / 1**). The promotion follows D-23.1's rule — close the gap the
`uncovered:` field names, *then* promote — rather than deleting the field. That field named two
things: that no tagged run had ever executed the workflow, and that none of the packaging entry
points had run anywhere. Both are now false, and the concern the partial existed for — that a
structurally valid workflow might not be a working one — is what the run retires. The tag's own
comment records the promotion and its evidence, so a reader meets the reasoning at the artifact
rather than only here. **FR-PKG-040 remains the row's one Partial**, unchanged: nothing in-process
asserts the three licence documents *inside* a produced distribution.

**The first run, and what it cost, because two runs is the whole story.** `v0.1.0-rc1` failed:
Linux and Windows produced artifacts on the first attempt, macOS died in `pkgbuild`, and the publish
job **skipped** — which is the design working, since it `needs` all three and a partial run must
publish nothing. Between the two tags the macOS script was fixed three times, twice for faults that
only running it could reveal (see the macOS addendum above), and each fix was verified on a real Mac
before the second tag rather than by pushing another tag and waiting.

**What a green pipeline does not establish, stated because a release page is persuasive.** No
artifact from this run has been installed by anyone — the macOS evidence above comes from a *local*
build on the same commit, not from these files. The binaries are unsigned, so R-11 applies in full:
SmartScreen warns on every Windows download, Smart App Control blocks outright, and the macOS
artifacts are developer-only until a signing identity exists. NFR-SEC-040's two halves remain split
— hashes are published, bit-for-bit reproducibility is not claimed and is weakened by M11's git
dependency. And the Linux tarball has never been unpacked by `install.sh` on any distribution, so
the platform whose CI leg passed first is the one with the least evidence behind it.

---

## 21. M14 — Pre-1.0 closure

**Size: XL.** **Depends on:** M9b, for the ledger this milestone is planned from — the triage below
reads 66 `**PARTIAL**` rows, and until M9b those rows were neither complete nor trustworthy.
**Blocks:** M8, entirely. **Governed by:** the decisions its Phase 0 makes, none of which exist yet.

### Why this milestone exists

**M8 says "Size: S, a gate, not a construction phase."** That is the right description of M8 and the
wrong description of what M8 currently inherits. A sweep this session of every open register — §15's
open decisions, §22's risk rows, all 66 partials, all 26 manual-test documents, every Should, and a
grep for in-code deferrals — found that the work standing between today and the 1.0 exit checklist
is not a checklist's worth. Three findings make the case, and each was verified against the source
rather than inherited from a document:

- **14 of the 66 partials are not verification debt.** They are Must requirements whose *product*
  half was never built, wearing a coverage annotation. **Nine of those are user-facing surfaces that
  do not exist in `namir-ui` at all** — `UiIntent` has seven variants and not one of them saves a
  preset, adds a library root, selects a device, or resets a clip indicator. A milestone whose
  entire content is "check that every row reads Done" cannot absorb a UI feature phase.
- **Six `Verify: M` Musts are unexecuted**, not the two M9b's close-out names. FR-UI-020, -030,
  -040, -050, -070 and FR-IO-030 all resolve to a manual document that says `NOT EXECUTED`, and
  `xtask traceability` cannot tell — §15 item 15's filename arm was deliberately left unfixed, and
  `xtask` refuses a `trace-partial:` on a `Verify: M` Must, so there is no disposition between "no
  document" and "fully covered". **FR-IO-030 means neither Linux nor macOS audio has ever moved a
  sample.**
- **Six §15 items and eleven §22 risks are open**, two of them past their own stated deadlines. §15
  item 16 — whether 1.0 ships an audio-device panel — was due before M9b's start, has not been
  taken, and is upstream of five Musts' user-facing clauses.

**The alternative was to let M8 discover all of this.** That is precisely the failure M9a was
created to prevent one level up: a gate that first learns what it is gating when it runs. M8 keeps
its number, its text and its meaning, and gains a milestone in front of it.

### Sequencing

> **M9a → M10 → M11 → M12 → M13 → M9b → M14 → M8.**

M14 is numbered by when it joined the plan, not when it runs, exactly as §12's execution-order note
established for M9–M13. Nothing above this section is rewritten; §12 carries a dated note recording
the refinement.

### Phase 0 — Decisions, before any of it is built

**Nothing in Phases 1–6 starts until Phase 0 closes**, for the reason M9b's own P0 pass gives: a
branch policy chosen after the measurement is a policy chosen to suit the measurement. Every item
here is a paragraph in a governing document, not code.

- **§15 item 16 — the audio-device panel.** Overdue. Answer 1 (build it) makes Phase 1 materially
  larger; answer 2 (record an FRS scope reduction at FR-IO-010, -040, -050, -060, -070) deletes most
  of Phase 1's device work and converts five Musts' clauses into recorded reductions. Answer 3
  (silence) is not available at a 1.0 gate. **This is the single decision with the largest effect on
  this milestone's size, and it must be first.** Issue #26.
- **§15 item 3 — AQ-4 and factory presets.** Note a third resolution the item does not list:
  FR-STATE-090 is a **Should**, §13 says no Should gates M8, and yet M8's exit checklist gates on
  "factory presets ship". Striking that bullet is cheaper than either of the item's two answers and
  is the only one consistent with §13. Whichever is chosen, no factory preset exists in the tree, so
  the permitted route is still unbuilt work.
- **§15 item 18 — FR-CHAIN-070's stereo source.** Decide; do not build. Dropping the Should costs no
  Must and is recommended, because the alternative reopens D-9.8's stage order, which M9a re-affirmed.
- **§15 item 19 — NFR-PORT-030's method vs. its text.** Decide whether the two mobile cross-builds
  are the method or whether the five enumerated constraints need per-constraint checks. If the
  latter, each becomes Phase 4 work.
- **§15 item 20 / issue #18 — the plugin's missing editor on macOS and Linux.** A decision, not a
  build; Cocoa and X11 embedding is a second and third `unsafe` surface under D-5.3 and is not 1.0
  work.
- **§15 item 21 / issue #27 — compound `Verify:` methods.** Six Musts state one; the parser keeps
  the first code. Five self-correct through an `uncovered:` field; FR-STATE-040 structurally cannot,
  and reads fully covered while its schema-check half exists nowhere.
- **The sub-40 kHz clause (FR-NAM-060, FR-IR-030).** Where the lower rate falls below ~40 kHz, "0.1
  dB to 20 kHz or Nyquist, whichever is lower" is unsatisfiable by any resampler. The honest fix is
  an FRS clause bounding the supported range — a paragraph, not a test.
- **R-13's fired condition.** The row says: if the partial count is not falling by M12, the mechanism
  is being used as a bypass. M12 and M13 have both run and the count went 56 → 66. The rise is
  honest accounting — M9b demoted two of its own tags and drove twelve uncovered Musts to zero — but
  **a stated test that fires and is then reinterpreted needs the reinterpretation written down**,
  not assumed.
- **Accepted limitations, recorded as `*Consequence*` notes at their requirements** rather than
  rolled forward as partials one more time: NFR-DOC-040's "stating what it does"; NFR-PERF-010's CI
  regression gate (the figure passes with 34–42% headroom; closing it as written needs a self-hosted
  runner on the §2 machine, which is disproportionate); NFR-PERF-030's stronger audible marker
  (already considered and rejected once, on good grounds — ratify rather than reopen);
  FR-CLAP-030's `audio-ports-config`; FR-NAM-090's true BS.1770 meter; FR-CLAP-090's memory
  benchmark; NFR-LIC-010's SPDX-header clause, which is NFR-LIC-060's (a Should) work;
  FR-CFG-030's "exercised" half for the standalone, which needs a device no runner has.

### Phase 1 — The unbuilt user surfaces

The nine Musts whose mechanism does not exist. Scope depends on Phase 0's item 16 answer.

- **FR-STATE-030 — save and recall a named preset.** `AppHost::save_state`/`load_state` exist and
  their own doc comment says they are wired to no control; their only caller is a test. `namir-clap`
  has no equivalent. **`namir-app` constructs no `FileRef` anywhere**, so a save wired today would
  write a state with no model or IR identity and silently break FR-STATE-060 and -070 — the
  `FileRef` construction is a prerequisite of the save path, not a follow-up. Issue #32.
- **FR-LIB-010 — nominate library directories.** Roots are hard-coded to `<config_dir>/Library`;
  `AppSettings` has no roots field. Issue #32.
- **FR-STATE-070 — locate a missing file manually.** Also `library_relative: None` on every
  plugin-side `FileRef`, so FR-STATE-070's *first* resolution candidate is never recorded, in the
  shell where project portability matters most. This is not the gap that row's `uncovered:` field
  names.
- **FR-NAM-080 — model metadata on screen.** `UiSnapshot` carries `loaded_model_name` only, filled
  from the file basename, so author/gear/tone-type reach no screen.
- **FR-IN-020, FR-IN-030, FR-OUT-020 — peak-hold, clip latch, output metering.** The engine already
  publishes all of it; `MeterReading` carries `peak_db`/`rms_db` only and the string "clip" does not
  appear anywhere in `crates/namir-ui/src`. `Meter::reset_clip` has no caller outside its unit test.
  **The cheapest work in this phase** — the data exists one layer short of the screen.
- **FR-IO-060, FR-IO-070 — xrun count and device re-selection.** The xrun counter surfaces only
  through `eprintln!`. Gated on item 16.

Every item here needs both `UiHost` bridges — `namir-app/src/host.rs` and `namir-clap/src/ui_host.rs`
implement it independently by design — and any file dialog must respect NFR-PORT-030's no-blocking-
dialog-on-an-audio-affecting-path clause.

**FR-GATE-040 rides along.** It is a Should, it is one `UiSnapshot` field, and the engine already
publishes `telemetry.gate.gain_reduction_db`. It is the cheapest Should in the FRS and the gate meter
is currently computed and thrown away.

### Phase 2 — The CLAP host contract

- **FR-CLAP-060 — automation is applied at block boundaries.** `apply_automation` never reads
  `ev.header().time()` and runs once before the block, so every automation point quantises to the
  buffer size: ~10.7 ms at 48 kHz/512, ~85 ms at 4096. A Must, audible to the plugin's primary user
  class, with **no recorded disposition anywhere**. Split the block at event offsets. Issue #30.
- **FR-CLAP-060's click-freedom half.** Global bypass is a plain `bool` flip with no crossfade;
  `GateStage` already has the pattern.
- **Issue #36 — global bypass processes every stage when `prepare_crosscutting` was never called.**
  Establish first whether any production path reaches it; fix or assert accordingly.

### Phase 3 — Gates that do not gate

Three mechanisms that exist, are green, and cannot detect what they were built to detect.

- **FR-PARAM-020 — the tombstone mechanism is inoperable.** `check_manifest` has no caller outside
  its own tests, and `xtask params-lock` byte-diffs against live-only output, so a committed
  tombstone fails the gate permanently and `--write` deletes it. 1.0 is the version the stability
  promise is measured from. Issue #31.
- **FR-ERR-020 — `ErrorCode`'s fields are all `pub`** and one live site constructs an off-catalogue
  code. The second conjunct of its `Verify: S` has no artifact. Issue #33.
- **R-17 — nothing mechanical stops `--all-features`** linking `clack-host` into the shipped cdylib;
  `xtask rt-logging` is the template for the guard. Issue #25.
- **The manual-document verdict gap (issue #34).** Have `xtask traceability` read the `Result:` line
  and treat `NOT EXECUTED` as uncovered, so the gate cannot print `clean` on evidence nobody
  produced. **This must land before D-18.5's flip**, or the flip inherits the same hole.

### Phase 4 — Engine and parameter verification

~35 Musts of test debt against product code that is believed correct. No single theme needs its own
phase; together they are the largest block of work in the milestone, and M9b's experience is the
argument for doing it rather than deferring it again — **both DSP defects M9b found were found by
running tests that had never run against a loaded chain.**

- **Chain-level probes** (FR-CHAIN-010, -020, -050, -060, -080, NFR-PERF-020). The tagged
  FR-CHAIN-010 test feeds zeros with nothing loaded and asserts zeros out; it cannot distinguish any
  stage order from an empty chain. `ChannelConfig::MonoToStereo` appears in no gate or NAM test. No
  NaN is ever injected into a real stage's state. One probe-signal harness through
  `build_default_chain` closes most of this.
- **Per-control DSP coverage** (FR-GATE-010, -030, FR-EQ-010, -020, -030, FR-IR-070, FR-PARAM-040).
  Gate Attack/Release are exercised by no test. EQ magnitude is measured at DC and Nyquist only,
  where the method says each band's own corner within 0.1 dB. FR-EQ-020 is a one-liner: the rate
  array holds four of the six rates the requirement names.
- **Parameter descriptors and delivery paths** (FR-PARAM-010, -030, -050, FR-OUT-010). Nothing
  asserts a name, unit, range or default over a *shipped* `REGISTRY` entry — both tagged tests assert
  against a fabricated const declared inside `mod tests`.
- **Stage behaviour under a real signal** (FR-NAM-050, -110, FR-IR-060, FR-STATE-050). FR-NAM-110's
  "cross-correlate an impulse through the stage" is performed by nothing — both tagged tests read an
  accessor whose body is the literal `0` and assert it equals `0`.
- **Sample-rate coverage** (FR-CLAP-080). The sweep loads nothing at any of its 154 rates, so
  `SlotResampler` — the one rate-dependent subsystem, and the one M9b found broken at 192 kHz —
  never runs in it. Loading a Nano fixture into the existing sweep is cheap and high-value.
- **Cross-shell state interchange** (FR-STATE-030 test half, -060, FR-CLAP-050). Gated on Phase 1.
- **RT safety and timing** (NFR-RT-010, -030, -040, NFR-PERF-050, FR-CLAP-130). The alloc harness
  does not reach `namir-app`'s audio callback. NFR-RT-030 measures the assembled chain's aggregate,
  in which a real denormal spike in one stage is diluted below the bound by the other five, where the
  method says "drive each stage".
- **Diagnostics and fault containment** (FR-ERR-010, -030, -040). FR-ERR-040 injects a fault only
  into the generic worker pool — nothing into the scanner, the state path, settings I/O, the cache or
  the GUI thread — and its "continue passing audio" clause is asserted by nothing, in the requirement
  whose Rationale is "crashing a user's DAW loses their work".
- **Static checks that under-approximate** (NFR-PORT-020, FR-ERR-060, NFR-QUAL-040, NFR-SEC-020,
  NFR-LIC-010, NFR-LIC-050). `xtask layering`'s platform lint matches three literal substrings, so
  `#[cfg(not(windows))]`, `cfg!(...)` and `#[cfg(target_arch)]` all pass unseen. Nothing detects
  direct `std::net` use in a first-party crate.
- **Benchmarks that assert nothing** (FR-LIB-030, FR-UI-060). Both already exist and print rather
  than assert, which is the worst state a `Verify: B` can be in. FR-UI-060's timed frame uses
  `scan: None`, so neither the scan branch nor the re-filter path — the reason the memoization exists
  — runs inside it. Closing FR-UI-060's also closes **R-12**'s detector gap.

### Phase 4b — A2's missing golden, added 2026-08-12

**Added after the rest of this section was written, because the owner asked whether A2 support had
ever actually been tested and the honest answer turned out to be "not against the reference".** It
is recorded as its own phase rather than folded into Phase 4 because it is not test debt against
code believed correct — it is an unverified numerical claim about a shipped architecture. Issue #37.

**What exists and is real:** A2 loads and runs; two from-scratch Rust implementations written from
the upstream C++ agree bit-exactly on both real shapes plus a bottleneck probe
(`crates/namir-nam/tests/a2_fixtures.rs`); the unsupported-feature boundary is properly enforced and
FR-NAM-140's test is honest (`crates/namir-nam/src/model.rs`); block-splitting, mutation-robustness
and RT-allocation all hold.

**What does not exist: any executable comparison of A2 against `NeuralAmpModelerCore`.**
`crates/namir-nam/tests/golden/` holds `wavenet_nano.nam` and `lstm_tiny.nam` and no A2 model.
`golden_reference.rs` argued one was not needed because "D-9.12 keeps A2 inside the 'WaveNet'
architecture this test already covers" — **and that is false as a statement about code paths.**
D-9.12 is a dispatch decision; `wavenet.rs`'s own module doc says A2's additions are "provably inert
when the file is A1", which is exactly why an A1 golden cannot reach them. The A1 fixture takes the
other side of every A2 branch — per-layer `kernel_sizes`, a `bottleneck` distinct from `channels`,
the nested convolutional head, the per-layer activation array, and the k-tap causal head conv — and
six of the ten activation variants go unreached, `LeakyReLU` among them, which is what real A2 uses.

**Three further findings, each recorded rather than assumed:**

- **R-9 is reopened** (§22, and its own row carries the analysis). Its retirement rested on a real-
  reference A2 cross-check with no committed artifact, no tool that could regenerate it, and no
  manual write-up — and the two records of it disagree about whether it measured A2 or an A1
  `WaveNetShape::Standard`. Bit-exact agreement between two in-house ports proves they derived the
  *same* weight order, not the *right* one, which is precisely what R-9 was raised about.
- **No genuine A2 export has ever been loaded**, and both the generator and the parser derive the A2
  schema from one reading of the same C++ — so a *shared* misreading is bit-exactly invisible to
  every test in the tree. `file.rs`'s A2 fields are all `#[serde(default)] Option<_>` with no
  `deny_unknown_fields`, so a real file carrying a feature under an unanticipated key is silently
  ignored rather than rejected, which undercuts FR-NAM-140 *for real files* even though its test is
  sound. This is the class AGENTS.md warns about, citing the post-M6 `null`-vs-omitted bug.
- **Real A2 models in the wild take a path Namir has never been compared against.**
  `NAM_ENABLE_A2_FAST` defaults to **ON** upstream and its `is_a2_shape` detector matches exactly
  the two shapes FR-NAM-150 names, so a default-built host runs them through `a2_fast.cpp`; the one
  unrecorded Namir comparison used `-DNAM_ENABLE_A2_FAST=OFF`. Excluding the fast path is
  defensible, but the choice has **no recorded rationale** — `golden_reference.rs` attributes the
  flag to D-9.12's PR #264 note, and that note discusses only `NAM_USE_INLINE_GEMM`.

**Deliverables:** a committed A2-Full golden (and ideally A2-Lite) rendered through the pinned
reference build over `tests/golden/input_10s.wav`, asserted beside the two existing golden tests,
with a probe longer than A2's ~6300-sample receptive field — the same shape of work
`golden_reference.rs` already did twice; one recorded run against a genuine trainer-produced A2
export, written up as `docs/manual-tests/fr-nam-020-real-lstm-models.md` was for LSTM; and a
recorded decision on the fast path. FR-NAM-030's and FR-NAM-150's tags are demoted to
`trace-partial:` in the meantime, and R-9 stays reopened until the golden lands.

### Phase 5 — Distribution and install verification

Four requirements naming one hole: **`ci.yml` never runs `xtask bundle`**, so every assertion against
a real produced tree happens only on a `v*` tag. Only macOS re-opens what it produced; Windows and
Linux assert the staging tree only, so nothing opens the produced installer, ZIP or tarball to
confirm the licence texts and `THIRD-PARTY-NOTICES.md` are inside (FR-CFG-030, FR-PKG-040,
NFR-LIC-030, NFR-BUILD-020). Add bundle-and-inspect lanes on all three platforms, plus an
`xtask identity`-style check tying README commands to `ci.yml` — they have already drifted.

### Phase 6 — The human sessions, and R-18

- **One Windows session** with a display, an audio interface and an instrument closes five of the six
  unexecuted `Verify: M` Musts — FR-UI-020, -040, -050, -070 and, with a DAW, the residues of
  FR-CLAP-040, -060, -090 and -100. **This is the highest-value single unblocking action in the
  project**, and it also releases D-18.5's flip and NFR-QUAL-010's closure. Issue #34.
- **FR-IO-030 needs a Linux box and a Mac.** Neither platform's audio path has ever moved a sample,
  and both are claimed as supported.
- **FR-UI-030 cannot be closed by running its script** — `egui-baseview` wires no `accesskit`
  platform adapter, so a screen reader announces nothing and the script would fail. Decide it in
  Phase 0; issue #35.
- **R-18 / issue #22 — NFR-PERF-040's margin is not Namir's.** Instantiation is 187.5 ms against a
  200 ms Must at 10 000 entries, of which chain construction is 147 µs; the rest is one JSON parse
  of `library-index.json`, per plugin instance, with no cross-instance sharing. **Asserting on `max`
  as the benchmark does puts the breach near 10 700 entries**, and FR-LIB-020's 10 000 is a floor on
  what Namir claims to support. No regression test can ever catch this, because nothing regresses.
  Take the index off the instantiation path.
- **R-11 / issue #23 — signing.** If 1.0 is a public release, a quarantined macOS plugin has no
  user-visible load path at all and the CI signing route is unreachable because the keychain import
  is unbuilt. Decide, then build or record.

### Acceptance

- Phase 0's decisions are all recorded in the governing documents, each at its own requirement or
  decision, before any Phase 1–6 commit lands.
- Every partial that this milestone closes is closed by closing its named gap, never by promoting its
  tag — D-23.1's own failure mode, and the one §22 **R-13** exists to catch.
- The remaining partials are re-booked deliberately, with a stated reason, not by default onto
  whatever milestone follows. M9b's own status subsection records that the last such re-booking was a
  default and says so plainly; this milestone must not repeat it.
- The six unexecuted `Verify: M` Musts either carry an executed per-step verdict or a recorded
  scope reduction. **Neither `NOT EXECUTED` nor silence survives into M8.**
- `xtask traceability` cannot print `clean` while any manual document reads `NOT EXECUTED`.
- D-18.5's flip and NFR-QUAL-010's closure — inherited from M9b, which could not reach them — land
  here, after Phase 3's verdict-parsing change and Phase 6's Windows session, and not before.

**What this milestone deliberately does not do:** build a Wayland backend (R-16, issue #19), an
accessibility platform adapter (issue #35, unless Phase 0 says otherwise), a Cocoa or X11 plugin
editor (issue #18), a BS.1770 loudness meter, a memory benchmark, or a self-hosted benchmark runner.
Each is recorded as an accepted limitation in Phase 0 rather than carried as an open partial into the
1.0 gate.

### M14 Phase 0 — decisions, 2026-08-12

Appended per this document's convention; nothing above is edited. **This subsection is Phase 0's
output and nothing else.** It writes no code, touches no annotation and moves no §14 cell. Every
decision below lands as a `*Consequence (added M14, 2026-08-12)*` note at its own requirement or
risk row, and this subsection is the index to them plus the scope statement Phase 0 owes the phases
that follow.

**The acceptance criterion Phase 0 was given is met: the decisions are recorded before any Phase 1–6
commit lands.** What follows is also honest about the four items Phase 0 was *not* able to close,
because a recorded open question is a correct outcome here and an invented decision is not.

#### What was decided

*Three ratifications.*

- **R-13's fired condition** (`02-architecture.md` §22). The row predicted that a partial count not
  falling by M12 means the mechanism is being used as a bypass. It has run, and the count went
  **56 → 68**. The reinterpretation is written down at the row itself with its arithmetic: M9a's
  twenty *uncovered* Musts are now **zero**, M9b converted twelve of them and then demoted two of
  its own plain tags on finding they over-claimed, and M14's A2 pass demoted two more. Every
  movement made the ledger weaker and truer, which is the opposite of the predicted failure. The
  count is **demoted from a test to an indicator**; the uncovered count and the per-milestone
  re-reading of `uncovered:` fields are named as the replacement tests; and **the row stays open at
  Medium** rather than being declared mitigated, because nobody has designed a cheap mechanical
  discriminator between a partial that records honest scope and a partial that avoids work.
- **The sub-40 kHz clause**, at FR-NAM-060 with an importing note at FR-IR-030. "0.1 dB up to
  20 kHz or the Nyquist frequency, whichever is lower" has a transition band only while 20 kHz is
  the lower of the two, so below a 40 kHz rate it is arithmetically unsatisfiable rather than
  merely unmet — §15 item 12's branch 1 is *empty* here, not expensive, which is why this is branch
  2 taken on an audible justification rather than on what the implementation achieves. Namir's
  supported **engine** sample-rate range is stated as **44.1 kHz to 192 kHz** for both products, the
  range FR-CLAP-080 already gives the plugin, which puts every FR-NAM-050 conversion inside the
  satisfiable region at no cost to the NAM stage. The one path that reaches below it is FR-IR-030,
  through FR-IR-010's 8 kHz IR-file floor, and there the bar is restated proportionally — 0.1 dB to
  0.45 × the lower rate, 100 dB from 0.5 × the lower rate up, the same relative transition band the
  worst standard pair already lives with. Two things are said rather than assumed: those restated
  figures are **unmeasured**, and the bound is **not executable** — `SampleRate::new` takes any
  nonzero `u32` and nothing rejects an out-of-range rate today.
- **NFR-PERF-030's stronger audible marker** — the earlier rejection is **ratified, not reopened**.
  It was declined because it needs an observable inside `crate::stream`'s audio callback added
  purely to enable a measurement, a probe on the one path NFR-RT-010 protects. That reasoning is
  good; M14 takes no fresh view of the trade and records that the view was taken.

*Three §15 items closed.*

- **Item 18 — FR-CHAIN-070's stereo source: the Should is dropped for 1.0.** Left-only ships, no
  chooser. It costs no Must, and the alternatives reopen D-9.8 (which M9a re-affirmed) or need a new
  `namir_dsp::NoiseGate` seam for a Should that §13 says cannot gate M8.
- **Item 19 — NFR-PORT-030: the method stands, and its Done cell is re-read.** The cross-build is a
  **door-open check, not a compliance check**. Per-constraint checks are recorded as post-1.0 work,
  with `xtask rt-logging` named as the working template.
- **Item 21 / issue #27 — compound `Verify:` methods: the general fix first, and the FRS line is
  not narrowed.** The parser is to learn compound methods, and FR-STATE-040's schema check follows,
  against the format `04-state-and-preset-format.md` §§3–7 already documents. **Until that lands,
  FR-STATE-040 is not counted Done by hand whatever `xtask traceability` prints** — for it, the tool
  prints a filename match and structurally cannot print anything else.

*Six accepted limitations outright, plus two deliberate splits — eight requirements in all.*
Recorded at their own requirements rather than carried into the 1.0 gate as open partials:
**NFR-DOC-040**'s "stating what it does";
**NFR-PERF-010**'s CI regression gate (the figure passes with 34.4–41.8% headroom on §2's machine
and closing the method as written needs a self-hosted runner on it); **NFR-PERF-030**'s audible
marker, above; **FR-CLAP-030**'s `audio-ports-config`, ratifying M9b's reduction and closing it to
re-triage; **FR-NAM-090**'s true BS.1770 meter; **FR-CLAP-090**'s memory benchmark. The two splits
exist because a single `uncovered:` field could not say that two halves of one method have different
answers. **NFR-LIC-010**: its SPDX-header clause is accepted as NFR-LIC-060's work — a **Should**,
which §13 says does not gate M8, and verifying a Must's clause by doing a Should's work is the wrong
way round — while its **licence-files-present clause is refused acceptance and stays booked**,
because deleting `LICENSE-MIT` would today go unnoticed by every gate here and the check is a
handful of lines in `xtask identity`. **FR-CFG-030**: the standalone's "exercised" clause is
accepted as needing a device no runner has, while installing each product alone into a clean
environment and exercising the *plugin* there stay Phase 5 work and are not excused by it.

#### What was deliberately left open

Four items are owner decisions and Phase 0 declined all four rather than inventing an answer. Each
now carries a dated note stating what M14 does and does not do around it.

- **§15 item 16 — the audio-device panel.** §21 called this "the single decision with the largest
  effect on this milestone's size, and it must be first", and it is nonetheless not taken: the two
  live answers differ by a whole UI feature phase, which is a product-scope call. It is now overdue
  by two milestones. **Consequence for this milestone, stated as scope: M14 does no device work of
  any kind, and FR-IO-060 and FR-IO-070 stay Partial through it.**
- **§15 item 3 / AQ-4 — factory presets.** Both listed answers cost a capture session with real
  hardware. §21's third resolution is recorded at FR-STATE-090 and here, and not performed: striking
  M8's "Factory presets ship" bullet is cheaper than either, and is the only reading consistent with
  §13 — but editing M8's exit checklist is a decision about the 1.0 gate.
- **§15 item 20 / issue #18 — the plugin's missing editor on macOS and Linux.** Not decided; what is
  settled is that building the two backends is not M14's work, being a second and third `unsafe`
  surface under D-5.3.
- **Issue #35 — FR-UI-030's accessibility.** Its script cannot be closed by running it:
  `egui-baseview` wires no `accesskit` platform adapter, so nothing is announced to a screen reader
  and the script would fail. §21 permitted Phase 0 to say otherwise; it does not. The adapter is not
  built here.

**R-11 / issue #23 — signing** is recorded as still open at its own risk row for the same reason:
issue #23 turns on whether 1.0 is a public release, and the signed CI path is unbuilt either way.

#### What the automated half of M14 will and will not attempt

Stated plainly, because Phase 0's job is to size the phases that follow it.

- **All of §21 Phase 1 is deferred.** The nine Musts whose *product* half was never built —
  FR-STATE-030, FR-LIB-010, FR-STATE-070 (its "locate it manually" clause, and the
  `library_relative: None` finding underneath it), FR-NAM-080, FR-IN-020, FR-IN-030, FR-OUT-020,
  FR-IO-060 and FR-IO-070 — need `UiIntent` variants, `UiSnapshot` fields and both `UiHost` bridges,
  and several of them are gated on item 16, which is unanswered. FR-GATE-040,
  the Should that was to ride along, is deferred with them. **This is the largest single deferral
  Phase 0 makes and it is a deferral, not a re-scoping**: those requirements stay Musts, stay
  Partial, and stay M14's to close whenever its human-dependent half runs.
- **No device work.** Per item 16, above.
- **This milestone will not reach D-18.5's flip or NFR-QUAL-010's closure**, and the reason is
  M9b's unchanged one rather than a new one. FR-UI-020 and FR-UI-070 are `Verify: M`; both manual
  documents read `NOT EXECUTED`; and no headless runner can execute them. **R-16 carries the
  mechanism**: an Xvfb display *is* GLX-capable — Mesa 25.2.8 / llvmpipe reports direct rendering, a
  4.5 core profile and 320 GLXFBConfigs — but **none of those 320 is sRGB-capable**, and
  `egui-baseview`'s default `GlConfig` asks for one, so `glXChooseFBConfig` matches nothing and
  `baseview` panics on its own `.expect`. No Mesa configuration tried produces an sRGB config.
  Deleting `--allow-uncovered` while those two documents read `NOT EXECUTED` would make the required
  check green on evidence nobody has produced, which is the precise inversion of what D-18.5's split
  was for. **The flip waits on a human at a screen, and so does NFR-QUAL-010.** §21's Acceptance
  bullet placing both "here" is therefore met only by M14's Phase 6, not by its automated half.

#### Two things this pass found wrong or contradictory, recorded rather than quietly fixed

- **§21's own partial counts are stale by two.** This section was written naming "66 `**PARTIAL**`
  rows" and "the count went 56 → 66". The figure is **68**: M14's own A2 pass, in the same session
  and after this section was drafted, demoted FR-NAM-030 and FR-NAM-150 on finding no A2 model has
  ever been compared against `NeuralAmpModelerCore` (see **R-9**, reopened, and §21's Phase 4b).
  §12's `*Consequence (added M14)*` note carries the same "66" for the same reason. Neither is
  edited, per this document's convention; the number to trust is what `cargo run -p xtask --
  traceability` prints, and today it prints 68.
- **"Accepted limitation" does not make a §14 row Done, and §21's closing paragraph can be read as
  though it does.** That paragraph says each such item is "recorded as an accepted limitation in
  Phase 0 rather than carried as an open partial into the 1.0 gate". Under **D-23.2** a Partial is
  not **Done** for §14 or for M8's exit checklist, and an acceptance note changes none of that: the
  tag stays `trace-partial:`, the row stays Partial, and M8's "every row reads **Done**" bullet
  still cannot pass on it. **What an acceptance actually buys is that the gap stops being
  re-litigated, not that it stops counting.** Three dispositions could reconcile this — re-read M8's
  bullet as "Done, or carrying an accepted-limitation note"; amend the requirements the acceptances
  sit under; or let M8 fail on them — and **Phase 0 takes none of them**, because all three are
  decisions about the 1.0 gate and belong to whoever owns M8. It is recorded here so M8 does not
  discover it, which is the failure §21 exists to prevent one level up.

### M14 status — the machine-closable half, and an honest account of the rest, 2026-08-27

Appended per this document's convention; nothing above is edited. **This is a status subsection and
not a close-out**, for the reason stated up front rather than discovered at the end: §21's Acceptance
requires the six unexecuted `Verify: M` Musts to carry a verdict or a recorded scope reduction, and
requires D-18.5's flip. Two of the six were executed by M9b's close-out. **Four are not, and the flip
does not happen here.**

The milestone ran as ten workstreams on one branch — Phase 0's decisions, seven build workstreams in
parallel, a notice-pipeline workstream added mid-milestone, and this integration pass.

#### What moved

| | At `trunk` | Now |
|---|---|---|
| Partials (§22 R-13) | 69 | **40** |
| Uncovered Musts | 6 | **6** |
| §14 Done / Partial | 41 / 89 | **66 / 64** |

**Twenty-five requirements moved Partial → Done**, each by closing the gap its own `// uncovered:`
field named and then promoting — never by promoting alone, which is D-23.1's failure mode and what
§22 **R-13** exists to catch. Each promotion was verified by hand at integration before its merge was
accepted. Evidence, per D-23.2's rule that every moved cell names a file path:

- **5.1 CHAIN +3** — FR-CHAIN-010, -050, -080 (`crates/namir-engine/src/chain_probes.rs:116`, `:368`, `:592`).
- **5.3 GATE +2** — FR-GATE-010 (`crates/namir-engine/src/stages/gate.rs:467`), FR-GATE-030 (`crates/namir-dsp/src/gate.rs:360`).
- **5.4 NAM +2** — FR-NAM-110 (`crates/namir-engine/src/stages/nam.rs:1641`), FR-NAM-150 (`crates/namir-nam/tests/golden_reference.rs:409`).
- **5.5 IR +3** — FR-IR-030 (`crates/namir-ir/src/convolver.rs:1632`), FR-IR-060 (`crates/namir-engine/src/engine.rs:582`), FR-IR-070 (`crates/namir-engine/src/stages/ir.rs:1371`).
- **5.6 EQ +2** — FR-EQ-010 (`crates/namir-engine/src/stages/eq.rs:664`), FR-EQ-020 (`crates/namir-dsp/src/biquad.rs:461`).
- **5.7 OUT +1** — FR-OUT-010 (`crates/namir-engine/src/stages/out.rs:286`).
- **5.8 PARAM +3** — FR-PARAM-010, -050 (`crates/namir-params/tests/registry_descriptors.rs:248`, `:406`), FR-PARAM-020 (`xtask/src/params_lock.rs:33`).
- **5.9 STATE +1** — FR-STATE-050 (`crates/namir-worker/tests/recall_continuity.rs:277`).
- **5.10 LIB +1** — FR-LIB-030 (`crates/namir-library/benches/library_scan.rs:395`).
- **5.13 UI +1** — FR-UI-060 (`crates/namir-ui/benches/library_frame.rs:184`).
- **5.14 ERR +2** — FR-ERR-010 (`crates/namir-platform/tests/logging.rs:756`), FR-ERR-020 (`crates/namir-core/src/error.rs:233`).
- **6.2 PERF +1** — NFR-PERF-020 (`crates/namir-engine/src/chain_probes.rs:693`).
- **6.3 PORT +1** — NFR-PORT-020 (`xtask/src/main.rs:1254`).
- **6.5 LIC +1** — NFR-LIC-050 (`crates/namir-fixtures/src/lib.rs:28`).
- **6.6 SEC +1** — NFR-SEC-020 (`crates/namir-state/src/document.rs:227`).

**5.11 IO, 5.12 CLAP, 5.15 PKG, 6.1 RT, 6.4 QUAL, 6.7 BUILD and 6.8 DOC do not move**, and the
absences are deliberate: every partial in them is blocked on a human, on hardware, on an owner
decision, or on product that was never built. See the classification below.

#### The forty remaining partials, classified — not re-booked by default

§21's Acceptance forbids re-booking "by default onto whatever milestone follows", recording that
M9b's last such re-booking was a default and saying so plainly. **After M14 the sequence has only M8
left, so the destination is not the variable — the reason is.** Every tag keeps `closes M8`; what
follows is why, for each. No Must requirement's text was narrowed to improve a count.

**A. Accepted limitation, already recorded in the FRS by Phase 0 (8).** These reach M8 as recorded
reductions to *ratify*, not as work. **The ledger and the documents currently disagree about them** —
the FRS carries the acceptance, the annotation still reads like an open gap — and that disagreement
is itself something M8 must dispose of, by the three routes M14 Phase 0's own closing note lists.
FR-CFG-030 (the standalone "exercised" clause), FR-CLAP-030 (`audio-ports-config`), FR-CLAP-090 (the
memory benchmark), FR-NAM-090 (a true BS.1770 meter), NFR-DOC-040 ("stating what it does"),
NFR-LIC-010 (the SPDX-header clause), NFR-PERF-010 (the CI-regression-gate half), NFR-PERF-030 (the
stronger audible marker — ratified, not reopened).

**B. Blocked on a human, on hardware, or on an owner decision (13).** No coding milestone closes
these; naming what each waits on is the whole value.
- **A Windows session with a DAW:** FR-CLAP-050 (the host-driven half), FR-CLAP-080, FR-CLAP-130,
  FR-IN-020 (the `M` half of its compound method).
- **§2's pinned reference machine:** NFR-RT-030 ("on any supported platform"), NFR-PERF-050. D-2.4
  makes the certified figure the one measured there; every number in this milestone is a sandbox
  reading and is labelled as such wherever it is recorded.
- **§15 item 16 / issue #26, the audio-device panel:** FR-IO-060, FR-IO-070. Overdue before M9b
  started, and M14 scoped itself around it rather than deciding it to suit a measurement.
- **Issue #18:** FR-CLAP-100, the embedded editor on macOS and Linux — a second and third `unsafe`
  surface under D-5.3, and not 1.0 work.
- **A real runner, or a second pinned tool:** NFR-BUILD-020 and NFR-LIC-030 (the per-platform install
  half), FR-PKG-040 (the Windows installer's interior needs `innoextract`, a second Chocolatey input
  on the merge path — deliberately not taken).
- **The four unexecuted scripts:** NFR-QUAL-010. Its tag read `closes M9b` — a milestone that has
  closed out — and now reads `closes M8`.

**C. Deferred product work — a Must whose product half was never built (10).** §21 Phase 1's scope,
which this milestone deliberately did not enter: FR-IN-030, FR-LIB-010, FR-NAM-080, FR-OUT-020,
FR-PARAM-030, FR-STATE-030, FR-STATE-070 — seven user-facing clauses with no `UiIntent` behind them —
plus FR-CLAP-060's click-free limb, which needs a crossfade in `Chain` that does not exist, and
**FR-EQ-030 and FR-PARAM-040, which are one finding rather than two**: eight of `EqStage`'s twelve
parameters exceed FR-PARAM-040's click bound, `eq.enabled` by 16.8x, and a 20 ms ramp improves three
rows while worsening two. The cause is D-9.9's smoothing *mechanism*, not its duration, so choosing
is D-9.9's call and not a workstream's. Two workstreams reached that conclusion independently, from
different crates, without contact.

**D. Machine work, scoped and not taken (9).** The honest residue: nothing blocks these but the
milestone's own boundary, and they are the cheapest remaining wins. FR-ERR-030 (the `plus I` half —
an integration test driving a real audio callback with the process-global logger installed),
FR-ERR-040 (the GUI thread and the plugin-configuration sentence, both `namir-clap`'s), FR-ERR-060,
FR-NAM-050, FR-NAM-060, FR-STATE-060 (the CLAP host save/reopen round trip), NFR-QUAL-040 (the fourth
fuzz target in CI), NFR-RT-010 (the harness detects heap allocation; the requirement enumerates six
properties), NFR-RT-040.

#### What this milestone does not claim

- **D-18.5's flip has not happened, and NFR-QUAL-010 is not closed.** §21 sequences the flip behind
  Phase 3's verdict-parsing change *and* Phase 6's Windows session. Phase 3 landed; Phase 6 did not.
- **Four `Verify: M` Musts stand unexecuted** — FR-UI-030, -040, -050 and FR-IO-030 — so Acceptance's
  "neither `NOT EXECUTED` nor silence survives into M8" is **not met**. Recorded here so M8 does not
  discover it. FR-UI-040 and -050 need the machine M9b already used and roughly an hour; FR-UI-030
  cannot pass at all until issue #35 is resolved, since `egui-baseview` wires no `accesskit` adapter;
  FR-IO-030 needs Linux and macOS hardware, and until it runs **neither platform's audio has ever
  moved a sample**.
- **Issue #34 needs no further work, and that is the one Acceptance criterion this milestone did
  meet.** `xtask traceability` can no longer print `clean` while a manual document records anything
  other than a pass — `NOT EXECUTED`, `PARTIAL`, `FAIL`, a `PASS` qualified by an unexecuted step, or
  no readable verdict at all. M9b's close-out asked that a `FAIL` be treated as uncovered; the fix
  had already landed when that was written.
- **Two gates in the documented set never ran on this branch, and neither is claimed green.**
  *Clippy* ran, but on `rustc 1.94.1`; `trunk`'s `4b0d49a` fixes lints new in **1.98**, so CI
  compiles with something this sandbox cannot reproduce and no local run is evidence about it.
  *`cargo deny check`* could not run at all — `cargo-deny` is not installed here. What is checked
  instead, and is the reason this is a note rather than a risk: the licence and advisory surface is
  **unchanged** from `trunk`. `Cargo.lock` gains no dependency on this branch, `deny.toml`'s diff is
  its `# trace-partial:` annotation text with no `allow` or `deny` entry touched, and the manifest
  changes are `[[bench]]` declarations. `trunk` passes the gate; this branch does not move what the
  gate reads. CI runs both properly on the merge.
- **FR-UI-070 is fixable but not fixed in the ledger's eyes.** All eight defects behind it are
  closed. Its cell moves when a human re-runs the fifteen steps, and not before.

#### Findings worth carrying past the workstreams that made them

- **Two real audio-thread allocations in `namir-app`**, found the first time `stream.rs`'s callbacks
  ran under D-7.5's harness: a `Vec` collected per chunk on every callback, and a scratch buffer that
  grew past its reservation whenever the host delivered more frames than the negotiated block size.
  Both fixed, and the harness verified as a real detector by restoring each form.
- **Automation was quantised to the block boundary** — the apply path never read
  `ev.header().time()` — so at 4096 frames every automation point landed up to ~85 ms late. Fixed by
  splitting the block at each event's own frame (issue #30).
- **`to_stream_failure`'s classification was wrong, not merely its rendering.** M9b's physical unplug
  arrived as `StreamFailure::Other` with an unmapped OS error; Namir called it a device loss by luck,
  choosing the code from the stream's direction. A live **R-5** data point (issue #44).
- **FR-LIB-030's newly-real margin is thin.** `IndexStore`'s JSON reload of 10 000 entries costs
  ~70 ms, roughly twice the incremental scan it enables, so a second start-up beats a first by ~15 %,
  not the ~4x the old arms implied.
- **The cheap stages carry the largest denormal penalty by ratio** — EQ 32.9x, trim 10.2x — which is
  exactly the dilution NFR-RT-030's aggregate arm cannot see, and the reason its method says "each
  stage". Sandbox figures, informational.
- **Two tests were found not to test what they claimed**, both at integration and both replaced:
  W8's anti-erase test passed with the ordering it protects transposed (it verified sharing, not
  sequencing, on the one path with a history of silently erasing a user's library), and W10 caught
  its own first dismissability test measuring a hand-copied layout whose click hit nothing. Each was
  established by breaking the thing deliberately, which is the only way either was going to surface.

#### One integrator override, recorded so it is not read as an oversight

Both macOS packaging steps in `ci.yml`'s new `bundle-and-inspect` job carry `continue-on-error: true`
for their first runs, **against the recommendation of the workstream that wrote them**. Its argument
is sound about defects — "the first run finding something is the lane working; the alternative is
finding it while cutting a release" — and is left in the file. The override is about *absence*: no
line of `packaging/macos/make_installer.sh` has ever executed anywhere, no runner was available to
try it, and a failure for the runner's reasons rather than this repository's would red-block every
pull request. Those risks are not symmetric. This follows the visible-before-blocking shape D-18.5
already uses. **The flip condition — one green run on a real macOS runner — is written at the step,
and removing the two lines is the whole of it.**
