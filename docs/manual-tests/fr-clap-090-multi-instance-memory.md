# FR-CLAP-090 manual test: multiple instances share cached weights, measured

**Requirement (literal):** multiple instances of the plugin shall coexist in one host process
without interfering with each other's state, and shall share immutable resources (such as a
loaded model's weights) where they reference the same file.
*Verify: I plus B — measure that N instances of one model use materially less memory than N
separate copies.*

## What's mechanically true today, proven at the unit level

`crates/namir-clap/src/shared.rs`'s `SharedInner::new` calls **`namir_worker::ResourceCache::
shared()`**, not `ResourceCache::new()` — a `OnceLock`-backed process-global cache
(`crates/namir-worker/src/cache.rs`), so every `namir-clap` instance created in one host process
resolves the *same* `Arc<ResourceCache>`. This is checked directly:
`shared::tests::two_shared_inners_resolve_to_the_same_process_global_cache` constructs two
independent `SharedInner`s and asserts `Arc::ptr_eq` on their `cache` fields — passing. One level
down, `namir-worker`'s own `two_instances_loading_one_file_share_its_weights` test proves that two
`Instance`s loading identical bytes through that shared cache converge on one `Arc<PreparedNam>`
(`Arc::strong_count` and `cache.nam_entries() == 1`), and `cache.rs`'s
`concurrent_loads_of_the_same_bytes_converge_on_one_arc` proves the same under genuine thread
contention (8 threads racing to load the same bytes at once — the exact scenario FR-CLAP-090
actually describes, "N instances loading the same file at the same moment").

**What is real, not merely theoretical:** the cache-sharing mechanism has exactly one call site
per instance (`ResourceCache::shared()` in `SharedInner::new`), so every code path that creates a
`namir-clap` plugin instance goes through it — there is no separate, un-shared cache construction
anywhere in this crate. `namir-worker/src/cache.rs`'s own doc comment states the honest scope of
this guarantee: *"every instance created through `namir-clap`/`namir-app` passes
`ResourceCache::shared()`... checkable by review"* — a real, small weakening for testability
(`cargo test`'s threaded runner needs independent caches per test), not a gap in the product path.

**Lock-free from the audio thread (NFR-RT-010's condition on this requirement):** the audio thread
(`crates/namir-clap/src/audio.rs`'s `NamirAudioProcessor::process`) never touches
`SharedInner::cache` at all — only worker-pool jobs (`crates/namir-clap/src/worker_jobs.rs`) and
`Instance::load`/`recall` (both explicitly "not RT-safe, may block") reach into it. The cache's own
internal `Mutex`es (`namir-worker/src/cache.rs`) are therefore never contended by an audio
callback, satisfying "without introducing a lock the audio thread can contend on" (OQ-8) by
construction — checkable directly from `crates/namir-clap/src/audio.rs`'s own source, which has no
`cache` reference anywhere in `NamirAudioProcessor` or its `process`/`apply_automation`/
`publish_latency` methods.

## Why the memory measurement (the "B" in "I plus B") needs a real host

Proving *sharing occurs* is unit-testable (done, above). Proving *N instances of one model use
materially less memory than N separate copies* is a whole-process memory measurement against a
real NAM model (tens of MB, per NFR-PERF-050's own 50 MB figure) loaded into several real plugin
instances inside a real host's process — this agent session has no host to load instances into,
and a synthetic in-process benchmark inside this crate's own test suite would only re-prove the
unit-level sharing already covered above, not the end-to-end host-process memory figure FR-CLAP-090
actually asks to be measured.

## Script

1. Obtain (or generate, via `namir-fixtures`) a NAM model file at least 10 MB.
2. In Reaper (or any CLAP host that reports per-plugin or total process memory), load **one**
   instance of Namir on a track, load that model, and note the host process's total working-set
   memory (Task Manager's "Memory" column for the host's process, or the host's own plugin memory
   reporting if it has one).
3. Load **four more** instances of Namir (five total), each loading the *same* model file by
   content (same bytes — copy the file if the host resolves by path rather than letting the same
   file be selected twice in its browser). Note the process's total memory again.
4. Compute the delta per additional instance. If sharing is working, the delta per instance after
   the first should be far smaller than the model file's own size (only per-instance inference
   scratch and resampler state — see `crates/namir-engine/src/stages/nam.rs`'s slot construction —
   not another copy of the weights). If sharing were *not* working, the delta per instance would be
   approximately the model file's size, every time.
5. Repeat with five instances loading five *different* models, confirming memory grows roughly
   linearly in that case (a control, proving step 4's flat-ish growth isn't an artifact of the
   measurement method).

## Executed run (this session)

**Result: NOT EXECUTED** (real-host memory measurement). This agent session has no host to
load multiple
plugin instances into and no way to observe a host process's memory footprint — see
`docs/manual-tests/fr-ui-010-standalone-window-renders.md`'s identical limitation note. What *is*
verified automatically and stands as strong indirect evidence: every unit-level sharing test named
above passes, `clap-validator`'s own suite ran cleanly with the plugin loaded and unloaded
repeatedly (no crash or leak observed by the validator itself across ~44 test runs in one process),
and the cache-sharing code path has exactly one call site, reviewed directly in this document.
