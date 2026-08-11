# FR-UI-110 manual test: the brand mark renders, and the icon clauses that do not close here

**Requirement (literal):** the interface shall display the Namir brand mark, and the standalone
application's window and executable shall carry the application icon. **(Should.)**

**Verify: M.** FR-UI-110 is the only M12 requirement whose method is a manual test, so this document
is its traced artifact rather than supplementary evidence (D-18.6). It carries no source
`// trace:` annotation: a plain tag would over-claim, and `xtask traceability` refuses a
`// trace-partial:` naming a `Verify: M` requirement outright.

## What M12 built, and what it deliberately did not

Only the **brand mark** clause is in scope for M12. Both **icon** clauses defer to M13, for two
independent reasons recorded at `02-architecture.md` **D-17.3**:

- The **executable** icon needs a build script to embed a Windows resource. D-17.3 holds the
  dependency-adoption bar and moves it into M13's packaging pipeline instead.
- The **window** icon cannot be set at all through the current stack. `baseview` 0.2.2's
  `WindowOpenOptions` is `#[non_exhaustive]` and carries exactly `title`, `size` and `scale` plus an
  `opengl`-gated `gl_config` — there is no icon field. `03-implementation-roadmap.md` §19's
  instruction to set it "through baseview's own window options" is mistaken; see D-17.3's second
  consequence note.

So this requirement stays **open** after M12. It is recorded here, not closed here.

## Script

1. **Standalone shell.** Open a real window carrying FR-UI-020's screen and confirm the mark paints
   where `ui.heading("Namir")` used to, at the top-left of the top panel, with the
   `* unsaved changes` label still beside it and nothing below it shifted:
   ```
   cargo run --example manual_window_smoke -p namir-ui
   cargo run -p namir-app
   ```
2. **Plugin shell.** Load the built CLAP plugin in a real host and confirm the same mark appears in
   the embedded GUI. Both shells route through one `namir_ui::render`, so this checks the embedding
   path rather than a second implementation.
3. **Appearance.** Confirm the mark is the `#ff6600` wordmark plus leopard head, not a coloured
   rectangle or a blank gap — i.e. that the alpha mask decoded and tinted correctly — and that it
   is legible rather than aliased at 1x and at a HiDPI scale factor.
4. **Accessibility (FR-UI-030 regression check).** Confirm the image still exposes the accessible
   name "Namir" that the text heading previously provided, and that hovering it shows that name.

## Executed run (M12, 2026-08-10)

Superseded by the run below, and kept because it is the reason the design notes it produced were
written against an unobserved mark. **Steps 1-4 were NOT EXECUTED in the build environment**: no
display of any kind exists there, and the failure is in the windowing library rather than in
anything M12 changed:

```
$ xvfb-run -a cargo run --example manual_window_smoke -p namir-ui
thread 'main' panicked at baseview-0.2.2/src/platform/x11/window.rs:111:27:
called `Result::unwrap()` on an `Err` value: RecvError
```

`cargo run -p namir-app` reached further -- it started audio (`namir: audio stream started`,
`48000 Hz, 256-frame buffer`) and then panicked at the same `baseview` X11 call. `xvfb-run` did not
help: `baseview` 0.2.2's X11 path needs a GLX-capable display and `Xvfb` provides none. No CLAP host
was available there either.

## Executed run on Windows (M12, 2026-08-11)

Run by the author on Windows, which is the only platform where step 2 is possible at all --
`namir-clap` declares Win32 embedded-only GUI support (`crates/namir-clap/src/gui.rs`), so there is
no plugin GUI on Linux or macOS to check.

| Step | Verdict |
|---|---|
| 1. Standalone window shows the mark | **PASS** -- "the logo is visible in the standalone window" |
| 2. Plugin shell shows the mark in a host | **PASS** -- "Namir logo is visible in CLAP plugin" |
| 3. Appearance | **PARTIAL -- one defect found and fixed; see below** |
| 4. Accessible name is "Namir" | **PASS** -- reported as "Namir" |

**Step 3 found a real defect: the mark was too small in both shells.** It was drawn at exactly one
`TextStyle::Heading` row -- about 25 logical pixels, which is the height of a line of text rather
than of a logo. Fixed by `brand::MARK_HEIGHT_IN_HEADINGS = 2.0`, doubling it to ~50 logical pixels.
Two is not an arbitrary choice: the embedded blob is 96 rows, so at ~50 logical pixels a 2x HiDPI
display asks for ~100 physical pixels against 96 stored, which is ~1.04x and effectively 1:1 -- the
sharpest this asset can be drawn without regenerating it taller. `MARK_TARGET_HEIGHT`'s doc comment
in `xtask/src/identity.rs` is updated in the same change to record that its former ~2x margin is now
spent, so a future size increase must raise it too.

**The rest of step 3 is still open, and the fix above is unobserved.** The step also asks that the
mark be *legible rather than aliased* at 1x and at a HiDPI scale factor. That was not reported
either way, and it is the specific thing worth looking at: a review had already found the mark was
being minified ~4-5x with `mipmap_mode: None`, which is the classic recipe for a thin wordmark
shimmering, and mipmapping was enabled to address it. Doubling the drawn size halves that
minification to ~2x, which should help again -- but **nobody has yet confirmed either fix
visually**, and the enlarged mark has not been seen at all.

## Status after this run

**Steps 1, 2 and 4 are closed.** "The interface shall display the Namir brand mark" is satisfied and
observed in both product shells, which is the clause M12 scoped.

**FR-UI-110 as a whole remains open**, on two counts, neither of which M12 can close:

1. **Step 3's legibility half**, plus a re-check of the enlarged mark. Cheap: it needs one person to
   look at a window at 1x and at a 2x scale factor.
2. **Both icon clauses** -- "the standalone application's window and executable shall carry the
   application icon" -- deferred to M13 by `02-architecture.md` D-17.3. The executable icon needs a
   build script that decision declines to admit to a shipped crate; the window icon has no route at
   all through `baseview` 0.2.2, which has no icon field.

Both want the same Windows machine, so M13 is where this document should be finished.
