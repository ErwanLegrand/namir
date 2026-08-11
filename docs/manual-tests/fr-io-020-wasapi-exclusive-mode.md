# FR-IO-020 manual test: WASAPI shared and exclusive mode

**Requirement (literal, Must):** "On Windows, WASAPI shall be supported in both shared and
exclusive mode. ASIO support is **Should**, and if included shall be built such that ASIO SDK
licensing does not contaminate the distribution of Namir's own source (NFR-LIC-040)."

**Verify: M.** Under D-18.6 a `Verify: M` Must is traced by this document and by nothing else — no
source annotation resolves it, and `xtask/src/traceability.rs:616-641` refuses a
`// trace-partial: FR-IO-020` outright. So this file is FR-IO-020's entire evidence, and the
Result section at the bottom is the whole of it.

**Scope note:** this script covers the exclusive-mode half. WASAPI **shared** mode has been
verified working against real hardware since M6 — see `fr-io-010-device-enumeration.md`'s executed
run — and is exercised on every launch that does not opt in below. ASIO is a Should and is not
built; it is out of scope here.

## History of this document

Until M11 this file recorded a **documented FAIL**: `cpal` 0.18.1 hardcoded
`AUDCLNT_SHAREMODE_SHARED` with no way to request `AUDCLNT_SHAREMODE_EXCLUSIVE`, verified against
that version's vendored source rather than inferred. That is no longer the situation. D-13.4 chose
a Namir-maintained fork of `cpal`; M11 built it, and `AppSettings::exclusive_mode` — added
forward-compatibly at M6 and read by nothing for five milestones — is now read.

The old text is deliberately not preserved here beyond this paragraph: it described an absence, and
the absence is gone. The reasoning survives in D-13.4 and in `docs/03-implementation-roadmap.md`
§18's M11 status.

## Before you start

**There is no user interface control for exclusive mode.** M11 added a mode *indicator*, not a
switch. This is a real limitation, not an oversight of this script: FR-IO-020 does not require a
chooser the way FR-IO-010 does ("the user shall be able to select"), so a persisted setting
satisfies its literal text — but resting on a hand-edited JSON key is thin, and it is one more
instance of the absent device panel that roadmap §15 item 16 carries as an open scope decision.

To enable it:

1. Launch Namir once and quit, so the settings file exists.
2. With Namir **closed** — it rewrites this file on exit — edit
   `%APPDATA%\Namir\audio-settings.json` and set `"exclusive_mode": true`.
3. Relaunch.

## What is under test, and what is genuinely unproven

Everything below has been verified only by unit tests against fakes, by `IsFormatSupported`
queries, or by a type-checker. **No part of the exclusive-mode path has ever moved real audio.**
Three pieces carry most of the risk and each has a step of its own:

- **The integer sample-format converter** (`crates/namir-app/src/audio_io/convert.rs`), never run
  against hardware. Exclusive mode does no format conversion, so Namir converts f32 to the device's
  native integer format itself.
- **Exclusive capture** (the fork's `process_input`), which had to stop using
  `GetNextPacketSize` — MSDN states it does not work on exclusive-mode streams. Written from that
  prose, never executed. **Its failure mode is silent: no error, simply no input.**
- **The `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` retry**, absent from upstream `cpal` and from PR #843,
  written fresh for this fork and never triggered.

## Reference-machine baseline

Measured on the §2 reference machine (Windows 11, PreSonus AudioBox 22VSL) with
`cargo run --example wasapi_exclusive_probe` from the fork, before the channel-mask fix:

- Both AudioBox endpoints accept **24-in-32 at 48 kHz stereo** in exclusive mode — but **only with
  a positional `dwChannelMask`**. With `dwChannelMask = 0`, which is what `cpal` sent, that format
  is refused. Fixed in fork commit `ab5f40a`.
- The device accepts **nothing at 44100, 88200 or 96000 Hz** in exclusive mode. It is 48 kHz-only
  in this configuration. Namir negotiates 48 kHz by default, so this happens to work — by luck
  rather than design, since the rate is settled from the *shared*-mode config set before the share
  mode is decided.
- `GetDevicePeriod` minimum is 3 ms = **144 frames at 48 kHz, a whole number**. So
  `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` is *unlikely* to fire on this device, and step 8 records that
  as an expected non-event rather than a pass.
- Realtek Digital Output and the AMD HDMI endpoint on the same machine engaged exclusive mode even
  with `dwChannelMask = 0` — Microsoft's own HD Audio driver accepts a zero mask. That contrast is
  why the defect was invisible until a third-party USB interface was tried.

## Script

Record the actual outcome of each step, including failures. A step that could not be run is
recorded as not run, with the reason.

1. **Probe.** Run `cargo run --example wasapi_exclusive_probe` from the pinned fork. Confirm the
   AudioBox blocks report `24-in-32 @ 48000` as `OK` under the positional mask. Paste the verdict
   lines.
2. **Namir's own view.** Run `cargo run --example list_devices -p namir-app`. The AudioBox should
   now read `exclusive mode, 2 ch: engaged at [48000] Hz`. Before `ab5f40a` it read `unsupported at
   any probed rate`.
3. **The stream actually opens.** With `"exclusive_mode": true`, launch Namir. `IsFormatSupported`
   succeeding does not mean `Initialize` will. Confirm the window opens, stderr reports the audio
   stream started, and the mode indicator in the top panel reads **exclusive** and names the device.
4. **Exclusivity itself — the defining observable.** While Namir holds the device, start playback
   from another application on the same endpoint. Windows must refuse it. **This refusal is the
   proof that exclusive mode engaged**; Namir merely not erroring is not. If the other application
   plays happily, the stream is shared no matter what the indicator says, and that is a failure of
   step 3, not of this step.
5. **Audio is correct through the converter.** Play guitar through the chain and listen for:
   plausible level (a scaling error halves or doubles it); clean peaks (a clamp error produces
   full-scale noise at exactly the loudest moment, not gentle distortion); no periodic clicking (a
   chunk-boundary error); correct channels. Compare against the same signal in shared mode.
6. **Capture works at all.** Confirm the input meter moves. This is the silent-failure path: if the
   `GetNextPacketSize` bypass is wrong, there is no error anywhere, the output is simply silent and
   the input meter dead.
7. **Fallback, and an honest indicator.** Select a device that refuses exclusive mode — the webcam
   microphone, or force 44.1 kHz where this interface refuses every format. Confirm: a notice
   appears naming the device and the reason, the indicator reads **shared**, and audio still works.
   Roadmap §18 forbids "a mode indicator that lies", and this is the only test of that claim.
8. **Buffer alignment.** Note whether `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` appeared at any point. On
   this device it is expected **not** to, per the baseline above. If it does not appear, that retry
   path remains unexercised and must be recorded as untested — a green run here is not evidence it
   works.
9. **Shared mode is unregressed.** Set `"exclusive_mode": false`, relaunch, confirm audio works as
   before and the indicator reads shared. The fork changes the shared path's channel mask nowhere,
   but this is the check that says so from outside.

## Executed run

*Not yet executed. To be filled in from a run on the `docs/02-architecture.md` §2 reference
machine — and from nowhere else: per AGENTS.md a sandbox or dev-machine result is informational
only and is never the evidence that closes a requirement.*

**Result: NOT EXECUTED.**
