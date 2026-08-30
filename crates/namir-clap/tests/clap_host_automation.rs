//! **FR-CLAP-060, both limbs**: "the host's bypass is sample-accurate and click-free, equivalent
//! to FR-CHAIN-030", driven through the real C vtable with real `clap_event_param_value` events
//! carrying real sample offsets. Sample accuracy is located to the frame at six offsets; click
//! freedom is measured as a blend trajectory, in both directions, against the same 15 ms linear
//! ramp bound `namir-engine`'s own per-stage bypass tests use.
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
//! **Deliberately stated as "differs", not as "equals the input".** That was written before issue
//! #142, when `Chain::set_global_bypass` flipped a `bool` and the post-event frames *were* the
//! input exactly — and it is why the tagged test needed no revisiting when the crossfade landed: a
//! fade that *begins* at frame `t` satisfies "differs at `t`, matches before it" unchanged, and one
//! that begins anywhere else does not. The bypassed frames are now the input exactly only once the
//! fade has settled, which is what
//! [`a_bypassed_block_is_unity_gain_passthrough_not_the_processed_signal`] waits for.
//!
//! # Why global bypass is the parameter under test rather than trim gain
//!
//! First, because FR-CLAP-060 is a requirement about this parameter and no other. Second, because
//! its position is the most exactly readable: `global.bypass` reaches `namir_engine::Chain::apply`,
//! whose effect — a `bool` before issue #142, that blend's *target* since — begins on the very next
//! sample, and the blend's range is the whole difference between a processed and a bypassed block,
//! so even its first frame departs from the reference by about a 720th of a factor of two. Every
//! continuous parameter in `REGISTRY` is declared `SmoothingCategory::GainLike` and ramps over
//! ~20 ms from wherever it happens to be, which is both slower and, at a small parameter change,
//! arbitrarily close to no departure at all. The block-splitting machinery this exercises
//! (`src/audio.rs`'s `process`) is the same for every parameter.
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
//! # How click-freedom is measured (issue #142)
//!
//! Until #142 the transition this file locates to the frame was a genuine discontinuity:
//! `Chain::set_global_bypass` flipped a `bool` with no crossfade, where every *per-stage* bypass in
//! the chain (`GateStage`'s `mix`/`mix_target`/`mix_coeff`, FR-CHAIN-020) faded over 15 ms. The
//! global bypass — the one a host actually automates — was the only one that stepped. It now runs
//! the same 15 ms one-pole blend, and [`the_bypass_transition_is_a_crossfade_not_a_step`] is what
//! `the_bypass_transition_is_a_single_sample_step_today` became: the same event at the same
//! frame, with the bound inverted from "moves more than 10% of the settled peak in one sample" to
//! "never moves more than a linear 15 ms ramp would, in either direction".
//!
//! What makes that measurable from the host's side is that **the reference run is the wet signal**.
//! While a fade is in flight the chain runs its stages in both runs, on the same input, from the
//! same state, so the no-event reference is exactly the wet term of the blend; the input is the dry
//! term (this file loads nothing, so the chain reports zero latency and the compensation delay is
//! zero); and the blend position itself falls out by division — see [`inferred_mix`]. Every
//! assertion about the fade's *shape* is made on that inferred trajectory rather than on the audio,
//! so the tone's own slew never has to be subtracted from a click.
//!
//! # What this file does not cover
//!
//! **Bypass at a nonzero chain latency.** Nothing here loads a model or an IR, so the chain
//! reports zero latency throughout and FR-CHAIN-030's compensation delay is never exercised from
//! the plugin's side. That half is the engine's own — `namir_engine::chain`'s null test at
//! latencies 0/3/97, and `chain_probes`' resampled-model probe — and FR-CLAP-060 is a statement
//! about the adapter delivering the host's bypass to that mechanism, not a second copy of it.

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

/// Blocks rendered after a bypass change before the chain is treated as settled on the far side of
/// it. `namir_engine::chain`'s blend is a 15 ms one-pole that takes its last step outright once the
/// remainder is one ordinary step wide, which is about 100 ms — 9.4 blocks at 48 kHz/512, so twelve
/// leaves margin without making the tests slow.
const CROSSFADE_SETTLE_BLOCKS: usize = 12;

/// The per-sample bound every fade in this file is held to: the movement a **linear** ramp over the
/// blend's full range would make in one frame, at FR-CHAIN-020's 15 ms. Expressed on the blend
/// position itself (0 to 1) rather than on audio, so it is the same number whatever the signal is
/// doing. `namir_engine`'s `stages/gate.rs` holds its own per-stage bypass to the identical bound,
/// in the same words; a one-pole of the same time constant clears it by about 0.1%.
const MAX_MIX_STEP_PER_FRAME: f32 = 1.0 / (0.015 * DEFAULT_SAMPLE_RATE as f32);

/// How far apart the dry and wet signals must be at a frame for [`inferred_mix`] to divide by their
/// difference. The probe tone crosses zero 24 times a block and both sides go with it; a twentieth
/// of [`AMPLITUDE`] keeps the ~13% of frames nearest each crossing out of the arithmetic.
const MIX_INFERENCE_FLOOR: f32 = AMPLITUDE / 20.0;

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

/// The blend position `namir_engine::chain`'s bypass crossfade must have been at, implied by one
/// frame of a run against its no-event reference. `0.0` = the chain's own output, `1.0` = the input
/// passed through.
///
/// `out = wet * (1 - m) + dry * m`, where the reference run *is* `wet` (both runs render the same
/// stages on the same input from the same state for as long as the fade is in flight, and once it
/// settles the bypassed run is the input exactly, which the same formula still reports as `1.0`)
/// and the input *is* `dry` (nothing is loaded here, so the chain reports zero latency and
/// FR-CHAIN-030's compensation delay is zero). Inverting it recovers `m`.
///
/// `None` where the two sides are too close for the division to carry information — see
/// [`MIX_INFERENCE_FLOOR`]. Nothing is asserted at those frames rather than something weak being
/// asserted at all of them.
fn inferred_mix(switched: f32, reference: f32, input: f32) -> Option<f32> {
    let range = input - reference;
    (range.abs() > MIX_INFERENCE_FLOOR).then(|| (switched - reference) / range)
}

/// Every frame of `switched` at which [`inferred_mix`] has an answer, as `(frame, mix)`.
fn mix_trajectory(switched: &[f32], reference: &[f32], input: &[f32]) -> Vec<(usize, f32)> {
    switched
        .iter()
        .zip(reference)
        .zip(input)
        .enumerate()
        .filter_map(|(frame, ((switched, reference), input))| {
            inferred_mix(*switched, *reference, *input).map(|mix| (frame, mix))
        })
        .collect()
}

/// Asserts that no frame of `trajectory` moves the blend further than [`MAX_MIX_STEP_PER_FRAME`]
/// allows, scaling the allowance by the gap where frames were skipped for being too near a zero
/// crossing. Returns the total distance travelled, so a caller can also check the fade went
/// somewhere — a blend that never moves passes every step bound trivially.
fn assert_no_step_exceeds_a_15ms_ramp(trajectory: &[(usize, f32)], what: &str) -> f32 {
    let mut travelled = 0.0f32;
    for pair in trajectory.windows(2) {
        let (previous_frame, previous_mix) = pair[0];
        let (frame, mix) = pair[1];
        let step = (mix - previous_mix).abs();
        travelled += step;
        let allowed = (frame - previous_frame) as f32 * MAX_MIX_STEP_PER_FRAME * 1.01;
        assert!(
            step <= allowed,
            "{what}: the blend moved {step} between frames {previous_frame} and {frame}, past the \
             {allowed} a linear 15 ms ramp would -- the bypass is stepping, not fading"
        );
    }
    travelled
}

/// FR-CLAP-060's sample-accuracy limb, at six offsets including both boundaries: the block is split
/// at the event's own frame, neither earlier nor later. See this file's doc comment for the shape
/// of the two-sided assertion and for why it is stated as "differs" rather than "equals the input".
///
/// **The tag is plain, and it rests on two tests, not this one alone.** FR-CLAP-060 asks for a
/// bypass that is "sample-accurate *and* click-free"; this test is the first half and
/// [`the_bypass_transition_is_a_crossfade_not_a_step`], below, is the second. It was a
/// `trace-partial:` until issue #142, because the click-free half was not merely untested but
/// unmet — `Chain::set_global_bypass` flipped a `bool` — and the `uncovered:` field said so. The
/// fade landed in `namir-engine`, the measurement below inverted its bound, and the ledger entry
/// was retired by closing the gap it named rather than by promoting the tag over it.
// trace-partial: FR-CLAP-060
// uncovered: FR-CLAP-060 — the click-free and sample-accuracy limbs are both executed here, but
// uncovered: the requirement asks for bypass "equivalent to FR-CHAIN-030", and FR-CHAIN-030's own
// uncovered: content is the null against the *delayed* input: nothing in this file loads a model,
// uncovered: so the plugin's bypass is only ever observed at zero chain latency, and the
// uncovered: compensation limb is exercised engine-side alone; closes M8
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

/// Two bypass events in one block: the block must be split twice, so the middle segment fades
/// toward the input and the segment after it turns around and fades back.
///
/// A splitter that honours only the first event, or that applies both before the block, fails this
/// where the single-event test above could still pass.
///
/// **Rewritten for issue #142.** The middle segment used to be required to *be* the input, which
/// was true only while bypass was a one-sample switch; 200 frames is a quarter of the blend's time
/// constant, so it is now a rising blend rather than a settled one. What the two events produce is
/// a turning point, and the turning point is what is asserted: the inferred blend position rises
/// strictly across the first segment and falls strictly across the second, with the turn at the
/// second event's own frame. That is a stronger statement about the split than the old one — a
/// second event applied one block late, or not at all, leaves the trajectory rising to the end.
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

    // The stages run for the whole block in both runs -- 400 frames is nowhere near enough of the
    // blend for the bypassed side to take over and stop them -- so the reference is the wet term
    // throughout and `inferred_mix` is exact on both segments.
    let trajectory = mix_trajectory(&switched, &reference, &input);
    let rising: Vec<(usize, f32)> = trajectory
        .iter()
        .copied()
        .filter(|(frame, _)| (ON_AT..OFF_AT).contains(frame))
        .collect();
    let falling: Vec<(usize, f32)> = trajectory
        .iter()
        .copied()
        .filter(|(frame, _)| *frame >= OFF_AT)
        .collect();

    assert!(
        rising
            .first()
            .expect("the first segment has frames to measure")
            .1
            > 0.0,
        "the blend must have started moving by the first measurable frame after the first event"
    );
    for pair in rising.windows(2) {
        assert!(
            pair[1].1 > pair[0].1,
            "frames between the two events must fade *toward* the input: the blend fell from {} \
             at frame {} to {} at frame {}",
            pair[0].1,
            pair[0].0,
            pair[1].1,
            pair[1].0
        );
    }
    for pair in falling.windows(2) {
        assert!(
            pair[1].1 < pair[0].1,
            "frames after the second event must fade back: the blend rose from {} at frame {} to \
             {} at frame {}",
            pair[0].1,
            pair[0].0,
            pair[1].1,
            pair[1].0
        );
    }
    assert!(
        falling
            .first()
            .expect("the second segment has frames to measure")
            .1
            < rising
                .last()
                .expect("the first segment has frames to measure")
                .1,
        "the turn must happen at the second event's own frame, not later"
    );
}

/// FR-CHAIN-030's passthrough, and **issue #36 answered from the plugin's own side**: a bypassed
/// block is the input at unity gain, not the processed signal.
///
/// `namir_engine::Chain::process`'s no-`prepare_crosscutting` fallback runs every stage while
/// nominally bypassed. If a product path ever reached that state this block would come out at
/// +6 dB, so this is the assertion that the shipped path does not — see this file's doc comment.
///
/// **Revisited for issue #142, as its own doc comment predicted it would have to be.** The block
/// carrying the event is now a blend rather than the input, so the assertion moved to the far side
/// of the fade: a *settled* bypass must be the input exactly. That is not a weaker claim — it is
/// the one FR-CHAIN-030 makes, and `namir_engine::chain`'s blend snaps to its endpoint precisely so
/// "exactly" stays true rather than becoming "to within about 2e-5".
#[test]
fn a_bypassed_block_is_unity_gain_passthrough_not_the_processed_signal() {
    let mut rig = Rig::new();
    let input = rig.input();

    rig.run_block(&one_event(&namir_params::global::GLOBAL_BYPASS, 1.0, 0));
    for _ in 0..CROSSFADE_SETTLE_BLOCKS {
        rig.run_block(&EventBuffer::new());
    }
    let bypassed = rig.run_block(&EventBuffer::new());

    rig.run_block(&one_event(&namir_params::global::GLOBAL_BYPASS, 0.0, 0));
    for _ in 0..CROSSFADE_SETTLE_BLOCKS {
        rig.run_block(&EventBuffer::new());
    }
    let processed = rig.run_block(&EventBuffer::new());
    rig.finish();

    assert_eq!(
        first_difference(&bypassed, &input),
        None,
        "a settled bypassed block must be the input at unity gain (FR-CHAIN-030); it is not, \
         which is what `Chain::process`'s no-crosscutting fallback -- running every stage while \
         nominally bypassed -- would produce"
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

/// **FR-CLAP-060's click-free limb, and what this file's
/// `the_bypass_transition_is_a_single_sample_step_today` became.** That test recorded the defect:
/// with `Chain::set_global_bypass` flipping a `bool`, the event's own frame moved by the full
/// difference between the processed and bypassed renderings —
/// measured at about half the settled peak — and it asserted *that*, so a later fix would have a
/// before-figure to move. Issue #142 moved it. The bound is inverted here rather than deleted.
///
/// The event still lands on a peak of the probe tone ([`PHASE_FRAMES`]'s own doc comment), where
/// the signal's own sample-to-sample movement is smallest and the whole of any observed jump is the
/// bypass. Three things are asserted, in the order they matter:
///
/// 1. the first frame moves by a fraction of what it used to — the same measurement, three orders
///    of magnitude smaller;
/// 2. no frame of *either* transition moves the blend further than a linear 15 ms ramp would,
///    which is `namir_engine`'s own per-stage bypass bound (`stages/gate.rs`) applied to the global
///    one through the real C vtable;
/// 3. both transitions actually travel — a blend that never moves would satisfy 1 and 2 vacuously.
///
/// **Both directions, and the release deliberately happens before the engagement completes.** Once
/// the fade settles at fully bypassed the chain stops running its stages, so their state stops
/// advancing and the no-event reference stops being this run's wet signal; releasing while the
/// blend is still in flight keeps both runs rendering the same stages from the same state, which is
/// what makes [`inferred_mix`] exact in both directions. That the *settled* endpoints are reached
/// exactly is [`a_bypassed_block_is_unity_gain_passthrough_not_the_processed_signal`]'s assertion.
///
/// Untagged, deliberately: this is one of FR-CLAP-060's two limbs and the requirement's tag sits
/// on the other, at [`host_bypass_automation_takes_effect_at_the_event_s_own_frame`], which says
/// so in its own doc comment.
#[test]
fn the_bypass_transition_is_a_crossfade_not_a_step() {
    /// A multiple of the tone's 48-frame period, so the event lands on a peak.
    const ENGAGE_AT: usize = 240;
    /// Blocks rendered while the engagement fades before it is released, chosen to leave the blend
    /// well short of settling (two blocks is ~21 ms against a 15 ms time constant, so it is around
    /// three quarters of the way across) — see this test's doc comment for why that matters.
    const BLOCKS_BEFORE_RELEASE: usize = 2;
    /// Blocks rendered after the release, enough to watch the trajectory come back down.
    const BLOCKS_AFTER_RELEASE: usize = 2;
    const BLOCKS: usize = 1 + BLOCKS_BEFORE_RELEASE + BLOCKS_AFTER_RELEASE;

    // Reference: the identical rig driven through the identical block sequence, no events at all.
    let mut reference_rig = Rig::new();
    let reference: Vec<Vec<f32>> = (0..BLOCKS)
        .map(|_| reference_rig.run_block(&EventBuffer::new()))
        .collect();
    let input = reference_rig.input();
    reference_rig.finish();

    let mut rig = Rig::new();
    let mut switched = Vec::with_capacity(BLOCKS);
    switched.push(rig.run_block(&one_event(
        &namir_params::global::GLOBAL_BYPASS,
        1.0,
        ENGAGE_AT as u32,
    )));
    for _ in 0..BLOCKS_BEFORE_RELEASE {
        switched.push(rig.run_block(&EventBuffer::new()));
    }
    switched.push(rig.run_block(&one_event(&namir_params::global::GLOBAL_BYPASS, 0.0, 0)));
    for _ in 0..BLOCKS_AFTER_RELEASE - 1 {
        switched.push(rig.run_block(&EventBuffer::new()));
    }
    rig.finish();

    // 1: the measurement the old test made, with the inequality the other way round.
    let settled_peak = peak(&reference[0]);
    let jump = (switched[0][ENGAGE_AT] - reference[0][ENGAGE_AT]).abs();
    assert!(
        jump <= settled_peak * 0.01,
        "the bypass transition still completes in something close to one sample: frame \
         {ENGAGE_AT} moved {jump} against a settled peak of {settled_peak}, where a 15 ms \
         one-pole moves about 1/720 of the blend's range in its first frame"
    );

    // 2 and 3, on each transition separately: the engagement runs from the event to the release,
    // the release from there to the end.
    let engaging: Vec<(usize, f32)> = (0..=BLOCKS_BEFORE_RELEASE)
        .flat_map(|block| {
            let offset = block * BLOCK as usize;
            mix_trajectory(&switched[block], &reference[block], &input)
                .into_iter()
                .map(move |(frame, mix)| (offset + frame, mix))
        })
        .filter(|(frame, _)| *frame >= ENGAGE_AT)
        .collect();
    let releasing: Vec<(usize, f32)> = (BLOCKS_BEFORE_RELEASE + 1..BLOCKS)
        .flat_map(|block| {
            let offset = block * BLOCK as usize;
            mix_trajectory(&switched[block], &reference[block], &input)
                .into_iter()
                .map(move |(frame, mix)| (offset + frame, mix))
        })
        .collect();

    let engaged_distance = assert_no_step_exceeds_a_15ms_ramp(&engaging, "engaging");
    let released_distance = assert_no_step_exceeds_a_15ms_ramp(&releasing, "releasing");
    assert!(
        engaged_distance > 0.5,
        "the blend travelled only {engaged_distance} of its range while engaging; a 15 ms \
         one-pole covers about three quarters of it in the {BLOCKS_BEFORE_RELEASE} blocks this \
         waits, and a blend that never moves passes every step bound above vacuously"
    );
    assert!(
        released_distance > 0.25,
        "the blend travelled only {released_distance} of its range while releasing"
    );
}
