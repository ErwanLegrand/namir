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
use namir_core::{ErrorCode, Severity};
use namir_library::{FileTime, Index, ItemKind, ItemMetadata, LibraryEntry, Origin};
use namir_ui::{LibrarySnapshot, MeterReading, UiHost, UiIntent, UiNotice, UiSnapshot, ViewState};

/// Frames to render before the window closes itself.
const FRAMES_BEFORE_CLOSE: u64 = 90;

const SAMPLE_NOTICE: ErrorCode = ErrorCode {
    id: "ui.manual_smoke.example_notice",
    severity: Severity::Warning,
    message_template: "This is a sample FR-UI-070 notice, for visual inspection only.",
};

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
            notices: vec![UiNotice {
                id: 1,
                code: SAMPLE_NOTICE,
                detail: "example.nam".to_string(),
            }],
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
