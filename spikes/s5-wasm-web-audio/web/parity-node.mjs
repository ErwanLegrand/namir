// Runs the same parity check and the same bench as web/bench-worker.js, but under
// bare Node (V8/TurboFan, same wasm engine family as Edge/Chrome). Chrome is not
// installed on the reference machine, so this is how Task 3's parity figure was
// obtained; it exercises the ABI and the dB comparison, NOT the browser's
// AudioWorklet or its cross-origin-isolated clock.
//
//   node web/parity-node.mjs [--bench] [--model a1_standard|a2_lite]
//                            [--decaying] [--reps N] [--measured N] [--warmup N]
//
// Run from the spike root. Needs both native renders in fixtures/ -- see RESULTS.md's
// "The native-vs-native reproducibility floor" section for how the control is made.
import { readFileSync } from "node:fs";
import { loadNamir, writeBytes, parity, CONTROL_MARGIN_DB, dbBetween, f32 } from "./namir.js";

// Default to the scalar artefact `run-matrix.sh` builds; `S5_WASM` picks another.
const WASM = process.env.S5_WASM || "web/build/scalar.wasm";
const PARITY_SAMPLES = 128 * 256;

const argv = process.argv.slice(2);
const flag = (n) => argv.includes(n);
const opt = (n, d) => {
  const i = argv.indexOf(n);
  return i >= 0 ? argv[i + 1] : d;
};

const modelName = opt("--model", "a1_standard");
const modelBytes = new Uint8Array(readFileSync(`fixtures/${modelName}.nam`));
// The reference renders are made from a1_standard, so parity always renders a1_standard
// whatever --model is being benchmarked; see web/bench-worker.js's note.
const parityModelBytes = new Uint8Array(readFileSync("fixtures/a1_standard.nam"));
const irBytes = new Uint8Array(readFileSync("fixtures/ir_48k.wav"));
const refBytes = new Uint8Array(readFileSync("fixtures/reference_render_f32le.bin"));
const controlBytes = new Uint8Array(readFileSync("fixtures/reference_control_f32le.bin"));

// process.hrtime.bigint() is nanosecond-resolution and unthrottled, i.e. strictly
// better than the browser's 5us performance.now(). Reported as such, never as a
// browser figure.
const nowUs = () => Number(process.hrtime.bigint()) / 1000;
const mod = await loadNamir(readFileSync(WASM), nowUs);

function fresh(m = modelBytes) {
  if (mod.exports.init(48000) !== 0) throw new Error("init failed");
  if (mod.exports.load_nam(writeBytes(mod, m), m.length) !== 0)
    throw new Error("load_nam failed");
  if (mod.exports.load_ir(writeBytes(mod, irBytes), irBytes.length) !== 0)
    throw new Error("load_ir failed");
}

fresh(parityModelBytes);
const p = parity(mod, refBytes, controlBytes, PARITY_SAMPLES);
console.log(
  `parity: residual ${p.residual.toFixed(2)} dB | native-vs-native control ` +
  `${p.control.toFixed(2)} dB | margin ${p.margin.toFixed(2)} dB -- ` +
  `${p.pass ? "PASS" : "FAIL"} (bar: margin <= ${CONTROL_MARGIN_DB} dB)`,
);

// Evidence that the comparison can actually fail. The assertion itself now lives in
// `parity()` so the browser path inherits it too -- this only prints the figure.
{
  const db = dbBetween(new Float32Array(PARITY_SAMPLES), f32(refBytes, PARITY_SAMPLES), PARITY_SAMPLES);
  console.log(`control: a silent render scores ${db.toFixed(2)} dB against this reference`);
}

// Task 6's ABI, smoke-tested here because nothing else exercises it yet: `process()`
// must write the chain's output back over `io_ptr`, or the AudioWorklet would emit its
// own input and read as a working-but-bypassed plugin.
{
  // 300 calls, not 1: process() runs `assert_resources_loaded` on its 256th call, and a
  // single call would leave that guard untested. Also re-inits first, so the call count
  // starts from zero rather than continuing the parity render's.
  fresh(parityModelBytes);
  const io = new Float32Array(mod.memory.buffer, mod.exports.io_ptr(), 128);
  let changed = 0, energy = 0;
  for (let call = 0; call < 300; call++) {
    for (let i = 0; i < 128; i++) io[i] = Math.sin((call * 128 + i) * 0.05) * 0.5;
    const before = io.slice();
    mod.exports.process();
    changed = 0;
    energy = 0;
    for (let i = 0; i < 128; i++) {
      if (io[i] !== before[i]) changed++;
      energy += io[i] * io[i];
    }
  }
  const ok = changed === 128 && energy > 0 && Number.isFinite(energy);
  console.log(`process() x300 in-place: last block ${changed}/128 samples written back, out energy ${energy.toExponential(2)} -- ${ok ? "OK" : "BROKEN"} (handover guard at call 256 passed)`);
  if (!ok) process.exitCode = 1;
}

if (!p.pass) {
  process.exitCode = 1;
} else if (flag("--bench")) {
  const warmup = Number(opt("--warmup", 5000));
  const measured = Number(opt("--measured", 100000));
  const decaying = flag("--decaying") ? 1 : 0;
  const reps = Number(opt("--reps", 5));
  for (let rep = 1; rep <= reps; rep++) {
    fresh();
    if (mod.exports.bench(warmup, measured, decaying) !== 0) throw new Error("bench failed");
    const s = new Float64Array(mod.memory.buffer, mod.exports.stats_ptr(), 5);
    console.log(
      `node ${modelName} ${decaying ? "decaying" : "steady"} rep ${rep}/${reps}: ` +
      `p50 ${s[0].toFixed(2)}% | p99 ${s[1].toFixed(2)}% | p99.9 ${s[2].toFixed(2)}% | ` +
      `max ${s[3].toFixed(2)}% | estimator ${s[4].toFixed(2)}% | ` +
      (s[2] - s[4] <= 5.0 ? "quotable" : "CONTAMINATED"),
    );
  }
}

// --- Negative checks. A guard nobody has seen fire is a guard nobody knows works.
// These run last: the second one deliberately traps the wasm instance, after which the
// module is unusable.

// 1. A degenerate control must be refused, not silently trusted. This is the hole that
//    fix round 2 closed: with control ~ 0 dB the relative bar collapses to an absolute
//    3 dB, under which a silent chain would PASS. The assertion lives in `parity()`, so
//    web/bench-worker.js inherits it.
{
  const zeroed = new Uint8Array(PARITY_SAMPLES * 4); // a silent control render
  let threw = null;
  try {
    fresh(parityModelBytes);
    parity(mod, refBytes, zeroed, PARITY_SAMPLES);
  } catch (e) {
    threw = String(e.message);
  }
  const ok = threw !== null && threw.includes("degenerate control");
  console.log(`negative: silent control -> ${ok ? "REFUSED (correct)" : `NOT REFUSED (BROKEN): ${threw}`}`);
  if (!ok) process.exitCode = 1;
}

// 2. process() against a chain that was never handed a NAM/IR must trap at its
//    handover-guard call, not emit silence quietly. A wasm panic surfaces in JS as a
//    catchable RuntimeError; the instance is dead afterwards.
{
  let threw = null;
  try {
    if (mod.exports.init(48000) !== 0) throw new Error("init failed"); // no load_nam/load_ir
    for (let call = 0; call < 300; call++) mod.exports.process();
  } catch (e) {
    threw = e.constructor.name;
  }
  const ok = threw === "RuntimeError";
  console.log(`negative: process() on an unloaded chain -> ${ok ? "TRAPPED (correct)" : `did not trap (BROKEN): ${threw}`}`);
  if (!ok) process.exitCode = 1;
}
