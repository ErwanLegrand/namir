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
- `.nam` models and `.wav` IRs in that `Library` root, generated straight into it (D-19.1 — never a
  captured file):

  ```
  # PowerShell -- %APPDATA% does not expand here, and passing it creates a directory
  # literally named "%APPDATA%" under the working directory instead:
  cargo run -p namir-fixtures --example seed-library -- "$env:APPDATA\Namir\Library"
  # cmd.exe: cargo run -p namir-fixtures --example seed-library -- "%APPDATA%\Namir\Library"
  ```

  That writes `namir_fixtures::library::mutable_probe_set`'s 12 IRs and 4 `WaveNetShape::Nano`
  models, a `nam_standard.nam` at `WaveNetShape::Standard` (the one to load when you want output
  worth judging by ear), and `ir_overlong_12s.wav` — 12 seconds at 48 kHz, which is step 3's
  induction and is longer than any IR either fixture generator otherwise produces. **Copy the whole
  directory somewhere outside the library root before starting**, since steps 1, 2 and 7 corrupt or
  delete files in place; re-running the example regenerates identical bytes from the same seed if
  you would rather restore that way.
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
3. **`worker.ir.truncated` — an over-long IR.** Load `ir_overlong_12s.wav` (written by the example
   in the preconditions; 12 s against `namir-ir`'s 10-second `MAX_LOAD_SECONDS`, so it exceeds the
   ceiling at every engine rate), rescanning first if it was added after the last scan.
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


**Executed 2026-08-27 on SALON**, a 1440p display at 100 % scale, with a real audio interface
(PreSonus AudioBox 22VSL) monitoring audibly and an instrument playing. Standalone
(`cargo run -p namir-app --release`) for Parts A and B; the CLAP plugin in **Reaper**, transport
rolling, for Part C. Fixtures from `cargo run -p namir-fixtures --example seed-library`.

**Twelve of fifteen steps pass. Three fail: 4, 8 and 14.** Every notice below is transcribed
verbatim, as the script demands.

| Step | Induction | Verdict |
|---|---|---|
| 1 | `app.host.load_failed` — corrupt model | PASS |
| 2 | deleted file | PASS |
| 3 | over-long IR (`worker.ir.truncated`) | PASS |
| 4 | notice text vs the requirement's second sentence | **FAIL** |
| 5 | non-modality under real interaction | PASS |
| 6 | dismissal of the middle of three | PASS |
| 7 | `app.host.scan_save_failed` during a scan | PASS |
| 8 | `app.audio_io.device_lost` | **FAIL** (naming clause only) |
| 9 | `app.settings.unreadable` | PASS |
| 10 | corrupt library index | PASS |
| 11 | `app.audio_io.remembered_device_unavailable` | PASS |
| 12 | `app.audio_io.exclusive_mode_unavailable` | PASS |
| 13 | `app.audio_io.no_supported_config` | PASS |
| 14 | steps 1-6 in Reaper | **FAIL** (dismissal clause) |
| 15 | missing file reference on project reload | PASS |

### The requirement's own two clauses

**"Surfaced non-modally" — met.** Step 5 exercised it against a real window: with notices showing,
controls behind them responded, the Library search box accepted typing, the central column
scrolled, and the window moved and resized from its title bar. Nothing required acknowledgement,
nothing followed focus, no dialog appeared anywhere in fifteen inductions, and Reaper never raised
one of its own.

**"Shall never interrupt audio" — met.** Audio continued without gap, click or mute across a
corrupt model, a deleted file, a truncating IR load, a full library rescan against an unwritable
index, and a project reload with a missing reference. Step 8 is the deliberate exception the script
calls out: audio stops when the interface is unplugged, which is the hardware's doing.

**"An error shall state what failed, which file or device it concerned, and what the user can do" —
NOT met.** This is step 4's verdict and it generalises past the three notices it was asked about.
*What failed* is always stated. *Which file or device* is usually stated — but not by
`app.audio_io.device_lost`, which names neither, nor by `app.settings.unreadable`, which names no
path. *What the user can do* is stated **nowhere**: of the 69 `message_template` values in the
tree, none offers the user an action. Two come closest by saying what the system did instead —
"using {fallback} instead" (`crates/namir-app/src/error_codes.rs:55`) and "will be rebuilt by the
next scan" (`crates/namir-library/src/error_codes.rs:37`). FR-UI-070's method is "M **against the
error catalogue of FR-ERR-020**", so the catalogue-wide answer is the right scope for this verdict.

**FR-UI-070 is therefore verified and failing, which is a different disposition from unverified.**
The first sentence holds in both shells; the second does not.

### Findings, in the order they compound

**1. Placeholders are never substituted (GitHub issue #15) — and this run breaks that issue's own
severity assessment.** Every notice showed literal `{source}`, `{reason}`, `{path}`, `{device}`,
`{direction}`, `{fallback}`, `{display_name}`, `{hash}`. #15 judged this "presentation quality
rather than a missing capability" because the `detail` string carries the file or device. **Step 8
is the counter-example**: its detail is cpal's raw error and names neither the device nor the
direction, while `AppEvent::StreamFailure` carries `direction` (it is used to pick the code) and the
app knows the device name. The facts exist and are dropped at the last step. Detail quality is
per-call-site, not uniform — step 11's detail carries every fact its template promised, step 8's
carries none — so any fix must be checked against both ends of that range, since substituting
templates properly would make step 11's line say everything twice.

**2. The code and message are rendered twice on some paths.** Step 1's notice read
`nam.load.malformed_json: The model file is not valid JSON.` twice in one line. Three layers each
apply the same `{id}: {template} ({detail})` shape, and the middle one stores a fully-rendered
string where a bare detail belongs: `NamLoadError::Display`
(`crates/namir-nam/src/error_codes.rs:157`), then `From<NamLoadError> for WorkerError`
(`crates/namir-worker/src/error.rs:46`) which keeps `code` *and* sets `detail: e.to_string()`, then
`notice_text` (`crates/namir-ui/src/notices.rs:43`). `WorkerError::detail`'s own doc comment says
what the field must not hold — "Never the user-facing string itself: the template lives in the
catalogue" (`:15`) — and three `From` impls put exactly that in it. A fourth site does the same at
another layer: `crates/namir-app/src/worker.rs:115` maps the error to `warning.to_string()` and
`host.rs:295` pushes that as a `detail`. Step 2's notice, built from a real detail rather than
through a `From` impl, shows no duplication — that contrast is what localises the defect.

**3. Three notices carry a code whose text contradicts what happened.** Not placeholders: the wrong
catalogue entry.

- `library.index.corrupt` on the **save** path (step 7). Its own doc says the on-disk index failed
  to *parse*; its template says the index "could not be read … and will be rebuilt by the next
  scan". Nothing was read, the previous index is intact, and what was lost is the new scan's
  results. `IndexStore::save_atomic`'s four error paths all use it
  (`crates/namir-library/src/store.rs:160/166/172/180`) while `:107/116/123` use it correctly on the
  open path. Step 10 induced the open-path case and every word of the same text was true there —
  that contrast is the argument.
- `app.audio_io.no_supported_config` for "no device exists at all" (step 13). Its doc is FR-IO-040's
  "none of the rates/buffer sizes **a device** reports could be negotiated"; here there is no
  device, so `{device}` is not merely unsubstituted but unsubstitutable. Two lines earlier the same
  function passes `None` for the share-mode indicator rather than a "truthful-looking Shared, which
  would claim a device this window does not have" (`crates/namir-app/src/app.rs:532`) — the same
  judgement, made correctly, one call apart. This is a missing catalogue entry, not carelessness.
- `app.audio_io.device_lost` for a failure cpal classified as `Other` (step 8), because
  `stream_failure_code(direction)` maps on direction alone. Right by accident here; the same path
  would report an unrelated stream error as a device loss.

**4. A Rust `Debug` rendering reaches the screen.** Step 8's detail is
`Other("OS Error -2004287450 (FormatMessageW() returned error 317) …")` — the enum variant name
included — because `crates/namir-app/src/app.rs:409` builds it with `format!("{other:?}")`.

**5. One event, several identical notices.** Step 8 produced two indistinguishable `device_lost`
notices (one per direction, indistinguishable precisely because `{direction}` is not rendered).
Step 15 produced two identical `state.reference.not_found` notices for one deleted file:
`spawn_recall` has two deliberate triggers — `crates/namir-clap/src/state_ext.rs:61` on state load
and `crates/namir-clap/src/audio.rs:178` on activation — and the comments anticipate both running,
but each pushes its own notice (`worker_jobs.rs:139`) and nothing deduplicates. The replay being
idempotent does not make its reporting idempotent.

**6. Notices never expire, and the list is unbounded.** `push_notice` appends to a plain `Vec` in
both shells (`crates/namir-app/src/host.rs:208`, `crates/namir-clap/src/shared.rs:212`) and
`Dismiss` is the only removal path (`:456` / `:220`). No expiry, no severity-based timeout, no cap.

**7. And therefore, in the plugin, notices that can never be removed — step 14's failure.** Four
defensible choices compose into an unusable state: `notices.rs:28` lays each notice out as
`ui.horizontal`, label first and `Dismiss` after, and an egui horizontal layout does not wrap, so a
long label pushes the button past the right edge; the editor is fixed at 960x640 with
`can_resize() == false` (`crates/namir-clap/src/gui.rs:87`/`:196`), so the standalone's escape hatch
— widen the window — does not exist; notices never expire (finding 6); and the duplication
(finding 2) makes these lines roughly twice as long as they need to be, which is what pushes the
button off-screen in the first place. **A defect that looked cosmetic at step 1 produces a
functional one at step 14**: a notice permanently occupying part of a screen that, per
`docs/manual-tests/fr-ui-020-single-screen-elements.md`'s own run, already cannot show every
element at once. Either half of the minimal fix would prevent it — pin `Dismiss` with a
right-to-left layout or draw it before the label, and/or let the text wrap.

### Observations outside FR-UI-070's own clauses

- **A corrupt `audio-settings.json` is replaced at shutdown.** Step 9's file survives while the app
  runs and is then overwritten by `crates/namir-app/src/app.rs:486`, which unconditionally persists
  the negotiated settings — FR-IO-080's intent, applied to a file the user may have been in the
  middle of hand-editing. The notice says "using defaults"; it does not say the file will be
  overwritten. That save is also the one report in the program that cannot become a notice, by
  design, because the window is already closed (`app.rs:481`).
- **FR-IO-070's third clause has no surface.** Re-plugging the interface after step 8 did not
  resume audio, and device selection happens once at startup from `audio-settings.json`
  (`app.rs:217`): there is no device-selection UI in either crate — no combo box, no `UiIntent`,
  nothing. FR-IO-070 (Must) requires the application "allow the user to select another device";
  today that means editing JSON and restarting. FR-UI-010's "differing only in the presence of the
  audio-device panel" also presumes a panel that does not exist. Recorded in full in
  `docs/manual-tests/fr-io-070-device-removal.md`, which owns that clause.
- **Nothing on screen names the input device.** The only device name the UI shows is the share-mode
  indicator's, which `namir-app` deliberately feeds with the *output* device
  (`crates/namir-ui/src/host.rs:118-122`).
- **The over-long IR produced an extremely high output level**, identically in both shells. That is
  the fixture's energy — `ir_overlong_12s.wav` is 12 s of noise with a one-second decay constant,
  where a real cabinet IR is 20-200 ms — meeting FR-IR-090 (Should: normalise on load, defeatable)
  being unimplemented (`crates/namir-ir/src/lib.rs:50`). Not a step-3 failure; its criteria are
  rejection or an audio drop, and neither happened.

### Three things that were right, recorded so the record is not only defects

- **Step 12's indicator.** With `"exclusive_mode": true` still in the settings file, the top panel
  read `Shared mode — Haut-parleurs (AudioBox 22VSL)`. FR-IO-020 requires the indicator to report
  what the device actually opened as, never what was asked for, and the request had every chance to
  leak into the display and did not.
- **Step 10's degradation.** A corrupt index yielded an *empty* library and a warning, not a broken
  one — and the entry list survived step 7's failed save rather than being erased, which is the
  failure mode this project has already had once.
- **Step 6's dismissal.** Dismissing the middle of three removed exactly that notice, in the shell
  where the button is reachable.

### Catalogue entries this run did not reach

As the script anticipated: `app.host.state_save_failed`, `app.host.state_load_failed` and
`app.host.reference_missing` in the **standalone** — `AppHost::save_state`/`load_state` are wired to
no control, so no user gesture reaches them; step 15 exercised the equivalent plugin path instead.
Also unreached: `app.host.load_not_delivered`/`worker.submit.not_delivered` (needs audio-thread
timing), `worker.file.too_large` (inducible but slow, and tests the same machinery as steps 1-2),
and `worker.job.panicked`, `clap.gui.invalid_parent`, `clap.activate.invalid_sample_rate` (each
needs an internal fault or a misbehaving host).

**Result: FAIL, 2026-08-27, both product configurations.** Non-modality and the never-interrupt-
audio clause are met and observed. The requirement's second sentence is not met: no notice in the
catalogue tells the user what they can do, and two notices name neither the file nor the device.
Steps 4, 8 and 14 carry the detail; the remaining twelve steps pass.
