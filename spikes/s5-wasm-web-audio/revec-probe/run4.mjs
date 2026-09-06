import { readFileSync } from 'node:fs';
const { instance } = await WebAssembly.instantiate(readFileSync('./target/wasm32-unknown-unknown/release/revec_probe.wasm'), {});
const e = instance.exports;
const S = 65536, D = 131072;
new Float32Array(e.memory.buffer, S, 4096).fill(1.5);
for (let r = 0; r < 300000; r++) e.axpy_entry(D, S, 512, 0.5);
console.log('ok', new Float32Array(e.memory.buffer, D, 1)[0]);
