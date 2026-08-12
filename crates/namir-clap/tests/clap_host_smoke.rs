//! Proof that `tests/support/mod.rs` works — nothing more.
//!
//! This file deliberately carries **no** `// trace:` or `// trace-partial:` annotation. The six
//! M9b CLAP tests that follow each own one requirement and each write their own tag; a tag here
//! would put a claim into `docs/03-test-plan.md` that this file's assertions do not support.
//!
//! It exists so that a breakage in the shared harness surfaces as one obviously-harness-shaped
//! failure rather than as six confusing ones, and so the `host-ext-tests` feature path is proven
//! to compile *and run*, not merely to compile.
//!
//! **Before writing a test against `support`, read that module's doc comment — in particular the
//! HAZARD about `start_library_scan` and the developer's real library index.**

mod support;

use support::{
    DEFAULT_SAMPLE_RATE, StereoBuffers, activate_default, audio_section, instantiate_default, peak,
    sine_1k,
};

/// The whole life cycle the six downstream tests build on: instantiate, activate at 48 kHz,
/// process a few blocks of varying size through the real C vtable, deactivate, destroy.
#[test]
fn the_harness_instantiates_activates_processes_and_destroys() {
    let (_entry, mut instance) = instantiate_default();

    let stopped = activate_default(&mut instance);
    let mut processor = stopped.start_processing().expect("processing must start");

    let mut bufs = StereoBuffers::default_size();
    let tone = sine_1k(bufs.max_frames(), DEFAULT_SAMPLE_RATE, 0.25);
    bufs.fill_input(|_channel, frame| tone[frame]);

    // Varying block sizes on the same allocation — the `truncate` path this harness exists for.
    for frames in [64u32, 1, 512, 37, 128] {
        bufs.poison_output(f32::NAN);
        // `process` maps CLAP_PROCESS_ERROR to `Err`, so the `expect` is the error check.
        audio_section(|| bufs.process_block(&mut processor, frames))
            .unwrap_or_else(|e| panic!("a {frames}-frame block must process: {e}"));

        for channel in 0..support::CHANNELS {
            let written = &bufs.output(channel)[..frames as usize];
            assert!(
                support::all_finite(written),
                "channel {channel} of a {frames}-frame block contains a non-finite sample -- \
                 either the plugin produced one or it did not write the block at all"
            );
        }
    }

    // With no model or IR loaded the chain is not required to be audible, so this asserts only
    // that reading the output is meaningful, not that it is loud.
    let _ = peak(bufs.output(0));

    let stopped = processor.stop_processing();
    instance.deactivate(stopped);
    drop(instance); // `clap_plugin.destroy`
}

/// The harness's own determinism claim: same seed, same samples; and a 1 kHz sine that is actually
/// a signal rather than silence. Cheap, but it is what the six downstream tests will rely on when
/// they compare two runs against each other.
#[test]
fn the_signal_generators_are_deterministic() {
    const SEED: u64 = 0x0B1E_C0DE_1234_5678;
    let a = support::noise(256, SEED, 1.0);
    let b = support::noise(256, SEED, 1.0);
    assert_eq!(a, b, "the same seed must produce the same noise");

    let tone = sine_1k(480, DEFAULT_SAMPLE_RATE, 1.0);
    assert!(
        peak(&tone) > 0.99,
        "a full-scale 1 kHz sine must reach ~1.0"
    );
}

/// The `host-ext-tests` feature path (D-18.7): reach one host-side extension method through the
/// real vtable. `audio-ports` is the cheapest one — `src/audio_ports_ext.rs` declares exactly one
/// input and one output port, so this both proves the feature links and confirms the harness is
/// talking to the real plugin rather than a stub.
///
/// Not a `trace:` for FR-CLAP-030: that requirement is about negotiating the *configuration*, and
/// its test is a downstream agent's file.
#[cfg(feature = "host-ext-tests")]
#[test]
fn a_host_side_extension_is_reachable_under_the_feature() {
    use clack_extensions::audio_ports::PluginAudioPorts;
    use support::{main_thread_handle, require_plugin_extension};

    let (_entry, mut instance) = instantiate_default();

    let ports = require_plugin_extension::<PluginAudioPorts>(&mut instance);
    let mut handle = main_thread_handle(&mut instance);

    assert_eq!(ports.count(&mut handle, true), 1, "one input port");
    assert_eq!(ports.count(&mut handle, false), 1, "one output port");
}
