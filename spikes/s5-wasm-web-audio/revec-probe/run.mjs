import { readFileSync } from 'node:fs';
const buf = readFileSync(process.argv[2] ?? './target/wasm32-unknown-unknown/release/revec_probe.wasm');
const { instance } = await WebAssembly.instantiate(buf, {});
const e = instance.exports;
console.log('exports:', Object.keys(e).join(','));
e.seed?.(1.0);
let s = 0;
for (let r = 0; r < 200000; r++) { e.canonical?.(1024); e.straight?.(0); e.via_wide?.(1024); s += (e.accumulate?.(1024) ?? 0) + (e.checksum?.() ?? 0); }
console.log('sink', s);
