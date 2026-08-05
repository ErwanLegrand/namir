# Namir — Architecture

| | |
|---|---|
| **Project** | Namir |
| **Document** | 02 — Architecture |
| **Version** | 0.1 (draft) |
| **Date** | 2026-08-04 |
| **Status** | Draft — awaiting review |
| **Author / Copyright holder** | Erwan Patrick Legrand |
| **Licence** | MIT OR Apache-2.0 |
| **Governing document** | `01-functional-requirements.md` v0.2 |

---

## 1. Purpose and standing

This document decides **how** Namir is built. It is subordinate to the FRS: where this
document and the FRS disagree, the FRS wins and this document is a defect.

Every significant decision here is stated as **Decision — Rationale — Consequence —
Alternatives rejected**. A decision without a recorded rejected alternative is usually a
decision nobody actually made, and is treated as suspect during review.

Two open questions from FRS §9 — **OQ-1** and **OQ-2** — are *not* decided here. Per the
agreed sequencing, they are settled by measurement. §19 specifies the spikes that produce
those measurements and the criteria they are judged against; §20 records their status as
pending. Everything else in FRS §9 is decided in this document.

---

## 2. Reference machine and measurement baseline

FRS assumption **A-4** requires a single reference machine against which every performance
figure is stated. It is:

| | |
|---|---|
| CPU | AMD Ryzen 9 5950X — 16 physical cores / 32 threads, 3.4 GHz base |
| Memory | 64 GB |
| Board | Gigabyte B550 AORUS ELITE V2 |
| OS | Windows 11 Pro, build 26200, x86-64 |

**Decision D-2.1** — All benchmark thresholds are expressed as a fraction of **one core** of
this machine, never as wall-clock time and never as a fraction of total machine capacity.

*Rationale:* An audio callback runs on one thread. A budget expressed against a 32-thread
machine's aggregate capacity is meaningless, and a budget in milliseconds silently changes
meaning when the block size changes.

*Consequence:* NFR-PERF-010's figure is per-instance, single-core, and must be restated as a
percentage of the *block period* — at 48 kHz with a 64-sample block, the period is 1.333 ms,
so a 25 % budget is 333 µs of single-core time at the 99.9th percentile.

*Rejected:* Expressing budgets in absolute microseconds (breaks under block-size change);
using CI runner hardware as the baseline (too variable to regress against).

**Decision D-2.2** — Benchmarks report the **99.9th percentile** of per-block processing time,
never the mean. CI gates on that percentile plus the maximum observed.

*Rationale:* NFR-RT-040 is a statement about worst case. A mean can be excellent while every
thousandth block causes an audible dropout, which is precisely the failure users report and
mean-based benchmarks hide.

*Consequence:* Benchmark harnesses must retain per-block timing distributions, not summary
statistics, and must run long enough for the 99.9th percentile to be meaningful (≥ 100 000
blocks).

---

## 3. Architectural principles

These are derived from the FRS, not invented here. Each is traceable, and each is a rule that
later sections are checked against.

| # | Principle | Derived from |
|---|---|---|
| **P1** | The audio thread allocates nothing, locks nothing a non-RT thread can hold, and waits for nothing. Every other decision yields to this one. | NFR-RT-010/020 |
| **P2** | Anything expensive, fallible or blocking happens on a worker thread and is handed to the audio thread already finished. | FR-NAM-070, FR-IR-060, FR-STATE-050 |
| **P3** | The engine is a sequence of uniform stages. 1.0 fixes the sequence; the abstraction does not. | FR-CHAIN-010, RD-2 |
| **P4** | CLAP is an external interface, not an internal boundary. | OQ-10 |
| **P5** | Platform-specific code exists only in designated crates. The engine, parameters, state and library are platform-free and build for mobile targets from day one. | NFR-PORT-020/030 |
| **P6** | Untrusted input is parsed in one hardened place per format, and that place is fuzzed. | NFR-QUAL-040, NFR-SEC-010 |
| **P7** | Identity of a model or IR is its content hash. Paths are hints. | FR-STATE-070 |
| **P8** | Failure degrades; it does not propagate. A fault silences a stage, never the host process. | FR-ERR-040, FR-CHAIN-080 |

---

## 4. System context

```
        ┌──────────────────────── namir-app (standalone) ────────────────────────┐
        │  cpal ↔ audio device      egui window       settings/persistence        │
        └───────────────────────────────┬────────────────────────────────────────┘
                                        │
        ┌──────────────── namir-clap (plugin) ───────────┐        both embed:
        │  clack ↔ host   host params   host state       │
        └───────────────────────┬────────────────────────┘
                                │
        ╔═══════════════════════▼════════════════════════════════════════════════╗
        ║                        namir-engine                                     ║
        ║   Chain = [Trim] [Gate] [Nam] [Ir] [Eq] [Out]   ← uniform Stage trait   ║
        ║   RT-safe. No allocation. No locks. No I/O.                             ║
        ╚════▲══════════════════════════════════════════════════▲═════════════════╝
             │ SPSC commands (in) / telemetry (out)             │ Arc<Prepared*>
        ┌────┴──────────────────────────────────────────────────┴─────────────────┐
        │  namir-worker: model & IR loading, resource cache, library scanning      │
        │  namir-library ·  namir-state  ·  namir-nam  ·  namir-ir (prepare paths) │
        └──────────────────────────────────────────────────────────────────────────┘
```

The engine is a guest on a thread it does not own (FRS A-2). Both product configurations are
thin adapters that feed it.

---

## 5. Crate structure and layering

**Decision D-5.1** — A single Cargo workspace with the following crates and a strict acyclic
dependency rule.

| Crate | Responsibility | May depend on | Platform code? | Builds for mobile? |
|---|---|---|---|---|
| `namir-core` | Shared vocabulary types: sample rate, channel layout, dB/linear, content hash, error catalogue. No logic. | — | No | Yes |
| `namir-params` | Parameter identity, ranges, formatting, smoothing, automation intake. | core | No | Yes |
| `namir-dsp` | Primitive DSP: biquads, gate detector, meters, gain ramps, DC blocker. | core | No | Yes |
| `namir-nam` | `.nam` parsing, validation, inference, model preparation. | core, dsp | No | Yes |
| `namir-ir` | IR file decoding, resampling, partitioned convolution. | core, dsp | No | Yes |
| `namir-engine` | The `Stage` trait, the chain, RT-safe scheduling, resource handover, telemetry. | core, params, dsp, nam, ir | No | Yes |
| `namir-state` | Preset and plugin-state document, versioning, file-reference resolution. | core, params | No | Yes |
| `namir-library` | Library index, scanning, hashing, search, persistence. | core, nam, ir, state | Path handling only, via `namir-platform` | Yes |
| `namir-platform` | Filesystem locations, config dirs, logging sink, thread priority. **The only crate with `#[cfg(target_os)]`.** | core | Yes | Yes |
| `namir-worker` | Off-thread orchestration: load requests, resource cache, scan jobs. | everything above | No | Yes |
| `namir-ui` | egui-based interface. Renderer- and window-agnostic. | core, params, library, state | No | Yes |
| `namir-app` | Standalone application: audio device I/O, window, settings. | everything | Via platform + cpal | Not for 1.0 |
| `namir-clap` | CLAP adapter. **The only crate that names CLAP.** | everything except app | Via clack | No |

**Decision D-5.2** — The layering is enforced mechanically in CI, not by convention.

*Rationale:* NFR-PORT-020 says the engine contains no platform-conditional code "at all". A rule
nobody checks is a rule that decays within a month.

*Consequence:* CI runs (a) a dependency-graph check rejecting any edge not in the table above,
(b) a lint rejecting `#[cfg(target_os` / `#[cfg(windows` / `#[cfg(unix` outside `namir-platform`,
(c) `cargo build` of every crate marked "builds for mobile" against `aarch64-linux-android` and
`aarch64-apple-ios` (NFR-PORT-030's verification clause).

*Rejected:* Documenting the layering in prose only — this is what NFR-PORT-020 already
anticipated by demanding a lint.

**Decision D-5.3** — `namir-engine` and everything below it declare `#![forbid(unsafe_code)]`.
Unsafe is permitted only in `namir-platform`, `namir-clap`, and any SIMD kernel module, each of
which carries a written safety argument per unsafe block.

*Traces:* NFR-QUAL-070.

---

## 6. The stage abstraction — OQ-9

This is the central abstraction. It must serve 1.0's fixed chain and RD-2's flexible one without
redesign, and OQ-5 and OQ-10 are answered in its terms.

**Decision D-6.1** — Every processing element implements one trait, split across two lifecycles:
a **non-real-time preparation** path that may allocate and fail, and a **real-time processing**
path that may do neither.

```rust
/// Non-RT. Runs on a worker thread. May allocate, may fail, may take milliseconds.
pub trait StagePrep {
    type Prepared: Stage;
    fn prepare(&self, ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError>;
}

/// RT. Runs on the audio thread. Must not allocate, lock, block, or fail.
pub trait Stage: Send {
    fn process(&mut self, io: &mut StageIo<'_>);
    fn reset(&mut self);
    fn latency_samples(&self) -> u32;
    fn tail_samples(&self) -> u32;
    fn apply(&mut self, change: ParamChange);
    fn telemetry(&self, out: &mut TelemetrySink<'_>);
}
```

*Rationale:* The split is the whole point. Most real-time audio bugs are one of these two paths
doing the other's job. Making them different traits on different types means the compiler
rejects the mistake instead of a reviewer catching it.

*Consequence:* A stage cannot change its own allocation footprint mid-stream. Any change that
would require reallocation — a new model, a new IR, a sample-rate change, a block-size increase —
is a *replacement*, produced by `prepare` on a worker and swapped in per §8.

*Consequence for RD-2:* Adding stages, reordering them, or having several of the same kind
requires no change to this trait. The chain is `Vec<Box<dyn Stage>>` built once during
preparation; 1.0 simply always builds the same six entries.

*Rejected:* A single trait with an `init()` that allocates — indistinguishable at the type level
from the RT path, and the source of exactly the bug class P1 exists to prevent. Rejected:
generic-over-stage static dispatch for the whole chain — it would make RD-2's dynamic chain a
rewrite, and the virtual-call cost is negligible against per-block work of hundreds of samples.

**Decision D-6.2** — `StageIo` carries scratch buffers owned by the chain, not by the stage, sized
at preparation time for the maximum block size the host declared.

*Consequence:* FR-CLAP-070 requires supporting a block size of one sample and varying block sizes.
Buffers are sized for the declared maximum; a smaller block simply uses a prefix. If a host
exceeds its declared maximum, the chain processes in slices of the declared maximum rather than
allocating (P1) — correctness is preserved, and the event is recorded as telemetry.

---

## 7. Threading and real-time strategy

**Decision D-7.1** — Three thread roles, no more.

| Role | Owner | May allocate | May block |
|---|---|---|---|
| **Audio** | Driver (standalone) or host (plugin) | No | No |
| **Worker** | Namir, one pool sized to `min(2, cores-1)` | Yes | Yes |
| **UI** | OS / host | Yes | Briefly, never on audio |

*Rationale:* NFR-PORT-030 forbids assuming unlimited threads (mobile). A small fixed pool is
also easier to reason about for P1 than a work-stealing runtime.

*Rejected:* An async runtime (tokio et al.) — it brings a scheduler, a large dependency surface
and no benefit for a workload that is a handful of long CPU/IO tasks, not thousands of concurrent
sockets.

**Decision D-7.2** — Audio thread inbound communication is a **single-producer, single-consumer,
wait-free ring buffer** of fixed-size command records, pre-allocated at preparation. The worker is
the sole producer; a mutex on the *producer side only* serialises UI and worker submissions.

*Rationale:* NFR-RT-020 requires wait-freedom *from the audio thread's side*. The producer side
may block, because it is not the audio thread. This is the cheapest structure that satisfies the
requirement honestly.

*Consequence:* Commands are fixed-size and contain no owned heap data — a model handover command
carries an `Arc<PreparedNam>` (a pointer), never the model.

*Consequence:* If the ring is full, the producer waits and retries; it never drops a command
silently, because a dropped parameter change is a stuck control.

**Decision D-7.3** — Audio thread outbound communication (meters, gate reduction, fault flags,
xrun counts) uses **atomics and a lock-free telemetry ring**, read at UI frame rate. Loss is
acceptable outbound and the buffer overwrites oldest.

*Rationale:* FR-IN-020, FR-GATE-040, FR-OUT-020 need values, not a reliable stream. FR-ERR-030
forbids formatting or allocating for diagnostics on the audio thread, so the audio side writes
numeric codes and the UI side does the formatting.

**Decision D-7.4** — Denormal suppression is set once per audio callback by putting the FPU into
flush-to-zero / denormals-are-zero for the duration of the callback, and restoring the previous
mode on exit.

*Rationale:* NFR-RT-030. Restoring on exit matters: the host's other plugins may depend on the
prior mode, and silently changing a host's FPU state is a defect users cannot diagnose.

*Consequence:* This is `unsafe` and platform-specific — it lives in `namir-platform` behind a
guard type whose `Drop` restores the mode, so it cannot leak even on an early return.

*Rejected:* Adding tiny DC offsets to filter states — it works, but it pollutes every DSP module
with a workaround for a problem the CPU has a flag for.

**Decision D-7.5** — Real-time safety is verified by a test harness that installs a global
allocator which **panics on allocation while an "audio section" marker is active**, and every
engine test runs inside that marker.

*Rationale:* NFR-RT-010's verification clause demands exactly this. It converts P1 from an
aspiration into a build failure.

*Consequence:* This harness is test-only and must not be linked into release binaries.

---

## 8. Resource lifecycle — OQ-5 and OQ-8

The hardest requirement in the FRS is FR-NAM-070: swap a model, mid-performance, with no glitch
and no allocation on the audio thread. Everything here follows from it.

**Decision D-8.1 (answers OQ-5)** — Model and IR handover is a four-step protocol:

1. **Prepare** — worker loads, validates and prepares the resource into an `Arc<PreparedNam>` /
   `Arc<PreparedIr>`. Fully allocated, fully warmed, sample rate matched. Failure ends here and
   is reported; the audio thread is never told.
2. **Offer** — worker pushes a command carrying the `Arc` into the SPSC ring.
3. **Crossfade** — the audio thread installs the new resource *alongside* the old, runs both for
   the crossfade window (equal-power, 5–50 ms per FR-NAM-070) and mixes. The stage is prepared
   with capacity for two live resources precisely so this needs no allocation.
4. **Retire** — the audio thread pushes the old `Arc` into a **return ring**, and never drops it.
   The worker drains that ring and drops the `Arc` there.

*Rationale for step 4:* Dropping the last `Arc` on the audio thread would run a deallocator —
a P1 violation, and one that is invisible in testing because it only happens when the refcount
happens to reach zero on that particular swap.

*Consequence:* The return ring must be drained reliably. If the worker dies, the ring fills and
memory is retained but audio continues. Degradation, not failure (P8).

*Consequence:* Running two models during the crossfade momentarily doubles the NAM stage's cost.
NFR-PERF-010's budget must therefore be met with headroom for a 2× transient, or the crossfade
must be measured as part of the benchmark. **The benchmark measures it.**

*Rejected:* Muting during the swap (fails FR-NAM-070's no-dropout intent). Rejected: a mutex
around the resource slot with `try_lock` on the audio thread — `try_lock` is wait-free but a
failed acquisition means the swap silently doesn't happen, which is a worse failure than the
problem it solves.

**Decision D-8.2 (answers OQ-8)** — A process-global resource cache maps **content hash →
`Weak<Prepared*>`. It is guarded by an ordinary mutex, and the audio thread never touches it.**

*Rationale:* FR-CLAP-090 wants N plugin instances loading the same model to share one copy of the
weights. OQ-8 asked how to do that without a lock the audio thread can contend on. The answer is
structural: only workers consult the cache; the audio thread receives an already-resolved `Arc`
through the ring. There is no lock for it to contend on because it never participates.

*Consequence:* `Weak` rather than `Arc` in the cache, so an unreferenced model is freed rather
than pinned for the process lifetime.

*Consequence:* `Prepared*` must be immutable and `Sync` — all per-instance mutable inference
state (ring buffers, filter memory) lives in the `Stage`, not in the shared resource. This is a
hard constraint on the NAM inference design and is called out again in §9.1.

---

## 9. DSP design

### 9.1 NAM stage

**Pending — OQ-1 and OQ-2.** The choice between a Rust implementation and a binding to
`NeuralAmpModelerCore` is deferred to spike **S-1** (§19). The following decisions hold either way
and constrain both options.

**Finding (licence):** `sdatkinson/NeuralAmpModelerCore` is **MIT**-licensed, active (last push
2026-07-08). *Verified 2026-08-04.* The C++ option is therefore licence-clean under NFR-LIC-020;
the case against it is entirely NFR-PORT-040 (no C++ toolchain) and NFR-PORT-030 (mobile).

**Decision D-9.1** — Whatever the implementation, the model's immutable weights and architecture
live in a shared, `Sync`, read-only `PreparedNam`, and all mutable inference state lives in the
per-instance `Stage`.

*Consequence:* This is a genuine constraint on a C++ binding. If the upstream core couples weights
and state in one object, FR-CLAP-090's sharing cannot be achieved without modifying it, and that
counts against the C++ option in S-1's evaluation. **S-1 must test this explicitly, not assume it.**

**Decision D-9.2 (answers OQ-6)** — Sample-rate conversion is applied **around the NAM stage
only**, not to the whole engine.

*Rationale:* Three reasons, in order of weight. (a) The IR stage must run at the engine rate to
keep FR-IR-040's zero latency aligned to the host's block boundary; running it at the model rate
would reintroduce conversion after it. (b) When engine rate equals model rate — 48 kHz, the
overwhelmingly common case — the resampler is bypassed entirely, giving exactly zero cost and
zero added latency, satisfying NFR-PERF-020 without a special case elsewhere. (c) It confines the
rate change to one stage, so no other stage needs to know two sample rates exist.

*Consequence:* When active, the resampler introduces latency, which propagates to
`latency_samples()` and thence to FR-CLAP-040's host notification. A model change that changes the
rate ratio changes reported latency — which is exactly the case FR-CLAP-040 calls out.

*Consequence:* Rate conversion breaks the 1:1 relationship between engine block size and model
block size. The stage therefore runs the model on a **fixed internal block** with input and output
FIFOs, giving deterministic latency and constant per-call work rather than a ragged
sample count that would violate NFR-RT-040.

*Rejected:* Running the whole engine at the model rate — pushes conversion to the engine boundary,
where it affects the IR, the meters and the host's block contract. Rejected: refusing to run at a
mismatched rate — user-hostile, and 44.1 kHz sessions are common.

**Decision D-9.3** — Resampling uses `rubato`'s fixed-ratio sinc/FFT resamplers, configured to
meet FR-NAM-060 (≥ 100 dB stopband, ≤ 0.1 dB ripple to 20 kHz). The configuration is verified by a
direct measurement test, not by trusting the library's defaults.

### 9.2 IR stage — OQ-4

**Decision D-9.4 (answers OQ-4)** — **Non-uniform partitioned convolution.** The first partition
equals the host block size, giving zero latency (FR-IR-040); subsequent partitions grow
geometrically and are processed with the slack the earlier partitions' delay provides.

*Rationale:* FR-IR-050 requires ≥ 2 s IRs. At 48 kHz that is 96 000 taps. Uniform partitioning at
a 64-sample block would need 1 500 partitions per block — the arithmetic makes the naive option
untenable, so this is decided rather than deferred.

*Consequence:* Complexity is real and concentrated. It is mitigated by D-9.5.

**Decision D-9.5** — A straightforward **direct time-domain convolution** is implemented as a
*reference*, retained permanently in the test suite, and the partitioned implementation is
verified against it to a stated numerical tolerance for every IR length and block size in the test
matrix.

*Rationale:* NFR-QUAL-030 forbids verifying DSP by ear. A slow-but-obviously-correct reference is
the cheapest way to make a fast-and-subtle implementation trustworthy, and it makes OQ-4's
complexity safe to take on.

**Decision D-9.6** — FFTs use `rustfft` / `realfft`. The partition schedule (first-partition size,
growth factor, maximum partition) is a tunable recorded in the architecture, defaulted from S-2's
measurements, not guessed. **S-2 result, 2026-08-05: growth factor 2, maximum partition 8192
samples** (first partition equals the host block size per D-9.4). See §19 for the measurements
behind this default and R-8 for the follow-up work — phase-staggering same-size partitions —
required before the IR stage can meet NFR-PERF-010 at small block sizes regardless of this
tunable's value.

**Decision D-9.7** — FR-IR-050's open choice — truncate long IRs or process them in full — is
resolved as: **process in full up to a documented ceiling of 10 seconds at the engine rate; beyond
that, truncate with a report to the user** (FR-ERR-020 catalogue entry). The ceiling exists to
satisfy NFR-SEC-020's bounded-allocation requirement.

### 9.3 Gate, EQ, trim, meters

**Decision D-9.8** — The gate detector runs on the signal *before* input trim so its threshold is
referenced to the interface's actual noise floor and does not move when the user adjusts trim.

*Rationale:* Not specified by the FRS; a usability judgement. Recorded here so it is a decision
rather than an accident, and flagged for review.

**Decision D-9.9** — EQ uses transposed-direct-form-II biquads with coefficient interpolation
across the block rather than coefficient recalculation per sample.

*Rationale:* FR-EQ-030 (no zipper noise) and NFR-RT-040 (content-independent cost). TDF-II is
chosen over DF-I for its numerical behaviour with modulated coefficients at high Q, which is
precisely FR-EQ-020's stability requirement.

*Consequence:* FR-EQ-020 demands stability across the full range at up to 192 kHz. Coefficient
computation must be done in `f64` even though processing is `f32`, because shelf and peak
coefficients at low frequencies and high sample rates lose significance in `f32`.

**Decision D-9.10** — All internal processing is `f32`; accumulation in the convolution and
coefficient computation are `f64`.

*Rationale:* `f32` matches every host's buffer format and halves memory traffic in the
convolution's inner loop. The two exceptions are where `f32` demonstrably loses precision.

### 9.4 Verification strategy

**Decision D-9.11 (resolves NFR-QUAL-030's wording)** — NFR-QUAL-030's text stands unchanged.
This decision records that its intent — a stated, numerical, reproducible correctness reference,
never a claim verified "by ear" — is already satisfied by S-1's cross-implementation NAM parity
result and D-9.5's direct time-domain convolution reference, not by literally-worded "golden
reference audio held in the repository."

*Rationale:* NFR-QUAL-030 was written to forbid one specific failure mode: a DSP correctness claim
resting on someone having listened to it and judged it "close enough," with no stated number and
no way for a later contributor to re-check the claim. The literal phrase "golden reference audio"
names the mechanism the FRS's author had in mind for preventing that failure mode, not the failure
mode itself. D-19.1 (§19), decided for an unrelated reason (AQ-1, redistributability), commits
this project to *no* captured audio in the repository at all — every fixture is generated from a
seed. Taken literally, "golden reference audio held in the repository" and "no captured audio in
the repository" are in tension. Taken as intent, they are not: S-1 (§19) compares Rust WaveNet
inference against an independent, from-scratch reference implementation
(`NeuralAmpModelerCore`) to a numerically stated tolerance (-131 dB measured, 90 dB floor), and
D-9.5 compares the partitioned convolution against a direct time-domain reference to a numerically
stated tolerance, retained permanently in the test suite. Both are stated numbers, both are
reproducible from a seed on any machine without access to any author's hardware, and both are
strictly harder to satisfy by accident than "sounds right to me" — an independent
cross-implementation or a direct time-domain computation cannot silently agree with a subtly wrong
result the way a human ear can. Neither is "audio held in the repository" in the literal sense,
and neither needs to be: the actual thing NFR-QUAL-030 protects against is unverifiable
correctness claims, and both routes already close that gap numerically.

*Consequence:* Every future DSP stage follows the same pattern, not the literal wording. Gate and
EQ at M2, and LSTM at M3, are each verified against either an analytic target (a closed-form
filter response, a designed test signal) or an independent cross-implementation, compared
numerically, with the tolerance stated and the comparison kept in the permanent test suite — never
against a captured recording treated as ground truth. A DSP stage whose correctness argument comes
down to "an author listened to it" still fails NFR-QUAL-030, exactly as a literal reading would
forbid — the standard did not weaken, only its concrete realization changed to match D-19.1.

*Consequence:* This decision does not rewrite NFR-QUAL-030's text in `01-functional-requirements.md`.
The FRS is the governing document (its own §1.1), and a requirement's wording, once assigned an
ID, is a stable handle other documents cite by number, not free text a lower-authority document is
entitled to silently edit. What changes is the *recorded route to satisfaction*, exactly as this
document already does for AQ-2 and AQ-5 (§21) — the open question raised against a requirement is
resolved in place, with a decision that records how the requirement is met, and the requirement's
own text is left alone.

*Rejected:* Amending NFR-QUAL-030's wording directly (e.g. to read "verified to a stated numerical
tolerance, either against an independent cross-implementation or an analytically-derived
reference" in place of "golden reference audio held in the repository") — rejected because it
would mean a document lower in the authority order (02, "how") editing a document higher in the
authority order (01, "what") to make itself consistent, which is backwards: per §1, where the two
disagree, the FRS wins and this document is the defect. The correct fix for an apparent conflict
is to show the FRS's intent is already met, not to rewrite the FRS to match this document's
implementation choice.

---

## 10. Parameter system

**Decision D-10.1** — Parameters are declared in one place per stage as static descriptors, and
the full set is emitted at build time into a checked-in **parameter manifest** (`params.lock`).

*Consequence:* FR-PARAM-020 demands permanently stable identifiers and a CI failure on reuse. The
manifest is diffed in CI: adding a parameter is allowed, changing or removing an existing entry's
identifier or type fails the build, and retiring one requires an explicit tombstone entry.

**Decision D-10.2** — A parameter identifier is a stable `u32` derived from a namespaced string
(`"gate.threshold"`), with the string retained in the manifest. Hosts see the `u32`; humans see
the string.

*Consequence for RD-2:* When the chain becomes dynamic, the identifier gains a stage-instance
index. The scheme is designed with that field present and set to zero in 1.0, so growing the chain
does not renumber existing parameters and does not invalidate saved projects.

**Decision D-10.3** — Smoothing is a property of the parameter, declared in its descriptor, not
open-coded in each stage: gain-like parameters get a one-pole ramp; frequency-like parameters get
per-block coefficient interpolation; stepped parameters get a crossfade or a click-free switch
point.

*Traces:* FR-PARAM-040, FR-PARAM-050.

---

## 11. State and presets — OQ-7

**Decision D-11.1 (answers OQ-7)** — The state and preset format is **JSON**, pretty-printed with
stable key ordering, carrying an explicit `format_version`.

*Rationale:* The decisive argument is NFR-QUAL-040, not aesthetics. `.nam` files are already JSON,
so a JSON state format means **one** parser to harden and fuzz instead of two. TOML is nicer to
hand-edit, but the state document contains nested per-stage structures where TOML is worse, and
adding a second parser to the fuzzing surface to gain marginal readability is a poor trade for a
program whose main attack surface is file parsing.

*Consequence:* Stable key ordering is required, not incidental — FR-STATE-040 promises diffability,
and a serialiser with non-deterministic map ordering makes every save a spurious diff.

*Rejected:* TOML (second parser, poor nesting); RON (Rust-specific, poor third-party tooling,
against NFR-DOC-010's "third party can write a compatible reader"); binary formats (fails
FR-STATE-040 outright).

**Decision D-11.2** — Deserialisation is tolerant and versioned: unknown fields are preserved and
written back; missing fields take documented defaults; `format_version` gates migrations.

*Rationale:* FR-STATE-020 requires every past version's documents to load. Preserving unknown
fields additionally means a project saved by a newer Namir and opened by an older one does not
silently lose settings on the next save — a failure mode FR-STATE-020 does not require us to
handle but which costs almost nothing to prevent here and is impossible to retrofit later.

**Decision D-11.3** — A file reference is a record of all three of: library-relative path, absolute
path, and BLAKE3 content hash, resolved in that order, then by hash search of the library index
(FR-STATE-070).

*Rationale for BLAKE3:* fast enough to hash a large library during scanning without dominating it,
and not a legacy hash we would have to migrate away from. The hash is an identity, not a security
primitive, but using a broken hash for identity invites collisions in a shared community corpus.

*Consequence:* The library index must maintain a hash → path map (§12), otherwise the third
resolution step in FR-STATE-070 cannot work.

---

## 12. Library subsystem

**Decision D-12.1** — The library index is an on-disk table of `(path, size, mtime, content hash,
extracted metadata)`, persisted between sessions and updated incrementally by comparing size and
mtime before rehashing.

*Traces:* FR-LIB-030, NFR-PERF-060 (10 000 files rescanned in ≤ 2 s — achievable only because
unchanged files are not rehashed).

**Decision D-12.2** — Scanning is a cancellable worker job reporting progress; the UI never waits
on it (FR-LIB-020, FR-UI-060).

**Decision D-12.3** — The index is stored as a single-file embedded key-value store or a simple
append-only log with compaction — **decided in implementation, constrained here**: no dependency
carrying a copyleft licence, no C or C++ dependency (NFR-PORT-040), and corruption must degrade to
a full rescan rather than to a crash or to wrong results (P8).

**Decision D-12.4 (for RD-1)** — A library entry carries an `origin` field from the outset —
`Local` in 1.0, extensible to a remote source later. Tone3000 integration then adds a variant
rather than a schema migration across every user's index.

---

## 13. Platform layer

**Decision D-13.1** — Audio I/O for the standalone app uses **`cpal`** (Apache-2.0, v0.18.1,
*verified 2026-08-04*), behind a Namir-owned trait so the engine and UI never see cpal types.

*Consequence:* FR-IO-020 requires WASAPI shared *and* exclusive mode; FR-IO-030 requires ALSA and
CoreAudio. cpal covers these. ASIO is a **Should**, is behind a cargo feature, and requires the
user to supply the ASIO SDK themselves — satisfying NFR-LIC-040 by never making it a required
build dependency.

*Consequence:* FR-IO-070 (device removal while in use) is the requirement most likely to expose
gaps in any cross-platform audio library. It is called out as a risk in §22 and needs a real test
with a device that can be made to fail, not a happy-path test.

**Decision D-13.2** — Filesystem locations, config directories, log sinks and thread priority
elevation live in `namir-platform` and nowhere else (P5, NFR-PORT-020).

**Decision D-13.3** — The CLAP plugin installs to the **CLAP-specified search paths only**, and the
per-user path is the default.

| Platform | Per-user (default) | System-wide (opt-in, needs elevation) |
|---|---|---|
| Windows | `%LOCALAPPDATA%\Programs\Common\CLAP` | `%COMMONPROGRAMFILES%\CLAP` |
| macOS | `~/Library/Audio/Plug-Ins/CLAP` | `/Library/Audio/Plug-Ins/CLAP` |
| Linux | `~/.clap` | `/usr/lib/clap` |

*Rationale:* Established empirically in S-4. Reaper does not scan
`%APPDATA%\REAPER\UserPlugins\CLAP`, and a plugin placed there fails **silently** — it simply never
appears, with no diagnostic in any log. Defaulting to the per-user path also means installation
needs no administrator rights, which matters for users without them.

*Consequence:* The installer, the build documentation (NFR-BUILD-020) and the troubleshooting
section of the user guide (NFR-DOC-030) must all state these paths explicitly. "Plugin does not
appear in my host" will otherwise be the most common support question, and it has no error message
to search for.

*Consequence:* FR-ERR-050's diagnostic bundle should record which CLAP paths exist and what they
contain, since that single fact resolves this class of report immediately.

---

## 14. CLAP adapter — OQ-10

**Decision D-14.1 (answers OQ-10, confirming the FRS)** — CLAP is Namir's **external interface
only**. It is not the internal module boundary. `namir-clap` is the only crate that names CLAP,
and it depends on the engine, never the reverse.

*Rationale, recorded permanently because this question will recur:* (a) iOS does not permit loading
arbitrary third-party dynamic libraries, so a CLAP-based internal bus forfeits RD-4 entirely;
(b) NFR-RT-010 cannot be guaranteed for foreign code, so making foreign code the norm would
downgrade Namir's central promise; (c) an opaque plugin cannot expose which `.nam` it holds,
breaking FR-STATE-070, FR-CLAP-090 and the whole library subsystem; (d) a stable C ABI between
crates compiled into one binary costs `unsafe` at every boundary and buys nothing.

*Consequence for RD-3:* Hosting foreign CLAP plugins, if ever built, is **one additional
implementation of the §6 `Stage` trait** — a `ClapHostStage`. Opt-in, feature-flagged,
desktop-only, and accompanied by a restatement of NFR-RT-010 as conditional for any chain
containing one. It is a leaf feature, not a foundation.

**Decision D-14.2** — The CLAP binding is **`clack`** (`clack-plugin` v0.1.1, MIT OR Apache-2.0,
last published 2026-07-29, *verified 2026-08-04*).

*Rationale:* Licence matches Namir's exactly; CLAP-only, matching constraint C-3; and it leaves us
owning our own parameter and state model, which FR-PARAM-020's permanent-ID rule and FR-STATE-070's
three-way resolution both need.

*Risk, stated plainly:* clack is at **0.1.1 with roughly 10 000 recent downloads**. That is low
adoption and a pre-1.0 API. This is the least-proven dependency in the design.

*Mitigation:* `namir-clap` wraps clack behind Namir's own adapter types so no other crate sees
them; and clack is a thin safe wrapper over a **stable C ABI**, so the fallback — dropping to
`clap-sys` and writing the wrapper ourselves — is bounded, well-understood work rather than a
redesign. §19's spike **S-4** validates this before commitment.

*Rejected — `nih-plug`:* it is the most-travelled path and genuinely good, but (a) its VST3
bindings are **GPLv3** while the framework is ISC, meaning a permanent licence hazard to fence off
in a project that promises MIT OR Apache-2.0; (b) it is distributed via git rather than crates.io;
(c) most importantly, adopting its parameter and state model means fighting it to satisfy
FR-PARAM-020 and FR-STATE-070, and its windowing is desktop-only, working against NFR-PORT-030.

---

## 15. User interface — OQ-3

**Decision D-15.1 (answers OQ-3)** — The UI is built on **`egui`** (v0.35.0, MIT OR Apache-2.0,
*verified 2026-08-04*), with all Namir UI code in `namir-ui`, independent of windowing.

*Rationale:* It is the only candidate that satisfies all four binding constraints at once —
licence (NFR-LIC-020), plugin embedding (FR-CLAP-100), touch viability (FR-UI-090), and a credible
mobile path (NFR-PORT-030). Immediate mode also suits an audio UI, where most of the screen is
meters redrawn every frame anyway.

*Rejected — `vizia`:* MIT and designed for audio plugins, but **511 recent downloads** and
described as a *desktop* framework. Adoption that low means we would be the ones finding its bugs,
and "desktop" works directly against NFR-PORT-030.

*Rejected — `slint`:* licensing is GPLv3 / commercial / royalty-free-proprietary, none of which is
compatible with NFR-LIC-020. Excluded on licence alone, regardless of merit.

**Decision D-15.2** — Plugin window embedding uses **`baseview`** (v0.3.0, MIT OR Apache-2.0, last
published 2026-08-02 — actively maintained, same author as clack; *verified 2026-08-04*), behind a
Namir-owned windowing trait.

*Risk:* baseview has ~4 400 recent downloads — the same low-adoption risk as clack, and the
egui↔baseview↔GPU integration is the specific piece I have **not** verified exists in maintained
form. Spike **S-3** exists to establish this before any UI work begins. If it fails, the fallback
is a Namir-owned thin windowing shim per platform, which is real work and is recorded as the
highest-severity risk in §22.

**Decision D-15.3** — `namir-ui` receives an immutable snapshot of engine telemetry and emits
parameter-change intents. It never reads engine state directly and never blocks on the worker.

*Traces:* FR-UI-060, FR-UI-070, NFR-RT-010.

---

## 16. Errors and diagnostics

**Decision D-16.1** — One enumerated error catalogue in `namir-core`, each entry with a stable
identifier, a severity, and a user-facing message template.

*Consequence:* FR-ERR-020 requires every user-visible error to map to a catalogue entry, verified
statically. The catalogue is the single source for that check and for the user documentation.

**Decision D-16.2** — The audio thread emits **numeric fault codes** through the telemetry ring.
All formatting, allocation and logging happen on the UI or worker side.

*Traces:* FR-ERR-030.

**Decision D-16.3** — Worker jobs are isolated such that a panic in one is caught at the job
boundary, recorded, and does not unwind into the host (FR-ERR-040). The audio thread does not
panic: the engine is written so that its RT paths have no panicking operations, and this is
enforced by review plus by the absence of indexing/unwrapping in RT code.

*Honest limitation:* "no panics on the audio thread" cannot be fully proven by the tools available
here. It is an engineering discipline backed by lints and review, not a guarantee. Recorded as
such rather than overclaimed.

---

## 17. Dependency register

All facts verified 2026-08-04 against crates.io and GitHub, except `assert_no_alloc` (added
2026-08-05, verified the same day). NFR-LIC-020 requires this to be mechanically re-checked in CI
(`cargo-deny`), not maintained by hand.

| Crate | Version | Licence | Verified activity | Role | Risk |
|---|---|---|---|---|---|
| `egui` | 0.35.0 | MIT OR Apache-2.0 | 2026-06-25, ~4.6 M recent downloads | UI | Low |
| `cpal` | 0.18.1 | Apache-2.0 | 2026-06-07, ~4.3 M | Standalone audio I/O | Low |
| `rubato` | 4.0.0 | MIT OR Apache-2.0 | 2026-07-09, ~3.0 M | Resampling | Low |
| `rustfft` | 6.4.1 | MIT OR Apache-2.0 | 2025-09-18, ~6.0 M | FFT for convolution | Low — stale but mature and stable |
| `hound` | 3.5.1 | Apache-2.0 | 2023-09-25, ~4.0 M | WAV decode (FR-IR-010) | Low — unmaintained, but WAV is a frozen format |
| `serde_json` | — | MIT OR Apache-2.0 | ubiquitous | `.nam` + state parsing | Low |
| `clack-plugin` | 0.1.1 | MIT OR Apache-2.0 | 2026-07-29, ~9.8 k | CLAP binding | **High — pre-1.0, low adoption.** See D-14.2 |
| `baseview` | 0.3.0 | MIT OR Apache-2.0 | 2026-08-02, ~4.4 k | Plugin windowing | **High — low adoption; integration unverified.** See D-15.2 |
| `symphonia` | 0.6.0 | **MPL-2.0** | 2026-05-15, ~3.3 M | *Candidate* for FR-IR-020 (AIFF/FLAC, a **Should**) | **Licence caveat — see below** |
| `assert_no_alloc` | 1.1.2 | BSD-1-Clause | 2021-08-03, ~1.6 M recent downloads | D-7.5's RT-allocation test harness in `namir-engine`. **Dev-dependency only — never linked into a release build.** | Low — stale (no release since 2021) but small, single-purpose, and off the shipped binary entirely |

**Decision D-17.1** — `symphonia` is **not** adopted for 1.0. FR-IR-010 (WAV) is a **Must** and is
served by `hound` (Apache-2.0). FR-IR-020 (AIFF/FLAC) is a **Should**.

*Rationale:* MPL-2.0 is file-level copyleft. It is compatible with distributing a larger permissive
work, but it imposes obligations Namir does not otherwise carry, and NFR-LIC-020's spirit is to
keep the dependency set unambiguously permissive. Taking on those obligations to satisfy a
**Should** is the wrong trade. If FR-IR-020 is promoted to a Must, this decision is revisited
explicitly, with the obligations documented — not absorbed silently.

**Decision D-17.2** — `cpal` and `hound` are Apache-2.0-only, not dual. This is compatible: Namir's
*own* code remains MIT OR Apache-2.0, and dependencies retain their own licences in the
attribution file (NFR-LIC-030). Recorded because a reviewer will reasonably ask.

**Note on `hound`:** last published 2023. WAV is a stable format and the crate is widely used, so
the staleness is acceptable — but it means we own any bug we find. §22 records this.

**Note on `assert_no_alloc`:** D-7.5's RT-safety harness needs a `GlobalAlloc` that panics on
allocation while an "audio section" marker is active. D-5.3's workspace-wide `unsafe_code =
"forbid"` cannot be locally overridden even inside `#[cfg(test)]` — `forbid` is stronger than
`deny` specifically so that a later `#[allow]` is a hard error, not a suppressible one — so
implementing `GlobalAlloc` (an `unsafe trait`) inside `namir-engine` itself is not an option.
`assert_no_alloc` puts that `unsafe impl` in its own crate instead, which is consistent with
D-5.3's isolation intent even though it isn't the literal case D-5.3 anticipated (a dependency
carrying the unsafe rather than a designated in-tree crate). It runs with its `warn_debug`
feature (count violations rather than aborting the process), so the harness's own test can turn
a violation into an ordinary `#[should_panic]`.

---

## 18. Build, CI and target matrix

**Decision D-18.1** — CI gates every merge on: build + test on Windows/Linux/macOS; cross-*build*
of the mobile-capable crates for `aarch64-linux-android` and `aarch64-apple-ios`; a build in a
container **with no C++ compiler present** (NFR-PORT-040's verification clause); `cargo-deny`
licence audit; the layering lint; the `params.lock` diff; the RT-allocation harness; the fuzz
targets; formatting and lints as errors.

**Decision D-18.2** — A **network-free build configuration is a permanent CI target**, per
FR-ERR-070.5, so that RD-1's future Tone3000 support can never quietly become mandatory.

---

## 19. Spike specifications

Spikes are throwaway code written to answer a specific question, with success criteria fixed
*before* the code is written. They are not product code and are not carried forward.

**Decision D-19.1 (resolves AQ-1) — Every automated test fixture is *generated*, never captured.**

Test assets are split by the purpose they serve, and only one category needs real hardware:

| Purpose | What the test needs | Source |
|---|---|---|
| Parity — FR-NAM-030, S-1 | A *valid* `.nam` per architecture. Tonal realism is irrelevant: the test compares two implementations on identical input. | Generated |
| Performance — NFR-PERF-010 | Realistic architecture *shapes* (WaveNet standard/lite/feather/nano, LSTM sizes). Cost follows topology, not weight values. | Generated |
| Robustness — FR-NAM-040, NFR-QUAL-040 | Valid files as fuzz seeds, plus mutations. | Generated + mutated |
| Convolution correctness — FR-IR-040/050, D-9.5 | Delta, delayed delta, decaying noise, designed minimum-phase filters — all analytically verifiable. | Generated |
| Perceptual review, factory presets — FR-STATE-090 | Real captures. **Never runs in CI.** | Author's own hardware |

*Rationale:* **CI must not depend on an asset a contributor cannot regenerate.** A generated
fixture is reproducible from a seed on any machine, carries no licence surface, does not bloat the
repository, and cannot be invalidated later by a licence problem. A captured fixture fails on every
one of those counts. Note that the main community model collection
(`pelennor2170/NAM_models`) is **GPLv3** and is therefore unusable in this project regardless —
verified 2026-08-04.

*Consequence:* Fixtures are emitted by a build-time generator from a fixed seed. The generator is
product-adjacent tooling and is maintained, unlike the spikes in this section.

*Consequence — a real hazard:* Naively random weights can leave a network degenerate, with output
either near-silent or divergent, which makes an error metric stated *relative to reference RMS*
(FR-NAM-030) meaningless. The generator must therefore either constrain initialisation so
activations stay in a sane range, or briefly train each fixture against a **known analytic**
nonlinearity (soft clipper plus tone stack). The second is preferred: it yields an analytically
known target, so the parity test no longer depends solely on agreeing with the C++ reference.

*Consequence for captures:* Author-supplied captures are perceptual-review material and factory
preset content only. Preferred subjects are **analog** drive pedals and a bass amp DI (the latter
exercising the model's dilated receptive field at low frequencies, which guitar material does not).
Digital devices — modelers, multi-effects — are excluded: capturing them copies a DSP algorithm
rather than recording a circuit, and is commonly prohibited by their terms. Time-variant devices
(compressors, limiters, modulation, delay, reverb) cannot be represented by a time-invariant model
with a finite receptive field and shall not be used as positive fixtures; a limiter capture is
useful only as a documented *negative* fixture.

*Open:* the licence of NAM's standardised capture input signal is unverified. It must be checked
before redistributing any capture derived from it (AQ-4).

### S-1 — NAM inference: Rust or C++ (answers OQ-1, OQ-2)

**Question:** Does a Rust implementation of WaveNet/LSTM NAM inference meet FR-NAM-030's accuracy
requirement and NFR-PERF-010's cost budget, and can it satisfy D-9.1's weight/state separation?

**Method:** implement WaveNet inference in Rust for a real `.nam` model; generate reference output
from `NeuralAmpModelerCore`; compare over the FR-NAM-030 test signal; benchmark both per D-2.1/D-2.2.

**Note:** building the C++ reference requires a C++ toolchain *for fixture generation only*. That
does not violate NFR-PORT-040, which governs building Namir itself. Recorded so the distinction is
not lost.

**Produces:** the real number for FR-NAM-030's accuracy floor (placeholder: 90 dB); the real number
for NFR-PERF-010 (placeholder: 25 % of one core); a decision on OQ-1 with measurements attached.

**Unblocked by D-19.1:** S-1 uses generated fixtures, not captured models. Parity is a property of
the file's architecture and weights, not of its tonal realism, so a generated model tests it fully.

**Result — 2026-08-05. PASS, with a recorded follow-up.** Spike at `spikes/s1-nam-inference/`.

A from-scratch Rust WaveNet inference engine was implemented, reading `.nam` JSON directly, and
compared against `NeuralAmpModelerCore` (MIT, commit `3cde95c3`, 2026-07-08) on a seeded,
non-degenerate "standard"-shaped WaveNet (per D-19.1: generated by constrained initialisation
and RMS-calibrated, not trained — the accepted fallback to training an analytic target, and
sufficient because tonal realism is irrelevant to a parity test) over a generated 10 s
FR-NAM-030 test signal (clean / transient / saturated material), measured on the §2 reference
machine. The operation order and flat weight-array layout were taken from reading
`NeuralAmpModelerCore`'s source directly, not from secondary sources: an earlier pass, based on
a remote research summary, got the array-to-array chaining wrong (assumed one shared tensor
crosses each layer-array boundary; it's actually two — the residual "trunk" and the head-sum
seed are distinct signals) and was caught only because the C++ loader rejected the resulting
weight count. Recorded because it's exactly the kind of error S-1 exists to catch before it
reaches product code.

**FR-NAM-030 (accuracy).** Measured error: **-131 dB**, comfortably past the 90 dB placeholder
floor (clean -129 dB, transient -129 dB, saturated -132 dB, all segments individually past it
too). This fixture doesn't stress-test where the true ceiling is — it shows 90 dB is a safe,
non-binding floor for this architecture, not that a tighter number would also hold under
harder conditions. -131 dB is in the range of float32 rounding-level disagreement between two
independently-ordered implementations (consistent with `NeuralAmpModelerCore`'s own
Eigen-version-bump measurements of ~1e-7 typical difference, per the spike's citations) rather
than a measurable structural discrepancy. **The 90 dB placeholder is retained as FR-NAM-030's
figure.**

**NFR-PERF-010 (performance).** Measured, single-core-pinned, 200,000 blocks of 64 samples at
48 kHz, 99.9th percentile per D-2.1/D-2.2: **41 % of one core** (median 13 %, p99 35 %, max
68 %) for a scalar `f32` Rust implementation with no SIMD. For same-machine context only —
`NeuralAmpModelerCore`'s own `benchmodel` tool reports **9–12 %** on the identical fixture, but
as a single mean over 1,500 buffers with no percentile and no core pin, so it's informal
context, not a directly comparable D-2.2 figure (see the spike README). The gap reads as an
absence of vectorization, not a structural cost: the two implementations' central tendencies sit
within the same order of magnitude despite the Rust side doing no SIMD work at all, and a
hand-counted ~850,000 multiply-adds per block for this shape is consistent with the measured
scalar timing at roughly one op per cycle. **The 25 % NFR-PERF-010 placeholder is retained, not
loosened** — the median is under it, but the required 99.9th-percentile gate is not met by this
unoptimized reference implementation, and closing that gap is recorded as required follow-up
work (R-4, §22), not assumed away by the favourable comparison to Eigen.

**D-9.1 (weight/state separation).** The Rust side's split is structural, not a convention:
`PreparedWaveNet` (`Sync`, immutable weights) and `WaveNetState` (per-instance history and
reusable scratch, never shared) are different types. Read directly from
`NeuralAmpModelerCore`'s source, per D-9.1's instruction not to assume this: its `Conv1D` class
(`NAM/conv1d.h`) holds weights (`_weight`, `_bias`) and mutable per-instance state
(`_input_buffer`, a ring buffer) as fields of the *same* object, held by value throughout
`Layer`/`LayerArray`/`WaveNet`, with no `Arc`/shared-pointer separation anywhere in the type.
Every loaded C++ model instance therefore owns an independent copy of its own weights.
**FR-CLAP-090's cross-instance weight sharing is not achievable with this core as it stands** —
it would require modifying `NeuralAmpModelerCore` itself, not merely binding to it. This is
exactly the risk D-9.1 flagged, confirmed rather than assumed.

**Decision on OQ-1: Rust.** FR-NAM-030 is met with wide margin by either implementation, so
accuracy doesn't discriminate between them. NFR-PORT-040 (no C++ toolchain to build Namir) and
NFR-PORT-030 (mobile) both favour Rust outright regardless of the numbers above — the C++
option keeps a C++ toolchain in Namir's own build (not just the reference generator's) and has
no mobile story. D-9.1 favours Rust structurally: the C++ core would need to be modified, not
just wrapped, to satisfy FR-CLAP-090. The only point against Rust is NFR-PERF-010's
99.9th-percentile gate, and the evidence gathered here is that it is a closeable engineering
gap — same order of magnitude as Eigen with zero Rust-side vectorization — not a structural
ceiling on the approach. R-4 is downgraded on that basis, not retired: vectorizing the Rust
WaveNet inner loops (the dilated and 1×1 convolutions) is recorded as required work before 1.0
ships, not as a precondition for this decision.

**Scope note, recorded rather than silently narrowed:** this spike covers WaveNet only, matching
its own Method statement above. LSTM — also a Must under FR-NAM-020 — is unaddressed and remains
an open implementation risk, not resolved by this PASS.

### S-2 — Partitioned convolution cost curve (informs D-9.6)

**Question:** What partition schedule minimises worst-case per-block cost for IRs of 0.1–10 s at
block sizes 32–2048 and rates 44.1–192 kHz?

**Produces:** the default partition schedule; the IR-stage share of NFR-PERF-010.

**Result — 2026-08-05. PASS, with a significant recorded follow-up.** Spike at
`spikes/s2-ir-convolution/`.

A from-scratch Rust implementation of D-9.4's non-uniform partitioned convolution (direct
time-domain head partition equal to the block size, geometrically-growing FFT-based partitions
after it, via `rustfft`/`realfft` per D-9.6) was verified against a straightforward
time-domain reference (D-9.5) and then measured across the required matrix: IRs of 0.1–10 s,
block sizes 32–2048, rates 44.1–192 kHz. Fixtures — delta, delayed delta, decaying noise — are
generated per D-19.1, since convolution cost is a function of IR length, not tap values.
Sample rate decouples from the cost measurement itself (the engine has no notion of Hz; rate
only rescales the block period used to turn a raw time figure into a D-2.1 percentage), so the
sweep varied IR length directly in samples and re-derived each rate's percentage afterwards —
a legitimate 4x reduction in sweep size, not a narrowing of coverage.

**D-9.5 (correctness).** 480 cases (3 fixture kinds × 8 IR lengths × 5 block sizes × 4
schedules, including the uniform degenerate case) against the direct-convolution reference: **0
failures, worst error −119.91 dB** against a −100 dB tolerance itself well past any audible
threshold. The partitioning arithmetic — including the causality requirement that a size-*P*
partition at IR offset *off* is only computable in time if *off* ≥ *P*, which the schedule
guarantees by growing partition size only after exactly `growth_factor` partitions of the
current size, not a fixed count — is correct.

**Uniform partitioning is measurably the worst choice**, confirming D-9.4's rationale by
direct measurement rather than arithmetic alone: worst observed per-block cost ~44–48 ms,
several times any non-uniform candidate tested.

**Key finding, not anticipated going in, and the main result of this spike: same-size
partitions fire in lockstep.** Every FFT partition starts accumulating input at stream time
zero, independent of its own tap offset into the IR — so every partition of a given nominal
size completes its input window, and triggers its FFT, on the *same* block, forever. For a
multi-second IR under a schedule with a partition-size ceiling (`max_partition`), there can be
dozens of same-size partitions at that ceiling, and their entire combined FFT cost lands on one
recurring block instead of being spread out. This dominates worst-case cost far more than the
precise choice of `growth_factor` or `max_partition` within any reasonable range once the
same-size groups get large; `growth_factor` values above 2 make it *worse* (more partitions per
level piling onto the same block), and `growth_factor ≤ 2` consistently ties-or-beats 3, 4 and
8 across the grid.

**Consequence — this is not an edge-case finding, it reproduces at FR-IR-050's own Must
minimum.** At a 32-sample block against a 2-second IR (48 kHz — the *minimum* FR-IR-050
requires Namir to accept, paired with the *smallest* Must block size) the periodic same-tier
pileup alone costs on the order of 90–400% of that block's entire period across 44.1–192 kHz.
This was checked directly against `max_partition` values spanning 256 to 32,768 with **no
material improvement at any of them** — not assumed, measured. At longer IRs and block sizes up
to 128–256 it is far worse (many hundreds to several thousand percent over budget). Only at the
large-block end (1024–2048 samples) does the picture become "over budget by a factor of 2–4"
rather than catastrophic, because a bigger block gives each pileup more time to hide inside.

**This is a real gap in the naive synchronous scheme implemented here, not a flaw in D-9.4's
decision to go non-uniform** — uniform partitioning is strictly worse at every point measured.
The standard fix, out of scope for this spike, is to stagger same-size partitions' trigger
phases (equivalently: amortize each large FFT's computation across several block calls instead
of computing it synchronously in one), spreading a size-*P* group's cost across roughly
*P*/block_size blocks instead of concentrating it on one. Recorded as required follow-up work
before 1.0 — see R-8, §22 — the IR-stage analogue of R-4's NAM-vectorization gap from S-1.

**Decision on D-9.6: `growth_factor = 2`, `max_partition = 8192` samples.** Among the
candidates clustered near the achievable optimum (`max_partition` 4096–32768 at `growth_factor`
2 or 4, all within ~15% of each other at both the NFR-PERF-010 canonical condition and the
worst grid point), 8192 was marginally best or tied-best at NFR-PERF-010's own literal test
condition and carries the smallest FFT working-set memory among the close contenders.

**NFR-PERF-010 IR-stage share (resolves the remainder of OQ-2).** Measured single-core-pinned
per D-2.1/D-2.2 at the chosen default (block counts adapted from D-2.2's flat ≥100,000 per a
documented and reasoned deviation in `bench.rs` — the worst case here is *periodic*, not rare,
so far fewer samples give an equally reliable percentile; see the spike README):

| Condition | p99.9 | max |
|---|---|---|
| NFR-PERF-010's own condition (48 kHz, 64-sample block, 2 s IR) | 56% of one core | 94% of one core |
| Worst grid point (2048-sample block, 10 s IR, 192 kHz) | 254% of one core | 259% of one core |
| FR-IR-050 floor at the smallest block (32-sample block, 2 s IR, 48 kHz) | 99% of one core | 193% of one core |

**The IR stage alone, at NFR-PERF-010's own literal condition, already consumes roughly 2–4×
the entire 25% engine budget**, before adding NAM's own measured 41% (S-1), gate, or EQ. **The
25% NFR-PERF-010 placeholder is retained, not loosened** — matching S-1's precedent: OQ-2 exists
to establish the real numbers, not to move the target to fit an unoptimized reference
implementation. Closing this gap needs both R-4 (NAM SIMD) and R-8 (IR-stage phase-staggering)
before 1.0.

### S-3 — egui in an embedded plugin window (validates D-15.1, D-15.2)

**Question:** Can egui render into a baseview window parented to a CLAP host's window, on Windows
11, with correct input, DPI and resize behaviour (FR-CLAP-100, FR-CLAP-110, FR-UI-080)?

**Why it exists:** this is the one integration in the design that could not be verified from
sources. It was the highest-severity technical risk in §22.

**Produces:** either confirmation, or a costed fallback plan.

**Result — part 1 of 2, 2026-08-04. PASS.** Spike at `spikes/s3-egui-baseview/`.

The integration exists and is current: **`egui-baseview` 0.6.0**, MIT OR Apache-2.0, published
2026-07-15, maintained under the RustAudio organisation. It requires `egui ^0.35.0` — matching the
current egui exactly — and `baseview ^0.2.2`. Renderer backends `egui_glow` and `egui-wgpu` are
both offered; `opengl` (glow) is the default feature.

Measured on the §2 reference machine: full dependency tree (222 packages) compiles clean in
**23.9 s**; the spike renders **90 frames in a baseview window, closes itself, and exits 0 in
2.5 s**, with no interaction required. The spike source compiled without correction on the first
attempt.

**Two findings recorded so they are not rediscovered:**

1. `egui-baseview` 0.6.0 pins `baseview ^0.2.2`, which under 0.x semver **excludes** the published
   `baseview` 0.3.0. The lag is benign — baseview 0.3.0 was published 2026-08-02, after
   egui-baseview 0.6.0 — but Namir must pin **baseview 0.2.x** until egui-baseview catches up.
   Recorded as a dependency constraint, not a defect.
2. An apparent upstream break — `epaint 0.35.0` requiring an unpublished `epaint_default_fonts
   ^0.35.0` — was traced to a **stale local crates.io index cache** on the development machine,
   dated 2026-03-12. The raw sparse index confirms 0.35.0 exists, is unyanked, and declares MSRV
   1.92. Clearing the cached entry resolved it. Noted because the symptom looks exactly like an
   upstream publishing failure and cost real time to disprove.

**Result — part 2 of 2, 2026-08-04. PASS.** `EguiWindow::open_parented` was implemented in the S-4
spike against clack's GUI extension and loaded in Reaper on Windows 11. The editor renders
**embedded in the host's own window**, with a per-frame counter advancing continuously — proving
egui's render loop runs inside the host window rather than merely presenting a cleared buffer.

**R-1 is retired.** D-15.1 (egui) and D-15.2 (baseview) are fully validated for both the standalone
and plugin cases. FR-CLAP-100 is satisfied. FR-CLAP-110 (host-driven resize) is *not* covered — the
spike declares `can_resize() = false` — and remains to be implemented, but it is now ordinary work
rather than an unknown.

**Bridging detail worth keeping:** clack's `Window` does not implement `HasWindowHandle`. The
bridge to egui-baseview is `Window::borrow_handle_unchecked()`, gated behind the
`raw-window-handle_06` feature, and it is `unsafe`. Per D-5.3 this confines to `namir-clap` and
requires a written safety argument. Note also that `clack-extensions` needs its **`clack-plugin`
feature** enabled before any plugin-side `*Impl` trait exists — omitting it produces a confusing
"no such item" error rather than a helpful one.

### S-4 — clack against real hosts (validates D-14.2)

**Question:** Does a minimal clack plugin pass the CLAP validator and load correctly in at least
two real hosts (FR-CLAP-020, FR-CLAP-030)?

**Produces:** confirmation, or a decision to drop to `clap-sys`.

**Result — part 1 of 3, 2026-08-04. PASS.** Spike at `spikes/s4-clack-clap/`.

A minimal `clack` plugin builds as a `cdylib`, exports the `clap_entry` symbol, and **passes
`clap-validator` with 44 tests run, 15 passed, 0 failed, 0 warnings, 29 skipped, exit code 0**.
The 29 skips are extensions the spike does not implement (`params`, `state`, `audio-ports`,
`gui`); they are skips, not failures.

`clap-validator` itself is MIT, active (pushed 2026-07-24), and is **not published on crates.io** —
CI must install it from git and pin a revision.

**Extension coverage confirmed.** `clack-extensions` 0.1.1 exposes 29 named features, including
every extension the FRS requires: `gui` (FR-CLAP-100), `params` (FR-PARAM-\*), `state`
(FR-CLAP-050), `audio-ports` (FR-CLAP-030), `latency` (FR-CLAP-040), `tail` (IR decay),
`note-ports` (FR-CLAP-120), `thread-check` (useful for verifying NFR-RT-010), and
`preset-discovery`. This materially reduces the D-14.2 maturity concern: clack is thin, but it is
not partial.

**One finding for the CI gate:** the only validator failure was `features-categories` — a plugin
must declare one of `instrument` / `audio-effect` / `note-effect` / `analyzer`. Namir declares
`AUDIO_EFFECT` and `STEREO`. Recorded because it is metadata, not code, and is exactly the kind of
omission that ships unnoticed without FR-CLAP-020's automated gate.

**Second finding for NFR-QUAL-060:** on Windows with the MSVC toolchain, the linker writes to
stdout and Cargo surfaces this as a `linker_messages` warning on every `cdylib` build. It is not a
code defect. The "no warnings" gate must allow it explicitly, or it will either fail every Windows
build or train everyone to ignore warnings.

**Result — parts 2 and 3, 2026-08-04. PASS.** The plugin loads in **Reaper on Windows 11**,
appears in the FX browser under its declared name, and opens an editor embedded in the host
window with a live frame counter. FR-CLAP-030 is satisfied for one host; the second host of record
is `clap-validator` in CI (FR-CLAP-020).

**R-2 is retired.** D-14.2 (clack) is validated end to end: entry point, descriptor, audio
processing, host discovery, and the GUI extension.

**Install-path finding — this cost a failed first attempt and is a real product requirement, not a
spike detail.** Reaper does **not** scan `%APPDATA%\REAPER\UserPlugins\CLAP`; a plugin placed there
is silently invisible, with no error anywhere. The paths that work are the CLAP-specified ones:

| Path | Scope | Admin required |
|---|---|---|
| `%COMMONPROGRAMFILES%\CLAP` | All users | Yes |
| `%LOCALAPPDATA%\Programs\Common\CLAP` | Current user | **No** — this is where the spike was found from |

See D-13.3.

**Sequencing:** S-3 and S-4 are cheap and de-risk the two high-risk dependencies; run them first.
S-1 is the largest and gates the most numbers — **complete, 2026-08-05.** S-2 is also **complete,
2026-08-05.** All four spikes are done.

---

## 20. Disposition of FRS open questions

| OQ | Status | Where |
|---|---|---|
| OQ-1 — Rust vs C++ inference | Decided — Rust, S-1 PASS with a recorded performance follow-up (R-4) | §9.1, §19 |
| OQ-2 — real numbers for placeholders | Decided, S-1 and S-2 both complete: FR-NAM-030 90 dB and NFR-PERF-010 25 % both retained as-is. Measured shares — NAM 41 % (S-1), IR stage 56–94 % at NFR-PERF-010's own condition and up to several thousand percent at small block sizes (S-2) — already exceed the 25 % total on their own; closing the gap is required pre-1.0 work (R-4, R-8) | §2, §19 |
| OQ-3 — GUI approach | Decided — egui + baseview, S-3 validating | D-15.1, D-15.2 |
| OQ-4 — convolution partitioning | Decided — non-uniform, with a direct reference | D-9.4, D-9.5 |
| OQ-5 — glitch-free handover | Decided — four-step prepare/offer/crossfade/retire | D-8.1 |
| OQ-6 — where resampling happens | Decided — around the NAM stage only | D-9.2 |
| OQ-7 — state format | Decided — JSON, one hardened parser | D-11.1 |
| OQ-8 — cross-instance sharing | Decided — worker-only hash cache of `Weak` | D-8.2 |
| OQ-9 — the stage abstraction | Decided — split `StagePrep` / `Stage` traits | D-6.1 |
| OQ-10 — CLAP internal or external | Decided — external only; RD-3 is a leaf stage | D-14.1 |

---

## 21. Open items raised by this document

| ID | Item | Needed by |
|---|---|---|
| **AQ-1** | ~~Redistributable `.nam` and IR corpus.~~ **Resolved by D-19.1** — all automated fixtures are generated; captures are perceptual-review material only. | — |
| **AQ-2** | ~~Confirm D-9.8 (gate detector before input trim).~~ **Confirmed by the author, 2026-08-04.** D-9.8 stands. | — |
| **AQ-3** | Choice of embedded index store for D-12.3, within the stated constraints. | Test phase |
| **AQ-4** | Licence of NAM's standardised capture input signal, if any author capture is to be redistributed. Does not block the test phase (D-19.1), only the shipping of captures. | Before shipping factory presets |
| **AQ-5** | ~~Bass-amp DI tap point.~~ **Resolved 2026-08-04: DI is post-EQ; the limiter is switchable.** Two consequences for the capture session, recorded so they are not rediscovered afterwards: (a) the **limiter must be switched off** — it is time-variant and violates the constraint in D-19.1; (b) because the DI is post-EQ, the amp's EQ setting is baked into the capture, so the EQ must be set flat and its position recorded in the model metadata. | — |

---

## 22. Risks

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R-1 | ~~egui/baseview embedded-plugin-window integration does not exist in maintained form.~~ **RETIRED 2026-08-04.** S-3 parts 1 and 2 both PASS: egui renders standalone in baseview, and embedded in Reaper's own window via `open_parented` with a live frame counter. | Retired | — |
| R-2 | ~~`clack` is pre-1.0 with low adoption and may stall or break API.~~ **RETIRED 2026-08-04.** S-4 all parts PASS: clap-validator 15/15 with zero failures, loads and runs in Reaper, GUI extension works. Residual concern is pre-1.0 API churn, managed by exact version pinning and the `namir-clap` wrapper, not by redesign. | Retired (churn managed) | Pin exact versions; wrapper confines the blast radius. |
| R-3 | ~~No redistributable `.nam` test corpus.~~ **Downgraded High → Low** by D-19.1: fixtures are generated from a seed, so there is no licence surface and no capture dependency in CI. Residual risk is only that the generator produces numerically degenerate models, which D-19.1 addresses directly. | Low | D-19.1; generator validated against an analytic target. |
| R-4 | ~~NAM inference in Rust misses the accuracy or performance bar.~~ **Downgraded High-relevant-question → Low-Medium by S-1, 2026-08-05.** Accuracy: PASS with wide margin (-131 dB vs. a 90 dB floor). Performance: the reference implementation misses the NFR-PERF-010 99.9th-percentile gate (41 % vs. 25 %) at median-comparable cost to Eigen-vectorized C++ — the residual risk is narrowly "does a SIMD pass close this gap," not "is Rust inference viable at all." | Low-Medium (was Medium, pending resolution of the same open question) | Vectorize the Rust WaveNet's dilated/1×1 convolution inner loops before 1.0; re-measure against the unchanged 25 % budget. |
| R-5 | FR-IO-070 device-removal handling is weak in any cross-platform audio library. | Medium | Test with a failable virtual device, not the happy path. |
| R-6 | `hound` unmaintained since 2023. | Low | WAV is frozen; we own any bug. Vendoring is a viable last resort. |
| R-7 | Crossfade doubles NAM cost transiently, eating the NFR-PERF-010 budget. | Medium | The benchmark measures the crossfade, not just steady state (D-8.1). |
| R-8 | **New, from S-2, 2026-08-05.** Same-size IR partitions all start accumulating input at stream time zero, so every partition at a given size — including every partition at `max_partition`, of which a multi-second IR can have dozens — triggers its FFT on the *same* block, forever. Measured directly: at a 32-sample block against a 2 s IR (48 kHz — FR-IR-050's own Must minimum, paired with the smallest Must block size), this alone costs 90–400 % of that block's entire period, tested across `max_partition` 256–32,768 with no material improvement at any value. Schedule tuning (D-9.6) cannot fix this; it is a gap in the synchronous, non-staggered scheme itself. | High (for the small end of the required block-size range) | Stagger same-size partitions' trigger phases, or equivalently amortize each large FFT's computation across the ~`P`/block_size blocks its slack provides, instead of computing it synchronously in one. Not implemented in S-2; required before 1.0. |

---

## 23. Traceability

Every decision above cites the requirement it serves. The reverse mapping — every **Must**
requirement to the component that satisfies it — is generated and checked in CI per
FRS §10 and NFR-QUAL-010, and is not maintained by hand in this document.

---

## 24. Change log

| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-08-04 | Initial draft. Decides OQ-3 through OQ-10; defers OQ-1 and OQ-2 to spikes S-1/S-2 per agreed sequencing. |
| 0.2 | 2026-08-04 | D-19.1 added: all automated test fixtures are generated, not captured. Resolves AQ-1 and downgrades R-3 from High to Low, unblocking the test phase. AQ-4 (capture input signal licence) and AQ-5 (bass DI tap point) added. |
| 0.3 | 2026-08-04 | AQ-2 confirmed (D-9.8 stands) and AQ-5 resolved, both by the author. **S-3 part 1 executed: PASS.** egui-baseview 0.6.0 validated on Windows 11; R-1 downgraded High → Medium; baseview pinned to 0.2.x as a recorded constraint. S-3 part 2 (`open_parented`) folded into S-4. |
| 0.4 | 2026-08-04 | **S-4 part 1 executed: PASS.** Minimal clack plugin passes clap-validator 15/15 with 0 failures; clack-extensions confirmed to cover every FRS-required extension. R-2 downgraded High → Medium. Two CI constraints recorded: clap-validator must be installed from git (not on crates.io), and the MSVC `linker_messages` warning must be explicitly allowed under NFR-QUAL-060. |
| 0.5 | 2026-08-04 | **S-3 and S-4 complete: all parts PASS.** Plugin loads in Reaper with an embedded egui editor rendering live. **R-1 and R-2 both retired** — the two High risks in the design are gone. D-13.3 added, fixing CLAP install paths after Reaper was found to silently ignore `UserPlugins\CLAP`. Remaining spikes: S-1 and S-2. |
| 0.6 | 2026-08-05 | **S-1 executed: PASS, with a recorded follow-up.** OQ-1 decided in favour of Rust: FR-NAM-030 met with wide margin (-131 dB vs. a 90 dB floor); D-9.1's weight/state-coupling concern confirmed against `NeuralAmpModelerCore` source (no sharing mechanism exists, would need modification for FR-CLAP-090); NFR-PERF-010's 99.9th-percentile gate is not met by the unoptimized reference implementation (41 % vs. 25 %) despite being competitive with Eigen at the median, so R-4 is downgraded, not retired, pending a SIMD pass recorded as required pre-1.0 work. FR-NAM-030 and NFR-PERF-010 placeholders both retained as-is (OQ-2 resolved for NAM; IR-stage share still pending S-2). Scope note: S-1 covered WaveNet only, per its own Method — LSTM is unaddressed. |
| 0.7 | 2026-08-05 | **S-2 executed: PASS, with a significant recorded follow-up. All four spikes now complete.** D-9.6 finalised: growth factor 2, max partition 8192 samples, verified correct against a direct-convolution reference (480/480 cases, worst error -119.91 dB) and confirming D-9.4's non-uniform-over-uniform rationale by direct measurement (uniform's worst case ran 44-48 ms/block vs. non-uniform's much lower figures). **New finding: same-size partitions trigger their FFT in lockstep** (all start accumulating at stream time zero), so a multi-second IR's dozens of same-size partitions at the schedule's ceiling dump their combined cost onto one recurring block — measured to cost 90-400 % of a 32-sample block's entire period even at FR-IR-050's own 2 s minimum, across every `max_partition` from 256 to 32,768 tested. **New risk R-8** records this: schedule tuning alone cannot fix it — the proper fix (phase-staggering / amortized computation) is required pre-1.0 work, not implemented in this spike. OQ-2 now fully resolved: the IR stage alone measures 56-94 % of the 25 % NFR-PERF-010 budget at its own literal test condition, and NAM (S-1, 41 %) plus IR already exceed the total budget before gate or EQ; the 25 % placeholder is retained, not loosened, per the same reasoning as S-1. |
| 0.8 | 2026-08-05 | **Implementation begins.** Cargo workspace created (`crates/`, excluding `spikes/` from workspace discovery). `namir-core` (D-5.1's shared vocabulary types), `namir-engine` (D-6.1's `Stage`/`StagePrep` split, `Chain`, and D-7.5's RT-allocation test harness), and `namir-fixtures` (D-19.1's generator: WaveNet fixture shapes, all four D-9.5 convolution fixtures including a new minimum-phase design via the complex-cepstrum method, and fuzz-mutation seeding) built test-first. §17 gains `assert_no_alloc` (dev-dependency only) — D-7.5's harness needs a custom `GlobalAlloc`, and D-5.3's workspace-wide `forbid(unsafe_code)` cannot be locally overridden even in tests, so the `unsafe impl` lives in this dependency instead of in `namir-engine`. |
| 0.9 | 2026-08-05 | **D-9.11 added, resolving the M1-flagged NFR-QUAL-030 wording question (roadmap §15 item 1).** Records that the requirement's intent — a stated, numerical, reproducible correctness reference, never "by ear" — is already satisfied by S-1's cross-implementation NAM parity result and D-9.5's direct-convolution IR reference, not by literal "golden reference audio held in the repository," which is in tension with D-19.1's no-captured-audio commitment. No code changed; NFR-QUAL-030's text in the FRS is left as written. |
