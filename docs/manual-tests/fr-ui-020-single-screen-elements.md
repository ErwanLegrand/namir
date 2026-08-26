# FR-UI-020 manual test: every listed element on one screen, without navigation

**Requirement (literal, Must):** "The interface shall present, on one screen without navigation:
input meter and trim, gate controls, the loaded model's name, the loaded IR's name, EQ controls,
output meter and level, and a global bypass."
**Verify:** M.

## This document is the traced artifact, not supplementary evidence

D-18.6: a `Verify: M` Must is *traced* by its manual document, and only by it. This file carries
FR-UI-020's coverage on its own — there is deliberately no source `// trace: FR-UI-020` anywhere,
because a plain tag would over-claim (no automated test observes a rendered screen) and
`xtask traceability` refuses a `// trace-partial:` naming a `Verify: M` requirement outright. The
tool finds this file by its filename prefix (`fr-ui-020-`), so the prefix is load-bearing and must
not be renamed.

## What is already true mechanically, and what it does not reach

`crates/namir-ui/src/app.rs`'s `render` builds every element the requirement lists, unconditionally,
every frame, in one `CentralPanel` with no tab widget, no menu and no view-switching state of any
kind — the layout is a straight-line sequence of calls, so "the screen has the element" is a
property of the source rather than of any runtime condition. Two headless tests exercise it:

- `rendering_the_full_screen_from_a_default_snapshot_does_not_panic` drives the whole layout
  through `egui::Context::run_ui` at 960x640 and asserts it builds without panicking.
- `every_registry_key_is_covered_by_a_section_prefix_or_a_named_single_control` asserts every
  `namir_params::REGISTRY` key reaches the screen through one of the six section prefixes
  (`trim.`, `gate.`, `nam.`, `ir.`, `eq.`, `out.`) or one of the two named singles
  (`global.bypass`, `global.output_ceiling_db`), so a parameter cannot be added to the registry and
  silently never appear.

What neither reaches, and what this script exists for: whether the elements are *presented* — laid
out legibly, labelled with the text the requirement's words map onto, and all reachable without a
navigation gesture — as observed by a person looking at a real window. A headless `run_ui` proves
the widget tree was constructed; it cannot see an element pushed off-screen, overlapped, clipped to
zero width, or rendered as an empty rectangle.

One judgement call this script deliberately does **not** pre-decide: the central column is an
`egui::ScrollArea` (`namir_ui_main_scroll`), so on a short window some of the listed elements are
below the fold and need a scroll to bring into view. Whether scrolling within a single screen
counts as "navigation" for this requirement is for whoever runs this to adjudicate and record in
step 11 — not something to settle silently by leaving it unmentioned.

## Preconditions

- A machine with a display. Windows is the primary platform; on macOS/Linux the standalone
  application still opens its own window (only the CLAP editor is Win32-only — see FR-CLAP-100's
  M13 consequence note), so steps 1-11 are runnable on all three, and step 12 on Windows only.
- A real audio input/output device, so the meters have signal to show. If no device can be opened,
  `namir-app` falls back to `open_window_without_audio`, whose window renders the same screen with
  silent meters — usable for the layout half of this script, but step 3's moving-meter check then
  cannot be executed and must be recorded as such.
- A `.nam` model and a `.wav` IR in the library root — on Windows `%APPDATA%\Namir\Library`, on
  macOS `~/Library/Application Support/Namir/Library`, on Linux `$XDG_CONFIG_HOME/namir/Library`
  (`namir_platform::config_dir` plus `LibraryService::open_default`'s `Library` subdirectory).
  Generate them straight into that directory, per D-19.1 — do not add a captured/licensed model or
  IR:

  ```
  cargo run -p namir-fixtures --example seed-library -- "%APPDATA%\Namir\Library"
  ```

  That writes `namir_fixtures::library::mutable_probe_set`'s 12 IRs and 4 models (all
  `WaveNetShape::Nano`, cloned from one base model — thin but valid), plus a `nam_standard.nam`
  generated at `WaveNetShape::Standard`, which is the shape worth using for steps 3, 8 and 9 since
  its output is worth judging by ear. Seeded from 1 by default; pass a second argument to vary it.
  Then launch and press **Rescan library** — the index only picks up files present at scan time.

## Script

Launch the standalone application and leave it open for steps 1-11:

```
cargo run -p namir-app --release
```

Each step below names the on-screen text to look for. A step passes only if the named element is
visible **at the same time as every other element in this list**, with no tab, menu, drawer,
accordion or window switch used to reveal it.

1. **Input meter.** A row labelled `Input` carrying a horizontal bar whose text reads
   `<n>.<n> dBFS`. Hovering it shows `Peak <n>.<n> dBFS, RMS <n>.<n> dBFS`.
   *Pass:* the row is present and the bar's fill and its dBFS text both track the instrument
   signal — play into the input and confirm both move. *Fail:* absent, or the bar never moves
   while signal is audibly present.
2. **Trim.** A heading `Input Trim`, below it a control named `Input Trim` reading a dB value
   (default `0.0`), and a control named `DC Blocker` reading `On`/`Off` (default `On`).
   *Pass:* both present, and dragging `Input Trim` changes its displayed value.
   *Fail:* either control absent, or the value display does not follow the drag.
3. **Input meter responds to trim.** With signal playing, raise `Input Trim` by ~+12 dB.
   *Pass:* the `Input` meter's reading rises correspondingly (the meter is fed from
   `telemetry.trim.*`, i.e. it reads the signal *after* trim). *Fail:* the meter is unaffected —
   record the observed dBFS before and after either way, since which side of trim the meter reads
   is exactly what this step pins down.
4. **Gate controls.** A heading `Gate`, below it five controls: `Gate Enabled`, `Gate Threshold`,
   `Gate Attack`, `Gate Hold`, `Gate Release`.
   *Pass:* all five present and each shows a current value. *Fail:* any missing.
5. **The loaded model's name.** A heading `Model` with a line of text beneath it. With nothing
   loaded this reads exactly `(no model loaded)`. Double-click a `[NAM] ...` entry in the left
   Library panel.
   *Pass:* the line changes to the loaded model's file name (the basename, e.g. `plexi.nam`).
   *Fail:* the line stays at the placeholder after a successful load, or shows a full path where a
   name was expected — record which.
6. **The loaded IR's name.** A heading `Impulse Response` with a line of text beneath it, reading
   exactly `(no IR loaded)` when nothing is loaded. Double-click an `[IR] ...` entry in the Library
   panel.
   *Pass:* the line changes to the loaded IR's file name. *Fail:* as step 5.
7. **EQ controls.** A heading `EQ`, below it twelve controls: `EQ Enabled`, `EQ Low Shelf Freq`,
   `EQ Low Shelf Gain`, `EQ Mid Freq`, `EQ Mid Gain`, `EQ Mid Q`, `EQ High Shelf Freq`,
   `EQ High Shelf Gain`, `EQ High-pass Enabled`, `EQ High-pass Freq`, `EQ Low-pass Enabled`,
   `EQ Low-pass Freq`.
   *Pass:* all twelve present, each showing a current value. *Fail:* any missing — list which.
8. **Output meter and level.** A heading `Output`; below it a meter row labelled `Output` in the
   same form as step 1, then a control named `Output Level`, then a control named `Output Ceiling`.
   *Pass:* the meter is present and moves with signal, and `Output Level` is present and adjustable
   — change it and confirm the `Output` meter's reading follows. *Fail:* either half absent, or the
   meter does not respond to `Output Level`.
9. **Global bypass.** Below a horizontal separator, a control named `Global Bypass` reading
   `Off`/`On`.
   *Pass:* present, and toggling it to `On` audibly bypasses the whole chain (dry signal passes)
   while the screen stays otherwise unchanged. *Fail:* absent, or toggling it has no audible
   effect.
10. **All at once, in one screenshot.** Size the window so that the elements from steps 1-9 are
    simultaneously visible, and capture a single screenshot showing all of them.
    *Pass:* one image contains every element. *Fail:* no window size makes that possible — record
    the smallest size at which it nearly does, and which element is the one that will not fit.
11. **Without navigation.** Confirm there is no tab bar, menu bar, page selector, "advanced" drawer
    or second window anywhere on the screen, and that reaching any element in steps 1-9 required no
    gesture other than resizing the window or scrolling the central column. Then adjudicate the
    scrolling question raised above and **record the judgement explicitly**: if any listed element
    could only be reached by scrolling at the window's default 960x640 opening size, say which, and
    say whether you are counting that as navigation. Do not leave it implied.
    *Pass:* no navigation control of any kind exists, and the scrolling question is answered in
    writing. *Fail:* any element sits behind a tab/menu/mode switch.
12. **The plugin shell (Windows only).** Load the built CLAP plugin in a real host, open its
    editor, and repeat steps 1, 2, 4, 5, 6, 7, 8, 9 and 11 against the embedded window. Both shells
    route through the one `namir_ui::render`, so what this checks is the embedding path and the
    host-supplied window size, not a second layout. The audio-device panel is absent here by
    design (FR-UI-010), as is the `Shared mode — <device>` indicator (`audio_mode` is `None` for a
    plugin, which owns no device).
    *Pass:* every listed element appears in the embedded editor too. *Fail:* any element is missing
    or clipped by the host's window — name the host and its window size.

## Executed run

**Not executed.** No step of this script has been run, in either product configuration. It was
written in M9b as the traced artifact FR-UI-020 had none of — the requirement read `**UNRESOLVED**`
in `docs/03-test-plan.md` until this file existed — and running it needs a person at a display with
a real audio interface and an instrument, which this session had neither of.

No screenshot has been taken, no element has been observed on screen, and no pass/fail verdict has
been recorded for any of the twelve steps. The headless tests named above are real and do pass, but
they observe a widget tree, not a presented screen, which is the entire distinction this document
exists to keep honest.

Two constraints for whoever runs it, so the result is worth recording:

- **State the window size and display scale** used for step 10 and step 11. "Everything fits" at
  2560x1440 and at 100 % scale is a different finding from the same claim at 1280x720 or at 200 %
  (FR-UI-080's own territory, checked separately).
- **Record the verdict per step, not one verdict for the document.** The requirement is an
  enumerated list; a single "PASS" would lose which of the eight listed items was actually looked
  at.

**Result: NOT EXECUTED.** FR-UI-020 has no observed evidence yet in either product configuration.
