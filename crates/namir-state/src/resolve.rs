//! §7.4 of `docs/04-state-and-preset-format.md`'s resolution order — FR-STATE-070's three
//! external steps ("try the library-relative path, then the absolute path, then a content-hash
//! search of the library") followed by FR-STATE-080's embedded copy as the terminal fallback — as
//! data ([`candidates`]) and as a driven algorithm ([`resolve`]) against an injected port
//! ([`FileResolver`]).
//!
//! # Three steps a resolver answers, and a fourth nobody has to
//!
//! [`Candidate`] enumerates the three steps that are *questions for a [`FileResolver`]*: each one
//! is a place the bytes might be, which only something with a filesystem and a library index can
//! answer. §7.4's fourth step is not one of those — an `embedded` copy needs no resolver, no I/O
//! and no root list, because the bytes are already in the reference. So it is not a `Candidate`;
//! it is what both drivers do once [`candidates`] is exhausted: [`resolve`] returns
//! [`Resolution::Embedded`], and `namir-worker`'s `recall::locate` hashes the embedded bytes and
//! uses them. Issue #113 recorded the state before this: [`candidates`] terminated at
//! `ContentHash` and so did [`resolve`], which meant this crate's public, documented-as-complete
//! algorithm reported `Missing` for a reference whose embedded copy was sitting right there —
//! three of the spec's four steps, with only `namir-worker` implementing the fourth.
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
//! no content verification, and that applies to the embedded step too: [`Resolution::Embedded`]
//! says an embedded copy is *there*, not that its bytes hash to `expected`, exactly as
//! [`Resolution::Resolved`] says a path exists without vouching for what is at it — useful to a
//! caller that only needs "does this reference point at something", and the vehicle for this
//! crate's own tests of FR-STATE-070's four outcomes.

use std::path::PathBuf;

use namir_core::ContentHash;

use crate::error::StateWarning;
use crate::error_codes;
use crate::reference::{EmbeddedRef, FileRef, RelPath};

/// One of a [`FileRef`]'s three *externally resolved* candidates, in FR-STATE-070's order — the
/// steps a [`FileResolver`] is asked about. §7.4's fourth step, `embedded`, is deliberately not a
/// variant here: it asks a resolver nothing (see this module's doc comment), and every driver
/// applies it after this iterator runs out. Borrows from the `FileRef` it came from rather than
/// cloning, since a resolver only ever needs to read from it.
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

/// Yields `reference`'s externally-resolved candidates in FR-STATE-070's order:
/// library-relative (if present), absolute (if present), then content hash (always). This is the
/// *only* place that order is written down — [`resolve`] and any other driver (`namir-worker`'s
/// `recall::locate`) both walk this iterator rather than each re-encoding the sequence.
///
/// **Does not yield §7.4's fourth step.** A caller that stops here has implemented three quarters
/// of the documented order: after this iterator is exhausted, `reference.embedded` is the final
/// fallback, and a driver that ignores it will report a shared preset's self-contained copy as
/// missing. [`resolve`] does this for you; a driver that reads bytes itself should do what
/// `namir-worker`'s `recall::locate` does and try the embedded copy last.
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
pub enum Resolution<'a> {
    /// One of [`candidates`]' three steps existed. Not yet content-verified — see
    /// [`ResolvedFile`]'s doc comment.
    Resolved(ResolvedFile),
    /// §7.4's fourth step: no external candidate existed, but the reference carries its own
    /// embedded copy of the resource — the case that whole field exists for (a preset shared
    /// with someone whose library is configured differently, or who has no library at all).
    /// Borrowed from the reference rather than cloned: an embedded model can be tens of
    /// megabytes, and a caller that only wanted to know *whether* the reference resolves should
    /// not pay for a copy of it.
    ///
    /// Existence-only like [`Self::Resolved`]: the bytes are here, but this says nothing about
    /// whether they hash to the reference's own `hash`. A caller that will actually use them
    /// verifies that itself, exactly as it must for a path candidate (`namir-worker`'s
    /// `recall::locate` does, and treats a mismatched embed as a miss).
    Embedded(&'a EmbeddedRef),
    /// Neither the three external candidates nor an embedded copy produced anything.
    Missing(MissingFile),
}

/// Drives [`candidates`] through `resolver` in FR-STATE-070's order and returns the first hit;
/// failing all three, falls back to §7.4's fourth step, the reference's own `embedded` copy, and
/// only then reports [`Resolution::Missing`]. Existence-only (see this module's doc comment); a
/// caller needing content verification composes this crate's [`candidates`] with its own
/// byte-reading instead.
pub fn resolve<'a>(reference: &'a FileRef, resolver: &dyn FileResolver) -> Resolution<'a> {
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
    // §7.4 step 4, and deliberately last: an embedded copy is what a reference falls back *to*
    // when nothing external can be found, never what it prefers. A resolvable library or absolute
    // path is what FR-STATE-070 is actually about, and preferring the embed would mean a preset
    // silently ignoring the very file the user has on disk.
    if let Some(embedded) = &reference.embedded {
        return Resolution::Embedded(embedded);
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
    // trace-partial: FR-STATE-070
    // uncovered: FR-STATE-070 — the third member of the failure list, "with an option to locate
    // uncovered: it manually", is spanned by nothing and exists nowhere in the product: UiIntent
    // uncovered: carries no locate or browse variant and neither shell offers such a path, the
    // uncovered: only mention in the tree being a doc comment paraphrasing the requirement;
    // uncovered: closes M8
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
            other => panic!("expected a resolution, got {other:?}"),
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
            other => panic!("expected a resolution, got {other:?}"),
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
            other => panic!("expected a fall-through resolution, got {other:?}"),
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
            other => panic!("expected a resolution, got {other:?}"),
        }
    }

    fn an_embedded_copy() -> EmbeddedRef {
        EmbeddedRef {
            media_type: "application/vnd.namir.nam+json".to_string(),
            data: br#"{"fake":"the whole resource, carried in the document"}"#.to_vec(),
        }
    }

    /// Issue #113 / §7.4 step 4: a reference whose only usable payload is its embedded copy
    /// resolves through it rather than being reported missing. Before this, `resolve` walked
    /// `candidates` and stopped — three of the format spec's four steps — so the one case
    /// `embedded` exists for (a preset opened by someone with a different library, or none) was
    /// the one case this crate's own algorithm could not serve.
    #[test]
    fn resolves_via_the_embedded_copy_when_no_external_candidate_does() {
        let mut reference = reference_with(Some("marshall/plexi.nam"), Some("/abs/plexi.nam"));
        reference.embedded = Some(an_embedded_copy());
        let resolver = FakeResolver::default(); // nothing registered: all three steps miss

        match resolve(&reference, &resolver) {
            Resolution::Embedded(embedded) => assert_eq!(embedded, &an_embedded_copy()),
            other => panic!("expected the embedded fallback, got {other:?}"),
        }
    }

    /// And deliberately *last*: an external candidate that resolves wins over an embedded copy,
    /// so a preset never ignores the file the user actually has on disk.
    #[test]
    fn an_embedded_copy_is_tried_only_after_every_external_candidate() {
        let mut reference = reference_with(Some("marshall/plexi.nam"), None);
        reference.embedded = Some(an_embedded_copy());
        let mut resolver = FakeResolver::default();
        resolver.by_relative.insert(
            "marshall/plexi.nam".to_string(),
            PathBuf::from("/library/marshall/plexi.nam"),
        );

        match resolve(&reference, &resolver) {
            Resolution::Resolved(found) => assert_eq!(found.via, ResolvedVia::LibraryRelative),
            other => panic!("the library-relative hit must win over the embed, got {other:?}"),
        }
    }

    /// The same, one step further down the order: the content-hash search is still an external
    /// candidate and still precedes the embed.
    #[test]
    fn a_content_hash_hit_still_precedes_the_embedded_copy() {
        let mut reference = reference_with(None, None);
        reference.embedded = Some(an_embedded_copy());
        let mut resolver = FakeResolver::default();
        resolver
            .by_hash
            .insert(reference.hash, PathBuf::from("/library/found-by-hash.nam"));

        match resolve(&reference, &resolver) {
            Resolution::Resolved(found) => assert_eq!(found.via, ResolvedVia::ContentHash),
            other => panic!("the hash hit must win over the embed, got {other:?}"),
        }
    }

    /// `candidates` covers the three steps a `FileResolver` can answer, and says so — `embedded`
    /// is not among them, by design (see the module doc comment), which is why a driver that
    /// walks this iterator has to apply the fourth step itself.
    #[test]
    fn candidates_yields_only_the_three_externally_resolved_steps() {
        let mut reference = reference_with(Some("marshall/plexi.nam"), Some("/abs/plexi.nam"));
        reference.embedded = Some(an_embedded_copy());
        assert_eq!(candidates(&reference).count(), 3);
    }

    /// The fourth outcome: nothing resolves — no external candidate, and no embedded copy
    /// either (`reference_with` builds one without, which is what makes this `Missing` rather
    /// than the embedded fallback).
    // trace: FR-STATE-070
    #[test]
    fn all_three_candidates_failing_yields_missing_with_name_and_hash() {
        let reference = reference_with(Some("marshall/plexi.nam"), Some("/abs/plexi.nam"));
        assert!(reference.embedded.is_none());
        let resolver = FakeResolver::default(); // nothing registered at all

        match resolve(&reference, &resolver) {
            Resolution::Missing(missing) => {
                assert_eq!(missing.display_name, "plexi.nam");
                assert_eq!(missing.hash, reference.hash);
            }
            other => panic!("expected Missing, got {other:?}"),
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
