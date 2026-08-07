// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Erwan Patrick Legrand
//
//! D-5.1's role for this crate: "CLAP adapter. **The only crate that names CLAP.**" M6's second
//! product shell (alongside `namir-app`, built independently against the same `namir-ui`/
//! `namir-worker`/`namir-engine` seams — see D-5.1's layering table for why both are permitted to
//! depend on everything below `namir-app`/`namir-clap` and nothing else).
//!
//! # Starting point
//!
//! `spikes/s4-clack-clap` (D-14.2, validated: `clap-validator` 15/15, loads and runs in Reaper,
//! GUI extension confirmed working — `docs/02-architecture.md` §19) proved the shape: `clack`'s
//! entry point/descriptor/`PluginGuiImpl`/`process()` skeleton. This crate is that shape, with the
//! spike's straight copy-through `process()` replaced by the real six-stage engine
//! (`namir_engine::build_default_engine`), its empty GUI window replaced by `namir_ui`'s real one,
//! and every other FR-CLAP extension the spike deliberately skipped (`params`, `state`,
//! `audio-ports`, `latency`) filled in.
//!
//! # Module map
//!
//! - [`param_mirror`] — the lock-free "current value of every parameter" store shared across
//!   threads.
//! - [`shared`] — [`shared::NamirShared`]/[`shared::SharedInner`], this instance's CLAP
//!   `[thread-safe]` half; the process-global [`namir_worker::ResourceCache`] (FR-CLAP-090) and
//!   the live [`namir_worker::Instance`] both live here.
//! - [`worker_jobs`] — every place a `namir_ui::UiIntent` or a fresh `activate()` needs
//!   off-thread file I/O or a blocking handover submit.
//! - [`ui_host`] — [`ui_host::ClapUiHost`], this crate's `namir_ui::UiHost` implementation
//!   (FR-CLAP-100's GUI bridge).
//! - [`audio`] — [`audio::NamirAudioProcessor`], CLAP's `[audio-thread]` half: the real engine,
//!   host-automation-to-`Chain` wiring, D-7.4/D-13.2's first real callers.
//! - [`main_thread`] — [`main_thread::NamirMainThread`], CLAP's `[main-thread]` half.
//! - [`gui`] — the `gui` extension impl, including D-5.3's written safety argument for the one
//!   `unsafe` block this crate's GUI embedding needs.
//! - [`params_ext`], [`audio_ports_ext`], [`latency_ext`], [`state_ext`] — the remaining CLAP
//!   extensions (`params`/FR-CLAP-060's bypass convention, `audio-ports`/FR-CLAP-030,
//!   `latency`/FR-CLAP-040, `state`/FR-CLAP-050).
//! - [`error_codes`] — this crate's own D-16.1 catalogue entries.
//!
//! # Deliberately out of scope this round
//!
//! - FR-CLAP-110 (Should: host-driven resize) — `can_resize() == false`, matching the spike.
//! - FR-CLAP-120 (Should: MIDI/note-expression program change) — no note-port extension is
//!   declared at all.
//! - Configuring library roots from the GUI — `namir-ui`'s `UiIntent` set has no such intent yet
//!   (see `shared`'s module doc comment); this crate's library wiring is real but inert without
//!   it.

#![doc(test(attr(deny(warnings))))]

mod audio;
mod audio_ports_ext;
mod error_codes;
mod gui;
mod latency_ext;
mod main_thread;
mod param_mirror;
mod params_ext;
mod shared;
mod state_ext;
mod ui_host;
mod worker_jobs;

use clack_extensions::audio_ports::PluginAudioPorts;
use clack_extensions::gui::PluginGui;
use clack_extensions::latency::PluginLatency;
use clack_extensions::params::PluginParams;
use clack_extensions::state::PluginState;
use clack_plugin::plugin::features::{AUDIO_EFFECT, STEREO};
use clack_plugin::prelude::*;

use audio::NamirAudioProcessor;
use main_thread::NamirMainThread;
use shared::NamirShared;

/// The reverse-DNS plugin identifier FR-CLAP-010 requires — distinct from
/// `spikes/s4-clack-clap`'s own `org.legrand.namir.spike.s4`, which is a throwaway spike id
/// (§19: spikes are "not carried forward").
const PLUGIN_ID: &str = "org.legrand.namir";

/// The marker type tying [`audio::NamirAudioProcessor`], [`shared::NamirShared`] and
/// [`main_thread::NamirMainThread`] together into one CLAP plugin (`clack_plugin::plugin::
/// Plugin`) — see this crate's top doc comment for the module map.
pub struct NamirClapPlugin;

impl Plugin for NamirClapPlugin {
    type AudioProcessor<'a> = NamirAudioProcessor<'a>;
    type Shared<'a> = NamirShared<'a>;
    type MainThread<'a> = NamirMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<'_, Self>, _shared: Option<&NamirShared>) {
        builder
            .register::<PluginGui>()
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginLatency>();
    }
}

impl DefaultPluginFactory for NamirClapPlugin {
    fn get_descriptor() -> PluginDescriptor {
        // `features-categories` (clap-validator) requires at least one of the four main CLAP
        // categories; Namir is an audio effect. `STEREO` is advisory, matching FR-CLAP-030's
        // declared port configuration (`crate::audio_ports_ext`).
        PluginDescriptor::new(PLUGIN_ID, "Namir")
            .with_vendor("Erwan Patrick Legrand")
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_description("NAM neural amp model + IR convolution, as a CLAP plugin.")
            .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        Ok(NamirShared::new())
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a NamirShared<'a>,
    ) -> Result<Self::MainThread<'a>, PluginError> {
        Ok(NamirMainThread::new(host, shared))
    }
}

clack_export_entry!(SinglePluginEntry<NamirClapPlugin>);

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-CLAP-010: a stable, reverse-DNS plugin identifier, distinct from the spike's.
    #[test]
    fn plugin_id_is_reverse_dns_and_not_the_spike_id() {
        assert!(PLUGIN_ID.contains('.'));
        assert_ne!(PLUGIN_ID, "org.legrand.namir.spike.s4");
    }

    /// `clap-validator`'s own `features-categories` test (S-4's one recorded finding, see this
    /// crate's top doc comment) requires at least one of `instrument`/`audio-effect`/
    /// `note-effect`/`analyzer` — pinned here so a future edit cannot silently drop it and only
    /// discover the regression when the validator (or a host) runs.
    #[test]
    fn descriptor_declares_audio_effect_and_matches_the_plugin_id() {
        let descriptor = NamirClapPlugin::get_descriptor();
        assert_eq!(descriptor.id().unwrap().to_str().unwrap(), PLUGIN_ID);
        let features: Vec<String> = descriptor
            .features()
            .map(|f| f.to_string_lossy().into_owned())
            .collect();
        assert!(
            features.iter().any(|f| f == "audio-effect"),
            "descriptor features {features:?} must include audio-effect"
        );
    }
}
