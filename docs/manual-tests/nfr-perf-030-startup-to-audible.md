# NFR-PERF-030 manual test: start-up to an audible state, confirmed by ear

**Requirement (literal):** the standalone application shall reach an audible state (audio
streaming, default state loaded) within 3 seconds on the reference machine with a warm library
index.
*Verify: B.*

## This document is the supplementary half, not the traced artifact

D-18.6: only a `Verify: M` Must is *traced* by its manual document. NFR-PERF-030 is `Verify: B`,
so its traced artifact is and stays the benchmark —
`crates/namir-app/benches/startup_to_audible.rs`, which asserts the 3 s ceiling in-process across
at least five launches of the real `namir` binary. This file is the residue that benchmark's own
`// uncovered:` field names, and nothing more.

## What the benchmark proves, and the one thing it cannot

The benchmark times a real process from immediately before `Command::spawn` to the instant
`crate::stream::RunningStreams::play` returns `Ok(())` — the call that crate's own doc comment
calls "the one call that actually makes audio flow" — with the requirement's two stated
preconditions checked rather than assumed (the launch reports the size of the index it read and
the number of parameters `State::defaults()` built, and both are asserted).

What it cannot do is confirm that sound left the interface. `play()` returning `Ok(())` is the
instant both streams are *told* to run; no output callback is observed to have processed a block,
so a build that started its streams and then produced silence would still be timed as having
reached an audible state. The stronger marking event — waiting for the first output callback —
was considered and rejected, because it would put an observable inside `crate::stream`'s audio
callback, on this project's single most-reviewed path, purely to enable a measurement. That
trade is recorded here rather than hidden: the automated half gets the clock, a human gets the
ear.

## Script

Run on `docs/02-architecture.md` §2's pinned reference machine, with a real audio interface
connected and monitoring audible, and with a guitar or DI source plugged into the selected input.

1. Launch `namir` (release build) with a warm library index — that is, having launched it at least
   once before against the same configuration directory, so the index file exists and no scan is
   pending. Start a stopwatch as you launch it.
2. Play the instrument continuously from before the launch.
3. Record the wall-clock moment sound first passes through. Confirm it is under 3 seconds and that
   the sound is the processed signal, not a passthrough from the interface's own direct monitoring
   — mute the interface's direct monitoring first if it has any.
4. Confirm the window's meter (FR-UI-020) shows activity, and that the parameters shown are the
   documented defaults (FR-STATE-020) rather than a recalled state.
5. Repeat five times, per D-2.4, and record the slowest.
6. Cross-check against the benchmark's own figure for the same machine:
   `cargo bench -p namir-app --bench startup_to_audible`. The by-ear figure should be at or a
   little after the benchmark's, never before it: the benchmark's marker is emitted at
   `play()`, and a human's perception of first sound cannot precede it.

## Executed run (this session, 2026-08-11)

**Partially executed. The by-ear step was not executed and cannot be by an agent session.**

Executed, on the §2 reference machine (AMD Ryzen 9 5950X, 63.9 GB, Windows 11 build 26200), with a
PreSonus AudioBox 22VSL as both input and output device:

- The benchmark, four times (5, 10, 5 and 5 measured launches, each after a discarded warm-up),
  with a planted 10 000-entry warm library index. All 25 measured launches reached their audible
  marker; slowest observed **485.24 ms** against the 3 s ceiling, min 440.86 ms, and every run's
  own spread under 45 ms. Informational, not certified: this machine is the reference machine but
  was not verified quiet — this session's own agent processes were running throughout.
- The **not-audible** branch, twice, unintentionally and then deliberately: with a second `namir`
  already holding the AudioBox, a probed launch reported
  `reason=stream-not-started detail=... Failed to initialize audio client: OS Error -2004287478`
  (`AUDCLNT_E_DEVICE_IN_USE`). The harness refused to produce a timing figure for it and said why,
  which is the behaviour that branch exists for. Worth knowing when running the script above: close
  any other Namir instance first, because on this interface a second one does not get the device.
- An ordinary launch (no probe), left running for six seconds and then terminated. Its stderr
  carried `namir: audio stream started`, the negotiated-format line
  (`48000 Hz, 480-frame buffer, ~20.0 ms estimated round-trip latency`), and then
  `namir: xrun count is now 2 (session total, FR-IO-060)`. That last line is worth recording as
  evidence rather than noise: the xrun counter is incremented by `crate::stream`'s **output
  callback** pulling from the bridge, and the logger only prints when the count changes — so the
  output callback demonstrably ran and processed blocks. It is not a substitute for hearing the
  signal (it says the callback ran, not that the result was correct or audible at the speaker),
  but it is one step past what the benchmark's marker alone establishes.

Not executed: steps 2, 3, 4 and 6's comparison. This agent session has neither ears nor a
guitar — the same limitation `docs/manual-tests/fr-ui-010-standalone-window-renders.md` records
for the window itself. The script above is ready to run by a person at that machine.
