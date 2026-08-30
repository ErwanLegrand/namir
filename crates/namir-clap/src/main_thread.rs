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
        // Recorded whether or not the host advertised the extension, and *before* the call:
        // `latency_announced` is "the figure this plugin has published as authoritative", which is
        // what `on_main_thread` below decides a restart against (issue #93). A host with no
        // `latency` extension is one that will never be told anything, so restarting it repeatedly
        // for a figure it cannot read would be the same loop with none of the benefit.
        let published = self.shared.inner.latency_samples.load(Ordering::Relaxed);
        self.shared
            .inner
            .latency_announced
            .store(published, Ordering::Relaxed);
        if let Some(latency) = self.host_latency {
            latency.changed(&mut self.host);
        }
    }

    /// Asks the host to schedule a `params` flush, so a parameter the user moved in this plugin's
    /// own editor reaches it as automation even when no `process()` call is coming (issue #94).
    ///
    /// **Opportunistic, and honestly so.** The natural caller would be
    /// `crate::ui_host::ClapUiHost::set_param`, on the GUI thread, at the moment the knob moves —
    /// and it cannot be: `namir_ui::open_parented` requires `H: UiHost + 'static`, so `ClapUiHost`
    /// holds only the `'static` `Arc<SharedInner>` and can reach no `HostSharedHandle`, which is
    /// `'a`-bound (see `crate::shared`'s module doc comment for why that split exists at all).
    /// While the plugin is active this costs nothing, because the host is calling `process()`
    /// every block and the changes go out there; while it is inactive, this fires on whatever
    /// main-thread callback happens next, and until then the change is held — never lost — in the
    /// mirror's pending set.
    /// Tells the host to re-read every parameter if something behind its back changed them all —
    /// today, a preset recalled from this plugin's own editor
    /// (`crate::worker_jobs::spawn_recall_preset`).
    ///
    /// Serviced here and from `PluginMainThreadParams::flush`, both of which are `[main-thread]`;
    /// see [`Self::request_param_flush_if_pending`] for why a GUI-thread caller cannot do it
    /// itself, which is the same constraint in the same place.
    pub(crate) fn rescan_params_if_pending(&mut self) {
        if self
            .shared
            .inner
            .params_rescan_pending
            .swap(false, Ordering::AcqRel)
        {
            self.notify_params_changed();
        }
    }

    fn request_param_flush_if_pending(&mut self) {
        // A gesture this instance opened and could not close counts as pending work too (issue
        // #145): the change itself may have reached the host perfectly well, and the only thing
        // still owed is the `ParamGestureEnd` that `crate::params_ext`'s
        // `emit_gui_param_changes` will push at the head of its next call. Without this an
        // inactive plugin with nothing left to report would never ask for that call.
        if !self.shared.inner.params.has_gui_pending() && !self.shared.inner.gestures.has_open() {
            return;
        }
        if let Some(params) = self.host_params {
            params.request_flush(&self.host.shared());
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
        // D-13.2's elevation outcome, produced on the audio thread and reportable only here.
        self.shared.inner.report_thread_priority_outcome();

        // A preset recalled from the plugin's own editor replaced every parameter value on a pool
        // thread, where `HostParams::rescan` — `[main-thread]` — cannot be called. Same
        // opportunism, and same cause, as `request_param_flush_if_pending` below.
        self.rescan_params_if_pending();

        if self
            .shared
            .inner
            .latency_dirty
            .swap(false, Ordering::Relaxed)
        {
            let latency = self.shared.inner.latency_samples.load(Ordering::Relaxed);
            if !self.shared.inner.active.load(Ordering::Relaxed) {
                self.notify_latency_changed();
            } else if latency != self.shared.inner.latency_announced.load(Ordering::Relaxed) {
                self.host.shared().request_restart();
            }
            // Active, and the figure already matches what the host was told: nothing to
            // renegotiate. **This is issue #93's exit.** Every `activate()` rebuilds a default
            // engine and replays this instance's model onto it asynchronously, so the audio thread
            // observes that model's latency arrive again on *every* activation; asking for another
            // restart each time is a cycle that never terminates while a rate-mismatched model
            // stays loaded. A restart is only worth requesting when the host's figure is actually
            // wrong.
        }

        self.request_param_flush_if_pending();
    }
}
