# FR-CLAP-060 manual test: the host's own bypass button, sample-accurate and click-free

**Requirement (literal):** the plugin shall implement host-driven bypass such that the host's
bypass is sample-accurate and click-free, equivalent to FR-CHAIN-030.
*Verify: I.*

## What's mechanically true today

D-10.4 (this session's own prerequisite decision) made `global.bypass` an ordinary
`namir_params::REGISTRY` entry rather than a side channel only Rust code could reach.
`crates/namir-clap/src/params_ext.rs`'s `param_info` marks that one descriptor's CLAP
`ParamInfo.flags` with `ParamInfoFlags::IS_BYPASS` (plus `IS_STEPPED`, `IS_AUTOMATABLE`) —
`clack_extensions::params`'s own module doc comment states this flag is *"used to merge the
plugin and host bypass button"*. This is CLAP's actual mechanism for host-driven bypass: not a
separate extension, a flagged entry in the ordinary `params` extension. Once flagged, a host's own
bypass control sends a normal `CLAP_EVENT_PARAM_VALUE` event on `global.bypass`'s id, which
reaches `Chain::apply`/`Chain::set_global_bypass` through the identical path every other automated
parameter uses (`AudioEngine::apply_param_direct`, `crates/namir-clap/src/audio.rs`) — proven by
unit test: `params_ext::tests::global_bypass_param_info_carries_the_is_bypass_flag` asserts the
flag is present with `min_value == 0.0`/`max_value == 1.0`, and
`namir-engine::engine::tests::apply_param_direct_takes_effect_on_the_next_process_call_like_a_ring_delivered_change`
proves a direct-applied change converges to the same engine state a ring-delivered one does.

**The click-free mechanism itself is `Chain`'s own existing behaviour, unmodified by this round:**
FR-CHAIN-030's "applying only the latency compensation needed for sample alignment" is
`Chain::process`'s bypass path — `prepare_crosscutting_bypass_delays_by_declared_latency_for_
sample_alignment` (`crates/namir-engine/src/chain.rs`) proves the bypass path delays the dry
signal by the chain's declared latency before switching to it, so a toggle does not introduce a
sample-alignment discontinuity between the wet and dry paths. **Read literally, this is an
instantaneous switch between two paths at a block boundary, not an explicit anti-click crossfade**
— "click-free" rests on the two paths being sample-aligned in time, not on a fade between them;
this is `namir-engine`'s existing, previously-scoped design, unchanged by D-10.4 (which only
changed *how the value reaches* `set_global_bypass` — via `Chain::apply`'s ordinary `ParamChange`
routing now, rather than a bespoke `Command::SetGlobalBypass` — not what `set_global_bypass` or
the bypass path themselves do). Whether an instantaneous, sample-aligned switch is audibly
click-free in practice (rather than merely non-discontinuous in sample *position*) is exactly the
kind of thing this manual script exists to confirm by ear.

## Why this needs a real host and can't be fully automated

`clap-validator`'s `param-fuzz-*` tests set `global.bypass` (among every other parameter) to
random values via automation and assert no NaN/Inf/crash — genuine coverage of "the flag is wired
and doesn't break", but not of "a specific host's own dedicated bypass button (not a generic
automation lane) correctly maps to this parameter and the audible transition is click-free to a
human ear", which is what FR-CLAP-060 actually asks to be verified.

## Script

1. Load Namir in Reaper on a track with a continuous audio signal (a sustained note or a sine
   test tone) passing through a loaded model with an audible effect (distortion/EQ change).
2. Use Reaper's **own** per-plugin bypass button (the small power icon on the FX chain row, not a
   generic automation lane) to toggle bypass on and off several times while audio is playing.
   Confirm:
   - The transition is audibly click-free (no pop or discontinuity) in both directions.
   - Toggling **on** routes input straight to output (the model's effect disappears).
   - Toggling **off** restores the model's effect.
3. Automate `global.bypass` via Reaper's own automation lane (not the dedicated bypass button) and
   confirm it produces the identical audible behaviour — proving the flagged parameter and the
   host's dedicated control are the same underlying value, not two independent paths.

## Executed run (this session)

**Result: NOT EXECUTED** (requires a real host and audible confirmation). This agent session
has no way to
play audio through a host or listen for a click — see
`docs/manual-tests/fr-ui-010-standalone-window-renders.md`'s identical limitation note. What *is*
verified this session: the `IS_BYPASS` flag is present and correctly shaped (unit test, passing),
`clap-validator`'s full parameter-fuzzing suite exercised `global.bypass` among all 29 `REGISTRY`
entries without any NaN/Inf/crash (32 passed, 0 failed), and the underlying click-free bypass
behaviour is unchanged, previously-verified `namir-engine` logic this round does not touch. The
one genuinely new, unverified-by-this-session claim is "Reaper's own dedicated bypass button maps
to this parameter and sounds click-free to a human" — ready to run per the script above.
