//! D-10.4's chain-level descriptors: FR-CHAIN-030's global bypass and FR-CHAIN-090's output
//! ceiling. Declared here rather than under [`crate::stages`] because neither belongs to one of
//! the six product stages — both are fields on `namir_engine::Chain` itself (see that crate's
//! `chain.rs` module doc comment) — but FR-PARAM-030 ("parameter changes shall be accepted from
//! the UI, CLAP automation, and preset loading, and converge to the same engine state regardless
//! of source") does not carve out an exception for chain-level state, so it needs a
//! `ParamDescriptor` home exactly as a stage parameter does.
//!
//! Before D-10.4, these two values had no such home: `namir_engine::Chain` exposed them through
//! dedicated `set_global_bypass`/`set_output_ceiling_db` methods and dedicated
//! `Command::SetGlobalBypass`/`SetOutputCeilingDb` variants, and `namir-state` mirrored the split
//! with a second, parallel `global` document section instead of `parameters`/`REGISTRY` — D-10.3's
//! own consequence note recorded the gap, and `descriptor.rs`'s test-only `out.channel_mode`
//! descriptor was the evidence it had been noticed once already and left unclosed. D-10.4 closes
//! it: both values are now ordinary `REGISTRY` entries, keyed `global.*` rather than `<stage>.*`,
//! so a host (M6's `namir-clap`, in particular) sees global bypass as a normal automatable
//! parameter rather than a side-channel only Rust code could reach.

use crate::descriptor::{
    ParamDescriptor, ParamKind, SmoothingCategory, StepIndex, Unit, ValueFormat,
};

/// FR-CHAIN-030: "the engine shall provide a global bypass that routes input to output with unity
/// gain, applying only the latency compensation needed for sample alignment." Off by default,
/// matching `namir_engine::Chain::new`'s own `global_bypass: false`.
pub const GLOBAL_BYPASS: ParamDescriptor = ParamDescriptor::new(
    "global.bypass",
    "Global Bypass",
    Unit::None,
    ParamKind::Stepped {
        values: &["Off", "On"],
        default_index: StepIndex(0),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

/// FR-CHAIN-090: "the engine shall not emit a sample whose magnitude exceeds a configurable
/// ceiling (default 0 dBFS) at the output stage." Range mirrors `stages::out::GAIN_DB`'s -60..+12
/// dB span — the same headroom convention this project already uses for an output-adjacent dB
/// control — since FR-CHAIN-090 states only the default, not a range.
pub const OUTPUT_CEILING_DB: ParamDescriptor = ParamDescriptor::new(
    "global.output_ceiling_db",
    "Output Ceiling",
    Unit::Decibels,
    ParamKind::Continuous {
        min: -60.0,
        max: 12.0,
        default: 0.0,
    },
    ValueFormat::FixedDecimals(1),
    SmoothingCategory::GainLike,
);

/// Prototype control for independent per-channel processing (double-tracking use case: a stereo
/// input whose two channels carry genuinely different signals — e.g. a REAPER parent bus summing
/// two panned, double-tracked guitar takes — rather than one mono source merely widened by the IR).
///
/// **Off by default, and this default is load-bearing, not cosmetic.** With this at "Linked" every
/// existing FR-CHAIN-050 guarantee holds unchanged (Gate/Nam mono-core, Trim's downmix establishes
/// the identical-channel invariant) — this descriptor changes nothing about the shipped product
/// until a caller actively turns it on. "Independent" is a genuine, deliberate departure from
/// FR-CHAIN-050's text ("the engine core shall process a single channel"): Gate and Nam each run a
/// full, separate instance per physical channel instead of mono-core-then-duplicate, and Trim skips
/// its downmix in favour of per-channel gain/DC-block/metering (the same shape `eq.rs` already
/// uses). Only meaningful when the chain has more than one channel; a `Mono` chain ignores it.
///
/// Not yet reconciled with FR-CHAIN-050's own text or priority — this is this branch's working
/// prototype of the idea, not a ratified decision. See `stages::gate`/`stages::nam`/`stages::trim`
/// in `namir-engine` for where it is actually read.
pub const INDEPENDENT_CHANNELS: ParamDescriptor = ParamDescriptor::new(
    "global.independent_channels",
    "Stereo Channels",
    Unit::None,
    ParamKind::Stepped {
        values: &["Linked", "Independent"],
        default_index: StepIndex(0),
    },
    ValueFormat::Named,
    SmoothingCategory::Stepped,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_have_distinct_keys() {
        assert_ne!(GLOBAL_BYPASS.key, OUTPUT_CEILING_DB.key);
        assert_ne!(GLOBAL_BYPASS.key, INDEPENDENT_CHANNELS.key);
        assert_ne!(OUTPUT_CEILING_DB.key, INDEPENDENT_CHANNELS.key);
    }

    #[test]
    fn independent_channels_default_is_linked() {
        let ParamKind::Stepped { default_index, .. } = INDEPENDENT_CHANNELS.kind else {
            panic!("expected Stepped");
        };
        assert_eq!(default_index, StepIndex(0));
    }

    #[test]
    fn output_ceiling_default_is_zero_dbfs_per_fr_chain_090() {
        let ParamKind::Continuous { default, .. } = OUTPUT_CEILING_DB.kind else {
            panic!("expected Continuous");
        };
        assert_eq!(default, 0.0);
    }

    #[test]
    fn global_bypass_default_is_off() {
        let ParamKind::Stepped { default_index, .. } = GLOBAL_BYPASS.kind else {
            panic!("expected Stepped");
        };
        assert_eq!(default_index, StepIndex(0));
    }
}
