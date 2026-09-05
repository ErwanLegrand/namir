#!/usr/bin/env bash
# Builds the two wasm artefacts. The third configuration is the same simd128
# binary run under a V8 flag, not a separate build.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p web/build

# NOTE: an *environment* RUSTFLAGS replaces `.cargo/config.toml`'s
# target.wasm32-unknown-unknown.rustflags wholesale rather than appending to it, so the
# `--import-undefined` link arg that resolves `env.now_us` has to be repeated here or the
# simd128 link fails with an undefined symbol.
LINK="-C link-arg=--import-undefined"

echo "== scalar =="
cargo build --release --target wasm32-unknown-unknown --lib
cp target/wasm32-unknown-unknown/release/s5_wasm_web_audio.wasm web/build/scalar.wasm

echo "== simd128 =="
# rustfft's wasm_simd has no runtime detection: this artefact TRAPS on a runtime
# without simd128. That is intended -- it is a different artefact, not a fallback.
RUSTFLAGS="$LINK -C target-feature=+simd128" \
  cargo build --release --target wasm32-unknown-unknown --lib --features wasm-simd
cp target/wasm32-unknown-unknown/release/s5_wasm_web_audio.wasm web/build/simd128.wasm

ls -la web/build/
