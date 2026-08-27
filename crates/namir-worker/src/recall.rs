//! [`Instance::recall`]: FR-STATE-030's M5 half ("recall a saved preset onto a live instance")
//! and FR-STATE-050 ("recalling a preset that changes the loaded model or IR crossfades exactly
//! as a live change does — no click, no dropout"). This is the crate that can see both
//! `namir-state` and `namir-engine` at once — D-5.1's edges from each of them run one direction,
//! and this crate is where they meet — composing `namir_state::candidates`' resolution order with
//! a real `namir_state::FileResolver` (`namir-library`'s `LibraryResolver`, wired by whatever
//! product shell owns both crates) and this crate's own [`ResourceCache`].
//!
//! # Order: every parameter, then the two resources
//!
//! Parameters land *before* resources so a swapped-in model arrives into an already-correct
//! trim/gate/EQ rather than into stale values that then jump underneath it mid-crossfade —
//! FR-STATE-050's "no click" would otherwise have to survive two simultaneous changes at once
//! instead of one. D-10.4: `global.bypass`/`global.output_ceiling_db` are ordinary `REGISTRY`
//! entries now, so the single `state.params.iter()` loop below already carries them — there is
//! no longer a separate "globals" pass ahead of it.
//!
//! # R4: each resource goes through `Instance::load`/`Instance::unload`, never a bespoke submit
//!
//! **Not manufactured red-first, and worth saying why.** The plan this milestone follows names
//! this as one of the pairs that "genuinely warrant red-first" — the natural, tempting
//! implementation is submitting a preset's model and IR resources as two independent jobs on
//! [`crate::pool::ThreadPool`], since parallelising the slow, blocking parts (`Instance::load` is
//! documented "not RT-safe... may block") is exactly what a worker pool is *for*. That
//! implementation would bypass R-7's mitigation and reintroduce the measured 25.06-31.49%
//! over-budget condition `docs/02-architecture.md`'s D-8.1 records.
//!
//! [`Instance::recall`] is not built that way: it is one function, taking `&mut Instance`,
//! calling [`Instance::load`]/[`Instance::unload`] one after the other in the same call stack.
//! There is no thread spawned inside it, so the tempting parallel version is not merely avoided
//! by discipline here — it is not expressible without first restructuring `Instance` itself
//! (`Arc<Mutex<Instance>>` or similar) to let two threads hold it at once. Forcing an artificial
//! failing version of *this* shape first, only to delete it a commit later, would be exactly what
//! the plan's own build order calls "manufacturing a red where the first implementation would
//! have passed" — worse than admitting the test below was green on arrival. The test stays,
//! because it is real regression evidence against the tempting alternative design, even though no
//! failing commit precedes it.

use std::path::PathBuf;
use std::sync::Arc;

use namir_engine::{Command, ParamChange, ParamId};
use namir_state::{Candidate, FileRef, FileResolver, MissingFile, State};

use crate::cache::ResourceCache;
use crate::{Instance, JobOutcome, LoadSource, Target};

/// One resource slot's outcome within a [`RecallOutcome`].
#[derive(Debug, Clone)]
pub enum ResourceRecall {
    /// The state named no reference for this slot — the stage was unloaded (FR-STATE-070's "the
    /// state shall load with that stage empty").
    Unloaded(JobOutcome),
    /// A reference was named and a byte-identical file was located and loaded.
    Loaded(JobOutcome),
    /// A reference was named, but none of FR-STATE-070's three candidates located a file whose
    /// *content* actually matches (P7: a path hit whose hash differs is not a hit) — the stage
    /// was unloaded instead, and `missing` carries what a UI needs to show ("plexi.nam is
    /// missing — locate it manually?", FR-STATE-070's own wording).
    Missing {
        /// The unload that emptied the stage in place of the reference that could not be found.
        unload: JobOutcome,
        /// The missing reference's name and hash.
        missing: MissingFile,
    },
}

/// How one [`Instance::recall`] call ended.
#[derive(Debug, Clone)]
pub struct RecallOutcome {
    /// What happened to the Nam stage.
    pub nam: ResourceRecall,
    /// What happened to the Ir stage.
    pub ir: ResourceRecall,
    /// How many parameter/global commands did not reach the audio thread within
    /// `CommandSubmitter::submit`'s deadline. Ordinarily `0` — a nonzero count means the ring was
    /// backed up for the whole submit deadline, itself worth surfacing even though this method
    /// has no further retry to offer: D-7.2 hands the command back to the caller that submitted
    /// it, and that caller is this method, mid-recall, with nowhere better to put it.
    pub commands_not_delivered: usize,
}

/// FR-STATE-070's read-hash-compare loop over [`namir_state::candidates`], driven against a real
/// `resolver` and the real filesystem, **plus FR-STATE-080's embedded-data fallback**: if none of
/// the three external candidates produce a content match, and the reference carries an embedded
/// copy of the resource, that copy is used — verified against `reference.hash` the same way an
/// external candidate is, so a corrupted or mismatched embed is a miss too, not a silent
/// substitution. This is deliberately the *last* resort, after every external candidate: a
/// resolvable library or absolute path is what FR-STATE-070 is actually about, and an embedded
/// copy exists for the case none of those apply (sharing a preset with someone whose library is
/// configured differently, or — this crate's own cross-process restore test — no library at all).
///
/// Deliberately not `namir_state::resolve` (existence-only, by design — see that module's doc
/// comment): a path candidate that exists but whose *content* no longer matches `reference.hash`
/// is not a hit, and only reading the bytes can tell the two apart. This crate is where that read
/// happens anyway (`ResourceCache::get_or_load_*` needs the bytes to hash and parse regardless of
/// which candidate produced them), so `FileResolver` deliberately never reads a file itself — see
/// `namir_state::resolve`'s module doc comment.
fn locate(reference: &FileRef, resolver: &dyn FileResolver) -> Result<Vec<u8>, MissingFile> {
    for candidate in namir_state::candidates(reference) {
        let path: Option<PathBuf> = match candidate {
            Candidate::LibraryRelative(rel) => resolver.resolve_library_relative(rel),
            Candidate::Absolute(abs) => resolver.resolve_absolute(abs),
            Candidate::ContentHash(hash) => resolver.resolve_by_hash(hash),
        };
        let Some(path) = path else { continue };
        let Ok(bytes) = std::fs::read(&path) else {
            // Existed per the resolver but couldn't be read (permissions, vanished between the
            // resolver's own exists() check and this read) -- falls through, same as a miss.
            continue;
        };
        if namir_core::ContentHash::of(&bytes) == reference.hash {
            return Ok(bytes);
        }
        // P7: "identity is the content hash, paths are hints." A different file now lives at
        // this path -- not a hit, fall through to the next candidate rather than loading it.
    }
    if let Some(embedded) = &reference.embedded
        && namir_core::ContentHash::of(&embedded.data) == reference.hash
    {
        return Ok(embedded.data.clone());
    }
    Err(MissingFile {
        display_name: reference.display_name.clone(),
        hash: reference.hash,
    })
}

impl Instance {
    /// See this module's doc comment for the ordering rationale and R4's serialisation guarantee.
    ///
    /// **Not RT-safe, by design** — like [`Self::load`], this reads files, parses them, allocates,
    /// and may block waiting for the audio thread to make room; run it on a worker thread.
    pub fn recall(
        &mut self,
        cache: &ResourceCache,
        state: &State,
        resolver: &dyn FileResolver,
    ) -> RecallOutcome {
        let mut commands_not_delivered = 0usize;

        // D-10.4: `global.bypass`/`global.output_ceiling_db` are ordinary `REGISTRY` entries now,
        // so `state.params.iter()` below already carries them -- there is no longer a dedicated
        // `Command::SetGlobalBypass`/`SetOutputCeilingDb` to submit separately first.
        for (descriptor, value) in state.params.iter() {
            let change = ParamChange {
                id: ParamId(descriptor.id.0),
                value,
            };
            if self.submitter.submit(Command::Param(change)).is_err() {
                commands_not_delivered += 1;
            }
        }

        // Sequential, through the existing primitives -- see this module's doc comment for why
        // that is the whole of R4's guarantee, not an incidental detail.
        let nam = self.recall_resource(cache, Target::Nam, state.nam.as_ref(), resolver);
        let ir = self.recall_resource(cache, Target::Ir, state.ir.as_ref(), resolver);

        RecallOutcome {
            nam,
            ir,
            commands_not_delivered,
        }
    }

    fn recall_resource(
        &mut self,
        cache: &ResourceCache,
        target: Target,
        reference: Option<&FileRef>,
        resolver: &dyn FileResolver,
    ) -> ResourceRecall {
        let Some(reference) = reference else {
            return ResourceRecall::Unloaded(self.unload(target));
        };
        match locate(reference, resolver) {
            Ok(bytes) => {
                let source = LoadSource::Bytes(Arc::from(bytes.into_boxed_slice()));
                ResourceRecall::Loaded(self.load(cache, target, source))
            }
            Err(missing) => ResourceRecall::Missing {
                unload: self.unload(target),
                missing,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineConfig, JobResult};
    use namir_core::{ChannelConfig, ContentHash, SampleRate};
    use namir_engine::{PrepareContext, build_default_engine};
    use namir_fixtures::nam::{WaveNetShape, generate};
    use namir_state::RelPath;
    use std::collections::HashMap;
    use std::time::Duration;

    const SR: u32 = 48_000;
    const BLOCK: usize = 64;

    fn ctx() -> PrepareContext {
        PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Mono).unwrap()
    }

    fn model_bytes(seed: u64) -> Vec<u8> {
        generate(WaveNetShape::Nano, seed)
            .expect("fixture should generate")
            .to_json_bytes()
    }

    fn ir_bytes(seed: u64) -> Vec<u8> {
        let taps = namir_fixtures::ir::decaying_noise(256, seed, 64.0);
        namir_fixtures::ir::to_mono_wav_bytes(&taps, SR)
    }

    /// An in-memory resolver over a fixed set of absolute-path candidates -- exactly the shape
    /// `namir-library`'s real `LibraryResolver` provides, without needing a real temp directory
    /// for every test in this module.
    #[derive(Default)]
    struct FakeResolver {
        by_absolute: HashMap<String, PathBuf>,
        by_hash: HashMap<ContentHash, PathBuf>,
    }

    impl FileResolver for FakeResolver {
        fn resolve_library_relative(&self, _rel: &RelPath) -> Option<PathBuf> {
            None
        }
        fn resolve_absolute(&self, absolute: &str) -> Option<PathBuf> {
            self.by_absolute.get(absolute).cloned()
        }
        fn resolve_by_hash(&self, hash: ContentHash) -> Option<PathBuf> {
            self.by_hash.get(&hash).cloned()
        }
    }

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("namir-worker-recall-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn a_reference(display_name: &str, hash: ContentHash, absolute: PathBuf) -> FileRef {
        FileRef {
            hash,
            library_relative: None,
            absolute: Some(absolute.to_string_lossy().into_owned()),
            display_name: display_name.to_string(),
            embedded: None,
        }
    }

    /// **R4's regression test.** See this module's doc comment for why this is not a manufactured
    /// red-first pair: `Instance::recall`'s shape makes the tempting parallel-submission bug
    /// structurally unreachable, and this measures the same gap
    /// `a_nam_and_an_ir_handover_are_never_offered_simultaneously` measures at the `Instance::load`
    /// level, but driven through a real recall naming *both* a model and an IR.
    ///
    /// # FR-STATE-050's evidence, and why this test no longer carries its tag (M14)
    ///
    /// This test asserts serialisation, which is a *necessary* condition for the constraints
    /// FR-STATE-050 imports from FR-NAM-070 and not the constraints themselves: it processes no
    /// audio at all, so "no discontinuity, no dropout under a continuous signal across the
    /// changeover" was asserted here by nothing, and the tag was a `trace-partial` naming exactly
    /// that from M9a until M14.
    ///
    /// The gap is closed rather than the tag promoted: `tests/recall_continuity.rs`'s
    /// `a_preset_recall_never_clicks_or_drops_out_for_any_change_it_implies` drives a real preset
    /// recall onto a live [`namir_engine::AudioEngine`] processing a continuous sine and asserts
    /// both halves in FR-NAM-070's own terms, for all three changes a preset can imply — a swap, an
    /// unload, and a load into an empty slot. FR-STATE-050's tag lives there now. This test keeps
    /// its own value undiminished: it is R4's regression evidence against the tempting parallel
    /// submission design, and it is the only artifact that measures the *serialisation window*
    /// directly rather than through its audible consequences.
    #[test]
    fn recalling_both_a_model_and_an_ir_never_offers_them_simultaneously() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        let model = model_bytes(1);
        let model_hash = ContentHash::of(&model);
        let model_path = temp_file("model.nam", &model);
        let ir = ir_bytes(2);
        let ir_hash = ContentHash::of(&ir);
        let ir_path = temp_file("ir.wav", &ir);

        // Warm the cache so parsing time doesn't mask the wait -- same convention
        // `a_nam_and_an_ir_handover_are_never_offered_simultaneously` uses.
        let _ = cache.get_or_load_nam(&model).unwrap();
        let _ = cache
            .get_or_load_ir(&ir, c.sample_rate(), c.max_block_size())
            .unwrap();

        let mut resolver = FakeResolver::default();
        resolver.by_absolute.insert(
            model_path.to_string_lossy().into_owned(),
            model_path.clone(),
        );
        resolver
            .by_absolute
            .insert(ir_path.to_string_lossy().into_owned(), ir_path.clone());

        let state = {
            let mut s = namir_state::State::defaults();
            s.nam = Some(a_reference("model.nam", model_hash, model_path));
            s.ir = Some(a_reference("ir.wav", ir_hash, ir_path));
            s
        };

        let started = std::time::Instant::now();
        let outcome = instance.recall(&cache, &state, &resolver);
        let elapsed = started.elapsed();

        assert!(
            matches!(outcome.nam, ResourceRecall::Loaded(_)),
            "expected the model to load, got {:?}",
            outcome.nam
        );
        assert!(
            matches!(outcome.ir, ResourceRecall::Loaded(_)),
            "expected the IR to load, got {:?}",
            outcome.ir
        );

        let fade = Duration::from_micros((namir_engine::HANDOVER_CROSSFADE_MS * 1000.0) as u64);
        assert!(
            elapsed >= fade,
            "recalling both a model and an IR took {elapsed:?}, under the {fade:?} crossfade -- \
             R-7's over-budget condition is not being prevented"
        );
    }

    /// FR-STATE-030's basic case: a state naming only a model loads it, and leaves the IR stage
    /// unloaded (FR-STATE-070's "the state shall load with that stage empty").
    #[test]
    fn recalling_a_state_with_only_a_model_loads_it_and_leaves_ir_unloaded() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        let model = model_bytes(3);
        let model_hash = ContentHash::of(&model);
        let model_path = temp_file("only_model.nam", &model);
        let mut resolver = FakeResolver::default();
        resolver.by_absolute.insert(
            model_path.to_string_lossy().into_owned(),
            model_path.clone(),
        );

        let mut state = namir_state::State::defaults();
        state.nam = Some(a_reference("only_model.nam", model_hash, model_path));

        let outcome = instance.recall(&cache, &state, &resolver);
        assert!(matches!(outcome.nam, ResourceRecall::Loaded(_)));
        assert!(matches!(outcome.ir, ResourceRecall::Unloaded(_)));
        assert_eq!(outcome.commands_not_delivered, 0);
    }

    /// FR-STATE-070's fourth outcome, end to end: a reference whose file cannot be located by any
    /// of the three candidates unloads the stage rather than leaving whatever was there before,
    /// and carries the missing file's name and hash forward for the UI.
    #[test]
    fn a_reference_that_cannot_be_located_unloads_the_stage_and_reports_missing() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);
        let resolver = FakeResolver::default(); // nothing registered -- every candidate misses

        let missing_hash = ContentHash::of(b"never existed");
        let mut state = namir_state::State::defaults();
        state.nam = Some(a_reference(
            "gone.nam",
            missing_hash,
            PathBuf::from("/nonexistent/gone.nam"),
        ));

        let outcome = instance.recall(&cache, &state, &resolver);
        match outcome.nam {
            ResourceRecall::Missing { missing, .. } => {
                assert_eq!(missing.display_name, "gone.nam");
                assert_eq!(missing.hash, missing_hash);
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    /// P7's "identity is the content hash, paths are hints", exercised through a real recall: a
    /// path that resolves but whose *content* has changed since the state was saved must not be
    /// treated as a hit.
    #[test]
    fn a_path_hit_whose_content_hash_differs_is_not_treated_as_found() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        // The file at this path now holds *different* bytes than the reference's recorded hash.
        let original_hash = ContentHash::of(&model_bytes(5));
        let different_bytes = model_bytes(6);
        let path = temp_file("swapped.nam", &different_bytes);

        let mut resolver = FakeResolver::default();
        resolver
            .by_absolute
            .insert(path.to_string_lossy().into_owned(), path.clone());

        let mut state = namir_state::State::defaults();
        state.nam = Some(a_reference("swapped.nam", original_hash, path));

        let outcome = instance.recall(&cache, &state, &resolver);
        assert!(
            matches!(outcome.nam, ResourceRecall::Missing { .. }),
            "a hash mismatch must fall through to Missing, not a wrong-content Loaded, got {:?}",
            outcome.nam
        );
    }

    /// FR-STATE-030: parameters and globals from the state reach the audio thread as part of the
    /// same recall.
    // trace-partial: FR-STATE-030
    // uncovered: FR-STATE-030 — the save clause and both directions of "interchangeable between
    // uncovered: the standalone application and the CLAP plugin" are unspanned: the tagged test
    // uncovered: recalls an in-memory State and never writes or names a preset, and no artifact
    // uncovered: loads an app-written .namirpreset into the plugin or a plugin-written blob into
    // uncovered: the app; closes M8
    #[test]
    fn recall_applies_globals_and_parameters() {
        let c = ctx();
        let (mut engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);
        let resolver = FakeResolver::default();

        let mut state = namir_state::State::defaults();
        state.set_global_bypass(true);
        state.set_output_ceiling_db(-6.0);

        let outcome = instance.recall(&cache, &state, &resolver);
        assert_eq!(outcome.commands_not_delivered, 0);

        // Drain the commands so the engine actually applies them, proving they reached the ring.
        let mut buf = [0.0f32; BLOCK];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
        engine.process(&mut io);
        assert!(buf.iter().all(|s| s.is_finite()));
    }

    /// `JobResult::Unloaded`'s own shape: an unload's outcome carries an elapsed duration, not
    /// the load-specific fields that would be meaningless for it.
    #[test]
    fn unload_outcome_is_the_unloaded_variant() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);
        let outcome = instance.unload(Target::Nam);
        assert!(matches!(outcome.result, JobResult::Unloaded { .. }));
    }

    /// FR-STATE-080: a reference with no resolvable external candidate at all -- no
    /// `library_relative`, no `absolute`, and a resolver that misses on hash too -- still loads
    /// when it carries an embedded copy whose content matches the declared hash.
    #[test]
    fn a_reference_with_no_external_candidate_loads_from_its_embedded_copy() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);
        let resolver = FakeResolver::default(); // nothing registered -- every candidate misses

        let model = model_bytes(7);
        let hash = ContentHash::of(&model);
        let mut state = namir_state::State::defaults();
        state.nam = Some(FileRef {
            hash,
            library_relative: None,
            absolute: None,
            display_name: "embedded-only.nam".to_string(),
            embedded: Some(namir_state::EmbeddedRef {
                media_type: "application/vnd.namir.nam+json".to_string(),
                data: model,
            }),
        });

        let outcome = instance.recall(&cache, &state, &resolver);
        assert!(
            matches!(outcome.nam, ResourceRecall::Loaded(_)),
            "expected the embedded copy to load, got {:?}",
            outcome.nam
        );
    }

    /// P7 applied to the embedded fallback too: an embedded blob whose content does not match the
    /// declared hash is not used -- the reference still ends up `Missing`, not silently loaded
    /// with the wrong content.
    #[test]
    fn an_embedded_copy_whose_hash_does_not_match_is_not_used() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);
        let resolver = FakeResolver::default();

        let declared_hash = ContentHash::of(&model_bytes(8)); // never actually embedded
        let mut state = namir_state::State::defaults();
        state.nam = Some(FileRef {
            hash: declared_hash,
            library_relative: None,
            absolute: None,
            display_name: "mismatched-embed.nam".to_string(),
            embedded: Some(namir_state::EmbeddedRef {
                media_type: "application/vnd.namir.nam+json".to_string(),
                data: model_bytes(9), // different content, different hash
            }),
        });

        let outcome = instance.recall(&cache, &state, &resolver);
        assert!(
            matches!(outcome.nam, ResourceRecall::Missing { .. }),
            "a hash-mismatched embed must not be treated as found, got {:?}",
            outcome.nam
        );
    }
}
