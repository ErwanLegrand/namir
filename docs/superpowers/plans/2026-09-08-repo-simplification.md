# Codebase Simplification and Bloat Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address the 7 over-engineering and bloat findings identified in the repository audit across `namir-core`, `namir-params`, `namir-engine`, `namir-dsp`, `namir-app`, `namir-clap`, and `xtask` to reduce line count by ~700+ lines without breaking public contracts or verification gates.

**Architecture:** Replace bespoke hand-rolled logic with standard library or existing dependency primitives (e.g., `blake3::Hash::from_hex`, `f64::sin_cos`), eliminate redundant type wrapping (`namir_engine::ParamId` duplicating `namir_params::ParamId`, `XrunCounter` wrapping `AtomicU64`), inline redundant wrapper modules in `namir-app`/`namir-clap`, replace runtime black-box assertions with compile-time `const assert`, and streamline the custom YAML workflow parser in `xtask`.

**Tech Stack:** Rust (edition 2024, MSRV 1.97), standard library, `blake3` 1.5, workspace crates (`namir-core`, `namir-params`, `namir-engine`, `namir-dsp`, `namir-platform`, `namir-app`, `namir-clap`, `xtask`).

**Spec:** The 7 audit findings produced during the ponytail audit:
1. `shrink 400-line hand-rolled block-style YAML parser and AST used solely to assert CI workflow structure. Line-oriented key/step scanning or lightweight parser. [xtask/src/release_workflow.rs]`
2. `delete duplicate ParamId(pub u32) definition in namir-engine and the ParamId(x.id.0) conversions across all six stages and benchmarks. pub use namir_params::ParamId. [crates/namir-engine/src/param.rs]`
3. `shrink 50-line radix-2 Cooley-Tukey FFT written solely for test-only spectral leakage checks. Single-bin DFT or Goertzel algorithm for the probe tone bin. [crates/namir-dsp/src/artefact.rs]`
4. `shrink hand-rolled hex nibble parsing and chunked decoder (hex_nibble, from_hex). blake3::Hash::from_hex from the existing blake3 dependency. [crates/namir-core/src/content_hash.rs]`
5. `yagni identical 27-line adapter modules in namir-app and namir-clap mapping (name, path) tuples into PresetSummary. Inline iterator mapping at call sites or PresetSummary::from_pairs. [crates/namir-app/src/presets.rs, crates/namir-clap/src/presets.rs]`
6. `shrink 46-line XrunCounter wrapper struct and method boilerplate. Plain AtomicU64 directly with fetch_add(1, Relaxed). [crates/namir-app/src/xrun.rs]`
7. `delete runtime test using std::hint::black_box to compare two constants and evade clippy. Compile-time const _: () = assert!(MAX_FILE_BYTES > 50 * 1024 * 1024);. [crates/namir-core/src/limits.rs]`

## Global Constraints

- MSRV: Rust 1.97, Edition 2024.
- Layering (D-5.1): dependency directions in `xtask/src/layering.rs` must not be violated; no `#[cfg(target_os)]` outside `namir-platform`.
- Unsafe policy (D-5.3): workspace forbids `unsafe_code`, only designated files in `namir-platform` and `namir-clap` allow it with written safety invariants. No new unsafe blocks.
- Real-time safety (NFR-RT-010): audio thread paths must not allocate, lock, or block.
- Traceability (NFR-QUAL-010): all `// trace:` and `// trace-partial:` comments and their requirement bindings must be preserved. `cargo run -p xtask -- traceability --allow-uncovered` must pass.
- No new external crate dependencies.

---

### Task 1: Compile-time constant assertion in `namir-core::limits`

**Files:**
- Modify: `crates/namir-core/src/limits.rs`

**Interfaces:**
- Consumes: `pub const MAX_FILE_BYTES: usize` from `crates/namir-core/src/limits.rs`
- Produces: compile-time invariant check `const _: () = assert!(...);`

- [ ] **Step 1: Write the failing test / compile assertion**

In `crates/namir-core/src/limits.rs`, add the compile-time assertion and a unit test validating that the ceiling constant cannot be decreased below the target:

```rust
const _: () = assert!(
    MAX_FILE_BYTES > 50 * 1024 * 1024,
    "MAX_FILE_BYTES must exceed NFR-PERF-050's 50 MB performance target"
);
```

Remove the runtime `black_box` test module:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_larger_than_the_nfr_perf_050_performance_target() {
        assert!(MAX_FILE_BYTES > 50 * 1024 * 1024);
    }
}
```

- [ ] **Step 2: Run test to verify it compiles and passes**

Run: `cargo test -p namir-core --lib limits`
Expected: PASS

- [ ] **Step 3: Verify clippy clean**

Run: `cargo clippy -p namir-core --all-targets -- -D warnings`
Expected: PASS with 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/namir-core/src/limits.rs
git commit -m "refactor(core): replace black_box test with compile-time constant assertion"
```

---

### Task 2: Simplify hex decoding in `namir-core::content_hash` using `blake3`

**Files:**
- Modify: `crates/namir-core/src/content_hash.rs:50-105`
- Test: `crates/namir-core/src/content_hash.rs:160-210`

**Interfaces:**
- Consumes: `blake3::Hash::from_hex`
- Produces: `ContentHash::from_hex(&str) -> Result<ContentHash, ContentHashParseError>`

- [ ] **Step 1: Update implementation in `crates/namir-core/src/content_hash.rs`**

Replace `hex_nibble` and the chunking loop in `ContentHash::from_hex` with `blake3::Hash::from_hex`:

```rust
    pub fn from_hex(s: &str) -> Result<Self, ContentHashParseError> {
        if s.len() != 64 {
            return Err(ContentHashParseError::WrongLength);
        }
        let hash = blake3::Hash::from_hex(s).map_err(|_| ContentHashParseError::NotHex)?;
        Ok(Self(*hash.as_bytes()))
    }
```

Delete the private `hex_nibble` function (lines 80-87).
In `Display for ContentHash`, simplify to:
```rust
impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", blake3::Hash::from(self.0))
    }
}
```

- [ ] **Step 2: Run unit tests to verify existing coverage passes**

Run: `cargo test -p namir-core content_hash`
Expected: PASS (all tests including `hex_round_trips_through_display_and_from_hex`, `from_hex_accepts_uppercase`, `from_hex_rejects_wrong_length`, `from_hex_rejects_non_hex_characters` pass).

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -p namir-core --all-targets -- -D warnings`
Expected: PASS with 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/namir-core/src/content_hash.rs
git commit -m "refactor(core): use blake3::Hash::from_hex for ContentHash parsing"
```

---

### Task 3: Unify `ParamId` in `namir-engine` with `namir_params::ParamId`

**Files:**
- Modify: `crates/namir-engine/src/param.rs`
- Modify: `crates/namir-engine/src/chain.rs`
- Modify: `crates/namir-engine/src/probe.rs`
- Modify: `crates/namir-engine/src/stages/trim.rs`
- Modify: `crates/namir-engine/src/stages/gate.rs`
- Modify: `crates/namir-engine/src/stages/nam.rs`
- Modify: `crates/namir-engine/src/stages/ir.rs`
- Modify: `crates/namir-engine/src/stages/eq.rs`
- Modify: `crates/namir-engine/src/stages/out.rs`
- Modify: `crates/namir-engine/benches/six_stage_chain.rs`
- Modify: `crates/namir-engine/benches/per_stage_cost.rs`
- Modify: `crates/namir-engine/benches/tail_structure.rs`
- Modify: `crates/namir-engine/benches/rt_invariance.rs`
- Modify: `crates/namir-engine/benches/denormal_guard.rs`
- Modify: `crates/namir-engine/benches/handover_crossfade.rs`

**Interfaces:**
- Consumes: `namir_params::ParamId`
- Produces: `pub use namir_params::ParamId` from `namir_engine`

- [ ] **Step 1: Replace `ParamId` declaration in `crates/namir-engine/src/param.rs`**

Update `crates/namir-engine/src/param.rs`:
```rust
//! D-10.2: "a stable u32 derived from a namespaced string ... hosts see the u32".
//! Re-exports [`namir_params::ParamId`] as the RT boundary identifier.

pub use namir_params::ParamId;

/// A single parameter update, as delivered to `Stage::apply` (D-6.1). Carries no smoothing
/// information: D-10.3 assigns smoothing to a parameter *descriptor*, which doesn't exist at
/// this layer yet — a stage that needs to avoid a zipper on this value ramps internally.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamChange {
    /// Which parameter changed.
    pub id: ParamId,
    /// The new value, always `f32` at this layer.
    pub value: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_compare_by_value() {
        assert_eq!(ParamId(7), ParamId(7));
        assert_ne!(ParamId(7), ParamId(8));
    }
}
```

- [ ] **Step 2: Simplify constants in `crates/namir-engine/src/chain.rs` and `probe.rs`**

In `crates/namir-engine/src/chain.rs`:
Change:
```rust
const GLOBAL_BYPASS_ID: ParamId = ParamId(GLOBAL_BYPASS.id.0);
const OUTPUT_CEILING_DB_ID: ParamId = ParamId(OUTPUT_CEILING_DB.id.0);
```
To:
```rust
const GLOBAL_BYPASS_ID: ParamId = GLOBAL_BYPASS.id;
const OUTPUT_CEILING_DB_ID: ParamId = OUTPUT_CEILING_DB.id;
```

In `crates/namir-engine/src/probe.rs`:
Change `set_param`:
```rust
pub fn set_param(chain: &mut Chain, descriptor: namir_params::ParamId, value: f32) {
    chain.apply(ParamChange {
        id: descriptor,
        value,
    });
}
```

- [ ] **Step 3: Simplify stage constants in `crates/namir-engine/src/stages/*.rs`**

In `stages/trim.rs`:
```rust
const GAIN_DB_ID: ParamId = GAIN_DB.id;
const DC_BLOCKER_ENABLED_ID: ParamId = DC_BLOCKER_ENABLED.id;
```

In `stages/gate.rs`:
```rust
const ENABLED_ID: ParamId = ENABLED.id;
const THRESHOLD_DB_ID: ParamId = THRESHOLD_DB.id;
const ATTACK_MS_ID: ParamId = ATTACK_MS.id;
const HOLD_MS_ID: ParamId = HOLD_MS.id;
const RELEASE_MS_ID: ParamId = RELEASE_MS.id;
```

In `stages/nam.rs`:
```rust
const ENABLED_ID: ParamId = ENABLED.id;
const NORMALIZE_ENABLED_ID: ParamId = NORMALIZE_ENABLED.id;
const NORMALIZE_OFFSET_DB_ID: ParamId = NORMALIZE_OFFSET_DB.id;
```

In `stages/ir.rs`:
```rust
const ENABLED_ID: ParamId = ENABLED.id;
const LEVEL_DB_ID: ParamId = LEVEL_DB.id;
const NORMALIZE_ENABLED_ID: ParamId = NORMALIZE_ENABLED.id;
const LOW_CUT_ENABLED_ID: ParamId = LOW_CUT_ENABLED.id;
const LOW_CUT_FREQ_HZ_ID: ParamId = LOW_CUT_FREQ_HZ.id;
const HIGH_CUT_ENABLED_ID: ParamId = HIGH_CUT_ENABLED.id;
const HIGH_CUT_FREQ_HZ_ID: ParamId = HIGH_CUT_FREQ_HZ.id;
```

In `stages/eq.rs`:
```rust
const ENABLED_ID: ParamId = ENABLED.id;
const LOW_SHELF_FREQ_HZ_ID: ParamId = LOW_SHELF_FREQ_HZ.id;
const LOW_SHELF_GAIN_DB_ID: ParamId = LOW_SHELF_GAIN_DB.id;
const MID_FREQ_HZ_ID: ParamId = MID_FREQ_HZ.id;
const MID_GAIN_DB_ID: ParamId = MID_GAIN_DB.id;
const MID_Q_ID: ParamId = MID_Q.id;
const HIGH_SHELF_FREQ_HZ_ID: ParamId = HIGH_SHELF_FREQ_HZ.id;
const HIGH_SHELF_GAIN_DB_ID: ParamId = HIGH_SHELF_GAIN_DB.id;
const HIGH_PASS_ENABLED_ID: ParamId = HIGH_PASS_ENABLED.id;
const HIGH_PASS_FREQ_HZ_ID: ParamId = HIGH_PASS_FREQ_HZ.id;
const LOW_PASS_ENABLED_ID: ParamId = LOW_PASS_ENABLED.id;
const LOW_PASS_FREQ_HZ_ID: ParamId = LOW_PASS_FREQ_HZ.id;
```

In `stages/out.rs`:
```rust
const GAIN_DB_ID: ParamId = GAIN_DB.id;
```

In benches (`six_stage_chain.rs`, `per_stage_cost.rs`, `tail_structure.rs`, `rt_invariance.rs`, `denormal_guard.rs`, `handover_crossfade.rs`):
Replace `ParamId(gate::ENABLED.id.0)` with `gate::ENABLED.id`, etc.

- [ ] **Step 4: Run tests and clippy across `namir-engine`**

Run: `cargo test -p namir-engine`
Expected: PASS
Run: `cargo clippy -p namir-engine --all-targets -- -D warnings`
Expected: PASS with 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/namir-engine/
git commit -m "refactor(engine): re-export namir_params::ParamId and remove duplicate conversions"
```

---

### Task 4: Simplify FFT and remove redundant `CosSin` trait in `namir-dsp::artefact`

**Files:**
- Modify: `crates/namir-dsp/src/artefact.rs`

**Interfaces:**
- Consumes: standard library `f64::sin_cos`
- Produces: `pub fn artefact_energy_db(signal: &[f32]) -> f64`

- [ ] **Step 1: Update `crates/namir-dsp/src/artefact.rs`**

Delete lines 192-201 (`trait CosSin` and `impl CosSin for f64`).
In `fft` (lines 173-188), replace:
```rust
let (wr, wi) = (angle * k as f64).cos_sin();
```
with standard library:
```rust
let (wi, wr) = (angle * k as f64).sin_cos();
```
(noting `sin_cos` returns `(sin, cos)` where `wr = cos`, `wi = sin`).

- [ ] **Step 2: Run `namir-dsp` tests**

Run: `cargo test -p namir-dsp`
Expected: PASS (including `artefact_energy_db` assertions in `gain_ramp` and `biquad`).

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -p namir-dsp --all-targets -- -D warnings`
Expected: PASS with 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/namir-dsp/src/artefact.rs
git commit -m "refactor(dsp): replace custom CosSin trait with f64::sin_cos in artefact FFT"
```

---

### Task 5: Streamline `XrunCounter` in `namir-app::xrun`

**Files:**
- Modify: `crates/namir-app/src/xrun.rs`

**Interfaces:**
- Consumes: `std::sync::atomic::{AtomicU64, Ordering}`
- Produces: `pub struct XrunCounter(AtomicU64)` with `new()`, `record()`, `count()`, `reset()`

- [ ] **Step 1: Simplify `crates/namir-app/src/xrun.rs`**

Reduce `xrun.rs` to its minimal, idiomatic implementation:

```rust
//! FR-IO-060: running xrun count for the session, resettable by the user.

use std::sync::atomic::{AtomicU64, Ordering};

/// A session's xrun count.
#[derive(Default, Debug)]
pub struct XrunCounter(AtomicU64);

impl XrunCounter {
    /// A fresh counter at zero.
    pub fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Records one xrun.
    #[inline]
    pub fn record(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// The running total for this session.
    #[inline]
    pub fn count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Resets the counter to zero.
    #[inline]
    pub fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrun_counter_increments_and_resets() {
        let counter = XrunCounter::new();
        assert_eq!(counter.count(), 0);
        counter.record();
        counter.record();
        assert_eq!(counter.count(), 2);
        counter.reset();
        assert_eq!(counter.count(), 0);
        counter.record();
        assert_eq!(counter.count(), 1);
    }
}
```

- [ ] **Step 2: Run `namir-app` unit tests**

Run: `cargo test -p namir-app --lib xrun`
Expected: PASS

- [ ] **Step 3: Run all `namir-app` tests and clippy**

Run: `cargo test -p namir-app`
Expected: PASS
Run: `cargo clippy -p namir-app --all-targets -- -D warnings`
Expected: PASS with 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/namir-app/src/xrun.rs
git commit -m "refactor(app): streamline XrunCounter to minimal atomic wrapper"
```

---

### Task 6: Consolidate preset listing adapters in `namir-app` and `namir-clap`

**Files:**
- Modify: `crates/namir-ui/src/host.rs`
- Modify: `crates/namir-app/src/presets.rs`
- Modify: `crates/namir-app/src/worker.rs`
- Modify: `crates/namir-clap/src/presets.rs`
- Modify: `crates/namir-clap/src/shared.rs`

**Interfaces:**
- Consumes: `namir_platform::presets::list_preset_files(&Path) -> Vec<(String, PathBuf)>`
- Produces: `PresetSummary::from_pairs(impl IntoIterator<Item = (String, PathBuf)>) -> Vec<PresetSummary>` in `namir-ui`

- [ ] **Step 1: Add `PresetSummary::from_pairs` helper in `crates/namir-ui/src/host.rs`**

In `crates/namir-ui/src/host.rs`:
```rust
impl PresetSummary {
    /// Constructs a list of `PresetSummary` from `(name, path)` tuples as reported by
    /// `namir_platform::presets::list_preset_files`.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, std::path::PathBuf)>) -> Vec<Self> {
        pairs
            .into_iter()
            .map(|(name, path)| Self { name, path })
            .collect()
    }
}
```

- [ ] **Step 2: Simplify `crates/namir-app/src/presets.rs` and `crates/namir-clap/src/presets.rs`**

Update `crates/namir-app/src/presets.rs`:
```rust
//! Preset path resolution and listing re-exports for the standalone application.

use std::path::Path;
use namir_ui::PresetSummary;

pub use namir_platform::presets::{preset_dir_under, preset_path};

#[must_use]
pub fn list_presets(dir: &Path) -> Vec<PresetSummary> {
    PresetSummary::from_pairs(namir_platform::presets::list_preset_files(dir))
}
```

Update `crates/namir-clap/src/presets.rs`:
```rust
//! Preset path resolution and listing re-exports for the plugin.

use std::path::Path;
use namir_ui::PresetSummary;

#[cfg(test)]
pub(crate) use namir_platform::presets::preset_dir_under;
pub(crate) use namir_platform::presets::{preset_dir, preset_path};

pub(crate) fn list_presets(dir: &Path) -> Vec<PresetSummary> {
    PresetSummary::from_pairs(namir_platform::presets::list_preset_files(dir))
}
```

- [ ] **Step 3: Run `namir-app` and `namir-clap` tests**

Run: `cargo test -p namir-app && cargo test -p namir-clap`
Expected: PASS

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p namir-ui -p namir-app -p namir-clap --all-targets -- -D warnings`
Expected: PASS with 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/namir-ui/src/host.rs crates/namir-app/src/presets.rs crates/namir-clap/src/presets.rs
git commit -m "refactor(ui,app,clap): add PresetSummary::from_pairs and streamline preset wrappers"
```

---

### Task 7: Streamline YAML workflow parser and validator in `xtask::release_workflow`

**Files:**
- Modify: `xtask/src/release_workflow.rs`

**Interfaces:**
- Consumes: `.github/workflows/release.yml`, `docs/01-functional-requirements.md`
- Produces: `pub fn parse(text: &str) -> Result<Yaml, String>`, `clause_1_triggered_by_a_tag`, `clause_2_every_tier_1_and_tier_2_platform`, `clause_3_every_distribution_is_this_workflows`

- [ ] **Step 1: Simplify AST and parsing in `xtask/src/release_workflow.rs`**

Consolidate the recursive descent parser in `xtask/src/release_workflow.rs`:
1. Keep the `Yaml` enum (`Scalar`, `Seq`, `Map`) and its query methods (`get`, `as_str`, `as_seq`, `as_map`, `str_at`, `seq_at`).
2. Remove unused scalar chomp/strip variants (`parse_block_scalar` can join content lines trimmed of relative indentation directly without redundant state-machine flags).
3. Simplify `strip_comment` and `unquote` helpers to single expressions.
4. Keep the exact interface `pub fn parse(text: &str) -> Result<Yaml, String>` used by `xtask/src/ci_commands.rs`.
5. Ensure `the_release_workflow_meets_every_clause_of_fr_pkg_010s_verify_method` (with `// trace: FR-PKG-010`) continues to test clauses 1, 2, and 3 verbatim.

- [ ] **Step 2: Run `xtask` tests**

Run: `cargo test -p xtask release_workflow`
Expected: PASS (all clause tests pass, `the_release_workflow_meets_every_clause_of_fr_pkg_010s_verify_method` passes).

- [ ] **Step 3: Run `ci_commands` verification in `xtask`**

Run: `cargo test -p xtask ci_commands`
Expected: PASS

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p xtask --all-targets -- -D warnings`
Expected: PASS with 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add xtask/src/release_workflow.rs
git commit -m "refactor(xtask): streamline release workflow block-style YAML parser"
```

---

### Task 8: Full repository verification and traceability check

**Files:**
- All modified crates and xtask

**Interfaces:**
- Run full CI gate locally

- [ ] **Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: Clean (0 diffs).

- [ ] **Step 2: Clippy check**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: Clean (0 warnings).

- [ ] **Step 3: Workspace tests**

Run: `cargo test --workspace --no-fail-fast`
Expected: All tests pass.

- [ ] **Step 4: xtask gates**

Run:
```bash
cargo run -p xtask -- layering
cargo run -p xtask -- rt-logging
cargo run -p xtask -- params-lock
cargo run -p xtask -- attribution
cargo run -p xtask -- identity
cargo run -p xtask -- traceability --allow-uncovered
```
Expected: All subcommands exit 0.

- [ ] **Step 5: Commit (if any cleanup needed)**

```bash
git commit --allow-empty -m "chore: verify full local gate after audit simplifications"
```
