//! D-19.1's index-scale fixture: a deterministic, seeded, cached 10,000-file synthetic
//! model+IR library. Serves `namir-library`'s FR-LIB-020 ("scan a >=10,000-file library,
//! cancellable") and NFR-PERF-060/FR-LIB-030 ("an unchanged 10,000-file incremental rescan
//! completes within 2 seconds and is measurably faster than the first scan"), and is earmarked
//! for M6's FR-UI-060 ("UI stays responsive during a 10,000-file scan") — which is why this lives
//! here rather than inside `namir-library`'s own test module: `namir-fixtures` is the one crate
//! `xtask layering`'s D-5.1 edge-check exempts entirely (see `xtask/src/layering.rs`'s `FIXTURES`
//! constant and its module doc comment), so any future crate can dev-depend on it without a
//! layering exception.
//!
//! # Composition: hash-diverse, deliberately not model-diverse
//!
//! This corpus's job is *index scale*, not model diversity. It is [`IR_COUNT`] tiny mono IRs
//! (`.wav`, via [`crate::ir::decaying_noise`] and [`crate::ir::to_mono_wav_bytes`]) plus
//! [`NAM_COUNT`] `.nam` files (via [`crate::nam::generate`]'s [`crate::nam::WaveNetShape::Nano`],
//! the cheapest shape). [`IR_COUNT`] + [`NAM_COUNT`] == [`TOTAL_COUNT`] == 10,000.
//!
//! A hash-keyed index needs real diversity to test against, so every file's
//! [`namir_core::ContentHash`] must differ (and [`generate_shared_corpus`]'s own test confirms
//! this) — but running [`crate::nam::generate`]'s two-pass, calibration-inference generator
//! 1,000 separate times is needless work for content nobody is going to inspect. So the `.nam`
//! side generates **one** base [`crate::nam::NamModel`] for the whole corpus and clones it per
//! file, varying only [`crate::nam::NamMetadata::name`]/`description` (cheap: a `format!` and a
//! re-serialize) to keep every file's JSON bytes, and therefore its hash, unique. The `.wav` side
//! does the opposite — regenerating [`crate::ir::decaying_noise`] per file with a distinct seed —
//! because unlike WaveNet calibration, noise generation is already cheap at this length, so there
//! is no cost to buy back by faking it.
//!
//! **This means the corpus is hash-diverse but structurally uniform**: right for testing index
//! *scale* (distinct paths, distinct hashes, a lot of files), wrong for testing model
//! *correctness* (every `.nam` file in the corpus is architecturally identical). A caller that
//! needs the latter should reach for [`crate::nam::generate`] directly with varying
//! [`crate::nam::WaveNetShape`]/seed, not this module.
//!
//! # Tree shape
//!
//! Nested directories, not one flat directory of 10,000 files — real libraries are organized in
//! folders, and NTFS (this project's primary target) behaves differently on a single huge flat
//! directory than on a shallow nested tree. The layout is 10 "banks" x 10 "folders" x 100 files
//! per leaf folder = 10,000 (`BANKS` x `FOLDERS_PER_BANK` x `FILES_PER_LEAF`). Within each
//! leaf folder the first 90 slots are IRs and the last 10 are `.nam` files (matching the overall
//! 9:1 ratio), so every leaf folder looks like a small, realistic mixed-content directory rather
//! than being segregated by type.
//!
//! # Caching
//!
//! [`generate_shared_corpus`] writes into a content-addressed directory under the workspace
//! `target/` (this module's private `cache_root` function), not into the repo, and not
//! regenerated on every run. The directory name is keyed on a hash of `GENERATOR_VERSION`, the
//! seed, the composition constants above, **and the two generators whose output the corpus
//! actually is** — [`crate::nam::GENERATOR_VERSION`], [`crate::ir::GENERATOR_VERSION`] and the
//! serialized `.nam` shape (this module's private `cache_signature` function) — so changing any of
//! them (including bumping a `GENERATOR_VERSION` by hand after a change to the corresponding
//! module's generation logic) invalidates stale cached output automatically rather than silently
//! serving last run's corpus. Naming the two other generators is not decoration: nothing in
//! `cargo test` re-validates a cached corpus, so before the key folded them in, a change to
//! `nam/mod.rs`'s weight layout left this cache serving the old layout's files indefinitely and
//! the failure surfaced as a `namir-library` scan test rejecting them, pointing nowhere near the
//! cause. A cache hit
//! reads one small JSON manifest and stats two files (this module's private `try_load_cached`
//! function) — it never re-walks or re-hashes all 10,000 files. Building is race-safe across
//! concurrent processes/threads sharing
//! one cache root: each caller builds into a private temp directory and atomically renames it
//! into place, so two callers racing to fill a cold cache never observe each other's partial
//! output (see [`generate_shared_corpus`]'s doc comment).
//!
//! # Honest caveat: tiny files understate real per-file cost, directionally
//!
//! Each generated file is roughly 1-11 KB; a real model/IR library has files from a few KB up to
//! 50 MB. For NFR-PERF-060 this is benign: the incremental-rescan path only compares size and
//! mtime and never rehashes an unchanged file, so file size barely affects the quantity being
//! measured. For "the second scan is measurably faster than the first" it biases *conservatively*
//! — small files make the *first* (full-hash) scan cheaper than a real library's would be,
//! shrinking the gap incremental rescanning has to demonstrate, not inflating it. A benchmark
//! built on this corpus that shows a convincing speed-up is not being flattered by the file sizes;
//! if anything it is working against a smaller margin than production will have.
//!
//! # Two fixtures, not one
//!
//! [`generate_shared_corpus`]'s 10,000-file tree is read-only and shared/cached across every
//! caller and test run; mutating it would break the cache invariant for every other consumer and
//! make test execution order matter, which this project's culture treats as a real defect. FR-
//! LIB-070 (files that change/disappear/appear mid-session) instead gets [`mutable_probe_set`]: a
//! small, freshly-generated-per-call, deliberately mutable set the caller owns outright and is
//! free to edit, delete, rename or touch.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use namir_core::ContentHash;

use crate::ir;
use crate::nam::{self, WaveNetShape};

/// Bumped whenever this module's generation logic changes in any way that would produce
/// different bytes for the same seed — the cache key folds this in specifically so a stale
/// on-disk corpus from a previous version of this generator is never mistaken for a fresh one.
const GENERATOR_VERSION: u32 = 1;

/// Number of `.wav` IR fixtures in the shared corpus.
pub const IR_COUNT: usize = 9_000;
/// Number of `.nam` model fixtures in the shared corpus.
pub const NAM_COUNT: usize = 1_000;
/// [`IR_COUNT`] + [`NAM_COUNT`] — the shared corpus's total file count (FR-LIB-020's ">=10,000").
pub const TOTAL_COUNT: usize = IR_COUNT + NAM_COUNT;

const BANKS: usize = 10;
const FOLDERS_PER_BANK: usize = 10;
const FILES_PER_LEAF: usize = 100;
/// Within a leaf folder, slots `0..IR_SLOTS_PER_LEAF` are IRs and the rest are `.nam` files.
const IR_SLOTS_PER_LEAF: usize = 90;

const _: () = assert!(BANKS * FOLDERS_PER_BANK * FILES_PER_LEAF == TOTAL_COUNT);
const _: () = assert!(IR_SLOTS_PER_LEAF * BANKS * FOLDERS_PER_BANK == IR_COUNT);
const _: () = assert!((FILES_PER_LEAF - IR_SLOTS_PER_LEAF) * BANKS * FOLDERS_PER_BANK == NAM_COUNT);

/// The `.nam` shape every model in the corpus is generated from — the cheapest one, per this
/// module's doc comment on structural uniformity. Named rather than repeated at its two use
/// sites (`base_nam_model` and `cache_signature`) so the shape the cache key describes cannot
/// drift from the shape the corpus is actually built with.
const CORPUS_NAM_SHAPE: WaveNetShape = WaveNetShape::Nano;

/// Each generated IR's length in samples — small deliberately (see this module's doc comment on
/// tiny files understating real per-file cost).
const IR_LEN_SAMPLES: usize = 1_024;
const IR_TAU_SAMPLES: f64 = 80.0;
const IR_SAMPLE_RATE: u32 = 48_000;

/// A generated fixture's kind, so a caller can assert "the index found N `.nam` and M `.wav`
/// entries" without re-deriving it from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// A `.nam` model fixture, generated via [`crate::nam::generate`].
    Nam,
    /// A `.wav` IR fixture, generated via [`crate::ir::decaying_noise`].
    Ir,
}

/// One generated fixture file: everything a library-index test needs to assert "the index found
/// exactly this" without re-deriving it (parsing the file again, re-hashing it, guessing the kind
/// from the extension).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    /// Absolute path to the generated file.
    pub path: PathBuf,
    /// Which generator produced this file.
    pub kind: EntryKind,
    /// The file's content hash, computed once at generation time — a caller building/checking a
    /// hash-keyed index does not need to re-hash the file itself just to get this value.
    pub content_hash: ContentHash,
}

/// The result of [`generate_shared_corpus`]: where the tree lives, and every file in it.
#[derive(Debug, Clone)]
pub struct LibraryCorpus {
    /// The corpus's root directory. Contains `BANKS` `bank_NN` directories, each containing
    /// `FOLDERS_PER_BANK` `folder_NN` directories, each containing `FILES_PER_LEAF` files.
    pub root: PathBuf,
    /// The seed this corpus was generated from.
    pub seed: u64,
    /// Every generated file, in deterministic (bank, folder, slot) order. Always
    /// [`TOTAL_COUNT`] long.
    pub entries: Vec<LibraryEntry>,
}

/// The result of [`mutable_probe_set`]: a small fixture the caller owns and may freely mutate.
#[derive(Debug, Clone)]
pub struct MutableProbeSet {
    /// The directory the fixture was written into (the caller-supplied `dir`).
    pub root: PathBuf,
    /// Every generated file, as they existed immediately after generation. FR-LIB-070 tests are
    /// expected to go on to modify/delete/add files under `root` themselves — this list is a
    /// snapshot of the starting state, not a live view.
    pub entries: Vec<LibraryEntry>,
}

/// Number of files [`mutable_probe_set`] generates — small on purpose (see this module's doc
/// comment on why FR-LIB-070's fixture must not be the shared corpus).
const MUTABLE_IR_COUNT: usize = 12;
const MUTABLE_NAM_COUNT: usize = 4;

/// Builds a base `.nam` model once for the whole shared corpus (see this module's doc comment on
/// why the corpus is structurally uniform on the `.nam` side). Panics on
/// [`crate::nam::DegenerateFixtureError`]: that would mean `Nano` at this fixed seed produces a
/// degenerate model, which is a bug in the seed choice, not a condition a corpus-scale caller
/// should have to handle per call.
fn base_nam_model(seed: u64) -> nam::NamModel {
    nam::generate(CORPUS_NAM_SHAPE, seed)
        .unwrap_or_else(|e| panic!("library corpus base NAM model is degenerate: {e}"))
}

/// Clones `base`, overwrites its display metadata with `label` (unique per file, see the module
/// doc comment), and returns the re-serialized JSON bytes — the corpus's actual per-file cost on
/// the `.nam` side, versus a full [`nam::generate`] call.
fn nam_variant_bytes(base: &nam::NamModel, label: &str) -> Vec<u8> {
    let mut model = base.clone();
    model.metadata.name = format!("namir-fixtures library corpus: {label}");
    model.metadata.description = format!(
        "Seeded namir-fixtures library-corpus fixture ({label}); weights are shared across the \
         corpus (see namir_fixtures::library's doc comment on structural uniformity), only this \
         metadata varies."
    );
    model.to_json_bytes()
}

/// One IR variant's WAV bytes for `seed` — real per-file regeneration, not a metadata trick (see
/// the module doc comment on why the two fixture kinds take different shortcuts).
fn ir_variant_bytes(seed: u64) -> Vec<u8> {
    let samples = ir::decaying_noise(IR_LEN_SAMPLES, seed, IR_TAU_SAMPLES);
    ir::to_mono_wav_bytes(&samples, IR_SAMPLE_RATE)
}

/// Derives a distinct per-file seed from the corpus seed and a file index — cheap, deterministic,
/// and avoids ever reusing `corpus_seed` itself as a per-file seed (which would make file 0 of
/// every corpus share a seed with, e.g., every other seeded fixture built from `corpus_seed`
/// directly).
fn derive_seed(corpus_seed: u64, index: u64) -> u64 {
    // splitmix64's mixing step: fast, well-distributed, and needs no extra dependency.
    let mut z = corpus_seed.wrapping_add(index.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The workspace's shared `target/` directory: `namir-fixtures`'s own manifest directory
/// (fixed at this crate's compile time) two levels up, joined with `target`, unless
/// `CARGO_TARGET_DIR` overrides it at runtime (matching how Cargo itself resolves the target
/// directory). Deliberately not `std::env::temp_dir()`: the whole point of the cache is that it
/// survives between runs in a place a developer would expect generated build artifacts to live
/// and get swept by an ordinary `cargo clean`, not the OS's general scratch space.
fn workspace_target_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
}

/// Where every [`generate_shared_corpus`] cache directory lives.
fn cache_root() -> PathBuf {
    workspace_target_dir().join("namir-fixtures-cache")
}

/// Everything the bytes of a cached corpus depend on, as one string — the input [`cache_key`]
/// hashes. Parameterized over the two *other* modules' generator versions rather than reading
/// them directly so a test can vary them; [`cache_key`] passes the real ones.
///
/// # Why the two generator versions are in here
///
/// [`GENERATOR_VERSION`] covers this module's own logic and the constants below cover its
/// composition, but the corpus's actual file content comes from [`crate::nam::generate`] and
/// [`crate::ir::decaying_noise`], which this signature named nothing about. Changing the WaveNet
/// weight layout in `nam/mod.rs` therefore left a warm `target/namir-fixtures-cache` serving the
/// old layout's `.nam` files under an unchanged key, indefinitely — and no `cargo test` path
/// re-validates a cached corpus (a hit reads the manifest and stats two files, by design), so the
/// visible symptom was a `namir-library` scan test failing against files the current parser
/// rejects, with nothing pointing at the cache. Both generators now carry a
/// `GENERATOR_VERSION` of their own and both are folded in here.
///
/// The `.nam` side additionally folds in the *shape* the corpus is built from, serialized: it is
/// derived rather than hand-maintained, needs no inference pass to compute (so a cache hit stays
/// as cheap as it was), and so catches a change to [`WaveNetShape::Nano`]'s topology even if
/// whoever made it forgets to bump [`crate::nam::GENERATOR_VERSION`].
fn cache_signature(seed: u64, nam_generator_version: u32, ir_generator_version: u32) -> String {
    let nam_shape = serde_json::to_string(&nam::shape_signature(CORPUS_NAM_SHAPE))
        .expect("a layer-config list always serializes (plain numbers, strings and arrays)");
    let nam_init = nam_weight_fingerprint(seed);
    format!(
        "namir-fixtures-library-corpus|v{GENERATOR_VERSION}|seed={seed}|ir={IR_COUNT}|\
         nam={NAM_COUNT}|banks={BANKS}|folders={FOLDERS_PER_BANK}|leaf={FILES_PER_LEAF}|\
         ir_len={IR_LEN_SAMPLES}|ir_tau={IR_TAU_SAMPLES}|ir_rate={IR_SAMPLE_RATE}|\
         nam_gen=v{nam_generator_version}|ir_gen=v{ir_generator_version}|nam_shape={nam_shape}|\
         nam_init={nam_init}"
    )
}

/// A hash of the weights [`nam::uncalibrated_weights`] produces for this corpus's shape and
/// `seed` — the layout-sensitive half of the cache key, and the one that needs no hand
/// maintenance: reordering `build_weights`' sections or changing an initialisation scale changes
/// these bytes and therefore the cache directory. Costs one seeded RNG pass over the cheapest
/// shape's ~2,000 weights (microseconds), so a cache hit stays cheap; deliberately not a hash of a
/// fully generated model, which would put `generate`'s two inference passes on every hit.
fn nam_weight_fingerprint(seed: u64) -> String {
    let weights = nam::uncalibrated_weights(CORPUS_NAM_SHAPE, seed);
    let mut bytes = Vec::with_capacity(weights.len() * 4);
    for w in &weights {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    ContentHash::of(&bytes).to_string()[..16].to_string()
}

/// The cache key for a shared corpus generated from `seed`: [`namir_core::ContentHash`] of
/// [`cache_signature`], so a change to anything that signature names changes the key and
/// therefore the cache directory name, rather than silently reusing an incompatible directory.
/// Truncated to 16 hex characters (64 bits, ample collision resistance for a cache-directory
/// name) purely to keep the resulting nested path short on Windows.
fn cache_key(seed: u64) -> String {
    let signature = cache_signature(seed, nam::GENERATOR_VERSION, ir::GENERATOR_VERSION);
    let hash = ContentHash::of(signature.as_bytes()).to_string();
    hash[..16].to_string()
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestEntry {
    /// Path relative to the corpus root, forward-slash-separated (portable regardless of the
    /// platform that generated it).
    rel_path: String,
    kind: EntryKind,
    hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    generator_version: u32,
    key: String,
    seed: u64,
    entries: Vec<ManifestEntry>,
}

const MANIFEST_FILE_NAME: &str = "_manifest.json";

fn leaf_dir_name(bank: usize, folder: usize) -> (String, String) {
    (format!("bank_{bank:02}"), format!("folder_{folder:02}"))
}

/// Generates (or reuses a cached) [`TOTAL_COUNT`]-file shared library corpus for `seed`. Same
/// `seed` always yields the same set of relative paths, kinds and content hashes (see this
/// module's test that generating twice is equivalent) — the directory is cached under
/// `target/namir-fixtures-cache` (this module's private `cache_root` function) and reused across
/// calls/processes rather than rebuilt every time.
///
/// # Concurrency
///
/// Safe to call concurrently from multiple threads or processes sharing the same `target/`
/// directory (e.g. a test binary and a bench binary both racing to fill a cold cache): each
/// caller that misses the cache builds into a private `_manifest.json`-less temp directory (named
/// with this process's PID plus a per-process counter, so concurrent attempts never collide with
/// each other) and finishes by writing the manifest and atomically renaming the temp directory
/// into the final cache path. A reader only ever observes the final path after the rename makes
/// it appear, so it never sees a partially-written corpus. If two callers both finish building at
/// nearly the same time, the loser's `rename` fails (the destination already exists), and it
/// reads the winner's instead — both built byte-identical content from the same seed, so which
/// one "wins" is immaterial.
///
/// **The publish step does not trust a single failed `rename` to mean "someone else already
/// published it", and does not trust an existing `dir` to mean it holds something valid.** Found
/// by real CI (this crate's own dev sandbox never exercised it, since its corpus cache was
/// already warm or uncontended every time this milestone ran `cargo test --workspace` there): a
/// non-empty `dir` can be stale, invalid content left by an earlier interrupted build rather than
/// a genuine winner's publish, and if so, every future `rename` onto it fails forever — no amount
/// of retrying a read-only check recovers from that. The publish step validates what is actually
/// at `dir` and clears it if it is not a valid corpus, before attempting to claim the path, over
/// several bounded retries. See [`build_and_publish_corpus`]'s own doc comment for the full shape.
///
/// # Errors
///
/// Returns an [`io::Error`] if the cache directory can't be created or written to, or if a
/// generated file somehow fails re-parsing by its own probe (a bug in this generator, not a
/// caller error — see the module's tests for the coverage that is meant to catch this before a
/// consumer ever would).
pub fn generate_shared_corpus(seed: u64) -> io::Result<LibraryCorpus> {
    let key = cache_key(seed);
    let dir = cache_root().join(format!("lib-corpus-{key}"));

    // In-process guard: several threads in the same test binary (e.g. this module's own tests,
    // several of which share `TEST_SEED`) racing a cold cache would otherwise each pay the full
    // ~10,000-file build cost concurrently before any of them could see the others' output. This
    // does not replace the cross-process safety net below (a lock here says nothing about a
    // sibling `cargo test` process or a bench binary) -- it just avoids the common in-process case
    // of duplicate work. Poisoning is not treated as fatal: a panic in another test while holding
    // this guard doesn't mean the cache logic itself is broken.
    let _guard = BUILD_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(corpus) = try_load_cached(&dir, seed, &key)? {
        return Ok(corpus);
    }
    build_and_publish_corpus(&dir, seed, &key)
}

static BUILD_GUARD: Mutex<()> = Mutex::new(());
static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Counts every *attempted* full-corpus build (i.e. every cache miss that reached
/// [`build_and_publish_corpus`]), keyed by seed, regardless of whether that attempt went on to
/// win or lose the publish race. Keyed by seed (not a single global count) because this process
/// may be generating several different seeds' corpora concurrently (this module's own tests do
/// exactly that) — a single global counter cannot tell "seed A got rebuilt" apart from "seed B
/// happened to get its first build in the same window", which is a real trap, not a hypothetical
/// one (this crate's own test suite hit it during development).
///
/// Not test-only instrumentation bolted on afterwards — this is the honest, deterministic way to
/// answer "did the cache actually get reused", which a wall-clock timing assertion cannot be on a
/// shared, possibly antivirus-scanned, possibly disk-contended Windows machine (this suite also
/// hit *that* flakiness during development: a genuine same-process cache *hit* still took several
/// seconds under concurrent disk load from sibling tests, which a timing threshold cannot tell
/// apart from an actual rebuild). See [`tests::the_cache_is_actually_reused_on_a_second_call`].
static BUILD_ATTEMPTS_BY_SEED: Mutex<Option<std::collections::HashMap<u64, u64>>> =
    Mutex::new(None);

fn record_build_attempt(seed: u64) {
    let mut guard = BUILD_ATTEMPTS_BY_SEED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard
        .get_or_insert_with(std::collections::HashMap::new)
        .entry(seed)
        .or_insert(0) += 1;
}

#[cfg(test)]
fn build_attempts_for_seed(seed: u64) -> u64 {
    let guard = BUILD_ATTEMPTS_BY_SEED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .as_ref()
        .and_then(|m| m.get(&seed).copied())
        .unwrap_or(0)
}

/// Attempts a cache hit: reads `dir/_manifest.json`, checks it agrees with `seed`/`key`/
/// [`GENERATOR_VERSION`] and carries exactly [`TOTAL_COUNT`] entries, then stats the first and
/// last entry's files (two `fs::metadata` calls, not [`TOTAL_COUNT`] of them) as a cheap sanity
/// check against a cache directory that was tampered with or partially removed after being
/// written. Returns `Ok(None)` (not an error) for an ordinary cache miss — anything that makes
/// the directory unusable, including it simply not existing yet.
fn try_load_cached(dir: &Path, seed: u64, key: &str) -> io::Result<Option<LibraryCorpus>> {
    let manifest_path = dir.join(MANIFEST_FILE_NAME);
    let manifest_bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let manifest: Manifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(m) => m,
        Err(_) => return Ok(None), // corrupt manifest: treat exactly like a miss and rebuild.
    };
    if manifest.generator_version != GENERATOR_VERSION
        || manifest.key != key
        || manifest.seed != seed
        || manifest.entries.len() != TOTAL_COUNT
    {
        return Ok(None);
    }

    let entries = manifest_entries_to_library_entries(dir, &manifest.entries);
    // Cheap spot check: the first and last file (in manifest order) must actually exist. This is
    // not a guarantee every one of the 10,000 files is intact -- that would mean re-walking the
    // tree, exactly what caching exists to avoid -- but it catches the common failure mode of a
    // human partially deleting the cache directory by hand.
    if let (Some(first), Some(last)) = (entries.first(), entries.last())
        && (fs::metadata(&first.path).is_err() || fs::metadata(&last.path).is_err())
    {
        return Ok(None);
    }

    Ok(Some(LibraryCorpus {
        root: dir.to_path_buf(),
        seed,
        entries,
    }))
}

fn manifest_entries_to_library_entries(
    root: &Path,
    entries: &[ManifestEntry],
) -> Vec<LibraryEntry> {
    entries
        .iter()
        .map(|e| LibraryEntry {
            path: root.join(&e.rel_path),
            kind: e.kind,
            content_hash: ContentHash::from_hex(&e.hash)
                .unwrap_or_else(|err| panic!("manifest hash {:?} is malformed: {err}", e.hash)),
        })
        .collect()
}

/// Bounded retries around the publish step — see [`build_and_publish_corpus`]'s doc comment for
/// why one attempt is not enough.
const MAX_PUBLISH_ATTEMPTS: u32 = 20;

/// Backoff between publish attempts. 20 attempts costs at most ~2 s of sleeping in the worst case
/// (this constant times [`MAX_PUBLISH_ATTEMPTS`]) — negligible next to the ~5 s a full 10,000-file
/// build already costs, and ample for a genuine winner's rename to become visible or for
/// transient contention on the shared cache directory to clear.
const PUBLISH_RETRY_BACKOFF: Duration = Duration::from_millis(100);

/// Cold-cache path: builds a full corpus into a private temp directory, writes its manifest, then
/// atomically publishes it to `dir` (see [`generate_shared_corpus`]'s doc comment on the race).
///
/// **The publish step actively clears whatever is at `dir` before claiming it, rather than
/// assuming any existing entry there is a valid winner's publish.** The original version treated
/// a failed `rename` as proof "another caller already published `dir` first" and moved on to read
/// it. That is too strong an assumption: `dir` can also be occupied by *stale, invalid* content —
/// observed on CI (not reproducible on this crate's own dev sandbox, whose corpus cache was
/// already warm or empty-and-uncontended every time this milestone ran `cargo test --workspace`
/// there) — a directory left behind by an earlier, interrupted build (a previous CI attempt that
/// failed partway, or a build-cache restore of one) is non-empty but never becomes valid, so
/// *every* `rename` onto it fails forever and no amount of retrying the read-back alone recovers.
/// This version checks what is actually at `dir` first: a *valid* corpus (real winner or an
/// earlier attempt's own success) is used as-is; anything else — missing, corrupt, or genuinely
/// stale — is removed before attempting to claim the path, so a bad destination can never
/// permanently block every future attempt. Content is deterministic per `(seed, key)`, so
/// clearing and re-publishing loses nothing even in the case this turns out to have been a
/// genuine, resolvable race rather than stale state.
///
/// **"Clear whatever is there" means whatever is *known* not to be a valid corpus, never whatever
/// merely failed to read.** See [`publish_built_corpus`], which this function delegates the
/// publish half to.
fn build_and_publish_corpus(dir: &Path, seed: u64, key: &str) -> io::Result<LibraryCorpus> {
    record_build_attempt(seed);
    let nonce = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        "{}.tmp-{}-{}",
        dir.file_name()
            .expect("cache dir always has a file name")
            .to_string_lossy(),
        std::process::id(),
        nonce
    );
    let tmp_dir = dir
        .parent()
        .expect("cache dir always has a parent (cache_root())")
        .join(tmp_name);
    fs::create_dir_all(&tmp_dir)?;

    if let Err(e) = build_corpus_into(&tmp_dir, seed, key) {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    publish_built_corpus(&tmp_dir, dir, seed, key)
}

/// The publish half of [`build_and_publish_corpus`]: claims `dir` for the already-built corpus in
/// `tmp_dir`, over [`MAX_PUBLISH_ATTEMPTS`] bounded retries, and returns the published corpus.
/// Separate from the build half so the failure paths below are testable without paying for a
/// [`TOTAL_COUNT`]-file build first.
///
/// # Only a *known-invalid* destination is ever cleared
///
/// [`try_load_cached`] answers three ways, and the third is not the second: `Ok(Some)` (a valid
/// corpus is there), `Ok(None)` (there is demonstrably nothing usable there — absent, corrupt,
/// stale, or the wrong seed), and `Err` (**the destination could not be read at all**, so nothing
/// is known about it). Until this was fixed the `Err` arm fell through into the same
/// `fs::remove_dir_all(dir)` as `Ok(None)`, which turned any transient read failure of
/// `_manifest.json` — a permission error, an antivirus lock, a Windows sharing violation, all
/// routine on this project's primary target — into the deletion of a perfectly valid corpus that
/// another process was, at that moment, reading its 10,000 files out of. Worse, a *published*
/// corpus could be destroyed by its own publisher: the loop's next iteration re-read `dir`, and
/// an `Err` there deleted what the previous iteration's `rename` had just successfully put in
/// place, with `tmp_dir` already consumed by that rename and so nothing left to re-publish from.
///
/// So an `Err` readback now clears nothing, renames nothing, and simply retries. A destination
/// that keeps failing to read is reported as an error (with that read failure named) rather than
/// resolved by deleting it: this cache holds regenerable content, but *this* caller is not the
/// only reader of it, and a corpus that can be rebuilt in seconds is still not one to delete out
/// from under a concurrent reader on no evidence. A genuinely stale destination reads back as
/// `Ok(None)` — the self-healing path the CI failure this retry loop exists for actually took —
/// and is still cleared exactly as before.
///
/// The loop also stops as soon as a `rename` reports success, rather than looping back around to
/// re-derive that fact from a second read of `dir`.
fn publish_built_corpus(
    tmp_dir: &Path,
    dir: &Path,
    seed: u64,
    key: &str,
) -> io::Result<LibraryCorpus> {
    // Diagnostics only -- captures the *last* attempt's state so a persistent (not transient)
    // failure reports something actionable instead of the same opaque message every time. Not
    // load-bearing for the retry logic itself.
    let mut last_rename_err: Option<io::Error> = None;
    let mut last_readback_err: Option<io::Error> = None;
    let mut published = false;

    for attempt in 0..MAX_PUBLISH_ATTEMPTS {
        match try_load_cached(dir, seed, key) {
            Ok(Some(corpus)) => {
                // A valid corpus already sits at `dir` -- ours from an earlier attempt in this
                // same loop, or a genuine other winner's. Either way, use it.
                let _ = fs::remove_dir_all(tmp_dir);
                return Ok(corpus);
            }
            Ok(None) => {
                last_readback_err = None;
                // Nothing valid at `dir`, and that is *known*, not assumed. Clear whatever is
                // there -- absent, corrupt, or stale -- before trying to claim the path, so a
                // non-empty-but-invalid destination can never make every future `rename` fail
                // forever. A `NotFound` error here (the ordinary case: nothing to clear) is
                // expected and ignored.
                let _ = fs::remove_dir_all(dir);
                match fs::rename(tmp_dir, dir) {
                    Ok(()) => {
                        published = true;
                        break;
                    }
                    Err(e) => last_rename_err = Some(e),
                }
            }
            // Unreadable, so unknown: never destroy what could not be inspected. Just wait and
            // look again.
            Err(e) => last_readback_err = Some(e),
        }

        if attempt + 1 < MAX_PUBLISH_ATTEMPTS {
            std::thread::sleep(PUBLISH_RETRY_BACKOFF);
        }
    }

    // Read back what is now at `dir`: after a successful `rename` this is our own publish, and
    // otherwise it is a last chance for a concurrent winner's to have become visible.
    let readback = try_load_cached(dir, seed, key);
    let _ = fs::remove_dir_all(tmp_dir);
    match readback {
        Ok(Some(corpus)) => return Ok(corpus),
        Ok(None) => {}
        Err(e) => last_readback_err = Some(e),
    }

    if published {
        return Err(io::Error::other(format!(
            "corpus directory {} was published but did not read back as a valid corpus (last \
             try_load_cached error: {last_readback_err:?})",
            dir.display()
        )));
    }
    Err(io::Error::other(format!(
        "could not publish the corpus to {} after {MAX_PUBLISH_ATTEMPTS} attempts; the \
         destination was left untouched if it could not be read (last rename error: \
         {last_rename_err:?}; last try_load_cached error: {last_readback_err:?})",
        dir.display()
    )))
}

/// Actually writes [`TOTAL_COUNT`] fixture files (plus the manifest) into `root`, which must
/// already exist and be empty. See the module doc comment for the tree shape and per-kind
/// generation strategy.
fn build_corpus_into(root: &Path, seed: u64, key: &str) -> io::Result<()> {
    let base_model = base_nam_model(seed);
    let mut manifest_entries = Vec::with_capacity(TOTAL_COUNT);

    let mut global_index: u64 = 0;
    for bank in 0..BANKS {
        for folder in 0..FOLDERS_PER_BANK {
            let (bank_name, folder_name) = leaf_dir_name(bank, folder);
            let leaf_dir = root.join(&bank_name).join(&folder_name);
            fs::create_dir_all(&leaf_dir)?;

            for slot in 0..FILES_PER_LEAF {
                let per_file_seed = derive_seed(seed, global_index);
                let (file_name, kind, bytes) = if slot < IR_SLOTS_PER_LEAF {
                    let name = format!("ir_{slot:02}.wav");
                    let bytes = ir_variant_bytes(per_file_seed);
                    (name, EntryKind::Ir, bytes)
                } else {
                    let name = format!("nam_{slot:02}.nam");
                    let label = format!("{bank_name}/{folder_name}/{name}#{per_file_seed:016x}");
                    let bytes = nam_variant_bytes(&base_model, &label);
                    (name, EntryKind::Nam, bytes)
                };

                let path = leaf_dir.join(&file_name);
                fs::write(&path, &bytes)?;

                let hash = ContentHash::of(&bytes);
                let rel_path = format!("{bank_name}/{folder_name}/{file_name}");
                manifest_entries.push(ManifestEntry {
                    rel_path,
                    kind,
                    hash: hash.to_string(),
                });

                global_index += 1;
            }
        }
    }

    let manifest = Manifest {
        generator_version: GENERATOR_VERSION,
        key: key.to_string(),
        seed,
        entries: manifest_entries,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .expect("Manifest always serializes (plain strings and integers only)");
    fs::write(root.join(MANIFEST_FILE_NAME), manifest_bytes)?;

    Ok(())
}

/// Generates a small (`MUTABLE_IR_COUNT` + `MUTABLE_NAM_COUNT` files), freshly-built,
/// explicitly mutable fixture into `dir` for FR-LIB-070 ("files that change/disappear/appear
/// mid-session"). `dir` must exist (or be creatable via [`fs::create_dir_all`]) and be a
/// directory the caller owns for the duration of its test — unlike
/// [`generate_shared_corpus`]'s tree, this is never cached or reused across calls: every call
/// regenerates from scratch (same `seed` still produces the same *initial* content, but the
/// directory itself is not a shared, read-only cache entry the way the big corpus is), so a test
/// is free to mutate, delete, or add files under `root` afterward without endangering any other
/// test's fixture.
pub fn mutable_probe_set(dir: &Path, seed: u64) -> io::Result<MutableProbeSet> {
    fs::create_dir_all(dir)?;
    let base_model = base_nam_model(seed);
    let mut entries = Vec::with_capacity(MUTABLE_IR_COUNT + MUTABLE_NAM_COUNT);

    for i in 0..MUTABLE_IR_COUNT {
        let per_file_seed = derive_seed(seed, i as u64);
        let bytes = ir_variant_bytes(per_file_seed);
        let file_name = format!("ir_{i:02}.wav");
        let path = dir.join(&file_name);
        fs::write(&path, &bytes)?;
        entries.push(LibraryEntry {
            path,
            kind: EntryKind::Ir,
            content_hash: ContentHash::of(&bytes),
        });
    }
    for i in 0..MUTABLE_NAM_COUNT {
        let per_file_seed = derive_seed(seed, (MUTABLE_IR_COUNT + i) as u64);
        let label = format!("mutable_probe_set#{per_file_seed:016x}");
        let bytes = nam_variant_bytes(&base_model, &label);
        let file_name = format!("nam_{i:02}.nam");
        let path = dir.join(&file_name);
        fs::write(&path, &bytes)?;
        entries.push(LibraryEntry {
            path,
            kind: EntryKind::Nam,
            content_hash: ContentHash::of(&bytes),
        });
    }

    Ok(MutableProbeSet {
        root: dir.to_path_buf(),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A small seed used by every test below except the ones specifically about cross-seed
    /// differences, so tests share one cache entry instead of each cold-building their own.
    const TEST_SEED: u64 = 12_345;

    /// Determinism, checked against two *independent* builds — deliberately not two
    /// [`generate_shared_corpus`] calls. That version of this test (through M14) called the
    /// public entry point twice and compared the results, which cannot fail: the second call is a
    /// cache hit that re-reads the first call's own `_manifest.json`, so both sides of every
    /// assertion came from one build and genuine non-determinism anywhere in `derive_seed`,
    /// `ir_variant_bytes` or `nam_variant_bytes` would have shipped silently. This builds twice
    /// into two distinct roots via `build_corpus_into` (bypassing the cache entirely), then
    /// compares the manifests byte-for-byte *and* every generated file's bytes on disk.
    ///
    /// It uses its own seed rather than `TEST_SEED` so it never publishes into, reads from, or
    /// races the shared cache entry the rest of this module's tests share.
    #[test]
    fn generating_twice_with_the_same_seed_is_byte_identical() {
        const DETERMINISM_SEED: u64 = 24_680;
        let key = cache_key(DETERMINISM_SEED);
        let base = workspace_target_dir()
            .join("namir-fixtures-determinism-test")
            .join("generating_twice_with_the_same_seed_is_byte_identical");
        let _ = fs::remove_dir_all(&base);

        let roots = ["build_a", "build_b"].map(|name| base.join(name));
        for root in &roots {
            fs::create_dir_all(root).expect("create an independent build root");
            build_corpus_into(root, DETERMINISM_SEED, &key).expect("independent build");
        }
        let (a, b) = (&roots[0], &roots[1]);

        let manifest_a = fs::read(a.join(MANIFEST_FILE_NAME)).expect("build a's manifest");
        let manifest_b = fs::read(b.join(MANIFEST_FILE_NAME)).expect("build b's manifest");
        assert_eq!(
            manifest_a, manifest_b,
            "two independent builds of seed {DETERMINISM_SEED} disagree on their manifests"
        );

        // The manifest agreeing is necessary but not sufficient: it records hashes this generator
        // computed itself, so the files on disk are compared directly as well.
        let manifest: Manifest = serde_json::from_slice(&manifest_a).expect("manifest parses back");
        assert_eq!(manifest.entries.len(), TOTAL_COUNT);
        for entry in &manifest.entries {
            let bytes_a = fs::read(a.join(&entry.rel_path)).expect("build a's file");
            let bytes_b = fs::read(b.join(&entry.rel_path)).expect("build b's file");
            assert_eq!(
                bytes_a, bytes_b,
                "{} differs between two independent builds of the same seed",
                entry.rel_path
            );
            assert_eq!(
                ContentHash::of(&bytes_a).to_string(),
                entry.hash,
                "{}'s manifest hash does not match its bytes on disk",
                entry.rel_path
            );
        }

        let _ = fs::remove_dir_all(&base);
    }

    /// Regression test for the CI-only failure this crate's own dev sandbox could never
    /// reproduce (its cache was always either warm or cleanly absent): a *non-empty but invalid*
    /// directory already sitting at the publish destination -- content left by an earlier,
    /// interrupted build, simulated here directly rather than by racing real processes -- must not
    /// permanently block every future `rename` onto it. `build_and_publish_corpus` is expected to
    /// notice the existing entry is not a valid corpus, clear it, and publish successfully anyway.
    #[test]
    fn stale_invalid_content_at_the_destination_is_cleared_and_replaced() {
        const STALE_SEED: u64 = 99_991;
        let dir = cache_root().join(format!("lib-corpus-{}", cache_key(STALE_SEED)));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create a stale destination directory");
        fs::write(dir.join("leftover_junk.bin"), b"not a manifest, not empty")
            .expect("write stale content into the destination");

        let corpus =
            generate_shared_corpus(STALE_SEED).expect("should self-heal past the stale content");
        assert_eq!(corpus.entries.len(), TOTAL_COUNT);
        assert!(
            dir.join(MANIFEST_FILE_NAME).exists(),
            "a valid manifest should now be published at the destination"
        );
    }

    /// The other half of the publish step's failure handling, and the one the self-healing path
    /// above got wrong: a destination that **cannot be read** is not a destination that is known
    /// to be junk. `try_load_cached` returns `Err` for any non-`NotFound` I/O failure reading
    /// `_manifest.json` — a permission error, an antivirus lock, a Windows sharing violation —
    /// and the publish step used to feed that straight into the same `remove_dir_all(dir)` it
    /// uses for a stale destination, deleting a valid, published, possibly-being-read corpus on
    /// the strength of one failed `open`.
    ///
    /// The unreadable manifest here is a *directory* where the file belongs: `fs::read` fails on
    /// it with a non-`NotFound` error on every platform this project builds for (`IsADirectory`
    /// on Linux/macOS, `PermissionDenied` on Windows), which reproduces the failure shape exactly
    /// without permission games, an injected fault, or a real race.
    #[test]
    fn a_destination_that_cannot_be_read_is_never_deleted() {
        let base = workspace_target_dir()
            .join("namir-fixtures-publish-test")
            .join("a_destination_that_cannot_be_read_is_never_deleted");
        let _ = fs::remove_dir_all(&base);
        let dir = base.join("lib-corpus-unreadable");
        let tmp_dir = base.join("lib-corpus-unreadable.tmp-0");
        fs::create_dir_all(&dir).expect("create the destination");
        fs::create_dir_all(&tmp_dir).expect("create the source");
        fs::write(tmp_dir.join("payload.bin"), b"freshly built corpus").expect("write the source");

        // Stands in for the 10,000 files a published corpus holds, and for the caller that is
        // reading them while this publish attempt runs.
        let published = dir.join("published_file.bin");
        fs::write(&published, b"a valid corpus another process is mid-read of")
            .expect("write the published file");
        fs::create_dir_all(dir.join(MANIFEST_FILE_NAME)).expect("make the manifest unreadable");

        let err = publish_built_corpus(&tmp_dir, &dir, 4_242, "unreadable-destination")
            .expect_err("an unreadable destination cannot be published to");

        assert!(
            published.exists(),
            "the publish step deleted a corpus it could not read ({err})"
        );
        assert_eq!(
            fs::read(&published).expect("the published file is still readable"),
            b"a valid corpus another process is mid-read of",
            "the published corpus was replaced rather than left alone"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// The complement of the test above: a destination that reads back as *demonstrably* not a
    /// corpus (`Ok(None)`, not `Err`) is still cleared and claimed — the self-healing behaviour
    /// the retry loop exists for, checked here at the publish seam rather than through a full
    /// 10,000-file build.
    #[test]
    fn a_destination_that_reads_back_as_junk_is_still_cleared_and_claimed() {
        const JUNK_SEED: u64 = 4_243;
        let base = workspace_target_dir()
            .join("namir-fixtures-publish-test")
            .join("a_destination_that_reads_back_as_junk_is_still_cleared_and_claimed");
        let _ = fs::remove_dir_all(&base);
        let dir = base.join("lib-corpus-junk");
        let tmp_dir = base.join("lib-corpus-junk.tmp-0");
        let key = cache_key(JUNK_SEED);
        fs::create_dir_all(&dir).expect("create the destination");
        fs::create_dir_all(&tmp_dir).expect("create the source");
        fs::write(
            dir.join("leftover_junk.bin"),
            b"an interrupted build's leftovers",
        )
        .expect("write the stale destination");

        // A minimal but genuinely valid corpus in the source: one file plus a manifest naming it,
        // which is all `try_load_cached` reads back (it stats the first and last entry, not all
        // 10,000, by design).
        let bytes = b"published payload";
        fs::write(tmp_dir.join("only.bin"), bytes).expect("write the source file");
        let manifest = Manifest {
            generator_version: GENERATOR_VERSION,
            key: key.clone(),
            seed: JUNK_SEED,
            entries: (0..TOTAL_COUNT)
                .map(|_| ManifestEntry {
                    rel_path: "only.bin".to_string(),
                    kind: EntryKind::Ir,
                    hash: ContentHash::of(bytes).to_string(),
                })
                .collect(),
        };
        fs::write(
            tmp_dir.join(MANIFEST_FILE_NAME),
            serde_json::to_vec(&manifest).expect("manifest serializes"),
        )
        .expect("write the source manifest");

        let corpus = publish_built_corpus(&tmp_dir, &dir, JUNK_SEED, &key)
            .expect("a junk destination should be cleared and claimed");

        assert_eq!(corpus.root, dir);
        assert!(
            !dir.join("leftover_junk.bin").exists(),
            "the stale content should have been cleared"
        );
        assert!(
            dir.join("only.bin").exists(),
            "our build should be at `dir`"
        );
        assert!(!tmp_dir.exists(), "the temp directory should be consumed");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_cache_is_actually_reused_on_a_second_call() {
        // Deliberately not a wall-clock timing assertion: on a shared/possibly-antivirus-scanned
        // Windows disk, even a genuine cache *hit* (one small manifest read plus two `fs::metadata`
        // calls) can occasionally take seconds under concurrent I/O load from other tests writing
        // thousands of files at the same time -- a timing threshold cannot distinguish that from an
        // actual rebuild. `build_attempts_for_seed` is incremented exactly where a real rebuild for
        // that seed begins, so it answers the question directly and deterministically instead.
        let _ = generate_shared_corpus(TEST_SEED).expect("warm the cache");
        let after_warm = build_attempts_for_seed(TEST_SEED);

        let corpus = generate_shared_corpus(TEST_SEED).expect("cache hit");
        let after_second_call = build_attempts_for_seed(TEST_SEED);

        assert_eq!(corpus.entries.len(), TOTAL_COUNT);
        assert_eq!(
            after_second_call, after_warm,
            "the second call attempted a full rebuild instead of reusing the cache"
        );
    }

    #[test]
    fn different_seeds_produce_different_cache_directories() {
        let a = generate_shared_corpus(TEST_SEED).expect("seed a");
        let b = generate_shared_corpus(TEST_SEED + 1).expect("seed b");
        assert_ne!(a.root, b.root);
    }

    /// The corpus is `nam::generate`'s and `ir::decaying_noise`'s output, so a change to either
    /// of those generators has to change this module's cache key — otherwise a warm cache serves
    /// the previous generator's files forever, and nothing re-validates a cached corpus to catch
    /// it. Checked by varying each generator's version through `cache_signature` (the same
    /// function `cache_key` hashes, taking the versions as arguments precisely so this test can
    /// move them) rather than by asserting the signature contains a particular substring: what
    /// matters is that the *key* changes, not how the version is spelled inside it.
    #[test]
    fn bumping_either_generators_version_changes_the_cache_key() {
        let key_of = |s: &str| ContentHash::of(s.as_bytes()).to_string()[..16].to_string();
        let (nam_v, ir_v) = (nam::GENERATOR_VERSION, ir::GENERATOR_VERSION);
        let current = cache_signature(TEST_SEED, nam_v, ir_v);

        assert_eq!(
            key_of(&current),
            cache_key(TEST_SEED),
            "cache_key must hash exactly the signature this test varies"
        );
        assert_ne!(
            key_of(&current),
            key_of(&cache_signature(TEST_SEED, nam_v + 1, ir_v)),
            "a bump to nam::GENERATOR_VERSION left the cache key unchanged"
        );
        assert_ne!(
            key_of(&current),
            key_of(&cache_signature(TEST_SEED, nam_v, ir_v + 1)),
            "a bump to ir::GENERATOR_VERSION left the cache key unchanged"
        );
    }

    /// The mechanical half of the same guard: the `.nam` shape the corpus is built from, *and*
    /// the weights that shape initialises to, are folded into the signature as data — so changing
    /// the topology, the weight layout or an initialisation scale invalidates the cache even if
    /// nobody remembers to bump `nam::GENERATOR_VERSION`. That is the issue's own scenario
    /// ("change the WaveNet weight layout, and a warm cache keeps serving the old layout")
    /// answered without a hand-maintained constant.
    #[test]
    fn the_cache_key_covers_the_nam_shape_the_corpus_is_built_from() {
        let signature = cache_signature(TEST_SEED, nam::GENERATOR_VERSION, ir::GENERATOR_VERSION);
        let shape = serde_json::to_string(&nam::shape_signature(CORPUS_NAM_SHAPE)).unwrap();
        assert!(
            signature.contains(&shape),
            "the corpus's own shape is missing from the cache signature: {signature}"
        );
        let other = serde_json::to_string(&nam::shape_signature(WaveNetShape::Lite)).unwrap();
        assert_ne!(shape, other, "two shapes must serialize differently");

        assert!(
            signature.contains(&nam_weight_fingerprint(TEST_SEED)),
            "the corpus's initialised weights are missing from the cache signature: {signature}"
        );
        assert_ne!(
            nam_weight_fingerprint(TEST_SEED),
            nam_weight_fingerprint(TEST_SEED + 1),
            "the weight fingerprint must depend on the corpus seed"
        );
    }

    #[test]
    fn shared_corpus_has_the_expected_counts_and_tree_shape() {
        let corpus = generate_shared_corpus(TEST_SEED).expect("generate");
        assert_eq!(corpus.entries.len(), TOTAL_COUNT);

        let ir_count = corpus
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Ir)
            .count();
        let nam_count = corpus
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Nam)
            .count();
        assert_eq!(ir_count, IR_COUNT);
        assert_eq!(nam_count, NAM_COUNT);

        // Every entry must live under a bank_NN/folder_NN nested path, not flat in the root.
        for entry in &corpus.entries {
            let rel = entry
                .path
                .strip_prefix(&corpus.root)
                .expect("entry path is under the corpus root");
            let components: Vec<_> = rel.components().collect();
            assert_eq!(
                components.len(),
                3,
                "expected bank/folder/file, got {rel:?}"
            );
        }
    }

    /// Uniqueness, checked by re-hashing the bytes on disk rather than by trusting the manifest.
    /// The manifest's hashes are this generator's own claims about files it wrote; hashing what
    /// is actually there is what makes this a check on the *corpus* instead of a check on the
    /// manifest's internal consistency (through M14 it was the latter).
    #[test]
    fn every_content_hash_in_the_shared_corpus_is_unique() {
        let corpus = generate_shared_corpus(TEST_SEED).expect("generate");
        let mut unique: HashSet<ContentHash> = HashSet::with_capacity(corpus.entries.len());
        for entry in &corpus.entries {
            let bytes = fs::read(&entry.path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", entry.path.display()));
            let hash = ContentHash::of(&bytes);
            assert_eq!(
                hash,
                entry.content_hash,
                "{}: the manifest's hash does not match the file's actual bytes",
                entry.path.display()
            );
            assert!(
                unique.insert(hash),
                "{} shares a content hash with an earlier file",
                entry.path.display()
            );
        }
        assert_eq!(
            unique.len(),
            corpus.entries.len(),
            "expected every one of {} files to have a distinct content hash",
            corpus.entries.len()
        );
    }

    /// The whole reason this corpus exists: every generated file must be genuinely valid per its
    /// own crate's real parser, not merely well-formed-looking bytes.
    #[test]
    fn every_generated_file_round_trips_through_its_real_probe() {
        let corpus = generate_shared_corpus(TEST_SEED).expect("generate");
        for entry in &corpus.entries {
            let bytes = fs::read(&entry.path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", entry.path.display()));
            match entry.kind {
                EntryKind::Nam => {
                    namir_nam::probe_metadata(&bytes).unwrap_or_else(|e| {
                        panic!("{} failed probe_metadata: {e:?}", entry.path.display())
                    });
                }
                EntryKind::Ir => {
                    namir_ir::probe_wav(&bytes).unwrap_or_else(|e| {
                        panic!("{} failed probe_wav: {e:?}", entry.path.display())
                    });
                }
            }
        }
    }

    #[test]
    fn mutable_probe_set_is_small_and_distinct_from_the_shared_corpus() {
        let dir = workspace_target_dir()
            .join("namir-fixtures-mutable-test")
            .join("mutable_probe_set_is_small_and_distinct_from_the_shared_corpus");
        let _ = fs::remove_dir_all(&dir);

        let set = mutable_probe_set(&dir, 7).expect("generate mutable set");
        assert_eq!(set.entries.len(), MUTABLE_IR_COUNT + MUTABLE_NAM_COUNT);
        assert!(set.entries.len() <= 20);
        assert!(
            set.root
                != generate_shared_corpus(7)
                    .expect("shared corpus for seed 7")
                    .root
        );

        for entry in &set.entries {
            let bytes = fs::read(&entry.path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", entry.path.display()));
            match entry.kind {
                EntryKind::Nam => {
                    namir_nam::probe_metadata(&bytes).expect("mutable NAM entry probes");
                }
                EntryKind::Ir => {
                    namir_ir::probe_wav(&bytes).expect("mutable IR entry probes");
                }
            }
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mutable_probe_set_can_actually_be_mutated_after_generation() {
        // FR-LIB-070's whole point: the caller must be free to delete/modify/add files here
        // without touching anything else. This is a smoke test that nothing about the fixture
        // itself (permissions, an open file handle, etc.) prevents that.
        let dir = workspace_target_dir()
            .join("namir-fixtures-mutable-test")
            .join("mutable_probe_set_can_actually_be_mutated_after_generation");
        let _ = fs::remove_dir_all(&dir);
        let set = mutable_probe_set(&dir, 99).expect("generate");

        let victim = &set.entries[0].path;
        fs::remove_file(victim).expect("delete a fixture file after generation");
        assert!(!victim.exists());

        let new_file = dir.join("added_after_generation.wav");
        let bytes = ir::to_mono_wav_bytes(&ir::decaying_noise(64, 1, 10.0), 48_000);
        fs::write(&new_file, &bytes).expect("add a new file after generation");
        assert!(new_file.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn derive_seed_is_deterministic_and_varies_by_index() {
        let a = derive_seed(1, 0);
        let b = derive_seed(1, 0);
        let c = derive_seed(1, 1);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
