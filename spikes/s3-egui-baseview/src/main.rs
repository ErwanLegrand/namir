// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Erwan Patrick Legrand
//
// Namir spike S-3 — validates architecture decisions D-15.1 (egui) and D-15.2 (baseview).
//
// Question: can egui render into a baseview window on Windows 11?
//
// Success criterion: the process opens a window, renders a non-zero number of frames,
// closes itself, and exits 0. It must not require interaction, so that it can run
// unattended.
//
// Scope limit, stated honestly: this exercises `open_blocking`, which is the standalone
// case. Plugin embedding uses `open_parented` and needs a real CLAP host window; that is
// validated together with spike S-4, not here.

use egui_baseview::{EguiWindow, EguiWindowSettings};

/// Frames to render before the spike closes itself.
const FRAMES_BEFORE_CLOSE: u64 = 90;

struct SpikeState {
    frames: u64,
}

fn main() {
    let settings = EguiWindowSettings {
        title: "Namir — spike S-3".to_string(),
        ..Default::default()
    };

    EguiWindow::open_blocking(
        settings,
        SpikeState { frames: 0 },
        // build: runs once, at startup
        |_ctx, _cmds, _state| {
            println!("build: egui context created");
        },
        // output: runs after each frame
        |_output, _viewport, _state| {},
        // update: runs every frame
        |ui, _cmds, state| {
            state.frames += 1;

            ui.heading("Namir — spike S-3");
            ui.label("egui rendering inside a baseview window.");
            ui.separator();
            ui.label(format!("frame {} of {}", state.frames, FRAMES_BEFORE_CLOSE));

            // Repaint continuously so the frame counter advances without input.
            ui.ctx().request_repaint();

            if state.frames >= FRAMES_BEFORE_CLOSE {
                println!("rendered {} frames; closing", state.frames);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        },
    );

    println!("window closed cleanly");
}
