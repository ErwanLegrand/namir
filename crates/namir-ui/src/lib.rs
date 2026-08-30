//! D-5.1's role for this crate: "The one GUI implementation, shared by the standalone app and the
//! CLAP plugin." May depend on `core`, `params`, `library`, `state` only (`xtask layering`'s
//! `LAYERING_TABLE` already carries this row) -- notably **not** `namir-engine`, `namir-worker`,
//! or `namir-platform`. Both `namir-app` and `namir-clap` (built directly on top of this crate)
//! *do* depend on everything, including this one.
//!
//! # Consequence: this crate is a pure view + interaction layer, not an owner of anything real
//!
//! Because `namir-engine` and `namir-worker` are off limits, this crate **cannot own a live
//! `namir_engine::Chain`, cannot own a `namir_worker` instance, and cannot itself scan a library
//! on a thread.** Every one of those is real, stateful, and lives in exactly one place: whichever
//! crate (`namir-app` or `namir-clap`) actually owns the engine/worker/library this session is
//! running against.
//!
//! So `namir-ui` is built the other way around: every frame, it is handed a plain, already-computed
//! [`UiSnapshot`] (current parameter values, meter readings, the loaded model/IR's names, the
//! library index, scan progress, pending error notices) and it renders exactly that -- nothing it
//! draws can be more than one frame stale, and nothing it draws has a side effect on its own. The
//! only way this crate ever asks for something to change is by producing a [`UiIntent`] ("set this
//! parameter to X", "load this library entry", "start/cancel a scan", "dismiss this notice",
//! "save the current state under this name", "recall the preset at this path"),
//! which is handed to the [`UiHost`] trait the caller implements. `namir-app` and `namir-clap` are
//! this milestone's two implementors: each turns a `UiIntent` into a real call against its own
//! `Chain`/`namir-worker`/`namir-library::Index`, and each turns its own state into a fresh
//! `UiSnapshot` every frame. See [`host`]'s module doc comment for the exact contract each frame
//! follows.
//!
//! This split is also what makes FR-UI-070's "shall never interrupt audio" true *by construction*
//! rather than by discipline: there is no code path anywhere in this crate that could reach an
//! audio thread, because this crate has no way to name one.
//!
//! # Scope
//!
//! In scope, and closed by this crate (`docs/03-implementation-roadmap.md`'s M6):
//! - FR-UI-010 -- [`NamirUi`], the one widget/window type both [`open_blocking`] (`namir-app`)
//!   and [`open_parented`] (`namir-clap`) construct; see `app.rs`'s module doc comment for how
//!   parameterizing by *which `open_*` call wraps it* satisfies "a single implementation" rather
//!   than two.
//! - FR-UI-020 -- [`app::render`]'s layout: input meter+trim, gate, loaded model name, loaded IR
//!   name, EQ, output meter+level, and global bypass, all on one screen with no navigation/tabs
//!   (the library browser lives in a simultaneously visible side panel, not a separate tab).
//! - FR-UI-030 -- every [`controls::param_control`] pairs its value control with an
//!   `egui`-accessible name via `Response::labelled_by`, and is keyboard-operable via `egui`'s own
//!   `DragValue` focus/arrow-key handling. **Honest gap, recorded rather than glossed over:**
//!   `egui-baseview` 0.6 does not itself forward `egui`'s accesskit tree to a platform screen
//!   reader -- see `controls.rs`'s module doc comment and
//!   `docs/manual-tests/fr-ui-030-accessibility-script.md`.
//! - FR-UI-040 -- [`format::parse_value`] (typed entry) plus `ParamDescriptor::format_value`
//!   (already in `namir-params`, reused rather than duplicated) for numeric display.
//! - FR-UI-050 -- documented in `controls.rs`'s module doc comment and in-app via each control's
//!   hover tooltip: double-click a control's name to reset it; hold Shift while dragging its
//!   value for fine adjustment (the latter is `egui::DragValue`'s own built-in behaviour).
//! - FR-UI-060 -- [`library_view`]'s virtualized, filter-cached list; proven against a real
//!   10,000-file corpus in that module's own test, not a guessed row count.
//! - FR-UI-070 -- [`notices::render`]'s non-modal, individually-dismissible notice lines.
//!
//! Contributed to but **not** closed here (issue #100): FR-STATE-030's save and recall gestures.
//! `app::preset_controls` renders the two controls and emits [`UiIntent::SavePreset`] /
//! [`UiIntent::RecallPreset`]; the requirement's `*Verify:*` code is `I` and its subject is
//! "interchangeable between the standalone application and the CLAP plugin", so what closes it is
//! an integration test across both shells' `UiHost` implementations, not anything in this crate.
//! Its `trace-partial:` therefore stays where it is, on `namir-worker`'s recall test.
//!
//! Out of scope, deliberately:
//! - **Actually driving a `Chain`, a `namir-worker` instance, or a `namir-library` scan** -- that
//!   is precisely what [`UiHost`] exists to hand off, to `namir-app`/`namir-clap`.
//! - **Building the CLAP plugin itself** -- `namir-clap`'s job; this crate only provides
//!   [`open_parented`], the embedding half `spikes/s4-clack-clap` already validated the shape of.
//! - FR-UI-080/090 (Should): display-scale and touch-target sizing are not specifically tuned
//!   here beyond what `egui`'s own layout gives for free; left for whichever milestone first needs
//!   to verify them against a real scaled display or touch device.

mod app;
mod brand;
mod controls;
mod format;
mod host;
mod library_view;
mod meter;
mod notices;

pub use app::{NamirUi, ViewState, open_blocking, open_parented, open_with_srgb_fallback, render};
pub use host::{
    AudioModeStatus, AudioShareMode, LibrarySnapshot, MeterReading, PresetSummary, UiHost,
    UiIntent, UiNotice, UiSnapshot,
};
pub use library_view::{LibraryViewState, entry_label};
// The list-side half of FR-UI-070, shared by both shells rather than copied into each -- see
// `notices`' own module doc comment for the duplicate-notice and unbounded-list defects it closes.
pub use notices::{MAX_NOTICES, push_deduplicated};
