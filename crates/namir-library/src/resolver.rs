//! Seam 1's other half (`docs/02-architecture.md` §11's D-11.3 consequence note): this crate
//! implements `namir_state::FileResolver` against a real [`Index`] and a caller-supplied set of
//! library roots.
//!
//! Two implementations: [`LibraryResolver`] for the ordinary case (a real index exists), and
//! [`RootsOnlyResolver`] for the case a resolver is needed before any scan has ever run (first
//! launch, or a CLAP instance in a host with no library configured yet) — step 3 (hash search)
//! always misses, since there is no index to search.

use std::path::PathBuf;

use namir_core::ContentHash;
use namir_state::{FileResolver, RelPath};

use crate::index::Index;

fn resolve_relative_against_roots(roots: &[PathBuf], rel: &RelPath) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| rel.join_onto(root))
        .find(|p| p.exists())
}

fn resolve_absolute_path(absolute: &str) -> Option<PathBuf> {
    let path = PathBuf::from(absolute);
    path.exists().then_some(path)
}

/// Resolves against a real [`Index`] and a set of library roots, in the caller's configured
/// order. Existence is checked directly against the real filesystem (`Path::exists`) — a simple
/// enough operation that, unlike `scan.rs`'s directory walk, does not need its own injected port;
/// resolving one reference is a one-shot check, not a long-running, cancellable operation.
pub struct LibraryResolver<'a> {
    index: &'a Index,
    roots: &'a [PathBuf],
}

impl<'a> LibraryResolver<'a> {
    /// Resolves against `index`, trying `roots` in the given order.
    pub fn new(index: &'a Index, roots: &'a [PathBuf]) -> Self {
        Self { index, roots }
    }
}

impl FileResolver for LibraryResolver<'_> {
    fn resolve_library_relative(&self, rel: &RelPath) -> Option<PathBuf> {
        resolve_relative_against_roots(self.roots, rel)
    }

    fn resolve_absolute(&self, absolute: &str) -> Option<PathBuf> {
        resolve_absolute_path(absolute)
    }

    /// D-11.3's consequence note, exercised: the index's hash → path map, filtered to a path that
    /// still exists (the index may be stale — a file moved or was deleted since the last scan —
    /// and P7's "paths are hints" means a stale hit is worth skipping past, not returning). When
    /// several paths share a hash (the normal case for a duplicated model in a community
    /// library), the first that exists, in the index's own iteration order, wins — a
    /// deterministic tie-break, not a meaningful ranking.
    fn resolve_by_hash(&self, hash: ContentHash) -> Option<PathBuf> {
        self.index
            .paths_for_hash(hash)
            .iter()
            .find(|p| p.exists())
            .cloned()
    }
}

/// The no-index-yet resolver: steps 1 and 2 behave exactly like [`LibraryResolver`], step 3
/// always misses.
pub struct RootsOnlyResolver<'a> {
    roots: &'a [PathBuf],
}

impl<'a> RootsOnlyResolver<'a> {
    /// Resolves against `roots` only; hash search always misses.
    pub fn new(roots: &'a [PathBuf]) -> Self {
        Self { roots }
    }
}

impl FileResolver for RootsOnlyResolver<'_> {
    fn resolve_library_relative(&self, rel: &RelPath) -> Option<PathBuf> {
        resolve_relative_against_roots(self.roots, rel)
    }

    fn resolve_absolute(&self, absolute: &str) -> Option<PathBuf> {
        resolve_absolute_path(absolute)
    }

    fn resolve_by_hash(&self, _hash: ContentHash) -> Option<PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{FileTime, ItemKind, ItemMetadata, Origin};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-library-resolver-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn library_resolver_finds_a_file_under_a_root_by_relative_path() {
        let root = temp_dir("relative");
        std::fs::create_dir_all(root.join("marshall")).unwrap();
        std::fs::write(root.join("marshall/plexi.nam"), b"x").unwrap();

        let roots = vec![root.clone()];
        let empty = Index::empty();
        let resolver = LibraryResolver::new(&empty, &roots);
        let rel = RelPath::parse("marshall/plexi.nam").unwrap();
        assert_eq!(
            resolver.resolve_library_relative(&rel),
            Some(root.join("marshall").join("plexi.nam"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_resolver_misses_a_relative_path_under_no_root() {
        let roots = vec![PathBuf::from("/nonexistent-root-for-test")];
        let empty = Index::empty();
        let resolver = LibraryResolver::new(&empty, &roots);
        let rel = RelPath::parse("marshall/plexi.nam").unwrap();
        assert_eq!(resolver.resolve_library_relative(&rel), None);
    }

    #[test]
    fn library_resolver_tries_roots_in_order() {
        let root_a = temp_dir("order_a");
        let root_b = temp_dir("order_b");
        std::fs::write(root_b.join("plexi.nam"), b"x").unwrap(); // only present under root_b

        let roots = vec![root_a.clone(), root_b.clone()];
        let empty = Index::empty();
        let resolver = LibraryResolver::new(&empty, &roots);
        let rel = RelPath::parse("plexi.nam").unwrap();
        assert_eq!(
            resolver.resolve_library_relative(&rel),
            Some(root_b.join("plexi.nam"))
        );

        let _ = std::fs::remove_dir_all(&root_a);
        let _ = std::fs::remove_dir_all(&root_b);
    }

    #[test]
    fn library_resolver_finds_an_absolute_path_that_exists() {
        let root = temp_dir("absolute");
        let file = root.join("plexi.nam");
        std::fs::write(&file, b"x").unwrap();

        let roots: Vec<PathBuf> = vec![];
        let empty = Index::empty();
        let resolver = LibraryResolver::new(&empty, &roots);
        assert_eq!(
            resolver.resolve_absolute(file.to_str().unwrap()),
            Some(file)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_resolver_misses_an_absolute_path_that_does_not_exist() {
        let roots: Vec<PathBuf> = vec![];
        let empty = Index::empty();
        let resolver = LibraryResolver::new(&empty, &roots);
        assert_eq!(
            resolver.resolve_absolute("/nonexistent-file-for-test.nam"),
            None
        );
    }

    #[test]
    fn library_resolver_finds_by_hash_via_the_index() {
        let root = temp_dir("by_hash");
        let file = root.join("plexi.nam");
        std::fs::write(&file, b"model bytes").unwrap();
        let hash = namir_core::ContentHash::of(b"model bytes");

        let mut index = Index::empty();
        index.upsert(crate::entry::LibraryEntry {
            path: file.clone(),
            kind: ItemKind::Nam,
            size: 11,
            mtime: FileTime::now(),
            hash: Some(hash),
            metadata: ItemMetadata::None,
            origin: Origin::Local,
        });

        let roots: Vec<PathBuf> = vec![];
        let resolver = LibraryResolver::new(&index, &roots);
        assert_eq!(resolver.resolve_by_hash(hash), Some(file));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// P7's "paths are hints": a hash match whose indexed path no longer exists (the file moved
    /// or was deleted since the last scan) must not be returned.
    #[test]
    fn library_resolver_skips_a_stale_hash_hit_whose_path_no_longer_exists() {
        let hash = namir_core::ContentHash::of(b"gone");
        let mut index = Index::empty();
        index.upsert(crate::entry::LibraryEntry {
            path: PathBuf::from("/this/file/does/not/exist.nam"),
            kind: ItemKind::Nam,
            size: 4,
            mtime: FileTime::now(),
            hash: Some(hash),
            metadata: ItemMetadata::None,
            origin: Origin::Local,
        });

        let roots: Vec<PathBuf> = vec![];
        let resolver = LibraryResolver::new(&index, &roots);
        assert_eq!(resolver.resolve_by_hash(hash), None);
    }

    #[test]
    fn roots_only_resolver_always_misses_by_hash() {
        let roots: Vec<PathBuf> = vec![];
        let resolver = RootsOnlyResolver::new(&roots);
        assert_eq!(
            resolver.resolve_by_hash(namir_core::ContentHash::of(b"x")),
            None
        );
    }

    #[test]
    fn roots_only_resolver_still_resolves_relative_and_absolute_paths() {
        let root = temp_dir("roots_only");
        std::fs::write(root.join("plexi.nam"), b"x").unwrap();

        let roots = vec![root.clone()];
        let resolver = RootsOnlyResolver::new(&roots);
        let rel = RelPath::parse("plexi.nam").unwrap();
        assert_eq!(
            resolver.resolve_library_relative(&rel),
            Some(root.join("plexi.nam"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
