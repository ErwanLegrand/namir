//! FR-CLAP-030: the audio port configuration this plugin declares, and what it does with the one
//! the host therefore hands it.
//!
//! Driven through the real C vtable by the shared `support` harness — **read that module's doc
//! comment first**, in particular the HAZARD about `start_library_scan` and the developer's real
//! library index. Nothing here starts a scan.
//!
//! # The two host implementations the `Verify:` line names
//!
//! FR-CLAP-030's method is `I across at least two host implementations`, and the two below are
//! genuinely independent implementations of the same C ABI rather than two runs of one — different
//! loaders, different processes, one vtable:
//!
//! 1. **This file, through `clack-host`** (`tests/support/mod.rs`). The plugin is loaded in-process
//!    by `PluginEntry::load_from_clack::<SinglePluginEntry<NamirClapPlugin>>` — no `dlopen` and no
//!    `unsafe` of ours, which is the whole reason D-18.6 chose this route — and its `audio-ports`
//!    extension is read through `clack-extensions`' *host* half, available only under the
//!    `host-ext-tests` feature (D-18.7). That is the test tagged below.
//! 2. **`clap-validator`**, the reference CLAP validator, run as a required (no
//!    `continue-on-error`) job in `.github/workflows/ci.yml`, pinned by commit
//!    `b2f1d9b79b1d264a5747f46707d72b1aa40a02ef`, against the release `cdylib` staged as
//!    `Namir.clap`. It is a separate executable that loads the plugin the way a DAW does —
//!    `libloading`'s `dlopen`/`LoadLibrary` onto the exported `clap_entry` symbol, out of process
//!    by default — and its `audio-ports` test group calls the same `count`/`get` vtable entries
//!    this file calls, on every run. It also covers one thing this harness structurally cannot:
//!    its `process-audio-basic-in-place` test exercises the `in_place_pair` this file can only
//!    assert is *declared*, because aliasing one buffer into both an input and an output port from
//!    the host side needs `unsafe`, which D-5.3 forbids anywhere in this crate outside
//!    `src/gui.rs`. That job carries FR-CLAP-020's own trace tag; it is cited here as the second
//!    host, not claimed as this file's artifact.
//!    `docs/manual-tests/fr-clap-030-audio-ports-negotiation.md` records its
//!    executed run (44 tests, 32 passed, 0 failed) and the real-DAW half that remains unexecuted.
//!
//! # Why the tag is a `trace-partial:` and stays one
//!
//! `crates/namir-clap/src/audio_ports_ext.rs` declares the **Stereo** configuration alone and
//! implements no `audio-ports-config` extension, while FR-CHAIN-060 names three (Mono,
//! Mono→stereo, Stereo). FR-CLAP-030's own `*Consequence*` note in the FRS (added 2026-08-12)
//! settles that as an accepted 1.0 scope reduction and states the disposition outright: the
//! negotiation limb spans one configuration of three, D-23.1's enumerated-set rule bites regardless
//! of how many hosts exercise it, and satisfying the "at least two host implementations" clause —
//! which the two hosts above do — does **not** promote the tag. So this file is written to be the
//! strongest honest evidence for what exists, not an argument that the gap is smaller than it is.
//!
//! # What is asserted, and one finding worth knowing before reading the assertions
//!
//! The declaration limb reads `count`/`get` in both directions and pins every field of the one
//! port each direction reports, including that an out-of-range index writes nothing at all (CLAP's
//! `get` returns `false` and the host must not read the buffer — here, `None`).
//!
//! The behavioural limb then activates and processes against that declaration. It asserts that both
//! declared output channels are genuinely written — the buffers are poisoned with `NaN` first, so
//! "wrote silence" and "wrote nothing" are distinguishable — and that they carry the same signal,
//! which is FR-CHAIN-050's identical-channel invariant, not an accident.
//!
//! **It does not assert that the two channels are processed independently, because they are not,
//! and asserting so would be false.** The chain is a mono core by design (FR-CHAIN-050):
//! `GateStage` detects on channel 0 and copies its gated result over every other channel
//! (`crates/namir-engine/src/stages/gate.rs:163-172`), and `TrimStage` re-establishes the same
//! invariant after its downmix (`crates/namir-engine/src/stages/trim.rs:167-174`). Because D-9.8
//! puts Gate *upstream* of Trim, and Gate is enabled by its own descriptor default, channel 1's
//! input is annihilated before Trim's sum ever sees it — so the shipped Stereo behaviour is
//! FR-CHAIN-060's second permitted Stereo input, `L-only (FR-CHAIN-070)`, not its `2 ch summed`
//! sibling. That is a satisfied requirement, not a defect: the FRS says so in as many words at its
//! own M9a correction to §5.3. The second test below pins it, so that a future change to the
//! gate default or to D-9.8's ordering fails loudly here rather than silently changing which of
//! the two permitted readings the product ships.

mod support;

use clack_host::prelude::PluginInstance;
use support::{
    CHANNELS, DEFAULT_SAMPLE_RATE, SINE_FREQ_HZ, StereoBuffers, TestHost, activate, all_finite,
    audio_section, config, fill_noise, fill_sine, instantiate_default, peak,
};

/// Block size every run here uses. Nothing about this test is block-size sensitive — FR-CLAP-070's
/// file owns that — so one middling size keeps the runs cheap.
const BLOCK: u32 = 256;

/// Amplitude of the channel-0 probe tone: -12 dBFS, far above the gate's -70 dBFS default
/// threshold, so the gate is open for the whole measurement rather than closing over it.
const AMPLITUDE: f32 = 0.25;

/// Blocks per run: 16 × 256 frames is ~85 ms at 48 kHz, past the gate's 1 ms attack and every
/// `GainLike` smoothing ramp in the chain, so the block that gets measured is a settled one.
const BLOCKS: usize = 16;

/// Seeds the channel-1 signal. Fixed, so a failure reproduces exactly (D-19.1's spirit).
const RIGHT_NOISE_SEED: u64 = 0x0A11_D107_5EED_0030;

/// Activates `instance`, runs [`BLOCKS`] blocks with channel 0 carrying a phase-continuous 1 kHz
/// sine and channel 1 carrying whatever `right` writes into it, and returns the final block's two
/// output channels.
///
/// Every block poisons the output with `NaN` first, so an unwritten channel reaches the caller as
/// `NaN` rather than as whatever the previous block left behind. Leaves the instance deactivated,
/// so a caller may call this again with a different channel-1 signal — which is exactly the
/// comparison the second test makes.
fn run_stereo_blocks(
    instance: &mut PluginInstance<TestHost>,
    mut right: impl FnMut(&mut [f32]),
) -> [Vec<f32>; CHANNELS] {
    let stopped = activate(instance, config(DEFAULT_SAMPLE_RATE, 1, BLOCK));
    let mut processor = stopped.start_processing().expect("processing must start");
    let mut bufs = StereoBuffers::new(BLOCK as usize);

    let mut done: u64 = 0;
    for _ in 0..BLOCKS {
        fill_sine(
            bufs.input_mut(0),
            SINE_FREQ_HZ,
            DEFAULT_SAMPLE_RATE,
            AMPLITUDE,
            done,
        );
        right(bufs.input_mut(1));
        bufs.poison_output(f32::NAN);

        audio_section(|| bufs.process_block(&mut processor, BLOCK))
            .unwrap_or_else(|e| panic!("a {BLOCK}-frame stereo block must process: {e}"));

        done += u64::from(BLOCK);
    }

    let out = [bufs.output(0).to_vec(), bufs.output(1).to_vec()];

    let stopped = processor.stop_processing();
    instance.deactivate(stopped);
    out
}

/// Fills channel 1 with seeded noise — a signal that is emphatically *not* channel 0's tone, which
/// is what makes "did the plugin treat the two channels differently" an answerable question.
fn distinct_right_channel(dst: &mut [f32]) {
    fill_noise(dst, RIGHT_NOISE_SEED, AMPLITUDE);
}

/// Asserts that the run wrote both declared output channels with a real, finite signal, and that
/// the two carry the identical signal FR-CHAIN-050 requires of every stage downstream of Trim.
fn assert_declared_output_pair_was_written(out: &[Vec<f32>; CHANNELS]) {
    for (channel, samples) in out.iter().enumerate() {
        assert_eq!(
            samples.len(),
            BLOCK as usize,
            "channel {channel} should carry a whole {BLOCK}-frame block"
        );
        assert!(
            all_finite(samples),
            "channel {channel} of the declared stereo output pair contains a non-finite sample -- \
             either the plugin produced one or it never wrote that channel and the NaN poison \
             survived, which for a port declared with channel_count 2 is the same defect"
        );
        assert!(
            peak(samples) > AMPLITUDE * 0.5,
            "channel {channel} of the declared stereo output pair came out at peak {} from a \
             -12 dBFS tone -- a declared channel that stays near-silent is a channel the plugin \
             is not really driving",
            peak(samples)
        );
    }

    assert_eq!(
        out[0], out[1],
        "the two channels of the declared stereo output pair differ -- every stage downstream of \
         Trim relies on FR-CHAIN-050's identical-channel invariant \
         (crates/namir-engine/src/stages/trim.rs:167-174), so a divergence here is a chain defect, \
         not a stereo feature"
    );
}

// trace-partial: FR-CLAP-030
// uncovered: FR-CLAP-030 — the negotiation limb spans one configuration of the three FR-CHAIN-060
// uncovered: names. crates/namir-clap/src/audio_ports_ext.rs declares Stereo alone and implements
// uncovered: no audio-ports-config extension, so Mono and Mono→stereo are never offered and no
// uncovered: host can request them: there is no vtable entry for a test to call, in this harness
// uncovered: or in any other host. Accepted 1.0 scope reduction per FR-CLAP-030's Consequence note
// uncovered: (FRS, 2026-08-12), adjudicated at the 1.0 exit gate; closes M8
#[cfg(feature = "host-ext-tests")]
#[test]
fn the_declared_audio_ports_are_one_in_place_stereo_pair_that_the_plugin_then_processes() {
    use clack_extensions::audio_ports::{
        AudioPortFlags, AudioPortInfoBuffer, AudioPortType, PluginAudioPorts,
    };
    use support::{main_thread_handle, require_plugin_extension};

    let (_entry, mut instance) = instantiate_default();

    let ports = require_plugin_extension::<PluginAudioPorts>(&mut instance);
    let mut handle = main_thread_handle(&mut instance);
    let mut buffer = AudioPortInfoBuffer::new();

    for (is_input, direction, expected_name) in [
        (true, "input", b"Stereo In".as_slice()),
        (false, "output", b"Stereo Out".as_slice()),
    ] {
        assert_eq!(
            ports.count(&mut handle, is_input),
            1,
            "exactly one {direction} port is declared -- FR-CHAIN-060's Stereo row is one port of \
             two channels, not two ports of one"
        );

        {
            let info = ports
                .get(&mut handle, 0, is_input, &mut buffer)
                .unwrap_or_else(|| panic!("the {direction} port at index 0 must report itself"));

            assert_eq!(
                info.channel_count, 2,
                "the {direction} port must declare 2 channels ({info:?})"
            );
            assert_eq!(
                info.flags,
                AudioPortFlags::IS_MAIN,
                "the {direction} port must be the main one, and carry no other flag -- \
                 SUPPORTS_64BITS in particular would promise a 64-bit path src/audio.rs does not \
                 have ({info:?})"
            );
            assert_eq!(
                info.port_type,
                Some(AudioPortType::STEREO),
                "the {direction} port must be typed stereo, not left untyped, or a host has to \
                 guess the channel layout from the count alone ({info:?})"
            );
            assert_eq!(
                info.name, expected_name,
                "the {direction} port's display name is what a host shows in its routing UI \
                 ({info:?})"
            );
            assert_eq!(
                info.id.get(),
                0,
                "the {direction} port's id is stable at 0 across both directions -- the \
                 in_place_pair below refers to it ({info:?})"
            );
            assert_eq!(
                info.in_place_pair,
                Some(info.id),
                "the {direction} port must declare its in-place pair: the engine processes in \
                 place (StageIo's own contract), so a host is entitled to hand one buffer for both \
                 directions, and clap-validator's process-audio-basic-in-place test takes it up on \
                 that ({info:?})"
            );
        }

        for index in [1u32, 2, 7, u32::MAX - 1] {
            assert!(
                ports
                    .get(&mut handle, index, is_input, &mut buffer)
                    .is_none(),
                "there is no {direction} port at index {index}, so `get` must leave the host's \
                 buffer untouched and report failure -- writing a port there would contradict the \
                 count of 1 above"
            );
        }
    }

    // The declaration is only half the requirement: what the host then hands over on the strength
    // of it has to be processed. Same instance, same session -- the ports were read from exactly
    // the plugin now being driven. `handle`'s borrow of `instance` ends at its last use above, so
    // nothing has to be dropped by hand for the mutable re-borrow below.
    let out = run_stereo_blocks(&mut instance, distinct_right_channel);
    assert_declared_output_pair_was_written(&out);

    drop(instance); // `clap_plugin.destroy`
}

/// The behavioural half, un-gated so that a plain `cargo test -p namir-clap` (no `host-ext-tests`,
/// which is how `cargo test --workspace` runs) still exercises the negotiated configuration end to
/// end, and so the second finding below is checked in both feature configurations.
///
/// Two claims, both about the *one* configuration this plugin declares:
///
/// 1. Both declared output channels are written with a real signal, identical to each other
///    (FR-CHAIN-050).
/// 2. Channel 1's *input* does not reach the output at all. This is the shipped reading of
///    FR-CHAIN-060's Stereo row — `2 ch summed or L-only (FR-CHAIN-070)`, the second option — and
///    it is bit-exact rather than approximate: Gate runs upstream of Trim (D-9.8) and, at its
///    descriptor default of enabled, its bypass mix sits settled at exactly 1.0, so the dry term
///    carrying channel 1 is multiplied by exactly `1.0 - 1.0`. Pinned here because it is the one
///    observable difference between the two readings FR-CHAIN-060 permits, and nothing else in the
///    tree asserts which one a host actually gets.
#[test]
fn the_negotiated_stereo_pair_is_processed_left_only_as_fr_chain_060_permits() {
    let (_entry, mut instance) = instantiate_default();

    let with_signal = run_stereo_blocks(&mut instance, distinct_right_channel);
    assert_declared_output_pair_was_written(&with_signal);

    let with_silence = run_stereo_blocks(&mut instance, |dst| dst.fill(0.0));
    assert_declared_output_pair_was_written(&with_silence);

    assert_eq!(
        with_signal, with_silence,
        "channel 1 of the declared stereo input pair changed the output -- the shipped chain feeds \
         its mono core the left channel alone (FR-CHAIN-060's `L-only (FR-CHAIN-070)` option), \
         because Gate is enabled by default and overwrites channel 1 with channel 0's gated result \
         (crates/namir-engine/src/stages/gate.rs:163-172) before Trim's downmix can sum it \
         (crates/namir-engine/src/stages/trim.rs:145-156). If that is now intended, this test is \
         what has to change -- but the change of reading is a real one and FR-CHAIN-070's control \
         is the requirement that decides it"
    );

    drop(instance); // `clap_plugin.destroy`
}
