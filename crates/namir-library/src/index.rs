//! [`Index`]: the in-memory library index, keyed by path with a maintained hash → paths reverse
//! map (D-11.3's consequence note: "the library index must maintain a hash → path map, otherwise
//! the third resolution step in FR-STATE-070 cannot work").

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use namir_core::ContentHash;

use crate::entry::{FileTime, LibraryEntry};
use crate::favourites::Favourites;

/// The library index. `BTreeMap` for `by_path` (not `HashMap`) purely so iteration order is
/// deterministic — useful for tests and for a future UI listing — not because anything here
/// depends on sorted order the way `namir-state`'s JSON output does.
#[derive(Debug, Clone, Default)]
pub struct Index {
    by_path: BTreeMap<PathBuf, Indexed>,
    by_hash: HashMap<ContentHash, Vec<PathBuf>>,
    /// When the most recent *complete* scan **began**, if any. `scan.rs`'s `Scanner` consults this
    /// to close D-12.1's mtime-settling gap — see that module's doc comment on why a file whose
    /// mtime lands close to this timestamp is rehashed regardless of whether it matches the
    /// stored `(size, mtime)`. Persisted by `store.rs` alongside the entries, so the protection
    /// survives a restart rather than resetting to "no prior scan" every time the process starts.
    ///
    /// The scan's *start*, not its completion, since issue #67: the window has to cover every
    /// file's examination time, and on any scan longer than the window itself those are nowhere
    /// near the moment it finished.
    last_scan_started_at: Option<FileTime>,
    /// FR-LIB-050's favourite marks, keyed by content hash — kept alongside the entries here
    /// since AQ-3's single-document store already exists.
    ///
    /// This field used to add that a separate file "would just be a second thing that can go
    /// missing independently". Issue #68 is that the trade-off runs the other way: co-location
    /// made them go missing *together*, and the index's documented corruption policy is to
    /// discard everything and rescan — which rebuilds the entries and permanently destroys marks
    /// no scan can reconstruct. `store.rs` therefore mirrors them to a sidecar document and
    /// recovers them from it when the index cannot be read; this stays their in-memory home.
    favourites: Favourites,
}

/// One indexed entry plus the lowercase text [`crate::search`] matches against.
///
/// Issue #72: `Cargo.toml`'s rationale for taking no search-index dependency is "a linear scan
/// over a precomputed lowercase blob", and it was not precomputed — `filter` allocated a `String`
/// and ran a full Unicode case-fold per entry per call, i.e. ten thousand allocations on every
/// keystroke. Folding once here, where an entry enters the index, is what makes that sentence
/// true; the cost moves to `upsert`, which happens once per changed file per scan.
#[derive(Debug, Clone)]
struct Indexed {
    entry: LibraryEntry,
    folded: String,
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
        self.by_path.get(path).map(|i| &i.entry)
    }

    /// Every entry, in path order.
    pub fn iter(&self) -> impl Iterator<Item = &LibraryEntry> {
        self.by_path.values().map(|i| &i.entry)
    }

    /// Every entry, in path order, paired with the precomputed lowercase text
    /// [`crate::search::filter`] matches a query against (issue #72). `pub(crate)`: the folded
    /// form is this crate's own search mechanism, not part of an entry's identity.
    pub(crate) fn iter_searchable(&self) -> impl Iterator<Item = (&LibraryEntry, &str)> {
        self.by_path.values().map(|i| (&i.entry, i.folded.as_str()))
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
            self.remove_from_hash_bucket(previous.entry.hash, &entry.path);
        }
        if let Some(hash) = entry.hash {
            self.by_hash
                .entry(hash)
                .or_default()
                .push(entry.path.clone());
        }
        // Folded here, once, rather than per search call -- issue #72. Recomputed on every
        // upsert, so a rescan that changes an entry's metadata can never leave a stale blob
        // behind, which is the one hazard a cache of derived text has.
        let folded = crate::search::searchable_text(&entry);
        self.by_path
            .insert(entry.path.clone(), Indexed { entry, folded });
    }

    /// Removes the entry at `path`, if any (FR-LIB-070's "files that disappear").
    pub fn remove(&mut self, path: &Path) {
        if let Some(indexed) = self.by_path.remove(path) {
            self.remove_from_hash_bucket(indexed.entry.hash, path);
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

    /// When the most recent complete scan began, if any — `None` before this index has ever
    /// finished a scan.
    pub fn last_scan_started_at(&self) -> Option<FileTime> {
        self.last_scan_started_at
    }

    /// Records that the scan whose results were just applied began at `at`. `store.rs` persists
    /// this; `scan.rs`'s `Scanner` reads it back via [`Self::last_scan_started_at`] on the *next*
    /// scan.
    pub(crate) fn set_last_scan_started_at(&mut self, at: FileTime) {
        self.last_scan_started_at = Some(at);
    }

    /// FR-LIB-050's favourite marks. `&mut` access, not a wrapper method per mark/unmark, since
    /// `Favourites` is already a small, self-contained public type — adding `Index::mark_favourite`
    /// etc. would just be forwarding methods with nothing of this type's own to add.
    pub fn favourites(&self) -> &Favourites {
        &self.favourites
    }

    /// Mutable access to [`Self::favourites`] — see that method's doc comment.
    pub fn favourites_mut(&mut self) -> &mut Favourites {
        &mut self.favourites
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
    /// **Issue #72:** the lowercase text `search::filter` matches against is computed where an
    /// entry enters the index, not per entry per call — that is what `Cargo.toml`'s "a linear scan
    /// over a precomputed lowercase blob" claims, and it was not true while `filter` allocated a
    /// `String` and ran a full Unicode case-fold for every entry on every keystroke.
    ///
    /// The hazard a cache of derived text brings with it is staleness, so the re-upsert half
    /// matters as much as the first: a rescan that changes a file's metadata must not leave the
    /// old text searchable.
    #[test]
    fn the_searchable_text_is_precomputed_and_never_stale() {
        use crate::entry::NamItemMetadata;

        let named = |name: &str| LibraryEntry {
            path: PathBuf::from("cabs/Plexi.nam"),
            kind: ItemKind::Nam,
            size: 10,
            mtime: FileTime::now(),
            hash: None,
            metadata: ItemMetadata::Nam(NamItemMetadata {
                architecture: "WaveNet".to_string(),
                sample_rate: Some(48_000),
                name: name.to_string(),
                modeled_by: String::new(),
                gear_type: String::new(),
                tone_type: String::new(),
                description: String::new(),
            }),
            origin: Origin::Local,
        };

        let mut index = Index::empty();
        index.upsert(named("Before"));
        let (_, folded) = index.iter_searchable().next().unwrap();
        assert!(folded.contains("plexi"), "the file stem, folded: {folded}");
        assert!(folded.contains("before"), "the metadata, folded: {folded}");
        assert!(
            !folded.contains("Plexi"),
            "the stored text is already lowercase, so a search folds nothing per call: {folded}"
        );

        index.upsert(named("After"));
        let (_, folded) = index.iter_searchable().next().unwrap();
        assert!(folded.contains("after"));
        assert!(
            !folded.contains("before"),
            "a re-upsert must refold, or the index would answer searches from the old metadata"
        );
    }
}
