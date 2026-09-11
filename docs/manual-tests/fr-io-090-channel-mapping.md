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

## Note appended 2026-09-11 (input-channel selector)

The "No UI to set these interactively" bullet above now overstates the gap for the **input**
channel only, and the recorded PARTIAL verdict is unchanged. `namir-ui`'s audio settings panel
carries an "Input Channel:" combo beside the sample-rate and buffer-size selectors, listing one
entry per channel the open input stream offers, labelled 1-based ("Input 1", "Input 2", …) over the
0-based index `ChannelMapping::input_channel` stores. Choosing one dispatches
`UiIntent::SelectInputChannel`, which `namir-app`'s `AppHost` persists and then reopens the stream
through, the same path `SelectSampleRate` takes. A persisted index the current device does not have
is clamped to the last channel it does offer (`clamp_input_channel`, used by both the selector and
the stream setup), so switching from an 8-in interface to a 2-in one no longer captures silence out
of a channel that is not there.

What that automates of the script above: step 3's *control* half for the input channel, verified in
`crates/namir-ui/tests/ui_interaction_scripts.rs`
(`input_channel_combo_dispatches_the_zero_based_index_of_the_chosen_channel`) and
`crates/namir-app/src/host.rs`
(`a_persisted_input_channel_past_the_device_end_is_clamped_to_an_existing_one`). What still needs a
human with hardware, and so keeps this file's `Verify: M` unpromoted: that the remapped physical
channel is the one actually heard, the output-channel half (no UI for it), and
`ChannelConfig::Stereo`, which remains unbuilt.

### Amendment, same day (review of the note above)

Two things the note as first written did not say, both found in review and now built. **The
selector's range is the device's own reported channel count**, not the count one stream opened
with: `negotiate_channels` prefers the smallest config that suffices for the engine, so a snapshot
fed from the opened stream showed a single entry on an eight-in interface and made every channel
but the first unreachable. The range now comes from `device_state::max_channels_at_rate`, and the
chosen index is passed into `negotiate_channels`' `minimum`, so the stream is opened wide enough
to carry it (`crates/namir-app/src/host.rs`,
`an_eight_input_device_offers_every_channel_and_opens_a_stream_containing_the_chosen_one`).
**A clamped channel is now reported**, as `app.audio_io.input_channel_declined`, rather than
silently substituted — the same treatment a declined buffer size gets.

Still unautomated, and still the reason this file's `Verify: M` is unpromoted: that the channel
selected is the one physically heard needs a signal fed into one input of a real multi-input
interface. No hardware of that shape was available in this session; the eight-input device above
is a fake backend reporting eight configs, which proves the plumbing and not the wiring.
