//! FR-CLAP-100: the CLAP `gui` extension — what this plugin negotiates with a host that wants to
//! embed an editor, and the full audio lifecycle of a host that declines a GUI entirely.
//!
//! Driven through the real C vtable by the shared `support` harness — **read that module's doc
//! comment first**, in particular the HAZARD about `start_library_scan` and the developer's real
//! library index. Nothing here starts a scan.
//!
//! # The requirement's two clauses, and which half this file can reach
//!
//! FR-CLAP-100 asks for two things: an embedded graphical editor via the CLAP GUI extension
//! *supporting the host embedding it*, and correct function *if the host declines to show a GUI at
//! all*. They are verified very differently.
//!
//! **The declined half is fully automatable and is the more important one to have automated**, so
//! it is the one this file asserts hardest. `TestHost` (`tests/support/mod.rs`) registers no
//! `HostGui` host-side extension in either feature state, and the two tests below that exercise it
//! never touch the plugin's `gui` extension at all — which is exactly the shape of a host that
//! declines a GUI. The claim proved is not merely "it does not crash": the plugin completes
//! instantiate → activate → process → deactivate → destroy and renders the probe tone, and the
//! samples it renders are **bit-identical** to those an instance that *did* go through the host's
//! GUI-opening sequence renders from the same input.
//!
//! **The embedding half cannot be finished in process, for two independent reasons.**
//!
//! 1. `set_parent` — the one call that actually embeds anything — takes a live native window
//!    handle. There is no host window in a `cargo test` binary, and manufacturing a fake `HWND`
//!    would prove nothing about embedding while risking a real `CreateWindowEx`/`SetParent` in a
//!    test process. `set_parent` is therefore deliberately **not called** from this file; the real
//!    embedding remains `docs/manual-tests/fr-clap-100-gui-embedding.md`'s script, run against
//!    Reaper. Everything the host does *before* `set_parent` in `clack_extensions::gui`'s own
//!    documented opening sequence (negotiate the API, `create`, `set_scale`, `get_size`,
//!    `can_resize`) and everything *after* it (`show`, `hide`, `destroy`) is driven here.
//! 2. `crates/namir-clap/src/gui.rs`'s `is_api_supported` accepts `GuiApiType::WIN32` only, with no
//!    `cfg` — so on macOS and Linux the host's negotiation fails and **no editor is ever
//!    embedded**.
//!    That gap is tracked as GitHub issue #18 ("CLAP plugin has no embedded editor on macOS or
//!    Linux (FR-CLAP-100)"), which lays out the three possible answers and their costs; it is not
//!    restated here, and this file does not attempt to close it.
//!
//! Hence the `// trace-partial:` below rather than a plain `// trace:` (D-23.1).
//!
//! # Why these assertions are not `cfg`-gated per platform
//!
//! Because the code under test is not either. `is_api_supported` has no `#[cfg]` in it, so the
//! answers it gives are the same on all three CI platforms: `win32`/embedded is accepted
//! everywhere, `cocoa` and `x11` are refused everywhere — *including on the platforms where they
//! are the only APIs the host can offer*. The matrix below therefore passes unchanged on Windows,
//! macOS and Linux, and its message text says plainly that on the latter two the answer it records
//! is issue #18's defect rather than a design boundary. Pinning the current answers is worth doing
//! for both: on Windows it is the negotiation contract `src/gui.rs`'s own safety argument depends
//! on (that module trusts `set_parent` to be reachable only for a configuration this plugin already
//! accepted), and on macOS/Linux it is a live, executing record of the gap — one that will fail
//! loudly the day someone adds a Cocoa or X11 backend without revisiting this requirement.

mod support;

use clack_host::prelude::PluginInstance;
use support::{
    CHANNELS, DEFAULT_SAMPLE_RATE, SINE_FREQ_HZ, StereoBuffers, TestHost, activate_default,
    all_finite, audio_section, fill_sine, instantiate_default, peak,
};

/// Block size the audio limbs run at. Small enough to stay cheap, large enough that per-block
/// overhead does not dominate.
const BLOCK: u32 = 256;

/// Blocks per run — 8192 frames, about 170 ms at 48 kHz, comfortably past the gate's attack and
/// every `GainLike` smoothing ramp in the chain.
const BLOCKS: u32 = 32;

/// Amplitude of the probe tone: -12 dBFS, far above the gate's -70 dBFS default threshold, so the
/// gate is open throughout.
const AMPLITUDE: f32 = 0.25;

/// The editor size `src/gui.rs` reports, restated here so a change to it is a visible test failure
/// rather than a silent one.
#[cfg(feature = "host-ext-tests")]
const EXPECTED_GUI_SIZE: clack_extensions::gui::GuiSize = clack_extensions::gui::GuiSize {
    width: 960,
    height: 640,
};

/// The one configuration `src/gui.rs` accepts: Win32, embedded (not floating).
#[cfg(feature = "host-ext-tests")]
const EMBEDDED_WIN32: clack_extensions::gui::GuiConfiguration<'static> =
    clack_extensions::gui::GuiConfiguration {
        api_type: clack_extensions::gui::GuiApiType::WIN32,
        is_floating: false,
    };

/// Activates `instance` at 48 kHz, runs a phase-continuous 1 kHz sine through it, deactivates, and
/// returns channel 0's output for the whole run.
///
/// Every block is asserted finite as it is produced, and the output is poisoned with `NaN` first,
/// so "the plugin did not write this block" fails here rather than becoming a confusing comparison
/// failure later. Leaves `instance` deactivated and ready to be dropped.
fn run_probe_tone(instance: &mut PluginInstance<TestHost>) -> Vec<f32> {
    let stopped = activate_default(instance);
    let mut processor = stopped.start_processing().expect("processing must start");

    let mut bufs = StereoBuffers::new(BLOCK as usize);
    let mut collected = Vec::with_capacity((BLOCK * BLOCKS) as usize);
    let mut done: u64 = 0;

    for _ in 0..BLOCKS {
        for channel in 0..CHANNELS {
            fill_sine(
                bufs.input_mut(channel),
                SINE_FREQ_HZ,
                DEFAULT_SAMPLE_RATE,
                AMPLITUDE,
                done,
            );
        }
        bufs.poison_output(f32::NAN);

        audio_section(|| bufs.process_block(&mut processor, BLOCK))
            .unwrap_or_else(|e| panic!("a {BLOCK}-frame block must process: {e}"));

        for channel in 0..CHANNELS {
            assert!(
                all_finite(bufs.output(channel)),
                "channel {channel} of the block starting at frame {done} is not finite -- the \
                 plugin either produced a non-finite sample or did not write the block"
            );
        }

        collected.extend_from_slice(bufs.output(0));
        done += u64::from(BLOCK);
    }

    let stopped = processor.stop_processing();
    instance.deactivate(stopped);
    collected
}

/// Asserts `rendered` is a settled rendering of the probe tone rather than silence or garbage.
///
/// Only the second half is measured for level: the first half covers the chain's warm-up, which is
/// legitimately quieter while the gate opens and the gain ramps settle.
fn assert_is_the_probe_tone(rendered: &[f32], what: &str) {
    assert_eq!(
        rendered.len(),
        (BLOCK * BLOCKS) as usize,
        "{what}: the run should have produced every frame it was asked for"
    );
    assert!(all_finite(rendered), "{what}: a non-finite sample");

    let settled = &rendered[rendered.len() / 2..];
    let settled_peak = peak(settled);
    assert!(
        settled_peak > AMPLITUDE * 0.5,
        "{what}: near-silence (peak {settled_peak}) from a -12 dBFS tone"
    );
}

/// The negotiation half of "supporting the host embedding it": what
/// `clap_plugin_gui.is_api_supported` answers for every windowing API a host can offer, and that
/// `create` agrees with it exactly.
///
/// The matrix is the whole of CLAP's standard API set in both float states, plus one unrecognised
/// string — a host is free to invent one, and a plugin that accepted it would be promising an
/// embedding it cannot perform.
///
/// **`cocoa` and `x11` reading `false` is issue #18, not a design boundary.** See this file's own
/// doc comment.
// trace-partial: FR-CLAP-100
// uncovered: FR-CLAP-100 — the embedded-editor clause on macOS and Linux, where
// uncovered: `is_api_supported` accepts only `GuiApiType::WIN32` with no `cfg` (issue #18), and
// uncovered: `set_parent`, whose real embedding needs a live host window and stays in
// uncovered: docs/manual-tests/fr-clap-100-gui-embedding.md; closes M8
#[cfg(feature = "host-ext-tests")]
#[test]
fn the_gui_extension_accepts_win32_embedded_and_refuses_every_other_windowing_api() {
    use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiError, PluginGui};
    use support::{main_thread_handle, require_plugin_extension};

    // (api, is_floating, is supported, why this row is what it is).
    let cases: [(GuiApiType<'static>, bool, bool, &str); 9] = [
        (
            GuiApiType::WIN32,
            false,
            true,
            "the one configuration src/gui.rs accepts",
        ),
        (
            GuiApiType::WIN32,
            true,
            false,
            "floating windows are refused on every platform -- src/gui.rs's set_transient says so \
             in as many words, and FR-CLAP-100 asks for an *embedded* editor",
        ),
        (
            GuiApiType::COCOA,
            false,
            false,
            "issue #18: refused even on macOS, where it is the only API a host can offer",
        ),
        (GuiApiType::COCOA, true, false, "issue #18, and floating"),
        (
            GuiApiType::X11,
            false,
            false,
            "issue #18: refused even on Linux, where it is the API a host offers",
        ),
        (GuiApiType::X11, true, false, "issue #18, and floating"),
        (
            GuiApiType::WAYLAND,
            false,
            false,
            "Wayland does not support embedding at all (clack_extensions::gui's own note), so \
             refusing it is correct on every platform",
        ),
        (GuiApiType::WAYLAND, true, false, "Wayland, and floating"),
        (
            GuiApiType(c"namir-not-a-windowing-api"),
            false,
            false,
            "an unrecognised api string a host is free to invent",
        ),
    ];

    let (_entry, mut instance) = instantiate_default();
    let gui = require_plugin_extension::<PluginGui>(&mut instance);
    let mut handle = main_thread_handle(&mut instance);

    for (api_type, is_floating, supported, why) in cases {
        let configuration = GuiConfiguration {
            api_type,
            is_floating,
        };

        assert_eq!(
            gui.is_api_supported(&mut handle, configuration),
            supported,
            "is_api_supported({api_type:?}, floating={is_floating}) -- {why}"
        );

        // `create` must not be more permissive than the negotiation that gates it: `src/gui.rs`'s
        // written safety argument for `set_parent`'s `unsafe` block depends on this plugin never
        // reaching an embedding path for an API it did not declare support for.
        let created = gui.create(&mut handle, configuration);
        assert_eq!(
            created.is_ok(),
            supported,
            "create({api_type:?}, floating={is_floating}) must agree with is_api_supported -- {why}"
        );
        match created {
            Ok(()) => gui.destroy(&mut handle),
            Err(e) => assert_eq!(
                e,
                GuiError::CreateError,
                "a refused create should report a create failure"
            ),
        }
    }
}

/// `get_preferred_api` is only a hint, but it must be a *self-consistent* one: a host that takes
/// the hint verbatim and hands it straight back to `is_api_supported` (and then to `create`) must
/// be accepted, or the plugin has advertised a configuration it will then refuse.
#[cfg(feature = "host-ext-tests")]
#[test]
fn the_preferred_api_is_win32_embedded_and_is_one_the_plugin_then_accepts() {
    use clack_extensions::gui::PluginGui;
    use support::{main_thread_handle, require_plugin_extension};

    let (_entry, mut instance) = instantiate_default();
    let gui = require_plugin_extension::<PluginGui>(&mut instance);
    let mut handle = main_thread_handle(&mut instance);

    let preferred = gui
        .get_preferred_api(&mut handle)
        .expect("the plugin must express a preferred GUI API");
    assert_eq!(
        preferred, EMBEDDED_WIN32,
        "src/gui.rs prefers Win32, embedded (issue #18: on macOS and Linux this is a preference no \
         host can satisfy)"
    );

    assert!(
        gui.is_api_supported(&mut handle, preferred),
        "the plugin must support the very configuration it says it prefers"
    );
    gui.create(&mut handle, preferred)
        .expect("the plugin must be able to create the GUI it says it prefers");
    gui.destroy(&mut handle);
}

/// The host's documented GUI-opening sequence (`clack_extensions::gui`'s module doc), driven
/// through the real vtable — **stopping short of `set_parent`**, which needs a live host window and
/// stays in `docs/manual-tests/fr-clap-100-gui-embedding.md`.
///
/// This is what a host does between `create` and `set_parent` to size and scale the editor, plus
/// the `show`/`hide`/`destroy` it does afterwards, and none of it may fail.
#[cfg(feature = "host-ext-tests")]
#[test]
fn the_host_side_embedding_sequence_runs_up_to_and_after_set_parent() {
    use clack_extensions::gui::PluginGui;
    use support::{main_thread_handle, require_plugin_extension};

    let (_entry, mut instance) = instantiate_default();
    let gui = require_plugin_extension::<PluginGui>(&mut instance);
    let mut handle = main_thread_handle(&mut instance);

    gui.create(&mut handle, EMBEDDED_WIN32)
        .expect("create must succeed for the negotiated configuration");

    // Embedded, Win32: the host sets scaling, then asks whether it may choose a size.
    gui.set_scale(&mut handle, 1.5)
        .expect("set_scale must be accepted for an embedded Win32 editor");
    assert!(
        !gui.can_resize(&mut handle),
        "src/gui.rs reports a fixed-size editor (FR-CLAP-110 is out of scope), so a host must not \
         be told it may resize"
    );
    assert_eq!(
        gui.get_size(&mut handle),
        Some(EXPECTED_GUI_SIZE),
        "a non-resizable editor must report the size the host should give its parent window"
    );

    // `set_parent` would go here. See this file's doc comment.

    gui.show(&mut handle).expect("show must succeed");
    gui.hide(&mut handle).expect("hide must succeed");
    gui.destroy(&mut handle);

    // `destroy` is idempotent in `src/gui.rs` (`self.window.take()`), and a host that calls it on a
    // GUI it never parented must not be punished for it.
    gui.destroy(&mut handle);
}

/// FR-CLAP-100's second clause: a host that declines to show a GUI at all.
///
/// `TestHost` registers no `HostGui` extension in either feature state and this test never reaches
/// for the plugin's `gui` extension, so the plugin's editor is never created, parented or shown.
/// Under those conditions the plugin must still complete the whole audio lifecycle and render the
/// probe tone — which is the half of the requirement that is *met*, on every platform, and the one
/// worth having a machine re-check on every commit.
///
/// Deliberately **not** feature-gated: this is the case a plain `cargo test -p namir-clap` should
/// cover too, and it needs no host-side extension to state.
#[test]
fn a_host_that_declines_a_gui_completes_a_full_audio_lifecycle() {
    let (_entry, mut instance) = instantiate_default();

    let rendered = run_probe_tone(&mut instance);
    assert_is_the_probe_tone(&rendered, "an instance whose host never asked for a GUI");

    // The instance's own destroy, on the thread that created it (`clap_plugin.destroy`).
    drop(instance);
}

/// The sharper form of the same clause: declining the GUI must not merely avoid crashing, it must
/// make **no difference to the audio**.
///
/// One instance is driven exactly as
/// [`a_host_that_declines_a_gui_completes_a_full_audio_lifecycle`] drives it — the `gui` extension
/// never touched. A second is put through the host's whole GUI-opening sequence first (short of
/// `set_parent`), left with its editor "open" across the entire audio run, and only destroyed
/// afterwards. The two renderings are compared sample for sample.
///
/// Bit-exact, not within a tolerance: nothing in either path is a source of numerical difference,
/// so any difference at all would mean GUI state had reached the signal path — which is precisely
/// what "functions correctly if the host declines a GUI" forbids, in the direction a tolerance
/// would hide.
#[cfg(feature = "host-ext-tests")]
#[test]
fn declining_the_gui_renders_bit_identical_audio_to_opening_it() {
    use clack_extensions::gui::PluginGui;
    use support::{main_thread_handle, require_plugin_extension};

    let (_entry, mut declined) = instantiate_default();
    let declined_output = run_probe_tone(&mut declined);
    drop(declined);
    assert_is_the_probe_tone(&declined_output, "the GUI-declined instance");

    let (_entry, mut opened) = instantiate_default();
    {
        let gui = require_plugin_extension::<PluginGui>(&mut opened);
        let mut handle = main_thread_handle(&mut opened);
        gui.create(&mut handle, EMBEDDED_WIN32)
            .expect("create must succeed for the negotiated configuration");
        gui.set_scale(&mut handle, 1.0)
            .expect("set_scale must be accepted");
        gui.show(&mut handle).expect("show must succeed");
    }
    let opened_output = run_probe_tone(&mut opened);
    {
        let gui = require_plugin_extension::<PluginGui>(&mut opened);
        let mut handle = main_thread_handle(&mut opened);
        gui.hide(&mut handle).expect("hide must succeed");
        gui.destroy(&mut handle);
    }
    drop(opened);

    assert_eq!(
        declined_output.len(),
        opened_output.len(),
        "both runs must render the same number of frames"
    );
    let differing = declined_output
        .iter()
        .zip(&opened_output)
        .position(|(a, b)| a.to_bits() != b.to_bits());
    assert!(
        differing.is_none(),
        "declining the GUI changed the audio: first difference at frame {} ({} vs {})",
        differing.unwrap_or_default(),
        declined_output[differing.unwrap_or_default()],
        opened_output[differing.unwrap_or_default()]
    );
}
