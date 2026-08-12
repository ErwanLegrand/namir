//! FR-CLAP-070: arbitrary and varying block sizes, one sample included, without artefacts.
//!
//! # The method, executed as the FRS states it
//!
//! *"Verify: U — process the same signal in randomised block sizes and assert the output matches
//! the fixed-block reference to within numerical tolerance."* So: one 32 768-frame stereo signal,
//! processed twice through the real plugin behind the real C vtable — once in fixed 512-frame
//! blocks, once in a seeded-random block schedule that starts `1, 1, 1, 2, 3, 511, 512, 1` and
//! then keeps drawing from `[1, 512]` — and a sample-by-sample comparison of the two outputs.
//!
//! # Why `max_frames_count` is identical in both runs, and why that is not a weakening
//!
//! `crate::audio::NamirAudioProcessor::activate` feeds `audio_config.max_frames_count` straight
//! into `PrepareContext::new` (`src/audio.rs`), which sizes every stage's scratch. A run activated
//! at a different maximum is therefore a **different engine**, and comparing its output to this
//! one would be measuring the maximum rather than the block schedule. Every run here activates at
//! `config(48 kHz, 1, 512)`, so the only thing that differs between them is how the same frames
//! are divided into `process()` calls — which is precisely what the requirement is about. The
//! declared maximum being honoured at all is D-6.2's "buffers sized for the declared maximum, a
//! smaller block uses a prefix" consequence; a *varying* maximum is FR-CLAP-080's neighbouring
//! question (a re-activation), not this one.
//!
//! # Why the tolerance is 1e-6 absolute rather than bit-equality
//!
//! Today's default chain is per-sample throughout, so the two runs are expected to agree bitwise —
//! and, measured, they do: the observed maximum difference is exactly 0. Asserting `==` would
//! therefore pass, and would still be the wrong assertion: the first FFT-partitioned or
//! SIMD-blocked stage whose summation order legitimately depends on the block length would fail it
//! for a reason that is not a defect. 1e-6 sits two orders above f32 accumulation over this signal
//! (~1e-7 · peak · √n) and three below any real block-dependency defect — a filter, envelope or
//! ramp that re-initialises per block shows ≥1e-3 immediately. The observed maximum difference is
//! carried in the failure message so a drift from 0 to ~1e-7 is legible in a CI log rather than
//! silently absorbed by the threshold.
//!
//! # The "without artefacts" limb, checked without reference to the other run
//!
//! Two outputs can match each other and still both be broken: a discontinuity a deterministic
//! engine injects at every `process()` boundary reproduces identically in both runs and cancels
//! out of the comparison entirely. So, independently of it: every output sample is finite, the
//! output is not silence (a comparison of two zero buffers would otherwise pass while proving
//! nothing), and the largest sample-to-sample step *at a block boundary* stays within
//! [`ARTEFACT_SLACK`] of the largest step *inside* a block.
//!
//! That last check runs on its own third pass, with a **100 Hz** sine rather than the broadband
//! signal the comparison uses, and that choice is what makes it worth anything. Its sensitivity is
//! bounded by the input's own steepest slope: under the noise signal the steady-state step is
//! ~5e-1, so a half-scale discontinuity dropped at a boundary passes unnoticed (measured, not
//! assumed — it was the first negative control this test was written against, and the check let it
//! through). At 100 Hz the steepest step is ~6.4e-3, so the same check now trips on anything above
//! ~1e-2, under 2% of peak. Low frequency is the sharp instrument here; broadband excitation is the
//! right thing for the *comparison* and the wrong thing for this.
//!
//! # Two properties of the default chain this file depends on
//!
//! **FR-CHAIN-050's mono core.** `GateStage`, `NamStage` and `IrStage` process channel 0 and
//! duplicate the result onto every physical channel (`crates/namir-engine/src/stages/gate.rs`), so
//! the two output channels of a stereo run are identical and channel 1's input never reaches the
//! output. Channel 1 still carries a *different* signal from channel 0 here, per this test's own
//! design, but nothing is inferred from that: the assertions are stated per channel and hold on
//! both.
//!
//! **The startup transient is not a block-boundary artefact.** The gate's attack and the bypass
//! crossfade move fastest in the first few milliseconds, giving steps larger than the steady-state
//! signal's own — 1.1e-2 against 6.4e-3, measured. Whether that maximum lands on a boundary frame
//! or an interior one is an accident of the schedule, so the artefact limb skips
//! [`ARTEFACT_SETTLE_FRAMES`] and judges the settled signal. The requirement is about state that
//! resets per `process()` call, which shows up over all 32 768 frames, not only the first hundred.
//!
//! # Allocation
//!
//! Every processing loop runs inside [`audio_section`]. Block size 1 across thousands of calls is
//! the sharpest NFR-RT-010 probe this repository has: any per-call allocation the engine or the
//! adapter performs is hit once per frame rather than once per 512 frames.
//!
//! # Two chains, and which of them carries the tag
//!
//! [`randomised_block_sizes_match_the_fixed_block_reference`] runs the **resource-free default
//! chain** — no `.nam`, no IR. That is a real test and it runs under a plain `cargo test
//! --workspace`, but on its own it does not span FR-CLAP-070: the two stages whose internal
//! scheduling is genuinely block-size dependent are exactly the two it leaves as pass-throughs.
//! `nam.rs`'s `SlotResampler` (D-9.2) buffers engine-rate samples in a `VecDeque` until a *fixed
//! internal block* is ready, runs the model on that, and resamples back through a second FIFO —
//! machinery that only exists at all when a model is loaded and its declared rate differs from the
//! engine's. `namir-ir`'s partitioned convolver (D-9.4/D-9.5/D-9.6) accumulates into per-partition
//! input buffers and fires each partition's FFT when its own accumulator fills, on a schedule keyed
//! to absolute stream time rather than to the host's block boundaries. Neither is instantiated by
//! the default chain, so a schedule of one-frame blocks never reaches either.
//!
//! [`loaded::randomised_block_sizes_match_the_fixed_block_reference_with_a_model_and_an_ir`] is
//! therefore the artifact this file traces: same signal, same two schedules, same tolerance and the
//! same artefact limb, but with a generated 44.1 kHz `.nam` model (so the resampler is engaged, not
//! merely present) and a generated 4 800-tap IR (so the convolver has FFT partitions past its
//! 512-tap head) driven into the live plugin through the host's own `state` extension. It needs the
//! `host-ext-tests` feature, for the reason `clap_host_latency.rs` documents at length:
//! `PluginState::load` is `clack-extensions`' *host* half. `.github/workflows/ci.yml`'s second,
//! required `cargo test -p namir-clap --features host-ext-tests` step is what runs it.
//!
//! # What the loaded limb found, and how it was fixed (M9b, 2026-08-12)
//!
//! **`randomised_block_sizes_match_the_fixed_block_reference_with_a_model_and_an_ir` failed the
//! first time it was run, and it was right to.** What it found was a real defect in
//! `namir-engine`, fixed in the same commit that added this file. The investigation is kept here
//! rather than deleted along with the bug it found, per this project's practice of keeping a
//! finding on the record.
//!
//! `crates/namir-engine/src/stages/nam.rs`'s `SlotResampler::process` ends with
//! `*sample = self.engine_out_fifo.pop_front().unwrap_or(0.0)` — any output frame the FIFO cannot
//! supply is emitted as silence — and its produce loop is gated on
//! `engine_out_fifo.len() < output.len()`, i.e. it primes the pipeline to *this call's* output size
//! and no further. Those two together made the stage's delay a function of the block-size history:
//! every starved frame inserted one sample of silence that was never taken back, so the accumulated
//! delay grew whenever a block arrived that the current FIFO occupancy could not cover, and never
//! shrank. Measured on a 48 kHz engine against a 44.1 kHz model (`engine_block` = 320 engine-rate
//! frames), against a 512-frame reference, after an identical 512-frame warm-up:
//!
//! | Uniform block size | 512, 320, 319, 256, 160, 128, 64 | 32 | 16 | 8 | 4 | 2 | 1 |
//! |---|---|---|---|---|---|---|---|
//! | Extra delay (samples) | 0 | 32 | 48 | 56 | 60 | 62 | 63 |
//!
//! The divergence was a pure time shift — remove the lag and the residual is ~4.4e-6 RMS — and it
//! moved *mid-stream*: 512-frame blocks for the first half of a stream and one-frame blocks for the
//! second gave lag 0 then lag 63, which is 63 samples of silence spliced in at the moment the host
//! changed its block size. That is FR-CLAP-070's "without artefacts" clause as much as its
//! comparison clause. `latency_samples` reported `in_delay + out_delay + engine_block` regardless,
//! so the figure was an upper bound the actual delay only met by accident.
//!
//! Three things localised it to the resampler and nothing else, all measured through a plain
//! `namir_engine::AudioEngine` with no plugin and no CLAP in the picture: an IR alone under the
//! same two schedules differed by **exactly 0**; a 48 kHz model (same weights, so the same
//! inference, but `NamSlot::resample` is `None`) differed by 7.5e-8; the two together by 5.4e-7,
//! both inside this file's tolerance. So `namir-ir`'s partitioned convolver is block-size
//! independent as D-9.4 says, and so is the model's own history.
//!
//! **The fix landed in `namir-engine`, not here.** `SlotResampler::new` now primes
//! `engine_out_fifo` with `engine_block` zeros (`crates/namir-engine/src/stages/nam.rs:481-504`),
//! and `NamStage::reset` re-primes it rather than clearing both FIFOs (`nam.rs:981-989`), so the
//! invariant `engine_in_fifo.len() + engine_out_fifo.len() == engine_block` holds for the life of
//! the slot. Under it the produce loop can never be starved — when `out < n`,
//! `in == engine_block + n - out > engine_block`, so the loop's second condition always holds — and
//! the actual delay is now exactly the `latency_samples` the stage already reported, where before
//! that figure was an upper bound the truth met only by accident (576 under 512-frame blocks, 639
//! under 1-frame blocks, both reported as 640).
//!
//! **Running the test at a distance, without touching `namir-engine`, is precisely what was *not*
//! done**, and the alternative is recorded because it exists and works: warming the same chain with
//! **one-frame** blocks drives the accumulated delay to its ceiling, and from there the fixed and
//! varying schedules agree bit-exactly, model-only and model-plus-IR alike. That would have made
//! this file green over an engine still splicing 63 samples of silence into any host that changed
//! its block size mid-stream — a test describing its own workaround rather than the requirement —
//! so roadmap §15 item 13 settled the branch policy (fix the engine) before this test was written.
//! The warm-up below is accordingly [`loaded::WARMUP_BLOCKS`] blocks of `DEFAULT_MAX_BLOCK` and not
//! one-frame blocks, so the plain tag this file carries rests on the engine fix and on nothing
//! else: take the priming out and the test goes red again. With it in, the two schedules agree
//! **bit-exactly** (`0e0`) with no warm-up trick at all.
//!
//! **Before changing anything here, read `tests/support/mod.rs`'s doc comment — in particular the
//! HAZARD about `start_library_scan` and the developer's real library index.** This file starts no
//! scan; the one thing it does to `SharedInner` beyond instantiating it is hand it a state
//! document, which is a read-and-adopt path.

mod support;

use clack_host::prelude::{PluginInstanceError, StartedPluginAudioProcessor};
use support::{
    CHANNELS, DEFAULT_MAX_BLOCK, DEFAULT_SAMPLE_RATE, Lcg, StereoBuffers, TestHost, activate,
    all_finite, audio_section, config, fill_sine, instantiate_default, noise, peak, sine_1k,
};

/// One stereo signal's worth of storage, allocated once per run.
type Stereo = [Vec<f32>; CHANNELS];

/// How many frames each run processes. 64 reference blocks of 512, and — with the schedule below —
/// several hundred varying ones, which is enough for the block-boundary statistics to be worth
/// reading while keeping the whole test well under a second.
const TOTAL_FRAMES: usize = 32_768;

/// The numerical tolerance the FRS's `Verify: U` line asks for. See this module's doc comment for
/// why this is an absolute epsilon and not bit-equality.
const TOLERANCE: f32 = 1e-6;

/// The requirement names a one-sample block explicitly, so the schedule must contain far more than
/// a token few. Asserted against the generated schedule rather than assumed.
const MIN_SINGLE_SAMPLE_BLOCKS: usize = 200;

/// Block sizes the schedule always begins with, before the seeded draws take over: the minimum
/// three times over, two small sizes, and the two largest the configuration permits. These are the
/// sizes a bug is most likely to sit on, so they are forced rather than left to chance.
const FORCED_PREFIX: [u32; 8] = [1, 1, 1, 2, 3, 511, 512, 1];

/// Seeds the block-size schedule. Fixed, so a failure reproduces exactly (D-19.1's spirit).
const SCHEDULE_SEED: u64 = 0x0C1A_9070_5A11_3E00;

/// Seeds the noise channel.
const NOISE_SEED: u64 = 0x0C1A_9070_0E15_E000;

/// The artefact limb's excitation frequency — see this module's doc comment for why it is two
/// decades below the comparison run's broadband signal.
const ARTEFACT_FREQ_HZ: f64 = 100.0;

/// Frames the artefact limb skips before measuring: 100 ms at 48 kHz, comfortably past the gate
/// attack and bypass crossfade whose own (legitimate) steps exceed the settled signal's.
const ARTEFACT_SETTLE_FRAMES: usize = 4_800;

/// How far a block boundary's largest step may exceed the largest interior step before it counts
/// as an artefact.
///
/// Not 1.0, and the reason is statistical rather than a softening: the boundary frames are ~1% of
/// the buffer, so the two maxima are order statistics of the same distribution and the boundary
/// set can win by chance — under this schedule the settled figures are 6.4401e-3 against
/// 6.4411e-3, a margin of 1e-6 that a different seed could invert with no defect involved. A
/// genuine per-block state reset is not a 50% excess; it is one to three orders of magnitude, and
/// this still trips on anything above ~1e-2 (under 2% of peak).
const ARTEFACT_SLACK: f32 = 1.5;

/// The resource-free default chain, under both schedules. Deliberately **not** tagged: see this
/// module's doc comment on which of this file's two chains carries FR-CLAP-070's annotation and
/// why. This one keeps its own value — it is the limb that runs under a plain `cargo test
/// --workspace`, and it is where the artefact check's negative control was established — but the
/// requirement is spanned by [`loaded`]'s counterpart, not by this.
#[test]
fn randomised_block_sizes_match_the_fixed_block_reference() {
    let input = comparison_signal();

    let reference_schedule = fixed_schedule();
    let varying_schedule = varying_schedule();

    assert_eq!(
        total(&reference_schedule),
        TOTAL_FRAMES,
        "the reference schedule must cover the whole signal"
    );
    assert_eq!(
        total(&varying_schedule),
        TOTAL_FRAMES,
        "the varying schedule must cover the whole signal, or the two runs see different audio"
    );
    assert!(
        varying_schedule.contains(&DEFAULT_MAX_BLOCK),
        "the varying schedule must include a full {DEFAULT_MAX_BLOCK}-frame block"
    );
    let single_sample_blocks = varying_schedule.iter().filter(|&&n| n == 1).count();
    assert!(
        single_sample_blocks >= MIN_SINGLE_SAMPLE_BLOCKS,
        "the varying schedule contains only {single_sample_blocks} one-frame blocks, fewer than \
         the {MIN_SINGLE_SAMPLE_BLOCKS} this test requires -- FR-CLAP-070 names the one-sample \
         block explicitly, so exercising it incidentally is not enough"
    );
    println!(
        "FR-CLAP-070: {TOTAL_FRAMES} frames in {} varying blocks, {single_sample_blocks} of them \
         one frame, against {} fixed {DEFAULT_MAX_BLOCK}-frame blocks",
        varying_schedule.len(),
        reference_schedule.len()
    );

    let reference = run_schedule(&reference_schedule, &input);
    let varying = run_schedule(&varying_schedule, &input);

    assert_sane(&reference, "the fixed-block reference");
    assert_sane(&varying, "the varying-block run");

    // The `Verify: U` method itself.
    let (max_diff, channel, frame) = worst_difference(&reference, &varying);
    assert!(
        max_diff <= TOLERANCE,
        "FR-CLAP-070: the varying-block output diverges from the fixed-block reference by \
         {max_diff:e} (tolerance {TOLERANCE:e}), worst at channel {channel} frame {frame}: \
         reference {:e} vs varying {:e}. A difference this size is block-size-dependent state, not \
         f32 accumulation -- see D-6.2 (buffers sized for the declared maximum; a smaller block \
         uses a prefix).",
        reference[channel][frame],
        varying[channel][frame]
    );
    println!("FR-CLAP-070: max |reference - varying| = {max_diff:e} (tolerance {TOLERANCE:e})");

    // "Without artefacts", judged on one run alone — so a discontinuity both runs reproduce
    // identically, which the comparison above cancels out, still fails here.
    let smooth = run_schedule(&varying_schedule, &artefact_signal());
    assert_sane(&smooth, "the artefact-limb run");

    let boundaries = boundary_flags(&varying_schedule);
    for (channel, buf) in smooth.iter().enumerate() {
        let (at_boundary, inside) = step_extremes(
            &buf[ARTEFACT_SETTLE_FRAMES..],
            &boundaries[ARTEFACT_SETTLE_FRAMES..],
        );
        println!(
            "FR-CLAP-070: channel {channel} settled step maxima -- boundary {at_boundary:e}, \
             interior {inside:e}"
        );
        assert!(
            at_boundary <= inside * ARTEFACT_SLACK,
            "FR-CLAP-070: on channel {channel} the largest sample-to-sample step at a block \
             boundary ({at_boundary:e}) exceeds {ARTEFACT_SLACK}x the largest step inside a block \
             ({inside:e}) -- with a {ARTEFACT_FREQ_HZ} Hz excitation that is a discontinuity at a \
             process() boundary, which is the artefact this requirement forbids, not signal"
        );
    }
}

/// The signal the comparison runs process: broadband on channel 0, which FR-CHAIN-050's mono core
/// makes the one that reaches the output, and a distinct tone on channel 1 so the two channels are
/// never interchangeable. Amplitudes sit well above the gate's default -70 dBFS threshold and well
/// below clipping.
fn comparison_signal() -> Stereo {
    [
        noise(TOTAL_FRAMES, NOISE_SEED, 0.25),
        sine_1k(TOTAL_FRAMES, DEFAULT_SAMPLE_RATE, 0.5),
    ]
}

/// The artefact limb's signal: a low-frequency sine on both channels, whose own steepest step is
/// small enough that a boundary discontinuity stands out against it (this module's doc comment).
fn artefact_signal() -> Stereo {
    let mut tone = vec![0.0f32; TOTAL_FRAMES];
    fill_sine(&mut tone, ARTEFACT_FREQ_HZ, DEFAULT_SAMPLE_RATE, 0.5, 0);
    [tone.clone(), tone]
}

/// [`TOTAL_FRAMES`] in blocks of exactly [`DEFAULT_MAX_BLOCK`] — the reference schedule.
fn fixed_schedule() -> Vec<u32> {
    let mut sizes = Vec::new();
    let mut remaining = TOTAL_FRAMES;
    while remaining > 0 {
        let frames = DEFAULT_MAX_BLOCK.min(remaining as u32);
        sizes.push(frames);
        remaining -= frames as usize;
    }
    sizes
}

/// [`FORCED_PREFIX`], then seeded draws over `[1, DEFAULT_MAX_BLOCK]`, until [`TOTAL_FRAMES`] is
/// covered exactly.
///
/// Two draws per block, both taken from the generator's **high** 32 bits: an LCG's low bits have
/// short periods, and `% 3` on the raw output would inherit that. Roughly two blocks in three are
/// forced to one frame — a uniform draw alone would average 256 frames and leave the one-sample
/// case to a handful of accidents, where the requirement names it explicitly.
fn varying_schedule() -> Vec<u32> {
    let mut lcg = Lcg::new(SCHEDULE_SEED);
    let mut sizes = Vec::new();
    let mut remaining = TOTAL_FRAMES;

    let push = |sizes: &mut Vec<u32>, remaining: &mut usize, frames: u32| {
        let frames = frames.min(*remaining as u32);
        if frames > 0 {
            sizes.push(frames);
            *remaining -= frames as usize;
        }
    };

    for frames in FORCED_PREFIX {
        push(&mut sizes, &mut remaining, frames);
    }
    while remaining > 0 {
        let selector = lcg.next_u64() >> 32;
        let frames = if selector.is_multiple_of(3) {
            ((lcg.next_u64() >> 32) % u64::from(DEFAULT_MAX_BLOCK)) as u32 + 1
        } else {
            1
        };
        push(&mut sizes, &mut remaining, frames);
    }
    sizes
}

/// Total frames a schedule covers.
fn total(schedule: &[u32]) -> usize {
    schedule.iter().map(|&n| n as usize).sum()
}

/// One complete run: a fresh instance, activated at the one configuration every run shares, fed
/// [`TOTAL_FRAMES`] frames in `schedule`'s blocks, then torn down in the order `clack-host`
/// requires (stop → deactivate → drop, or the instance leaks and never destroys).
fn run_schedule(schedule: &[u32], input: &Stereo) -> Stereo {
    let (_entry, mut instance) = instantiate_default();
    let stopped = activate(
        &mut instance,
        config(DEFAULT_SAMPLE_RATE, 1, DEFAULT_MAX_BLOCK),
    );
    let mut processor = stopped.start_processing().expect("processing must start");

    let mut bufs = StereoBuffers::new(DEFAULT_MAX_BLOCK as usize);
    let mut output: Stereo = [vec![0.0; TOTAL_FRAMES], vec![0.0; TOTAL_FRAMES]];

    // Everything inside allocates nothing: the buffers exist, `output` is already sized, and the
    // failure path returns the error by value rather than formatting it.
    let outcome =
        audio_section(|| process_schedule(&mut processor, &mut bufs, input, &mut output, schedule));

    let stopped = processor.stop_processing();
    instance.deactivate(stopped);
    drop(instance); // `clap_plugin.destroy`

    if let Err((index, frames, error)) = outcome {
        panic!("block {index} of the schedule ({frames} frames) failed to process: {error}");
    }
    output
}

/// The processing loop itself, factored out so [`audio_section`] wraps every `process()` call in
/// the run and nothing else.
///
/// Returns the offending block's index, size and error rather than panicking, so a `process()`
/// failure is reported after teardown instead of leaking the instance — and so the failure path
/// does not allocate a message inside the audio section and report an allocation violation on top
/// of the real fault.
fn process_schedule(
    processor: &mut StartedPluginAudioProcessor<TestHost>,
    bufs: &mut StereoBuffers,
    input: &Stereo,
    output: &mut Stereo,
    schedule: &[u32],
) -> Result<(), (usize, u32, PluginInstanceError)> {
    let mut pos = 0usize;
    for (index, &frames) in schedule.iter().enumerate() {
        let n = frames as usize;

        for (channel, source) in input.iter().enumerate() {
            bufs.input_mut(channel)[..n].copy_from_slice(&source[pos..pos + n]);
        }
        // So "the plugin did not write this block" is distinguishable from "it wrote silence":
        // an unwritten frame reaches the finiteness assertion below as a NaN.
        bufs.poison_output(f32::NAN);

        if let Err(error) = bufs.process_block(processor, frames) {
            return Err((index, frames, error));
        }

        for (channel, destination) in output.iter_mut().enumerate() {
            destination[pos..pos + n].copy_from_slice(&bufs.output(channel)[..n]);
        }
        pos += n;
    }
    Ok(())
}

/// Every output sample finite, and the run not silent.
///
/// The second half is what keeps the comparison from being vacuous: two silent buffers match each
/// other perfectly. The default chain carries no model and no IR, but its gate opens (-70 dBFS
/// default threshold against a -12 dBFS input) and its trim and output gains default to 0 dB, so
/// real signal is due.
fn assert_sane(run: &Stereo, what: &str) {
    for (channel, buf) in run.iter().enumerate() {
        assert!(
            all_finite(buf),
            "{what} produced a non-finite sample on channel {channel} -- either the plugin emitted \
             one or it left part of a block unwritten (the harness poisons the output with NaN \
             before every block)"
        );
        assert!(
            peak(buf) > 0.01,
            "channel {channel} of {what} peaks at {:e} -- the chain passed no signal, so every \
             comparison below would be comparing silence to silence",
            peak(buf)
        );
    }
}

/// The largest absolute difference between two runs, with where it is: `(difference, channel,
/// frame)`.
fn worst_difference(reference: &Stereo, varying: &Stereo) -> (f32, usize, usize) {
    let mut worst = (0.0f32, 0usize, 0usize);
    for (channel, (a, b)) in reference.iter().zip(varying.iter()).enumerate() {
        for (frame, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            let difference = (x - y).abs();
            if difference > worst.0 {
                worst = (difference, channel, frame);
            }
        }
    }
    worst
}

/// `flags[i] == true` where frame `i` is the first frame of a block other than the first — i.e.
/// exactly the frames whose predecessor was computed in a *previous* `process()` call.
fn boundary_flags(schedule: &[u32]) -> Vec<bool> {
    let mut flags = vec![false; TOTAL_FRAMES];
    let mut pos = 0usize;
    for &frames in schedule {
        if pos > 0 && pos < TOTAL_FRAMES {
            flags[pos] = true;
        }
        pos += frames as usize;
    }
    flags
}

/// The largest sample-to-sample step across a block boundary and the largest one strictly inside a
/// block, as `(at_boundary, inside)`. `boundaries` must be [`boundary_flags`] sliced the same way
/// `buf` is.
fn step_extremes(buf: &[f32], boundaries: &[bool]) -> (f32, f32) {
    let mut at_boundary = 0.0f32;
    let mut inside = 0.0f32;
    for (previous, pair) in buf.windows(2).enumerate() {
        let step = (pair[1] - pair[0]).abs();
        if boundaries[previous + 1] {
            at_boundary = at_boundary.max(step);
        } else {
            inside = inside.max(step);
        }
    }
    (at_boundary, inside)
}

/// The same comparison as above, with a real `.nam` model and a real IR loaded into the live
/// plugin — the chain FR-CLAP-070 is actually about. See this file's module doc comment for why
/// this, and not its resource-free sibling, is what the requirement's tag is attached to, and for
/// why the whole module is behind `host-ext-tests`.
#[cfg(feature = "host-ext-tests")]
mod loaded {
    use std::path::PathBuf;
    use std::time::Duration;

    use clack_extensions::latency::PluginLatency;
    use clack_extensions::state::PluginState;
    use namir_core::{ChannelConfig, ContentHash, SampleRate};
    use namir_engine::{PrepareContext, StageIo, build_default_engine};
    use namir_fixtures::nam::{WaveNetShape, generate};
    use namir_state::{Document, EmbeddedRef, FileRef, FileResolver, RelPath, State};
    use namir_worker::recall::ResourceRecall;
    use namir_worker::{EngineConfig, Instance, ResourceCache};

    use super::support::{
        DEFAULT_MAX_BLOCK, DEFAULT_SAMPLE_RATE, StereoBuffers, activate, audio_section, config,
        fill_sine, instantiate_default, main_thread_handle, require_plugin_extension,
    };
    use super::{
        ARTEFACT_FREQ_HZ, ARTEFACT_SETTLE_FRAMES, ARTEFACT_SLACK, Stereo, TOLERANCE, TOTAL_FRAMES,
        assert_sane, boundary_flags, comparison_signal, fixed_schedule, process_schedule,
        run_schedule, step_extremes, varying_schedule, worst_difference,
    };

    /// The rate the loaded model declares. Any value other than the engine's own 48 kHz engages
    /// D-9.2's `SlotResampler`; 44.1 kHz is the one a real user actually hits, and is what
    /// `clap_host_latency.rs` uses for the same reason.
    const MODEL_RATE_HZ: u32 = 44_100;

    /// Seeds the generated model. Fixed, so a failure reproduces exactly (D-19.1).
    const MODEL_SEED: u64 = 0x0C1A_9070_4A11_0000;

    /// Seeds the generated IR.
    const IR_SEED: u64 = 0x0C1A_9070_1BE0_0000;

    /// Taps in the generated IR: 100 ms at 48 kHz, and — the load-bearing part — comfortably more
    /// than the 512-sample head partition, so `PreparedChannel::new` builds real FFT stages past
    /// the head rather than a single direct-convolution block. With `growth_factor = 2` that is a
    /// schedule of several partitions at two or three distinct sizes, each firing on its own
    /// stream-time boundary rather than the host's.
    const IR_TAPS: usize = 4_800;

    /// The generated IR's exponential decay constant, in samples. Short enough relative to
    /// [`IR_TAPS`] that the tail is genuinely decayed rather than truncated mid-energy.
    const IR_TAU_SAMPLES: f64 = 600.0;

    /// How long to wait, **processing nothing**, between handing the plugin its state document and
    /// the first `process()` call.
    ///
    /// This is what makes the install frame deterministic, and it is not a guess about scheduler
    /// luck. `namir_worker::Instance::load` pushes its offer into the command ring and then waits
    /// out the crossfade on a timer; it does not wait on the audio thread making progress
    /// (`namir-worker`'s own `recalling_both_a_model_and_an_ir_never_offers_them_simultaneously`
    /// loads both resources without processing a single block). So a wait here that outlasts the
    /// whole serialised recall — parse the model, offer it, wait one crossfade, parse the IR, offer
    /// it, wait another — leaves **both** offers sitting in the ring before frame 0, and every run
    /// in this module then installs them on the same block. That matters: the resampler's FIFO fill
    /// and the convolver's partition phase are both counted from install, so two runs that
    /// installed on different blocks would differ for a reason that has nothing to do with their
    /// block schedules. Asserted, not assumed — [`LoadedRun::landed_at`] is compared across runs.
    const RECALL_SETTLE: Duration = Duration::from_millis(750);

    /// Silent blocks of [`DEFAULT_MAX_BLOCK`] frames every run processes between the install and the
    /// signal under test. Fixed, so every run reaches frame 0 of the comparison having processed
    /// the identical number of frames.
    ///
    /// 128 x 512 = 65 536 frames. Two things have to have finished by then, and both do by orders
    /// of magnitude: the model's own causal-convolution history has to be flushed with the zeros
    /// that silence puts through it (a WaveNet is feed-forward, so a receptive field's worth of
    /// zeros leaves it in exactly the zero-input state, and Nano's is a few hundred samples at
    /// model rate), and the convolver's ring has to be saturated with the constant the model emits
    /// for a zero input (one IR length, 4 800 samples). `NamStage::reset`/`IrStage::reset` clear
    /// neither of those — both say so in as many words — which is exactly why this warm-up is a
    /// flush rather than a `reset()` call.
    const WARMUP_BLOCKS: usize = 128;

    /// The block by which the reported latency must have moved off zero, counted from the first
    /// block after [`RECALL_SETTLE`]. `nam.rs` only moves `active` once the handover crossfade
    /// completes (20 ms, so under two 512-frame blocks), so with both offers already in the ring
    /// this is a small constant; a larger value means the settle above was outrun, which the
    /// failure message says.
    const LANDING_LIMIT: usize = 8;

    /// The artefact limb's excitation amplitude on the loaded chain — a tenth of what the
    /// resource-free limb uses, and the reason is sensitivity, not caution.
    ///
    /// A model and a cabinet IR together add a great deal of gain, and at the resource-free limb's
    /// own 0.5 the chain runs into FR-CHAIN-090's output ceiling: measured peak exactly 1.0, and a
    /// clipped waveform's own corners are steps this check then has to tolerate. The settled
    /// interior step maximum comes out at 7.9e-2 against a peak of 1.0, so the check would only
    /// trip above ~1.2e-1 — 12% of peak, where the resource-free limb trips at 2%. At 0.05 the
    /// chain peaks at 6.4e-1 with room to spare, the interior step maximum is 8.1e-3, and the check
    /// trips above ~1.2e-2: the same ~2% of peak, restored.
    ///
    /// Re-verified with a negative control rather than reasoned about, since changing this signal
    /// is exactly the change that could quietly blunt the limb: adding a half-peak discontinuity at
    /// a block boundary of the settled output takes the boundary/interior ratio from 0.9999 to
    /// **40.0**, against [`ARTEFACT_SLACK`]'s bar of 1.5. (At 0.5, clipped, the same injection only
    /// reaches 6.3 — it still trips, but with a sixth of the margin.)
    const ARTEFACT_AMPLITUDE: f32 = 0.05;

    /// How different two chains' outputs must be before this file will call the difference
    /// evidence that a stage is actually running. Both signals peak near 0.1-0.5, so this is a
    /// floor far under any real difference and far over f32 noise.
    const ACTIVITY_FLOOR: f32 = 1e-3;

    /// One complete resource-loaded run.
    struct LoadedRun {
        /// The processed signal.
        output: Stereo,
        /// Index of the first warm-up block at which the plugin reported non-zero latency — i.e.
        /// the block on which the model's handover crossfade completed. Compared across runs; see
        /// [`RECALL_SETTLE`].
        landed_at: usize,
        /// The latency the plugin reported once warm.
        latency: u32,
    }

    /// A generated WaveNet model, re-declared at [`MODEL_RATE_HZ`] so the engine has to resample
    /// around it.
    ///
    /// `namir-fixtures` always stamps its own 48 kHz on a generated fixture, which is precisely the
    /// case that leaves `NamSlot::resample` as `None`; overwriting the one field is what turns this
    /// from "a model is loaded" into "the D-9.2 resampler is running". Everything numeric about the
    /// model — topology, weights, the RMS calibration that keeps its output neither silent nor
    /// exploding — is the generator's, untouched.
    fn model_bytes() -> Vec<u8> {
        let mut model =
            generate(WaveNetShape::Nano, MODEL_SEED).expect("the WaveNet fixture must generate");
        model.sample_rate = MODEL_RATE_HZ;
        model.to_json_bytes()
    }

    /// A generated mono IR, as the WAV bytes a real one would arrive as.
    fn ir_bytes() -> Vec<u8> {
        let taps = namir_fixtures::ir::decaying_noise(IR_TAPS, IR_SEED, IR_TAU_SAMPLES);
        namir_fixtures::ir::to_mono_wav_bytes(&taps, DEFAULT_SAMPLE_RATE as u32)
    }

    /// FR-STATE-080's embedded form of `data`: no external candidate at all, so
    /// `namir_worker::recall::locate` can only resolve it from the bytes carried in the document —
    /// nothing is written to disk and the developer's library is neither consulted for a hit nor
    /// modified. Same device `clap_host_latency.rs` uses, for the same reason.
    fn embedded(data: &[u8], media_type: &str, display_name: &str) -> FileRef {
        FileRef {
            hash: ContentHash::of(data),
            library_relative: None,
            absolute: None,
            display_name: display_name.to_string(),
            embedded: Some(EmbeddedRef {
                media_type: media_type.to_string(),
                data: data.to_vec(),
            }),
        }
    }

    /// The `State` a document is written from: the model always, the IR only when `ir` is `Some`.
    /// The IR-less form is what the "is the convolver actually contributing" comparison runs
    /// against.
    fn state_for(model: &[u8], ir: Option<&[u8]>) -> State {
        let mut state = State::defaults();
        state.nam = Some(embedded(
            model,
            "application/vnd.namir.nam+json",
            "fr-clap-070-44k1.nam",
        ));
        state.ir = ir.map(|bytes| embedded(bytes, "audio/wav", "fr-clap-070.wav"));
        state
    }

    /// The bytes a host hands to `clap_plugin_state.load`, written through the real `State`/
    /// `Document` writers so the base64 encoding is the format's own rather than this file's.
    fn document_bytes(model: &[u8], ir: Option<&[u8]>) -> Vec<u8> {
        state_for(model, ir)
            .write_onto(&Document::empty())
            .to_pretty_bytes()
    }

    /// A resolver that finds nothing, so every external candidate misses and only FR-STATE-080's
    /// embedded fallback can resolve the reference.
    struct NoResolver;

    impl FileResolver for NoResolver {
        fn resolve_library_relative(&self, _rel: &RelPath) -> Option<PathBuf> {
            None
        }
        fn resolve_absolute(&self, _absolute: &str) -> Option<PathBuf> {
            None
        }
        fn resolve_by_hash(&self, _hash: ContentHash) -> Option<PathBuf> {
            None
        }
    }

    /// Loads the same document into a plain `namir_engine::AudioEngine` — no plugin, no CLAP, no
    /// worker pool — and returns the latency its chain reports.
    ///
    /// Two things come out of this. Both resource slots are asserted to reach
    /// `ResourceRecall::Loaded`, so "the document is well formed and both resources parse" is
    /// established outside the thing under test rather than inferred from it; and the latency
    /// figure is derived independently, so the plugin's own reported number has something to be
    /// checked against.
    fn reference_latency(model: &[u8], ir: &[u8]) -> u32 {
        let frames = DEFAULT_MAX_BLOCK as usize;
        let ctx = PrepareContext::new(
            SampleRate::new(DEFAULT_SAMPLE_RATE as u32).expect("48 kHz is a valid sample rate"),
            frames,
            ChannelConfig::Stereo,
        )
        .expect("the reference prepare context must build");
        let (mut engine, endpoint) =
            build_default_engine(&ctx).expect("the reference engine must build");
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx }, endpoint);

        let outcome = instance.recall(&cache, &state_for(model, Some(ir)), &NoResolver);
        assert!(
            matches!(outcome.nam, ResourceRecall::Loaded(_)),
            "the generated model must load into a plain engine before it is worth asking a plugin \
             to load it, got {:?}",
            outcome.nam
        );
        assert!(
            matches!(outcome.ir, ResourceRecall::Loaded(_)),
            "the generated IR must load into a plain engine before it is worth asking a plugin to \
             load it, got {:?}",
            outcome.ir
        );

        let mut left = vec![0.0f32; frames];
        let mut right = vec![0.0f32; frames];
        for _ in 0..64 {
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            let mut io = StageIo::new(&mut channels, frames);
            engine.process(&mut io);
            if engine.chain().latency_samples() != 0 {
                break;
            }
        }
        engine.chain().latency_samples()
    }

    /// One complete run: a fresh instance activated at the one configuration every run in this
    /// file shares, handed `document`, warmed up for exactly [`WARMUP_BLOCKS`] silent blocks, then
    /// fed [`TOTAL_FRAMES`] frames in `schedule`'s blocks.
    ///
    /// `max_frames_count` is `config(48 kHz, 1, DEFAULT_MAX_BLOCK)` here exactly as it is in the
    /// resource-free runs, and for the reason this file's module doc comment gives: it feeds
    /// `PrepareContext`, so a run activated at a different maximum is a different engine — and
    /// with an IR loaded it is a *visibly* different one, since the head partition's size is the
    /// declared block size (`PreparedChannel::new`).
    fn run_loaded(document: &[u8], schedule: &[u32], input: &Stereo) -> LoadedRun {
        let (_entry, mut instance) = instantiate_default();
        let latency_ext = require_plugin_extension::<PluginLatency>(&mut instance);
        let state_ext = require_plugin_extension::<PluginState>(&mut instance);

        let stopped = activate(
            &mut instance,
            config(DEFAULT_SAMPLE_RATE, 1, DEFAULT_MAX_BLOCK),
        );
        let mut processor = stopped.start_processing().expect("processing must start");

        let mut reader = document;
        state_ext
            .load(&mut main_thread_handle(&mut instance), &mut reader)
            .expect("the host-driven state load must succeed");
        std::thread::sleep(RECALL_SETTLE);

        let mut bufs = StereoBuffers::new(DEFAULT_MAX_BLOCK as usize);
        bufs.silence_input();

        let mut landed_at = None;
        for block in 0..WARMUP_BLOCKS {
            // Inside the marker deliberately: these are the blocks carrying D-8.1's install and
            // the handover crossfade, which is where an allocation on the audio thread would be.
            audio_section(|| bufs.process_block(&mut processor, DEFAULT_MAX_BLOCK))
                .expect("a warm-up block must process");
            if landed_at.is_none() && latency_ext.get(&mut main_thread_handle(&mut instance)) != 0 {
                landed_at = Some(block);
            }
        }
        let latency = latency_ext.get(&mut main_thread_handle(&mut instance));

        let mut output: Stereo = [vec![0.0; TOTAL_FRAMES], vec![0.0; TOTAL_FRAMES]];
        let outcome = audio_section(|| {
            process_schedule(&mut processor, &mut bufs, input, &mut output, schedule)
        });

        let stopped = processor.stop_processing();
        instance.deactivate(stopped);
        drop(instance); // `clap_plugin.destroy`

        if let Err((index, frames, error)) = outcome {
            panic!("block {index} of the schedule ({frames} frames) failed to process: {error}");
        }
        let landed_at = landed_at.unwrap_or_else(|| {
            panic!(
                "the plugin still reports zero latency after {WARMUP_BLOCKS} warm-up blocks -- the \
                 {MODEL_RATE_HZ} Hz model never reached the engine, so this run would be comparing \
                 two pass-through chains and proving nothing"
            )
        });
        assert!(
            landed_at < LANDING_LIMIT,
            "the model's handover only completed on warm-up block {landed_at}, past the {LANDING_LIMIT} \
             this file allows -- the {RECALL_SETTLE:?} settle was outrun, so the install frame is no \
             longer the same in every run and the comparison below would be measuring install \
             phase rather than block schedule"
        );
        LoadedRun {
            output,
            landed_at,
            latency,
        }
    }

    /// FR-CLAP-070 on the chain the requirement is actually about.
    ///
    /// The comparison, the tolerance, the schedules and the artefact limb are the resource-free
    /// test's, unchanged — see this file's module doc comment for each. What is added is that the
    /// two stages with block-size-dependent internal scheduling are *running*, and that the test
    /// proves they are rather than trusting that a state document arrived:
    ///
    /// - **The D-9.2 resampler.** `ir.rs`'s `latency_samples` returns 0 unconditionally and says
    ///   so, and every other stage in the fixed six-stage chain does the same, so a non-zero
    ///   `clap_plugin_latency.get` has exactly one possible source in 1.0: `nam.rs`'s
    ///   `SlotResampler`, which exists only on a slot whose model declares a rate other than the
    ///   engine's. The figure is additionally checked against an independently built
    ///   `namir_engine::AudioEngine` recalling the same document, so it is not merely non-zero.
    /// - **The partitioned convolver.** Proven behaviourally, because there is no extension that
    ///   reports it: the same fixed-block signal is run through three chains — nothing loaded,
    ///   model only, model and IR — and each step has to move the output. A pass-through IR stage
    ///   would leave the last two identical, which is the exact failure this limb exists to catch.
    // trace: FR-CLAP-070
    #[test]
    fn randomised_block_sizes_match_the_fixed_block_reference_with_a_model_and_an_ir() {
        let model = model_bytes();
        let ir = ir_bytes();
        let expected_latency = reference_latency(&model, &ir);
        assert!(
            expected_latency > 0,
            "a {MODEL_RATE_HZ} Hz model in a {DEFAULT_SAMPLE_RATE} Hz engine must engage D-9.2's \
             resampler -- with zero reported there is no resampler for the schedules below to \
             exercise"
        );

        let with_both = document_bytes(&model, Some(&ir));
        let model_only = document_bytes(&model, None);

        let input = comparison_signal();
        let reference_schedule = fixed_schedule();
        let varying_schedule = varying_schedule();

        let reference = run_loaded(&with_both, &reference_schedule, &input);
        let varying = run_loaded(&with_both, &varying_schedule, &input);

        // -- The resources are in the engine, and both of them ------------------------------
        assert_eq!(
            reference.landed_at, varying.landed_at,
            "the two runs installed the model on different warm-up blocks ({} and {}), so their \
             resampler FIFOs and convolver partitions are at different phases and the comparison \
             below would not be measuring the block schedule -- raise RECALL_SETTLE",
            reference.landed_at, varying.landed_at
        );
        for (run, what) in [(&reference, "fixed-block"), (&varying, "varying-block")] {
            assert_eq!(
                run.latency, expected_latency,
                "the {what} run's plugin reports {} samples of latency, but an independently built \
                 engine recalling the same document reports {expected_latency} -- the model is not \
                 in this run's chain the way it is in that one",
                run.latency
            );
        }

        let empty_chain = run_schedule(&reference_schedule, &input);
        let nam_only = run_loaded(&model_only, &reference_schedule, &input);
        let nam_effect = peak_difference(&empty_chain, &nam_only.output);
        let ir_effect = peak_difference(&nam_only.output, &reference.output);
        println!(
            "FR-CLAP-070 (loaded): reported latency {expected_latency} samples, model installed on \
             warm-up block {}, |default - model| = {nam_effect:e}, |model - model+IR| = \
             {ir_effect:e}",
            reference.landed_at
        );
        assert!(
            nam_effect > ACTIVITY_FLOOR,
            "loading the model changed the output by only {nam_effect:e} -- the NAM stage is \
             passing through, so its history and D-9.2 resampler are not under test"
        );
        assert!(
            ir_effect > ACTIVITY_FLOOR,
            "adding the IR to the same document changed the output by only {ir_effect:e} -- the IR \
             stage is passing through, so namir-ir's partitioned convolver is not under test"
        );

        assert_sane(&reference.output, "the loaded fixed-block reference");
        assert_sane(&varying.output, "the loaded varying-block run");

        // -- The `Verify: U` method itself ---------------------------------------------------
        let (max_diff, channel, frame) = worst_difference(&reference.output, &varying.output);
        assert!(
            max_diff <= TOLERANCE,
            "FR-CLAP-070: with a model and an IR loaded, the varying-block output diverges from \
             the fixed-block reference by {max_diff:e} (tolerance {TOLERANCE:e}), worst at channel \
             {channel} frame {frame}: reference {:e} vs varying {:e}. Both runs installed on the \
             same block and processed the same warm-up, so this is block-size-dependent state in \
             the NAM slot's resampler FIFOs or the convolver's partition schedule -- see D-6.2, \
             which asserts the design handles arbitrary block sizes.",
            reference.output[channel][frame],
            varying.output[channel][frame]
        );
        println!(
            "FR-CLAP-070 (loaded): max |reference - varying| = {max_diff:e} (tolerance \
             {TOLERANCE:e})"
        );

        // -- "Without artefacts", judged on one run alone -------------------------------------
        let smooth = run_loaded(&with_both, &varying_schedule, &loaded_artefact_signal());
        assert_sane(&smooth.output, "the loaded artefact-limb run");

        let boundaries = boundary_flags(&varying_schedule);
        for (channel, buf) in smooth.output.iter().enumerate() {
            let (at_boundary, inside) = step_extremes(
                &buf[ARTEFACT_SETTLE_FRAMES..],
                &boundaries[ARTEFACT_SETTLE_FRAMES..],
            );
            println!(
                "FR-CLAP-070 (loaded): channel {channel} settled step maxima -- boundary \
                 {at_boundary:e}, interior {inside:e}"
            );
            assert!(
                at_boundary <= inside * ARTEFACT_SLACK,
                "FR-CLAP-070: on channel {channel} of the loaded chain the largest \
                 sample-to-sample step at a block boundary ({at_boundary:e}) exceeds \
                 {ARTEFACT_SLACK}x the largest step inside a block ({inside:e}) -- with a \
                 {ARTEFACT_FREQ_HZ} Hz excitation that is a discontinuity at a process() boundary, \
                 which is the artefact this requirement forbids, not signal"
            );
        }
    }

    /// The artefact limb's signal on the loaded chain: the same [`ARTEFACT_FREQ_HZ`] tone the
    /// resource-free limb uses, at [`ARTEFACT_AMPLITUDE`] rather than 0.5 — see that constant for
    /// the measurement and the negative control behind the change.
    fn loaded_artefact_signal() -> Stereo {
        let mut tone = vec![0.0f32; TOTAL_FRAMES];
        fill_sine(
            &mut tone,
            ARTEFACT_FREQ_HZ,
            DEFAULT_SAMPLE_RATE,
            ARTEFACT_AMPLITUDE,
            0,
        );
        [tone.clone(), tone]
    }

    /// The largest absolute difference between two runs, over both channels — the "did loading
    /// this actually change anything" measure.
    fn peak_difference(a: &Stereo, b: &Stereo) -> f32 {
        let mut worst = 0.0f32;
        for (x, y) in a.iter().zip(b.iter()) {
            for (p, q) in x.iter().zip(y.iter()) {
                worst = worst.max((p - q).abs());
            }
        }
        worst
    }
}
