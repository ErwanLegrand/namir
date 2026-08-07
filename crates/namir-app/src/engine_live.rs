//! Live command submission and resource load/unload/recall orchestration — [`LiveEngine`] is what
//! this crate drives instead of `namir_worker::Instance`.
//!
//! # Why not `namir_worker::Instance`, stated plainly
//!
//! `Instance::new(config, endpoint)` takes ownership of the *entire* `WorkerEndpoint`, including
//! its one and only `RingProducer<Command>`, and wraps it in a **private**
//! `namir_worker::CommandSubmitter` field. `Instance`'s public surface is exactly four methods:
//! `new`, `drain_retired`, `load`, `unload` (plus `recall`, from `recall.rs`). None of them submits
//! an arbitrary `Command::Param` or `Command::Reset` — there is no `Instance::submit`,
//! `Instance::try_submit`, or an accessor onto its internal submitter.
//!
//! That is a real gap for a product shell, not a hypothetical one: `submit.rs`'s own module doc
//! comment describes `CommandSubmitter`'s producer-side mutex as existing precisely so "several
//! worker threads and **the UI**" can share one producer, and `CommandSubmitter::try_submit`'s own
//! doc comment says outright "this is what the UI thread uses". But there is only one
//! `RingProducer<Command>` per engine instance (SPSC, not `Clone`), and `Instance::new` consumes
//! it whole — so once an engine is wired to a real `Instance`, nothing outside `namir-worker` can
//! reach the ring to submit an ordinary parameter change at all, blocking or not. Every
//! per-knob-turn `UiIntent::SetParam`, every `ResetParamToDefault`, and FR-CHAIN's transport reset
//! all need exactly this and have no way to get it through `Instance`.
//!
//! This is flagged in this crate's final report rather than fixed in `namir-worker` itself — see
//! `crates/namir-app/src/lib.rs`'s module doc comment for why (a second agent is building
//! `namir-clap` against the same `namir-worker` API concurrently; an unreviewed structural change
//! risks contradicting whatever that agent needs). The substitute built here constructs its own
//! `Arc<CommandSubmitter>` directly from `namir_engine::split`'s `WorkerEndpoint`, shares it
//! between the UI thread ([`LiveEngine::submitter`], for `try_submit`-style plain parameter
//! changes) and this module's own load/unload/recall orchestration (for the blocking `submit`
//! calls a handover needs) — the one thing `Instance` cannot do because it never lets that
//! producer out of its own private field.
//!
//! # What is and is not duplicated from `namir-worker`
//!
//! **Reused directly, unchanged:** `namir_worker::ResourceCache` (parsing, hashing, D-8.2's
//! sharing), `namir_worker::CommandSubmitter`/`SubmitError` (D-7.2's retry/backoff/deadline
//! semantics), `namir_worker::WorkerError` and its `error_codes` (so a user sees the identical
//! catalogue id/message a real `Instance` would have produced), `namir_engine::Command::load_nam`/
//! `load_ir` (D-8.1 step 1's slot construction), and `namir_state::candidates`/`FileResolver`
//! (FR-STATE-070's resolution order).
//!
//! **Re-derived here, small and self-contained:** R-7's cross-target serialisation window
//! ([`LiveEngine::serialise_against_other_target`] — the same `Instant`-based wait
//! `namir_worker::Instance` documents, a handful of lines), a file-size-checked byte read
//! ([`LoadSource::read`] — `namir_worker::LoadSource::read` exists and does the identical thing
//! but is a private method, not part of that type's public API), and the content-hash-verifying
//! locate step ([`locate`] — `namir_worker::recall::locate` is the same algorithm, also private).
//! None of these touch a `Command` ring directly or reimplement D-7.2/D-8.1's own delivery
//! guarantees; they reuse [`namir_worker::CommandSubmitter`] for every actual submission.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use namir_core::MAX_FILE_BYTES;
use namir_engine::{
    Command, HANDOVER_CROSSFADE_MS, PrepareContext, ResourceKind, RingConsumer, WorkerEndpoint,
};
use namir_params::ParamId;
use namir_state::{Candidate, FileRef, FileResolver, MissingFile, State};
use namir_worker::{
    CacheOutcome, CommandSubmitter, ResourceCache, SubmitError, WorkerError, error_codes,
};

/// Multiplier on the crossfade duration for the serialisation wait — identical to
/// `namir_worker::Instance`'s own `HANDOVER_WINDOW_MARGIN` and identical reasoning (this module's
/// own doc comment covers why the value itself is not re-derived independently: it exists so the
/// wait covers the fade's last block plus block-quantisation slack).
const HANDOVER_WINDOW_MARGIN: f64 = 1.25;

/// Which resource stage a load/unload targets. A local type rather than `namir_worker::Target`:
/// that type's variants are public but its `index`/`other`/`resource_kind` helper methods are not,
/// so nothing outside `namir-worker` can actually use it as more than an opaque tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The Nam stage.
    Nam,
    /// The Ir stage.
    Ir,
}

impl Target {
    fn index(self) -> usize {
        match self {
            Self::Nam => 0,
            Self::Ir => 1,
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Nam => Self::Ir,
            Self::Ir => Self::Nam,
        }
    }

    fn resource_kind(self) -> ResourceKind {
        match self {
            Self::Nam => ResourceKind::Nam,
            Self::Ir => ResourceKind::Ir,
        }
    }
}

/// Where a load's bytes come from — the same two cases `namir_worker::LoadSource` offers, redrawn
/// here because that type's own `read`/`describe` methods are private (see this module's doc
/// comment).
#[derive(Debug, Clone)]
pub enum LoadSource {
    /// Bytes already in memory — no filesystem access.
    Bytes(Arc<[u8]>),
    /// Read from disk on this (worker) thread. Blocking is fine here, matching
    /// `namir_worker::LoadSource::File`'s own contract.
    File(PathBuf),
}

impl LoadSource {
    /// NFR-SEC-020's byte-count ceiling, checked before the file is read into memory — the same
    /// rule `namir_worker::LoadSource::read` applies, reusing the identical
    /// `namir_worker::error_codes` catalogue entries so the reported id/message a user sees is
    /// unchanged by which code path produced it.
    fn read(&self) -> Result<Arc<[u8]>, WorkerError> {
        match self {
            Self::Bytes(bytes) => Ok(Arc::clone(bytes)),
            Self::File(path) => {
                let display = path.display().to_string();
                let meta = std::fs::metadata(path).map_err(|e| {
                    WorkerError::new(error_codes::FILE_UNREADABLE, format!("{display}: {e}"))
                })?;
                if meta.len() as usize > MAX_FILE_BYTES {
                    return Err(WorkerError::new(
                        error_codes::FILE_TOO_LARGE,
                        format!("{display}: {} bytes", meta.len()),
                    ));
                }
                let bytes = std::fs::read(path).map_err(|e| {
                    WorkerError::new(error_codes::FILE_UNREADABLE, format!("{display}: {e}"))
                })?;
                Ok(Arc::from(bytes.into_boxed_slice()))
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Bytes(b) => format!("<{} bytes>", b.len()),
            Self::File(p) => p.display().to_string(),
        }
    }
}

/// How one load/unload ended — the same shape `namir_worker::JobResult` uses (this module's doc
/// comment explains why that type itself isn't reused: it's produced only by `Instance`'s own
/// methods).
#[derive(Debug, Clone)]
pub enum LoadOutcome {
    /// Prepared and handed to the audio thread.
    Loaded {
        /// Whether the prepared resource was reused from `ResourceCache` rather than parsed.
        cache_hit: bool,
        /// How long the whole request took.
        elapsed: Duration,
        /// A non-fatal condition worth surfacing (D-9.7's IR truncation).
        warning: Option<WorkerError>,
    },
    /// Preparation failed; nothing reached the audio thread.
    Failed(WorkerError),
    /// Prepared, but the audio thread never drained the ring within the deadline.
    NotDelivered(WorkerError),
    /// FR-STATE-070's "the state shall load with that stage empty".
    Unloaded {
        /// How long the request took.
        elapsed: Duration,
    },
}

/// One resource slot's outcome within a [`RecallOutcome`] — mirrors
/// `namir_worker::recall::ResourceRecall`.
#[derive(Debug, Clone)]
pub enum ResourceRecall {
    /// No reference was named for this slot.
    Unloaded(LoadOutcome),
    /// A reference was named and located.
    Loaded(LoadOutcome),
    /// A reference was named but none of FR-STATE-070's candidates located matching content.
    Missing {
        /// The unload that emptied the stage in place of the missing reference.
        unload: LoadOutcome,
        /// What was missing.
        missing: MissingFile,
    },
}

/// How one [`LiveEngine::recall`] call ended — mirrors `namir_worker::recall::RecallOutcome`.
#[derive(Debug, Clone)]
pub struct RecallOutcome {
    /// What happened to the Nam stage.
    pub nam: ResourceRecall,
    /// What happened to the Ir stage.
    pub ir: ResourceRecall,
    /// How many parameter/global commands did not reach the audio thread within the submit
    /// deadline.
    pub commands_not_delivered: usize,
}

/// FR-STATE-070's read-hash-compare loop, redrawn from `namir_worker::recall::locate` (private in
/// that crate — see this module's doc comment). P7: "identity of a model or IR is its content
/// hash. Paths are hints" — a path candidate that exists but whose *content* no longer matches is
/// not a hit, and only reading the bytes can tell the two apart, which is why this is not simply
/// `namir_state::resolve` (existence-only, by that module's own design).
fn locate(reference: &FileRef, resolver: &dyn FileResolver) -> Result<Vec<u8>, MissingFile> {
    for candidate in namir_state::candidates(reference) {
        let path: Option<PathBuf> = match candidate {
            Candidate::LibraryRelative(rel) => resolver.resolve_library_relative(rel),
            Candidate::Absolute(abs) => resolver.resolve_absolute(abs),
            Candidate::ContentHash(hash) => resolver.resolve_by_hash(hash),
        };
        let Some(path) = path else { continue };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if namir_core::ContentHash::of(&bytes) == reference.hash {
            return Ok(bytes);
        }
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

/// This crate's live-engine handle: owns the return-ring drain and the R-7 serialisation clock,
/// and shares its command submitter with the UI thread. See this module's doc comment for the
/// full rationale.
pub struct LiveEngine {
    submitter: Arc<CommandSubmitter>,
    retire: RingConsumer<namir_engine::Resource>,
    cache: Arc<ResourceCache>,
    ctx: PrepareContext,
    last_handover: [Option<Instant>; 2],
}

impl LiveEngine {
    /// Adopts a freshly split engine's [`WorkerEndpoint`]. Returns the live engine (for the
    /// worker thread that will drive load/unload/recall) alongside a second, independent
    /// [`Arc<CommandSubmitter>`] clone the UI thread uses directly for ordinary parameter
    /// changes, and the [`namir_engine::TelemetryReader`] for meters (cloned *before* the
    /// producer half of `endpoint` is consumed, since `TelemetryReader` is `Clone` and nothing
    /// else about `WorkerEndpoint` is).
    pub fn new(
        ctx: PrepareContext,
        endpoint: WorkerEndpoint,
        cache: Arc<ResourceCache>,
    ) -> (Self, Arc<CommandSubmitter>, namir_engine::TelemetryReader) {
        let telemetry = endpoint.telemetry.clone();
        let submitter = Arc::new(CommandSubmitter::new(endpoint.commands));
        let live = Self {
            submitter: Arc::clone(&submitter),
            retire: endpoint.retire,
            cache,
            ctx,
            last_handover: [None, None],
        };
        (live, submitter, telemetry)
    }

    /// A second handle onto the same command submitter this instance uses for load/unload —
    /// the UI thread's own path for `Command::Param`/`Command::Reset` (see this module's doc
    /// comment for why this has to be a shared `Arc` rather than something `LiveEngine` hides).
    pub fn submitter(&self) -> Arc<CommandSubmitter> {
        Arc::clone(&self.submitter)
    }

    /// D-8.1 step 4: drains the return ring, dropping every retired resource on this thread.
    pub fn drain_retired(&mut self) -> usize {
        let mut freed = 0;
        while let Some(resource) = self.retire.try_pop() {
            drop(resource);
            freed += 1;
        }
        freed
    }

    /// R-7's mitigation, identical in shape to `namir_worker::Instance::serialise_against_other_target`
    /// — see that method's own extensive doc comment (`crates/namir-worker/src/lib.rs`) for the
    /// full measured rationale; reproduced here only because `Instance` does not expose it for
    /// reuse.
    fn serialise_against_other_target(&self, target: Target) {
        let Some(at) = self.last_handover[target.other().index()] else {
            return;
        };
        let window =
            Duration::from_micros((HANDOVER_CROSSFADE_MS * 1000.0 * HANDOVER_WINDOW_MARGIN) as u64);
        let elapsed = at.elapsed();
        if elapsed < window {
            std::thread::sleep(window - elapsed);
        }
    }

    /// D-8.1 steps 1, 2 and 4 for one load request — mirrors `Instance::load`'s ordering exactly
    /// (drain first, prepare, serialise immediately before the offer, submit).
    pub fn load(&mut self, target: Target, source: LoadSource) -> LoadOutcome {
        let started = Instant::now();
        self.drain_retired();

        let bytes = match source.read() {
            Ok(b) => b,
            Err(e) => return LoadOutcome::Failed(e),
        };

        let (command, cache_hit, warning) = match target {
            Target::Nam => match self.cache.get_or_load_nam(&bytes) {
                Ok((model, outcome)) => (Command::load_nam(model, &self.ctx), outcome.hit, None),
                Err(e) => return LoadOutcome::Failed(e),
            },
            Target::Ir => match self.cache.get_or_load_ir(
                &bytes,
                self.ctx.sample_rate(),
                self.ctx.max_block_size(),
            ) {
                Ok((ir, outcome)) => {
                    let warning = warning_for(outcome, &source);
                    (Command::load_ir(ir, &self.ctx), outcome.hit, warning)
                }
                Err(e) => return LoadOutcome::Failed(e),
            },
        };

        self.serialise_against_other_target(target);

        match self.submitter.submit(command) {
            Ok(()) => {
                self.last_handover[target.index()] = Some(Instant::now());
                LoadOutcome::Loaded {
                    cache_hit,
                    elapsed: started.elapsed(),
                    warning,
                }
            }
            Err(SubmitError::Timeout(_)) | Err(SubmitError::Abandoned(_)) => {
                LoadOutcome::NotDelivered(WorkerError::new(
                    error_codes::NOT_DELIVERED,
                    source.describe(),
                ))
            }
        }
    }

    /// FR-STATE-070's "the state shall load with that stage empty" — mirrors `Instance::unload`.
    pub fn unload(&mut self, target: Target) -> LoadOutcome {
        let started = Instant::now();
        self.drain_retired();
        self.serialise_against_other_target(target);
        match self
            .submitter
            .submit(Command::Unload(target.resource_kind()))
        {
            Ok(()) => {
                self.last_handover[target.index()] = Some(Instant::now());
                LoadOutcome::Unloaded {
                    elapsed: started.elapsed(),
                }
            }
            Err(SubmitError::Timeout(_)) | Err(SubmitError::Abandoned(_)) => {
                LoadOutcome::NotDelivered(WorkerError::new(
                    error_codes::NOT_DELIVERED,
                    "(no reference)",
                ))
            }
        }
    }

    /// FR-STATE-030/050: mirrors `Instance::recall`'s ordering (every parameter first, then each
    /// resource through [`Self::load`]/[`Self::unload`] — never a bespoke submit, for the same R4
    /// reason `namir_worker::recall`'s own module doc comment gives).
    pub fn recall(&mut self, state: &State, resolver: &dyn FileResolver) -> RecallOutcome {
        let mut commands_not_delivered = 0usize;
        for (descriptor, value) in state.params.iter() {
            let change = namir_engine::ParamChange {
                id: namir_engine::ParamId(descriptor.id.0),
                value,
            };
            if self.submitter.submit(Command::Param(change)).is_err() {
                commands_not_delivered += 1;
            }
        }

        let nam = self.recall_resource(Target::Nam, state.nam.as_ref(), resolver);
        let ir = self.recall_resource(Target::Ir, state.ir.as_ref(), resolver);

        RecallOutcome {
            nam,
            ir,
            commands_not_delivered,
        }
    }

    fn recall_resource(
        &mut self,
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
                ResourceRecall::Loaded(self.load(target, source))
            }
            Err(missing) => ResourceRecall::Missing {
                unload: self.unload(target),
                missing,
            },
        }
    }

    /// Submits a single ordinary parameter change directly through the shared submitter, without
    /// waiting for it to land (`try_submit`, D-7.2's non-blocking UI path). Provided here mainly
    /// so tests and callers that already hold a `LiveEngine` don't have to clone
    /// [`Self::submitter`] just to move a single knob.
    pub fn try_submit_param(&self, id: ParamId, value: f32) -> Result<(), SubmitError> {
        self.submitter
            .try_submit(Command::Param(namir_engine::ParamChange {
                id: namir_engine::ParamId(id.0),
                value,
            }))
    }
}

fn warning_for(outcome: CacheOutcome, source: &LoadSource) -> Option<WorkerError> {
    outcome
        .truncated
        .then(|| WorkerError::new(error_codes::IR_TRUNCATED, source.describe()))
}

/// A `Mutex`-guarded [`LiveEngine`] for the shape [`crate::worker`] actually needs: one background
/// thread owns it exclusively for load/unload/recall, while the constructing code keeps the
/// [`Arc<CommandSubmitter>`] clone for the UI thread's own direct use. Kept as a thin type alias
/// rather than baking `Mutex` into `LiveEngine` itself, so this module's own tests can construct a
/// bare `LiveEngine` with no locking overhead.
pub type SharedLiveEngine = Arc<Mutex<LiveEngine>>;

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::{ChannelConfig, SampleRate};
    use namir_engine::{RingCapacities, split};
    use namir_fixtures::nam::{WaveNetShape, generate};

    const SR: u32 = 48_000;
    const BLOCK: usize = 64;

    fn ctx() -> PrepareContext {
        PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Mono).unwrap()
    }

    fn model_bytes(seed: u64) -> Arc<[u8]> {
        Arc::from(
            generate(WaveNetShape::Nano, seed)
                .expect("fixture should generate")
                .to_json_bytes()
                .into_boxed_slice(),
        )
    }

    fn build() -> (namir_engine::AudioEngine, LiveEngine, Arc<CommandSubmitter>) {
        let c = ctx();
        let chain = namir_engine::build_default_chain(&c).unwrap();
        let (engine, endpoint) = split(chain, RingCapacities::default());
        let (live, submitter, _telemetry) =
            LiveEngine::new(c, endpoint, Arc::new(ResourceCache::new()));
        (engine, live, submitter)
    }

    /// D-8.1 steps 1/2, end to end: a load prepares and offers the resource, exactly as
    /// `Instance::load` does.
    #[test]
    fn a_load_prepares_and_offers_the_resource() {
        let (_engine, mut live, _submitter) = build();
        let outcome = live.load(Target::Nam, LoadSource::Bytes(model_bytes(1)));
        match outcome {
            LoadOutcome::Loaded { cache_hit, .. } => assert!(!cache_hit),
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    /// Parse failure never reaches the audio thread -- the engine still runs cleanly afterward.
    #[test]
    fn a_load_failure_never_reaches_the_audio_thread() {
        let (mut engine, mut live, _submitter) = build();
        let outcome = live.load(
            Target::Nam,
            LoadSource::Bytes(Arc::from(b"not a nam file".to_vec().into_boxed_slice())),
        );
        assert!(matches!(outcome, LoadOutcome::Failed(_)));

        let mut buf = [0.0f32; BLOCK];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
        engine.process(&mut io);
        assert!(buf.iter().all(|s| s.is_finite()));
    }

    /// **The whole reason this module exists:** an ordinary parameter change submitted through
    /// the *shared* submitter (the UI thread's own path) reaches the audio thread, at the same
    /// time as this module's own load/unload orchestration is available on the same engine.
    #[test]
    fn a_plain_param_change_reaches_the_audio_thread_via_the_shared_submitter() {
        let (mut engine, live, submitter) = build();
        let gain_id = namir_params::stages::trim::GAIN_DB.id;
        submitter
            .try_submit(Command::Param(namir_engine::ParamChange {
                id: namir_engine::ParamId(gain_id.0),
                value: -60.0,
            }))
            .expect("the ring should have room");
        drop(live); // the UI-thread path does not need LiveEngine itself, only the submitter.

        // The gain ramp needs several blocks (its own ~25 ms time constant) to settle -- run
        // enough of them, matching the settling convention `namir-engine`'s own tests use. A
        // fresh `buf`/`channels` each iteration, not hoisted out of the loop: `StageIo<'a>`
        // unifies its outer and inner reference lifetimes, so reusing one `channels` array across
        // iterations would force the borrow checker to treat every iteration's reborrow as living
        // as long as the whole loop.
        let mut buf = [0.0f32; BLOCK];
        for _ in 0..100 {
            buf.fill(0.5);
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
            engine.process(&mut io);
        }
        // -60 dB is far enough down that the ramp's settled tail must be near silence.
        assert!(buf.last().unwrap().abs() < 0.05);
    }

    /// FR-STATE-070's "the state shall load with that stage empty": an unloaded stage's retired
    /// resource still arrives on the return ring.
    #[test]
    fn unload_retires_the_slot() {
        let (mut engine, mut live, _submitter) = build();
        live.load(Target::Nam, LoadSource::Bytes(model_bytes(1)));
        // Settle the load across a few blocks before unloading.
        for _ in 0..40 {
            let mut buf = [0.05f32; BLOCK];
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
            engine.process(&mut io);
        }
        let outcome = live.unload(Target::Nam);
        assert!(matches!(outcome, LoadOutcome::Unloaded { .. }));
        for _ in 0..40 {
            let mut buf = [0.05f32; BLOCK];
            let mut channels: [&mut [f32]; 1] = [&mut buf];
            let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
            engine.process(&mut io);
        }
        assert!(live.drain_retired() > 0);
    }

    /// A missing file is reported through the same catalogue `namir_worker::LoadSource` would
    /// use.
    #[test]
    fn an_unreadable_file_is_reported_through_the_catalogue() {
        let source = LoadSource::File(PathBuf::from("no-such-file-for-this-test.nam"));
        let err = source.read().unwrap_err();
        assert_eq!(err.code.id, error_codes::FILE_UNREADABLE.id);
    }
}
