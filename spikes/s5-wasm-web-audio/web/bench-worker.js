import { loadNamir, writeBytes, parityDb } from "./namir.js";

const WASM = "../target/wasm32-unknown-unknown/release/s5_wasm_web_audio.wasm";
const PARITY_SAMPLES = 128 * 256;
// The Task 3 brief's stated bar. Deliberately NOT tuned to make this pass -- see
// RESULTS.md's "Parity" section. Measured control: the *native* chain compared against
// *itself*, same source, only `-C target-feature=-fma,-avx,-avx2` differing, scores
// -82.72 dB. So no build of this chain reaches -100 dB, and a failure here is a
// statement about the bar, not about the wasm port.
const PARITY_THRESHOLD_DB = -100;

async function bytes(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return new Uint8Array(await r.arrayBuffer());
}

self.onmessage = async (e) => {
  try {
    const { model, ir, warmup, measured, decaying, reps, allowParityFail } = e.data;
    const mod = await loadNamir(await bytes(WASM), () => performance.now() * 1000);

    // Parity check first: a broken chain must not be benchmarked.
    if (mod.exports.init(48000) !== 0) throw new Error("init failed");
    const modelBytes = await bytes(model);
    if (mod.exports.load_nam(writeBytes(mod, modelBytes), modelBytes.length) !== 0)
      throw new Error("load_nam failed");
    const irBytes = await bytes(ir);
    if (mod.exports.load_ir(writeBytes(mod, irBytes), irBytes.length) !== 0)
      throw new Error("load_ir failed");

    const refBuf = await bytes("../fixtures/reference_render_f32le.bin");
    const db = parityDb(mod, refBuf, PARITY_SAMPLES);
    const pass = db < PARITY_THRESHOLD_DB;
    self.postMessage({ kind: "parity", parityDb: db, pass });
    if (!pass && !allowParityFail) {
      // Step 7: a figure above the bar must be investigated before any timing figure is
      // recorded. Refusing by default is the point; the page's checkbox is the explicit
      // human act that says the investigation happened.
      self.postMessage({
        kind: "error",
        message: `parity ${db.toFixed(2)} dB is not below ${PARITY_THRESHOLD_DB} dB; refusing to benchmark. ` +
          `Tick "bench anyway" only after reading RESULTS.md's Parity section.`,
      });
      return;
    }

    // Then the measurement runs.
    for (let rep = 1; rep <= reps; rep++) {
      if (mod.exports.init(48000) !== 0) throw new Error("re-init failed");
      if (mod.exports.load_nam(writeBytes(mod, modelBytes), modelBytes.length) !== 0)
        throw new Error("load_nam failed");
      if (mod.exports.load_ir(writeBytes(mod, irBytes), irBytes.length) !== 0)
        throw new Error("load_ir failed");
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
    self.postMessage({ kind: "error", message: String(err && err.stack || err) });
  }
};
