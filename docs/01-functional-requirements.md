# Namir — Functional Requirements Specification

| | |
|---|---|
| **Project** | Namir — a host for Neural Amp Modeler profiles and impulse responses |
| **Document** | 01 — Functional Requirements Specification (FRS) |
| **Version** | 0.1 (draft) |
| **Date** | 2026-08-03 |
| **Status** | Draft — awaiting review |
| **Author / Copyright holder** | Erwan Patrick Legrand |
| **Licence** | MIT OR Apache-2.0 |

---

## 1. Purpose and scope

### 1.1 Purpose

This document defines **what** Namir must do. It does not define **how**. Implementation
technology, module decomposition, threading model and data structures are the subject of
document `02-architecture.md` and must not be pre-empted here.

Every requirement in Section 5 and 6 is written to be independently verifiable, because
the next SDLC phase writes the test suite directly from this document. A requirement that
cannot be turned into a test is a defect in this document and shall be rewritten.

### 1.2 Product summary

Namir is a real-time guitar/bass amplifier and cabinet simulator. It applies a
**Neural Amp Modeler (NAM)** profile to an instrument signal and then a **cabinet impulse
response (IR)**, with a noise gate ahead of the amp and a tone-shaping EQ after the
cabinet. It ships in two forms from one codebase: a **standalone native application** that
talks to the system audio hardware, and a **CLAP plugin** loaded by a host DAW.

### 1.3 Scope of version 1.0

In scope: the fixed signal chain of Section 5.1, model and IR loading, preset and state
management, parameter automation, the standalone audio I/O layer, the CLAP plugin
integration, and a graphical user interface shared by both product forms.

Out of scope for 1.0: see Section 7.

### 1.4 Target platforms

| Tier | Platform | Commitment for 1.0 |
|---|---|---|
| Primary | Windows 11 (x86-64) | Fully supported, release binaries, CI-tested |
| Secondary | Linux (x86-64), macOS (aarch64) | Supported, CI-tested, best-effort release binaries |
| Prospective | Android, iOS/iPadOS | No 1.0 build. The design must not preclude them (NFR-PORT-030) |

### 1.5 Requirement conventions

Requirements are identified as `FR-<AREA>-<NNN>` (functional) or `NFR-<AREA>-<NNN>`
(non-functional). Numbers are assigned in tens so later requirements can be inserted
without renumbering. Identifiers are permanent: a withdrawn requirement is marked
*Withdrawn* and its number is never reused.

Priority uses MoSCoW: **Must** (1.0 does not ship without it), **Should** (1.0 ships
without it only under an explicitly recorded decision), **Could** (desirable, first to be
cut), **Won't** (explicitly excluded from 1.0, recorded to prevent scope drift).

The keywords *shall*, *shall not*, *should* and *may* are used per RFC 2119.

*Verify* names the intended verification method: **U** unit test, **I** integration test,
**G** golden-reference comparison against recorded audio, **B** benchmark with a numeric
threshold, **S** static analysis or build-time check, **M** manual test against a written
script, **Process** enforced by review and evidenced by commit order rather than by any
artifact a build can inspect (found missing from this legend at M7, while building
NFR-QUAL-010's mechanical traceability check against every code actually used below --
NFR-QUAL-020 is this document's one user of it).

*Consequence (added M9, 2026-08-08)* — The parenthetical immediately after a requirement's
identifier is the **sole** priority marker, and the only one any tool or audit reads:
`xtask/src/traceability.rs:81`'s `extract_must_id` takes the text between a line's leading `**` and
the next `**`, splits it at the ` (`, and keeps the requirement only when the tag is exactly `Must`.
Bold **Must**/**Should** words appearing inside a requirement's *body* are scope qualifiers on part
of that requirement, not priority tags on it: FR-NAM-020 is a Must that makes `Linear`/`ConvNet` a
Should, FR-IO-020 a Must that makes ASIO a Should, FR-IO-030 a Must that makes PipeWire/JACK a
Should. Each counts as **one** Must and is adjudicated whole — a Must whose Should-scoped clause is
unbuilt is not thereby Partial (FR-IO-020 is Partial for its exclusive-mode clause, not for ASIO).
Recorded while deriving the authoritative per-section Must counts for `03-implementation-roadmap.md`
§14's M9a re-audit, under `02-architecture.md` D-23.2, where several people adjudicate the same
document and the reading has to be the same one. On that count: this document declares **130** Must
requirements, 31 Should and 3 Could — 164 in all, every one written in the `**ID (Priority)** — `
form above. A requirement written in any other shape is invisible to both `xtask traceability` and
that count, and silently so: a missing `*Verify:*` line is a hard parse error
(`xtask/src/traceability.rs:62-71`), but a declaration line the parser does not recognise as a
declaration at all is simply never seen.

---

## 2. Definitions

| Term | Meaning |
|---|---|
| **NAM** | Neural Amp Modeler. Here, specifically the `.nam` file format: a JSON document containing a network architecture identifier, its configuration, its weights and descriptive metadata. |
| **NAM model** | One `.nam` file and the inference network it describes. Always single-channel. |
| **IR** | Impulse response. A short audio recording of a linear system (here, a speaker cabinet and microphone) that is applied by convolution. |
| **Engine** | The real-time audio processing core: everything between the audio input buffer and the audio output buffer. |
| **Audio thread** | The thread (or callback context) on which the engine runs. Owned by the OS audio driver or by the plugin host; never owned by Namir. |
| **Block** | One buffer of audio samples delivered to the engine in a single audio callback. |
| **Engine sample rate** | The sample rate at which the audio thread delivers audio. Set by the driver or the host, never chosen by Namir. |
| **Model sample rate** | The sample rate a NAM model was trained at, declared in the `.nam` file. Typically 48 kHz. |
| **Preset** | A named, serialisable capture of every user-settable value in the signal chain, including references to the loaded model and IR. |
| **Plugin state** | The serialised form of the engine that a CLAP host saves inside a project file. A superset of a preset (see FR-STATE-060). |
| **Library** | The user's collection of `.nam` and IR files on disk, together with whatever index Namir maintains over it. |
| **Real-time safe** | Executing with a hard upper bound on time, with no memory allocation, no lock that a non-real-time thread can hold, no file or network I/O, and no unbounded loop. |
| **Bypass** | A state in which a stage passes its input to its output unchanged, other than any latency compensation required to keep the chain sample-aligned. |

---

## 3. Users and use cases

### 3.1 User classes

| Class | Description | Consequences for requirements |
|---|---|---|
| **U1 — Practising player** | Uses the standalone app with an audio interface and headphones. Wants low latency and to switch tones quickly. Not necessarily technical. | Drives FR-IO-*, FR-UI-*, and the latency budget NFR-PERF-010. |
| **U2 — Recording musician** | Uses the CLAP plugin inside a DAW. Expects automation, project recall, and a plugin that never blocks the host. | Drives FR-CLAP-*, FR-PARAM-*, FR-STATE-*. |
| **U3 — Tone builder** | Owns hundreds of `.nam` and IR files. Auditions, compares, tags and organises them. | Drives FR-LIB-*. |
| **U4 — Packager / distributor** | Builds Namir for a platform or a distribution. Needs a reproducible, dependency-clear, licence-clean build. | Drives NFR-LIC-*, NFR-BUILD-*. |

### 3.2 Primary use cases

- **UC-1 Practise**: launch the standalone app, choose an audio device, load a model and an IR, play.
- **UC-2 Track**: instantiate the CLAP plugin on a guitar track, dial a tone, record, close the project, reopen it a week later on the same machine and get the identical tone.
- **UC-3 Collaborate**: send a project to another person who has the same model and IR files; the project opens with the correct tone (FR-STATE-070).
- **UC-4 Audition**: browse a library of models, hear each one applied to the live input without dropouts while switching (FR-NAM-070).
- **UC-5 Recall**: save a dialled-in tone as a preset and load it in either product form (FR-STATE-030).

---

## 4. Product configurations

**FR-CFG-010 (Must)** — Namir shall be distributed in two configurations built from a single
shared engine implementation: a **standalone application** and a **CLAP plugin**.
*Verify:* S — both build targets are produced by CI from one workspace.

**FR-CFG-020 (Must)** — The engine shall produce bit-identical output in both configurations
for identical input, identical parameter values and identical block sizes.
*Verify:* G — the same golden test vectors are run through both configurations.

**FR-CFG-030 (Must)** — The standalone application shall not require the CLAP plugin to be
installed, and the CLAP plugin shall not require the standalone application to be installed.
*Verify:* I — each is installed alone into a clean environment and exercised.

**FR-CFG-040 (Should)** — The standalone application shall be usable without an installer,
by running a single executable from any directory, storing its settings under the platform's
per-user configuration directory.
*Verify:* M.
*Consequence (added M8-planning, 2026-08-08)* — FR-PKG-010's installer does not conflict with this
requirement. What is required here is that the standalone application **can** run without an
installer, not that no installer exists. FR-PKG-050's plain archive is the artifact that keeps this
requirement literally true once installers ship; if that archive were ever dropped, this
requirement would fail with it.

---

## 5. Functional requirements

### 5.1 Signal chain (CHAIN)

**FR-CHAIN-010 (Must)** — The engine shall implement exactly this chain, in this order:

```
input → input trim → noise gate → NAM → IR → EQ → output level → output
```

The order shall be fixed and not user-reorderable in 1.0.
*Verify:* I — measure stage interaction against a specified probe signal.

*Consequence (added M9a, 2026-08-09 — amending this requirement's order to the order the product
ships)* — **The order in the block above is superseded.** The engine shall implement exactly this
chain, in this order:

```
input → noise gate → input trim → NAM → IR → EQ → output level → output
```

The gate precedes the trim; nothing else moves. `02-architecture.md` **D-9.8** is the reason and
gives it in one sentence: the gate detector runs on the signal *before* input trim so its threshold
is referenced to the interface's actual noise floor and does not move when the user adjusts trim. A
threshold that shifts every time the player touches a gain control is not a threshold, and the
superseded order buys nothing else in this document in exchange. Both of this document's other
statements of placement stay literally true as written — Section 1.2's "a noise gate ahead of the
amp" and FR-GATE-010's "ahead of the NAM stage" — because the gate remains ahead of the NAM stage
either way; the trim is the only thing that changes side.

**This amendment changes the document, not the product.** `build_default_chain`
(`crates/namir-engine/src/stages/mod.rs:47-67`) assembles `gate → trim → nam → ir → eq → out`, and
has done since M2 (2026-08-06) — precisely, M2's first commit `7941577` declared the order in the
function's doc comment while its body was still `todo!("wired once every one of the six stages
exists")`, and the assembly itself landed later in the same milestone. It was directed there by
`03-implementation-roadmap.md` §6's own deliverable text ("placed *before* Trim in the actual chain
per D-9.8"), and the three modules concerned each say so in their doc comments
(`crates/namir-engine/src/stages/mod.rs:31-36`, `stages/gate.rs:5-6`, `stages/trim.rs:5-7`). D-9.8's
own `*Rationale:*` ends "flagged for review"; that review was never performed, so the divergence
stood from M2 to M9a as a decision recorded against a requirement it contradicted, in a document
hierarchy where this one wins. M9a's set-quantification sweep is what surfaced it, and surfaced it
as a requirement-versus-code conflict rather than as a coverage gap. Which way to resolve it was
genuinely open. It is resolved in favour of the shipped order: the product is right and this
paragraph is the document catching up.

**What this amendment does not do.** It does not touch the `*Verify:*` line above, which stands
unchanged and unexecuted. No probe signal has ever been put through this chain to measure stage
interaction: the one test annotated for this requirement fills every channel with 0.0 and asserts
0.0 out with nothing loaded, which cannot distinguish any ordering of any stages from an empty
chain. That is a separate and real gap, it is unaffected by which order this requirement mandates,
and it is what this requirement's `// trace-partial:` names after this amendment.

**One interaction, recorded rather than repaired.** Gate-before-trim has a consequence for
FR-CHAIN-060's Stereo row that neither D-9.8 nor anything since states, found while checking this
amendment against the code. Trim owns the chain's only cross-channel mixing, including that row's
"2 ch summed" at −6 dB per term (`crates/namir-engine/src/stages/trim.rs:145-156`). Gate, now
upstream of it, detects on channel 0 alone and copies its gated result over every other channel
(`crates/namir-engine/src/stages/gate.rs:163-172`) to establish the identical-channel invariant
FR-CHAIN-050 lets every later stage assume. The right channel is therefore discarded before Trim can
sum it, and Trim's sum evaluates L·g + L·g at g = −6 dB — the left channel, at unity. Shipped
behaviour in the Stereo configuration is thus FR-CHAIN-070's default (left channel), not
FR-CHAIN-060's table default (both channels summed). Stated here as an observed fact of the product
as of M9a, verified at the two sites cited and at no point decided by this amendment: FR-CHAIN-060
and FR-CHAIN-070 are unchanged, and neither requirement's disposition follows from anything above.

*Consequence (added M9a, 2026-08-09 — correcting the paragraph immediately above)* — **That
paragraph overstates its own finding, and overstates it in the direction of severity.** It ends by
naming shipped Stereo behaviour "FR-CHAIN-070's default (left channel), not FR-CHAIN-060's table
default (both channels summed)", which reads as FR-CHAIN-060 — a Must — being unmet. FR-CHAIN-060 is
met. Its Stereo row reads `2 ch summed or L-only (FR-CHAIN-070)`: two permitted inputs to the mono
core, joined by an *or*, with no default named between them. "FR-CHAIN-060's table default" names
something the table does not contain. Feeding the core the left channel alone is the second of the
two options that row allows, so the behaviour observed above **satisfies** FR-CHAIN-060 rather than
deviating from it. The same mistaken phrasing sits in
`crates/namir-engine/src/stages/trim.rs:19-21`, which calls the sum "FR-CHAIN-060's default" — the
likely source of it, recorded here and not repaired.

**What cannot be met is FR-CHAIN-070, and only FR-CHAIN-070.** Its clause is that the user "shall be
able to **choose** whether the core is fed the left channel, the right channel, or the sum of both
at −6 dB". Nothing offers that choice: `params.lock`'s twenty-nine live parameters are the gate,
trim, NAM, IR, EQ, output-level and global sets, and none of them selects a stereo source. The
requirement fails on its own *choose* clause before the chain order is reached — a fixed left
channel would leave it unmet even if the summing path were the one that ran.

**And FR-CHAIN-070 is a Should, not a Must** (§1.5 above: the parenthetical after the identifier is
the sole priority marker). It is therefore in no `03-implementation-roadmap.md` §14 row — that table
counts Musts, 8 of them for this section's 9 requirements — and no §14 verdict, no denominator and
no M8 exit-gate item follows from any of this. The finding is real and stands. It is one priority
level below what the paragraph above makes it read as: a Should left open, not a Must left broken.

**The mechanism, restated with the condition the paragraph above drops.** Gate copies each channel's
pre-gate signal aside (`crates/namir-engine/src/stages/gate.rs:159-161`), gates channel 0,
duplicates that result over every other channel (`:163-172`), then crossfades duplicate against
pre-gate copy per channel by its own bypass mix (`:174-192`). With the gate **on** — the descriptor
default, `gate.enabled` being step index 1, "On" (`crates/namir-params/src/stages/gate.rs:8-16`,
read at `stages/gate.rs:85-88`) — that mix sits at 1.0 and the right channel is gone before Trim's
−6 dB-per-term sum (`stages/trim.rs:145-156`) can use it, exactly as described above. With the gate
**off** the mix settles at 0.0, each channel's own pre-gate copy is restored, and Trim sums both
channels as written. So the sum is not unreachable, which "discarded before Trim can sum it" implies
by stating no condition; it is reachable only as a side effect of disabling an unrelated stage,
which is not something FR-CHAIN-070 can be met by offering.

**Unchanged by this note**, as by the paragraph it corrects: FR-CHAIN-060, FR-CHAIN-070 and every
`*Verify:*` line above stand exactly as written, and nothing here decides whether or how
FR-CHAIN-070's control is built. Verified at each site cited, as of M9a.

**FR-CHAIN-020 (Must)** — Each of the noise gate, NAM, IR and EQ stages shall be individually
bypassable without disturbing the other stages and without an audible click or discontinuity.
*Verify:* U per stage; I for click-freedom (see FR-PARAM-040).

**FR-CHAIN-030 (Must)** — The engine shall provide a global bypass that routes input to output
with unity gain, applying only the latency compensation needed for sample alignment.
*Verify:* U — null test: bypassed output minus delayed input is silence to within −120 dBFS.

**FR-CHAIN-040 (Must)** — A stage that has nothing loaded (no NAM model, no IR) shall behave as
if bypassed. It shall not mute the signal and shall not produce an error to the user on every
block.
*Verify:* U.

**FR-CHAIN-050 (Must)** — The engine core shall process a single channel. Channel
configurations shall be realised by the placement of the mono core within the surrounding
routing, per FR-CHAIN-060.
*Rationale:* NAM models are inherently monophonic; forcing stereo through the core would
double the cost for no benefit.
*Verify:* I.

**FR-CHAIN-060 (Must)** — The engine shall support these channel configurations:

| Configuration | Input | Core | IR stage | Output |
|---|---|---|---|---|
| Mono | 1 ch | mono | mono IR | 1 ch |
| Mono→stereo | 1 ch | mono | stereo IR, or dual mono IR | 2 ch |
| Stereo | 2 ch summed or L-only (FR-CHAIN-070) | mono | stereo | 2 ch |

*Verify:* I per configuration.

**FR-CHAIN-070 (Should)** — When a stereo input is presented, the user shall be able to choose
whether the core is fed the left channel, the right channel, or the sum of both at −6 dB.
The default shall be left channel.
*Verify:* U.

**FR-CHAIN-080 (Must)** — Every sample the engine emits shall be a finite number. If any stage
produces a NaN or an infinity, the engine shall replace the affected block with silence, set a
fault indicator, and continue processing subsequent blocks.
*Rationale:* A NaN escaping into a DAW's mix bus can silence an entire session and, on some
hardware, produce a damaging transient.
*Verify:* U — inject a NaN into each stage's state and assert output finiteness.

**FR-CHAIN-090 (Must)** — The engine shall not emit a sample whose magnitude exceeds a
configurable ceiling (default 0 dBFS) at the output stage.
*Verify:* U.

### 5.2 Input stage (IN)

**FR-IN-010 (Must)** — An input trim control shall be provided with a range of at least
−24 dB to +24 dB, default 0 dB, resolution no coarser than 0.1 dB.
*Verify:* U.

**FR-IN-020 (Must)** — An input level meter shall be provided, reporting peak and a
short-term average, with a peak-hold indicator that latches for at least 1 second.
*Verify:* U for the measurement; M for the display.

**FR-IN-030 (Must)** — A clip indicator shall latch when any input sample reaches or exceeds
0 dBFS, and shall be resettable by the user.
*Verify:* U.

**FR-IN-040 (Should)** — A DC-blocking high-pass filter, corner no higher than 20 Hz, shall be
applicable at the input and shall be enabled by default.
*Rationale:* Some audio interfaces present a DC offset that biases the NAM model's operating
point and audibly changes the tone.
*Verify:* U — measure attenuation at DC and at 100 Hz.

### 5.3 Noise gate (GATE)

**FR-GATE-010 (Must)** — A noise gate shall be provided ahead of the NAM stage with, at
minimum, these controls:

| Control | Range | Default |
|---|---|---|
| Threshold | −100 dBFS to 0 dBFS | −70 dBFS |
| Attack | 0.1 ms to 50 ms | 1 ms |
| Hold | 0 ms to 500 ms | 30 ms |
| Release | 1 ms to 2000 ms | 100 ms |
| Enabled | on / off | on |

*Verify:* U per control against a synthesised burst signal.

**FR-GATE-020 (Must)** — The gate shall apply hysteresis: the level at which the gate closes
shall be measurably below the level at which it opens, to prevent chatter on a signal hovering
at the threshold.
*Verify:* U — a signal decaying through the threshold shall produce exactly one close event.

**FR-GATE-030 (Must)** — Gate gain changes shall be applied with sample-accurate interpolation
within the block, not stepped at block boundaries.
*Verify:* U — measure the maximum sample-to-sample delta during a gate transition.

**FR-GATE-040 (Should)** — The gate's current gain reduction shall be exposed to the user
interface as a metered value.
*Verify:* U.

**FR-GATE-050 (Could)** — The gate's detector shall optionally operate on a high-pass-filtered
copy of the input so that low-frequency hum does not hold the gate open.
*Verify:* U.

### 5.4 NAM model stage (NAM)

**FR-NAM-010 (Must)** — Namir shall load NAM models from files conforming to the `.nam` JSON
format, identified by the `.nam` extension and validated by content, not by extension alone.
*Verify:* U with a corpus of valid files.

**FR-NAM-020 (Must)** — Namir shall support, at minimum, the `WaveNet` and `LSTM` architectures
as published in the NAM model format. Support for `Linear` and `ConvNet` is **Should**.
*Verify:* G — one model of each architecture compared against reference output.

**FR-NAM-030 (Must)** — For each supported architecture, the output of Namir's inference shall
match the reference NAM implementation to within an error whose RMS is at least 90 dB below
the RMS of the reference output, over a specified 10-second test signal containing clean,
transient and saturated material.
*Rationale:* This is the requirement that makes a from-scratch inference implementation
testable, and it is deliberately stated as behaviour rather than as an implementation choice.
The 90 dB figure is a placeholder to be confirmed by the architecture-phase spike
(OQ-2, Section 9).
*Verify:* G.

**FR-NAM-040 (Must)** — A model file that is malformed, truncated, of an unknown architecture,
or whose declared configuration is inconsistent with its weight count, shall be rejected with a
message naming the file and the specific reason. The currently loaded model shall remain
loaded and audio shall not be interrupted.
*Verify:* U with a corpus of deliberately corrupted files (fuzz corpus, NFR-QUAL-040).

**FR-NAM-050 (Must)** — When the model sample rate differs from the engine sample rate, Namir
shall resample the signal so that the model runs at its declared sample rate, and shall
resample the result back to the engine sample rate.
*Rationale:* A model run at the wrong rate is not merely detuned; its non-linearity is
evaluated at the wrong operating points and the tone is wrong in a way users misattribute to
the model.
*Verify:* I — a 48 kHz model driven at 44.1 kHz shall match, within the FR-NAM-030 tolerance,
the same model driven at 48 kHz with the input and output resampled offline.

**FR-NAM-060 (Must)** — The resampling of FR-NAM-050 shall have a stopband attenuation of at
least 100 dB and passband ripple no greater than 0.1 dB up to 20 kHz or the Nyquist frequency,
whichever is lower.
*Verify:* U — measure the frequency response of the resampler in isolation.

**FR-NAM-070 (Must)** — Loading a model shall not block, glitch or mute the audio thread. The
previously loaded model shall continue to process audio until the new model is fully prepared,
at which point the changeover shall occur at a block boundary with an equal-power crossfade of
no less than 5 ms and no more than 50 ms.
*Rationale:* This is UC-4. Auditioning a library is the single most common thing a user does
and it must be seamless.
*Verify:* I — swap models under a continuous sine input and assert no discontinuity exceeding a
stated threshold and no dropout.

**FR-NAM-080 (Must)** — Namir shall read and display the model's metadata where present:
name, author (`modeled_by`), gear make/model/type, tone type, and any free-text description.
*Verify:* U.

**FR-NAM-090 (Must)** — Namir shall apply the model's declared loudness normalisation so that
models of differing recorded loudness are perceived at comparable level when swapped. The user
shall be able to disable this normalisation and to offset it.
*Verify:* U — measure integrated loudness of two models with differing declared loudness driven
by the same input; the difference shall be within 1 LU with normalisation enabled.

**FR-NAM-100 (Should)** — Where a model declares input and output calibration levels in dBu,
Namir shall offer a calibrated mode in which the user states their interface's input
sensitivity and Namir drives the model at its intended operating level.
*Verify:* U.

**FR-NAM-110 (Must)** — Namir shall report the model stage's processing latency in samples, and
shall report zero if the architecture is causal and introduces none.
*Verify:* U — cross-correlate an impulse through the stage.

**FR-NAM-120 (Should)** — Namir shall expose the model's computational cost to the user as a
real-time factor or an equivalent indicator, measured rather than estimated, so that a user can
tell before committing whether a model will run on their machine.
*Verify:* B.

**FR-NAM-130 (Must)** — The NAM stage shall be usable with no model loaded, behaving per
FR-CHAIN-040.
*Verify:* U.

**FR-NAM-140 (Must)** — A model file whose declared architecture, or whose configuration within a
supported architecture, Namir does not support shall be rejected with an error that names the
unsupported feature. That error shall be a distinct catalogue entry (FR-ERR-020) from the one
reported for a malformed or truncated file under FR-NAM-040.
*Verify:* U — a file that is well-formed but unsupported and a file that is malformed shall yield
different error identifiers.

**FR-NAM-150 (Must)** — Namir shall load and run NAM Architecture 2 (A2) models in the A2-Full and
A2-Lite configurations, to the accuracy of FR-NAM-030.
*Verify:* U — cross-implementation parity against an independent reference implementation, per
NFR-QUAL-030.

### 5.5 Impulse response stage (IR)

**FR-IR-010 (Must)** — Namir shall load impulse responses from WAV files, supporting 16-bit
integer, 24-bit integer, 32-bit integer and 32-bit IEEE float sample formats, mono or stereo,
at any sample rate from 8 kHz to 192 kHz.
*Verify:* U with a corpus covering the matrix of formats.

**FR-IR-020 (Should)** — Namir shall additionally load AIFF and FLAC impulse responses.
*Verify:* U.

**FR-IR-030 (Must)** — An IR whose sample rate differs from the engine sample rate shall be
resampled to the engine sample rate on load, meeting the quality requirement of FR-NAM-060.
*Verify:* U.

**FR-IR-040 (Must)** — The IR stage shall add no latency beyond that inherent in the impulse
response itself. Convolution shall be zero-latency with respect to the engine's block boundary.
*Rationale:* Latency in the cabinet stage is unacceptable for live monitoring and would have to
be reported and compensated by the host, degrading the experience for every user.
*Verify:* U — impulse in, measure the sample index of first non-zero output.

**FR-IR-050 (Must)** — The IR stage shall accept impulse responses of at least 2 seconds at the
engine sample rate. Longer files shall be accepted and truncated, with the truncation reported
to the user, or processed in full — the choice shall be recorded in the architecture document,
not left to the implementation.
*Verify:* U.

**FR-IR-060 (Must)** — Loading an IR shall meet the same no-glitch, crossfaded changeover
requirement as FR-NAM-070.
*Verify:* I.

**FR-IR-070 (Must)** — The following per-IR controls shall be provided:

| Control | Range | Default |
|---|---|---|
| Level | −24 dB to +24 dB | 0 dB |
| Enabled | on / off | on |
| Low cut | off, or 20 Hz to 500 Hz | off |
| High cut | off, or 1 kHz to 20 kHz | off |

*Verify:* U per control.

**FR-IR-080 (Should)** — Two IR slots shall be provided with a blend control between them, so
that a user can mix two microphone positions. Both slots feed the same channel configuration
rules of FR-CHAIN-060.
*Verify:* U.

**FR-IR-090 (Should)** — An IR may be normalised on load to a stated target so that switching
IRs does not produce large level jumps. Normalisation shall be defeatable.
*Verify:* U.

**FR-IR-100 (Must)** — The IR stage shall be usable with no IR loaded, behaving per
FR-CHAIN-040.
*Verify:* U.

### 5.6 Equaliser (EQ)

**FR-EQ-010 (Must)** — A post-cabinet equaliser shall be provided with, at minimum:

| Band | Type | Frequency range | Gain range |
|---|---|---|---|
| Low | Shelf | 40 Hz – 500 Hz | ±15 dB |
| Mid | Peaking, adjustable Q (0.2 – 5.0) | 200 Hz – 5 kHz | ±15 dB |
| High | Shelf | 1 kHz – 12 kHz | ±15 dB |

Plus a defeatable high-pass and low-pass filter as in FR-IR-070.
*Verify:* U — measure the magnitude response against the analytic target within 0.1 dB.

**FR-EQ-020 (Must)** — The EQ shall be numerically stable across the full parameter range at
every supported sample rate, including at extreme gain and Q settings, and shall not
self-oscillate or produce denormal-driven CPU spikes.
*Verify:* U — parameter sweep across the full range at 44.1/48/88.2/96/176.4/192 kHz asserting
bounded output and finite state.

**FR-EQ-030 (Must)** — Changing any EQ parameter shall not produce a click or a zipper artefact.
*Verify:* U per FR-PARAM-040.

**FR-EQ-040 (Should)** — A frequency-response curve of the current EQ settings shall be
displayed to the user.
*Verify:* M.

### 5.7 Output stage (OUT)

**FR-OUT-010 (Must)** — An output level control shall be provided, range −60 dB to +12 dB,
default 0 dB, with −60 dB or below being exact silence.
*Verify:* U.

**FR-OUT-020 (Must)** — An output meter shall be provided with the same characteristics as
FR-IN-020, plus a latching clip indicator as FR-IN-030.
*Verify:* U.

**FR-OUT-030 (Should)** — A brickwall output limiter shall be available, defeatable, disabled
by default, with its gain reduction metered.
*Verify:* U.

### 5.8 Parameters and automation (PARAM)

**FR-PARAM-010 (Must)** — Every continuous control identified in Section 5 shall be exposed as
an automatable parameter with a stable identifier, a human-readable name, a unit, a minimum, a
maximum, a default and a value-to-text formatting rule.
*Verify:* U — enumerate parameters and assert the completeness of each descriptor.

**FR-PARAM-020 (Must)** — Parameter identifiers shall be stable across versions. A parameter
that is removed shall have its identifier retired permanently, never reassigned.
*Rationale:* Reassigning an identifier silently corrupts every saved project that used it.
*Verify:* S — a checked-in parameter manifest is diffed in CI; a changed or reused identifier
fails the build.

**FR-PARAM-030 (Must)** — Parameter changes shall be accepted from the user interface, from
CLAP host automation, and from preset loading, and shall converge to the same engine state
regardless of source.
*Verify:* I.

**FR-PARAM-040 (Must)** — Every gain-affecting parameter shall be smoothed such that a full-range
instantaneous change produces no output discontinuity greater than that of a 20 ms linear ramp.
Frequency-affecting parameters shall be smoothed or their coefficients interpolated to the same
audible standard.
*Verify:* U — assert maximum sample-to-sample delta and measure the artefact's spectral energy.

**FR-PARAM-050 (Must)** — Discrete choices (enabled/disabled, filter type, channel mode) shall be
exposed as stepped parameters with named values, not as continuous ranges.
*Verify:* U.

**FR-PARAM-060 (Should)** — Parameters shall declare a modulation/automation appropriateness
flag so that hosts can distinguish per-sample-automatable parameters from configuration-like
ones (e.g. channel mode).
*Verify:* U.

### 5.9 Presets and state (STATE)

**FR-STATE-010 (Must)** — The complete user-settable state of the engine shall be serialisable
to, and restorable from, a self-describing document containing a format version.
*Verify:* U — round-trip property test: serialise, restore, serialise again, assert equality.

**FR-STATE-020 (Must)** — Restoring a state document produced by any earlier released version of
Namir shall succeed, with any parameter absent from the document taking its documented default.
*Verify:* U — a checked-in corpus of state documents from every released version is restored in
CI.

**FR-STATE-030 (Must)** — The user shall be able to save the current state as a named preset and
recall it. Presets shall be interchangeable between the standalone application and the CLAP
plugin.
*Verify:* I.

**FR-STATE-040 (Must)** — Preset files shall be plain, human-readable text in a documented
format, so that a user can inspect, diff, version-control and hand-edit them.
*Verify:* M plus S (schema check).

**FR-STATE-050 (Must)** — Preset recall shall not glitch the audio thread; the constraints of
FR-NAM-070 apply to any model or IR change a preset implies.
*Verify:* I.

**FR-STATE-060 (Must)** — Plugin state saved into a host project shall capture everything needed
to reproduce the tone, including the identity of the loaded model and IR files.
*Verify:* I — save a project, restart the host, reopen, assert bit-identical output for identical
input.

**FR-STATE-070 (Must)** — A reference to a model or IR file shall be recorded as **all** of: the
path relative to a configured library root, the original absolute path, and a content hash of the
file. On restore, Namir shall resolve the reference by trying the library-relative path, then the
absolute path, then a content-hash search of the library. If all fail, the state shall load with
that stage empty, and the user shall be shown the missing file's name and hash, with an option to
locate it manually.
*Rationale:* This is UC-3 and it is the single most common failure mode of every plugin of this
kind. Failing to open a project because a file moved is unacceptable; failing silently is worse.
*Verify:* I — each resolution path and each failure path exercised individually.

**FR-STATE-080 (Should)** — The user shall be able to embed the model and IR data directly in the
plugin state, making the project fully portable at the cost of size. This shall be a per-instance
choice with a configurable default.
*Verify:* I.

**FR-STATE-090 (Should)** — A factory set of presets shall ship with Namir, and factory presets
shall be read-only, with "save as" offered instead of "save".
*Verify:* M.

### 5.10 Model and IR library (LIB)

**FR-LIB-010 (Must)** — The user shall be able to nominate one or more directories as library
roots, which Namir scans recursively for `.nam` and IR files.
*Verify:* I.

**FR-LIB-020 (Must)** — Library scanning shall occur off the audio thread and shall not block the
user interface. Progress shall be visible and the scan cancellable.
*Verify:* I with a synthetic library of at least 10 000 files.

**FR-LIB-030 (Must)** — The library index shall be persisted between sessions and updated
incrementally, so that startup does not require a full rescan.
*Verify:* B — second start-up with an unchanged 10 000-file library shall be measurably faster
than the first and shall meet NFR-PERF-060.

**FR-LIB-040 (Must)** — The user shall be able to filter the library by free-text search over file
name and metadata fields.
*Verify:* U.

**FR-LIB-050 (Should)** — The user shall be able to mark items as favourites and filter by that
mark. Favourites shall persist independently of file location, keyed by content hash.
*Verify:* U.

**FR-LIB-060 (Should)** — The user shall be able to step to the next/previous library item with a
single action, so that a library can be auditioned rapidly under FR-NAM-070.
*Verify:* M.

**FR-LIB-070 (Must)** — Files that disappear, change or are added while Namir is running shall be
reflected in the library within one rescan, and a missing file shall never crash Namir or the host.
*Verify:* I.

### 5.11 Standalone audio I/O (IO)

These requirements apply to the standalone application only.

**FR-IO-010 (Must)** — The user shall be able to select an audio input device and an audio output
device from those the system reports, including selecting different devices for each where the
platform permits.
*Verify:* M per platform.

**FR-IO-020 (Must)** — On Windows, WASAPI shall be supported in both shared and exclusive mode.
ASIO support is **Should**, and if included shall be built such that ASIO SDK licensing does not
contaminate the distribution of Namir's own source (NFR-LIC-040).
*Verify:* M.

**FR-IO-030 (Must)** — On Linux, ALSA shall be supported; PipeWire and/or JACK support is
**Should**. On macOS, CoreAudio shall be supported.
*Verify:* M per platform.

**FR-IO-040 (Must)** — The user shall be able to select sample rate and buffer size from those the
selected device reports as supported, and the current values shall always be displayed.
*Verify:* M.

**FR-IO-050 (Must)** — The application shall display the measured round-trip latency, or the
driver-reported latency where measurement is not possible, in both samples and milliseconds.
*Verify:* M.

**FR-IO-060 (Must)** — The application shall detect and report audio dropouts (xruns), showing a
running count for the session, resettable by the user.
*Verify:* I — induce an xrun with a synthetic overload and assert it is counted.

**FR-IO-070 (Must)** — Device removal while in use, or a device failing to open, shall be handled
without crashing or hanging: the application shall report the condition, stop the stream cleanly,
and allow the user to select another device.
*Verify:* I with a virtual device that can be made to fail on demand.

**FR-IO-080 (Must)** — Audio device selection, sample rate, buffer size and channel mapping shall
persist between sessions, and the application shall degrade gracefully to a working default if the
remembered device is unavailable at start-up.
*Verify:* I.

**FR-IO-090 (Should)** — The user shall be able to map which hardware input channel feeds the
engine and which hardware output channels receive it.
*Verify:* M.

**FR-IO-100 (Could)** — The application shall be able to play a backing track file through the
output alongside the processed input, with independent level.
*Verify:* M.

### 5.12 CLAP plugin integration (CLAP)

**FR-CLAP-010 (Must)** — Namir shall be distributed as a CLAP plugin conforming to the CLAP 1.x
ABI, with a stable, globally unique plugin identifier in reverse-DNS form.
*Verify:* I — validated with the reference CLAP validator tool.

**FR-CLAP-020 (Must)** — The plugin shall pass the reference CLAP validator with no errors, as a
gate in CI.
*Verify:* S.

**FR-CLAP-030 (Must)** — The plugin shall declare audio port configurations corresponding to
FR-CHAIN-060 and shall correctly negotiate the configuration the host requests.
*Verify:* I across at least two host implementations.

**FR-CLAP-040 (Must)** — The plugin shall report its total latency in samples and shall notify the
host whenever that latency changes, including as a result of a model change under FR-NAM-050.
*Verify:* I.

**FR-CLAP-050 (Must)** — The plugin shall support host-driven state save and load per Section 5.9.
*Verify:* I.

**FR-CLAP-060 (Must)** — The plugin shall implement host-driven bypass such that the host's bypass
is sample-accurate and click-free, equivalent to FR-CHAIN-030.
*Verify:* I.

**FR-CLAP-070 (Must)** — The plugin shall support arbitrary and varying block sizes, including a
block size of one sample and block sizes that change between calls, without artefacts.
*Verify:* U — process the same signal in randomised block sizes and assert the output matches the
fixed-block reference to within numerical tolerance.

**FR-CLAP-080 (Must)** — The plugin shall correctly handle every sample rate the host may present
within 44.1 kHz to 192 kHz inclusive, including a mid-session sample-rate change.
*Verify:* I.

**FR-CLAP-090 (Must)** — Multiple instances of the plugin shall coexist in one host process
without interfering with each other's state, and shall share immutable resources (such as a loaded
model's weights) where they reference the same file.
*Verify:* I plus B — measure that N instances of one model use materially less memory than N
separate copies.

**FR-CLAP-100 (Must)** — The plugin shall provide an embedded graphical editor via the CLAP GUI
extension, supporting the host embedding it, and shall function correctly if the host declines to
show a GUI at all.
*Verify:* I.

*Consequence (added M13, 2026-08-12, from loading the plugin on macOS)* — **the first clause is not
met on macOS or Linux, and cannot be as the plugin is built.**
`crates/namir-clap/src/gui.rs`'s `is_api_supported` returns true only for `GuiApiType::WIN32`, so a
host on any other platform is refused and never embeds an editor. Observed rather than inferred: in
Reaper on macOS the plugin loads and processes audio, and the window the host shows is **Reaper's
own generic parameter panel** — no brand mark, no meters, because Namir draws nothing. The
standalone application is unaffected on those platforms, since it opens its own window and never
uses this extension.

*The second clause is met, and the same observation is what demonstrates it.* "Shall function
correctly if the host declines to show a GUI at all" is exactly the situation a macOS host is in,
and audio, parameters and state all work.

This note changes no text, priority or `*Verify:*` line above; the requirement remains unqualified
about platform and remains unmet. It is `**UNRESOLVED**` in the generated plan and owned by M9b, so
nothing in the ledger claims otherwise. **Whether 1.0 ships a plugin with no interface on two of
three supported platforms is a scope question, not a verification one** —
`03-implementation-roadmap.md` §15 carries it as an open decision rather than this note settling it.
The restriction was introduced at M6 following spike S-4, which ran on Windows only, and until this
note it was recorded nowhere outside a code comment.

**FR-CLAP-110 (Should)** — The GUI shall support host-driven resizing and shall report its size
constraints and preferred aspect to the host.
*Verify:* M.

**FR-CLAP-120 (Should)** — The plugin shall accept MIDI/note-expression program change messages to
select presets, so that a foot controller can change tones.
*Verify:* I.

**FR-CLAP-130 (Must)** — The plugin shall never block the audio thread waiting on the GUI thread,
the file system, or the host, under any user action including model loading, preset recall and
library scanning.
*Verify:* S plus I — see NFR-RT-010.

### 5.13 User interface (UI)

**FR-UI-010 (Must)** — A single graphical user interface implementation shall serve both product
configurations, differing only in the presence of the audio-device panel (Section 5.11).
*Verify:* S.

**FR-UI-020 (Must)** — The interface shall present, on one screen without navigation: input meter
and trim, gate controls, the loaded model's name, the loaded IR's name, EQ controls, output meter
and level, and a global bypass.
*Verify:* M.

**FR-UI-030 (Must)** — Every control shall be operable by mouse and by keyboard, and every control
shall have an accessible name.
*Verify:* M against a written accessibility script.

**FR-UI-040 (Must)** — Every control shall display its current value numerically on demand, and
shall accept a typed numeric value.
*Verify:* M.

**FR-UI-050 (Must)** — A control shall reset to its default on a documented gesture (double-click
or equivalent), and shall support fine adjustment on a documented modifier.
*Verify:* M.

**FR-UI-060 (Must)** — The interface shall remain responsive (no frame exceeding 100 ms) while a
library scan of 10 000 files is in progress.
*Verify:* B.

**FR-UI-070 (Must)** — Errors shall be surfaced non-modally and shall never interrupt audio.
An error shall state what failed, which file or device it concerned, and what the user can do.
*Verify:* M against the error catalogue of FR-ERR-020.

**FR-UI-080 (Should)** — The interface shall be usable at a display scale of 100 % to 300 % and on
a window as small as 800×600 logical pixels.
*Verify:* M.

**FR-UI-090 (Should)** — Controls shall be sized and spaced such that touch operation is viable,
in anticipation of the mobile platforms of NFR-PORT-030.
*Verify:* M.

**FR-UI-100 (Could)** — A light and a dark theme shall be provided, following the OS preference by
default.
*Verify:* M.

**FR-UI-110 (Should)** — The interface shall display the Namir brand mark, and the standalone
application's window and executable shall carry the application icon.
*Verify:* M.

*Consequence (added M12, 2026-08-10)* — M12 closes the **brand mark** clause only. Both **icon**
clauses defer to M13, for two independent reasons. The executable icon needs a build script to embed
a Windows resource, which `02-architecture.md` **D-17.3** declines to admit into a shipped crate for
a cosmetic feature, moving it to M13's packaging pipeline. The window icon has no route through the
pinned stack at all: `baseview` 0.2.2's `WindowOpenOptions` is `#[non_exhaustive]` and carries
exactly `title`, `size` and `scale` plus an `opengl`-gated `gl_config`, with no icon field, so
`03-implementation-roadmap.md` §19's instruction to set it "through baseview's own window options"
cannot be followed as written. This note changes no text, priority or `*Verify:*` line above — only
the milestone that closes the requirement moves. `docs/manual-tests/fr-ui-110-brand-mark.md` records
what was and was not executed, including that no display was available to execute the brand-mark
half either.

*Consequence (added M13, 2026-08-11)* — the **executable** icon clause is built; the **window** icon
clause **cannot be met through the pinned stack**, which is a finding rather than a further
deferral. `images/namir.ico` is generated from `images/namir.png` by `xtask identity` and gated for
freshness the same way M12's brand-mark blob is, and `rcedit` embeds it into `namir.exe` in the
packaging pipeline, so `02-architecture.md` **D-17.3**'s refusal of a build script in a shipped
crate holds and its stated cost is now real: a plain `cargo build` produces an icon-less executable.
The window clause is a different matter. M12 left "whether `baseview` 0.3.0 gained an icon field"
explicitly unchecked; M13 checked, and **no published `baseview` version has ever exposed an icon on
any backend** — 0.3.0 does not even have `WindowOpenOptions`, and its Win32 window class registers
`hIcon: null_mut()` byte-identically to 0.2.2. The upgrade is unreachable regardless, published
`egui-baseview` 0.6.0 requiring `baseview` 0.2.2. The only in-process route is `WM_SETICON`, which
D-17.3 priced at a fourth `#![allow(unsafe_code)]` file for a cosmetic feature and declined. What
remains untested is whether the shell's own executable-icon fallback gives the window, the taskbar
button and Alt-Tab an icon anyway; `docs/manual-tests/fr-ui-110-brand-mark.md` carries those as
three separate unexecuted steps, because they need not agree. This note changes no text, priority or
`*Verify:*` line above. **FR-UI-110 remains open**, on the window clause and on those observations.

### 5.14 Diagnostics and error handling (ERR)

**FR-ERR-010 (Must)** — Namir shall write a log to a per-user location, with a configurable
verbosity, rotated so that it cannot grow without bound.
*Verify:* I.

**FR-ERR-020 (Must)** — Every user-visible error shall be drawn from a catalogue with a stable
identifier, so that errors can be documented, searched and tested.
*Verify:* S — the catalogue is enumerable and every error path in the code maps to an entry.

**FR-ERR-030 (Must)** — No logging, allocation or formatting for logging shall occur on the audio
thread. Diagnostics originating in the engine shall be communicated to a non-real-time thread
without blocking.
*Verify:* S plus I.

**FR-ERR-040 (Must)** — A panic or unexpected fault in a non-audio thread shall not take down the
host process. In the plugin configuration, Namir shall contain such a fault and continue passing
audio, degraded if necessary.
*Rationale:* Crashing a user's DAW loses their work. This is the harm Namir must most carefully
avoid.
*Verify:* I — inject a fault into each non-audio subsystem.

**FR-ERR-050 (Should)** — The user shall be able to export a diagnostic bundle (log, configuration,
platform and device information, engine state) with one action, for bug reports.
*Verify:* M.

**FR-ERR-060 (Must)** — In 1.0, Namir shall make no outbound network connection and shall transmit
no data off the user's machine: no telemetry, no crash-report upload, no update check.
*Verify:* S — a build-time check that no network-capable dependency is linked into the 1.0 binaries.

**FR-ERR-070 (Must)** — Any network capability added after 1.0 (see RD-1) shall satisfy all of the
following, and this requirement shall survive the relaxation of FR-ERR-060:

1. it is initiated only by an explicit user action, never on start-up and never in the background;
2. it is confined to a single, named, documented endpoint per feature;
3. it transmits no information about the user, their machine, their session or their content beyond
   what the request itself requires;
4. it is disabled by default and its enablement is persisted as an explicit user choice;
5. it is compiled out entirely by a build feature flag, so that a network-free build remains
   producible and verifiable indefinitely;
6. it never executes on, blocks, or is reachable from the audio thread.

*Rationale:* Recorded now, while the answer is uncontroversial, so that the first network feature is
built against a standard rather than negotiating one under delivery pressure.
*Verify:* S — the network-free build configuration is a permanent CI target; I per feature.

### 5.15 Packaging and distribution (PKG)

**FR-PKG-010 (Must)** — Namir shall produce an installable distribution for each supported platform,
built by CI from a tagged source tree.
*Verify:* S — the release workflow is triggered by a tag, runs on every tier-1 and tier-2 platform,
and every published distribution is an artifact of that workflow rather than of a local build.

**FR-PKG-020 (Must)** — The CLAP artifact shall be produced in the form the platform's plugin loader
requires: a shared library renamed to the `.clap` extension on Windows and Linux, and a **bundle
directory** on macOS containing `Namir.clap/Contents/Info.plist`, `Namir.clap/Contents/PkgInfo` and
`Namir.clap/Contents/MacOS/<dylib>`.
*Rationale:* CLAP's `entry.h` defines the plugin path as the shared object on Windows and Linux but
as the bundle directory on macOS; a renamed dylib does not load there.
*Verify:* S — the packaging step asserts the produced layout against the required form for the
platform it targets, and fails the build on any deviation.

**FR-PKG-030 (Must)** — The Windows installer shall offer both a per-user and a system-wide install
scope, shall default to per-user, and shall place the CLAP artifact in the CLAP directory
corresponding to the chosen scope, as recorded in `02-architecture.md`.
*Verify:* M.

**FR-PKG-040 (Must)** — Every distribution, installer and archive alike, shall contain the
machine-generated attribution file of NFR-LIC-030 and the full text of both licences of
NFR-LIC-010.
*Verify:* S — the packaging step asserts the presence of all three files in every distribution it
produces.

**FR-PKG-050 (Should)** — A plain archive requiring no installer, containing the same artifacts,
shall be published alongside each platform's installer.
*Verify:* S.

---

## 6. Non-functional requirements

### 6.1 Real-time safety (RT)

**NFR-RT-010 (Must)** — The audio thread shall be real-time safe as defined in Section 2: no heap
allocation or deallocation, no lock that any non-real-time thread can hold, no file or network I/O,
no system call that may block, no unbounded loop, and no operation whose worst-case time is not
bounded.
*Rationale:* This is the requirement from which most of the architecture follows. It is listed
first for that reason.
*Verify:* S — an allocation-detecting harness fails any test that allocates on the audio thread —
plus I under a stress test with concurrent model loading, preset recall and library scanning.

**NFR-RT-020 (Must)** — Communication between the audio thread and other threads shall be
wait-free from the audio thread's side.
*Verify:* S plus code review.

**NFR-RT-030 (Must)** — Denormal floating-point numbers shall not cause a measurable CPU spike in
any stage, on any supported platform.
*Verify:* B — drive each stage with a decaying signal into the denormal range and assert processing
time stays within 10 % of nominal.

**NFR-RT-040 (Must)** — The engine's worst-case per-block processing time shall not depend on the
audio content, on parameter values, or on how long the engine has been running.
*Verify:* B — statistical analysis of per-block timing over a long run with varied material,
reporting the 99.9th percentile, not the mean.

### 6.2 Performance (PERF)

**NFR-PERF-010 (Must)** — With a standard NAM WaveNet model, a 2-second stereo IR, gate and EQ all
active, at 48 kHz with a 64-sample block, one instance shall consume no more than 25 % of one core
of the reference machine, measured at the 99.9th percentile of per-block time.
*Note:* The reference machine is defined in `02-architecture.md`. The 25 % figure is a placeholder
pending the spike (OQ-2).
*Verify:* B, as a CI regression gate.

**NFR-PERF-020 (Must)** — The engine shall add no latency beyond that reported per FR-CLAP-040, and
that reported latency shall be zero when no sample-rate conversion is active and no limiter
look-ahead is engaged.
*Verify:* U.

**NFR-PERF-030 (Must)** — The standalone application shall reach an audible state (audio streaming,
default state loaded) within 3 seconds on the reference machine with a warm library index.
*Verify:* B.

**NFR-PERF-040 (Must)** — Plugin instantiation in a host shall complete within 200 ms, excluding
model loading.
*Verify:* B.

**NFR-PERF-050 (Must)** — Loading a model or IR shall complete within 500 ms for files up to 50 MB
on the reference machine, and shall never delay the audio thread regardless of duration
(FR-NAM-070).
*Verify:* B.

**NFR-PERF-060 (Must)** — An incremental library scan of an unchanged 10 000-file library shall
complete within 2 seconds.
*Verify:* B.

**NFR-PERF-070 (Should)** — Idle memory footprint of one plugin instance with no model loaded shall
not exceed 64 MB.
*Verify:* B.

### 6.3 Portability (PORT)

**NFR-PORT-010 (Must)** — Namir shall be written in Rust, using the stable toolchain. The minimum
supported Rust version shall be stated in the manifest and enforced in CI, and shall not be raised
in a patch release.
*Verify:* S.

**NFR-PORT-020 (Must)** — All platform-specific code shall reside behind explicit abstractions such
that the engine, the parameter system, the state system and the library system contain no
platform-conditional code at all.
*Verify:* S — a lint over the source tree rejects platform conditionals outside designated modules.

**NFR-PORT-030 (Must)** — No design decision shall be taken that precludes an Android or iOS build.
Specifically: no assumption of a file-system-wide path namespace, no assumption of a mouse, no
assumption that the process can spawn unlimited threads, no blocking dialog on the path of any
audio-affecting operation, and no dependency on a desktop-only windowing model in the engine or in
the state, parameter and library systems.
*Rationale:* Mobile is not a 1.0 deliverable, but the cost of keeping the door open now is small and
the cost of reopening it later is not.
*Verify:* S — the engine and its supporting crates shall build for `aarch64-linux-android` and
`aarch64-apple-ios` in CI, even though no application is shipped for those targets.

**NFR-PORT-040 (Must)** — Namir shall not require a C++ toolchain to build the standalone
application or the CLAP plugin for any tier-1 or tier-2 platform.
*Note:* This constrains, but does not decide, the NAM inference question (OQ-1). A C++ dependency
would have to be justified against this requirement and the requirement amended.
*Verify:* S — CI builds in a container with no C++ compiler present.

**NFR-PORT-050 (Must)** — Byte order, path separators, line endings and text encoding shall be
handled such that preset and state files written on one platform load identically on another.
*Verify:* U — cross-platform round-trip corpus in CI.

**NFR-PORT-060 (Should)** — Namir shall build for `x86-64` and `aarch64` on every tier-1 and tier-2
platform.
*Verify:* S.

### 6.4 Quality and verification (QUAL)

**NFR-QUAL-010 (Must)** — Every requirement in this document marked **Must** shall be covered by at
least one automated test, except where the *Verify* field states **M**, in which case it shall be
covered by a written manual test script held in the repository.
*Verify:* S — a traceability check in CI maps requirement identifiers to test identifiers and fails
on any uncovered **Must**.

**NFR-QUAL-020 (Must)** — Tests shall be written before the implementation of the corresponding
functionality, per the agreed SDLC.
*Verify:* Process — enforced by review, evidenced by commit order.

**NFR-QUAL-030 (Must)** — The DSP stages shall be verified against golden reference audio held in
the repository, with tolerances stated numerically, never by ear alone.
*Verify:* G.

**NFR-QUAL-040 (Must)** — All parsers that consume untrusted input — the `.nam` reader, the audio
file readers, the preset and state readers — shall be fuzz-tested continuously, with a corpus
retained in the repository, and shall not panic, hang, over-allocate or read out of bounds on any
input.
*Rationale:* Model and IR files are routinely downloaded from forums and file-sharing sites by
non-technical users. These parsers are Namir's real attack surface.
*Verify:* S — fuzz targets run in CI; any crash is a release blocker.

**NFR-QUAL-050 (Must)** — CI shall run the full test suite on every tier-1 and tier-2 platform for
every change, and shall gate merges.
*Verify:* S.

**NFR-QUAL-060 (Must)** — The code shall compile with no warnings under the project's configured
lint set, and formatting shall be enforced mechanically.
*Verify:* S.

**NFR-QUAL-070 (Should)** — Unsafe code shall be confined to designated modules, each carrying a
written justification and a safety argument for every unsafe block. The rest of the workspace shall
forbid unsafe code by attribute.
*Verify:* S.

### 6.5 Licensing and provenance (LIC)

**NFR-LIC-010 (Must)** — Namir shall be published under `MIT OR Apache-2.0`, at the recipient's
option, with the copyright held by Erwan Patrick Legrand.
*Verify:* S — licence files present, manifest metadata correct, SPDX headers checked.

**NFR-LIC-020 (Must)** — Every dependency, transitively, shall carry a licence compatible with
distribution under both MIT and Apache-2.0. Copyleft licences (GPL, AGPL, LGPL where static linking
applies) shall be rejected.
*Verify:* S — an automated licence audit gates CI.

**NFR-LIC-030 (Must)** — A machine-generated attribution file listing every dependency and its
licence shall be produced by the build and shipped with the binaries.
*Verify:* S.

**NFR-LIC-040 (Must)** — Any component whose licence terms cannot be satisfied by MIT/Apache
distribution — such as the Steinberg VST3 SDK or the ASIO SDK — shall not be a required build
dependency. If such a component is supported at all, it shall be an optional feature that the user
enables and builds themselves.
*Rationale:* This is why CLAP, whose specification and reference headers are MIT-licensed, is the
plugin format of record for 1.0.
*Verify:* S.

**NFR-LIC-050 (Must)** — Test assets in the repository — NAM models, impulse responses, audio
signals — shall be either generated by this project or carry a licence permitting redistribution,
recorded in a manifest.
*Verify:* S.

**NFR-LIC-060 (Should)** — The repository shall be REUSE-compliant, with every file carrying or
inheriting an SPDX identifier.
*Verify:* S.

**NFR-LIC-070 (Must)** — Brand assets — the name "Namir" and the logo — are not covered by the code
licence of NFR-LIC-010. The terms on which they may be used shall be stated explicitly in the
repository.
*Verify:* S.

### 6.6 Security and privacy (SEC)

**NFR-SEC-010 (Must)** — A malicious or corrupted `.nam`, IR, preset or state file shall not lead
to code execution, out-of-bounds access, unbounded allocation or a hang. See NFR-QUAL-040.
*Verify:* S.

**NFR-SEC-020 (Must)** — Namir shall impose a documented upper bound on the resources a single file
may cause it to allocate, and shall reject a file that exceeds it with a clear message rather than
exhausting memory.
*Verify:* U.

**NFR-SEC-030 (Must)** — Namir 1.0 shall make no outbound network connection (FR-ERR-060), and a
network-free build shall remain producible for every subsequent version (FR-ERR-070.5).
*Verify:* S.

**NFR-SEC-040 (Should)** — Release binaries shall be reproducible from a tagged source tree, and the
build shall publish the hashes needed to verify that.
*Verify:* S.

### 6.7 Maintainability and build (BUILD)

**NFR-BUILD-010 (Must)** — A clean build from a fresh checkout shall require only the Rust toolchain
and the platform's standard system libraries, with every other dependency fetched by the package
manager and pinned by a lockfile.
*Verify:* S.

**NFR-BUILD-020 (Must)** — The repository shall document how to build, test and run every product
configuration on every supported platform, and that documentation shall be exercised by CI so it
cannot drift.
*Verify:* S.

**NFR-BUILD-030 (Should)** — A full clean build of the workspace shall complete within 5 minutes on
the reference machine.
*Verify:* B.

### 6.8 Documentation (DOC)

**NFR-DOC-010 (Must)** — The `.nam` subset Namir supports, the preset format and the state format
shall each be documented in the repository to the level of detail needed for a third party to write
a compatible reader.
*Verify:* M.

**NFR-DOC-020 (Must)** — Every public API item in every published crate shall carry documentation,
enforced mechanically.
*Verify:* S.

**NFR-DOC-030 (Should)** — A user guide covering installation, audio setup, the signal chain and
troubleshooting shall ship with 1.0.
*Verify:* M.

**NFR-DOC-040 (Must)** — The repository shall carry a README identifying the product, stating what
it does, naming its licence, and giving the commands to build, run and test it.
*Verify:* S.

---

## 7. Scope boundary

### 7.1 Explicitly out of scope for 1.0

Recorded here to prevent scope drift. Each is a **Won't** for 1.0, not a rejection.

| Item | Note |
|---|---|
| VST3, AU, AUv3, AAX, LV2 plugin formats | VST3's licensing (GPLv3 or a proprietary Steinberg agreement) conflicts with NFR-LIC-010; the others are deferred on effort grounds. CLAP only for 1.0. |
| User-reorderable or branching signal chain | Section 5.1 is fixed for 1.0. Planned: RD-2. |
| More than one NAM stage, or more than two IR stages | One NAM, two IR slots (FR-IR-080) for 1.0. Planned: RD-2. |
| Additional effects: delay, reverb, modulation, pitch | Not in the chosen chain. |
| Tuner, metronome, looper | Live-performance features deferred. |
| Multi-channel amp/preset switching for live use | Deferred with the above. |
| NAM model training or capture | A different product. |
| Online model/IR browsing and download, including the Tone3000 API | Excluded from 1.0 by FR-ERR-060. Planned: RD-1. |
| Hosting third-party CLAP plugins inside Namir's chain | Not a 1.0 capability. Considered and deliberately shaped in RD-3. |
| Audio recording or multitrack | The DAW's job in the plugin configuration; deferred for standalone. |
| Mobile applications | Enabled by NFR-PORT-030, but not built for 1.0. |
| Localisation into languages other than English | Strings should be externalised, but no translations ship. |
| Sidechain, external routing, MIDI-controlled parameters beyond FR-CLAP-120 | Deferred. |

### 7.2 Known post-1.0 direction (RD-*)

These are **not** requirements and shall not be built for 1.0. They are recorded because the
architecture phase must not design them out. Each states only what it constrains **now**.

**RD-1 — Online model and IR acquisition, via the Tone3000 API (`https://www.tone3000.com/api`).**
Browse, search and download `.nam` models and IRs from within Namir.
*Constrains now:* the library subsystem (Section 5.10) must treat "where a file came from" as a
property of a library entry rather than assuming every entry is a local file the user placed there;
FR-STATE-070's content-hash identity becomes the mechanism by which a downloaded file is recognised
across machines. FR-ERR-070 already sets the terms this feature must meet.
*To be settled when specified:* the API's authentication model, rate limits and terms of use; and —
separately and importantly — the licence under which individual community models are distributed.
Namir may cache a downloaded file for the user who downloaded it; it must not assume any right to
redistribute one, bundle one, or include one in an exported preset under FR-STATE-080.

**RD-2 — More than one NAM stage and a more flexible chain.** Multiple amp stages, more IR slots,
and eventually a user-ordered chain.
*Constrains now:* the chain must be an ordered collection of uniform stages internally, even though
1.0 populates it with a fixed list (Section 5.1); and the parameter system must be able to address
a parameter as (stage instance, parameter) without either the identifier stability of FR-PARAM-020
or the state format of FR-STATE-010 needing to change when the chain grows. This is the single
largest architectural consequence in this section — see OQ-9.

**RD-3 — Hosting third-party CLAP plugins as chain stages.** Insert a foreign CLAP plugin into
Namir's chain.
*Constrains now:* only that RD-2's uniform stage abstraction must be expressible by something whose
processing Namir does not control. It must **not** be taken to mean that CLAP is Namir's internal
module boundary — see OQ-10, which records why. If built, this is a desktop-only, opt-in,
feature-flagged capability, and NFR-RT-010 must be restated as conditional for any chain containing
a foreign plugin, because Namir cannot guarantee real-time safety for code it did not write.

**RD-4 — Android and iOS applications.** Already constrained by NFR-PORT-030, which is a Must for
1.0 and is what keeps this reachable. Note that RD-3 is largely incompatible with iOS, which does
not permit loading arbitrary third-party dynamic libraries; the two roadmap items must not be
allowed to entangle.

---

## 8. Assumptions and constraints

**A-1** — The `.nam` format is defined by an external project and may change. Namir tracks it but
does not control it. Requirements are written against the format as of this document's date.

**A-2** — Namir does not host the audio thread; it is a guest on a thread owned by a driver or a
host. Every requirement about audio-thread behaviour is a constraint Namir must satisfy, not a
policy it can set.

**A-3** — Users obtain models and IRs from the internet. They are untrusted input (NFR-QUAL-040).

**A-4** — The reference machine for every benchmark in Section 6 is defined once, in
`02-architecture.md`, and every performance figure in this document is relative to it.

**C-1** — Licence: MIT OR Apache-2.0 (NFR-LIC-010). This constrains dependency choice and excludes
some otherwise-attractive components.

**C-2** — Language: Rust, stable toolchain (NFR-PORT-010).

**C-3** — Plugin format for 1.0: CLAP only (Section 7).

**C-4** — Primary platform: Windows 11. Where platform requirements conflict, Windows wins.

---

## 9. Open questions for the architecture phase

These are recorded here because they affect requirements, but they shall be **answered in
`02-architecture.md`**, not here.

| ID | Question | Requirements affected |
|---|---|---|
| **OQ-1** | Is NAM inference implemented in Rust, or bound to the existing C++ core? Decided by a measured spike against FR-NAM-030 (accuracy), NFR-PERF-010 (speed) and NFR-PORT-040 (no C++ toolchain). | FR-NAM-020/030, NFR-PORT-040 |
| **OQ-2** | What are the real numbers behind the placeholders: the 90 dB accuracy floor, the 25 % CPU budget, the reference machine? To be set from spike measurements, not guessed. | FR-NAM-030, NFR-PERF-010 |
| **OQ-3** | Which GUI approach satisfies FR-UI-010, FR-CLAP-100, NFR-PORT-030 and NFR-LIC-020 simultaneously? Plugin-embedded GUI and touch-viability are the hard constraints. | FR-UI-*, FR-CLAP-100/110 |
| **OQ-4** | Uniform or non-uniform partitioned convolution for FR-IR-040/050, and what is the resulting cost curve against IR length? | FR-IR-040, FR-IR-050, NFR-PERF-010 |
| **OQ-5** | How is the model/IR handover of FR-NAM-070 realised without allocation on the audio thread, and how is the freeing of the old resource sequenced? | FR-NAM-070, NFR-RT-010 |
| **OQ-6** | Is resampling (FR-NAM-050) applied around the model only, or does the whole engine run at the model's rate? Each has consequences for the IR stage and for reported latency. | FR-NAM-050, FR-CLAP-040 |
| **OQ-7** | What is the state document format, given FR-STATE-040 (human-readable) and NFR-QUAL-040 (safe parsing)? | FR-STATE-010/040 |
| **OQ-8** | How is FR-CLAP-090's cross-instance resource sharing achieved without introducing a lock the audio thread can contend on? | FR-CLAP-090, NFR-RT-010 |
| **OQ-9** | What is the internal stage abstraction — the trait every chain stage implements, covering preparation, processing, parameters, state, latency and resource handover? It must serve 1.0's fixed chain and RD-2's flexible one without a redesign, and must be the thing OQ-5 and OQ-10 are answered in terms of. | FR-CHAIN-010, FR-PARAM-020, FR-STATE-010, RD-2 |
| **OQ-10** | Confirm and record the decision that CLAP is Namir's **external** interface only, not its internal module bus, and state the consequences for RD-3. The case against CLAP-as-internal-bus: it forces an `unsafe` C ABI between crates that ship in one binary; it hides model and IR identity from the host that needs it for FR-STATE-070, FR-CLAP-090 and Section 5.10; it makes NFR-RT-010 unguaranteeable; and dynamic library loading is not permitted on iOS, which would forfeit RD-4. | NFR-PORT-030, NFR-RT-010, NFR-QUAL-070, RD-3, RD-4 |

---

## 10. Traceability

Each requirement identifier in this document shall appear in:

1. `02-architecture.md`, mapped to the component that satisfies it;
2. `03-test-plan.md`, mapped to one or more test identifiers;
3. the test source itself, as a machine-readable annotation.

CI shall verify this mapping and fail on any **Must** requirement that is unmapped at any of the
three levels (NFR-QUAL-010). A requirement that cannot be traced is either not implemented or not
tested, and both are defects.

*Consequence (added M7)* — `03-test-plan.md` never existed as a separate file: the implementation
roadmap took the "03" slot in `docs/` first, and this gap went unnoticed until M7 built the
mechanical check this section calls for. Rather than hand-author and maintain a document that
would drift the moment a test moved (the exact failure mode this project's own methodology
distrusts elsewhere -- see `02-architecture.md`'s own reasoning for generating `params.lock`),
`docs/03-test-plan.md` is now itself **generated** by `cargo run -p xtask -- traceability --write`
and diffed in CI, collapsing this section's points 2 and 3 into one mechanism: a
`// trace: FR-XXX-NNN` comment (or, for the handful of tests already following it, an
`fr_xxx_nnn_...`-named test function) in the covering test source is both the "machine-readable
annotation" point 3 asks for and the sole input `xtask traceability` uses to populate the
generated point-2 document. Point 1 ("mapped to the component that satisfies it") is produced at
**crate granularity** -- the crate containing a requirement's covering `// trace:` annotation --
not module/function granularity; see `02-architecture.md` §23's matching note for why that scope
was chosen. A `Verify: M` requirement's "test identifier" is the `docs/manual-tests/*.md` file
matching its id, unchanged from what already existed. `Verify: Process` (NFR-QUAL-020, the FRS's
one user of a code missing from §1.5's own legend until this same M7 pass added it) has nothing a
build can inspect by definition and is exempted from source/manual lookup entirely.

*Consequence (added M8-planning, 2026-08-08)* — Section 5.15's packaging requirements take no
exception to the mechanism above, and one point is worth stating before it is discovered the hard
way: **being checked by CI is not itself traceability.** `xtask traceability` reads `// trace:`
annotations in repository source, not workflow YAML, so a `Verify: S` packaging requirement is
covered only when the assertion CI runs lives in the repository as an annotated test or `xtask`
subcommand — FR-PKG-010, FR-PKG-020 and FR-PKG-040 are all of this kind. FR-PKG-030 is `Verify: M`
and therefore needs a `docs/manual-tests/fr-pkg-030-*.md` file, on the same terms as every other
manual-verified requirement; it cannot be closed by a green release workflow.

*Consequence (added M9, 2026-08-08 — correcting the note above on a matter of fact, and stating one
corollary of the mechanism)* — "`xtask traceability` reads `// trace:` annotations in repository
source, not workflow YAML" is not what the tool does, and never was. `xtask/src/traceability.rs:111`
recognises two marker spellings — `// trace:` and `# trace:` — and `xtask/src/main.rs:205-216`
deliberately adds four non-Rust files to the scanned set: `.github/workflows/ci.yml` and `fuzz.yml`
under the component name `ci`, and the root `Cargo.toml` and `deny.toml` under `workspace`. Fifteen
Must requirements are covered today by nothing else — FR-CFG-010, FR-ERR-060, FR-ERR-070,
NFR-BUILD-010, NFR-BUILD-020, NFR-DOC-020, NFR-LIC-010, NFR-LIC-020, NFR-LIC-040, NFR-PORT-010,
NFR-PORT-030, NFR-PORT-040, NFR-QUAL-050, NFR-QUAL-060 and NFR-SEC-030 — carried by eight tags, at
`Cargo.toml:1` and `:37`, `deny.toml:15` and `:82`, and `.github/workflows/ci.yml:34`, `:158`,
`:197` and `:322`. Checked this pass rather than assumed: none of the fifteen has a `// trace:`
annotation or an `fr_*`/`nfr_*`-named test function anywhere in Rust source, and
`docs/03-test-plan.md` records all fifteen as covered by `ci` or `workspace`. Enforcing the sentence
as written would therefore take the uncovered count from 24 to 39 rather than tighten anything, and
would leave NFR-PORT-030 and NFR-QUAL-050 unclosable in principle, since CI is what those two are
*about*.

**The distinction the note was reaching for is real; it is about adequacy, not file extension.** A
`# trace:` in build or CI configuration is admissible evidence exactly when one of two things holds:

1. **the requirement's own assertion is about the repository's build or CI configuration** — what
   the manifest declares, what the lint set forbids, what the workflow matrix runs, what may be a
   required build dependency. FR-CFG-010, NFR-BUILD-010, NFR-BUILD-020, NFR-DOC-020, NFR-LIC-040,
   NFR-PORT-010, NFR-QUAL-050, NFR-QUAL-060 and NFR-SEC-030 are of this kind; or
2. **the requirement's own `*Verify:*` line elects that configuration as the artifact to inspect** —
   FR-ERR-060 ("a build-time check that no network-capable dependency is linked into the 1.0
   binaries"), FR-ERR-070 ("the network-free build configuration is a permanent CI target; I per
   feature"), NFR-LIC-010 ("licence files present, manifest metadata correct, SPDX headers
   checked"), NFR-LIC-020 ("an automated licence audit gates CI"), NFR-PORT-030 ("shall build for
   `aarch64-linux-android` and `aarch64-apple-ios` in CI"), NFR-PORT-040 ("CI builds in a container
   with no C++ compiler present"). Where the `*Verify:*` line names the configuration, that election
   governs even though the requirement's own subject is the product's behaviour.

Nine of the fifteen fall under limb 1 and six under limb 2, which accounts for all of them. Eight of
the nine rest on limb 1 alone, their `*Verify:*` lines being a bare `S.` that elects nothing:
NFR-BUILD-010, NFR-BUILD-020, NFR-DOC-020, NFR-LIC-040, NFR-PORT-010, NFR-QUAL-050, NFR-QUAL-060 and
NFR-SEC-030. FR-CFG-010 is the ninth and the only limb-1 requirement whose `*Verify:*` line
elaborates at all ("both build targets are produced by CI from one workspace"); that elaboration
itself elects the configuration, so FR-CFG-010 satisfies both limbs. Two of the eight are worth
separating: NFR-LIC-040 rests on limb 1 cleanly, since what it asserts *is* which components may be
a required build dependency; NFR-SEC-030 is split in shape, its build-producibility clause being
limb 1 while its no-outbound-connection clause is derivative of FR-ERR-060 and leans on that
requirement's own election rather than on anything NFR-SEC-030 itself says.

Where neither limb holds — the requirement is about how the built product behaves, and its
`*Verify:*` line does not elect the configuration — a workflow step is only the thing that *runs*
the check, and the check itself must live in the repository as an annotated test or `xtask`
subcommand. That is the disposition the note above reached for FR-PKG-020 and FR-PKG-040, and it
stands: each of those names "the packaging step" as the thing that asserts, which is a check to be
written, not a configuration to be inspected. FR-PKG-010, grouped with them above, is the one this
pass reopens — its `*Verify:*` line elects the release workflow, which is limb 2, but `release.yml`
does not exist yet and is not on the scanned list; `03-implementation-roadmap.md` §15 item 10
records that choice, due before M13. Stated so the rule can be applied without re-deriving it:
**tag the configuration when the configuration is the artifact under test; tag a test when the
product is.** FR-CLAP-020 ("shall pass the reference CLAP validator with no errors, **as a gate in
CI**") is limb 1 — its own assertion is about the repository's CI configuration, and its `*Verify:*`
line is a bare `S.` that elects nothing — so the `clap-validator` step M9a adds may carry its
`# trace:` directly and needs no hand-written test behind it.

Checked against those fifteen, **two fail this rule as currently tagged**, recorded here rather than
repaired:

- **FR-ERR-070.** Its `*Verify:*` line elects the network-free build configuration, which is what
  puts it in limb 2 — but that election reaches sub-clause 5 alone. The other five numbered
  sub-clauses — explicit user action, one named endpoint, no user information, disabled by default,
  unreachable from the audio thread — are runtime assertions about a capability that does not exist,
  and `deny.toml`'s deny-list asserts none of them. That file's own comment says as much: the list
  is "a standing assertion that stays true rather than a fix for a real violation"
  (`deny.toml:70-72`), and FR-ERR-070's remaining sub-clauses are what will govern the first real
  network client when one arrives (`deny.toml:79-80`). The requirement is **vacuously satisfied
  while Namir has no network capability**, and the tag records that condition rather than
  compliance. The commit that adds the first network feature re-opens it, and the "I per feature"
  half of its own `*Verify:*` line is what closes it then.
- **NFR-QUAL-050.** The platform-matrix half is in `ci.yml`; the "**and shall gate merges**" half is
  a branch-protection setting held outside the repository, which no annotation inside it can assert.
  A known limit of the evidence, not coverage of the whole requirement.

One corollary of the mechanism above, stated because three Must requirements had already been read
the other way. `xtask traceability`'s dispatch is **mutually exclusive**: `Verify: M` is resolved
against `docs/manual-tests/` and never against source (`xtask/src/traceability.rs:177-186`);
`Verify: Process` is exempt (`:187-189`); and **every other code is resolved against source and
never against `docs/manual-tests/`** (`:190-192`). So a manual-test document written for a
`Verify: I`/`U`/`G`/`B`/`S` requirement is structurally invisible to the check, however much
executed evidence it records. That is deliberate and stays: for those codes the document is
**supplementary** evidence about a residue no automated test can reach, and the traced artifact
remains an annotated test. FR-CLAP-030, FR-CLAP-040 and FR-CLAP-100 each had the document and no
test, and read as uncovered for that reason alone — FR-CLAP-060, FR-CLAP-090, FR-IO-060, FR-IO-070
and FR-IO-080 are the same shape with both halves present, and all five resolve.
`02-architecture.md` **D-18.6** records the policy and rejects the two alternatives considered:
changing a requirement's `Verify` code, and teaching the tool to count a manual document for
`Verify: I`.

The two rules stated here do not weaken each other, because they are about different artifacts.
Configuration is admissible evidence under the two limbs precisely when the configuration *is* the
thing under test. A manual-test document is never the traced artifact for any code but **M**,
because for every other code the thing under test is the built product, and a document is a record
of someone having looked at it rather than the check itself.

---

## 11. Change log

| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-08-03 | Initial draft. |
| 0.2 | 2026-08-03 | Copyright holder set. Section 7 restructured into 7.1 (out of scope) and 7.2 (post-1.0 direction, RD-1..RD-4). FR-ERR-060 scoped to 1.0; FR-ERR-070 added to set standing terms for any future network feature; NFR-SEC-030 aligned. OQ-9 and OQ-10 added. |
| 0.3 | 2026-08-08 | M8-planning intake. New Section 5.15 Packaging and distribution: FR-PKG-010..050. FR-NAM-140 (unsupported architecture/configuration rejected distinctly from malformed) and FR-NAM-150 (NAM Architecture 2, A2-Full and A2-Lite) added; FR-UI-110 (brand mark and application icon), NFR-LIC-070 (brand assets not covered by the code licence), NFR-DOC-040 (README) added. Note appended at FR-CFG-040 recording that FR-PKG-010's installer does not conflict with it and that FR-PKG-050's plain archive is what keeps it true. Note appended at Section 10 recording that a CI workflow is not itself a traceable artifact. Planning index proposed FR-NAM-130, FR-UI-060 and NFR-LIC-060 for three of these; all three identifiers were already in use, and per Section 1.5 identifiers are never reused, so the new requirements took the next free numbers instead. |
| 0.4 | 2026-08-08 | M9 P0 decision pass. Section 10's 2026-08-08 note is corrected on a matter of fact by an appended `*Consequence (added M9)*` note: `xtask traceability` does read CI and build configuration — `xtask/src/traceability.rs:111`'s `# trace:` marker and `xtask/src/main.rs:205-216`'s four hard-coded paths — and fifteen Must requirements rest on that alone, so the original sentence would have taken the uncovered count from 24 to 39 rather than tightening anything, and would have left NFR-PORT-030 and NFR-QUAL-050 unclosable in principle. Replaced with a two-limb adequacy rule (configuration is admissible evidence when the requirement's own assertion is about the configuration, or when its own `*Verify:*` line elects it) and its one-sentence form: tag the configuration when the configuration is the artifact under test; tag a test when the product is. Nine of the fifteen fall under limb 1 and six under limb 2; eight of the nine limb-1 requirements have a bare `*Verify:* S.` and rest on limb 1 alone, FR-CFG-010 being the only one whose Verify line also elects the configuration and so satisfies both. FR-PKG-020's and FR-PKG-040's disposition under the original note is unchanged; FR-PKG-010's is reopened, since its `*Verify:*` line elects a release workflow that does not exist yet (roadmap §15 item 10); FR-CLAP-020 is confirmed taggable in CI as limb 1, its "as a gate in CI" clause being in its body and its `*Verify:*` line a bare `S.` that elects nothing. Two of the fifteen are recorded as failing the rule rather than repaired: FR-ERR-070 (its limb-2 election reaches sub-clause 5 alone; the other five sub-clauses are runtime assertions about a capability that does not exist — vacuously satisfied, re-opened by the first network feature) and NFR-QUAL-050 ("shall gate merges" is a branch-protection setting outside the repository). The same note records the tool's dispatch exclusivity — `Verify: M` resolves against `docs/manual-tests/` only, `Verify: Process` is exempt, every other code resolves against source only — which is why FR-CLAP-030, FR-CLAP-040 and FR-CLAP-100 read uncovered while FR-CLAP-060, FR-CLAP-090 and FR-IO-060/-070/-080 resolve; `02-architecture.md` D-18.6 holds the policy. Note appended at Section 1.5: the parenthetical after an identifier is the sole priority marker (`xtask/src/traceability.rs:81`), bold **Must**/**Should** inside a requirement body scopes a clause rather than retagging the requirement (FR-NAM-020, FR-IO-020, FR-IO-030), and this document declares 130 Musts, 31 Shoulds and 3 Coulds — 164 in all. |
| 0.5 | 2026-08-09 | M9a set-quantification sweep. **FR-CHAIN-010's chain order is amended to the order the product ships**, by an appended `*Consequence (added M9a, 2026-08-09)*` note that supersedes the original block rather than rewriting it: `input → noise gate → input trim → NAM → IR → EQ → output level → output`. The gate precedes the trim per `02-architecture.md` D-9.8 — a gate threshold should reference the interface's real noise floor and not move when the user adjusts trim. `build_default_chain` (`crates/namir-engine/src/stages/mod.rs:47-67`) has assembled it that way since M2 (2026-08-06; `7941577` declared the order in the doc comment with a `todo!()` body, the assembly landed later in the milestone) on `03-implementation-roadmap.md` §6's own direction, so the amendment makes this document describe the product rather than changing the product; D-9.8's *Rationale* had flagged the point for review and it went unresolved from M2 until this sweep read the requirement against its code and surfaced a requirement-versus-code conflict. Section 1.2's "noise gate ahead of the amp" and FR-GATE-010's "ahead of the NAM stage" are unaffected and stand as written. FR-CHAIN-010's `*Verify:*` line is deliberately **not** touched: its probe-signal method has never been executed, that gap is independent of the order, and it is what the requirement's `// trace-partial:` names. The same note records, without deciding, one interaction the amendment exposes: with Gate upstream of Trim, Gate's channel-0-then-duplicate pattern (`stages/gate.rs:163-172`) discards the right channel before Trim's −6 dB-per-term sum (`stages/trim.rs:145-156`) can use it, so the shipped Stereo configuration realises FR-CHAIN-070's default (left channel) rather than FR-CHAIN-060's table default (both channels summed). FR-CHAIN-060 and FR-CHAIN-070 are unchanged. |
| 0.6 | 2026-08-09 | M9a §14 adjudication pass. **Corrects 0.5's own record of the Stereo interaction**, by a further appended `*Consequence (added M9a, 2026-08-09)*` note at FR-CHAIN-010 rather than an edit to the paragraph it corrects. That paragraph overstated the finding, in the direction of severity: naming shipped Stereo behaviour "not FR-CHAIN-060's table default (both channels summed)" reads as FR-CHAIN-060 — a Must — being unmet, when FR-CHAIN-060's Stereo row reads `2 ch summed or L-only (FR-CHAIN-070)`, two permitted inputs with no default between them, and left-only is the second of them. FR-CHAIN-060 is **satisfied**. What cannot be met is **FR-CHAIN-070**, whose clause is that the user shall be able to *choose* left, right or the −6 dB sum: `params.lock` carries no stereo-source parameter among its twenty-nine live ones, so the choose clause fails before the chain order is reached. FR-CHAIN-070 is a **Should** (§1.5's sole priority marker), so it sits in no `03-implementation-roadmap.md` §14 row — §5.1 CHAIN's 8 Musts out of 9 requirements — and no §14 verdict or M8 exit-gate item follows; the finding is a Should left open, not a Must left broken. The mechanism is also restated with the condition 0.5 omitted: the right channel is discarded only while the gate is **on** (`gate.enabled` defaults to "On", `crates/namir-params/src/stages/gate.rs:8-16`); with the gate off, `stages/gate.rs:174-192`'s bypass crossfade settles at 0.0, each channel's pre-gate copy is restored, and `stages/trim.rs:145-156` sums both — so the sum is reachable, though only as a side effect of disabling an unrelated stage, which is not the choice FR-CHAIN-070 asks for. The same mistaken "FR-CHAIN-060's default" phrasing at `crates/namir-engine/src/stages/trim.rs:19-21` is recorded as the likely source, not repaired. No requirement text, priority or `*Verify:*` line is changed by this row. |
| 0.7 | 2026-08-10 | M12 (brand, README and product identity). **NFR-DOC-040 and NFR-LIC-070 gain their first artifacts**: `README.md` and `TRADEMARK.md` at the repository root, both asserted by a new `xtask identity` static check wired into CI, which is what makes a `Verify: S` method executable rather than a claim. NFR-LIC-070 is tagged plainly; **NFR-DOC-040 is `trace-partial:`** — a substring check cannot reach its "stating what it does" clause, and the gap is written down rather than papered over by a plain tag. **FR-UI-110 gains an appended `*Consequence*` note** recording that only its brand-mark clause is in scope for M12 and that both icon clauses defer to M13: the executable icon by `02-architecture.md` D-17.3's refusal to admit a build script into a shipped crate for a cosmetic feature, the window icon because `baseview` 0.2.2 has no icon field at all — which also makes `03-implementation-roadmap.md` §19's window-option instruction unfollowable, corrected there rather than here. No requirement text, priority or `*Verify:*` line is changed by this row. |
