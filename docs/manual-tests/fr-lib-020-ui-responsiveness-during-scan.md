# FR-LIB-020 manual test: the interface stays usable while a 10 000-file scan runs

**Requirement (literal, Must):** "Library scanning shall occur off the audio thread and shall not
block the user interface. Progress shall be visible and the scan cancellable."
**Verify:** I with a synthetic library of at least 10 000 files.

**This document is supplementary evidence, never the traced artifact.** FR-LIB-020 is `Verify: I`,
so under `02-architecture.md` D-18.6 it is traced only by an annotated in-process artifact — since
M9b, the plain `// trace: FR-LIB-020` at
`crates/namir-worker/tests/library_scan_scale.rs` (before M9b it was a `// trace-partial:` at
`crates/namir-worker/src/library.rs`, whose declared gap that new test closed). `xtask traceability`
does not read this file for anything (its manual-document lookup applies to `Verify: M` only), so
nothing mechanical will notice if this script rots. It exists because one clause of the
requirement — "shall not block the user interface" — has a residue that only a human at a screen can
observe, and writing that down is better than letting the in-process tests be read as covering it.

## What is built and automatically tested

- **All four clauses at once, at the requirement's own scale (M9b).**
  `fr_lib_020_a_ten_thousand_file_scan_blocks_neither_the_audio_thread_nor_the_ui`
  (`crates/namir-worker/tests/library_scan_scale.rs`) scans the 10 000-file shared corpus while its
  own thread runs a live `namir_engine::AudioEngine` inside D-7.5's `assert_no_alloc` harness and a
  second thread plays the UI's part at ~60 Hz (`snapshot()`, `is_scanning()`, `namir_library::filter`
  over the snapshot — the calls `namir-ui`'s library view makes per frame). It asserts zero
  audio-thread allocations, no silent block, no block over 200x its period, that a simulated UI
  frame observed the scan *in flight* and that no frame's work exceeded its ceiling, more than one
  progress report reaching the full 10 000 files, and then cancels a second scan of the same corpus
  with the audio loop still running.
- **"Progress shall be visible", at the requirement's own scale.**
  `a_full_scan_of_the_shared_corpus_reports_progress_more_than_once`
  (`crates/namir-worker/src/library.rs`) runs `namir-fixtures`' 10 000-file shared corpus to
  completion and asserts `on_progress` fired more than once, so at least one call came from
  `start_scan`'s 50 ms cadence branch rather than from the unconditional terminal report.
- **"the scan cancellable", same scale.** `cancelling_a_large_scan_stops_it_before_completion`
  cancels immediately after `start_scan` returns and asserts `ScanOutcome::complete == false` and
  zero removals.
- **The scan does not run on the caller's thread.** `start_scan` flips `scanning` synchronously and
  hands the walk to `crate::pool::ThreadPool`; `a_second_concurrent_scan_is_refused` depends on
  exactly that ordering to be deterministic rather than a race.
- **Rendering 10 000 rows is inside FR-UI-060's frame budget.**
  `rendering_ten_thousand_entries_stays_well_under_the_100ms_frame_budget`
  (`crates/namir-ui/src/library_view.rs`) builds a real `namir_library::Index` from the same
  generator (a different seed — `20_260_807` rather than the worker tests' `1`, so it is the same
  10 000-file shape, not literally the same directory) and renders the full library view headlessly
  through `egui::Context::run_ui`, asserting the frame completed under 100 ms.

## What this script adds, and why it cannot be automated here

The FR-UI-060 test above renders with `scan: None` — `02-architecture.md` §22's **R-12** records
that limitation against this exact reading — so it measures a *settled* 10 000-entry list, never a
frame drawn while a scan is in flight. M9b's `library_scan_scale.rs` closes half of that: a UI-rate
thread and a scanning pool thread genuinely do run at once there, competing for the same disk, with
the index replaced underneath the reader when the scan commits. What it still does not do is
*render* — `namir-ui` sits below `namir-worker` in D-5.1's table, so nothing at that layer may drive
egui, and no crate that can see both a live `LibraryService` and `namir-ui` has a test that does.
So the automated set proves the scan never blocks the calls a UI makes; it does not prove a window
keeps painting during one.

"Shall not block the user interface" is also, in the end, a claim about what a person experiences:
whether the window keeps repainting, whether the search box still accepts typing, whether the
Cancel button responds on the first click. A headless `run_ui` timing harness measures frame *cost*;
it cannot observe a window that has stopped presenting, an input queue that has backed up, or a
compositor that has marked the app Not Responding. That is what needs a human at a screen.

## Script

1. Build and run the standalone application against the shared corpus:
   ```
   cargo run -p namir-app --release
   ```
   Point the library root at a directory holding at least 10 000 `.nam`/`.wav` files.
   `namir-fixtures`' generated corpus is the intended one — it is written under the workspace
   fixture cache by `namir_fixtures::library::generate_shared_corpus(1)`, which the two
   `namir-worker` tests above produce on first run; copy or symlink that directory rather than
   hand-building one, so this script measures the same corpus those tests do (D-19.1: generated,
   never captured).
2. With audio running (a real instrument or a signal generator into the selected input device, so
   an audible dropout would be heard), press **Rescan library**.
3. During the scan, confirm all of the following:
   - The **"Scanning... N examined (M hashed), K pending"** label updates repeatedly, not once at
     the end. This is FR-LIB-020's "progress shall be visible" as a user sees it.
   - The window keeps repainting and never enters the OS "Not Responding" state.
   - Typing in the **Search** box echoes immediately and filters the list.
   - Scrolling the library list is smooth.
   - **Audio does not glitch, drop out or mute** — this is the "off the audio thread" clause, heard
     rather than measured.
4. Press **Cancel scan** while it is still running. Confirm the button responds on the first click,
   the scanning label is replaced by **Rescan library**, and whatever the scan had already found is
   still listed (a cancelled scan commits what it learned and reports no removals).
5. Press **Rescan library** again and let it run to completion. Confirm the label returns to
   **Rescan library** and the entry count is the full corpus.
6. Repeat steps 2–3 in the CLAP plugin, hosted in a real DAW, with the transport rolling. The plugin
   shares this code path through `namir_worker::library::LibraryService::open_default`, but its UI
   thread is the host's, not its own, which is the part that differs.

## Executed run

**Not executed.** No part of this script has been run — not step 1, and not in either product
configuration. It was written in M9a alongside the `// trace-partial: FR-LIB-020` annotation it
accompanies, as the record of what the in-process tests do *not* reach; running it needs a person at
a screen with a real audio interface, and this session had neither. Recorded as unexecuted rather
than presumed to pass by extension from the automated tests above, per this directory's convention.

Two constraints for whoever does run it, so the result is worth recording:

- **Use a release build.** A debug-build scan is dominated by unoptimised hashing and would produce
  a pessimistic result that says nothing about the shipped product.
- **State the volume.** Scan cost is dominated by per-file I/O, so "smooth on an NVMe SSD" and
  "smooth on a network share" are different findings. Record which one was tested.

**Result: NOT EXECUTED.** The clause this document exists for — "shall not block the user
interface", observed by a human during a real 10 000-file scan — has no result yet, in either
product configuration.
