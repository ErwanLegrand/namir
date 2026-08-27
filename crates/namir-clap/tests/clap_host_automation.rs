//! **FR-CLAP-060's sample-accuracy limb**: "the host's bypass is sample-accurate and click-free,
//! equivalent to FR-CHAIN-030", driven through the real C vtable with real
//! `clap_event_param_value` events carrying real sample offsets.
//!
//! **Before writing a test against `support`, read that module's doc comment — in particular the
//! HAZARD about `start_library_scan` and the developer's real library index.** Nothing here starts
//! a scan, loads a resource or touches the file system at all.
//!
//! # What "sample-accurate" is taken to mean, and how it is measured
//!
//! CLAP's `clap_event_header.time` is the frame, relative to the start of this `process()` call, at
//! which the event takes effect. A plugin honours it by *splitting the block* there: everything
//! before frame `t` is rendered with the old parameter value and everything from frame `t` onward
//! with the new one. So the assertion is two-sided, against a reference run of the identical
//! instance through the identical block sequence that never sees an event at all:
//!
//! - frames `[0, t)` must match the reference **sample for sample** — nothing may change early;
//! - frame `t` must differ from it — something must change by then, and not one frame later.
//!
//! Bit-equality is the right bar for the first half and not an over-tight one: both sides come from
//! the same deterministic engine running the same arithmetic on the same input in the same process,
//! and the whole question is *where* the switch landed. The pre-M14 behaviour — apply every event
//! before the block, never reading `header().time()` — fails the first half by `t` frames.
//!
//! **Deliberately stated as "differs", not as "equals the input".** Today `Chain::set_global_bypass`
//! flips a `bool`, so the post-event frames *are* the input exactly; that is asserted, separately,
//! by [`a_bypassed_block_is_unity_gain_passthrough_not_the_processed_signal`], which is the test a
//! future click-free crossfade has to revisit. The tagged test above it is written so that it does
//! not have to: a crossfade that *begins* at frame `t` satisfies it unchanged, and one that begins
//! anywhere else does not.
//!
//! # Why global bypass is the parameter under test rather than trim gain
//!
//! `global.bypass` reaches `namir_engine::Chain::apply`, which flips one `bool` and takes effect on
//! the very next sample — so the transition's position is exactly readable. Every continuous
//! parameter in `REGISTRY` is declared `SmoothingCategory::GainLike` and ramps over ~20 ms, which
//! spreads a mis-timed application across hundreds of frames and would make a single-frame error
//! indistinguishable from the ramp itself. Testing the crisp parameter is what gives the assertion
//! its resolution; the block-splitting machinery it exercises (`src/audio.rs`'s `process`) is the
//! same for every parameter.
//!
//! Input trim is still involved, at +6 dB: with **nothing loaded** the six-stage chain is very
//! nearly unity, so a bypassed block and a processed block would be almost the same buffer and the
//! test would assert nothing. Driving trim to +6 dB first — and letting its ramp settle — makes the
//! processed signal about twice the bypassed one, so the switch is a factor-of-two step.
//!
//! # Issue #36, answered from this side (M14)
//!
//! `namir_engine::Chain::process` has a documented fallback: when `global_bypass` is set but
//! `prepare_crosscutting` was never called, it **runs every stage anyway**. That state is
//! unreachable from either product shell — both build their chain through
//! `namir_engine::build_default_engine` → `build_default_chain`, whose last statement before
//! returning is `chain.prepare_crosscutting(ctx)` — and this file asserts it from the plugin's own
//! side rather than by reading that code: a bypassed block here is required to equal the *input*,
//! and under the fallback it would equal the input at +6 dB.
//!
//! # What this file does not cover
//!
//! **Click-freedom.** The transition this file locates to the frame is a genuine discontinuity:
//! `Chain::set_global_bypass` flips a `bool` with no crossfade, where every *per-stage* bypass in
//! the chain (`GateStage`'s `mix`/`mix_target`/`mix_coeff`, FR-CHAIN-020) fades over 15 ms. Its
//! magnitude is recorded here as a *measurement*, not as an approval — see
//! [`the_bypass_transition_is_a_single_sample_step_today`] — and the fix belongs in `namir-engine`,
//! not in this crate. The tagged test's `uncovered:` field says so.

mod support;

use clack_host::events::event_types::ParamValueEvent;
use clack_host::events::io::EventBuffer;
use clack_host::events::{Match, Pckn};
use clack_host::prelude::{ClapId, PluginEntry, PluginInstance, StartedPluginAudioProcessor};
use clack_host::utils::Cookie;
use namir_params::ParamDescriptor;

use support::{
    CHANNELS, DEFAULT_MAX_BLOCK, DEFAULT_SAMPLE_RATE, SINE_FREQ_HZ, StereoBuffers, TestHost,
    activate, all_finite, audio_section, config, fill_sine, instantiate_default, peak,
};

/// Frames per block, everywhere in this file. The harness's own maximum, so `StereoBuffers` never
/// reallocates and every processing call stays inside an [`audio_section`].
const BLOCK: u32 = DEFAULT_MAX_BLOCK;

/// Probe amplitude. -12 dBFS, far above the gate's -70 dBFS default threshold so the gate is open
/// throughout, and low enough that +6 dB of trim (0.5 peak) stays well under FR-CHAIN-090's 0 dBFS
/// output ceiling — a clamped signal would make the two sides of the step incomparable.
const AMPLITUDE: f32 = 0.25;

/// Phase offset, in frames, of the probe tone.
///
/// Not zero, and that is load-bearing twice over. `sin(0) == 0`, so a tone starting at phase zero
/// makes frame 0 identical whether it was processed or bypassed, and the one-frame split offset
/// would prove nothing. And at 48 kHz a 1 kHz tone is 48 frames per cycle, so 12 frames is a
/// quarter cycle: frame 0 sits exactly on a peak, and so does every frame at a multiple of 48 —
/// which is what [`the_bypass_transition_is_a_single_sample_step_today`] places its event on.
const PHASE_FRAMES: u64 = 12;

/// The trim setting that makes a processed block distinguishable from a bypassed one. +6 dB is a
/// factor of two: far outside any tolerance, and still inside the ceiling at [`AMPLITUDE`].
const TRIM_GAIN_DB: f32 = 6.0;

/// Blocks processed after the trim change and before any measurement. `SmoothingCategory::GainLike`
/// ramps over ~20 ms — under two blocks at 48 kHz/512 — and the gate's own attack and bypass
/// crossfade are shorter still, so eight blocks (85 ms) leaves the chain settled well before
/// anything is compared.
const WARMUP_BLOCKS: usize = 8;

/// The offsets within a block at which the event under test is placed. Chosen to include both
/// boundaries a splitter can get wrong (`0` — the whole block is post-event; `BLOCK - 1` — exactly
/// one frame is), a mid-block value, and values that are multiples of nothing in particular.
const SPLIT_OFFSETS: [u32; 6] = [0, 1, 37, 256, 300, BLOCK - 1];

/// One `clap_event_param_value` for `descriptor` at frame `time`, targeting every port/channel/
/// key/note the way a host's own automation does.
fn param_event(descriptor: &ParamDescriptor, value: f64, time: u32) -> ParamValueEvent {
    ParamValueEvent::new(
        time,
        ClapId::new(descriptor.id.0),
        Pckn::new(Match::All, Match::All, Match::All, Match::All),
        value,
        Cookie::empty(),
    )
}

/// A single-event buffer, since that is what almost every run here needs.
fn one_event(descriptor: &ParamDescriptor, value: f64, time: u32) -> EventBuffer {
    let mut events = EventBuffer::with_capacity(4);
    events.push(&param_event(descriptor, value, time));
    events
}

/// A live, activated, started instance plus the buffers to drive it — everything a run in this file
/// needs, built the same way every time so two runs differ only in the events they deliver.
struct Rig {
    instance: PluginInstance<TestHost>,
    processor: Option<StartedPluginAudioProcessor<TestHost>>,
    bufs: StereoBuffers,
    /// Kept alive for the life of the instance; dropping it early would be pointless churn.
    _entry: PluginEntry,
}

impl Rig {
    /// Instantiates, activates at 48 kHz/512, fills both input channels with a 1 kHz tone at
    /// [`PHASE_FRAMES`], drives trim to [`TRIM_GAIN_DB`] and warms up.
    ///
    /// The trim event is delivered at frame 0 of its own block, so this setup does not itself
    /// depend on the block-splitting behaviour under test.
    fn new() -> Self {
        let (entry, mut instance) = instantiate_default();
        let stopped = activate(&mut instance, config(DEFAULT_SAMPLE_RATE, 1, BLOCK));
        let processor = stopped.start_processing().expect("processing must start");

        let mut bufs = StereoBuffers::new(BLOCK as usize);
        for channel in 0..CHANNELS {
            fill_sine(
                bufs.input_mut(channel),
                SINE_FREQ_HZ,
                DEFAULT_SAMPLE_RATE,
                AMPLITUDE,
                PHASE_FRAMES,
            );
        }

        let mut rig = Self {
            instance,
            processor: Some(processor),
            bufs,
            _entry: entry,
        };

        rig.run_block(&one_event(
            &namir_params::stages::trim::GAIN_DB,
            f64::from(TRIM_GAIN_DB),
            0,
        ));
        for _ in 0..WARMUP_BLOCKS {
            rig.run_block(&EventBuffer::new());
        }
        rig
    }

    /// Processes one [`BLOCK`]-frame block with `events` delivered to it, and returns channel 0's
    /// output.
    ///
    /// The input is the *same* buffer every call, deliberately: the tone is not advanced between
    /// blocks, so two runs that differ only in their events see byte-identical input and the
    /// comparison measures the events alone. The chain's own state does advance, which is why
    /// every run in this file is a fresh [`Rig`] driven through the identical block sequence.
    fn run_block(&mut self, events: &EventBuffer) -> Vec<f32> {
        let frames = BLOCK as usize;
        let processor = self
            .processor
            .as_mut()
            .expect("the processor is taken only by `finish`");
        self.bufs.poison_output(f32::NAN);
        audio_section(|| {
            self.bufs
                .process_block_with_events(processor, BLOCK, &events.as_input())
        })
        .expect("a block must process");

        for channel in 0..CHANNELS {
            assert!(
                all_finite(&self.bufs.output(channel)[..frames]),
                "channel {channel} is not finite -- the plugin either emitted a non-finite \
                 sample or did not write the block"
            );
        }
        self.bufs.output(0)[..frames].to_vec()
    }

    /// Channel 0's input, which several assertions here compare against.
    fn input(&self) -> Vec<f32> {
        self.bufs.input(0)[..BLOCK as usize].to_vec()
    }

    /// Stops, deactivates and destroys — the order `support`'s own contract requires.
    fn finish(mut self) {
        let processor = self.processor.take().expect("finish runs once");
        self.instance.deactivate(processor.stop_processing());
    }
}

/// The index of the first frame at which `a` and `b` differ, or `None` if they are equal.
fn first_difference(a: &[f32], b: &[f32]) -> Option<usize> {
    a.iter().zip(b.iter()).position(|(x, y)| x != y)
}

/// FR-CLAP-060's sample-accuracy limb, at six offsets including both boundaries: the block is split
/// at the event's own frame, neither earlier nor later. See this file's doc comment for the shape
/// of the two-sided assertion and for why it is stated as "differs" rather than "equals the input".
// trace-partial: FR-CLAP-060
// uncovered: FR-CLAP-060 — the click-free limb is unspanned, and it is a live defect rather than
// uncovered: missing coverage: namir_engine::Chain::set_global_bypass flips a bool with no
// uncovered: crossfade, so the transition this file locates to the frame completes in one sample
// uncovered: (measured at ~0.5x the settled peak, by this file's last test) where FR-CHAIN-020's
// uncovered: per-stage bypass fades over 15 ms; the fix belongs to Chain, in namir-engine, and
// uncovered: this crate cannot make it; closes M8
#[test]
fn host_bypass_automation_takes_effect_at_the_event_s_own_frame() {
    for split in SPLIT_OFFSETS {
        let at = split as usize;

        // Reference: the identical rig and the identical block, with no event at all.
        let mut reference_rig = Rig::new();
        let reference = reference_rig.run_block(&EventBuffer::new());
        reference_rig.finish();

        // Under test: the same again, with the bypass event at frame `split`.
        let mut rig = Rig::new();
        let switched = rig.run_block(&one_event(&namir_params::global::GLOBAL_BYPASS, 1.0, split));
        rig.finish();

        assert_eq!(
            first_difference(&switched[..at], &reference[..at]),
            None,
            "with the bypass event at frame {split}, every frame before it must render exactly as \
             it does with no event at all -- the plugin applied the change early, which is what \
             applying automation once before the block does"
        );
        assert_ne!(
            switched[at], reference[at],
            "with the bypass event at frame {split}, frame {split} itself must already reflect the \
             change -- the plugin applied it late"
        );
    }
}

/// Two bypass events in one block: the block must be split twice, so the middle segment is
/// bypassed and both outer segments are not.
///
/// A splitter that honours only the first event, or that applies both before the block, fails this
/// where the single-event test above could still pass.
#[test]
fn two_automation_points_in_one_block_split_it_twice() {
    const ON_AT: usize = 100;
    const OFF_AT: usize = 300;

    let mut reference_rig = Rig::new();
    let reference = reference_rig.run_block(&EventBuffer::new());
    let input = reference_rig.input();
    reference_rig.finish();

    let mut rig = Rig::new();
    let mut events = EventBuffer::with_capacity(4);
    events.push(&param_event(
        &namir_params::global::GLOBAL_BYPASS,
        1.0,
        ON_AT as u32,
    ));
    events.push(&param_event(
        &namir_params::global::GLOBAL_BYPASS,
        0.0,
        OFF_AT as u32,
    ));
    let switched = rig.run_block(&events);
    rig.finish();

    assert_eq!(
        first_difference(&switched[..ON_AT], &reference[..ON_AT]),
        None,
        "frames before the first event must render as they do with no events"
    );
    assert_eq!(
        first_difference(&switched[ON_AT..OFF_AT], &input[ON_AT..OFF_AT]),
        None,
        "frames between the two events must be the input at unity gain (FR-CHAIN-030)"
    );
    // After the bypass is released the stages run again, on state they did not advance while
    // bypassed, so this segment is not comparable to `reference` sample for sample. What is
    // assertable -- and is exactly what the second event is for -- is that it stops being the
    // input, from its own first frame.
    assert_ne!(
        switched[OFF_AT], input[OFF_AT],
        "the frame the second event names must already have the bypass released"
    );
}

/// FR-CHAIN-030's passthrough, and **issue #36 answered from the plugin's own side**: a bypassed
/// block is the input at unity gain, not the processed signal.
///
/// `namir_engine::Chain::process`'s no-`prepare_crosscutting` fallback runs every stage while
/// nominally bypassed. If a product path ever reached that state this block would come out at
/// +6 dB, so this is the assertion that the shipped path does not — see this file's doc comment.
///
/// **This is the test a click-free global bypass has to revisit**, since a crossfade would make the
/// first few hundred frames after the switch a blend rather than the input exactly. The tagged test
/// above is written not to need revisiting.
#[test]
fn a_bypassed_block_is_unity_gain_passthrough_not_the_processed_signal() {
    let mut rig = Rig::new();
    let input = rig.input();

    let bypassed = rig.run_block(&one_event(&namir_params::global::GLOBAL_BYPASS, 1.0, 0));
    let processed = rig.run_block(&one_event(&namir_params::global::GLOBAL_BYPASS, 0.0, 0));
    rig.finish();

    assert_eq!(
        first_difference(&bypassed, &input),
        None,
        "a bypassed block must be the input at unity gain (FR-CHAIN-030); it is not, which is \
         what `Chain::process`'s no-crosscutting fallback -- running every stage while nominally \
         bypassed -- would produce"
    );

    let peak_bypassed = peak(&bypassed);
    let peak_processed = peak(&processed);
    assert!(
        peak_processed > peak_bypassed * 1.5,
        "the processed block (peak {peak_processed}) should be about twice the bypassed one \
         (peak {peak_bypassed}) at +{TRIM_GAIN_DB} dB of trim; if they match, the chain is not \
         actually processing and the passthrough assertion above proves nothing"
    );
}

/// **A measurement, not an approval**: FR-CLAP-060's click-free limb is not met today, and this
/// records by how much, so a later fix has a before-figure to move.
///
/// The event is placed on a peak of the probe tone ([`PHASE_FRAMES`]'s own doc comment), where the
/// signal's genuine sample-to-sample movement is at its smallest and the whole of the observed jump
/// is the bypass. Today the transition completes in one sample, so the jump is the full difference
/// between the processed and bypassed renderings — about half the settled peak. A 15 ms one-pole
/// crossfade of the kind `GateStage` already runs would move roughly 1/720 of that in the first
/// sample, three orders of magnitude under the bound below.
#[test]
fn the_bypass_transition_is_a_single_sample_step_today() {
    /// A multiple of the tone's 48-frame period, so the event lands on a peak.
    const AT: usize = 240;

    let mut reference_rig = Rig::new();
    let reference = reference_rig.run_block(&EventBuffer::new());
    reference_rig.finish();

    let mut rig = Rig::new();
    let switched = rig.run_block(&one_event(
        &namir_params::global::GLOBAL_BYPASS,
        1.0,
        AT as u32,
    ));
    rig.finish();

    let settled_peak = peak(&reference);
    let jump = (switched[AT] - reference[AT]).abs();
    assert!(
        jump > settled_peak * 0.1,
        "the bypass transition no longer completes in one sample (it moved {jump} against a \
         settled peak of {settled_peak}). If `namir_engine::Chain` has learned FR-CHAIN-020's \
         crossfade, this test has done its job: delete it, revisit \
         `a_bypassed_block_is_unity_gain_passthrough_not_the_processed_signal`, and promote \
         FR-CLAP-060's tag rather than loosening the bound"
    );
}
