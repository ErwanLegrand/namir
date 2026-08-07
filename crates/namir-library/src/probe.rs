//! Bytes → [`ItemMetadata`], delegating entirely to `namir_nam::probe_metadata` /
//! `namir_ir::probe_wav` (P6: exactly one hardened parser per format — this crate never parses
//! `.nam` JSON or WAV headers itself).

use std::path::Path;

use crate::entry::{IrItemMetadata, ItemKind, ItemMetadata, NamItemMetadata};

/// Which [`ItemKind`] a path's extension names, or `None` for anything else — the filter a
/// directory walk applies before this module's [`probe`] is ever called.
pub fn kind_from_extension(path: &Path) -> Option<ItemKind> {
    let ext = path.extension()?.to_str()?;
    if ext.eq_ignore_ascii_case("nam") {
        Some(ItemKind::Nam)
    } else if ext.eq_ignore_ascii_case("wav") {
        Some(ItemKind::Ir)
    } else {
        None
    }
}

/// Extracts `ItemMetadata` for `bytes`, known to be `kind`. A probe failure (malformed content
/// masquerading under the right extension) degrades to [`ItemMetadata::None`] rather than
/// failing the whole scan (P8) — the entry is still indexed, just with nothing extracted.
pub fn probe(bytes: &[u8], kind: ItemKind) -> ItemMetadata {
    match kind {
        ItemKind::Nam => match namir_nam::probe_metadata(bytes) {
            Ok(p) => ItemMetadata::Nam(NamItemMetadata {
                architecture: p.architecture,
                sample_rate: p.sample_rate,
                name: p.metadata.name,
                modeled_by: p.metadata.modeled_by,
                gear_type: p.metadata.gear_type,
                tone_type: p.metadata.tone_type,
                description: p.metadata.description,
            }),
            Err(_) => ItemMetadata::None,
        },
        ItemKind::Ir => match namir_ir::probe_wav(bytes) {
            Ok(info) => ItemMetadata::Ir(IrItemMetadata {
                sample_rate: info.sample_rate,
                channels: info.channels,
                bits_per_sample: info.bits_per_sample,
                declared_frames: info.declared_frames,
            }),
            Err(_) => ItemMetadata::None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn kind_from_extension_recognises_nam_case_insensitively() {
        assert_eq!(
            kind_from_extension(&PathBuf::from("x.nam")),
            Some(ItemKind::Nam)
        );
        assert_eq!(
            kind_from_extension(&PathBuf::from("x.NAM")),
            Some(ItemKind::Nam)
        );
    }

    #[test]
    fn kind_from_extension_recognises_wav_case_insensitively() {
        assert_eq!(
            kind_from_extension(&PathBuf::from("x.wav")),
            Some(ItemKind::Ir)
        );
        assert_eq!(
            kind_from_extension(&PathBuf::from("x.WAV")),
            Some(ItemKind::Ir)
        );
    }

    #[test]
    fn kind_from_extension_rejects_other_extensions() {
        assert_eq!(kind_from_extension(&PathBuf::from("x.txt")), None);
        assert_eq!(kind_from_extension(&PathBuf::from("x")), None);
    }

    #[test]
    fn probe_degrades_to_none_on_malformed_content() {
        assert_eq!(probe(b"not a nam file", ItemKind::Nam), ItemMetadata::None);
        assert_eq!(probe(b"not a wav file", ItemKind::Ir), ItemMetadata::None);
    }

    #[test]
    fn probe_extracts_real_nam_metadata() {
        let model =
            namir_fixtures::nam::generate(namir_fixtures::nam::WaveNetShape::Nano, 1).unwrap();
        let bytes = model.to_json_bytes();
        match probe(&bytes, ItemKind::Nam) {
            ItemMetadata::Nam(m) => assert_eq!(m.architecture, "WaveNet"),
            other => panic!("expected Nam metadata, got {other:?}"),
        }
    }

    #[test]
    fn probe_extracts_real_ir_metadata() {
        let samples = namir_fixtures::ir::decaying_noise(256, 1, 50.0);
        let bytes = namir_fixtures::ir::to_mono_wav_bytes(&samples, 44_100);
        match probe(&bytes, ItemKind::Ir) {
            ItemMetadata::Ir(m) => {
                assert_eq!(m.sample_rate, 44_100);
                assert_eq!(m.channels, 1);
            }
            other => panic!("expected Ir metadata, got {other:?}"),
        }
    }
}
