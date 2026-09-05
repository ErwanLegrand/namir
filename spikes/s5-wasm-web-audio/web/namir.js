// Instantiates the S-5 module. Kept free of DOM and Worker APIs so both the
// AudioWorklet (Task 6) and the Node parity runner can reuse it.
export async function loadNamir(wasmBytes, nowUs) {
  const { instance } = await WebAssembly.instantiate(wasmBytes, {
    env: { now_us: nowUs },
  });
  return { exports: instance.exports, memory: instance.exports.memory };
}

export function writeBytes(mod, bytes) {
  const ptr = mod.exports.alloc(bytes.length);
  // Read `.buffer` after `alloc`: growing linear memory detaches the old ArrayBuffer.
  new Uint8Array(mod.memory.buffer, ptr, bytes.length).set(bytes);
  return ptr;
}

/// Renders PARITY_SAMPLES through the wasm chain and compares against the native
/// reference render, in dB. Shared by the browser worker and the Node runner so both
/// report the same number computed the same way.
export function parityDb(mod, refBytes, samples) {
  const outPtr = mod.exports.render(samples);
  const got = new Float32Array(mod.memory.buffer, outPtr, samples);
  const want = new Float32Array(refBytes.buffer, refBytes.byteOffset, samples);
  let num = 0, den = 0;
  for (let i = 0; i < samples; i++) {
    const d = got[i] - want[i];
    num += d * d;
    den += want[i] * want[i];
  }
  // den == 0 would mean the *native* reference is silent; report that as +Infinity
  // rather than NaN, so it can never be mistaken for a pass.
  return den === 0 ? Infinity : 10 * Math.log10(num / den);
}
