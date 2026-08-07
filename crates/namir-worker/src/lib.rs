//! Off-thread orchestration: load requests, the resource cache, and the worker half of D-8.1's
//! handover (D-5.1).
//!
//! This crate is the "worker" of D-7.1's three thread roles — it may allocate and may block, and it
//! exists so the audio thread never has to do either. Its whole job is the two ends of the
//! handover protocol the audio thread cannot perform:
//!
//! - **D-8.1 step 1, prepare.** Read bytes, hash them, consult [`ResourceCache`], parse if needed,
//!   and build the engine-side slot. "Failure ends here and is reported; the audio thread is never
//!   told."
//! - **D-8.1 step 2, offer.** Push the built resource through the SPSC command ring, behind
//!   D-7.2's producer-side mutex ([`CommandSubmitter`]).
//! - **D-8.1 step 4, retire.** Drain the return ring and **drop the resources here**, which is the
//!   entire point of that ring.
//!
//! Step 3, the crossfade, belongs to `namir-engine` and this crate never sees it.
//!
//! M5 adds two more roles, alongside handover: [`library::LibraryService`] drives D-12.2's
//! "cancellable worker job" on [`pool::ThreadPool`] — the pool this crate's doc comment always
//! said would live here, before `namir-library` existed to need it — and [`recall`] replays a
//! `namir_state::State` onto a live instance (FR-STATE-030/050), the crate that can see both
//! `namir-state` and `namir-engine` at once composing them, exactly as `namir-state`'s own crate
//! doc comment says this crate would.
//!
//! # Deliberately out of scope
//!
//! - **A `LoadSource::File` path resolver** beyond `std::fs::read`. Anything that needs to *know*
//!   where files live is `namir-platform`'s job (D-13.2), and this crate carries no platform code
//!   at all (D-5.1's own column, enforced by `xtask layering`'s cfg scan) — [`library::LibraryService::open`]
//!   takes the index path and every library root as caller-supplied arguments for exactly that
//!   reason (seam 3, `namir-library`'s own crate doc comment).

#![doc(test(attr(deny(warnings))))]

pub mod cache;
pub mod error;
pub mod error_codes;
pub mod library;
pub mod pool;
pub mod recall;
pub mod submit;

use std::sync::Arc;
use std::time::{Duration, Instant};

use namir_core::SampleRate;
use namir_engine::{Command, PrepareContext, Resource, RingConsumer, WorkerEndpoint};

pub use cache::{CacheOutcome, IrKey, ResourceCache};
pub use error::WorkerError;
pub use pool::{ThreadPool, pool_size};
pub use submit::{CommandSubmitter, SubmitError};

/// NFR-SEC-020's byte-count ceiling for a single file — see [`error_codes::FILE_TOO_LARGE`] for
/// why this is separate from NFR-PERF-050's 50 MB performance target.
///
/// Re-exported from `namir_core` (moved there in M5) rather than defined here, so
/// `namir-library` — which reads files off disk directly and may not depend on `namir-worker`
/// (D-5.1) — can share the same bound instead of risking a second, silently drifting copy.
pub use namir_core::MAX_FILE_BYTES;

/// Which stage a load request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The Nam stage.
    Nam,
    /// The Ir stage.
    Ir,
}

impl Target {
    /// Index into [`Instance`]'s per-target arrays.
    fn index(self) -> usize {
        match self {
            Self::Nam => 0,
            Self::Ir => 1,
        }
    }

    /// The other stage — the one R-7's serialisation rule has to wait for.
    fn other(self) -> Self {
        match self {
            Self::Nam => Self::Ir,
            Self::Ir => Self::Nam,
        }
    }

    /// The `namir_engine::ResourceKind` this target names — [`Instance::unload`]'s own
    /// `Command::Unload` needs the engine's vocabulary, not this crate's.
    fn resource_kind(self) -> namir_engine::ResourceKind {
        match self {
            Self::Nam => namir_engine::ResourceKind::Nam,
            Self::Ir => namir_engine::ResourceKind::Ir,
        }
    }
}

/// Multiplier on the crossfade duration for the serialisation wait. Slightly over 1 so the wait
/// covers the fade's own last block plus the block-quantisation slack between the worker's clock
/// and the audio thread's, without materially delaying the second changeover.
const HANDOVER_WINDOW_MARGIN: f64 = 1.25;

/// Where a load's bytes come from.
#[derive(Debug, Clone)]
pub enum LoadSource {
    /// The primitive. Tests and any caller that already holds the bytes use this; no filesystem is
    /// involved, which is also what keeps this crate buildable for NFR-PORT-030's mobile targets
    /// without assuming a filesystem namespace.
    Bytes(Arc<[u8]>),
    /// Read with `std::fs::read` on a worker thread — blocking is permitted here (D-7.1).
    ///
    /// Not platform code: nothing here is conditionally compiled per OS, and the *caller* supplies
    /// the path, so this crate never assumes a filesystem layout (that is `namir-platform`'s job,
    /// D-13.2). Note that `xtask layering`'s D-5.2 scan is textual rather than AST-based, so even
    /// naming the attribute in prose here would trip it -- which is why this comment describes the
    /// property instead of spelling it.
    File(std::path::PathBuf),
}

impl LoadSource {
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

/// How a load request ended.
#[derive(Debug, Clone)]
pub enum JobResult {
    /// The resource was prepared and handed to the audio thread.
    Loaded {
        /// Whether the prepared resource was reused from the cache rather than parsed.
        cache_hit: bool,
        /// How long the whole job took (NFR-PERF-050's figure).
        elapsed: Duration,
        /// A non-fatal condition worth telling the user about — today only D-9.7's IR truncation.
        warning: Option<WorkerError>,
    },
    /// D-8.1 step 1's "failure ends here and is reported; the audio thread is never told."
    Failed(WorkerError),
    /// Prepared successfully, but the audio thread never drained the ring within the deadline, so
    /// the resource was dropped **here**, on the worker.
    NotDelivered(WorkerError),
    /// M5, FR-STATE-070's "the state shall load with that stage empty": the stage was emptied
    /// rather than given a new resource. [`Instance::unload`]'s success case — the `Unload`
    /// analogue of `Loaded`, minus `cache_hit` (nothing was read or parsed) and `warning`
    /// (unloading has no D-9.7-style non-fatal condition to report).
    Unloaded {
        /// How long the whole request took, from the caller's call into [`Instance::unload`] to
        /// the command landing in the ring.
        elapsed: Duration,
    },
}

/// One completed request, as reported back to the UI.
#[derive(Debug, Clone)]
pub struct JobOutcome {
    /// Which stage the request targeted.
    pub target: Target,
    /// What the request was, for the message template's placeholder.
    pub source: String,
    /// How it ended.
    pub result: JobResult,
}

/// Everything the worker needs to know about one engine instance to prepare resources for it.
#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    /// The `PrepareContext` the instance's chain was built with. Slots are built against exactly
    /// this, and the receiving stage checks it rather than trusting it.
    pub ctx: PrepareContext,
}

impl EngineConfig {
    /// The engine sample rate, for the IR cache key.
    pub fn sample_rate(&self) -> SampleRate {
        self.ctx.sample_rate()
    }

    /// The declared maximum block size, for the IR cache key.
    pub fn block_size(&self) -> usize {
        self.ctx.max_block_size()
    }
}

/// One engine instance's worker-side state: the submitter, the return-ring drain, and the config
/// its resources are prepared against.
pub struct Instance {
    config: EngineConfig,
    submitter: CommandSubmitter,
    retire: RingConsumer<Resource>,
    /// When this instance last handed a handover to the audio thread, per target — the state
    /// [`Instance::serialise_against_other_target`] needs. Indexed by [`Target`] via
    /// [`Target::index`].
    last_handover: [Option<Instant>; 2],
    /// How long a stage stays occupied by one handover, plus margin. See
    /// [`Instance::serialise_against_other_target`].
    handover_window: Duration,
}

impl Instance {
    /// Adopts an engine's [`WorkerEndpoint`].
    pub fn new(config: EngineConfig, endpoint: WorkerEndpoint) -> Self {
        Self {
            config,
            submitter: CommandSubmitter::new(endpoint.commands),
            retire: endpoint.retire,
            last_handover: [None, None],
            handover_window: Duration::from_micros(
                (namir_engine::HANDOVER_CROSSFADE_MS * 1000.0 * HANDOVER_WINDOW_MARGIN) as u64,
            ),
        }
    }

    /// **R-7's mitigation: never let a NAM and an IR handover be in flight at the same time.**
    ///
    /// **Decision:** before offering a handover for one target, wait out any handover this instance
    /// recently offered for the *other* target.
    ///
    /// **Rationale:** M4 measured the crossfade for the first time
    /// (`namir-engine/benches/handover_crossfade.rs`, six retained repetitions on the §2 reference
    /// machine under D-2.4's conditions) and found R-7's own wording to be half right with the
    /// wrong half named. A NAM handover alone stays inside NFR-PERF-010's 25% budget at every swap
    /// rate measured — worst 24.31%, at a duty faster than any human audition — and an IR handover
    /// alone likewise (worst 24.63%). What exceeds the budget is **both at once**: 25.06-31.49%,
    /// reproducible to under 0.6 points across six runs. Two stages each running two resources is
    /// the condition that does not fit, and it is the only one.
    ///
    /// Serialising them costs a bounded wait on a worker thread — which D-7.1 explicitly permits
    /// workers to do — and removes the over-budget condition by construction rather than by hoping
    /// users do not do it.
    ///
    /// **Consequence:** a user who changes model *and* IR in one action (loading a preset, once M5
    /// exists) hears the second changeover start after the first finishes, roughly 20 ms later
    /// rather than simultaneously. That is a real behavioural change and it is the intended one:
    /// FR-NAM-070 and FR-IR-060 each require *their own* changeover to be glitch-free, and neither
    /// requires the two to coincide. 20 ms is below the threshold at which a listener hears two
    /// changes as separate events rather than one.
    ///
    /// **Consequence:** this is per-instance state, so two plugin instances can still crossfade
    /// simultaneously. That is correct and deliberately not addressed — NFR-PERF-010's budget is
    /// per-instance ("one instance shall consume no more than 25% of one core"), so two instances
    /// each within budget is two instances within budget.
    ///
    /// **Rejected:** shortening the crossfade toward FR-NAM-070's 5 ms floor. That reduces the
    /// transient's *duty cycle*, not its peak, which is 2x regardless of duration — so it would
    /// make the over-budget blocks rarer without making any of them fit. **Rejected:** having the
    /// audio thread refuse a second offer while one fade is in flight. The audio thread cannot hold
    /// the refused resource without unbounded parking state, and D-7.2 forbids dropping a command,
    /// so the refusal would have to become back-pressure on the command ring — head-of-line
    /// blocking every parameter change behind a 20 ms wait. Better to wait on the worker, which is
    /// allowed to.
    ///
    /// **Rejected:** closing the loop on `telemetry.*.handover_active` instead of a timer. It is
    /// the more precise signal, but it cannot stand alone: the *first* load into an empty stage
    /// retires nothing and, between submission and the audio thread's next block, reports no fade
    /// in flight — so a purely feedback-driven rule races. A timer needs no feedback and cannot
    /// deadlock; if the audio thread stalls, it expires anyway, which is the right failure mode.
    fn serialise_against_other_target(&self, target: Target) {
        let other = self.last_handover[target.other().index()];
        let Some(at) = other else { return };
        let elapsed = at.elapsed();
        if elapsed < self.handover_window {
            std::thread::sleep(self.handover_window - elapsed);
        }
    }

    /// **D-8.1 step 4.** Drains the return ring, dropping every retired resource here, on this
    /// thread. Returns how many were freed.
    ///
    /// "The return ring must be drained reliably. If the worker dies, the ring fills and memory is
    /// retained but audio continues." Call this regularly — and, specifically, *before* submitting
    /// a handover, so the audio side's drain gate always has headroom.
    pub fn drain_retired(&mut self) -> usize {
        let mut freed = 0;
        while let Some(resource) = self.retire.try_pop() {
            drop(resource); // Explicit: this drop is the whole reason the return ring exists.
            freed += 1;
        }
        freed
    }

    /// D-8.1 steps 1, 2 and 4 for one load request, start to finish.
    ///
    /// **Not RT-safe, by design** — it reads a file, parses it, allocates, and may block waiting
    /// for the audio thread to make room.
    ///
    /// The ordering here is load-bearing, not incidental: the cache guard is released inside
    /// `get_or_load_*` before this function ever reaches `submit`, so the cache lock is **never**
    /// held across a blocking submit. See [`CommandSubmitter::submit_with_deadline`] for why that
    /// rule matters.
    pub fn load(
        &mut self,
        cache: &ResourceCache,
        target: Target,
        source: LoadSource,
    ) -> JobOutcome {
        let started = Instant::now();
        let described = source.describe();

        // Step 4 first: make room before asking for any, so the audio side's gate is never the
        // thing that blocks a handover this worker could have unblocked itself.
        self.drain_retired();

        // R-7's mitigation. Deliberately *before* preparation rather than after: preparing takes
        // time that counts toward the other stage's fade anyway, so waiting first would double the
        // delay for no benefit. This runs immediately before the offer instead -- see
        // `prepare_and_offer`.
        let outcome = self.prepare_and_offer(cache, target, &source, &described, started);
        if matches!(outcome, JobResult::Loaded { .. }) {
            self.last_handover[target.index()] = Some(Instant::now());
        }
        JobOutcome {
            target,
            source: described,
            result: outcome,
        }
    }

    /// M5: FR-STATE-070's "the state shall load with that stage empty" — the worker-side entry
    /// point onto `Command::Unload`, mirroring [`Self::load`]'s structure exactly (drain first,
    /// serialise against the other target, submit, record the handover) because an unload is a
    /// handover like any other, subject to R-7's rule the same way (`docs/02-architecture.md`'s
    /// D-8.1 M5 consequence note: "an unload is a handover to nothing"). **This is why
    /// [`crate::recall`] calls this method rather than submitting `Command::Unload` itself** — a
    /// bespoke submit path here would silently skip the serialisation this method provides.
    pub fn unload(&mut self, target: Target) -> JobOutcome {
        let started = Instant::now();
        self.drain_retired();
        self.serialise_against_other_target(target);
        let result = match self
            .submitter
            .submit(Command::Unload(target.resource_kind()))
        {
            Ok(()) => {
                self.last_handover[target.index()] = Some(Instant::now());
                JobResult::Unloaded {
                    elapsed: started.elapsed(),
                }
            }
            Err(SubmitError::Timeout(_)) | Err(SubmitError::Abandoned(_)) => {
                JobResult::NotDelivered(WorkerError::new(
                    error_codes::NOT_DELIVERED,
                    "(no reference)".to_string(),
                ))
            }
        };
        JobOutcome {
            target,
            source: "(no reference)".to_string(),
            result,
        }
    }

    fn prepare_and_offer(
        &mut self,
        cache: &ResourceCache,
        target: Target,
        source: &LoadSource,
        described: &str,
        started: Instant,
    ) -> JobResult {
        let bytes = match source.read() {
            Ok(b) => b,
            Err(e) => return JobResult::Failed(e),
        };

        let (command, cache_hit, warning) = match target {
            Target::Nam => match cache.get_or_load_nam(&bytes) {
                Ok((model, outcome)) => (
                    Command::load_nam(model, &self.config.ctx),
                    outcome.hit,
                    None,
                ),
                Err(e) => return JobResult::Failed(e),
            },
            Target::Ir => match cache.get_or_load_ir(
                &bytes,
                self.config.sample_rate(),
                self.config.block_size(),
            ) {
                Ok((ir, outcome)) => {
                    let warning = outcome.truncated.then(|| {
                        WorkerError::new(error_codes::IR_TRUNCATED, described.to_string())
                    });
                    (Command::load_ir(ir, &self.config.ctx), outcome.hit, warning)
                }
                Err(e) => return JobResult::Failed(e),
            },
        };

        // R-7's serialisation, applied here rather than at the top of `load`: preparation has
        // already consumed real time, and whatever it consumed counts toward the other stage's
        // fade. Waiting here charges only the remainder.
        self.serialise_against_other_target(target);

        match self.submitter.submit(command) {
            Ok(()) => JobResult::Loaded {
                cache_hit,
                elapsed: started.elapsed(),
                warning,
            },
            // The command comes back on both error paths and is dropped here, on this thread,
            // which is legal — never on the audio thread.
            Err(SubmitError::Timeout(_)) | Err(SubmitError::Abandoned(_)) => {
                JobResult::NotDelivered(WorkerError::new(
                    error_codes::NOT_DELIVERED,
                    described.to_string(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::ChannelConfig;
    use namir_engine::build_default_engine;
    use namir_fixtures::nam::{WaveNetShape, generate};

    const SR: u32 = 48_000;
    const BLOCK: usize = 64;

    fn ctx() -> PrepareContext {
        PrepareContext::new(SampleRate::new(SR).unwrap(), BLOCK, ChannelConfig::Mono).unwrap()
    }

    fn ir_bytes(seed: u64) -> Arc<[u8]> {
        let taps = namir_fixtures::ir::decaying_noise(256, seed, 64.0);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SR,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut buf = Vec::new();
        {
            let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut buf), spec).unwrap();
            for &t in &taps {
                w.write_sample(t).unwrap();
            }
            w.finalize().unwrap();
        }
        Arc::from(buf.into_boxed_slice())
    }

    fn model_bytes(seed: u64) -> Arc<[u8]> {
        Arc::from(
            generate(WaveNetShape::Nano, seed)
                .expect("fixture should generate")
                .to_json_bytes()
                .into_boxed_slice(),
        )
    }

    /// D-8.2's hard constraint, checked at compile time from the crate that actually depends on it.
    #[test]
    fn prepared_resources_are_send_and_sync() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<namir_nam::PreparedNam>();
        assert_send_sync::<namir_ir::PreparedIr>();
        assert_send_sync::<ResourceCache>();
    }

    /// **D-8.1 steps 1 and 2, end to end.** The worker prepares and offers; the command lands in
    /// the ring for the audio thread to pick up.
    #[test]
    fn a_load_prepares_and_offers_the_resource() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        let outcome = instance.load(&cache, Target::Nam, LoadSource::Bytes(model_bytes(1)));
        match outcome.result {
            JobResult::Loaded { cache_hit, .. } => {
                assert!(!cache_hit, "first load cannot be a hit")
            }
            other => panic!("expected a successful load, got {other:?}"),
        }
    }

    /// **D-8.1 step 1: "Failure ends here and is reported; the audio thread is never told."**
    /// Nothing may reach the command ring when preparation fails.
    #[test]
    fn a_load_failure_never_reaches_the_audio_thread() {
        let c = ctx();
        let (mut engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        let outcome = instance.load(
            &cache,
            Target::Nam,
            LoadSource::Bytes(Arc::from(b"not a nam file".to_vec().into_boxed_slice())),
        );
        match outcome.result {
            JobResult::Failed(e) => assert!(
                e.code.id.starts_with("nam.load."),
                "the specific parse reason must survive, got {}",
                e.code.id
            ),
            other => panic!("expected a reported failure, got {other:?}"),
        }

        // The audio side sees nothing at all: a block runs, and no handover happens.
        let mut buf = [0.0f32; BLOCK];
        let mut channels: [&mut [f32]; 1] = [&mut buf];
        let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
        engine.process(&mut io);
        assert!(buf.iter().all(|s| s.is_finite()));
    }

    /// **D-8.1 step 4:** the retired resource is dropped on the worker's thread, not the audio
    /// thread. The `Weak` is the proof — it upgrades while the resource is still in flight and
    /// stops only once this crate's drain has run.
    #[test]
    fn the_retired_resource_is_freed_by_the_worker_not_the_audio_thread() {
        let c = ctx();
        let (mut engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        // First model, settled.
        instance.load(&cache, Target::Nam, LoadSource::Bytes(model_bytes(1)));
        let first = cache
            .get_or_load_nam(&model_bytes(1))
            .expect("already cached")
            .0;
        let weak = Arc::downgrade(&first);
        drop(first);

        let run = |engine: &mut namir_engine::AudioEngine, blocks: usize| {
            for _ in 0..blocks {
                let mut buf = [0.05f32; BLOCK];
                let mut channels: [&mut [f32]; 1] = [&mut buf];
                let mut io = namir_engine::StageIo::new(&mut channels, BLOCK);
                engine.process(&mut io);
            }
        };
        run(&mut engine, 60);

        // Second model: the first retires across the crossfade.
        instance.load(&cache, Target::Nam, LoadSource::Bytes(model_bytes(2)));
        run(&mut engine, 60);

        assert!(
            weak.upgrade().is_some(),
            "the retired model must still be alive -- the audio thread must not have freed it"
        );
        let freed = instance.drain_retired();
        assert!(
            freed > 0,
            "the return ring should have carried a retirement"
        );
        cache.reap();
        assert!(
            weak.upgrade().is_none(),
            "draining the return ring on the worker is what frees it (D-8.1 step 4)"
        );
    }

    /// FR-CLAP-090's mechanism, from the worker's own API: two instances loading the same file get
    /// the same weights rather than a copy each.
    #[test]
    fn two_instances_loading_one_file_share_its_weights() {
        let c = ctx();
        let cache = Arc::new(ResourceCache::new());
        let bytes = model_bytes(9);

        let (_e1, ep1) = build_default_engine(&c).unwrap();
        let (_e2, ep2) = build_default_engine(&c).unwrap();
        let mut i1 = Instance::new(EngineConfig { ctx: c }, ep1);
        let mut i2 = Instance::new(EngineConfig { ctx: c }, ep2);

        i1.load(&cache, Target::Nam, LoadSource::Bytes(Arc::clone(&bytes)));
        i2.load(&cache, Target::Nam, LoadSource::Bytes(Arc::clone(&bytes)));

        assert_eq!(
            cache.nam_entries(),
            1,
            "both instances should have resolved to one cache entry"
        );
    }

    /// **R-7's mitigation: a NAM and an IR handover are never offered simultaneously.**
    ///
    /// Measures the wall-clock gap between the two offers landing in the command ring. It must be
    /// at least the crossfade duration, because that is the whole point: M4 measured
    /// 25.06-31.49% of the block period when the two overlap, against a 25% budget, versus at
    /// worst 24.63% for either alone.
    #[test]
    fn a_nam_and_an_ir_handover_are_never_offered_simultaneously() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        // Warm the cache so neither load is dominated by parse time, which would mask the wait.
        let model = model_bytes(41);
        let ir = ir_bytes(42);
        let _ = cache.get_or_load_nam(&model).unwrap();
        let _ = cache
            .get_or_load_ir(&ir, c.sample_rate(), c.max_block_size())
            .unwrap();

        instance.load(&cache, Target::Nam, LoadSource::Bytes(Arc::clone(&model)));
        let after_nam = Instant::now();
        instance.load(&cache, Target::Ir, LoadSource::Bytes(Arc::clone(&ir)));
        let gap = after_nam.elapsed();

        let fade = Duration::from_micros((namir_engine::HANDOVER_CROSSFADE_MS * 1000.0) as u64);
        assert!(
            gap >= fade,
            "the IR offer landed {gap:?} after the NAM offer, inside the {fade:?} crossfade --              R-7's over-budget condition is not being prevented"
        );
    }

    /// The rule must not slow down the common case. Two successive loads of the *same* target are
    /// one stage crossfading twice, which stays inside budget, so nothing should wait.
    #[test]
    fn two_handovers_for_the_same_target_are_not_serialised() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        let a = model_bytes(43);
        let b = model_bytes(44);
        let _ = cache.get_or_load_nam(&a).unwrap();
        let _ = cache.get_or_load_nam(&b).unwrap();

        instance.load(&cache, Target::Nam, LoadSource::Bytes(Arc::clone(&a)));
        let started = Instant::now();
        instance.load(&cache, Target::Nam, LoadSource::Bytes(Arc::clone(&b)));
        let fade = Duration::from_micros((namir_engine::HANDOVER_CROSSFADE_MS * 1000.0) as u64);
        assert!(
            started.elapsed() < fade,
            "a same-target reload waited {:?}; only cross-target handovers need serialising",
            started.elapsed()
        );
    }

    /// A *failed* load must not arm the serialisation timer -- nothing was offered, so there is no
    /// fade for the next handover to wait out.
    #[test]
    fn a_failed_load_does_not_delay_the_other_target() {
        let c = ctx();
        let (_engine, endpoint) = build_default_engine(&c).unwrap();
        let cache = ResourceCache::new();
        let mut instance = Instance::new(EngineConfig { ctx: c }, endpoint);

        let ir = ir_bytes(45);
        let _ = cache
            .get_or_load_ir(&ir, c.sample_rate(), c.max_block_size())
            .unwrap();

        instance.load(
            &cache,
            Target::Nam,
            LoadSource::Bytes(Arc::from(b"not a nam file".to_vec().into_boxed_slice())),
        );
        let started = Instant::now();
        instance.load(&cache, Target::Ir, LoadSource::Bytes(Arc::clone(&ir)));
        let fade = Duration::from_micros((namir_engine::HANDOVER_CROSSFADE_MS * 1000.0) as u64);
        assert!(
            started.elapsed() < fade,
            "a failed NAM load delayed the IR handover by {:?}",
            started.elapsed()
        );
    }

    /// NFR-SEC-020: the worker is the first component that reads a file, so it owns the byte-count
    /// ceiling. A missing file is reported through the catalogue rather than panicking.
    #[test]
    fn an_unreadable_file_is_reported_through_the_catalogue() {
        let source = LoadSource::File(std::path::PathBuf::from("no-such-file-here.nam"));
        let err = source.read().unwrap_err();
        assert_eq!(err.code.id, error_codes::FILE_UNREADABLE.id);
    }
}
