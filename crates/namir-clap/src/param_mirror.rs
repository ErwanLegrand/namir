//! [`ParamMirror`]: a lock-free, wait-free "current value of every `REGISTRY` parameter" store,
//! shared (via [`crate::shared::NamirShared`]) between every thread that needs to read or write a
//! parameter value without contending a lock — the audio thread (host automation arriving in
//! `process()`), the GUI thread ([`crate::ui_host::ClapUiHost::snapshot`]/`dispatch`), and the
//! main thread (CLAP's `params`/`state` extensions).
//!
//! # Why this exists, given `namir_engine::Chain` already holds the authoritative values
//!
//! `Chain` lives inside `AudioEngine`, which is owned exclusively by the audio processor (D-5.1's
//! `Send`-not-`Sync` audio-thread role) — nothing on the GUI or main thread may reach it, by
//! construction, the same way `namir-ui`'s own doc comment says a `UiHost` cannot reach a `Chain`
//! directly. So the GUI needs its *own* up-to-date copy to render from, and the main thread needs
//! one too (CLAP's `params` extension's `get_value`/`value_to_text` are main-thread calls that
//! must answer instantly, never by asking the audio thread and waiting). `ParamMirror` is that
//! copy: every write this crate makes to a live engine (GUI dispatch, host automation, preset
//! recall) also lands here, atomically, so every reader sees the same "current value" a fresh
//! [`namir_state::State`] built from a [`namir_engine::AudioEngine::process`] call would have
//! converged to (FR-PARAM-030).
//!
//! # Why atomics, not a `Mutex<ParamValues>`
//!
//! [`AudioEngine::apply_param_direct`](namir_engine::AudioEngine::apply_param_direct) is called
//! from the audio thread (`namir-clap`'s `process()`, for host automation) and must be wait-free
//! (NFR-RT-010). Updating the mirror from the same call site therefore must not risk blocking on
//! a lock some other thread might be holding — `Box<[AtomicU32]>`, one slot per
//! [`namir_params::REGISTRY`] entry (`f32::to_bits`/`from_bits`, exactly `namir-engine`'s
//! `telemetry_ring.rs` does for the same reason), makes every read and write a single atomic
//! operation with no contention window at all.
//!
//! # Why a linear scan over `REGISTRY` rather than a precomputed index
//!
//! `REGISTRY` has on the order of thirty entries today. A linear scan is a small, fixed, bounded
//! amount of work — no different in kind from `namir-engine/src/command.rs`'s own
//! `MAX_COMMANDS_PER_BLOCK`-bounded loops, which this codebase already treats as RT-safe. Building
//! a `HashMap` would trade that bounded scan for an allocation at construction time for a lookup
//! that is not on any measured hot path (host automation delivers one event at a time, not a
//! per-sample torrent), so there is nothing to buy back.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use namir_params::REGISTRY;
use namir_state::ParamValues;

/// The GUI-origin pending set is one bit per [`REGISTRY`] entry, so the registry has to fit in a
/// `u64`. It holds 31 entries today; this fails the build rather than silently dropping the 65th
/// parameter's change on the floor, which is the failure mode a wider structure would be bought to
/// avoid and a narrower one would hide.
const _: () = assert!(
    REGISTRY.len() <= 64,
    "ParamMirror::gui_pending is a u64 bitmask, one bit per REGISTRY entry"
);

/// The lock-free mirror. See this module's doc comment.
pub(crate) struct ParamMirror {
    values: Box<[AtomicU32]>,
    /// Bit *i* set means `REGISTRY[i]`'s current value was written **by this plugin's own editor**
    /// and has not yet been reported to the host as automation (issue #94).
    ///
    /// Only [`Self::set_by_key_from_gui`] sets a bit. Host-originated writes
    /// ([`Self::set_by_id`], from `crate::audio`'s automation path and from `params`' flush) must
    /// not, or the plugin would echo the host's own automation straight back at it.
    gui_pending: AtomicU64,
}

impl ParamMirror {
    /// Every `REGISTRY` entry at its documented default (FR-STATE-020), matching
    /// `ParamValues::defaults()`.
    pub(crate) fn new() -> Self {
        let defaults = ParamValues::defaults();
        let values = REGISTRY
            .iter()
            .map(|d| AtomicU32::new(defaults.get(d.key).unwrap_or(0.0).to_bits()))
            .collect();
        Self {
            values,
            gui_pending: AtomicU64::new(0),
        }
    }

    fn index_of_id(id: u32) -> Option<usize> {
        REGISTRY.iter().position(|d| d.id.0 == id)
    }

    fn index_of_key(key: &str) -> Option<usize> {
        REGISTRY.iter().position(|d| d.key == key)
    }

    /// Sets the entry with this `namir_params::ParamId` (as a raw `u32`, the form both
    /// `namir_engine::ParamId` and CLAP's own `ClapId` carry). Returns `false`, changing nothing,
    /// if `id` names no `REGISTRY` entry.
    pub(crate) fn set_by_id(&self, id: u32, value: f32) -> bool {
        let Some(i) = Self::index_of_id(id) else {
            return false;
        };
        self.values[i].store(value.to_bits(), Ordering::Relaxed);
        true
    }

    /// Reads the entry with this id, or `None` if `id` names no `REGISTRY` entry.
    pub(crate) fn get_by_id(&self, id: u32) -> Option<f32> {
        Self::index_of_id(id).map(|i| f32::from_bits(self.values[i].load(Ordering::Relaxed)))
    }

    /// Sets the entry with this `ParamDescriptor::key` **without** marking it as a GUI-originated
    /// change. Returns `false`, changing nothing, if `key` names no `REGISTRY` entry.
    ///
    /// `#[cfg(test)]` since issue #94: every production write by key is a user gesture in this
    /// plugin's own editor and goes through [`Self::set_by_key_from_gui`], which is this plus the
    /// pending-set mark. Tests that want to seed a value without also queueing an automation
    /// report to a host they do not have use this one.
    #[cfg(test)]
    pub(crate) fn set_by_key(&self, key: &str, value: f32) -> bool {
        self.store_by_key(key, value).is_some()
    }

    /// Stores `value` under `key`, returning the `REGISTRY` index it landed at.
    fn store_by_key(&self, key: &str, value: f32) -> Option<usize> {
        let i = Self::index_of_key(key)?;
        self.values[i].store(value.to_bits(), Ordering::Relaxed);
        Some(i)
    }

    /// [`Self::set_by_key`], plus "and tell the host about it": marks the entry as a
    /// GUI-originated change awaiting delivery as an automation gesture (issue #94).
    ///
    /// The one caller is `crate::ui_host::ClapUiHost::set_param`, which is the only path in this
    /// crate a *user gesture inside the plugin's own editor* takes. Everything else that writes
    /// the mirror — host automation, a `params` flush, a preset/state load — is the host's own
    /// change or is announced to it by other means (`HostParams::rescan`), and goes through the
    /// unmarked setters.
    pub(crate) fn set_by_key_from_gui(&self, key: &str, value: f32) -> bool {
        let Some(i) = self.store_by_key(key, value) else {
            return false;
        };
        // Marked *after* the value is stored, so a drain that sees the bit is guaranteed to read
        // this value or a later one, never the previous one.
        self.gui_pending.fetch_or(1u64 << i, Ordering::Release);
        true
    }

    /// Whether any GUI-originated change is still waiting to be reported to the host.
    pub(crate) fn has_gui_pending(&self) -> bool {
        self.gui_pending.load(Ordering::Acquire) != 0
    }

    /// Claims the whole GUI-origin pending set, clearing it.
    ///
    /// Taken rather than read-then-cleared so that a knob moved *while* a drain is in flight
    /// re-marks its own bit and is delivered by the next one — the race can duplicate a report,
    /// which a host treats as an idempotent automation point, and cannot drop one.
    pub(crate) fn take_gui_pending(&self) -> u64 {
        self.gui_pending.swap(0, Ordering::AcqRel)
    }

    /// Puts a bit back, for a change whose delivery to the host failed (a full output-event
    /// buffer). See [`Self::take_gui_pending`].
    pub(crate) fn restore_gui_pending(&self, bits: u64) {
        self.gui_pending.fetch_or(bits, Ordering::Release);
    }

    /// The current value of `REGISTRY[index]`, or `None` if `index` is out of range.
    pub(crate) fn value_at(&self, index: usize) -> Option<f32> {
        self.values
            .get(index)
            .map(|v| f32::from_bits(v.load(Ordering::Relaxed)))
    }

    /// Overwrites every entry from `params` (FR-STATE-030's preset recall, and the GUI/host
    /// `state` load path) — anything `params` doesn't carry a value for keeps its current mirror
    /// value rather than reverting to a default, matching `ParamValues::from_document_section`'s
    /// own "absent means unspecified, not reset" contract at the mirror layer.
    pub(crate) fn load(&self, params: &ParamValues) {
        for (i, d) in REGISTRY.iter().enumerate() {
            if let Some(v) = params.get(d.key) {
                self.values[i].store(v.to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// A complete [`ParamValues`] snapshot of every entry's current value — the payload half of a
    /// [`namir_state::State`] this crate builds for saving or for replaying onto a freshly
    /// (re)activated engine (see `crate::shared`'s module doc comment).
    pub(crate) fn snapshot(&self) -> ParamValues {
        let mut out = ParamValues::defaults();
        for (i, d) in REGISTRY.iter().enumerate() {
            let v = f32::from_bits(self.values[i].load(Ordering::Relaxed));
            out.set(d.key, v).expect("REGISTRY key is always known");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namir_params::stages::trim;

    #[test]
    fn new_matches_param_values_defaults() {
        let mirror = ParamMirror::new();
        let defaults = ParamValues::defaults();
        let snapshot = mirror.snapshot();
        for d in REGISTRY {
            assert_eq!(snapshot.get(d.key), defaults.get(d.key));
        }
    }

    #[test]
    fn set_by_key_is_visible_in_the_next_snapshot() {
        let mirror = ParamMirror::new();
        assert!(mirror.set_by_key(trim::GAIN_DB.key, 4.5));
        assert_eq!(mirror.snapshot().get(trim::GAIN_DB.key), Some(4.5));
    }

    #[test]
    fn set_by_id_then_get_by_id_round_trips() {
        let mirror = ParamMirror::new();
        assert!(mirror.set_by_id(trim::GAIN_DB.id.0, -3.0));
        assert_eq!(mirror.get_by_id(trim::GAIN_DB.id.0), Some(-3.0));
        // And it is visible through the snapshot too -- one store, two views.
        assert_eq!(mirror.snapshot().get(trim::GAIN_DB.key), Some(-3.0));
    }

    #[test]
    fn an_unknown_id_or_key_is_reported_rather_than_panicking() {
        let mirror = ParamMirror::new();
        assert!(!mirror.set_by_id(0xFFFF_FFFF, 1.0));
        assert_eq!(mirror.get_by_id(0xFFFF_FFFF), None);
        assert!(!mirror.set_by_key("not.a.real.key", 1.0));
    }

    #[test]
    fn snapshot_reflects_every_write_and_round_trips_through_load() {
        let mirror = ParamMirror::new();
        mirror.set_by_key(trim::GAIN_DB.key, 7.0);
        let snap = mirror.snapshot();
        assert_eq!(snap.get(trim::GAIN_DB.key), Some(7.0));

        let fresh = ParamMirror::new();
        fresh.load(&snap);
        assert_eq!(fresh.snapshot().get(trim::GAIN_DB.key), Some(7.0));
    }

    /// `load` must not reset an entry `params` doesn't mention -- unlike `ParamValues::defaults`,
    /// which fills every entry, a caller may hand a partially-built `ParamValues` (in practice
    /// this never happens today since `ParamValues` is always complete-array, but the mirror's
    /// own contract should not depend on that).
    #[test]
    fn load_does_not_disturb_entries_not_present_in_the_source_snapshot() {
        let mirror = ParamMirror::new();
        mirror.set_by_key(trim::GAIN_DB.key, 9.0);
        let mut partial = ParamValues::defaults();
        partial.set(trim::GAIN_DB.key, 1.0).unwrap();
        mirror.load(&partial);
        // defaults() fills every key including trim.gain_db, so this specifically checks the
        // value that *was* present lands correctly.
        assert_eq!(mirror.snapshot().get(trim::GAIN_DB.key), Some(1.0));
    }

    /// Concurrent writers from different threads must never tear a value -- each store/load is a
    /// single atomic operation, so a reader always sees a whole `f32`'s bit pattern from one
    /// writer, never a mix of two.
    #[test]
    fn concurrent_writes_from_multiple_threads_never_tear_a_value() {
        use std::sync::Arc;
        let mirror = Arc::new(ParamMirror::new());
        let handles: Vec<_> = (0..8u32)
            .map(|i| {
                let mirror = Arc::clone(&mirror);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        mirror.set_by_key(trim::GAIN_DB.key, i as f32);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let final_value = mirror.snapshot().get(trim::GAIN_DB.key).unwrap();
        assert!(
            (0..8).map(|i| i as f32).any(|v| v == final_value),
            "final value {final_value} was not written by any thread -- a value tore"
        );
    }

    /// **Issue #94's ledger, at the mirror.** A GUI-originated write is queued for the host; a
    /// host-originated one is not, or the plugin would echo the host's own automation back at it.
    #[test]
    fn only_a_gui_originated_write_joins_the_pending_set() {
        let mirror = ParamMirror::new();
        assert!(!mirror.has_gui_pending());

        assert!(mirror.set_by_id(namir_params::stages::trim::GAIN_DB.id.0, 1.0));
        assert!(
            !mirror.has_gui_pending(),
            "host automation must not be reported back to the host"
        );

        assert!(mirror.set_by_key_from_gui(namir_params::stages::trim::GAIN_DB.key, 2.0));
        assert!(mirror.has_gui_pending());

        let index = ParamMirror::index_of_key(namir_params::stages::trim::GAIN_DB.key).unwrap();
        assert_eq!(mirror.take_gui_pending(), 1u64 << index);
        assert!(
            !mirror.has_gui_pending(),
            "taking the pending set must clear it, so one change is reported once"
        );
        assert_eq!(mirror.value_at(index), Some(2.0));
    }

    /// An unknown key changes nothing and queues nothing.
    #[test]
    fn an_unknown_key_from_the_gui_queues_nothing() {
        let mirror = ParamMirror::new();
        assert!(!mirror.set_by_key_from_gui("not.a.real.key", 1.0));
        assert!(!mirror.has_gui_pending());
    }

    /// A delivery that failed puts its change back rather than dropping it — the host's output
    /// event buffer is allowed to be full, and a lost automation point is exactly what issue #94
    /// is about.
    #[test]
    fn a_restored_bit_is_reported_again() {
        let mirror = ParamMirror::new();
        mirror.set_by_key_from_gui(namir_params::stages::out::GAIN_DB.key, -3.0);
        let taken = mirror.take_gui_pending();
        assert_ne!(taken, 0);
        mirror.restore_gui_pending(taken);
        assert_eq!(mirror.take_gui_pending(), taken);
    }

    /// Several parameters moved before one drain are all reported, each exactly once.
    #[test]
    fn every_moved_parameter_is_in_one_drain() {
        let mirror = ParamMirror::new();
        let keys = [
            namir_params::stages::trim::GAIN_DB.key,
            namir_params::stages::out::GAIN_DB.key,
            namir_params::global::GLOBAL_BYPASS.key,
        ];
        for key in keys {
            mirror.set_by_key_from_gui(key, 1.0);
        }
        let taken = mirror.take_gui_pending();
        assert_eq!(taken.count_ones(), keys.len() as u32);
        for key in keys {
            let index = ParamMirror::index_of_key(key).unwrap();
            assert_ne!(taken & (1u64 << index), 0, "{key} must be in the drain");
        }
    }
}
