//! FR-STATE-070's three-step resolution order — "try the library-relative path, then the
//! absolute path, then a content-hash search of the library" — as data ([`candidates`]) and as a
//! driven algorithm ([`resolve`]) against an injected port ([`FileResolver`]).
//!
//! # Why the order lives here and the filesystem access doesn't
//!
//! D-5.1 puts the dependency edge as `namir-library → namir-state`, the opposite direction a
//! naive reading of D-11.3's consequence note ("the library index must maintain a hash → path
//! map") might suggest is needed. Resolved by splitting the algorithm from the data it needs:
//! this crate owns the **order**, expressed twice — [`candidates`] as an iterator (the order,
//! literally, as data) and [`resolve`] as the algorithm driven through a port — and defines the
//! [`FileResolver`] trait `namir-library` implements. The trait runs *against* D-5.1's edge
//! direction, so the dependency does not have to.
//!
//! `namir-worker` (M5's `recall.rs`) is the crate that can see both `namir-state` and
//! `namir-library`, and composes them. It calls [`candidates`] directly rather than [`resolve`]
//! for its own production path, because verifying a path candidate's content against the
//! recorded hash requires reading the file's bytes — something `FileResolver` deliberately
//! cannot do (it only answers "does something exist here?"), because whoever reads and hashes
//! those bytes is going to do so anyway once loading the resource (`ResourceCache::get_or_load_*`
//! in `namir-worker`), so doing it a second time here would be wasted work. [`resolve`] is the
//! simpler, complete-in-itself algorithm this crate can prove correct on its own — existence-only,
//! no content verification — useful to a caller that only needs "does this reference point at
//! something", and the vehicle for this crate's own tests of FR-STATE-070's four outcomes.

use std::path::PathBuf;

use namir_core::ContentHash;

use crate::error::StateWarning;
use crate::error_codes;
use crate::reference::{FileRef, RelPath};

/// One of a [`FileRef`]'s resolution candidates, in FR-STATE-070's order. Borrows from the
/// `FileRef` it came from rather than cloning, since a resolver only ever needs to read from it.
#[derive(Debug, Clone, Copy)]
pub enum Candidate<'a> {
    /// Step 1: the library-relative path, tried against every configured library root.
    LibraryRelative(&'a RelPath),
    /// Step 2: the recorded absolute path, tried verbatim.
    Absolute(&'a str),
    /// Step 3: a content-hash search of the library index. Always present — [`FileRef::hash`] is
    /// never optional, so this candidate always exists, even when the other two don't.
    ContentHash(ContentHash),
}

/// Yields `reference`'s candidates in FR-STATE-070's order: library-relative (if present),
/// absolute (if present), then content hash (always). This is the *only* place that order is
/// written down — [`resolve`] and any other driver (`namir-worker`'s `recall::locate`) both walk
/// this iterator rather than each re-encoding the sequence.
pub fn candidates(reference: &FileRef) -> impl Iterator<Item = Candidate<'_>> {
    reference
        .library_relative
        .iter()
        .map(Candidate::LibraryRelative)
        .chain(
            reference
                .absolute
                .iter()
                .map(|s| Candidate::Absolute(s.as_str())),
        )
        .chain(std::iter::once(Candidate::ContentHash(reference.hash)))
}

/// The injected port. **Defined in `namir-state`, implemented in `namir-library`.** Each method
/// answers only "does something exist here", never "and does its content match" — see this
/// module's doc comment for why content verification is deliberately not this trait's job.
pub trait FileResolver {
    /// Tries `rel` under whichever configured library root(s) the implementation knows about, in
    /// its own configured order, and returns the first that exists. A `FileRef` stores no root
    /// identity (see [`FileRef`]'s own doc comment), so the implementation's root list — not
    /// this trait — is what "a configured library root" (FR-STATE-070's own wording) resolves to.
    fn resolve_library_relative(&self, rel: &RelPath) -> Option<PathBuf>;
    /// Tries the recorded absolute path exactly as written, no normalisation, no fixups. A path
    /// authored on a different platform is expected to fail here (correctly) rather than be
    /// coerced into something that happens to open.
    fn resolve_absolute(&self, absolute: &str) -> Option<PathBuf>;
    /// D-11.3's consequence note made an API: the library index's hash → path map.
    fn resolve_by_hash(&self, hash: ContentHash) -> Option<PathBuf>;
}

/// Which of FR-STATE-070's three steps produced a [`ResolvedFile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedVia {
    /// Step 1: found under a configured library root.
    LibraryRelative,
    /// Step 2: found at the recorded absolute path.
    Absolute,
    /// Step 3: found via a content-hash search of the library.
    ContentHash,
}

/// A located file, from [`resolve`]'s existence-only check — **not** verified against
/// `expected`; a caller that needs that guarantee (`namir-worker`'s `recall::locate`) reads the
/// bytes and compares itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    /// Where the resolver found something.
    pub path: PathBuf,
    /// The hash this file is expected — not yet confirmed — to have.
    pub expected: ContentHash,
    /// Which of the three steps found it.
    pub via: ResolvedVia,
}

/// FR-STATE-070's failure payload: "the user shall be shown the missing file's name and hash,
/// with an option to locate it manually." This carries the first two; the third is a UI
/// affordance M6 builds on top of this data, not something this crate can offer on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingFile {
    /// The name to show the user.
    pub display_name: String,
    /// The hash to show the user.
    pub hash: ContentHash,
}

impl MissingFile {
    /// The catalogued [`StateWarning`] a caller can surface for this outcome.
    pub fn warning(&self) -> StateWarning {
        StateWarning::new(
            error_codes::REFERENCE_NOT_FOUND,
            format!("{} (hash {})", self.display_name, self.hash),
        )
    }
}

/// The outcome of [`resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A candidate existed. Not yet content-verified — see [`ResolvedFile`]'s doc comment.
    Resolved(ResolvedFile),
    /// None of the three candidates existed.
    Missing(MissingFile),
}

/// Drives [`candidates`] through `resolver` in FR-STATE-070's order and returns the first hit —
/// or [`Resolution::Missing`] if none of the three candidates resolved to anything. Existence-only
/// (see this module's doc comment); a caller needing content verification composes this crate's
/// [`candidates`] with its own byte-reading instead.
pub fn resolve(reference: &FileRef, resolver: &dyn FileResolver) -> Resolution {
    for candidate in candidates(reference) {
        let (found, via) = match candidate {
            Candidate::LibraryRelative(rel) => (
                resolver.resolve_library_relative(rel),
                ResolvedVia::LibraryRelative,
            ),
            Candidate::Absolute(abs) => (resolver.resolve_absolute(abs), ResolvedVia::Absolute),
            Candidate::ContentHash(hash) => {
                (resolver.resolve_by_hash(hash), ResolvedVia::ContentHash)
            }
        };
        if let Some(path) = found {
            return Resolution::Resolved(ResolvedFile {
                path,
                expected: reference.hash,
                via,
            });
        }
    }
    Resolution::Missing(MissingFile {
        display_name: reference.display_name.clone(),
        hash: reference.hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// An in-memory fake — the whole reason `FileResolver` is a trait rather than a concrete
    /// filesystem type: FR-STATE-070's four outcomes are each exercisable without touching disk.
    #[derive(Default)]
    struct FakeResolver {
        by_relative: HashMap<String, PathBuf>,
        by_absolute: HashMap<String, PathBuf>,
        by_hash: HashMap<ContentHash, PathBuf>,
    }

    impl FileResolver for FakeResolver {
        fn resolve_library_relative(&self, rel: &RelPath) -> Option<PathBuf> {
            self.by_relative.get(rel.as_str()).cloned()
        }
        fn resolve_absolute(&self, absolute: &str) -> Option<PathBuf> {
            self.by_absolute.get(absolute).cloned()
        }
        fn resolve_by_hash(&self, hash: ContentHash) -> Option<PathBuf> {
            self.by_hash.get(&hash).cloned()
        }
    }

    fn reference_with(library_relative: Option<&str>, absolute: Option<&str>) -> FileRef {
        FileRef {
            hash: ContentHash::of(b"the reference under test"),
            library_relative: library_relative.map(|s| RelPath::parse(s).unwrap()),
            absolute: absolute.map(str::to_string),
            display_name: "plexi.nam".to_string(),
            embedded: None,
        }
    }

    #[test]
    fn candidates_yields_all_three_steps_in_order_when_all_are_present() {
        let reference = reference_with(Some("marshall/plexi.nam"), Some("/abs/plexi.nam"));
        let steps: Vec<&str> = candidates(&reference)
            .map(|c| match c {
                Candidate::LibraryRelative(_) => "library_relative",
                Candidate::Absolute(_) => "absolute",
                Candidate::ContentHash(_) => "content_hash",
            })
            .collect();
        assert_eq!(steps, ["library_relative", "absolute", "content_hash"]);
    }

    #[test]
    fn candidates_skips_absent_steps_but_always_ends_with_content_hash() {
        let reference = reference_with(None, None);
        let steps: Vec<&str> = candidates(&reference)
            .map(|c| match c {
                Candidate::LibraryRelative(_) => "library_relative",
                Candidate::Absolute(_) => "absolute",
                Candidate::ContentHash(_) => "content_hash",
            })
            .collect();
        assert_eq!(steps, ["content_hash"]);
    }

    /// FR-STATE-070's *Verify*: "each resolution path... exercised individually." Path 1.
    // trace: FR-STATE-070
    #[test]
    fn resolves_via_library_relative_when_that_candidate_exists() {
        let reference = reference_with(Some("marshall/plexi.nam"), None);
        let mut resolver = FakeResolver::default();
        resolver.by_relative.insert(
            "marshall/plexi.nam".to_string(),
            PathBuf::from("/library/marshall/plexi.nam"),
        );

        match resolve(&reference, &resolver) {
            Resolution::Resolved(found) => {
                assert_eq!(found.via, ResolvedVia::LibraryRelative);
                assert_eq!(found.path, PathBuf::from("/library/marshall/plexi.nam"));
                assert_eq!(found.expected, reference.hash);
            }
            Resolution::Missing(m) => panic!("expected a resolution, got Missing: {m:?}"),
        }
    }

    /// Path 2.
    // trace: FR-STATE-070
    #[test]
    fn resolves_via_absolute_when_library_relative_is_absent() {
        let reference = reference_with(None, Some("/abs/plexi.nam"));
        let mut resolver = FakeResolver::default();
        resolver.by_absolute.insert(
            "/abs/plexi.nam".to_string(),
            PathBuf::from("/abs/plexi.nam"),
        );

        match resolve(&reference, &resolver) {
            Resolution::Resolved(found) => assert_eq!(found.via, ResolvedVia::Absolute),
            Resolution::Missing(m) => panic!("expected a resolution, got Missing: {m:?}"),
        }
    }

    /// Path 2, the case that actually matters: library-relative is *present* but doesn't
    /// resolve, so the algorithm falls through to absolute rather than stopping.
    #[test]
    fn falls_through_to_absolute_when_library_relative_does_not_resolve() {
        let reference = reference_with(Some("marshall/plexi.nam"), Some("/abs/plexi.nam"));
        let mut resolver = FakeResolver::default();
        // Deliberately no entry in by_relative -- the library-relative candidate misses.
        resolver.by_absolute.insert(
            "/abs/plexi.nam".to_string(),
            PathBuf::from("/abs/plexi.nam"),
        );

        match resolve(&reference, &resolver) {
            Resolution::Resolved(found) => assert_eq!(found.via, ResolvedVia::Absolute),
            Resolution::Missing(m) => panic!("expected a fall-through resolution, got: {m:?}"),
        }
    }

    /// Path 3.
    // trace: FR-STATE-070
    #[test]
    fn resolves_via_content_hash_when_the_other_two_are_absent_or_miss() {
        let reference = reference_with(None, None);
        let mut resolver = FakeResolver::default();
        resolver
            .by_hash
            .insert(reference.hash, PathBuf::from("/library/found-by-hash.nam"));

        match resolve(&reference, &resolver) {
            Resolution::Resolved(found) => {
                assert_eq!(found.via, ResolvedVia::ContentHash);
                assert_eq!(found.path, PathBuf::from("/library/found-by-hash.nam"));
            }
            Resolution::Missing(m) => panic!("expected a resolution, got Missing: {m:?}"),
        }
    }

    /// The fourth outcome: none of the three candidates resolve.
    // trace: FR-STATE-070
    #[test]
    fn all_three_candidates_failing_yields_missing_with_name_and_hash() {
        let reference = reference_with(Some("marshall/plexi.nam"), Some("/abs/plexi.nam"));
        let resolver = FakeResolver::default(); // nothing registered at all

        match resolve(&reference, &resolver) {
            Resolution::Resolved(found) => panic!("expected Missing, got a resolution: {found:?}"),
            Resolution::Missing(missing) => {
                assert_eq!(missing.display_name, "plexi.nam");
                assert_eq!(missing.hash, reference.hash);
            }
        }
    }

    #[test]
    fn missing_file_produces_the_catalogued_warning() {
        let missing = MissingFile {
            display_name: "plexi.nam".to_string(),
            hash: ContentHash::of(b"x"),
        };
        let warning = missing.warning();
        assert_eq!(warning.code.id, error_codes::REFERENCE_NOT_FOUND.id);
        assert!(warning.detail.contains("plexi.nam"));
    }
}
