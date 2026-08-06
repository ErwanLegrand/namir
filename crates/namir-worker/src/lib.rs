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
//! # Deliberately out of scope
//!
//! - **Library scanning** (D-12.2's cancellable scan job) — needs `namir-library`, which is M5.
//!   D-5.1 already lists it among this crate's responsibilities; the pool it will run on is here.
//! - **Preset recall** — needs `namir-state`, also M5.
//! - **A `LoadSource::File` path resolver** beyond `std::fs::read`. Anything that needs to *know*
//!   where files live is `namir-platform`'s job (D-13.2), and this crate carries no platform code
//!   at all (D-5.1's own column, enforced by `xtask layering`'s cfg scan).

#![doc(test(attr(deny(warnings))))]

pub mod cache;
pub mod error;
pub mod error_codes;
pub mod pool;
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
pub const MAX_FILE_BYTES: usize = 256 * 1024 * 1024;

/// Which stage a load request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The Nam stage.
    Nam,
    /// The Ir stage.
    Ir,
}

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
}

impl Instance {
    /// Adopts an engine's [`WorkerEndpoint`].
    pub fn new(config: EngineConfig, endpoint: WorkerEndpoint) -> Self {
        Self {
            config,
            submitter: CommandSubmitter::new(endpoint.commands),
            retire: endpoint.retire,
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

        let outcome = self.prepare_and_offer(cache, target, &source, &described, started);
        JobOutcome {
            target,
            source: described,
            result: outcome,
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

    /// NFR-SEC-020: the worker is the first component that reads a file, so it owns the byte-count
    /// ceiling. A missing file is reported through the catalogue rather than panicking.
    #[test]
    fn an_unreadable_file_is_reported_through_the_catalogue() {
        let source = LoadSource::File(std::path::PathBuf::from("no-such-file-here.nam"));
        let err = source.read().unwrap_err();
        assert_eq!(err.code.id, error_codes::FILE_UNREADABLE.id);
    }
}
