use crate::param::ParamChange;
use crate::stage::Stage;
use crate::stage_io::StageIo;

/// D-6.1: "the chain is `Vec<Box<dyn Stage>>` built once during preparation." Building that
/// vector — running each configured stage's `StagePrep::prepare` and boxing the result — is the
/// caller's job. 1.0's fixed six-stage assembly and any future dynamic chain-building (RD-2)
/// both belong to whatever owns the stage *list* (worker/adapter code, not yet built), not to
/// `Chain` itself.
pub struct Chain {
    stages: Vec<Box<dyn Stage>>,
}

impl Chain {
    /// Wraps an already-`prepare`d stage list. Building that list is the caller's job; see this
    /// struct's doc comment.
    pub fn new(stages: Vec<Box<dyn Stage>>) -> Self {
        Self { stages }
    }

    /// Runs every stage in order, on the audio thread (RT).
    pub fn process(&mut self, io: &mut StageIo<'_>) {
        for stage in &mut self.stages {
            stage.process(io);
        }
    }

    /// Resets every stage's internal state, e.g. on transport stop/reposition.
    pub fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
    }

    /// Each stage's delay accumulates serially through the chain — stage *i+1* receives stage
    /// *i*'s already-delayed output — so this is a plain sum.
    pub fn latency_samples(&self) -> u32 {
        self.stages.iter().map(|s| s.latency_samples()).sum()
    }

    /// Deliberately not a sum. "Tail" is how long a stage keeps producing non-negligible output
    /// after its *own* input goes silent (e.g. convolution/reverb decay). For a chain, the tail
    /// that reaches the chain's output is whichever internal stage's tail takes longest to
    /// *exit* — and a tail produced partway through the chain still has to cross every later
    /// stage's latency before it does.
    ///
    /// So stage `i` contributes `tail_i + sum(latency_j for j after i)`, and the chain's tail is
    /// the **max** over stages, not the sum: these are delayed views of the *same* physical
    /// decay reaching the output at different times, not independent decays that stack. Summing
    /// would be the right model if two stages independently re-decayed the *same* signal — e.g.
    /// two convolution/reverb stages in series, where the true combined tail is closer to the
    /// sum of both impulse-response lengths — but 1.0's six-stage chain has at most one stage
    /// with a nonzero tail (the IR stage), so that compounding case doesn't arise yet. If RD-2
    /// ever puts two tail-bearing stages in series, this is the first place to revisit.
    pub fn tail_samples(&self) -> u32 {
        let mut downstream_latency = 0u32;
        let mut max_contribution = 0u32;
        for stage in self.stages.iter().rev() {
            let contribution = stage.tail_samples().saturating_add(downstream_latency);
            max_contribution = max_contribution.max(contribution);
            downstream_latency = downstream_latency.saturating_add(stage.latency_samples());
        }
        max_contribution
    }

    /// Broadcasts to every stage. RD-2's per-instance parameter addressing (D-10.2) is future
    /// work by design — 1.0's fixed chain has no ambiguity to resolve, so each stage just
    /// ignores ids it doesn't own.
    pub fn apply(&mut self, change: ParamChange) {
        for stage in &mut self.stages {
            stage.apply(change);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::ParamId;
    use crate::prepare::PrepareContext;
    use crate::rt_harness::audio_section;
    use crate::stage::StagePrep;
    use crate::test_support::{ConstantTail, FixedGainPrep, GAIN_PARAM_ID};
    use namir_core::{ChannelConfig, SampleRate};

    fn ctx() -> PrepareContext {
        PrepareContext::new(SampleRate::new(48_000).unwrap(), 64, ChannelConfig::Mono).unwrap()
    }

    #[test]
    fn empty_chain_has_zero_latency_and_tail() {
        let chain = Chain::new(Vec::new());
        assert_eq!(chain.latency_samples(), 0);
        assert_eq!(chain.tail_samples(), 0);
    }

    #[test]
    fn latency_sums_across_stages() {
        let stages: Vec<Box<dyn Stage>> = vec![
            Box::new(ConstantTail {
                latency: 10,
                tail: 0,
            }),
            Box::new(ConstantTail {
                latency: 5,
                tail: 0,
            }),
        ];
        let chain = Chain::new(stages);
        assert_eq!(chain.latency_samples(), 15);
    }

    #[test]
    fn tail_of_a_single_stage_passes_through_unchanged() {
        let stages: Vec<Box<dyn Stage>> = vec![Box::new(ConstantTail {
            latency: 0,
            tail: 100,
        })];
        let chain = Chain::new(stages);
        assert_eq!(chain.tail_samples(), 100);
    }

    #[test]
    fn tail_from_an_earlier_stage_gains_downstream_latency() {
        // Stage 1 has the tail; stage 2 has no tail but adds latency the tail must cross.
        let stages: Vec<Box<dyn Stage>> = vec![
            Box::new(ConstantTail {
                latency: 0,
                tail: 100,
            }),
            Box::new(ConstantTail {
                latency: 20,
                tail: 0,
            }),
        ];
        let chain = Chain::new(stages);
        assert_eq!(chain.tail_samples(), 120);
    }

    #[test]
    fn tail_is_the_max_contribution_not_the_sum() {
        // Stage 1's contribution: 100 + 20 (downstream latency) = 120.
        // Stage 2's contribution: 30 + 0 = 30.
        // A sum (150, or 120 + 30) would overcount: these are the same input's decay observed
        // at two points, not two independent decays.
        let stages: Vec<Box<dyn Stage>> = vec![
            Box::new(ConstantTail {
                latency: 0,
                tail: 100,
            }),
            Box::new(ConstantTail {
                latency: 20,
                tail: 30,
            }),
        ];
        let chain = Chain::new(stages);
        assert_eq!(chain.tail_samples(), 120);
    }

    #[test]
    fn later_stage_tail_can_dominate() {
        let stages: Vec<Box<dyn Stage>> = vec![
            Box::new(ConstantTail {
                latency: 5,
                tail: 10,
            }),
            Box::new(ConstantTail {
                latency: 0,
                tail: 200,
            }),
        ];
        let chain = Chain::new(stages);
        assert_eq!(chain.tail_samples(), 200);
    }

    #[test]
    fn apply_broadcasts_to_every_stage() {
        let prep = FixedGainPrep { gain_db: 0.0 };
        let a = prep.prepare(&ctx()).unwrap();
        let b = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(a), Box::new(b)]);

        chain.apply(ParamChange {
            id: GAIN_PARAM_ID,
            value: 6.0,
        });

        let mut left = [1.0f32; 4];
        let mut channels: [&mut [f32]; 1] = [&mut left];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));

        // Both stages picked up the change, so gain was applied twice (cascaded).
        let expected = namir_core::db_to_linear(6.0) * namir_core::db_to_linear(6.0);
        for s in io.channel(0) {
            assert!((*s - expected).abs() < 1e-4);
        }
    }

    #[test]
    fn apply_ignores_unrelated_ids() {
        let prep = FixedGainPrep { gain_db: 0.0 };
        let stage = prep.prepare(&ctx()).unwrap();
        let mut chain = Chain::new(vec![Box::new(stage)]);

        chain.apply(ParamChange {
            id: ParamId(999),
            value: 6.0,
        });

        let mut left = [1.0f32; 4];
        let mut channels: [&mut [f32]; 1] = [&mut left];
        let mut io = StageIo::new(&mut channels, 4);
        audio_section(|| chain.process(&mut io));
        for s in io.channel(0) {
            assert!((*s - 1.0).abs() < 1e-6);
        }
    }
}
