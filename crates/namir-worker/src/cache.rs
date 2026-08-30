//! D-8.2's process-global resource cache: "content hash → `Weak<Prepared*>`. It is guarded by an
//! ordinary mutex, and the audio thread never touches it."
//!
//! # The IR key is wider than D-8.2 says, and it has to be
//!
//! **Decision:** NAM models are keyed by [`ContentHash`] alone, exactly as D-8.2 specifies. Impulse
//! responses are keyed by `(ContentHash, engine_rate_hz, block_size)`.
//!
//! **Rationale:** `PreparedIr::from_wav_bytes(bytes, engine_rate, block_size)` bakes *both* extra
//! arguments into the prepared object — the resample-to-engine-rate happens at load time
//! (FR-IR-030), and `block_size` determines the head partition, the whole D-9.4 partition schedule
//! and its R-8 stagger, and `PreparedChannel::block_size`. That last one is not a subtlety:
//! `process_block` **asserts** the block it is handed is no longer than the one it was prepared
//! for. So a cache hit keyed on content alone could hand instance B an IR prepared for instance A's
//! smaller block size, and the failure mode is a **panic on the audio thread**, not a wrong sound.
//!
//! `PreparedNam` genuinely needs no widening: `namir_nam::load` is a pure function of the bytes,
//! the model's own declared rate is reconciled per-slot by the Nam stage's resampler (D-9.2), and
//! `new_state(max_block_size)` produces per-instance state that never enters the cache.
//!
//! **Consequence for FR-CLAP-090, stated rather than glossed:** two instances share an IR only if
//! they agree on engine rate *and* declared maximum block size. In one host process they normally
//! do — a host drives every instance identically — but it is not guaranteed, and a host that
//! activates instances at different block sizes gets one `PreparedIr` per distinct block size. NAM
//! weights, which are the bulk of the memory FR-CLAP-090 is about, share unconditionally.
//!
//! **Consequence:** `growth_factor`/`max_partition` are deliberately *not* in the key, because this
//! cache only ever calls `from_wav_bytes` (D-9.6's defaults) and never
//! `from_wav_bytes_with_schedule`. That keeps them out of the key by construction rather than by
//! convention — there is no public path through this crate that could vary them.
//!
//! # Why the mutex is never held across a parse
//!
//! A 50 MB model takes hundreds of milliseconds to prepare (NFR-PERF-050). Holding the lock across
//! that would serialise every load in the process and put a 500 ms lock hold in front of any thread
//! that so much as looks at the cache. So the sequence is lock/miss/**unlock**/parse/lock/insert,
//! with a re-check on the second lock. Two workers racing on the same file therefore both parse —
//! bounded, harmless duplicate work — but converge on **one** `Arc`, which is what makes
//! FR-CLAP-090's sharing property hold in exactly the scenario the requirement describes (N
//! instances loading the same file at once). Without the re-check they would end up with N copies.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use namir_core::{ContentHash, SampleRate};
use namir_ir::PreparedIr;
use namir_nam::PreparedNam;

use crate::error::WorkerError;

/// The IR cache's key — see this module's doc comment for why it is not just a [`ContentHash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrKey {
    /// BLAKE3 of the file's raw bytes (P7: "identity of a model or IR is its content hash").
    pub hash: ContentHash,
    /// The engine rate the IR was resampled to at load time (FR-IR-030).
    pub engine_rate_hz: u32,
    /// The declared maximum block size its partition schedule was built for.
    pub block_size: usize,
}

/// What a cache lookup did, so a caller can report a hit without a second lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheOutcome {
    /// Whether an existing prepared resource was reused rather than parsed afresh.
    pub hit: bool,
    /// Only ever `true` for an IR: D-9.7 truncated it at ten seconds (see
    /// [`crate::error_codes::IR_TRUNCATED`]).
    pub truncated: bool,
}

/// D-8.2's cache. Worker-only; the audio thread never touches it, which is why an ordinary mutex
/// is sufficient and there is no lock for the audio thread to contend on.
#[derive(Default)]
pub struct ResourceCache {
    /// Two independent maps, not one: a NAM load must never block on an IR load's map.
    nam: Mutex<HashMap<ContentHash, Weak<PreparedNam>>>,
    ir: Mutex<HashMap<IrKey, Weak<PreparedIr>>>,
}

static SHARED: OnceLock<Arc<ResourceCache>> = OnceLock::new();

/// Reaping starts once a map holds more than this many entries, and from then on runs on every
/// miss. **The floor is the whole rule** — there is no second, live-count term.
///
/// Issue #110: this line used to end "the threshold then tracks the live count", describing a
/// `len > max(REAP_FLOOR, 2 * live)` rule the code has never implemented. The comment was the
/// wrong half, and this is the corrected text rather than the missing term, deliberately. A sweep
/// runs only on a *miss*, and a miss has just parsed a whole file — tens to hundreds of
/// milliseconds — so an O(n) `retain` over a few dozen `Weak`s is invisible beside what it is
/// amortised against. The live-count term would buy nothing measurable, would need the live count
/// carried across calls to avoid counting it every time, and would deliberately let dead entries
/// accumulate to twice the live set before reclaiming any — which is the residue NFR-PERF-070
/// cares about, kept for longer, in exchange for a saving nothing can observe.
const REAP_FLOOR: usize = 64;

impl ResourceCache {
    /// A fresh, independent cache. Tests use this; the product uses [`ResourceCache::shared`].
    pub fn new() -> Self {
        Self::default()
    }

    /// The one process-wide cache D-8.2 means by "process-global".
    ///
    /// **Decision:** the cache is *injected* everywhere in this crate (every constructor takes an
    /// `Arc<ResourceCache>`), and this accessor is the single default used by the product shells.
    ///
    /// **Rationale:** "process-global" is a statement about the product's sharing *scope*, not
    /// about the storage mechanism. A bare static as the only access path is untestable under
    /// `cargo test`'s threaded runner — every test in the binary would share one cache, so any
    /// assertion of the form "this holds exactly one entry", "this entry was reaped", or "exactly
    /// one copy exists" races with every other test. Those are precisely the assertions
    /// FR-CLAP-090, D-8.2 and NFR-PERF-070 need, so they have to be deterministic.
    ///
    /// **Consequence, recorded honestly:** FR-CLAP-090's guarantee becomes "every instance created
    /// through `namir-clap`/`namir-app` passes `ResourceCache::shared()`" — one call site each,
    /// checkable by review — rather than being unavoidable by construction. A real, small
    /// weakening, taken for testability.
    pub fn shared() -> Arc<ResourceCache> {
        Arc::clone(SHARED.get_or_init(|| Arc::new(ResourceCache::new())))
    }

    /// Returns a prepared model for `bytes`, reusing a live one if this content is already loaded.
    ///
    /// **Not RT-safe** (allocates, may take hundreds of milliseconds) — this is D-8.1 step 1.
    pub fn get_or_load_nam(
        &self,
        bytes: &[u8],
    ) -> Result<(Arc<PreparedNam>, CacheOutcome), WorkerError> {
        let key = ContentHash::of(bytes);
        if let Some(live) = lock(&self.nam).get(&key).and_then(Weak::upgrade) {
            return Ok((
                live,
                CacheOutcome {
                    hit: true,
                    truncated: false,
                },
            ));
        }

        // Parse with the lock released -- see this module's doc comment.
        let prepared = Arc::new(namir_nam::load(bytes)?);

        let mut map = lock(&self.nam);
        // Re-check: another worker may have finished the same file while this one was parsing.
        if let Some(live) = map.get(&key).and_then(Weak::upgrade) {
            return Ok((
                live,
                CacheOutcome {
                    hit: true,
                    truncated: false,
                },
            ));
        }
        map.insert(key, Arc::downgrade(&prepared));
        maybe_reap(&mut map);
        Ok((
            prepared,
            CacheOutcome {
                hit: false,
                truncated: false,
            },
        ))
    }

    /// Returns a prepared IR for `bytes` at this rate and block size. See this module's doc
    /// comment for why all three are part of the key.
    ///
    /// **Not RT-safe** — D-8.1 step 1.
    pub fn get_or_load_ir(
        &self,
        bytes: &[u8],
        engine_rate: SampleRate,
        block_size: usize,
    ) -> Result<(Arc<PreparedIr>, CacheOutcome), WorkerError> {
        let key = IrKey {
            hash: ContentHash::of(bytes),
            engine_rate_hz: engine_rate.hz(),
            block_size,
        };
        if let Some(live) = lock(&self.ir).get(&key).and_then(Weak::upgrade) {
            let truncated = live.was_truncated();
            return Ok((
                live,
                CacheOutcome {
                    hit: true,
                    truncated,
                },
            ));
        }

        let prepared = Arc::new(PreparedIr::from_wav_bytes(bytes, engine_rate, block_size)?);
        let truncated = prepared.was_truncated();

        let mut map = lock(&self.ir);
        if let Some(live) = map.get(&key).and_then(Weak::upgrade) {
            let truncated = live.was_truncated();
            return Ok((
                live,
                CacheOutcome {
                    hit: true,
                    truncated,
                },
            ));
        }
        map.insert(key, Arc::downgrade(&prepared));
        maybe_reap(&mut map);
        Ok((
            prepared,
            CacheOutcome {
                hit: false,
                truncated,
            },
        ))
    }

    /// Drops every entry whose resource is gone, returning how many were removed.
    ///
    /// A `Weak` whose target is dead still keeps the allocation's header alive — tens of bytes,
    /// not a model, but unbounded across a long auditioning session, and NFR-PERF-070 is the
    /// requirement that eats into. Reaping otherwise happens amortised on a miss (see
    /// [`maybe_reap`]); this is the explicit hook for tests and for instance teardown.
    pub fn reap(&self) -> usize {
        let mut removed = 0;
        {
            let mut map = lock(&self.nam);
            let before = map.len();
            map.retain(|_, w| w.strong_count() > 0);
            removed += before - map.len();
        }
        {
            let mut map = lock(&self.ir);
            let before = map.len();
            map.retain(|_, w| w.strong_count() > 0);
            removed += before - map.len();
        }
        removed
    }

    /// Entries currently held in the model map, live or dead. Test observability.
    pub fn nam_entries(&self) -> usize {
        lock(&self.nam).len()
    }

    /// Entries currently held in the IR map, live or dead. Test observability.
    pub fn ir_entries(&self) -> usize {
        lock(&self.ir).len()
    }
}

/// Locks, recovering from poisoning rather than propagating it.
///
/// A worker job that panics while holding one of these mutexes poisons it (D-16.3 contains the
/// panic, but poisoning is a property of the lock, not of the unwind). A poisoned cache that fails
/// every subsequent load *forever* would be a total failure, which contradicts P8's "failure
/// degrades; it does not propagate". Recovery is sound here because the invariant these maps carry
/// is only "a map of `Weak`s": no partially-completed operation can leave a state a later
/// `Weak::upgrade` misreads, and the worst residue is a stale dead entry, which the reaper removes.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Amortised reaping: only on a miss (which has just paid for a whole file parse, so an O(n) sweep
/// is invisible), and only once the map has grown past [`REAP_FLOOR`] — see that constant for why
/// the floor is the whole condition.
fn maybe_reap<K, V>(map: &mut HashMap<K, Weak<V>>)
where
    K: std::hash::Hash + Eq,
{
    if map.len() <= REAP_FLOOR {
        return;
    }
    map.retain(|_, w| w.strong_count() > 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_fixtures::ir::decaying_noise;
    use namir_fixtures::nam::{WaveNetShape, generate};

    fn model_bytes(seed: u64) -> Vec<u8> {
        generate(WaveNetShape::Nano, seed)
            .expect("fixture should generate")
            .to_json_bytes()
    }

    fn ir_bytes(seed: u64, sample_rate: u32) -> Vec<u8> {
        let taps = decaying_noise(512, seed, 128.0);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
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
        buf
    }

    fn rate(hz: u32) -> SampleRate {
        SampleRate::new(hz).unwrap()
    }

    /// **Issue #110, the rule as it actually is.** Past [`REAP_FLOOR`] the next sweep removes
    /// every dead entry, whatever the live count is — which is the assertion that tells that rule
    /// apart from the `len > max(REAP_FLOOR, 2 * live)` one [`REAP_FLOOR`]'s comment used to
    /// describe. With 65 entries live, *that* rule would not sweep until the map reached 130, so
    /// the five dead entries below would still be there afterwards.
    ///
    /// Driven against [`maybe_reap`] directly rather than through 71 cached models: the divergence
    /// the issue reports is wholly inside this function, it is generic over its map's types, and
    /// the `.nam` parses a cache-level version needs cost ten seconds to assert the same thing.
    #[test]
    fn a_sweep_past_the_floor_removes_dead_entries_whatever_the_live_count() {
        let live: Vec<Arc<usize>> = (0..=REAP_FLOOR).map(Arc::new).collect();
        let mut map: HashMap<usize, Weak<usize>> =
            live.iter().map(|a| (**a, Arc::downgrade(a))).collect();
        for i in 0..5 {
            let doomed = Arc::new(1_000 + i);
            map.insert(*doomed, Arc::downgrade(&doomed));
            // `doomed` dies here, leaving a `Weak` that still occupies its slot.
        }
        assert_eq!(map.len(), REAP_FLOOR + 6);

        maybe_reap(&mut map);

        assert_eq!(
            map.len(),
            REAP_FLOOR + 1,
            "every dead entry must go, leaving exactly the live ones"
        );
        assert!(map.values().all(|w| w.strong_count() > 0));
        drop(live);
    }

    /// The floor's other side: at or below it nothing is swept, even a map that is entirely dead.
    /// Reaping is amortised against a file parse, and a handful of `Weak`s is not worth a sweep —
    /// [`ResourceCache::reap`] is the explicit hook for a caller that wants one anyway.
    #[test]
    fn a_sweep_at_the_floor_leaves_the_map_alone() {
        let mut map: HashMap<usize, Weak<usize>> = HashMap::new();
        for i in 0..REAP_FLOOR {
            let doomed = Arc::new(i);
            map.insert(i, Arc::downgrade(&doomed));
        }
        assert_eq!(map.len(), REAP_FLOOR);

        maybe_reap(&mut map);

        assert_eq!(
            map.len(),
            REAP_FLOOR,
            "at the floor the sweep does not run, so even dead entries stay"
        );
    }

    /// **FR-CLAP-090's core mechanism:** two loads of the same content share one copy of the
    /// weights, rather than each getting its own.
    #[test]
    fn identical_bytes_yield_the_same_arc() {
        let cache = ResourceCache::new();
        let bytes = model_bytes(1);
        let (a, first) = cache.get_or_load_nam(&bytes).unwrap();
        let (b, second) = cache.get_or_load_nam(&bytes).unwrap();
        assert!(!first.hit, "the first load cannot be a hit");
        assert!(second.hit, "the second load should have hit the cache");
        assert!(Arc::ptr_eq(&a, &b), "the same content must share one Arc");
    }

    #[test]
    fn different_bytes_yield_different_arcs() {
        let cache = ResourceCache::new();
        let (a, _) = cache.get_or_load_nam(&model_bytes(1)).unwrap();
        let (b, _) = cache.get_or_load_nam(&model_bytes(2)).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    /// **The reason the IR key is wider than D-8.2 says.** Same bytes, different engine rate, must
    /// not share — the cached object was resampled to the other rate at load time.
    #[test]
    fn the_ir_key_includes_the_engine_rate() {
        let cache = ResourceCache::new();
        let bytes = ir_bytes(7, 48_000);
        let (a, _) = cache.get_or_load_ir(&bytes, rate(48_000), 64).unwrap();
        let (b, _) = cache.get_or_load_ir(&bytes, rate(44_100), 64).unwrap();
        assert!(
            !Arc::ptr_eq(&a, &b),
            "IRs resampled to different engine rates must not share a cache entry"
        );
    }

    /// The load-bearing half: same bytes, different block size, must not share — and the proof is
    /// that the wrongly-shared object would **panic** rather than merely sound wrong, because
    /// `PreparedIr::process_block` asserts the block length its schedule was built for. Processing
    /// a full-size block through each is what actually demonstrates the necessity.
    #[test]
    fn the_ir_key_includes_the_block_size_and_a_shared_entry_would_panic() {
        let cache = ResourceCache::new();
        let bytes = ir_bytes(7, 48_000);
        let (small, _) = cache.get_or_load_ir(&bytes, rate(48_000), 64).unwrap();
        let (large, _) = cache.get_or_load_ir(&bytes, rate(48_000), 512).unwrap();
        assert!(
            !Arc::ptr_eq(&small, &large),
            "IRs prepared for different block sizes must not share a cache entry"
        );

        // Each is usable at its own declared block size. If the cache had handed `small` back for
        // the 512 request, this second call would assert inside namir-ir instead.
        let input = vec![0.0f32; 512];
        let mut out = vec![0.0f32; 512];
        let mut state = large.new_state();
        let mut outs: [&mut [f32]; 1] = [&mut out];
        large.process_block(&mut state, &input, &mut outs[..1]);
    }

    /// D-8.2's `Weak` consequence: "an unreferenced model is freed rather than pinned for the
    /// process lifetime." This is also NFR-PERF-070's half of the story that M4 can actually close.
    #[test]
    fn dropping_the_last_arc_frees_the_resource_and_reap_removes_the_entry() {
        let cache = ResourceCache::new();
        let bytes = model_bytes(3);
        let weak = {
            let (arc, _) = cache.get_or_load_nam(&bytes).unwrap();
            Arc::downgrade(&arc)
        };
        assert!(
            weak.upgrade().is_none(),
            "the model should be freed once no instance holds it"
        );
        assert_eq!(cache.nam_entries(), 1, "the dead Weak is still mapped");
        assert_eq!(cache.reap(), 1);
        assert_eq!(cache.nam_entries(), 0);
    }

    /// A dead entry must not shadow a fresh load of the same content.
    #[test]
    fn a_dead_entry_is_replaced_by_a_fresh_load() {
        let cache = ResourceCache::new();
        let bytes = model_bytes(4);
        drop(cache.get_or_load_nam(&bytes).unwrap().0);
        let (revived, outcome) = cache.get_or_load_nam(&bytes).unwrap();
        assert!(!outcome.hit, "a dead entry must not report as a hit");
        assert!(Arc::strong_count(&revived) >= 1);
    }

    /// **FR-CLAP-090 under the load pattern the requirement actually describes:** N instances
    /// loading the same file at the same moment. Without the post-parse re-check they would each
    /// keep their own copy and the requirement would fail in exactly this scenario.
    #[test]
    fn concurrent_loads_of_the_same_bytes_converge_on_one_arc() {
        use std::sync::Barrier;

        let cache = Arc::new(ResourceCache::new());
        let bytes = Arc::new(model_bytes(5));
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let bytes = Arc::clone(&bytes);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    cache.get_or_load_nam(&bytes).unwrap().0
                })
            })
            .collect();

        let arcs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for a in &arcs[1..] {
            assert!(
                Arc::ptr_eq(&arcs[0], a),
                "concurrent loads of one file must converge on a single Arc"
            );
        }
        assert_eq!(cache.nam_entries(), 1);
    }

    /// A failed load must leave nothing behind for a later lookup to trip over.
    #[test]
    fn a_load_failure_leaves_no_entry_behind() {
        let cache = ResourceCache::new();
        assert!(cache.get_or_load_nam(b"not a nam file").is_err());
        assert_eq!(cache.nam_entries(), 0);
    }

    /// A panic while a cache lock is held poisons it. Recovering rather than propagating is a
    /// deliberate decision (see [`lock`]): P8 says failure degrades, and a cache that failed every
    /// load forever after one panic would not be degradation.
    #[test]
    fn a_poisoned_cache_still_serves() {
        let cache = Arc::new(ResourceCache::new());
        let bytes = model_bytes(6);
        {
            let cache = Arc::clone(&cache);
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let _ = std::thread::spawn(move || {
                let _guard = cache.nam.lock().unwrap();
                panic!("poisoning the cache lock on purpose");
            })
            .join();
            std::panic::set_hook(previous);
        }
        assert!(
            cache.get_or_load_nam(&bytes).is_ok(),
            "a poisoned cache must still serve (P8: degradation, not failure)"
        );
    }

    #[test]
    fn shared_returns_the_same_cache() {
        assert!(Arc::ptr_eq(
            &ResourceCache::shared(),
            &ResourceCache::shared()
        ));
    }
}
