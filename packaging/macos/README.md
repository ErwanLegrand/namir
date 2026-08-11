# macOS packaging

`make_installer.sh` turns the staging tree `cargo run -p xtask -- bundle --target macos` produces
into Namir's macOS distribution: a `.pkg` inside a `.dmg`, plus the plain `.zip` archive FR-PKG-050
asks for. It is the third step of D-18.3's fixed release order — **build → `xtask bundle` → per-OS
package → GitHub Release** — and it does only that step: it builds no Rust, downloads nothing, and
copies only what the staging tree already contains.

> **Nothing in this directory has ever run.** It was written on Windows against the documented
> behaviour of `pkgbuild`, `productbuild`, `hdiutil`, `ditto` and `notarytool`, with Surge XT's
> `make_installer.sh` as D-18.3's named reference. [What is untested](#what-is-untested) lists the
> parts most likely to be wrong, in order of how likely that is.

## Running it locally on a Mac

```bash
# 1. Build the release artifacts the staging tree is assembled from.
cargo build --release --workspace

# 2. Stage them. This is what fixes the CLAP bundle's structure, and it asserts what it produced.
cargo run -p xtask -- bundle --target macos

# 3. Package. No arguments needed; every default is derived from the repository.
bash packaging/macos/make_installer.sh
```

Output lands in `target/packaging/macos/`:

| Artifact | What it is |
|---|---|
| `Namir-<version>-macos.dmg` | **the release artifact** — the `.pkg` plus the four documents, on a mountable volume |
| `Namir-<version>.pkg` | the installer alone, also inside the `.dmg`; kept out for inspection |
| `Namir-<version>-macos.zip` | FR-PKG-050's plain archive: the same artifacts, no installer |
| `*.sha256` | NFR-SEC-040's published-hashes half |

Flags, all optional: `--staging DIR` (default `$CARGO_TARGET_DIR/bundle/macos`, else
`target/bundle/macos`), `--out DIR`, `--version V` (default `$NAMIR_VERSION`, else the `version` in
`crates/namir-app/Cargo.toml` — the workspace has no shared `[workspace.package] version`), and
`--skip-verify`. An unrecognised flag exits **2** rather than being ignored, the same house rule
`xtask bundle` and `xtask traceability` follow.

There is deliberately **no `--sign` flag**; see [Signing](#signing-is-conditional-on-a-secret).

## What each tool does, and why it is the one used

| Tool | Role here |
|---|---|
| `pkgbuild` | builds one **component package** per payload root: the CLAP bundle, the standalone app, the documents. `--install-location` is where that component's contents land. |
| `pkgbuild --analyze` | emits a component property list so `BundleIsRelocatable` can be set **false** — see below; this is the single worst `pkgbuild` trap. |
| `/usr/libexec/PlistBuddy` | flips that key for every bundle in the component list. Present on every macOS; no dependency added. |
| `productbuild` | combines the three component packages into the **product archive** the user double-clicks, driven by a `distribution.xml` that carries the choices, the licence pane and the install domains. |
| `hdiutil` | creates the compressed read-only `.dmg` (UDZO) that carries the `.pkg`. |
| `ditto` | every copy in the script, and the archive. It is the only archiver/copier on macOS that round-trips a bundle's symlinks, permissions and extended attributes — `cp -R` and `zip -r` both produce a broken `.clap`. |
| `codesign` | signs the `.clap`, the `.app` and the `.dmg` with a *Developer ID Application* identity, hardened runtime and a secure timestamp (both required before notarisation will accept the submission). |
| `notarytool` / `stapler` | submits to Apple and staples the ticket, so Gatekeeper can approve offline. The `.pkg` and the `.dmg` are submitted separately: stapling only the outer container leaves the inner package unstapled as soon as a user copies it out. |
| `pkgutil --expand-full` | used at the end to look **inside** the produced `.pkg` and assert FR-PKG-040's three files are really in it. |

### Why `.pkg` inside `.dmg` and not a bare `.dmg`

D-18.3's two reasons, unchanged: only `pkgbuild`/`productbuild` can place multiple payloads at
multiple absolute paths (a `.dmg` is a folder the user drags from — one destination well, several
badly), and files placed by `installer` never carry `com.apple.quarantine`, whereas files extracted
from a downloaded zip do. The `.dmg` is a delivery container for the `.pkg` and nothing more.

### `BundleIsRelocatable false`

Left at its default of true, `installer` asks Spotlight whether a bundle with that identifier
already exists anywhere on the volume and installs **over that copy** instead of at the stated
install-location. A developer with a `Namir.clap` in a scratch directory would get the release
written into their scratch directory and nothing at the CLAP path at all. Every audio-plugin
installer script hits this once.

### D-13.3's two macOS paths from one install-location

The distribution declares:

```xml
<domains enable_anywhere="false" enable_currentUserHome="true" enable_localSystem="true"/>
```

so `installer` offers "Install for me only" / "Install for all users of this computer", and re-roots
each component under the user's home for the former. One `--install-location` therefore yields both
cells of D-13.3's macOS row:

| Component | System-wide | Per-user |
|---|---|---|
| `Namir.clap` | `/Library/Audio/Plug-Ins/CLAP` | `~/Library/Audio/Plug-Ins/CLAP` |
| `Namir.app` | `/Applications` | `~/Applications` |
| licences + notices | `/Library/Application Support/Namir/Legal` | `~/Library/Application Support/Namir/Legal` |

This is the same trick Inno's `{autocf}` plays on Windows, and it is why `enable_anywhere` is
**false**: an arbitrary volume would put the plugin somewhere no host scans, and D-13.3's own
rationale is that a plugin at an unscanned path fails *silently*.

`Legal/` rather than the directory root because `~/Library/Application Support/Namir` is D-13.2's
config directory, where `namir-platform` writes real runtime state. An installer should not drop
files next to those.

The documents choice is visible but **not deselectable**. FR-PKG-040 says every distribution
contains the attribution file and both licence texts; a checkbox that could turn that off would make
the installed product's compliance depend on a user's click.

## Signing is conditional on a secret

Per D-18.3, following Surge's pattern: there is no build flag. Each step reads an environment
variable, does nothing when it is empty, and the run continues down the identical sequence either
way. Enabling notarisation later is *adding a secret*, not restructuring anything — and the unsigned
path is the one exercised on every run rather than an untested fallback.

| Variable | Effect when set | When empty |
|---|---|---|
| `NAMIR_CODESIGN_IDENTITY` | `codesign` the `.clap`, the `.app` and the `.dmg` (`Developer ID Application: …`) | those artifacts ship unsigned |
| `NAMIR_INSTALLER_IDENTITY` | `productbuild --sign` (`Developer ID Installer: …`) | the `.pkg` ships unsigned |
| `NAMIR_NOTARY_PROFILE` | `notarytool --keychain-profile` | no submission |
| `NAMIR_NOTARY_APPLE_ID` + `…_TEAM_ID` + `…_PASSWORD` | `notarytool` with explicit credentials | no submission |

Two identities rather than one because Apple issues two, and they are independently optional: a
workflow holding one gets the half it can do. Notarisation additionally requires *something* to have
been signed — Apple rejects an unsigned submission — so it is gated on that too.

Mechanically, the conditional lives in exactly two shapes. `codesign_payloads()` is called
unconditionally and returns early when no identity is set (`codesign` has no "sign with nothing"
mode, so this one cannot be an argument). Everywhere else it is an argument array that is empty when
the secret is absent, expanded as `${sign_args[@]+"${sign_args[@]}"}` — one invocation, one argument
list, both paths. `productbuild` is called from exactly one place.

## The honest caveat — macOS is developer-only until signing is real

Risk **R-11**. An unsigned, quarantined **plugin** does not fail the way an unsigned application
does. An application gets Gatekeeper's "Open Anyway" path. A plugin loaded by a DAW gets no
user-visible override at all — it simply fails to load, with no dialog and nothing in the host's UI
to click — and macOS 15 removed the Control-click bypass that used to work.

Installing from the `.pkg` avoids quarantine, which is most of why the `.pkg` exists. Extracting the
`.zip` does not: FR-PKG-050's archive is for people who know that.

The caveat is in three places on purpose, not one: the script's header, the installer's own welcome
pane (shown only when the build is actually unsigned — it is the one screen a user reliably sees
before the plugin silently fails to appear), and a warning printed at the end of every unsigned run.
State it in the release notes too.

## The `Namir.app` wrapper — a known stopgap

`xtask bundle`'s macOS layout stages the standalone as a **bare Mach-O executable named `namir`**,
not as a `Namir.app`. D-18.3 says a release places "the standalone app under `/Applications`". Both
cannot hold:

- a bare executable in `/Applications` is a Unix executable as far as Finder is concerned —
  double-clicking it opens Terminal;
- and an unbundled process cannot declare `NSMicrophoneUsageDescription`, which macOS 10.14+
  requires before a process may open an audio **input** device. Without it the standalone is denied
  the microphone, which for an amplifier simulator is the whole product.

So `assemble_standalone_app()` wraps the staged binary into a minimal `.app`. **That wrapper belongs
in `xtask/src/bundle.rs`, not in a shell script**, because everything else that ships was asserted
by `xtask bundle --check` and this one payload was not. The concrete change is three more `Entry`
rows in `plan(Platform::MacOs)` — `Namir.app/Contents/Info.plist` (`Generated`),
`Namir.app/Contents/PkgInfo` (`Generated`), `Namir.app/Contents/MacOS/namir` (`Build`) — after which
`check` byte-compares that plist exactly as it already does the plugin's.

The script is written so that lands cleanly: if the staging tree already contains a `Namir.app` it
is used as-is and the generator is skipped, so `xtask` growing those rows needs **no change here**.

Two things the wrapper had to decide that properly belong in `docs/`:

- **`CFBundleIdentifier` for the app is `org.legrand.namir.standalone`**, not the plugin's
  `org.legrand.namir`. Bundle identifiers must be unique per bundle, and this one is what TCC keys
  the microphone grant on. `bundle.rs` argues that one product should have one reverse-DNS identity;
  that argument is about the plugin bundle matching `PLUGIN_ID`, and it cannot extend to a second
  bundle installed on the same machine. This wants ratifying in the architecture document.
- **No `CFBundleIconFile`.** There is no `Namir.icns` in the tree. FR-UI-110's icon clauses are
  M13's (D-17.3 deferred both here), and on macOS the icon is just a file in `Contents/Resources`
  named by that key — no build script, no `unsafe`, none of what made the Windows clause hard. It is
  simply not built yet, and is deliberately not invented here.

## Traceability — what this script can and cannot close

`xtask traceability` scans `crates/**` and `xtask/**` `.rs` files, `.github/workflows/{ci,fuzz}.yml`
and the root `Cargo.toml`/`deny.toml`. **`packaging/**` is not on that list**, so a `# trace:` marker
in `make_installer.sh` would be read by nothing, and there is deliberately none in it.

That matters for FR-PKG-040, whose `// trace-partial:` in `xtask/src/bundle.rs` says its gap is that
"the Windows installer, the macOS .pkg/.dmg and the plain archives … do not exist yet, so nothing
asserts the three files inside a produced distribution". This script now asserts exactly that, in
`verify_outputs()` — inside the `.pkg` (via `pkgutil --expand-full`), inside the mounted `.dmg`, and
inside the extracted `.zip`. But a shell assertion is not a traced artifact. Closing the requirement
needs the assertion to exist in Rust under `xtask/`.

## What is untested

Everything, but not equally. In descending order of how likely it is to be wrong:

1. **The per-user install domain.** `enable_currentUserHome="true"` is documented to re-root
   install-locations under the user's home, and is how audio-plugin installers offer a per-user
   scope — but this has not been observed here, and home-directory installs are the least-exercised
   corner of `installer`. If it misbehaves, the fallback is two product archives (`Namir-user.pkg`
   and `Namir-system.pkg`), each with a single fixed install-location, at the cost of a second
   download to explain. **Verify before shipping**: install per-user, then confirm `Namir.clap` is
   at `~/Library/Audio/Plug-Ins/CLAP` and nowhere else.
2. **Whether a host actually loads the installed bundle.** `xtask bundle` gets the *structure*
   right and a test asserts it, but no host has opened the result. Roadmap §20's acceptance clause
   is explicit that the macOS CLAP artifact must "load in a host on a machine where quarantine does
   not apply" — that is a manual check nothing here substitutes for.
3. **The `distribution.xml` details** — `hostArchitectures`, `allowed-os-versions`, the `#pkg`
   references, the disabled-choice syntax. Each is per Apple's Distribution XML reference; none has
   been through `productbuild`. A malformed distribution fails loudly, so this is a "first run
   fails" risk rather than a silent one.
4. **Signing and notarisation.** No identity exists, so the entire signed path — including whether
   signing the dylib before the bundle wrapper is sufficient, and whether the two-submission
   staple order works — is written from documentation. The **unsigned** path is the one every run
   will exercise until a secret exists, which is exactly D-18.3's point.
5. **`pkgbuild --analyze` on the app root.** The `PlistBuddy` loop assumes the component list is an
   array of dicts each carrying `BundleIsRelocatable`, and fails loudly if it finds no bundle at
   all. It has not been run against a real component plist.
6. **`LSMinimumSystemVersion` / `allowed-os-versions` are set to 11.0** on no evidence: nothing in
   the repository states a minimum macOS version, and no CI leg pins a deployment target. Pick this
   deliberately rather than inheriting it from here.
7. **The executable bit.** `make_installer.sh` needs one (`git update-index --chmod=+x`), and it
   `chmod 755`s the standalone binary itself because a staging tree that travelled through
   `actions/upload-artifact` arrives at 0644 and the `.app` then fails to launch saying nothing
   useful.
