//! M2's six product `Stage`/`StagePrep` pairs (Trim/Gate/Nam/Ir/Eq/Out) and their fixed-chain
//! assembly (FR-CHAIN-010), per `03-implementation-roadmap.md` §6.

pub mod eq;
pub mod gate;
pub mod nam;
pub mod out;
pub mod trim;
// `ir` is added once `namir-ir` exists (this crate's own second implementation wave).

use crate::chain::Chain;
use crate::prepare::{PrepareContext, PrepareError};

/// Builds the fixed 1.0 signal chain. **Not yet wired** — assembled once every one of the six
/// stages exists; see this module's own tracking in `03-implementation-roadmap.md` §6.
///
/// Runtime order is **gate before trim**, not FR-CHAIN-010's literal prose order ("input trim →
/// noise gate → ..."): `02-architecture.md` D-9.8 records this as a deliberate usability decision
/// (the gate's threshold should reference the interface's actual noise floor, not move when the
/// user adjusts trim), explicitly flagged there for review rather than an oversight, and
/// `03-implementation-roadmap.md` §6 directs M2 to build the actual chain that way:
/// `gate → trim → nam → ir → eq → out`.
pub fn build_default_chain(_ctx: &PrepareContext) -> Result<Chain, PrepareError> {
    todo!("wired once every one of the six stages exists")
}
