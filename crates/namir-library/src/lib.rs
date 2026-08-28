//! D-5.1's role for this crate: "Library index, scanning, hashing, search, persistence." May
//! depend on `core`, `nam`, `ir`, `state` only (`xtask layering`'s `LAYERING_TABLE` already
//! carries this row) — notably **not** `namir-worker`, `namir-engine`, or `namir-platform`. Two
//! consequences worth stating up front, both recorded as `*Consequence (added M5)*` notes in
//! `docs/02-architecture.md`:
//!
//! - **This crate never learns where library roots or its index file live.** D-13.2's filesystem
//!   locations belong to `namir-platform`, an M6 crate this one may not depend on regardless.
//!   Every path this crate touches is a caller-supplied argument — the same discipline
//!   `namir-worker`'s pre-existing `LoadSource::File` already applies.
//! - **This crate never learns that threads exist.** D-12.2 calls scanning "a cancellable worker
//!   job", but D-5.1 forbids depending on `namir-worker`. [`scan::Scanner`] is a caller-pumped
//!   step machine instead — see that module's doc comment.
//!
//! # Scope
//!
//! In scope, and closed by this crate (`docs/03-implementation-roadmap.md` §9 — M5):
//! - FR-LIB-010 — [`scan::Scanner`] over one or more roots, `.nam`/`.wav` only
//!   ([`probe::kind_from_extension`]).
//! - D-12.1 — the incremental size+mtime rule ([`scan`]'s module doc comment).
//! - D-12.3/AQ-3 — [`store::IndexStore`]'s single-JSON-document, atomic-replace persistence.
//! - D-11.3's consequence — [`index::Index::paths_for_hash`], and [`resolve`]'s
//!   `namir_state::FileResolver` implementation (lands with `namir-worker`'s `recall.rs`, M5).
//! - NFR-SEC-020 — [`MAX_INDEXED_FILE_BYTES`], checked before a file's bytes are read.
//!
//! Out of scope, deliberately:
//! - **Scheduling the scan on a thread, cancelling it, reporting progress on a cadence** —
//!   `namir-worker`'s `library.rs` (M5), which drives [`scan::Scanner`] on its existing pool.
//! - **Where the index file or library roots live on disk** — always a constructor argument.

mod entry;
mod error;
mod error_codes;
mod favourites;
mod fs;
mod hash_hex;
mod index;
mod probe;
mod resolver;
mod scan;
mod search;
mod store;

pub use entry::{
    FileTime, IrItemMetadata, ItemKind, ItemMetadata, LibraryEntry, NamItemMetadata, Origin,
};
pub use error::{LibraryError, LibraryWarning};
pub use favourites::Favourites;
pub use fs::{DirEntryInfo, DirListing, ScanFs, StdFs};
pub use index::Index;
pub use probe::{kind_from_extension, probe};
pub use resolver::{LibraryResolver, RootsOnlyResolver};
pub use scan::{ScanDelta, ScanProgress, Scanner, Step};
pub use search::{Query, filter, next_after, previous_before};
pub use store::IndexStore;

/// NFR-SEC-020's documented upper bound on a single file this crate will read into memory while
/// scanning — the same figure `namir_core::MAX_FILE_BYTES` already documents for "an untrusted
/// file Namir reads into memory in one piece" (moved there in this same milestone specifically
/// so this crate, which may not depend on `namir-worker`, could reach it). A file over this
/// ceiling is still indexed (browsable by path) with no hash and no extracted metadata — see
/// [`entry::LibraryEntry::hash`]'s doc comment.
pub const MAX_INDEXED_FILE_BYTES: usize = namir_core::MAX_FILE_BYTES;
