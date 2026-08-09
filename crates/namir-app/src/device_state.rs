//! FR-IO-010/040/080's selection logic: which device, sample rate, buffer size and channel count
//! to open, given what the system reports *today* and what [`crate::settings::AppSettings`]
//! remembered from a previous session. Deliberately pure — every function here takes plain data
//! ([`crate::audio_io::DeviceInfo`]/[`crate::audio_io::SupportedConfigRange`], not a live
//! [`crate::audio_io::AudioBackend`]) so this module is exercised by ordinary unit tests with no
//! real audio hardware, `cpal` call, or even a fake backend implementation — the actual
//! system-facing enumeration happens once, in [`crate::worker`], and its result is handed here.
//!
//! FR-IO-080's "degrade gracefully to a working default if the remembered device is unavailable"
//! is this module's central concern: every `select_*` function accepts a remembered value that
//! may no longer be valid and always returns *something* usable when the input data allows it,
//! rather than failing outright.

use crate::audio_io::{BufferSizeRange, DeviceInfo, SupportedConfigRange};

/// Sample rates tried, in order, when nothing remembered applies — common, widely-supported
/// rates first. Not exhaustive; a device supporting neither falls back further (see
/// [`negotiate_sample_rate`]'s own doc comment).
const PREFERRED_SAMPLE_RATES_HZ: &[u32] = &[48_000, 44_100];

/// Buffer size, in frames, tried when nothing remembered applies — a middle ground between
/// FR-IO's low-latency intent and NFR-RT-030-adjacent glitch margin on an unfamiliar device; a
/// user who wants lower latency picks a smaller one explicitly (FR-IO-040's own "user selectable"
/// clause), this is only the unconfigured starting point.
const PREFERRED_BUFFER_FRAMES: u32 = 256;

/// [`select_device`]'s outcome: which device to use, and whether that took a fallback away from
/// what was remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSelection {
    /// The device to open.
    pub device: DeviceInfo,
    /// `Some(name)` — the remembered device's name — if the remembered choice was unavailable and
    /// this selection is a fallback; `None` if the remembered device (or no remembered choice at
    /// all, on a first run) was honoured directly.
    pub fell_back_from: Option<String>,
}

/// FR-IO-010/080: picks a device from `available`, preferring `remembered` by name if it is still
/// present, otherwise the host's own reported default, otherwise the first device enumerated.
/// Returns `None` only when `available` is empty — nothing this module can do about a system with
/// no devices of the requested direction at all.
pub fn select_device(
    available: &[DeviceInfo],
    remembered: Option<&str>,
) -> Option<DeviceSelection> {
    if let Some(name) = remembered
        && let Some(found) = available.iter().find(|d| d.name == name)
    {
        return Some(DeviceSelection {
            device: found.clone(),
            fell_back_from: None,
        });
    }
    let fallback = available
        .iter()
        .find(|d| d.is_default)
        .or_else(|| available.first())?;
    Some(DeviceSelection {
        device: fallback.clone(),
        fell_back_from: remembered.map(str::to_string),
    })
}

fn config_covers_rate(config: &SupportedConfigRange, hz: u32) -> bool {
    hz >= config.min_sample_rate_hz && hz <= config.max_sample_rate_hz
}

/// FR-IO-040: picks a sample rate `configs` (the *selected* device's own reported ranges)
/// supports. Prefers `remembered` if any config covers it; otherwise tries
/// [`PREFERRED_SAMPLE_RATES_HZ`] in order; otherwise falls back to the highest rate any config
/// reports, on the theory that a device offering only unusual rates should still get a definite
/// answer rather than none. `None` only if `configs` is empty.
pub fn negotiate_sample_rate(
    configs: &[SupportedConfigRange],
    remembered: Option<u32>,
) -> Option<u32> {
    if configs.is_empty() {
        return None;
    }
    if let Some(hz) = remembered
        && configs.iter().any(|c| config_covers_rate(c, hz))
    {
        return Some(hz);
    }
    for &preferred in PREFERRED_SAMPLE_RATES_HZ {
        if configs.iter().any(|c| config_covers_rate(c, preferred)) {
            return Some(preferred);
        }
    }
    configs.iter().map(|c| c.max_sample_rate_hz).max()
}

/// The configs (of `configs`) that actually cover `sample_rate_hz` — every other `negotiate_*`
/// function in this module restricts itself to this subset, since a config's buffer-size/channel
/// range is only meaningful at a rate it covers.
fn configs_at_rate(
    configs: &[SupportedConfigRange],
    sample_rate_hz: u32,
) -> impl Iterator<Item = &SupportedConfigRange> {
    configs
        .iter()
        .filter(move |c| config_covers_rate(c, sample_rate_hz))
}

/// FR-IO-040: picks a buffer size, in frames, for `sample_rate_hz` from whichever of `configs`
/// cover that rate. Prefers `remembered` if it fits some applicable range (or the range is
/// [`BufferSizeRange::Unknown`], which imposes no constraint to check against). Otherwise clamps
/// [`PREFERRED_BUFFER_FRAMES`] into the first applicable range, or returns `None` (meaning "ask
/// the backend for its own default") if that range is [`BufferSizeRange::Unknown`]. Returns `None`
/// if nothing covers `sample_rate_hz` at all — the caller has a rate/config mismatch to handle
/// separately, not a buffer size to pick.
pub fn negotiate_buffer_size(
    configs: &[SupportedConfigRange],
    sample_rate_hz: u32,
    remembered: Option<u32>,
) -> Option<u32> {
    let applicable: Vec<&SupportedConfigRange> = configs_at_rate(configs, sample_rate_hz).collect();
    if applicable.is_empty() {
        return None;
    }
    if let Some(frames) = remembered
        && applicable.iter().any(|c| match c.buffer_size {
            BufferSizeRange::Range { min, max } => frames >= min && frames <= max,
            BufferSizeRange::Unknown => true,
        })
    {
        return Some(frames);
    }
    match applicable[0].buffer_size {
        BufferSizeRange::Range { min, max } => Some(PREFERRED_BUFFER_FRAMES.clamp(min, max)),
        BufferSizeRange::Unknown => None,
    }
}

/// FR-IO-040 (channel count is implied by "select... from those the selected device reports as
/// supported", and the engine needs to know how many interleaved channels a stream will deliver):
/// picks the smallest channel count, among configs covering `sample_rate_hz`, that is at least
/// `minimum` (the engine's own requirement — e.g. 1 for a mono capture read, 2 for a stereo
/// output write) — opening more channels than the engine reads/writes is wasted bandwidth, so
/// "smallest that suffices" is preferred over "largest available". Falls back to the largest
/// channel count reported at all if nothing meets `minimum` (a device with fewer channels than
/// wanted still gets a definite, usable answer — [`crate::stream`] duplicates/sums as needed).
pub fn negotiate_channels(
    configs: &[SupportedConfigRange],
    sample_rate_hz: u32,
    minimum: u16,
) -> Option<u16> {
    let applicable: Vec<&SupportedConfigRange> = configs_at_rate(configs, sample_rate_hz).collect();
    if applicable.is_empty() {
        return None;
    }
    applicable
        .iter()
        .map(|c| c.channels)
        .filter(|&ch| ch >= minimum)
        .min()
        .or_else(|| applicable.iter().map(|c| c.channels).max())
}

/// FR-IO-040 applied to a duplex pair: the engine has one sample rate, so input and output must
/// agree on it. Tries [`negotiate_sample_rate`] against `input_configs` first (input drives the
/// engine's own clock in this crate's design — see `crate::stream`'s module doc comment on why
/// the output callback paces `process`, which is independent of which side's rate this function
/// prefers when picking one), then checks the output side actually covers that same rate; if not,
/// negotiates from the output side instead and re-checks the input side. Returns `None` only if
/// neither device covers any rate the other could also use — a genuine mismatch
/// [`crate::app`] has to report rather than silently pick a wrong one for.
pub fn negotiate_shared_sample_rate(
    input_configs: &[SupportedConfigRange],
    output_configs: &[SupportedConfigRange],
    remembered: Option<u32>,
) -> Option<u32> {
    let from_input = negotiate_sample_rate(input_configs, remembered);
    if let Some(hz) = from_input
        && output_configs.iter().any(|c| config_covers_rate(c, hz))
    {
        return Some(hz);
    }
    let from_output = negotiate_sample_rate(output_configs, remembered);
    if let Some(hz) = from_output
        && input_configs.iter().any(|c| config_covers_rate(c, hz))
    {
        return Some(hz);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, is_default: bool) -> DeviceInfo {
        DeviceInfo {
            name: name.to_string(),
            is_default,
        }
    }

    fn config(
        channels: u16,
        min_hz: u32,
        max_hz: u32,
        buffer: BufferSizeRange,
    ) -> SupportedConfigRange {
        SupportedConfigRange {
            channels,
            min_sample_rate_hz: min_hz,
            max_sample_rate_hz: max_hz,
            buffer_size: buffer,
        }
    }

    // --- select_device: FR-IO-010/080 ---

    #[test]
    fn selects_the_remembered_device_when_still_present() {
        let devices = vec![device("A", true), device("B", false)];
        let selection = select_device(&devices, Some("B")).unwrap();
        assert_eq!(selection.device.name, "B");
        assert!(selection.fell_back_from.is_none());
    }

    /// FR-IO-080's central case: the remembered device is gone, so this degrades to the system
    /// default rather than failing.
    // trace: FR-IO-080
    #[test]
    fn falls_back_to_the_default_device_when_remembered_is_gone() {
        let devices = vec![device("A", true), device("B", false)];
        let selection = select_device(&devices, Some("Gone")).unwrap();
        assert_eq!(selection.device.name, "A");
        assert_eq!(selection.fell_back_from.as_deref(), Some("Gone"));
    }

    #[test]
    fn falls_back_to_the_first_device_when_no_default_is_reported() {
        let devices = vec![device("A", false), device("B", false)];
        let selection = select_device(&devices, None).unwrap();
        assert_eq!(selection.device.name, "A");
        // No prior remembered choice at all (first run) is not a "fallback from" anything.
        assert!(selection.fell_back_from.is_none());
    }

    /// FR-IO-070's non-hardware-dependent half: a device that cannot be opened at all (here,
    /// none present) is handled by returning `None` rather than panicking, which is what lets
    /// `crate::app::run` fall back to `open_window_without_audio` instead of crashing or hanging.
    // trace-partial: FR-IO-070
    // uncovered: FR-IO-070 — the method's named apparatus, a virtual device that can be made to
    // uncovered: fail on demand, does not exist and the tagged test opens no device, its whole body
    // uncovered: asserting that selecting from an empty slice is None, so device removal while in
    // uncovered: use, "stop the stream cleanly" and "allow the user to select another device" are
    // uncovered: all unexercised; closes M9b
    #[test]
    fn no_devices_at_all_yields_none() {
        assert!(select_device(&[], Some("Anything")).is_none());
    }

    #[test]
    fn first_run_with_no_remembered_choice_picks_the_default() {
        let devices = vec![device("A", false), device("B", true)];
        let selection = select_device(&devices, None).unwrap();
        assert_eq!(selection.device.name, "B");
    }

    // --- negotiate_sample_rate: FR-IO-040 ---

    #[test]
    fn prefers_the_remembered_sample_rate_when_supported() {
        let configs = vec![config(2, 44_100, 96_000, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_sample_rate(&configs, Some(88_200)), Some(88_200));
    }

    #[test]
    fn falls_back_to_48khz_when_nothing_remembered_and_supported() {
        let configs = vec![config(2, 44_100, 96_000, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_sample_rate(&configs, None), Some(48_000));
    }

    #[test]
    fn falls_back_to_44_1khz_when_48khz_is_not_supported() {
        let configs = vec![config(2, 22_050, 44_100, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_sample_rate(&configs, None), Some(44_100));
    }

    #[test]
    fn an_unsupported_remembered_rate_is_not_used() {
        let configs = vec![config(2, 44_100, 44_100, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_sample_rate(&configs, Some(192_000)), Some(44_100));
    }

    /// Neither preferred rate is supported: falls back to the highest reported rather than `None`.
    #[test]
    fn falls_back_to_the_highest_reported_rate_when_no_preferred_rate_fits() {
        let configs = vec![config(2, 8_000, 22_050, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_sample_rate(&configs, None), Some(22_050));
    }

    #[test]
    fn empty_configs_yield_no_sample_rate() {
        assert_eq!(negotiate_sample_rate(&[], None), None);
    }

    // --- negotiate_buffer_size: FR-IO-040 ---

    #[test]
    fn clamps_the_preferred_buffer_into_a_reported_range() {
        let configs = vec![config(
            2,
            48_000,
            48_000,
            BufferSizeRange::Range { min: 32, max: 128 },
        )];
        // PREFERRED_BUFFER_FRAMES (256) is above this device's max (128) -- clamp down.
        assert_eq!(negotiate_buffer_size(&configs, 48_000, None), Some(128));
    }

    #[test]
    fn prefers_the_remembered_buffer_size_when_it_fits() {
        let configs = vec![config(
            2,
            48_000,
            48_000,
            BufferSizeRange::Range { min: 32, max: 2048 },
        )];
        assert_eq!(
            negotiate_buffer_size(&configs, 48_000, Some(512)),
            Some(512)
        );
    }

    #[test]
    fn an_unsupported_remembered_buffer_is_not_used() {
        let configs = vec![config(
            2,
            48_000,
            48_000,
            BufferSizeRange::Range { min: 64, max: 512 },
        )];
        // 4 is below this device's minimum -- must not be honoured verbatim.
        assert_eq!(negotiate_buffer_size(&configs, 48_000, Some(4)), Some(256));
    }

    #[test]
    fn unknown_buffer_range_yields_none_meaning_use_the_backend_default() {
        let configs = vec![config(2, 48_000, 48_000, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_buffer_size(&configs, 48_000, None), None);
    }

    #[test]
    fn no_config_covers_the_requested_rate_yields_none() {
        let configs = vec![config(2, 44_100, 44_100, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_buffer_size(&configs, 48_000, None), None);
    }

    // --- negotiate_channels ---

    #[test]
    fn picks_the_smallest_channel_count_meeting_the_minimum() {
        let configs = vec![
            config(1, 48_000, 48_000, BufferSizeRange::Unknown),
            config(2, 48_000, 48_000, BufferSizeRange::Unknown),
            config(8, 48_000, 48_000, BufferSizeRange::Unknown),
        ];
        assert_eq!(negotiate_channels(&configs, 48_000, 2), Some(2));
    }

    #[test]
    fn falls_back_to_the_largest_channel_count_when_minimum_cannot_be_met() {
        let configs = vec![config(1, 48_000, 48_000, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_channels(&configs, 48_000, 2), Some(1));
    }

    #[test]
    fn channels_from_a_config_at_a_different_rate_are_ignored() {
        let configs = vec![config(2, 44_100, 44_100, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_channels(&configs, 48_000, 1), None);
    }

    // --- negotiate_shared_sample_rate ---

    #[test]
    fn picks_a_rate_both_sides_support() {
        let input = vec![config(1, 44_100, 96_000, BufferSizeRange::Unknown)];
        let output = vec![config(2, 44_100, 48_000, BufferSizeRange::Unknown)];
        // 48_000 is preferred and both sides cover it.
        assert_eq!(
            negotiate_shared_sample_rate(&input, &output, None),
            Some(48_000)
        );
    }

    /// The input side alone would pick 48kHz, but the output device cannot do it -- falls back to
    /// negotiating from the output side instead, and that rate is confirmed to work for input too.
    #[test]
    fn falls_back_to_the_output_side_when_the_input_sides_pick_does_not_fit_the_output() {
        let input = vec![config(1, 44_100, 96_000, BufferSizeRange::Unknown)];
        let output = vec![config(2, 44_100, 44_100, BufferSizeRange::Unknown)];
        assert_eq!(
            negotiate_shared_sample_rate(&input, &output, None),
            Some(44_100)
        );
    }

    #[test]
    fn no_shared_rate_yields_none() {
        let input = vec![config(1, 96_000, 96_000, BufferSizeRange::Unknown)];
        let output = vec![config(2, 22_050, 22_050, BufferSizeRange::Unknown)];
        assert_eq!(negotiate_shared_sample_rate(&input, &output, None), None);
    }

    #[test]
    fn a_remembered_rate_both_sides_support_is_used() {
        let input = vec![config(1, 8_000, 192_000, BufferSizeRange::Unknown)];
        let output = vec![config(2, 8_000, 192_000, BufferSizeRange::Unknown)];
        assert_eq!(
            negotiate_shared_sample_rate(&input, &output, Some(88_200)),
            Some(88_200)
        );
    }
}
