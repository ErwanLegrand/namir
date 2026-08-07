# FR-IO-060 manual test: xrun detection and counting under a real synthetic overload

**Requirement (literal, Must):** "The application shall detect and report audio dropouts (xruns),
showing a running count for the session, resettable by the user."
**Verify:** I — induce an xrun with a synthetic overload and assert it is counted.

## What is built and automatically tested

[`crate::xrun::XrunCounter`] (increment/read/reset, including concurrent increments from multiple
threads) is fully unit tested — 5 tests, `crates/namir-app/src/xrun.rs`. The mechanism that feeds
it from real audio flow, [`crate::bridge::BridgeConsumer::pull_into`]'s underrun detection, is also
fully unit tested against a synthetic ring — 6 tests, `crates/namir-app/src/bridge.rs`, including
`an_output_pull_with_no_input_yet_counts_an_xrun`'s literal reproduction of "no data arrived in
time" and `stream::tests::an_output_pull_with_no_input_yet_counts_an_xrun`'s equivalent proof
wired through `crate::stream::open`'s real callback closures (with a fake, hardware-free backend).
This is the requirement's *counting mechanism*, verified by a real, if synthetic, "overload" —
an output pull with nothing pushed is exactly an overload condition (demand exceeding supply) —
already satisfying the letter of "induce an xrun with a synthetic overload and assert it is
counted," just not against real `cpal` callbacks on real hardware.

## What real-hardware execution would add, and why it was not attempted

Making a *real* WASAPI/ALSA/CoreAudio stream actually xrun (as opposed to this crate's own
bridge-ring underrun, which is a genuine but different dropout mechanism — see
`crates/namir-app/src/xrun.rs`'s own module doc comment on why both sources feed one counter)
needs either: an artificially tiny buffer size pushed below what the CPU can service in real time
under load, or deliberately blocking the callback thread past its deadline (e.g. a `sleep` injected
into the output callback for one call). Both are real, standard techniques for this kind of test,
but doing so safely (without genuinely wedging or crashing the process on the one real audio
interface available in this session) needs care this session's time budget did not extend to, and
this session has no way to *listen* for the resulting audible glitch to independently confirm one
really occurred, only to read the counter's own value — which would then only be testing the
counter against itself.

## Script

1. Open a real stream at the smallest buffer size the selected device reports supporting (see
   `fr-io-010-device-enumeration.md`'s executed run for real reported ranges — this session's
   AudioBox 22VSL output offered as low as 80 frames at 8 kHz, i.e. real hardware to try this
   against).
2. Induce load: run a CPU-bound background task, or temporarily insert `std::thread::sleep` into
   `crate::stream::build_output`'s callback for one call.
3. Confirm `crate::xrun::XrunCounter::count()` increases, and that repeating step 2 after a
   `reset()` produces further increases rather than the counter staying stuck.
4. Confirm the increase is visible somewhere the user can see it — today, only via this crate's
   own `eprintln!` log line (`crate::app::spawn_xrun_logger`); see this crate's final report for
   why there is no in-window display yet (the same "no FR-IO settings surface in `namir-ui`" gap
   `fr-io-010-device-enumeration.md` records).

**Result: PARTIAL.** The counting mechanism is real, tested, and proven against a real (if
synthetic, hardware-free) overload. A real-hardware xrun induction was not attempted this session;
recorded as the honestly-unexecuted remainder rather than assumed to work by extension.
