//! **FR-CLAP-050's host-driven half**: `clap_plugin_state.save` and `.load`, called by a real host
//! through the real C vtable, over the real `clap_ostream`/`clap_istream` adapters.
//!
//! **Before writing a test against `support`, read that module's doc comment — in particular the
//! HAZARD about `start_library_scan` and the developer's real library index.** Nothing here starts
//! a scan, and every document in this file is parameter-only — no `nam`, no `ir`, no file
//! reference of any kind — so nothing is written to disk and no library is consulted.
//!
//! # What this adds over `src/state_ext.rs`'s own unit tests
//!
//! Those exercise the *payload* logic — `State::write_onto`, `Document::to_pretty_bytes`,
//! `State::read`, `SharedInner::adopt_state` — by calling it directly. That is the right shape for
//! what it covers and it is what FR-CLAP-050's tag rested on until M14, but it bypasses every part
//! CLAP contributes: `PluginStateImpl::save` was called by no test at all, so nothing drove the
//! output stream adapter or the `last_document` write-back it performs, and `load`'s sequel steps
//! (`notify_params_changed`'s host round trip in particular) were reached by no assertion.
//!
//! This file drives the extension itself:
//!
//! - [`a_host_save_writes_a_document_the_host_can_load_back`] — save into a plain `Vec<u8>`, and
//!   require the bytes to be a well-formed document carrying the value the mirror held.
//! - [`a_host_load_then_save_preserves_a_section_this_build_does_not_understand`] — D-11.2's
//!   write-back promise, through the streams rather than around them.
//! - [`a_host_load_asks_the_host_to_rescan_every_parameter_value`] — `HostParams::rescan(VALUES)`,
//!   counted on the host side by `TestHostMainThread`, which had no reader before M14.
//! - [`a_save_load_save_round_trip_between_two_instances_is_byte_stable`] — the whole
//!   host-facing contract at once, across two independent plugin instances.
//! - [`a_host_load_of_an_unreadable_document_fails_rather_than_adopting_it`] — the error arm CLAP
//!   defines, reported to the host as `false` from `clap_plugin_state.load`.
//!
//! # Why the whole file is behind `host-ext-tests` (D-18.7)
//!
//! `clack_extensions::state::PluginState`'s `save`/`load` are its *host* halves and exist only
//! under `clack-extensions`' own `clack-host` feature. `.github/workflows/ci.yml`'s second,
//! required `cargo test -p namir-clap --features host-ext-tests` step is what runs this.
#![cfg(feature = "host-ext-tests")]

mod support;

use clack_extensions::params::ParamRescanFlags;
use clack_extensions::state::PluginState;
use clack_host::events::event_types::ParamValueEvent;
use clack_host::events::io::EventBuffer;
use clack_host::events::{Match, Pckn};
use clack_host::prelude::{ClapId, PluginInstance};
use clack_host::utils::Cookie;
use namir_params::ParamDescriptor;
use namir_params::stages::{eq, trim};
use namir_state::Document;

use support::{
    DEFAULT_MAX_BLOCK, DEFAULT_SAMPLE_RATE, StereoBuffers, TestHost, activate, audio_section,
    config, instantiate_default, main_thread_handle, require_plugin_extension,
};

/// The value the round-trip tests drive `trim.gain_db` to. Not its default (0.0), so "the document
/// carried it" is distinguishable from "the document carried nothing and the reader defaulted".
const TRIM_GAIN_DB: f64 = -7.5;

/// A second parameter, in a different stage, so a save that happens to serialise one section
/// correctly and another not at all is visible.
const EQ_MID_DB: f64 = 4.25;

/// Saves through the host's own `clap_plugin_state.save` and returns the bytes it wrote.
fn host_save(instance: &mut PluginInstance<TestHost>, state_ext: &PluginState) -> Vec<u8> {
    let mut bytes = Vec::new();
    state_ext
        .save(&mut main_thread_handle(instance), &mut bytes)
        .expect("the host-driven state save must succeed");
    bytes
}

/// Loads through the host's own `clap_plugin_state.load`.
fn host_load(
    instance: &mut PluginInstance<TestHost>,
    state_ext: &PluginState,
    bytes: &[u8],
) -> Result<(), clack_extensions::state::StateError> {
    let mut reader = bytes;
    state_ext.load(&mut main_thread_handle(instance), &mut reader)
}

/// The `parameters` section of `bytes`, as raw JSON — `namir_state::Document`'s own section
/// accessors are `pub(crate)` to that crate, exactly as `src/state_ext.rs`'s existing D-11.2 test
/// already notes.
fn parameters_of(bytes: &[u8]) -> serde_json::Value {
    let json: serde_json::Value =
        serde_json::from_slice(bytes).expect("a saved document must be JSON");
    json["parameters"].clone()
}

/// One `clap_event_param_value` at frame 0, the way a host delivers a knob move.
fn param_event(descriptor: &ParamDescriptor, value: f64) -> ParamValueEvent {
    ParamValueEvent::new(
        0,
        ClapId::new(descriptor.id.0),
        Pckn::new(Match::All, Match::All, Match::All, Match::All),
        value,
        Cookie::empty(),
    )
}

/// Drives two parameters to non-default values through the plugin's own audio-thread automation
/// path, so what a subsequent save serialises came from the engine rather than from this test
/// reaching into `SharedInner` (which it could not do anyway — that type is `pub(crate)`).
///
/// Returns nothing: the point is the side effect on the instance's `ParamMirror`.
fn drive_two_parameters(instance: &mut PluginInstance<TestHost>) {
    let stopped = activate(instance, config(DEFAULT_SAMPLE_RATE, 1, DEFAULT_MAX_BLOCK));
    let mut processor = stopped.start_processing().expect("processing must start");

    let mut events = EventBuffer::with_capacity(4);
    events.push(&param_event(&trim::GAIN_DB, TRIM_GAIN_DB));
    events.push(&param_event(&eq::MID_GAIN_DB, EQ_MID_DB));

    let mut bufs = StereoBuffers::new(DEFAULT_MAX_BLOCK as usize);
    bufs.silence_input();
    audio_section(|| bufs.process_block_with_events(&mut processor, 64, &events.as_input()))
        .expect("a block must process");

    instance.deactivate(processor.stop_processing());
}

/// `PluginStateImpl::save` itself, through the real output stream: the bytes a host receives are a
/// document this build can read back, carrying the values the plugin actually held.
// trace-partial: FR-CLAP-050
// uncovered: FR-CLAP-050 — Section 5.9 is FR-STATE-010..090, and the host-driven half is spanned
// uncovered: here for the parameter payload, D-11.2's write-back, the params rescan and the
// uncovered: failure arm, but not for every clause of every requirement it defers to: the
// uncovered: resource half of a document is driven through this extension only in its embedded
// uncovered: form (FR-STATE-080), with FR-STATE-070's library-relative and absolute candidates
// uncovered: resolved by clap_host_rt_blocking.rs and by namir-worker's own tests rather than
// uncovered: here, and FR-STATE-040's compound-method migration (issue #27) has no parser to
// uncovered: exercise; closes M8
#[test]
fn a_host_save_writes_a_document_the_host_can_load_back() {
    let (_entry, mut instance) = instantiate_default();
    let state_ext = require_plugin_extension::<PluginState>(&mut instance);
    drive_two_parameters(&mut instance);

    let bytes = host_save(&mut instance, &state_ext);
    assert!(!bytes.is_empty(), "the save wrote nothing to the stream");

    // Well-formed by this build's own reader, not merely by `serde_json`.
    let (_state, warnings) = namir_state::State::read(&bytes)
        .expect("a document this build wrote must be one it can read");
    assert!(
        warnings.is_empty(),
        "a freshly written document should round-trip with no tolerated defects: {warnings:?}"
    );

    let parameters = parameters_of(&bytes);
    assert_eq!(
        parameters[trim::GAIN_DB.key].as_f64(),
        Some(TRIM_GAIN_DB),
        "the saved document should carry the automated trim value, got {parameters}"
    );
    assert_eq!(
        parameters[eq::MID_GAIN_DB.key].as_f64(),
        Some(EQ_MID_DB),
        "the saved document should carry the automated EQ value, got {parameters}"
    );

    drop(instance); // `clap_plugin.destroy`
}

/// D-11.2's write-back promise driven through both streams: a section this build has no idea about
/// arrives in a host `load` and comes back out of the next host `save`.
///
/// `src/state_ext.rs` has a unit test for the same promise that calls `State::write_onto` directly.
/// This one goes through `clap_plugin_state.load`/`.save`, so it also covers the `last_document`
/// retention each of those performs — the part a direct call cannot reach.
#[test]
fn a_host_load_then_save_preserves_a_section_this_build_does_not_understand() {
    let (_entry, mut instance) = instantiate_default();
    let state_ext = require_plugin_extension::<PluginState>(&mut instance);

    let original = br#"{
        "format_version": 1,
        "parameters": { "trim.gain_db": -7.5 },
        "host_specific": { "vendor_extra": "kept", "nested": { "n": 3 } }
    }"#;
    host_load(&mut instance, &state_ext, original).expect("the host-driven load must succeed");

    let saved = host_save(&mut instance, &state_ext);
    let json: serde_json::Value = serde_json::from_slice(&saved).expect("the save must be JSON");
    assert_eq!(
        json["host_specific"]["vendor_extra"], "kept",
        "an unrecognised section did not survive the save: {json}"
    );
    assert_eq!(json["host_specific"]["nested"]["n"], 3);
    assert_eq!(
        parameters_of(&saved)[trim::GAIN_DB.key].as_f64(),
        Some(TRIM_GAIN_DB),
        "the loaded parameter did not come back out of the save"
    );

    drop(instance); // `clap_plugin.destroy`
}

/// `load`'s sequel step that talks back to the host: `HostParams::rescan(VALUES)`.
///
/// `clack_extensions::params`' own "Loading a preset" scenario requires it — without it a host
/// keeps displaying and automating the values it read before the load. `TestHostMainThread` has
/// counted the callback since M9b and nothing read the counter; this is its first reader.
#[test]
fn a_host_load_asks_the_host_to_rescan_every_parameter_value() {
    let (_entry, mut instance) = instantiate_default();
    let state_ext = require_plugin_extension::<PluginState>(&mut instance);

    instance.access_handler_mut(|main_thread| main_thread.reset_callback_counts());

    let document = br#"{ "format_version": 1, "parameters": { "trim.gain_db": -7.5 } }"#;
    host_load(&mut instance, &state_ext, document).expect("the host-driven load must succeed");

    let (rescans, flags) = instance.access_handler(|main_thread| {
        (main_thread.param_rescans(), main_thread.last_rescan_flags())
    });
    assert_eq!(
        rescans, 1,
        "a state load must ask the host to re-read parameter values exactly once"
    );
    assert_eq!(
        flags,
        Some(ParamRescanFlags::VALUES),
        "the rescan should name VALUES: the set of parameters did not change, only their values"
    );

    drop(instance); // `clap_plugin.destroy`
}

/// The whole host-facing contract in one shape, across two independent instances: instance A is
/// automated, saved; instance B loads A's bytes and saves its own; the two documents agree.
///
/// Byte-stability is the right bar and is not incidental — `Document::to_pretty_bytes` writes a
/// deterministic serialisation, so a difference here means B adopted something A did not save, or
/// dropped something A did.
#[test]
fn a_save_load_save_round_trip_between_two_instances_is_byte_stable() {
    let (_entry_a, mut a) = instantiate_default();
    let state_a = require_plugin_extension::<PluginState>(&mut a);
    drive_two_parameters(&mut a);
    let from_a = host_save(&mut a, &state_a);
    drop(a);

    let (_entry_b, mut b) = instantiate_default();
    let state_b = require_plugin_extension::<PluginState>(&mut b);
    host_load(&mut b, &state_b, &from_a).expect("the host-driven load must succeed");
    let from_b = host_save(&mut b, &state_b);
    drop(b);

    assert_eq!(
        String::from_utf8_lossy(&from_a),
        String::from_utf8_lossy(&from_b),
        "a second instance's save of a document it loaded should reproduce it exactly"
    );
    // Not vacuous: both must actually carry the automated values rather than both being defaults.
    assert_eq!(
        parameters_of(&from_b)[trim::GAIN_DB.key].as_f64(),
        Some(TRIM_GAIN_DB)
    );
}

/// The failure arm: a document this build cannot parse is refused, reported to the host as a
/// `false` return from `clap_plugin_state.load`, and leaves the plugin's own state alone.
#[test]
fn a_host_load_of_an_unreadable_document_fails_rather_than_adopting_it() {
    let (_entry, mut instance) = instantiate_default();
    let state_ext = require_plugin_extension::<PluginState>(&mut instance);
    drive_two_parameters(&mut instance);
    let before = host_save(&mut instance, &state_ext);

    assert!(
        host_load(&mut instance, &state_ext, b"{ not a document").is_err(),
        "an unparseable document must be refused, not silently adopted"
    );

    let after = host_save(&mut instance, &state_ext);
    assert_eq!(
        String::from_utf8_lossy(&before),
        String::from_utf8_lossy(&after),
        "a refused load must leave the plugin's state exactly as it was"
    );
    assert!(
        Document::parse(&after).is_ok(),
        "and the state it saves afterwards must still be a valid document"
    );

    drop(instance); // `clap_plugin.destroy`
}
