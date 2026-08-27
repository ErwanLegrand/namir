//! FR-CFG-020 (Must, `Verify: G`): "The engine shall produce bit-identical output in both
//! configurations for identical input, identical parameter values and identical block sizes ——
//! the same golden test vectors are run through both configurations."
//!
//! # What "both configurations" means, and why the obvious test is not one
//!
//! FR-CFG-010, immediately above it in the FRS, defines the two: **the standalone application**
//! (`namir-app`) and **the CLAP plugin** (`namir-clap`). Building two engines with
//! `namir_engine::build_default_engine` and comparing them would prove only that one function is
//! deterministic; it would say nothing about the two *products*, which drive that engine through
//! entirely separate code — a `cpal` output callback on one side, `clap_plugin.process` on the
//! other, each with its own channel plumbing, its own activation sequence and its own preset
//! recall path. So this test runs the golden vector through **each shell's own audio-driving
//! path**:
//!
//! - **CLAP** — the real `NamirClapPlugin` behind the real C vtable, loaded in-process by
//!   `clack-host` (`tests/support/mod.rs`): `clap_plugin_state.load` -> `activate` ->
//!   `start_processing` -> `clap_plugin.process` -> `crate::audio::process_port_pair`.
//! - **Standalone** — `namir_app::stream::open`'s real output callback, the one
//!   `crate::app::run` opens: bridge pull, `ChannelConfig::MonoToStereo` duplication,
//!   `AudioEngine::process`, interleaved write-back and FR-IO-060 xrun accounting. The `cpal`
//!   device is the only thing substituted, through D-13.1's own `AudioBackend` seam, which exists
//!   precisely so this path runs with no hardware ([`HarnessBackend`] below). Everything above
//!   the device — the engine, the `namir_worker::Instance`, `namir_app::worker`'s background
//!   thread and its `AppCommand::LoadState` recall — is the shipping code.
//!
//! # Why this file lives in `namir-clap` and takes a dev-dependency on `namir-app`
//!
//! Bit-identity between two runs is only a meaningful claim inside **one process and one build**.
//! The chain reaches `f32::tanh`, `exp`, `sin` and `cos` through the platform's libm, so a
//! checked-in *expected output* file compared by two separate test binaries would be asserting
//! cross-platform bit-exactness of libm — something Namir never claims and CI's Linux and macOS
//! runners would be entitled to fail. The two configurations therefore have to be driven from one
//! test binary, and only one direction of dependency is expressible: `namir-clap` may name
//! `namir-app`, never the reverse. D-5.1's `namir-app` row does not name this crate, and
//! `xtask/src/layering.rs`'s own doc comment leans on exactly that asymmetry for FR-CFG-030's
//! structural half. See this crate's `Cargo.toml` for why a **dev** edge in the other direction
//! breaches neither the layering gate nor FR-CFG-030 nor NFR-LIC-030's attribution file.
//!
//! # The golden vector (D-19.1: generated, never captured)
//!
//! Two checked-in artefacts under `tests/golden/fr-cfg-020/`, both consumed by both
//! configurations — not two independently generated signals that happen to agree:
//!
//! - `input.f32` — raw little-endian mono `f32`, 48 kHz: a fade-in, a saturating low tone, a hard
//!   transient, seeded noise, a chord, and a decay into near-silence, so the gate, the amp, the
//!   cabinet and the tone stack are all exercised rather than idling.
//! - `preset.namirpreset` — a real `namir_state` document carrying a **non-default value for every
//!   parameter in `namir_params::REGISTRY` that has one** (`global.bypass` deliberately excepted,
//!   since bypassing the chain would make the comparison vacuous), plus a generated WaveNet `.nam`
//!   and a generated cabinet IR, both embedded (FR-STATE-080) so no path resolution and no
//!   library are involved.
//!
//! [`regenerate_the_golden_vector`] is the recipe, `#[ignore]`d in the same shape as
//! `params.lock`'s generator; [`the_golden_vector_is_intact`] is the guard that the checked-in
//! bytes still say what this test needs them to say.
//!
//! # Why a settling window precedes the comparison, and what the vector had to avoid
//!
//! Both shells load the preset through the same `namir_worker::Instance::recall`, but they learn
//! it has landed differently. The standalone worker reports `AppEvent::StateLoaded` before any
//! audio runs, so its handover offer is in the command ring before its first block. The plugin
//! dispatches its replay to a pool (`crate::worker_jobs::spawn_recall`) and a CLAP host has no way
//! to observe that job finishing, so the plugin gets a wall-clock grace period instead
//! ([`REPLAY_GRACE`]) — which is enough in the ordinary case and is *not* relied on: under load
//! the plugin's handover can still land a block or two later than the application's, and the
//! comparison has to survive that.
//!
//! It survives it because both engines are driven with silence for [`SETTLE_FRAMES`] first, and
//! because the vector is built so that every piece of state that survives silence is
//! **convergent** rather than **cyclic**. Convergent state does not care which block the handover
//! landed on: the gate closes, the biquads and the DC blocker decay to a flushed zero under
//! D-7.4's denormal guard, the model's history fills with silence, and each `namir_dsp::GainRamp`
//! reaches and sticks at its target (25 ms one-pole, measured here at ~12 000 samples to the last
//! `f32` bit — [`SETTLE_FRAMES`] is four times that). Cyclic state does care, permanently, and
//! two of them were found by measurement rather than reasoning while this test was written — a
//! rate converter's resampling phase and the IR convolver's per-partition FFT accumulators, each
//! of which made the comparison depend on the *parity* of the block count since its own
//! installation. Both are kept out of the chain by the golden vector's own construction, and both
//! choices are recorded at [`MODEL_RATE_HZ`] and [`IR_FRAMES`] with what they cost.
//!
//! The comparison then covers every sample of the vector from the first.
//!
//! Read `tests/support/mod.rs`'s HAZARD before adding anything here: instantiating the plugin
//! opens the developer's real library index, and no test may start a scan.

mod support;

#[cfg(feature = "host-ext-tests")]
mod host_ext {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use clack_extensions::latency::PluginLatency;
    use clack_extensions::state::PluginState;

    use namir_app::audio_io::{
        AudioBackend, AudioIoError, AudioStream, BufferSizeRange, DeviceInfo, ExclusiveModeOutcome,
        HostInfo, ShareMode, StreamFailure, StreamParams, SupportedConfigRange,
    };
    use namir_app::instance::SharedInstance;
    use namir_app::stream::{self, Direction, StreamSetup};
    use namir_app::worker::{
        AppCommand, AppEvent, LoadOutcomeSummary, RecallOutcomeSummary, WorkerContext, WorkerHandle,
    };
    use namir_app::xrun::XrunCounter;
    use namir_core::{ChannelConfig, ContentHash, SampleRate};
    use namir_engine::{PrepareContext, build_default_engine};
    use namir_params::{ParamKind, REGISTRY};
    use namir_state::{Document, EmbeddedRef, FileRef, State};
    use namir_worker::library::LibraryService;
    use namir_worker::pool::ThreadPool;
    use namir_worker::{EngineConfig, Instance, ResourceCache};

    use super::support::{
        DEFAULT_SAMPLE_RATE, StereoBuffers, activate, config, instantiate_default,
        main_thread_handle, require_plugin_extension,
    };

    // -------------------------------------------------------------------------------------
    // Shared configuration. Every constant here is shared by both configurations by
    // construction — that is the whole point of the requirement.
    // -------------------------------------------------------------------------------------

    /// The engine sample rate both configurations run at.
    const SAMPLE_RATE_HZ: u32 = 48_000;

    /// The maximum block size both `PrepareContext`s declare: the plugin's from
    /// `PluginAudioConfiguration::max_frames_count`, the application's from
    /// `crate::audio_io::block_frames`. Fixed so the two engines are prepared identically and only
    /// the *processed* block size varies below.
    const MAX_BLOCK: u32 = 512;

    /// The block sizes the vector is run at. The requirement names "identical block sizes", so one
    /// size would sample the clause rather than span it: a sub-buffer size, an intermediate one,
    /// and the declared maximum. Every one divides [`VECTOR_FRAMES`].
    const BLOCK_SIZES: [u32; 3] = [64, 128, 512];

    /// Length of the golden input signal, in frames — 0.512 s at 48 kHz. A multiple of every
    /// entry in [`BLOCK_SIZES`], so no run ends on a short block.
    const VECTOR_FRAMES: usize = 24_576;

    /// How many frames of silence each configuration is driven with before the vector starts —
    /// see this file's module doc comment for what the window is for.
    ///
    /// 1 s, and the figure is measured rather than guessed: driving the standalone configuration
    /// alone (deterministic, no pool timing in it at all) with 8 192, 12 288, 16 384, 24 064,
    /// 32 768, 48 128, 65 536 and 102 400 settling frames, its output of this vector stops
    /// changing between 8 192 and 12 288 — the last `GainRamp` reaching its target. This is four
    /// times that.
    const SETTLE_FRAMES: usize = 48_000;

    /// The sample rate the golden model declares, and deliberately the engine's own.
    ///
    /// A model at some *other* rate would put D-9.2's resampler in the chain, which sounds like
    /// more of the engine under comparison and is in fact the one thing that would make this
    /// comparison unsound. Every other piece of state a settling window has to neutralise is
    /// *convergent* — filters and envelopes decay to a flushed zero, smoothed parameters reach
    /// their target, the model's history fills with silence — so it does not matter which block
    /// the handover landed on. A rate converter's phase is not convergent: it is cyclic in the
    /// resampling ratio (160:147 for 48 kHz against 44.1 kHz), so two engines whose models arrived
    /// one block apart would resample on permanently different grids. The two shells cannot be
    /// given a common handover block (see this file's module doc comment), so the vector keeps the
    /// resampler out of the chain instead. Recorded plainly: D-9.2's resampler is therefore *not*
    /// exercised by this comparison, and `crates/namir-clap/tests/clap_host_latency.rs` is where
    /// it is.
    const MODEL_RATE_HZ: u32 = SAMPLE_RATE_HZ;

    /// Seeds for the two generated resources. Any value works; these are fixed so the golden
    /// artefacts regenerate byte-for-byte.
    const MODEL_SEED: u64 = 0x00CF_0020;
    /// See [`MODEL_SEED`].
    const IR_SEED: u64 = 0x00CF_0021;

    /// Length of the generated cabinet IR, in samples — 10.7 ms, and deliberately no longer than
    /// [`MAX_BLOCK`].
    ///
    /// `namir_ir`'s schedule gives the first `min(block_size, ir_len)` taps to the direct,
    /// time-domain head partition and covers everything beyond that with FFT partitions, each
    /// carrying an input accumulator that fires "once every `P` samples" counted from the moment
    /// the IR was installed (`crates/namir-ir/src/convolver.rs`'s own module doc comment, and
    /// `crates/namir-engine/src/stages/ir.rs` partitions against `ctx.max_block_size()`, so this
    /// bound holds at every entry in [`BLOCK_SIZES`]). That accumulator phase is cyclic, not
    /// convergent: with a 2 048-tap IR this comparison depended on the *parity* of the block count
    /// since installation, and failed roughly half the time — found by measurement, not foreseen.
    /// An IR that fits entirely in the head partition has no such accumulator.
    ///
    /// What it costs, plainly: this vector exercises the IR stage's resampling check, its
    /// level ramp and its low/high cut filters, but only the direct-convolution head of the
    /// partitioned convolver. The FFT partitions are `namir-ir`'s own tests' subject
    /// (`crates/namir-ir/src/convolver.rs`'s direct-convolution reference), not this
    /// requirement's.
    const IR_FRAMES: usize = 512;
    /// Decay constant of the generated cabinet IR, in samples.
    const IR_TAU: f64 = 320.0;

    /// How long the standalone worker may take to report its recall before the test gives up.
    /// Generous: a shared, contended machine may schedule that thread late.
    const REPLAY_TIMEOUT: Duration = Duration::from_secs(30);

    /// How long the plugin is given to complete its pool-dispatched replay before its first block.
    /// See [`run_clap_configuration`] for why this is a grace period rather than a wait, and why
    /// nothing silently passes if it turns out to be too short.
    const REPLAY_GRACE: Duration = Duration::from_millis(500);

    // -------------------------------------------------------------------------------------
    // The golden vector.
    // -------------------------------------------------------------------------------------

    /// Where the two checked-in artefacts live.
    fn golden_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join("fr-cfg-020")
    }

    /// The golden input signal, as mono `f32` samples.
    fn golden_input() -> Vec<f32> {
        let path = golden_dir().join("input.f32");
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("the golden input vector {path:?} must be readable: {e}"));
        assert_eq!(
            bytes.len() % 4,
            0,
            "{path:?} is not a whole number of little-endian f32 samples"
        );
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// The golden preset document, as the bytes both a CLAP host and `AppCommand::LoadState` hand
    /// to `namir_state::State::read`.
    fn golden_preset() -> Vec<u8> {
        let path = golden_dir().join("preset.namirpreset");
        std::fs::read(&path)
            .unwrap_or_else(|e| panic!("the golden preset {path:?} must be readable: {e}"))
    }

    /// The non-default value this vector assigns to every parameter that has one.
    ///
    /// Every `REGISTRY` key appears exactly once — [`the_golden_vector_is_intact`] checks that
    /// against `REGISTRY` itself rather than trusting this list — because "identical parameter
    /// values" is a clause about the whole parameter set, not about the handful a test happened to
    /// touch. Two entries are deliberately *at* their default and say why: `global.bypass` must
    /// stay off (a bypassed chain would compare two copies of the dry signal), and the four stage
    /// `enabled` switches must stay on for the same reason.
    const PARAM_OVERRIDES: &[(&str, f32, &str)] = &[
        ("eq.enabled", 1.0, "at default: the tone stack must run"),
        ("eq.high_pass_enabled", 1.0, "non-default"),
        ("eq.high_pass_freq_hz", 65.0, "non-default"),
        ("eq.high_shelf_freq_hz", 4_500.0, "non-default"),
        ("eq.high_shelf_gain_db", -2.75, "non-default"),
        ("eq.low_pass_enabled", 1.0, "non-default"),
        ("eq.low_pass_freq_hz", 12_000.0, "non-default"),
        ("eq.low_shelf_freq_hz", 160.0, "non-default"),
        ("eq.low_shelf_gain_db", -3.5, "non-default"),
        ("eq.mid_freq_hz", 900.0, "non-default"),
        ("eq.mid_gain_db", 4.25, "non-default"),
        ("eq.mid_q", 1.4, "non-default"),
        ("gate.attack_ms", 0.5, "non-default"),
        ("gate.enabled", 1.0, "at default: the gate must run"),
        // Short deliberately, both of them: the gate's envelope is the one state here whose decay
        // constant this vector chooses, and the settling window has to be long enough (in decay
        // constants) for it to reach a flushed zero rather than a residue that still remembers how
        // many blocks it has seen. 4 ms against SETTLE_FRAMES' 500 ms is 125 of them.
        ("gate.hold_ms", 2.0, "non-default"),
        ("gate.release_ms", 4.0, "non-default"),
        ("gate.threshold_db", -55.0, "non-default"),
        (
            "global.bypass",
            0.0,
            "at default: bypass would void the comparison",
        ),
        ("global.output_ceiling_db", -1.0, "non-default"),
        ("ir.enabled", 1.0, "at default: the cabinet must run"),
        ("ir.high_cut_enabled", 1.0, "non-default"),
        ("ir.high_cut_freq_hz", 9_500.0, "non-default"),
        ("ir.level_db", -2.5, "non-default"),
        ("ir.low_cut_enabled", 1.0, "non-default"),
        ("ir.low_cut_freq_hz", 95.0, "non-default"),
        ("nam.enabled", 1.0, "at default: the amp must run"),
        ("nam.normalize_enabled", 1.0, "at default"),
        ("nam.normalize_offset_db", -4.0, "non-default"),
        ("out.gain_db", -3.0, "non-default"),
        ("trim.dc_blocker_enabled", 1.0, "at default"),
        ("trim.gain_db", 6.0, "non-default"),
    ];

    /// FR-STATE-080's embedded form of a resource: no external candidate at all, so a recall can
    /// only resolve it from the bytes the document itself carries. Keeps the golden vector
    /// self-contained — no library, no absolute paths, nothing that could resolve differently
    /// between the two shells.
    fn embedded_ref(data: Vec<u8>, display_name: &str, media_type: &str) -> FileRef {
        FileRef {
            hash: ContentHash::of(&data),
            library_relative: None,
            absolute: None,
            display_name: display_name.to_string(),
            embedded: Some(EmbeddedRef {
                media_type: media_type.to_string(),
                data,
            }),
        }
    }

    /// The golden input signal's recipe: clean, transient and saturated material, all from a seed
    /// (D-19.1). Only [`regenerate_the_golden_vector`] runs this — every ordinary run reads the
    /// checked-in bytes, so a libm difference between platforms cannot make the two configurations
    /// disagree about what they were fed.
    fn build_input_signal() -> Vec<f32> {
        let mut out = vec![0.0f32; VECTOR_FRAMES];
        let rate = SAMPLE_RATE_HZ as f64;
        let mut lcg: u64 = 0x00CF_0022;
        let mut next_noise = || {
            lcg = lcg
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((lcg >> 40) as u32 as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
        };

        for (i, sample) in out.iter_mut().enumerate() {
            let t = i as f64 / rate;
            let phase = |hz: f64| (std::f64::consts::TAU * hz * t).sin() as f32;
            *sample = match i {
                // A fade-in from silence: the gate opening, and a smoothed parameter's worth of
                // low-level material.
                0..4096 => phase(220.0) * 0.25 * (i as f32 / 4096.0),
                // Loud and low: the model's saturating region.
                4096..8192 => phase(110.0) * 0.9,
                // A hard transient.
                8192..8320 => {
                    if i == 8192 {
                        1.0
                    } else {
                        0.9 * (-((i - 8192) as f32) / 24.0).exp()
                    }
                }
                // Broadband seeded noise.
                8320..16384 => next_noise() * 0.3,
                // A chord: three partials at once.
                16384..20480 => (phase(110.0) + phase(164.81) + phase(220.0)) * 0.28,
                // A decay back into near-silence, so the gate closes again.
                _ => phase(146.83) * 0.6 * (-((i - 20480) as f32) / 900.0).exp(),
            };
        }
        out
    }

    /// Regenerates both checked-in golden artefacts from their seeds. `#[ignore]`d, in the same
    /// shape as `params.lock`'s own generator (`cargo test -p namir-params --lib -- --ignored
    /// generate_params_lock`): the artefacts are the vector, this is only the recipe that produced
    /// them.
    ///
    /// ```text
    /// cargo test -p namir-clap --features host-ext-tests --test fr_cfg_020_shell_parity \
    ///     -- --ignored regenerate_the_golden_vector
    /// ```
    #[test]
    #[ignore = "regenerates the checked-in golden vector; run explicitly after changing the recipe"]
    fn regenerate_the_golden_vector() {
        let dir = golden_dir();
        std::fs::create_dir_all(&dir).expect("the golden directory must be creatable");

        let mut bytes = Vec::with_capacity(VECTOR_FRAMES * 4);
        for sample in build_input_signal() {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(dir.join("input.f32"), &bytes).expect("the input vector must be writable");

        let mut model =
            namir_fixtures::nam::generate(namir_fixtures::nam::WaveNetShape::Nano, MODEL_SEED)
                .expect("the generated WaveNet fixture must not be degenerate");
        model.sample_rate = MODEL_RATE_HZ;
        let ir = namir_fixtures::ir::to_mono_wav_bytes(
            &namir_fixtures::ir::decaying_noise(IR_FRAMES, IR_SEED, IR_TAU),
            SAMPLE_RATE_HZ,
        );

        let mut state = State::defaults();
        for (key, value, _why) in PARAM_OVERRIDES {
            state
                .params
                .set(key, *value)
                .unwrap_or_else(|e| panic!("{key} is not a REGISTRY parameter: {e:?}"));
        }
        state.nam = Some(embedded_ref(
            model.to_json_bytes(),
            "fr-cfg-020-wavenet-nano.nam",
            "application/vnd.namir.nam+json",
        ));
        state.ir = Some(embedded_ref(ir, "fr-cfg-020-cabinet.wav", "audio/wav"));

        std::fs::write(
            dir.join("preset.namirpreset"),
            state.write_onto(&Document::empty()).to_pretty_bytes(),
        )
        .expect("the preset must be writable");
    }

    /// The guard on the golden vector itself: a parity test comparing two runs of a *trivial*
    /// vector would pass while proving nothing, so what the artefacts have to contain is checked
    /// here rather than assumed.
    #[test]
    fn the_golden_vector_is_intact() {
        let input = golden_input();
        assert_eq!(input.len(), VECTOR_FRAMES, "unexpected golden input length");
        assert!(
            input.iter().all(|s| s.is_finite()),
            "the golden input carries a non-finite sample"
        );
        assert!(
            input.iter().fold(0.0f32, |a, s| a.max(s.abs())) > 0.5,
            "the golden input must be a real signal, not near-silence"
        );

        let (state, warnings) =
            State::read(&golden_preset()).expect("the golden preset must parse cleanly");
        assert!(
            warnings.is_empty(),
            "unexpected preset warnings: {warnings:?}"
        );

        // Every REGISTRY key is spoken for, and the table above says so for exactly one reason
        // per key.
        let mut declared: Vec<&str> = PARAM_OVERRIDES.iter().map(|(k, _, _)| *k).collect();
        declared.sort_unstable();
        let mut registry: Vec<&str> = REGISTRY.iter().map(|d| d.key).collect();
        registry.sort_unstable();
        assert_eq!(
            declared, registry,
            "PARAM_OVERRIDES must name every REGISTRY parameter exactly once"
        );

        let defaults = State::defaults();
        for descriptor in REGISTRY {
            let value = state
                .params
                .get(descriptor.key)
                .expect("every REGISTRY parameter must be present in the preset");
            let (_, expected, _) = PARAM_OVERRIDES
                .iter()
                .find(|(k, _, _)| *k == descriptor.key)
                .expect("checked against REGISTRY above");
            assert_eq!(
                value, *expected,
                "the golden preset's {} does not carry the recipe's value",
                descriptor.key
            );
            if matches!(descriptor.kind, ParamKind::Continuous { .. })
                && descriptor.key != "global.bypass"
            {
                assert_ne!(
                    value,
                    defaults.params.get(descriptor.key).unwrap(),
                    "{} is at its default: the vector would not exercise it",
                    descriptor.key
                );
            }
        }

        for (slot, reference) in [("nam", &state.nam), ("ir", &state.ir)] {
            let reference = reference
                .as_ref()
                .unwrap_or_else(|| panic!("the golden preset must reference a {slot} resource"));
            let embedded = reference
                .embedded
                .as_ref()
                .unwrap_or_else(|| panic!("the golden {slot} reference must be self-contained"));
            assert_eq!(
                ContentHash::of(&embedded.data),
                reference.hash,
                "the golden {slot} reference's embedded bytes do not match its own hash"
            );
            assert!(
                reference.library_relative.is_none() && reference.absolute.is_none(),
                "the golden {slot} reference must not depend on a path outside the document"
            );
        }
    }

    // -------------------------------------------------------------------------------------
    // Configuration 1 — the CLAP plugin.
    // -------------------------------------------------------------------------------------

    /// How many silent blocks of `block` frames make up the settling window.
    fn settle_blocks(block: u32) -> usize {
        SETTLE_FRAMES.div_ceil(block as usize)
    }

    /// Runs the golden vector through the CLAP plugin's own path and returns its two output
    /// channels.
    ///
    /// The preset is handed to `clap_plugin_state.load` *before* `activate`, which is the order
    /// `crate::state_ext`'s own doc comment records most hosts using, so the replay under test is
    /// the one `crate::audio::activate` performs onto a freshly built engine.
    fn run_clap_configuration(preset: &[u8], input: &[f32], block: u32) -> [Vec<f32>; 2] {
        let (_entry, mut instance) = instantiate_default();
        let state_ext = require_plugin_extension::<PluginState>(&mut instance);
        let latency_ext = require_plugin_extension::<PluginLatency>(&mut instance);

        let mut reader = preset;
        state_ext
            .load(&mut main_thread_handle(&mut instance), &mut reader)
            .expect("the host-driven state load must succeed");

        let mut processor = activate(&mut instance, config(DEFAULT_SAMPLE_RATE, 1, MAX_BLOCK))
            .start_processing()
            .expect("processing must start");

        let mut bufs = StereoBuffers::new(MAX_BLOCK as usize);
        bufs.silence_input();

        // `activate` dispatches the replay to the pool (`crate::worker_jobs::spawn_recall`) and a
        // CLAP host has no way to observe its completion, so this is where the standalone side's
        // `AppEvent::StateLoaded` has no counterpart: the wait is a wall-clock one, sized for the
        // parse of a few-kilobyte model and the decode of a 512-sample IR. It is not
        // load-bearing for correctness —
        // the settling window below neutralises a handover that lands a few blocks late, and a
        // replay that never lands at all cannot produce output identical to the standalone
        // engine's, which is asserted to have loaded both resources before it processed anything.
        // It is here so the two engines run the same block schedule in the ordinary case.
        std::thread::sleep(REPLAY_GRACE);

        for _ in 0..settle_blocks(block) {
            bufs.process_block(&mut processor, block)
                .expect("a settling block must process");
        }

        // The premise of the settling argument, checked rather than assumed: a non-zero reported
        // latency would mean a rate converter is in the chain, and its phase is the one piece of
        // state a settling window cannot neutralise (see [`MODEL_RATE_HZ`]).
        assert_eq!(
            latency_ext.get(&mut main_thread_handle(&mut instance)),
            0,
            "the golden model declares the engine's own sample rate, so no stage may report \
             latency -- with a resampler in the chain this comparison would not be sound"
        );

        let frames = block as usize;
        let mut out = [
            Vec::with_capacity(input.len()),
            Vec::with_capacity(input.len()),
        ];
        for chunk in input.chunks_exact(frames) {
            // The same mono signal on both channels — what a host hands a mono guitar track on a
            // stereo bus, and what `namir-app`'s `ChannelConfig::MonoToStereo` duplication
            // produces on the other side of this comparison.
            bufs.input_mut(0)[..frames].copy_from_slice(chunk);
            bufs.input_mut(1)[..frames].copy_from_slice(chunk);
            bufs.poison_output(f32::NAN);
            bufs.process_block(&mut processor, block)
                .expect("a vector block must process");
            out[0].extend_from_slice(&bufs.output(0)[..frames]);
            out[1].extend_from_slice(&bufs.output(1)[..frames]);
        }

        let stopped = processor.stop_processing();
        instance.deactivate(stopped);
        drop(instance);
        out
    }

    // -------------------------------------------------------------------------------------
    // Configuration 2 — the standalone application.
    // -------------------------------------------------------------------------------------

    /// D-13.1's `AudioBackend` with no device behind it: it captures the two callbacks
    /// `namir_app::stream::open` builds so this test can drive them directly, and answers
    /// enumeration with the one configuration `crate::app::run` would have negotiated.
    ///
    /// `namir-app` has an equivalent fake of its own (`stream::FakeBackend`), but it is
    /// `#[cfg(test)] pub(crate)` and therefore invisible from outside that crate; this one is
    /// deliberately minimal and exists only to reach [`stream::open`], which is the real code
    /// under test.
    /// The capture callback `namir_app::stream::open` hands its input stream.
    type InputCallback = Box<dyn FnMut(&[f32]) + Send>;
    /// The render callback `namir_app::stream::open` hands its output stream.
    type OutputCallback = Box<dyn FnMut(&mut [f32]) + Send>;

    struct HarnessBackend {
        input: Mutex<Option<InputCallback>>,
        output: Mutex<Option<OutputCallback>>,
    }

    impl HarnessBackend {
        fn new() -> Self {
            Self {
                input: Mutex::new(None),
                output: Mutex::new(None),
            }
        }
    }

    /// A stream with nothing behind it: `namir_app::stream::open` builds both streams paused and
    /// the caller plays them, so both methods have to exist and succeed.
    struct HarnessStream;

    impl AudioStream for HarnessStream {
        fn play(&self) -> Result<(), AudioIoError> {
            Ok(())
        }
        fn pause(&self) -> Result<(), AudioIoError> {
            Ok(())
        }
    }

    impl AudioBackend for HarnessBackend {
        fn hosts(&self) -> Vec<HostInfo> {
            vec![self.default_host()]
        }
        fn default_host(&self) -> HostInfo {
            HostInfo {
                name: "fr-cfg-020".to_string(),
            }
        }
        fn input_devices(&self, _host: &HostInfo) -> Result<Vec<DeviceInfo>, AudioIoError> {
            Ok(vec![DeviceInfo {
                name: "in".to_string(),
                is_default: true,
            }])
        }
        fn output_devices(&self, _host: &HostInfo) -> Result<Vec<DeviceInfo>, AudioIoError> {
            Ok(vec![DeviceInfo {
                name: "out".to_string(),
                is_default: true,
            }])
        }
        fn input_configs(
            &self,
            _host: &HostInfo,
            _device: &DeviceInfo,
        ) -> Result<Vec<SupportedConfigRange>, AudioIoError> {
            Ok(vec![SupportedConfigRange {
                channels: 1,
                min_sample_rate_hz: SAMPLE_RATE_HZ,
                max_sample_rate_hz: SAMPLE_RATE_HZ,
                buffer_size: BufferSizeRange::Unknown,
            }])
        }
        fn output_configs(
            &self,
            _host: &HostInfo,
            _device: &DeviceInfo,
        ) -> Result<Vec<SupportedConfigRange>, AudioIoError> {
            Ok(vec![SupportedConfigRange {
                channels: 2,
                min_sample_rate_hz: SAMPLE_RATE_HZ,
                max_sample_rate_hz: SAMPLE_RATE_HZ,
                buffer_size: BufferSizeRange::Unknown,
            }])
        }
        fn supports_exclusive(
            &self,
            _host: &HostInfo,
            _device: &DeviceInfo,
            _params: StreamParams,
        ) -> ExclusiveModeOutcome {
            ExclusiveModeOutcome::Unsupported
        }
        fn build_input_stream(
            &self,
            _host: &HostInfo,
            _device: &DeviceInfo,
            _params: StreamParams,
            on_data: Box<dyn FnMut(&[f32]) + Send>,
            _on_error: Box<dyn FnMut(StreamFailure) + Send>,
            _timeout: Duration,
        ) -> Result<Box<dyn AudioStream>, AudioIoError> {
            *self.input.lock().unwrap() = Some(on_data);
            Ok(Box::new(HarnessStream))
        }
        fn build_output_stream(
            &self,
            _host: &HostInfo,
            _device: &DeviceInfo,
            _params: StreamParams,
            on_data: Box<dyn FnMut(&mut [f32]) + Send>,
            _on_error: Box<dyn FnMut(StreamFailure) + Send>,
            _timeout: Duration,
        ) -> Result<Box<dyn AudioStream>, AudioIoError> {
            *self.output.lock().unwrap() = Some(on_data);
            Ok(Box::new(HarnessStream))
        }
    }

    /// The stream configuration `crate::app::run` settles on for a mono input and a stereo output
    /// device at 48 kHz — `negotiate_channels`' own defaults (1 in, 2 out) and the
    /// `output_channels >= 2` branch that selects `ChannelConfig::MonoToStereo`.
    fn app_stream_setup(backend: &HarnessBackend) -> StreamSetup<'_> {
        let params = |channels: u16| StreamParams {
            sample_rate_hz: SAMPLE_RATE_HZ,
            buffer_frames: Some(MAX_BLOCK),
            channels,
            share_mode: ShareMode::Shared,
        };
        StreamSetup {
            backend,
            input_host: backend.default_host(),
            input_device: DeviceInfo {
                name: "in".to_string(),
                is_default: true,
            },
            input_params: params(1),
            output_host: backend.default_host(),
            output_device: DeviceInfo {
                name: "out".to_string(),
                is_default: true,
            },
            output_params: params(2),
            channel_config: ChannelConfig::MonoToStereo,
            input_channel_index: 0,
            output_channel_left: 0,
            output_channel_right: 1,
            max_block_size: MAX_BLOCK as usize,
        }
    }

    /// Blocks until `worker` reports the recall `AppCommand::LoadState` was given.
    fn wait_for_state_loaded(worker: &WorkerHandle) -> RecallOutcomeSummary {
        let deadline = Instant::now() + REPLAY_TIMEOUT;
        loop {
            for event in worker.drain_events() {
                if let AppEvent::StateLoaded { outcome, error, .. } = event {
                    assert!(
                        error.is_none(),
                        "the golden preset failed to load: {error:?}"
                    );
                    return outcome.expect("a readable preset must produce a recall outcome");
                }
            }
            assert!(
                Instant::now() < deadline,
                "the standalone worker never reported the preset recall"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Runs the golden vector through the standalone application's own path and returns its two
    /// output channels, de-interleaved from the device buffer `crate::stream`'s output callback
    /// writes.
    fn run_app_configuration(
        preset: &[u8],
        input: &[f32],
        block: u32,
        work_dir: &Path,
    ) -> [Vec<f32>; 2] {
        let ctx = PrepareContext::new(
            SampleRate::new(SAMPLE_RATE_HZ).expect("48 kHz is a valid sample rate"),
            MAX_BLOCK as usize,
            ChannelConfig::MonoToStereo,
        )
        .expect("the engine must prepare");
        let (engine, endpoint) = build_default_engine(&ctx).expect("the engine must build");
        let instance = SharedInstance::new(Instance::new(EngineConfig { ctx }, endpoint));

        // `open_at`, never `open_default`: a `LibraryService` rooted in this test's own directory
        // cannot touch the developer's real index (see `tests/support/mod.rs`'s HAZARD). Nothing
        // here ever asks for a scan, and the golden preset resolves from its own embedded bytes,
        // so the library is inert by construction.
        let (library, _warnings) = LibraryService::open_at(work_dir);
        let preset_path = work_dir.join("fr-cfg-020.namirpreset");
        std::fs::write(&preset_path, preset).expect("the preset must be writable");

        let worker = WorkerHandle::spawn(WorkerContext {
            instance: instance.clone(),
            cache: ResourceCache::shared(),
            library: Arc::new(library),
            pool: ThreadPool::new(),
            library_roots: Vec::new(),
            state: Arc::new(Mutex::new(State::defaults())),
        });

        // Before any audio: `Instance::recall` submits into the command ring without needing the
        // audio thread to be running, so the offer is already there when the first block runs.
        worker.send(AppCommand::LoadState(preset_path));
        let recall = wait_for_state_loaded(&worker);
        assert!(
            matches!(recall.nam, LoadOutcomeSummary::Loaded { .. }),
            "the golden model must load into the standalone engine, got {:?}",
            recall.nam
        );
        assert!(
            matches!(recall.ir, LoadOutcomeSummary::Loaded { .. }),
            "the golden IR must load into the standalone engine, got {:?}",
            recall.ir
        );

        let backend = HarnessBackend::new();
        let xruns = Arc::new(XrunCounter::new());
        let failures = Arc::new(AtomicUsize::new(0));
        let streams = {
            let failures = Arc::clone(&failures);
            stream::open(
                app_stream_setup(&backend),
                engine,
                Arc::clone(&xruns),
                move |_direction: Direction, _failure: StreamFailure| {
                    failures.fetch_add(1, Ordering::SeqCst);
                },
            )
            .expect("the streams must open")
        };
        streams.play().expect("the streams must play");

        let mut input_cb = backend
            .input
            .lock()
            .unwrap()
            .take()
            .expect("stream::open must have built an input stream");
        let mut output_cb = backend
            .output
            .lock()
            .unwrap()
            .take()
            .expect("stream::open must have built an output stream");

        let frames = block as usize;
        let silence = vec![0.0f32; frames];
        let mut device_buffer = vec![0.0f32; frames * 2];
        for _ in 0..settle_blocks(block) {
            input_cb(&silence);
            output_cb(&mut device_buffer);
        }

        let mut out = [
            Vec::with_capacity(input.len()),
            Vec::with_capacity(input.len()),
        ];
        for chunk in input.chunks_exact(frames) {
            input_cb(chunk);
            device_buffer.fill(f32::NAN);
            output_cb(&mut device_buffer);
            out[0].extend(device_buffer.chunks_exact(2).map(|f| f[0]));
            out[1].extend(device_buffer.chunks_exact(2).map(|f| f[1]));
        }

        assert_eq!(
            xruns.count(),
            0,
            "the bridge underran: the standalone run was fed padding rather than the vector"
        );
        assert_eq!(
            failures.load(Ordering::SeqCst),
            0,
            "a stream reported a failure"
        );

        drop(streams);
        drop(worker);
        out
    }

    // -------------------------------------------------------------------------------------
    // The comparison.
    // -------------------------------------------------------------------------------------

    /// Asserts `a` and `b` are the same sequence of `f32` **bit patterns** — `==` on `f32` would
    /// call two `NaN`s different and `+0.0`/`-0.0` the same, and neither is what "bit-identical"
    /// means.
    fn assert_bit_identical(a: &[f32], b: &[f32], what: &str) {
        assert_eq!(a.len(), b.len(), "{what}: different lengths");
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert!(
                x.to_bits() == y.to_bits(),
                "{what}: first difference at frame {i}: CLAP {x:e} ({:#010x}) vs standalone \
                 {y:e} ({:#010x})",
                x.to_bits(),
                y.to_bits()
            );
        }
    }

    /// FR-CFG-020, end to end: the same golden vector — the same input samples, the same
    /// parameter values, the same model and IR — driven through the CLAP plugin's own
    /// `clap_plugin.process` path and through the standalone application's own `cpal` output
    /// callback, at three block sizes, compared sample for sample.
    ///
    /// The two engines are prepared with different `ChannelConfig` variants, and that is the
    /// shipping configuration of each rather than an oversight: the plugin declares fixed stereo
    /// I/O (FR-CLAP-030), the application widens a single captured channel (`MonoToStereo`). The
    /// variants are engine-equivalent — every stage sizes itself from
    /// `ChannelConfig::output_channels()`, which is 2 for both, and no stage reads the variant
    /// itself — so what reaches `Chain::process` on both sides is two channels carrying the same
    /// samples. If that ever stops being true this test is the thing that says so.
    ///
    /// **Why a pass cannot mean "neither side loaded anything".** The standalone half asserts
    /// `LoadOutcomeSummary::Loaded` for both the model and the IR before it processes a sample, so
    /// one engine is known to be carrying them; an engine that was not would not produce the same
    /// samples as one that was. The two shells reach those resources through the same process-wide
    /// `namir_worker::ResourceCache` (D-8.2/FR-CLAP-090), so what they share is not two equal
    /// copies but one.
    // trace: FR-CFG-020
    #[test]
    fn both_product_configurations_produce_bit_identical_output_for_the_same_golden_vector() {
        let preset = golden_preset();
        let input = golden_input();
        // Process-unique: the standalone half writes the preset to a real file (that is what
        // `AppCommand::LoadState` takes) and opens a `LibraryService` over this directory, and two
        // copies of this binary running at once would otherwise read each other's half-written
        // document. Found by running eight copies concurrently, which is also how the bit-identity
        // claim below was stress-tested.
        let work_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("fr-cfg-020-{}", std::process::id()));
        std::fs::create_dir_all(&work_dir).expect("the work directory must be creatable");

        for block in BLOCK_SIZES {
            let clap = run_clap_configuration(&preset, &input, block);
            let app = run_app_configuration(&preset, &input, block, &work_dir);

            for (channel, (clap_channel, app_channel)) in clap.iter().zip(app.iter()).enumerate() {
                assert_eq!(clap_channel.len(), input.len());
                assert!(
                    clap_channel.iter().all(|s| s.is_finite()),
                    "the plugin produced a non-finite sample on channel {channel}"
                );
                assert_bit_identical(
                    clap_channel,
                    app_channel,
                    &format!("{block}-frame blocks, channel {channel}"),
                );
            }

            // Two silent outputs would compare equal and prove nothing.
            let peak = clap[0].iter().fold(0.0f32, |a, s| a.max(s.abs()));
            assert!(
                peak > 0.001,
                "both configurations produced near-silence (peak {peak:e}); the comparison would \
                 be vacuous"
            );
        }
    }
}
