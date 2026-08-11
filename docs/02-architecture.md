# Namir — Architecture

| | |
|---|---|
| **Project** | Namir |
| **Document** | 02 — Architecture |
| **Version** | 0.14 |
| **Date** | 2026-08-06 |
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

**Decision D-2.3** — The **x86-64 baseline instruction set is `x86-64-v3`** (AVX2 + FMA + BMI),
set in the checked-in `.cargo/config.toml` and scoped to `cfg(target_arch = "x86_64")` so
aarch64 targets keep their own default. Added during M3.

*Rationale:* Discovered during M3's performance work that no `target-cpu` was set anywhere in the
repository, so the whole workspace was compiling to the bare x86-64 baseline — SSE2, no AVX, no
FMA. The vectorized kernels this project relies on (`wide::f32x8` in `namir-nam`'s WaveNet inner
loops and `namir-ir`'s head convolution) were therefore emitting *two* 4-lane SSE operations per
8-lane vector operation. `namir-nam/src/wavenet.rs`'s own R-4 note had assumed the reference
machine's AVX2 support was being used; it was not, and nothing had ever checked. Measured on the
§2 reference machine, setting this baseline cut the NAM stage's p99.9 from 30.3% to ~10.5% of the
block period and the assembled six-stage chain's p50 from 10.34% to ~7.9% — the single largest
performance change found in that milestone, for no source change at all.

*Consequence:* Namir's x86-64 builds require a CPU supporting AVX2/FMA — Intel Haswell (2013) or
later, AMD Excavator (2015) or later, so every Zen part. Older x86-64 hardware is **not
supported**, and a binary built this way will fault with an illegal instruction there rather than
degrade. Accepted deliberately: the hardware floor is over a decade old, real-time neural amp
modelling is not a workload those parts can carry regardless, and the alternative (below) costs
more than it returns. CI runners must also meet this floor — a runner that does not will fail
loudly at test time, not silently produce wrong numbers. The `-100 dB` cross-implementation
numeric-parity tests were re-run under FMA and read `-130.8 dB`, unchanged: contracting a
multiply-add into a single rounding step is a smaller error than two separate roundings, not a
larger one.

*Rejected:* leaving the SSE2 baseline (measured to cost roughly 3x on the NAM stage's p99.9 —
this is not a marginal tuning knob); `target-cpu=native` (unshippable, produces binaries tied to
the build machine, and would silently make every developer's and CI runner's measurements
incomparable, defeating D-2.1's whole purpose); runtime CPU-feature dispatch with a scalar
fallback (the standard way to keep old-hardware support, but it needs either `unsafe`
`target_feature` functions — which D-5.3 forbids outside `namir-platform`/`namir-clap` — or a
duplicated, separately-parity-tested kernel per feature level, for hardware nobody is going to run
this on).

**Decision D-2.4** — D-2.2's 99.9th-percentile gate is **kept exactly as written**. What is added
is the set of **measurement conditions under which that percentile is valid**, plus a mandatory
**validity check** that must accompany any quoted figure. Added during M3, after that milestone
spent most of its effort attributing to Namir a tail that was not Namir's.

*Rationale:* M3 briefly appeared to force a choice between two statistics — the literal p99.9 (17%
to 52% across identical runs, unusable as a gate) and a contamination-immune per-residue-minimum
estimator (~15.2%, reproducible). Choosing the second because it passed would have been picking
the flattering measurement, which this project's methodology refuses. That dilemma turned out to be
false: p99.9 was unstable because of *how* it was being measured, not because the metric is wrong.
Measured under the conditions below, the same benchmark reports p99.9 = 16.5-17.1% across repeated
runs and agrees with the estimator to within ~1.8 points. A metric that is reproducible when
measured correctly does not need replacing — it needs its preconditions written down. This also
keeps NFR-PERF-010 verifiable exactly as the FRS specifies it ("*Verify:* B, as a CI regression
gate"), which a hand-computed estimator would not.

*Consequence:* every quoted NFR-PERF-010 figure must satisfy all of:

1. **Pinned away from cores that carry device interrupts.** Logical CPU 0 on the §2 machine absorbs
   `dxgkrnl.sys`'s ISRs (128-512 µs, ~165/second, zero on all other cores — measured by an elevated
   `xperf -on Latency` trace). ISRs run at DIRQL, above every thread priority, so this cannot be
   mitigated in software from user mode. CPU 2 carries the heaviest kernel DPC load and is likewise
   avoided. Benchmarks default to core 4; see `pin_to_measurement_core`.
2. **No unrelated load on the machine, verified rather than assumed.** M3 measured its own tooling
   (background agents, concurrent `cargo` builds) doubling p50 and tripling p99.9. "The machine
   looked idle" is not evidence; check what is actually running.
3. **At least five repetitions**, with the spread reported, never a single run. Four separate
   conclusions in M3 were announced from unreplicated readings and each had to be retracted.
4. **The validity check:** run `namir-engine/benches/tail_structure.rs` alongside, and compare its
   per-residue-minimum estimate against the raw p99.9. That estimator is immune to interference
   (interference is additive and aperiodic; the IR schedule is periodic, so the cheapest occurrence
   of each residue is the uncontaminated one). **If raw p99.9 substantially exceeds the estimator,
   the run was contaminated and the figure must be discarded, not quoted.** A clean run has the two
   within a couple of percentage points.

The estimator is therefore promoted to a permanent part of the methodology — not as the gate, but
as the instrument that tells you whether the gate's reading means anything. Reporting p99.9 without
it is how M3 lost several days to a GPU driver.

*Rejected:* replacing p99.9 with the estimator (drops a real property — that the *observed* worst
case matters to users — and departs from the FRS's own wording for no reason once the metric is
measured properly); gating on end-to-end latency including OS interference (that is a property of
the host machine, not of Namir, and directly contradicts D-2.1's framing of the budget as
per-instance and single-core; it is also unbounded by anything the project can engineer — the right
home for it is M6's thread-priority/affinity work and a deployment note, not this requirement);
loosening the 25% budget (nothing measured justifies it — the chain passes).

**Decision D-2.5 (added M5)** — D-2.1's "never as wall-clock time" is **scoped to audio-thread
per-block budgets**, not restated as a blanket rule. NFR-PERF-030/040/050/060 are stated in
wall-clock by the FRS itself and are not per-block audio-thread figures, so D-2.1 does not — and
was never meant to — forbid them; a milestone gating a wall-clock requirement is complying with the
FRS, not violating D-2.1. M5 is the first milestone to measure a wall-clock NFR (NFR-PERF-060,
FR-LIB-030's incremental library scan) and is the first to need this stated rather than assumed.

*Rationale:* D-2.1's own rationale is entirely about a *per-block audio callback budget silently
changing meaning when the block size changes* — that argument has no purchase on a one-shot,
off-audio-thread operation like a library scan or a model load, which has no block size to change
meaning against. Leaving D-2.1 unscoped would either force a nonsensical "fraction of one core"
restatement of a 2-second wall-clock budget, or tempt an implementer to quietly ignore D-2.1 without
saying so. Neither is acceptable in a project whose methodology (D-2.1 through D-2.4) already
exists to stop exactly that kind of quiet reinterpretation.

*Consequence — the additional measurement conditions a wall-clock, I/O-bound benchmark needs.*
D-2.4's four conditions (core selection, no unrelated load, ≥ 5 repetitions, the estimator
cross-check) are written for a CPU-bound per-block measurement; each is still necessary for an
I/O-bound one and none is sufficient on its own. A quoted wall-clock figure must additionally state:

1. **Page-cache state, per arm, never assumed.** A "second run is faster" claim conflates OS page
   caching with any incrementality the code itself provides unless the comparison holds cache state
   constant across the two arms being compared.
2. **Anti-malware state.** A real-time scanner dominates the cost of a many-thousand-file traversal
   and its overhead is non-deterministic run to run. State whether it was active.
3. **Volume, filesystem and cluster size named** — the same figure means something different on a
   different filesystem, and this document's own reference machine (§2) does not currently record
   its drive's filesystem.
4. **Corpus size, file count and tree shape printed by the benchmark itself**, not left to the
   reader to infer from the harness source, whenever the corpus is a synthetic stand-in rather than
   a captured real-world library (as D-19.1 requires it to be).
5. **mtime settling** — a corpus generated immediately before the benchmark run has every file's
   mtime reading "now", which can mask a change-detection rule's real-world granularity behaviour
   (see D-12.1's consequence note in §12).

*Rejected:* leaving D-2.1 as an unscoped blanket rule and either restating every wall-clock NFR as a
meaningless single-core fraction, or measuring wall-clock NFRs without ever writing down that D-2.1
does not apply to them (the FRS-vs-architecture-doc contradiction M5's own drafting hit, and the
reason this decision exists rather than being silently worked around).

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
| `namir-library` | Library index, scanning, hashing, search, persistence. | core, nam, ir, state | No | Yes |
| `namir-platform` | Filesystem locations, config dirs, logging sink, thread priority. **The only crate with `#[cfg(target_os)]`.** | core | Yes | Yes |
| `namir-worker` | Off-thread orchestration: load requests, resource cache, scan jobs. | everything above | No | Yes |
| `namir-ui` | egui-based interface. Renderer- and window-agnostic. | core, params, library, state | No | No *(M7)* |
| `namir-app` | Standalone application: audio device I/O, window, settings. | everything | Via platform + cpal | Not for 1.0 |
| `namir-clap` | CLAP adapter. **The only crate that names CLAP.** | everything except app | Via clack | No |

*Consequence (added M7)* — `namir-ui`'s "Builds for mobile?" cell is corrected from this table's
original "Yes" to "No". That original entry predated `namir-ui`'s actual construction at M6; once
built, it took a direct dependency on `baseview` 0.2 (`crates/namir-ui/Cargo.toml`) for
`egui-baseview`'s windowing. `baseview-0.2.2/src/platform.rs` compiles a backend only under
`#[cfg(target_os = "macos")]` / `"linux"` / `"windows"` — neither `"android"` nor `"ios"` matches
any of those, so the module is empty on both mobile targets and nothing using
`baseview::Window`/`WindowHandle` can compile there. Verified directly, not inferred: `cargo check
--target aarch64-linux-android -p namir-ui` was run on this session's machine (which does carry the
Android NDK and the `aarch64-linux-android`/`aarch64-apple-ios` `rustup` targets, unlike the M6
session's own Windows machine) and reached the dependency graph baseview pulls in without a
platform-mismatch error surfacing yet at the point it failed for an unrelated, already-documented
reason (`blake3`'s NEON C build step needing `aarch64-linux-android-clang`, the same gap this
document's CI job for the *other* mobile-capable crates already works around) — the platform-cfg
read above is a source-level fact about `baseview` 0.2.2, not a guess. `namir-ui` is therefore
excluded from `.github/workflows/ci.yml`'s `mobile-cross-build-android`/`-ios` `-p` lists rather
than added to them. This is a real, if distant, constraint on RD's mobile aspiration (P5): whichever
future milestone actually targets mobile will need a different UI windowing story for `namir-ui`
specifically (egui has other mobile-capable backends; `baseview` is desktop-only by design), not
just a CI list update.

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

*Consequence (added M5)* — `namir-library`'s row above previously read "Path handling only, via
`namir-platform`" in the *Platform code?* column while its *May depend on* column omitted
`namir-platform` — a contradiction `xtask layering`'s mechanical edge check (D-5.2) would reject the
moment `namir-library` tried to act on the first reading. `namir-platform` is also an M6 deliverable
and does not exist as anything but `DenormalGuard` when `namir-library` is built at M5. Resolved by
correcting the cell to "No": `namir-library` never learns where library roots or its index file
live. `LibraryService::open(index_path, roots)`, in `namir-worker`, takes both as constructor
arguments — the same discipline `namir-worker`'s pre-existing `LoadSource::File` already applies
("the *caller* supplies the path, so this crate never assumes a filesystem layout"). M6's product
shells obtain the real paths from `namir-platform` and pass them in; `namir-library` stays
unaware of `namir-platform` at every point in the roadmap, not only until M6.

*Consequence (added M9's P0 decision pass, 2026-08-08 — a policy question this decision never
covered, plus a correction to how it was first answered)* — the pass asked whether `namir-clap` may
carry `unsafe` inside a `#[cfg(test)]` module or a `benches/` file, to build FR-CLAP-130's
`assert_no_alloc` harness and NFR-PERF-040's instantiation benchmark. **It may not, and —
established before deciding rather than after — it does not need to.** No amendment to this
decision, and no new designated module.

*The mechanism, because the question was first put on a false premise.* The justification offered
was that such code is "legal because the crate sets `unsafe_code = "deny"` rather than `"forbid"`".
`deny` is not permission: it fails the build exactly as `forbid` does. What `deny` permits and
`forbid` refuses is a *later* `#![allow(unsafe_code)]` inside the file itself, and that inner
attribute — not the crate's lint level — is what makes `crates/namir-platform/src/denormal.rs:10`,
`crates/namir-platform/src/thread_priority.rs:46` and `crates/namir-clap/src/gui.rs:71` legal. Cargo
applies a package's `[lints]` table to every target it builds, benches and integration tests
included, so a `namir-clap` bench file genuinely *could* opt itself back in where the same file in
any `forbid` crate could not. The question is therefore a policy question, not a compile error,
which is why it is answered here rather than left to whoever writes the harness first.

*The policy.* This decision's carve-out names crates, and within them modules of **shipped** code
carrying a written safety argument. **It does not extend to test or bench targets, and M9 designates
no new module in either crate.** The workspace's whole `unsafe` surface stays at three files across
two crates.

*Why that costs nothing here — verified in-tree, not reasoned about.* `assert_no_alloc` needs no
`unsafe` of ours: §17's own note and `crates/namir-engine/src/rt_harness.rs:6-11` already spell out
that the `unsafe impl GlobalAlloc` lives in the dependency, and
`crates/namir-worker/tests/rt_stress.rs:64-65` installs it inside a crate taking the workspace
`forbid` unchanged. `core_affinity` likewise (`crates/namir-worker/benches/resource_load.rs:64,75`,
same crate, same lint). There is in fact **no `unsafe` in any bench or integration test anywhere in
this workspace today** — a whole-word `unsafe` scan over every `.rs` file under a `benches/` or
`tests/` directory in `crates/` returns nothing. On the `clack` side most of what a harness needs is
safely constructible: `Events`'s two fields are `pub` (`clack-plugin` 0.1.1 `src/process.rs:63-68`),
`InputEvents::from_buffer`/`OutputEvents::from_buffer` are safe `const fn`s (`clack-common` 0.1.1
`src/events/io/input.rs:68`, `output.rs:97`), `ChannelPair` is a public enum with public variants
(`clack-plugin` `src/process/audio/pair.rs:381`) — so `audio.rs`'s `prepare_channel`
(`crates/namir-clap/src/audio.rs:271`), a free `fn` over `ChannelPair`, is directly testable — and
`PluginAudioConfiguration` is a plain public-field struct (`clack-common` `src/process.rs:72-79`).
Only `Audio`/`PairedChannels` and the `HostInfo`/`Host*Handle` `from_raw` family have no safe
constructor (`clack-plugin` `src/host.rs:26,258,341,427`, every one of them an `unsafe fn from_raw`;
`:26` is `HostInfo`'s and is additionally `const`, the other three are the three handle types'), and
**none of them carries any Namir logic.** D-18.6 reaches those through a real in-process host held
as a `namir-clap` dev-dependency rather than through an `unsafe` block of ours — the same instinct
§17 records twice already, for `assert_no_alloc` and for `rtrb`: when `unsafe` is unavoidable, it
goes in a dependency, not in this tree.

*Also corrected, being a matter of fact rather than of policy:* `AGENTS.md` described the two
carve-out crates as "confined to one module each". `namir-platform` has carried **two** designated
modules since M6 (`denormal.rs`, `thread_priority.rs`); `crates/namir-clap/src/gui.rs`'s own doc
comment repeats the error in miniature at `:68` ("exactly one designated module can opt back in", in
a sentence that goes on to name two files). Both are corrected in M9a.

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

*Consequence (added M4, from building it):* the sizing formula `min(2, cores-1)` yields **zero** on
a single-core machine, and a zero-thread pool never runs a job — every model load would hang
forever, which is a total failure of FR-NAM-070 rather than P8's "degrades". The implementation
therefore clamps to a floor of one (`namir-worker`'s `pool_size`, with a test sweeping `0..=1024`
cores). The formula's evident intent is "at most two, and leave a core for audio"; on a one-core
machine there is no core to leave, and a worker may block anyway, so it yields naturally.
NFR-PORT-030 is satisfied more strongly than the formula requires — the pool is at most two
threads, created once at construction and never grown. `namir-worker` takes **no** third-party
dependency for any of this; see §17.

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

*Consequence (added M4, from building it):* "waits and retries" is implemented **bounded** — a short
spin, then sleeps at sub-block granularity (500 µs against NFR-PERF-010's own 1.333 ms block
period), then a two-second deadline after which the command is handed *back* to the caller. A
literal unbounded retry is a liveness hazard this decision does not discuss: a host that deactivates
a plugin stops calling `process` entirely, so the ring is never drained again, and an unbounded
retry would wedge a pool thread permanently — with a two-thread pool (D-7.1), two such submissions
wedge the whole worker. The operative word above is **silently**: a command is given up on only
after a bounded wait, with a catalogued error (`worker.submit.not_delivered`) and a reported
outcome, and the value itself returns to the caller so even that drop happens on a worker thread.

*Consequence (added M4):* the two rings' full-ring policies have to compose, which this decision and
D-8.1 do not say separately. When the **return** ring is full, the audio thread declines to consume
the next handover command rather than popping one it has nowhere to park the displacement for. The
command therefore stays in the command ring, back-pressure reaches the producer, and the rule above
absorbs it. The honest cost is head-of-line blocking — a deferred offer also stalls parameter
changes queued behind it — which is acceptable because the only way to reach that state is a worker
that submits without draining, and the worker drains before it submits. The state is published as
`telemetry.engine.deferred_blocks` rather than being invisible.

*Consequence (added M6, an API gap found building `namir-app`, not yet fixed anywhere):* this
decision's own wording — "a mutex on the producer side... serialises UI and worker submissions" —
and `namir_worker::submit::CommandSubmitter::try_submit`'s own doc comment ("this is what the UI
thread uses") both describe an architecture where the UI thread and a worker thread share *one*
`CommandSubmitter` for one engine instance. `namir_worker::Instance` (M4) does not actually let
that happen: `Instance::new` takes ownership of the whole `WorkerEndpoint`, including its one
`RingProducer<Command>`, and wraps it in a **private** `CommandSubmitter` field. `Instance`'s
public surface is exactly `new`/`drain_retired`/`load`/`unload`/`recall` — nothing submits a bare
`Command::Param` or `Command::Reset`, and nothing exposes the submitter for a caller to do so
itself. A product shell therefore cannot use `Instance` for ordinary per-knob-turn parameter
changes at all — the single highest-frequency interaction the whole system has. `namir-app`
works around this without modifying `namir-worker` (two agents were building product shells
against it concurrently this round, so a structural change was deliberately left for a coordinated
follow-up rather than made unilaterally): it does not construct an `Instance`, and instead builds
its own `Arc<CommandSubmitter>` directly from `namir_engine::split`'s `WorkerEndpoint`, shared
between the UI thread (`try_submit`) and its own re-derived load/unload/recall orchestration
(`crates/namir-app/src/engine_live.rs`, which reuses every substantial piece —
`ResourceCache`, `Command::load_nam`/`load_ir`, `namir_state::candidates`/`FileResolver` — and
only re-derives the thin ordering glue `Instance` would otherwise have provided: R-7's
serialisation wait and the drain-before-submit sequence). `namir-clap` needs the identical fix and
should not have to rediscover this independently — flagged here for both crates' sake. The
smallest closing change would be additive: `Instance::submitter(&self) -> Arc<CommandSubmitter>`
(or an equivalent that lets a caller share the ring producer `Instance` already owns), with no
existing signature changed.

*Consequence (closed M6):* the smallest closing change was built, not the one first sketched above
— `Instance::try_submit_param(&mut self, ParamChange) -> Result<(), SubmitError>`
(`crates/namir-worker/src/lib.rs`), a single non-blocking `try_submit` call behind `&mut self`
rather than an accessor onto the whole producer. Smaller than exposing `Arc<CommandSubmitter>`
itself: it does not let a caller reach `submit`'s *blocking* form (only `Instance::load`/`unload`/
`recall` should ever block a worker thread on a handover), and it needs no new field on `Instance`,
only a two-line method forwarding to the submitter it already owns privately. `namir-app`'s
substitute (`crates/namir-app/src/engine_live.rs`'s `LiveEngine`, ~575 lines re-deriving R-7's
serialisation window, a file-size-checked read, and FR-STATE-070's locate loop — all three already
real, tested code inside `Instance`/`namir_worker::recall`) is deleted in the same pass that adds
this method: `namir-app` now builds a real `Instance`, shared behind a `Mutex` between its GUI
thread and worker thread (`crates/namir-app/src/instance.rs`'s `SharedInstance`) the same way
`namir-clap`'s `SharedInner` already shared one behind `Mutex<Option<Instance>>` — the two crates'
independent M6 solutions to "share one `&mut Instance`-shaped thing across threads" converge once
both can actually hold an `Instance` at all.

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

*Consequence (added M3, from an audit finding — the guard type is built but **not yet wired in**):*
`namir-platform`'s `DenormalGuard` exists and is unit-tested, but as of M3 it is referenced
**nowhere outside `namir-platform` itself** — not by `namir-engine`, not by any benchmark, not by
anything. M1 built the type and no milestone has yet engaged it, so **NFR-RT-030 currently holds
only by accident** (no measured stage happens to drive values subnormal today), not by
construction. This is tracked as a required M6 deliverable in
`03-implementation-roadmap.md` §10.

Where it must be engaged is constrained, and the constraint is worth stating so nobody wires it
into the wrong layer: **`namir-engine` cannot do it.** D-5.1's layering table (enforced by
`xtask layering`) permits `namir-engine` to depend only on `namir-core`, `namir-params`,
`namir-dsp`, `namir-nam` and `namir-ir` — not on `namir-platform`. So `Chain::process` must not
acquire the guard itself; it must be acquired once per callback by whoever owns the callback,
which is `namir-app` (its `cpal` stream callback) and `namir-clap` (its `process()` entry point),
both of which D-5.1 does permit to depend on `namir-platform`. That placement also matches this
decision's own "once per audio callback" wording, rather than once per stage or once per block.

A consequence for measurement, recorded because it affects how existing numbers should be read:
the M3 benchmarks call `Chain::process` directly and therefore run *without* FTZ/DAZ engaged. Their
figures are valid for what they measure, but they are not evidence about NFR-RT-030 either way, and
the benchmark that verifies NFR-RT-030 (method **B**: drive each stage with a signal decaying into
the denormal range, assert processing time stays within 10% of nominal) still has to be written.

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

*Consequence (added M4, correcting a real gap in step 1 above):* the worker prepares the **whole
stage slot**, not just the `Arc`. This decision says step 3 "needs no allocation" because "the stage
is prepared with capacity for two live resources" — true of the slot *array*, but not of the slot
*contents*. Installing a bare `Arc<PreparedNam>` still has to build this instance's `NamState` and,
at a mismatched rate, a whole `rubato` resampler pair and its FIFOs; `NamSlot::new` and
`IrSlot::new` are both documented "not RT-safe" for exactly that reason. So a handover command
carries a built, **boxed** slot, which also makes D-7.2's "carries a pointer, never the model"
literally true rather than an argument, and keeps the ring's preallocated element a pointer
regardless of how large a slot grows.

*Consequence (added M4):* step 4's "pushes the old `Arc` into a return ring" is implemented as
pushing the **whole slot**, for the same reason — an `IrState`'s convolution ring buffers are as
illegal to free on the audio thread as the `Arc` is. This decision names the `Arc` because that is
the *subtle* half (it only deallocates when the refcount happens to reach zero, so it hides in
testing), not because it is the only half.

*Consequence (added M4):* there is a **second** audio-thread drop site this decision does not
anticipate. An offer arriving while a handover is still in flight replaces the slot currently fading
*in*, and replacing it drops it. That was harmless while the loader was documented non-RT and only
tests called it; once offers arrive through the ring they install on the audio thread, so it is
closed the same way — the displaced slot is moved to the retire pen, never dropped.

*Consequence (added M4):* when the return ring is full, a completing crossfade **defers its own
finalization** rather than dropping the outgoing slot to make progress. The audio is already correct
at that point (the fade has saturated, and the stage runs the incoming slot alone rather than
blending in an outgoing one scaled by `cos(pi/2)`, which is -4.4e-8 in f32 rather than zero), so
only bookkeeping waits. This is the concrete shape of this decision's own "memory is retained but
audio continues. Degradation, not failure (P8)".

*Consequence (added M4, from measuring the protocol rather than reasoning about it):* **a NAM and
an IR handover are never in flight simultaneously.** This decision says nothing about two stages
handing over at once, and R-7 assumed the NAM crossfade alone was the budget risk. Measured, neither
stage's crossfade alone exceeds NFR-PERF-010 — both together do, and only together. `namir-worker`
therefore serialises them: before offering a handover for one target, an instance waits out any
handover it recently offered for the other, on a worker thread, which D-7.1 permits. Serialised, the
condition measures 22.20–24.63% against the 25% budget; unserialised it measures 28.77–31.26%.

*Consequence (added M4):* the user-visible effect of that rule, stated so it is a decision rather
than a surprise — changing model *and* IR in one action (loading a preset, once M5 exists) starts
the second changeover roughly 20 ms after the first rather than simultaneously. Neither FR-NAM-070
nor FR-IR-060 forbids this: each requires *its own* changeover to be crossfaded and glitch-free, and
neither requires the two to coincide. The rule is per-instance, so two plugin instances may still
crossfade at the same time — correct, because NFR-PERF-010's budget is itself per-instance.

*Consequence (added M5):* **an unload is a handover to nothing, and is therefore also subject to
the rule above.** FR-STATE-070 says "the state shall load with that stage empty" when a preset's
model or IR reference cannot be resolved — closing that gap needed a way to *remove* an installed
resource, which this decision's four steps never anticipated (they only ever add one). M5 adds
`Command::Unload`/`Chain::unload` and a `NamStage`/`IrStage::unload` method apiece: step 3's
crossfade already treats a `None` slot as a dry passthrough on either side of the fade (needed
already for the very first load into an empty stage), so fading *into* `None` reuses that same
state machine rather than adding a second one, and the outgoing slot still leaves through step 4's
return ring, never dropped. Because it is a handover like any other, it is exactly as capable of
overlapping the *other* target's handover and reproducing R-7's over-budget condition — a preset
recall that unloads one stage while loading the other is not a hypothetical, it is what a preset
with only one of the two references set does. `namir-worker`'s serialisation rule (the consequence
above) therefore treats `Unload` the same as `Load` when M5's `recall.rs` submits it.

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

*Consequence (added M4, and this decision's stated key is insufficient without it):* the **IR cache
key must be `(content hash, engine rate, block size)`**, not the content hash alone.
`PreparedIr::from_wav_bytes` bakes both extra arguments into the prepared object — it resamples to
the engine rate at load time (FR-IR-030), and the block size determines the head partition, the
whole D-9.4 schedule and its R-8 stagger, and `PreparedChannel::block_size`. That last one is not a
subtlety: `process_block` **asserts** the block length its schedule was built for, so a hit keyed on
content alone could hand one instance an IR prepared for another's smaller block, and the failure
mode is a *panic on the audio thread*, not a wrong sound. `PreparedNam` genuinely needs no widening
— `namir_nam::load` is a pure function of the bytes.

*Consequence (added M4):* FR-CLAP-090's sharing is therefore conditional for IRs and unconditional
for models. Two instances share an IR only if they agree on engine rate *and* declared maximum block
size — which they normally do, since a host drives every instance identically, but it is not
guaranteed. NAM weights, which are the bulk of the memory FR-CLAP-090 is about, share regardless.

*Consequence (added M4):* "process-global" is implemented as an injected `Arc<ResourceCache>` with a
single `OnceLock`-backed default (`ResourceCache::shared()`), rather than as a bare static. A static
as the *only* access path is untestable under `cargo test`'s threaded runner — every test in the
binary would share one cache, so "holds exactly one entry", "was reaped" and "exactly one copy
exists" all race, and those are precisely the assertions this decision and FR-CLAP-090 need. The
honest cost: the guarantee becomes "both product shells pass `shared()`", one call site each,
checkable by review rather than unavoidable by construction.

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

*Consequence (added M9a, 2026-08-09) — the review this decision asked for, finally held. D-9.8
stands; the FRS is amended to match it.* The Rationale's last clause flagged this for review, and
§21's **AQ-2** records the author confirming it on 2026-08-04 — but confirmed it in isolation,
against the usability argument alone. **The Rationale's first clause is false, and has been since it
was written.** The FRS does specify this, and specifies it against D-9.8: FR-CHAIN-010 mandates
`input → input trim → noise gate → NAM → IR → EQ → output level → output`
(`01-functional-requirements.md:163-166`), while `build_default_chain` ships `gate → trim → nam →
ir → eq → out` (`crates/namir-engine/src/stages/mod.rs:47-67`). Not an ambiguity to be read one way
or the other — two documents stating contradictory facts about the same six stages, one of which had
to move.

**Resolved in D-9.8's favour: the FRS is amended to the shipped order** (done in
`01-functional-requirements.md` in this same pass), not the code corrected to the FRS's. A gate
whose threshold references the interface's actual noise floor rather than walking under the user's
trim hand is the better product, nothing in the milestones since has argued otherwise, and the
divergence was deliberate at every point — so what was wrong was a governing document describing a
product this project had decided not to build. Recorded precisely because §1's authority order is
unchanged: this document does not edit the FRS, the FRS's owner did, which is a **different route**
from the one D-9.11 and D-23.1 took for NFR-QUAL-030 and NFR-QUAL-010, where the requirement's text
was left standing and only the route to satisfying it was recorded here. That route was unavailable
here. There is no reading under which a chain can satisfy both orders at once.

*How long this stood, and why nothing caught it.* Both texts landed in `875068e`, the first commit
carrying either document, so the contradiction is exactly as old as the documents and predates every
milestone. It survived AQ-2's confirmation, then M2 building the chain, then every milestone since
shipping it, then M9's P0 decision pass. **It was never hidden**: `stages/mod.rs:31-36` has stated
the divergence in plain prose since M2's `7941577` — naming D-9.8, naming FR-CHAIN-010's "literal
prose order", citing roadmap §6 as directing it — and every reader since has read past it. Visible
and unresolved for seven milestones, not concealed for seven milestones, which is the more
uncomfortable of the two findings and the one worth keeping on the record.

*Nothing mechanical detects a requirement-versus-code contradiction of this kind, and nothing here
was ever going to.* `xtask traceability` asks whether some artifact references an identifier, never
whether the artifact agrees with the requirement's text; FR-CHAIN-010 has carried a tag throughout
and the tool has read it covered throughout, correct by its own rules the entire time — the same
blind spot D-23.1's Rationale names for quantified requirements, one step further out. `layering`,
`params-lock` and `attribution` read dependency edges, a parameter manifest and licence metadata,
none of which is requirement prose. What found it was M9a's set-quantification sweep reading
FR-CHAIN-010's own text beside its artifact in order to answer D-23.1's two questions — a byproduct
of that reading, not a design pass, and not something any of the five gates could have produced. The
sweep's second product, after the fifty-four partials, and the argument for the reading being
periodic rather than once.

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

### 9.5 NAM Architecture 2 (A2) — added M8-planning

This subsection belongs to §9.1's NAM stage; it is appended here rather than inserted there so the
decision numbering stays monotonic and no existing text moves.

**Decision D-9.12** — A2 support **extends the existing WaveNet parser and inference path** rather
than adding a second architecture beside it. An A2 file declares `architecture: "WaveNet"` exactly
as an A1 file does; what differs is the `config` object's schema, not the network family. The
private `enum Architecture` seam in `namir-nam`'s `model.rs` therefore stays private and stays as
it is — A2 is a wider `config` grammar behind that seam, not a new public trait, not a parallel
`PreparedNam` variant, and not a second inference module. Scope is **core A2 only**: enough to load
and run the **A2-Full** and **A2-Lite** configurations (FR-NAM-150), no more.

*Rationale:* The architecture tag is the only thing a parser can dispatch on cheaply, and A2 does
not change it. Introducing a public architecture trait to accommodate a config-schema change would
export an extension point the FRS does not ask for and that D-6.1's `Stage`/`StagePrep` split
already covers at the level that matters (the engine sees `PreparedNam`, never a network kind).
Keeping the seam private also keeps the blast radius of a future A3 inside one crate.

*Consequence — FR-NAM-140 is a prerequisite, not a by-product.* Today an A2 file fails with
`nam.load.malformed_json` ("not valid JSON"), because A2 layers dropped the scalar `kernel_size`,
dropped the `gated` bool, and turned `activation` from a string into an object — a shape the A1
schema rejects at the deserialiser, before any architecture check runs. That message is actively
misleading: the file is well-formed JSON. FR-NAM-140's distinct "unsupported feature" error is
therefore worth shipping *ahead* of A2 inference, so that a user with an A2 model gets a true
statement about why it did not load even while the answer is still "not yet supported."

*Consequence — the `config` grammar widens in specific, enumerated ways.* `kernel_sizes[]` (an
array, replacing the scalar `kernel_size`); `activation` as an object or an array of objects;
`gating_mode` replacing the `gated` bool; plus `bottleneck`, `groups_input`,
`groups_input_mixin`, `layer1x1`, `head1x1`, `in_channels`, and a nested `head` object — which
`namir-nam` currently *rejects* outright whenever `config.head` is non-null. Each of these is a
parser change; none of them is an architecture change.

*Consequence — new DSP primitives in `namir-nam`, not in `namir-dsp`.* Grouped dilated
convolution, bottleneck expand/contract, per-layer variable kernel size (namir's `Conv1D` assumes
one kernel size for a whole layer array), the 1×1 residual/skip projections, a convolutional head,
and the activations LeakyReLU, SiLU, PReLU, Softsign, Hardswish and LeakyHardtanh. These are
WaveNet-internal shapes, not primitives another stage would reuse, so they stay behind the NAM
crate's own boundary per D-5.1.

*Consequence — weight-layout order must be re-derived, not assumed.* A1's flat weight-vector
ordering was established by reading `NeuralAmpModelerCore` directly and proven to −131 dB (S-1,
§19). A2's ordering must be re-derived the same way, from `NAM/wavenet/detail.h` and
`NAM/wavenet/params.h`, and then **proven** by a new `namir-fixtures` A2 generator acting as a
parity oracle — an independent from-scratch A2 reference compared numerically, per D-19.1 and the
pattern D-9.11 makes general. This is not optional diligence: a silently-wrong weight order
produces a model that loads cleanly, runs at the right cost, and sounds plausible while being
wrong, which is precisely the failure mode NFR-QUAL-030 exists to forbid and no amount of listening
will catch. Recorded as **R-9** (§22), the highest-severity item this work carries.

*Consequence (added 2026-08-08, from `NeuralAmpModelerCore` PR #264) — pin which build of the
reference the oracle is measured against.* That PR bumps the vendored Eigen submodule from a 3.4-era
dev snapshot to 5.0.1 (merged 2026-06-10) and documents, with hashes, that the reference's **default
Eigen GEMM path is not bit-exact across the bump** — max absolute difference 9.5e-8, mean 5.7e-9, on
a signal peaking at 0.071. Namir links no Eigen and is unaffected as a dependency (NFR-PORT-040's
no-C++-compiler build is the standing proof). What it affects is the *target*: S-1 already reasoned
that −131 dB sits "in the range of float32 rounding-level disagreement… consistent with
`NeuralAmpModelerCore`'s own Eigen-version-bump measurements of ~1e-7 typical difference" (§19), and
PR #264 is that prediction measured directly rather than cited — so it **confirms** the existing
parity claim rather than unsettling it. The operational consequence is narrow but real: a
re-measurement against a differently-built reference can move in the last digits for reasons that
have nothing to do with Namir, so any parity run must record *which* reference build produced the
target. PR #264 also supplies the lever — the `NAM_USE_INLINE_GEMM` path **is** bit-exact across the
bump, because it bypasses Eigen's matrix product entirely. **Build the reference with
`-DNAM_USE_INLINE_GEMM` for A2's oracle and for any re-anchoring under roadmap §15's item 4**, so
the target is reproducible and a future Eigen bump cannot silently move it.

*Consequence — FR-NAM-090/100 stop being blocked.* `namir-nam` currently declares loudness
normalisation and calibration out of scope *because* they need metadata fields the `.nam` schema
this crate reads does not carry. A2-era files carry `loudness`, `input_level_dbu` and
`output_level_dbu`, which removes exactly that blocker. The requirements are then a matter of
deciding how to apply the figures, not of the data being absent.

*Consequence — the performance budget should get easier, but not for free.* A2 is claimed to cost
30–40 % *less* CPU than A1, which would give NFR-PERF-010 headroom rather than take it. Namir's
`wide::f32x8` kernels (D-2.3, R-4) assume dense convolutions, though; grouped and bottleneck
convolutions need their own vectorized variants before any of that claimed saving shows up in a
measured p99.9 on the §2 reference machine. Until it is measured there under D-2.4's conditions,
the saving is a claim, not a number this project may cite.

*Explicitly deferred, by decision rather than by omission:* `SlimmableContainer`, `condition_dsp`,
FiLM conditioning, and the `.namb` container. None of the four is needed for A2-Full or A2-Lite,
each is a distinct piece of work with its own verification burden, and bundling them into the same
milestone would put the R-9 parity work behind three unrelated features. They are revisited as
their own decision when a requirement asks for them.

*Consequence (added M10, 2026-08-09) — the naming this decision left implicit, resolved.*
"A2-Full"/"A2-Lite" do not appear anywhere in `NeuralAmpModelerCore`'s own source. They map to
upstream's **A2 standard** (`channels == bottleneck == 8`) and **A2 nano** (`channels == bottleneck
== 3`) — `NAM/wavenet/a2_fast.h`'s own header comment and its strict shape detector
(`a2_fast.cpp`'s `is_a2_shape`) are the source. Recorded once, at
`crates/namir-fixtures/src/nam/mod.rs`'s `A2Shape` doc comment, per D-9.11's precedent (a
lower-authority document records the route to satisfaction, not a rewording of the FRS's own text).

*Consequence (added M10, 2026-08-09) — the performance claim above, measured, not assumed.* Built
scope turned out narrower than "grouped dilated convolution... the convolutional head" implied:
core A2's own detector requires `groups_input == groups_input_mixin == layer1x1.groups == 1`, so no
real A2 shape ever exercises a grouped kernel, and none was built — `wide::f32x8`'s existing dense
kernels are the whole of what core A2 needed. Measured (three interleaved runs, this crate's own
inference in isolation, not the certified full-chain figure): A1 Standard 10.57–10.88% of one core,
A2 Full 8.34–8.68%, A2 Lite 1.92–2.28% (p99.9). A2 Full costs *less* than A1 Standard despite more
than double the layers (23 vs. 10) — the narrower per-layer channel count and the absence of any
grouped-conv overhead more than offset the extra layer count, for this specific comparison. Not the
30–40% figure this decision's own text names, and not certified under D-2.4 — recorded as the
actual measurement now available, in place of the "a claim, not a number" caveat above, which
stays as written since a different shape or a certified run could still move it either direction.

*Consequence (added M10, 2026-08-09) — R-9 retired.* See §22's risk register row.

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

*Consequence (added M5, flagged rather than closed) —* FR-STATE-010 requires the state format to
cover "the complete user-settable state", but two such values have no `ParamDescriptor` at all:
FR-CHAIN-030's global bypass and FR-CHAIN-090's output ceiling are fields on `namir_engine::Chain`
directly, set via `Command::SetGlobalBypass`/`SetOutputCeilingDb`, not `Command::Param`. §11's
`namir-state` therefore covers them with a second, parallel mechanism — a `global` document section
backed by nothing but a plain struct — rather than folding them into `parameters`/`REGISTRY`. That
is not a bug (both values genuinely have no natural `ParamDescriptor` home: bypass in particular is
usually transport-level in a host, not an ordinary automatable control), but it means there are now
**two** mechanisms for user-settable values in one format, and M6's CLAP adapter will want bypass
exposed as a **host** parameter, which this shape does not provide for on its own. Evidence the gap
was noticed once already and left unclosed: `namir_params::descriptor`'s own test module carries a
fully-formed `out.channel_mode` descriptor that was never moved into `REGISTRY`. ~~Flagged here as
a decision M6 needs to make (a new `D-10.4`, once taken), not solved by this milestone pre-emptively
guessing at CLAP's own shape for host-exposed bypass.~~ **Resolved by D-10.4** (added M6): both
values are now real `ParamDescriptor`s and the `global` document section is retired. See D-10.4
below.

**Decision D-10.4 (added M6)** — `global.bypass` (FR-CHAIN-030) and `global.output_ceiling_db`
(FR-CHAIN-090) are declared as ordinary `ParamDescriptor`s (`namir_params::global::GLOBAL_BYPASS`/
`OUTPUT_CEILING_DB`) and added to `REGISTRY`, exactly like every stage's own parameters.

*Rationale:* D-10.3's own consequence note above records the gap this closes. Both values are real,
user-settable, host-automatable state (FR-STATE-010) that had no `ParamDescriptor` home: routed
instead through dedicated `Chain::set_global_bypass`/`set_output_ceiling_db` methods, dedicated
`Command::SetGlobalBypass`/`SetOutputCeilingDb` variants, and a parallel `global` section in
`namir-state`'s document format — a second, special-cased mechanism alongside the
`parameters`/`REGISTRY` path every other control already used. M6's `namir-clap` is the concrete
reason this needed deciding now rather than later: a CLAP host expects to control bypass through
its own automation surface like any other parameter, not through a side channel only Rust code
could reach. Giving both values a real descriptor removes exactly the special case FR-PARAM-030
("parameter changes shall be accepted from the UI, CLAP automation, and preset loading, and
converge to the same engine state regardless of source") already requires every other control to
satisfy.

*Consequence:* `namir_engine::Chain::apply`/`Command::Param` now carry `global.bypass`/
`global.output_ceiling_db` changes the same way they carry every stage's own parameters —
`Chain::apply` recognises the two ids itself before falling back to broadcasting to every stage.
The dedicated `Command::SetGlobalBypass`/`SetOutputCeilingDb` variants are retired; `Chain`'s own
`set_global_bypass`/`set_output_ceiling_db` methods remain as the low-level setters `Chain::apply`
and this crate's tests call directly. `namir-state`'s document format drops the separate `global`
section in favour of two more `parameters` entries (`global.bypass`, `global.output_ceiling_db`);
D-11.2's tolerant/versioned deserialisation reads an existing document's old `global` section as a
fallback for whichever of the two keys `parameters` doesn't itself carry, so an already-saved
`.namirpreset` file still loads correctly, but every save now writes the new shape.
`docs/04-state-and-preset-format.md` §5/§9 are updated accordingly.

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

*Consequence (added M5)* — This decision's own "stable key ordering" consequence above is
undermined by a repository-level default nobody had checked against it: `.gitattributes`'s `*
text=auto eol=lf` normalises every text file's line endings on commit. A `.namirpreset` file is
plain JSON text, so it would fall under that rule — meaning a serialiser regression that started
emitting `\r\n` would be silently repaired by Git before it ever reached a diff or a checked-in
test fixture, and the very corpus meant to catch that class of bug (`crates/namir-state/tests/
corpus/`, asserting on the writer's raw output bytes per NFR-PORT-050) would pass against a broken
writer. **Corrected:** `.gitattributes` marks `*.namirpreset binary`, alongside the existing
`*.nam`/`*.wav`/`*.bin` entries — a preset/state document is JSON text by construction, but must be
treated as opaque bytes for this one purpose, the same way this project's other structured-text
fixtures already are.

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

*Consequence (added M5)* — D-5.1 puts the dependency edge as `namir-library → namir-state`, the
opposite direction from what "the library index must maintain a hash → path map" above might
suggest is needed for resolution to work. Resolved by splitting the algorithm from its data:
`namir-state` defines the **order** (`resolve::candidates`, yielding library-relative, then
absolute, then content-hash, always in that sequence since `hash` is non-optional) and a
`FileResolver` port with one method per step; `namir-library` implements the port. The trait runs
against the dependency edge, not with it, so no edge reversal is needed.

*Consequence (added M5)* — Two things D-11.3 as originally written left unstated, both closed by
FR-STATE-070's own rationale ("failing to open a project because a file moved is unacceptable;
failing silently is worse"):

1. **Which library root** a stored relative path is relative to, when FR-LIB-010 permits several.
   Resolved: the reference stores only the relative path, with no root identity, and a resolver
   tries every configured root in configured order. Storing a root index or name would embed
   machine-specific data in the one field D-11.3 exists to keep portable (UC-3: sending a project to
   someone whose roots are named differently).
2. **What a path hit whose content does not match the recorded hash means.** P7 ("identity is the
   content hash, paths are hints") and FR-STATE-070's own rationale together require that this is
   **not** treated as a resolution: silently loading a different amp under an old path is exactly
   the "failing silently" the rationale calls worse than failing outright. A library-relative or
   absolute path hit is verified against the recorded hash before being accepted; a mismatch falls
   through to the next candidate exactly as a missing file would, and the near-miss (the path that
   was tried, and what it actually hashed to) is carried into the failure report so a future UI can
   offer "use it anyway" as an explicit choice rather than a silent default.

---

## 12. Library subsystem

**Decision D-12.1** — The library index is an on-disk table of `(path, size, mtime, content hash,
extracted metadata)`, persisted between sessions and updated incrementally by comparing size and
mtime before rehashing.

*Traces:* FR-LIB-030, NFR-PERF-060 (10 000 files rescanned in ≤ 2 s — achievable only because
unchanged files are not rehashed).

*Consequence (added M5) — the rule as originally written contradicts FR-LIB-070.* FR-LIB-070
requires that files which "change … shall be reflected in the library within one rescan". A file
edited in place to the same length, within the same filesystem's mtime granularity as the previous
scan, is invisible to "comparing size and mtime" taken literally — and a hand-edited `.nam`
metadata field is exactly this same-length case, not an edge case. FR-LIB-070 cannot honestly close
against D-12.1 as originally stated. **Corrected rule:** a file is rehashed if its size differs,
**or** if its mtime differs, **or** if its mtime falls within the previous scan's own completion
timestamp plus the filesystem's mtime granularity (NTFS: 100 ns claimed, ~1 s to ~2 s observed
depending on volume; treated conservatively as 2 s) — i.e. a file that could plausibly have changed
*during or immediately after* the scan that indexed it is rehashed the next time regardless of what
its mtime reads. This costs nothing in the common case (an unchanged library's files have mtimes far
older than any recent scan) and closes the window D-12.1's literal wording left open.

*Consequence (added post-M6 close, found while answering a user's report that the CLAP plugin
couldn't see files scanned via the standalone app)* — D-12.1's own incremental rule assumes every
caller scans against the *same* root(s) session to session; nothing enforces that assumption. M6's
`namir-clap` had opened its `LibraryService` with an empty root list, on the theory that "no UI to
configure one yet" made that harmless. It wasn't: a scan against zero roots still completes (there
is nothing to walk), and a complete scan concludes every path it didn't see is removed — so
clicking "Rescan library" inside the plugin didn't just fail to find new files, it actively erased
every entry `namir-app`'s own correctly-rooted scan had already committed to the identical
`library-index.json` both products read and write. Fixed by moving the default-root computation out
of each product shell and into `namir_worker::library::LibraryService::open_default`/`open_at`
(`crates/namir-worker/src/library.rs`), the one function both `namir-app` and `namir-clap` now call
— see that function's own doc comment, and
`crates/namir-worker/src/library.rs`'s
`two_opens_of_the_same_config_dir_share_a_root_and_a_second_scan_does_not_erase_the_first` test,
which exists specifically to keep this from recurring a third way.

**Decision D-12.2** — Scanning is a cancellable worker job reporting progress; the UI never waits
on it (FR-LIB-020, FR-UI-060).

*Consequence (added M5)* — D-5.1 forbids `namir-library` from depending on `namir-worker`, so
"cancellable worker job" is necessarily split: `namir-library`'s scanner is a caller-pumped step
machine (`Scanner::step`, doing at most one directory expansion or one file examination per call
and returning progress), with cancellation expressed as the caller simply not calling it again.
`namir-worker` owns the thread, the cancellation flag and the progress cadence, driving the step
machine on its existing pool. `namir-library` needs no concurrency primitives and never learns
threads exist. A cancelled scan commits every record it already examined — discarding correctly
hashed work would make cancellation pure waste — but **suppresses the removal list**: a scan that
did not see the whole tree cannot conclude a file it didn't reach is gone, and treating "not seen"
as "deleted" would silently empty a user's library on every cancelled scan, violating both P8 and
FR-LIB-070's "never crash Namir or the host" spirit (an emptied library is a data-loss failure mode,
not a crash, but the requirement's intent is the same: a missing file must degrade gracefully, not
propagate as false information).

**Decision D-12.3 (AQ-3 resolved — added M5)** — The index is stored as a single pretty-printed
JSON document, written whole and replaced atomically (temp file, `sync_all`, `std::fs::rename` over
the destination — which replaces an existing file on both Unix and Windows, so no
platform-conditional code is needed). This is **not** a copy of D-11.1's state-document choice made
by default; it is AQ-3 decided against D-12.3's own constraints (no copyleft dependency, no C/C++
dependency per NFR-PORT-040, corruption degrades to a full rescan rather than a crash or wrong
results per P8), with reasoning recorded here rather than left to the code.

*Rationale:* FR-LIB-040's free-text search has no key by which it could be an indexed lookup — it
filters over every record's name and every metadata field — so the whole index must be resident in
memory regardless of how it is stored on disk (at 10 000 records of a few hundred bytes each, well
under 5 MB). An embedded key-value store's entire value proposition — random access to one record
without its neighbours — is therefore a property this workload has no use for: the index is a
rebuildable cache, not a database. A single JSON document reuses `serde_json`, already the one
hardened parser D-11.1 chose specifically so there would be only one to fuzz (P6) — a second,
third-party binary format would be a second attack surface owned by someone else. Atomic
whole-file replacement makes a torn write **impossible by construction**: a reader sees either the
complete old file or the complete new one, never a partial one, which satisfies D-12.3's corruption
clause by construction rather than by recovery logic. Any other read failure (missing file, wrong
`format_version`, malformed JSON) yields an empty index and a warning rather than an error — the
next scan repopulates it, which is D-12.3's "degrades to a full rescan" exactly as stated.

*Rejected:* an append-only log with compaction (D-12.3's other named option) — it can tear on a
crash mid-append, which atomic whole-file replacement cannot, and it needs its own compaction
policy, for an incremental-write saving (avoiding rewriting ~3 MB) measured at roughly 10 ms, which
does not justify the added failure mode against a workload NFR-PERF-060 already budgets 2 seconds
for. `redb` 4.1.0 (MIT OR Apache-2.0, verified 2026; one transitive dependency, `libc`, unix-only) —
it clears the licence bar but carries a build script, as does `libc`, and §17's adoption bar for a
new dependency (set by `rtrb`'s adoption: "zero transitive dependencies, no build script,
`no_std`-capable pure Rust, MSRV far below this workspace's own") is not met on three of its four
criteria. D-17.1 rejected `symphonia` over a licence nuance on a **Should** requirement; taking on
an embedded B-tree store's build-script and cross-compilation risk (both new crates must build for
`aarch64-linux-android`/`aarch64-apple-ios`, NFR-PORT-030) for a 5 MB rebuildable cache on a **Must**
is a weaker case than that one was, and D-17.1 already set the precedent for how this project
weighs that trade.

**Decision D-12.4 (for RD-1)** — A library entry carries an `origin` field from the outset —
`Local` in 1.0, extensible to a remote source later. Tone3000 integration then adds a variant
rather than a schema migration across every user's index.

*Consequence (added M5)* — `Origin` also carries an `Unknown(String)` catch-all in addition to
`Local`, so a 1.0 build reading an index a later build wrote keeps the record rather than dropping
it. This is D-12.4's own "adds a variant rather than a schema migration" applied in the direction
D-12.4 did not originally state: forward-compatibility of the *reader*, not only of the *format*.

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

*Consequence (added M6, from building `namir-app` — corrects the claim two paragraphs above):*
**"cpal covers these" was wrong for WASAPI exclusive mode specifically.** Reading `cpal` 0.18.1's
own WASAPI backend source (`host/wasapi/device.rs`, both `build_input_stream_raw_inner` and
`build_output_stream_raw_inner`) shows the share mode is a hardcoded `AUDCLNT_SHAREMODE_SHARED`
local with no parameter, feature, or extension trait anywhere in the crate to request
`AUDCLNT_SHAREMODE_EXCLUSIVE` instead — checked against the vendored source directly, not inferred.
Shared mode, ALSA, and CoreAudio are all covered as this decision originally said; exclusive mode
is not, on any platform, through this dependency as pinned. `namir-app` cannot work around this
itself: D-5.3 confines `unsafe` to `namir-platform`/`namir-clap` (plus a future SIMD kernel
module), and a raw WASAPI `IAudioClient::Initialize(..., AUDCLNT_SHAREMODE_EXCLUSIVE, ...)` call
needs exactly that. Closing this gap needs either a `namir-platform`-owned unsafe WASAPI-exclusive
helper (mirroring `DenormalGuard`/`elevate_current_thread_priority`'s existing pattern) or an
upstream `cpal` change; neither is built as of M6. `AppSettings::exclusive_mode` exists as a
forward-compatible persisted field so a future fix needs no settings migration, but nothing reads
it yet. See `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md` for the full record.

*Consequence (added M6, from building it):* `namir-app` implements this decision: `AudioBackend`/
`AudioStream` (`crates/namir-app/src/audio_io.rs`) are the Namir-owned trait, with a real `cpal`
implementation confined to that one module — verified against real WASAPI hardware in this
session (a PreSonus AudioBox 22VSL interface): device enumeration, sample-rate/buffer negotiation,
and a real opened, playing stream, all recorded in
`docs/manual-tests/fr-io-010-device-enumeration.md`'s executed run. FR-IO-080 (settings
persistence, including the degrade-gracefully-on-a-missing-remembered-device case) was also
verified against the real filesystem and a real fallback in the same session — see
`docs/manual-tests/fr-io-080-settings-persistence.md`. FR-IO-050 (latency) and FR-IO-060 (xruns)
are built and unit-tested for the parts that do not require real hardware to observe (buffer-based
latency estimate, dropout counting) but not for their real-hardware-only halves (a true measured
loopback figure; inducing a genuine backend xrun) — see those two manual-test docs. FR-IO-070
(device removal) has its report/stop-cleanly machinery built and tested against every piece not
requiring a device that can be told to fail on command, but R-5's own literal ask (such a device)
remains unmet, as §22 already anticipated it might. FR-IO-090 (channel mapping) is implemented for
a single physical input channel (optionally remapped) but not for `ChannelConfig::Stereo`'s
genuinely independent two-channel input.

*Consequence (added M8-planning, 2026-08-08):* the choice the M6 note above left open — a
`namir-platform`-owned unsafe WASAPI-exclusive helper *or* an upstream `cpal` change — is now
decided, as **D-13.4** below: a Namir-maintained **fork of `cpal`**, pinned by commit. This
decision's own text stands; what changes is only which of the two routes it named gets built.
FR-IO-020's exclusive-mode half is closed through D-13.4, not through this decision's original
"cpal covers these" claim, which the M6 note already corrected.

**Decision D-13.2** — Filesystem locations, config directories, log sinks and thread priority
elevation live in `namir-platform` and nowhere else (P5, NFR-PORT-020).

*Consequence (added M6, from building it):* `namir-platform` reaches this decision's full scope.
`paths.rs` computes Namir's own config directory (`%APPDATA%\Namir` / `~/Library/Application
Support/Namir` / `$XDG_CONFIG_HOME/namir` else `~/.config/namir` — not specified anywhere in the
FRS or `docs/04-state-and-preset-format.md`, so this follows each OS's own documented convention)
and a log-sink path beneath it; `thread_priority.rs` adds
`elevate_current_thread_priority`. Both are pure path/outcome computation with no I/O and no
audio-callback wiring — that wiring is out of this crate's own scope by construction (D-5.1 does
not let `namir-engine` depend on `namir-platform` at all) and is explicitly `namir-app`'s/
`namir-clap`'s job, the same split D-7.4's own M3 consequence note already states for
`DenormalGuard`. `thread_priority.rs` is therefore built, unit-tested, and **not yet called from
any audio thread** — recorded so it is not mistaken for wired-in, the same distinction that note
draws for the denormal guard. See that module's own doc comment for exactly when and how a future
caller should invoke it, and §17's dependency register for the one new dependency it takes
(`libc`, Linux/macOS only) and why.

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

*Consequence (added M6, from building it):* `namir-platform`'s `clap_paths.rs` implements exactly
this table as `clap_install_dir(ClapInstallScope::{PerUser, SystemWide})`, computing a path only —
no directory creation, no install, no privilege escalation, per this milestone's own scoping ("just
return the path, don't attempt privileged install logic"). A regression test
(`per_user_is_not_appdata_the_path_s4_found_reaper_ignores`) pins the specific failure mode S-4
found: a lookup keyed on `%APPDATA%` instead of `%LOCALAPPDATA%` must resolve to `None`, not
silently fall back to the path Reaper is known to ignore.

*Consequence (added M8-planning, 2026-08-08) — this table says **where**, and had been read as
also implying **what**; it does not.* On Windows and Linux the artifact placed at these paths is
the built shared library, renamed to `Namir.clap`. On macOS it is **not**: a `.clap` there is a
**bundle directory**, `Namir.clap/Contents/{Info.plist, PkgInfo, MacOS/<dylib>}`, and a
`libnamir_clap.dylib` simply renamed to `Namir.clap` is something no host will load. This is
CLAP's own definition, not a macOS convention layered on top of it — `entry.h` defines a plugin's
`plugin_path` as the DSO on Linux and Windows but as the *bundle* on macOS. `docs/user-guide.md`
stated the renamed-library rule uniformly across all three platforms and was therefore wrong for
macOS; it has been corrected in this planning pass, and the requirement itself is now carried
explicitly by **FR-PKG-020** rather than left as an unstated implication of this table. D-18.3's
`xtask bundle` is the mechanism that produces the bundle; without it there is no macOS artifact to
put at either path in this row.

*Consequence (added M13, 2026-08-11) — the Linux system-wide cell is too narrow, and the installer
deliberately disagrees with it.* This table records `/usr/lib/clap`. Fedora, RHEL, openSUSE and
other multilib distributions use **`/usr/lib64/clap`**, and D-18.3's Linux paragraph already
required the installer to "detect rather than assume". `packaging/linux/install.sh` therefore
probes: an existing `clap` directory at either path wins outright; otherwise a real, non-symlink
`/usr/lib64` containing `libc.so.6` selects `lib64`. That test rather than a bare `[ -d /usr/lib64 ]`
because the naive check is wrong twice — Debian and Ubuntu have a real `/usr/lib64` holding only the
loader, and Arch's is a compatibility symlink to `/usr/lib`.

*The divergence this creates is real and is left standing, on a decision taken at M13.*
`namir-platform`'s `clap_paths.rs` still returns the literal `/usr/lib/clap` for
`ClapInstallScope::SystemWide`, pinned by its own unit test. So on a multilib system the installer
places the plugin at one path while the product's own lookup names the other. The alternative —
teaching `clap_install_dir` to probe — was rejected because that function is a **pure path
computation that performs no I/O**, a property its module doc states, its injectable-`getenv` test
design rests on, and the M6 note above records as deliberate scoping. Trading it away for one
platform's edge case is the larger loss.

*What it costs, named rather than left to be discovered:* this row's own consequence that
"FR-ERR-050's diagnostic bundle should record which CLAP paths exist and what they contain" is
weakened on multilib systems — the bundle would inspect `/usr/lib/clap`, find nothing, and report
the plugin absent when it is installed one directory away. That is precisely the support question
this decision exists to make answerable, so it is a real gap and not a theoretical one. **M9b owns
it**, alongside the rest of FR-ERR-050's diagnostics work; the fix is for the diagnostic to report
every candidate path rather than for `clap_paths` to pick one.

**Decision D-13.4 (added M8-planning; resolves the choice D-13.1's M6 note left open)** —
FR-IO-020's WASAPI **exclusive** mode is closed by a **Namir-maintained fork of `cpal`** that adds
`AUDCLNT_SHAREMODE_EXCLUSIVE` as a requestable share mode, consumed as a git dependency **pinned by
commit**. The alternative D-13.1 named — a `namir-platform`-owned unsafe WASAPI helper — is
rejected.

*Rationale:* The share mode is not a parameter cpal exposes and forgot to plumb; it is a hardcoded
local inside `build_input_stream_raw_inner`/`build_output_stream_raw_inner`, chosen at the
`IAudioClient::Initialize` call in the middle of cpal's own stream-construction sequence (verified
against the vendored 0.18.1 source at M6 — see D-13.1's note). A `namir-platform`-owned helper
cannot reach into that sequence; it would have to own the whole Windows stream lifecycle in
parallel — device enumeration, format negotiation, event-driven buffer servicing, error and
device-removal reporting — meaning Namir would ship two independent Windows audio paths, one per
share mode, with FR-IO-050/060/070/080 needing to hold on both. Changing one line's worth of share
mode in the library that already owns that lifecycle is a far smaller, far better-tested surface
than reimplementing it, even counting the cost of maintaining a fork.

*Consequence — this is a real dependency change, not a patch.* A git dependency touches three
things at once. (a) §17's register gains a row for the fork, distinct from the upstream `cpal` row,
marked prospective until it is actually built. (b) `cargo-deny`'s `[sources]` policy must be
extended to allow that specific git URL — the default posture is to reject non-registry sources,
and that rejection is doing its job, so the allowance is narrow and named rather than a blanket
`allow-git = []` relaxation. (c) NFR-SEC-040's reproducible-build ambition is weakened by a git
dependency in a way a crates.io dependency does not weaken it; pinning by commit hash rather than
by branch is the minimum mitigation, and vendoring the fork into the tree is the fallback if the
pin proves insufficient. Recorded as **R-10** (§22).

*Consequence — no settings migration is needed.* `AppSettings::exclusive_mode` already exists as a
persisted field, added forward-compatibly at M6 precisely so a later fix would not require one. It
is currently written and never read; this decision's implementation is what finally reads it, wiring
it through `namir-app`'s `AudioBackend`/`AudioStream` trait (`crates/namir-app/src/audio_io.rs`) —
the Namir-owned seam D-13.1 requires, so nothing outside that module learns that a share mode
exists.

*Consequence — the fork is a liability with a stated exit.* The change should be offered upstream;
if it is accepted, this decision reverts to a plain version bump on the existing §17 `cpal` row and
the git-source allowance is removed. If it is not, the fork must be rebased as upstream moves, and
that ongoing cost is the substance of R-10 rather than an afterthought. The fork's diff is
deliberately kept minimal — a share-mode parameter and the format-negotiation consequences of it,
nothing else — so that rebasing stays mechanical.

*Consequence — exclusive mode is a mode, not a default.* It takes exclusive control of the device
and will fail to open where shared mode succeeds. FR-ERR-020's catalogue therefore needs an entry
for "exclusive mode unavailable on this device/format," and the settings path must degrade to
shared rather than leave the app with no audio — the same graceful-degradation behaviour FR-IO-080
already requires for a missing remembered device.

*Consequence (added M11, 2026-08-11, from building it) — built and adopted; what this decision
predicted, and what it did not.* The fork exists and closes FR-IO-020's exclusive-mode
half on real hardware: `https://github.com/ErwanLegrand/cpal`, branch `wasapi-exclusive-mode`,
pinned by `rev = "2edbacb44b10e56801e5dbfa251517fb2c9e2ef4"` in `crates/namir-app/Cargo.toml`, with
the narrow named `[sources]` allowance this decision required (`deny.toml`'s `allow-git`, one
repository, `unknown-git = "deny"` left standing for every other). §17's fork row is no longer
prospective. The parts of this decision that predicted correctly are recorded as such rather than
re-argued: no settings migration was needed — `AppSettings::exclusive_mode`, written since M6 and
never read, is read for the first time here — and FR-ERR-020's catalogue gained
`app.audio_io.exclusive_mode_unavailable` (`crates/namir-app/src/error_codes.rs`) at **`Warning`**,
not `Error`, because the session still has audio, which is what this decision's
degrade-to-shared consequence asks for. Two things fell out that this decision did not name. The
share mode is settled **once, before any stream opens**, by an all-or-nothing rule that ANDs both
devices' answers (`crate::app::negotiate_share_mode`): one direction granting exclusive mode is not
enough, because a single mode indicator cannot truthfully describe a half-exclusive duplex path, and
`docs/03-implementation-roadmap.md` §18 rules out "a mode indicator that lies". And exclusive mode
does no format conversion — the fork drops `AUTOCONVERTPCM`/`SRC_DEFAULT_QUALITY` there because
WASAPI rejects both — so `crates/namir-app/src/audio_io/convert.rs` converts `f32` to and from `I32`
and `I24` inside the audio callback, `I16` being **deliberately excluded** rather than unfinished:
an undithered 16-bit truncation would silently degrade a device that ran perfectly well in shared
mode. Without that converter FR-IO-020 would have closed on a path that essentially never activates
on real hardware, since `f32` looks universal on Windows only because shared mode's `GetMixFormat`
reports the *engine's* mix format.

*Consequence (added M11, 2026-08-11) — the API shape, and why D-5.1's layering lint chose it.* The
fork adds `cpal::platform::{ShareMode, WasapiStreamOptions, WasapiDeviceExt}`: a `#[non_exhaustive]`
options struct and an extension trait mirroring `DeviceTrait`'s configuration queries *and* its
stream builders, re-exported from `cpal::platform` with **no `cfg` of any kind**. The types compile
on every platform and `WasapiDeviceExt` is implemented for the platform-dispatch `Device`
everywhere, refusing exclusive mode at *runtime* — including in the configuration queries, so a
pre-flight probe gets an honest "no" instead of a shared-mode answer that reads as a yes. That
unconditional surface is not a stylistic preference; it is the only shape this workspace can
consume. `namir-app` is the crate that must name these types, D-5.1 confines
`#[cfg(target_os)]`/`#[cfg(windows)]`/`#[cfg(unix)]` to `namir-platform` and `xtask layering`
enforces that on every merge, and `namir-platform` may depend on `namir-core` alone — so it cannot
wrap `cpal` on `namir-app`'s behalf, and there is no escape hatch. A `cfg`-gated surface would have
left no legal way to use the fork at all. Two other shapes were considered against the same
constraint and rejected: a new `HostId` (PR #843's shape, which duplicates device enumeration) and a
field on the shared `StreamConfig` (PR #1195's shape, which two upstream maintainers have pushed
back on in favour of extension traits). A side effect worth naming: because the surface is
unconditional, every path below it — the exclusive probe included — is reachable from a headless
Linux test run.

*Consequence (added M11, 2026-08-11) — two real defects were found only by running on hardware,
after every automated check had passed.* Both are upstream `cpal` bugs that exclusive mode exposed
rather than caused, and both are independently upstreamable. **(a) Channel mask.**
`config_to_waveformatextensible` sent `dwChannelMask = KSAUDIO_SPEAKER_DIRECTOUT` (0) for every
format. In shared mode that is harmless — the audio engine accepts it — but in exclusive mode the
format reaches the driver, and a PreSonus AudioBox 22VSL refuses a zero mask: 24-in-32 at 48 kHz
reads `AUDCLNT_E_UNSUPPORTED_FORMAT` with mask 0 and `S_OK` with `FL|FR`, every other field
identical. Microsoft's own HD Audio driver accepts either, so the Realtek and HDMI endpoints on the
same machine engaged exclusive mode throughout and hid it — the defect was invisible until a
third-party USB interface was tried. Fixed in fork commit `ab5f40a`. **(b) Container
justification.** WASAPI stores 24 valid bits **left-justified** in a 32-bit container;
`dasp_sample::I24` is right-aligned in the low 24. `cpal` wrote one as the other — a factor of 2^8
in both directions: render about 48 dB too quiet, capture 256x too large, so the input meter pinned
on anything above near-silence. Both directions confirmed on the reference machine before the fix.
Fixed in fork commit `2edbacb`, the revision now pinned.

*Consequence (added M11, 2026-08-11) — why those two are recorded here and not only in the
manual-test file.* Nothing internal to this project could have found either one, and the reason is
structural rather than a gap someone could close with more tests. The conversion arithmetic in
`crates/namir-app/src/audio_io/convert.rs` is correct and exhaustively unit-tested at its
boundaries; the `WAVEFORMATEXTENSIBLE` was verified by hexdump; the fork type-checked for
`x86_64-pc-windows-msvc`; the workspace's 913 tests passed and CI was green on all three platforms —
with both defects present. In each case the disagreement was about the *container convention around*
correct arithmetic, and both sides of that boundary agreed with themselves, so no in-process test
could see the gap: one defect is observable only where the driver is, the other only by a person
listening to a speaker. This is the concrete argument for FR-IO-020 carrying `*Verify:* M` and for
D-18.6's rule that a `Verify: M` Must is traced by its manual document — here the residue a real
machine and a human are needed to observe was not a supplement to the automated evidence, it was the
only place the bugs existed. See `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md` for the
executed script and the per-endpoint readings.

*Consequence (added M11, 2026-08-11) — the diff is far larger than this decision's own minimality
instruction anticipated.* "Deliberately kept minimal — a share-mode parameter and the
format-negotiation consequences of it, nothing else — so that rebasing stays mechanical" describes a
change of a few dozen lines. The fork as pinned is seven commits on upstream trunk `e0893c3`:
**2867 insertions and 144 deletions across 11 files.** Some of that cannot ever conflict —
`src/platform/wasapi_ext.rs` is a new 456-line file and `examples/wasapi_exclusive_probe.rs` a new
1443-line diagnostic example — but `src/host/wasapi/device.rs` alone changed 546 insertions against
130 deletions, and that file *will* conflict on a rebase. The cause is structural, not sloppiness: a
per-call extension trait — the shape upstream itself wants — means threading the share mode through
every enumeration, negotiation and stream-building path, where a hidden global or a `HostId` would
not. The instruction above is not withdrawn; a smaller diff is still better, and nothing here was
added that a share-mode-aware API does not need. What is withdrawn is the expectation that followed
from it, that rebasing stays mechanical. **§22's R-10 carries the corrected estimate**, so a future
reader budgeting a rebase reads it there rather than from this decision's original sentence. One
further note on the base: the fork is taken from upstream **trunk** at `e0893c3` — `version =
"0.18.1"` plus the unreleased commits after the release-preparation commit — and not from the
crates.io 0.18.1 release, because forking the release would have started **eight** WASAPI fixes
behind, one of them a device-lost panic fix (`bc101ac`).

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

**Decision D-16.4 (added M8-planning, 2026-08-08)** — FR-ERR-010's diagnostic log is implemented as
a **small, Namir-owned, bounded rotating writer in `namir-platform`**, with no logging dependency
adopted. Verbosity is configurable (off / errors / info / verbose) from settings and overridable by
an environment variable for a support session. Rotation is bounded by size with a fixed small
number of generations, so the log can never grow without limit — NFR-SEC-020's bounded-allocation
posture applied to disk. The sink path is `namir-platform`'s existing `log_file_path()`, which
D-13.2 already computes and which nothing has yet written to.

*Numbering note:* the shared M9–M13 planning index calls this decision "D-16.3". That identifier
was already taken, above, by the worker-panic-isolation decision, and this document does not reuse
or renumber identifiers, so the logging decision is **D-16.4**. A citation to "D-16.3" from a
planning document written against that index means this decision.

*Why this is a decision at all, and not a detail:* the workspace today has **no logging dependency
of any kind** — not `log`, not `tracing`, nothing. §17's record of what M4, M5 and M6 each
deliberately did *not* add shows the bar this project applies (zero transitive dependencies, no
build script, `no_std`-capable, MSRV below this workspace's own), and shows that `libc` is the one
knowing exception to it, taken for an ABI-soundness reason. Adding a logging stack would be the
first dependency taken purely for developer convenience, so the alternatives are recorded rather
than one being assumed.

*Option A — adopt the `log` facade plus a hand-written sink.* `log` is a zero-dependency,
MIT OR Apache-2.0 facade; the sink would still be ours, so the writer work is unchanged. Its one
real benefit is that a dependency which already logs through `log` would have its diagnostics
captured in Namir's log for free. Its cost is that the facade is *global and reachable from
anywhere*: once `log` is in the tree, `namir-engine` can take it as a dependency and a `log!` call
can appear inside `process()`, which allocates and takes a lock. Nothing mechanical stops that.

*Option B (chosen) — hand-roll the writer in `namir-platform`, no dependency.* The writer is on the
order of a hundred lines: open-append, a level filter, a size check, a rename-and-reopen. The
decisive property is structural, not the line count: **D-5.1 already forbids `namir-engine` from
depending on `namir-platform` at all**, and `cargo run -p xtask -- layering` enforces that on every
merge. Putting the logger there makes it *unreachable from the audio thread by the same lint that
already runs*, rather than by a review convention. That is the strongest available enforcement of
NFR-RT-010 for this feature, and it is free.

*Consequence — the audio thread never touches the logger, and now cannot.* D-16.2's existing route
stands unchanged: the audio thread emits numeric fault codes through the telemetry ring, and the UI
or worker side formats, allocates and writes. Logging is a non-RT-side activity in this design, and
Option B makes that a compile-time fact on the engine crate rather than a rule someone must
remember.

*Consequence — what Option B gives up, stated rather than glossed.* A dependency that logs through
the `log` facade writes into a facade Namir has not installed, so its output is silently dropped
rather than appearing in the user's log. That is a real loss for diagnosing a fault inside `cpal`
or `clack`. If that loss ever bites, the fix is additive — install a `log` facade adapter in front
of the same writer — and this decision is revisited then, on evidence, rather than pre-emptively.
`log` is recorded in §17 as a prospective, not-adopted candidate so the option stays visible.

*Rejected — `tracing`:* it is the better library for the problem it solves, and that problem is not
this one. Structured spans across async task boundaries are worth their dependency tree in a
service; Namir is a single-process audio application whose FR-ERR-010 need is "a file a user can
attach to a bug report." Adopting `tracing` means adopting `tracing-core`, a subscriber stack and
its own transitive tree, all for a feature the chosen option delivers in one module — and it would
carry Option A's reachability problem as well.

*Consequence — FR-ERR-050's diagnostic bundle includes the current log and its retained
generations.* Combined with D-13.3's note that the bundle should record which CLAP paths exist and
what they contain, that makes the bundle self-sufficient for the two most likely support reports.

*Consequence (added M9's P0 decision pass, 2026-08-08) — this decision named a writer and left its
numbers blank.* "Bounded by size with a fixed small number of generations", a verbosity
"configurable from settings and overridable by an environment variable", and an implied record
format are shapes without values, and FR-ERR-010 cannot be implemented from a shape. **D-16.5 below
supplies the six missing values** — maximum file size, retained generations, the line format, the
variable's name, what each level admits, and the thread model — so that M9b's implementation work
starts from numbers rather than from an argument. Nothing this decision settled is reopened: the
writer is still hand-rolled, still sited in `namir-platform`, still takes no dependency, and `log`
stays in §17 as a prospective, not-adopted candidate.

**Decision D-16.5 (added M9's P0 decision pass, 2026-08-08; supplies D-16.4's unstated parameters)**
— FR-ERR-010's writer is specified as follows.

| Parameter | Value |
|---|---|
| Maximum file size before rotation | **4 MiB** — `LOG_MAX_BYTES: u64 = 4 * 1024 * 1024` |
| Retained generations | **2** — `namir.log`, `namir.log.1`, `namir.log.2`; a **12 MiB** total ceiling |
| Record format | one UTF-8 line: `<timestamp> <LEVEL> <pid> <thread> <code-id> <detail>` |
| Verbosity environment variable | **`NAMIR_LOG`** — `off` / `error` / `info` / `verbose` |
| Default level | **`info`** |
| Thread model | **synchronous**: one process-global writer behind a `Mutex`, no logger thread |

*Rationale — 4 MiB and two generations.* At the ~100-byte line this format produces, 4 MiB holds
roughly forty thousand records — several full sessions at a level whose records are per user action.
Two generations exist for the report Namir actually receives, which is "it broke, I restarted twice,
here is the log": the interesting file is the one from before the restarts, and a third generation
buys nothing the second does not. The 12 MiB total is chosen to keep the whole `logs` directory an
ordinary issue or email attachment, which is the only thing FR-ERR-050's bundle is for.

*The record format, exactly.* Fields one to five never contain a space, so `detail` is unambiguously
everything after the fifth space and the format needs no quoting scheme and no parser. `LEVEL` is
the record's `namir_core::Severity` rendered `INFO`/`WARN`/`ERROR`/`FAULT`, so the level and the
catalogue severity are one fact rather than two that can disagree. `<code-id>` is the `id` field of
the record's `ErrorCode` verbatim (`crates/namir-core/src/error.rs:34`) and is **mandatory** — every
record is catalogue-backed, which makes the log greppable by the identifier FR-ERR-020 already makes
documentable and testable, and it means lifecycle events get `Severity::Info` consts in
`namir-platform`'s own catalogue (`platform.log.session_started`, `platform.log.rotated`,
`platform.log.bad_level`) rather than a second, id-less record shape. `message_template` is never
written: D-16.2 puts template formatting on the UI side, and the log carries the id plus the
already-materialised detail. CR and LF inside `detail` are written as the two-character sequences
`\n` and `\r`, and whitespace in a thread name becomes `_`, so one record is one line
unconditionally — a panic payload with embedded newlines cannot break a `grep`. Timestamps are UTC
with milliseconds, `2026-08-09T14:03:57.412Z`.

```
2026-08-09T14:03:57.412Z ERROR 18244 namir-worker-0 worker.file.unreadable path=C:\Users\alice\Models\lead.nam; io=The system cannot find the file specified. (os error 2)
2026-08-09T14:04:02.006Z INFO 18244 main platform.log.rotated namir.log reached 4194304 bytes; namir.log.1 replaced
```

*Rationale — `NAMIR_LOG`, and what each level admits.* The name matches the only `NAMIR_*` variables
that exist (`NAMIR_PIN_CORE`, `NAMIR_DENORMAL_WARMUP_BLOCKS`/`_MEASURED_BLOCKS`) and is short enough
to dictate to a user in a support thread. Deliberately **not** `RUST_LOG`: that name belongs to
`env_logger`'s per-module filter grammar, and D-16.4 installs no facade, so borrowing it would
promise a syntax this writer does not implement.

| `NAMIR_LOG` | Admits |
|---|---|
| `off` | nothing; the file is never opened or created |
| `error` | `Severity::Error` and `Severity::Fault` |
| `info` *(default)* | the above, plus `Severity::Warning` and `Severity::Info` |
| `verbose` | the above, plus records submitted through `record_verbose`, a no-op at every other level |

The level is `NAMIR_LOG` if set and valid, else the persisted setting where one exists
(`namir-app`'s `AppSettings`), else `info`. An unparseable value falls back the same way and writes
one `WARN platform.log.bad_level` record naming the value — never silently off, the same
degrade-rather-than-assume posture `paths.rs` already applies per NFR-PORT-030. `error` is accepted
as a synonym for D-16.4's own prose spelling "errors", so an instruction copied from that decision
still works. The default is `info` rather than `error` because the only bug report most users will
ever file is the one they send before anyone asks them to change a setting: it has to already carry
the session's shape, not just the failure.

*Rationale — synchronous, under one mutex, with no thread of its own.* A logger thread costs a
permanent thread per process, which NFR-PORT-030 names explicitly ("no assumption that the process
can spawn unlimited threads", `01-functional-requirements.md:966`) and which is worse in the plugin
configuration than in the standalone: a thread parked inside a `.clap` the host may unload needs a
shutdown handshake the synchronous design needs not at all. The writer is a `OnceLock<Logger>`
holding an `AtomicU8` level and a `Mutex<SinkState>`, so a below-threshold record costs one relaxed
atomic load and returns without touching the lock. **A record from a `namir-worker` pool thread and
a record from `namir-app`'s UI thread interleave by taking that same mutex**: the lock is acquired
after the level check, held across formatting into the sink's own reused scratch `String` and
exactly one `write_all` of the complete line, then released. Records are therefore totally ordered
within a process and no line is ever torn. There is no `BufWriter`, deliberately: a half-flushed
buffer loses precisely the records written in the moments a crash makes interesting. §22's **R-12**
records the one interaction this leaves unmeasured.

*Consequence — the audio thread cannot reach any of this, and that is what makes the mutex safe.*
D-5.1's table gives `namir-engine` `core, params, dsp, nam, ir` and nothing else (§5's table above),
and `xtask layering` checks that edge on every merge, so no code on the audio thread can name this
module — D-16.4's decisive argument, restated here because a mutex in a logger is only acceptable
under it. What that lint does *not* cover is stated rather than assumed: `namir-app` and
`namir-clap` depend on everything and own the audio callbacks, so those two crates *could* call the
logger from `cpal`'s callback or from `process()`. Nothing mechanical stops them. The rule is that
no record is emitted from an audio callback or from a per-frame UI path; it is held by review plus
`crates/namir-worker/tests/rt_stress.rs`'s `assert_no_alloc` harness, which fails on the allocation
a record's formatting performs. That is an engineering discipline, not a guarantee — the same shape
of limitation D-16.3 records for "the audio thread does not panic".

*Consequence — how an engine-detected fault reaches the log instead.* By the route that already
exists, unchanged: the audio thread pushes a numeric fault code through the telemetry ring
(`crates/namir-engine/src/telemetry_ring.rs`), the UI side drains it
(`crates/namir-app/src/host.rs`'s `read_meters` at `:294`, `crates/namir-clap/src/ui_host.rs`), maps
it to an `ErrorCode` and pushes an FR-UI-070 notice — and `AppHost::push_notice`
(`crates/namir-app/src/host.rs:167`) / `SharedInner::push_notice`
(`crates/namir-clap/src/shared.rs:201`) are the log's call site, one line each, so a notice and a
log record cannot drift apart. Worker-side faults never touch telemetry: `namir-worker` may depend
on `namir-platform` and logs at the job boundary where `catch_unwind` already yields
`worker.job.panicked` (FR-ERR-040). `namir-ui` logs nothing at all — D-5.1 forbids it from depending
on `namir-platform`, which stays true and is the reason the shells own the call sites.

*Consequence — mobile, and the two cross-build gates.* The module is pure `std` (`fs`, `io::Write`,
`sync::{Mutex, OnceLock, atomic}`, `time::SystemTime`, `process::id`, `thread`) and adds **no**
`#[cfg(target_os)]`: every platform difference is already absorbed by `log_file_path()`
(`crates/namir-platform/src/paths.rs:68`), which returns `Option<PathBuf>` and yields `None` on
Android and iOS because `config_dir_from` (`:79`) has no branch for them. On `None` the writer
constructs a **no-op sink** — the level check still runs, every record is dropped, no file is
created and no error is raised — which is exactly the caller behaviour `paths.rs`'s own doc comment
specifies for that case. `mobile-cross-build-android` and `mobile-cross-build-ios`
(`.github/workflows/ci.yml:323`, `:367`) both build `-p namir-platform` on every push, and with no
new `cfg` and no new dependency there is nothing for either to trip over; `xtask layering`'s
`scan_platform_cfg` (`xtask/src/layering.rs:168`) is likewise unaffected. No `unsafe` either, so
this crate's `unsafe_code = "deny"` is satisfied without the module opting back in, unlike
`denormal.rs` and `thread_priority.rs` — and per D-5.3's own M9 note, no new designated module
appears anywhere.

*Consequence — no dependency, confirmed rather than asserted.* Nothing above needs a crate. The
timestamp is the standard days-from-civil arithmetic over `SystemTime`'s epoch offset, not a date
library. §17 gains no row for this decision and `log`'s prospective, not-adopted row stands exactly
as D-16.4 left it.

*Consequence — how FR-ERR-010 is verified.* Its `Verify:` code is **I**, and the test lands at
`crates/namir-platform/tests/logging.rs` — a new file, and this crate's **first** `tests/`
directory; it has none today. It drives a logger built against a caller-supplied temporary path
rather than the process-global one, the same "pure logic, wired to the real world only at the edge"
split `config_dir_from` already uses, and covers: level filtering per severity; rotation at the byte
cap with content preserved; the retention bound holding across many rotations (never a fourth file);
one intact line per record under eight concurrent threads; the `None`-path no-op sink; and the
`NAMIR_LOG` value parser. That parser must be a pure function over `Option<&OsStr>` for a hard
reason, not a stylistic one: `std::env::set_var` is `unsafe` in this edition and this crate denies
`unsafe` outside its two carve-out modules, so an env-mutating test cannot be written here at all. A
plain `// trace: FR-ERR-010` on the comment line immediately above that file's covering `#[test]`,
per D-23.1's adjacency rule, is what moves the row off **UNRESOLVED** in `docs/03-test-plan.md`
(`:32` today) — and per D-23.1 a plain tag asserts the whole requirement, so it is added only once
the six clauses above are all exercised. This is **M9b** work: the parameters are settled here so
that the phase which builds it has nothing left to decide.

*Honest limitation — two processes share one file.* The standalone application and a DAW hosting the
plugin write to the same `namir.log`; every plugin instance inside one DAW is covered by the
process-global mutex, but two processes are not. Records stay attributable because each carries its
pid, and two consequences are recorded rather than glossed. First, a rotation performed by one
process leaves the other appending to the renamed generation until its own size check fires, so a
few records land in `namir.log.1` rather than `namir.log`. Second, whether `fs::rename` succeeds
over a file another process holds open is **inferred, not measured**: Rust's `File` opens with
`FILE_SHARE_DELETE` on Windows, which is the flag that permits it, but nothing here has tested it.
The writer must therefore treat a failed rename as an ordinary outcome — keep the current handle,
retry the size check on the next record — never an `unwrap`. The 12 MiB ceiling can consequently be
exceeded transiently by a losing process; it cannot be exceeded indefinitely.

*Honest limitation — UTC only.* `std` carries no timezone database, so local time is unavailable
without the dependency D-16.4 declined. Timestamps are UTC and labelled `Z`; a mislabelled local
time would be worse than a correctly labelled foreign one.

*Honest limitation — the fourth level is not a severity.* `namir_core::Severity` has four values and
none of them means "trace"; adding one would change a type every crate's catalogue and the UI's
severity mapping share, for a distinction only the log makes. So `verbose` is expressed by a second
entry point rather than by a fifth severity, and it is the one place where the level ladder and the
severity ladder are not the same ladder.

*Left open rather than settled here:* `namir-clap` cannot see `namir-app`'s `AppSettings`, so in the
plugin configuration `NAMIR_LOG` is the only verbosity control 1.0 has. Whether the plugin ever
gains a persisted verbosity setting is a product question about plugin preferences, not a logging
one, and is registered as roadmap §15 item 8, due before M8.

---

## 17. Dependency register

All facts verified 2026-08-04 against crates.io and GitHub, except `assert_no_alloc` (added
2026-08-05, verified the same day). NFR-LIC-020 requires this to be mechanically re-checked in CI
(`cargo-deny`), not maintained by hand.

| Crate | Version | Licence | Verified activity | Role | Risk |
|---|---|---|---|---|---|
| `egui` | 0.35.0 | MIT OR Apache-2.0 | 2026-06-25, ~4.6 M recent downloads | UI | Low |
| `cpal` | 0.18.1 | Apache-2.0 | 2026-06-07, ~4.3 M | Standalone audio I/O. **Superseded at M11, 2026-08-11 — this crates.io release is no longer in the tree.** The fork row below replaces it, exactly as that row's own "replaces, rather than supplements" wording said it would once adopted. Kept, not deleted: it records the release line the fork still reports as its `version`, and it is the row this workspace returns to if D-13.4's change is accepted upstream | Low |
| `rubato` | 4.0.0 | MIT OR Apache-2.0 | 2026-07-09, ~3.0 M | Resampling | Low |
| `rustfft` | 6.4.1 | MIT OR Apache-2.0 | 2025-09-18, ~6.0 M | FFT for convolution | Low — stale but mature and stable |
| `hound` | 3.5.1 | Apache-2.0 | 2023-09-25, ~4.0 M | WAV decode (FR-IR-010) | Low — unmaintained, but WAV is a frozen format |
| `serde_json` | — | MIT OR Apache-2.0 | ubiquitous | `.nam` + state parsing | Low |
| `clack-plugin` | 0.1.1 | MIT OR Apache-2.0 | 2026-07-29, ~9.8 k | CLAP binding | **High — pre-1.0, low adoption.** See D-14.2 |
| `baseview` | 0.2.2 | MIT OR Apache-2.0 | published 2026-07-14; **version corrected at M12, 2026-08-10** | Plugin windowing | **High — low adoption; integration unverified.** See D-15.2. **This row read `0.3.0` (2026-08-02) until M12** — the latest *published* version, never the *pinned* one: `Cargo.lock` has 0.2.2 and both `crates/namir-ui/Cargo.toml:32` and `crates/namir-clap/Cargo.toml:60` pin `"0.2"`. Recorded rather than quietly overwritten, because M12's window-icon finding turns on exactly this: D-17.3 enumerates 0.2.2's `WindowOpenOptions`, the version that ships. **Checked at M13, 2026-08-11: no.** 0.3.0 has no icon field either — `WindowOpenOptions` does not exist there at all, having been renamed and reshaped to `WindowSettings` (`title`, `size`, `parent`, `wait_for_parent`, `fallback_scale_factor`, `opengl`-gated `gl_config`, still `#[non_exhaustive]`), and the only `icon` in its whole tarball is `hIcon: null_mut(), // Default icon` in the window class it registers — byte-identical to 0.2.2. **No published `baseview` has ever exposed an icon on any backend**, so `WM_SETICON` is confirmed as the only in-process route and D-17.3 declines it. The pin is also **forced rather than conservative**: the newest published `egui-baseview` is 0.6.0, the pinned one, and its manifest requires `baseview = "0.2.2"`. (The `BillyDM/egui-baseview` GitHub repository reads 0.7.0 but is stale, on egui 0.33; upstream moved to Codeberg.) |
| `symphonia` | 0.6.0 | **MPL-2.0** | 2026-05-15, ~3.3 M | *Candidate* for FR-IR-020 (AIFF/FLAC, a **Should**) | **Licence caveat — see below** |
| `assert_no_alloc` | 1.1.2 | BSD-1-Clause | 2021-08-03, ~1.6 M recent downloads | D-7.5's RT-allocation test harness in `namir-engine`. **Dev-dependency only — never linked into a release build.** | Low — stale (no release since 2021) but small, single-purpose, and off the shipped binary entirely |
| `rtrb` | 0.3.4 | MIT OR Apache-2.0 | published 2026-04-26; verified 2026-08-06 | D-7.2's SPSC command ring and D-8.1's return ring, in `namir-engine`. **A normal dependency — this one ships.** | Low — **zero** transitive dependencies, no build script, `no_std`-capable pure Rust, `rust-version = "1.38"` |
| `libc` | 0.2.189 | MIT OR Apache-2.0 | published 2026-07-21; verified 2026-08-07; ~317.6 M recent (90-day) downloads, maintained by the Rust language team | D-13.2's thread-priority elevation (`namir-platform/src/thread_priority.rs`), Linux/macOS `pthread_setschedparam` bindings only. **A normal dependency, `cfg`-scoped to `target_os = "linux"`/`"macos"` — ships on those two platforms, absent from the Windows and mobile dependency graphs entirely.** | Low — one of the most widely used and actively maintained crates in the ecosystem; carries a build script (see note below for why that did not block adoption here) but no C-compiler invocation from it (`cargo tree -e normal,build` shows no `cc`/`cc-rs` under it, unlike `blake3`) |
| `cpal` (Namir fork) | **Adopted at M11, 2026-08-11** (this row read "Prospective — not yet adopted" until then). `git+https://github.com/ErwanLegrand/cpal`, branch `wasapi-exclusive-mode`, pinned by `rev = "2edbacb44b10e56801e5dbfa251517fb2c9e2ef4"` in `crates/namir-app/Cargo.toml` — by commit, never by branch, per D-13.4 and R-10. Reports `version = "0.18.1"`, but its base is upstream **trunk** at `e0893c3` — the 0.18.1 release plus the unreleased commits after it — **not** the crates.io 0.18.1 release: forking the release would have started eight WASAPI fixes behind, one of them a device-lost panic fix | Apache-2.0, inherited from upstream | n/a — a Namir-maintained branch, not an upstream release; "activity" here is our own rebase cadence, which is the point of R-10. As pinned: **seven commits on `e0893c3`, 2867 insertions / 144 deletions across 11 files** (measured 2026-08-11 against the pinned revision) | D-13.4's WASAPI exclusive-mode support (FR-IO-020). Replaces, rather than supplements, the upstream `cpal` row above — which is marked superseded accordingly. Adds `cpal::platform::{ShareMode, WasapiStreamOptions, WasapiDeviceExt}`, re-exported with **no `cfg`**, which is what lets `namir-app` name those types without the platform attribute D-5.1 forbids it and `namir-platform` cannot supply on its behalf. **A normal dependency — this one ships**, in `namir-app` only; `namir-clap` does not take it | **High — unchanged by adoption.** A maintained fork is an ongoing cost and divergence from upstream is the risk, and the diff is far larger than D-13.4's own minimality instruction assumed, so R-10's rebase burden is higher than that decision predicted. Still the first and only non-registry source in the tree; its named `cargo-deny` `[sources]` allowance is `deny.toml`'s `allow-git`, and CI's `license-audit` job gained a `cargo deny check sources` step at M11 because until then no CI step ran that sub-check at all. See D-13.4's M11 notes and R-10's M11 status note |
| `log` | **Prospective — not adopted.** — (unpinned; version and activity **not** re-verified for this entry — verify before any adoption) | MIT OR Apache-2.0 | ubiquitous facade, Rust-language-team maintained | *Candidate* for FR-ERR-010's logging, evaluated as Option A of D-16.4 | **Deliberately not adopted — see D-16.4.** Zero-dependency and licence-clean; rejected on reachability (a global facade `namir-engine` could call from `process()`), not on quality. Listed so the option stays visible if a dependency's own diagnostics ever need capturing |
| `clack-host` | 0.1.1 | MIT OR Apache-2.0 — **an inherited claim, not independently re-verified: read from `clack-extensions` 0.1.1's own vendored manifest, same repository and release train as `clack-plugin`. This machine's registry has no vendored `clack-host` to read a licence file out of. Confirm against crates.io before merging.** | Same 0.1.1 line as `clack-plugin` (2026-07-29). `clack-extensions` 0.1.1 already carries it both as an optional dependency (its `Cargo.toml:118-121`) and as its own dev-dependency (`:140-143`, `features = ["clack-plugin"]`, `default-features = false`) | **Adopted (added M9's P0 decision pass, 2026-08-08).** D-18.6's in-process CLAP host harness for `namir-clap`'s FR-CLAP-030/-040/-070/-080/-100/-130 tests and NFR-PERF-040's instantiation benchmark. **Dev-dependency only — never linked into a release build**, on the same terms as `assert_no_alloc` | **Medium — pre-1.0, the same churn R-2 retired against for `clack-plugin`, pinned to exactly the version `clack-plugin` is pinned to.** Off the shipped binary, so it carries no NFR-LIC-030 attribution weight — but that is asserted, not yet measured. **Three gates before this row is anything but a plan, all of them M9a's:** `cargo deny check bans` and `cargo deny check licenses` green with the dev-dependency present; D-18.2's `network-free` job green; and `cargo tree -e normal` showing that enabling `clack-extensions`' `clack-host` feature for the test target does not reach the cdylib's graph, with `cargo run -p xtask -- attribution` unchanged. If any of the three fails, this row reverts to prospective and D-18.6 needs another vehicle. **Adjudicated at M9a, 2026-08-08: all three cleared and this row does not revert — but the third is cleared only for the configuration that landed, `clack-extensions`' own `clack-host` feature being left off precisely because it fails that gate's attribution half (§22's R-15). The licence caveat above is discharged in the same pass, from `clack-host` 0.1.1's own crates.io-published manifest rather than an inherited reading. See D-18.6's dated `*Consequence (added M9a, 2026-08-08, from landing it)*` note for the gate-by-gate evidence.** |
| `png` | 0.17.16 | MIT OR Apache-2.0 | published 2024-12-20; verified 2026-08-10; ~57.5 M recent (90-day) downloads. **0.18.1 (2026-02-14) is current, and 0.17 -> 0.18 is a breaking boundary under 0.x semver; the pin is where M12 built and tested, not a considered rejection of 0.18 — bumping it is a low-stakes future change.** | M12's `xtask identity` brand-mark generator: decodes `images/namir.png` to the alpha mask `namir-ui` ships as a blob, so that no image decoder enters either product. **An `xtask` dependency only.** Checked at adoption: `cargo tree -e normal -p namir-app -p namir-clap` contains no `png` node, and `xtask attribution` reports THIRD-PARTY-NOTICES.md unchanged; and `cargo deny check` is green on advisories, bans, licenses and sources with it in the lockfile — the first of the three gates §17 asked of `clack-host`, recorded here for the same reason. **The adoption bar is deliberately not applied to this row**: `png` brings six transitive packages (`adler2`, `crc32fast`, `fdeflate`, `flate2`, `miniz_oxide`, `simd-adler32`) and the bar would reject it outright. It is admitted on reachability instead — nothing here enters a shipped binary, so the bar D-17.3 declines to bend is not the bar in question. | Low — `image-rs`-maintained and ubiquitous, and outside the shipped graph entirely, so its only exposure is a developer's own build of `xtask`. It has a row rather than being excluded as build tooling because it is a **cargo** dependency in the workspace lockfile and therefore inside `cargo deny check`'s reach — the same reason `clack-host` has one. The note below excludes Inno Setup, `cargo-deny` and `clap-validator`, none of which cargo resolves. |

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
carrying the unsafe rather than a designated in-tree crate). It runs with its `warn_debug` and
`warn_release` features (count violations rather than aborting the process, in debug and release
profiles respectively — each is gated on its own `debug_assertions` state, so both are needed for
the harness to compile under `cargo test` and `cargo test --release` alike), so the harness's own
test can turn a violation into an ordinary `#[should_panic]`.

**Note on `rtrb` (added M4):** the same isolation argument as `assert_no_alloc` above, with one
difference a reviewer should not have to discover for themselves — **`rtrb` is a normal dependency
and does ship**, where `assert_no_alloc` is dev-only. D-7.2 requires a wait-free SPSC ring and
D-8.1's return ring must carry an `Arc<Prepared*>`, i.e. a non-`Copy` value with a destructor. A
queue of those with wait-free concurrent access cannot be written in safe Rust — the slot storage
needs `UnsafeCell` and every read out of it is an `unsafe` move — and `namir-engine`'s
`forbid(unsafe_code)` cannot be locally relaxed. So the unsafe lives in the dependency, exactly as
D-5.3's isolation intent wants, just in a crate that is linked rather than one that is not.

Checked against this project's own constraints before adoption rather than assumed: MIT OR
Apache-2.0 (already on `deny.toml`'s allow-list, so that file needed no edit and `cargo deny check
licenses` stayed green), **zero** transitive dependencies, no build script (so NFR-PORT-040's
no-C++-toolchain build is unaffected), `no_std`-capable pure Rust (so NFR-PORT-030's
`aarch64-linux-android`/`aarch64-apple-ios` cross-builds are unaffected), and an MSRV far below
this workspace's own. One property is load-bearing rather than incidental: `PushError::Full(T)`
hands the rejected value *back*, which is what lets D-8.1 step 4's "the audio thread ... never
drops it" be expressed at all.

**What M4 deliberately did *not* add**, recorded because an empty addition is itself a decision:
no thread-pool crate and no channel crate (D-7.1's pool is at most two threads and is about a
hundred lines of `std::sync`; and the pool must be able to *inspect* its queue, which a `Receiver`
does not allow), no async runtime (D-7.1 rejects that explicitly), and no counting-allocator crate
(which would need an `unsafe impl` D-5.3 forbids here). `namir-worker` therefore adds **no**
third-party dependency of its own.

**What M5 deliberately did not add**, following the same convention: no embedded key-value store or
database crate for `namir-library`'s index (AQ-3/D-12.3 — a single JSON document reusing
`serde_json`, already in the tree via `namir-nam`, serves a 10 000-record rebuildable cache better
than a B-tree store built for random access this workload never performs); no search-index crate
(`fst`, `tantivy`) for FR-LIB-040 — a linear scan over a precomputed lowercase blob is sub-millisecond
at this scale and adds nothing a real search library would improve on; no directory-walking crate
(`walkdir`) — `std::fs::read_dir` plus `DirEntry::file_type()` (which does not follow symlinks, so
loops are impossible by construction) is sufficient and keeps the caller-pumped step machine's
control flow local. `namir-library` therefore adds **no** third-party dependency beyond
`serde`/`serde_json`, both already present in the workspace. `namir-state` adds exactly **one**:
`base64` (FR-STATE-080's embedded-data encoding, D-11.1's own note), checked against this
workspace's adoption bar before taking it (MIT OR Apache-2.0, zero transitive dependencies, no
build script, `no_std`-capable, MSRV far below this workspace's own) and built with
`default-features = false, features = ["alloc"]` specifically to exclude the default
`simd-unsafe` feature, so the dependency carries no unsafe SIMD code path at all rather than merely
one this crate never calls into.

**Note on `libc` (added M6):** the one new dependency `namir-platform` takes to reach D-13.2/D-13.3's
full scope, and the first case in this project where the usual adoption bar (`rtrb`'s criteria,
restated at D-12.3: "zero transitive dependencies, no build script, `no_std`-capable pure Rust")
is knowingly **not** met, rather than met or the dependency rejected. `libc` does carry a build
script — exactly what counted against `libc` itself when D-12.3 rejected `redb` for depending on
it. The difference this time is what the dependency is *for*: `thread_priority.rs`'s
`pthread_setschedparam` call needs a `libc::sched_param` whose memory layout matches each target's
real C ABI exactly, and Darwin's definition carries a private padding field this crate cannot see
or reproduce correctly by hand — passing a mismatched struct layout across that FFI boundary by
pointer is a genuine out-of-bounds read, not a style nit, which is exactly the class of risk D-5.3's
"written safety argument" requirement exists to force a reviewer to reason through explicitly
rather than paper over. A vetted, ecosystem-standard binding removes that risk; hand-rolling it to
avoid one dependency would trade a real soundness risk for a cosmetic saving. (D-12.3's own
`redb` rejection was a different trade entirely — a build-script/cross-compilation risk taken on
for a 5 MB rebuildable cache that a dependency-free JSON document already served just as well; there
was no ABI-correctness question forcing the issue the way there is here.)

Checked before adoption despite the build-script exception: MIT OR Apache-2.0 (already on
`deny.toml`'s allow-list), maintained by the Rust language team itself with ~317.6 M 90-day
downloads (2026-08-07) — about as low-risk as a maintainer/adoption profile gets — and, concretely,
its build script does **not** itself need a C compiler (`cargo tree -p namir-platform --target
x86_64-unknown-linux-gnu -e normal,build` shows no `cc`/`cc-rs` node under `libc`, unlike `blake3`'s
NEON backend), so NFR-PORT-040's no-C++-toolchain build is unaffected. `Cargo.toml` scopes it to
`[target.'cfg(any(target_os = "linux", target_os = "macos"))'.dependencies]` — it is absent from
the dependency graph entirely on Windows and, confirmed with `cargo tree --target
aarch64-linux-android`/`aarch64-apple-ios`, on both mobile targets too, so NFR-PORT-030's
cross-builds carry no new risk from it either. Windows's equivalent three functions
(`GetCurrentThread`/`SetThreadPriority`/`GetLastError`) are hand-rolled `extern` declarations
instead of a `windows`/`windows-sys` dependency, precisely because none of the three carries an
analogous struct-layout risk — the usual adoption bar applies unmodified there and is met more
cheaply by three `extern` declarations. See `namir-platform/Cargo.toml` and `thread_priority.rs`'s
own module doc comment for the full safety argument.

**Note on the two prospective rows (added M8-planning, 2026-08-08):** the `cpal` fork and `log`
rows are the first entries in this register that describe something **not in the tree**. They are
recorded here rather than left to their own decisions because both are dependency questions this
register is the place to answer, and because a reviewer reading the table should see the whole
intended dependency surface, including the parts deliberately left out. Every fact in both rows is
provisional until the dependency is actually taken: the fork does not exist yet, and `log`'s
version and activity are stated from general knowledge rather than checked on the date at the head
of this section. **Re-verify against crates.io and GitHub at adoption time, as every other row in
this table was**, and update the row in place with the verification date.

**Note on build tooling, and why it is absent from this register (added M8-planning):** D-18.3
names **Inno Setup** (Windows installer), **`pkgbuild`/`productbuild`** (macOS package
construction) and **`notarytool`/`stapler`** (Apple notarization). None of the three appears in
this register, deliberately. This register exists for NFR-LIC-020 and NFR-LIC-030: it tracks what
is **linked into the shipped binary** and therefore what Namir redistributes and must attribute.
Installer generators and Apple's own CLI tools run on a CI machine, produce an artifact, and put
nothing of their own into it — no code, no runtime, no licence obligation travelling with what the
user installs. Listing them here would blur the one distinction the register is for and would
imply a licence-audit obligation (`cargo deny check`) that cannot mechanically apply to a tool
that is not a cargo dependency. Their availability, versions and licences are D-18.3's and the CI
configuration's business, recorded there. The same reasoning already covers `clap-validator` and
`cargo-deny` themselves, which have likewise never appeared in this table.

**Decision D-17.3 (added M12, 2026-08-10)** — the adoption bar does not bend for presentation.
`namir-app` takes no build script; the Windows `.exe` icon is embedded by M13's packaging pipeline
instead. This resolves `03-implementation-roadmap.md` §15 item 7, which named three answers and
declined to pick one.

*Rationale:* the `libc` note above is the only knowing exception **in a shipped crate**, and what earns it
is that a mismatched `sched_param` layout across an FFI boundary is a genuine out-of-bounds read. An
icon is cosmetic and can borrow none of that reasoning; if the bar bends here it is not a bar. A
build script in a shipped crate is also a cross-compilation surface, and three CI jobs would have to
be proven inert against it — `no-cxx-toolchain`, `mobile-cross-build-android` and
`mobile-cross-build-ios` — for a feature none of those targets can display.

*Rejected:* `winresource`/`embed-resource` as a second knowing exception — the bar is worth more than
the icon. Also rejected: shipping with no executable icon at all, which leaves FR-UI-110 permanently
unmet rather than deferred to the milestone that is already building installers.

*Consequence:* a `cargo build` and a released binary differ in a user-visible way until M13 lands.
That is the cost this decision accepts, and it is small because M13 is what produces a distributable
artifact at all. Recorded here rather than in §18 because the question is whether the adoption bar
bends, not how CI is arranged.

*Consequence (added M12, 2026-08-10, from planning the work):* FR-UI-110's **window** icon clause is
blocked in M12 independently of this decision, so the two clauses defer together. `baseview` 0.2.2's
`WindowOpenOptions` is `#[non_exhaustive]` and carries exactly `title`, `size` and `scale` plus an
`opengl`-gated `gl_config` — there is no icon field of any kind, so `03-implementation-roadmap.md`
§19's instruction that the window icon "has to be set through baseview's own window options" is
mistaken. Setting one would mean `WM_SETICON` against the HWND: `#[cfg(windows)]` and `unsafe`,
admissible only in `namir-platform` under D-5.1/D-5.3, and only by adding a fourth
`#![allow(unsafe_code)]` file for a cosmetic feature. Whether embedding the executable icon also
gives the window one depends on how `baseview` registers its window class and is **not** asserted
here; M13 verifies that on real Windows rather than assuming it.

*Consequence (added M13, 2026-08-11, from building it) — the executable icon is built, and the
question the note above left open is answered: **no `baseview` version has ever had an icon API**.*

The executable half took the third of §15 item 7's three answers, as this decision chose:
`images/namir.ico` is a **generated** artifact, rendered from `images/namir.png` by
`xtask identity --write` and byte-compared by plain `xtask identity` — the same freshness gate M12's
brand-mark blob runs under, and for the same reason. A hand-committed `.ico` would have been the
only artwork in the tree with no stated derivation from its source, so an artwork change would leave
it silently stale beside a blob that could not go stale. Four sizes (16/32/48/256), uncompressed
32-bit BGRA, integer-only throughout so the artifact cannot depend on which machine ran `--write`; a
PNG-compressed 256 entry would have cut it from 285 KB to ~10 KB and made a byte-compared artifact
depend on a third-party deflate's heuristics, which is the one property these generated artifacts
must not have. `rcedit` embeds it post-build on the Windows leg (D-18.3), so no build script enters
any shipped crate and **this decision's stated cost is unchanged and now real**: a plain
`cargo build` produces an icon-less executable and a released binary does not.

*Two findings from doing it, both worth more than the feature.* The crop is the **leopard head**, not
the letterboxed wordmark, which at 16 px would give about four legible rows — and because that is a
claim about this artwork's layout rather than about artwork in general, the generator refuses source
art whose wordmark reaches into the cropped square, the same shape as the existing single-fill
refusal. And the **16×16 size reads as a smudge**; 32 is readable, 48 and 256 are good. A
contrast-rescale step was written to fix it, then measured properly and **deleted**: the first
comparison had been between a magnified render and an unmagnified one, and the 16×16 tile's peak
alpha is already 243/255, so the rescale gained 1.05× and changed nothing visible. A test pins that
number so the idea is not re-invented. The real fix is a simplified icon-specific asset, which is an
artwork decision and not a downsampler one.

*The window half cannot close through the pinned stack, and this is a finding rather than a
deferral.* The note above left "whether 0.3.0 adds an icon field" unchecked; M13 checked, against
the published source rather than a changelog. `WindowOpenOptions` **does not exist in 0.3.0** — it
was renamed and reshaped to `WindowSettings`, which is still `#[non_exhaustive]` and carries
`title`, `size`, `parent`, `wait_for_parent`, `fallback_scale_factor` and an `opengl`-gated
`gl_config`, and **no icon field**. A search of the whole 0.3.0 tarball for `icon` returns five
mouse-cursor names and one line: `hIcon: null_mut(), // Default icon`, in the window class baseview
registers itself, with no override seam — **byte-identical to 0.2.2**. So the constraint is not
"0.2.2 is behind"; no `baseview` has ever exposed an icon on any backend. The upgrade is unreachable
in any case: the newest *published* `egui-baseview` is 0.6.0, the pinned one, and its manifest
requires `baseview = "0.2.2"`. The pin is **forced, not merely conservative**. (A caution for anyone
re-checking: the `BillyDM/egui-baseview` GitHub repository reads 0.7.0 but is stale, on egui 0.33;
upstream moved to Codeberg.) `WM_SETICON` therefore is the only in-process route, at the fourth
`#![allow(unsafe_code)]` file this decision already priced and declined. The remaining hope is the
shell's own executable-icon fallback, which this decision said M13 would verify on real Windows
rather than assume — **it has not been verified**; `docs/manual-tests/fr-ui-110-brand-mark.md`
records the title bar, taskbar button and Alt-Tab as separate unexecuted steps, because they need
not agree.

*Traces:* FR-UI-110.

---

## 18. Build, CI and target matrix

**Decision D-18.1** — CI gates every merge on: build + test on Windows/Linux/macOS; cross-*build*
of the mobile-capable crates for `aarch64-linux-android` and `aarch64-apple-ios`; a build in a
container **with no C++ compiler present** (NFR-PORT-040's verification clause); `cargo-deny`
licence audit; the layering lint; the `params.lock` diff; the RT-allocation harness; the fuzz
targets; formatting and lints as errors.

**Decision D-18.2** — A **network-free build configuration is a permanent CI target**, per
FR-ERR-070.5, so that RD-1's future Tone3000 support can never quietly become mandatory.

*Consequence (added M7)* — Built at M7, as the roadmap's own M7 section predicted ("there's no
network feature yet for a feature flag to gate; building that infrastructure now, once, is cheaper
than guessing at its shape earlier"). Mechanism chosen: rather than a bespoke build-time scanner,
`deny.toml`'s existing `[bans]` section (already CI-gated for D-18.1's licence audit) now also
carries a `deny` list naming the well-known HTTP/TLS/DNS/async-networking crates most likely to
arrive as an unnoticed transitive dependency (`reqwest`, `hyper`, `tokio`, `rustls`, etc. — see that
file's own comment for the full list and rationale), enforced by a new `network-free` CI job running
`cargo deny check bans`. This is a named-crate denylist, not a semantic "detect any networking"
check — cargo-deny has no such classification — so it is necessarily incomplete against a
sufficiently obscure or renamed networking crate; it is deliberately scoped to catch the realistic
failure mode (something else's dependency tree quietly growing an HTTP client) rather than to be
adversarially unbeatable. Extend the list, don't replace the mechanism, if/when RD-1 adds a real
network client behind its own feature flag.

**Decision D-18.3 (added M8-planning, 2026-08-08)** — Release artifacts (FR-PKG-010 through
FR-PKG-050) are produced by a tag-triggered `release.yml` running on all three runners, in a fixed
order: **build → `xtask bundle` → per-OS package → GitHub Release**. `xtask bundle` is the
new primitive and everything else depends on it; per-OS packaging is **Inno Setup** on Windows, a
`.pkg` inside a `.dmg` on macOS, and a tarball plus `install.sh` on Linux.

*Rationale for `xtask bundle` existing at all:* nothing in the Rust ecosystem will build a macOS
`.clap` bundle. `cargo` produces a `.dylib`; the bundle directory D-13.3's M8-planning note
describes (`Contents/Info.plist`, `Contents/PkgInfo`, `Contents/MacOS/<dylib>`) has to be
assembled by something, and on Windows and Linux the same step is the cdylib rename. Putting it in
`xtask` — which already exists and is already exempt from D-5.1's layering table — means the same
command runs locally and in CI, so a developer can reproduce a release artifact without reading
the workflow file. `nih_plug_xtask` is the model: it solves exactly this problem for a different
plugin framework, and its shape (a bundler subcommand driven by a manifest of what to build) is
worth copying rather than rediscovering.

*Windows — Inno Setup, and specifically for `{autocf}`.* FR-PKG-030 requires both per-user and
system-wide scope, per-user by default, installing to "the CLAP directory corresponding to the
chosen scope, as recorded in `02-architecture.md`" — the FRS deliberately does not cite `D-x.y`
identifiers (its own §1.1), so **the binding it defers to is D-13.3's table, and this decision is
where that is stated**: per-user is D-13.3's per-user cell for the platform, system-wide is its
system-wide cell, on all three platforms, with no third location. Inno's `{autocf}` constant
resolves to `%COMMONPROGRAMFILES%` when the installer is running elevated and to
`%LOCALAPPDATA%\Programs\Common` when it is not — which is D-13.3's Windows row, both cells, from
one line in the `.iss`. Paired with `PrivilegesRequired=lowest`, the installer defaults to
non-elevated per-user and escalates only if the user asks, which is the behaviour D-13.3's
rationale argues for ("installation needs no administrator rights, which matters for users without
them"). Inno is preinstalled on GitHub's `windows-latest` image, so this adds no toolchain
provisioning step. A plain ZIP ships alongside it for FR-PKG-050.

*macOS — a `.pkg` inside a `.dmg`, not a `.dmg` alone.* Two reasons, both concrete. (a) A release
places multiple payloads at multiple absolute paths — the `.clap` bundle under
`~/Library/Audio/Plug-Ins/CLAP` or `/Library/...`, the standalone app under `/Applications`, the
attribution file with them — and only `pkgbuild`/`productbuild` can express that; a `.dmg` is a
mountable folder the user drags from, which handles one destination well and several badly.
(b) Files placed by `installer` from a `.pkg` do not carry `com.apple.quarantine`, whereas files a
user extracts from a downloaded zip do — and a quarantined plugin is exactly the case R-11
describes. `notarytool` and `stapler` complete the chain when signing is available. Surge's
`make_installer.sh` is the working reference for the whole sequence.

*Signing is conditional on a secret being present, not on a build flag.* Following Surge's pattern:
the workflow's signing steps are skipped when the signing identity secret is absent, and the
unsigned build takes the **identical** code path otherwise. This means notarization can be turned
on later by adding a secret, with no restructuring, and that the unsigned path is the one exercised
on every run rather than an untested fallback.

*Honest caveat, recorded because it determines who may use a macOS release:* an unsigned,
quarantined **plugin** does not fail the way an unsigned application does. An application gets
Gatekeeper's "Open Anyway" path; a plugin loaded by a DAW gets no user-visible override at all —
it simply fails to load — and macOS 15 removed the Control-click bypass that used to work. Until a
signing identity exists, macOS releases are **developer-only in practice**, and this should be
stated in the release notes rather than discovered by users. Recorded as **R-11** (§22).

*Linux — tarball plus `install.sh`, defaulting to `~/.clap`.* Per D-13.3's Linux row. Two facts the
script must not paper over: Fedora and other multilib distributions use `/usr/lib64/clap` rather
than `/usr/lib/clap` for the system-wide path, so the script detects rather than assumes; and
CLAP's own issue #46 — whether `~/.clap` or an XDG-conformant path is correct — is **still open**
upstream, so `~/.clap` is chosen because it is what the specification says today and hosts scan
today, with the awareness that it may need to become "both" later.

*Consequence — FR-PKG-030's default must be empirically verified before it ships, not assumed.*
D-13.3's own doc comment already warns that a plugin at an unscanned path fails silently, with no
diagnostic anywhere, and that this will be the most common support question. Defaulting to the
per-user path is therefore only safe if hosts actually scan it: **verify Reaper genuinely scans
`%LOCALAPPDATA%\Programs\Common\CLAP`** on a clean machine before the default ships. The precedent
demanding this is specific — Dexed ships its per-user install mode *commented out*, with a note
that DAW-side issues were never resolved. If verification fails, the default changes; the
requirement is per-user-by-default because it is better for users, not because it is known-good.

*Consequence — FR-PKG-040 is a packaging step, not a documentation step.* `THIRD-PARTY-NOTICES.md`
and the licence texts must be physically placed inside every artifact by the packaging job. M7
generated the attribution file but explicitly deferred bundling it; this is where that closes, and
it applies to the plain archives (FR-PKG-050) as much as to the installers.

*Rejected — `cargo-dist`:* the obvious candidate, and it cannot do the two things that matter.
It has no `lib-aliases` mechanism, so it **cannot rename a cdylib** — which is the entire Windows
and Linux CLAP artifact — and it cannot build a macOS bundle. Its MSI installs only `bin/`, so
even the Windows installer it does produce would ship the wrong thing to the wrong place. This is
a capability gap, not a configuration gap; there is no way to hold it right.

*Rejected — `cargo-wix`:* it has **no per-user install token**, so FR-PKG-030's per-user default —
the whole point of D-13.3's table — cannot be expressed at all, and its WiX v4+ support is
unreleased. Rejected on the requirement, not on the tool's quality.

*Rejected — hand-written NSIS or raw WiX:* both are capable enough, and both would mean being the
only CLAP project doing it that way. Every installer-shipping open-source CLAP project surveyed —
Surge, Dexed, Odin2, Cardinal — arrived at Inno Setup independently, which is the strongest
available evidence about which path has its edge cases already found. Choosing differently buys
nothing and forfeits that.

*Consequence (added M13, 2026-08-11, from building it) — two things this decision says about the
Windows installer are wrong, and each would have failed FR-PKG-030 on its own.* Both were found
writing `packaging/windows/namir.iss` against Inno's actual documented behaviour, not by running it.

1. **`PrivilegesRequired=lowest` alone yields one scope, not two.** The paragraph above says the
   installer "defaults to non-elevated per-user and escalates only if the user asks". Nothing in
   Inno lets the user ask. `PrivilegesRequired=lowest` means Setup never requests elevation and
   never offers the choice; the install-mode dialog appears only when
   `PrivilegesRequiredOverridesAllowed` is also set. Without it FR-PKG-030's "shall **offer both**"
   fails outright, while every other clause passes — the failure a reader of this decision would
   not have looked for. The `.iss` sets `PrivilegesRequiredOverridesAllowed=dialog commandline`;
   `commandline` additionally gives the manual test `/ALLUSERS` and `/CURRENTUSER`.
2. **`{autocf}` when elevated is the *32-bit* Common Files directory**, `%CommonProgramFiles(x86)%`
   — `C:\Program Files (x86)\Common Files` — and **not** `%COMMONPROGRAMFILES%`, which is D-13.3's
   Windows system-wide cell and the only one CLAP's `entry.h` lists. It resolves to the 64-bit
   directory only in 64-bit install mode, which needs `ArchitecturesInstallIn64BitMode=x64compatible`.
   So this decision's "which is D-13.3's Windows row, both cells, from one line in the `.iss`" is
   **false as written**: it is both cells from one line *plus* that directive. The failure it would
   have produced is precisely the one D-13.3's rationale exists to warn about — the installer
   succeeds, the plugin lands where nothing scans, and no host, log or error message says so. This
   is the single most consequential line in that file, and it is the reason the `.iss` requires
   **Inno 6.3 or later**: an older Inno rejects the `x64compatible` identifier at compile time,
   which is the right failure to have.

*Consequence (added M13, 2026-08-11) — `rcedit` joins the Windows leg as a build input, and the
signed macOS path is still not reachable from CI.* FR-UI-110's executable icon is embedded
post-build by `rcedit` (D-17.3 having declined a build script in a shipped crate), installed on the
runner with `choco install rcedit --version=2.0.0` and run **before** `xtask bundle` so the staging
tree, the installer payload and the ZIP all carry the same binary and `bundle --check`'s assertions
still hold over it. No §17 register row: like Inno Setup and `notarytool` it runs on a CI machine
and puts nothing of its own into the artifact. Two costs stated rather than absorbed — it is pinned
**by version, not by hash**, one level weaker than the commit pin `cpal` and `clap-validator` carry,
and it is a source `cargo deny check sources` cannot see (R-10's class). Separately, and more
seriously: this decision's signing-conditional structure is built and correct, but **an identity
string is not an identity on a CI runner.** `codesign` needs the certificate imported into a
keychain — a `.p12` secret plus `security create-keychain`/`import` — and no secret named here
provides that. The signed macOS path therefore remains unreachable from CI even once the identity
secrets exist, which is unbuilt work rather than an oversight, and R-11 stands undiminished.

*Consequence (added M13, 2026-08-11) — the publish job's shape, because a requirement is now
asserted against it.* `release.yml` publishes from **one** job that `needs` all three build jobs,
checks nothing out, and takes `actions/download-artifact` as its only input. That shape is not
tidiness: FR-PKG-010's third clause — "every published distribution is an artifact of that workflow
rather than of a local build" — is checkable only if the publishing step provably cannot obtain a
file from anywhere else. `xtask/src/release_workflow.rs` asserts exactly that, along with the
per-job ordering `cargo build --release` < `xtask bundle` < the platform's packaging entry point <
upload. It is also what makes a single GitHub Release across three runners possible at all.

**Decision D-18.4 (added M8-planning, 2026-08-08)** — Namir publishes **nothing** to crates.io.
`publish = false` is set workspace-wide. Path dependencies nonetheless gain a `version` field, as
hygiene.

*Rationale:* Namir is one product, in the shape Zed and uv are — a workspace whose crates are the
internal seams of a single application — not a library ecosystem. Twelve of the workspace's
fourteen crates (excluding `xtask`) are implementation details of that product, with no meaning
outside it; `namir-clap` is a cdylib, which nothing can depend on as a library even in principle;
and `namir-fixtures` is test tooling whose whole purpose is generating this project's own fixtures.
There is no consumer for any of it. Publishing would create fourteen public maintenance
obligations to serve zero users.

*Rationale — name reservation is no longer an argument either.* The historical reason to publish
an unused crate was to hold the name. RFC 3463 now prohibits placeholder and name-reservation
crates outright, and RFC 3646 removed crates.io team mediation for name disputes, so publishing
empty shells to reserve `namir-*` is both against policy and no longer a reliable protection. The
option this decision forgoes is smaller than it looks.

*Consequence — `cargo publish` already fails today, so this changes the failure from accidental to
intentional.* Every path dependency in this workspace lacks a `version` field, which `cargo
publish` rejects. Setting `publish = false` replaces an incidental blocker with a stated policy,
and the difference matters: the incidental blocker would silently disappear the moment someone
added versions for an unrelated reason.

*Consequence — the `version` fields go in anyway.* Adding `version = "0.1.0"` alongside each
`path = "..."` is hygiene worth having regardless: it documents the intended compatibility
relationship between crates, it keeps `cargo` able to reason about the workspace the same way it
would about a published one, and it means reversing this decision later is a one-line change per
manifest rather than an audit. Keeping the reversal cheap is deliberate — this is a policy
decision, and policy decisions should stay revisitable.

*Accepted costs, stated:* no docs.rs-hosted API documentation (`cargo doc` locally, or a CI-built
static site, is the substitute), and no `cargo install namir` (D-18.3's installers and archives are
the distribution channel instead — which is the right channel for an audio application with a GUI
and a plugin artifact anyway).

*Consequence (added M13, 2026-08-11) — applied, and the mechanism is inheritance rather than
fourteen copies.* `publish = false` sits in the root `Cargo.toml`'s `[workspace.package]` and each
of the fourteen crates takes it with `publish.workspace = true`, so the policy has one home and
reversing it is one key rather than an audit — which is what "keeping the reversal cheap is
deliberate" above asks for. `xtask` keeps its own literal `publish = false`, which predates this
decision and says the same thing; it is left as it is rather than churned for uniformity. All
**59** path dependencies across the workspace gained `version = "0.1.0"`, including
dev-dependencies on `namir-fixtures`, which `cargo publish` does not require but which this
decision's hygiene argument covers equally.

*Consequence (added M13, 2026-08-11) — the claimed change in failure mode is real, and was
checked rather than assumed.* `cargo publish -p namir-core --dry-run` now stops at
``error: `namir-core` cannot be published. `package.publish` must be set to `true` or a non-empty
list in Cargo.toml to publish.`` — the policy, refusing before any packaging work happens. Before
this change it would have failed further in, on a path dependency carrying no `version`. That is
exactly the substitution this decision predicted: an incidental blocker replaced by a stated one.
`Cargo.lock` is unchanged by the whole edit, which is the other thing worth recording — adding a
`version` beside a `path` alters nothing about resolution inside a workspace.

**Decision D-18.5 (added M9's P0 decision pass, 2026-08-08)** — NFR-QUAL-010's traceability check is
gated in **two halves with different flip dates**. The plan-diff half — `docs/03-test-plan.md`
matches what `xtask traceability` generates — is a **required** check from **M9a** onward. The
uncovered-Musts half stays informational (`continue-on-error: true`) and becomes required at **M13's
close-out**. Both halves print the full uncovered list, and each id is printed alongside the
milestone the roadmap makes responsible for it; **the exit status never depends on that
attribution**.

*Rationale:* the tool returns one value for two independent properties (`plan_up_to_date &&
coverage_clean`, `xtask/src/main.rs:304`) and CI's single invocation carries a single
`continue-on-error: true` (`.github/workflows/ci.yml:108-120`), so today a coverage annotation can
be deleted from a currently-covered Must and CI stays green — the regression half of NFR-QUAL-010 is
enforced nowhere, the pre-commit hook included. The two properties have different readiness dates:
the plan diff is enforceable now, while zero-uncovered cannot be reached inside M9 because nine of
the twenty-four Musts the generated plan reports uncovered are owned by M10, M12 and M13 as the
roadmap stands — ten once this same pass moves NFR-PERF-030 to M13. Splitting them buys the
regression gate four milestones early at no cost to M7's original argument against a required check
nobody can act on.

*Mechanism — written as a specification of M9a's tool work, because none of it exists today.* `xtask
traceability` grows `--allow-uncovered`, which prints exactly what it prints today but derives its
exit status from the plan diff alone. The flag is genuinely absent right now, not merely unused:
`run_traceability` returns the single expression `plan_up_to_date && coverage_clean`
(`xtask/src/main.rs:304`) and the argument parser recognises only `--write` (`:328-329`), so an
unknown flag passed today is silently ignored and the plain, exit-1-on-any-gap form runs. CI then
runs the required step with that flag and keeps a second, `continue-on-error` step running the plain
form, so the coverage half stays a visible annotation rather than a line in a log nobody opens. The
printed list keeps its present shape — one line per uncovered id (`main.rs:299`) — with the owning
milestone appended to each. **Where that id→milestone mapping comes from is deliberately constrained
rather than specified here: it is left to M9a's implementation, and it must not be a hard-coded
table of ids inside `xtask`, which would be the allowlist rejected below wearing a different name.**
The flip at M13's close-out is the deletion of the flag and of the second step — two lines,
deliberately.

*Mechanism — what must land in the same commit as the CI change, stated because getting this wrong
makes the new gate red on arrival.* Making the plan-diff half required is only safe alongside every
change that moves the generated plan: `--allow-uncovered` and the printed attribution; D-23.1's
`trace-partial:`/`uncovered:` parsing, its **PARTIAL** rendering and its adjacency and fn-name
rules; D-23.2's per-FRS-section Must counts; every annotation this pass adds or re-lays; and the
regenerated `docs/03-test-plan.md` itself, which no one may hand-edit. Landing the CI edit ahead of
the flag makes a required step invoke an argument the tool ignores; landing an annotation ahead of
the regenerated plan fails the very diff the step now enforces. This pass therefore lands as **two
commits**: the documents, which change no gate and no coverage; then the tool, the annotations, the
regenerated plan and the CI edit **together**.

*Consequence — the owning-milestone attribution is printed text and nothing else.* No code path
reads it, so it can never quietly turn a red check green, and it is not an exemption mechanism by
another name. What it is for is the reader: an uncovered id with no owner named beside it is a gap
nobody has claimed, which is the state this pass found §14 in and the state the printed line makes
visible on every run.

*Rejected — an allowlist or exemption register of known-uncovered ids*, which would let the whole
check be marked required today, **in every form it can take**: a declared deferral table in `xtask`
mapping id → closing milestone; a checked-in uncovered *count* permitted only to decrease; and a
`--strict` mode, which is the same list read from the other end. The checked-in generated plan is
already this project's ratchet — hand-editing forbidden by its own header, diffed on every run, and
any change to the uncovered set landing as a legible line in review — so an exemption list would
duplicate it, invert the default from "uncovered until covered" to "exempt if listed", and need its
own freshness gate to stop it rotting. There is a nearer argument than any of those: a
hand-maintained register of what is *allowed* to be missing is the exact artifact this pass exists
to stop trusting. §14's snapshot table is that shape, and §22's **R-14** records what became of it.

*Consequence — what this does not do.* NFR-QUAL-010's own *Verify* text asks for a check that "fails
on any uncovered **Must**" (`01-functional-requirements.md:993-994`). That is met when the second
half flips at M13's close-out, **not** at M9a, and **M9 must not record NFR-QUAL-010 as closed in
either phase**. The ratchet is also review-visible rather than mechanically monotone: regenerating
the plan with `--write` and committing a new `**UNRESOLVED**` row passes the required half. That is
the same enforcement model NFR-QUAL-020 already runs on, and it is stated here rather than left to
be discovered.

*Consequence — M13 inherits an obligation this decision creates.* §20's own deliverable text says a
packaging milestone shipping without its tags "turns CI red rather than merely leaving a hole"
(`03-implementation-roadmap.md:2166-2167`). Under this decision it does not: FR-PKG-010 through -040
are already `**UNRESOLVED**` in the checked-in plan, so shipping packaging code without annotations
leaves the plan unchanged and the required half green. §20's acceptance already requires those
annotations explicitly; that sentence, not the gate, is what enforces them until the second half
flips — and the flip itself is M13's close-out work. Recorded as a dated scope note in §20 rather
than left in this document alone.

*Consequence (added M13, 2026-08-11) — **the flip moves from M13's close-out to M9b's**, because it
was never reachable at M13's and the arithmetic that shows it was already on the page.* This
decision's own rationale says the uncovered half "cannot be reached inside M9 because nine of the
twenty-four Musts the generated plan reports uncovered are owned by M10, M12 and M13 … ten once this
same pass moves NFR-PERF-030 to M13", and it concluded from that ten that M13's close-out is where
the remaining gaps run out. It read one side of the ledger. The **other fourteen** of the
twenty-four were M9's own, and `03-implementation-roadmap.md` §16's second deliverable splits M9
into M9a and M9b with those fourteen landing in **M9b** — which the same pass ordered **after** M13
(`M9a → M10 → M11 → M12 → M13 → M9b → M8`, §16). So at the moment M13 closes, the M9b-owned gaps are
by construction still open. Measured rather than inferred: the checked-in plan today carries **15**
`**UNRESOLVED**` Must rows, of which M13 owns five (FR-PKG-010, -020, -030, -040, NFR-PERF-030) and
**ten** belong to M9b — FR-CFG-020, FR-CLAP-030, -040, -070, -080, -100, -130, FR-ERR-010,
FR-NAM-060 and NFR-PERF-040. Deleting `--allow-uncovered` at M13's close would therefore make a
**required** check red on the day it became required, and stay red until M9b — precisely the
permanently-red required check M7's reasoning rejected and which this decision cites as its own
ground for splitting the gate in the first place. **M9b's close-out owns the flip**: deleting the
flag from the required step and deleting the informational step, still two lines, still deliberate.
Nothing else here changes — the required plan-diff half is untouched, both halves still print the
full uncovered list, and the rejected allowlist stays rejected. Two consequences travel with the
move. NFR-QUAL-010 is closed by **M9b**, not M13, and neither milestone before it may record it
closed; the note above is amended only in which milestone it names. And §15 item 15's deadline —
"due before M13's close-out … that is when D-18.5's zero-uncovered half becomes required" — follows
the flip to M9b, though M13 fixes it anyway for its own reasons (§20's M13 status). §16's restated
acceptance and §20's scope note carry the same correction, each as an appended note rather than an
edit to the text that got it wrong.

**Decision D-18.6 (added M9's P0 decision pass, 2026-08-08)** — A Must requirement whose `Verify`
code is anything other than **M** is traced **only** by an annotated artifact in this repository.
Where such a requirement has a residue that only a real host, real hardware or a human at a screen
can observe, its evidence is **split**: an annotated in-process test covering the part that can be
automated — which is what `xtask traceability` counts — plus a `docs/manual-tests/*.md` document
recording the residue and whether it was executed. For any code other than `M`, that document is
**supplementary evidence, never the traced artifact**. Neither the FRS's `Verify` codes nor `xtask
traceability`'s dispatch changes. For `namir-clap`, whose extension impls cannot be called at all
without a host, the in-process vehicle is **`clack-host` 0.1.1 as a dev-dependency, adopted by this
decision** (§17): `PluginEntry::load_from_clack::<SinglePluginEntry<NamirClapPlugin>>` instantiates
this crate's real plugin in-process, through the real C vtable, with no `dlopen` and no `unsafe`.

*Rationale — this names a pattern the project already follows; it does not introduce one.* Five Must
requirements carry both an annotated test and a manual-test document today, every one of them
`Verify: I`: FR-CLAP-060 (`crates/namir-clap/src/params_ext.rs`'s annotation plus
`fr-clap-060-host-bypass.md`), FR-CLAP-090 (`crates/namir-clap/src/shared.rs` plus
`fr-clap-090-multi-instance-memory.md`), FR-IO-060, FR-IO-070 and FR-IO-080. All five resolve.
FR-CLAP-030, FR-CLAP-040 and FR-CLAP-100 have the document and not the test, and that single
difference — nothing about the requirements, and nothing about the tool — is why they read as
uncovered. The fix is to write the missing half, which the roadmap's own M9 deliverable list already
asked for before this decision existed.

*Rationale — an in-crate host stub is not available, so the dependency is load-bearing rather than
convenient.* `clack_extensions::audio_ports::AudioPortInfoWriter::from_raw` is `pub(crate) unsafe
fn` (`clack-extensions` 0.1.1 `src/audio_ports/plugin.rs:19`), so `PluginAudioPortsImpl::get` cannot
be called from outside clack at all; the `HostInfo`/`Host*Handle` `from_raw` family is `unsafe`
throughout (`clack-plugin` 0.1.1 `src/host.rs:26,258,341,427` — `:26` is `HostInfo`'s and
additionally `const`, the other three are the three handle types'), so even `count()` would need a
fabricated `clap_host`. D-5.3 confines this crate's `unsafe` to `gui.rs`, and a `[lints.rust]` table
applies to test targets too — as that decision's own M9 note now records. `clack-host` is the route
that stays inside D-5.3 rather than the one that widens it.

*Rationale — the vehicle is verified to exist, not presumed.* `clack-extensions` 0.1.1 declares
`clack-host` 0.1.1 both as an optional dependency (`Cargo.toml:118-121`) and as its own
dev-dependency (`:140-143`), and its `src/__doc_utils.rs:114-146` `get_working_instance` does
exactly what is proposed here — `PluginEntry::load_from_clack::<SinglePluginEntry<…>>` against a
plugin defined in the same workspace, with **no `unsafe`** anywhere in the function.
`crates/namir-clap` already exposes what the harness needs without a visibility change: `pub struct
NamirClapPlugin` (`src/lib.rs:84`), `clack_export_entry!(SinglePluginEntry<NamirClapPlugin>)`
(`:125`), and `crate-type = ["cdylib", "lib"]` (`Cargo.toml:26`).

*Consequence — six requirements become countable, not six requirements become met.* The harness
serves FR-CLAP-030, -040, -070, -080, -100 and -130, all six currently `**UNRESOLVED**` in the
generated plan (`docs/03-test-plan.md:20`, `:21`, `:24`, `:25`, `:27`, `:28`). They are six of the
**seven** `namir-clap` Musts M9 owes: the seventh, FR-CLAP-020 (`:19`), needs no in-process vehicle
at all — it is traced by the `clap-validator` step M9a adds, for the reason the *this constrains the
artifact, not its kind* consequence below gives.

*Consequence — `clack-host` is a regression detector, not FR-CLAP-030's second host.* It is the
other half of the same library: `clack-host` and `clack-plugin` both sit on `clack-common`, so the
two agreeing rules out this crate's own bugs and rules out nothing they share. That is exactly the
weakness the roadmap already records against FR-NAM-030's LSTM parity — two Rust ports agreeing
rules out independent bugs, but both could share a misreading — and it transfers here verbatim.
FR-CLAP-030's *Verify* says "across at least two host implementations"; an in-process clack harness
is not one of the two, `clap-validator` is arguably the first, and a real DAW remains the only
unambiguous second. This decision makes that requirement countable and its declaration re-checked on
every merge. It does not make it met, and §14's re-audited **5.12 CLAP** row must count it
accordingly.

*Consequence — what must clear before the dependency lands.* §17's row states three gates, all
M9a's: `cargo deny check bans` and `cargo deny check licenses` green with the dev-dependency
present; D-18.2's `network-free` job green; and `cargo tree -e normal` showing that enabling
`clack-extensions`' `clack-host` feature for the test target does not reach the cdylib's graph, with
`xtask attribution` unchanged. Because the gates are M9a's, so is adding the dev-dependency they run
against — M9a owns the manifest edit, M9b owns the tests built on top of it. §22's retired **R-2**
gains a dated note recording that its pre-1.0-churn residual is narrowly reopened on a dev-only
surface, pinned to exactly `clack-plugin`'s version.

*Consequence — this constrains the artifact, not its kind.* Nothing here says an annotation must sit
on a Rust test. A `Verify: S` requirement whose own text is an assertion about this repository's
build or CI configuration is traced by that configuration — FR-CLAP-020 ("shall pass the reference
CLAP validator with no errors, **as a gate in CI**", `01-functional-requirements.md:712-714`) is the
live case, and the `clap-validator` step M9a adds carries its annotation directly. FRS §10's
`*Consequence (added M9, 2026-08-08)*` note holds the adequacy rule that governs when that is
legitimate; this decision does not narrow it.

*Rejected — amending the `Verify` code from `I` to `M` in the FRS.* FRS §1.5 freezes identifiers,
not `Verify` codes, so this is permitted rather than forbidden — and no code has ever been changed:
`git log -p` over `01-functional-requirements.md` shows every `*Verify:*` line as an addition from
the initial commit, M7's only FRS edits being §1.5's missing `Process` legend entry and §10's
Consequence note. It is nonetheless the wrong instrument twice over. First, **D-9.11** already
settled the principle for the neighbouring case: an apparent conflict between a requirement and what
the project can do is closed by showing the intent is met, not by editing the requirement down to
what was built. Second, `I → M` is a strict loss of evidence rather than a relabelling — a manual
script is re-run when a human chooses to, an annotated test is re-run on every merge, and the
genuinely automatable parts of all three requirements would stop being checked at the exact moment
M9 exists to start checking them.

*Rejected — teaching `xtask traceability` to accept a manual-test document as coverage for `Verify:
I`.* The blast radius is every one of the FRS's **28** `Verify: I` Musts, **21** of which have real
annotated source coverage today (`docs/03-test-plan.md`: 28 rows carry `I`, seven of them
`**UNRESOLVED**`); the tool would begin accepting a markdown file in place of a test for all 28, and
it cannot check whether a document's claim to have been executed is true — `build_report` matches on
filename prefix and literal-id containment, nothing more (`xtask/src/traceability.rs:179-181`). It
also fails on its own terms: applied today it would close **none** of the three requirements that
motivated it, because `fr-clap-040-latency-restart.md:68` and `fr-clap-100-gui-embedding.md:59` both
record **"Not executed"**, and `fr-clap-030-audio-ports-negotiation.md:51` records its real-host
half as not executed. A change that weakens the gate across 28 requirements and closes zero of the
three is not a trade.

*Consequence (added M9a, 2026-08-08, from landing it — the three gates adjudicated one at a time,
and the third is not a simple pass).* The *what must clear before the dependency lands* consequence
above, and §17's row it restates, name three gates. All three were run on this tree with
`clack-host` 0.1.1 present in `Cargo.lock` (`:311-319`): **one** new package, its own three
dependencies `clack-common`, `clack-plugin` and `clap-sys` all already resolved, because
`default-features = false` drops `libloading` and with it the macOS-only `objc2-foundation`.

**Gate 1 — `cargo deny check bans` and `cargo deny check licenses` green with the dev-dependency
present. PASSES, and not vacuously.** Both exit 0 (`bans ok`, `licenses ok`) and `deny.toml` needed
no edit, the licence already being on its allow-list (`deny.toml:29-30`). Vacuity is the thing worth
checking rather than assuming here, since a tool that skipped dev edges would pass this gate while
looking at nothing: it does not skip them — under `cargo deny -L debug check licenses` the inclusion
graph prints ``clack-host v0.1.1 └── (dev) namir-clap v0.1.0``, so the crate is in the evaluated
graph. **§17's licence caveat
is discharged on the same run, and by a stronger reading than the row asked for.** That row records
`MIT OR Apache-2.0` as "an inherited claim… read from `clack-extensions` 0.1.1's own vendored
manifest… Confirm against crates.io before merging". `clack-host` 0.1.1's *own* crates.io-published
manifest is now in this machine's registry and declares `license = "MIT OR Apache-2.0"` at its
`Cargo.toml:37`, with `LICENSE-MIT` and `LICENSE-APACHE` both present in the `.crate`. The claim is
no longer inherited.

**Gate 2 — D-18.2's `network-free` job green. PASSES, with its one real limit stated rather than
glossed.** That job runs `cargo deny check bans` and nothing else
(`.github/workflows/ci.yml:172-180`), which is Gate 1's second command, so the same green run
discharges it; no crate on `deny.toml`'s network deny-list enters `Cargo.lock`. The limit: the job
runs on `ubuntu-latest` and this run was on a Windows host, so the two see slightly different
platform-filtered crate sets. That difference cannot reach this crate — `clack-host`'s
only platform-scoped dependency is `objc2-foundation` under `cfg(target_os = "macos")`, and it is
`optional`, reachable only through the `libloading` feature this manifest turns off.

**Gate 3 — `cargo tree -e normal` showing the feature does not reach the cdylib's graph, with `xtask
attribution` unchanged. This is the half that needs interpreting.** Taken as two halves against the
configuration each is true of:

- *The tree half passes, more strongly than asked.* `cargo tree -e normal` over this workspace
  contains no `clack-host` node at all — the only `clack` nodes are `clack-extensions`,
  `clack-common` and `clack-plugin` — and `cargo tree -p namir-clap -e normal` likewise. The
  cdylib's normal graph does not reach it.
- *The attribution half passes as landed.* `cargo run -p xtask -- attribution` reports
  `THIRD-PARTY-NOTICES.md is up to date` and exits 0. The file gains no row.
- *But the configuration this gate is worded about is not the configuration that landed, and in the
  worded one the attribution half fails.* The gate says "enabling `clack-extensions`' `clack-host`
  feature for the test target". That feature is **not** enabled:
  `crates/namir-clap/Cargo.toml:90-92` takes `clack-host` directly with `default-features = false,
  features = ["clack-plugin"]`, and `:93-105` records the feature deliberately left off together
  with the measurement behind it. **The failing configuration is recorded as measured there, not
  re-measured for this note** — what this pass verified independently is the mechanism, which is
  readable without running it. `clack-extensions` 0.1.1 declares `clack-host` as an **optional
  normal** dependency (its `Cargo.toml:118-121`), so turning that feature on — even from a
  `[dev-dependencies]` entry here — makes `clack-extensions → clack-host` a `Normal`-kind edge in
  `cargo metadata`'s single unified resolve, and `xtask attribution` keeps any edge carrying a
  `Normal` `dep_kinds` entry (`xtask/src/cargo_meta.rs:83-87`), so THIRD-PARTY-NOTICES.md gains a
  `clack-host` row — the one thing §17's row says must not happen. The tree half still passes in
  that configuration, which is the tell: the cdylib genuinely does not link it, and what fails is
  `xtask attribution`'s fidelity rather than the dependency's confinement. §22's **R-15** is the
  standing record of that blind spot.

*What this adjudicates to.* The three gates are cleared **for the dependency as it landed**, so
§17's row's "if any of the three fails, this row reverts to prospective and D-18.6 needs another
vehicle" clause is **not** triggered — and the vehicle survives on its own terms, not by leniency:
`PluginEntry::load_from_clack::<SinglePluginEntry<NamirClapPlugin>>` lives in `clack-host` itself,
behind its own `clack-plugin` feature (`clack-host` 0.1.1 `Cargo.toml:49`), and needs nothing from
`clack-extensions`' host halves. They are **not** cleared for the wider configuration this
consequence's original text anticipated. **M9b inherits that as a named blocker rather than
discovering it**: a harness that wants the host-side halves of `audio-ports`/`params`/`state`/
`latency` must first make `xtask attribution` resolve dependency kind per shipped path rather than
per unified-resolve node, or observe those extensions another way.

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

*AQ-4 researched 2026-08-08 (see §21's open-questions table for the full note and sources): no
explicit licence was found for `input.wav`/`output.wav`; the file is distributed off-repo (personal
Google Drive links in `neural-amp-modeler`'s own docs) with no accompanying terms, and the upstream
project's own conduct (seeking "explicit written permission" to reference a third party's
alternative capture signal) points toward all-rights-reserved-by-default. Still blocks shipping
factory presets; does not block anything else.*

*Consequence (added M10, 2026-08-09) — committing a golden-reference render is generated, not
captured, and this decision permits it.* FR-NAM-030's `Verify: G` needs a comparison against the
actual reference implementation, which no in-tree Rust-vs-Rust test can be — closing it needed
committing something beyond a seed. `crates/namir-nam/tests/golden/` holds two small (~4.6 MB
total), *generated* fixtures (`namir_fixtures::nam::generate`/`generate_lstm` output, not a
captured model) plus each one's render through the real `NeuralAmpModelerCore` reference —
regenerable by any contributor with a local checkout, per the recipe
`crates/namir-nam/tests/golden_reference.rs`'s own doc comment gives in full. This is the "Parity —
FR-NAM-030" row of the table above, made concrete: this decision already named the reference
implementation's *comparison target*, it just did not yet say the comparison's *output* could be
committed alongside the generated input. It is a natural reading of the existing row, not a new
exception to it — the asset committed is the render of a generated fixture, never a capture, and
carries no licence surface for exactly that reason.

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
| **AQ-3** | ~~Choice of embedded index store for D-12.3, within the stated constraints.~~ **Resolved at M5, 2026-08-07: a single pretty-printed JSON document, written whole and replaced atomically. No new dependency.** See D-12.3. | — |
| **AQ-4** | Licence of NAM's standardised capture input signal, if any author capture is to be redistributed. Does not block the test phase (D-19.1), only the shipping of captures. **Researched 2026-08-08: no explicit licence found.** The standardised reamp/capture signal (`input.wav`, formerly `v3_0_0.wav`, and its predecessors `v2_0_0.wav`/`v1.wav`/`Proteus_Capture.wav`) is not bundled in `sdatkinson/neural-amp-modeler`'s MIT-licensed source tree (`nam/train/_names.py`'s `INPUT_BASENAMES`) or in the MIT-licensed `NeuralAmpModelerCore`; MIT covers "software and associated documentation files," i.e. code, not this asset. The project's own docs (`docs/source/tutorials/full.rst`, `colab.rst`) link `input.wav`/`output.wav` from Steven Atkinson's personal Google Drive with no accompanying licence, copyright notice, or redistribution terms anywhere the search reached (repo, docs, Colab notebook, neuralampmodeler.com). Corroborating evidence the project treats such files as conventionally (all-rights-reserved-by-default) copyrighted rather than freely reusable: a community-contributed alternative reamp signal ("Super Input," by François NEURALNET, discussed on the Fractal Audio forum) was referenced in NAM's docs only after being "used with explicit written permission" — the maintainer sought permission for someone else's capture-signal file rather than treating it as open. **Conclusion: treat as all-rights-reserved unless/until Atkinson clarifies.** Before shipping any factory-preset capture, either obtain explicit written permission to use/redistribute the standard signal (the precedent NAM's own maintainer used for "Super Input"), or record and use a self-recorded/self-licensed reamp signal for that capture session instead — sidestepping the question rather than resolving it upstream. | Before shipping factory presets |
| **AQ-5** | ~~Bass-amp DI tap point.~~ **Resolved 2026-08-04: DI is post-EQ; the limiter is switchable.** Two consequences for the capture session, recorded so they are not rediscovered afterwards: (a) the **limiter must be switched off** — it is time-variant and violates the constraint in D-19.1; (b) because the DI is post-EQ, the amp's EQ setting is baked into the capture, so the EQ must be set flat and its position recorded in the model metadata. | — |

---

## 22. Risks

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R-1 | ~~egui/baseview embedded-plugin-window integration does not exist in maintained form.~~ **RETIRED 2026-08-04.** S-3 parts 1 and 2 both PASS: egui renders standalone in baseview, and embedded in Reaper's own window via `open_parented` with a live frame counter. | Retired | — |
| R-2 | ~~`clack` is pre-1.0 with low adoption and may stall or break API.~~ **RETIRED 2026-08-04.** S-4 all parts PASS: clap-validator 15/15 with zero failures, loads and runs in Reaper, GUI extension works. Residual concern is pre-1.0 API churn, managed by exact version pinning and the `namir-clap` wrapper, not by redesign. **Narrowly reopened on a dev-only surface, M9's P0 decision pass, 2026-08-08.** D-18.6 adopts **`clack-host` 0.1.1** as a `namir-clap` **dev-dependency**, pinned to exactly the version `clack-plugin` is pinned to and drawn from the same release train, so the pre-1.0 churn this row retired against now applies to a second crate. The exposure is deliberately the smaller kind: `clack-host` is never linked into a release build, a break there breaks the test harness rather than the product, and the mitigation is unchanged — exact version pinning, both crates moved together. Recorded rather than absorbed silently, because "residual concern is pre-1.0 API churn, managed by exact version pinning" was written about one crate and now covers two. | Retired (churn managed) | Pin exact versions; wrapper confines the blast radius. |
| R-3 | ~~No redistributable `.nam` test corpus.~~ **Downgraded High → Low** by D-19.1: fixtures are generated from a seed, so there is no licence surface and no capture dependency in CI. Residual risk is only that the generator produces numerically degenerate models, which D-19.1 addresses directly. | Low | D-19.1; generator validated against an analytic target. |
| R-4 | ~~NAM inference in Rust misses the accuracy or performance bar.~~ **Downgraded High-relevant-question → Low-Medium by S-1, 2026-08-05.** Accuracy: PASS with wide margin (-131 dB vs. a 90 dB floor). Performance: the reference implementation misses the NFR-PERF-010 99.9th-percentile gate (41 % vs. 25 %) at median-comparable cost to Eigen-vectorized C++ — the residual risk is narrowly "does a SIMD pass close this gap," not "is Rust inference viable at all." **Vectorized and re-measured by M3, 2026-08-06 — measured, but not a confidently-distinguishable improvement on this sandbox, NOT retired.** `wavenet.rs`'s `axpy` now vectorizes every dilated/1×1-convolution AXPY-shaped inner loop with `wide::f32x8`. `namir-nam/benches/wavenet_inner_loops.rs`, measured on this M3 session's sandbox (4-core Intel Xeon @ 2.10 GHz, **not** this section's reference machine), re-measured a second time during this session's close-out pass via an interleaved scalar-vs-vector A/B under a load average confirmed quiet throughout (9-11 runs each): p50 essentially identical between scalar (mean 26.58%) and vectorized (mean 26.80%) — no reproducible win; p99.9 overlapping but vectorized modestly lower on average (scalar mean 49.53%, range 44.3–54.8%; vectorized mean 45.15%, range 43.7–48.6%) — see `wavenet.rs`'s own Decision-note for the full run-by-run numbers and why this reading supersedes an intermediate re-measurement that reported unreproducible 330–345% p99.9 spikes (most likely itself a sandbox-contention artifact, per the same phenomenon R-8's own re-verification documented). **Even at the more favourable ~45% p99.9 reading, this already exceeds the 25% budget on this sandbox in isolation**, and the real six-stage-chain benchmark (`namir-engine/benches/six_stage_chain.rs`, new this session) measured the assembled chain, gate+EQ active, at 61–76% p99.9 on the same sandbox — a clear FAIL. Whether vectorization closes a measurable part of the gap S-1 found is itself not confidently established on this non-AVX sandbox build. **RETIRED at M3's close-out, 2026-08-06 — this row's own "confirm on the reference machine before retiring" condition is now met, and the answer differs from everything above it.** The sandbox figures were measuring two confounds rather than the code. First, no `target-cpu` was set anywhere in the repository, so the workspace compiled to bare x86-64 (SSE2, no AVX, no FMA) and every `wide::f32x8` became two 4-lane SSE ops; setting `x86-64-v3` (now **D-2.3**) took the NAM stage from p99.9 30.3% to **~10.5%** on the §2 reference machine, with numeric parity re-verified under FMA at -130.8 dB. Second, every benchmark pinned to CPU 0, which absorbs the GPU driver's ISRs (see D-2.4). The assembled chain now measures p99.9 **16.45-17.08%** against the 25% budget on the §2 machine across five repetitions under D-2.4's conditions. Vectorization's benefit is directly measured rather than inferred, and NFR-PERF-010 closes. | Retired 2026-08-06 | Retired: D-2.3's AVX2/FMA baseline plus D-2.4's measurement conditions; NFR-PERF-010 certified on the §2 reference machine. |
| R-5 | FR-IO-070 device-removal handling is weak in any cross-platform audio library. | Medium | Test with a failable virtual device, not the happy path. |
| R-6 | `hound` unmaintained since 2023. | Low | WAV is frozen; we own any bug. Vendoring is a viable last resort. |
| R-7 | ~~Crossfade doubles NAM cost transiently, eating the NFR-PERF-010 budget.~~ **RETIRED at M4, 2026-08-06 — measured, and then mitigated and re-measured.** Measured first (`namir-engine/benches/handover_crossfade.rs`, §2 reference machine, D-2.4 conditions, six retained repetitions of ten): this risk's wording is half right with the wrong half named. A NAM handover alone stays inside the 25% budget at every swap rate tested (worst **24.31%**), including a duty faster than any human audition, and an IR handover alone likewise (worst **24.63%**). What exceeded the budget was **both stages crossfading at once**: 25.06–31.49%. Mitigated by a worker-side rule (`namir-worker`'s `Instance::serialise_against_other_target`): a NAM and an IR handover are never offered simultaneously, the second waiting out the first's crossfade on a worker thread, which D-7.1 permits workers to do. Re-measured with arms D and E **interleaved in the same runs** (six retained repetitions of nine, the only comparison form this machine supports reliably): unserialised **28.77–31.26%** against serialised **22.20–24.63%** at every rate where the rule applies, with the measured both-fades-active overlap going from 10.9–43.8% to **exactly 0%**. Steady state read 16.04–16.84% in the same runs. **Every condition the system can actually produce is now within budget.** | Retired 2026-08-06 | Retired: the over-budget condition is removed by construction, not by hoping users avoid it. **Two residuals recorded rather than glossed.** (a) The margin at the worst achievable condition is about **0.4 points** (24.63% against 25%), so this is the path any future per-stage cost increase will breach first — a reason to re-run this benchmark whenever NAM or IR per-block cost changes, and the reason it exists as a permanent target rather than a one-off. (b) The benchmark's arm E at `period 16` still reads 26.99–31.89% with 75% overlap, and that is **not** a failure of the mitigation: half a period is 8 blocks against a 15-block fade, so the bench's fixed-offset *simulation* of the rule cannot serialise there. The real rule does not offset, it waits — at least 25 ms, or ~19 blocks, which exceeds the fade — so the condition arm E period 16 depicts is one the worker cannot produce. **Re-run at M5's close, 2026-08-07, per this row's own "reason it exists as a permanent target rather than a one-off"** — preset recall (this milestone) is a new, more frequent way to reach the both-stages-changing condition than a human clicking two controls. Five repetitions, this session's own sandbox (**not** the §2 reference machine, which this session has no access to). Two of five were contaminated by the benchmark's own stated check — not just arm A's raw/estimator gap, which one run passed while still showing a 23-point gap on arm C, a stronger signal arm A alone cannot catch — and are discarded. Across the three clean runs, arm E at periods 32/64/128 (period 16 excluded per residual (b) above) read **22.16-24.35%**, matching the original 22.20-24.63% range closely; period 16 read 26.43-32.36%, matching the already-documented 26.99-31.89% benchmark-simulation artifact. **No evidence of regression; the risk remains retired.** M5 added no code to `namir-engine`'s crossfade/chain path this benchmark exercises (`Command::Unload` is the only M5 addition here, and this benchmark never issues one), so a regression was never mechanically plausible — this re-run is a check against the *system* (preset recall's new access pattern), not against a suspected code change. **Re-run again at M6's close, 2026-08-07** — the first M6 session to actually add code to the audio callback path this benchmark exercises (`DenormalGuard` acquisition, thread-priority elevation, both in `namir-app`'s `cpal` callback and `namir-clap`'s `process()`), so unlike M5's re-run this one *could* plausibly regress. Five repetitions on the §2 reference machine; raw p99.9 was contaminated by this session's own concurrent tooling load (confirmed, not assumed — even arm A, steady state with no handover activity at all to vary, swung 18.5-28.9% raw p99.9 across the five runs). The contamination-immune estimator, unaffected by that load, stayed tight and stable at **14.0-14.5%** across every gating period and every repetition, comfortably under budget and consistent with the certified quiet-machine range this risk originally retired against. **No evidence of regression; the risk remains retired.** |
| R-8 | **New, from S-2, 2026-08-05.** Same-size IR partitions all start accumulating input at stream time zero, so every partition at a given size — including every partition at `max_partition`, of which a multi-second IR can have dozens — triggers its FFT on the *same* block, forever. Measured directly: at a 32-sample block against a 2 s IR (48 kHz — FR-IR-050's own Must minimum, paired with the smallest Must block size), this alone costs 90–400 % of that block's entire period, tested across `max_partition` 256–32,768 with no material improvement at any value. Schedule tuning (D-9.6) cannot fix this; it is a gap in the synchronous, non-staggered scheme itself. **Verified and tuned by M3, 2026-08-06 — the scheduling defect itself is closed; the risk to NFR-PERF-010's acceptance is not.** M2's per-*group* stagger is replaced with a per-*size*, block-aligned stagger (`convolver.rs`'s own Decision/Rationale note). Re-measured on this M3 session's sandbox (4-core Intel Xeon @ 2.10 GHz, **not** this section's reference machine) via the ported `perf_sweep.rs`/`perf_bench.rs`: at this risk's own named condition (48 kHz, 32-sample block, 2 s IR), p99.9/max fell from 616.0%/1290.7% to **30.7%/70.4%** — comfortably under budget; at NFR-PERF-010's own literal condition (64-sample block), 337.7%/602.5% → **16.8%/41.3%**. Two gaps remain, not glossed over: 2048-sample blocks at 192 kHz/10 s IRs stay just over budget (117.8% p99.9, the head partition's own `O(block_size^2)` cost, not a staggering gap); a 32-sample-block/192 kHz `max` outlier is plausibly sandbox jitter, not confirmed. IR-stage-alone is no longer this risk's binding constraint. **RETIRED at M3's close-out, 2026-08-06.** `build_schedule`'s cross-size phase alignment is fixed with a permanent quantitative regression test (worst-block modelled FFT load 11.893x -> 6.793x the mean, against a 6.507x floor), and the residual tail this row was suspected of causing turned out not to be Namir's at all: an elevated `xperf` trace attributed it to `dxgkrnl.sys` issuing ~165 interrupts/second of 128-512 us, landing on CPU 0 exclusively — the core every benchmark here used to pin to. On a clean core the IR stage's p99 and p99.9 converge (51.6 / 55.0 us), which is the tight schedule-bounded distribution the cost model predicted throughout: the model was right, the measurement was contaminated. See D-2.4. | Retired 2026-08-06 | Retired: the scheduling defect is fixed with a permanent regression test; the residual tail was the GPU driver, addressed by D-2.4's core-selection rule. |
| R-9 | ~~A2's flat weight-layout order must be re-derived from `NeuralAmpModelerCore` (`NAM/wavenet/detail.h`, `NAM/wavenet/params.h`) the same way A1's was, and A1's was then proven correct to −131 dB by S-1's cross-implementation parity. A silently-wrong order is the dangerous outcome, not a loud one: the model loads without error, runs at the expected cost, and produces plausible-sounding output that is wrong — and no amount of listening will detect it, which is exactly the failure mode NFR-QUAL-030 exists to forbid. Grouped and bottleneck convolutions, per-layer variable kernel sizes and the 1×1 projections all multiply the number of ways the order can be subtly wrong relative to A1.~~ **RETIRED at M10, 2026-08-09.** Mitigated by process, not only by a passing test: two agents derived the weight order independently from the upstream C++ source, neither reading the other's code — `crates/namir-nam`'s own A2 implementation, and `crates/namir-fixtures`'s A2 generator plus `reference_infer_a2`. Their independently-derived orders agree exactly, and the in-tree Rust-vs-Rust parity test (`crates/namir-nam/tests/a2_fixtures.rs`) agrees to the bit, `-inf` dB — core A2's `LeakyReLU` has no rounding-sensitive transcendental function, so bit-exact agreement between two independent implementations is the strongest evidence available that neither has a subtle porting bug. Cross-checked a third way, against the actual reference this row named: a real `NeuralAmpModelerCore` render (pinned `3cde95c`, `-DNAM_USE_INLINE_GEMM -DNAM_ENABLE_A2_FAST=OFF`) measured A2 Full/A2 Lite at -90.31 dB each (`WaveNetShape::Standard`-scale synthetic fixtures) and, on smaller golden-reference fixtures committed for FR-NAM-030 (`crates/namir-nam/tests/golden_reference.rs`), -137 dB — comfortably inside FR-NAM-030's -90 dB floor either way, and nowhere near the near-zero-correlation signature a genuinely wrong weight order would produce. None of grouped convolution, per-layer variable kernel size interacting with groups, or the 1×1 projections this row worried about compounding turned out to need building: core A2's own shape requires `groups == 1` throughout, so that multiplicative risk never existed in the shipped scope. | Retired 2026-08-09 | Retired: independent double implementation plus a real-reference cross-check, both measured; see M10's roadmap close-out (`docs/03-implementation-roadmap.md` §17) for the full figures. |
| R-10 | **New, from M8-planning, 2026-08-08.** D-13.4's forked `cpal` is a maintained fork: it must be rebased as upstream moves, and every rebase is work that produces no user-visible benefit and can silently regress the Windows audio path. It is also the first non-registry dependency in the tree, which weakens the reproducibility NFR-SEC-040 (Should) wants and needs a named `cargo-deny` `[sources]` allowance that the default policy would otherwise reject. **Status note (added M11, 2026-08-11, from building it) — the rebase burden is higher than D-13.4 assumed, and the upstreaming exit is in better shape than this row's own wording implies.** *Burden.* The fork as pinned (`rev 2edbacb`) is seven commits on upstream trunk `e0893c3`: **2867 insertions / 144 deletions across 11 files**, not the few-dozen-line change the Mitigation column's "keep the fork's diff minimal" was written about. `src/platform/wasapi_ext.rs` (456 new lines) and `examples/wasapi_exclusive_probe.rs` (1443 new lines) are new files that can never conflict, but `src/host/wasapi/device.rs` changed 546 insertions against 130 deletions and **will** conflict. The cause is structural rather than carelessness: a per-call extension trait means threading the share mode through every enumeration, negotiation and stream-building path, and D-13.4's M11 note records why no smaller shape was available to this workspace. Severity stays **Medium** — the work is bounded and one file concentrates almost all of it — but budget a real rebase with a re-run of `docs/manual-tests/fr-io-020-wasapi-exclusive-mode.md`, not a mechanical replay; M11's own two defects were both invisible to every automated check, so a rebase that compiles and passes CI is not evidence that the Windows audio path still works. *Exit.* "Offer the change upstream; if accepted, the fork disappears" reads as a hope, and the evidence says it is better than that. Upstream wants this feature **and wants it in this shape**: issue **#1220** (v0.19 design goals, open) lists "Exclusive mode on CoreAudio, PipeWire & WASAPI (#106)" under working out extension traits, which is exactly what this fork implements. PR **#843** ("Added WASAPI exclusive host") is **not** evidence that upstream refused the feature, and should not be cited as such: its author closed it himself in November 2024 saying it needed updating and bug fixing, and **no maintainer ever reviewed it**. PR **#1195** (`AUDCLNT_STREAMOPTIONS_RAW`) is a different feature that establishes no reusable seam; it is still open, with two maintainers pushing back on its field-on-`StreamConfig` shape in favour of the same extension traits #1220 names. What the exit lacks is a date and an agreed API, not upstream willingness — so the mitigation to keep alive is offering the change, not preparing to carry the fork indefinitely. (Facts checked against the upstream repository on 2026-08-11; #1195 and #1220 were open on that date and may have moved since.) | Medium | Pin by commit hash, never by branch. Keep the fork's diff minimal — a share-mode parameter and its format-negotiation consequences, nothing else — so rebasing stays mechanical. Offer the change upstream; if accepted, the fork disappears and D-13.4 reverts to a version bump. Vendoring into the tree is the fallback if the pin proves insufficient for NFR-SEC-040. **Added M11, 2026-08-11:** the `[sources]` allowance this row names was, until M11, enforced by no CI step at all — `license-audit` ran `cargo deny check licenses` and `network-free` ran `check bans`, and `check sources` ran only when somebody typed the full `cargo deny check` locally. `license-audit` now runs it, so a *second*, unreviewed git source fails CI rather than passing unnoticed. |
| R-11 | **New, from M8-planning, 2026-08-08.** Release binaries are unsigned on both signing-relevant platforms, and the two platforms fail differently. Windows: SmartScreen warns on every release until reputation accrues, which it never does for a low-volume unsigned publisher, and Smart App Control blocks unsigned binaries outright rather than warning. macOS is worse in kind, not degree: a quarantined **plugin** loaded by a DAW has **no user-visible "Open Anyway" path at all** — it simply fails to load — and macOS 15 removed the Control-click bypass that previously worked. macOS releases are therefore developer-only in practice until a signing identity exists. | Medium | D-18.3's signing-conditional structure: signing steps are skipped when the identity secret is absent and the unsigned build takes the identical code path, so enabling signing later is adding a secret, not restructuring. State the caveat in the release notes rather than letting users discover it. Revisit before any release aimed at non-developers. **Restated at M13, 2026-08-11, now that the pipeline exists and this row is about shipped artifacts rather than planned ones.** The structure is built and correct: `packaging/macos/make_installer.sh` reads its identities from the environment, does nothing when they are empty, and runs the identical sequence either way, with the caveat printed at the end of every unsigned run and shown in the installer's own welcome pane so it travels with the artifact. **But "adding a secret" understates what turning signing on costs, and this row should not have implied otherwise:** an identity *string* is not an identity on a CI runner — `codesign` needs the certificate imported into a keychain, i.e. a `.p12` secret plus `security create-keychain`/`import`, which is unbuilt. So the signed path is unreachable from CI today even with the identity secrets set, and macOS remains developer-only in practice for the reason this row gives, not merely for want of a purchase. Windows is unchanged and unmitigated: SmartScreen warns on every release, Smart App Control blocks outright, and the installer is unsigned. |
| R-12 | **New, from M9's P0 decision pass, 2026-08-08.** D-16.5's writer is synchronous: the thread that emits a record holds the logger mutex across one unbuffered `write_all`, and `namir-app`'s UI thread is one of the threads that emits records. FR-UI-060 budgets no frame above 100 ms while a 10 000-file library scan runs — which is also the condition under which `namir-worker`'s pool threads log most heavily, so the UI thread can queue behind a pool thread's disk write on a slow or contended volume. Not observed; what is recorded here is the shape of the interaction, before anyone builds it. | Low | Keep the default level at `info`, where records are per user action rather than per frame, and hold D-16.5's rule that no record is emitted from a per-frame path. **The detector this row wants does not quite exist, which is worth stating rather than assuming.** FR-UI-060's own timed check (`crates/namir-ui/src/library_view.rs:280-338`) renders the full library view against a real 10 000-entry corpus and asserts the 100 ms ceiling, so it would catch a per-frame regression in the view itself — but it builds its snapshot with `scan: None` (`:316`), so it never runs under FR-UI-060's own "while a scan is in progress" condition and would not see this interaction at all. Extending it to a scan-in-progress snapshot is the cheap way to make it the detector, and FR-UI-060 is one of the requirements M9a's quantifier sweep reaches on its own terms under D-23.1. If a regression does appear, the fix is additive and leaves D-16.5 intact: a bounded queue in front of the same writer, overwriting oldest with a count — the policy D-7.3 already applies to outbound telemetry. |
| R-13 | **New, from M9's P0 decision pass, 2026-08-08.** D-23.1's `trace-partial` is a laundering surface: it is strictly easier to write than the test that would close a gap, and a contributor under time pressure has a sanctioned way to make a half-met Must read as covered. The failure mode is quiet and cumulative rather than acute — no build breaks, no number moves, the gate stays green, and partials accrete until "PARTIAL" is the normal disposition and carries no information. This project has the precedent: §14's snapshot table went six rows untouched since M0 while reading as current, and M7's close-out claimed sixteen requirements were individually investigated when at least three were not. | Medium | Four mechanisms, none relying on diligence. (a) A partial costs *more* keystrokes than a full tag — the `uncovered:` line must name the specific member and a closing milestone in prose, in the diff. (b) It is rendered into generated, checked-in `docs/03-test-plan.md`, so it is visible in every PR and in `git log -p` permanently. (c) There is no path to 1.0 through a partial: D-18.5's zero-uncovered half becomes required at M13's close-out, and M8's exit checklist reads §14's table adjudicated under D-23.2, where a partial cannot be counted **Done**. (d) The ordinary run prints the partial count on every invocation, so the number is in front of whoever runs the gate rather than buried in a table. Re-read the partial list at each milestone close, the way §22's own rows are re-checked; if the count is not falling by M12, the mechanism is being used as a bypass and this row is not mitigated. |
| R-14 | **New, from M9's P0 decision pass, 2026-08-08.** M8's exit gate is "every row in §14 reads **Done**", and after D-23.2 the only mechanically-checked part of that table is its Must-count column and row set. The three verdict columns stay hand-adjudicated — 72 cells in the re-audited table, 24 FRS-area rows by three columns — so a wrongly-**Done** cell still passes CI silently, the same failure mode that produced six rows untouched since M0 and five contradicted by prose written beneath them, now sitting directly under the 1.0 ship decision. The M0 evidence says this is not hypothetical: two denominators were wrong the day they were written and nothing noticed for seven milestones. | Medium | D-23.2's per-cell evidence-naming rule: a **Done** cell citing no file path is not a **Done** cell, which makes an unsupported verdict visible in review instead of invisible inside a number. M9a's re-audit establishes one dated baseline; M9b and every later milestone append their moves beneath it as the six prior sessions did, so a cell's age stays readable. Re-check at M8 against evidence rather than trusting the accumulated table, and treat any **Done** cell whose evidence is a one-time manual run as **Partial** until it is repeatable. |
| R-15 | **New, from M9a, 2026-08-08 — found while landing D-18.6's `clack-host` dev-dependency.** `xtask attribution` can list in THIRD-PARTY-NOTICES.md a crate the shipped binaries do not contain. It walks `cargo metadata`'s **single unified resolve**, keeping any edge that carries a `Normal` `dep_kinds` entry (`xtask/src/cargo_meta.rs:82-91`, the test itself at `:83-87`) — but that resolve does not decouple features by dependency kind, so features a **dev**-dependency turns on sit on the same node as the shipped one. The trigger condition is therefore precise rather than vague: **any dev-dependency edge that enables an *optional normal* dependency of a crate the shipped graph already reaches.** That optional dependency becomes a `Normal` edge out of a package the walk visits, and it is attributed. Live instance, which is why this row exists rather than being theoretical: `clack-extensions` 0.1.1 declares `clack-host` `optional` under `[dependencies]` (its `Cargo.toml:118-121`), so enabling that crate's own `clack-host` feature from `namir-clap`'s `[dev-dependencies]` puts `clack-host` into the attribution file — recorded as measured at `crates/namir-clap/Cargo.toml:93-105`, and readable without re-running it from those two manifests plus `cargo_meta.rs`. The error direction is the safe one and that is why this is **Low**: the walk over-approximates and cannot *omit* a shipped crate, so NFR-LIC-030 is never under-served. What it costs is still real — an attribution file naming crates the binary does not carry is a false statement about what Namir redistributes, and `xtask attribution` becomes a gate that fails for a reason no reviewer can act on, which is the shape D-18.5's own reasoning warns about. | Low | The detector already exists and needs no new tool: **`cargo tree -e normal` disagreeing with `xtask attribution`** is exactly this condition, which is why D-18.6's third landing gate pairs those two commands rather than either alone. Run both whenever a dev-dependency edge is added or its feature list changes, and read a crate that `attribution` lists while `cargo tree -e normal` cannot reach as this row, not as a real leak. Avoided rather than fixed today — `clack-extensions`' `clack-host` feature is off, so the notices file is correct as it stands. The fix, when a harness finally needs those host halves, is to resolve dependency kind per shipped path rather than per unified-resolve node (`--filter-platform` plus a per-root walk, or the resolution `cargo tree -e normal` already performs); **M9b owns it**, per D-18.6's dated landing note. |
| R-16 | **New, from M13, 2026-08-11 — found by the Linux packaging lane while writing `install.sh`, not by anyone looking at the UI.** Both products draw through `baseview` 0.2.2, whose **only Unix backend is X11 + GLX**; there is no Wayland backend in any published `baseview` version. A Wayland-only session therefore needs XWayland present, and a session without it gets no window at all. This has been observed rather than inferred: M12's own status subsection records `cargo run -p namir-app` starting audio and then panicking inside `baseview`'s X11 window open, with `xvfb-run` not helping because that path needs a GLX-capable display. The risk is not that the constraint exists — D-15.2 pinned this stack knowingly — but that **M13 is the milestone at which it stops being a developer's problem and becomes a user's**: before M13 the only way to run Namir on Linux was to build it, and anyone building it already had the X11 development headers. An installed binary reaches people who did not. Wayland is the default session on current Fedora, Ubuntu and RHEL, and several distributions are actively removing their X11 sessions rather than merely defaulting away from them. | Medium | Stated rather than fixed, because fixing it is a windowing-stack migration and not a packaging change: `packaging/linux/install.sh` reports the runtime constraint alongside its `libGL.so.1` check, `packaging/linux/README.md` records it, and `docs/user-guide.md`'s Known Limitations carries it in the user's own words. The upgrade path is known to be closed at both ends and was checked at M13 rather than assumed: `baseview` 0.3.0 renames `WindowOpenOptions` to `WindowSettings` and still has no Wayland backend, and the newest published `egui-baseview` (0.6.0, the pinned one) requires `baseview` 0.2.2 — so the pin is forced, not merely conservative. Revisit when a `baseview` with a Wayland backend exists, or when XWayland stops being present by default on a tier-2 platform, whichever comes first. |

---

## 23. Traceability

Every decision above cites the requirement it serves. The reverse mapping — every **Must**
requirement to the component that satisfies it — is generated and checked in CI per
FRS §10 and NFR-QUAL-010, and is not maintained by hand in this document.

*Consequence (added M7)* — Built as `cargo run -p xtask -- traceability [--write]`. The
"ID → component" mapping this section promises is produced at **crate granularity**: for each
Must requirement, the crate(s) containing a `// trace: FR-XXX-NNN` comment (or an
`fr_xxx_nnn_...`-named test function, the pre-existing convention) next to the covering test are
that requirement's recorded component(s). This is a deliberate, named scope reduction, not a
silent gap — resolving to the exact module or function would need either a heavier annotation
convention repeated at every call site or a source-level static analysis this project has no other
use for, and crate granularity already matches D-5.1's own level of description for what each
crate is responsible for. See FRS §10's matching `*Consequence (added M7)*` note for the full
mechanism, including why `docs/03-test-plan.md` is generated rather than hand-authored.

*Consequence (added M9, 2026-08-08 — the adjacency this note describes was never checked)* — the
note above overstates what a recorded component means, and there is a live false positive proving
it. "The crate(s) containing a `// trace: FR-XXX-NNN` comment … next to the covering test" describes
an adjacency the tool does not check. `trace_annotations` matches the marker **anywhere in a line**
(`xtask/src/traceability.rs:115-131`) and `build_report` never verifies that a test exists at all
(`:190`); the `fn_name_embeds_id` fallback is a whole-file substring match (`:138-141`) requiring
neither a `#[test]` attribute nor a function that is real. Both leak from the tool's own source: `//
trace: FR-NAM-070` inside string literals at `:310` and `:318`, `"fn
fr_nam_070_crossfade_glitch_free() {}"` at `:331`, and its own doc comment's `fn fr_nam_070_...` at
`:135`. The result is checked in — `docs/03-test-plan.md:70` records FR-NAM-070's components as ``
`namir-engine`, `xtask` ``, and **xtask contains no crossfade test**. The requirement is genuinely
covered (`crates/namir-engine/src/engine.rs:514`, via the fn-name fallback — there is no `// trace:
FR-NAM-070` anywhere under `crates/`), so nothing was ever claimed that is untrue about FR-NAM-070
itself; what is untrue is the component attribution this note promises. There is a second, smaller
instance of the same class: the marker parse applies no id-shape filter, so
`.github/workflows/ci.yml:109`'s own explanatory comment — which names the marker in prose — is read
as a tag and pushes a garbage id under component `ci` on every run. It cannot affect exit status,
because an id nothing looks up is never looked up; it is nonetheless the tool counting comments as
coverage. D-23.1 below closes all three.

*Consequence (added M9, 2026-08-08 — what the scanner actually reads)* — the scanned set this
mechanism draws on is wider than "repository source", and it is **hard-coded**.
`xtask/src/main.rs:184-216` walks every `.rs` file under `crates/` (component = the crate directory
name) and under `xtask/`, then appends four fixed paths: `.github/workflows/ci.yml` and `fuzz.yml`
as component `ci`, and the root `Cargo.toml` and `deny.toml` as component `workspace`.
`traceability.rs:111`'s second marker spelling, `# trace:`, exists for exactly those four. Fifteen
Must requirements rest on them alone — the eight annotations at `Cargo.toml:1,37`,
`ci.yml:34,158,197,322` and `deny.toml:15,82` name fifteen ids between them, re-derived one site at
a time — and FRS §10's matching `*Consequence (added M9, 2026-08-08)*` note carries the adequacy
rule that governs when that is legitimate, correcting the M8-planning note there which asserted the
tool reads source "not workflow YAML". That assertion is false as written, and enforcing it as
written would have moved the uncovered-Must count from 24 to 39.

Two mechanical consequences of that list being fixed rather than derived. First, `fuzz.yml` is
scanned and carries no tag today, so it costs nothing and is evidence of nothing. Second,
**`.github/workflows/release.yml` is not on the list** — it does not exist yet either — and
FR-PKG-010's `*Verify:*` line elects the release workflow as its artifact under the FRS rule's
second limb (`01-functional-requirements.md:861-862`), so M13 must either extend `main.rs`'s list or
give FR-PKG-010 different evidence. Defaulting into the second by not noticing is the failure this
note exists to prevent; recorded as roadmap §15 item 10.

Two further rows pass the rule on thinner evidence than the generated table implies, and are M9a
re-audit items rather than corrections here. **NFR-LIC-010** — `Cargo.toml:1`'s tag reaches the
manifest's `license` field only; "licence files present, manifest metadata correct, SPDX headers
checked" names three checks and nothing performs the first or third (`LICENSE-MIT` and
`LICENSE-APACHE` do exist, asserted by nothing). **NFR-BUILD-020** — `ci.yml:34`'s `build-test` job
runs `cargo build`/`cargo test` directly rather than the documented commands it is supposed to keep
from drifting, so the requirement's documentation half has no artifact behind it.

This correction takes **no decision number**, deliberately: it is a statement of fact about a tool
plus an adequacy rule, recorded as a paired note here and at FRS §10 in the same shape M7 used for
these same two sections. D-23.1 and D-23.2 below do take numbers, because the rules they carry are
cited from other documents and need citable identifiers; this one is cited from nowhere but its own
pair.

*Consequence (added M9, 2026-08-08 — which half of this check blocks a merge)* — the check this
section describes is **split into two gates with different flip dates by D-18.5**: the generated
plan's freshness is required from **M9a**, and the zero-uncovered condition stays informational
until **M13's close-out**, because nine of the twenty-four Musts the tool reports uncovered are
owned by M10, M12 and M13 rather than by M9 — ten once the same pass moves NFR-PERF-030 to M13. The
reverse mapping this section promises is therefore defended against *regression* four milestones
before it is *complete*, which is the opposite of the order this section's wording implies. Read
"checked in CI per FRS §10 and NFR-QUAL-010" against D-18.5 for which half actually blocks a merge
on any given date.

**Decision D-23.1 (added M9's P0 decision pass, 2026-08-08)** — A `// trace:` tag is an assertion
that the annotated artifact verifies the **whole** requirement, **by the requirement's own stated
`Verify:` method**. Tagging is three-valued, and which value applies is stated in the source, never
left silent. Numbered 23.1 because §23 is the section that governs what this project may claim about
requirement coverage, and it has carried no decision until now.

Before adding a tag, answer two questions:

1. **Does the requirement — or its `Verify:` method — quantify over a set?** ("each", "every",
   "all", "any supported…", an enumerated list, or a scale the method names: FR-LIB-020's "with a
   synthetic library of at least 10 000 files" is a quantifier even though the requirement's own
   sentence has none.) If so, does the artifact span every member?
2. **Does the artifact execute the method as written?** A `Verify: B` needs a benchmark that
   **asserts** a numeric threshold in-process, not one that prints a figure for a human to read. A
   `Verify: G` needs the named external reference, not a second in-house implementation. A `Verify:
   I` needs the integration the method describes.

Then:

- **Both yes → `// trace: <ID>`.** Unchanged from M7.
- **An artifact exists but fails either question → `// trace-partial: <ID>`, immediately followed by
  `// uncovered: <ID> — <the unspanned member or unexecuted half>; closes M<n>`.** Both lines or
  neither: a `trace-partial` without an `uncovered` line on the next line is a hard error from
  `cargo run -p xtask -- traceability`, in the same class as a Must with no `*Verify:*` line. **The
  `uncovered:` text may wrap**: consecutive `// uncovered:` comment lines are joined with a single
  space into one rendered field, because the FR-LIB-020 annotation this decision prescribes runs to
  about 185 characters and a single-line rule would put the doctrine at odds with the 100-column
  comment width the rest of this tree keeps. **FR-LIB-020 is the worked example, and it is this
  pass's own case.** Its method names a scale — "I with a synthetic library of at least 10 000
  files" (`01-functional-requirements.md:635`) — and the cancellation and progress clauses are
  exercised at that scale, but the evidence for "shall occur off the audio thread" is
  `rt_stress.rs`'s scanning axis, whose corpus is six files by deliberate design
  (`crates/namir-worker/tests/rt_stress.rs:138-149`, whose own doc comment explains that it wants
  many fast scan cycles rather than one slow one). So FR-LIB-020 takes a `trace-partial` naming that
  clause, not the plain tag M9's own draft work list proposed for it — which is precisely the
  disposition these two questions produce, applied to the requirement that prompted them.
- **Nothing covers it → no tag, in any form.** A requirement whose implementing milestone has not
  run is this case: it stays `**UNRESOLVED**` in the generated plan and is attributed there to its
  owning milestone by D-18.5's printed attribution, which never affects exit status. FR-NAM-140 is
  the worked example — its configuration clause is currently *false* rather than partly verified, so
  a `trace-partial` would be as untrue as a plain tag; it closes at M10 Phase 0. The same
  disposition covers FR-NAM-090 and FR-NAM-150 (M10), NFR-LIC-070 and NFR-DOC-040 (M12), and
  FR-PKG-010/-020/-030/-040 (M13) — **nine** of the twenty-four Musts `docs/03-test-plan.md`
  currently lists as `**UNRESOLVED**`, ten once NFR-PERF-030 moves to M13 in this same pass.

*Rationale:* `xtask traceability` answers "does something reference this identifier?", which is the
wrong question for any requirement quantifying over a set — one matching test satisfies the tool
however much of the set it misses. FR-NAM-030 is the proven instance: it has read covered since M3
while only WaveNet was ever compared against `NeuralAmpModelerCore` (changelog 0.6's own scope note,
"S-1 covered WaveNet only… LSTM is unaddressed"), and the tool is right by its own rules the whole
time. The failure is not the tool's; it is that a tag had no defined meaning, so two contributors
could tag identically-shaped situations in opposite directions — and did, within one milestone's
draft work list. This decision gives the tag a meaning precise enough that a reviewer asks a
mechanical question instead of exercising judgement.

*Consequence — the uncovered member lives next to the test, in source, written by whoever adds the
tag.* Not in a milestone close-out, not in a side document. The M7 note above chose to generate
`docs/03-test-plan.md` rather than hand-maintain "a document that would drift the moment a test
moved"; the same reasoning applies with more force to a record of what a test *fails* to cover.
`xtask traceability` renders every `trace-partial` as a **PARTIAL** row carrying its `uncovered:`
text verbatim and its closing milestone, so a partial cannot be introduced without appearing in a
generated, checked-in, diffable file in the same pull request.

*Consequence — a partial counts as covered for the ordinary run, and there is no path to 1.0 through
one.* `cargo run -p xtask -- traceability` treats a `trace-partial` as coverage. It must: FR-NAM-030
is knowingly half-met until M10 Phase 4, and a gate that cannot go green is the
red-check-nobody-can-act-on problem M7's own reasoning gave for marking this check informational in
the first place. The teeth are elsewhere, in two places that already exist rather than in a new
mode: D-18.5's zero-uncovered half becomes **required at M13's close-out**, after which a partial
that has not closed is a red required check; and M8's exit checklist reads §14's table adjudicated
under **D-23.2**, where a requirement with a named unmet clause is **Partial** and a Partial is not
**Done**. A partial is therefore a debt with a named due date, and §22's **R-13** records what
happens if it stops being one.

*Consequence — NFR-QUAL-010's text stands unchanged, on D-9.11's precedent.* NFR-QUAL-010 says every
Must "shall be covered by at least one automated test", and an untagged Must whose milestone has not
run is not that. The requirement is a statement about the shipped product, not about every
intermediate commit; a milestone sequence in which some Musts are not yet built is what a roadmap
*is*. **D-18.5's M13 close-out flip is what makes NFR-QUAL-010 literally true at the only moment it
must be.** Per §1's authority order, this document does not edit the FRS to match itself — it
records the route to satisfaction, exactly as D-9.11 did for NFR-QUAL-030.

*Consequence — the three integrity holes the M9 note above records are closed in the same change.*
The `trace:`/`trace-partial:` marker must begin a comment line whose next non-blank line is a
`#[test]`, `#[bench]` or `#[cfg(test)]` item, a `[[bench]]` declaration, a CI job or step
declaration, or — stated explicitly because it is the majority case here — a `fn main()` in a
`benches/*.rs` target. Every benchmark in this workspace is `harness = false` with a plain `fn
main()`, and all four existing bench tags already sit directly above one
(`crates/namir-engine/benches/denormal_guard.rs:411`, `six_stage_chain.rs:242`,
`tail_structure.rs:211`, `crates/namir-library/benches/library_scan.rs:154`); for **all five** of
the ids those tags carry — NFR-RT-030, NFR-PERF-010, NFR-RT-040, FR-LIB-030, NFR-PERF-060 — the
bench tag is the only evidence there is, checked one id at a time across `crates/` and `xtask/` and
confirmed by the generated plan recording exactly one component for each (`docs/03-test-plan.md:61`,
`:113`, `:118`, `:132`, `:133`). A rule that did not admit `fn main()` would therefore turn five
covered Musts into hard errors on the same commit that makes the plan-diff half required. The
fn-name fallback must find the identifier on a line beginning `fn ` that is itself preceded by a
test attribute. And the adjacency requirement is what stops prose that merely *names* the marker
from parsing as a tag: `ci.yml:109` is the standing instance, and the tool applies no id-shape
filter, so nothing else would. None of the three is optional given what a tag now asserts: a
doctrine that gives tags meaning while the tool counts string literals and comments as tags is worth
nothing.

*Rejected — leaving the doctrine to code review, with no annotation change.* Every case in the
confirmed set was reviewed and merged by someone who had read the requirement. FR-NAM-030's half-met
status survived M3 through M8 that way. A convention with no artifact leaves nothing for the next
reader to find.

*Rejected — treating a partial as uncovered (the strict reading).* Correct in principle and unusable
in sequence: it would keep the zero-uncovered half informational past M13 rather than flipping it at
M13's close-out, leave four milestones' claims landing in the ledger M9 exists to repair, and move
FR-NAM-030 from covered to uncovered and back for reasons that are not changes in verification.

*Consequence (added M9a, 2026-08-08, from building it — the adjacency rule as built is weaker than
the clause above, deliberately, and this note is the divergence rather than a restatement).* The
clause above names six admissible following declarations. `check_adjacency`
(`xtask/src/traceability.rs:502-520`) enforces a weaker rule instead: **the tag's next non-blank
line must exist and must not itself be a comment**, where "comment" is `//`-anything, or
`#`-anything except the two Rust attribute forms `#[` and `#!` (`is_comment_line`, `:526-533`). It
is not a whitelist, so it admits anything that is not a comment. Beyond the six named members, the
shapes it actually admits in this tree are three: a **TOML table header or key line**
(`Cargo.toml:2`'s `[workspace]`, `:38`'s `missing_docs = "warn"`, `deny.toml:17`'s `[graph]`,
`:83`'s `deny = [`); a **Rust inner attribute** `#![…]` (the three fuzz targets' `#![no_main]`); and
a **plain Rust item declaration**, where the tag is file- or item-level rather than test-level —
`use` (`xtask/src/attribution.rs:16`, `params_lock.rs:10`, `traceability.rs:47`), `pub mod`
(`crates/namir-fixtures/src/lib.rs:18`), `pub struct` (`crates/namir-ui/src/app.rs:134`) and `const`
(`xtask/src/layering.rs:42`).

*Why the literal reading could not ship.* Enumerated over the scanned set rather than argued: the
six-member whitelist rejects **13 of the 105 live tag sites, carrying 18 distinct requirement ids**,
and a rejection here is a hard error that aborts the entire run (`xtask/src/main.rs:292-298`) — not
a coverage loss but a red build, on the same commit that makes the plan-diff half required. The
thirteen are `Cargo.toml:1` and `:37`; `deny.toml:15` and `:82`; the three fuzz entry points
`crates/namir-ir/fuzz/fuzz_targets/probe_wav.rs:13`,
`crates/namir-nam/fuzz/fuzz_targets/load_nam.rs:9` and
`crates/namir-state/fuzz/fuzz_targets/read_state.rs:16`; `crates/namir-fixtures/src/lib.rs:16`;
`crates/namir-ui/src/app.rs:133`; and `xtask/src/attribution.rs:14`, `layering.rs:40`,
`params_lock.rs:8` and `traceability.rs:45`. The eighteen ids are FR-CFG-010, FR-CFG-030,
FR-ERR-060, FR-ERR-070, FR-PARAM-020, FR-UI-010, NFR-BUILD-010, NFR-DOC-020, NFR-LIC-010,
NFR-LIC-020, NFR-LIC-030, NFR-LIC-040, NFR-LIC-050, NFR-PORT-020, NFR-QUAL-010, NFR-QUAL-040,
NFR-SEC-010 and NFR-SEC-030 — and for **every one of them the rejected site is the only evidence
there is**, none appearing at any admissible site (`docs/03-test-plan.md:7`, `:9`, `:36`, `:37`,
`:80`, `:95`, `:102`, `:105`, `:107-111`, `:120`, `:124`, `:127`, `:134`, `:136`). The structural
reason is worth stating rather than treating this as thirteen accidents: **seventeen of the eighteen
are `Verify: S`.** A static or build-time check is verified by the build configuration that performs
it, so its tag sits above a manifest key, a lint attribute or a `use` at the head of the module that
*is* the check — never above a `#[test]`. The six-member list is test-shaped, and `Verify: S` is the
one code it cannot describe. This is the same near-miss as the `fn main()` case the clause above
caught explicitly, one category wider and three and a half times larger.

The eighteenth, recorded rather than rounded away because it slightly weakens the rule stated above:
**FR-CFG-030 is `Verify: I`, not `S`** (`01-functional-requirements.md:145`). It rides on
`xtask/src/layering.rs:40` beside NFR-PORT-020 because `scan_platform_cfg` is in fact what checks it
(`layering.rs:8` states the equivalence). So it takes the same *shape* for the same reason — a
build-time check rather than a test — while its Verify code says otherwise, which is a small
instance of the gap D-23.1's first question exists to catch and is left as a finding for M9a's
per-requirement sweep rather than repaired here.

*What the weaker rule still buys — and the claim above it does not support.* The clause above says
"the adjacency requirement is what stops prose that merely *names* the marker from parsing as a tag:
`ci.yml:109` is the standing instance". **That attribution is wrong, and adjacency closes none of
the three false positives the M9 note above records.** All three fall to the other two fixes in the
same change. Rule 1 — the marker must *begin* the trimmed line, matched by `strip_prefix`
(`traceability.rs:225`, `:312-317`) — kills both the string-literal class (the M9 note's `:310` and
`:318`, line numbers in the pre-fix source rather than in the tree as built) and `ci.yml:109`
itself, whose trimmed line reads ``# `// trace:`/manual-test coverage found…`` and is therefore a
prefix of neither marker spelling; the tightened `fn_name_embeds_id` (`:546-569`) kills the fn-name
class (that note's `:135` and `:331`, likewise). The tool's own tests say so by name
(`a_marker_that_does_not_begin_the_line_is_not_a_tag`, `:1347-1358`, whose two cases are labelled
"the `:310`/`:318` class" and "the `ci.yml:109` class"), and `check_adjacency`'s doc comment carries
the same correction (`:483-501`). What adjacency does buy is a fourth class none of those reach: a
**well-formed** tag, naming real ids at the start of its own line, that sits above no declaration at
all — a marker inside a prose or doc-comment block, one stranded at end of file, or one that has
*drifted* from its artifact because a doc comment or another item was inserted between the two
(`a_tag_above_a_comment_line_is_a_hard_error`, `:1459-1463`; `a_tag_with_nothing_after_it_is_a_hard_error`,
`:1466-1469`). That is a regression guard on tags that already exist, which is worth having on its
own terms and is not what the clause above claimed for it.

*What it does not buy, stated plainly so the rule is not read as tighter than it is.* It tests one
line for one property. A tag whose next non-blank line is a bare `}`, a `#[derive(Debug)]`, a
`struct S;` or any other line of ordinary code is **accepted**, and its ids are recorded as
coverage. Adjacency as built means "not detached from *something*", not "attached to a test", and it
cannot tell a tag above the test it describes from a tag above the closing brace of the test before
it. The assertion a tag makes is still the whole one this decision defines — the whole requirement,
by its own `Verify:` method — and it is still carried entirely by whoever writes the tag and whoever
reviews it. Narrowing this later is possible per file type (one anchor set for `.rs`, another for
TOML and YAML) and is deliberately not attempted at M9a: every shape such a whitelist would have to
enumerate is already in this tree, so it would be a transcription of today's tag sites rather than a
rule, and the next `Verify: S` requirement to arrive in a shape nobody listed would break the build
for being correctly annotated — which is exactly the failure this note exists to record having
avoided once.

*Consequence (added M9a, 2026-08-09) — the Rationale's own worked example is understated, and the
correction runs the same direction as the decision.* The Rationale above says FR-NAM-030 "has read
covered since M3 while only WaveNet was ever compared against `NeuralAmpModelerCore`", and D-23.2's
Rationale and clause 4 below (`:3030`, `:3057`) and changelog 0.19 carry the same half-met wording.
**M9a's sweep established that no in-tree runnable artifact compares *either* architecture against
that implementation.** Both parity tests compare against `namir-fixtures`' own from-scratch Rust
ports: `crates/namir-nam/tests/fixtures.rs:129` calls `nam::reference_infer`
(`crates/namir-fixtures/src/nam/mod.rs:95`, a re-export of `infer::run`) and `lstm_fixtures.rs:120`
calls `nam::reference_infer_lstm` (`mod.rs:142`, of `lstm_infer::run`). Neither reaches C++. The two
`// trace-partial: FR-NAM-030` pairs the sweep wrote (`fixtures.rs:120-126`,
`lstm_fixtures.rs:112-118`) say so in the source, and both name the same gap rather than one naming
LSTM.

*What was true, and why it is nonetheless not this requirement's evidence.* The WaveNet comparison
the Rationale credits was real and is not retracted: §19's S-1 Result records a `NeuralAmpModelerCore`
comparison (MIT, commit `3cde95c3`) at **-131 dB** over a generated 10 s FR-NAM-030 signal on the §2
reference machine. Three facts stop it being FR-NAM-030's artifact. It lives in
`spikes/s1-nam-inference/`, which the root manifest `exclude`s from the workspace
(`Cargo.toml:15-20`) and which pins its own lockfile, so nothing re-runs it under `cargo test` —
D-23.2's clause 1 requires evidence "being **repeatable**, i.e. something re-runs it when the code
changes", and a one-time out-of-workspace measurement is not that. It measured the **spike's**
engine, and `crates/namir-nam/src/wavenet.rs` is a port of it (`namir-nam/src/lib.rs:20-33`) whose
inner loops were vectorized with `wide::f32x8` at M3 (changelog 0.10; `wavenet.rs:35`) — so the
shipped code differs from the code that was measured, in exactly the arithmetic-ordering dimension a
-131 dB parity figure is sensitive to, and nobody re-measured. And a `Verify: G` names an external
reference: D-23.1's **own second question** already says a `G` "needs the named external reference,
not a second in-house implementation", which is precisely what both live tests are.

*A second unmet clause, independent of the first.* FR-NAM-030 also specifies "a specified 10-second
test signal containing clean, transient and saturated material"
(`01-functional-requirements.md:373-376`). Both in-tree tests drive 4 000 samples of a 110 Hz sine
plus noise (`fixtures.rs:20-31` and `:134`; `lstm_fixtures.rs:126`) — about 83 ms at the 48 kHz the
generator assumes, with no transient and no saturated material. So the requirement fails on two
independent clauses for both architectures, not on one clause for one architecture.

*Worth recording about the parity tests themselves, since they are good evidence of something else.*
`namir-fixtures` takes no dependency on `namir-nam` (`crates/namir-fixtures/Cargo.toml:11-17`; the
edge exists only as a dev-dependency in the other direction), so the two implementations are
genuinely independent code and their agreement is real evidence against a porting bug in either.
But they are not independent *readings*: `infer.rs:9-11` states that its operation order and flat
weight layout follow the semantics documented in `spikes/s1-nam-inference/src/lib.rs`, the same
source `namir-nam/src/lib.rs:20-24` cites for the shipped engine. A misreading of the C++ in the
spike would be reproduced in both and the parity test would still pass — the "two Rust ports
agreeing" weakness already recorded at `:2175` for `clack-host`, here with a *shared ancestor*
rather than merely a shared crate. That is what makes these tests `NFR-QUAL-030` evidence (which
`fixtures.rs:128` correctly still tags plain) and not `FR-NAM-030` evidence.

*Why this strengthens D-23.1 rather than weakening it.* The decision's argument is that a tag had no
defined meaning and that the tool cannot see a requirement's own method. FR-NAM-030 turning out to
be **wholly** unverified by its stated method, rather than half, makes that argument harder, not
softer — and the way it was missed is the decision's own case: D-23.1 wrote down the exact test that
catches it, in question 2, and its Rationale then did not apply that test to the example it chose to
prove the point with. The claim inherited from changelog 0.6's scope note ("S-1 covered WaveNet
only… LSTM is unaddressed") is accurate **about the spike** and was carried forward as though it
were a statement about the workspace; nobody re-derived it against the tests until the sweep did.
No text above is rewritten and no decision changes: this note is the correction, and D-23.2's
clause 4, its Rationale and changelog 0.19 are all read subject to it. FR-NAM-030's closing
milestone is unchanged at **M10**, where both annotations point.

**Decision D-23.2 (added M9's P0 decision pass, 2026-08-08)** — A **Must** requirement's status in
`03-implementation-roadmap.md` §14 is adjudicated against **that requirement's own text and its own
`*Verify:*` method** — never against whether an implementation exists, and never against `xtask
traceability`'s verdict. The three states are defined below and **every cell names the evidence that
puts it there, by file path**. Separately, that table's **Must-count column and its row set are
derived from the FRS rather than maintained by hand**: `xtask traceability` already parses every
`**ID (Must)**` line (`xtask/src/traceability.rs:43`, `:81`) and now also emits a per-FRS-section
Must count into the generated `docs/03-test-plan.md`, failing the build when §14's denominators or
its row set disagree with it.

*Rationale:* §14 is what M8's exit gate reads, and it was wrong in three distinct ways at once. Its
Must-count column was wrong in two rows **the day it was written** — 5.1 CHAIN said 7 against the
FRS's 8, 5.12 CLAP said 10 against 11, both checkable against the FRS as it stood at commit
`984b0b6` — drifted in three more when FRS 0.3 landed, and omitted §4 CFG and §5.15 PKG entirely.
Its verdict cells were moved by six sessions each applying a rule none of them wrote down, which
worked for each session alone and produced six rows untouched since M0 and five contradicted by
prose beneath them. And its two mechanical cross-checks pull in opposite directions: `xtask
traceability` reports FR-NAM-030 covered while only WaveNet was ever compared against the reference
implementation, and M7 had to correct 6.3 PORT from 0/4/1 to 5/0/0 on finding all five Musts already
true and merely untagged. A denominator a tool can derive should not be transcribed; a verdict no
tool can derive needs a written rule instead.

*Consequence:* the adjudication rule, stated once so that several people applying it to the same
document reach the same cell.

1. **Done** requires all four of: every clause of the requirement's own text satisfied, including
   any clause quantifying over a set ("each", "every", "all", "any supported"); the artifact its own
   `*Verify:*` code names existing in this repository and passing — `U`/`I`/`G`/`S` a test or check
   that runs under `cargo test` or in CI, `B` a **certified** figure per D-2.4 (§2's reference
   machine, ≥5 repetitions, estimator cross-check — a sandbox figure is never one), `M` a
   `docs/manual-tests/<id>-*.md` recording an execution that **passed**, not a script that exists;
   that evidence being **repeatable**, i.e. something re-runs it when the code changes; and the
   evidence named by path in the cell's audit entry. A one-time manual execution of a `Verify:
   U/I/G/B/S` requirement is Partial, not Done — which is exactly the finding §14 already carries
   against 5.12 CLAP's eight Done cells and their single `clap-validator` run.
2. **Partial** — materially built, but at least one **named** clause of its text or of its `Verify`
   method is unmet. Where the unmet part needs hardware, a host or a human, it is named in a
   `docs/manual-tests/*.md` file recording what was and was not executed and why; FR-IO-020,
   FR-UI-030 and FR-CLAP-090/-100 are the standing examples. **A Partial with no named gap is not a
   Partial** — it is an unaudited cell and is recorded as unaudited rather than guessed.
3. **Not started** — neither the behaviour nor its verification exists. This narrows §14's own M0
   gloss rather than replacing it: "primitive or scaffolding exists, not integrated into a real
   stage/product" is **Partial** here, and the M0 wording stays where it is as the historical text.
4. **A quantified requirement whose covering test spans only part of the set is Partial**, with the
   uncovered members named. FR-NAM-030 is the worked example — WaveNet is compared against the
   reference NAM implementation, LSTM is not. This is the class the traceability tool cannot see by
   construction, and M9a's sweep is what finds the rest; where such a requirement carries D-23.1's
   `// trace-partial:` pair, that pair is the cell's named evidence.
5. **A manual test recording FAIL or NOT EXECUTED is never Done.** It is **Partial** when the
   behaviour demonstrably exists and the blocker is external and named — absent hardware, no host to
   drive interactively, an upstream gap, FR-IO-020's `cpal` 0.18.1 finding being the precedent — and
   **Not started** when the script could not run because the thing it tests does not exist. The
   manual-test file states which.
6. **`xtask traceability` is not evidence for a §14 cell in either direction.** Covered ≠ Done
   (FR-NAM-030). UNRESOLVED ≠ Not started (M7's 6.3 PORT correction). §14's own note already records
   that the two artifacts over-report in opposite directions; this makes it a rule rather than an
   observation.

*Consequence — which CI gate the derived half rides on.* The per-FRS-section Must-count and row-set
check is mechanical and satisfiable immediately, so it rides on **D-18.5's required plan-diff half**
from M9a. It is not affected by the zero-uncovered half's informational status, and does not wait
for that half's M13 flip. The three verdict columns remain outside any gate — 24 FRS-area rows by
three columns, 72 cells adjudicated by hand — and **R-14** records exactly that.

*Consequence — the re-audited table is a baseline, not a freeze.* M9a publishes a corrected **row
set and denominators** with the verdict cells re-derived from evidence as of M9a, under the heading
`### M9a re-audit — corrected row set and denominators (2026-08-08)`. That heading, the column order
and the row-label form are machine-parsed by the check above, so they are fixed; the verdicts are
not. **M9b and every later milestone move only the cells their own evidence justifies**, appending
their moves beneath the table in prose exactly as the six prior sessions did. A cell's age must stay
readable, which is the whole reason the M0 table is superseded rather than rewritten.

*Implementation note:* the per-section grouping keys on the FRS heading in force when each
requirement is parsed, and takes the area token from the ids rather than from the heading — `## 4.
Product configurations` (`01-functional-requirements.md:115`) carries no `(CFG)` suffix where every
`### 5.x`/`### 6.x` heading does, and §4's three Musts are exactly the ones §14 has never carried.
Deriving section number from the heading and area from the ids handles both forms without amending
the FRS. One row is special-cased rather than matched against an FRS area: the re-audited table ends
with a `| **Total** | **130** | … |` row the M0 table lacks, and the check treats that label as a
**checked sum of the Must-count column** — it fails if the sum disagrees — instead of demanding an
FRS section named "Total". Stated here because a row-set comparison that did not know this would
reject the very table this decision prescribes.

*Rejected:* **generating the Done/Partial/Not-started columns as well** — nothing mechanical can
decide whether a requirement's text is met, and a tool that appeared to would put its own authority
behind the "the gate is green so the requirement is met" confusion M9 exists to remove. **Leaving
the Must-count column hand-maintained on the grounds that it is only twenty-four numbers** — those
numbers were wrong in two rows on the day they were written, drifted in three more within one FRS
revision, and omitted two sections; numbers derived from a document a tool already parses is
precisely the case for generating rather than transcribing, on the same reasoning that produced
`params.lock` and `docs/03-test-plan.md`. **Rewriting §14's existing table in place** — six
sessions' prose bullets are anchored to the physical cell values they moved, so rewriting the cells
makes each of those sentences unverifiable and destroys the audit trail that is the only reason this
drift was findable.

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
| 0.10 | 2026-08-06 | **M3 session: LSTM lands, R-4/R-8 both re-measured with real but partial results, NFR-PERF-010 stays open.** `namir-nam` gains `lstm.rs` (FR-NAM-020's other Must architecture, ported from `NeuralAmpModelerCore`'s `NAM/lstm.h`/`lstm.cpp`), unified behind the existing `PreparedNam`/`NamState` surface with zero `namir-engine` changes and parity-tested against an independent from-scratch reference — FR-NAM-020 closes. R-4: `wavenet.rs`'s inner loops are vectorized (`wide::f32x8`); measured (this session's sandbox, not this section's reference machine) at 42–47% p99.9 in isolation, down from S-1's 41% baseline only marginally in the direction that matters, and still over budget alone. R-8: `convolver.rs`'s stagger is retuned to a per-size, block-aligned scheme; measured at NFR-PERF-010's own condition, IR-stage-alone p99.9/max fell from 337.7%/602.5% to 16.8%/41.3%, a 15–20x improvement that closes the scheduling *defect* R-8 names. **New this session: `namir-engine/benches/six_stage_chain.rs`, the first real assembled-six-stage-chain benchmark** (gate → trim → nam → ir → eq → out, real generated WaveNet model and 2 s stereo IR loaded, gate + EQ actually engaged) — measured FAIL on this sandbox, p99.9 61–76% against the 25% budget. §22's R-4 and R-8 rows are both updated to reflect this: R-8's own defect is closed, R-4 measured real if insufficient progress, but neither is retired — the assembled-chain evidence is worse than either isolated figure alone would suggest, and the certified reference-machine run neither risk's retirement depends on has still not happened. Roadmap §7's M3 acceptance criteria are recorded as not met this session. |
| 0.11 | 2026-08-06 | **M3 close-out pass: R-4's isolated-loop figure re-verified and corrected.** Between 0.10 (above) and this pass, an independent review of R-4 rewrote `wavenet.rs`'s own Decision-note with a re-measurement claiming p99.9 spiked to 330–345% on every one of 8 runs in both the scalar and vectorized configurations — a materially worse and more pessimistic reading than 0.10's 42–47% figure, but never propagated into this document. This close-out pass re-ran the same A/B a third time (interleaved scalar-vs-vector, `uptime` load average explicitly checked quiet throughout, 9-11 runs) and could not reproduce the 330–345% reading at all — the highest p99.9 observed was 54.83%. No reproducible p50 win either way (scalar mean 26.58%, vector mean 26.80%); p99.9 overlaps heavily but doesn't cleanly separate from run-to-run noise (scalar range 44.3–54.8%, vector range 43.7–48.6%) — close to 0.10's original 42–47% estimate, not the intervening 330–345% one. The most consistent available explanation, not confirmed: this same session's R-8 re-verification separately documented that this shared sandbox's single-sample runs can read 10-20x high under concurrent CPU contention, and the intervening re-measurement's own revert-rebuild-rerun sequence did not record checking for it. §22's R-4 row and `wavenet.rs`'s own Decision-note are both updated with the full run-by-run numbers so this reading is now the one on record everywhere. Net effect on the milestone's conclusions: unchanged — R-4 stays downgraded, not retired, NFR-PERF-010 stays open, the six-stage-chain FAIL stands. Also confirmed via a fresh full gate run this session: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace --all-targets`, `cargo test --workspace`, `cargo run -p xtask -- layering`, and `cargo run -p xtask -- params-lock` are all green on this sandbox as of this pass. |
| 0.12 | 2026-08-06 | **Recorded retrospectively during M4, closing a gap M3's close-out left in this log.** §2 gained **D-2.3** and **D-2.4** and the certified NFR-PERF-010 pass during M3, but no changelog row was ever written for any of them — so this document's own history said the requirement was still open while §2 said it had closed. D-2.3: the x86-64 baseline is `x86-64-v3` (AVX2+FMA+BMI), set in `.cargo/config.toml`; no `target-cpu` had ever been set, so every `wide::f32x8` was compiling to two 4-lane SSE ops. Setting it took the NAM stage from p99.9 30.3% to ~10.5% on the §2 reference machine with numeric parity unchanged at -130.8 dB — by §2's own account the single largest performance change in the project, and it was absent from this log entirely. D-2.4: D-2.2's p99.9 gate is kept exactly as written, with the measurement conditions under which it is valid added (pin away from device-ISR cores — `dxgkrnl.sys` puts ~165 interrupts/second of 128-512 µs on CPU 0 exclusively; verify the machine is quiet rather than assuming; ≥ 5 repetitions with the spread; and a mandatory cross-check against `tail_structure.rs`'s contamination-immune estimator, discarding any run whose raw p99.9 substantially exceeds it). NFR-PERF-010 closes at p99.9 **16.45-17.08%** against its 25% budget. §22's R-4 and R-8 rows are updated to **Retired** in the same pass — they had still read Medium while the roadmap's M3 close-out said both had retired. |
| 0.13 | 2026-08-06 | **M4: resource handover, worker, and cross-instance sharing.** `namir-engine` gains D-7.2's SPSC command ring and D-8.1's return ring (`ring.rs`, on the new `rtrb` dependency — §17), D-7.3's lock-free telemetry ring (`telemetry_ring.rs`, plain `AtomicU64`s, no dependency: a `TelemetryEntry` is exactly 64 bits, so tearing within an entry is impossible by construction while the across-entry tearing D-7.3 permits stays possible), and `AudioEngine`, which owns the rings so `Chain` stays the pure DSP object `six_stage_chain.rs` measures. New crate **`namir-worker`** (D-5.1's row, finally built): D-7.1's pool, D-8.2's cache, and the worker halves of D-8.1 — with **no** third-party dependency of its own. **The one known P1 violation in the codebase is closed**: a completing crossfade used to drop the outgoing slot on the audio thread, and a second, previously-undocumented site did the same to a slot displaced mid-fade; both are now moves into a retire pen that the return ring drains. Evidence: both stages' handover tests now run a full real-to-real handover *inside* the D-7.5 harness, which they could not before. FR-NAM-070 and FR-IR-060 close, each verified by its own literal *Verify: I* method. Five decisions gained M4 consequence notes recording things they did not anticipate: D-7.1 (the pool formula yields zero on one core), D-7.2 (bounded retry; the two rings' full-ring policies compose), D-8.1 (the worker must prepare the whole *slot*, not just the `Arc`; the second drop site; deferred finalization), D-8.2 (**the IR cache key must include engine rate and block size**, or a hit can hand back an IR whose `process_block` asserts — a panic on the audio thread, not a wrong sound). R-7's benchmark exists for the first time (`benches/handover_crossfade.rs`); see §22 for its disposition. |
| 0.14 | 2026-08-06 | **M4 follow-up: R-7's mitigation built and measured; the risk retires.** M4's first pass measured the crossfade and found the over-budget condition was not the one R-7 names — a NAM handover alone fits, and so does an IR handover alone; what does not fit is both at once. `namir-worker` now serialises them: before offering a handover for one target, an instance waits out any handover it recently offered for the other. A timer rather than telemetry feedback, and for a real reason rather than simplicity — the *first* load into an empty stage retires nothing and reports no fade in flight between submission and the audio thread's next block, so a purely feedback-driven rule races; a timer needs no feedback, cannot deadlock, and expires anyway if the audio thread stalls, which is the right failure mode. D-8.1 gains a `*Consequence (added M4)*` note recording the rule and its one user-visible effect: changing model and IR in a single action starts the second changeover ~20 ms after the first rather than simultaneously, which neither FR-NAM-070 nor FR-IR-060 forbids (each requires *its own* changeover to be glitch-free). `HANDOVER_CROSSFADE_MS` is promoted to a single public constant in `namir-engine`, having been privately duplicated in `nam.rs` and `ir.rs` — two copies of a figure the two stages must agree on, with the worker now needing a third. Measured with arms D and E interleaved in the same runs: **28.77–31.26% unserialised against 22.20–24.63% serialised**, overlap 10.9–43.8% → **0%**. §22's R-7 row retires with two residuals recorded, including that the remaining margin is only ~0.4 points. |
| 0.15 | 2026-08-07 | **M5: state, presets, and library.** Two new crates land: `namir-state` (D-11.1's JSON preset/state format, D-11.2's tolerant/versioned deserialisation via a never-discarded `Document` carrier rather than `#[serde(flatten)]`, D-11.3's three-way file reference and FR-STATE-070's resolution order behind a `FileResolver` port `namir-library` implements against the opposite dependency edge) and `namir-library` (D-12.1's incremental index — corrected mid-milestone, see below — D-12.2's caller-pumped scan step machine, **AQ-3 resolved**: a single pretty-printed JSON document, atomic replace, no new dependency). `namir-engine` gains `Command::Unload` (FR-STATE-070's "load with that stage empty" reuses the existing crossfade-to-`None` machinery, no new DSP). `namir-worker` gains `library::LibraryService` (driving the scan step machine on its pool, D-12.2's split finally whole) and `Instance::recall` (FR-STATE-030/050, sequential through the existing `load`/`unload` primitives specifically so R-7's cross-target serialisation rule cannot be bypassed by construction). **Six corrections to the governing documents**, each its own `*Consequence (added M5)*` note at the relevant section rather than collected in one place: D-5.1's self-contradictory `namir-library` platform-code cell; FR-LIB-070 vs. D-12.1's literal wording (a same-length edit inside one mtime tick was invisible — closed by a settling-window rule); new **D-2.5** scoping D-2.1's wall-clock rule to audio-thread budgets specifically, since four M5 requirements are wall-clock by their own FRS wording; `.gitattributes` marking `*.namirpreset binary` before Git's line-ending normalisation could silently repair a serialiser regression in the checked-in portability corpus; FR-STATE-070's silence on which library root and on a hash-mismatched path hit (resolved: no stored root identity, a mismatch falls through rather than substituting); and global bypass/output ceiling's missing `ParamDescriptor` home, flagged for an M6 decision. NFR-RT-010 moves Partial → Done (`crates/namir-worker/tests/rt_stress.rs`, all three axes concurrent, zero audio-thread allocation). NFR-PERF-050 and NFR-PERF-060 both close with margin (`benches/resource_load.rs`, `benches/library_scan.rs`). NFR-QUAL-040's second fuzz target (`crates/namir-state/fuzz`) lands in M5 rather than M7. NFR-DOC-010 closes: `docs/04-state-and-preset-format.md`, with its own and FR-STATE-040's manual tests actually executed rather than left as unrun scripts. `handover_crossfade.rs` re-run at close (preset recall is a new, more frequent path to R-7's worst condition): five repetitions on this session's own sandbox, two discarded as contaminated, the three clean runs matching M4's original figures closely — **no evidence of regression; R-7 remains retired.** §14's snapshot gains six live-updated cells (5.9, 5.10, 6.1, 6.2, 6.4, 6.6, 6.8) and §15 strikes AQ-3. The FRS §5.9/§5.10 "close in full" acceptance line in roadmap §9 is restated honestly: seven of twelve Musts close in full, five close only their M5-resolvable half (the rest is M6 UI or, for FR-STATE-020, the first release itself). |
| 0.16 | 2026-08-07 | **M6: product shells — platform, app, UI, CLAP.** Four new/completed crates: `namir-platform` reaches full scope (D-13.2 config/log/thread-priority, D-13.3's CLAP install-path table); `namir-ui` (new) is a pure view+intent layer behind a `UiHost` trait, since D-5.1 forbids it depending on `namir-engine`/`namir-worker`/`namir-platform` — `namir-app` and `namir-clap` each implement that trait against their own real engine; `namir-app` (new) wires real `cpal` WASAPI I/O, verified against real hardware this session (a PreSonus AudioBox 22VSL: device enumeration, rate/buffer negotiation, a genuine opened-and-playing duplex stream); `namir-clap` (new) wires the real `Chain`/`Instance`/`REGISTRY`/`State` behind `clack-plugin`, validated for real against `clap-validator` (32/32 applicable tests passed, 0 failed, one real state-rescan bug found and fixed). **D-10.4 added**: `global.bypass`/`global.output_ceiling_db` become real `ParamDescriptor`s (`namir_params::global`), migrated off `namir-engine`'s dedicated `Command::SetGlobalBypass`/`SetOutputCeilingDb` side channel and `namir-state`'s parallel `global` JSON section — the concrete trigger was `namir-clap` needing bypass exposed as a normal CLAP host parameter (`IS_BYPASS`), which the side channel couldn't provide. `namir-worker` gains `Instance::try_submit_param` and `namir-engine` gains `AudioEngine::apply_param_direct`/`reset_direct` (host-automation events applied directly from `process()`, sound because the audio thread already holds `&mut AudioEngine` exclusively) — closing a real gap D-7.2's own module doc comment had promised but `Instance` never actually exposed; `namir-app`'s independently-built 575-line `LiveEngine` workaround for the same gap is deleted once `try_submit_param` existed, in favour of the identical `Mutex<Instance>` pattern `namir-clap` already used. **This crate's `set_parent` is the workspace's first new `unsafe` code since M1** (`namir-platform`'s `denormal.rs`); its D-5.3 safety argument was adversarially reviewed by an independent agent before merging, which found and this session fixed two real gaps (a recognised-but-wrong host window-API tag reaching a panic instead of this crate's own diagnostic; a double-`set_parent` orphaning the previous native window) — no soundness/UB hole was found. **NFR-RT-030 closes**: `namir-engine/benches/denormal_guard.rs` (new, a `namir-platform` dev-dependency exempt from D-5.1's layering check) certifies `DenormalGuard` — unused since M1 — keeps denormal-input processing within 1.6% of nominal across five §2-reference-machine repetitions against a 10% budget, while confirming guard-absent processing costs 1.33-1.38x more, real evidence the guard does something. §22's R-7 row gains a third re-check (its own standing "re-run whenever the audio callback changes" reason, and M6 is the first session since R-7 retired to actually touch that callback): raw p99.9 was contaminated by this session's own concurrent tooling load, but the contamination-immune estimator stayed at 14.0-14.5% across five repetitions — no evidence of regression. **Two Must-requirement gaps recorded honestly, not worked around**: FR-IO-020's WASAPI exclusive mode is architecturally absent from `cpal` 0.18.1 (verified against its vendored source), and R-5's failable-device test had no real hardware available this session — both flagged in `docs/03-implementation-roadmap.md` §15 as open decisions due before M8. §14's snapshot gains four live-updated rows (5.11, 5.12, 5.13, 6.1) plus a correction to 6.1's own stale cell (M5's prose had claimed a Done-count move the physical table was never edited to match). |
| 0.17 | 2026-08-08 | **AQ-4 researched (post-M6, not tied to a milestone).** No explicit licence found for NAM's standardised reamp/capture signal (`input.wav` and predecessors): it is distributed off the MIT-licensed `neural-amp-modeler`/`NeuralAmpModelerCore` source trees, from Steven Atkinson's personal Google Drive, with no licence or redistribution terms found in the repos, docs, Colab notebook, or project site. Treated as all-rights-reserved pending upstream clarification; §21's open-questions table and the S-0 "Open" note both updated with the finding and sources. Does not block anything currently scoped — only shipping factory presets, per AQ-4's own text. |
| 0.18 | 2026-08-08 | **M9–M13 planning: five decisions added ahead of the work, none of it built.** This row records *plans*, not results — every decision below is written before its milestone runs, which is the opposite of this log's usual direction, and each will need its own status note when the work actually happens. **D-9.12** (new §9.5, appended so §9's numbering stays monotonic rather than inserted into §9.1) decides NAM Architecture 2 support: extend the existing WaveNet parser rather than add a parallel architecture, because an A2 file declares `architecture: "WaveNet"` exactly as A1 does and differs only in its `config` schema — so `model.rs`'s private `enum Architecture` seam stays private and no public trait appears. Scope is core A2 only (A2-Full and A2-Lite, FR-NAM-150); `SlimmableContainer`, `condition_dsp`, FiLM and `.namb` are deferred by decision rather than overlooked. Two things fall out of it that were not obvious: FR-NAM-140's distinct unsupported-feature error is a *prerequisite*, because an A2 file today fails with `nam.load.malformed_json` — a false statement, the file is valid JSON, the A1 schema simply rejects `kernel_sizes[]`/object-`activation`/`gating_mode` at the deserialiser; and FR-NAM-090/100 stop being blocked, since `namir-nam` declared loudness normalisation out of scope precisely because the schema it reads carries no loudness metadata, which A2-era files (`loudness`, `input_level_dbu`, `output_level_dbu`) do. **D-13.4** closes the choice D-13.1's own M6 note left open — a `namir-platform`-owned unsafe WASAPI helper *or* an upstream `cpal` change — in favour of a Namir-maintained **fork of `cpal`** pinned by commit, on the reasoning that the share mode is chosen at an `IAudioClient::Initialize` call in the middle of cpal's stream construction, so the helper route means owning a second complete Windows audio path (enumeration, negotiation, buffer servicing, device-removal reporting) with FR-IO-050/060/070/080 holding on both. `AppSettings::exclusive_mode` was already added forward-compatibly at M6, so nothing migrates. **D-16.4** — *numbered 16.4, not the 16.3 the planning index called it, because D-16.3 was already taken by the worker-panic-isolation decision and this document does not reuse identifiers* — decides FR-ERR-010's logging as a hand-written bounded rotating writer in `namir-platform` with **no logging dependency**, over the `log` facade (Option A, recorded and rejected) and `tracing`. The deciding argument is structural rather than dependency-count: D-5.1 already forbids `namir-engine` from depending on `namir-platform`, and `xtask layering` already enforces it on every merge, so siting the logger there makes it unreachable from the audio thread by a lint that already runs — the strongest available NFR-RT-010 enforcement for this feature, and free. What that gives up is stated: a dependency logging through the `log` facade writes into a facade Namir has not installed, and its output is silently dropped. **D-18.3** decides the release pipeline. `xtask bundle` is the missing primitive — nothing in the ecosystem builds a macOS `.clap` bundle, and D-13.3 gains a `*Consequence*` note recording why that matters: a `.clap` on macOS is a **bundle directory**, not a renamed dylib (CLAP's `entry.h` defines `plugin_path` as the DSO on Linux/Windows but as the bundle on macOS), `docs/user-guide.md` had stated the renamed-library rule uniformly and was wrong for macOS, and FR-PKG-020 now carries the requirement explicitly instead of leaving it an unstated implication of D-13.3's table. Windows uses **Inno Setup** specifically for `{autocf}`, which resolves to `%COMMONPROGRAMFILES%` elevated and `%LOCALAPPDATA%\Programs\Common` not — D-13.3's two Windows cells from one line — with `PrivilegesRequired=lowest`; `cargo-dist` is rejected because it has no `lib-aliases` and therefore **cannot rename a cdylib**, which is the entire Windows/Linux CLAP artifact, and cannot build a macOS bundle either; `cargo-wix` because it has no per-user token, so FR-PKG-030's per-user default cannot be expressed at all; NSIS/raw WiX because Surge, Dexed, Odin2 and Cardinal each arrived at Inno independently and that convergence is the best evidence available about which path has its edge cases already found. macOS ships `.pkg`-in-`.dmg` rather than `.dmg` alone for two concrete reasons — only `pkgbuild`/`productbuild` can place multiple payloads at multiple absolute paths, and files placed by `installer` never carry `com.apple.quarantine` where zip-delivered ones do — with signing conditional on a secret being present (Surge's pattern) so the unsigned build takes the identical code path. Linux is a tarball plus `install.sh` defaulting to `~/.clap`, detecting Fedora's `/usr/lib64/clap` for the system path, and noting CLAP issue #46 (`~/.clap` vs. XDG) is still open upstream. One verification obligation is recorded rather than assumed: **Reaper must be confirmed to actually scan `%LOCALAPPDATA%\Programs\Common\CLAP`** before per-user-by-default ships, the precedent being that Dexed ships its per-user mode commented out over unresolved DAW issues, and D-13.3's own doc comment already warns this failure is silent. FR-PKG-040 also closes M7's explicitly-deferred item: the attribution file is *physically placed* in every artifact by the packaging job. **D-18.4** sets `publish = false` workspace-wide — twelve of fourteen crates are implementation details of one product (the Zed/uv shape, not a library ecosystem), `namir-clap` is a cdylib nothing can depend on, `namir-fixtures` is test tooling, and name reservation is no longer an argument since RFC 3463 prohibits placeholder crates and RFC 3646 removed crates.io team mediation for name disputes; `cargo publish` already hard-fails today because every path dep lacks a `version`, and those `version` fields go in anyway as hygiene, specifically to keep reversing this policy a one-line-per-manifest change. §17 gains its first two **prospective** rows (the `cpal` fork, High; `log`, recorded as evaluated-and-not-adopted, in the same shape as `symphonia`'s standing rejection at D-17.1) plus an explicit note that Inno Setup, `pkgbuild`/`productbuild` and `notarytool` are **build tooling, not linked dependencies**, and are absent from that register deliberately — it exists for NFR-LIC-020/030, i.e. for what is redistributed inside the binary, and a tool that runs on a CI machine and puts nothing of its own into the artifact would blur exactly that distinction. §22 gains **R-9** (A2 weight-layout re-derivation, **High** — the failure mode is a model that loads, costs what it should, and sounds plausible while being wrong; mitigated only by D-9.12's parity oracle running before any A2 model ships), **R-10** (forked-`cpal` maintenance, Medium — rebase cost plus the first non-registry source weakening NFR-SEC-040) and **R-11** (unsigned binaries, Medium — SmartScreen and Smart App Control on Windows; on macOS a quarantined *plugin* fails to load with no "Open Anyway" path at all, and macOS 15 removed the Control-click bypass, making macOS releases developer-only in practice until a signing identity exists). |
| 0.19 | 2026-08-08 | **M9's P0 decision pass — seven blockers decided in one pass, before any of the work runs.** This row records *decisions*, not results: nothing below is implemented, and each will need its own status note when the work actually happens. **D-18.5** splits NFR-QUAL-010's traceability check into two gates with different flip dates — the generated-plan-freshness half **required from M9a**, the zero-uncovered half `continue-on-error` until **M13's close-out**, with the full uncovered list printed in both modes and every uncovered id printed beside its owning milestone, an attribution the exit status never reads. The deciding fact, verified rather than assumed: `xtask traceability` returns `plan_up_to_date && coverage_clean` as one value (`xtask/src/main.rs:304`) and CI's single invocation carries one `continue-on-error: true` (`.github/workflows/ci.yml:108-120`), so deleting a coverage annotation from a currently-covered Must leaves CI green today — the regression half of NFR-QUAL-010 is enforced nowhere, the pre-commit hook included. An allowlist is **rejected in every form it can take** — a declared deferral table in `xtask`, a checked-in count permitted only to decrease, a `--strict` mode — because `docs/03-test-plan.md` is already this project's ratchet and an exemption register would duplicate it, invert its default, need a freshness gate of its own, and be the same shape as the hand-maintained table this pass exists to stop trusting; the honest limit, that the ratchet is review-visible rather than mechanically monotone, is written into the decision rather than glossed. Stated explicitly so it is not misread later: **M9 does not close NFR-QUAL-010** in either phase. **D-23.1** (new; §23 had carried a Consequence note since M7 but no numbered decision) defines what a `// trace:` tag asserts, which until now had no definition and consequently no consistency — M9's own draft work list proposed tagging FR-LIB-020 and refusing FR-NAM-140 on directly opposite reasoning about the same question. A tag now asserts the **whole** requirement **by its stated `Verify:` method**, with two mechanical questions deciding between a plain tag, a `// trace-partial:` that must carry a `// uncovered:` line naming the unspanned member and its closing milestone, and no tag at all; applied to the case that prompted it, FR-LIB-020 takes the partial, its off-the-audio-thread clause resting on a deliberately six-file corpus (`crates/namir-worker/tests/rt_stress.rs:138-149`) against a method naming 10 000. A partial counts as coverage for the ordinary run and has no path to 1.0, via D-18.5's M13 flip and D-23.2's rule that a Partial is not Done (**R-14** new, Medium). §23's M7 note gains a correction with a **live false positive** to prove it: `trace_annotations` matches the marker anywhere in a line (`xtask/src/traceability.rs:115-131`), `build_report` never verifies a test exists (`:190`) and the fn-name fallback is a whole-file substring match (`:138-141`), so string literals in the tool's own tests (`:310`, `:318`, `:331`) and its own doc comment (`:135`) put `xtask` in `docs/03-test-plan.md:70` as a component covering FR-NAM-070, which xtask does not test; a third instance is `ci.yml:109`'s prose comment parsing as a tag, there being no id-shape filter. All three holes close in the same change, and the adjacency rule explicitly admits `fn main()` in a `benches/*.rs` target — every bench here is `harness = false`, and for **all five** of the ids the existing bench tags carry, that tag is the only evidence there is. **D-23.2** gives §14's status table an evidence rule and a derived denominator before its **72 verdict cells** — 24 FRS-area rows by three columns — are adjudicated: a Must's status is adjudicated against its own text and its own `*Verify:*` method, never against whether code exists and never against the tool's verdict; **Done** needs the named artifact to exist, pass *and* be repeatable, with the evidence cited by path; **Partial** requires the gap to be *named*; a requirement quantifying over a set whose test spans part of it is **Partial** with the remainder named (FR-NAM-030, half-met since M3 while the tool reported it covered); and the tool is evidence in neither direction, per M7's own 6.3 PORT correction from 0/4/1 to 5/0/0. The derived half emits per-FRS-section Must counts into the generated plan and fails the build when §14 disagrees, riding on D-18.5's required plan-diff half from M9a. **Ground truth established: the FRS holds 130 Musts across 24 sections; §14's column sums to 117 across 22.** 5.1 CHAIN (7 vs 8) and 5.12 CLAP (10 vs 11) were both wrong on the day the table was written — verified against the FRS at commit `984b0b6`, which already held 8 and 11 — so CLAP's is an M0 counting error rather than the drift §14's own note implied, and because both rows summed internally, one CHAIN Must and one CLAP Must have never been adjudicated in any column in any version; **§4 CFG has never had a row at all**, though FR-CFG-020 is both an M8 exit-checklist item and an M9 deliverable; 5.4 NAM (+2), 6.5 LIC (+1), 6.8 DOC (+1) and a new 5.15 PKG row (+4) are FRS 0.3's arrivals. The re-audit is published as a new dated table appended below the M0 one, which gains a superseded marker but is otherwise left unedited, since six sessions' prose bullets quote its physical cell values (**R-14**, new, Medium: the three verdict columns stay hand-adjudicated under the 1.0 ship gate, and **R-13**, new, Medium: `trace-partial` as a laundering surface, unmitigated if the partial count is not falling by M12). **D-18.6** settles how a Must whose `Verify` code is not `M` gets countable evidence when it has a host, hardware or human residue: the evidence is **split** — an annotated in-process test for the automatable part, plus a `docs/manual-tests/*.md` document that is supplementary evidence and never the traced artifact — with no `Verify` code changed and the tool's dispatch untouched. The trigger was FR-CLAP-030, -040 and -100 reading as permanently uncovered while FR-CLAP-060, -090, FR-IO-060, -070 and -080 are the identical shape *with* a test as well and all five resolve. **`clack-host` 0.1.1 is adopted as a `namir-clap` dev-dependency** (§17) as the in-process vehicle for six of the seven `namir-clap` Musts M9 owes — FR-CLAP-020, the seventh, needs no host and is traced by the `clap-validator` CI step M9a adds — verified rather than presumed: `clack-extensions` 0.1.1 carries it as its own dev-dependency (`Cargo.toml:140-143`) and its `src/__doc_utils.rs:114-146` instantiates a plugin in-process via `PluginEntry::load_from_clack` with **no `unsafe`**; `AudioPortInfoWriter::from_raw` is `pub(crate)` and the `HostInfo`/`Host*Handle` `from_raw` family is `unsafe`, so no in-crate host stub D-5.3 would permit exists. Recorded honestly with it: `clack-host` shares `clack-common` with `clack-plugin`, so it is a regression detector, **not** FR-CLAP-030's second host — the same "two Rust ports agreeing" weakness already on record for FR-NAM-030's LSTM parity. Its licence is an inherited claim from a vendored manifest, not a crates.io reading, and three gates must clear before it lands, all M9a's: `cargo deny check bans`, `cargo deny check licenses` and D-18.2's network-free build, plus `cargo tree -e normal` proof that the feature does not reach the cdylib. §22's retired **R-2** gains a dated note recording that its pre-1.0-churn residual is narrowly reopened on that dev-only surface. Two alternatives are recorded and rejected: amending the `Verify` code `I → M` (permitted, since FRS §1.5 freezes identifiers and says nothing about codes, but a strict loss of evidence and against the principle **D-9.11** already settled), and teaching the tool to count a manual document for `Verify: I` (blast radius all **28** `Verify: I` Musts, **21** of which have real annotated coverage today, with no way to check an "executed" claim — and it would close none of the three, two of the documents recording "Not executed"). **D-16.5** supplies the six parameters D-16.4 left blank and reopens none of it: **4 MiB** per file, **2** retained generations (12 MiB ceiling), one UTF-8 line per record as `<timestamp> <LEVEL> <pid> <thread> <code-id> <detail>` with fields one to five space-free so `detail` needs no quoting scheme, **`NAMIR_LOG`** (`off`/`error`/`info`/`verbose`, default **`info`**), and a **synchronous** process-global writer behind one `Mutex` with **no logger thread** — the last settled by NFR-PORT-030's literal "no assumption that the process can spawn unlimited threads" (`01-functional-requirements.md:966`), which makes a logger thread the option that must justify itself. `NAMIR_LOG` is deliberately not `RUST_LOG`, a name that promises `env_logger`'s filter grammar behind a facade D-16.4 declined to install. Recorded as limitations rather than smoothed over: two processes share one file and interleave without a lock (bounded — records carry a pid, and whether Windows permits the rotation rename at all is **inferred from std's share flags, not measured**); timestamps are UTC only; and `verbose` needs a second entry point because `Severity` has no value below `Info`. §22 gains **R-12** (Low — the synchronous writer against FR-UI-060's 100 ms frame budget, unobserved; the row also records that FR-UI-060's own timed check would not detect it as written, since it renders with `scan: None`), and roadmap §15 item 8 leaves open whether the plugin configuration ever gains a persisted verbosity setting. **D-5.3** gains a `*Consequence (added M9)*` note answering a question it never covered — whether its per-crate carve-out reaches `#[cfg(test)]` modules and `benches/` files in `namir-clap`, where the crate-level lint is `deny` rather than `forbid`. **It does not, and does not need to**, established in-tree before deciding: `assert_no_alloc` and `core_affinity` need no `unsafe` of ours, there is no `unsafe` in any bench or integration test anywhere in this workspace, and of `clack`'s types `Events`, `ChannelPair`, the `from_buffer` constructors and `PluginAudioConfiguration` are all safely constructible, leaving only `Audio`/`PairedChannels` and the `HostInfo`/`Host*Handle` `from_raw` family behind an `unsafe` constructor, none of which carries Namir logic — and D-18.6's adopted host reaches those without one. The note also **retracts the reasoning first offered** for permitting the `unsafe` ("legal because the crate sets `deny`, not `forbid`"), which reads `deny` as permission: `deny` fails the build too, and only a file-level `#![allow(unsafe_code)]` compiles. No amendment, no new designated module, no FRS change. `AGENTS.md`'s "confined to one module each" and `gui.rs:68`'s miniature of the same error are corrected in M9a: `namir-platform` has carried two designated modules since M6. FRS §10 and §23 gain a paired `*Consequence*` note, deliberately **without** a decision number, correcting §10's M8-planning claim that the tool ignores workflow YAML — false, and if enforced as written would have moved the uncovered-Must count from 24 to 39 — and recording the four hard-coded scanned paths and the fifteen Musts that rest on them alone. Finally, M9 is restated **Size L in two phases**: **M9a** (ledger, tooling, docs) and **M9b** (build work), executing M9a → M10 → M11 → M12 → M13 → M9b → M8, with NFR-PERF-030 moved to M13 because it cannot run on any CI runner. The pass itself lands as **two commits**, per D-18.5's mechanism: this documents-only one, which changes no gate; then the `xtask` work, the annotations, the regenerated `docs/03-test-plan.md` and the `ci.yml` flip together, because a required plan-diff half cannot precede the flag it invokes or the plan it diffs. |
| 0.20 | 2026-08-08 | **M9a: D-23.1's adjacency rule diverges from its own text, recorded rather than silently narrowed.** The decision names six admissible following declarations; `xtask/src/traceability.rs:502-533` enforces only "the next non-blank line must exist and must not itself be a comment". The divergence is deliberate and the note appended at D-23.1 carries the enumeration behind it: the literal six-member whitelist rejects **13 of the 105 live tag sites, carrying 18 distinct requirement ids**, and a rejection is a hard error aborting the whole run — `Cargo.toml:1,37`, `deny.toml:15,82`, the three fuzz entry points' `#![no_main]`, `namir-fixtures/src/lib.rs:16`, `namir-ui/src/app.rs:133` and the four `xtask/src/*.rs` file-level tags. **Seventeen of the eighteen ids are `Verify: S`** (the exception, FR-CFG-030, is `Verify: I` and rides on `xtask/src/layering.rs:40` because the layering lint is what checks it — recorded in the note as a small instance of the code-versus-evidence gap D-23.1's first question exists to catch), and for each the rejected site is the only evidence there is, which is the structural point: a static or build-time check's tag sits above a manifest key or a lint attribute, never above a `#[test]`, so the six-member list is test-shaped and cannot describe the one `Verify` code that never has a test. Same near-miss as the `fn main()` case D-23.1 caught explicitly, one category wider. Two corrections ride with it. D-23.1's claim that "the adjacency requirement is what stops prose that merely names the marker from parsing as a tag" is **wrong**: adjacency closes none of the three false positives §23's M9 note records — rule 1 (the marker must *begin* the line) kills the string-literal class and `ci.yml:109`, the tightened `fn_name_embeds_id` kills the fn-name class, and the tool's own tests name both classes. What adjacency actually buys is a fourth class none of those reach — a well-formed tag that has drifted from its artifact, sits inside a prose block, or is stranded at end of file. And what it does not buy is stated rather than implied: a tag whose next non-blank line is a bare `}`, a `#[derive(Debug)]` or a `struct S;` is accepted. Per-file-type narrowing is left unattempted at M9a, since every shape it would enumerate is already in-tree and the whitelist would transcribe today's sites rather than state a rule. No decision text edited, no new decision number, no change to what a tag asserts. |
| 0.21 | 2026-08-08 | **M9a: `clack-host` lands and D-18.6's three landing gates are adjudicated — the third is cleared for a narrower configuration than the gate was written about.** Gates 1 and 2 pass, and non-vacuously, which is the part worth checking rather than assuming: `cargo deny check bans` and `cargo deny check licenses` are both green with the dev-dependency in `Cargo.lock` (`:311-319`, exactly one new package), and cargo-deny's `-L debug` inclusion graph shows ``clack-host v0.1.1 └── (dev) namir-clap v0.1.0``, so the dev edge is genuinely evaluated rather than skipped; D-18.2's `network-free` job is that same `check bans` command (`.github/workflows/ci.yml:172-180`), discharged by the same run, with the Windows-host/`ubuntu-latest` difference stated and shown not to reach this crate. **§17's licence caveat is discharged in the same pass** — no longer "an inherited claim… read from `clack-extensions`' own vendored manifest" but `clack-host` 0.1.1's own crates.io-published manifest, `license = "MIT OR Apache-2.0"` at its `Cargo.toml:37`, `LICENSE-MIT` and `LICENSE-APACHE` both in the `.crate`. Gate 3 splits: `cargo tree -e normal` contains no `clack-host` node anywhere and `xtask attribution` reports the notices file up to date — but **the configuration that gate is worded about, `clack-extensions`' own `clack-host` feature enabled for the test target, is not the configuration that landed, and in it the attribution half fails.** The feature is deliberately off (`crates/namir-clap/Cargo.toml:93-105`); the dependency is taken directly with `default-features = false, features = ["clack-plugin"]`, which is all `PluginEntry::load_from_clack` needs and is why it resolves one package and no transitive ones. §17's "reverts to prospective" clause is therefore **not** triggered, and M9b inherits the wider configuration as a named blocker rather than a discovery. **New R-15 (Low)** records the mechanism behind that failure as a standing property of the tool, not an incident: `xtask attribution` walks `cargo metadata`'s single unified resolve and keeps any edge carrying a `Normal` `dep_kinds` entry (`xtask/src/cargo_meta.rs:82-91`), so **any** dev-dependency edge enabling an *optional normal* dependency of a crate the shipped graph already reaches puts that dependency into THIRD-PARTY-NOTICES.md. Low because the error direction is over- rather than under-attribution — the walk cannot omit a shipped crate, so NFR-LIC-030 is never under-served — and because the detector already exists: `cargo tree -e normal` disagreeing with `xtask attribution` is exactly this condition, which is why D-18.6's third gate names both commands rather than either alone. Roadmap §15 **item 11 is struck** in the same pass: `clap-validator`'s supply-chain shape is decided as a commit pin (`--rev b2f1d9b…`, `--locked`, from `https://github.com/free-audio/clap-validator`), vendoring a built binary and a floating install both recorded rejected, with the record kept in the CI job's own supply-chain comment rather than in §17 — on that register's own titled instruction that build tooling putting nothing into the shipped artifact does not belong in it. |
| 0.22 | 2026-08-09 | **M9a: the set-quantification sweep's two document findings — D-9.8's conflict with FR-CHAIN-010 resolved after standing since the first commit, and D-23.1's own worked example corrected as understated.** Both were found by *reading* a requirement's text beside its artifact to answer D-23.1's two questions, not by any gate, and are recorded together because they are the same failure class one step apart. **D-9.8 gains the review its Rationale flagged and never got.** That Rationale's opening clause — "not specified by the FRS" — is **false and always was**: FR-CHAIN-010 mandates `input → input trim → noise gate → NAM → IR → EQ → output level → output` (`01-functional-requirements.md:163-166`) while `build_default_chain` ships `gate → trim → nam → ir → eq → out` (`crates/namir-engine/src/stages/mod.rs:47-67`), and both texts landed in the same first commit `875068e`, so the contradiction is exactly as old as the two documents. **Resolved in D-9.8's favour: the FRS is amended to the shipped order** in this same pass — a gate whose threshold references the interface's real noise floor rather than walking under the user's trim hand is the product this project decided to build, and the governing document described a different one. Deliberately a *different route* from the one D-9.11 and D-23.1 took for NFR-QUAL-030 and NFR-QUAL-010, which left the FRS's text standing and recorded only the route to satisfying it: no chain satisfies both orders at once, so one document had to move, and it is the FRS's owner amending it rather than this document editing it (§1's authority order unchanged). Recorded honestly rather than as a clean resolution: it survived M2 building the chain, every milestone since shipping it, **AQ-2's own 2026-08-04 confirmation** — which confirmed the usability argument in isolation, never against FR-CHAIN-010 — and M9's P0 pass; and **it was never hidden**, `stages/mod.rs:31-36` having stated the divergence in plain prose since M2's `7941577`, naming both D-9.8 and FR-CHAIN-010's "literal prose order", with every reader since reading past it. Visible and unresolved for seven milestones, not concealed for seven. Nothing mechanical detects a requirement-versus-code contradiction of this kind and nothing was going to: `xtask traceability` asks whether an artifact references an identifier, never whether it agrees with the requirement, so FR-CHAIN-010 read covered throughout and was correct by its own rules throughout; `layering`, `params-lock` and `attribution` read dependency edges, a parameter manifest and licence metadata, none of which is requirement prose. **D-23.1's Rationale is corrected on FR-NAM-030.** It calls the requirement half-met ("only WaveNet was ever compared against `NeuralAmpModelerCore`"), and D-23.2's Rationale, its clause 4, changelog 0.19, `03-implementation-roadmap.md:2168-2173` and `AGENTS.md:281` all repeat it. **No in-tree runnable artifact compares *either* architecture against that implementation.** Both parity tests call `namir-fixtures`' own from-scratch Rust ports — `crates/namir-nam/tests/fixtures.rs:129` → `nam::reference_infer` (`crates/namir-fixtures/src/nam/mod.rs:95`), `lstm_fixtures.rs:120` → `reference_infer_lstm` (`mod.rs:142`) — which is exactly what D-23.1's **own question 2** forbids for a `Verify: G` ("the named external reference, not a second in-house implementation"): the decision wrote down the test that catches this and then did not apply it to the example it chose to prove the point with. S-1's real **-131 dB** comparison is **not retracted** and is **not this requirement's evidence**, on three counts: it lives in `spikes/`, `exclude`d from the workspace (`Cargo.toml:15-20`) with its own lockfile and re-run by nothing, failing D-23.2 clause 1's repeatability test; it measured the *spike's* engine, while the shipped `wavenet.rs` has been vectorized with `wide::f32x8` since M3 (changelog 0.10; `wavenet.rs:35`) with no re-measurement, in precisely the arithmetic-ordering dimension a parity figure is sensitive to; and a second, independent clause is unmet for both architectures — FR-NAM-030 names "a specified 10-second test signal containing clean, transient and saturated material" (`01-functional-requirements.md:373-376`) against 4 000 samples of 110 Hz sine plus noise, about 83 ms (`fixtures.rs:20-31`, `:134`; `lstm_fixtures.rs:126`). So the worked example was understated in the direction that **strengthens** D-23.1, and the sweep's two `// trace-partial: FR-NAM-030` pairs (`fixtures.rs:120-126`, `lstm_fixtures.rs:112-118`) name the same gap for both architectures rather than one naming LSTM. Recorded with it, because the parity tests remain good evidence of something else: `namir-fixtures` takes no `namir-nam` dependency (`crates/namir-fixtures/Cargo.toml:11-17`), so the two ports are genuinely independent *code* — but not independent *readings*, both descending from `spikes/s1-nam-inference/src/lib.rs`'s reading of the C++ (`infer.rs:9-11`, `namir-nam/src/lib.rs:20-24`), so a misreading there reproduces in both and parity still passes; the "two Rust ports agreeing" weakness already on record for `clack-host`, here with a *shared ancestor* rather than a shared crate. They therefore stay plain-tagged for NFR-QUAL-030 (`fixtures.rs:128`) and partial for FR-NAM-030, which still closes at **M10**. No text above is rewritten and no decision changes; the roadmap and `AGENTS.md` carried the same understatement and are corrected in their own passes in this commit. |
| 0.23 | 2026-08-09 | **M10 built: core NAM Architecture 2 support, FR-NAM-030 closed for real against the actual reference, and FR-NAM-090.** Three stacked PRs. FR-NAM-140 closes first (Phase 0): `crates/namir-nam`'s `WaveNetConfig`/`LayerArrayConfig` widen in place (D-9.12: one grammar, not a second parse path) to recognise every A2 field, rejecting each not-yet-or-never-supported one by name (`nam.load.unsupported_configuration`/`nam.load.inconsistent_configuration`) instead of the previously-misleading `nam.load.malformed_json`; closes a live NFR-SEC-020 gap found while scoping the rest (an unbounded dilation could force a multi-gigabyte allocation from a handful of declared weights). Core A2 lands next (Phases 1-3, FR-NAM-150): `bottleneck` threading, a redesigned per-layer `Activation` (`LeakyReLU`/`SiLU`/`Hardswish`/`Softsign`/`LeakyHardtanh`/`PReLU`, up from A1's four), and `Conv1D`/head-rechannel unification, with **R-9 retired** — see §22's row for the full independent-double-implementation-plus-real-reference-cross-check record. **FR-NAM-030 (`Verify: G`) closes for both architectures, in-repo, for the first time**, correcting D-23.1's own worked example above from "closes at M10" to actually closed: `crates/namir-nam/tests/golden_reference.rs` commits small, generated, regenerable fixtures rendered through a real `NeuralAmpModelerCore` build (pinned `3cde95c`, `-DNAM_USE_INLINE_GEMM`) and asserts against them in-process — WaveNet to -137 dB, LSTM to the bit once the reference's own default silent-prewarm behaviour is matched (a real, previously-undocumented finding: `NAM/lstm.cpp`'s `GetPrewarmSamples` runs 0.5 s of silence through the model before real audio by default, a host convenience the reference DSP wrapper applies, not part of the LSTM model's mathematical definition — reproduced in the test, not in `namir-nam`'s production `LstmState` initialisation). Corroborated against seven real, locally-held LSTM models via the new `xtask nam-parity` tool (-114 to -130 dB, no prewarm treatment needed), recorded in `docs/manual-tests/fr-nam-020-real-lstm-models.md`. The two `// trace-partial: FR-NAM-030` sites changelog 0.22 found now carry a note pointing to the new tests instead; the golden-reference tests themselves are tagged plain `// trace: FR-NAM-030`, split across both (D-23.1: the pair jointly spans "each supported architecture," neither alone does). **FR-NAM-090 closes as `trace-partial:`**: `namir-nam` parses `metadata.loudness`; `namir-params` gains `nam.normalize_enabled`/`nam.normalize_offset_db`; `namir-engine`'s Nam stage applies a smoothed, allocation-free correction toward a new `TARGET_LOUDNESS_LUFS = -18.0` reference point; the covering test substitutes plain RMS-in-dB for the `Verify: U` method's named "integrated loudness" (no ITU-R BS.1770 meter exists in this codebase), which is why the tag is partial rather than plain. **FR-NAM-100 was not built** and stays Not started — a distinct, Should-priority requirement this milestone did not scope. A new LSTM cost curve (`crates/namir-nam/benches/lstm_inner_loops.rs`) is a genuine, uncomfortable finding, not acted on this milestone: several real shapes at the larger end of the 67-model grid breach NFR-PERF-010's 25% budget. §14's `### M9a re-audit` table moves four cells (5.4 NAM: 3/8/2 → 6/7/0; Total: 29/92/9 → 32/91/7), evidence appended beneath the table per D-23.2. |
| 0.24 | 2026-08-11 | **M11 built: FR-IO-020's WASAPI exclusive mode ships, on a `cpal` fork that is real rather than prospective — and two of its three findings are about what automated checks cannot see.** D-13.4's fork is adopted and pinned (`git+https://github.com/ErwanLegrand/cpal`, branch `wasapi-exclusive-mode`, `rev = 2edbacb4`), with the narrow `allow-git` allowance the decision required in `deny.toml` and a new `cargo deny check sources` step in CI's `license-audit` job — a sub-check nothing in CI had been running, so the allowance was until now enforced by no gate. §17's fork row loses its "Prospective" label and the upstream `cpal` row is marked superseded, per that fork row's own "replaces, rather than supplements" wording. The fork's public surface is `cpal::platform::{ShareMode, WasapiStreamOptions, WasapiDeviceExt}` — a `#[non_exhaustive]` options struct plus an extension trait over `DeviceTrait`'s queries *and* builders — re-exported with **no `cfg`**, because a `cfg`-gated surface would have been unusable here: `namir-app` must name the types, D-5.1 confines platform attributes to `namir-platform`, and `namir-platform` may depend on `namir-core` alone, so it cannot wrap `cpal` on `namir-app`'s behalf. A new `HostId` (PR #843's shape) and a field on the shared `StreamConfig` (PR #1195's shape) were both rejected against that constraint. Namir's side: `ShareMode` on `StreamParams`, `AudioBackend::supports_exclusive` asking the device rather than answering from a constant, the first read of `AppSettings::exclusive_mode` since M6 added it forward-compatibly, an all-or-nothing rule ANDing both devices' answers (roadmap §18 rules out "a mode indicator that lies"), the catalogued `app.audio_io.exclusive_mode_unavailable` warning, an `I32`/`I24` sample-format converter with `I16` deliberately excluded rather than shipping undithered truncation, and one `UiSnapshot` field rendering the mode actually granted. **The two findings worth carrying forward.** First, **two real defects were found only by running on hardware, after every automated check had passed** — `dwChannelMask = 0`, which shared mode's engine accepts and a PreSonus AudioBox 22VSL refuses (fixed in `ab5f40a`), and 24-bit container justification, where WASAPI left-justifies and `dasp_sample::I24` right-aligns, a factor of 2^8 in both directions (fixed in `2edbacb`). In both cases the arithmetic was correct and unit-tested at its boundaries, the workspace's 913 tests passed and CI was green on three platforms; the disagreement was about the container convention *around* correct arithmetic, and both sides of that boundary agreed with themselves. That is the concrete argument for FR-IO-020's `Verify: M` and for D-18.6's manual-document rule, recorded at D-13.4 rather than left in the manual-test file alone. Second, **the fork's diff is far larger than D-13.4's "deliberately kept minimal" instruction anticipated**: seven commits, 2867 insertions / 144 deletions across 11 files, with `src/host/wasapi/device.rs` (546/130) certain to conflict on a rebase — structural, since a per-call extension trait threads the share mode through every enumeration and build path. R-10 gains a status note carrying that corrected burden, and correcting the exit in the other direction: PR #843 was abandoned by its own author and never reviewed by a maintainer, and upstream issue #1220 lists exclusive mode under "work out extension traits", so the upstreaming exit D-13.4 states is real and lacks a date and an agreed API rather than upstream willingness. No requirement's verdict in `docs/03-implementation-roadmap.md` §14 is moved by this document; the roadmap's own M11 close-out is a separate pass. |
| 0.25 | 2026-08-10 | **M12 built: product identity — a README, a stated trademark position, and the brand mark in the interface. Run before M11, which its own header permits ("Depends on: nothing technical") and which M11's then-unbuilt `cpal` fork made the sensible order.** *(This row carries a later version number than its date because M11 reached trunk first and M12 was rebased onto it; the two milestones touched disjoint requirements, and only §14's Total row — which both write — needed recomputing, to 34 / 91 / 5.)* **D-17.3** resolves roadmap §15 item 7 against admitting a build script to a shipped crate: the `libc` exception is earned by FFI-layout soundness and an icon cannot borrow that, so the Windows `.exe` icon moves to M13's pipeline. Its second consequence note records a finding that changes more than the decision did — **`baseview` 0.2.2 has no icon field at all** (`WindowOpenOptions` is `#[non_exhaustive]` with `title`, `size`, `scale` and an `opengl`-gated `gl_config`), so §19's instruction to set the window icon "through baseview's own window options" is unfollowable and **both** icon clauses defer to M13 together, not just the executable one. FR-UI-110 therefore closes only its brand-mark clause and stays open; the FRS carries an appended `*Consequence*` note saying so. **The mark ships without any image decoder entering either product**: the artwork is a single fill on a transparent background, which makes an 8-bit alpha mask a **colour**-lossless re-encoding (the 1767x474 → 358x96 downsample is a separate, deliberate loss), so `xtask identity` decodes `images/namir.png` with an xtask-only `png` dependency (new §17 row; `cargo tree -e normal` reaches it from neither product and `xtask attribution` is unchanged) and emits a 34 376-byte blob `namir-ui` `include_bytes!`s and tints at upload. Generation is integer-only so the blob cannot depend on which machine ran `--write` — **not**, as the code's own rationale first claimed and a review corrected, because CI compares it on three runners: the `identity` step is `ubuntu-latest` only. **NFR-LIC-070 closes (plain tag); NFR-DOC-040 lands `trace-partial:`** — a substring check cannot reach "stating what it does" — so §14's re-audit table moves two cells, 6.5 LIC to 3/3/0 and 6.8 DOC to 1/2/0, Total 32/91/7 → 33/92/5, evidence appended beneath the table per D-23.2. **One correction against this milestone's own interest:** `ci.yml`'s NFR-BUILD-020 partial declared `closes M12`, and M12 pins the README's commands without executing any of them as documented, so that field is rewritten and moved to M13 rather than counted as met. Two things this milestone could not verify and does not claim: no display exists in the build environment — `baseview` panics on X11 open even under `xvfb-run` — so **the mark has never been seen rendered**, and no host was available for the plugin shell; `docs/manual-tests/fr-ui-110-brand-mark.md` records both as unexecuted and hands them to M13, which needs the same Windows machine anyway. |
| 0.26 | 2026-08-11 | **M12's manual test executed on Windows: FR-UI-110's brand-mark clause is now observed rather than argued, and step 3 found the mark too small in both shells.** Steps 1, 2 and 4 pass — the mark renders in the standalone window and in the CLAP plugin inside a host, and its accessible name still reads "Namir", so FR-UI-030 did not regress when an image replaced a text heading. Step 3's size verdict is a real defect fixed here: the mark was drawn at one `TextStyle::Heading` row, ~25 logical pixels, and `brand::MARK_HEIGHT_IN_HEADINGS` now doubles it. **Two is the largest free factor and the margin is now spent** — the blob is 96 rows, so ~50 logical pixels is ~100 physical at 2x HiDPI against 96 stored, effectively 1:1 and the sharpest this asset can be drawn; `MARK_TARGET_HEIGHT`'s doc comment is rewritten from claiming a ~2x margin to recording that any further size increase must raise it in the same change. Step 3's remaining half — legible rather than aliased at 1x and at HiDPI — is **still unobserved**, and two reasoned but unseen fixes ride on it: the mipmapping added when a review found ~4-5x minification with `mipmap_mode: None`, and this change, which halves that minification. FR-UI-110 stays open on that half plus both icon clauses (D-17.3, M13); it is a Should, so no §14 cell moves. The status subsection's "never been seen" paragraph is superseded by a dated addendum rather than rewritten. |
| 0.27 | 2026-08-11 | **FR-UI-110's brand-mark clause closes: step 3's legibility half passes, and with it the two fixes that were riding on it unobserved.** The enlarged mark was inspected and reads fine at the drawn size, which is the only way the mipmapping (added after a review found ~4-5x minification with `mipmap_mode: None`) and 0.25's size increase could ever have been confirmed — both were reasoned from the pinned sources and neither had been looked at. All four steps of `docs/manual-tests/fr-ui-110-brand-mark.md` now pass. **The requirement stays open on its two icon clauses only**, deferred to M13 by D-17.3; it is a Should, so no §14 cell moves. This leaves §19's Acceptance met on four of five clauses — `images/` tracked, README followed on a clean clone (though not a cold machine), mark rendering in both shells, brand terms stated — with the icon clause the one deliberate exception. Recorded by appending to the roadmap's M12 addendum rather than rewriting the paragraph that called the half unobserved. |
| 0.28 | 2026-08-11 | **M13 wave 0: three roadmap decisions settled before any packaging code, and one of them is a correction to D-18.5 itself.** **D-18.5's zero-uncovered flip moves from M13's close-out to M9b's**, recorded as a dated consequence note at the decision and mirrored at `03-implementation-roadmap.md` §16's restated acceptance and §20's scope note. The decision's rationale counted the ten uncovered Musts owned by M10, M12 and M13 and concluded M13 was where the list ran out; it did not carry the other fourteen, which are M9's own, which §16 assigns to **M9b**, and which §16 then orders *after* M13. Measured rather than inferred: 15 `**UNRESOLVED**` Must rows at M13's start, five M13's and ten M9b's, so deleting `--allow-uncovered` here would make a required check red on the day it became required and keep it red until M9b — the failure mode the split exists to prevent. NFR-QUAL-010 therefore closes at **M9b**. Nothing else in D-18.5 changes. **§15 item 10 resolved**: FR-PKG-010 closes by an in-repo `xtask` assertion over `release.yml`, and `xtask traceability`'s hard-coded scan set is left alone — a tag in a workflow asserts nothing a reader can check, a test runs on every pull request where the workflow runs only on a tag, and M13 adds three more packaging files that would each reopen the question if the precedent were set. **§15 item 15 resolved and built**: `build_report`'s `'M'` content arm now reads the document's `**Requirement (literal…):**` paragraph (`declared_requirement_ids`) instead of the whole file. Three things the item did not predict — all 22 manual-test documents already carry the line, so its costed backfill was unnecessary; the block wraps across lines, and a single-line read would have dropped the tree's one legitimate multi-requirement document while keeping every false positive; and there was a **third** false positive, FR-UI-070 resolving to `fr-ui-010-standalone-window-renders.md` on the words "one FR-UI-070 notice" in a canned snapshot. Two plan rows move to `**UNRESOLVED**` and the informational count goes 15 → 17; no §14 cell moves, M9a having already adjudicated both **Partial** by hand. The filename arm's own weakness — a document recording "not executed" credits identically to one recording a pass — is explicitly **not** fixed. **§15 item 17 resolved**, three milestones past its deadline: FR-CFG-030 and NFR-LIC-030 are adopted into M13 (the latter contradicting §14's M7-session bullet, which closed the "produced by the build" clause and could not have closed "shipped with the binaries" when there were no binaries), and FR-IO-070 stays M9b's because M11 ran and declined it. |
| 0.29 | 2026-08-11 | **M13 built: Namir has installers. `xtask bundle`, three per-OS packagers, a tag-triggered `release.yml`, D-18.4 applied, NFR-PERF-030 closed to a benchmark that asserts, and FR-UI-110's executable icon — with four corrections to decisions this milestone was supposed to merely execute.** **`xtask bundle`** is the primitive everything else consumes: a **pure, host-independent** `plan(Platform) -> Layout`, so the macOS bundle's assertion is an ordinary test on every runner rather than a check only a macOS release job runs. That purity is what makes FR-PKG-020's tag plain — the test materialises all three platforms from a fake dylib, checks the *produced* tree, and asserts both negative cases (a plain file named `Namir.clap` on macOS, a *directory* named `Namir.clap` on Windows). `Info.plist` is deterministic text and `PkgInfo` its eight literal bytes, so no cargo dependency and no §17 row. **The macOS layout was wrong when first built, and the packaging lane caught it:** it staged the standalone as a bare Mach-O, and an unbundled process cannot declare `NSMicrophoneUsageDescription`, which macOS 10.14+ requires before opening an audio *input* device — for an instrument amplifier simulator that is the whole product, so `Namir.app` is now staged too, with `LSMinimumSystemVersion` **derived** (CI's macOS leg is `macos-latest`, hence `aarch64-apple-darwin`, whose floor is 11.0) rather than guessed. **Two errors in D-18.3's Windows paragraph, each of which would have failed FR-PKG-030 alone**, both recorded as consequence notes there: `PrivilegesRequired=lowest` *alone* offers one scope, not two, without `PrivilegesRequiredOverridesAllowed`; and `{autocf}` when elevated is the **32-bit** Common Files directory unless `ArchitecturesInstallIn64BitMode=x64compatible` is set, so "both cells from one line" is false and the failure would have been the silent one D-13.3 exists to warn about. **A third correction at D-13.3**: the Linux system-wide cell `/usr/lib/clap` is too narrow for multilib distributions, the installer probes for `/usr/lib64/clap`, and `clap_paths.rs` is deliberately *not* taught to probe — it is a pure no-I/O path computation and trading that away for one platform's edge case is the larger loss — with the cost named (FR-ERR-050's diagnostic would look at the wrong directory; **M9b** owns it). **A fourth at D-17.3**: `images/namir.ico` is generated and freshness-gated like the brand-mark blob, but the window-icon clause **cannot close through the pinned stack** — M12 left "does 0.3.0 have an icon field" unchecked and the answer is that **no published `baseview` has ever exposed an icon on any backend**, with published `egui-baseview` 0.6.0 requiring 0.2.2 so the pin is forced rather than conservative; §17's `baseview` row and the FRS both carry the answer. A contrast-rescale for the 16×16 icon was written, measured, found to gain 1.05× against an already-243/255 peak alpha, and **deleted**, with a test pinning the number. **NFR-PERF-030** gets a seam whose environment variable *is* the config directory it uses, so "warm library index" becomes a precondition the harness establishes and a measurement provably cannot touch the user's real config: **440–486 ms against a 3000 ms ceiling**, 25 launches over 4 runs on §2's machine, **informational not certified** since the machine was not verified quiet. Its tag is `trace-partial` on a real residue — "audible" is `play()` returning `Ok`, so a build that started its streams and produced silence would still time as audible; the stronger marker was rejected rather than glossed, being a measurement-only atomic on the most-reviewed path in the project. **FR-PKG-010** likewise lands `trace-partial`: `xtask/src/release_workflow.rs` parses a YAML subset in-repo (no dependency, hence no §17 row) and asserts the method's three clauses separately, proven by twelve mutation tests, but the requirement's own verb is *produce* and no tagged run has happened. **D-18.4 applied**: `publish = false` inherited from `[workspace.package]`, 59 path dependencies given `version`, and the predicted substitution checked rather than assumed — `cargo publish` now stops on the policy instead of on a versionless path dependency, with `Cargo.lock` unchanged. **R-16 is new** (baseview is X11+GLX only, no Wayland backend in any version — pre-existing, but M13 is where it stops being a developer's problem and becomes a user's), and **R-11 is restated**: the signing-conditional structure is built and correct, but "adding a secret" understated the cost, since `codesign` needs a certificate imported into a keychain and that is unbuilt, so the signed macOS path is unreachable from CI even with identities set. **Nothing in this milestone has been executed end to end**: no `iscc`, no `pkgbuild`, no `install.sh`, no tagged workflow run. Every packaging README says so of itself, `docs/manual-tests/fr-pkg-030-windows-install-scope.md` records every step NOT EXECUTED, and D-18.3's standing precondition — that a real host be *observed* to scan `%LOCALAPPDATA%\Programs\Common\CLAP` — is still outstanding. |
| 0.30 | 2026-08-12 | **M13's Windows manual run passed, discharging D-18.3's standing precondition — and found a bug in a requirement it was not testing.** `packaging/windows/namir.iss` compiled on the first attempt with no edit, on **Inno Setup 7.0.2** rather than the 6.3 every M13 document states as the floor; that floor is unchanged and still correct but is now demonstrably **stated and untested**. All five steps of `docs/manual-tests/fr-pkg-030-windows-install-scope.md` pass, so **FR-PKG-030 moves Partial → Done** (§14's 5.15 PKG row to 2 / 2 / 0, Total to 36 / 93 / 1). **Step 0 is the consequential one:** D-18.3's requirement that a real host be *observed* to scan `%LOCALAPPDATA%\Programs\Common\CLAP` had been outstanding since that decision was written, and the only prior evidence was spike S-4's **negative** result about `%APPDATA%\REAPER\UserPlugins\CLAP` — a negative about one path being no confirmation of another. The per-user default is now known-good, and Dexed's precedent (per-user mode shipped commented out) does not apply. Step 3 separately confirms the elevated install lands in `C:\Program Files\Common Files\CLAP` and not the `(x86)` directory, which is the one result `ArchitecturesInstallIn64BitMode=x64compatible` could not have earned by argument. Step 2.1 was re-confirmed on its own rather than folded into a general pass, since that preselection literally *is* "shall default to per-user": asked specifically, the runner confirmed the installer defaults to per-user. One weakness in the record, stated rather than hidden — the **verbatim** wording of that preselected option is still not captured, so a future reader cannot tell from the document what they should see on screen. **The run also exposed an FR-UI-020 defect**, unrelated to packaging: `namir-clap`'s `drain_meters` accumulated the output peak into a **struct field** rather than a local, making it a maximum over the plugin instance's lifetime, so the output meter read full and never moved while the level control worked normally. Unchanged since M6 and never executed by any test, every test in that module having passed `telemetry: None`; `namir-app`'s `read_meters` has always had the correct shape, which is why the standalone was unaffected. Fixed and verified in a host, with five tests of which two fail against the old code. **The coincidence is recorded rather than smoothed over:** FR-UI-020 has no manual-test document of its own and was, until M13's §15 item 15 fix, credited to a CLAP audio-ports script on a single parenthesis about watching a meter — so the requirement whose only evidence was a passing mention of a meter had a broken meter, in the shell whose document was crediting it. It stays `**UNRESOLVED**`; writing a document now to absorb the observation would be creating one to move a number. |
