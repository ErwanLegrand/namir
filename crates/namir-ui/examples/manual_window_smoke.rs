// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Erwan Patrick Legrand

//! Manual-verification aid for
//! `docs/manual-tests/fr-ui-010-standalone-window-renders.md`: opens FR-UI-020's actual screen
//! (`namir_ui::render`, the same function `NamirUi`/`open_blocking` call every real frame) in a
//! real `egui-baseview` window against a canned `UiSnapshot`, renders a fixed number of frames,
//! then closes itself -- unattended, like `spikes/s3-egui-baseview`'s own smoke test, so it can
//! run in CI or a headless session without hanging waiting for a click.
//!
//! This is deliberately not a `#[test]`: it opens a real OS window and needs a real
//! GPU/display/windowing stack, which `cargo test`'s headless `egui::Context::run_ui`-based tests
//! throughout this crate specifically avoid depending on. Run it by hand (`cargo run --example
//! manual_window_smoke -p namir-ui`) to visually confirm the crate actually paints, as opposed to
//! merely laying out widgets correctly headlessly.
//!
//! Since issue #143 it needs no *physical* display: it opens through
//! `namir_ui::open_with_srgb_fallback`, so a software X server serves it. Measured, not assumed --
//! under `Xvfb :99 -screen 0 1280x1024x24` on Mesa 25.2.8/llvmpipe this example panicked with
//! `Could not fetch framebuffer config: CreationFailed(NoValidFBConfig)` before that change and
//! renders its 90 frames and exits 0 after it. `DISPLAY=:99 cargo run --example
//! manual_window_smoke -p namir-ui` is the whole invocation.
//!
//! **Its exit status is an assertion about frames rendered, not about reaching the end of `main`**
//! (M15 review, note b). `EguiWindow::open_blocking` runs the window on its own thread and joins it
//! with `unwrap_or_else(eprintln!)`, and `open_with_srgb_fallback` catches only the *first*
//! attempt's panic -- so a `namir_ui::render` that panicked on every frame would unwind that
//! thread, return here as if the window had closed, and exit 0. Everything the CI job driving this
//! example asserts would have held while the interface drew nothing at all. So the frames are
//! counted outside the window, [`FRAMES_BEFORE_CLOSE`] of them are required, and the final line
//! this prints names the count so a caller can assert on it too.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use egui_baseview::{EguiWindow, EguiWindowSettings};
use namir_library::{FileTime, Index, ItemKind, ItemMetadata, LibraryEntry, Origin};
use namir_ui::{LibrarySnapshot, MeterReading, UiHost, UiIntent, UiNotice, UiSnapshot, ViewState};

/// Frames to render before the window closes itself.
const FRAMES_BEFORE_CLOSE: u64 = 90;

/// This example's own catalogue: the one notice it paints, so a reader can see what an FR-UI-070
/// notice looks like on screen.
///
/// A module, rather than the bare `const` that stood here until M14, because FR-ERR-020 says every
/// error code is a catalogue entry and `xtask error-catalogue` now enforces that shape — a
/// construction outside a catalogue module is a code no enumeration of any catalogue would list,
/// which is exactly the second conjunct of that requirement's method. This one is not a product
/// error path at all; declaring it here is what says so.
mod error_codes {
    use namir_core::{ErrorCode, Severity};

    pub const SAMPLE_NOTICE: ErrorCode = ErrorCode::new(
        "ui.manual_smoke.example_notice",
        Severity::Warning,
        "This is a sample FR-UI-070 notice, for visual inspection only ({detail}).",
        "Nothing to do -- this entry exists so a reader can see what a notice and its remedy look \
         like on screen, including how a long one wraps against the Dismiss button.",
    );
}

/// A host that hands back a fixed, representative snapshot -- enough on-screen content (a couple
/// of library entries, a notice, non-silent meters) to actually eyeball FR-UI-020's layout, rather
/// than an empty screen.
struct SmokeHost;

impl UiHost for SmokeHost {
    fn snapshot(&mut self) -> UiSnapshot {
        let mut index = Index::empty();
        index.upsert(LibraryEntry {
            path: PathBuf::from("marshall/plexi.nam"),
            kind: ItemKind::Nam,
            size: 4096,
            mtime: FileTime::now(),
            hash: None,
            metadata: ItemMetadata::None,
            origin: Origin::Local,
        });
        index.upsert(LibraryEntry {
            path: PathBuf::from("cabs/1960a.wav"),
            kind: ItemKind::Ir,
            size: 8192,
            mtime: FileTime::now(),
            hash: None,
            metadata: ItemMetadata::None,
            origin: Origin::Local,
        });

        UiSnapshot {
            input_meter: MeterReading {
                peak_db: -6.0,
                rms_db: -14.0,
            },
            output_meter: MeterReading {
                peak_db: -3.0,
                rms_db: -10.0,
            },
            loaded_model_name: Some("Plexi 800 (sample)".to_string()),
            loaded_ir_name: Some("1960A (sample)".to_string()),
            library: LibrarySnapshot {
                index: std::sync::Arc::new(index),
                scan: None,
            },
            unsaved_changes: true,
            // A short notice and a deliberately long one: FR-UI-070's step 14 (issue #42) turned
            // on a row that did not wrap, so the example that exists to be looked at must show a
            // row long enough to wrap.
            notices: vec![
                UiNotice {
                    id: 1,
                    code: error_codes::SAMPLE_NOTICE,
                    detail: "example.nam".to_string(),
                },
                UiNotice {
                    id: 2,
                    code: error_codes::SAMPLE_NOTICE,
                    detail: "C:/Users/somebody/Documents/Namir/Library/marshall/\
                             a-very-long-model-name-of-the-kind-a-capture-session-produces.nam"
                        .to_string(),
                },
            ],
            ..UiSnapshot::default()
        }
    }

    fn dispatch(&mut self, _intent: UiIntent) {}
}

fn main() {
    let settings = EguiWindowSettings {
        title: "Namir UI -- manual smoke test".to_string(),
        ..Default::default()
    };

    // Frames whose `namir_ui::render` call *returned*, counted here rather than inside the window
    // state so it survives the window thread. See this file's header: nothing else in this program
    // can tell "the interface drew ninety frames and closed itself" from "the render panicked and
    // baseview swallowed it", and the two must not share an exit status.
    let rendered = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&rendered);

    // Through `namir_ui::open_with_srgb_fallback`, exactly as `namir_ui::open_blocking` and
    // `open_parented` do, so this example opens under a headless X server too (issue #143) --
    // which is the whole point of an unattended smoke test. Note what that costs: the closure may
    // run twice, so the host and view state are built *inside* it rather than moved in from
    // outside, since the first attempt's copies are dropped with `baseview`'s window thread.
    namir_ui::open_with_srgb_fallback(settings, move |settings| {
        // A retry counts from zero. The first attempt fails while opening the window, so it has
        // drawn nothing -- but adding two partial attempts together would be the one arithmetic
        // that could satisfy the assertion below without a single complete run.
        counter.store(0, Ordering::Relaxed);
        let frames = Arc::clone(&counter);
        let mut host = SmokeHost;
        let mut view = ViewState::default();

        EguiWindow::open_blocking(
            settings,
            (),
            |_ctx, _cmds, _state| {
                println!("build: egui context created");
            },
            |_output, _viewport, _state| {},
            move |ui, _cmds, _state| {
                let snapshot = host.snapshot();
                let mut intents = Vec::new();
                namir_ui::render(ui, &mut view, &snapshot, &mut intents);
                for intent in intents {
                    host.dispatch(intent);
                }
                // After `render` returned, never before it: a frame that panicked half-way through
                // painting is not a frame this example may count.
                let drawn = frames.fetch_add(1, Ordering::Relaxed) + 1;
                ui.ctx().request_repaint();

                if drawn >= FRAMES_BEFORE_CLOSE {
                    println!("rendered {drawn} frames; closing");
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            },
        );
    });

    let drawn = rendered.load(Ordering::Relaxed);
    if drawn < FRAMES_BEFORE_CLOSE {
        eprintln!(
            "manual_window_smoke: rendered {drawn} of {FRAMES_BEFORE_CLOSE} frames -- the window \
             closed before the interface had been drawn. A panic inside namir_ui::render unwinds \
             baseview's window thread, which open_blocking joins and reports without failing, so \
             this is what a broken render looks like from outside the window."
        );
        std::process::exit(1);
    }

    // The line CI greps for. It names both numbers so the assertion is on the count rather than on
    // this program having reached its last statement.
    println!(
        "manual_window_smoke: rendered {drawn} of {FRAMES_BEFORE_CLOSE} frames; window closed cleanly"
    );
}
