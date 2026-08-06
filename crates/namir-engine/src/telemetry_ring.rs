//! D-7.3's real outbound ring: "atomics and a lock-free telemetry ring, read at UI frame rate.
//! Loss is acceptable outbound and the buffer overwrites oldest."
//!
//! # Why this needs no dependency, when `ring.rs` did
//!
//! **Decision:** an array of `AtomicU64` plus one `AtomicU64` write sequence, with a
//! [`TelemetryEntry`] packed into exactly 64 bits. No external crate, no `unsafe`.
//!
//! **Rationale:** a `TelemetryEntry` is `{ id: u32, value: f32 }` — exactly 64 bits. Packing it
//! into a single `AtomicU64` makes tearing *within* an entry impossible by construction rather than
//! by protocol: a store or load of one `AtomicU64` is indivisible, so no reader can ever observe
//! one entry's `id` beside another's `value`. Tearing *across* entries remains possible and is
//! explicitly acceptable — D-7.3 says loss is fine outbound — and because every word is
//! self-describing (it carries its own `id`), a reader that gets lapped mid-drain still observes a
//! set of individually-valid readings, never a fabricated one. That property is what makes the
//! dependency-free design safe *here*; it would not hold for a wider record, which is exactly why
//! `ring.rs`'s command/return rings, whose elements are pointers with destructors, could not be
//! built this way.
//!
//! **Consequence:** requires 64-bit atomics. Both of D-18.1's mobile targets
//! (`aarch64-linux-android`, `aarch64-apple-ios`) are 64-bit, so this costs nothing today; the
//! `const` assertion below turns a future 32-bit port into a compile error naming the reason
//! rather than a mystery.
//!
//! **Consequence:** the producer is not `Clone`, which makes "single producer" a fact the type
//! system enforces rather than a comment. The reader *is* `Clone`, and each clone carries its own
//! cursor, so any number of UI readers is safe.
//!
//! **Alternatives rejected:** a second `rtrb` ring — rtrb is reliable-delivery SPSC, so a full ring
//! would either block the audio thread's write or drop the *newest* reading, when D-7.3 wants the
//! *oldest* dropped. A seqlock per entry — two atomics and two fences per reading, to buy a
//! guarantee the 64-bit packing already gives for free. `Mutex<Vec<_>>` — P1.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::telemetry::TelemetryEntry;

const _: () = assert!(
    cfg!(target_has_atomic = "64"),
    "D-7.3's telemetry ring packs a TelemetryEntry into one AtomicU64; this target has no 64-bit \
     atomics. See this module's doc comment."
);

/// `id` in the high half, `value`'s bit pattern in the low half.
const fn pack(entry: TelemetryEntry) -> u64 {
    ((entry.id as u64) << 32) | (entry.value.to_bits() as u64)
}

const fn unpack(word: u64) -> TelemetryEntry {
    TelemetryEntry {
        id: (word >> 32) as u32,
        value: f32::from_bits(word as u32),
    }
}

struct Inner {
    /// Length is a power of two, so the index wrap is a mask rather than a division.
    slots: Box<[AtomicU64]>,
    mask: usize,
    /// Total entries ever written. Monotonic, and only the audio thread ever stores to it.
    write_seq: AtomicU64,
}

/// The audio thread's write end. Deliberately not `Clone` — see this module's doc comment.
pub struct TelemetryProducer {
    inner: Arc<Inner>,
}

/// A UI-side read end. Cloneable; each clone tracks its own position, so readers never interfere.
#[derive(Clone)]
pub struct TelemetryReader {
    inner: Arc<Inner>,
    cursor: u64,
}

/// What one [`TelemetryReader::drain`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryDrain {
    /// How many entries were copied out.
    pub read: usize,
    /// How many entries were overwritten before this reader reached them. D-7.3 accepts the loss,
    /// but a persistently nonzero value means the UI is draining more slowly than the ring can
    /// hold — a real tuning fact, worth surfacing rather than hiding.
    pub missed: u64,
}

/// Allocates a telemetry ring holding at least `capacity` entries (rounded up to a power of two)
/// and splits it into its two ends.
///
/// **Not RT-safe** — the one allocation, made once at preparation time.
pub fn telemetry_ring(capacity: usize) -> (TelemetryProducer, TelemetryReader) {
    let capacity = capacity.max(1).next_power_of_two();
    let slots = (0..capacity).map(|_| AtomicU64::new(0)).collect::<Vec<_>>();
    let inner = Arc::new(Inner {
        slots: slots.into_boxed_slice(),
        mask: capacity - 1,
        write_seq: AtomicU64::new(0),
    });
    (
        TelemetryProducer {
            inner: Arc::clone(&inner),
        },
        TelemetryReader { inner, cursor: 0 },
    )
}

impl TelemetryProducer {
    /// Publishes one reading, overwriting the oldest if the ring is full.
    ///
    /// Wait-free (NFR-RT-020): one relaxed load of a value only this thread writes, one relaxed
    /// store, one release store. No compare-and-swap, no loop, and no branch on another thread's
    /// progress — which is what "wait-free from the audio thread's side" actually requires, as
    /// distinct from merely lock-free.
    pub fn push(&mut self, entry: TelemetryEntry) {
        // Sole writer, so a relaxed load of our own counter is sufficient.
        let seq = self.inner.write_seq.load(Ordering::Relaxed);
        self.inner.slots[(seq as usize) & self.inner.mask].store(pack(entry), Ordering::Relaxed);
        // Release: the slot's store must be visible to any reader that observes this bump.
        self.inner
            .write_seq
            .store(seq.wrapping_add(1), Ordering::Release);
    }
}

impl TelemetryReader {
    /// Copies up to `out.len()` entries into `out`, oldest first, advancing this reader's cursor.
    ///
    /// Never blocks the audio thread and never blocks itself. A slot can be overwritten between
    /// this call reading the write sequence and reading that slot; the result is that a *newer*
    /// valid reading appears in an older position. That is precisely the across-entry tearing D-7.3
    /// permits, and the entry is still internally consistent — so [`TelemetryDrain::missed`] is a
    /// close lower bound on what was lost, not an exact count, and is documented as such rather
    /// than presented as precise.
    pub fn drain(&mut self, out: &mut [TelemetryEntry]) -> TelemetryDrain {
        let end = self.inner.write_seq.load(Ordering::Acquire);
        let capacity = self.inner.slots.len() as u64;
        // Anything older than this has already been overwritten.
        let oldest_live = end.saturating_sub(capacity);
        let missed = oldest_live.saturating_sub(self.cursor);
        let mut seq = self.cursor.max(oldest_live);
        let mut read = 0;
        while seq < end && read < out.len() {
            out[read] =
                unpack(self.inner.slots[(seq as usize) & self.inner.mask].load(Ordering::Acquire));
            read += 1;
            seq += 1;
        }
        self.cursor = seq;
        TelemetryDrain { read, missed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt_harness::audio_section;

    /// The claim the whole design rests on: an entry survives a round trip through 64 bits
    /// bit-exactly, for every shape of `id` and `value` — including NaN, the infinities and
    /// negative zero, which a naive `f32` comparison would get wrong.
    #[test]
    fn packing_round_trips_bit_exactly_including_nan_and_negative_zero() {
        let ids = [
            0u32,
            1,
            0x7FFF_FFFF,
            u32::MAX,
            namir_params::ParamId::from_key("telemetry.nam.loaded").0,
        ];
        let values = [
            0.0f32,
            -0.0,
            1.0,
            -1.0,
            f32::MIN,
            f32::MAX,
            f32::MIN_POSITIVE,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
        ];
        for id in ids {
            for value in values {
                let entry = TelemetryEntry { id, value };
                let back = unpack(pack(entry));
                assert_eq!(back.id, entry.id, "id round trip failed for {id}");
                assert_eq!(
                    back.value.to_bits(),
                    entry.value.to_bits(),
                    "value round trip failed for {value}"
                );
            }
        }
    }

    /// If `TelemetryEntry` ever grows past 64 bits this module's core safety argument — that
    /// tearing within an entry is impossible — silently stops holding. This is the tripwire.
    #[test]
    fn an_entry_is_exactly_sixty_four_bits() {
        assert_eq!(size_of::<TelemetryEntry>(), 8);
    }

    #[test]
    fn entries_are_read_oldest_first_and_the_cursor_advances() {
        let (mut tx, mut rx) = telemetry_ring(8);
        for i in 0..5u32 {
            tx.push(TelemetryEntry {
                id: i,
                value: i as f32,
            });
        }
        let mut out = [TelemetryEntry { id: 0, value: 0.0 }; 8];
        let drain = rx.drain(&mut out);
        assert_eq!(drain.read, 5);
        assert_eq!(drain.missed, 0);
        for (i, entry) in out.iter().take(5).enumerate() {
            assert_eq!(entry.id, i as u32);
        }
        // Nothing new since: a second drain reads nothing rather than repeating.
        assert_eq!(rx.drain(&mut out).read, 0);
    }

    /// D-7.3: "loss is acceptable outbound and the buffer overwrites oldest." The *newest*
    /// entries must survive, not the oldest — an outbound ring that dropped new readings would
    /// leave the UI showing stale meters, which is the opposite of what the decision wants.
    #[test]
    fn overflow_overwrites_oldest_and_reports_what_was_missed() {
        let (mut tx, mut rx) = telemetry_ring(4);
        for i in 0..10u32 {
            tx.push(TelemetryEntry {
                id: i,
                value: i as f32,
            });
        }
        let mut out = [TelemetryEntry { id: 0, value: 0.0 }; 8];
        let drain = rx.drain(&mut out);
        assert_eq!(drain.read, 4, "only the ring's capacity survives");
        assert_eq!(drain.missed, 6);
        for (i, id) in (6..10u32).enumerate() {
            assert_eq!(out[i].id, id, "the four newest entries should survive");
        }
    }

    #[test]
    fn readers_have_independent_cursors() {
        let (mut tx, mut rx1) = telemetry_ring(8);
        let mut rx2 = rx1.clone();
        tx.push(TelemetryEntry { id: 7, value: 1.0 });
        let mut out = [TelemetryEntry { id: 0, value: 0.0 }; 8];
        assert_eq!(rx1.drain(&mut out).read, 1);
        assert_eq!(rx1.drain(&mut out).read, 0);
        // The second reader has not consumed anything yet and still sees it.
        assert_eq!(rx2.drain(&mut out).read, 1);
        assert_eq!(out[0].id, 7);
    }

    /// NFR-RT-010/D-7.5: the audio thread's side of D-7.3 allocates nothing.
    #[test]
    fn publishing_does_not_allocate() {
        let (mut tx, _rx) = telemetry_ring(16);
        audio_section(|| {
            for i in 0..1_000u32 {
                tx.push(TelemetryEntry {
                    id: i,
                    value: i as f32,
                });
            }
        });
    }

    /// The across-entry tearing D-7.3 permits must never become *within*-entry tearing. Every
    /// entry written here satisfies `value == id as f32`; a reader racing a writer may miss
    /// entries, but every entry it does observe must still satisfy that invariant. A torn read
    /// would pair one entry's id with another's value and fail it.
    #[test]
    fn a_lapped_reader_never_observes_a_fabricated_entry() {
        use std::sync::atomic::{AtomicBool, Ordering as O};

        let (mut tx, mut rx) = telemetry_ring(16);
        let done = Arc::new(AtomicBool::new(false));
        let done_reader = Arc::clone(&done);

        let reader = std::thread::spawn(move || {
            let mut out = [TelemetryEntry { id: 0, value: 0.0 }; 16];
            let mut seen = 0u64;
            while !done_reader.load(O::Acquire) {
                let drain = rx.drain(&mut out);
                for entry in out.iter().take(drain.read) {
                    assert_eq!(
                        entry.value, entry.id as f32,
                        "observed a fabricated entry: id {} paired with value {}",
                        entry.id, entry.value
                    );
                    seen += 1;
                }
            }
            seen
        });

        for i in 0..200_000u32 {
            tx.push(TelemetryEntry {
                id: i,
                value: i as f32,
            });
        }
        done.store(true, O::Release);
        let seen = reader.join().expect("reader thread should not panic");
        // Loss is expected and fine; observing nothing at all would mean the test proved nothing.
        assert!(seen > 0, "the reader observed no entries at all");
    }

    #[test]
    fn both_ends_are_send_and_the_reader_is_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<TelemetryProducer>();
        assert_send::<TelemetryReader>();
        assert_sync::<TelemetryReader>();
    }
}
