import { readFileSync } from 'node:fs';
const { instance } = await WebAssembly.instantiate(readFileSync('./target/wasm32-unknown-unknown/release/revec_probe.wasm'), {});
const e = instance.exports;
const A=65536,B=131072;
new Float32Array(e.memory.buffer, A, 8192).fill(1.5);
for (let r = 0; r < 300000; r++) { e.loop_unaligned(A,B,64); e.loop_indexed(A,B,64); }
console.log('ok', new Float32Array(e.memory.buffer, B, 1)[0]);
