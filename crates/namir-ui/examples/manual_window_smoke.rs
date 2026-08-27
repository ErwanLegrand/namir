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

use std::path::PathBuf;

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
struct SmokeHost {
    frames: u64,
}

impl UiHost for SmokeHost {
    fn snapshot(&mut self) -> UiSnapshot {
        self.frames += 1;

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
    let mut host = SmokeHost { frames: 0 };
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
            ui.ctx().request_repaint();

            if host.frames >= FRAMES_BEFORE_CLOSE {
                println!("rendered {} frames; closing", host.frames);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        },
    );

    println!("window closed cleanly");
}
