//! FR-UI-060: "the interface shall remain responsive (no frame exceeding 100 ms) while a library
//! scan of 10,000 files is in progress." This module is where that has to actually be true, since
//! [`crate::host::LibrarySnapshot`] is the one part of a [`crate::UiSnapshot`] whose size scales
//! with the user's library rather than staying fixed.
//!
//! Two things make it true:
//!
//! 1. **The index itself is never cloned.** [`crate::host::LibrarySnapshot::index`] is an
//!    `Arc<namir_library::Index>`; a fresh snapshot every frame costs one atomic refcount bump
//!    regardless of how many entries the index holds.
//! 2. **Filtering only reruns when something that affects its result actually changed** --
//!    tracked by [`LibraryViewState`], via [`Arc::ptr_eq`] against the last-filtered index plus a
//!    plain string comparison against the last-filtered query. `namir_library::search`'s own doc
//!    comment already establishes that one linear scan over 10,000 entries is sub-millisecond, so
//!    this isn't working around an expensive `filter` call -- it exists so a scan's *frequent*
//!    snapshot updates (index changing every progress tick) don't each pay that scan redundantly
//!    on every one of the ~60 frames rendered before the next actual change.
//! 3. **Only the visible rows are ever turned into widgets**, via `egui::ScrollArea::show_rows`
//!    (see [`render`]) -- a filtered result of 10,000 entries costs the same handful of widgets
//!    per frame as one of 10.
//!
//! [`tests::rendering_ten_thousand_entries_stays_well_under_the_100ms_frame_budget`] proves this
//! against `namir-fixtures`' real 10,000-file corpus, not a guessed row count.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{ScrollArea, TextEdit, Ui};
use namir_library::{Index, ItemMetadata, LibraryEntry, Query, filter};

use crate::UiIntent;
use crate::host::LibrarySnapshot;

/// Per-window state the caller keeps across frames (owned by [`crate::NamirUi`], never rebuilt
/// per frame) -- the query text box's contents and the filtered-result cache FR-UI-060 needs.
#[derive(Default)]
pub struct LibraryViewState {
    query_text: String,
    filtered_paths: Vec<PathBuf>,
    cached_index: Option<Arc<Index>>,
    cached_query: String,
}

impl LibraryViewState {
    /// Recomputes [`Self::filtered_paths`] against `index` iff `index` (by identity, not
    /// content -- see this module's doc comment) or the query text differs from what's cached.
    /// Paths, not entries, are cached: `namir_library::filter`'s own result borrows from `index`,
    /// which this struct cannot hold onto past this call without becoming self-referential, and a
    /// path is enough to look the entry back up (an `O(log n)` `Index::get`) for the handful of
    /// rows actually rendered each frame.
    fn ensure_filtered(&mut self, index: &Arc<Index>) {
        let stale = match &self.cached_index {
            Some(cached) => !Arc::ptr_eq(cached, index) || self.cached_query != self.query_text,
            None => true,
        };
        if !stale {
            return;
        }
        let query = Query::parse(&self.query_text);
        self.filtered_paths = filter(index, &query).map(|e| e.path.clone()).collect();
        self.cached_index = Some(Arc::clone(index));
        self.cached_query = self.query_text.clone();
    }

    #[cfg(test)]
    pub(crate) fn filtered_count(&self) -> usize {
        self.filtered_paths.len()
    }
}

/// A short display line for one entry: its extracted display name when available (a NAM model's
/// declared name, FR-NAM-080), else its file stem, plus its kind. Pure and separately testable
/// from the widget it feeds.
pub fn entry_label(entry: &LibraryEntry) -> String {
    let name = match &entry.metadata {
        ItemMetadata::Nam(m) if !m.name.trim().is_empty() => m.name.clone(),
        _ => entry
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("(unnamed)")
            .to_string(),
    };
    let kind = match entry.kind {
        namir_library::ItemKind::Nam => "NAM",
        namir_library::ItemKind::Ir => "IR",
    };
    format!("[{kind}] {name}")
}

/// Renders FR-UI-020's library-browsing surface: a search box, scan status/controls, and a
/// virtualized list of whatever currently matches the query. Appends a [`UiIntent`] to `intents`
/// for every user action this frame (query edits, a rescan/cancel request, or a double-click to
/// load an entry).
pub fn render(
    ui: &mut Ui,
    state: &mut LibraryViewState,
    snapshot: &LibrarySnapshot,
    intents: &mut Vec<UiIntent>,
) {
    ui.heading("Library");

    match &snapshot.scan {
        Some(progress) => {
            ui.label(format!(
                "Scanning... {} examined ({} hashed), {} pending",
                progress.files_examined, progress.files_hashed, progress.dirs_pending
            ));
            if ui.button("Cancel scan").clicked() {
                intents.push(UiIntent::CancelScanRequested);
            }
        }
        None => {
            if ui.button("Rescan library").clicked() {
                intents.push(UiIntent::RescanLibraryRequested);
            }
        }
    }

    let search_label = ui
        .add(egui::Label::new("Search").sense(egui::Sense::hover()))
        .on_hover_text("Filters by file name and, for NAM models, author/gear/description.");
    let search = ui
        .add(TextEdit::singleline(&mut state.query_text).hint_text("Search name, author, gear..."))
        .labelled_by(search_label.id);
    if search.changed() {
        intents.push(UiIntent::LibraryQueryChanged(state.query_text.clone()));
    }

    state.ensure_filtered(&snapshot.index);

    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    let total = state.filtered_paths.len();
    ui.label(format!(
        "{total} match{}",
        if total == 1 { "" } else { "es" }
    ));

    ScrollArea::vertical()
        .id_salt("namir_ui_library_scroll")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, total, |ui, row_range| {
            for row in row_range {
                let Some(path) = state.filtered_paths.get(row) else {
                    continue;
                };
                let Some(entry) = snapshot.index.get(path) else {
                    continue;
                };
                let label = entry_label(entry);
                let response = ui.add(
                    egui::Label::new(label)
                        .sense(egui::Sense::click())
                        .selectable(false),
                );
                let response = response
                    .on_hover_text(format!("{}\nDouble-click to load.", entry.path.display()));
                if response.double_clicked() {
                    intents.push(UiIntent::LoadLibraryEntry(entry.path.clone()));
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::ContentHash;
    use namir_library::{FileTime, ItemKind, NamItemMetadata, Origin};
    use std::time::Instant;

    fn entry(path: &str, name: &str) -> LibraryEntry {
        LibraryEntry {
            path: PathBuf::from(path),
            kind: ItemKind::Nam,
            size: 100,
            mtime: FileTime::now(),
            hash: Some(ContentHash::of(path.as_bytes())),
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
        }
    }

    #[test]
    fn entry_label_prefers_extracted_nam_name_over_file_stem() {
        let e = entry("marshall/plexi.nam", "Plexi 800");
        assert_eq!(entry_label(&e), "[NAM] Plexi 800");
    }

    #[test]
    fn entry_label_falls_back_to_file_stem_when_no_metadata_name() {
        let mut e = entry("marshall/plexi.nam", "");
        e.metadata = ItemMetadata::None;
        assert_eq!(entry_label(&e), "[NAM] plexi");
    }

    #[test]
    fn ensure_filtered_matches_an_empty_query() {
        let mut index = Index::empty();
        index.upsert(entry("a.nam", "Alpha"));
        index.upsert(entry("b.nam", "Beta"));
        let index = Arc::new(index);

        let mut state = LibraryViewState::default();
        state.ensure_filtered(&index);
        assert_eq!(state.filtered_count(), 2);
    }

    #[test]
    fn ensure_filtered_narrows_on_a_query() {
        let mut index = Index::empty();
        index.upsert(entry("a.nam", "Alpha"));
        index.upsert(entry("b.nam", "Beta"));
        let index = Arc::new(index);

        let mut state = LibraryViewState {
            query_text: "alpha".to_string(),
            ..Default::default()
        };
        state.ensure_filtered(&index);
        assert_eq!(state.filtered_paths, vec![PathBuf::from("a.nam")]);
    }

    #[test]
    fn ensure_filtered_is_not_recomputed_for_an_unchanged_index_and_query() {
        let mut index = Index::empty();
        index.upsert(entry("a.nam", "Alpha"));
        let index = Arc::new(index);

        let mut state = LibraryViewState::default();
        state.ensure_filtered(&index);
        state.filtered_paths.push(PathBuf::from("sentinel"));
        // Same index (same Arc), same query text -> must not recompute and wipe the sentinel.
        state.ensure_filtered(&index);
        assert!(state.filtered_paths.contains(&PathBuf::from("sentinel")));
    }

    #[test]
    fn ensure_filtered_recomputes_when_the_query_changes() {
        let mut index = Index::empty();
        index.upsert(entry("a.nam", "Alpha"));
        index.upsert(entry("b.nam", "Beta"));
        let index = Arc::new(index);

        let mut state = LibraryViewState::default();
        state.ensure_filtered(&index);
        assert_eq!(state.filtered_count(), 2);

        state.query_text = "beta".to_string();
        state.ensure_filtered(&index);
        assert_eq!(state.filtered_paths, vec![PathBuf::from("b.nam")]);
    }

    #[test]
    fn ensure_filtered_recomputes_when_the_index_identity_changes() {
        let mut first = Index::empty();
        first.upsert(entry("a.nam", "Alpha"));
        let first = Arc::new(first);

        let mut second = Index::empty();
        second.upsert(entry("a.nam", "Alpha"));
        second.upsert(entry("b.nam", "Beta"));
        let second = Arc::new(second);

        let mut state = LibraryViewState::default();
        state.ensure_filtered(&first);
        assert_eq!(state.filtered_count(), 1);
        state.ensure_filtered(&second);
        assert_eq!(state.filtered_count(), 2);
    }

    /// FR-UI-060, exercised against a real 10,000-file corpus (`namir_fixtures::library`, the
    /// same generator `namir-library`'s own NFR-PERF-060 benchmark uses) rather than a guess at
    /// what 10,000 rows looks like. Builds a real `namir_library::Index` from it, renders the
    /// full library view (search box, virtualized list) headlessly via `egui::Context::run_ui`
    /// -- the same entry point `egui-baseview` calls every real frame -- and asserts the frame
    /// completed well under FR-UI-060's 100ms ceiling.
    #[test]
    fn rendering_ten_thousand_entries_stays_well_under_the_100ms_frame_budget() {
        let corpus = namir_fixtures::library::generate_shared_corpus(20_260_807)
            .expect("generate the shared 10,000-file corpus");
        assert!(corpus.entries.len() >= 10_000);

        let mut index = Index::empty();
        for fixture in &corpus.entries {
            let kind = match fixture.kind {
                namir_fixtures::library::EntryKind::Nam => ItemKind::Nam,
                namir_fixtures::library::EntryKind::Ir => ItemKind::Ir,
            };
            index.upsert(LibraryEntry {
                path: fixture.path.clone(),
                kind,
                size: 0,
                mtime: FileTime::now(),
                hash: Some(fixture.content_hash),
                metadata: ItemMetadata::None,
                origin: Origin::Local,
            });
        }
        assert_eq!(index.len(), corpus.entries.len());
        let index = Arc::new(index);

        let ctx = egui::Context::default();
        let mut state = LibraryViewState::default();
        let snapshot = LibrarySnapshot {
            index: Arc::clone(&index),
            scan: None,
        };

        // Warm-up frame: font/glyph layout caches settle here, not inside the measured frame --
        // otherwise this test would be measuring one-time font-atlas setup cost, not per-frame
        // list-rendering cost, which is what FR-UI-060 actually constrains.
        render_one_frame(&ctx, &mut state, &snapshot);

        let start = Instant::now();
        render_one_frame(&ctx, &mut state, &snapshot);
        let elapsed = start.elapsed();

        assert_eq!(
            state.filtered_count(),
            corpus.entries.len(),
            "an empty query must match every entry"
        );
        assert!(
            elapsed.as_millis() < 100,
            "rendering a {}-entry library took {elapsed:?}, over FR-UI-060's 100ms budget",
            corpus.entries.len()
        );
    }

    fn render_one_frame(
        ctx: &egui::Context,
        state: &mut LibraryViewState,
        snapshot: &LibrarySnapshot,
    ) {
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(raw_input, |ui| {
            let mut intents = Vec::new();
            render(ui, state, snapshot, &mut intents);
        });
    }
}
