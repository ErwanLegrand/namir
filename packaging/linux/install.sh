#!/bin/sh
#
# install.sh -- install Namir's CLAP plugin and standalone application on Linux.
#
# This is the Linux half of D-18.3's release pipeline (build -> `xtask bundle` -> per-OS package
# -> GitHub Release): "Linux -- tarball plus `install.sh`, defaulting to `~/.clap`". It installs
# from, and only from, the staging tree `cargo run -p xtask -- bundle --target linux` produces and
# `--check` asserts -- exactly six files, no more:
#
#     Namir.clap              the CLAP plugin (on Linux the built `libnamir_clap.so`, renamed;
#                             a bundle directory is macOS's rule, not this platform's)
#     namir                   the standalone application
#     THIRD-PARTY-NOTICES.md  ) FR-PKG-040: all three in every distribution, which is why this
#     LICENSE-MIT             ) script refuses to install a payload missing any of them rather
#     LICENSE-APACHE          ) than quietly installing the parts that are there
#     README.md
#     TRADEMARK.md
#
# The tarball this script ships inside is built by the release workflow; the exact `tar` command
# is in `packaging/linux/README.md` beside this file, not here, because building the archive is
# `release.yml`'s job and installing from it is this script's.
#
# Usage:      ./install.sh [--user | --system] [options]
#             ./install.sh --uninstall [--user | --system]
# Full help:  ./install.sh --help
#
# POSIX `sh`. No bashisms, no arrays, no `local`, no process substitution -- it has to run under
# dash (Debian/Ubuntu `/bin/sh`), busybox ash (Alpine) and bash alike. The default per-user
# install needs no root and asks for none; only `--system` does, and it refuses to escalate on
# your behalf.
#
# ---------------------------------------------------------------------------------------------
# Two things this script is deliberately *not* silent about. Both are named in `02-architecture.md`
# D-18.3 as things to handle rather than paper over, and each has its own comment at the code that
# handles it, below:
#
#   1. `/usr/lib64/clap` versus `/usr/lib/clap` -- see `detect_system_clap_dir`. D-13.3's table
#      records `/usr/lib/clap`; the detection here is a deliberate, D-18.3-authorised widening of
#      that row, not a disagreement with it.
#
#   2. `~/.clap` versus an XDG-conformant per-user path -- see `PER_USER_CLAP_DIR_SUFFIX`. Issue #46
#      is still open upstream; `~/.clap` is what the specification says today and what hosts scan
#      today, and this may need to become "both" later.
# ---------------------------------------------------------------------------------------------

set -eu

# --- What the staging tree contains, by name --------------------------------------------------
#
# These names mirror `xtask/src/bundle.rs`'s `CLAP_ARTIFACT`, `Platform::standalone()`,
# `LICENCE_DOCUMENTS` and `README` for `Platform::Linux`. If that layout ever changes, this list
# changes with it -- there is no mechanism keeping the two in step, which is recorded as a known
# gap in the README beside this file rather than left to be discovered.
CLAP_ARTIFACT='Namir.clap'
STANDALONE='namir'
# The last two are not FR-PKG-040 files -- that set is exactly the first three, named by the
# requirement. TRADEMARK.md rides along because the staged README's licence section points at it and
# NFR-LIC-070 wants the mark's terms stated wherever the mark travels; the installed binaries carry
# the mark. A distribution that names a file and then omits it is worse than one that says nothing.
DOCUMENTS='THIRD-PARTY-NOTICES.md LICENSE-MIT LICENSE-APACHE README.md TRADEMARK.md'

# The per-user CLAP directory, and why it is this literal path.
#
# D-13.3's table: per-user Linux is `~/.clap`, and per-user is the default because installing must
# not need root. That path is what the CLAP specification defines and what hosts scan today.
#
# It is also **contested upstream**: CLAP issue #46 -- whether `~/.clap` or an XDG-conformant path
# (`$XDG_DATA_HOME/clap`, `~/.local/share/clap`) is the right per-user location -- is still open at
# the time of writing. `~/.clap` is chosen here because a path a host does not scan fails
# *silently*: the plugin installs perfectly, the host simply never lists it, and there is no error
# message anywhere to search for (D-13.3's own rationale, established empirically in S-4 against
# Reaper on Windows). Guessing ahead of the specification would trade a working install for a
# silent failure. If #46 resolves in favour of XDG, the fix here is expected to be installing to
# **both** locations rather than moving to the new one, so that hosts which only ever learned the
# old path keep working; `--clap-dir` already lets a user do that by hand today.
PER_USER_CLAP_DIR_SUFFIX='.clap'

SELF='install.sh'

# --- Small helpers ----------------------------------------------------------------------------

say() { printf '%s\n' "$*"; }
note() { printf '  %s\n' "$*"; }
warn() { printf '%s: warning: %s\n' "$SELF" "$*" >&2; }

die() {
    printf '%s: %s\n' "$SELF" "$1" >&2
    exit 1
}

die_usage() {
    printf '%s: %s\n' "$SELF" "$1" >&2
    printf 'Run `%s --help` for usage.\n' "$SELF" >&2
    exit 2
}

# Every mutating action goes through this, so `--dry-run` is one branch rather than a branch per
# call site. The display form loses shell quoting; it is display only.
run() {
    if [ "$DRY_RUN" = 1 ]; then
        printf '  would run: %s\n' "$*"
    else
        "$@"
    fi
}

usage() {
    cat <<'EOF'
install.sh -- install Namir (CLAP plugin + standalone) on Linux.

  ./install.sh                     install for the current user (default; no root needed)
  ./install.sh --system            install system-wide for every user (needs root)
  ./install.sh --uninstall         remove a per-user install
  ./install.sh --system --uninstall
                                   remove a system-wide install (needs root)

Options:
  --user                 per-user scope (the default). CLAP plugin -> ~/.clap
  --system               system-wide scope. CLAP plugin -> /usr/lib64/clap or /usr/lib/clap,
                         detected (see this script's comments for how, and why it is detected)
  --prefix DIR           where the standalone and the documents go. Default ~/.local for
                         --user, /usr/local for --system. Does NOT move the CLAP plugin:
                         that path is fixed by the CLAP specification, not by this script.
  --clap-dir DIR         install the CLAP plugin here instead, overriding all detection
  --payload DIR          read the artifacts from DIR instead of from this script's own
                         directory (for running this script out of a source checkout:
                         --payload target/bundle/linux)
  --uninstall            remove instead of install, from the same scope resolution
  --dry-run              print exactly what would happen, touch nothing
  --help                 this text

Installed layout (per-user defaults shown):
  ~/.clap/Namir.clap                       the CLAP plugin
  ~/.local/bin/namir                       the standalone application
  ~/.local/share/doc/namir/                THIRD-PARTY-NOTICES.md, LICENSE-MIT,
                                           LICENSE-APACHE, README.md, install-manifest

Exit status: 0 success, 1 failure, 2 bad usage.
EOF
}

# --- Argument parsing -------------------------------------------------------------------------
#
# Strict, like `xtask`'s own `bundle`/`traceability` argument parsing and for the same reason: the
# flags here select between *behaviours* (install or uninstall) and between *scopes* (a directory
# under $HOME or one under /usr), so a typo that silently selected a different one of those would
# be worse than a loud refusal. Anything unrecognised exits 2.

SCOPE='user'
PREFIX=''
# Recorded as the flag is parsed rather than inferred later by comparing the resolved prefix
# against the defaults: `--uninstall` needs to tell "the user chose a prefix" from "the default
# applied" before it decides whether the install manifest may override it.
PREFIX_WAS_GIVEN=''
CLAP_DIR_OVERRIDE=''
PAYLOAD_DIR=''
ACTION='install'
DRY_RUN=0

while [ $# -gt 0 ]; do
    case "$1" in
        --user) SCOPE='user' ;;
        --system) SCOPE='system' ;;
        --uninstall) ACTION='uninstall' ;;
        --dry-run) DRY_RUN=1 ;;
        --help | -h)
            usage
            exit 0
            ;;
        --prefix)
            [ $# -ge 2 ] || die_usage '`--prefix` needs a directory'
            PREFIX="$2"
            PREFIX_WAS_GIVEN='yes'
            shift
            ;;
        --prefix=*)
            PREFIX="${1#--prefix=}"
            PREFIX_WAS_GIVEN='yes'
            ;;
        --clap-dir)
            [ $# -ge 2 ] || die_usage '`--clap-dir` needs a directory'
            CLAP_DIR_OVERRIDE="$2"
            shift
            ;;
        --clap-dir=*) CLAP_DIR_OVERRIDE="${1#--clap-dir=}" ;;
        --payload)
            [ $# -ge 2 ] || die_usage '`--payload` needs a directory'
            PAYLOAD_DIR="$2"
            shift
            ;;
        --payload=*) PAYLOAD_DIR="${1#--payload=}" ;;
        *)
            die_usage "unrecognised argument \`$1\` (expected --user, --system, --uninstall,
--prefix DIR, --clap-dir DIR, --payload DIR, --dry-run or --help)"
            ;;
    esac
    shift
done

# --- Where the artifacts are ------------------------------------------------------------------
#
# By default, beside this script: inside the release tarball, `install.sh` sits at the root of the
# unpacked directory with the six staged files. `--payload` covers running this script straight
# out of a source checkout, where it lives in `packaging/linux/` and the artifacts do not.

# `dirname "$0"` without a `--` guard, deliberately: busybox's `dirname` parses no options at all,
# so `dirname -- "$0"` there returns the directory of the literal string `--`. GNU coreutils
# accepts both spellings, busybox only this one.
if [ -z "$PAYLOAD_DIR" ]; then
    PAYLOAD_DIR=$(CDPATH='' cd -- "$(dirname "$0")" && pwd) ||
        die "could not determine this script's own directory; pass --payload DIR"
else
    PAYLOAD_DIR=$(CDPATH='' cd -- "$PAYLOAD_DIR" && pwd) ||
        die "--payload directory does not exist: $PAYLOAD_DIR"
fi

# Every missing artifact, not the first one -- a reader who fixes one and re-runs should not have
# to discover the next by trial. Same posture as `xtask bundle --check`, which lists violations
# rather than aborting on the first.
verify_payload() {
    vp_missing=''
    [ -f "$PAYLOAD_DIR/$CLAP_ARTIFACT" ] || vp_missing="$vp_missing $CLAP_ARTIFACT"
    [ -f "$PAYLOAD_DIR/$STANDALONE" ] || vp_missing="$vp_missing $STANDALONE"
    for vp_doc in $DOCUMENTS; do
        [ -f "$PAYLOAD_DIR/$vp_doc" ] || vp_missing="$vp_missing $vp_doc"
    done

    if [ -d "$PAYLOAD_DIR/$CLAP_ARTIFACT" ]; then
        die "$PAYLOAD_DIR/$CLAP_ARTIFACT is a directory. On Linux the CLAP artifact is the shared
library renamed to $CLAP_ARTIFACT; a bundle directory is macOS's form and no Linux host loads it."
    fi

    if [ -n "$vp_missing" ]; then
        printf '%s: this is not a complete Namir distribution.\n' "$SELF" >&2
        printf 'Missing from %s:\n' "$PAYLOAD_DIR" >&2
        for vp_name in $vp_missing; do
            printf '  - %s\n' "$vp_name" >&2
        done
        printf '\n' >&2
        printf 'The three licence documents are required in every distribution (FR-PKG-040),\n' >&2
        printf 'so a payload missing any of them is refused rather than half-installed.\n' >&2
        printf 'If you are running this from a source checkout, build and stage first:\n' >&2
        printf '  cargo build --release --workspace\n' >&2
        printf '  cargo run -p xtask -- bundle --target linux\n' >&2
        printf '  ./packaging/linux/install.sh --payload target/bundle/linux\n' >&2
        exit 1
    fi
}

# --- The system-wide CLAP directory: detected, not assumed ------------------------------------
#
# D-13.3's table records the Linux system-wide path as `/usr/lib/clap`. That row is right for every
# distribution that keeps 64-bit shared objects in /usr/lib -- Debian, Ubuntu, Arch -- and wrong
# for the multilib ones. Fedora, RHEL and openSUSE put 64-bit libraries in /usr/lib64, and a host
# on those systems scans **/usr/lib64/clap**. D-18.3 anticipated exactly this and requires this
# script to "detect rather than assume", so the detection below is a deliberate widening of that
# table row, authorised by the later decision -- not a script quietly disagreeing with the
# architecture document. If you are reading the two side by side, this comment is the
# reconciliation, and `packaging/linux/README.md` says the same thing in prose.
#
# **The obvious test is wrong.** `[ -d /usr/lib64 ]` is true on Debian and Ubuntu, where
# /usr/lib64 exists and holds only the ELF interpreter (ld-linux-x86-64.so.2), and true on Arch,
# where /usr/lib64 is a compatibility *symlink* to /usr/lib. Neither is a multilib layout and
# neither wants /usr/lib64/clap. The test used here asks the stronger question -- is /usr/lib64 a
# real directory that actually holds the C library:
#
#   Fedora / RHEL / openSUSE   /usr/lib64 real, /usr/lib64/libc.so.6 present  -> /usr/lib64/clap
#   Debian / Ubuntu            /usr/lib64 real, holds only the loader         -> /usr/lib/clap
#   Arch                       /usr/lib64 is a symlink                        -> /usr/lib/clap
#   Alpine / musl / anything else                                             -> /usr/lib/clap
#
# An existing `clap` directory outranks the probe: if exactly one of the two is already there, it
# is where a host is already scanning, and matching it beats being theoretically right.
#
# None of this has been executed on any of those distributions -- see the README's "What is
# untested". `--clap-dir` exists so that a user on a layout this gets wrong is never stuck.
detect_system_clap_dir() {
    if [ -d /usr/lib64/clap ] && [ ! -d /usr/lib/clap ]; then
        printf '%s\n' '/usr/lib64/clap'
        return 0
    fi
    if [ -d /usr/lib/clap ] && [ ! -d /usr/lib64/clap ]; then
        printf '%s\n' '/usr/lib/clap'
        return 0
    fi
    if [ -d /usr/lib64 ] && [ ! -L /usr/lib64 ] && [ -e /usr/lib64/libc.so.6 ]; then
        printf '%s\n' '/usr/lib64/clap'
    else
        printf '%s\n' '/usr/lib/clap'
    fi
}

# --- Resolving the destination ----------------------------------------------------------------

if [ "$SCOPE" = 'user' ]; then
    [ -n "${HOME:-}" ] || die 'HOME is not set, so the per-user paths cannot be resolved.
Pass --prefix and --clap-dir explicitly, or use --system.'
    [ -n "$PREFIX" ] || PREFIX="$HOME/.local"
    DEFAULT_CLAP_DIR="$HOME/$PER_USER_CLAP_DIR_SUFFIX"
    CLAP_DIR_ORIGIN="D-13.3's per-user path"
else
    [ -n "$PREFIX" ] || PREFIX='/usr/local'
    DEFAULT_CLAP_DIR=$(detect_system_clap_dir)
    CLAP_DIR_ORIGIN='detected system-wide path'
fi

if [ -n "$CLAP_DIR_OVERRIDE" ]; then
    CLAP_DIR="$CLAP_DIR_OVERRIDE"
    CLAP_DIR_ORIGIN='--clap-dir'
else
    CLAP_DIR="$DEFAULT_CLAP_DIR"
fi

# The standalone and the documents follow --prefix; the CLAP plugin deliberately does not. The
# plugin's directory is fixed by the CLAP specification and by what hosts scan, so it is not ours
# to relocate -- `--prefix /opt/namir` must not produce a plugin no host can find. `--clap-dir` is
# the way to move it, on purpose and by name.
BIN_DIR="$PREFIX/bin"
DOC_DIR="$PREFIX/share/doc/namir"
MANIFEST="$DOC_DIR/install-manifest"

# --- The install manifest ---------------------------------------------------------------------
#
# Written by an install, read by `--uninstall`, so that an uninstall removes what *this* install
# actually placed even when it was placed with --prefix or --clap-dir. Only the three directories
# are read back; the file names removed are this script's own constants, never strings taken from
# the file -- a corrupted or hand-edited manifest can therefore misdirect an uninstall to a
# directory, but can never name an arbitrary path to delete.

write_manifest() {
    if [ "$DRY_RUN" = 1 ]; then
        printf '  would write: %s\n' "$MANIFEST"
        return 0
    fi
    cat >"$MANIFEST" <<EOF
# Namir install manifest -- written by install.sh, read by \`install.sh --uninstall\`.
# Deleting this file does not break uninstallation; it only means --uninstall falls back to
# recomputing the default paths for the scope you give it.
scope=$SCOPE
clap_dir=$CLAP_DIR
bin_dir=$BIN_DIR
doc_dir=$DOC_DIR
EOF
}

# A recorded directory is used only if it is absolute and not `/` itself.
manifest_dir_ok() {
    case "$1" in
        /) return 1 ;;
        /*)
            case "$1" in
                *..*) return 1 ;;
                *) return 0 ;;
            esac
            ;;
        *) return 1 ;;
    esac
}

read_manifest() {
    [ -f "$MANIFEST" ] || return 1
    rm_clap=''
    rm_bin=''
    rm_doc=''
    while IFS='=' read -r rm_key rm_value; do
        case "$rm_key" in
            clap_dir) rm_clap="$rm_value" ;;
            bin_dir) rm_bin="$rm_value" ;;
            doc_dir) rm_doc="$rm_value" ;;
            *) ;;
        esac
    done <"$MANIFEST"

    if manifest_dir_ok "$rm_clap" && manifest_dir_ok "$rm_bin" && manifest_dir_ok "$rm_doc"; then
        CLAP_DIR="$rm_clap"
        BIN_DIR="$rm_bin"
        DOC_DIR="$rm_doc"
        return 0
    fi
    warn "ignoring $MANIFEST: it does not record three usable absolute directories"
    return 1
}

# --- Runtime dependencies ---------------------------------------------------------------------
#
# CI installs `libasound2-dev` to *build* `namir-app`, because `cpal`'s ALSA backend (`alsa-sys`)
# needs the ALSA development headers and `pkg-config` at compile time. That is not what a user of
# this tarball needs. `alsa-sys` links dynamically against the already-compiled system library, so
# the installed `namir` binary carries a runtime dependency on **libasound.so.2** -- the runtime
# package (`libasound2`/`libasound2t64` on Debian and Ubuntu, `alsa-lib` on Fedora, RHEL, Arch and
# openSUSE), never the `-dev` one. Almost every desktop system already has it; a minimal container
# or a headless server may not, and the failure without it is a loader error at startup rather than
# anything Namir can report for itself. That is the whole reason this check exists.
#
# The CLAP plugin has **no** ALSA dependency: `namir-clap` does not depend on `cpal` at all (the
# host owns the audio device), so a plugin-only user needs none of this.
#
# The GUI, in both products, is egui on baseview 0.2, which on Linux is **X11 with GLX** and has no
# Wayland backend -- so a Wayland-only session needs XWayland, and a machine with no GL-capable
# display cannot open either window. This is not inferred from the crate graph alone: M12 recorded
# `cargo run -p namir-app` panicking in baseview's X11 window open, with `xvfb-run` not helping
# because baseview 0.2.2 needs a GLX-capable display (`03-implementation-roadmap.md` §19's status).
#
# Every finding here is a warning. Installing onto a machine that will not run it today (an image
# being prepared, a headless build host) is legitimate, so nothing below refuses to install.

# 0 = found, 1 = not found, 2 = cannot tell (no usable ldconfig).
library_present() {
    for lp_ldconfig in ldconfig /sbin/ldconfig /usr/sbin/ldconfig; do
        if command -v "$lp_ldconfig" >/dev/null 2>&1; then
            if "$lp_ldconfig" -p 2>/dev/null | grep -F -q "$1"; then
                return 0
            fi
            return 1
        fi
    done
    return 2
}

report_runtime_dependencies() {
    # The status is captured explicitly rather than read out of `$?` in an `else`/`elif` branch:
    # that idiom works in practice but is subtle enough to be a trap for the next reader, and
    # `library_present` has three outcomes rather than two.
    rrd_alsa_status=0
    library_present 'libasound.so.2' || rrd_alsa_status=$?
    rrd_gl_status=0
    library_present 'libGL.so.1' || rrd_gl_status=$?

    say ''
    say 'Runtime dependencies'
    case "$rrd_alsa_status" in
        0) note 'ALSA (libasound.so.2): found.' ;;
        1)
            note 'ALSA (libasound.so.2): NOT FOUND.'
            note "  The standalone \`$STANDALONE\` will fail to start without it, with a loader"
            note '  error rather than a Namir message. Install the ALSA *runtime* library:'
            note '    Debian/Ubuntu   sudo apt-get install libasound2   (libasound2t64 on trixie+)'
            note '    Fedora/RHEL     sudo dnf install alsa-lib'
            note '    Arch            sudo pacman -S alsa-lib'
            note '    openSUSE        sudo zypper install libasound2'
            note '  Not the `-dev`/`-devel` package -- that one is only needed to compile Namir.'
            note "  The CLAP plugin ($CLAP_ARTIFACT) does not need ALSA at all; its host owns the"
            note '  audio device.'
            ;;
        *)
            note 'ALSA (libasound.so.2): could not check (no usable ldconfig on this system).'
            note "  The standalone \`$STANDALONE\` needs it; the CLAP plugin does not."
            ;;
    esac

    case "$rrd_gl_status" in
        0) note 'OpenGL (libGL.so.1): found.' ;;
        1) note 'OpenGL (libGL.so.1): NOT FOUND. Both products need it to draw their window.' ;;
        *)
            note 'OpenGL (libGL.so.1): could not check. Both products need it to draw a window.'
            ;;
    esac

    note 'The interface in both products is X11 + GLX only (baseview 0.2 has no Wayland backend),'
    note 'so a Wayland session needs XWayland, and a machine with no GL-capable display cannot'
    note 'open either window at all.'
}

report_path_advice() {
    case ":${PATH:-}:" in
        *":$BIN_DIR:"*) ;;
        *)
            say ''
            warn "$BIN_DIR is not on your PATH, so \`$STANDALONE\` will not be found by name."
            note "Either add it (e.g. \`export PATH=\"\$PATH:$BIN_DIR\"\` in your shell profile)"
            note "or run it by full path: $BIN_DIR/$STANDALONE"
            ;;
    esac
}

# --- Install ----------------------------------------------------------------------------------

install_one() {
    # $1 source name in the payload, $2 destination directory, $3 mode
    run rm -f "$2/$1"
    run cp "$PAYLOAD_DIR/$1" "$2/$1"
    run chmod "$3" "$2/$1"
    note "$2/$1"
}

do_install() {
    verify_payload

    say "Namir -- installing ($SCOPE scope)"
    say ''
    note "from            $PAYLOAD_DIR"
    note "CLAP plugin  -> $CLAP_DIR/$CLAP_ARTIFACT   [$CLAP_DIR_ORIGIN]"
    note "standalone   -> $BIN_DIR/$STANDALONE"
    note "documents    -> $DOC_DIR/"
    say ''

    if [ "$DRY_RUN" = 1 ]; then
        say 'Dry run -- nothing below is executed.'
    fi

    run mkdir -p "$CLAP_DIR" "$BIN_DIR" "$DOC_DIR"

    say 'Installed:'
    # 755 on the plugin: it is a shared object a host dlopens, and every CLAP installer surveyed
    # ships it executable. 755 on the standalone for the obvious reason; 644 on the documents.
    install_one "$CLAP_ARTIFACT" "$CLAP_DIR" 755
    install_one "$STANDALONE" "$BIN_DIR" 755
    for di_doc in $DOCUMENTS; do
        install_one "$di_doc" "$DOC_DIR" 644
    done

    write_manifest
    [ "$DRY_RUN" = 1 ] || note "$MANIFEST"

    report_runtime_dependencies
    report_path_advice

    say ''
    say 'Done. Rescan plugin paths in your host to pick up the CLAP plugin.'
    say "To remove it again: $SELF --uninstall${SCOPE:+ --$SCOPE}"
}

# --- Uninstall --------------------------------------------------------------------------------
#
# Removes exactly what an install of this scope placed, and nothing else: the two artifacts by
# their own names, the four documents by theirs, the manifest, and then the documents directory
# **only if it is empty** -- a plain `rmdir`, never `rm -r`, so anything a user put there survives
# and says so. The CLAP and bin directories are shared with other software and are never removed.

remove_one() {
    if [ -e "$1" ] || [ -L "$1" ]; then
        run rm -f "$1"
        note "removed  $1"
        UNINSTALL_HITS=$((UNINSTALL_HITS + 1))
    else
        note "absent   $1"
    fi
}

do_uninstall() {
    if [ -z "$CLAP_DIR_OVERRIDE" ] && [ -z "$PREFIX_WAS_GIVEN" ] && read_manifest; then
        MANIFEST="$DOC_DIR/install-manifest"
        UNINSTALL_SOURCE="as recorded in the install manifest"
    else
        UNINSTALL_SOURCE="from the default paths for this scope (no manifest read)"
    fi

    say "Namir -- uninstalling ($SCOPE scope), $UNINSTALL_SOURCE"
    say ''
    if [ "$DRY_RUN" = 1 ]; then
        say 'Dry run -- nothing below is executed.'
    fi

    UNINSTALL_HITS=0
    remove_one "$CLAP_DIR/$CLAP_ARTIFACT"
    remove_one "$BIN_DIR/$STANDALONE"
    for du_doc in $DOCUMENTS; do
        remove_one "$DOC_DIR/$du_doc"
    done
    remove_one "$MANIFEST"

    if [ -d "$DOC_DIR" ] && [ "$DRY_RUN" = 0 ]; then
        if rmdir "$DOC_DIR" 2>/dev/null; then
            note "removed  $DOC_DIR/"
        else
            note "kept     $DOC_DIR/ (not empty -- something else put files there)"
        fi
    fi

    # A plugin at the *other* system-wide path is reported, never removed: it was not placed by
    # this scope's install, and silently deleting a file this run did not account for is worse
    # than telling the user it is there.
    if [ "$SCOPE" = 'system' ]; then
        for du_other in /usr/lib/clap /usr/lib64/clap; do
            if [ "$du_other/$CLAP_ARTIFACT" != "$CLAP_DIR/$CLAP_ARTIFACT" ] &&
                [ -e "$du_other/$CLAP_ARTIFACT" ]; then
                say ''
                warn "$du_other/$CLAP_ARTIFACT also exists and was NOT removed."
                note 'It is outside what this uninstall accounted for. Remove it by hand, or re-run'
                note "with --clap-dir $du_other, if it is a leftover from an earlier install."
            fi
        done
    fi

    say ''
    if [ "$UNINSTALL_HITS" = 0 ]; then
        say 'Nothing was installed at those paths. Nothing removed.'
    else
        say 'Done.'
    fi
}

# --- Go ---------------------------------------------------------------------------------------

# A system-wide install writes under /usr, which needs root. This script neither escalates nor
# re-executes itself under `sudo`: a script that silently acquires privilege is a script whose
# blast radius the user did not agree to. It prints the command instead.
if [ "$SCOPE" = 'system' ] && [ "$DRY_RUN" = 0 ] && [ "$(id -u)" != '0' ]; then
    if [ "$ACTION" = 'uninstall' ]; then
        ROOT_HINT="sudo sh ./$SELF --system --uninstall"
        USER_HINT="./$SELF --uninstall"
    else
        ROOT_HINT="sudo sh ./$SELF --system"
        USER_HINT="./$SELF"
    fi
    die "a system-wide $ACTION needs root, and this script will not escalate for you.
Re-run it as:  $ROOT_HINT
Or use the per-user scope, which needs no root at all:  $USER_HINT"
fi

case "$ACTION" in
    install) do_install ;;
    uninstall) do_uninstall ;;
esac
