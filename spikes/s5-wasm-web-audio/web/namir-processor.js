// No fetch and no clock in an AudioWorkletGlobalScope. The module arrives as a compiled
// WebAssembly.Module over postMessage; the only in-worklet underrun witness available is
// a gap between `currentFrame` and this processor's own block count.
//
// Two deviations from the task brief, both load-bearing:
//
// 1. The processor GENERATES its input rather than reading `inputs[0][0]`. There is no
//    microphone in a headless run and an unconnected input is silence, which would have
//    exercised only the cheap half of the chain. Task 5 measured A1 Standard at
//    p99.9 44.25-58.13% of the block period under a SUBNORMAL TAIL (silence after
//    signal) against 32.4-33.6% on steady tone -- and wasm mandates no flush-to-zero, so
//    that is structural. The two regimes are therefore both driven here, from the same
//    xorshift32 generator `harness.rs` uses, and counted separately.
//
// 2. The output is attenuated by `outGain` (default 1e-4). This runs on a real machine
//    with a real audio interface; the chain's job is to be expensive, not loud. The DSP
//    is unchanged -- only the copy into `outputs` is scaled.

const BLOCK = 128;
// `Signal::SubnormalTail` in harness.rs: 16 of every 512 blocks carry signal, the rest is
// EXACT silence, and the chain's own IIR state and convolution tail decay into f32's
// subnormal range under their own poles.
const TAIL_BURST_BLOCKS = 16;
const TAIL_PERIOD_BLOCKS = 512;

class NamirProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ready = false;
    this.blocks = 0;
    this.expectedFrame = -1;
    this.outGain = 1e-4;
    // Per regime: [blocks, underruns, missedQuanta]
    this.regimes = { steady: [0, 0, 0], tail: [0, 0, 0] };
    this.regime = "steady";
    this.switchBlock = Infinity; // block at which steady -> tail
    this.stopBlock = Infinity;
    this.rng = 0x2545f491; // harness.rs's seed, same generator
    this.port.onmessage = (e) => this.setup(e.data);
    // Proof of life from the render thread, independent of setup. Keep it: a page that
    // sees `boot` but never `ready` has a delivery problem inbound, not a DSP problem,
    // and distinguishing those two took most of this task's debugging.
    this.port.postMessage({ kind: "boot" });
  }

  setup({ wasm, model, ir, switchBlock, stopBlock, regime, outGain }) {
    try {
      // The bytes are compiled HERE, not handed over as a compiled `WebAssembly.Module`
      // the way the task brief has it. Posting a Module into an AudioWorkletGlobalScope
      // is silently dropped in Edge 152 -- no DataCloneError on the sending side, no
      // message on the receiving side, so the processor simply stays un-ready forever.
      // (Measured: a `{kind:"boot"}` from this constructor arrives, the setup message
      // carrying a Module never does, and the same message carrying plain bytes does.)
      // Synchronous compilation is legal off the main thread, and it happens once,
      // before the first render callback is answered.
      const inst = new WebAssembly.Instance(new WebAssembly.Module(wasm), {
        env: { now_us: () => 0 },
      });
      this.ex = inst.exports;
      this.mem = inst.exports.memory;
      if (this.ex.init(sampleRate) !== 0) throw new Error("init failed");
      for (const [fn, bytes] of [[this.ex.load_nam, model], [this.ex.load_ir, ir]]) {
        const ptr = this.ex.alloc(bytes.length);
        // Read `.buffer` after `alloc`: growing linear memory detaches the old one.
        new Uint8Array(this.mem.buffer, ptr, bytes.length).set(bytes);
        if (fn.call(null, ptr, bytes.length) !== 0) throw new Error("load failed");
      }
      this.io = this.ex.io_ptr();
      if (regime) this.regime = regime;
      if (switchBlock !== undefined) this.switchBlock = switchBlock;
      if (stopBlock !== undefined) this.stopBlock = stopBlock;
      if (outGain !== undefined) this.outGain = outGain;
      this.ready = true;
      this.port.postMessage({ kind: "ready", sampleRate });
    } catch (err) {
      this.port.postMessage({ kind: "error", message: String(err && err.message || err) });
    }
  }

  next() {
    // xorshift32, identical to harness.rs's, so the wasm side sees the same signal the
    // native and Task 4/5 browser figures were measured on.
    let x = this.rng;
    x ^= (x << 13) >>> 0;
    x ^= x >>> 17;
    x ^= (x << 5) >>> 0;
    this.rng = x >>> 0;
    return (this.rng / 4294967295) * 2 - 1;
  }

  process(inputs, outputs) {
    if (!this.ready) return true;
    if (this.blocks >= this.stopBlock) {
      if (!this.stopped) {
        this.stopped = true;
        this.port.postMessage({
          kind: "final",
          blocks: this.blocks,
          frame: currentFrame,
          steady: this.regimes.steady,
          tail: this.regimes.tail,
        });
      }
      return false;
    }

    if (this.blocks === this.switchBlock) this.regime = "tail";
    const r = this.regimes[this.regime];

    // A gap in currentFrame means the render thread missed callbacks. Note the caveat
    // recorded in RESULTS.md: an engine that renders every quantum late rather than
    // dropping quanta will never trip this, which is why the page ALSO measures the
    // graph clock against the wall clock.
    if (this.expectedFrame >= 0 && currentFrame !== this.expectedFrame) {
      r[1]++;
      r[2] += Math.max(0, (currentFrame - this.expectedFrame) / BLOCK);
    }
    this.expectedFrame = currentFrame + BLOCK;

    const view = new Float32Array(this.mem.buffer, this.io, BLOCK);
    if (this.regime === "steady" || this.blocks % TAIL_PERIOD_BLOCKS < TAIL_BURST_BLOCKS) {
      for (let i = 0; i < BLOCK; i++) view[i] = this.next();
    } else {
      view.fill(0); // exact zero: the chain's own state is what must decay
    }

    this.ex.process(); // in place; also carries the fault-count and resources guards

    const g = this.outGain;
    for (const ch of outputs[0]) for (let i = 0; i < BLOCK; i++) ch[i] = view[i] * g;

    this.blocks++;
    r[0]++;
    if (this.blocks % 375 === 0) { // 375 blocks = 1 s at 48 kHz
      this.port.postMessage({
        kind: "tick",
        blocks: this.blocks,
        frame: currentFrame,
        regime: this.regime,
        steady: this.regimes.steady,
        tail: this.regimes.tail,
      });
    }
    return true;
  }
}

registerProcessor("namir", NamirProcessor);
