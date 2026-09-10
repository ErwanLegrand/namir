# Namir user guide

Namir applies a Neural Amp Modeler (NAM) profile to your instrument signal, then a cabinet
impulse response (IR), with a noise gate ahead of the amp and a tone EQ after the cabinet. It
ships as two products built from one codebase and sharing one interface:

- **Namir (standalone)** — a native application with its own audio input/output, for practicing
  or recording without a DAW.
- **Namir (CLAP plugin)** — the same engine and interface hosted inside a CLAP-compatible DAW
  (e.g. Reaper, Bitwig).

This guide covers installing both, getting audio flowing, what the signal chain does, and what to
check when something isn't working. It does not attempt to document every control — the interface
is a single screen (see [The signal chain](#the-signal-chain) below) and is largely
self-explanatory once audio is running; this guide focuses on the parts that aren't.

Primary supported platform is Windows 11 x86-64. Linux and macOS builds exist and are exercised in
CI, but are less thoroughly verified against real hardware at the time of writing — see
[Known limitations](#known-limitations) for specifics.

## Installation

### Standalone app

Build and run from the workspace root:

```bash
cargo run --bin namir
```

or build a release binary once and run it directly:

```bash
cargo build --release --bin namir
```

The binary is `namir` (`namir.exe` on Windows), produced by the `namir-app` crate. There is no
separate installer at the time of writing; running the built binary is the whole install.

### CLAP plugin

Build the plugin in release mode:

```bash
cargo build --release -p namir-clap
```

This produces a shared library — `namir_clap.dll` on Windows, `libnamir_clap.so` on Linux,
`libnamir_clap.dylib` on macOS. What you do with it differs by platform, and **macOS is not like
the other two**:

| Platform | Per-user path (default, no admin rights needed) | System-wide path (opt-in, needs elevation) |
|---|---|---|
| Windows | `%LOCALAPPDATA%\Programs\Common\CLAP` | `%COMMONPROGRAMFILES%\CLAP` |
| macOS | `~/Library/Audio/Plug-Ins/CLAP` | `/Library/Audio/Plug-Ins/CLAP` |
| Linux | `~/.clap` | `/usr/lib/clap` |

**Windows and Linux — rename the library.** A host looks for a file with a `.clap` extension, and
on these two platforms that file *is* the shared library. For example, on Windows: build
`namir_clap.dll`, copy it to `%LOCALAPPDATA%\Programs\Common\CLAP\Namir.clap`, then open your host
and rescan its plugin paths.

**macOS — build a bundle, not a renamed file.** On macOS a `.clap` is a *bundle directory*, not a
renamed dylib: the CLAP specification defines a plugin's path as the shared library on Windows and
Linux but as the **bundle** on macOS. Simply renaming `libnamir_clap.dylib` to `Namir.clap`
produces something no host will load. The required layout is:

```
Namir.clap/
└── Contents/
    ├── Info.plist
    ├── PkgInfo
    └── MacOS/
        └── libnamir_clap.dylib
```

`Info.plist` needs at minimum `CFBundleExecutable` (`libnamir_clap.dylib`), `CFBundleIdentifier`,
`CFBundlePackageType` (`BNDL`), and `CFBundleName`; `PkgInfo` contains the eight bytes `BNDL????`.
Assembling this by hand is fiddly and easy to get subtly wrong, so treat building on macOS as a
developer activity for now — see [Known limitations](#known-limitations).

**Use these paths exactly, not others that look plausible.** On Windows in particular, a
plausible-looking location such as `%APPDATA%\REAPER\UserPlugins\CLAP` is **not** scanned by
Reaper, and a plugin placed there fails silently — it simply never appears in the plugin browser,
with no error or log message anywhere to point you at the cause. This was found empirically during
development and is the single most likely reason a build "doesn't show up" in a host. If your
plugin isn't appearing:

1. Confirm the file is at the per-user path above (Windows: `%LOCALAPPDATA%`, not `%APPDATA%`).
2. Confirm the file extension is `.clap`, not `.dll`/`.so`/`.dylib` — and on macOS, confirm it is a
   bundle *directory* with the layout shown above, not a renamed `.dylib`.
3. Rescan/refresh your host's plugin list (most hosts have an explicit "rescan" action; a restart
   also works).
4. If you need a system-wide install instead, use the system-wide path in the table above — this
   needs administrator/root privileges and is not the default.

## Audio setup

### Standalone app

The standalone app negotiates audio devices, sample rate, and buffer size **automatically** on
startup, and the "Audio Settings" panel lets you change any of them in-session. On each launch it
picks, in order: the device/rate/buffer remembered from your last session, else your system's
default input/output devices, else the first device the audio backend enumerates. Whatever it picks
is logged to the console, e.g.:

```
namir: audio stream started
namir: 48000 Hz, 480-frame block, at least ~20.0 ms estimated round-trip latency (in: "...", out: "...")
```

"At least" because the output stream asks the device for its own buffer rather than one Namir
picked (issue #166), and `cpal` gives no portable way to read back what the device chose — so the
figure covers the two buffers Namir itself sizes and nothing for the device. The buffer size you
select still sets the engine's block size and the input stream.

**The rates and buffer sizes on offer depend on the share mode** (issue #190). In shared mode
Windows reports the audio engine's own values — typically a single buffer size, and, on an output
device, every sample rate the engine will resample to. In exclusive mode the device reports its own
hardware limits, which is usually a much wider choice of buffer sizes and only the rates the
interface is actually set to. So turning `exclusive_mode` on will normally *change* what the two
lists contain; that is the device answering for itself rather than through the mixer. If a device
refuses exclusive mode, the session runs shared and the lists show shared's answer.

Your negotiated choice (device names, sample rate, buffer size, and channel mapping) is saved when
you close the app, and reloaded next time. If a remembered device is no longer available, the app
falls back to a working default automatically rather than failing to start, and the settings file
self-heals to the device that actually worked.

The settings file lives at:

- Windows: `%APPDATA%\Namir\audio-settings.json`
- macOS: `~/Library/Application Support/Namir/audio-settings.json`
- Linux: `$XDG_CONFIG_HOME/namir/audio-settings.json`, or `~/.config/namir/audio-settings.json` if
  `XDG_CONFIG_HOME` isn't set

If you need to force a specific device today, the only way is to edit this file directly (or
temporarily disable other devices at the OS level) and relaunch. To pick a different set of
devices from a full list without editing JSON, use `cargo run --example list_devices -p namir-app`
to see what your system reports, since there's no equivalent view inside the app itself yet.

### Backend basics

Namir's audio I/O is backed by `cpal`, which selects the appropriate native backend per platform:
WASAPI on Windows, ALSA on Linux, CoreAudio on macOS. You don't choose the backend — it's fixed by
your OS.

- **Windows (WASAPI):** shared mode is fully supported. **Exclusive mode is not currently
  available** — see [Known limitations](#known-limitations).
- **Linux (ALSA):** if PulseAudio or PipeWire has claimed exclusive access to your interface,
  device negotiation may behave differently than on a bare ALSA setup; this is a known area of
  platform variance rather than a Namir-specific bug.
- **macOS (CoreAudio):** no special configuration is needed beyond selecting the interface as your
  system's default in/out device, since there's no in-app selector yet (see above).

### CLAP plugin

Inside a host, audio device selection is the host's job, not the plugin's — Namir just processes
whatever audio the host routes to it. Sample rate and buffer size follow the host's session
settings automatically.

## The signal chain

Namir's engine runs six fixed stages, always in this order, and it is not user-reorderable:

**Input → Gate → NAM → IR → EQ → Output**

The one consequence worth knowing about: the gate's detector sees the signal *after* your Input
Level, so raising the input raises what the gate hears. If you change Input Level by a lot, expect
to move the gate threshold by about the same amount. (Namir shipped the other order — gate first,
threshold referenced to your interface's raw noise floor — up to and including the previous
release. Putting Input first is what lets a stereo input reach the amp as both channels summed
rather than as its left channel alone.)

One sentence on what each stage does:

1. **Input** — input gain (−24 dB to +24 dB) with a level meter and clip indicator, applied before
   everything else, and a DC blocker. A stereo input is summed here to the single channel the amp
   model runs on, at −6 dB per side.
2. **Gate** — a noise gate with threshold, attack, hold, and release controls (defaults: −70 dBFS,
   1 ms, 30 ms, 100 ms), with hysteresis so a signal hovering near the threshold doesn't chatter
   open and closed.
3. **NAM** — loads a `.nam` model file and runs neural-network inference to emulate an amp/pedal;
   displays the model's declared metadata (name, author, gear, tone type, description) where
   present, and applies the model's declared loudness normalisation so different models sound
   comparably loud when you swap between them.
4. **IR** — loads a cabinet impulse response (WAV; AIFF/FLAC support is planned but not
   guaranteed) and convolves it with the amp's output, with per-IR level, on/off, low-cut, and
   high-cut controls.
5. **EQ** — a three-band post-cabinet tone EQ (low shelf, mid peaking with adjustable Q, high
   shelf), plus a defeatable high-pass/low-pass filter pair.
6. **Output** — output level and metering, plus a global bypass that routes input straight to
   output at unity gain.

Input Level and Output Level sit side by side at the top of the screen, each with its meter
directly beneath it. Both meters read the signal *after* their own control, so they show the result
of the setting rather than what arrived before it — which is what makes them usable for setting the
level in the first place. Everything between them (gate, model, IR, EQ) is below.

Any stage with nothing loaded (no model, no IR) behaves as if bypassed — it won't mute your signal
or throw an error. Swapping a model or IR while playing crossfades smoothly (an equal-power fade
of 5–50 ms) rather than clicking or dropping out. See `docs/01-functional-requirements.md`
sections FR-GATE, FR-NAM, FR-IR, and FR-EQ for the precise parameter ranges and behavioural
guarantees behind each stage, if you need the exact numbers.

## Troubleshooting

### "My plugin doesn't appear in my DAW"

See [CLAP plugin installation](#clap-plugin) above — by far the most common cause is the file
sitting at a path the host doesn't scan (Windows: `%APPDATA%\...` instead of
`%LOCALAPPDATA%\Programs\Common\CLAP`), which fails with **no error message anywhere**. Double-check
the exact path and the `.clap` extension, then rescan your host's plugin list.

### WASAPI exclusive mode isn't available (Windows, standalone app)

**Known limitation.** The standalone app's WASAPI backend currently only opens streams in shared
mode. Exclusive mode is architecturally unavailable in the audio I/O library this build uses —
it's not a missing setting, there's no way to request it — so if your workflow depends on
exclusive-mode WASAPI for lower latency or exclusive device access, that isn't available yet.
Shared mode is fully supported and is what every device negotiates today.

### A device was disconnected while Namir was running, or won't open

Namir is built to report a disconnected or failed device rather than crash or hang, and it falls
back to a working device automatically the next time it starts (see
[Audio setup](#standalone-app) above for the fallback order and settings self-healing). One
current gap: if a device is lost or fails to open **while the app is already running**, there is
no in-app control to pick a different device without restarting the app — restarting renegotiates
devices fresh using the same automatic fallback. If this happens often, check your device's own
drivers/cabling first; genuine mid-session recovery without a restart is planned but not yet built.

### CLAP host briefly stops and restarts when I swap models

If you load a model whose declared sample rate differs from your session's, Namir engages an
internal resampler, which adds latency the plugin has to report to the host. CLAP's specification
requires latency changes to be announced via a host restart cycle while the plugin is active, so
swapping to a model with a *different* resampling ratio than the one currently loaded can cause a
brief stop/restart in your host (a short silence, not a crash). This is a CLAP protocol
requirement, not a Namir bug, and only affects models whose declared rate doesn't match your
session rate — the common case (matching rates, no resampler) never triggers it.

### General

If none of the above matches what you're seeing, check the console/log output the app or your
host prints on startup — it names the negotiated device, sample rate, and buffer size, and reports
specific error conditions (e.g. a malformed `.nam` file is rejected by name and reason, without
interrupting whatever was already loaded).

## Known limitations

Recorded here plainly rather than glossed over, since these are genuine, currently-open gaps:

- **WASAPI exclusive mode (Windows, standalone app)** is not available — see above. Shared mode is
  unaffected.
- **No in-app audio device/channel selection UI yet**, in either product. The standalone app
  negotiates devices automatically (remembered choice, then system default, then first
  enumerated); changing devices mid-session requires a restart. Channel *remapping* (choosing which
  physical input/output channel feeds the engine) is implemented under the hood but has no
  interactive control yet; true independent stereo input (two distinct physical input channels
  kept separate through the whole chain) is not yet implemented — stereo output today is a mono
  input duplicated to both channels.
- **Recovering from a lost/failed device without restarting** the standalone app is not yet
  possible; a restart renegotiates devices automatically.
- **Linux (ALSA) and macOS (CoreAudio) support** builds and passes the automated test suite in CI
  on both platforms, but has not yet been verified against real hardware to the same extent as the
  Windows/WASAPI path, which has been.
- **There is no installer or packaged release yet**, on any platform — building from source is
  currently the only way to run Namir. On macOS this is a sharper limitation than elsewhere,
  because the `.clap` bundle described above has to be assembled by hand; a build step that
  produces it automatically is planned but not yet written.
- **The CLAP plugin's embedded interface works on Windows, macOS, and Linux** — it negotiates
  the platform-native GUI API (`WIN32`, `COCOA`, `X11`). The Windows path is the long-standing
  one; **macOS and Linux are new and have had little use** — macOS has been opened once in REAPER,
  and the Linux/X11 path has not yet been confirmed in a host at all. If your host shows its own
  generic parameter panel instead of Namir's screen, that is the host's fallback rather than a
  broken install, the standalone is unaffected, and it is worth reporting.
- **On Linux, Namir's window needs X11 — a Wayland-only session will not open one.** Both the
  standalone and the plugin's interface draw through a windowing library whose only Unix backend is
  X11, so on a Wayland desktop they rely on XWayland, which most distributions install by default.
  If XWayland is absent, the standalone starts audio and then fails to open its window. There is no
  workaround from Namir's side today; installing your distribution's XWayland package is the fix.
  A machine with no display at all — a headless build server, or a terminal-only session — cannot
  run either product for the same reason.

An earlier revision of this guide told macOS users to rename `libnamir_clap.dylib` to `Namir.clap`,
which does not work — that instruction was wrong and has been corrected above.
