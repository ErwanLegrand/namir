//! [`NamirMainThread`]: CLAP's `[main-thread]` half (`PluginMainThread`). Owns everything that
//! must never be touched from another thread: the embedded editor window handle (`crate::gui`)
//! and the `HostMainThreadHandle`/`HostLatency` pair FR-CLAP-040's notify-on-change path needs
//! (see `crate::audio`'s module doc comment for the full sequence this type's `on_main_thread`
//! completes).

use std::sync::atomic::Ordering;

use clack_extensions::latency::HostLatency;
use clack_extensions::params::{HostParams, ParamRescanFlags};
use clack_plugin::host::HostMainThreadHandle;
use clack_plugin::plugin::PluginMainThread;

use crate::shared::NamirShared;

// `pub`, not `pub(crate)` — see `crate::shared::NamirShared`'s doc comment for why.
pub struct NamirMainThread<'a> {
    pub(crate) shared: &'a NamirShared<'a>,
    pub(crate) host: HostMainThreadHandle<'a>,
    host_latency: Option<HostLatency>,
    host_params: Option<HostParams>,
    /// The embedded editor window, present only while the host has the GUI open. See
    /// `crate::gui`'s written safety argument for `set_parent`, which is what populates this.
    pub(crate) window: Option<baseview::WindowHandle>,
}

impl<'a> NamirMainThread<'a> {
    pub(crate) fn new(host: HostMainThreadHandle<'a>, shared: &'a NamirShared<'a>) -> Self {
        let host_latency = host.shared().get_extension::<HostLatency>();
        let host_params = host.shared().get_extension::<HostParams>();
        Self {
            shared,
            host,
            host_latency,
            host_params,
            window: None,
        }
    }

    /// Tells the host the latency should be re-queried — see `crate::audio`'s module doc comment
    /// for exactly when this is safe to call (only while inactive, or from inside `activate()`
    /// itself, per `HostLatency::changed`'s own contract). A host that never advertised the
    /// `latency` extension simply never hears about it (`host_latency` is `None`), which is a
    /// silent no-op rather than a panic — the same tolerance every other optional extension in
    /// this crate gets.
    pub(crate) fn notify_latency_changed(&mut self) {
        if let Some(latency) = self.host_latency {
            latency.changed(&mut self.host);
        }
    }

    /// Tells the host every parameter's value should be re-queried, without needing a restart —
    /// `clack_extensions::params`'s own "Loading a preset" scenario ("call `HostParams::rescan`
    /// if anything changed"). **Required, not a nicety:** `crate::state_ext`'s `load` adopts a
    /// freshly parsed `namir_state::State` onto the mirror directly, off any automation event the
    /// host would otherwise observe from — without this call, a host has no way to know the
    /// values it last read (e.g. a fresh instance's defaults, from before this load) are now
    /// stale. Found the hard way: `clap-validator`'s `state-reproducibility-*` tests failed with
    /// exactly this diagnosis ("these parameter values changed... without a rescan request")
    /// until this call was added.
    pub(crate) fn notify_params_changed(&mut self) {
        if let Some(params) = self.host_params {
            params.rescan(&mut self.host, ParamRescanFlags::VALUES);
        }
    }
}

impl<'a> PluginMainThread<'a, NamirShared<'a>> for NamirMainThread<'a> {
    /// FR-CLAP-040's deferred half: the audio thread flagged `latency_dirty` and requested this
    /// callback (`crate::audio::NamirAudioProcessor::publish_latency`). If the plugin is still
    /// active, CLAP's own contract requires a restart before the new value may be announced (see
    /// `crate::audio`'s module doc comment); otherwise it is safe to announce directly.
    fn on_main_thread(&mut self) {
        if self
            .shared
            .inner
            .latency_dirty
            .swap(false, Ordering::Relaxed)
        {
            if self.shared.inner.active.load(Ordering::Relaxed) {
                self.host.shared().request_restart();
            } else {
                self.notify_latency_changed();
            }
        }
    }
}
