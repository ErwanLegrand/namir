# FR-IO-050 manual test: round-trip latency, measured vs. driver-reported

**Requirement (literal, Must):** "The application shall display the measured round-trip latency,
or the driver-reported latency where measurement is not possible, in both samples and
milliseconds."

**Verify: M.**

## What is built and automatically tested

[`crate::latency::estimate_round_trip`] computes the *buffer-based* figure (input buffer frames +
output buffer frames, converted to ms at the negotiated sample rate) — FR-IO-050's own second
clause, "driver-reported latency where measurement is not possible." Its arithmetic is fully unit
tested (`crates/namir-app/src/latency.rs`, 5 tests) and was exercised for real in this session:
the executed run in `fr-io-010-device-enumeration.md` printed `~20.0 ms estimated round-trip
latency` for a real 480-frame buffer at 48 kHz on each side (480 + 480 = 960 samples = 20.0 ms —
matches the formula exactly).

## What is not built: a true *measured* loopback figure

FR-IO-050's first clause ("measured round-trip latency") means playing a known impulse out through
the output device and timing its arrival back on the input device — the actual round trip through
the OS mixer, the driver, and the hardware's own buffering, which is *not* fully captured by
`buffer_frames × 2` (WASAPI shared mode in particular adds its own internal buffering beyond the
requested period, which `cpal` 0.18.1 does not expose a portable way to query — see
`crate::latency`'s own module doc comment). Building this needs:

- A physical or virtual loopback path (a cable from a line output back into a line input, or a
  virtual audio cable driver on the test machine).
- Playing a sharp, recognisable impulse and detecting its arrival on the input side with
  sample-accurate timing.

Neither exists in this crate. This is inherently a real-hardware procedure and cannot be automated
in CI or exercised meaningfully without physical (or virtual-cable) loopback wiring — recorded here
as a scoped-out Must rather than silently skipped.

## Script, once loopback hardware/wiring is available

1. Connect the selected output device's output directly to the selected input device's input
   (a physical cable, or a virtual loopback device configured at the OS level).
2. Play a short, sharp impulse (a single-sample click, or `namir-fixtures`' own noise-burst
   generator repurposed for this) through the output.
3. Detect the impulse's arrival time on the input side (cross-correlation or a simple threshold
   detector) and compute the elapsed sample count since it was sent.
4. Compare against `crate::latency::estimate_round_trip`'s buffer-based figure for the same
   configuration — the measured figure should be equal to or somewhat larger than the estimate
   (the estimate is a lower bound; real hardware/driver buffering adds more).
5. Confirm the application labels which kind of figure ([`crate::latency::LatencyReport::measured`])
   is being shown, so a user is never told an estimate is a measurement.

**Result: PARTIAL.** The driver-reported/estimated half (FR-IO-050's own fallback clause) is built,
tested, and verified working against real hardware. The measured half needs loopback hardware this
session did not have and is not built.
