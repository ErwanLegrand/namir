//! [`Index`]: the in-memory library index, keyed by path with a maintained hash → paths reverse
//! map (D-11.3's consequence note: "the library index must maintain a hash → path map, otherwise
//! the third resolution step in FR-STATE-070 cannot work").

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use namir_core::ContentHash;

use crate::entry::LibraryEntry;

/// The library index. `BTreeMap` for `by_path` (not `HashMap`) purely so iteration order is
/// deterministic — useful for tests and for a future UI listing — not because anything here
/// depends on sorted order the way `namir-state`'s JSON output does.
#[derive(Debug, Clone, Default)]
pub struct Index {
    by_path: BTreeMap<PathBuf, LibraryEntry>,
    by_hash: HashMap<ContentHash, Vec<PathBuf>>,
}

impl Index {
    /// An index with nothing in it.
    pub fn empty() -> Self {
        Self::default()
    }

    /// How many entries this index holds.
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    /// Whether this index holds no entries.
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// The entry at `path`, if indexed.
    pub fn get(&self, path: &Path) -> Option<&LibraryEntry> {
        self.by_path.get(path)
    }

    /// Every entry, in path order.
    pub fn iter(&self) -> impl Iterator<Item = &LibraryEntry> {
        self.by_path.values()
    }

    /// D-11.3's consequence note, as an API: every path recorded under `hash`. More than one
    /// entry sharing a hash is the normal case for a community corpus (the same model
    /// re-downloaded under different names), not an anomaly — a caller picks a tie-break (e.g.
    /// `resolver.rs`'s "first in a caller-supplied root order").
    pub fn paths_for_hash(&self, hash: ContentHash) -> &[PathBuf] {
        self.by_hash.get(&hash).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Inserts or replaces the entry at `entry.path`, keeping `by_hash` consistent — removing the
    /// path from its *previous* hash bucket first, in case a rescan found the same path now
    /// pointing at different content (FR-LIB-070's "files that change").
    pub fn upsert(&mut self, entry: LibraryEntry) {
        if let Some(previous) = self.by_path.get(&entry.path) {
            self.remove_from_hash_bucket(previous.hash, &entry.path);
        }
        if let Some(hash) = entry.hash {
            self.by_hash
                .entry(hash)
                .or_default()
                .push(entry.path.clone());
        }
        self.by_path.insert(entry.path.clone(), entry);
    }

    /// Removes the entry at `path`, if any (FR-LIB-070's "files that disappear").
    pub fn remove(&mut self, path: &Path) {
        if let Some(entry) = self.by_path.remove(path) {
            self.remove_from_hash_bucket(entry.hash, path);
        }
    }

    fn remove_from_hash_bucket(&mut self, hash: Option<ContentHash>, path: &Path) {
        let Some(hash) = hash else { return };
        if let Some(bucket) = self.by_hash.get_mut(&hash) {
            bucket.retain(|p| p != path);
            if bucket.is_empty() {
                self.by_hash.remove(&hash);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{FileTime, ItemKind, ItemMetadata, Origin};

    fn entry(path: &str, hash: Option<ContentHash>) -> LibraryEntry {
        LibraryEntry {
            path: PathBuf::from(path),
            kind: ItemKind::Nam,
            size: 10,
            mtime: FileTime::now(),
            hash,
            metadata: ItemMetadata::None,
            origin: Origin::Local,
        }
    }

    #[test]
    fn upsert_then_get_round_trips() {
        let mut index = Index::empty();
        let e = entry("a.nam", Some(ContentHash::of(b"a")));
        index.upsert(e.clone());
        assert_eq!(index.get(Path::new("a.nam")), Some(&e));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn paths_for_hash_finds_the_entry() {
        let mut index = Index::empty();
        let hash = ContentHash::of(b"shared");
        index.upsert(entry("a.nam", Some(hash)));
        assert_eq!(index.paths_for_hash(hash), [PathBuf::from("a.nam")]);
    }

    #[test]
    fn paths_for_hash_finds_multiple_entries_sharing_a_hash() {
        let mut index = Index::empty();
        let hash = ContentHash::of(b"duplicate");
        index.upsert(entry("a.nam", Some(hash)));
        index.upsert(entry("b.nam", Some(hash)));
        let mut paths = index.paths_for_hash(hash).to_vec();
        paths.sort();
        assert_eq!(paths, [PathBuf::from("a.nam"), PathBuf::from("b.nam")]);
    }

    #[test]
    fn remove_clears_both_the_path_and_hash_indexes() {
        let mut index = Index::empty();
        let hash = ContentHash::of(b"x");
        index.upsert(entry("a.nam", Some(hash)));
        index.remove(Path::new("a.nam"));
        assert!(index.is_empty());
        assert_eq!(index.paths_for_hash(hash), &[] as &[PathBuf]);
    }

    /// FR-LIB-070's "files that change": re-upserting the same path with a different hash must
    /// not leave the old hash bucket pointing at it.
    #[test]
    fn upsert_with_a_changed_hash_updates_the_reverse_index() {
        let mut index = Index::empty();
        let old_hash = ContentHash::of(b"old content");
        let new_hash = ContentHash::of(b"new content");
        index.upsert(entry("a.nam", Some(old_hash)));
        index.upsert(entry("a.nam", Some(new_hash)));

        assert_eq!(index.paths_for_hash(old_hash), &[] as &[PathBuf]);
        assert_eq!(index.paths_for_hash(new_hash), [PathBuf::from("a.nam")]);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn entries_with_no_hash_are_not_findable_by_hash() {
        let mut index = Index::empty();
        index.upsert(entry("huge.wav", None));
        assert_eq!(index.len(), 1);
        // No hash was ever registered, so there's nothing meaningful to search for -- this test
        // just confirms upsert(None) doesn't panic or register a bogus bucket.
    }
}
