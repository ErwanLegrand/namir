//! [`UiHost`]: the seam D-5.1 requires between this crate's pure view layer and whatever crate
//! actually owns a live `namir_engine::Chain`, a `namir_worker` instance, and a real
//! `namir_library::Index` on disk -- `namir-app` and `namir-clap`, both built on top of this
//! crate. See this crate's top doc comment for the full architectural rationale; this module is
//! just the trait and the plain data types that cross it.
//!
//! # The shape of one frame
//!
//! Every rendered frame, in order:
//! 1. [`UiHost::snapshot`] is called once, first, producing a [`UiSnapshot`] -- a complete,
//!    read-only picture of everything FR-UI-020's screen shows, as of right now.
//! 2. The screen is rendered from that snapshot. Nothing rendered this frame can ever be stale by
//!    more than one frame's worth of host-side change, and nothing the view renders can itself
//!    change engine state -- only [`UiIntent`]s can.
//! 3. Every [`UiIntent`] the user actually triggered this frame (zero, on a frame with no
//!    interaction) is handed to [`UiHost::dispatch`], one call per intent, in the order the
//!    controls that produced them were laid out.

use std::path::PathBuf;
use std::sync::Arc;

use namir_core::ErrorCode;
use namir_library::{Index, ScanProgress};
use namir_state::ParamValues;

/// One audio meter's current reading, already converted to dB by the host. `namir-ui` never
/// touches a raw audio sample -- D-5.1 forbids this crate from depending on `namir-engine` (the
/// only crate that produces one) at all, so a meter reading can only ever arrive pre-computed,
/// through this struct.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterReading {
    /// Peak level, in dBFS. `f32::NEG_INFINITY` represents digital silence.
    pub peak_db: f32,
    /// RMS level, in dBFS. `f32::NEG_INFINITY` represents digital silence.
    pub rms_db: f32,
}

impl MeterReading {
    /// Digital silence on both the peak and RMS readings -- the sensible value before a host has
    /// ever reported a real one.
    pub const SILENT: MeterReading = MeterReading {
        peak_db: f32::NEG_INFINITY,
        rms_db: f32::NEG_INFINITY,
    };
}

impl Default for MeterReading {
    fn default() -> Self {
        Self::SILENT
    }
}

/// FR-UI-070's non-modal notice: one catalogue-backed ([`ErrorCode`]) message, plus the free-text
/// detail the code's `message_template` expects, plus a caller-assigned `id` this crate never
/// interprets -- it exists purely so [`crate::UiIntent::DismissNotice`] can name exactly the
/// notice a dismiss gesture dismissed, even after the list has since grown or reordered.
#[derive(Debug, Clone, PartialEq)]
pub struct UiNotice {
    /// Identifies this notice among whatever else is currently displayed. Opaque to this crate;
    /// the host is free to use a counter, a hash, or anything else that stays stable for as long
    /// as the notice is shown.
    pub id: u64,
    /// Which catalogue entry (FR-ERR-020) this notice reports.
    pub code: ErrorCode,
    /// Free-text context -- typically the file or device name FR-UI-070 requires be stated.
    pub detail: String,
}

/// What the host currently knows about the library, for [`crate::library_view`] to render.
///
/// `index` is an `Arc` specifically so a host holding a real, possibly >=10,000-entry
/// `namir_library::Index` can hand a fresh reference to this crate every single frame at the cost
/// of one atomic refcount bump -- never a clone of the index's contents. See
/// [`crate::library_view`]'s module doc comment for why that distinction is what makes FR-UI-060
/// achievable at all: a >=10,000-entry deep clone on every frame at 60fps would itself be the kind
/// of "expensive per-frame work" FR-UI-060 forbids, entirely independent of how the list is drawn.
#[derive(Clone)]
pub struct LibrarySnapshot {
    /// The library index as it stands right now. Never mutated by this crate -- filtering
    /// ([`namir_library::filter`]) only ever reads it.
    pub index: Arc<Index>,
    /// The in-progress scan's last-reported progress, or `None` if no scan is running.
    pub scan: Option<ScanProgress>,
}

impl Default for LibrarySnapshot {
    fn default() -> Self {
        Self {
            index: Arc::new(Index::empty()),
            scan: None,
        }
    }
}

/// Which share mode the audio device a host is running against is actually open in — FR-IO-020's
/// two WASAPI modes, as seen from the view layer.
///
/// A separate declaration from `namir_app::audio_io::ShareMode` rather than a re-export of it,
/// because D-5.1 forbids this crate from depending on `namir-app` at all (the dependency runs the
/// other way). The host performs the one-line conversion; see
/// `crates/namir-app/src/host.rs`'s `From` impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioShareMode {
    /// The device is shared with other applications on the system.
    Shared,
    /// The host process holds the device exclusively.
    Exclusive,
}

/// FR-IO-020's mode indicator: what the audio device **actually** opened as, never what was asked
/// for. `docs/03-implementation-roadmap.md` §18 rules out "a mode indicator that lies", so a host
/// that requested exclusive mode and was refused reports [`AudioShareMode::Shared`] here — the
/// refusal itself arrives separately, as a [`UiNotice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioModeStatus {
    /// The mode actually granted.
    pub share_mode: AudioShareMode,
    /// The device this names, as the host names it. The share mode is settled once per session
    /// across both directions (`namir-app` will not run an exclusive input against a shared
    /// output), so naming one device is a display choice about *which* name to show, not a claim
    /// that the mode applies to that device only — `namir-app` passes its output device.
    pub device_name: String,
}

/// Everything [`crate::render`] needs to draw one frame of FR-UI-020's screen -- a single,
/// self-contained, read-only picture of engine/library/preset state at one instant. Built fresh by
/// [`UiHost::snapshot`] every frame; this crate never retains one past the frame it was rendered
/// with, and never mutates it.
#[derive(Clone)]
pub struct UiSnapshot {
    /// Every `namir_params::REGISTRY` entry's current value, including `global.bypass`/
    /// `global.output_ceiling_db` (D-10.4) alongside every stage's own parameters -- the same
    /// complete-array shape `namir_state::State::params` already carries, reused here rather than
    /// re-declared, per this crate's instruction to drive every control from the registry.
    pub params: ParamValues,
    /// FR-UI-020's input meter.
    pub input_meter: MeterReading,
    /// FR-UI-020's output meter.
    pub output_meter: MeterReading,
    /// FR-UI-020's "the loaded model's name" -- `None` when no model is loaded.
    pub loaded_model_name: Option<String>,
    /// FR-UI-020's "the loaded IR's name" -- `None` when no IR is loaded.
    pub loaded_ir_name: Option<String>,
    /// The library-browsing surface's current state.
    pub library: LibrarySnapshot,
    /// FR-IO-020's mode indicator, or `None` when this host does not own an audio device to have
    /// a share mode for — which is the ordinary case for `namir-clap`, where the CLAP host owns
    /// the device and the plugin never opens one.
    pub audio_mode: Option<AudioModeStatus>,
    /// Whether the current in-memory state differs from what was last saved/recalled -- the
    /// `namir_state::State`-relative "dirty" concept this crate surfaces but never computes
    /// itself (the host is the one crate that can see both the live state and the last-saved
    /// `State` to compare against).
    pub unsaved_changes: bool,
    /// FR-UI-070's non-modal notices currently shown, oldest first.
    pub notices: Vec<UiNotice>,
}

impl Default for UiSnapshot {
    /// Every parameter at its documented default (mirroring `namir_state::State::defaults`), no
    /// meters, nothing loaded, an empty library, no audio-mode indicator, no notices -- the state a
    /// freshly opened window should render before its first real [`UiHost::snapshot`] call, and
    /// what every test in this crate starts from.
    fn default() -> Self {
        Self {
            params: ParamValues::defaults(),
            input_meter: MeterReading::default(),
            output_meter: MeterReading::default(),
            loaded_model_name: None,
            loaded_ir_name: None,
            library: LibrarySnapshot::default(),
            audio_mode: None,
            unsaved_changes: false,
            notices: Vec::new(),
        }
    }
}

/// One user-originated action from a single frame's interaction, ready for [`UiHost::dispatch`].
/// This is the *only* way this crate ever asks for a change to real engine, worker or library
/// state -- rendering itself never has a side effect beyond producing these.
#[derive(Debug, Clone, PartialEq)]
pub enum UiIntent {
    /// Set `key` (a `namir_params::ParamDescriptor::key`) to `value`, in that parameter's own
    /// unit -- the same convention `namir_state::ParamValues::set` uses. Emitted by
    /// [`crate::controls::param_control`] when its control changes, including a global-bypass
    /// toggle, since D-10.4 makes that an ordinary registry entry rather than a special case.
    SetParam {
        /// The parameter's stable string key.
        key: &'static str,
        /// The new value, in the parameter's own unit/step-index space.
        value: f32,
    },
    /// Reset `key` to its `ParamDescriptor`'s documented default (FR-UI-050's reset gesture).
    ResetParamToDefault {
        /// The parameter's stable string key.
        key: &'static str,
    },
    /// The library search box's text changed to this value.
    LibraryQueryChanged(String),
    /// The user asked to load the library entry at this path (FR-UI-050-adjacent: this is
    /// `namir-library`'s FR-LIB-060 "select" gesture, wired to a double-click in
    /// [`crate::library_view`]).
    LoadLibraryEntry(PathBuf),
    /// The user asked the host to (re)start a library scan.
    RescanLibraryRequested,
    /// The user asked the host to cancel an in-progress library scan.
    CancelScanRequested,
    /// The user dismissed the notice with this id (FR-UI-070).
    DismissNotice {
        /// The dismissed [`UiNotice`]'s `id`.
        id: u64,
    },
}

/// D-5.1's seam: implemented by whatever crate owns the real engine/worker/library underneath this
/// one -- `namir-app` and `namir-clap`, both built on top of `namir-ui`. See this crate's top doc
/// comment and this module's doc comment for the full contract each frame follows.
///
/// `Send` because the window this crate opens runs its own event loop (via `egui-baseview`'s
/// `WindowHandler`, which requires its state to be `'static + Send`) and a host is part of that
/// state.
pub trait UiHost: Send {
    /// A fresh, complete view of everything FR-UI-020's screen needs, as of right now. Called
    /// first, every frame, before any of that frame's user interaction is processed.
    fn snapshot(&mut self) -> UiSnapshot;

    /// Applies one [`UiIntent`] from this frame's interaction. Called once per intent actually
    /// triggered this frame -- zero times on a frame with no interaction, never batched, so a host
    /// that forwards each one straight onto a real command channel needs no batching logic of its
    /// own.
    fn dispatch(&mut self, intent: UiIntent);
}

/// A minimal [`UiHost`] a test can drive directly, recording every dispatched intent so a test
/// can assert exactly what a widget emitted. `pub(crate)` (not nested in `mod tests`) so
/// `controls.rs`'s and `library_view.rs`'s own tests can reuse it too, rather than each declaring
/// its own mock.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordingHost {
    pub(crate) snapshot: UiSnapshot,
    pub(crate) dispatched: Vec<UiIntent>,
}

#[cfg(test)]
impl UiHost for RecordingHost {
    fn snapshot(&mut self) -> UiSnapshot {
        self.snapshot.clone()
    }

    fn dispatch(&mut self, intent: UiIntent) {
        self.dispatched.push(intent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_reading_default_is_silent() {
        assert_eq!(MeterReading::default(), MeterReading::SILENT);
        assert_eq!(MeterReading::SILENT.peak_db, f32::NEG_INFINITY);
    }

    #[test]
    fn library_snapshot_default_is_an_empty_index_with_no_scan() {
        let snapshot = LibrarySnapshot::default();
        assert_eq!(snapshot.index.len(), 0);
        assert!(snapshot.scan.is_none());
    }

    #[test]
    fn ui_snapshot_default_matches_param_values_defaults() {
        let snapshot = UiSnapshot::default();
        assert_eq!(snapshot.params, ParamValues::defaults());
        assert!(snapshot.loaded_model_name.is_none());
        assert!(snapshot.loaded_ir_name.is_none());
        assert!(snapshot.audio_mode.is_none());
        assert!(!snapshot.unsaved_changes);
        assert!(snapshot.notices.is_empty());
    }

    /// FR-IO-020's indicator is `Option`al because "no audio device of my own" is a real state a
    /// host can be in (`namir-clap`), and is distinct from "shared mode" -- a default snapshot must
    /// not claim a mode nobody granted.
    #[test]
    fn a_default_snapshot_claims_no_share_mode_rather_than_claiming_shared() {
        assert_eq!(UiSnapshot::default().audio_mode, None);
        assert_ne!(
            UiSnapshot::default().audio_mode,
            Some(AudioModeStatus {
                share_mode: AudioShareMode::Shared,
                device_name: String::new(),
            })
        );
    }

    #[test]
    fn recording_host_records_every_dispatched_intent_in_order() {
        let mut host = RecordingHost::default();
        host.dispatch(UiIntent::RescanLibraryRequested);
        host.dispatch(UiIntent::SetParam {
            key: "trim.gain_db",
            value: 3.0,
        });
        assert_eq!(
            host.dispatched,
            vec![
                UiIntent::RescanLibraryRequested,
                UiIntent::SetParam {
                    key: "trim.gain_db",
                    value: 3.0
                },
            ]
        );
    }
}
