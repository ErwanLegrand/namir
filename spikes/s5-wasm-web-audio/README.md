# S-5 — Namir's DSP chain in the browser: WASM + Web Audio

Spike per `docs/02-architecture.md` §19, against the S-5 spec agreed 2026-09-05. It asks whether
Namir's existing six-stage DSP chain — unmodified, compiled to `wasm32-unknown-unknown` — can
hold a Web Audio deadline in a desktop browser, and at what cost relative to the native build,
so that a later go/no-go on a **try-before-download demo page** has evidence behind it. The
findings are drafted in **`FINDINGS-draft.md`** (a §19 entry that has deliberately *not* been
committed into `docs/`); the full measurement log, retractions included, is **`RESULTS.md`**,
whose first section is the verdict. This file is the reproduction record.

**Every browser figure in Tasks 3–8 is headless Microsoft Edge 152.0.4191.62 on one machine**,
used as a stand-in because Chrome was not installed. **Task 10 re-ran all three gates on real
Google Chrome 152.0.7977.83 — the same Chromium major — and every one reproduced** (raw logs
`chrome_task10*.txt`), so the substitution is now evidenced rather than assumed. **Task 11 then
ran all three gates, plus Task 5's and Task 8's cells, on Firefox 155.0.1** (raw logs
`firefox_task11*.txt`) — SpiderMonkey, a different wasm compiler, with cubeb as a different audio
backend. **Gates 1 and 2 reproduce on Gecko; Gate 3 does not, and cannot** — Firefox reports no
input latency and a zero `baseLatency`, so the 62 ms accounting is a Chromium construction. What
is now unmeasured is **Safari/WebKit**, and every non-desktop runtime. **No figure produced here
is certified** in `docs/02-architecture.md` §2's sense, and a browser figure cannot be: it is
measured through a JIT, a browser process model and an OS audio stack the project does not
control. Nothing under `crates/`, `docs/`, `.github/` or `xtask/` was modified by this spike's
measurement work — **one exception, added in review on 2026-09-06**: S-5's *specification* is now
recorded at `docs/02-architecture.md` §19, where S-1..S-4's specs already live, because a spec
that exists only on the branch cannot constrain the branch. That entry is the spec only; the
findings stay here, per `FINDINGS-draft.md`'s own argument, and no decision, requirement, §14 row
or CI gate was added.

## Divergence from the spikes convention

S-1 through S-4 each vendor a from-scratch implementation and depend on no product
crate. This spike path-depends on `crates/namir-*` instead. Those spikes asked whether
an implementation was possible; this one asks whether the code that exists ports, which
vendoring cannot answer. `Cargo.lock` still pins the measurement.

## What's here

- `src/harness.rs` — the shared measurement harness: assembles the real six-stage chain via
  `namir_engine::build_default_engine`, delivers resources over the real command ring, drives
  128-frame blocks and reports p50/p99/p99.9/max plus D-2.4's per-residue estimator. Compiled
  into both the native binary and the wasm module; the only difference is the clock, which the
  caller supplies. Also defines the three signals — `Steady`, `AmplitudeDecay` and
  `SubnormalTail` — and the subnormal census.
- `src/wasm_abi.rs` — a flat pointer/length ABI for the browser host (no `wasm-bindgen`; there
  is no `fetch` inside an `AudioWorkletGlobalScope` anyway). Imports `env.now_us` from the host.
  Carries `HANDOVER_GUARD_BLOCK`, the one-shot `assert_resources_loaded()` check.
- `src/bin/native_bench.rs` — native reference figures on the same harness, same fixtures, same
  128-frame block. Also generates the fixtures and writes the reference renders the browser
  parity check compares against.
- `run-matrix.sh` — builds the two wasm artefacts (`scalar.wasm`, `simd128.wasm`). The third
  configuration in the matrix is the *same* simd128 binary under a V8 flag, not a third build.
- `web/serve.py` — static server that sends COOP/COEP, so `performance.now()` resolves to 5 µs
  rather than 100 µs. Only the bench page needs this; a demo would not ship these headers.
- `web/namir.js` — module instantiation and the parity comparison, kept free of DOM and Worker
  APIs so the worklet and the Node runner can both reuse it.
- `web/bench.html` + `web/bench-worker.js` — Gate 1: the timed matrix, the output-parity gate
  that must pass before any figure is reported, the subnormal census, and Task 8's `load_ir`
  timing.
- `web/worklet.html` + `web/namir-processor.js` — Gate 2: a real `AudioWorkletProcessor` driving
  the chain against a real device, with a `currentFrame`-gap underrun witness. Also carries the
  `&preroll=N` and `&checkRenderSizeHint=N` probes.
- `web/latency.html` + `web/capture-processor.js` — Gate 3: `getUserMedia` to worklet to output
  round trip, measured by subtracting two `currentFrame` values on one clock.
- `web/parity-node.mjs` — the same parity check and bench under bare Node (V8, no browser).
  Informational only; never quoted as a browser figure. This is also how the revectorizer trace
  was captured, since Chromium does not surface the renderer's V8 stderr here.
- `coreaudio_probe.ps1` — proves the audio endpoint under Gate 2 is real hardware rather than a
  null sink, which was checked rather than assumed.
- `RESULTS.md` — the measurement log, one section per task, in chronological order.
- `REVECTORIZE.md` + `revec-probe/` — a closed sub-question: why V8's 128-to-256-bit
  revectorizer packs nothing on this module. `revec-probe/` is the standalone probe crate the
  investigation used (its own `[workspace]` and `Cargo.lock`, `target/` gitignored); the eight
  `run*.mjs` scripts are mapped to what each one showed at the foot of `REVECTORIZE.md`, so the
  discrimination is re-runnable after a toolchain bump rather than only described. See below.
- `FINDINGS-draft.md` — the drafted `docs/02-architecture.md` §19 entry and risk-register rows.
  **Drafted into the spike, deliberately not committed into `docs/`**; they land only if and when
  a phase-(b) decision is taken.
- `chrome_task10_gate1.txt`, `chrome_task10_gate2.txt`, `chrome_task10_gate3.txt`,
  `chrome_task10_task5.txt` — Task 10's Chrome transcripts, same beacon-log form as the Edge ones.
- `firefox_task11_gate1.txt`, `firefox_task11_gate2.txt`, `firefox_task11_gate3.txt`,
  `firefox_task11_task5.txt`, `firefox_task11_task8.txt` — Task 11's Firefox transcripts, same
  form again; `firefox_task11_backend.txt` (+ `_run.txt`) is the CoreAudio endpoint probe that
  proves headless Firefox drove the real device, with the meter reading back the exact `&gain`.
- `edge_task*.txt`, `native_bench_output*.txt`, `coreaudio_task6_rerun.txt` — raw transcripts for
  Tasks 5, 6, 7 and 8 and for every native run. **Tasks 3 and 4 have no committed beacon log** —
  the Gate 1 matrix and the sustained confirmation, i.e. the spike's headline figures, are
  re-derivable only from the 30 per-rep lines transcribed verbatim in `RESULTS.md`'s "Full per-rep
  results" block, not from a raw transcript.
- `fixtures/`, `web/build/`, `target/` are gitignored. Fixtures are **generated, not captured**
  (D-19.1) and rebuild in seconds; the two wasm artefacts rebuild in ~40 s.

## Reproducing

Toolchain `rustc 1.98.0 (88d9e12ae 2026-08-18)`, target `wasm32-unknown-unknown` added.
Reference machine: AMD Ryzen 9 5950X / 64 GB / Windows 11 Pro 26200, PreSonus AudioBox 22VSL at
48 kHz (the only endpoint Chromium enumerates here). **The laptop axis in the spec was never
run.**

```bash
cd spikes/s5-wasm-web-audio
cargo run --release --bin native_bench     # fixtures, reference renders, all native timing reps
./run-matrix.sh                            # web/build/scalar.wasm + web/build/simd128.wasm
python web/serve.py                        # from the spike root, in another shell
```

Then, per gate — Edge 152, run alone and sequentially, one browser process at a time (a second
Edge launched over a run is exactly the contamination AGENTS.md warns about on this machine, and
it happened once). **Task 10 ran the same URLs on Chrome 152**: substitute
`"C:\Program Files\Google\Chrome\Application\chrome.exe"` for `msedge`, add a throwaway
`--user-data-dir` so the run cannot attach to an existing profile, and note that `bench.html`
does not close its own tab — Task 10 waited for the `{"kind":"done"}` beacon and then killed the
browser:

```bash
# Gate 1 — compute matrix. 5 reps; 20 000 blocks screens, 100 000 confirms.
msedge --headless=new --disable-gpu --no-sandbox \
  "http://127.0.0.1:8080/web/bench.html?auto=1&reps=5&measured=100000&wasm=simd128&model=a1_standard&signal=0"
#   &wasm=scalar | &model=a2_lite | &signal=1 (amp-decay) | &signal=2 (subnormal tail)
#   &census=1&measured=20000                              (the in-browser subnormal witness)
#   &ir=ir_44k1&irtiming=1                                (Task 8's load_ir timing)
#   --js-flags=--experimental-wasm-revectorize            (the third Gate 1 configuration)

# Gate 2 — worklet underruns. Real device; needs no cross-origin isolation.
msedge --headless=new --no-sandbox --autoplay-policy=no-user-gesture-required \
  "http://127.0.0.1:8080/web/worklet.html?auto=1&secs=60&split=1&wasm=simd128&model=a1_standard"
#   &secs=300           (the growth run)
#   &preroll=750        (arm C of the start-up attribution: chain not driven for 2 s)
#   &checkRenderSizeHint=256

# Gate 3 — latency. --use-fake-ui-for-media-stream auto-grants the mic.
msedge --headless=new --no-sandbox --autoplay-policy=no-user-gesture-required \
  --use-fake-ui-for-media-stream \
  "http://127.0.0.1:8080/web/latency.html?auto=1&hint=interactive"
#   &hint=balanced|playback | &noinput=1 | plus --enable-exclusive-audio
```

**The physical loopback half of Gate 3 is `PENDING RUN`** — there is no cable on this machine,
which was established rather than assumed. It needs the AudioBox and one 1/4-inch TS/TRS lead
from **LINE OUTPUT 1** on the back to **INPUT 1** on the front, and about ten minutes.
`RESULTS.md`'s "PENDING RUN — how to run the loopback half" section is written for someone who
has not read the spike.

**Task 11 ran the same URLs on Firefox 155**, which takes prefs where Chromium takes flags.
Write a throwaway profile whose `user.js` sets `media.autoplay.default=0`,
`media.autoplay.blocking_policy=0` and `media.navigator.permission.disabled=true` (the last
replaces `--use-fake-ui-for-media-stream`; leave `media.navigator.streams.fake` at `false`, it is
the fake-*device* switch and must not be set), then run each gate's URL unchanged:

```bash
"C:\Program Files\Mozilla Firefox\firefox.exe" --headless --no-remote --new-instance \
  --profile <that profile> "<the same URL>"
```

Three Chromium-only things have no Firefox form: `--js-flags=--experimental-wasm-revectorize`
(so `REVECTORIZE.md` stays a V8 result), `--enable-exclusive-audio` (Gate 3's exclusive
condition is NOT RUNNABLE, not pending), and `--disable-gpu`/`--no-sandbox`, which are simply
unnecessary. Firefox's `performance.now()` quantum under the same COOP/COEP headers is **20 µs**,
not Chromium's 5 µs, and no `privacy.reduceTimerPrecision*` pref moves it — so no Firefox figure
resolves finer than 0.75% of the block period, and a p50 landing on a 0.75% multiple at all is
the proof the page was cross-origin isolated (a non-isolated Gecko context clamps to 1 ms).

The revectorizer trace is a Node run, not a browser one (Chromium does not surface the
renderer's V8 stderr here):

```bash
S5_WASM=web/build/simd128.wasm \
  node --experimental-wasm-revectorize --trace-wasm-revectorize web/parity-node.mjs
```

Every browser run reports the **output-parity gate before it will benchmark**, and there is no
bypass: residual -81.49 dB (scalar) / -81.69 dB (simd128) against a -82.72 dB native-vs-native
control. The figures exist because the port was proved correct first.

## Key facts established

- **The chain ports with no edits at all.** All six crates (`namir-core`, `-params`, `-dsp`,
  `-nam`, `-ir`, `-engine`) and their 39-crate transitive graph build for
  `wasm32-unknown-unknown` in 11.5 s, zero warnings. `telemetry_ring.rs`'s
  `target_has_atomic = "64"` assertion holds there — LLVM legalises 64-bit atomics to plain
  loads/stores on this single-threaded target. No `cpal`, no `clack`, nothing platform-specific
  leaked in, consistent with D-5.1's layering table.
- **Gates 1 and 2 hold on two engines and two audio backends; Gate 3 does not transfer.**
  Task 11 ran everything on Firefox 155/SpiderMonkey/cubeb. Gate 1 PASSES with the **best tail
  measured anywhere** — A1 `simd128` p99.9 **20.25–24.75%** against Chrome's 26.25–28.87% — and
  scalar A1 still FAILS but at **56.25–57.75%**, ~25 pp less severe than V8's, so carry the
  verdict and not the number. Gate 2 passes on the *literal* criterion Chromium needed a
  judgement for: zero underruns, zero missed quanta, nine 60 s runs, **no first-second start-up
  event at all** (0/9 against Edge's 4/11 and Chrome's 1/9 — Fisher p = 0.153, i.e. suggestive of
  nothing on its own). Gate 3 is the one that breaks: see the Gate 3 bullet below.
- **Gate 1 passes on `simd128` and fails on scalar for A1 Standard.** p99.9 of the 2 666.67 µs
  block period, sustained over 100 000 blocks: A1 **32.4-33.6%**, A2 Lite **14.8-16.3%**, against
  a <=50% bar. (A1's figure is the higher of two disagreeing measurements of that cell — a later
  five-rep replication on a rebuilt artefact reads 30.0-31.3. The conservative one is quoted;
  `RESULTS.md` carries both.) Scalar A1 is **74-86%** — a FAIL. `rustfft`'s `wasm_simd` feature has **no runtime
  detection**, so the simd128 artefact traps immediately where simd128 is absent: they are two
  artefacts, not one with a fallback. A browser build of A1 is therefore *gated* on WebAssembly
  SIMD and must fail loudly, not silently, without it.
- **With `simd128` the browser penalty is nearly uniform, ~1.8x (A1) and ~1.6x (A2) on p50.**
  Task 3's earlier reading — that the penalty scales with model size — was measured on the scalar
  artefact and its explanation is refuted: A1's scalar cost is dominated by dot products the
  native build auto-vectorises and scalar wasm does not. The residual ~1.7x is the ordinary
  wasm-vs-native gap (bounds checks, no FMA contraction, no `target-cpu` tuning).
- **256-bit SIMD is a closed question: `REVECTORIZE.md`.** V8's experimental revectorizer
  provably runs on this artefact (16 functions visited, 14 `Build tree failed!`, 9 `Empty seed`,
  zero packs, verified against a negative control) and changes nothing measurable. The cause was
  isolated: `wide::f32x8` *is* two adjacent `v128` ops on wasm, so the right pairs are being
  handed to the pass, but V8's seeder needs both stores off one address local with folded
  `offset=` immediates and LLVM's strength reduction gives our loops a recomputed base
  (`i32.const 16; i32.add`) instead. Four rewrites failed the same way. Not a `wide` bug, not a
  `rustfft` bug, not a V8 limitation — and not something Rust gives us a lever over. **The wasm
  performance story is `+simd128`.** The probe crate is committed (`revec-probe/`) because that
  conclusion depends on LLVM's codegen and V8's seeder both staying as they are.
- **Denormals are structural in a browser.** wasm mandates IEEE-754 subnormal handling and has no
  flush-to-zero, so `DenormalGuard` is a no-op there. Kill criterion 3 (>2x penalty) did **not**
  fire — worst **1.93x, on Firefox** (Task 11; 1.57x was the worst on V8) — but the absolute
  figure did move: A1 under a subnormal tail reaches p99.9 **44.25-58.13%** on Edge, one rep of
  five above Gate 1's bar. Natively, FTZ/DAZ removes essentially the
  whole penalty (1.39x to 1.00x); in a browser that saving is not available at any price. A1's
  browser margin under ordinary silence is a hairline, not 1.5x. **The ratio is worst on the
  fastest browser, which is a property of the criterion**: Firefox's absolute subnormal-tail
  figure (38.25–39.00%) is the *lowest* of the three, and 1.93x is high only because its steady
  baseline is 8–10 pp lower than V8's. At Gecko's 20 µs clock both terms carry ±0.75 pp, so read
  it as "roughly 1.8–2.1x, not fired on the measured medians". The in-browser subnormal census
  agrees between the two engines to every digit (9 521 blocks / 2 436 620 samples /
  1.401298e−45), which makes this the chain's arithmetic rather than either engine's codegen.
- **Gate 2 passes on real hardware — a reading of the criterion, not its literal text.** The
  literal bar ("every run zero over 60 s") is missed by the start-up event in the next bullet;
  the PASS is the spike's judgement that the gate asks about the chain rather than the platform,
  and it is the coordinator's ruling of 2026-09-05. Zero underruns in every steady-state second of every run —
  60 s and 300 s, both signal regimes, three cells — including 123 750 subnormal-tail blocks, the
  regime Gate 1 flagged as the risk. The scalar build passes too, which was not expected: at a
  40 ms device buffer the 50% budget is not the binding constraint.
- **The start-up underrun is the platform's, proved by experiment, not the spike's code.** About
  a third of runs drop one 3-4 quantum device callback in the *first second*. A three-arm, 36-run
  experiment (baseline / handover guard moved 10x later / chain not driven for two seconds)
  produced the event at the same rate and the same second in all three arms. It is Chromium's or
  WASAPI's stream start-up. A demo would hear a click at start and must hide it (ramp in a gain,
  or don't connect until the stream is warm).
- **`WebAssembly.Module` over `postMessage` into an AudioWorklet is silently dropped on Edge
  152.** No `DataCloneError`, no exception, no `onmessage` — the processor just renders silence
  forever, which is indistinguishable from a dead audio backend. Post the **bytes** and call
  `new WebAssembly.Module()` inside the processor; synchronous compilation is legal off the main
  thread. This cost most of a task and the failure mode is maximally misleading.
- **Gate 3's accounting is Chromium-shaped and does not survive the engine change.** Firefox
  reports `baseLatency` **0.000 ms**, **no input latency by any accessor**, and ignores
  `latencyHint` entirely; its `outputLatency` is per-stream (33.0–39.6 ms across twenty-one
  streams, fixed within each). So the 62 ms figure has no Firefox analogue — not a smaller number, an
  unconstructable one. On the one term both browsers disclose, the device buffer, Gecko is
  33–39 ms against Chromium's 42. **That is less accounted, not proven less latency**, and the
  physical loopback — still uncabled — is the only thing that can tell the difference. Firefox
  makes that measurement more load-bearing, not less.
- **Gate 3 says no for live guitar input on this path.** The browser's own accounting is **62 ms**
  best case (10 ms `baseLatency` + 42 ms `outputLatency` + a *declared* 10 ms input constant that
  Chromium reports as fixed and uninfluenceable), roughly twice the ~30 ms soft reference before
  anything the API does not account for — and a physical loopback can only be larger, never
  smaller. **`--enable-exclusive-audio` is a 2.6x regression** — the ratio of API-reported
  *output* totals, 52.0 to 133.3 ms, which is the definition used everywhere in this spike; the
  device buffer alone goes 42 to 128 ms and the input-inclusive total 62 to 143 ms. It is not the
  low-latency condition the spec assumed: the render quantum halves (10 to 5.33 ms) and the
  device buffer balloons anyway. `interactive` and `balanced` are identical here; only `playback` moves, adding
  20 ms.
- **The resampler cost is a one-off load cost.** `load_ir` of a 44.1 kHz IR into a 48 kHz context
  costs **+2.0-2.4 ms (+55-60%)**, reproducibly, because `rubato::FftFixedInOut` is scalar/NEON
  only with no `simd128` path. It lands on the load path, never per block, so it is immaterial to
  real-time safety and only matters to a demo's perceived load time. **The per-block question was
  unanswered, not answered negatively** — only one of three Edge runs produced any quotable rep,
  so there was never a clean pair to compare. **Task 11 answers it on
  Firefox**: all fifteen reps across three runs are quotable and the per-block figure is identical
  between a resampled and a native-rate IR (p50 12.00%, p99.9 20.25% in fourteen of fifteen), so
  there is no per-block resampler cost on that runtime. The Chromium half stays unanswered — and
  the reason the comparison worked on Gecko is issue #149's other half: its tail is tight enough
  that `is_quotable()` passes reps the same rule rejects on V8.
- **`renderSizeHint` is absent on Edge 152, Chrome 152 *and* Firefox 155.**
  `ctx.renderQuantumSize` is `undefined`, not merely defaulting to 128 — the property is not in
  any of the three runtimes. It shipped in Chrome 153, so the two Chromium reads are "one release
  behind"; Firefox 155 is well past that release, which makes this a **Blink-only feature Gecko
  has not implemented**. `PENDING RUN` is therefore specifically a Chrome-153+/newer-Edge item, and
  no newer Firefox will close it.
- **`is_quotable()` (D-2.4) cannot be applied literally in a browser.** It flags all 30 matrix
  reps, including the calmest, because a browser's tail is structurally fat (GC, tier-up, the
  renderer's scheduler all land inside the timed span). **Firefox sharpens issue #149 rather than
  softening it**: there the rule passes 48 of the 49 steady reps taken and then flags 9 of the 10
  subnormal-tail reps — i.e. on the one browser where it can discriminate, the only thing it discriminates
  against is a real, signal-dependent cost increase. Applying it as written would discard the
  whole matrix and leave no verdict. `RESULTS.md` states the substitution used, in full, as the
  author's rather than the project's — and the same weakness in the native benchmark is now
  issue **#149**.

## Findings that were corrected mid-spike

Kept on the record deliberately, per this project's practice of treating a retracted finding as
normal rather than something to tidy away. Four claims were overturned by later evidence, and in
each case the correction is the finding:

1. **"A1 Standard doesn't fit in the browser."** Task 3 measured the scalar artefact only and
   reported a ~6x browser penalty that scales with model size. Task 4's build matrix shows that
   was an artefact of the build, not of the browser: with `+simd128` the penalty is ~1.8x and
   nearly model-independent. Both the number and the explanation were wrong.
2. **"The amp-decay penalty is not a denormal effect."** A coordinator ruling said so, on the
   sound-as-far-as-it-went reasoning that the *input* never leaves normal range. Task 5's census
   found 47.6% of amp-decay blocks carrying subnormal *output* down to 1e-45, MXCSR raising DE on
   52.3% of them, and FTZ/DAZ collapsing the 1.44x p50 cost to 1.02x. It mostly is a denormal
   effect. The relabelling to `AmplitudeDecay` stands — the mode confounds amplitude with
   subnormality and `SubnormalTail` is the clean instrument — but that is about naming what the
   signal is, not disowning what it measured.
3. **"The first-second underrun is this spike's own handover guard."** Guessed in Task 6, tested
   in the fix round, disproved: moving the guard 10x later did not move the event, and not
   running the chain at all for two seconds did not remove it. Twelve reps per arm was the
   minimum that could separate the rates — six would have "confirmed" the wrong conclusion at
   Fisher p = 0.09.
4. **"There is a per-block resampler cost."** Retracted in Task 8 as session-order contamination —
   and then the retraction's own reasoning was corrected too: the honest statement is that only
   one of three runs produced any quotable rep, so a clean comparison was never made. "Not
   measured" is a weaker and different thing to have than "measured, no effect." **Task 11 finally
   measured it, on Firefox**: fifteen quotable reps over three runs, the same p50 and p99.9 with a
   resampled IR as with a native-rate one. So the retraction was right and is now backed by data
   on one runtime — the Chromium half is still the weaker statement.

A fifth was added at Task 11: Task 3 predicted that Chrome and Firefox would see "the same 5 µs
clock quantum under cross-origin isolation, so any difference should be engine and scheduler, not
timer resolution". Chrome's half held; **Firefox's is 20 µs**, four times coarser, and no pref
moves it — so a Gecko-vs-Chromium difference under ~0.75 pp of the block period is timer
resolution and nothing else. Annotated in place at Task 3's own text.

Two smaller retractions are recorded in place in `RESULTS.md`: the claim that V8 accepting
`--experimental-wasm-revectorize` could be inferred from the absence of an error message (it
cannot; a bogus flag is equally silent in headless Edge, and the question was re-settled with two
checks that do discriminate), and an early Gate 2 sentence attributing the start-up event before
it was tested.

## Out-of-branch consequences: three issues filed

Findings that belong to the product rather than to this spike, filed rather than fixed here
because nothing under `crates/` may be touched:

- **#147** — `DenormalGuard` has no AArch64 test; the `FPCR.FZ` set/restore path is never
  executed. Surfaced by Task 5 pricing the guard: it is worth the entire subnormal penalty on
  x86, and on ARM nothing proves it runs at all.
- **#148** — `wide::f32x8` vectorisation on AArch64/NEON is unverified, and no ARM benchmark
  exists at all. Surfaced by having to establish, for wasm, what `wide::f32x8` actually lowers
  to — the same question has never been asked of NEON.
- **#149** — `six_stage_chain.rs`'s `is_quotable()` reads signal-dependent cost as machine
  contamination. Surfaced by the browser matrix, where the same rule misfires on every rep, and
  by Task 5's subnormal signal, where a *real* cost increase is exactly what the rule discards.

## What this hands to a demo build (phase (b))

Spec §12, updated with what the spike actually learned:

- **Live model switching (D-8.1 in a browser) is unproven, and single-threaded operation may
  preclude it.** The spike loads models before rendering starts and never exercises the handover;
  what it *did* prove is only that the one-shot `assert_resources_loaded()` guard on the audio
  thread is not what drops a callback. Nothing here says a prepare-offer-crossfade-retire cycle
  survives with no worker thread to prepare on.
- **The UI needs a third `UiHost` consumer.** `egui` ports to the web; `baseview` does not. That
  seam already exists (`crates/namir-ui/src/host.rs`) and is the reason this is a bounded job
  rather than a rewrite.
- **A public demo needs a DI clip Namir is licensed to redistribute** — the same unresolved
  problem as AQ-4 (§15 item 3) wearing a different hat. Self-recorded material resolves it; that
  recording session is separate work on trunk. **Gate 3 makes this load-bearing rather than
  optional**: at 62 ms API-reported best case, a demo ships file-playback-first, so the clip *is*
  the demo.
- **`xtask layering` rejects `wasm` and `target_arch` as cfg predicates outside
  `namir-platform`.** Any workspace crate for the browser collides with that lint on day one.
  This spike dodges it entirely by living in `spikes/`, which is outside the workspace — a real
  build cannot.
- **`FileRef::embedded` (FR-STATE-080) is the pre-existing, gate-compatible way to carry a model
  with no I/O and no network.** A demo that fetches models from a CDN collides with `xtask
  network-free` and §13's RD-1 non-goal; embedding does not.
- **A `wasm32-unknown-unknown` CI job would mirror `mobile-cross-build-android`/`-ios` exactly** —
  same crate subset, same shape, plausibly under the same NFR-PORT-030 tag. Task 1 is the
  evidence that such a job would be green today.
- **Serve the simd128 artefact and fail loudly without it.** There is no working A1 Standard
  configuration on a simd128-less runtime, and `rustfft`'s feature traps rather than falling back.
  Every shipping browser has had simd128 by default since 2021 in Chrome/Edge 91 and Firefox 89,
  and since Safari 16.4 in March 2023, so this is a stated floor rather than a live risk — but it
  is a floor.
- **Budget A1 Standard against ~50%, not 33.6%.** Silence between notes is most of a session and
  costs +17 pp of the block budget with no `DenormalGuard` to claw it back. A2 Lite has ~2.5x
  headroom in the same regime and is the safer default for a demo.
- **Two things are open that a demo would want closed**: A1's run-length behaviour, and the
  Gate 3 loopback number — the one figure that gate existed to produce and the one it did not
  get. On the first, be careful what you inherit: Task 4 read a ~14% cost growth from 20 000 to
  100 000 blocks off **two** reps, and Task 5's **five** reps of the same cell at the same length
  read 30.00–31.31 — the screening level. Task 5's artefact is a rebuild, so neither supersedes
  the other. **Task 10 settles which way to read that disagreement**: across seven 100 000-block
  A1 reps on Chrome 152 the p50 is 11.4375% — identical to Chrome's own 20 000-block figure —
  and the p99.9 moves 26.25–28.87 to 27.37–29.25, i.e. no growth outside rep-to-rep spread on
  the other Chromium browser. **The ~14% is one two-rep reading that neither of the two later
  five-and-seven-rep sets reproduces**, so treat it as a measurement artefact rather than as a
  trend. Budgeting A1 against the higher figure stays the conservative choice — now as a margin
  of safety, not as something measured — and 112 500 worklet blocks produced no scheduling
  consequence either way.
- **Two engines are now measured, and the remaining gap is Safari and the loopback.** Task 10
  measured all three gates on Chrome 152 and they reproduce Edge 152; Task 11 measured them on
  Firefox 155, where Gates 1 and 2 reproduce and Gate 3 turns out to be a Chromium construction.
  So a demo can be announced as working on Chromium *and* Gecko for the compute and scheduling
  claims — and must not make any latency claim at all, on either. **WebKit/Safari is unmeasured**
  (not installable here), and the physical loopback is unmeasured on all three, which is the one
  number that would let a latency claim be made. Firefox raises that item's value rather than
  lowering it: it is the only way to see the terms Gecko does not declare.
- **Prefer the smaller model on the browser you cannot choose.** The engine spread on the same
  bytes is real — A1 `simd128` p99.9 ranges 20.25% (Firefox) to 33.56% (Edge), and scalar A1
  ranges 56% to 86% — and a demo does not pick its visitor's browser. A2 Lite is under 21% of the
  block period on every runtime measured, in every regime, including the subnormal tail.
