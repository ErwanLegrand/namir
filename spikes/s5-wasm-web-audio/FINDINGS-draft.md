# DRAFT — not committed into `docs/`

This file holds the `docs/02-architecture.md` §19 findings entry and the §22 risk-register rows
that S-5 would produce, written in the exact shape those sections use. **They live here, in the
spike, on purpose.** S-5's own spec — recorded at `docs/02-architecture.md` §19 in review on
2026-09-06, having until then existed only in conversation — forbids it from adding a decision, a
requirement, a §14 row or a CI gate, and M14 is still in flight; these paragraphs move into `docs/` only if and when a
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
| Edge 152 / wasm simd128, steady (100 000 blocks, 2 reps) | 13.12–13.31 | **32.44–33.56** | 18.19 |
| Edge 152 / wasm simd128, steady (100 000 blocks, 5-rep replication, rebuilt artefact) | 12.75–13.13 | 30.00–31.31 | 19.50–19.88 |
| **Chrome 152** / wasm simd128, steady (20 000 / 100 000 blocks, 5+7 reps) | 11.44–11.63 | 26.25–29.25 | 18.00–18.37 |
| **Firefox 155** / wasm scalar, steady (20 000 blocks, 5 reps) | 43.50 | **56.25–57.75** | 53.25–54.00 |
| **Firefox 155** / wasm simd128, steady (20 000 / 100 000 blocks, 5+7 reps) | 12.00 | **20.25–24.75** | 18.75 |
| Edge 152 / wasm simd128, subnormal tail (5 reps) | 17.44–17.81 | **44.25–58.13** | 19.50 |
| **Chrome 152** / wasm simd128, subnormal tail (5 reps) | 16.31–16.50 | **42.00–44.06** | 18.19–18.38 |
| **Firefox 155** / wasm simd128, subnormal tail (5 reps) | 19.50–22.50 | **38.25–39.00** | 18.75 |
| Edge 152 / wasm simd128, A2 Lite, steady (100 000 blocks, 2 reps) | 3.00 | 14.81–16.31 | 9.56–9.75 |
| Edge 152 / wasm simd128, A2 Lite, subnormal tail (5 reps) | 3.38–3.75 | 18.56–20.81 | 10.12–10.31 |

Each row is one run set; the two 100 000-block steady rows are the same cell measured twice and
they disagree. The second is a five-rep replication of the first's two reps and lands at the
20 000-block screening level, but its artefact is a rebuild (the estimator moves with it), so
neither supersedes the other. The higher figure is quoted above as the conservative one, not as
the settled one, and the "cost grows with run length" reading that the two-rep set suggested is
correspondingly weak. **Chrome weakens it further**: across seven 100 000-block A1 reps its p50
is 11.44% — identical to its own 20 000-block figure — so the ~14% growth is not reproduced on
the other Chromium browser at all. Budgeting A1 against ~33.6% remains the conservative choice;
it is no longer a measured trend. Chrome's subnormal-tail row also keeps every rep under the
<=50% bar where Edge put one rep over — the margin is still a hairline, just not a breach.

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
which the spec listed as the unmeasured low-latency condition, is a **2.6x regression**, defined
as the ratio of API-reported *output* totals (52.0 -> 133.3 ms) — the render quantum halves to
5.33 ms while the device buffer balloons from 42 ms to 128 ms, taking the input-inclusive total
from 62 ms to 143 ms. The
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

**Browser scope note, which bounds every figure above:** the figures quoted above are **headless
Microsoft Edge 152.0.4191.62** unless a row says otherwise. **All three gates were re-run on
real Google Chrome 152.0.7977.83 (Task 10, 2026-09-06) and every one of them reproduced** — Gate 1 PASS on simd128 /
FAIL on scalar A1 (Chrome's p99.9 runs 2–3 pp *below* Edge's), Gate 2 zero steady-state underruns
with the same one-in-several first-second start-up event, Gate 3 identical to the digit including
the 2.6x `--enable-exclusive-audio` regression. So the Edge-as-Chrome substitution these figures
rested on is now evidenced rather than assumed. **Firefox 155.0.1 was then measured too
(Task 11, 2026-09-06), and it splits the result.** SpiderMonkey is a different wasm compiler and
cubeb a different audio backend, and the two compute-side gates survive both changes: Gate 1
PASSES on `simd128` with the tightest tail of the three browsers (A1 p99.9 20.25–24.75%) and
FAILS on scalar A1 at 56.25–57.75% — the same verdict, ~25 pp less severe, so the verdict
transfers and the number does not. Gate 2 passes on the *literal* criterion, with zero underruns,
zero missed quanta and no first-second start-up event in nine 60 s runs (0/9 against Chromium's
combined 5/20; Fisher p = 0.153, so this weakens nothing and proves nothing on its own).

**Gate 3 does not transfer, and that is the finding.** Firefox reports `baseLatency` **0.000 ms**,
**no input latency through any accessor**, and ignores `latencyHint`; `outputLatency` is chosen
per stream (33.0–39.6 ms) rather than per machine. The 62 ms accounting above is therefore a
Chromium construction with no Gecko analogue — not a smaller number on Firefox, an unconstructable
one. On the single term both engines disclose, the device buffer, Gecko is 33–39 ms against
Chromium's 42 ms: **less accounted for, not shown to be less latency**. The physical loopback,
already the number this gate most needs, is the only instrument that can separate the two, and it
remains `PENDING RUN` on all three browsers.

Two smaller Firefox results are load-bearing for the rows below. The subnormal penalty **ratio**
is the highest measured anywhere — **1.93x against the 2x kill criterion** — while Firefox's
*absolute* subnormal-tail figure (38.25–39.00%) is the **lowest** of the three: the ratio is high
because the steady baseline is low, which is a property of a ratio-shaped criterion rather than of
the runtime, and at Gecko's 20 µs clock quantum (four times Chromium's) the reading is not
distinguishable from the bar. And the in-browser subnormal census agrees between the two engines
to every digit reported (9 521 blocks / 2 436 620 samples / 1.401298e−45), which makes the
denormal finding an arithmetic property of the chain rather than one engine's codegen.

**What remains unmeasured is WebKit/Safari** — no macOS or iOS device is reachable here — and
every non-desktop runtime. This project has over-generalised a Firefox figure to all browsers
once before; two engines measured is not three, and the mobile direction is untouched.

**Not certified:** no figure here was measured under `docs/02-architecture.md` §2's conditions,
and a browser figure cannot be — it passes through a JIT, a browser process model and an OS audio
stack the project does not control, and no browser run can be core-pinned. Nothing in this entry
may be quoted as closing a Must requirement.

---

## §22 risk-register rows, as drafted

Two new rows and one downgrade. Status column set as it would be on the day the entry lands.

| ID | Risk | Severity | Mitigation |
|---|---|---|---|
| R-nn | **New, from S-5, 2026-09-05.** A browser build has no flush-to-zero: wasm mandates IEEE-754 subnormal handling and `DenormalGuard` is a structural no-op there. Measured: A1 Standard's p99.9 under a subnormal tail (silence after signal — what a guitar input does between notes) is **44.25–58.13%** of the block period against a 50% design bar on Edge 152, one rep of five over, **42.00–44.06%** on Chrome 152 and **38.25–39.00%** on Firefox 155, no rep over on either — where the same configuration on a steady signal is 32.4–33.6% (Edge) / 28.1–29.3% (Chrome) / 20.3–21.0% (Firefox). The bar is breached by one rep on one browser only, but the margin is a hairline on all three. Note that the *ratio* runs the other way from the absolute figure: Firefox is the fastest runtime in both regimes and yet has the highest penalty ratio measured anywhere (**1.93x** against the spike's 2x kill criterion), because a ratio-shaped criterion rewards a slow baseline. Budget against the absolute number, not the ratio. Natively the guard removes essentially the whole penalty (1.39x -> 1.00x); in a browser the only remaining levers are inside the DSP itself (anti-denormal dither, or flushing small stage state to zero), which is a `crates/` change no spike may make. Scoped to a prospective browser target only — it does not touch `namir-app` or `namir-clap`. | Medium (browser target only) | Budget the browser build against ~50%, not 33.6%; prefer the smaller model as a demo default (~2.5x headroom in the same regime, and under 21% of the block period on every runtime measured). Revisit only if phase (b) proceeds. |
| R-nn | **New, from S-5, 2026-09-05.** Chromium on Windows cannot deliver playable live-input latency on this path: the browser's own accounting is **62 ms** best case (10 ms base + 42 ms output + a 10 ms input constant it declares fixed), roughly twice the ~30 ms soft reference, and `--enable-exclusive-audio` makes it worse rather than better — a 2.6x regression on the API-reported output total (52.0 -> 133.3 ms), taking the input-inclusive total to 143 ms. The physical loopback figure — which can only exceed the API's — is **PENDING RUN**, so the true gap is unmeasured. **Firefox 155 does not offer an escape and makes the gap harder to see, not easier**: Gecko reports `baseLatency` 0, no input latency through any accessor, and ignores `latencyHint`, so the 62 ms accounting has no Gecko analogue at all; on the one comparable term, the device buffer, it reads 33–39 ms against Chromium's 42. That is less *accounted for*, not shown to be less latency. Any browser demo that promises "plug in your guitar" is promising something not shown to work, on either engine. | Medium (prospective demo only) | A demo ships file-playback-first, which was already the decision; this makes it a constraint rather than a preference. Close the loopback measurement (one cable, ten minutes, procedure written up in the spike) before any live-input claim — on Firefox it is the *only* instrument that can see the terms the API does not declare. |
| R-nn | **Downgraded — "the DSP chain may not be portable off the desktop" -> Low, by S-5, 2026-09-05.** All six DSP-path crates and their 39-crate transitive graph compile for `wasm32-unknown-unknown` with **no edits under `crates/`**, and the assembled chain holds a real Web Audio deadline. `telemetry_ring.rs`'s `target_has_atomic = "64"` assertion — the one known hazard — holds on that target. Residual risk is not portability but the two rows above plus `xtask layering`'s rejection of `wasm`/`target_arch` outside `namir-platform`, which any in-workspace browser crate would collide with on day one. | Low | D-5.1's layering already keeps the DSP path platform-free; a `wasm32-unknown-unknown` CI job mirroring the existing mobile cross-build jobs would keep it that way at no design cost. |
