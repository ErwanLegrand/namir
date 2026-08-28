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
use crate::shared::SharedInner;

/// Everything [`PluginStateImpl::load`] does to `SharedInner` between reading the host's bytes and
/// telling the host about it — parse, surface D-11.2's tolerated-defect warnings as notices, retain
/// the whole document for write-back, adopt the state, and mark clean.
///
/// **Split out of `load` at M14 so it can be driven without a live CLAP host.** The two steps that
/// remain in `load` need one — `notify_params_changed` goes through the host's `params` extension
/// and `spawn_recall` needs the instance — and both are reachable through the in-process harness
/// (`tests/clap_host_state.rs`). This one is not: nothing outside this crate can read
/// `SharedInner::notices`, so a host-side test can drive the warning path and observe nothing.
/// Before the split, `push_notice`'s arm here ran in no test at all, which is what
/// FR-CLAP-050's `uncovered:` field said.
///
/// **Also the preset-recall path** (`crate::worker_jobs::spawn_recall_preset`): a `.namirpreset`
/// file and a host's state blob are the same document (`docs/04-state-and-preset-format.md`), so
/// they are adopted by the same function rather than by two that could come to disagree about
/// which warnings are tolerated.
///
/// Returns `namir-state`'s own error, not a `PluginError`: the caller that has a host stream to
/// answer maps it to one, and the caller that has a user in front of it reports the real
/// catalogue id.
pub(crate) fn adopt_document_bytes(
    inner: &SharedInner,
    bytes: &[u8],
) -> Result<(), namir_state::StateError> {
    let (state, warnings) = State::read(bytes)?;
    for w in warnings {
        inner.push_notice(w.code, w.detail);
    }

    // Preserve whatever this build doesn't understand (D-11.2) for the next save. A
    // corrupt/unparseable document already failed `State::read` above and returned early, so
    // `Document::parse` here cannot fail on a path `State::read` didn't already reject —
    // still degrades to `Document::empty()` rather than unwrapping, per P8.
    let document = Document::parse(bytes).unwrap_or_else(|_| Document::empty());
    inner.set_last_document(document);
    inner.adopt_state(&state);
    inner.mark_clean();
    Ok(())
}

impl<'a> PluginStateImpl for NamirMainThread<'a> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        let state = self.shared.inner.snapshot_state();
        let onto = self.shared.inner.last_document();
        let document = state.write_onto(&onto);
        // The *checked* writer: NFR-SEC-020's ceiling is enforced on the way out as well as on the
        // way in, so an FR-STATE-080 embedded copy large enough to exceed it fails here rather
        // than producing a blob this plugin's own `load` would refuse on the next session --
        // the moment at which the user's settings are already the thing being lost.
        let bytes = document
            .try_to_pretty_bytes()
            .map_err(|_| PluginError::Message("the plugin state is too large to write"))?;
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

        adopt_document_bytes(&self.shared.inner, &bytes)
            .map_err(|_| PluginError::Message("failed to parse plugin state"))?;

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
    ///
    /// **FR-CLAP-050's tag moved to `tests/clap_host_state.rs` at M14**, which drives
    /// `PluginStateImpl::save` and `load` themselves through the real `clap_ostream`/`clap_istream`
    /// adapters. This test's own subject — that the payload survives the round trip — is unchanged
    /// and still worth having; what it never was is evidence about the host-driven half.
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

    /// D-11.2's *tolerated defect* arm of `load`, which ran in no test before M14: a document
    /// carrying a parameter key no `REGISTRY` entry claims is accepted, and the warning
    /// `namir_state` produces for it is surfaced to the user as a notice rather than dropped.
    ///
    /// `namir_state::ParamValues::from_document_section`'s own doc comment enumerates the three
    /// rules; this drives the first. The unknown key is *not* applied (that is `namir-state`'s
    /// test), and the recognised key beside it is — so the document is genuinely tolerated rather
    /// than rejected, which is the property the notice is reporting on.
    #[test]
    fn loading_a_document_with_an_unknown_parameter_key_surfaces_a_notice() {
        let inner = crate::shared::SharedInner::new();
        assert!(inner.notices().is_empty());

        let bytes = br#"{
            "format_version": 1,
            "parameters": { "comp.ratio": 4.0, "trim.gain_db": 3.0 }
        }"#;
        adopt_document_bytes(&inner, bytes).expect("a tolerated defect must not fail the load");

        // The expected code is taken from `State::read`'s own output rather than named as a
        // literal: `namir_state::error_codes` is private to that crate, and asserting the two
        // agree is the stronger claim anyway — it says `load` surfaced *this* warning, not that
        // it happened to push a notice with a familiar id.
        let (_, warnings) = State::read(bytes).expect("the same bytes must parse");
        assert_eq!(warnings.len(), 1, "one tolerated defect: {warnings:?}");

        let notices = inner.notices();
        assert_eq!(
            notices.len(),
            1,
            "exactly one notice, for the one unknown key: {notices:?}"
        );
        assert_eq!(notices[0].code, warnings[0].code);
        assert_eq!(notices[0].detail, warnings[0].detail);
        assert!(
            notices[0].detail.contains("comp.ratio"),
            "the notice should name the key that was not understood, got {:?}",
            notices[0].detail
        );
        assert_eq!(
            inner.params.snapshot().get(trim::GAIN_DB.key),
            Some(3.0),
            "the recognised parameter beside it must still have been adopted"
        );
    }

    /// The other two of `from_document_section`'s three rules, on the same path: an out-of-range
    /// value is clamped and an unusable one is reset, each with its own catalogued notice.
    #[test]
    fn loading_a_document_with_out_of_range_and_invalid_values_surfaces_a_notice_for_each() {
        let inner = crate::shared::SharedInner::new();
        // `trim.gain_db` is declared -24..=+24 dB; `gate.threshold_db` gets a JSON string.
        let bytes = br#"{
            "format_version": 1,
            "parameters": { "trim.gain_db": 400.0, "gate.threshold_db": "loud" }
        }"#;
        adopt_document_bytes(&inner, bytes).expect("both defects are tolerated, not fatal");

        // Same device as the test above: the expected codes come from `State::read` itself.
        let (_, warnings) = State::read(bytes).expect("the same bytes must parse");
        assert_eq!(warnings.len(), 2, "two tolerated defects: {warnings:?}");

        let notices = inner.notices();
        let notice_codes: Vec<_> = notices.iter().map(|n| n.code).collect();
        for warning in &warnings {
            assert!(
                notice_codes.contains(&warning.code),
                "{:?} was produced by the parser but reached no notice: {notice_codes:?}",
                warning.code.id
            );
        }
        // Distinct codes, so the loop above cannot be satisfied twice by the same one.
        assert_ne!(warnings[0].code, warnings[1].code);
        assert_eq!(
            inner.params.snapshot().get(trim::GAIN_DB.key),
            Some(24.0),
            "the out-of-range value should have been clamped to the descriptor's maximum"
        );
    }

    /// A document this build cannot parse at all fails the load, leaving the mirror untouched —
    /// the arm `load` returns `PluginError` from, which a host reports rather than adopting
    /// nonsense.
    #[test]
    fn an_unparseable_document_fails_the_load_and_changes_nothing() {
        let inner = crate::shared::SharedInner::new();
        inner.params.set_by_key(trim::GAIN_DB.key, 5.0);

        assert!(adopt_document_bytes(&inner, b"this is not a document").is_err());
        assert_eq!(inner.params.snapshot().get(trim::GAIN_DB.key), Some(5.0));
        assert!(inner.notices().is_empty());
    }
}
