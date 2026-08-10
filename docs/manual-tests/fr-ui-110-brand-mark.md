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
   rectangle or a blank gap — i.e. that the alpha mask decoded and tinted correctly — and that it is
   legible rather than aliased at 1x and at a HiDPI scale factor.
4. **Accessibility (FR-UI-030 regression check).** Confirm the image still exposes the accessible
   name "Namir" that the text heading previously provided, and that hovering it shows that name.

## Executed run (M12, 2026-08-10)

**Steps 1-4: NOT EXECUTED. No display of any kind is available in this environment**, and the
failure is in the windowing library rather than in anything M12 changed:

```
$ xvfb-run -a cargo run --example manual_window_smoke -p namir-ui
thread 'main' panicked at baseview-0.2.2/src/platform/x11/window.rs:111:27:
called `Result::unwrap()` on an `Err` value: RecvError
```

`cargo run -p namir-app` reaches further — it starts audio (`namir: audio stream started`,
`48000 Hz, 256-frame buffer`) and then panics at the same `baseview` X11 call. A virtual framebuffer
(`xvfb-run`) does not help: `baseview` 0.2.2's X11 path needs a GLX-capable display and `Xvfb`
provides none here. There is no host in this environment either, so step 2 is equally unrun.

**What was verified instead, and what that is worth.** The headless tests in
`crates/namir-ui/src/brand/mod.rs` and `xtask/src/identity.rs` prove the blob decodes to the
expected dimensions, that the tint produces `#ff6600` at full alpha, that the texture is uploaded
once and reused rather than per frame, and that the checked-in blob is byte-identical to a fresh
render of `images/namir.png`. `crates/namir-ui/src/app.rs`'s existing headless
`egui::Context::run_ui` tests still pass with the mark in place of the heading. **None of that is a
pixel on a screen**, which is the whole reason this requirement's method is `M` and not `U`.

**This document is therefore incomplete by design, and FR-UI-110 is not closed by M12.** The four
steps above must be run on the §2 reference machine — which is the same Windows machine M13 needs
anyway for the icon work and for FR-PKG-030's install-scope test. Running them there, alongside the
icon clauses, is the cheaper sequencing and is what M13 should do.
