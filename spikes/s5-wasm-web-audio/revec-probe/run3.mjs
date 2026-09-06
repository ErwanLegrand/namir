import { readFileSync } from 'node:fs';
const buf = readFileSync('./target/wasm32-unknown-unknown/release/revec_probe.wasm');
const { instance } = await WebAssembly.instantiate(buf, {});
const e = instance.exports;
const S = 65536, D = 131072;
new Float32Array(e.memory.buffer, S, 4096).fill(1.5);
for (let r = 0; r < 300000; r++) { e.two_ptr(S, D); e.one_load_two_store(S, D); e.loop_indexed(S, D, 64); }
console.log('ok', new Float32Array(e.memory.buffer, D, 1)[0]);
