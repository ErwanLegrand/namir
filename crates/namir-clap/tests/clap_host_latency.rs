//! FR-CLAP-040: "The plugin shall report its total latency in samples and shall notify the host
//! whenever that latency changes, including as a result of a model change under FR-NAM-050."
//! *Verify:* I.
//!
//! Driven through the real C vtable by the shared `support` harness — **read that module's doc
//! comment first**, in particular the HAZARD about `start_library_scan` and the developer's real
//! library index. Nothing here starts a scan; the one thing this file does to `SharedInner` beyond
//! instantiating it is hand it a state document, which is a read-and-adopt path.
//!
//! # Feature gate (D-18.7)
//!
//! Everything below needs the `host-ext-tests` feature: `PluginLatency::get` and
//! `PluginState::load` are `clack-extensions`' *host* halves, and `TestHost` only declares
//! `HostLatency` — the extension the plugin's `changed()` notification travels over — under that
//! feature. With the feature off this file compiles to a test binary with no tests in it. It is
//! `.github/workflows/ci.yml`'s second, required `cargo test -p namir-clap --features
//! host-ext-tests` step that actually runs it; the default `cargo test --workspace` does not.
//!
//! # How a latency change is provoked without touching a real file
//!
//! `crates/namir-engine/src/stages/ir.rs:704-710` returns `0` from `latency_samples`
//! *unconditionally* and says so in as many words, and every other stage in the fixed six-stage
//! chain does the same, so `Chain::latency_samples` — the "total" this requirement is about — has
//! exactly one non-zero source in 1.0: `nam.rs`'s `SlotResampler`, which exists on a loaded slot
//! only when the model's declared sample rate differs from the engine's (D-9.2). That is
//! FR-NAM-050's condition verbatim, and it is the *only* way the reported figure can move.
//!
//! So: the plugin is activated at 48 kHz and handed, through the host-driven `state` extension, a
//! `namir_state::Document` whose `nam` reference declares 44.1 kHz and carries the model itself as
//! FR-STATE-080 embedded base64. `namir_worker::recall::locate` falls through every external
//! candidate (there are none — no `library_relative`, no `absolute`, and the reference's hash is a
//! synthetic model's) to the embedded copy, so no file is written anywhere and the developer's
//! library is neither read for a hit nor modified.
//!
//! The model itself is generated here rather than taken from `namir-fixtures`, which is not a
//! dependency of this crate: a `.nam` file is JSON, `serde_json` already *is* a dev-dependency,
//! and the topology below is the same minimal WaveNet shape `namir-engine`'s own `nam.rs` tests
//! build against the public `NamFile` surface. It stays inside D-19.1 — generated from fixed
//! values, nothing captured.
//!
//! # Why the tag is plain rather than partial (D-23.1)
//!
//! *Does the requirement, or its `Verify:` method, quantify over a set?* "Whenever" does, over
//! latency-change events. The paragraph above is why that set is spanned rather than sampled: the
//! chain has one latency source, this test drives it in both directions, and it drives **both**
//! notification paths the plugin implements — `crate::audio`'s `request_callback` →
//! `crate::main_thread::on_main_thread` → `request_restart` → the announcement inside the next
//! `activate()` (used while active, because `HostLatency::changed`'s own contract forbids
//! announcing a change while active), and `on_main_thread`'s other branch, which announces
//! directly when the host services the callback after deactivating. There is no third path and no
//! second source.
//!
//! *Does this artifact execute the method as written?* `Verify: I` — an integration test. This is
//! one: the real `NamirClapPlugin` through the real C vtable, the real `state` and `latency`
//! extensions, the real worker pool and command ring. The reported figure is not merely asserted
//! non-zero either; it is compared against an independently constructed `namir_engine::AudioEngine`
//! recalling the same model, so "reports its total latency in samples" is checked against a number
//! derived outside the plugin rather than against itself.
//!
//! `docs/manual-tests/fr-clap-040-latency-restart.md` remains supplementary evidence for what only
//! a real host can show (that a real DAW acts on the restart request and re-queries); per D-18.6 it
//! is not the traced artifact for a `Verify: I` requirement, and this file is.

mod support;

#[cfg(feature = "host-ext-tests")]
mod host_ext {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use clack_extensions::latency::PluginLatency;
    use clack_extensions::state::PluginState;
    use namir_core::{ChannelConfig, ContentHash, SampleRate};
    use namir_engine::{PrepareContext, StageIo, build_default_engine};
    use namir_state::{Document, EmbeddedRef, FileRef, FileResolver, RelPath, State};
    use namir_worker::recall::ResourceRecall;
    use namir_worker::{EngineConfig, Instance, ResourceCache};

    use super::support::{
        DEFAULT_MAX_BLOCK, DEFAULT_SAMPLE_RATE, StereoBuffers, activate_default, audio_section,
        instantiate_default, main_thread_handle, require_plugin_extension, sine_1k,
    };

    /// Block size every block this file processes uses — the harness's own ceiling, so one
    /// `StereoBuffers` covers every limb without a `truncate` in play.
    const BLOCK: u32 = DEFAULT_MAX_BLOCK;

    /// The declared sample rate of the model this test loads. Any value other than the engine's own
    /// 48 kHz would do; 44.1 kHz is the one a real user actually hits.
    const MODEL_RATE_HZ: u32 = 44_100;

    /// Amplitude of the probe tone: -12 dBFS, far above the gate's -70 dBFS default threshold, so
    /// the chain is not sitting closed while the handover happens.
    const AMPLITUDE: f32 = 0.25;

    /// The generated model's output scaling — also its trailing weight, per the `.nam` layout.
    const HEAD_SCALE: f32 = 0.5;

    /// How long any one wait-for-the-worker limb may take before the test gives up. Generous: the
    /// work being waited on is a parse of a ~1 kB model plus `Instance::unload`'s R-7 serialisation
    /// sleep (~24 ms), and a shared, contended machine may schedule that pool thread late.
    const LIMB_TIMEOUT: Duration = Duration::from_secs(30);

    // ---------------------------------------------------------------------------------------
    // The generated model, and the state document that carries it.
    // ---------------------------------------------------------------------------------------

    /// A minimal but real single-layer-array WaveNet `.nam` document, declaring [`MODEL_RATE_HZ`].
    ///
    /// Field names and the flat-weight ordering follow `namir_nam::file::NamFile` /
    /// `LayerArrayConfig`; the weight count is derived from the topology below exactly as
    /// `namir-engine`'s `nam.rs` test helper derives it, with `head_scale` repeated as the trailing
    /// weight. Deterministic by construction — no `rand`, nothing captured (D-19.1).
    fn model_json_bytes() -> Vec<u8> {
        const MODEL_CHANNELS: usize = 2;
        const INPUT_SIZE: usize = 1;
        const CONDITION_SIZE: usize = 1;
        const KERNEL_SIZE: usize = 2;
        const HEAD_SIZE: usize = 1;
        const DILATIONS: [usize; 1] = [1];

        let mut count = MODEL_CHANNELS * INPUT_SIZE; // rechannel, no bias
        for _ in DILATIONS {
            count += MODEL_CHANNELS * MODEL_CHANNELS * KERNEL_SIZE; // dilated weight
            count += MODEL_CHANNELS; // dilated bias
            count += MODEL_CHANNELS * CONDITION_SIZE; // mixin, no bias
            count += MODEL_CHANNELS * MODEL_CHANNELS; // residual weight
            count += MODEL_CHANNELS; // residual bias
        }
        count += HEAD_SIZE * MODEL_CHANNELS; // head rechannel

        let mut weights: Vec<f32> = (0..count).map(|i| 0.01 * ((i % 7) as f32 - 3.0)).collect();
        weights.push(HEAD_SCALE);

        serde_json::to_vec(&serde_json::json!({
            "architecture": "WaveNet",
            "sample_rate": MODEL_RATE_HZ,
            "config": {
                "layers": [{
                    "input_size": INPUT_SIZE,
                    "condition_size": CONDITION_SIZE,
                    "channels": MODEL_CHANNELS,
                    "dilations": DILATIONS,
                    "activation": "Tanh",
                    "kernel_size": KERNEL_SIZE,
                    "head_size": HEAD_SIZE,
                    "head_bias": false,
                    "gated": false,
                }],
                "head_scale": HEAD_SCALE,
            },
            "weights": weights,
        }))
        .expect("a fixed JSON value must serialise")
    }

    /// FR-STATE-080's embedded form of `model`: no external candidate at all, so
    /// `namir_worker::recall::locate` can only resolve it from the bytes carried in the document.
    fn embedded_nam_reference(model: &[u8]) -> FileRef {
        FileRef {
            hash: ContentHash::of(model),
            library_relative: None,
            absolute: None,
            display_name: "fr-clap-040-44k1.nam".to_string(),
            embedded: Some(EmbeddedRef {
                media_type: "application/vnd.namir.nam+json".to_string(),
                data: model.to_vec(),
            }),
        }
    }

    /// The bytes a host hands to `clap_plugin_state.load`. Built through the real
    /// `State`/`Document` writers, so the base64 encoding is the format's own rather than this
    /// file's (`namir-clap` has no `base64` dependency, and should not gain one for a test).
    fn state_document_bytes(model: &[u8]) -> Vec<u8> {
        let mut state = State::defaults();
        state.nam = Some(embedded_nam_reference(model));
        state.write_onto(&Document::empty()).to_pretty_bytes()
    }

    /// A resolver that finds nothing — every external candidate misses, exactly as it does inside
    /// the plugin (whose own resolver is a real `LibraryResolver` over an index that has never seen
    /// this synthetic model).
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

    /// What `Chain::latency_samples()` reports for `model` at the engine configuration the plugin
    /// activates with — measured on an `AudioEngine` this test builds itself, with its own
    /// `ResourceCache` and no plugin, no CLAP and no worker pool involved.
    ///
    /// This is the number the plugin's `clap_plugin_latency.get` is compared against, so that
    /// "reports its total latency in samples" is checked against a figure derived outside the
    /// thing under test rather than against the thing under test's own memory of it.
    fn engine_latency_for(model: &[u8]) -> u32 {
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

        let mut state = State::defaults();
        state.nam = Some(embedded_nam_reference(model));
        let outcome = instance.recall(&cache, &state, &NoResolver);
        assert!(
            matches!(outcome.nam, ResourceRecall::Loaded(_)),
            "the generated model must load into a plain engine before it is worth asking a \
             plugin to load it, got {:?}",
            outcome.nam
        );

        // The command sits in the ring until the audio side drains it, and `nam.rs` only moves
        // `active` (and therefore the reported latency) once the handover crossfade completes --
        // 20 ms, so a handful of blocks. 64 is ~680 ms of audio, far more than enough.
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

    // ---------------------------------------------------------------------------------------
    // The test.
    // ---------------------------------------------------------------------------------------

    /// FR-CLAP-040, end to end, in four limbs — see this file's module doc comment for why this
    /// spans the requirement rather than sampling it.
    ///
    /// 0. **Baseline.** Activated at 48 kHz with nothing loaded, the plugin reports `0`
    ///    (NFR-PERF-020's "zero when no sample-rate conversion is active") and asks for nothing.
    /// 1. **The model change under FR-NAM-050.** A 44.1 kHz model arrives through the host's
    ///    `state` extension. The audio thread observes the new figure, flags it, and calls
    ///    `request_callback` — and, correctly, does *not* announce it itself. Servicing that
    ///    callback while the plugin is active produces `request_restart`, per
    ///    `HostLatency::changed`'s own contract, and the new figure is already readable through
    ///    `clap_plugin_latency.get`.
    /// 2. **The restart the plugin asked for.** Deactivate/reactivate; `activate()` announces the
    ///    change with `HostLatency::changed`, which is the only point CLAP permits it while a
    ///    plugin is being (re)activated. The freshly built engine reports `0` again — the model is
    ///    replayed asynchronously (`crate::worker_jobs::spawn_recall`), so this is a genuine second
    ///    transition rather than a repeat of the first.
    /// 3. **The same change announced directly.** The replay lands, the latency moves back off
    ///    zero, and this time the host deactivates *before* servicing the callback — the other
    ///    branch of `on_main_thread`, which announces directly because a restart is meaningless
    ///    for an inactive plugin.
    // trace: FR-CLAP-040
    #[test]
    fn a_model_change_that_adds_resampling_latency_is_reported_and_notified_on_every_transition() {
        let model = model_json_bytes();
        let expected = engine_latency_for(&model);
        assert!(
            expected > 0,
            "a {MODEL_RATE_HZ} Hz model in a {DEFAULT_SAMPLE_RATE} Hz engine must engage D-9.2's \
             resampler and therefore report non-zero latency -- with zero there is no change for \
             this test to observe"
        );
        let document = state_document_bytes(&model);

        let (_entry, mut instance) = instantiate_default();
        let latency = require_plugin_extension::<PluginLatency>(&mut instance);
        let state = require_plugin_extension::<PluginState>(&mut instance);

        // -- Limb 0: baseline ---------------------------------------------------------------
        let mut processor = activate_default(&mut instance)
            .start_processing()
            .expect("processing must start");
        let mut bufs = StereoBuffers::default_size();
        let tone = sine_1k(bufs.max_frames(), DEFAULT_SAMPLE_RATE, AMPLITUDE);
        bufs.fill_input(|_channel, frame| tone[frame]);

        for _ in 0..4 {
            audio_section(|| bufs.process_block(&mut processor, BLOCK))
                .expect("a baseline block must process");
        }
        assert_eq!(
            latency.get(&mut main_thread_handle(&mut instance)),
            0,
            "with nothing loaded no stage adds delay, so the reported total must be zero"
        );

        instance.access_shared_handler(|shared| shared.reset_request_counts());
        instance.access_handler_mut(|main_thread| main_thread.reset_callback_counts());

        // -- Limb 1: the model change, and the restart-mediated notification -----------------
        let mut reader = document.as_slice();
        state
            .load(&mut main_thread_handle(&mut instance), &mut reader)
            .expect("the host-driven state load must succeed");

        let blocks = process_until(&mut bufs, &mut processor, LIMB_TIMEOUT, "limb 1", || {
            instance.access_shared_handler(|shared| shared.callback_requests()) > 0
        });

        assert_eq!(
            instance.access_shared_handler(|shared| shared.restart_requests()),
            0,
            "`request_restart` is a [main-thread] decision; the audio thread must only ask for a \
             callback (`src/audio.rs`'s `publish_latency`)"
        );
        assert_eq!(
            instance.access_handler_mut(|main_thread| main_thread.latency_changes()),
            0,
            "`HostLatency::changed` must not be called while the plugin is active -- that is the \
             contract the whole restart dance exists to honour"
        );
        let reported = latency.get(&mut main_thread_handle(&mut instance));
        assert_eq!(
            reported, expected,
            "after {blocks} blocks the plugin reports {reported} samples of latency, but an \
             independently built engine loading the same model reports {expected}"
        );

        instance.call_on_main_thread_callback();
        assert_eq!(
            instance.access_shared_handler(|shared| shared.restart_requests()),
            1,
            "servicing the callback while active must produce exactly one restart request"
        );
        assert_eq!(
            instance.access_handler_mut(|main_thread| main_thread.latency_changes()),
            0,
            "the announcement belongs to the next activate(), not to on_main_thread"
        );

        // -- Limb 2: the restart, and the announcement inside activate() ---------------------
        let stopped = processor.stop_processing();
        instance.deactivate(stopped);
        instance.access_shared_handler(|shared| shared.reset_request_counts());
        instance.access_handler_mut(|main_thread| main_thread.reset_callback_counts());

        let mut processor = activate_default(&mut instance)
            .start_processing()
            .expect("processing must restart");

        assert_eq!(
            instance.access_handler_mut(|main_thread| main_thread.latency_changes()),
            1,
            "activate() must announce the latency -- this is the notification the restart the \
             plugin requested was for"
        );
        assert_eq!(
            latency.get(&mut main_thread_handle(&mut instance)),
            0,
            "the freshly built engine has no model yet (the replay is dispatched to the worker \
             pool), so the announced figure is zero again"
        );

        // -- Limb 3: the replay lands, and the callback is serviced while inactive ------------
        let blocks = process_until(&mut bufs, &mut processor, LIMB_TIMEOUT, "limb 3", || {
            instance.access_shared_handler(|shared| shared.callback_requests()) > 0
        });

        let reported = latency.get(&mut main_thread_handle(&mut instance));
        assert_eq!(
            reported, expected,
            "after {blocks} blocks the replayed model must restore the same reported latency"
        );
        assert_eq!(
            instance.access_shared_handler(|shared| shared.restart_requests()),
            0,
            "the audio thread must still only ask for a callback"
        );
        assert_eq!(
            instance.access_handler_mut(|main_thread| main_thread.latency_changes()),
            1,
            "only activate()'s own announcement so far"
        );

        let stopped = processor.stop_processing();
        instance.deactivate(stopped);
        instance.call_on_main_thread_callback();

        assert_eq!(
            instance.access_handler_mut(|main_thread| main_thread.latency_changes()),
            2,
            "with the plugin inactive the change is announced directly -- `on_main_thread`'s \
             other branch, and the second of the two notification paths this plugin has"
        );
        assert_eq!(
            instance.access_shared_handler(|shared| shared.restart_requests()),
            0,
            "an inactive plugin needs no restart to change its latency"
        );
        assert_eq!(
            latency.get(&mut main_thread_handle(&mut instance)),
            expected,
            "the figure the host reads after being notified must be the new one"
        );

        drop(instance); // `clap_plugin.destroy`
    }

    /// Processes blocks until `done` returns `true`, returning how many it took.
    ///
    /// Panics with `label` once `timeout` elapses — the conditions this file waits on are all
    /// "some worker-pool job finished and the audio thread saw the result", so a hang here means a
    /// real failure (the model did not load, or the audio thread never noticed) rather than a slow
    /// machine, and a bounded wait turns that into a readable failure instead of a stuck run.
    fn process_until(
        bufs: &mut StereoBuffers,
        processor: &mut clack_host::prelude::StartedPluginAudioProcessor<super::support::TestHost>,
        timeout: Duration,
        label: &str,
        mut done: impl FnMut() -> bool,
    ) -> usize {
        let started = Instant::now();
        let mut blocks = 0usize;
        loop {
            // Wrapped in D-7.5's marker deliberately: the blocks this drives are exactly the ones
            // carrying D-8.1's install and the handover crossfade, which is where an allocation on
            // the audio thread would actually be. `assert_no_alloc`'s forbid flag is thread-local,
            // so the worker thread parsing the model concurrently is not implicated.
            audio_section(|| bufs.process_block(processor, BLOCK))
                .unwrap_or_else(|e| panic!("{label}: a block must process: {e}"));
            blocks += 1;
            if done() {
                return blocks;
            }
            assert!(
                started.elapsed() < timeout,
                "{label}: gave up after {blocks} blocks and {:?}",
                started.elapsed()
            );
            std::thread::yield_now();
        }
    }
}
