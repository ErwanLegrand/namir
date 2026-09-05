# DRAFT — not committed into `docs/`

This file holds the `docs/02-architecture.md` §19 findings entry and the §22 risk-register rows
that S-5 would produce, written in the exact shape those sections use. **They live here, in the
spike, on purpose.** S-5's own spec forbids it from adding a decision, a requirement, a §14 row
or a CI gate, and M14 is still in flight; these paragraphs move into `docs/` only if and when a
phase-(b) decision is taken, and whoever takes that decision should re-read them against the
tree as it is then, not paste them unread.

---

## §19 entry, as drafted

### S-5 — Namir's DSP chain in the browser: WASM + Web Audio

**Question:** Can the existing six-stage DSP chain — unmodified, compiled to
`wasm32-unknown-unknown` — hold a Web Audio deadline in a desktop browser, and at what cost
relative to the native build on the same machine? Three sub-questions, each with its own gate:
compute, scheduling, and the round-trip latency Chromium on Windows actually delivers for
`getUserMedia` -> `AudioWorklet` -> output, which is unmeasured in public sources.

**Method:** a spike crate with path dependencies on `crates/namir-*` (a stated divergence from
S-1..S-4's vendoring convention, D-S5.1: those asked whether an implementation was possible, this
asks whether the code that exists ports). One shared harness compiled to both native and wasm,
differing only in the clock. Two wasm artefacts (scalar, `+simd128`), an output-parity gate that
must pass before any timing figure is reported, a real `AudioWorkletProcessor` against real
hardware for scheduling, and a `currentFrame`-differencing page for latency. Generated fixtures
throughout, per D-19.1.

**Produces:** a browser-vs-native cost ratio for the assembled chain; a scheduling verdict on a
real device; the first measured statement about Chromium's Windows audio latency for this path;
and the evidence for a later go/no-go on a try-before-download demo page.

**Result — 2026-09-05. PARTIAL — two gates PASS, the third answers "no" for live input.** Spike
at `spikes/s5-wasm-web-audio/`.

**Key finding, the chain ports and computes with room to spare, and the browser's latency is
what stops a live-input demo, not the DSP:** all six crates build for `wasm32-unknown-unknown`
with **no edits to anything under `crates/`**, and the assembled chain holds a 128-frame Web
Audio quantum on a real device with zero underruns in every steady-state second across 60 s and
300 s runs. Compute passes at roughly 1.8x native, **conditional on WebAssembly SIMD** — the
scalar artefact fails the compute gate outright for the larger model, and `rustfft`'s wasm SIMD
feature has no runtime detection, so the two are different artefacts rather than one with a
fallback. What does not pass is latency: Chromium's own accounting is 62 ms best case, roughly
twice the ~30 ms soft reference, before anything the API does not account for.

| Condition (A1 Standard, 128-frame block, % of 2 666.67 us) | p50 | p99.9 | estimator |
|---|---|---|---|
| native, x86-64-v3, steady | 6.30–6.65 | 13.60–14.69 | — |
| Edge 152 / wasm scalar, steady | 38.63–39.00 | **74.44–85.88** | 50.25–50.44 |
| Edge 152 / wasm simd128, steady (20 000 blocks) | 11.44–11.81 | 28.88–30.94 | 18.00–18.37 |
| Edge 152 / wasm simd128, steady (100 000 blocks, sustained) | 12.94 | **32.44–33.56** | ~18 |
| Edge 152 / wasm simd128, subnormal tail | 17.44 | **44.25–58.13** | — |
| Edge 152 / wasm simd128, A2 Lite, steady (sustained) | 3.00–3.19 | 14.81–16.31 | 9.75–9.94 |
| Edge 152 / wasm simd128, A2 Lite, subnormal tail | 3.75 | 19.41 | — |

Gate 2, on a PreSonus AudioBox 22VSL at 48 kHz through Chromium's real audio service: **zero
underruns in every steady-state second of every run**, both signal regimes, three build/model
cells, over 60 s (22 500 blocks) and 300 s (112 500 blocks) — including 123 750 subnormal-tail
blocks, the regime the compute gate flagged as the risk. The literal "every run zero" reading is
missed by a single dropped device callback in the *first second* of about a third of runs; a
three-arm, 36-run experiment (baseline / one-shot handover guard moved 10x later / chain not
driven at all for two seconds) reproduced it at the same rate and the same second in all three
arms, so it is Chromium/WASAPI stream start-up rather than this chain. An earlier attribution to
the spike's own guard is retracted in place in the spike's log.

Gate 3, API-reported: 10 ms `baseLatency` + 42 ms `outputLatency` + a *declared* 10 ms input
constant Chromium reports as fixed and uninfluenceable = **62 ms**. `--enable-exclusive-audio`,
which the spec listed as the unmeasured low-latency condition, is a **2.6x regression** — the
render quantum halves to 5.33 ms and the device buffer balloons from 42 ms to 128 ms. The
physical loopback measurement, the one novel number this gate existed to produce, is **PENDING
RUN**: there is no cable on the machine, and a loopback figure can only exceed the API's, never
undercut it.

**Sub-finding, recorded because it closes a question rather than opening one:** 256-bit SIMD on
wasm is not reachable. V8's experimental revectorizer provably runs on this artefact and packs
nothing — 16 functions visited, 14 `Build tree failed!`, 9 `Empty seed`, verified against a
negative control — and the cause was isolated to LLVM's address-arithmetic form: `wide::f32x8`
*is* two adjacent `v128` ops on wasm, so the right instruction pairs reach the pass, but V8's
seeder needs both stores off one shared address local with folded `offset=` immediates and LLVM's
strength reduction restores a recomputed base every time. Four rewrites of the kernel failed
identically. Not an upstream `wide` or `rustfft` limitation and not a V8 one; simply not
something Rust gives a lever over. **The wasm performance story is `+simd128`, which works.** See
`spikes/s5-wasm-web-audio/REVECTORIZE.md`, and `spikes/s5-wasm-web-audio/revec-probe/` for the
probe crate itself — committed because the conclusion depends on LLVM's codegen and V8's seeder
both staying as they are, and is therefore re-checkable after a toolchain bump.

**Sub-finding, denormals:** wasm mandates IEEE-754 subnormal handling and offers no
flush-to-zero, so `DenormalGuard` is a no-op there. The kill criterion (>2x penalty) did not fire
— worst 1.57x — but natively FTZ/DAZ removes essentially the whole penalty (1.39x -> 1.00x), and
that saving is unavailable in a browser at any price. A1 Standard's compute margin under ordinary
silence between notes is therefore a hairline, not the 1.5x the steady-signal row implies. A
related ruling in the spike's own planning — that an earlier amplitude-decay penalty was *not* a
denormal effect — was overturned by measurement and is retracted in place.

**Scope note, recorded rather than silently narrowed:** the spike is single-threaded and never
exercises D-8.1's handover, so live model switching in a browser is unproven. `namir-worker`,
`namir-platform`, `namir-ui`, the library and preset persistence are all out of scope by design.
The laptop axis was never run, and Gate 3's loopback half was never run.

**Browser scope note, which bounds every figure above:** all browser measurements are **headless
Microsoft Edge 152.0.4191.62**. Chrome and Firefox were never installed and nothing was installed
to run these. Edge is Chromium and drives the same audio service over the same WASAPI path, so
these figures are informative about Chrome — but they are not Chrome measurements, and
Firefox/SpiderMonkey, a different wasm compiler with a different audio backend, is entirely
unmeasured. This project has over-generalised a Firefox figure to all browsers once before; the
same mistake is available in the other direction.

**Not certified:** no figure here was measured under `docs/02-architecture.md` §2's conditions,
and a browser figure cannot be — it passes through a JIT, a browser process model and an OS audio
stack the project does not control, and no browser run can be core-pinned. Nothing in this entry
may be quoted as closing a Must requirement.

---

## §22 risk-register rows, as drafted

Two new rows and one downgrade. Status column set as it would be on the day the entry lands.

| ID | Risk | Severity | Mitigation |
|---|---|---|---|
| R-nn | **New, from S-5, 2026-09-05.** A browser build has no flush-to-zero: wasm mandates IEEE-754 subnormal handling and `DenormalGuard` is a structural no-op there. Measured: A1 Standard's p99.9 under a subnormal tail (silence after signal — what a guitar input does between notes) is **44.25–58.13%** of the block period against a 50% design bar, one rep of five over, where the same configuration on a steady signal is 32.4–33.6%. Natively the guard removes essentially the whole penalty (1.39x -> 1.00x); in a browser the only remaining levers are inside the DSP itself (anti-denormal dither, or flushing small stage state to zero), which is a `crates/` change no spike may make. Scoped to a prospective browser target only — it does not touch `namir-app` or `namir-clap`. | Medium (browser target only) | Budget the browser build against ~50%, not 33.6%; prefer the smaller model as a demo default (~2.5x headroom in the same regime). Revisit only if phase (b) proceeds. |
| R-nn | **New, from S-5, 2026-09-05.** Chromium on Windows cannot deliver playable live-input latency on this path: the browser's own accounting is **62 ms** best case (10 ms base + 42 ms output + a 10 ms input constant it declares fixed), roughly twice the ~30 ms soft reference, and `--enable-exclusive-audio` makes it 2.6x worse rather than better. The physical loopback figure — which can only exceed the API's — is **PENDING RUN**, so the true gap is unmeasured. Any browser demo that promises "plug in your guitar" is promising something not shown to work. | Medium (prospective demo only) | A demo ships file-playback-first, which was already the decision; this makes it a constraint rather than a preference. Close the loopback measurement (one cable, ten minutes, procedure written up in the spike) before any live-input claim. |
| R-nn | **Downgraded — "the DSP chain may not be portable off the desktop" -> Low, by S-5, 2026-09-05.** All six DSP-path crates and their 39-crate transitive graph compile for `wasm32-unknown-unknown` with **no edits under `crates/`**, and the assembled chain holds a real Web Audio deadline. `telemetry_ring.rs`'s `target_has_atomic = "64"` assertion — the one known hazard — holds on that target. Residual risk is not portability but the two rows above plus `xtask layering`'s rejection of `wasm`/`target_arch` outside `namir-platform`, which any in-workspace browser crate would collide with on day one. | Low | D-5.1's layering already keeps the DSP path platform-free; a `wasm32-unknown-unknown` CI job mirroring the existing mobile cross-build jobs would keep it that way at no design cost. |
