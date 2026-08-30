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
help. No CLAP host was available there either.

**Correction (2026-08-30, issue #143).** The observation above stands; the explanation attached to it
did not. It was recorded as "`baseview` 0.2.2's X11 path needs a GLX-capable display and `Xvfb`
provides none", and that is wrong. The cause is **sRGB**, not GLX: `GlConfig::srgb` defaults to
`true`, and `glXChooseFBConfig` then matches none of Xvfb's configs. Measured under Xvfb -- 240
healthy `GLXFBConfig`s available, the sRGB flag clear on every one. `namir-ui` now retries with
`srgb: false` when the first attempt fails (`open_with_srgb_fallback`), and CI runs the interface
headless on every push (`headless-window`, asserting a frame count rather than an exit status). So a
headless window is no longer a blocker for the `Verify: M` UI scripts; executing them still is.

## Executed run on Windows (M12, 2026-08-11)

Run by the author on Windows, which is the only platform where step 2 is possible at all --
`namir-clap` declares Win32 embedded-only GUI support (`crates/namir-clap/src/gui.rs`), so there is
no plugin GUI on Linux or macOS to check.

| Step | Verdict |
|---|---|
| 1. Standalone window shows the mark | **PASS** -- "the logo is visible in the standalone window" |
| 2. Plugin shell shows the mark in a host | **PASS** -- "Namir logo is visible in CLAP plugin" |
| 3. Appearance | **PASS**, after one defect found and fixed; see below |
| 4. Accessible name is "Namir" | **PASS** -- reported as "Namir" |

**Step 3 found a real defect: the mark was too small in both shells.** It was drawn at exactly one
`TextStyle::Heading` row -- about 25 logical pixels, which is the height of a line of text rather
than of a logo. Fixed by `brand::MARK_HEIGHT_IN_HEADINGS = 2.0`, doubling it to ~50 logical pixels.
Two is not an arbitrary choice: the embedded blob is 96 rows, so at ~50 logical pixels a 2x HiDPI
display asks for ~100 physical pixels against 96 stored, which is ~1.04x and effectively 1:1 -- the
sharpest this asset can be drawn without regenerating it taller. `MARK_TARGET_HEIGHT`'s doc comment
in `xtask/src/identity.rs` is updated in the same change to record that its former ~2x margin is now
spent, so a future size increase must raise it too.

**Step 3's legibility half also passes, and it is the half that was carrying the most risk.** The
step asks that the mark be *legible rather than aliased*; the enlarged mark was inspected and
reported to read fine. That closes two fixes that until now were reasoned from the pinned sources
and never looked at: the mipmapping enabled after a review found the mark being minified ~4-5x with
`mipmap_mode: None` -- the classic recipe for a thin wordmark shimmering -- and the size increase
above, which halves that minification to ~2x. Both are now confirmed in the only way they could be.

## Status after this run

**Steps 1, 2 and 4 are closed.** "The interface shall display the Namir brand mark" is satisfied and
observed in both product shells, which is the clause M12 scoped.

**All four steps pass. The brand-mark clause of FR-UI-110 is closed** -- "the interface shall
display the Namir brand mark" is satisfied, observed in both product shells, and legible.

**FR-UI-110 as a whole remains open on exactly one count**, which M12 cannot close: **both icon
clauses** -- "the standalone application's window and executable shall carry the application icon"
-- deferred to M13 by `02-architecture.md` D-17.3. The executable icon needs a build script that
decision declines to admit to a shipped crate; the window icon has no route at all through
`baseview` 0.2.2, which has no icon field. M13 is where this document should be finished, on the
Windows machine it needs anyway.

## M13, 2026-08-11: the two icon clauses

The two clauses came into M13 for independent reasons and they leave it with different verdicts.
Written before either has been looked at on a screen, and the sections below say plainly which
statements are measurements and which are arguments.

### The `.ico` exists and is generated, not checked in by hand

`images/namir.ico` is produced from `images/namir.png` by `cargo run -p xtask -- identity --write`
and byte-compared by `cargo run -p xtask -- identity`, the same generate-and-diff gate M12 built for
the brand-mark blob (`xtask/src/identity.rs`). The alternative -- a binary `.ico` committed once and
regenerable by nobody -- was rejected for the reason that gate exists: it would be the only artwork
in the repository with no stated derivation from the source PNG, so a change to the artwork would
leave it silently stale.

Three things about it that are decisions rather than details:

- **It is the leopard head, not the whole mark.** The source is a 3.73:1 wordmark; an icon is
  square. `icon_crop` takes the rightmost `height x height` square, which is the head, and
  `check_icon_gutter` refuses artwork whose wordmark reaches into that square rather than trusting
  the layout. Measured on the shipped artwork: the column at the crop's left edge carries ink in 9
  of 474 rows, against 77 for the `r`'s stem seventeen columns further left.
- **It carries 16, 32, 48 and 256 pixel sizes, uncompressed.** A PNG-compressed 256 entry would take
  the file from 285 478 bytes to roughly 10 KiB and would make a byte-compared artifact depend on a
  third-party deflate implementation's heuristics, which is the one property `identity`'s design
  note says its generated artifacts must not have.
- **The small sizes are weak, and no code change fixes that.** A contrast rescale was written,
  measured and deleted: the 16x16 tile's peak alpha is already 243 of 255, so rescaling would gain
  1.05x and change nothing visible. The 16x16 is a 29.6x reduction of line art and reads as a
  smudge; 32 is readable, 48 and 256 are good. If the smallest size matters, the fix is a
  simplified icon-specific piece of artwork, which is an artwork decision.

**What has actually been observed, in this session, on this machine.** The generated file was
decoded by Windows itself, not only by the code that wrote it: `System.Windows.Media.Imaging`
(WIC -- the decoder the shell uses) reports four frames, `256x256 Bgra32`, `48x48`, `32x32`,
`16x16`; `System.Drawing.Icon` loads the 48 px entry as 48x48; `Icon.ExtractAssociatedIcon`
succeeds. Each size was rendered to a PNG and looked at. So "the file is a valid Windows icon and
shows the leopard head" is a measurement. Everything below about it appearing *on* an executable is
not.

### What still has to embed it, and where that leaves the executable clause

D-17.3 puts the embedding in the packaging pipeline rather than in a build script, so nothing a
`cargo build` produces carries the icon and nothing here changes that. Two edits outside this lane
complete the clause, both one line:

- `packaging/windows/namir.iss` -- `SetupIconFile=..\..\images\namir.ico`, replacing the comment
  that currently records the absence. This is the **Setup executable's** icon.
- `packaging/windows/README.md` -- an `rcedit target\release\namir.exe --set-icon images\namir.ico`
  step between `cargo build --release --workspace` and `cargo run -p xtask -- bundle`, so that the
  installed `namir.exe` and the plain archive's copy both carry it and both stay inside the tree
  `bundle` asserts. This is the **application's** icon and is the half `SetupIconFile` does not
  reach.

`rcedit` is a single-file MIT tool that runs on the packaging machine and puts nothing of its own
into the artifact, so `02-architecture.md` §17's own note excludes it from the dependency register
for the same reason Inno Setup, `pkgbuild` and `notarytool` are excluded. Nothing here adds a cargo
dependency.

**So the executable clause is built but not closed**: the artifact exists and is gated, the two
embedding lines are not yet written, and no `namir.exe` has been seen carrying an icon.

### The window clause cannot close through the pinned stack, and `baseview` 0.3.0 does not change that

M12 left one thing explicitly unchecked -- whether `baseview` 0.3.0 gained an icon field. Checked
this session against the published source rather than a changelog. It did not, and the position is
worse than "not yet":

- `WindowOpenOptions` **does not exist in 0.3.0**. `https://docs.rs/baseview/0.3.0/baseview/struct.WindowOpenOptions.html`
  is a 404, and the published `baseview-0.3.0.crate` tarball contains no occurrence of the name; the
  struct was renamed and reshaped to `WindowSettings` in `src/settings.rs`.
- `WindowSettings` carries `title`, `size`, `parent`, `wait_for_parent`, `fallback_scale_factor` and
  an `opengl`-gated `gl_config`. It is still `#[non_exhaustive]`, and there is **no icon field**.
- Searching the whole 0.3.0 tarball for "icon" returns six hits: five are mouse-cursor naming, and
  the sixth is `src/wrappers/win32/window/window_class.rs`, which registers baseview's own window
  class with `hIcon: null_mut(), // Default icon`. 0.2.2 has the identical line. **No version of
  `baseview` has ever had an icon API on any backend**, and the class it registers offers no seam to
  override.

The upgrade is also not reachable even if it helped: the newest published `egui-baseview` is 0.6.0,
the version pinned here, and its own manifest requires `baseview = "0.2.2"`. Moving to 0.3.0 would
mean moving off published `egui-baseview` entirely, onto a breaking rename (`WindowOpenOptions` ->
`WindowSettings`, `scale: WindowScalePolicy` -> `fallback_scale_factor: Option<f64>`), against a
stack D-15.1/D-15.2 pinned to exactly what the spikes validated -- for no icon field at either end
of the move. That cost is not worth paying for a Should, and it is not paid.

**FR-UI-110's window-icon clause therefore cannot close through the pinned stack**, and this is a
finding rather than a deferral: it is not blocked on a version that has not landed. The routes that
remain are a `WM_SETICON` against the HWND, which D-17.3 already priced at a fourth
`#![allow(unsafe_code)]` file in `namir-platform` for a cosmetic feature, and the shell's own
fallback -- with `hIcon` null and no `WM_SETICON`, Windows may use the process executable's icon for
the taskbar button and Alt-Tab, which would make the window clause a free consequence of the
executable one. D-17.3 says in as many words that this "is **not** asserted here" and must be
verified on real Windows. It is still not asserted, and step 7 below is what would settle it.

## M13 script -- the icon steps

Steps 1-4 above are closed and are not re-run. These are additional, and **none of them has been
executed**.

5. **The `.ico` is what the packaging pipeline embeds.** With the two one-line edits above applied,
   from a repository-root PowerShell:
   ```powershell
   cargo build --release --workspace
   rcedit target\release\namir.exe --set-icon images\namir.ico
   cargo run -p xtask -- bundle
   & "C:\Program Files (x86)\Inno Setup 6\iscc.exe" /DAppVersion=0.1.0 /DVersionInfoVersion=0.1.0.0 packaging\windows\namir.iss
   ```
   Confirm `target\bundle\windows\namir.exe` shows the leopard head in Explorer, and that the
   produced `target\dist\namir-0.1.0-windows-x86_64-setup.exe` does too.
6. **Every size renders.** In Explorer, view the installed `namir.exe` at Small, Medium, Large and
   Extra Large icons. Record whether the 16 px form is acceptable in the details view and on the
   taskbar; it is the one this session expects to be weak, and a "no" here is an artwork item, not a
   code defect.
7. **The window icon, which is the open question.** Run the installed `namir.exe` and look at three
   places: the window's own title bar, its taskbar button, and Alt-Tab. Record each separately --
   they do not have to agree. A leopard head in the taskbar button with a generic icon in the title
   bar is a perfectly possible outcome and is the one that would tell us the shell's executable
   fallback is doing the work. If all three show a generic icon, the window clause is unmet through
   this route as well and the only remaining option is the `WM_SETICON` D-17.3 priced.
8. **The plugin shell is out of scope for both clauses.** The requirement names "the standalone
   application's window and executable"; `Namir.clap` has neither, and its window is the host's.

## Status after M13

**Result: PARTIAL.** The brand-mark clause is closed and observed; the executable-icon clause is
built but has never been seen on an executable, and the window-icon clause is unknown. Steps 1-4
were not executed in the build environment and steps 5-7 have not been run at all — the three
paragraphs below say which is which.

**Brand-mark clause: closed** (M12, observed).

**Executable-icon clause: the artifact is built, generated, gated and validated as a Windows icon;
the two one-line embedding edits and steps 5-6 remain.** No executable has been seen carrying it.

**Window-icon clause: cannot close through the pinned stack**, on evidence rather than on a
deferral. Whether the executable icon supplies it for free is step 7 and is unknown.

FR-UI-110 is a **Should**, so none of this moves a `03-implementation-roadmap.md` §14 cell or a
`03-test-plan.md` row; the requirement's whole record is this document.
