<img src="images/namir.png" alt="Namir" width="480">

# Namir

Namir is a real-time guitar and bass amplifier and cabinet simulator. It applies a Neural Amp
Modeler (NAM) profile to an instrument signal and then a cabinet impulse response (IR), with a
noise gate ahead of the amp and a tone EQ after the cabinet. One Rust codebase ships two products
that share a single interface: a standalone native application with its own audio I/O, and a CLAP
plugin hosted inside a DAW.

The primary supported platform is Windows 11 x86-64. Linux and macOS are secondary — they build
and are exercised in CI, but are less thoroughly verified against real hardware.

## Signal chain

The engine runs six fixed stages, always in this order, not user-reorderable:

```
input → noise gate → input trim → NAM → IR → EQ → output level → output
```

The gate comes before the trim deliberately: its detector runs on the raw input, so the threshold
stays referenced to the interface's own noise floor and does not need re-tuning when the trim
changes. `docs/user-guide.md` describes each stage and its controls.

## Status

Namir is pre-1.0 and **not yet packaged**. There are no release binaries, no installer and no
published plugin bundle — distribution is a later milestone (M13, see
`docs/03-implementation-roadmap.md`). Building from source, as described below, is the only way to
run it today, and installing the plugin into a host is a manual copy.

## Building

Prerequisites:

- A stable Rust toolchain, at least **1.97** — the MSRV pinned in `[workspace.package].rust-version`
  in the root `Cargo.toml`, which CI's `msrv` job reads from there and enforces.
- On Linux only: ALSA development headers for `cpal`'s ALSA backend
  (`sudo apt-get install libasound2-dev` on Debian/Ubuntu). Windows and macOS use the WASAPI and
  CoreAudio backends and need nothing extra.
- No C++ toolchain is required — a C compiler is (rustc's linker driver), but nothing in the tree
  needs `g++`/`clang++`. Neither product links a network-capable dependency, and CI gates on that.

Then, from the repository root:

```bash
cargo build --workspace
```

## Running

The standalone application's binary is named `namir` (`namir.exe` on Windows), built by the
`namir-app` crate:

```bash
cargo run -p namir-app
```

It negotiates audio devices, sample rate and buffer size automatically on startup and logs what it
picked; there is no device-selection screen yet. For a release build, use
`cargo build --release -p namir-app` and run `target/release/namir` directly.

The same workspace build also produces the CLAP plugin as a shared library from the `namir-clap`
crate — `namir_clap.dll` on Windows, `libnamir_clap.so` on Linux, `libnamir_clap.dylib` on macOS
(`cargo build --release -p namir-clap`). On Windows and Linux the plugin file is that shared
library renamed to `Namir.clap`; on macOS a `.clap` is a bundle directory rather than a renamed
dylib. Producing the right form for the platform you are on, with the licence texts and the
attribution file beside it, is one command against a release build:

```bash
cargo build --release --workspace
cargo run -p xtask -- bundle
```

That stages `target/bundle/<platform>/` and then asserts what it staged. **Copying it into a
host's CLAP search path is still manual** — `docs/user-guide.md` gives the per-platform install
paths, and the installers and archives the release workflow builds from this same staging tree are
the automated route.

## Testing

The full test suite:

```bash
cargo test --workspace
```

The rest of the local gate — CI runs all of these too, alongside platform-specific jobs of
its own — is:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- layering       # crate dependency-graph and platform-cfg lint
cargo run -p xtask -- rt-logging     # no audio-thread module names the logger
cargo run -p xtask -- params-lock    # params.lock matches the parameter registry
cargo run -p xtask -- attribution    # THIRD-PARTY-NOTICES.md is current
cargo run -p xtask -- identity       # brand mark, README and TRADEMARK.md are current
cargo run -p xtask -- ci-commands    # this file's commands are the ones CI runs
cargo run -p xtask -- traceability   # requirement coverage and generated test-plan diff
cargo deny check                     # licence, advisory and dependency-ban audit
```

`params-lock`, `attribution`, `identity` and `traceability` take `--write` to regenerate their
artifact instead of verifying it. `traceability` also takes `--allow-uncovered`, which is the form
CI gates on until requirement coverage reaches zero gaps; the plain form runs alongside it as an
informational step.

A pre-commit hook running the fast half of the gate (`cargo fmt --check` plus
`cargo check --workspace --all-targets`) is available; opt in once per clone with:

```bash
git config core.hooksPath .githooks
```

## Documentation

- `docs/user-guide.md` — installing, audio setup, the controls, and troubleshooting.
- `docs/01-functional-requirements.md` — what Namir must do; every requirement has an id and a
  stated verification method.
- `docs/02-architecture.md` — how it is built, as numbered decisions with their rationale.
- `docs/03-implementation-roadmap.md` — the order the work happens in, milestone by milestone.
- `AGENTS.md` — the contributor's orientation: crate layering, real-time-safety rules, testing
  conventions and the commands above. Read it before making a non-trivial change.

## Licence

The source code is dual-licensed under either of

- the MIT licence (`LICENSE-MIT`), or
- the Apache Licence, Version 2.0 (`LICENSE-APACHE`),

at your option.

The name "Namir" and the logo and other brand assets under `images/` are **not** covered by that
licence. See `TRADEMARK.md` for their terms.

Third-party dependencies and their licences are listed in `THIRD-PARTY-NOTICES.md`, which is
generated and checked for freshness by `cargo run -p xtask -- attribution`.
