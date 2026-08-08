//! FR-LIB-040: "filter the library by free-text search over file name and metadata fields."
//! AND-over-terms substring matching against each entry's name and metadata, case-folded. No
//! fuzzy matching, no scoring, no search-index crate (`fst`, `tantivy`) — 10,000 records is a
//! sub-millisecond linear scan, and a real search library would buy nothing this workload needs.
//!
//! Also FR-LIB-060's ordered next/previous stepping — a mechanism over an already-ordered slice
//! ([`next_after`]/[`previous_before`]), not a UI gesture; M6 wires the gesture to it.

use std::path::Path;

use crate::entry::{ItemMetadata, LibraryEntry};
use crate::index::Index;

/// A parsed free-text query: whitespace-split terms, case-folded once at parse time rather than
/// per candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    terms: Vec<String>,
}

impl Query {
    /// Parses `text` into whitespace-split, lowercased terms.
    pub fn parse(text: &str) -> Query {
        Query {
            terms: text.split_whitespace().map(str::to_lowercase).collect(),
        }
    }

    /// An empty query — matches everything.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// The lowercase text one entry is searched against: its file stem, plus every metadata field
/// FR-LIB-040 names (name, author, gear/tone type, description for a model; nothing extra for an
/// IR, whose header fields aren't free text).
fn searchable_text(entry: &LibraryEntry) -> String {
    let mut blob = String::new();
    if let Some(stem) = entry.path.file_stem().and_then(|s| s.to_str()) {
        blob.push_str(stem);
    }
    if let ItemMetadata::Nam(m) = &entry.metadata {
        for field in [
            m.architecture.as_str(),
            m.name.as_str(),
            m.modeled_by.as_str(),
            m.gear_type.as_str(),
            m.tone_type.as_str(),
            m.description.as_str(),
        ] {
            blob.push(' ');
            blob.push_str(field);
        }
    }
    blob.to_lowercase()
}

/// Every entry in `index` matching `query` — an entry matches if every one of `query`'s terms is
/// a substring of its [`searchable_text`]. An empty query matches everything.
pub fn filter<'a>(index: &'a Index, query: &'a Query) -> impl Iterator<Item = &'a LibraryEntry> {
    index.iter().filter(move |entry| {
        query.is_empty() || {
            let blob = searchable_text(entry);
            query.terms.iter().all(|term| blob.contains(term.as_str()))
        }
    })
}

/// FR-LIB-060: the entry immediately after `current` in `ordered` (a caller-supplied ordering —
/// typically a [`filter`] result collected into a `Vec`), or the first entry if `current` is
/// `None`. `None` if `current` is the last entry, or isn't in `ordered` at all.
pub fn next_after<'a>(
    ordered: &[&'a LibraryEntry],
    current: Option<&Path>,
) -> Option<&'a LibraryEntry> {
    match current {
        None => ordered.first().copied(),
        Some(path) => {
            let position = ordered.iter().position(|e| e.path == path)?;
            ordered.get(position + 1).copied()
        }
    }
}

/// The mirror of [`next_after`]: the entry immediately before `current`, or the last entry if
/// `current` is `None`.
pub fn previous_before<'a>(
    ordered: &[&'a LibraryEntry],
    current: Option<&Path>,
) -> Option<&'a LibraryEntry> {
    match current {
        None => ordered.last().copied(),
        Some(path) => {
            let position = ordered.iter().position(|e| e.path == path)?;
            position
                .checked_sub(1)
                .and_then(|i| ordered.get(i))
                .copied()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{FileTime, ItemKind, NamItemMetadata, Origin};
    use std::path::PathBuf;

    fn nam_entry(path: &str, name: &str, description: &str) -> LibraryEntry {
        LibraryEntry {
            path: PathBuf::from(path),
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
                description: description.to_string(),
            }),
            origin: Origin::Local,
        }
    }

    fn ir_entry(path: &str) -> LibraryEntry {
        LibraryEntry {
            path: PathBuf::from(path),
            kind: ItemKind::Ir,
            size: 10,
            mtime: FileTime::now(),
            hash: None,
            metadata: ItemMetadata::None,
            origin: Origin::Local,
        }
    }

    fn index_with(entries: Vec<LibraryEntry>) -> Index {
        let mut index = Index::empty();
        for e in entries {
            index.upsert(e);
        }
        index
    }

    #[test]
    fn empty_query_matches_everything() {
        let index = index_with(vec![nam_entry("marshall/plexi.nam", "Plexi", "")]);
        let query = Query::parse("");
        assert_eq!(filter(&index, &query).count(), 1);
    }

    // trace: FR-LIB-040
    #[test]
    fn matches_the_file_stem_case_insensitively() {
        let index = index_with(vec![nam_entry("marshall/PLEXI.nam", "", "")]);
        assert_eq!(filter(&index, &Query::parse("plexi")).count(), 1);
        assert_eq!(filter(&index, &Query::parse("fender")).count(), 0);
    }

    // trace: FR-LIB-040
    #[test]
    fn matches_metadata_fields() {
        let index = index_with(vec![nam_entry("a.nam", "Plexi 1959", "a crunchy amp")]);
        assert_eq!(filter(&index, &Query::parse("crunchy")).count(), 1);
        assert_eq!(filter(&index, &Query::parse("1959")).count(), 1);
    }

    #[test]
    fn every_term_must_match_and_terms_are_split_on_whitespace() {
        let index = index_with(vec![nam_entry("a.nam", "Plexi 1959", "crunchy")]);
        assert_eq!(filter(&index, &Query::parse("plexi crunchy")).count(), 1);
        assert_eq!(filter(&index, &Query::parse("plexi missing")).count(), 0);
    }

    #[test]
    fn ir_entries_are_searched_by_file_stem_only() {
        let index = index_with(vec![ir_entry("cabs/1960a-sm57.wav")]);
        assert_eq!(filter(&index, &Query::parse("sm57")).count(), 1);
    }

    #[test]
    fn next_after_none_returns_the_first_entry() {
        let a = nam_entry("a.nam", "", "");
        let b = nam_entry("b.nam", "", "");
        let ordered = [&a, &b];
        assert_eq!(next_after(&ordered, None), Some(&a));
    }

    #[test]
    fn next_after_steps_forward() {
        let a = nam_entry("a.nam", "", "");
        let b = nam_entry("b.nam", "", "");
        let ordered = [&a, &b];
        assert_eq!(next_after(&ordered, Some(Path::new("a.nam"))), Some(&b));
    }

    #[test]
    fn next_after_the_last_entry_is_none() {
        let a = nam_entry("a.nam", "", "");
        let ordered = [&a];
        assert_eq!(next_after(&ordered, Some(Path::new("a.nam"))), None);
    }

    #[test]
    fn previous_before_none_returns_the_last_entry() {
        let a = nam_entry("a.nam", "", "");
        let b = nam_entry("b.nam", "", "");
        let ordered = [&a, &b];
        assert_eq!(previous_before(&ordered, None), Some(&b));
    }

    #[test]
    fn previous_before_steps_backward() {
        let a = nam_entry("a.nam", "", "");
        let b = nam_entry("b.nam", "", "");
        let ordered = [&a, &b];
        assert_eq!(
            previous_before(&ordered, Some(Path::new("b.nam"))),
            Some(&a)
        );
    }

    #[test]
    fn previous_before_the_first_entry_is_none() {
        let a = nam_entry("a.nam", "", "");
        let ordered = [&a];
        assert_eq!(previous_before(&ordered, Some(Path::new("a.nam"))), None);
    }
}
