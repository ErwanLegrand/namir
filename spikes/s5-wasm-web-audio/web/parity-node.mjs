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

const WASM = "target/wasm32-unknown-unknown/release/s5_wasm_web_audio.wasm";
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

// Prove the comparison can actually fail: a silent render must score 0.00 dB, so a
// chain that produced nothing could never be mistaken for a pass.
{
  const silent = new Float32Array(PARITY_SAMPLES);
  const db = dbBetween(silent, f32(refBytes, PARITY_SAMPLES), PARITY_SAMPLES);
  console.log(`control: a silent render scores ${db.toFixed(2)} dB against this reference`);
  if (db < CONTROL_MARGIN_DB + p.control) throw new Error("silence would pass -- the metric is broken");
}

// Task 6's ABI, smoke-tested here because nothing else exercises it yet: `process()`
// must write the chain's output back over `io_ptr`, or the AudioWorklet would emit its
// own input and read as a working-but-bypassed plugin.
{
  const io = new Float32Array(mod.memory.buffer, mod.exports.io_ptr(), 128);
  for (let i = 0; i < 128; i++) io[i] = Math.sin(i * 0.05) * 0.5;
  const before = io.slice();
  mod.exports.process();
  let changed = 0, energy = 0;
  for (let i = 0; i < 128; i++) {
    if (io[i] !== before[i]) changed++;
    energy += io[i] * io[i];
  }
  const ok = changed === 128 && energy > 0 && Number.isFinite(energy);
  console.log(`process() in-place: ${changed}/128 samples written back, out energy ${energy.toExponential(2)} -- ${ok ? "OK" : "BROKEN"}`);
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
