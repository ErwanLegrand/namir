# FR-CLAP-030 manual test: audio port configurations, negotiated across real hosts

**Requirement (literal):** the plugin shall declare audio port configurations corresponding to
FR-CHAIN-060 and shall correctly negotiate the configuration the host requests.
*Verify: I across at least two host implementations.*

## What's mechanically true today, and the scope reduction recorded honestly

`crates/namir-clap/src/audio_ports_ext.rs` declares exactly **one stereo input port and one
stereo output port**, in place (`in_place_pair` set), via CLAP's `audio-ports` extension
(`PluginAudioPortsImpl`). This is `clap-validator`'s own confirmation, not a claim: the
`process-audio-basic-in-place`/`-out-of-place` and `layout-audio-ports-*` test groups all ran
against this declaration (`process-audio-basic-*`: PASSED; `layout-audio-ports-config`/
`layout-configurable-audio-ports`/`layout-audio-ports-activation`: SKIPPED, because this round
implements none of the three optional extensions those tests probe).

**Scope reduction, stated in `audio_ports_ext.rs`'s own doc comment too:** FR-CHAIN-060 names
three channel configurations (Mono, Mono→stereo, Stereo). This round declares Stereo only, and
does not implement `audio-ports-config` (the CLAP extension that would let a host pick among
several declared configurations). A host that wants a track fed from a genuinely mono source
still works — CLAP hosts route mono content into a stereo-declared plugin's input by duplicating
or leaving the second channel silent, which is the host's own job, not this plugin's — but this
plugin never *declares* Mono or Mono→stereo as configurations of its own, so "correctly negotiate
the configuration the host requests" is true only in the narrow sense that this plugin correctly
answers `audio-ports.count`/`get` with the one configuration it has, every time, not in the sense
of adapting to more than one.

## Why this needs two real hosts and can't be fully automated

`clap-validator` is one host-shaped test harness, and it is genuinely useful (FR-CLAP-020 already
covers it as the automated half of "at least two hosts"). But it cannot substitute for a second,
independently-implemented CLAP host's own audio-graph logic — how a *specific* DAW's mixer wires a
mono-recorded guitar track through a stereo-declared plugin, whether it upmixes before the plugin
or expects the plugin to, is host UI/routing behaviour no headless validator exercises.

## Script

1. Load `namir_clap.dll` (renamed `namir.clap`, or installed to `namir-platform::clap_paths`'s
   per-user path — see `docs/manual-tests/fr-clap-100-gui-embedding.md` for exact steps) in
   **Reaper**. Create a mono audio track, insert Namir, confirm audio passes through (feed a
   guitar/DI signal, listen for correct routing on both output channels) and the meter (FR-UI-020)
   shows activity.
2. Repeat in a second, independently implemented host (Bitwig Studio, Ableton Live, or
   `clap-validator`'s own out-of-process host mode against a **stereo** track this time).
3. Confirm in each host: the plugin appears with a "Stereo In / Stereo Out" (or equivalent)
   port label, audio passes correctly in both directions, and no host reports a port-count/
   channel-count mismatch warning.

## Executed run (this session)

**Automated half executed, real-host half not executed.** `clap-validator validate` (both
`--in-process` and out-of-process/default modes) ran against the built `namir_clap.dll` in this
session's own environment: **44 tests run, 32 passed, 0 failed, 0 warnings, 12 skipped, exit code
0**, including every `process-audio-*` and `layout-audio-ports-*` test group. This agent session
has no way to launch a GUI DAW (Reaper, Bitwig, Live) — see
`docs/manual-tests/fr-ui-010-standalone-window-renders.md`'s identical note on the same
limitation. The script above is ready to run by a person with two such hosts installed.
