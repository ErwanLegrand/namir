# S-2 — Partitioned convolution cost curve

Spike per `docs/02-architecture.md` §19. Answers "what partition schedule minimises worst-case
per-block cost for IRs of 0.1-10 s at block sizes 32-2048 and rates 44.1-192 kHz?" (informs
D-9.6) and produces the IR-stage share of NFR-PERF-010 (OQ-2). Results are written up in
`docs/02-architecture.md` §19; this file is the reproduction record.

## What's here

- `src/lib.rs` — `build_schedule` turns (IR length, host block size, growth factor, max
  partition) into D-9.4's non-uniform schedule: the first partition equals the block size and is
  processed in the time domain (zero latency), every later partition is FFT-based (`realfft`)
  and grows geometrically. `PartitionedConvolver` runs it; `direct_convolve` is the D-9.5
  reference. See the module doc for the causality derivation (why growing by exactly
  `growth_factor` partitions per size level, not a fixed count, is what keeps every partition
  computable before its output is due) and a recorded scope note on the ring-buffer sizing used
  here (fine for an offline measurement tool, not how the shipping engine should allocate).
- `src/bin/verify.rs` — the D-9.5 correctness gate: `PartitionedConvolver` vs. `direct_convolve`
  across delta / delayed-delta / decaying-noise IRs (D-9.5's own fixture list), 5 block sizes, 8
  IR lengths, 4 schedules including the uniform degenerate case — 480 cases total.
- `src/bin/sweep.rs` — the comparative cost-curve grid: 7 candidate schedules x 8 IR lengths x 7
  block sizes (32-2048), block count calibrated to a wall-clock budget per combo (see its module
  doc for why — a naive direct head partition is O(block_size^2), and 2048-sample blocks make
  D-2.2's flat >=100,000-block rule take hours across a ~400-combo grid). Output is comparative,
  not the certified figure.
- `src/bin/bench.rs` — the D-2.2-rigor confirmatory benchmark for one (IR length, block size,
  schedule) combination, used only for the handful of points `sweep.rs` flags as worst-case. Its
  own module doc records a second, more interesting deviation from D-2.2's flat block count: the
  worst case here is *periodic*, not rare (every same-size partition fires in lockstep — see key
  finding below), so a much smaller sample size than 100,000 gives an equally or more reliable
  percentile, and this binary computes exactly how many blocks that requires per combination
  rather than assuming 100,000 is either necessary or sufficient.
- Fixtures (`fixtures::delta`, `delayed_delta`, `decaying_noise`) are generated per D-19.1, not
  captured — convolution cost is a function of IR *length*, not tap values, so a synthetic IR is
  exactly as informative as a captured one here.

## Reproducing

```bash
cd spikes/s2-ir-convolution
cargo test --release              # 3 unit tests: schedule causality, full IR coverage, one
                                   # end-to-end correctness check
cargo build --release
./target/release/verify            # D-9.5: 480 cases against the direct-convolution reference
./target/release/sweep > sweep.csv 2> sweep_summary.txt   # ~5 minutes; comparative grid + pick
./target/release/bench <ir_len_samples> <block_size> <growth_factor> <max_partition>
   # e.g.: ./target/release/bench 96000 64 2 8192   (NFR-PERF-010's own literal condition)
```

## Key facts and findings

**D-9.5 verification: PASS, 480/480 cases, worst error -119.91 dB** (limit -100 dB, itself well
past any audible threshold and consistent with plain f32 FFT round-trip noise, not a structural
bug) — across delta, delayed-delta and decaying-noise IRs, block sizes 32-2048, and four
schedules including the uniform degenerate case. The partitioning arithmetic is correct.

**Sample rate decouples from the cost sweep.** The engine has no notion of Hz — cost in raw time
depends only on IR length and block size in *samples*. Sample rate only rescales the block
*period* used to turn a raw nanosecond figure into a D-2.1 percentage. `sweep.rs` therefore
varies IR length directly in samples (chosen to span what 0.1-10 s at 44.1-192 kHz actually
produces, including both boundary extremes: 4,410 samples for 0.1 s@44.1 kHz, 1,920,000 for
10 s@192 kHz) and `bench.rs` re-derives the percentage at all four rates from one raw
measurement. This cut the grid by 4x with no loss of coverage.

**Uniform partitioning is measurably the worst choice, confirming D-9.4's rationale
empirically rather than just arithmetically.** Across the full grid, uniform partitioning's
worst observed block cost was ~44-48 ms — several times any of the non-uniform candidates.

**Key finding, not anticipated going in: same-size partitions fire in lockstep.** Every FFT
partition starts accumulating input at the stream's t=0, independent of its own tap offset into
the IR. Two partitions of the same nominal size therefore always complete their input window —
and trigger their FFT — on the *same* block, forever. For a long IR under a schedule whose
`max_partition` caps growth, every partition at that ceiling size (there can be dozens for a
multi-second IR) is one such same-size group, so their entire combined FFT cost lands on one
recurring block rather than being spread out. This is the dominant driver of worst-case cost —
far more than the precise choice of `growth_factor` or `max_partition` within any reasonable
range once the same-size-group sizes get large. Growth factors above 2 make it *worse* (more
partitions per size level means more of them piling onto the same block); `growth_factor <= 2`
consistently ties-or-beats the alternatives tested (3, 4, 8).

**Consequence: the small-block end of the required matrix cannot be brought into budget by
schedule tuning alone, and this is not an edge case — it reproduces at FR-IR-050's own Must
minimum.** At a 32-sample block against a 2-second IR (48 kHz, the minimum FR-IR-050 requires
Namir to accept) the periodic same-tier pileup alone costs on the order of 90-400% of the block's
*entire* period at 44.1-192 kHz, **tested across `max_partition` values from 256 to 32,768 with
no material improvement at any of them** — this was checked directly, not assumed. At larger
IRs (up to the D-9.7 10 s ceiling) and block sizes up to 128-256, it gets far worse — thousands
of percent over budget. Only at the large-block end (1024-2048 samples) does the picture become
merely "over budget by a factor of 2-4" rather than catastrophic, because a bigger block gives
each same-tier pileup more time to hide in.

**This is a real gap in the naive (synchronous, non-staggered) non-uniform scheme implemented
here, not a flaw in D-9.4's decision to use non-uniform partitioning at all — uniform is still
strictly worse at every point tested.** The standard fix, not implemented in this spike, is to
stagger same-size partitions' trigger phases (or equivalently, chunk/amortize each large FFT's
computation across several block calls instead of computing it synchronously in one) so that a
size-P group's total cost is spread across roughly P/block_size blocks instead of landing on one.
Recorded as required follow-up work before 1.0 (R-8, architecture §22), the IR-stage analogue of
R-4's NAM-vectorization gap from S-1.

## Default schedule (D-9.6) and the NFR-PERF-010 IR-stage share (OQ-2)

**Decision: `growth_factor = 2`, `max_partition = 8192` samples.** Among the candidates
clustered near the achievable optimum (`max_partition` 4096-32768 at `growth_factor` 2 or 4, all
within ~15% of each other at both the NFR-PERF-010 canonical condition and the worst grid point),
8192 was marginally best or tied-best at NFR-PERF-010's own literal test condition and carries
the smallest FFT working-set memory of the close contenders.

Measured at the chosen default, single-core-pinned, per D-2.1/D-2.2 (block counts adapted per
`bench.rs`'s documented deviation — see above):

| Condition | p99.9 | max |
|---|---|---|
| NFR-PERF-010's own condition (48 kHz, 64-sample block, 2 s IR) | **56%** of one core | **94%** of one core |
| Worst grid point (2048-sample block, 10 s IR, 192 kHz) | 254% of one core | 259% of one core |
| FR-IR-050 floor at the smallest block (32-sample block, 2 s IR, 48 kHz) | 99% of one core | 193% of one core |

(Figures carry ordinary wall-clock-benchmark run-to-run variance of a few percentage points —
the input and IR are seeded/deterministic, only measured timing jitters — the orders of
magnitude above are stable across repeated runs.)

**The IR stage alone, at NFR-PERF-010's own literal condition, already consumes roughly 2-4x
the entire 25% engine budget** (56-94% of one core), before adding NAM's own measured 41% (S-1),
gate or EQ. **The 25% NFR-PERF-010 placeholder is retained, not loosened** — matching S-1's
precedent: OQ-2 exists to establish the real numbers, not to move the target to match an
unoptimized reference implementation. Closing this gap needs both R-4 (NAM SIMD, from S-1) and
R-8 (IR-stage phase-staggering, from this spike) before 1.0.
