// Onset detector for the Gate 3 loopback measurement.
//
// The brief used a ScriptProcessorNode(4096) and accepted that its own buffering inflates
// the absolute figure by an unknown constant. Gate 3's whole value is an ABSOLUTE number,
// so that trade is not available here: this is an AudioWorkletNode instead.
//
// It buys more than "less buffering". A ScriptProcessorNode hands you a buffer with no
// reliable statement of WHEN, so the brief had to reconstruct the timeline by concatenating
// callbacks and counting samples from an assumed start. Inside an AudioWorkletGlobalScope
// `currentFrame` is the absolute frame index of the first sample of the quantum being
// rendered, on the same clock as `AudioContext.currentTime`. The click is scheduled at a
// known frame on that clock. So the round trip is a subtraction of two numbers from one
// clock, with no accumulation and no assumed origin.
//
// Detection is FIRST CROSSING, not peak. A peak search finds the largest sample of the
// captured burst, which can be several hundred microseconds -- or, through an AC-coupled
// line input that differentiates a rectangular pulse, an unpredictable amount -- after the
// edge actually arrived. Latency is a property of the edge.
//
// The threshold is derived from a measured noise floor rather than hard-coded, because
// "well above noise but not clipping" is set by a human turning a physical knob and cannot
// be assumed. `calibrate` measures it; `arm` uses it.
class CaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.mode = "idle"; // idle | calibrate | armed
    this.sumSq = 0;
    this.n = 0;
    this.thresh = 0.02;
    this.peak = 0;
    this.onsetFrame = -1;
    this.port.onmessage = (e) => {
      const d = e.data;
      if (d.cmd === "calibrate") {
        this.mode = "calibrate";
        this.sumSq = 0;
        this.n = 0;
      } else if (d.cmd === "arm") {
        this.mode = "armed";
        this.thresh = d.thresh;
        this.peak = 0;
        this.onsetFrame = -1;
      } else if (d.cmd === "read") {
        this.mode = "idle";
        this.port.postMessage({
          kind: "result",
          onsetFrame: this.onsetFrame,
          peak: this.peak,
          rms: this.n ? Math.sqrt(this.sumSq / this.n) : 0,
        });
      }
    };
    this.port.postMessage({ kind: "boot" });
  }

  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (!ch) return true; // no input yet; keep the node alive so the graph keeps pulling
    if (this.mode === "calibrate") {
      for (let i = 0; i < ch.length; i++) this.sumSq += ch[i] * ch[i];
      this.n += ch.length;
    } else if (this.mode === "armed") {
      for (let i = 0; i < ch.length; i++) {
        const v = Math.abs(ch[i]);
        if (v > this.peak) this.peak = v;
        if (this.onsetFrame < 0 && v > this.thresh) this.onsetFrame = currentFrame + i;
      }
    }
    return true;
  }
}
registerProcessor("capture", CaptureProcessor);
