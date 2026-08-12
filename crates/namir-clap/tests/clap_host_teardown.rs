//! The first in-process CLAP **host** harness in this repository (D-18.6's `clack-host` dev
//! dependency, adopted at M9a precisely so this surface became testable at all), driving the real
//! `NamirClapPlugin` through the real C vtable — `PluginEntry::load_from_clack` instantiates it
//! with no `dlopen` and, importantly, no `unsafe` of ours (D-5.3 permits exactly one
//! `#![allow(unsafe_code)]` file in this crate, `src/gui.rs`, and this is not it).
//!
//! # What it asserts, and why that is the interesting question
//!
//! That `clap_plugin.destroy` has joined every worker thread the instance started, **by the time it
//! returns**. A host is entitled to unload the plugin's shared library the instant destroy returns
//! — `clap-validator` does exactly that, once per test — so a thread still executing plugin code at
//! that moment is executing pages the loader is about to unmap. That is what M9a's CI run reported
//! as `ERROR Test state-reproducibility-basic crashed: exit code: 0xc0000005`, and
//! `impl Drop for NamirShared` (`src/shared.rs`) is the fix. There is no handle on the instance's
//! own pool from out here, which is what [`live_worker_threads`] exists for.
//!
//! # Honest scope
//!
//! This drives the `activate` call site of `crate::worker_jobs::spawn_recall`, not the
//! `state_ext::load` one that happened to lose the race on CI — both spawn the same job onto the
//! same pool, and the defect was never specific to either. The host-driven `state` save/load path
//! itself stays unreachable from here: calling it needs `clack-extensions`' `clack-host` feature
//! (`PluginState::save`/`load` live in its `state/host.rs`, behind that gate), and enabling it adds
//! a `clack-host` row to THIRD-PARTY-NOTICES.md — see `Cargo.toml`'s dev-dependency comment for the
//! measurement and why that blocker is not resolved here.
//!
//! **The last sentence of that paragraph used to read "So FR-CLAP-050's `// uncovered:` field stays
//! exactly as written", and M9b's own later work falsified it.** D-18.7 resolved the blocker with a
//! non-default `host-ext-tests` feature, so `PluginState::load` *is* now driven through the real
//! vtable — by `clap_host_block_sizes.rs`, `clap_host_latency.rs`, `clap_host_rt_blocking.rs` and
//! `fr_cfg_020_shell_parity.rs` — and FR-CLAP-050's field has been narrowed to the `save` direction
//! accordingly. Nothing about *this* file changed: it still reaches only the `activate` call site,
//! which is what its scope claim above is about.
//!
//! Measured against the pre-fix build (`Drop for NamirShared` neutralised, everything else
//! untouched) this fails on cycle 0 or 1, five runs out of five, on an *idle* machine — a far
//! sharper signal than the validator itself ever gave, which needed a contended runner to show
//! anything. That is because it asks the question directly rather than waiting for a fault: the
//! recall job is queued during `activate` and destroy follows microseconds later, so the pool has
//! reliably not drained by the time destroy returns. `clap-validator` only ever saw the subset of
//! those cases where the loader also got there first.

use clack_host::prelude::{
    HostHandlers, HostInfo, PluginAudioConfiguration, PluginEntry, PluginInstance,
};
use clack_plugin::prelude::{DefaultPluginFactory, SinglePluginEntry};
use namir_clap::NamirClapPlugin;
use namir_worker::pool::live_worker_threads;

/// The minimum a host has to be. `()` already implements all three handler traits (clack-host's own
/// "QoL implementations"), and this harness observes the plugin through thread counts rather than
/// through host callbacks, so none of them needs a body.
struct TeardownTestHost;

impl HostHandlers for TeardownTestHost {
    type Shared<'a> = ();
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

/// How many create/activate/deactivate/destroy cycles to run. More than one because the property
/// is about a race, and because a leak that recurs per instance is worth catching as a trend rather
/// than only on the first pass.
const CYCLES: usize = 64;

#[test]
fn destroying_an_instance_leaves_none_of_its_worker_threads_running() {
    let baseline = live_worker_threads();

    let host_info = HostInfo::new(
        "Namir teardown harness",
        "Namir",
        "https://example.invalid",
        env!("CARGO_PKG_VERSION"),
    )
    .expect("host info");

    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<NamirClapPlugin>>(c"")
        .expect("the in-process entry must load");

    // FR-CLAP-010's id, taken from the plugin's own descriptor rather than restated here, so this
    // harness cannot drift from what the entry actually advertises.
    let descriptor = <NamirClapPlugin as DefaultPluginFactory>::get_descriptor();
    let plugin_id = descriptor.id().expect("the descriptor must carry an id");

    for cycle in 0..CYCLES {
        let mut instance =
            PluginInstance::<TeardownTestHost>::new(|_| (), |_| (), &entry, plugin_id, &host_info)
                .expect("the plugin must instantiate");

        assert!(
            live_worker_threads() > baseline,
            "cycle {cycle}: instantiating the plugin must have started its worker pool -- \
             otherwise this test is asserting nothing"
        );

        // `activate` is one of the two `spawn_recall` call sites (`src/audio.rs`); it queues a job
        // that captures an `Arc<SharedInner>`, which is the shape that made teardown racy.
        let processor = instance
            .activate(|_, _| (), audio_configuration())
            .expect("the plugin must activate at 48 kHz stereo");
        instance.deactivate(processor);

        drop(instance); // `clap_plugin.destroy`

        assert_eq!(
            live_worker_threads(),
            baseline,
            "cycle {cycle}: destroy returned with worker threads still running -- a host that \
             unloads the plugin library here faults them mid-instruction (0xc0000005)"
        );
    }
}

fn audio_configuration() -> PluginAudioConfiguration {
    PluginAudioConfiguration {
        sample_rate: 48_000.0,
        min_frames_count: 1,
        max_frames_count: 512,
    }
}
