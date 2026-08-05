//! Test-only scaffolding: a trivial real `Stage`/`StagePrep` pair (`FixedGainPrep` /
//! `FixedGainStage`) to exercise `Chain` and the D-7.5 RT harness against real, non-allocating
//! code, plus two single-purpose fakes — `ConstantTail`, used only to pin down
//! `Chain::tail_samples`'s arithmetic without any DSP getting in the way, and `AllocatingStage`,
//! used only to prove the D-7.5 harness actually catches a violation. None of these is a 1.0
//! product stage (Trim/Gate/Nam/Ir/Eq/Out — see the crate doc); this module never compiles
//! outside `cfg(test)`.

use namir_core::db_to_linear;

use crate::param::{ParamChange, ParamId};
use crate::prepare::{PrepareContext, PrepareError};
use crate::stage::{Stage, StagePrep};
use crate::stage_io::StageIo;
use crate::telemetry::TelemetrySink;

pub const GAIN_PARAM_ID: ParamId = ParamId(1);

pub struct FixedGainPrep {
    pub gain_db: f32,
}

impl StagePrep for FixedGainPrep {
    type Prepared = FixedGainStage;

    fn prepare(&self, _ctx: &PrepareContext) -> Result<Self::Prepared, PrepareError> {
        Ok(FixedGainStage {
            linear: db_to_linear(self.gain_db),
        })
    }
}

pub struct FixedGainStage {
    linear: f32,
}

impl Stage for FixedGainStage {
    fn process(&mut self, io: &mut StageIo<'_>) {
        for channel in io.channels_mut() {
            for sample in channel.iter_mut() {
                *sample *= self.linear;
            }
        }
    }

    fn reset(&mut self) {}

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail_samples(&self) -> u32 {
        0
    }

    fn apply(&mut self, change: ParamChange) {
        if change.id == GAIN_PARAM_ID {
            self.linear = db_to_linear(change.value);
        }
    }

    fn telemetry(&self, _out: &mut TelemetrySink<'_>) {}
}

/// Reports fixed `latency_samples`/`tail_samples` and otherwise does nothing.
pub struct ConstantTail {
    pub latency: u32,
    pub tail: u32,
}

impl Stage for ConstantTail {
    fn process(&mut self, _io: &mut StageIo<'_>) {}
    fn reset(&mut self) {}

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn tail_samples(&self) -> u32 {
        self.tail
    }

    fn apply(&mut self, _change: ParamChange) {}
    fn telemetry(&self, _out: &mut TelemetrySink<'_>) {}
}

/// Deliberately violates P1 by allocating in `process`, to prove the D-7.5 harness catches
/// something real — a harness nobody has shown catches a violation isn't proven.
pub struct AllocatingStage;

impl Stage for AllocatingStage {
    fn process(&mut self, _io: &mut StageIo<'_>) {
        let v: Vec<u8> = Vec::with_capacity(16);
        std::hint::black_box(v);
    }

    fn reset(&mut self) {}

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail_samples(&self) -> u32 {
        0
    }

    fn apply(&mut self, _change: ParamChange) {}
    fn telemetry(&self, _out: &mut TelemetrySink<'_>) {}
}
