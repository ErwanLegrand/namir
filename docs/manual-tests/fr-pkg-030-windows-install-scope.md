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

## Executed run — as written (M13, 2026-08-11)

**Superseded in part by the run below**, and kept because it is why the `.iss` was reasoned rather
than observed. As written, nothing here had been executed: Inno Setup was not present in the
environment where `packaging/windows/namir.iss` was authored, so `namir.iss` rested entirely on
Inno's documented behaviour of `{autocf}`, `PrivilegesRequired`,
`PrivilegesRequiredOverridesAllowed` and `ArchitecturesInstallIn64BitMode`.

## Executed run — step 1, on Windows (M13, 2026-08-12)

**Step 1 PASSES: the installer compiles and produces an installer.** Inno Setup was installed
(`winget install JRSoftware.InnoSetup.7`) and the script compiled on the first attempt with no
edit to it.

```
> ISCC.exe /DAppVersion=0.1.0 /DVersionInfoVersion=0.1.0.0 packaging\windows\namir.iss
Creating setup files
   Verification successful
   Updating icons (Setup.exe)
   Compressing: ...\target\bundle\windows\Namir.clap
   Compressing: ...\target\bundle\windows\namir.exe
   Compressing: ...\target\bundle\windows\THIRD-PARTY-NOTICES.md
   Compressing: ...\target\bundle\windows\LICENSE-MIT
   Compressing: ...\target\bundle\windows\LICENSE-APACHE
   Compressing: ...\target\bundle\windows\README.md
   Compressing: ...\target\bundle\windows\TRADEMARK.md
Successful compile (2,625 sec). Resulting Setup program filename is:
...\target\dist\namir-0.1.0-windows-x86_64-setup.exe
```

5 210 436 bytes. `Get-AuthenticodeSignature` reports **NotSigned**, as R-11 says it will.

Four things this run establishes beyond "it compiled".

- **All seven staged files are in the payload**, FR-PKG-040's three among them, and `TRADEMARK.md`
  which M13 added. The `[Files]` list and `xtask bundle`'s staging tree agree in fact and not only
  by intention.
- **`SetupIconFile` works — "Updating icons (Setup.exe)"** — which is a second independent decoder
  accepting `images/namir.ico` after `xtask identity` generated it, this one Inno's own resource
  updater rather than Windows' imaging APIs.
- **`ArchitecturesInstallIn64BitMode=x64compatible` compiles**, so the directive that keeps the
  elevated install out of the 32-bit Common Files directory is accepted rather than merely believed
  to exist.
- **The version actually tested is Inno Setup 7.0.2, not 6.3.** This document, the `.iss` and
  `packaging/windows/README.md` all state 6.3 as the floor, and that statement is *unchanged and
  still correct* — `x64compatible` was introduced in 6.3 — but it remains **untested**: nothing has
  ever compiled this script on a 6.x Inno. What has been demonstrated is that Inno 7 compiles it,
  which is the more useful fact for anyone building today and the less useful one for anyone
  relying on the stated floor. The release workflow uses whatever `windows-latest` preinstalls, so
  that is a third version, also untested here.

**Steps 0, 2, 3 and 4 remain NOT EXECUTED.** Nothing has been installed, in either scope, and no
host has been asked whether it scans the per-user CLAP path. Note also that the `namir.exe` inside
this installer has **not** been through `rcedit`, which is not installed locally — so the installed
executable carries no icon even though Setup does, and FR-UI-110's executable clause is not
observed by this run.

**FR-PKG-030 is therefore still open**, on the four clauses that are actually about install scope.
Record each below as it happens, including the verbatim preselected option from step 2.1 — and
record a failure as a failure. This project's history has documented FAILs sitting in manual-test
files for milestones at a time (see `fr-io-020-wasapi-exclusive-mode.md`'s own history section),
which is the correct disposition for a requirement that does not hold yet.

| Step | Verdict | Notes |
|---|---|---|
| 0. Per-user path is scanned by a real host | **PASS** (2026-08-12) | D-18.3's standing precondition, discharged |
| 1. Installer compiles | **PASS** (2026-08-12) | Inno 7.0.2; 5 210 436 bytes; unsigned; all seven files in the payload |
| 2. Per-user scope, preselected by default | **PASS** (2026-08-12) | |
| 3. System-wide scope, 64-bit Common Files | **PASS** (2026-08-12) | |
| 4. Both scopes offered through the UI | **PASS** (2026-08-12) | |

**Result: PASS.**

## Executed run — steps 0, 2, 3 and 4, on Windows (M13, 2026-08-12)

Run by the author on the §2 reference machine, and **reported as passing in full**. The four
clauses this requirement is actually about are therefore met in fact rather than argued from Inno's
documentation, which is what the run above could not do.

**Step 0 discharges D-18.3's standing precondition**, and that is the most consequential of the
four. Until this run, the only empirical evidence about Windows CLAP paths was spike S-4's
**negative** result — that Reaper does *not* scan `%APPDATA%\REAPER\UserPlugins\CLAP`. A negative
about one path is not a confirmation of another, which is why D-18.3 required this specifically and
why the roadmap carried it as outstanding through the whole milestone. The per-user default is now
known-good rather than believed-good, and Dexed's precedent — shipping its per-user mode commented
out because the DAW-side issues were never resolved — does not apply here.

**Steps 2 and 3 confirm both scopes place the artifact where D-13.3's table says**, including
step 3's specific check that the elevated install lands in `C:\Program Files\Common Files\CLAP` and
not the `(x86)` directory. That is the one result that could not have been obtained by reading:
`ArchitecturesInstallIn64BitMode=x64compatible` was added on Inno's documented behaviour, and the
failure it prevents is silent by construction.

**One thing this document does not record, and should have.** Step 2.1 asks for the **verbatim**
wording of the option Inno's install-mode dialog preselects, because that preselection literally
*is* "shall default to per-user" — an installer offering both scopes but preselecting system-wide
passes every other clause and fails this one. The step is reported as passing; the string itself was
not captured. The verdict stands on the runner's report, and this note records that the strongest
form of the evidence is missing rather than pretending it is present. Worth capturing on the next
run, which the release pipeline will force anyway.

**Uninstall was exercised and worked**, in both scopes: after the run, neither
`%LOCALAPPDATA%\Programs\Common\CLAP\Namir.clap` nor `%CommonProgramFiles%\CLAP\Namir.clap`
existed. Checked directly rather than taken on report.

**Not part of this requirement, found during the same session, and recorded here because this is
where the trail starts.** With the plugin installed and running in a host, **the output meter read
full and never moved** — a lifetime-maximum ratchet in `crates/namir-clap/src/ui_host.rs`'s
`drain_meters`, unchanged since M6 and never covered by a test because every test in that module
passed `telemetry: None`. Fixed on `claude/clap-output-meter-ratchet` and verified in the host. It
is an **FR-UI-020** defect, not an FR-PKG-030 one, and the irony is worth keeping: FR-UI-020
requires "output meter and level" on one screen, has no manual-test document of its own, and until
this milestone was credited to `fr-clap-030-audio-ports-negotiation.md` on a single parenthesis
about watching a meter. §15 item 15's fix removed that credit and FR-UI-020 went `**UNRESOLVED**`.
The requirement whose only evidence was a passing mention of a meter turned out to have a broken
meter, in the very shell whose document was crediting it.
