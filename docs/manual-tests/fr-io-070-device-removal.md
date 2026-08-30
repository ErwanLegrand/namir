# FR-IO-070 manual test: device removal / open failure (R-5)

**Requirement (literal, Must):** "Device removal while in use, or a device failing to open, shall
be handled without crashing or hanging: the application shall report the condition, stop the
stream cleanly, and allow the user to select another device."
**Verify:** I with a virtual device that can be made to fail on demand.

**This is R-5** (`docs/02-architecture.md` §22's risk register: "FR-IO-070 device-removal handling
is weak in any cross-platform audio library... Test with a failable virtual device, not the happy
path") and D-13.1's own consequence note ("FR-IO-070 is the requirement most likely to expose gaps
in any cross-platform audio library... needs a real test with a device that can be made to fail,
not a happy-path test"). Both are explicit that a happy-path test does not satisfy this
requirement — recorded here rather than glossed over.

## What is built and automatically tested (the happy-path halves)

- **No device at all / negotiation fails outright:** `crate::app::run`'s own fallback path
  (`open_window_without_audio`) is reached whenever `setup_direction` returns `None` for either
  direction, or `PrepareContext`/`build_default_engine` fail. Not unit-tested directly (it is
  integration/wiring code — see `app.rs`'s own module doc comment for why this crate's tests focus
  on the pieces underneath it instead), but every one of its own preconditions
  (`device_state::select_device` returning `None` on an empty device list) is tested.
- **Stream open failure reported, not crashed:** `crate::app::run`'s `stream::open`/`.play()`
  error arms call `host.report(DEVICE_OPEN_FAILED, ...)` rather than `.unwrap()`/panicking — no
  automated test exercises this specific arm with a *real* failure (see below for why), but the
  surrounding notice-reporting mechanism (`AppHost::report`, `UiNotice` construction/dismissal) is
  tested in `crates/namir-app/src/host.rs`'s `a_load_failure_surfaces_as_a_notice` and
  `dismiss_notice_removes_only_the_named_notice`, which exercise the identical code path for a
  different failure source.
- **A running stream's error callback (device lost mid-session):** `crate::stream::open`'s
  `on_failure` callback is invoked by `cpal`'s own error callback and, in this crate's wiring
  (`crate::app::run`), routed to `AppEvent::StreamFailure` → `AppHost`'s notice list. This exact
  routing is proven end-to-end with a hardware-free fake backend in
  `crates/namir-app/src/stream.rs`'s test module (`captured_input_reaches_the_output_buffer...`'s
  sibling tests all construct the same `on_failure` wiring `crate::app::run` uses, just with a
  `FakeBackend` standing in for `cpal`).

## What is not built: R-5's own literal ask — a real failable device

A synthetic/virtual audio device that can be told, on command, to disconnect or refuse to open
does not exist in this crate, and none of Windows/Linux/macOS ships one usable from a plain
integration test without additional tooling (Windows: no built-in "fail on demand" WASAPI device;
Linux: ALSA's `null`/`loop` plugins simulate a device but don't simulate *failure*; macOS:
CoreAudio's aggregate-device APIs could construct one but need Apple-platform-specific work this
Windows-authored session could not attempt). Building or acquiring one is out of this session's
scope — recorded as R-5's own residual risk, unresolved by this milestone's namir-app build,
exactly as the risk register already anticipated it might remain until someone builds or finds
such a device.

## Script, once a failable device is available (or by physically unplugging real hardware)

1. **Open failure:** configure the failable device to refuse `IAudioClient::Initialize`/
   equivalent, select it, and confirm `namir` starts, shows a `DEVICE_OPEN_FAILED` notice, and the
   window remains usable (not frozen) rather than crashing.
2. **Removal while in use:** start a real stream against a real, unpluggable USB audio interface
   (the AudioBox 22VSL this session used for `fr-io-010-device-enumeration.md`'s executed run would
   serve), then physically unplug it while audio is flowing. Confirm: the process does not crash or
   hang, a notice is shown naming which side (input/output) was lost
   (`crate::worker::AppEvent::StreamFailure`'s `direction` field), and the stream is stopped
   cleanly (no repeating error spam, no runaway CPU).
3. Confirm the user can then select a different device and resume — this is the one sub-clause
   this crate's own known gap (`fr-io-010-device-enumeration.md`'s "no interactive device-selection
   UI") means cannot currently be exercised at all: today, recovering from step 2 requires
   restarting the application, which will renegotiate devices fresh (falling back automatically,
   per FR-IO-080) but is not the same as "select another device" from within a running session.

**Result: NOT EXECUTED this session against a real failable device (none available).** The
surrounding report/stop-cleanly machinery is built and tested against every piece that does not
require a device capable of failing on command; R-5's own specific ask (a device that can be made
to fail) remains open, and the "select another device without restarting" sub-clause is a known
gap independent of any failable-device availability.

## Executed run — step 2, by physical unplug, 2026-08-27

**Step 2 was executed on SALON**, not against a failable virtual device but by physically
unplugging the AudioBox 22VSL while audio was flowing. It was executed *incidentally*: it is
`docs/manual-tests/fr-ui-070-non-modal-error-notices.md`'s step 8, which covers how the resulting
error is **surfaced**, and this document owns the device-removal behaviour itself. Recording it
here rather than leaving this file reading NOT EXECUTED, since the observation is this document's
even though the induction happened elsewhere. **Steps 1 and 3 remain not executed** — step 1 still
needs the failable device R-5 asks for, and step 3 cannot be executed at all, for the reason its
own text gives.

**What held.** The process did not crash or hang. The window stayed functional and closable, and
parameters remained editable. No repeating error spam and no runaway CPU followed: exactly two
notices appeared and then nothing further. Audio stopped, which on this induction is the hardware's
doing rather than the application's.

**What did not hold: the notice names neither the side nor the device.** Step 2 asks for "a notice
naming which side (input/output) was lost (`crate::worker::AppEvent::StreamFailure`'s `direction`
field)". Two notices appeared, verbatim and identical:

```
app.audio_io.device_lost: The {direction} device "{device}" became unavailable and the stream was
stopped. (Other("OS Error -2004287450 (FormatMessageW() returned error 317) (os error -2004287450)"))
```

The `direction` field does exist and *is* consulted — `stream_failure_code(direction)` uses it to
pick the code — but neither it nor the device name is rendered, and the detail carries cpal's raw
error instead. So the two notices are indistinguishable from each other, and a user cannot tell
which side was lost. The information exists in the program and is dropped at the last step; see
FR-UI-070's own executed run, finding 1, for why this specific case matters to GitHub issue #15's
severity assessment.

**A direct data point for R-5, and it is the one R-5 predicted.** `CpalBackend`'s
`to_stream_failure` maps `cpal`'s `DeviceNotAvailable`/`HostUnavailable` to
`StreamFailure::DeviceLost` (`crates/namir-app/src/audio_io.rs:526`). **This unplug did not produce
either.** It produced `StreamFailure::Other`, carrying an unmapped OS error whose own message
formatting had failed (`FormatMessageW() returned error 317`). R-5's wording is that "FR-IO-070
device-removal handling is weak in any cross-platform audio library"; this is that weakness
observed rather than anticipated — on WASAPI, a physical unplug does not reliably arrive as a
device-lost classification.

Namir reported it correctly anyway, but by luck rather than by design: the notice code is chosen
from the `direction`, not from the classification, so an `Other` failure is reported as
`device_lost` regardless. The same path would report an unrelated stream error as a device loss.
Two changes suggest themselves and neither is this document's to make — widen
`to_stream_failure`'s mapping now that a real unmapped code is known, and choose the notice code
from the classification rather than only the direction.

**Step 3 is now observed rather than inferred.** Re-plugging the interface did not resume capture
or output. Device selection happens once at startup from `audio-settings.json`
(`crates/namir-app/src/app.rs:217`), and there is no device-selection surface anywhere in
`namir-ui` or `namir-app` — no combo box, no `UiIntent`, nothing. Recovering from a removal
requires editing JSON or restarting. FR-IO-070's third clause — "allow the user to select another
device" — is **unbuilt**, which this file already recorded as a known gap; what is new is that a
person has now watched it not happen on real hardware. FR-UI-010's "differing only in the presence
of the audio-device panel" presumes a panel that does not exist either.

**Result: PARTIAL.** Step 2 executed 2026-08-27, and it fails its naming clause while passing the
crash/hang/clean-stop clauses. **Steps 1 and 3 remain NOT EXECUTED**, step 1 for want of a failable
device (R-5's residual risk, unchanged) and step 3 because the capability it exercises is unbuilt.
FR-IO-070 as a whole is therefore **not met**: the requirement's "allow the user to select another
device" clause has no implementation, and its "report the condition" clause reports a condition
that names neither the device nor the side.

---

## Appended 2026-08-29 — the failable device now exists, and what it does and does not replace

Recorded as an addition, not a re-verdict: **no verdict line above is edited**, and no manual step
was re-run this session. What changed is the apparatus, in response to GitHub issue #24 ("FR-IO-070's
stated verification apparatus — a failable virtual device — does not exist and has no owner").

**Route 1 of that issue is built.** `crates/namir-app/src/stream.rs`'s `FakeBackend` — already the
crate's no-hardware backend, and already capturing each direction's `cpal` error callback since
issue #88 — can now be made to fail on demand in both of the shapes FR-IO-070's first sentence
names: `FakeBackend::failing_to_open(direction)` refuses an open, and the captured
`input_error`/`output_error` callbacks let a test induce a failure *while the stream is in use*.
What a stream then did is readable from `FakeStreamLog` (plays, pauses and — the observable that
mattered — stops, counted as drops, since dropping is what stopping a stream is in this crate).

This makes the requirement's stated method (`I` with a virtual device that can be made to fail on
demand) literally executable, on a headless container, on every merge. It needs no OS device
manipulation and no `#[cfg(target_os)]`, which D-5.1 forbids outside `namir-platform` anyway.

**What is now automated, and where.** All three tests are in `crates/namir-app/src/host.rs`:

1. `a_device_lost_mid_stream_is_reported_and_stops_both_streams_cleanly` — opens the duplex path
   through the production `crate::stream::open`, plays it, drives four output callbacks so the
   failure is genuinely mid-stream, then fires the output error callback with **this document's own
   step-2 transcript verbatim**, as `StreamFailure::Other`, which is the shape it really arrived in.
   Asserts: no crash or hang; exactly one `app.audio_io.device_lost` notice naming the side *and*
   the device; and both directions' streams stopped exactly once, after the report and not before.
2. `a_device_that_fails_to_open_reports_and_leaves_no_half_open_stream` — step 1's condition, at
   last inducible. The output open is refused after the input stream has been built, and what is
   asserted is the teardown: the half-open capture stream is stopped rather than leaked, the
   callbacks a refused open was handed are not retained, and the caller gets a reportable
   `app.audio_io.device_open_failed` rather than a panic.
3. `after_a_loss_the_next_launch_selects_another_device` — the restart-mediated continuation.

**Two behaviour changes came out of writing them**, both recorded here because this document's step
2 is the evidence for both:

- `to_stream_failure` now maps `cpal::ErrorKind::StreamInvalidated` to `StreamFailure::DeviceLost`.
  The note above says the unplug produced an error `cpal` had not classified. Reading the pinned
  fork's own source says otherwise: its WASAPI `From<windows::core::Error>` maps
  `AUDCLNT_E_RESOURCES_INVALIDATED` — precisely the code in the transcript — onto
  `ErrorKind::StreamInvalidated`, a kind Namir's `match` did not name. M14's message-substring
  recovery rescued this particular case only because the message happened to carry the raw OS
  number; the fork's `default_device_change_error` returns the same kind with *no message at all*,
  which no substring can reach.
- **The stream is now actually stopped.** `app.audio_io.device_lost`'s catalogue text has said "the
  stream was stopped" since M14 while nothing stopped it. `AppHost` owns the `RunningStreams` and
  drops them on a device-loss classification — and on that classification only, since `cpal`
  reports survivable conditions (`RealtimeDenied`, `DeviceChanged`) through the same callback.

**What this does not replace.** Three things stay this document's, and step 1's and step 3's
**NOT EXECUTED** stand:

- The virtual device is virtual. It proves Namir's response to a failure report; it proves nothing
  about *what a real OS and a real driver do* on a physical removal — which is R-5's actual subject,
  and why step 2's transcript above is still the only evidence of that and still worth re-running on
  hardware.
- Step 1's real form (a real device refusing a real open — exclusive contention, a disabled
  endpoint) is untouched: what is automated is Namir's teardown given a refusal, not any OS's
  refusal.
- Step 3 is **unbuilt, not merely unexecuted**. There is still no device-selection surface in either
  shell (issue #26, roadmap §15 item 16), so FR-IO-070's third clause has no implementation for any
  test to reach. Test 3 above asserts the restart-mediated substitute and says so; the requirement's
  `// trace-partial:` tag names this clause as its remaining gap and is deliberately not promoted.
