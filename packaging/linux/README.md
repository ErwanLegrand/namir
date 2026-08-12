# Namir on Linux — tarball and `install.sh`

This directory is the Linux half of M13's packaging work, as
[`docs/02-architecture.md`](../../docs/02-architecture.md) **D-18.3** specifies it: *"Linux —
tarball plus `install.sh`, defaulting to `~/.clap`"*. There is no distro package here, and
deliberately so — a `.deb`/`.rpm`/PKGBUILD trio is three packaging toolchains and three sets of
review conventions for one secondary platform, and D-18.3 chose the archive.

- `install.sh` — installs from the staging tree into either scope, and uninstalls.
- this file — how to use it, and what has and has not been verified.

Windows and macOS packaging live in their own sibling directories under `packaging/`.

---

## What is in the archive

The release tarball is the staging tree that `cargo run -p xtask -- bundle --target linux`
produces and `--check` asserts (`xtask/src/bundle.rs`), plus this directory's two files:

```
namir-<version>-linux-<arch>/
├── Namir.clap              the CLAP plugin — on Linux this is the built libnamir_clap.so,
│                           renamed. (A bundle directory is macOS's rule; see FR-PKG-020.)
├── namir                   the standalone application
├── THIRD-PARTY-NOTICES.md  ┐
├── LICENSE-MIT             ├ FR-PKG-040 — all three in every distribution
├── LICENSE-APACHE          ┘
├── README.md               the project README
├── INSTALL.md              this file
└── install.sh              the installer
```

`install.sh` refuses to run if any of the first six is missing, and says which. That refusal is
what makes FR-PKG-040 observable at install time as well as at bundle time: an archive that lost
a licence text cannot be installed and pretend otherwise.

---

## Installing

Unpack, then run the script. Nothing is compiled and nothing is downloaded.

```sh
tar xzf namir-<version>-linux-<arch>.tar.gz
cd namir-<version>-linux-<arch>
./install.sh                 # per-user — the default, and needs no root
```

System-wide, for every user on the machine:

```sh
sudo sh ./install.sh --system
```

The script prints exactly where each file will go before it writes anything, and `--dry-run`
stops it after that plan and touches nothing.

### Where things go

| | per-user (default) | system-wide (`--system`) |
|---|---|---|
| CLAP plugin | `~/.clap/Namir.clap` | `/usr/lib/clap/Namir.clap` **or** `/usr/lib64/clap/Namir.clap`, detected |
| standalone | `~/.local/bin/namir` | `/usr/local/bin/namir` |
| documents | `~/.local/share/doc/namir/` | `/usr/local/share/doc/namir/` |

The two CLAP rows are [`docs/02-architecture.md`](../../docs/02-architecture.md) **D-13.3**'s Linux
row — the CLAP-specified search paths, and nothing else. The other two rows are this script's own
choice: `~/.local` and `/usr/local` are where locally-installed (non-distro-packaged) software
belongs, and `/usr/bin` is the distribution package manager's territory, not ours.

`--prefix DIR` moves the standalone and the documents. It deliberately does **not** move the
plugin: that directory is fixed by the CLAP specification and by what hosts scan, so relocating it
silently would produce an install that succeeds and a plugin no host ever lists. `--clap-dir DIR`
is the explicit, by-name way to put the plugin somewhere else.

After installing, rescan plugin paths in your host.

### `/usr/lib64` versus `/usr/lib` — why this is detected rather than assumed

D-13.3's table records the system-wide Linux path as `/usr/lib/clap`. That is right for
distributions that keep 64-bit shared objects in `/usr/lib` — Debian, Ubuntu, Arch — and wrong for
the multilib ones: **Fedora, RHEL and openSUSE use `/usr/lib64`, and a host on those systems scans
`/usr/lib64/clap`.** D-18.3 anticipated this and requires the script to *"detect rather than
assume"*, so the detection is a deliberate widening of that table row authorised by the later
decision — not a script quietly disagreeing with the architecture document. Both places say so:
this section, and the comment above `detect_system_clap_dir` in the script itself.

The test is not `[ -d /usr/lib64 ]`, which would be wrong twice over: `/usr/lib64` exists on Debian
and Ubuntu holding only the ELF interpreter, and on Arch it is a compatibility *symlink* to
`/usr/lib`. The script asks the stronger question — is `/usr/lib64` a real directory that actually
holds the C library — and prefers an already-existing `clap` directory over the probe when exactly
one of the two exists, since that is where a host is already looking:

1. `--clap-dir DIR` if given — no detection at all.
2. Exactly one of `/usr/lib64/clap`, `/usr/lib/clap` exists → that one.
3. Otherwise `/usr/lib64/clap` if `/usr/lib64` is a real directory (not a symlink) containing
   `libc.so.6`; `/usr/lib/clap` in every other case.

### `~/.clap` — chosen with an open upstream question behind it

**CLAP issue #46 — whether `~/.clap` or an XDG-conformant path (`$XDG_DATA_HOME/clap`,
`~/.local/share/clap`) is the correct per-user location — is still open upstream.** `~/.clap` is
what the specification says today and what hosts scan today, which is why it is what gets
installed, and the awareness that this may have to become *both* later is recorded here and in the
script rather than left to be rediscovered. The failure mode if this is ever wrong is the reason
it is not guessed at ahead of the specification: a plugin at a path a host does not scan installs
perfectly and simply never appears, with no error message anywhere to search for — D-13.3's own
rationale, found the hard way in S-4.

If #46 resolves in favour of XDG, the expected change here is to install to **both** paths, not to
move; hosts that only ever learned `~/.clap` would otherwise lose the plugin. Today `--clap-dir`
lets a user do that second copy by hand.

---

## Runtime dependencies — and one thing that is easy to get backwards

The **standalone `namir`** needs **`libasound.so.2`** at run time. `cpal`'s ALSA backend links
against the system ALSA library dynamically, so the binary in this archive has a real dependency
on it:

| | package |
|---|---|
| Debian / Ubuntu | `libasound2` (`libasound2t64` on trixie and later) |
| Fedora / RHEL | `alsa-lib` |
| Arch | `alsa-lib` |
| openSUSE | `libasound2` |

**Not `libasound2-dev`.** That is the *build-time* package — headers and `pkg-config` metadata for
compiling `alsa-sys` — which is why CI installs it and why the project README lists it as a build
prerequisite. Installing a binary from this archive needs only the runtime library, which nearly
every desktop system already has.

The **CLAP plugin needs no ALSA at all**: `namir-clap` does not depend on `cpal`, because the host
owns the audio device.

Both products draw their window with egui on **baseview 0.2, which on Linux is X11 with GLX and
has no Wayland backend** — so a Wayland-only session needs XWayland, and a machine with no
GL-capable display cannot open either window. `libGL.so.1` and the X11 client libraries must be
present.

`install.sh` checks for `libasound.so.2` and `libGL.so.1` via `ldconfig -p` and prints what it
finds. Every finding is a warning, never a refusal: installing onto a machine that will not run it
today — an image being prepared, a headless build host — is legitimate.

---

## Uninstalling

```sh
./install.sh --uninstall                 # per-user
sudo sh ./install.sh --system --uninstall
```

It removes exactly what the matching install placed, from the scope it went to: `Namir.clap` from
the plugin directory, `namir` from the bin directory, the four documents and the manifest from the
documents directory, then the documents directory itself **only if it is now empty** — a plain
`rmdir`, never `rm -r`, so anything else you put there survives and the script says so. The plugin
and bin directories are shared with other software and are never removed.

An install writes `install-manifest` beside the documents recording the three directories it used,
and `--uninstall` reads it, so an install made with `--prefix` or `--clap-dir` is undone from those
same paths rather than from the defaults. Only the directories are read back — the file names
removed are the script's own constants — so a corrupted manifest can at worst point the uninstall
at the wrong directory, never name an arbitrary path to delete. Deleting the manifest by hand is
harmless; `--uninstall` falls back to the default paths for the scope you give it.

A system-wide uninstall that finds a `Namir.clap` at the *other* system path (`/usr/lib/clap` when
it was installed to `/usr/lib64/clap`, or the reverse) **reports it and does not remove it** — it
is outside what that install accounted for, and deleting an unaccounted-for file is worse than
naming it.

---

## Building the tarball (FR-PKG-050)

FR-PKG-050 asks for *"a plain archive requiring no installer, containing the same artifacts"*
alongside each platform's installer. On Linux the archive is the whole distribution, and
`install.sh` is a convenience inside it rather than a separate installer — the artifacts can simply
be copied out by hand.

This is the exact command sequence `release.yml` runs, after `xtask bundle --target linux` has
staged and asserted `target/bundle/linux`. **Writing the workflow is not this directory's job**;
this block is the payload it should carry.

```sh
VERSION="${GITHUB_REF_NAME#v}"          # tag v1.2.3 -> 1.2.3
ARCH="$(uname -m)"                      # x86_64
PKG="namir-${VERSION}-linux-${ARCH}"

# Assembled in its own directory, not in the staging tree: `xtask bundle` clears and rebuilds
# target/bundle/linux on every run, so anything added there would be wiped by the next bundle,
# and `bundle --check` stays meaningful for exactly the tree it asserted.
mkdir -p "target/package/${PKG}"
cp -R target/bundle/linux/. "target/package/${PKG}/"
cp packaging/linux/install.sh "target/package/${PKG}/install.sh"
cp packaging/linux/README.md  "target/package/${PKG}/INSTALL.md"
chmod 755 "target/package/${PKG}/install.sh" \
          "target/package/${PKG}/namir" \
          "target/package/${PKG}/Namir.clap"

# Deterministic flags throughout, for NFR-SEC-040: sorted member order, no uid/gid/user names from
# the build machine, a fixed mtime, and `gzip -n` so the compressor writes no timestamp or original
# filename of its own. SOURCE_DATE_EPOCH should be set from the tagged commit's date.
tar --create \
    --directory target/package \
    --format=gnu \
    --sort=name \
    --owner=0 --group=0 --numeric-owner \
    --mtime="@${SOURCE_DATE_EPOCH:-0}" \
    --file - "${PKG}" \
  | gzip -9 -n > "target/package/${PKG}.tar.gz"

# Published alongside the archive: NFR-SEC-040's "publish the hashes regardless", which is worth
# having even where bit-for-bit reproducibility is not achieved.
( cd target/package && sha256sum "${PKG}.tar.gz" > "${PKG}.tar.gz.sha256" )
```

`--format=gnu`, `--sort` and `--owner`/`--group` are GNU `tar` spellings. That is a constraint on
the release runner (`ubuntu-latest`, where GNU `tar` is what is installed), not on the installed
system — `install.sh` itself needs no GNU anything.

---

## What is untested

Stated plainly, because none of it was executed while this was written.

- **`install.sh` has never been run.** Not on any distribution, not in any shell, not in either
  scope, not for install and not for uninstall. It was written against the staging layout in
  `xtask/src/bundle.rs` and reviewed by reading; it has not been executed once, and no shell
  linter (`shellcheck`, `dash -n`) has been run over it either. Treat the first run on each
  distribution as the actual test.
- **The `/usr/lib64` detection has been reasoned, not observed.** The claims it rests on — that
  Fedora/RHEL/openSUSE have a real `/usr/lib64` containing `libc.so.6`, that Debian and Ubuntu have
  a real `/usr/lib64` containing only the ELF interpreter, and that Arch's `/usr/lib64` is a
  symlink — are from knowledge of those layouts, not from a probe run on any of them. If one is
  wrong, `--clap-dir` is the escape hatch and the fix belongs in `detect_system_clap_dir`.
- **No host has been asked whether it scans the path this installs to.** D-18.3 requires the
  equivalent empirical check on Windows (does REAPER really scan the per-user path) before the
  default ships; the Linux equivalent — does a real host on a real distribution list a plugin at
  `~/.clap` and at the detected system path — has not been done here.
- **The runtime-dependency probe is unexercised.** `ldconfig -p` output was not captured on any
  system; the three-state result (found / missing / no usable `ldconfig`) has never been observed.
- **The `tar` command above has not been run**, and neither its reproducibility claim nor the
  resulting archive's contents have been checked.
- **Nothing asserts FR-PKG-040 inside the produced `.tar.gz`.** `xtask bundle`'s
  `// trace-partial: FR-PKG-040` names exactly this gap: the staging tree is the packager's input,
  not a distribution. `install.sh` refusing an incomplete payload is a runtime check on the
  unpacked archive, not the in-process assertion the requirement's `Verify: S` asks for.
- **The staged file names are duplicated.** `install.sh` hard-codes `Namir.clap`, `namir` and the
  four document names that `xtask/src/bundle.rs` also declares. Nothing keeps the two in step; a
  change to the Linux layout there will not fail any check here until someone runs the script.
- **No `.desktop` entry, no icon, no MIME registration** is installed for the standalone. FR-UI-110
  defers both icon clauses to M13, but the ones named are the Windows executable icon and the
  window icon; a Linux desktop entry was not in this lane's scope and is not implemented.
