//! WAV decoding: the P6 "one hardened place" untrusted WAV bytes go through (NFR-SEC-020,
//! matching `namir-nam/src/wavenet.rs`'s own NFR-SEC-020 comment block, cited from that file's
//! `MAX_*` consts, for the pattern this module mirrors). Every rejection path is a catalogued
//! [`IrLoadError`](crate::error_codes::IrLoadError), never a panic.
//!
//! Supports exactly FR-IR-010's matrix: mono or stereo, 16-bit int / 24-bit int / 32-bit int /
//! 32-bit float, `8_000..=192_000` Hz. Every sample is converted to `f32` in (approximately)
//! `[-1.0, 1.0]` — see [`decode`]'s doc comment for the exact conversion and the empirical tests
//! that prove it, per this crate's build instructions: hound's exact integer sample range per bit
//! depth is not assumed from memory, it is read from hound 3.5.1's own source
//! (`hound::Sample::read` impls for `i32`/`f32` in its `lib.rs`) and then proven by round-tripping
//! known values through `hound::WavWriter` in this module's tests.
//!
//! ---
//!
//! **NFR-SEC-020 ceiling, checked *before* allocating from an untrusted declared size:** hound
//! computes a file's declared frame count (`WavReader::duration()`) directly from the `data`
//! chunk's declared byte length, without ever checking that length against how many bytes the
//! underlying reader actually has (`hound::read::read_until_data` trusts the chunk header). A
//! hostile file can therefore declare an enormous `data` chunk length backed by only a few actual
//! bytes. This module never allocates a buffer sized from that declared count directly: it first
//! caps the number of frames it will *read* to at most `MAX_LOAD_SECONDS` seconds at the file's
//! own declared sample rate (D-9.7's ceiling, applied here because it bounds allocation; the
//! engine-rate truncation check in `convolver.rs` is a *second*, independent application of the
//! same ceiling after resampling, not a substitute for this one) — only that capped count is ever
//! used to size an output `Vec`. A file whose actual byte content runs out before that many frames
//! are read produces an I/O error partway through decoding (mapped to
//! [`error_codes::MALFORMED_WAV`]), not an over-sized allocation attempt.

use std::io::Cursor;

use crate::error_codes::{self, IrLoadError};

/// D-9.7's 10-second ceiling, applied here (at the file's own sample rate) to bound allocation
/// per NFR-SEC-020. `convolver.rs` re-applies the same 10-second ceiling at the *engine* rate
/// after resampling — that is a separate, independent check, not redundant with this one: this
/// one exists purely to keep `decode` itself from over-allocating on a hostile file, regardless of
/// what the caller's `engine_rate` later turns out to be.
const MAX_LOAD_SECONDS: u64 = 10;

/// FR-IR-010's supported sample-rate matrix.
const MIN_SAMPLE_RATE_HZ: u32 = 8_000;
const MAX_SAMPLE_RATE_HZ: u32 = 192_000;

/// A decoded WAV file: sample rate and de-interleaved per-channel sample data.
/// `channel_data[c][i]` is channel `c`'s sample at frame `i`; every channel has the same length,
/// and `channel_data.len()` is the file's channel count (1 or 2, per FR-IR-010).
#[derive(Debug)]
pub(crate) struct DecodedWav {
    pub sample_rate: u32,
    pub channel_data: Vec<Vec<f32>>,
    /// Whether the file's declared duration exceeded `MAX_LOAD_SECONDS` at its own sample rate
    /// and was truncated to fit (D-9.7: "truncate with a report to the user").
    pub was_truncated: bool,
}

/// The header validation `decode` and `probe` both need, factored out so the two never drift:
/// a header that `probe_wav` accepts must be one `decode` would go on to accept too (modulo
/// `EMPTY_IR`, which needs the declared-frame check `probe_wav` also performs — see both
/// callers). Returns the parsed `hound::WavReader` so `decode` can go on to read samples from it
/// without re-parsing the header a second time.
fn open_and_validate_header(bytes: &[u8]) -> Result<hound::WavReader<Cursor<&[u8]>>, IrLoadError> {
    let reader = hound::WavReader::new(Cursor::new(bytes)).map_err(|e| IrLoadError {
        code: error_codes::MALFORMED_WAV,
        detail: e.to_string(),
    })?;
    let spec = reader.spec();

    if !(1..=2).contains(&spec.channels) {
        return Err(IrLoadError {
            code: error_codes::UNSUPPORTED_FORMAT,
            detail: format!("channels = {}, supported range is 1..=2", spec.channels),
        });
    }
    let supported_combo = matches!(
        (spec.bits_per_sample, spec.sample_format),
        (16, hound::SampleFormat::Int)
            | (24, hound::SampleFormat::Int)
            | (32, hound::SampleFormat::Int)
            | (32, hound::SampleFormat::Float)
    );
    if !supported_combo {
        return Err(IrLoadError {
            code: error_codes::UNSUPPORTED_FORMAT,
            detail: format!(
                "bits_per_sample = {}, sample_format = {:?} is not one of 16-bit int, \
                 24-bit int, 32-bit int, 32-bit float",
                spec.bits_per_sample, spec.sample_format
            ),
        });
    }
    if !(MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&spec.sample_rate) {
        return Err(IrLoadError {
            code: error_codes::INVALID_SAMPLE_RATE,
            detail: format!(
                "sample_rate = {} Hz, supported range is {MIN_SAMPLE_RATE_HZ}..={MAX_SAMPLE_RATE_HZ} Hz",
                spec.sample_rate
            ),
        });
    }

    Ok(reader)
}

/// Decodes `bytes` as a WAV file per FR-IR-010's matrix. See the module doc comment for the
/// NFR-SEC-020 allocation ceiling and the conversion this performs.
pub(crate) fn decode(bytes: &[u8]) -> Result<DecodedWav, IrLoadError> {
    let mut reader = open_and_validate_header(bytes)?;
    let spec = reader.spec();

    let declared_frames = reader.duration() as u64;
    if declared_frames == 0 {
        return Err(IrLoadError {
            code: error_codes::EMPTY_IR,
            detail: "0 audio frames".to_string(),
        });
    }

    // NFR-SEC-020: the frame count used to size every allocation below is capped *before* any
    // allocation happens, regardless of what the file's header declares.
    let cap_frames = MAX_LOAD_SECONDS * spec.sample_rate as u64;
    let frames_to_read = declared_frames.min(cap_frames) as usize;
    let was_truncated = declared_frames > cap_frames;

    let channels = spec.channels as usize;
    let mut channel_data: Vec<Vec<f32>> = (0..channels)
        .map(|_| Vec::with_capacity(frames_to_read))
        .collect();

    let total_samples = frames_to_read * channels;
    match spec.sample_format {
        hound::SampleFormat::Int => {
            let divisor = 2f32.powi(spec.bits_per_sample as i32 - 1);
            let mut samples = reader.samples::<i32>();
            for i in 0..total_samples {
                let s = samples.next().ok_or_else(|| IrLoadError {
                    code: error_codes::MALFORMED_WAV,
                    detail: format!(
                        "declared {frames_to_read} frames but data ran out at sample {i}"
                    ),
                })?;
                let s = s.map_err(|e| IrLoadError {
                    code: error_codes::MALFORMED_WAV,
                    detail: e.to_string(),
                })?;
                channel_data[i % channels].push(s as f32 / divisor);
            }
        }
        hound::SampleFormat::Float => {
            let mut samples = reader.samples::<f32>();
            for i in 0..total_samples {
                let s = samples.next().ok_or_else(|| IrLoadError {
                    code: error_codes::MALFORMED_WAV,
                    detail: format!(
                        "declared {frames_to_read} frames but data ran out at sample {i}"
                    ),
                })?;
                let s = s.map_err(|e| IrLoadError {
                    code: error_codes::MALFORMED_WAV,
                    detail: e.to_string(),
                })?;
                channel_data[i % channels].push(s);
            }
        }
    }

    Ok(DecodedWav {
        sample_rate: spec.sample_rate,
        channel_data,
        was_truncated,
    })
}

/// A WAV file's sample encoding, mirrored from `hound::SampleFormat` rather than re-exported
/// directly — `namir-library` (M5) consumes [`WavInfo`] without taking a `hound` dependency of
/// its own, matching how [`DecodedWav`] already keeps hound's types out of this crate's own
/// public surface (`DecodedWav` is `pub(crate)`, but `WavInfo`, added M5, is not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 16-, 24- or 32-bit signed integer PCM.
    Int,
    /// 32-bit IEEE float.
    Float,
}

/// Header-only information about a WAV file — everything [`decode`] validates, minus the audio
/// data itself. Added M5 so `namir-library`'s scan can learn an IR's native sample rate, channel
/// count and format without decoding, resampling, or building a convolution schedule for it, none
/// of which a library index has any use for (and the latter two need an `engine_rate`/`block_size`
/// this crate's caller doesn't have during a scan — see [`probe_wav`]'s own doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    /// The file's own sample rate, before any resampling `PreparedIr::from_wav_bytes` would apply.
    pub sample_rate: u32,
    /// 1 (mono) or 2 (stereo) — the same range [`decode`] enforces.
    pub channels: u16,
    /// 16, 24 or 32 — one of the depths [`decode`]'s FR-IR-010 matrix supports.
    pub bits_per_sample: u16,
    /// Integer or float PCM — see [`SampleFormat`].
    pub sample_format: SampleFormat,
    /// The file header's **own declared** frame count, exactly as `hound::WavReader::duration`
    /// reports it. **Untrusted — never size an allocation from this value.** This module's own
    /// doc comment records why: hound derives it from the `data` chunk's declared byte length
    /// without checking that length against how many bytes the file actually has, so a hostile
    /// file can declare an enormous count backed by only a few real bytes. A caller wanting to
    /// display "≈4m32s" or sort by length may use it; a caller sizing a `Vec` must not, and
    /// `probe_wav` itself never does.
    pub declared_frames: u64,
}

/// Reads a WAV file's header only — no sample data, no allocation proportional to the file's
/// length, no resampling. FR-IR-030's resample-to-engine-rate step and D-9.4's convolution
/// schedule both need an `engine_rate`/`block_size` that only `namir-engine`'s stage has, and
/// D-5.1 forbids `namir-library` (this function's motivating caller) from depending on
/// `namir-engine` at all — so `probe_wav` is deliberately shallower than
/// [`PreparedIr::from_wav_bytes`](crate::PreparedIr::from_wav_bytes): it answers "what is this
/// file", not "how would this engine play it".
///
/// Applies the same FR-IR-010 format/channel/rate validation `decode` does, and a file it rejects
/// fails with the identical catalogued [`IrLoadError`] `decode` would give the same bytes — with
/// one deliberate exception: a zero-frame file probes successfully (it is a legitimate, if
/// useless, library entry to display) but `decode` still refuses to load it
/// (`error_codes::EMPTY_IR`), because "usable as a convolution kernel" is a stronger, load-time
/// judgment this shallower header check does not make. So "probes successfully" means "a library
/// entry worth indexing", not "guaranteed loadable" — a caller that wants the stronger guarantee
/// still has to call `decode`/`PreparedIr::from_wav_bytes`.
pub fn probe_wav(bytes: &[u8]) -> Result<WavInfo, IrLoadError> {
    let reader = open_and_validate_header(bytes)?;
    let spec = reader.spec();
    Ok(WavInfo {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        bits_per_sample: spec.bits_per_sample,
        sample_format: match spec.sample_format {
            hound::SampleFormat::Int => SampleFormat::Int,
            hound::SampleFormat::Float => SampleFormat::Float,
        },
        declared_frames: reader.duration() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a small in-memory WAV via `hound::WavWriter` with known integer sample values at
    /// `bits_per_sample`, and returns the encoded bytes. `frames` is per-channel; values are
    /// supplied pre-interleaved (`values.len() == frames * channels`).
    fn write_int_wav(
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        values: &[i32],
    ) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = hound::WavWriter::new(cursor, spec).unwrap();
            for &v in values {
                writer.write_sample(v).unwrap();
            }
            writer.finalize().unwrap();
        }
        buf
    }

    fn write_float_wav(sample_rate: u32, channels: u16, values: &[f32]) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = hound::WavWriter::new(cursor, spec).unwrap();
            for &v in values {
                writer.write_sample(v).unwrap();
            }
            writer.finalize().unwrap();
        }
        buf
    }

    // -------------------------------------------------------------------------------------
    // Empirical round-trip proofs: 0.0, 0.5, -0.5, near +1.0, near -1.0 through
    // hound::WavWriter -> our decode() -> compare against the expected f32, per this crate's
    // build instruction not to trust a formula derived from documentation alone.
    // -------------------------------------------------------------------------------------

    // trace: FR-IR-010
    #[test]
    fn decodes_16bit_mono_round_trip() {
        // i16 full-scale is -32768..=32767; 0.5 * 32767 rounds to 16383 (hound narrows via `as`).
        let values = [0i32, 16384, -16384, 32767, -32768];
        let bytes = write_int_wav(48_000, 1, 16, &values);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.channel_data.len(), 1);
        assert_eq!(decoded.sample_rate, 48_000);
        let expected = [0.0, 0.5, -0.5, 32767.0 / 32768.0, -1.0];
        for (got, want) in decoded.channel_data[0].iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-3, "got {got}, want {want}");
        }
    }

    // trace: FR-IR-010
    #[test]
    fn decodes_16bit_stereo_round_trip() {
        // Interleaved L,R,L,R,...
        let values = [0i32, 0, 16384, -16384, -32768, 32767];
        let bytes = write_int_wav(44_100, 2, 16, &values);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.channel_data.len(), 2);
        let left = &decoded.channel_data[0];
        let right = &decoded.channel_data[1];
        assert!((left[0] - 0.0).abs() < 1e-3);
        assert!((right[0] - 0.0).abs() < 1e-3);
        assert!((left[1] - 0.5).abs() < 1e-3);
        assert!((right[1] - (-0.5)).abs() < 1e-3);
        assert!((left[2] - (-1.0)).abs() < 1e-3);
        assert!((right[2] - (32767.0 / 32768.0)).abs() < 1e-3);
    }

    // trace: FR-IR-010
    #[test]
    fn decodes_24bit_mono_round_trip() {
        let full = 1i32 << 23; // 8_388_608
        let values = [0i32, full / 2, -(full / 2), full - 1, -full];
        let bytes = write_int_wav(48_000, 1, 24, &values);
        let decoded = decode(&bytes).unwrap();
        let expected = [0.0, 0.5, -0.5, (full - 1) as f32 / full as f32, -1.0];
        for (got, want) in decoded.channel_data[0].iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
        }
    }

    // trace: FR-IR-010
    #[test]
    fn decodes_24bit_stereo_round_trip() {
        let full = 1i32 << 23;
        let values = [0i32, full - 1, full / 2, -(full / 2), -full, 0];
        let bytes = write_int_wav(96_000, 2, 24, &values);
        let decoded = decode(&bytes).unwrap();
        let left = &decoded.channel_data[0];
        let right = &decoded.channel_data[1];
        assert!((left[0] - 0.0).abs() < 1e-6);
        assert!((right[0] - (full - 1) as f32 / full as f32).abs() < 1e-6);
        assert!((left[1] - 0.5).abs() < 1e-6);
        assert!((right[1] - (-0.5)).abs() < 1e-6);
        assert!((left[2] - (-1.0)).abs() < 1e-6);
        assert!((right[2] - 0.0).abs() < 1e-6);
    }

    // trace: FR-IR-010
    #[test]
    fn decodes_32bit_int_mono_round_trip() {
        let full = 1i64 << 31; // 2_147_483_648
        let values = [
            0i32,
            (full / 2) as i32,
            -(full / 2) as i32,
            i32::MAX,
            i32::MIN,
        ];
        let bytes = write_int_wav(48_000, 1, 32, &values);
        let decoded = decode(&bytes).unwrap();
        let expected = [0.0, 0.5, -0.5, i32::MAX as f32 / full as f32, -1.0];
        for (got, want) in decoded.channel_data[0].iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
        }
    }

    // trace: FR-IR-010
    #[test]
    fn decodes_32bit_int_stereo_round_trip() {
        let full = 1i64 << 31;
        let values = [
            0i32,
            i32::MAX,
            (full / 2) as i32,
            -(full / 2) as i32,
            i32::MIN,
            0,
        ];
        let bytes = write_int_wav(192_000, 2, 32, &values);
        let decoded = decode(&bytes).unwrap();
        let left = &decoded.channel_data[0];
        let right = &decoded.channel_data[1];
        assert!((left[0] - 0.0).abs() < 1e-6);
        assert!((right[0] - i32::MAX as f32 / full as f32).abs() < 1e-6);
        assert!((left[1] - 0.5).abs() < 1e-6);
        assert!((right[1] - (-0.5)).abs() < 1e-6);
        assert!((left[2] - (-1.0)).abs() < 1e-6);
        assert!((right[2] - 0.0).abs() < 1e-6);
    }

    // trace: FR-IR-010
    #[test]
    fn decodes_32bit_float_mono_round_trip() {
        let values: [f32; 5] = [0.0, 0.5, -0.5, 0.999, -1.0];
        let bytes = write_float_wav(48_000, 1, &values);
        let decoded = decode(&bytes).unwrap();
        for (got, want) in decoded.channel_data[0].iter().zip(values.iter()) {
            assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
        }
    }

    // trace: FR-IR-010
    #[test]
    fn decodes_32bit_float_stereo_round_trip() {
        let values: [f32; 6] = [0.0, 0.999, 0.5, -0.5, -1.0, 0.0];
        let bytes = write_float_wav(44_100, 2, &values);
        let decoded = decode(&bytes).unwrap();
        let left = &decoded.channel_data[0];
        let right = &decoded.channel_data[1];
        assert!((left[0] - 0.0).abs() < 1e-6);
        assert!((right[0] - 0.999).abs() < 1e-6);
        assert!((left[1] - 0.5).abs() < 1e-6);
        assert!((right[1] - (-0.5)).abs() < 1e-6);
        assert!((left[2] - (-1.0)).abs() < 1e-6);
        assert!((right[2] - 0.0).abs() < 1e-6);
    }

    // -------------------------------------------------------------------------------------
    // Rejection paths — catalogued Result errors, never panics.
    // -------------------------------------------------------------------------------------

    #[test]
    fn rejects_non_wav_bytes() {
        let err = decode(b"not a wav file at all").unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_WAV.id);
    }

    #[test]
    fn rejects_empty_byte_slice() {
        let err = decode(&[]).unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_WAV.id);
    }

    #[test]
    fn rejects_too_many_channels() {
        let spec = hound::WavSpec {
            channels: 3,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = hound::WavWriter::new(cursor, spec).unwrap();
            for _ in 0..3 {
                writer.write_sample(0i16).unwrap();
            }
            writer.finalize().unwrap();
        }
        let err = decode(&buf).unwrap_err();
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_FORMAT.id);
    }

    #[test]
    fn rejects_out_of_range_sample_rate_too_low() {
        let bytes = write_int_wav(4_000, 1, 16, &[0]);
        let err = decode(&bytes).unwrap_err();
        assert_eq!(err.code.id, error_codes::INVALID_SAMPLE_RATE.id);
    }

    #[test]
    fn rejects_out_of_range_sample_rate_too_high() {
        let bytes = write_int_wav(200_000, 1, 16, &[0]);
        let err = decode(&bytes).unwrap_err();
        assert_eq!(err.code.id, error_codes::INVALID_SAMPLE_RATE.id);
    }

    #[test]
    fn rejects_empty_ir() {
        let bytes = write_int_wav(48_000, 1, 16, &[]);
        let err = decode(&bytes).unwrap_err();
        assert_eq!(err.code.id, error_codes::EMPTY_IR.id);
    }

    // trace: FR-IR-010
    #[test]
    fn accepts_boundary_sample_rates() {
        let low = write_int_wav(8_000, 1, 16, &[0]);
        assert!(decode(&low).is_ok());
        let high = write_int_wav(192_000, 1, 16, &[0]);
        assert!(decode(&high).is_ok());
    }

    /// NFR-SEC-020: `MAX_LOAD_SECONDS` is this module's documented allocation bound (see the
    /// module doc comment) — a file declaring more frames than it permits is capped to the bound
    /// and the capping is reported (`was_truncated`), rather than sizing a buffer from the
    /// untrusted declared count.
    // trace: NFR-SEC-020
    #[test]
    fn truncates_and_reports_ir_longer_than_ten_seconds_at_file_rate() {
        // 8 kHz * 11 s of frames, mono, cheap to synthesize and decode.
        let sample_rate = 8_000u32;
        let frames = sample_rate as usize * 11;
        let values: Vec<i32> = (0..frames).map(|_| 0).collect();
        let bytes = write_int_wav(sample_rate, 1, 16, &values);
        let decoded = decode(&bytes).unwrap();
        assert!(decoded.was_truncated);
        assert_eq!(decoded.channel_data[0].len(), sample_rate as usize * 10);
    }

    #[test]
    fn does_not_truncate_ir_at_or_under_ten_seconds_at_file_rate() {
        let sample_rate = 8_000u32;
        let frames = sample_rate as usize * 10;
        let values: Vec<i32> = (0..frames).map(|_| 0).collect();
        let bytes = write_int_wav(sample_rate, 1, 16, &values);
        let decoded = decode(&bytes).unwrap();
        assert!(!decoded.was_truncated);
        assert_eq!(decoded.channel_data[0].len(), frames);
    }

    // -------------------------------------------------------------------------------------
    // probe_wav (added M5): header-only reads.
    // -------------------------------------------------------------------------------------

    /// Hand-assembles minimal WAV bytes whose `data` chunk **declares** `declared_data_bytes`
    /// but is backed by only `real_data.len()` actual bytes — the hostile shape this module's
    /// own doc comment warns `hound::WavReader::duration()` trusts blindly. `hound::WavWriter`
    /// cannot produce this (it always writes the length it actually wrote), so this is built by
    /// hand, one field at a time, matching the canonical RIFF/WAVE layout.
    fn wav_with_declared_length_exceeding_actual_bytes(
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        declared_data_bytes: u32,
        real_data: &[u8],
    ) -> Vec<u8> {
        let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
        let block_align = channels * (bits_per_sample / 8);
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        // Overall RIFF chunk size: also lies, consistently with the data chunk, but hound is not
        // observed to validate this one against the reader's actual length either. `wrapping_add`
        // because the whole point of this helper is to write a size field the real byte count
        // doesn't back -- an overflow here would just be a different flavour of the same lie.
        buf.extend_from_slice(&36u32.wrapping_add(declared_data_bytes).to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size (PCM)
        buf.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits_per_sample.to_le_bytes());
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&declared_data_bytes.to_le_bytes()); // the lie
        buf.extend_from_slice(real_data); // far fewer bytes than declared
        buf
    }

    #[test]
    fn probe_wav_reads_header_fields_matching_decode() {
        let bytes = write_int_wav(44_100, 2, 24, &[0, 0, 100, -100]);
        let info = probe_wav(&bytes).unwrap();
        assert_eq!(info.sample_rate, 44_100);
        assert_eq!(info.channels, 2);
        assert_eq!(info.bits_per_sample, 24);
        assert_eq!(info.sample_format, SampleFormat::Int);
        assert_eq!(info.declared_frames, 2);
    }

    #[test]
    fn probe_wav_reads_float_format() {
        let bytes = write_float_wav(48_000, 1, &[0.0, 0.5]);
        let info = probe_wav(&bytes).unwrap();
        assert_eq!(info.sample_format, SampleFormat::Float);
    }

    /// The reason `probe_wav` exists rather than `namir-library` calling `decode` and discarding
    /// the samples: a header declaring gigabytes of audio, backed by only a handful of real
    /// bytes, must be probed without any allocation sized from that declared length — because
    /// `probe_wav` never allocates a sample buffer at all, this is true by construction, and this
    /// test is the tripwire that would catch a future change accidentally adding one.
    #[test]
    fn probe_wav_does_not_allocate_from_a_hostile_declared_length() {
        // Declares ~4 GiB of 16-bit mono audio data (over an hour at 192 kHz), backed by 4 real
        // bytes -- i.e. 2 real frames. A buffer actually sized from the declared length would be
        // a multi-gigabyte allocation attempt; this must return promptly instead.
        let declared_data_bytes = 0xFFFF_FFF0u32;
        let real_data = 4i16.to_le_bytes(); // one real i16 sample
        let bytes = wav_with_declared_length_exceeding_actual_bytes(
            192_000,
            1,
            16,
            declared_data_bytes,
            &real_data,
        );

        let info = probe_wav(&bytes).unwrap();
        assert_eq!(info.sample_rate, 192_000);
        // declared_frames reflects the header's lie, exactly as this type's doc comment warns --
        // callers must treat it as untrusted, which is why decode() re-derives its own capped
        // frame count independently rather than trusting this value for anything but display.
        assert_eq!(
            info.declared_frames,
            (declared_data_bytes / 2) as u64,
            "declared_frames must reflect the header field verbatim, untrusted or not"
        );

        // decode() on the same hostile bytes must reject it as malformed (data runs out before
        // the declared frame count), not attempt to honour the declared length -- confirming
        // probe_wav's promise ("a file probe_wav accepts, decode would go on to accept") holds
        // even when the header lies: both agree the file is unusable, for the same reason.
        let err = decode(&bytes).unwrap_err();
        assert_eq!(err.code.id, error_codes::MALFORMED_WAV.id);
    }

    #[test]
    fn probe_wav_rejects_the_same_malformed_bytes_decode_rejects() {
        let probe_err = probe_wav(b"not a wav file at all").unwrap_err();
        let decode_err = decode(b"not a wav file at all").unwrap_err();
        assert_eq!(probe_err.code.id, decode_err.code.id);
        assert_eq!(probe_err.code.id, error_codes::MALFORMED_WAV.id);
    }

    #[test]
    fn probe_wav_rejects_unsupported_channel_count() {
        let spec = hound::WavSpec {
            channels: 3,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = hound::WavWriter::new(cursor, spec).unwrap();
            writer.write_sample(0i16).unwrap();
            writer.write_sample(0i16).unwrap();
            writer.write_sample(0i16).unwrap();
            writer.finalize().unwrap();
        }
        let err = probe_wav(&buf).unwrap_err();
        assert_eq!(err.code.id, error_codes::UNSUPPORTED_FORMAT.id);
    }

    #[test]
    fn probe_wav_rejects_out_of_range_sample_rate() {
        let bytes = write_int_wav(4_000, 1, 16, &[0]);
        let err = probe_wav(&bytes).unwrap_err();
        assert_eq!(err.code.id, error_codes::INVALID_SAMPLE_RATE.id);
    }

    /// Unlike `decode`, `probe_wav` does not reject a zero-frame file: `EMPTY_IR` is a
    /// convolution-usability judgment (D-9's "an IR with nothing in it is not a usable IR"), not
    /// a header-shape fact. An empty file is a legitimate, if useless, library entry to index and
    /// display -- `decode` remains the gate that refuses to actually load it.
    #[test]
    fn probe_wav_accepts_a_zero_frame_file_decode_would_reject() {
        let bytes = write_int_wav(48_000, 1, 16, &[]);
        let info = probe_wav(&bytes).unwrap();
        assert_eq!(info.declared_frames, 0);
        assert_eq!(
            decode(&bytes).unwrap_err().code.id,
            error_codes::EMPTY_IR.id
        );
    }
}
