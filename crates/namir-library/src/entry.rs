//! D-12.1: the library index is an on-disk table of `(path, size, mtime, content hash, extracted
//! metadata)`. This module is the record shape; `store.rs` owns persistence, `scan.rs` owns
//! populating it, `probe.rs` owns extracting `ItemMetadata` from bytes.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use namir_core::ContentHash;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Which kind of file a [`LibraryEntry`] indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// A `.nam` model file.
    Nam,
    /// A `.wav` impulse response file.
    Ir,
}

/// D-12.4 (for RD-1): where a library entry came from. `Local` in 1.0; the `Unknown` catch-all is
/// D-12.4's "adds a variant rather than a schema migration" read from the *reading* side — a 1.0
/// build reading an index a later build wrote (once a remote origin exists) keeps the record's
/// origin string rather than dropping the record or failing to parse it, at the cost of not being
/// able to interpret what that origin means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Found by scanning a configured library root.
    Local,
    /// An origin string this build doesn't recognise, preserved verbatim.
    Unknown(String),
}

impl Serialize for Origin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = match self {
            Origin::Local => "local",
            Origin::Unknown(s) => s.as_str(),
        };
        serializer.serialize_str(s)
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "local" => Origin::Local,
            _ => Origin::Unknown(s),
        })
    }
}

/// Whole nanoseconds since the Unix epoch, signed so a pre-1970 mtime is representable. Only ever
/// compared against a value this same machine recorded earlier (D-12.1's own size+mtime
/// incremental-scan rule), so cross-platform mtime-granularity differences (Windows' 100 ns
/// `FILETIME` ticks vs. a coarser filesystem elsewhere) are not a correctness question — only
/// "did this exact value change since last time" is asked of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileTime(i128);

impl FileTime {
    /// Converts a `SystemTime` (as `std::fs::Metadata::modified()` returns).
    pub fn from_system_time(time: SystemTime) -> Self {
        match time.duration_since(UNIX_EPOCH) {
            Ok(since_epoch) => FileTime(since_epoch.as_nanos() as i128),
            Err(before_epoch) => FileTime(-(before_epoch.duration().as_nanos() as i128)),
        }
    }

    /// The current time.
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    /// The raw signed-nanoseconds-since-epoch value.
    pub fn as_nanos_since_epoch(self) -> i128 {
        self.0
    }
}

/// FR-NAM-080's display metadata, plus the two fields a library search benefits from that a
/// display-only view doesn't need (`architecture`, `sample_rate`) — extracted via
/// `namir_nam::probe_metadata`, never a full parse (see `probe.rs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamItemMetadata {
    /// The declared architecture (e.g. `"WaveNet"`, `"LSTM"`), unvalidated.
    pub architecture: String,
    /// The declared model sample rate, when present.
    pub sample_rate: Option<u32>,
    /// FR-NAM-080's display name.
    pub name: String,
    /// FR-NAM-080's author/creator credit.
    pub modeled_by: String,
    /// FR-NAM-080's modeled gear make/model/type.
    pub gear_type: String,
    /// FR-NAM-080's modeled gear tone type.
    pub tone_type: String,
    /// FR-NAM-080's free-text description.
    pub description: String,
}

/// A WAV IR's header fields, via `namir_ir::probe_wav` — never decoded, resampled, or scheduled
/// (see `probe.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IrItemMetadata {
    /// The file's own sample rate, before any resampling a load would apply.
    pub sample_rate: u32,
    /// 1 (mono) or 2 (stereo).
    pub channels: u16,
    /// 16, 24 or 32.
    pub bits_per_sample: u16,
    /// The file's own **declared** frame count — untrusted, exactly as
    /// `namir_ir::WavInfo::declared_frames` documents; never used to size an allocation here
    /// either.
    pub declared_frames: u64,
}

/// What a library index knows about one entry's content, beyond `path`/`size`/`mtime`/`hash`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemMetadata {
    /// A `.nam` model's extracted metadata.
    Nam(NamItemMetadata),
    /// A WAV IR's extracted header fields.
    Ir(IrItemMetadata),
    /// The file was too large to probe (over [`crate::MAX_INDEXED_FILE_BYTES`]) or failed its
    /// crate's own probe — still indexed and browsable by path, just with nothing extracted.
    None,
}

/// One row of D-12.1's on-disk table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryEntry {
    /// The file's path.
    pub path: PathBuf,
    /// Which kind of file this is.
    pub kind: ItemKind,
    /// The file's byte length, as last seen.
    pub size: u64,
    /// The file's last-modified time, as last seen.
    pub mtime: FileTime,
    /// `None` when the file exceeded [`crate::MAX_INDEXED_FILE_BYTES`] or could not be read —
    /// such an entry is still indexed (it exists, it is browsable) but invisible to a hash search
    /// (D-11.3's third resolution step), which is documented behaviour, not a silent gap.
    #[serde(with = "crate::hash_hex")]
    pub hash: Option<ContentHash>,
    /// Extracted metadata, or [`ItemMetadata::None`] if none was extracted.
    pub metadata: ItemMetadata,
    /// Where this entry came from (D-12.4).
    pub origin: Origin,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_local_round_trips() {
        let json = serde_json::to_string(&Origin::Local).unwrap();
        assert_eq!(json, "\"local\"");
        let restored: Origin = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Origin::Local);
    }

    /// D-12.4's forward-compatibility: an origin string this build has never heard of is
    /// preserved, not rejected.
    #[test]
    fn origin_unrecognised_string_round_trips_as_unknown() {
        let restored: Origin = serde_json::from_str("\"tone3000\"").unwrap();
        assert_eq!(restored, Origin::Unknown("tone3000".to_string()));
        let json = serde_json::to_string(&restored).unwrap();
        assert_eq!(json, "\"tone3000\"");
    }

    #[test]
    fn file_time_orders_by_nanoseconds() {
        let a = FileTime::from_system_time(UNIX_EPOCH);
        let b = FileTime::from_system_time(UNIX_EPOCH + std::time::Duration::from_secs(1));
        assert!(a < b);
    }

    #[test]
    fn file_time_before_epoch_is_negative() {
        let before = FileTime::from_system_time(UNIX_EPOCH - std::time::Duration::from_secs(1));
        assert!(before.as_nanos_since_epoch() < 0);
    }

    #[test]
    fn library_entry_round_trips_through_json() {
        let entry = LibraryEntry {
            path: PathBuf::from("marshall/plexi.nam"),
            kind: ItemKind::Nam,
            size: 1234,
            mtime: FileTime::now(),
            hash: Some(ContentHash::of(b"entry test")),
            metadata: ItemMetadata::Nam(NamItemMetadata {
                architecture: "WaveNet".to_string(),
                sample_rate: Some(48_000),
                name: "Plexi".to_string(),
                modeled_by: String::new(),
                gear_type: String::new(),
                tone_type: String::new(),
                description: String::new(),
            }),
            origin: Origin::Local,
        };
        let json = serde_json::to_string_pretty(&entry).unwrap();
        let restored: LibraryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, entry);
    }

    #[test]
    fn library_entry_with_no_hash_round_trips() {
        let entry = LibraryEntry {
            path: PathBuf::from("huge.wav"),
            kind: ItemKind::Ir,
            size: 999_999_999,
            mtime: FileTime::now(),
            hash: None,
            metadata: ItemMetadata::None,
            origin: Origin::Local,
        };
        let restored: LibraryEntry =
            serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        assert_eq!(restored, entry);
    }
}
