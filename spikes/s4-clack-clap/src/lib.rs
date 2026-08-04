// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Erwan Patrick Legrand
//
// Namir spike S-4 — validates architecture decision D-14.2 (clack as the CLAP binding).
//
// Question: can a minimal clack plugin export a valid CLAP entry point, pass
// clap-validator (FR-CLAP-020), and load in a real host (FR-CLAP-030)?
//
// This is deliberately the smallest plugin that is still a *correct* one: it declares a
// stereo audio port pair and writes its output, rather than leaving the output buffer
// untouched (which would leave the host reading uninitialised memory).

use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, PluginGui, PluginGuiImpl, Window as ClapWindow,
};
use clack_plugin::plugin::features::{AUDIO_EFFECT, STEREO};
use clack_plugin::prelude::*;
use egui_baseview::{EguiWindow, EguiWindowSettings};

/// Fixed editor size. Resizing (FR-CLAP-110) is deliberately out of scope for the spike.
const GUI_WIDTH: u32 = 480;
const GUI_HEIGHT: u32 = 320;

pub struct SpikePlugin;

pub struct SpikeMainThread {
    /// The embedded editor window, present only while the host has the GUI open.
    window: Option<baseview::WindowHandle>,
}

struct GuiState {
    frames: u64,
}

// The trait carries only a provided method (`on_main_thread`), so an empty impl is correct.
// A named type is used rather than `()` so the GUI extension can be hung off it.
impl<'a> PluginMainThread<'a, ()> for SpikeMainThread {}

impl Plugin for SpikePlugin {
    type AudioProcessor<'a> = SpikeAudioProcessor;
    type Shared<'a> = ();
    type MainThread<'a> = SpikeMainThread;

    fn declare_extensions(builder: &mut PluginExtensions<'_, Self>, _shared: Option<&()>) {
        builder.register::<PluginGui>();
    }
}

impl PluginGuiImpl for SpikeMainThread {
    fn is_api_supported(&mut self, configuration: GuiConfiguration<'_>) -> bool {
        // Embedded only. A floating window is the CLAP fallback and is not what R-1 asks about.
        configuration.api_type == GuiApiType::WIN32 && !configuration.is_floating
    }

    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        Some(GuiConfiguration {
            api_type: GuiApiType::WIN32,
            is_floating: false,
        })
    }

    fn create(&mut self, configuration: GuiConfiguration<'_>) -> Result<(), PluginError> {
        // The window itself is created in `set_parent`, once the host supplies its handle.
        if self.is_api_supported(configuration) {
            Ok(())
        } else {
            Err(PluginError::Message("unsupported GUI API"))
        }
    }

    fn destroy(&mut self) {
        if let Some(window) = self.window.take() {
            window.close();
        }
    }

    fn set_parent(&mut self, window: ClapWindow<'_>) -> Result<(), PluginError> {
        // SAFETY (spike): the host guarantees the handle is valid for the lifetime of the
        // editor. Namir's real implementation must confine this to `namir-clap` with a
        // written safety argument, per D-5.3.
        let handle = unsafe { window.borrow_handle_unchecked() }
            .map_err(|_| PluginError::Message("host window handle unavailable"))?;

        let settings = EguiWindowSettings {
            title: "Namir S-4".to_string(),
            ..Default::default()
        };

        self.window = Some(EguiWindow::open_parented(
            &handle,
            settings,
            GuiState { frames: 0 },
            |_ctx, _cmds, _state| {},
            |_output, _viewport, _state| {},
            |ui, _cmds, state| {
                state.frames += 1;
                ui.heading("Namir — spike S-4");
                ui.label("egui embedded in the host's window via open_parented.");
                ui.separator();
                ui.label(format!("frames: {}", state.frames));
                ui.ctx().request_repaint();
            },
        ));

        Ok(())
    }

    fn set_transient(&mut self, _window: ClapWindow<'_>) -> Result<(), PluginError> {
        Err(PluginError::Message("floating windows are not supported"))
    }

    fn set_scale(&mut self, _scale: f64) -> Result<(), PluginError> {
        Ok(())
    }

    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: GUI_WIDTH,
            height: GUI_HEIGHT,
        })
    }

    fn set_size(&mut self, _size: GuiSize) -> Result<(), PluginError> {
        Ok(())
    }

    fn can_resize(&mut self) -> bool {
        false
    }

    fn show(&mut self) -> Result<(), PluginError> {
        Ok(())
    }

    fn hide(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}

impl DefaultPluginFactory for SpikePlugin {
    fn get_descriptor() -> PluginDescriptor {
        // `features-categories` in clap-validator requires at least one of the four main
        // CLAP categories. Namir is an audio effect; the remaining tags are advisory and
        // are what hosts use to place the plugin in their browser.
        PluginDescriptor::new("org.legrand.namir.spike.s4", "Namir S-4 Spike")
            .with_vendor("Erwan Patrick Legrand")
            .with_version("0.1.0")
            .with_description("Spike validating clack as Namir's CLAP binding (D-14.2).")
            .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        Ok(())
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        _shared: &'a (),
    ) -> Result<Self::MainThread<'a>, PluginError> {
        Ok(SpikeMainThread { window: None })
    }
}

pub struct SpikeAudioProcessor;

impl<'a> PluginAudioProcessor<'a, (), SpikeMainThread> for SpikeAudioProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut SpikeMainThread,
        _shared: &'a (),
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Straight copy in to out. Namir's real engine replaces this entirely; here it
        // exists only so the host never reads an uninitialised output buffer.
        //
        // Only f32 is handled: D-9.10 fixes Namir's internal format at f32, and every
        // mainstream CLAP host presents f32 buffers.
        for mut port_pair in &mut audio {
            let Ok(channels) = port_pair.channels() else {
                continue;
            };
            let Some(channels) = channels.into_f32() else {
                continue;
            };

            for channel_pair in channels {
                match channel_pair {
                    ChannelPair::InputOutput(input, output) => output.copy_from_slice(input),
                    // Host gave one buffer for both directions: already correct for a copy.
                    ChannelPair::InPlace(_) => {}
                    // No input to copy from; emit silence rather than leave it uninitialised.
                    ChannelPair::OutputOnly(output) => output.fill(0.0),
                    ChannelPair::InputOnly(_) => {}
                }
            }
        }

        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<SpikePlugin>);
