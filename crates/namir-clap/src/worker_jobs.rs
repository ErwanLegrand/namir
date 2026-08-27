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
use namir_state::FileRef;
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

        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                shared.push_notice(
                    namir_worker::error_codes::FILE_UNREADABLE,
                    format!("{}: {e}", path.display()),
                );
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
        // A full library-relative path needs the matching root identity, which
        // `namir_library::LibraryEntry` does not carry back to the caller today (only the
        // resolved absolute path) -- recorded as `absolute` (FR-STATE-070's second resolution
        // candidate) rather than its first, library-relative one. See this module's doc comment.
        library_relative: None,
        absolute: Some(path.to_string_lossy().into_owned()),
        display_name,
        embedded: None,
    };
    match target {
        Target::Nam => shared.set_nam_ref(Some(reference)),
        Target::Ir => shared.set_ir_ref(Some(reference)),
    }
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
        let index = shared.library_snapshot().index;
        let roots: Vec<PathBuf> = Vec::new();
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

fn library_target(shared: &SharedInner, path: &Path) -> Option<Target> {
    let index = shared.library_snapshot().index;
    let entry = index.get(path)?;
    Some(match entry.kind {
        namir_library::ItemKind::Nam => Target::Nam,
        namir_library::ItemKind::Ir => Target::Ir,
    })
}
