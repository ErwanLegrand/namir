//! S-5: Namir's DSP chain under wasm32 + Web Audio. See ../README.md.
pub mod harness;

#[cfg(target_arch = "wasm32")]
mod wasm_abi;
