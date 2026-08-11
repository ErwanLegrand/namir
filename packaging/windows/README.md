# Windows packaging

`namir.iss` is the Inno Setup script that produces Namir's Windows installer, and this file is the
half of that deliverable no test can carry: nothing in `cargo test` can run Inno, so how to build
this, what the release workflow does with it, and what is *not* verified anywhere have to be
written down.

Governed by `docs/02-architecture.md` **D-18.3** (release and packaging pipeline; tool choice, and
why `cargo-dist` and `cargo-wix` were rejected) and **D-13.3** (the CLAP install-path table).
Closes the Windows half of **FR-PKG-030** and contributes to **FR-PKG-010**, **FR-PKG-040** and
**FR-PKG-050**.

## The binaries are unsigned

Stated first because it determines who can install this. SmartScreen warns on every release and
never stops warning — reputation is what silences it, and a low-volume unsigned publisher accrues
none. Smart App Control, where it is enabled, **blocks** an unsigned installer outright with no
user-visible override. This is risk **R-11**; the fix is a signing identity, not a workaround, and
the `SignTool` block in `namir.iss` is there so that adopting one later is a compile flag rather
than a restructuring. Say so in the release notes rather than letting users discover it.

## Building the installer locally

Inno Setup **6.3 or newer** is required (see "What differs from D-18.3 as written" below for why
that floor exists). Install it with `winget install JRSoftware.InnoSetup`, or from
<https://jrsoftware.org/isdl.php>; `iscc.exe` lands in `C:\Program Files (x86)\Inno Setup 6`. It is
preinstalled on GitHub's `windows-latest` image, so CI provisions no toolchain for this step.

From the repository root:

```powershell
cargo build --release --workspace
cargo run -p xtask -- bundle                       # stages target\bundle\windows, then asserts it
& "C:\Program Files (x86)\Inno Setup 6\iscc.exe" `
    /DAppVersion=0.1.0 /DVersionInfoVersion=0.1.0.0 `
    packaging\windows\namir.iss
```

The result is `target\dist\namir-0.1.0-windows-x86_64-setup.exe`.

The order matters and is D-18.3's: **build → `xtask bundle` → package**. `namir.iss` reads
`target\bundle\windows` and nothing else, so every file it ships has already been through
`bundle`'s own `check` (`xtask/src/bundle.rs`). Adding a `Source:` line that points anywhere else
would put a file into the distribution that no check has ever seen; don't.

Defines the script accepts:

| Define | Default | Why |
|---|---|---|
| `AppVersion` | `0.0.0-dev` | The release tag with its leading `v` stripped. Shown in the wizard and in Apps & Features, and part of the output filename. |
| `VersionInfoVersion` | `0.0.0.0` | The Win32 `VERSIONINFO` resource, which must be numeric `a.b.c.d`. Passed separately so a prerelease tag (`0.2.0-rc1`) stays a legal `AppVersion` instead of becoming a compile error. |
| `Staging` | `..\..\target\bundle\windows` | Override when `CARGO_TARGET_DIR` points elsewhere — `xtask bundle` honours that variable, so the staging tree moves with it. |
| `OutputDir` | `..\..\target\dist` | Where the setup executable is written. |
| `SignToolName` | *(unset)* | Names a signing tool configured with `iscc /S<name>=...`. Unset means the unsigned path, which is the one exercised on every run today. |

## The plain ZIP (FR-PKG-050)

The archive is not built by Inno. It is the same staging tree, zipped — this is the command the
release workflow runs, given `$Version` (the tag without its `v`) and a repository-root working
directory:

```powershell
$Name  = "namir-$Version-windows-x86_64"
$Stage = "target\dist\$Name"
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
Copy-Item -Recurse "target\bundle\windows" $Stage
Compress-Archive -Path $Stage -DestinationPath "target\dist\$Name.zip" -Force
```

Notes on that, each of which is a decision rather than an incidental:

- **The copy exists to give the archive a top-level folder.** `Compress-Archive -Path
  target\bundle\windows` would name that folder `windows`; `-Path target\bundle\windows\*` would
  put six loose files at the archive root and scatter them across whatever directory the user
  extracts into. The copy is the cheapest way to get `namir-<version>-windows-x86_64\...` inside
  the zip. The bytes are still the staging tree's, unchanged.
- **The archive contents are exactly the installer's contents**, which is what FR-PKG-050's "the
  same artifacts" and FR-PKG-040's "every distribution, installer and archive alike" both require:
  `Namir.clap`, `namir.exe`, `THIRD-PARTY-NOTICES.md`, `LICENSE-MIT`, `LICENSE-APACHE`, `README.md`.
- **The archive is also what keeps FR-CFG-040 literally true** once an installer exists — its own
  `*Consequence*` note says so in as many words. A user unzips it and runs `namir.exe` from
  anywhere; nothing about the standalone application needs the installer.
- `Compress-Archive` is in Windows PowerShell 5.1 and PowerShell 7, both present on
  `windows-latest`, so this adds no dependency. It does not preserve empty directories, and the
  staging tree has none.

## What the release workflow will do

Written here, not built here — `release.yml` is another lane's file and this section is the
contract it should implement for the Windows leg. In D-18.3's fixed order:

1. `cargo build --release --workspace` on `windows-latest`.
2. `cargo run -p xtask -- bundle` — stages and asserts `target\bundle\windows`.
3. `iscc /DAppVersion=<tag-without-v> /DVersionInfoVersion=<a.b.c.0> packaging\windows\namir.iss`.
4. The `Compress-Archive` block above.
5. Hash both outputs (`Get-FileHash -Algorithm SHA256`) and publish the hashes with them —
   NFR-SEC-040's "published hashes" half, which is worth having even where bit-for-bit
   reproducibility is not, and the roadmap asks for it explicitly.
6. Upload `target\dist\*.exe`, `target\dist\*.zip` and the hash file to the GitHub Release.

No step provisions Inno: it is preinstalled on the runner. Nothing in this leg is architecture-
matrixed — the artifacts are x86-64 only.

## How the two install scopes are produced

FR-PKG-030 asks for both a per-user and a system-wide scope, per-user by default, with the CLAP
artifact at "the CLAP directory corresponding to the chosen scope, as recorded in
`02-architecture.md`" — which is D-13.3's Windows row:

| Scope | D-13.3's cell | Reached by |
|---|---|---|
| Per-user (default) | `%LOCALAPPDATA%\Programs\Common\CLAP` | `{autocf}` non-elevated, which is `{usercf}` = `{localappdata}\Programs\Common` |
| System-wide (opt-in) | `%COMMONPROGRAMFILES%\CLAP` | `{autocf}` elevated, which is `{commoncf}` — **and `{commoncf}` is the 64-bit Common Files directory only in 64-bit install mode** |

Three directives carry that, and each is load-bearing:

- `PrivilegesRequired=lowest` — Setup never requests elevation on its own, so the default is the
  per-user scope and installing needs no administrator rights. This is D-13.3's rationale for the
  default ("which matters for users without them").
- `PrivilegesRequiredOverridesAllowed=dialog commandline` — and this is what makes the system-wide
  scope reachable at all. With `lowest` alone Setup would simply never elevate and there would be
  no second scope to offer. `dialog` puts Inno's install-mode page first ("Install for all users",
  which relaunches Setup elevated, or "Install for me only"); `commandline` allows `/ALLUSERS` and
  `/CURRENTUSER`, which is how the manual test drives both scopes without clicking.
- `ArchitecturesInstallIn64BitMode=x64compatible` — without it, an elevated install resolves
  `{autocf}` to `C:\Program Files (x86)\Common Files`, i.e. `%CommonProgramFiles(x86)%`, which is
  **not** `%COMMONPROGRAMFILES%` and is not a path CLAP's `entry.h` lists. The plugin would install
  successfully and no host would ever list it — exactly the silent failure D-13.3's own doc comment
  says will be this product's most common support ticket.

Uninstall is Inno's own and removes what was installed from the scope it was installed to: the
uninstall entry is registered under `HKCU` for a per-user install and `HKLM` for a system-wide one,
each `[Files]` entry is logged as it is copied and deleted in reverse, and a directory Inno created
is removed only when empty — so uninstalling never takes another vendor's plugin out of the shared
`CLAP` directory, and never deletes a `CLAP` directory that predated the install. `%APPDATA%\Namir`
(settings and library index) is deliberately left alone: the installer never wrote it.

## What differs from D-18.3 as written

Two refinements. Neither contradicts the decision's conclusion — Inno, `{autocf}`, per-user default
— but both are needed for that conclusion to actually hold, and D-18.3's text does not contain
them.

1. **`PrivilegesRequired=lowest` alone gives one scope, not two.** D-18.3 says the installer
   "defaults to non-elevated per-user and escalates only if the user asks". Nothing in Inno lets
   the user ask unless `PrivilegesRequiredOverridesAllowed` is set; with `lowest` alone, Setup is
   non-elevated always and FR-PKG-030's "shall offer both" fails. `dialog commandline` is the
   missing half.
2. **`{autocf}` elevated is the *32-bit* Common Files directory by default.** D-18.3 states that
   `{autocf}` "resolves to `%COMMONPROGRAMFILES%` when the installer is running elevated". That is
   true only in 64-bit install mode; otherwise it resolves to `%CommonProgramFiles(x86)%`, and the
   system-wide cell of D-13.3's table is missed silently. `ArchitecturesInstallIn64BitMode` is what
   makes the sentence true, and it is the single most consequential line in the script.

`x64compatible` (rather than the deprecated `x64`) is why Inno **6.3+** is required. That spelling
also admits ARM64 Windows 11, where x64 binaries run under emulation. On an older Inno the compile
fails loudly, which is the right failure: silently selecting a different architecture is how you
ship a plugin to a directory no host reads.

## Not verified by anything

Stated plainly, because this project would rather record a gap than imply a check that does not
exist.

- **This script has never been compiled.** It was written without Inno Setup available; no
  `iscc` run, no produced installer, no install performed. The first compile may find syntax or
  directive errors. Nothing under `crates/` or `xtask/` reads this file, so no `cargo` command
  will tell you either.
- **No automated test asserts FR-PKG-040 inside the *installer*.** `xtask bundle`'s
  `every_staged_tree_carries_the_attribution_file_and_both_licence_texts` covers the staging tree,
  which is the packager's input, and its `// uncovered:` field says so. Asserting the three files
  inside a produced `.exe`/`.zip` needs a check that unpacks a built distribution.
- **FR-PKG-030 is `Verify: M`** and closes through `docs/manual-tests/`, not through this file.
- **Which option Inno's install-mode dialog preselects has not been observed.** `PrivilegesRequired=
  lowest` is what should make it the per-user one, and that preselection is literally FR-PKG-030's
  "shall default to per-user" — so it is the first thing the manual test should record, not an
  incidental. (The dialog appears only for a user who is an administrator; a standard user is never
  offered the system-wide scope and gets the per-user install, which is correct.)
- **The per-user path has not been confirmed as scanned by a real DAW.** D-18.3 records this as an
  obligation, not an assumption: REAPER must be observed to actually scan
  `%LOCALAPPDATA%\Programs\Common\CLAP` on a clean machine **before the per-user default ships**.
  The precedent is Dexed, which ships its per-user mode commented out over DAW issues that were
  never resolved. If that verification fails, the default changes — the requirement is
  per-user-by-default because it is better for users, not because it is known-good.

## Wanted from outside `packaging/windows/`

Owned by other lanes; listed so they are not lost.

- **`docs/manual-tests/fr-pkg-030-windows-install-scope.md`** — FR-PKG-030's traced artifact
  (`Verify: M`; D-18.6 makes the manual document *the* artifact for an `M` Must). It should record,
  per scope: which wizard page appeared and which option was preselected; the resolved install
  directory; that `Namir.clap` landed at `%LOCALAPPDATA%\Programs\Common\CLAP` for per-user and
  `%COMMONPROGRAMFILES%\CLAP` — the 64-bit `C:\Program Files\Common Files\CLAP`, not the `(x86)`
  one — for system-wide; that a real host lists the plugin afterwards; and that uninstall removed
  both the artifact and the shortcut while leaving the shared `CLAP` directory and any other
  vendor's plugin in it intact. `/CURRENTUSER` and `/ALLUSERS` drive the two runs.
- **Version metadata.** `[workspace.package]` in the root `Cargo.toml` has no `version`,
  `repository`, `homepage` or `description`. The script needs none of them — it takes the version
  from the release tag via `/DAppVersion`, which is the right source anyway since the tag is what
  FR-PKG-010 makes the release identity. If a `version` key is added later (D-18.4's hygiene bullet
  adds `version` to path dependencies for a different reason), the workflow can derive the define
  from it and cross-check the tag; nothing here needs changing for that.
- **An application icon.** There is no `.ico` in the repository — `images/` holds `namir.png` and
  `namir.svg` — so `SetupIconFile` is unset and both Setup and the installed `namir.exe` show
  default icons. FR-UI-110's executable-icon clause was deferred to M13 by its own M12
  `*Consequence*` note. Whoever closes it should produce the `.ico` alongside the existing brand
  assets (`xtask identity` already owns brand-mark artifact freshness), after which `SetupIconFile`
  is one line here.
- **`TRADEMARK.md` is not staged by `xtask bundle`**, so it is in no distribution. The shipped
  binaries carry the brand mark (M12 embedded it), and NFR-LIC-070 requires the terms on which the
  name and mark may be used to be stated explicitly. `README.md`'s licence section points at
  `TRADEMARK.md`, and in a distribution that pointer dangles. Suggested: add it to
  `bundle.rs`'s `staged_documents()` beside `README`, with the same reasoning that constant already
  carries. It is not a FR-PKG-040 file — that set is exactly three — so this is a judgement about
  what a distribution should carry, not a requirement violation.
- **`release.yml`** — the six steps above. Note that `xtask traceability` does **not** scan
  `release.yml` (its hard-coded set is `ci.yml` and `fuzz.yml`; roadmap §15 item 10), so a
  `# trace:` annotation placed in it resolves nothing. FR-PKG-010's `Verify: S` elects that
  workflow, which is the open item behind its `**UNRESOLVED**` row.
