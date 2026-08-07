# FR-IO-090 manual test / known gap: channel mapping and true stereo input

**Requirement (literal, Should):** "The user shall be able to map which hardware input channel
feeds the engine and which hardware output channels receive it."
**Verify: M.**

Should, not Must — recorded honestly as partially built rather than claimed complete.

## What is built

- [`crate::settings::ChannelMapping`] persists an input channel index and two output channel
  indices (FR-IO-080's persistence applies to this data too — round-tripped in
  `settings.rs`'s `settings_round_trip` test).
- [`crate::stream::open`]/`build_input`/`build_output` actually **honour** the mapping: the input
  callback reads `data.chunks_exact(channel_count).map(|frame| frame[channel_index])` (the
  configured physical channel, not always channel 0), and the output callback writes to
  `out_frame[left]`/`out_frame[right]` (the configured physical output channels), verified by
  `crates/namir-app/src/stream.rs`'s own tests using a two-channel fake device.
- No UI to set these interactively — same gap as `fr-io-010-device-enumeration.md`'s device
  selection: `namir-ui`'s shared FR-UI-020 screen has no device/channel-settings panel, and this
  crate builds none of its own (see this crate's final report). `AppSettings::channel_mapping`
  defaults to `None` for every field, meaning "channel 0 / left+right 0+1" — a sensible default
  needing no configuration, but not user-adjustable today.

## What is not built: `ChannelConfig::Stereo` (two genuinely independent input channels)

`crate::stream`'s module doc comment states this explicitly: only `ChannelConfig::Mono` and
`ChannelConfig::MonoToStereo` (one physical input channel, optionally duplicated into two engine
channels) are wired. `ChannelConfig::Stereo` — two physical input channels captured and kept
independent all the way through the chain — needs reading a *second* channel index out of the same
interleaved input buffer and feeding it to `StageIo`'s second channel without duplication, which
`build_output`'s current loop does not do (it always duplicates channel 0's mono-captured value
into every engine channel when `duplicate_into_stereo` is set, or zero-fills the rest otherwise).
`crate::app::run` never selects `ChannelConfig::Stereo` for this reason — it picks `MonoToStereo`
whenever the negotiated output channel count is 2, which is every case this session's real hardware
(a 2-in/2-out AudioBox 22VSL) produced.

## Script, once `ChannelConfig::Stereo` is implemented

1. Select a genuinely stereo source (two different signals on the two input channels — e.g. two
   separate DI/mic sources into a 2-channel interface).
2. Confirm both channels reach the engine independently (FR-CHAIN-060's genuine stereo-downmix
   path in `namir-engine`'s Trim stage, not the `MonoToStereo` duplication path).
3. Confirm remapping `ChannelMapping::input_channel`/`output_channel_left`/`output_channel_right`
   to non-default physical channels (e.g. a 4-in interface, feed the engine from channel 2 instead
   of channel 0) actually changes which physical channel is read/written, by feeding a signal into
   only the remapped channel and confirming it (and not the default channel) reaches the engine.

**Result: PARTIAL.** Channel *remapping* for a mono-sourced signal is built, wired, and unit
tested. Genuine independent-stereo-input (`ChannelConfig::Stereo`) is not built. Neither has an
interactive UI. All three gaps are structural (recorded in code comments) rather than silently
absent.
