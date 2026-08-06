//! M2's six product-stage descriptor sets (`03-implementation-roadmap.md` §6), one module per
//! stage per D-10.1's "declared in one place per stage". Aggregated into [`crate::REGISTRY`].

pub mod eq;
pub mod gate;
pub mod ir;
pub mod nam;
pub mod out;
pub mod trim;
