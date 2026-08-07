//! D-5.1's role for this crate: "Preset and plugin-state document, versioning, file-reference
//! resolution." This crate owns the **format and the algorithm**; it never touches a filesystem
//! and never parses a `.nam`/WAV file itself — see the "Scope" section below and
//! `docs/02-architecture.md` §5's `namir-state` row (`core, params` only in its *May depend on*
//! column).
//!
//! # D-11.1: JSON, pretty-printed, sorted keys, an explicit `format_version`
//!
//! See [`document`]'s module doc comment for why a JSON object is never discarded on read (the
//! mechanism D-11.2's unknown-field preservation actually needs) and why sorted key order costs
//! nothing to guarantee in this workspace's build.
//!
//! # Scope
//!
//! In scope, and closed by this crate (`docs/03-implementation-roadmap.md` §9 — M5):
//! - FR-STATE-010 — [`State`]'s round trip through [`Document`], covering every section this
//!   crate owns: `parameters` (which, since D-10.4, carries `global.bypass`/
//!   `global.output_ceiling_db` alongside every stage's own parameters — see `state.rs`'s module
//!   doc comment) and `references.nam`/`references.ir`.
//! - FR-STATE-020 — [`ParamValues`]'s complete-array-over-`REGISTRY` shape, so an absent
//!   parameter's default cannot fail to apply.
//! - FR-STATE-040 — the JSON format itself: pretty-printed, sorted, and, via [`reference::RelPath`],
//!   free of platform-specific path syntax in what it stores.
//! - FR-STATE-070 — [`resolve::candidates`]/[`resolve::FileResolver`]/[`resolve::resolve`]: the
//!   three-step resolution order as data plus the port a resolving crate implements. This crate
//!   never resolves a reference against a real filesystem itself — see [`resolve`]'s module doc
//!   comment for why the algorithm and the filesystem access it needs are deliberately kept apart.
//! - FR-STATE-080 — [`reference::FileRef::embedded`], read and written (M5's Should-scope
//!   decision).
//! - D-11.2 — tolerant, versioned deserialisation; see [`document`], [`params`] and [`migrate`].
//!
//! Out of scope, deliberately, for this crate:
//! - **Resolving a file reference against a real filesystem or a real library index** —
//!   `namir-library` implements [`resolve::FileResolver`]; `namir-worker` drives it. D-5.1
//!   forbids this crate from depending on either.
//! - **Applying a restored value to a live engine instance** — `namir-worker`'s `recall` (M5),
//!   which is the crate that can see both this crate and `namir-engine`.
//! - **Where a preset file lives on disk** — a path is always a constructor argument to whatever
//!   reads/writes bytes; this crate only ever sees bytes already in memory.

mod document;
mod error;
mod error_codes;
mod migrate;
mod params;
mod reference;
mod resolve;
mod state;

pub use document::{Document, FORMAT_VERSION, MAX_DOCUMENT_BYTES};
pub use error::{StateError, StateWarning};
pub use params::{ParamValues, UnknownParameter};
pub use reference::{EmbeddedRef, FileRef, MAX_EMBEDDED_BYTES, RelPath, RelPathError};
pub use resolve::{
    Candidate, FileResolver, MissingFile, Resolution, ResolvedFile, ResolvedVia, candidates,
    resolve,
};
pub use state::State;
