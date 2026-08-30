//! Background jobs this crate spawns on [`namir_worker::pool::ThreadPool`] — every place a
//! [`namir_ui::UiIntent`] needs file I/O, parsing, or a blocking handover submit, none of which
//! may run on the GUI thread ([`crate::ui_host`]'s own contract) or the audio thread (FR-CLAP-130).
//!
//! Every job here follows the same shape: read/prepare off-thread, then take
//! `SharedInner`'s instance lock only for the (already-fast) submit step, exactly the ordering
//! `namir_worker::Instance::load`'s own doc comment requires of its callers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use namir_core::ContentHash;
use namir_state::{FileRef, RelPath};
use namir_worker::{LoadSource, Target};

use crate::shared::SharedInner;

/// FR-UI-050-adjacent (`namir-library`'s FR-LIB-060 "select" gesture): the user double-clicked a
/// library entry. Reads the file, determines its target from the library index's own recorded
/// [`namir_library::ItemKind`], and loads it through the normal [`namir_worker::Instance::load`]
/// path — the same crossfaded handover a host-driven preset recall uses, so there is nothing
/// library-specific about the load itself, only about how the path was chosen.
pub(crate) fn spawn_load_library_entry(shared: Arc<SharedInner>, path: PathBuf) {
    let inner = Arc::clone(&shared);
    shared.pool.spawn(move || {
        let shared = inner;
        let target = match library_target(&shared, &path) {
            Some(t) => t,
            None => {
                shared.push_notice(
                    crate::error_codes::LIBRARY_UNAVAILABLE,
                    format!("{} is not a recognised library entry", path.display()),
                );
                return;
            }
        };

        // **`read_file_bounded`, not `std::fs::read` (#145).** This job reads the bytes itself
        // because it needs their `ContentHash` for `record_reference` below (FR-STATE-060/-070),
        // which `LoadSource::File` never hands back -- but `LoadSource::File` was also the only
        // route through NFR-SEC-020's ceiling and through the `is_file()` check issue #107 added,
        // so taking the bytes this way silently dropped both. The bound belongs to the read, not
        // to the `LoadSource`; `namir_worker::read_file_bounded` is `pub` for this caller.
        let bytes = match namir_worker::read_file_bounded(&path) {
            Ok(b) => b,
            // Whole, with its own catalogue id: `read_file_bounded` already distinguishes
            // unreadable from too-large from not-a-regular-file, and collapsing the three back
            // into `FILE_UNREADABLE` would undo that (issue #39's rule at this boundary).
            Err(e) => {
                shared.push_notice(e.code, e.detail);
                return;
            }
        };
        let hash = ContentHash::of(&bytes);
        let display_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        let outcome = shared.with_instance(|instance| {
            instance.load(
                &shared.cache,
                target,
                LoadSource::Bytes(Arc::from(bytes.into_boxed_slice())),
            )
        });

        let Some(outcome) = outcome else {
            // No engine is active yet (plugin not activated) — nothing to load into. The
            // reference is still recorded so the *next* activation's replay (see `crate::audio`)
            // picks it up, matching how a host `state` load behaves before the first `activate`.
            record_reference(&shared, target, hash, display_name, &path);
            return;
        };

        match outcome.result {
            namir_worker::JobResult::Loaded { warning, .. } => {
                record_reference(&shared, target, hash, display_name, &path);
                if let Some(w) = warning {
                    shared.push_notice(w.code, w.detail);
                }
            }
            namir_worker::JobResult::Failed(e) => {
                shared.push_notice(e.code, e.detail);
            }
            namir_worker::JobResult::NotDelivered(e) => {
                shared.push_notice(e.code, e.detail);
            }
            namir_worker::JobResult::Unloaded { .. } => {}
        }
    });
}

fn record_reference(
    shared: &SharedInner,
    target: Target,
    hash: ContentHash,
    display_name: String,
    path: &Path,
) {
    let reference = FileRef {
        hash,
        // FR-STATE-070's **first** resolution candidate, and the one that makes a preset portable
        // between two machines whose library sits at two different absolute paths. It used to be
        // a hardcoded `None` here on the grounds that `namir_library::LibraryEntry` hands back
        // only the resolved absolute path -- but the root it was found under is not a mystery,
        // it is one of the roots this instance's own `LibraryService` is configured with, and
        // stripping it off is all "library-relative" means (issue #96's other half).
        library_relative: library_relative_reference(shared, path),
        absolute: Some(path.to_string_lossy().into_owned()),
        display_name,
        embedded: None,
    };
    match target {
        Target::Nam => shared.set_nam_ref(Some(reference)),
        Target::Ir => shared.set_ir_ref(Some(reference)),
    }
}

/// `path` expressed relative to whichever of this instance's library roots contains it, or `None`
/// if it lies outside all of them (a file the user loaded from somewhere else entirely, for which
/// there is no library-relative form to record).
///
/// The first containing root wins, matching the order `namir_library::LibraryResolver` itself
/// tries them in, so a path recorded here resolves back to the same file it came from.
fn library_relative_reference(shared: &SharedInner, path: &Path) -> Option<RelPath> {
    shared.library_roots().iter().find_map(|root| {
        let relative = path.strip_prefix(root).ok()?;
        RelPath::from_relative_path(relative).ok()
    })
}

/// FR-STATE-030's save half: write this instance's current state to `<preset dir>/<name>`.
///
/// On the pool, not the GUI thread, for the same reason every other job in this module is: it
/// creates a directory, serialises a document and writes a file. Every failure it can meet — no
/// preset directory on this system, a name that cannot be a filename, a document over
/// NFR-SEC-020's ceiling, a write the OS refused — becomes an FR-UI-070 notice rather than a
/// silently dropped click.
pub(crate) fn spawn_save_preset(shared: Arc<SharedInner>, name: String) {
    let inner = Arc::clone(&shared);
    shared.pool.spawn(move || {
        let shared = inner;
        let Some(dir) = crate::presets::preset_dir() else {
            shared.push_notice(
                crate::error_codes::PRESET_UNAVAILABLE,
                "this system has no per-user configuration directory to keep presets in",
            );
            return;
        };
        let Some(path) = crate::presets::preset_path(&dir, &name) else {
            shared.push_notice(
                crate::error_codes::PRESET_UNAVAILABLE,
                format!("{name:?} is not a usable preset name"),
            );
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            shared.push_notice(
                crate::error_codes::PRESET_IO_FAILED,
                format!("{}: {e}", dir.display()),
            );
            return;
        }

        // The same document a host `state` save produces -- D-11.2's write-back included, so a
        // preset written by a build that did not understand every section still carries them --
        // and the same *checked* writer, so a preset this build cannot read back is never written.
        let document = shared.snapshot_state().write_onto(&shared.last_document());
        let bytes = match document.try_to_pretty_bytes() {
            Ok(bytes) => bytes,
            Err(e) => {
                shared.push_notice(e.code, e.detail);
                return;
            }
        };
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                shared.set_last_document(document);
                shared.mark_clean();
                shared.mark_presets_stale();
            }
            Err(e) => shared.push_notice(
                crate::error_codes::PRESET_IO_FAILED,
                format!("{}: {e}", path.display()),
            ),
        }
    });
}

/// FR-STATE-030's recall half: load the preset at `path` onto this instance.
///
/// Follows the host-driven `state` load exactly (`crate::state_ext`) — the same
/// `adopt_document_bytes`, the same `spawn_recall` afterwards — because a `.namirpreset` and a
/// host's state blob are the same document. The two differences are both about who is asking:
/// the bytes come from a file rather than a `clap_istream`, and the host has to be told its
/// cached parameter values are stale, which a GUI-thread caller cannot do itself (see
/// `crate::main_thread`'s `notify_params_changed`).
pub(crate) fn spawn_recall_preset(shared: Arc<SharedInner>, path: PathBuf) {
    let inner = Arc::clone(&shared);
    shared.pool.spawn(move || {
        let shared = inner;
        // Bounded, like every other read either shell makes off a user-chosen path (#145). A
        // `.namirpreset` path is exactly as untrusted as a library entry: `namir_state::Document::
        // parse` does enforce `MAX_DOCUMENT_BYTES`, but only once the whole file is already in
        // memory -- the allocation NFR-SEC-020 exists to refuse -- and it says nothing at all
        // about a path that is not a regular file, where a bare `std::fs::read` blocks this pool
        // thread for as long as no writer appears.
        let bytes = match namir_worker::read_file_bounded(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                shared.push_notice(e.code, e.detail);
                return;
            }
        };
        if let Err(e) = crate::state_ext::adopt_document_bytes(&shared, &bytes) {
            shared.push_notice(e.code, e.detail);
            return;
        }
        // The mirror now holds values the host has never seen. It cannot be told from here --
        // this is a pool thread and `HostParams::rescan` is `[main-thread]` -- so the request is
        // parked for whichever main-thread callback comes next.
        shared
            .params_rescan_pending
            .store(true, std::sync::atomic::Ordering::Release);
        spawn_recall(Arc::clone(&shared));
    });
}

/// FR-STATE-030/050's replay: whatever `shared`'s `ParamMirror`/resource references currently
/// stand for is pushed onto its live engine (if any — see `SharedInner::with_instance`'s own
/// doc comment for why "no engine yet" is a normal, non-error outcome here). Shared between
/// `crate::audio::NamirAudioProcessor::activate` (every activation replays the instance's
/// current desired state onto the freshly built engine — FR-CLAP-080's sample-rate/block-size
/// change path) and `crate::state_ext`'s host-driven `state` load (a load that arrives while
/// already active must reach the live engine too, not just the mirror).
///
/// **Not RT-safe, by design** — like `Instance::recall` itself, this reads files, parses them,
/// allocates, and may block waiting for the audio thread to make room; both call sites dispatch
/// it to [`namir_worker::pool::ThreadPool`] rather than running it inline.
pub(crate) fn spawn_recall(shared: Arc<SharedInner>) {
    let inner = Arc::clone(&shared);
    shared.pool.spawn(move || {
        let shared = inner;
        let state = shared.snapshot_state();
        if state.nam.is_none() && state.ir.is_none() {
            return; // Nothing to replay; the common case for a brand-new instance.
        }
        // **The index has to be *there* before a resolver is built over it (#145).** M14 deferred
        // the load, so `library_snapshot()` is an empty `Index` for the first fraction of a second
        // of an instance's life -- and both callers of this function, a host `state` load and
        // every `activate`, land inside that window. A resolver over an empty index cannot try
        // FR-STATE-070's `library_relative` or hash candidates at all, so a preset whose `.nam`
        // was renamed or moved *inside* the library reported `state.reference.not_found` on
        // project load, and `namir_ui::push_deduplicated` then made the notice stick. This is the
        // same block `LibraryService::start_scan` performs inside its own pool job, for the same
        // reason and with the same cost: once per process, on a thread that is allowed to block.
        // `namir-app` has always done it (`crate::app`), which made this an FR-CFG-020 divergence
        // as well.
        if let Some(loader) = shared.library_loader() {
            loader.ensure_loaded();
        }
        let index = shared.library_snapshot().index;
        // **Issue #96: the real roots, off the `LibraryService` this instance already holds.**
        // This was a hardcoded `Vec::new()`, so `LibraryResolver::resolve_library_relative` could
        // never succeed here and FR-STATE-070's *first* resolution candidate was dead in the
        // plugin: a preset carrying a `library_relative` reference resolved in `namir-app` (which
        // passes `LibraryService::roots()`) and fell through to hash search -- or reported Missing
        // -- in `namir-clap`. That is an FR-CFG-020 parity divergence, and the same "the two
        // shells' library wiring drifted apart" failure `crate::shared`'s own module doc comment
        // records one layer up for the bootstrap itself.
        let roots = shared.library_roots();
        let resolver = namir_library::LibraryResolver::new(&index, &roots);
        shared.with_instance(|instance| {
            let outcome = instance.recall(&shared.cache, &state, &resolver);
            for recall in [&outcome.nam, &outcome.ir] {
                if let namir_worker::recall::ResourceRecall::Missing { missing, .. } = recall {
                    // **Both triggers reporting is correct; both being *shown* was not** (issue
                    // #43). This function is deliberately called from two places -- a host `state`
                    // load (`crate::state_ext`) and every activation (`crate::audio`) -- and the
                    // replay itself is idempotent, but its reporting was not: one deleted `.nam`
                    // produced two indistinguishable `state.reference.not_found` notices in the
                    // 2026-08-27 manual run's step 15. `SharedInner::push_notice` now goes through
                    // `namir_ui::push_deduplicated`, so the second is folded into the first here
                    // and at every other push site in either shell, rather than being suppressed
                    // by a special case at this one.
                    let warning = missing.warning();
                    shared.push_notice(warning.code, warning.detail);
                }
            }
        });
    });
}

/// Which stage the library entry at `path` belongs to, per the index's own recorded
/// [`namir_library::ItemKind`], or `None` if the library does not know this path.
///
/// **Blocks for the deferred index load first (#145)** — see `spawn_recall` above for the full
/// argument. Without it a double-click in the first moments of an instance's life consulted an
/// empty index and was reported back to the user as "not a recognised library entry". Called only
/// from `spawn_load_library_entry`'s pool job, which is allowed to block.
fn library_target(shared: &SharedInner, path: &Path) -> Option<Target> {
    if let Some(loader) = shared.library_loader() {
        loader.ensure_loaded();
    }
    let index = shared.library_snapshot().index;
    let entry = index.get(path)?;
    Some(match entry.kind {
        namir_library::ItemKind::Nam => Target::Nam,
        namir_library::ItemKind::Ir => Target::Ir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::SharedInner;

    fn temp_config_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "namir-clap-worker-jobs-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// **Issue #96's other half.** A file loaded from inside a library root is recorded with
    /// FR-STATE-070's *first* resolution candidate, not only its absolute path — that is the one
    /// field that survives the project being opened on another machine whose library sits
    /// somewhere else.
    #[test]
    fn a_file_under_a_library_root_is_recorded_library_relative() {
        let config = temp_config_dir("relative");
        let shared = SharedInner::new_at(&config);
        let path = config.join("Library").join("marshall").join("jcm800.nam");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{}").unwrap();

        record_reference(
            &shared,
            Target::Nam,
            ContentHash::of(b"{}"),
            "jcm800.nam".to_string(),
            &path,
        );

        let reference = shared.nam_ref().expect("the reference must be recorded");
        assert_eq!(
            reference.library_relative.as_ref().map(|r| r.as_str()),
            Some("marshall/jcm800.nam"),
            "a hardcoded None here is why a preset's library_relative reference resolved in \
             namir-app and missed in the plugin"
        );
        assert_eq!(
            reference.absolute,
            Some(path.to_string_lossy().into_owned())
        );

        let _ = std::fs::remove_dir_all(&config);
    }

    /// A file from outside every root has no library-relative form, and inventing one would be
    /// worse than recording none: it would resolve, on another machine, to a different file.
    #[test]
    fn a_file_outside_every_root_records_no_library_relative_form() {
        let config = temp_config_dir("outside");
        let shared = SharedInner::new_at(&config);
        let path = config.join("elsewhere.nam");
        std::fs::write(&path, b"{}").unwrap();

        record_reference(
            &shared,
            Target::Ir,
            ContentHash::of(b"{}"),
            "elsewhere.nam".to_string(),
            &path,
        );

        let reference = shared.ir_ref().expect("the reference must be recorded");
        assert!(reference.library_relative.is_none());
        assert!(reference.absolute.is_some());

        let _ = std::fs::remove_dir_all(&config);
    }

    /// A saved index at the location [`namir_worker::library::LibraryService::open_at`] uses,
    /// holding `count` entries, the last of which is `path`.
    ///
    /// **`count` is FR-LIB-020's own stated scale, and it is load-bearing rather than decorative.**
    /// M14 made the index load deferred: `LibraryService::open` returns before the file has been
    /// read, and `snapshot()` hands back an empty `Index` until a loader thread lands. A test that
    /// wrote a three-entry index would race that thread and pass whether or not the code under
    /// test blocks for the load. At 10 000 entries the parse is ~161 ms on the reference machine
    /// (`namir_worker::library`'s own `Stamp` doc comment), against the microseconds between
    /// `SharedInner::new_at` returning and the assertion below — so "the index was still loading"
    /// is established by a five-order-of-magnitude margin rather than by hope.
    fn saved_index_of(config: &Path, count: usize, path: &Path, hash: ContentHash) {
        let mut index = namir_library::Index::empty();
        for i in 0..count.saturating_sub(1) {
            index.upsert(namir_library::LibraryEntry {
                path: config.join("Library").join(format!("filler-{i}.nam")),
                kind: namir_library::ItemKind::Nam,
                size: 2,
                mtime: namir_library::FileTime::now(),
                hash: Some(ContentHash::of(format!("filler-{i}").as_bytes())),
                metadata: namir_library::ItemMetadata::None,
                origin: namir_library::Origin::Local,
            });
        }
        index.upsert(namir_library::LibraryEntry {
            path: path.to_path_buf(),
            kind: namir_library::ItemKind::Nam,
            size: 2,
            mtime: namir_library::FileTime::now(),
            hash: Some(hash),
            metadata: namir_library::ItemMetadata::None,
            origin: namir_library::Origin::Local,
        });
        let (store, _, _) = namir_library::IndexStore::open(config.join("library-index.json"));
        store.save_atomic(&index).unwrap();
    }

    /// FR-LIB-020's scale, and the reason this figure is here rather than inline — see
    /// [`saved_index_of`].
    const INDEXED_ENTRIES: usize = 10_000;

    /// A sparse file one byte past NFR-SEC-020's ceiling — `set_len`, not 256 MiB of writes, since
    /// the bound is checked against the file's length.
    fn oversized_file(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        file.set_len(namir_worker::MAX_FILE_BYTES as u64 + 1)
            .unwrap();
    }

    /// Waits for this instance to raise a notice, and returns the first one's catalogue id. Every
    /// job in this module runs on the pool, so a test has to wait rather than assume.
    fn wait_for_notice(shared: &SharedInner) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Some(notice) = shared.notices().first() {
                return notice.code.id.to_string();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("this instance raised no notice within the deadline");
    }

    /// **Issue #145, finding 1.** `library_target` consults the library index from a pool job, and
    /// M14 made that index arrive asynchronously — so it has to block for the load the way
    /// `LibraryService::start_scan`'s own job does, rather than reading whatever `snapshot()`
    /// happens to hold. Without that, a double-click on a library entry in the first fraction of a
    /// second of an instance's life resolves against an empty index and is reported back as "not a
    /// recognised library entry".
    #[test]
    fn a_library_entry_resolves_while_the_deferred_index_load_is_still_in_flight() {
        let config = temp_config_dir("deferred_target");
        let path = config.join("Library").join("jcm800.nam");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{}").unwrap();
        saved_index_of(&config, INDEXED_ENTRIES, &path, ContentHash::of(b"{}"));

        let shared = SharedInner::new_at(&config);
        let target = library_target(&shared, &path);

        assert_eq!(
            target,
            Some(Target::Nam),
            "the index has this entry; a job that reads the not-yet-loaded snapshot instead \
             reports the user's own library file as unrecognised"
        );

        shared.shutdown_workers();
        let _ = std::fs::remove_dir_all(&config);
    }

    /// **Issue #145, finding 2**, the library-entry half. This job reads the file itself — it needs
    /// the bytes' `ContentHash` for FR-STATE-060/-070's `FileRef`, which `LoadSource::File` never
    /// hands back — and that read has to be `namir_worker::read_file_bounded`. With a bare
    /// `std::fs::read` an oversized file is pulled whole into memory and only then refused by a
    /// parser, which is NFR-SEC-020's ceiling not being applied at all.
    #[test]
    fn an_oversized_library_entry_is_refused_before_it_is_read() {
        let config = temp_config_dir("oversized_entry");
        let path = config.join("Library").join("huge.nam");
        oversized_file(&path);
        saved_index_of(&config, 1, &path, ContentHash::of(b""));

        let shared = Arc::new(SharedInner::new_at(&config));
        spawn_load_library_entry(Arc::clone(&shared), path);

        assert_eq!(
            wait_for_notice(&shared),
            namir_worker::error_codes::FILE_TOO_LARGE.id,
            "an oversized library entry must be refused by NFR-SEC-020's ceiling, not read whole \
             into memory and then refused by a parser"
        );
        // The pre-fix behaviour raised no notice at all: with no live `Instance` the job took its
        // "nothing to load into yet" branch and recorded the 256 MiB file as this instance's model
        // reference, so the next save would have written a preset naming it.
        assert!(
            shared.nam_ref().is_none(),
            "a refused file must not be recorded as this instance's model reference"
        );

        shared.shutdown_workers();
        let _ = std::fs::remove_dir_all(&config);
    }

    /// **Issue #145, finding 2**, the preset half: a `.namirpreset` path the user chose is exactly
    /// as untrusted as a library entry. `namir_state::Document::parse` does enforce
    /// `MAX_DOCUMENT_BYTES`, but only once the whole file is already in memory — the allocation
    /// NFR-SEC-020 exists to refuse — and it says nothing at all about a path that is not a
    /// regular file.
    #[test]
    fn an_oversized_preset_is_refused_before_it_is_read() {
        let config = temp_config_dir("oversized_preset");
        let path = config.join("huge.namirpreset");
        oversized_file(&path);

        let shared = Arc::new(SharedInner::new_at(&config));
        spawn_recall_preset(Arc::clone(&shared), path);

        assert_eq!(
            wait_for_notice(&shared),
            namir_worker::error_codes::FILE_TOO_LARGE.id,
            "an oversized preset must be refused before the read, not after `Document::parse` has \
             already been handed 256 MiB"
        );

        shared.shutdown_workers();
        let _ = std::fs::remove_dir_all(&config);
    }
}
