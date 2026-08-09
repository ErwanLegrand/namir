//! [`namir_worker::pool::live_worker_threads`]'s own contract, pinned in a test binary of its own.
//!
//! It has to live here rather than in `pool.rs`'s `#[cfg(test)] mod tests`: the counter is
//! process-global, `cargo test` runs a binary's tests on concurrent threads, and several of
//! `namir-worker`'s own unit tests build pools — so an exact `baseline + n` assertion is only
//! meaningful in a process where nothing else is constructing pools. One test per binary is what
//! buys that.
//!
//! Why the counter exists at all: `crates/namir-clap/tests/clap_host_teardown.rs` drives the real
//! plugin through a real CLAP host and needs to ask "did `clap_plugin.destroy` join the threads
//! that instance started before it returned?" — a question with no other answer from outside the
//! instance, and the one whose wrong answer was M9a's `0xc0000005`.

use namir_worker::pool::{ThreadPool, live_worker_threads};

#[test]
fn the_count_is_exact_on_both_edges_of_a_pools_life() {
    let baseline = live_worker_threads();

    let pool = ThreadPool::with_threads(2);
    assert_eq!(
        live_worker_threads(),
        baseline + 2,
        "the count must already be exact when the constructor returns"
    );

    pool.shutdown();
    assert_eq!(
        live_worker_threads(),
        baseline,
        "shutdown must not return while any of its threads is still alive"
    );

    drop(pool);
    assert_eq!(live_worker_threads(), baseline);
}
