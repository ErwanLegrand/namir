//! M2's six product `Stage`/`StagePrep` pairs (Trim/Gate/Nam/Ir/Eq/Out) and their fixed-chain
//! assembly (FR-CHAIN-010), per `03-implementation-roadmap.md` §6.

pub mod eq;
pub mod gate;
pub mod ir;
pub mod nam;
pub mod out;
pub mod trim;

/// FR-NAM-070: "the crossfade shall be equal-power and 5-50 ms." 20 ms is this project's chosen
/// point in that window, shared by the Nam and Ir stages.
///
/// **Public since M4, and defined once rather than twice.** `nam.rs` and `ir.rs` each carried their
/// own private copy of this figure, which is a latent way for the two stages' fades to drift apart
/// silently. It is public because `namir-worker` needs it: D-8.1's serialisation rule has to know
/// how long a handover occupies a stage in order to keep two of them from overlapping, and hard-
/// coding a second copy of the number over there would be the same mistake one layer further out.
pub const HANDOVER_CROSSFADE_MS: f64 = 20.0;

use crate::chain::Chain;
use crate::prepare::{PrepareContext, PrepareError};
use crate::stage::{Stage, StagePrep};

/// Builds the fixed 1.0 signal chain (FR-CHAIN-010) with M2's cross-cutting features
/// (FR-CHAIN-030/080/090) active — this is the one real entry point M2 delivers for assembling a
/// product-shaped chain; every other construction path (`Chain::new` called directly, as this
/// crate's own tests still do) is test/scaffolding-only and does not get those features, per
/// `Chain::prepare_crosscutting`'s own doc comment.
///
/// Runtime order is **gate before trim**, not FR-CHAIN-010's literal prose order ("input trim →
/// noise gate → ..."): `02-architecture.md` D-9.8 records this as a deliberate usability decision
/// (the gate's threshold should reference the interface's actual noise floor, not move when the
/// user adjusts trim), explicitly flagged there for review rather than an oversight, and
/// `03-implementation-roadmap.md` §6 directs M2 to build the actual chain that way:
/// `gate → trim → nam → ir → eq → out`.
///
/// *Amended (M9a, 2026-08-09):* D-9.8's flagged-for-review divergence is resolved, and resolved in
/// this function's favour — FR-CHAIN-010 is amended to describe the shipped order rather than this
/// chain rebuilt to the old prose. The paragraph above is therefore history, not a live deviation:
/// `gate → trim → nam → ir → eq → out` is what the FRS and this function both say. See D-9.8's own
/// M9a consequence note in `02-architecture.md`.
///
/// None of the six stages has a resource loaded yet (no NAM model, no IR) — per FR-CHAIN-040/
/// FR-NAM-130/FR-IR-100 every stage that can hold one already behaves as bypassed until a future
/// caller (M4's worker, once it exists) calls `NamStage::load_model`/`IrStage::load_ir`.
pub fn build_default_chain(ctx: &PrepareContext) -> Result<Chain, PrepareError> {
    let gate_stage = gate::GatePrep.prepare(ctx)?;
    let trim_stage = trim::TrimPrep.prepare(ctx)?;
    let nam_stage = nam::NamPrep.prepare(ctx)?;
    let ir_stage = ir::IrPrep.prepare(ctx)?;
    let eq_stage = eq::EqPrep.prepare(ctx)?;
    let out_stage = out::OutPrep.prepare(ctx)?;

    let stages: Vec<Box<dyn Stage>> = vec![
        Box::new(gate_stage),
        Box::new(trim_stage),
        Box::new(nam_stage),
        Box::new(ir_stage),
        Box::new(eq_stage),
        Box::new(out_stage),
    ];

    let mut chain = Chain::new(stages);
    chain.prepare_crosscutting(ctx);
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_core::{ChannelConfig, SampleRate};

    fn ctx(channel_config: ChannelConfig) -> PrepareContext {
        PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, channel_config).unwrap()
    }

    /// The whole point of M2: this compiles and runs at all, for every channel configuration
    /// FR-CHAIN-060 requires, with nothing loaded (FR-CHAIN-040) and produces silence in, silence
    /// out without panicking, allocating on the audio thread, or emitting a non-finite sample.
    ///
    /// **It carries no FR-CHAIN-010 or FR-CHAIN-060 tag any more, and that is the point** (M14).
    /// Zeros in and zeros out with nothing loaded cannot distinguish any ordering of any stages
    /// from an empty chain, and it never puts an IR into an `IrStage` at all — so it was covering
    /// neither requirement's `Verify:` method. Both now resolve through `crate::chain_probes`,
    /// which runs a real probe signal through a loaded chain. This test keeps its own real job:
    /// FR-CHAIN-040's nothing-loaded behaviour, in every channel configuration.
    #[test]
    fn builds_and_runs_silently_for_every_channel_config() {
        for channel_config in [
            ChannelConfig::Mono,
            ChannelConfig::MonoToStereo,
            ChannelConfig::Stereo,
        ] {
            let ctx = ctx(channel_config);
            // trace: FR-CHAIN-040
            let mut chain = build_default_chain(&ctx).unwrap();
            let n = channel_config.output_channels() as usize;
            let mut bufs: Vec<Vec<f32>> = (0..n).map(|_| vec![0.0f32; 64]).collect();
            let mut refs: Vec<&mut [f32]> = bufs.iter_mut().map(|b| b.as_mut_slice()).collect();
            let mut io = crate::stage_io::StageIo::new(&mut refs, 64);
            crate::rt_harness::audio_section(|| chain.process(&mut io));
            for ch in io.channels_mut() {
                for s in ch {
                    assert!(s.is_finite());
                    assert_eq!(*s, 0.0, "expected silence with nothing loaded");
                }
            }
            assert_eq!(chain.fault_count(), 0);
        }
    }

    /// **No NFR-PERF-020 tag any more** (M14): asserting `0` with nothing loaded, where every
    /// stage's `latency_samples()` is `0` by construction, can only ever confirm the arithmetic of
    /// summing zeros — it never compared a chain's actual group delay against the figure it
    /// reports, at any configuration where that figure is not zero. That comparison now lives in
    /// `crate::chain_probes`. This stays as what it always really was: the assembly's
    /// nothing-loaded latency smoke test.
    #[test]
    fn reports_a_nonzero_latency_only_from_stages_that_declare_one() {
        let ctx = ctx(ChannelConfig::Mono);
        let chain = build_default_chain(&ctx).unwrap();
        // At M2, with nothing loaded, every stage's own latency_samples() is 0 (Nam's resampler
        // is bypassed at 48 kHz == its own default assumption, and no model is loaded to report a
        // different one anyway).
        assert_eq!(chain.latency_samples(), 0);
    }
}
