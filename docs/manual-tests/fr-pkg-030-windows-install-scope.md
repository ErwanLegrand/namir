# FR-PKG-030 manual test: the Windows installer's per-user and system-wide scopes

**Requirement (literal, Must):** FR-PKG-030 — "The Windows installer shall offer both a per-user and
a system-wide install scope, shall default to per-user, and shall place the CLAP artifact in the
CLAP directory corresponding to the chosen scope, as recorded in `02-architecture.md`."

**Verify: M.** Under D-18.6 a `Verify: M` Must is traced by this document and by nothing else — no
source annotation resolves it, and `xtask/src/traceability.rs` refuses a
`// trace-partial: FR-PKG-030` outright. So this file is FR-PKG-030's entire evidence, and the
Result section at the bottom is the whole of it.

**What "as recorded in `02-architecture.md`" binds to.** The FRS deliberately does not cite `D-x.y`
identifiers (its own §1.1), so the binding is **D-13.3's table, Windows row**: per-user
`%LOCALAPPDATA%\Programs\Common\CLAP`, system-wide `%COMMONPROGRAMFILES%\CLAP`. D-18.3's Windows
paragraph is where that identification is stated.

## Before you start

**Step 0 is not optional and is not part of this requirement.** D-18.3 carries a standing
obligation that the per-user default be *empirically* verified before it ships, because a plugin at
an unscanned path fails **silently** — it never appears, with no error in any log. The precedent
D-18.3 names is Dexed, which ships its per-user install mode commented out with a note that the
DAW-side issues were never resolved. If step 0 fails, the per-user default is wrong and
`packaging/windows/namir.iss` changes; do not proceed to score this requirement until it passes.

What existing evidence does and does not say, so step 0 is not skipped as already-done: D-13.3's
rationale and `docs/user-guide.md:82-90` record the **negative** result from spike S-4 — Reaper does
*not* scan `%APPDATA%\REAPER\UserPlugins\CLAP`. That is not a positive confirmation of
`%LOCALAPPDATA%\Programs\Common\CLAP`, and this step is what supplies one.

You will need: a real DAW (Reaper is the one D-18.3 and D-13.3 both name), an account with
administrator rights for step 3, and Inno Setup 6.3 or later to build the installer. **6.3 is a
floor, not a preference** — `packaging/windows/namir.iss` uses `ArchitecturesInstallIn64BitMode=`
`x64compatible`, which an older Inno rejects at compile time. That is the intended failure: without
that directive `{autocf}` when elevated resolves to the *32-bit* Common Files directory, which is
not a path CLAP's `entry.h` lists.

Uninstall any previous Namir before each scope's run, or you will be scoring a leftover.

## Script

### 0. The per-user path is actually scanned (D-18.3's precondition)

Testable before any installer exists, with the artifact `xtask bundle` stages:

```
cargo build --release --workspace
cargo run -p xtask -- bundle
```

Copy `target\bundle\windows\Namir.clap` to `%LOCALAPPDATA%\Programs\Common\CLAP\` (create the
directory if absent), rescan plugins in the host, and confirm **Namir appears in the plugin
browser**. Record the host and its version.

### 1. Build the installer

From the repository root, with a release build and a staged bundle already present:

```
cargo build --release --workspace
cargo run -p xtask -- bundle
cargo run -p xtask -- bundle --check
iscc /DAppVersion=0.1.0 /DVersionInfoVersion=0.1.0.0 packaging\windows\namir.iss
```

Confirm `iscc` exits 0 and names the produced `.exe`. If it fails on `x64compatible`, your Inno is
older than 6.3 — see above.

### 2. Per-user scope, and that it is the default

Run the installer **without** right-clicking "Run as administrator".

1. **Record which option the install-mode dialog preselects.** This is the whole of "shall default
   to per-user" — a dialog that offers both but preselects the system-wide option fails this
   requirement even though both scopes work. Write down what is preselected, verbatim.
2. Accept the default and complete the install.
3. Confirm `%LOCALAPPDATA%\Programs\Common\CLAP\Namir.clap` exists, and that
   `%COMMONPROGRAMFILES%\CLAP\Namir.clap` does **not**.
4. Confirm the three FR-PKG-040 documents are present in the install directory:
   `THIRD-PARTY-NOTICES.md`, `LICENSE-MIT`, `LICENSE-APACHE`.
5. Launch the host, rescan, and confirm Namir is listed. Launch the standalone and confirm it
   starts.
6. Uninstall. Confirm `Namir.clap` is gone from the per-user CLAP directory and that the directory
   itself survives if any other vendor's plugin is in it.

### 3. System-wide scope

Run the installer again and choose the system-wide option — either from the install-mode dialog, or
by launching it as `namir-setup.exe /ALLUSERS`. Elevation will be requested; accept it.

1. Confirm `%COMMONPROGRAMFILES%\CLAP\Namir.clap` exists — and specifically that it is under
   `C:\Program Files\Common Files\CLAP` and **not** `C:\Program Files (x86)\Common Files\CLAP`.
   The 32-bit directory is the failure `ArchitecturesInstallIn64BitMode=x64compatible` exists to
   prevent, and it is silent: the install succeeds and no host ever lists the plugin.
2. Confirm `%LOCALAPPDATA%\Programs\Common\CLAP\Namir.clap` does **not** exist.
3. Confirm the three FR-PKG-040 documents are present.
4. Rescan in the host and confirm Namir is listed.
5. Uninstall, elevated, and confirm removal.

### 4. Both scopes are genuinely offered

Confirm that the choice was reachable **through the installer's own UI**, not only through a command
line switch. `PrivilegesRequired=lowest` alone does not offer a choice; the `.iss` sets
`PrivilegesRequiredOverridesAllowed=dialog commandline` for exactly this clause, and step 2.1 is
where you observe that the `dialog` half works.

## Executed run

**NOT EXECUTED as of this document's creation (M13, 2026-08-11).** The installer has never been
compiled: Inno Setup is not present in the environment where `packaging/windows/namir.iss` was
written, so no `iscc` run, no produced installer, no install and no uninstall have happened, in
either scope. `namir.iss` is reasoned from Inno's documented behaviour of `{autocf}`,
`PrivilegesRequired`, `PrivilegesRequiredOverridesAllowed` and
`ArchitecturesInstallIn64BitMode` — not from a build.

Step 0 has likewise **not** been executed. It needs a real host on a Windows machine, and it is the
one step of this script that could have been run before the installer existed.

**FR-PKG-030 is therefore open.** Record the run below when it happens, per scope, including the
verbatim preselected option from step 2.1 — and record a failure as a failure. This project's
history has documented FAILs sitting in manual-test files for milestones at a time (see
`fr-io-020-wasapi-exclusive-mode.md`'s own history section), which is the correct disposition for a
requirement that does not hold yet.

| Step | Verdict | Notes |
|---|---|---|
| 0. Per-user path is scanned by a real host | NOT EXECUTED | |
| 1. Installer compiles | NOT EXECUTED | |
| 2. Per-user scope, preselected by default | NOT EXECUTED | |
| 3. System-wide scope, 64-bit Common Files | NOT EXECUTED | |
| 4. Both scopes offered through the UI | NOT EXECUTED | |

**Result: NOT EXECUTED.**
