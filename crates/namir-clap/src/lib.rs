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
//! - [`presets`] — FR-STATE-030's named-preset locations and listing. **Its `preset_dir` belongs
//!   in `namir-platform`** so both shells resolve one directory; see that module's own doc comment.
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
mod host_wake;
mod latency_ext;
mod main_thread;
mod param_mirror;
mod params_ext;
mod presets;
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

/// Internal seam for `tests/clap_host_state.rs` (issue #94). A test binary is a **separate
/// crate**, so it cannot reach this crate's `pub(crate)` `SharedInner`/`worker_jobs::spawn_recall_preset` —
/// the worker-pool preset-recall path that sets `params_rescan_pending` and wakes the host. This
/// module records the live instance's `Arc<SharedInner>` at construction and exposes one narrow
/// `pub` function that dispatches a preset recall through that same path, so the integration test
/// can drive the real worker job and observe the resulting host callback without widening any
/// production API. `#[doc(hidden)]`: this is a test seam, not public surface. Only compiled under
/// the `host-ext-tests` feature, so it adds nothing to the production build.
#[cfg(feature = "host-ext-tests")]
#[doc(hidden)]
pub mod __test_support {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, PoisonError};

    use crate::shared::SharedInner;

    /// The most recently constructed live instance's `SharedInner`, held so a test can recall a
    /// preset through it. One per process (only one `TestHost` instance is active per test), and
    /// dropped when the instance is dropped.
    static LAST_SHARED: Mutex<Option<Arc<SharedInner>>> = Mutex::new(None);

    /// Records `shared` as the live instance for [`recall_preset_for_test`]. Called from
    /// [`shared::NamirShared::new`] under the `host-ext-tests` feature only.
    pub(crate) fn record_shared(shared: &Arc<SharedInner>) {
        // P8 poison recovery, matching `crate::shared`'s own `lock` helper.
        *LAST_SHARED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(shared));
    }

    /// Recalls the preset at `path` through this instance's real worker-pool path
    /// ([`crate::worker_jobs::spawn_recall_preset`]), the same code a GUI-triggered recall runs.
    /// Panics if no instance has been constructed yet.
    pub fn recall_preset_for_test(path: PathBuf) {
        let shared = LAST_SHARED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .expect("no live plugin instance recorded; construct one before recalling a preset");
        crate::worker_jobs::spawn_recall_preset(shared, path);
    }

    /// Diagnostic for `tests/clap_host_state.rs`'s issue-#94 test: whether the recalled instance
    /// has raised `params_rescan_pending` (the flag the wake exists to service). Lets the test
    /// distinguish "the recall job failed before the wake" from "the wake itself was not
    /// delivered" when the host-callback counter does not move.
    #[cfg(feature = "host-ext-tests")]
    pub fn last_rescan_pending() -> bool {
        let shared = LAST_SHARED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        shared
            .map(|s| {
                s.params_rescan_pending
                    .load(std::sync::atomic::Ordering::Acquire)
            })
            .unwrap_or(false)
    }

    /// Diagnostic: the recorded instance's outstanding notice count, so the test can tell "the
    /// recall job failed during adopt (raised a notice, never reached the wake)" from "the job
    /// never ran / the wake died silently".
    #[cfg(feature = "host-ext-tests")]
    pub fn last_notice_count() -> usize {
        let shared = LAST_SHARED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        shared.map(|s| s.notices().len()).unwrap_or(usize::MAX)
    }

    /// Diagnostic: how many live worker threads the recorded instance's pool has. `0` with a
    /// `None`/dropped inner would be the sign that `LAST_SHARED` holds a shut-down (already
    /// `destroy`ed) instance whose pool no longer accepts `spawn` — which would explain a job
    /// that never runs.
    #[cfg(feature = "host-ext-tests")]
    pub fn last_pool_threads() -> usize {
        let shared = LAST_SHARED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        shared.map(|s| s.pool.threads()).unwrap_or(usize::MAX)
    }
}

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

    fn new_shared(host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        // FR-ERR-010, once per *process* rather than once per instance: several plugin instances
        // share one host process, and `namir_platform::logging::init` is idempotent behind a
        // `OnceLock`, so the first instance a host creates installs the writer and every later one
        // resolves the same logger. Sited here rather than in `SharedInner::new` so it runs before
        // that constructor's own first records (`LibraryService::open_default`'s warnings go
        // through `log_worker_warning`), and so this crate's unit tests — which build a bare
        // `SharedInner` — leave the user's real log alone.
        //
        // `None`: this crate has no settings file to hold a persisted verbosity (roadmap §15 item
        // 8), so `NAMIR_LOG` is the plugin's only verbosity control in 1.0. Passing `None` is a
        // decision, not an omission — see `namir_platform::logging::resolve_level`.
        namir_platform::logging::init(None);
        Ok(NamirShared::new(&host))
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
    // trace: FR-CLAP-010
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
