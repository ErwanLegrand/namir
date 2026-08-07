# FR-IO-030 manual test: ALSA (Linux) and CoreAudio (macOS)

**Requirement (literal, Must):** "On Linux, ALSA shall be supported ... On macOS, CoreAudio shall
be supported."

**Verify: M per platform.**

## Why this could not be executed in this session

This crate was authored and tested entirely on Windows 11 (this session's only available machine —
see `docs/02-architecture.md`'s own precedent for recording this kind of platform asymmetry
honestly, e.g. the `mobile-cross-build-ios` CI job's comment: "this authoring machine is Windows
and cannot run Xcode/xcrun at all"). Real ALSA and CoreAudio hardware/OS access was not available.

## What is structurally true regardless of platform

[`crate::audio_io::CpalBackend`] is not Windows-specific code: it calls only `cpal`'s
cross-platform `HostTrait`/`DeviceTrait`/`StreamTrait` API
(`cpal::available_hosts`/`host_from_id`/`default_host`, `Device::supported_input/output_configs`,
`Device::build_input/output_stream`), never anything behind `#[cfg(windows)]` — confirmed by
`xtask layering`'s own platform-cfg scan passing clean over
`crates/namir-app/src/*.rs` (that scan flags `#[cfg(target_os`/`#[cfg(windows`/`#[cfg(unix` outside
`namir-platform`; this crate has none of those). `cpal` itself, not this crate, is what selects
ALSA on Linux and CoreAudio on macOS at compile time via its own internal `#[cfg(target_os)]`
gates — D-13.1's whole point in choosing `cpal` was exactly this: one crate whose Namir-owned
trait wrapper needs no per-OS branching of its own.

This crate's CI (`.github/workflows/ci.yml`'s `build-test` matrix) does build and run
`cargo test --workspace` on `ubuntu-latest` and `macos-latest`, which compiles `cpal`'s ALSA and
CoreAudio backends respectively and runs every automated test in this crate against them (the pure
`device_state`/`settings`/`bridge`/`xrun`/`latency` tests, which are platform-independent by
construction — none of them touches a real device). This session added the one Linux-specific
build requirement CI needed (`libasound2-dev`, for `alsa-sys`'s link-time dependency — see
`ci.yml`'s own comment on that step) after finding it by reading `alsa-sys`'s `build.rs` directly,
not by running the CI job (no CI execution available in this session either).

## Script, for whoever has Linux/macOS hardware available

1. `cargo run --example list_devices -p namir-app` — confirm ALSA (Linux) / CoreAudio (macOS)
   devices enumerate with real names and configuration ranges, the same shape
   `fr-io-010-device-enumeration.md`'s Windows/WASAPI run already confirmed.
2. `cargo run --bin namir` — confirm a real window opens and an audio stream opens against a real
   device, the same shape as that Windows run.
3. On Linux specifically: confirm the ALSA backend degrades sensibly if PulseAudio/PipeWire has
   claimed exclusive access to a device (FR-IO-030's own "PipeWire and/or JACK support is Should"
   notes this is a known area of platform variance) rather than hanging or crashing.

**Result: NOT EXECUTED this session (no Linux/macOS hardware available).** Structural evidence
(platform-cfg scan, `cpal`'s own cross-platform trait design, the CI matrix already covering both
OSes for the platform-independent test suite) supports this working, but per this project's own
manual-test convention, "structurally likely" is recorded as exactly that, not asserted as a
result nobody observed.
