# FR-CLAP-040 manual test: latency reporting and host-observed restart on change

**Requirement (literal):** the plugin shall report its total latency in samples and shall notify
the host whenever that latency changes, including as a result of a model change under FR-NAM-050.
*Verify: I.*

## What's mechanically true today

`crates/namir-clap/src/latency_ext.rs`'s `PluginLatencyImpl::get` reads
`SharedInner::latency_samples` — a plain atomic the audio thread stores into every block
(`crates/namir-clap/src/audio.rs`'s `NamirAudioProcessor::publish_latency`, from
`AudioEngine::chain().latency_samples()`). When that value differs from what the audio thread
itself last saw, it flags `latency_dirty` and calls `host.shared().request_callback()`
(`clack_extensions::latency` documents this as thread-safe, so calling it from the audio thread is
sound). The host then calls `on_main_thread` (`crates/namir-clap/src/main_thread.rs`), which:

- calls `host.shared().request_restart()` if the plugin is currently active, or
- calls `HostLatency::changed()` directly if it is not.

**Why a restart, not a direct notification, while active:** `clack_extensions::latency::
HostLatency::changed`'s own doc comment states *"The latency is allowed to change only during the
`activate` callback... If the plugin is active, you should request a restart first."* This is a
CLAP protocol requirement, not a Namir engine limitation — `AudioEngine::activate` (this crate's
`NamirAudioProcessor::activate`) reads and reports the fresh latency, then calls
`main_thread.notify_latency_changed()` unconditionally, which is exactly the window the CLAP
specification allows the announcement in.

**Consequence, stated honestly:** a live model swap that happens to change the engine's
resampler-induced latency (D-9.2 — only when the new model's declared sample rate differs from
the session rate) triggers a brief deactivate/reactivate cycle in a compliant host. This is *not*
in conflict with FR-NAM-070's glitch-free crossfade requirement — the crossfade itself still has
no audible discontinuity — but a host-driven restart cycle around it is an audible gap (silence,
briefly, while the host tears down and rebuilds the audio graph) that Namir has no way to avoid
within the CLAP 1.x protocol as specified. The common case (a model whose declared rate matches
the session rate — no resampler engaged) never changes latency and never triggers this at all.

## Why this needs a real host and can't be fully automated

`clap-validator` exercises latency-extension *presence* and *value consistency* under fuzzing
(none of its tests specifically drive a live latency change mid-session and assert the host
receives a `request_restart` followed by a correct post-restart `get()` value — that is precisely
the sequencing this manual test targets). A synthetic host stub inside this crate's own test suite
could assert the *call sequence* (which `crates/namir-clap/src/main_thread.rs` does not currently
have unit tests for, since it needs a real `HostMainThreadHandle`/`HostLatency` pair clack does not
expose a lightweight mock for) but not the *host's own observed behaviour* — whether Reaper/Bitwig
actually deactivates, reactivates, and re-queries latency correctly is host-side behaviour outside
this process.

## Script

1. Load Namir in Reaper. Load a NAM model whose declared sample rate differs from the project's
   session rate (forces the D-9.2 resampler to engage, giving it nonzero latency). Confirm Reaper's
   PDC (plugin delay compensation) indicator updates and the track stays in sync with others.
2. While the model with resampler latency is loaded and the plugin is processing, load a
   *different* model whose declared rate also differs from the session rate but by a different
   resampling ratio (so the reported latency actually changes). Confirm:
   - Reaper briefly stops and restarts playback (the restart cycle this document explains above),
     rather than continuing with a stale PDC value.
   - After the restart, PDC is correct for the new model (check with a transient/click test signal
     and Reaper's own latency-compensation visualisation, or by ear for an obviously mistimed
     transient).
3. Load a model whose declared rate *matches* the session rate (no resampler, zero latency) and
   confirm no restart occurs when swapping between two such models — only the resampler-engaged
   case above should trigger one.

## Executed run (this session)

**Not executed.** This agent session has no way to load a real DAW project, set a session sample
rate deliberately mismatched from a model's declared rate, or observe a host's PDC indicator — see
`docs/manual-tests/fr-ui-010-standalone-window-renders.md`'s identical limitation note. What *is*
verified automatically: `clap-validator`'s full suite (32 passed, 0 failed, 0 warnings) confirms
the `latency` extension is correctly wired and produces consistent values under its own fuzzing,
and `crates/namir-clap/src/audio.rs`'s doc comment records the sequencing this script is meant to
observe for real.
