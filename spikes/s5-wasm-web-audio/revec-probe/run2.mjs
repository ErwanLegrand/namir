import { readFileSync } from 'node:fs';
const buf = readFileSync('./target/wasm32-unknown-unknown/release/revec_probe.wasm');
const { instance } = await WebAssembly.instantiate(buf, {});
const e = instance.exports;
const P = 65536; // scratch region well past statics
new Float32Array(e.memory.buffer, P, 4096).fill(1.5);
for (let r = 0; r < 300000; r++) { e.base_off(P); e.base_off_aligned(P); e.loop_buf(P, P + 4096, 64); }
console.log('ok', new Float32Array(e.memory.buffer, P + 64, 1)[0]);
