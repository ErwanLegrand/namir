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
