//! CLAP's `state` extension (FR-CLAP-050: "host-driven state save and load per Section 5.9").
//! Both directions go through `namir_state::State`/`Document` exactly as `namir-app` will (the
//! same payload FR-STATE-010 defines, D-11.2's write-back preservation included) — CLAP's own
//! contribution here is only the byte stream, via [`clack_common::stream`]'s `Read`/`Write`
//! adapters over the host-supplied `clap_istream`/`clap_ostream`.

use std::io::{Read, Write};

use clack_extensions::state::PluginStateImpl;
use clack_plugin::plugin::PluginError;
use clack_plugin::stream::{InputStream, OutputStream};
use namir_state::{Document, State};

use crate::main_thread::NamirMainThread;

impl<'a> PluginStateImpl for NamirMainThread<'a> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        let state = self.shared.inner.snapshot_state();
        let onto = self.shared.inner.last_document();
        let document = state.write_onto(&onto);
        let bytes = document.to_pretty_bytes();
        output
            .write_all(&bytes)
            .map_err(|_| PluginError::Message("failed to write plugin state"))?;
        self.shared.inner.set_last_document(document);
        self.shared.inner.mark_clean();
        Ok(())
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut bytes = Vec::new();
        input
            .read_to_end(&mut bytes)
            .map_err(|_| PluginError::Message("failed to read plugin state"))?;

        let (state, warnings) = State::read(&bytes)
            .map_err(|_| PluginError::Message("failed to parse plugin state"))?;
        for w in warnings {
            self.shared.inner.push_notice(w.code, w.detail);
        }

        // Preserve whatever this build doesn't understand (D-11.2) for the next save. A
        // corrupt/unparseable document already failed `State::read` above and returned early, so
        // `Document::parse` here cannot fail on a path `State::read` didn't already reject —
        // still degrades to `Document::empty()` rather than unwrapping, per P8.
        let document = Document::parse(&bytes).unwrap_or_else(|_| Document::empty());
        self.shared.inner.set_last_document(document);
        self.shared.inner.adopt_state(&state);
        self.shared.inner.mark_clean();

        // Tells the host every parameter's value should be re-queried (`clack_extensions::params`'s
        // own "Loading a preset" scenario) — see `NamirMainThread::notify_params_changed`'s own
        // doc comment for why this is required, not optional, and how its absence was found.
        self.notify_params_changed();

        // If an engine already exists (a `state` load while active — e.g. the host recalling a
        // different track's automation state without a full deactivate), replay onto it too.
        // `spawn_recall` itself is a no-op when there is no live `Instance` yet, which is the
        // ordinary case: most hosts load state *before* the first `activate()`, and that
        // activation's own replay (`crate::audio`) picks up what was just adopted above.
        crate::worker_jobs::spawn_recall(std::sync::Arc::clone(&self.shared.inner));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_params::stages::trim;

    /// FR-STATE-010's round trip, at the level this crate actually adds: a `SharedInner`'s
    /// current mirror/resource state serialises through `State::write_onto`/`Document::
    /// to_pretty_bytes` and restores through `State::read`/`SharedInner::adopt_state` with the
    /// same values, without a live CLAP host or stream at all (the pure logic `save`/`load` call
    /// through, exercised directly).
    // trace-partial: FR-CLAP-050
    // uncovered: FR-CLAP-050 — the save direction of the host-driven half is unspanned:
    // uncovered: PluginStateImpl::save is called by no test, so nothing drives the clap_ostream
    // uncovered: adapter or the write-back through last_document it performs. load is driven
    // uncovered: through the real vtable by the host-ext-tests suites (clap_host_latency,
    // uncovered: clap_host_block_sizes, clap_host_rt_blocking, fr_cfg_020_shell_parity), which
    // uncovered: reach set_last_document and spawn_recall; two of its sequel steps still are not
    // uncovered: exercised — push_notice never runs, no test loading a document State::read warns
    // uncovered: on, and notify_params_changed's rescan is asserted by nothing, since
    // uncovered: TestHostMainThread::param_rescans has no reader; closes M8
    #[test]
    fn a_snapshot_round_trips_through_bytes_and_adopt_state() {
        let a = crate::shared::SharedInner::new();
        a.params.set_by_key(trim::GAIN_DB.key, 3.0);
        let state = a.snapshot_state();
        let bytes = state.write_onto(&Document::empty()).to_pretty_bytes();

        let (restored, warnings) = State::read(&bytes).unwrap();
        assert!(warnings.is_empty());

        let b = crate::shared::SharedInner::new();
        b.adopt_state(&restored);
        assert_eq!(b.params.snapshot().get(trim::GAIN_DB.key), Some(3.0));
    }

    /// D-11.2's write-back promise, exercised at this crate's own boundary: an unrelated section
    /// a prior `load` preserved in `last_document` survives a subsequent `save`. `Document`'s
    /// section accessors are `pub(crate)` to `namir-state`, so this reaches in and out through
    /// raw JSON bytes (`Document::parse`/`to_pretty_bytes` are both public) exactly as a real
    /// host's save/load round trip would.
    #[test]
    fn saving_preserves_an_unrelated_section_from_the_last_loaded_document() {
        let shared = crate::shared::SharedInner::new();
        let original_bytes = br#"{"format_version":1,"host_specific":{"vendor_extra":"kept"}}"#;
        shared.set_last_document(Document::parse(original_bytes).unwrap());

        let state = shared.snapshot_state();
        let saved_bytes = state.write_onto(&shared.last_document()).to_pretty_bytes();
        let saved_json: serde_json::Value = serde_json::from_slice(&saved_bytes).unwrap();
        assert_eq!(saved_json["host_specific"]["vendor_extra"], "kept");
    }
}
