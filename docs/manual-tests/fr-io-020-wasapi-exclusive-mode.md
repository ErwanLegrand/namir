# FR-IO-020 manual test / known gap: WASAPI exclusive mode

**Requirement (literal, Must):** "On Windows, WASAPI shall be supported in both shared and
exclusive mode."

**Verify: M.**

## This is not a "not yet manually verified" gap — it is a verified absence in the chosen dependency

D-13.1 (`docs/02-architecture.md`) pins `cpal` v0.18.1 for standalone audio I/O. Reading that exact
version's own WASAPI backend source
(`cpal-0.18.1/src/host/wasapi/device.rs`, both `build_input_stream_raw_inner` and
`build_output_stream_raw_inner`) shows the share mode is a hardcoded local:

```rust
let share_mode = Audio::AUDCLNT_SHAREMODE_SHARED;
```

with no parameter, feature flag, or extension trait anywhere in the crate to request
`AUDCLNT_SHAREMODE_EXCLUSIVE` instead. This was checked directly against the vendored source under
`~/.cargo/registry/src/.../cpal-0.18.1/` in this session, not inferred from documentation — `grep
-rn "share_mode\|ShareMode\|AUDCLNT_SHAREMODE" cpal-0.18.1/src` finds exactly the two `SHARED`
assignments above and nothing else. **Exclusive mode is architecturally unreachable through this
dependency as pinned**, on any platform, for any caller.

## Why this crate does not work around it itself

D-5.3 confines `unsafe` code to exactly two crates plus a future SIMD kernel module:
`namir-platform` and `namir-clap`. `namir-app` (this crate) is not on that list and inherits the
workspace's `unsafe_code = "forbid"` lint unmodified (see `crates/namir-app/Cargo.toml`'s own
comment). A raw `IAudioClient::Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, ...)` call bypassing `cpal`
would need `unsafe` COM/WASAPI FFI, which this crate cannot carry. The only in-repo paths to close
this gap are: (a) a `namir-platform`-owned unsafe WASAPI-exclusive helper this crate could call
into, mirroring `DenormalGuard`/`elevate_current_thread_priority`'s existing pattern, or (b) an
upstream `cpal` patch/fork/newer version that exposes share mode. Neither is built here — this is
recorded as a scoping decision for a follow-up, not solved silently.

## What this crate does today

[`crate::settings::AppSettings::exclusive_mode`] exists as a persisted `bool` (forward-compatible
schema, so a future fix needs no settings-format migration), but nothing in
`crate::stream`/`crate::audio_io` reads it — every stream opens in shared mode regardless. There is
no UI control for it either (see `fr-io-010-device-enumeration.md`'s note on the same absent
device-settings surface). WASAPI **shared** mode is fully implemented and verified working against
real hardware — see `fr-io-010-device-enumeration.md`'s executed run.

## Script, if/when a fix lands

1. Toggle exclusive mode on for a device that supports it (most consumer WASAPI-only devices
   accept exclusive mode at their native format).
2. Confirm the stream opens with `ExclusiveModeOutcome::Engaged` and that another application
   attempting to use the same device concurrently is refused by Windows (the defining behavioural
   difference of exclusive mode) — this refusal is the actual observable proof exclusive mode
   engaged, not just an absence of an error from this application.
3. Confirm shared mode still works as a fallback / user choice on devices or configurations that
   reject exclusive mode.

**Result: FAIL (documented, not silently skipped).** FR-IO-020's exclusive-mode half is a Must
requirement this milestone's chosen dependency cannot satisfy without a change outside this crate's
own permitted scope. Flagged prominently in this crate's final report.
