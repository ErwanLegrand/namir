# S-1 — NAM inference: Rust or C++

Spike per `docs/02-architecture.md` §19. Answers OQ-1 (Rust vs. C++ NAM inference) and OQ-2
(the real numbers behind FR-NAM-030's 90 dB accuracy placeholder and NFR-PERF-010's 25% CPU
placeholder). Results are written up in `docs/02-architecture.md` §19; this file is the
reproduction record.

## What's here

- `src/lib.rs` — a from-scratch Rust WaveNet inference engine, reading `.nam` JSON directly.
  `PreparedWaveNet` (immutable weights, `Sync`) is structurally separate from `WaveNetState`
  (per-instance history + reusable scratch, never shared) — this is D-9.1's requirement, tested
  structurally rather than merely asserted.
- `src/bin/generate_fixture.rs` — emits a seeded, non-degenerate "standard"-shaped WaveNet
  `.nam` and a 10 s / 48 kHz FR-NAM-030 test signal (clean / transient / saturated). Generated,
  not captured, per D-19.1. Nothing under `fixtures/` is committed — regenerate it.
- `src/bin/render.rs` — Rust twin of `NeuralAmpModelerCore`'s `tools/render.cpp`.
- `src/bin/compare.rs` — the FR-NAM-030 RMS-error-in-dB figure, overall and per segment.
- `src/bin/bench.rs` — the NFR-PERF-010 figure: single-core-pinned, ≥100,000 blocks of 64
  samples, 99.9th percentile per D-2.1/D-2.2.

## Reproducing

### 1. Rust side

```bash
cd spikes/s1-nam-inference
cargo build --release
./target/release/generate-fixture fixtures        # writes fixtures/model.nam, fixtures/test_signal.wav
./target/release/render fixtures/model.nam fixtures/test_signal.wav fixtures/test_signal.rust_out.wav
./target/release/bench fixtures/model.nam
```

### 2. C++ reference (`NeuralAmpModelerCore`)

Not vendored into this repo — clone and build it somewhere outside the Namir tree:

```bash
git clone --recurse-submodules https://github.com/sdatkinson/NeuralAmpModelerCore
cd NeuralAmpModelerCore
git checkout 3cde95c354d5ba6da01316cad90b05cfc4855053   # pinned commit, 2026-07-08. MIT licence.
```

Two things needed correction from a stock checkout on this machine (Windows 11, VS2022 Community,
using its bundled CMake/Ninja — `cmake`/`cl` aren't on the default PATH, use
`...\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe`):

1. **The `Dependencies/AudioDSPTools` submodule's own nested `Dependencies/eigen` submodule can
   fail to clone** (`fatal: '$GIT_DIR' too big` — a Windows path-length issue, not a real
   failure) — harmless: `tools/CMakeLists.txt` never `add_subdirectory`s AudioDSPTools, it only
   compiles `dsp/wav.cpp` directly and points Eigen at the *top-level* `Dependencies/eigen`,
   which clones fine. Nothing needs fixing here.
2. **`tools/CMakeLists.txt` applies a GCC/Clang-only `-Wno-error` flag unconditionally** to
   `wav.cpp`, `dsp.cpp` and `conv1d.cpp` (to silence upstream warnings on those compilers). MSVC
   rejects the flag outright (`D8021: invalid numeric argument`) and doesn't even set `/WX` for
   this target, so it's dead weight there — wrap those three `set_source_files_properties(...)`
   calls in `if (NOT MSVC) ... endif()`.
3. **Long build paths break MSBuild's FileTracker** (`FTK1011`) if you configure deep under a
   temp/session directory — build from a short path (e.g. `C:\namref`) if you hit this.

```bash
cmake -S . -B build -G "Visual Studio 17 2022" -A x64
cmake --build build --target render --config Release
cmake --build build --target benchmodel --config Release   # optional, informal timing only
```

Then, from `spikes/s1-nam-inference`:

```bash
<path-to>/render.exe fixtures/model.nam fixtures/test_signal.wav fixtures/test_signal.cpp_out.wav
<path-to>/compare.exe fixtures/test_signal.cpp_out.wav fixtures/test_signal.rust_out.wav
<path-to>/benchmodel.exe fixtures/model.nam        # C++ reference point, see caveat below
```

Note: the Rust `compare` binary is what's actually invoked — `<path-to>` above refers to
`NeuralAmpModelerCore\build\tools\Release\` for `render`/`benchmodel`, and
`spikes/s1-nam-inference/target/release/` for `compare`.

**16-bit WAV, deliberately.** `generate_fixture.rs` writes the test signal as plain 16-bit PCM.
32-bit float and 24-bit int both make `hound` emit a `WAVE_FORMAT_EXTENSIBLE` fmt chunk, which
`AudioDSPTools/dsp/wav.cpp` doesn't handle (it wants a `fact` chunk before `data` for extensible
files; hound doesn't write one there — model loads fine, then `render.exe` fails with "Tried to
read data chunk before fact chunk"). Since both renderers consume the *same* input file, its
quantization is common-mode and doesn't cap the Rust-vs-C++ comparison precision.

## Key facts established by reading `NeuralAmpModelerCore` source directly

(Not taken from memory or secondary sources — cited against the pinned commit above.)

- **WaveNet forward pass** (`NAM/wavenet/model.cpp`, `NAM/wavenet/detail.h`): per layer array —
  rechannel (`Conv1x1`, **no bias**) → per layer [dilated causal `Conv1D` (bias) + input-mixin
  `Conv1x1` on the raw input (**no bias**), summed, activated (gated = tanh × sigmoid split when
  configured), accumulated into the array's head sum, then a residual `Conv1x1` (bias) added back
  to form the next layer's input] → head-rechannel (`Conv1D`, kernel size 1, bias iff
  `head_bias`). **Two distinct signals cross the array boundary**, not one: the residual "trunk"
  (dimension = `channels`) feeds the next array's rechannel input, while the head-rechannel
  output (dimension = `head_size`) separately seeds the next array's head-sum accumulator. An
  early version of this spike conflated the two (assumed the trunk and the head hand-off were
  the same tensor) and got a `D8021`-adjacent class of bug — not a build error but a silent
  wrong-dimension weight-count mismatch that threw inside the C++ loader — before this was
  caught by actually running the comparison.
- **Flat `weights` array order**: per layer array, `[rechannel, per-layer[dilated, mixin,
  residual], head_rechannel]`, then a **trailing `head_scale` float** appended after every
  array. `NAM/wavenet/model.cpp`'s `WaveNet::set_weights_` unconditionally overwrites
  `_head_scale` with this trailing weight — `config.head_scale` in the JSON is parsed but
  discarded; the two are meant to agree by construction (confirmed against
  `nam/models/wavenet/_wavenet.py`'s exporter in the training repo, `weights[-1]` is documented
  there as mirroring `config["head_scale"]`).
- **Internal precision**: all WaveNet math runs in Eigen `f32` (`MatrixXf`) regardless of the
  outer `NAM_SAMPLE` (`double` by default) — the Rust port matches this (`f32` throughout).
- **D-9.1's weight/state coupling concern, confirmed**: `NAM/conv1d.h`'s `Conv1D` class holds
  `_weight`/`_bias` (parameters) and `_input_buffer` (a `RingBuffer`, mutable per-call history)
  as fields of the *same* object, held by value, with no `Arc`/shared-pointer separation
  anywhere in the type. Every `Layer`/`LayerArray`/`WaveNet` composes these by value, so every
  loaded model instance owns an independent copy of its weights. There is **no existing
  mechanism** in this codebase for two instances sharing one model to share its weight
  matrices — achieving FR-CLAP-090's cross-instance sharing with this core would require
  modifying it, not just binding to it.
- **`NAM_ENABLE_A2_FAST`** (default `ON`): an alternate, hand-optimized WaveNet path for a
  specific *different* architecture shape (`is_a2_shape` in `NAM/wavenet/a2_fast.cpp` — single
  23-layer array, LeakyReLU, specific kernel/dilation pattern). It does **not** intercept the
  "standard" 2-array shape this spike uses (`is_a2_shape` rejects anything but exactly one layer
  array) — confirmed by reading the detector, not assumed, after it was briefly suspected as the
  cause of an early crash (the actual cause was the rechannel-bias/array-chaining bug above).

## Accuracy and performance figures

See `docs/02-architecture.md` §19 for the measured numbers and the OQ-1 decision. In short: the
comparison lands around **-131 dB** (FR-NAM-030 asks for ≥ -90 dB), and the naive (no SIMD,
scalar `f32`) Rust implementation's NFR-PERF-010 figure is in the same *order of magnitude* as
the Eigen-vectorized C++ reference on this machine, though above the 25% placeholder at the
99.9th percentile — see the architecture doc for the number and what it does and doesn't imply.

**`benchmodel.exe`'s number is informal context, not a D-2.2-conformant measurement**: it reports
one mean over 1,500 buffers with no percentile and no single-core pin, whereas
`bench.rs` (this spike's actual NFR-PERF-010 tool) reports the 99.9th percentile over 200,000
single-core-pinned blocks per D-2.1/D-2.2. Useful for a same-machine ballpark, not a substitute.
