//! FR-LIB-050 (Should): "mark items as favourites and filter by that mark. Favourites shall
//! persist independently of file location, keyed by content hash." Keying by hash rather than
//! path is the whole requirement — a favourite must survive the marked file moving or being
//! re-scanned under a new name, which a path-keyed mark could not do.

use std::collections::HashSet;

use namir_core::ContentHash;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The set of favourited content hashes. Serialises as a plain array of hex strings — the same
/// hex form `namir-state` uses for a `FileRef`'s hash, so a favourites list is as inspectable as
/// any other part of this project's on-disk formats (FR-STATE-040's diffability spirit, applied
/// here even though this isn't `namir-state`'s own format).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Favourites(HashSet<ContentHash>);

impl Favourites {
    /// An empty set of favourites.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks `hash` as a favourite. Idempotent.
    pub fn mark(&mut self, hash: ContentHash) {
        self.0.insert(hash);
    }

    /// Removes `hash`'s favourite mark, if any. Idempotent.
    pub fn unmark(&mut self, hash: ContentHash) {
        self.0.remove(&hash);
    }

    /// Whether `hash` is marked as a favourite.
    pub fn is_favourite(&self, hash: ContentHash) -> bool {
        self.0.contains(&hash)
    }

    /// How many hashes are marked as favourites.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no hashes are marked as favourites.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every favourited hash.
    pub fn iter(&self) -> impl Iterator<Item = ContentHash> + '_ {
        self.0.iter().copied()
    }
}

impl Serialize for Favourites {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let hex: Vec<String> = self.0.iter().map(ContentHash::to_string).collect();
        hex.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Favourites {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // D-11.2's tolerant-reading spirit: a malformed individual hash string is skipped, not a
        // reason to fail loading every other favourite alongside it.
        let hex: Vec<String> = Vec::deserialize(deserializer)?;
        let hashes = hex.into_iter().filter_map(|s| s.parse().ok()).collect();
        Ok(Favourites(hashes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_then_is_favourite() {
        let mut f = Favourites::new();
        let hash = ContentHash::of(b"a model");
        assert!(!f.is_favourite(hash));
        f.mark(hash);
        assert!(f.is_favourite(hash));
    }

    #[test]
    fn unmark_removes_the_mark() {
        let mut f = Favourites::new();
        let hash = ContentHash::of(b"a model");
        f.mark(hash);
        f.unmark(hash);
        assert!(!f.is_favourite(hash));
    }

    #[test]
    fn marking_twice_is_idempotent() {
        let mut f = Favourites::new();
        let hash = ContentHash::of(b"a model");
        f.mark(hash);
        f.mark(hash);
        assert_eq!(f.len(), 1);
    }

    /// The requirement's literal wording: the mark survives file movement, because it was never
    /// tied to a path in the first place.
    #[test]
    fn round_trips_through_json_independent_of_any_path() {
        let mut f = Favourites::new();
        f.mark(ContentHash::of(b"a"));
        f.mark(ContentHash::of(b"b"));
        let json = serde_json::to_string(&f).unwrap();
        let restored: Favourites = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, f);
    }

    #[test]
    fn a_malformed_hash_string_is_skipped_not_fatal() {
        let json = r#"["not-a-valid-hash", ""#.to_string()
            + &ContentHash::of(b"valid").to_string()
            + r#""]"#;
        let restored: Favourites = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.len(), 1);
        assert!(restored.is_favourite(ContentHash::of(b"valid")));
    }
}
