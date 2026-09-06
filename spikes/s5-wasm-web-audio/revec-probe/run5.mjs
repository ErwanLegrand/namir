import { readFileSync } from 'node:fs';
const { instance } = await WebAssembly.instantiate(readFileSync('./target/wasm32-unknown-unknown/release/revec_probe.wasm'), {});
const e = instance.exports;
const A=65536,B=131072,C=196608;
new Float32Array(e.memory.buffer, A, 8192).fill(1.5);
for (let r = 0; r < 300000; r++) e.axpy_out(C, A, B, 512, 0.5);
console.log('ok', new Float32Array(e.memory.buffer, C, 1)[0]);
