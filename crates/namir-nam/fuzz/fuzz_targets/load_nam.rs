//! `cargo fuzz` target for the `.nam` reader and the inference path a loaded model feeds
//! (NFR-QUAL-040, docs/03-implementation-roadmap.md §5 M1 quick win). The bar this holds them to
//! is exactly NFR-QUAL-040's: "shall not panic, hang, over-allocate or read out of bounds on any
//! input" — not "always reject garbage". `Ok` and `Err` are both acceptable outcomes for
//! arbitrary bytes; rejecting malformed input cleanly is `namir_nam`'s own job via its
//! `NamLoadError` catalogue, already exercised by `crates/namir-nam/tests/fixtures.rs`'s
//! `rejects_mutated_variants_without_panicking`. This target's job is continuous exploration of
//! the input space that test can't reach by itself.
//!
//! # Why it does not stop at `load`
//!
//! It did until issue #50, and the tag below said otherwise. A `.nam` file's dimensions are
//! validated against `wavenet.rs`/`lstm.rs`'s NFR-SEC-020 ceilings during `load`, but the
//! allocations and the indexing those dimensions *drive* happen afterwards, in
//! `PreparedNam::new_state` (WaveNet's per-layer causal-convolution history, sized
//! `channels * (kernel_size - 1) * dilation`; LSTM's per-cell `h`/`c`; either way the reusable
//! scratch, sized by the block) and in `PreparedNam::process_block`, which is where every read of
//! a weight and every write into that history actually happens. An out-of-bounds read or an
//! over-allocation traceable to a malicious file lives in those two functions rather than in the
//! JSON parse, and NFR-SEC-010 names exactly those failure modes — so a target that never
//! constructs a state and never processes a block was claiming the requirement it reached least.
//!
//! # The block sizes, and why they are not fuzzed
//!
//! `new_state`'s `max_block_size` is Namir's own value, not the attacker's — the host's block
//! size, or the standalone app's — so it is not taken from the input the way `load_ir.rs`'s
//! engine rate and block size are selected by leading bytes. Each is run in turn instead, which
//! also keeps every byte of `data` the candidate `.nam` file: `namir-fixtures`' mutation corpus
//! writes whole files, and a leading selector byte would shift every retained corpus entry out of
//! being one (NFR-QUAL-040's "with a corpus retained in the repository").
//!
//! More than one call per state is deliberate: WaveNet's history buffer is a ring, so the second
//! and later blocks are the ones that exercise its wrap, and a block of 1 wraps on every sample.

// trace: NFR-QUAL-040
#![no_main]

use libfuzzer_sys::fuzz_target;

/// Block sizes run against every model that loads: the smallest a host can present, a typical
/// one, and one large enough that a block spans more than the receptive field of a small model.
const BLOCK_SIZES: [usize; 3] = [1, 64, 512];

/// Calls per state — see the module comment's note on the history ring.
const BLOCKS_PER_STATE: usize = 3;

/// One period of the stimulus, repeated to fill a block: full-scale in both directions, silence,
/// and a denormal, so the inference path is driven at the edges of `f32` rather than at one level.
const STIMULUS: [f32; 6] = [0.0, 1.0, -1.0, 0.5, -0.5, 1e-38];

// trace-partial: NFR-SEC-010
// uncovered: NFR-SEC-010 — within the `.nam` kind this target reaches `load` and the
// uncovered: `new_state`/`process_block` path it feeds, and nothing else: `probe_metadata`, the
// uncovered: weights-free read `namir-library`'s scanner runs over every file it indexes, is the
// uncovered: `.nam` analogue of the separately-fuzzed `probe_wav` and is reached by no target in
// uncovered: this crate; closes M8
fuzz_target!(|data: &[u8]| {
    let Ok(prepared) = namir_nam::load(data) else {
        return;
    };

    for block in BLOCK_SIZES {
        let mut state = prepared.new_state(block);
        let input: Vec<f32> = (0..block).map(|i| STIMULUS[i % STIMULUS.len()]).collect();
        let mut out = vec![0.0f32; block];
        for _ in 0..BLOCKS_PER_STATE {
            prepared.process_block(&mut state, &input, &mut out);
        }
    }
});
