import { loadNamir, writeBytes, parity, CONTROL_MARGIN_DB } from "./namir.js";

const REF = "../fixtures/reference_render_f32le.bin";
const CONTROL = "../fixtures/reference_control_f32le.bin";
// The reference renders are made from a1_standard, so the parity check always renders
// a1_standard whatever model is being benchmarked. Parity is a fidelity check on the
// port; the bench model is a separate axis. (Learned the hard way: pointing the check
// at a2_lite scored +1.15 dB, i.e. "completely different signal" -- which is the gate
// working, not a port defect.)
const PARITY_MODEL = "../fixtures/a1_standard.nam";
const PARITY_SAMPLES = 128 * 256;
// The reference renders were also made against this IR. Task 8/Extra 1 lets the caller
// point the timed reps at a different IR (to price rubato's resample-on-load), but the
// parity gate must never follow that choice -- it always loads THIS IR, or a chain fed
// ir_44k1 would be compared against a reference rendered from ir_48k and fail on a
// real signal difference, not a port defect. Same lesson as the PARITY_MODEL comment.
const PARITY_IR = "../fixtures/ir_48k.wav";

async function bytes(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return new Uint8Array(await r.arrayBuffer());
}

self.onmessage = async (e) => {
  try {
    const { wasm, model, ir, warmup, measured, signal, census, reps, irtiming } = e.data;
    const mod = await loadNamir(await bytes(wasm), () => performance.now() * 1000);

    const irBytes = await bytes(ir);
    const parityIrBytes = ir === PARITY_IR ? irBytes : await bytes(PARITY_IR);
    const modelBytes = await bytes(model);
    // Returns the wall-clock ms spent inside load_ir alone -- init and load_nam are
    // timed by nothing here because Extra 1 only asks about the resampler.
    const load = (m, irb) => {
      if (mod.exports.init(48000) !== 0) throw new Error("init failed");
      if (mod.exports.load_nam(writeBytes(mod, m), m.length) !== 0)
        throw new Error("load_nam failed");
      const ptr = writeBytes(mod, irb);
      const t0 = performance.now();
      const rc = mod.exports.load_ir(ptr, irb.length);
      const ms = performance.now() - t0;
      if (rc !== 0) throw new Error("load_ir failed");
      return ms;
    };

    // Parity check first: a broken chain must not be benchmarked. Always against
    // PARITY_IR, never the (possibly resampling) `ir` the caller asked for -- see the
    // comment on PARITY_IR above.
    load(await bytes(PARITY_MODEL), parityIrBytes);

    const p = parity(mod, await bytes(REF), await bytes(CONTROL), PARITY_SAMPLES);
    self.postMessage({ kind: "parity", ...p });
    if (!p.pass) {
      // A residual more than CONTROL_MARGIN_DB worse than the chain's own
      // native-vs-native reproducibility floor is a real numerical difference, not
      // codegen noise. Refusing to benchmark is the point, and there is no bypass.
      self.postMessage({
        kind: "error",
        message: `parity residual ${p.residual.toFixed(2)} dB is ${p.margin.toFixed(2)} dB ` +
          `worse than the native-vs-native control ${p.control.toFixed(2)} dB ` +
          `(margin allowed: ${CONTROL_MARGIN_DB} dB); not benchmarking`,
      });
      return;
    }

    const SIGNALS = ["steady", "amp-decay", "subnormal"];

    // The untimed census: does this signal actually produce subnormals in this build?
    // Task 5's premise, measured rather than assumed. Note wasm32 has no MXCSR, so the
    // browser-side witness is the output-side count only.
    if (census) {
      load(modelBytes, irBytes);
      if (mod.exports.census(warmup, measured, signal) !== 0) throw new Error("census failed");
      const c = new Float64Array(mod.memory.buffer, mod.exports.census_ptr(), 6);
      self.postMessage({
        kind: "census", signal: SIGNALS[signal] || String(signal),
        blocks: c[0], subBlocks: c[1], subSamples: c[2], minAbs: c[5],
      });
      self.postMessage({ kind: "done" });
      return;
    }

    // Extra 1 (Task 8): price rubato's resample-on-load. This is a ONE-OFF LOAD cost,
    // never the per-block steady-state figure the stats loop below measures -- the IR
    // stage resamples once, at load_ir, not per block. Opt-in (`irtiming`) so every
    // other bench invocation's message shape is unchanged.
    if (irtiming) {
      for (let rep = 1; rep <= reps; rep++) {
        const ms = load(modelBytes, irBytes);
        self.postMessage({ kind: "irload", rep, ir, ms });
      }
    }

    // Then the measurement runs.
    for (let rep = 1; rep <= reps; rep++) {
      load(modelBytes, irBytes);
      if (mod.exports.bench(warmup, measured, signal) !== 0)
        throw new Error("bench failed");
      const s = new Float64Array(mod.memory.buffer, mod.exports.stats_ptr(), 5);
      self.postMessage({
        kind: "stats",
        rep,
        p50: s[0], p99: s[1], p999: s[2], max: s[3], estimator: s[4],
        quotable: s[2] - s[4] <= 5.0,
      });
    }
    self.postMessage({ kind: "done" });
  } catch (err) {
    self.postMessage({ kind: "error", message: String((err && err.stack) || err) });
  }
};
