import { loadNamir, writeBytes, parity, CONTROL_MARGIN_DB } from "./namir.js";

const WASM = "../target/wasm32-unknown-unknown/release/s5_wasm_web_audio.wasm";
const REF = "../fixtures/reference_render_f32le.bin";
const CONTROL = "../fixtures/reference_control_f32le.bin";
// The reference renders are made from a1_standard, so the parity check always renders
// a1_standard whatever model is being benchmarked. Parity is a fidelity check on the
// port; the bench model is a separate axis. (Learned the hard way: pointing the check
// at a2_lite scored +1.15 dB, i.e. "completely different signal" -- which is the gate
// working, not a port defect.)
const PARITY_MODEL = "../fixtures/a1_standard.nam";
const PARITY_SAMPLES = 128 * 256;

async function bytes(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return new Uint8Array(await r.arrayBuffer());
}

self.onmessage = async (e) => {
  try {
    const { model, ir, warmup, measured, decaying, reps } = e.data;
    const mod = await loadNamir(await bytes(WASM), () => performance.now() * 1000);

    const irBytes = await bytes(ir);
    const modelBytes = await bytes(model);
    const load = (m) => {
      if (mod.exports.init(48000) !== 0) throw new Error("init failed");
      if (mod.exports.load_nam(writeBytes(mod, m), m.length) !== 0)
        throw new Error("load_nam failed");
      if (mod.exports.load_ir(writeBytes(mod, irBytes), irBytes.length) !== 0)
        throw new Error("load_ir failed");
    };

    // Parity check first: a broken chain must not be benchmarked.
    load(await bytes(PARITY_MODEL));

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

    // Then the measurement runs.
    for (let rep = 1; rep <= reps; rep++) {
      load(modelBytes);
      if (mod.exports.bench(warmup, measured, decaying ? 1 : 0) !== 0)
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
