# AGENTS.md

This file provides guidance to coding agents when working with code in this repository.

## What this is

Namir is a real-time guitar/bass amplifier and cabinet simulator: it applies a Neural Amp Modeler
(NAM) profile to an instrument signal, then a cabinet impulse response (IR), with a noise gate
before the amp and a tone EQ after the cabinet. It ships as two products from one codebase — a
standalone native app (`namir-app`, audio I/O via `cpal`) and a CLAP audio plugin (`namir-clap`,
via `clack`) — sharing one GUI (`namir-ui`, egui-based). Rust workspace, edition 2024. Primary
platform is Windows 11 x86-64; Linux/macOS are secondary; mobile is a prospective, not-yet-shipped
target the architecture must not preclude.

## Governing documents — read before making a non-trivial change

Three documents in `docs/` form a strict hierarchy; where they conflict, the earlier one wins:

- **`docs/01-functional-requirements.md`** (FRS) — *what* Namir must do. Every requirement has a
  stable ID (`FR-*`, `NFR-*`) and a `Verify:` method (S/M/I/B — static, manual, integration test,
  benchmark).
- **`docs/02-architecture.md`** — *how*. Numbered Decisions (`D-x.y`) with Rationale/Consequence,
  a risk register (§22), a dependency register (§17), and a changelog (§24). Decisions are never
  silently rewritten — a later milestone that changes one appends a
  `*Consequence (added M<n>)*` note at the original decision, in place, rather than editing the
  original text.
- **`docs/03-implementation-roadmap.md`** — *order*. Milestones M0–M8, each with Deliverables and
  Acceptance criteria. §14 has a live Must-requirement status snapshot table (Done/Partial/Not
  started per FRS section); §15 tracks open decisions still to make.

Also load-bearing: `docs/04-state-and-preset-format.md` (the `.namirpreset`/state JSON format) and
`docs/manual-tests/*.md` — one file per requirement that can't be verified by an automated test
(needs real hardware, a real host, or a human at a screen), each recording exactly what was and
wasn't executed and why. When you can't automate a check, write one of these instead of silently
skipping the requirement.

**How a milestone's own section gets updated when the work actually happens:** append a dated
`### M<n> status` / `### M<n> close-out` subsection after the original Deliverables/Acceptance
text — never rewrite the original. If reality differs from what was predicted (a benchmark that
was expected to pass fails, a Must requirement turns out to be only partially closable), record
that honestly in the new subsection rather than editing the old claim. This project's history
(see `docs/02-architecture.md`'s changelog, and M3's multi-session saga in the roadmap) treats a
retracted or corrected finding as normal and worth keeping on the record, not something to clean
up after the fact.

## Common commands

```bash
# Full local gate (also what CI runs, on Windows/Linux/macOS)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- layering       # D-5.1 dependency-graph + platform-cfg lint
cargo run -p xtask -- params-lock    # checks params.lock is in sync with namir-params::REGISTRY
cargo deny check                     # NFR-LIC-020 license/advisory gate

# Single crate / single test
cargo test -p namir-engine
cargo test -p namir-engine some_test_name -- --nocapture

# Regenerate params.lock after changing namir-params::REGISTRY
cargo test -p namir-params --lib -- --ignored generate_params_lock

# xtask preset tooling (state/preset document round-trip)
cargo run -p xtask -- preset [output-path]
cargo run -p xtask -- preset --verify <path>

# Benchmarks (release only; see "Benchmark methodology" below before trusting a number)
cargo build --release --bench <name> -p <crate>
```

**Pre-commit hook** (opt in once per clone: `git config core.hooksPath .githooks`) runs `cargo
fmt --all -- --check` + `cargo check --workspace --all-targets` only — fast checks, deliberately
not clippy or the test suite (see the hook's own comment: slow checks on the commit path make
`--no-verify` tempting, which is worse than clippy catching it one step later in CI). Don't bypass
it with `--no-verify`; fix what it flags.

## Workspace layering (D-5.1) — a hard, mechanically-enforced dependency graph

`cargo run -p xtask -- layering` checks every crate's dependency edges against this table and
rejects `#[cfg(target_os)]`/`#[cfg(windows)]`/`#[cfg(unix)]` outside `namir-platform`. Dev-dependencies
are exempt from the edge check (e.g. `namir-nam`'s dev-dependency on `namir-fixtures`).

| Crate | Responsibility | May depend on |
|---|---|---|
| `namir-core` | Shared vocabulary types (sample rate, channel layout, dB/linear, content hash, error catalogue). No logic. | — |
| `namir-params` | Parameter identity, ranges, formatting, smoothing declarations. | core |
| `namir-dsp` | Primitive DSP: biquads, gate detector, meters, gain ramps, DC blocker. | core |
| `namir-nam` | `.nam` parsing, validation, inference, model preparation. | core, dsp |
| `namir-ir` | IR file decoding, resampling, partitioned convolution. | core, dsp |
| `namir-engine` | `Stage` trait, the six-stage chain, RT-safe scheduling, resource handover, telemetry. | core, params, dsp, nam, ir |
| `namir-state` | Preset/plugin-state document, versioning, file-reference resolution. | core, params |
| `namir-library` | Library index, scanning, hashing, search, persistence. | core, nam, ir, state |
| `namir-platform` | Filesystem/config paths, CLAP install paths, thread priority, denormal guard. **The only crate with `#[cfg(target_os)]`.** | core |
| `namir-worker` | Off-thread orchestration: load requests, resource cache, scan jobs, handover protocol, library bootstrap. | everything above, incl. platform |
| `namir-ui` | egui-based interface. A pure view+intent layer — see below. | core, params, library, state |
| `namir-app` | Standalone application: real `cpal` audio I/O, window, settings. | everything |
| `namir-clap` | CLAP plugin adapter. **The only crate that names CLAP.** | everything except `namir-app` |

`namir-fixtures` (dev/test tooling, generated-fixture generator) and `xtask` are exempt from the
table entirely. `spikes/` is throwaway, pins its own `Cargo.lock`, and is excluded from the
workspace — do not port code from there without re-reviewing it; it's a proof of feasibility, not
production code.

## `unsafe` code — confined to two crates

Workspace-wide `unsafe_code = "forbid"` (not just `deny` — chosen specifically so no crate can
locally `#![allow(unsafe_code)]` its way around it). Only `namir-platform` and `namir-clap` (plus a
possible future SIMD kernel module) declare their own `[lints.rust] unsafe_code = "deny"` to opt
back in, confined to one module each, with a written `// SAFETY:` argument on every block and a
module-level doc comment giving the fuller argument (see `namir-platform/src/denormal.rs` or
`namir-clap/src/gui.rs` for the house style). Any new `unsafe` block outside those two crates is a
bug, not a style choice — the type system enforces this, so it will fail to compile rather than
merely fail review.

## `namir-ui`'s host seam — the key cross-cutting design to know before touching UI or either product shell

`namir-ui` cannot depend on `namir-engine`/`namir-worker`/`namir-platform` (see the table above),
so it never owns a live `Chain` or `Instance`. It's a pure view+intent layer: every frame, a
caller-supplied `UiHost` implementation produces a `UiSnapshot` (current param values, meters,
loaded model/IR names, library state, notices) and receives `UiIntent`s (`SetParam`,
`LoadLibraryEntry`, etc.) back. `namir-app` and `namir-clap` each implement `UiHost` independently,
bridging to their own real `Instance`/`Chain` — see `crates/namir-ui/src/host.rs`,
`crates/namir-app/src/host.rs`, `crates/namir-clap/src/ui_host.rs`.

Both product shells also share their default library-bootstrap logic through
`namir_worker::library::LibraryService::open_default`/`open_at` — not their own copies. Duplicating
this once already caused a real bug (`namir-clap` opened with zero scan roots, and a scan against
zero roots silently erased the shared index on a rescan); don't reintroduce a second copy.

## Real-time safety (NFR-RT-010) and the audio/worker thread split

The audio thread (`AudioEngine::process`, and the direct-apply paths `apply_param_direct`/
`reset_direct` it also uses) must never allocate, lock anything a non-RT thread can hold, do file/
network I/O, or run an unbounded loop. Anything that can block or allocate — file reads, `.nam`/IR
parsing, library scanning, state save/load — runs on `namir-worker`'s pool and crosses to the audio
thread only via `namir-engine`'s SPSC command ring and D-8.1's four-step handover protocol
(prepare → offer → crossfade → retire). `assert_no_alloc`-based harnesses (`rt_harness` in
`namir-engine`, duplicated per-crate where needed since a `#[global_allocator]` is one-per-binary)
enforce this in tests. If you're adding anything the audio thread touches, check whether it
allocates or blocks before wiring it in — this is the single most-checked property in code review
across this project's history.

## Benchmark methodology (D-2.1/D-2.2/D-2.4) — don't trust a single run

This machine is not a quiet benchmarking rig; a shared desktop's own concurrent load (this session's
own agent processes included) has repeatedly produced 2-3x swings in raw `p99.9` readings that
turned out to be measurement contamination, not real regressions (see D-2.4 and the M3 close-out
in the roadmap for the full investigation). Before trusting a benchmark number:

- Benchmarks pin away from CPU 0/2 by default (`NAMIR_PIN_CORE`, defaults to core 4) — those cores
  absorb GPU-driver ISR interrupts on the reference machine.
- Take ≥5 repetitions, not one.
- Where a benchmark reports a contamination-immune estimator alongside raw `p99.9` (e.g.
  `tail_structure.rs`'s per-residue-minimum, `denormal_guard.rs`'s guard-vs-nominal comparison),
  discard any run whose raw `p99.9` substantially exceeds its own estimator — that run was
  contaminated, not evidence of a regression.
- The **certified** figure for a Must-requirement gate (NFR-PERF-010, NFR-RT-030, etc.) is only
  ever the one measured on `docs/02-architecture.md` §2's pinned reference machine (AMD Ryzen 9
  5950X, 64 GB RAM, Windows 11 Pro build 26200) under these conditions — a sandbox or dev-machine
  number is informational only, never the number recorded as closing a requirement.

## Testing philosophy

- **D-19.1: all test fixtures are generated, never captured.** `namir-fixtures` generates `.nam`
  models and IRs from a seed; there is no captured/licensed audio anywhere in the test suite. This
  is a licensing decision (AQ-4), not a convenience — don't add a real captured `.nam`/IR file to
  the repo or its tests.
- Correctness for numeric code is cross-implementation parity (an independent from-scratch
  reference in `namir-fixtures`) or direct analytic reference (`namir-ir`'s direct-convolution
  reference for the partitioned convolver), not "sounds right."
- Real-world files (e.g. actual `.nam` exports) can differ from generated fixtures in ways the
  fixtures never exercise — a parser bug found post-M6 (a metadata field set to JSON `null` rather
  than omitted) came from exactly this gap. When investigating a report against real user files,
  check whether the relevant fixture generator actually produces that shape before assuming the
  parser is exhaustively tested.
- `cargo-fuzz` targets exist per parser that reads untrusted bytes (`crates/namir-nam/fuzz`,
  `crates/namir-state/fuzz`), seeded from `namir-fixtures`' mutation corpus.

## Commit conventions

Keep commit messages short and to the point: list the changes, explain *why* only when intent
isn't obvious from the diff, don't go into implementation detail the diff already shows. Milestone
work has historically used a `M<n>: <summary>` subject line, and some feature work used a red/green
TDD pairing (`M<n> (red): ...` immediately followed by `M<n> (green): ...`) — neither is mandatory,
but match the surrounding history's granularity rather than one giant commit per milestone.
