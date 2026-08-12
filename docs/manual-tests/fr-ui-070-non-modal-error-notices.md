# FR-UI-070 manual test: errors surfaced non-modally, with audio never interrupted

**Requirement (literal, Must):** "Errors shall be surfaced non-modally and shall never interrupt
audio. An error shall state what failed, which file or device it concerned, and what the user can
do."
**Verify:** M against the error catalogue of FR-ERR-020.

## This document is the traced artifact, not supplementary evidence

D-18.6: a `Verify: M` Must is *traced* by its manual document, and only by it. This file carries
FR-UI-070's coverage on its own — there is deliberately no source `// trace: FR-UI-070` anywhere,
because a plain tag would over-claim (no automated test hears audio or observes a modal), and
`xtask traceability` refuses a `// trace-partial:` naming a `Verify: M` requirement outright. The
tool finds this file by its filename prefix (`fr-ui-070-`), so the prefix is load-bearing and must
not be renamed.

## What is already true mechanically, and what it does not reach

- **Non-modal, structurally.** `crates/namir-ui/src/notices.rs`'s `render` draws each notice as one
  inline `ui.horizontal` row inside the top panel's normal layout flow, each with its own `Dismiss`
  button. There is no `egui::Window`, no modal area and no `Order::Foreground` overlay anywhere in
  the crate. `dismissing_a_notice_emits_its_own_id_not_anothers` drives a real synthetic click onto
  the second of two notices' buttons and asserts only that notice's `DismissNotice` intent is
  emitted.
- **Never interrupts audio, by construction.** `namir-ui` cannot depend on `namir-engine` or
  `namir-worker` at all (D-5.1, enforced by `xtask layering`), so no code path in the view layer can
  name an audio thread, let alone block one. Notices are pushed on the worker/main side
  (`AppHost::push_notice`, `crates/namir-app/src/host.rs:192`) and only ever *read* by the view.
- **Catalogue-backed.** Every notice carries a `namir_core::ErrorCode` — a stable id, a severity and
  a message template (FR-ERR-020) — never a free-formatted string; `notice_text` renders
  `{code.id}: {message_template} ({detail})`.

What none of that reaches: whether a real user, with sound coming out of a real interface, sees the
notice appear without anything stealing focus or blocking a click, and hears no gap. "Non-modal" is
a claim about what the window does to the person in front of it, and "never interrupts audio" is a
claim about what the ear hears — neither is observable from a headless `run_ui` harness.

**One thing this script must observe rather than assume.** `notice_text` interpolates the code's
`message_template` **verbatim**: it substitutes nothing into the template's own `{path}`,
`{reason}`, `{device}`, `{direction}` placeholders, and appends the caller's free-text `detail` in
parentheses instead. So what appears on screen for, say, a stream failure is built from
`"The {direction} device \"{device}\" became unavailable and the stream was stopped."` plus a
detail string. Whether the resulting line satisfies the requirement's second sentence — *what*
failed, *which* file or device, and *what the user can do* — is exactly what steps 4, 8 and 12
below ask you to read off the screen and record verbatim. Do not paraphrase the text you see;
transcribe it.

**This is already a known, tracked defect — record it, do not re-file it.** GitHub issue **#15**,
"Notice text renders {placeholder} literally — no code substitutes ErrorCode::message_template",
was raised from M11's FR-IO-020 manual run against exactly this behaviour and carries the full
analysis: **33 placeholder-bearing `message_template` values in production code across five
crates**, the reason it survived (this module's doc justifies the *shape* of
`{code.id}: {message_template} ({detail})` but never addresses the placeholders inside the
template, and `notice_text`'s unit test asserts the concatenation contract rather than a rendered
example a reader would recognise as wrong), and two candidate fixes. That issue's own assessment of
requirement impact is worth carrying into whatever verdict this script reaches: the `detail` string
does carry the file or device, so this is presentation quality rather than a missing capability —
"but a user-facing string containing `{device}` is not shippable". Transcribing the literal tokens
here adds a second, independent observation to that issue; deciding what to do about it is not this
document's business and is deliberately not M9b's.

## Preconditions

- A display, a real audio interface with monitoring audible, and an instrument or signal generator
  feeding the selected input — the "never interrupt audio" clause is checked by ear and cannot be
  checked any other way here.
- The configuration directory: `%APPDATA%\Namir` on Windows,
  `~/Library/Application Support/Namir` on macOS, `$XDG_CONFIG_HOME/namir` (or `~/.config/namir`)
  on Linux. It holds `audio-settings.json`, `library-index.json` and the `Library` scan root.
- A `.nam` model and a `.wav` IR in that `Library` root, generated by
  `namir_fixtures::library::generate_shared_corpus(1)` and copied from the fixture cache (D-19.1 —
  never a captured file). Keep a pristine copy of each outside the library root, since several
  inductions below corrupt them.
- A release build: `cargo run -p namir-app --release`.

Record, for every notice a step produces: the full on-screen text verbatim, whether audio glitched,
and whether the rest of the screen stayed operable.

## Part A — errors that arrive while audio is flowing

These are the inductions that test both clauses at once. In each, start with audio audibly running
and a model and IR loaded, and keep playing throughout.

1. **`app.host.load_failed` — a corrupt model.** With the application running and a scan already
   complete, overwrite one of the library's `.nam` files with garbage (e.g.
   `[System.IO.File]::WriteAllText("$env:APPDATA\Namir\Library\<file>.nam", "not a model")`), then
   double-click that entry in the Library panel.
   *Pass:* a notice appears whose id is `app.host.load_failed`; **audio continues without a gap,
   click or mute**, still running the previously loaded model; the rest of the screen stays
   interactive (drag `Input Trim` while the notice is showing and confirm it responds).
   *Fail:* any audible interruption, any dialog, or a frozen screen.
2. **`worker.file.unreadable` / `app.host.load_failed` — a deleted file.** Delete a different
   library `.nam` after the scan has indexed it, then double-click its (still listed) entry.
   *Pass:* a notice naming the missing path appears; audio continues; the entry remains listed
   (the index is stale, not corrupted). *Fail:* as step 1.
3. **`worker.ir.truncated` — an over-long IR.** Place a `.wav` IR longer than 10 seconds in the
   library root, rescan, then load it.
   *Pass:* the IR **loads and is audible** *and* a `Warning`-severity notice
   (`worker.ir.truncated`) appears alongside it — this is the case that proves a notice is not the
   same thing as a failure. Audio continues throughout. *Fail:* the load is rejected, or audio
   drops while the truncation is applied.
4. **Read the notice text (the requirement's second sentence).** With the notices from steps 1-3 on
   screen, transcribe each line exactly as displayed and answer three questions per line, in
   writing: (a) does it state **what failed**? (b) does it name the **file or device** concerned?
   (c) does it state **what the user can do**?
   *Pass:* all three answered yes for every notice. *Fail:* any "no" — and record the literal text,
   including any unsubstituted `{placeholder}` tokens, rather than summarising. This is the step
   most likely to produce a real finding; it is worth more than a verdict.
5. **Non-modality, explicitly.** While at least one notice is showing: click a control behind/below
   it, type into the Library search box, scroll the central column, and use the window's title bar
   to move and resize the window.
   *Pass:* every one works immediately, with no acknowledgement required, and the notice does not
   follow focus or re-centre itself. *Fail:* anything is blocked until the notice is dismissed.
6. **Dismissal.** Click `Dismiss` on the middle notice of three.
   *Pass:* exactly that notice disappears, the other two remain, and audio is unaffected.
   *Fail:* the wrong notice is removed, all are cleared, or audio glitches on the click.
7. **`app.host.scan_save_failed` and scan warnings.** Make `library-index.json` unwritable (set it
   read-only, or hold it open exclusively in another process), then press **Rescan library** while
   audio plays.
   *Pass:* the scan completes, a `Warning` notice `app.host.scan_save_failed` appears, the
   in-memory library list is still current, and audio never drops for the duration of the scan.
   *Fail:* an audible dropout during the scan, or the index being erased rather than left stale.
8. **`app.audio_io.device_lost` (FR-IO-070 crossover).** Unplug the audio interface, or disable it
   in the OS, while audio is playing. **Read this criterion carefully:** here audio *does* stop, and
   that is the hardware's doing, not the error display's — this step tests the other clause.
   *Pass:* the application does not crash or hang, a notice `app.audio_io.device_lost` appears
   non-modally, the window stays interactive and closable, and the notice text identifies which
   direction (input/output) and which device was lost. *Fail:* a crash, a hang, a modal dialog, or a
   notice that does not name the device.
   Cross-reference `docs/manual-tests/fr-io-070-device-removal.md`, which covers the device-removal
   behaviour itself; this step covers only how the resulting error is *surfaced*.

## Part B — errors present when the window opens

These test the non-modality and text clauses for the start-up catalogue entries, which no gesture
inside a running session can produce. Induce one per launch, so it is unambiguous which notice came
from which cause. Audio may or may not be running depending on the induction; note which.

9. **`app.settings.unreadable`.** Write garbage into `audio-settings.json`, then launch.
    *Pass:* the application starts with default audio settings, audio runs, and a `Warning` notice
    `app.settings.unreadable` is present in the top panel from the first frame — not a dialog.
10. **A corrupt library index.** Write garbage into `library-index.json`, then launch.
    *Pass:* the application starts, the library list is empty rather than broken, and a warning
    notice from `namir_library::IndexStore::open` is shown, telling the user a rescan is needed.
11. **`app.audio_io.remembered_device_unavailable`.** Edit `audio-settings.json` so
    `input_device_name` names a device that does not exist, then launch.
    *Pass:* the application falls back to a working device, audio runs, and the notice names both
    the remembered device and the one used instead.
12. **`app.audio_io.exclusive_mode_unavailable`.** Set `"exclusive_mode": true` in
    `audio-settings.json` against a device that cannot grant it (a shared-only virtual device is the
    easiest), then launch.
    *Pass:* audio runs in shared mode, the top-panel indicator reads `Shared mode — <device>` (it
    must not claim exclusive), and a `Warning` notice explains the refusal. Transcribe the notice
    and re-answer step 4's three questions against it.
13. **`app.audio_io.no_supported_config`.** Disable every audio device in the OS, then launch.
    *Pass:* a window still opens (`open_window_without_audio`), parameters are still editable, and
    the notice states that nothing will be processed. *Fail:* the process exits silently, crashes,
    or opens no window.

## Part C — the CLAP plugin

14. Repeat steps 1-6 with the plugin loaded in a real host, transport rolling, on Windows (the
    editor is Win32-only — FR-CLAP-100's M13 consequence). The load path differs
    (`crates/namir-clap/src/worker_jobs.rs` pushes the notices, not `AppHost`), the UI is the same
    `namir_ui::render`.
    *Pass:* as steps 1-6, with the host's transport never dropping out and the host never showing a
    dialog of its own.
15. **Missing file references on project reload** (`ResourceRecall::Missing`, via
    `worker_jobs::spawn_recall`). Save a host project with a model and IR loaded, close it, delete
    or rename the referenced `.nam`, then reopen the project with the transport rolling.
    *Pass:* the plugin loads, audio runs (with the missing resource simply not loaded), and a
    warning notice names the resource that could not be found. *Fail:* a hang on project load, a
    silent omission with no notice, or an audio dropout attributable to the notice rather than to
    the missing model.

## Catalogue entries this script does not reach, and why

Stated rather than left as an unexplained absence — FR-UI-070's method is "M **against the error
catalogue of FR-ERR-020**", so which entries went unexercised is part of the result:

- **`app.host.state_save_failed`, `app.host.state_load_failed`, `app.host.reference_missing`
  (standalone).** Not inducible by any user gesture today. `AppHost::save_state`/`load_state`
  (`crates/namir-app/src/host.rs:461`/`:466`) are public but wired to no control — that function's
  own doc comment says so: "not a `UiIntent` today (`namir-ui`'s FR-UI-020 screen has no save/load
  control yet)". `reference_missing` is reached only from `apply_recall_summary`, i.e. only from a
  state load, so it is behind the same gap. Step 15 exercises the equivalent path in the plugin,
  where the host drives state load/save; the standalone half stays uncovered until a save/load
  surface exists.
- **`app.host.load_not_delivered` / `worker.submit.not_delivered`.** Requires the engine's command
  ring to refuse a handover, which depends on audio-thread timing and cannot be produced
  deterministically by a user gesture. Covered in-process by `namir-worker`'s own tests instead.
- **`worker.file.too_large`.** Inducible in principle — a file above `MAX_FILE_BYTES` (256 MB) in
  the library root — but the scan hashes it first, so the induction costs minutes and tests the
  same notice machinery steps 1-2 already cover. Optional; run it only if the size-limit path
  itself is under suspicion.
- **`worker.job.panicked`, `clap.gui.invalid_parent`, `clap.activate.invalid_sample_rate`.** Each
  requires an internal fault or a misbehaving host; no user-reachable induction exists.

## Executed run

**Not executed.** No step of this script has been run, in either product configuration. It was
written in M9b as the traced artifact FR-UI-070 had none of — the requirement read `**UNRESOLVED**`
in `docs/03-test-plan.md` until this file existed — and running it needs a person at a display with
a real audio interface, an instrument, and (for Part C) a real CLAP host, none of which this
session had.

Nothing here has been observed: no notice text has been read off a screen, no audio has been
listened to across an error, no modality has been tested against a real click, and no verdict is
recorded for any of the fifteen steps. In particular, step 4's three questions — the requirement's
second sentence — have **no answer yet**, and the placeholder-substitution behaviour described
above is stated as what the rendering code does, not as something seen on screen.

Two constraints for whoever runs it, so the result is worth recording:

- **One induction per launch in Part B**, so a notice cannot be attributed to the wrong cause.
- **Transcribe notice text verbatim**, including any literal `{placeholder}` tokens. A summarised
  "the message was clear enough" is not evidence against a requirement whose method is comparison
  with the catalogue.

**Result: NOT EXECUTED.** FR-UI-070 has no observed evidence yet for either clause, in either
product configuration.
