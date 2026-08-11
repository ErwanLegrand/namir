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

1. **Probe — the device's own answer, not Namir's.** Run
   `cargo run --example wasapi_exclusive_probe` from the fork. Confirm the AudioBox blocks report
   `24-in-32 @ 48000` as `OK` under the positional mask.

   **This step cannot verify any Namir or `cpal` code, and must not be read as doing so.** The
   example builds every `WAVEFORMATEXTENSIBLE` by hand and never calls
   `config_to_waveformatextensible`, so its output is identical on every revision of the library —
   it characterises the *endpoint*. It is step 2 that tests the fix. (This was got wrong once
   already: the example asserted "cpal hard-codes mask 0, this is a cpal bug" for two commits after
   that was fixed, and a run of it looked like confirmation. Corrected in fork commit `9970fb4`.)

2. **Namir's own view — this is the step that tests the channel-mask fix.** Run
   `cargo run --example list_devices -p namir-app`. The AudioBox should read
   `exclusive mode, 2 ch: engaged at [48000] Hz`. Before fork commit `ab5f40a` it read
   `unsupported at any probed rate`, because `cpal` sent `dwChannelMask = 0` and this endpoint
   refuses that — leaving only `I16`, which `namir-app` excludes by policy. This path runs through
   the real library, so a change here is real evidence.
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
7. **Fallback, and an honest indicator.** There is no device-selection surface either — the same
   §15 item 16 gap — so this is driven from the settings file. With Namir closed, set
   `"input_device_name"` to a device that refuses exclusive mode, keeping `"exclusive_mode": true`:

   ```json
   "input_device_name": "Microphone (Trust 1080p HD Webcam)",
   ```

   `device_state::select_device` matches on **exact string equality** and silently falls back to the
   default device on any mismatch, so the name must be copied verbatim from `list_devices` — a typo
   tests nothing and looks like a pass. Forcing a rate instead does **not** work: this interface
   reports only 48 kHz in shared mode, so `negotiate_sample_rate` never offers anything else.

   Confirm: a notice appears naming the refusing device and the reason, the indicator reads
   **shared**, and audio still works. Roadmap §18 forbids "a mode indicator that lies", and this is
   the only test of that claim — an all-or-nothing session where one endpoint accepts exclusive and
   the other does not is exactly where an indicator would be tempted to lie.
8. **Buffer alignment.** Note whether `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` appeared at any point. On
   this device it is expected **not** to, per the baseline above. If it does not appear, that retry
   path remains unexercised and must be recorded as untested — a green run here is not evidence it
   works.
9. **Shared mode is unregressed.** Set `"exclusive_mode": false`, relaunch, confirm audio works as
   before and the indicator reads shared. The fork changes the shared path's channel mask nowhere,
   but this is the check that says so from outside.

## Executed run — reported by the repository owner, 2026-08-11

Run on the `docs/02-architecture.md` §2 reference machine (Windows 11 Pro build 26200), against a
**PreSonus AudioBox 22VSL** — a 2-in/2-out, 24-bit USB interface — with the fork at its pinned
revision `2edbacb`. Every step was performed; step 8's condition never arose, so the path that step
exists to observe is recorded as unexercised rather than as passing. Nothing here was measured
anywhere else: per AGENTS.md a sandbox or dev-machine result is informational only and is never the
evidence that closes a requirement.

**The "What is under test, and what is genuinely unproven" section above was written before this run
and is superseded by it rather than deleted.** Of the three pieces it names as carrying most of the
risk: the integer sample-format converter was exercised and was **wrong** until fork commit
`2edbacb`; exclusive capture was exercised and was broken until that same fix, though **not for the
reason that section predicted** — the `GetNextPacketSize` bypass was fine and the container
convention was not; and the `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` retry **still has never executed**,
see step 8. Two of the three carried a real defect. Guessing *where* the risk sat was worth doing;
guessing *what* it would be was not, which is an argument for the script's per-step observations
rather than for its predictions.

**Step 1 — probe. Executed.** The AudioBox accepts `24-in-32 @ 48 kHz stereo` in exclusive mode
**only with a positional `dwChannelMask`** (`0x3`, `FL|FR`); with `dwChannelMask = 0` the same format
answers `AUDCLNT_E_UNSUPPORTED_FORMAT`, every other field identical. `I16` is accepted at 48 kHz
under **either** mask — which is why the pre-`ab5f40a` library saw a device that appeared to support
exclusive mode only in the one format `namir-app` excludes by policy, rather than one that refused
outright. **Nothing at all** is accepted at 44100, 88200 or 96000 Hz: this interface is 48 kHz-only
in exclusive mode. `GetDevicePeriod`'s minimum is 3 ms = **144 frames at 48 kHz, a whole number**.
Realtek Digital Output and an AMD HDMI endpoint on the same machine engaged exclusive mode even with
mask 0. These readings are the ones the baseline section above already records, unchanged — which is
what that section's own warning predicts, the example building its formats by hand and so
characterising the endpoint rather than any revision of the library.

**Step 2 — `list_devices`. PASS.** Both AudioBox endpoints read `exclusive mode, 2 ch: engaged at
[48000] Hz`. Before fork commit `ab5f40a` the same command read `unsupported at any probed rate`.
This is the step that puts the channel-mask fix through the real library, and it is the one that
moved.

**Step 3 — the stream opens. PASS.** With `"exclusive_mode": true` the window opened and the mode
indicator in the top panel read `Namir Exclusive mode — Haut-parleurs (AudioBox 22VSL)`
(`namir_ui::app::audio_mode_label`, beside the heading).

**Step 4 — exclusivity itself. PASS.** Firefox could not play audio while Namir held the device.
This is the defining observable, not the indicator: Windows refusing the second application is what
says `AUDCLNT_SHAREMODE_EXCLUSIVE` actually reached `Initialize`.

**Step 5 — audio correct through the converter. PASS, after fork commit `2edbacb`.** Before that
commit the output was audible only at high volume — roughly 48 dB down, i.e. a factor of 2^8, from
24 valid bits being written right-aligned into a container WASAPI reads left-justified. After the
fix: plausible level, clean peaks, no periodic clicking, correct channels, comparable with the same
signal in shared mode.

**Step 6 — capture. PASS, after `2edbacb`.** Before it, the same defect ran the other way: the input
meter moved only on near-silent input and pinned on anything meaningful — 256x too large. The
`GetNextPacketSize` bypass this step was written to catch was not the problem; the container
convention was. After the fix the input meter tracks the instrument normally.

**Step 7 — fallback, and an honest indicator. PASS, with a defect found in the notice text.** With
`"input_device_name"` set to the webcam microphone — which refuses exclusive mode — and
`"exclusive_mode"` still `true`, the all-or-nothing rule (`crate::app::negotiate_share_mode`)
refused exclusive for the whole session rather than running half-exclusive: the indicator read
`Namir Shared mode — Haut-parleurs (AudioBox 22VSL)`, a notice appeared naming the refusing device,
and audio worked. §18's "not a mode indicator that lies" holds.

**But the notice rendered its template placeholders literally:**

```
app.audio_io.exclusive_mode_unavailable: Exclusive mode is not available for {device} ({reason});
using shared mode. (input "Microphone (Trust 1080p HD Webcam)"; the audio backend reports no
exclusive-mode support for this device and format; continuing in shared mode)
```

`{device}` and `{reason}` reach the user unsubstituted. `namir_ui::notices::notice_text`
(`crates/namir-ui/src/notices.rs:43-47`) formats `{code.id}: {message_template} ({detail})` and
nothing in the workspace substitutes into `message_template` at all, so this has been true of every
placeholder-bearing catalogue entry since the mechanism landed at M5. Filed as **issue #15**, which
tallies 30 such entries across seven crates. It is pre-existing, workspace-wide, not caused by M11
and **deliberately not fixed here** — M11's own new entry is left worded like its neighbours so all
of them can be reworded in one consistent pass. The information a notice is required to state — what
failed, which device it concerned, and where that leaves the user — is present: the `detail` string
carries all three. What is wrong is the presentation. (That requirement is named in issue #15 rather
than by identifier here, deliberately: `xtask traceability` resolves a `Verify: M` Must to the first
`docs/manual-tests/*.md` in filename order that names it, so an id mentioned in passing in this file
re-points another requirement's evidence at this document. Verified by doing it accidentally while
writing this section.)

**Step 8 — buffer alignment. NOT EXERCISED.** `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` never appeared at
any point, exactly as step 1's whole-number device period predicts. **The retry path in the fork
therefore remains code that has never executed anywhere — not in a test, not on hardware.** Recorded
as untested, not as passing; a green run here is the expected non-event this step was written to
name, and is not evidence the path works.

**Step 9 — shared mode unregressed. PASS.** With `"exclusive_mode": false` the app behaves as it did
before the fork and the indicator reads shared. **The limit of what that proves, precisely:**
`namir-app` restricts shared mode to `F32` (`crates/namir-app/src/audio_io.rs:453`), so what this
exercised is the fork's `container_shift`/`padding_bits` path returning **zero** for a container that
is exactly full. **Shared-mode `I24` remains unexercised, and Namir cannot reach it** — no setting in
this product asks shared mode for an integer format. The fork's container-justification fix is
correct for shared-mode `I24` by the format contract (`wBitsPerSample` vs `wValidBitsPerSample`, read
off the `WAVEFORMATEXTENSIBLE` handed to `Initialize` rather than off the `SampleFormat`), and by
that alone — not by measurement.

**Result: PASS.** WASAPI is supported in **both** shared and exclusive mode on real hardware on the
§2 reference machine: exclusive mode opens, is genuinely exclusive, carries correct audio in both
directions through the integer converter, falls back to shared with a truthful indicator when a
device refuses it, and leaves shared mode unchanged. That is the whole of FR-IO-020's Must clause;
ASIO is the requirement's Should and is not built (see the scope note at the top).

**Carried forward as unverified, so nobody reads more into this PASS than it holds:**

- **The alignment-retry path** (step 8) has never run. It closes when a device with a fractional
  device period is available, or when a test can drive `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` directly.
- **Shared-mode `I24`** (step 9) is unreachable from this product and rests on the format contract.
- **Packed 24-bit (a 3-byte container) cannot be expressed by `cpal` at all** —
  `SampleFormat::I24`'s `sample_size()` is `size_of::<i32>()`, so every format this backend *builds*
  has a 32-bit container even though its parser maps a device-reported `wBitsPerSample == 24` to the
  same `I24`. A device offering only packed 24-bit would have every candidate refused and would fall
  back to shared. This endpoint offers 24-in-32, so the case did not arise here.
- **One machine, one third-party interface.** The two defects this run found were both invisible on
  the Microsoft HD Audio endpoints of the same machine; a second interface from a different vendor
  would be worth more than a second run on this one.
