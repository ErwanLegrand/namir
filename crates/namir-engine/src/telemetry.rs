//! D-7.3's real design is a lock-free ring, written by the audio thread and read at UI frame
//! rate, overwriting oldest on overflow because outbound loss is acceptable. That ring is out of
//! scope for this task — this module is only the trait-facing shape `Stage::telemetry` needs
//! (the `TelemetrySink` parameter), backed by a caller-provided fixed buffer so a `Stage` can
//! write into it without allocating. Wiring an actual lock-free structure that the UI thread
//! reads from is future work (D-7.3), tracked alongside the D-7.2 command ring and the D-8.1
//! handover protocol, neither of which exist yet either.

/// One numeric telemetry sample: `id` identifies which signal (meter level, gate reduction,
/// fault code, xrun count, ...). No string or formatted message travels with it — D-16.2 puts
/// all formatting and allocation off the audio thread, so the audio side writes only numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TelemetryEntry {
    /// Which signal this sample is (meter level, gate reduction, fault code, xrun count, ...).
    pub id: u32,
    /// The sample's numeric value; interpretation depends on `id`.
    pub value: f32,
}

/// Overwrites the oldest entry once full (D-7.3: "loss is acceptable outbound"). Backed by a
/// caller-owned buffer it never grows, so `Stage::telemetry` (RT) cannot reach an allocator
/// through this type even if it tried.
pub struct TelemetrySink<'a> {
    buffer: &'a mut [TelemetryEntry],
    len: usize,
    next: usize,
}

impl<'a> TelemetrySink<'a> {
    /// Wraps `buffer` as an initially-empty ring; capacity is fixed to `buffer.len()` for the
    /// sink's lifetime.
    pub fn new(buffer: &'a mut [TelemetryEntry]) -> Self {
        Self {
            buffer,
            len: 0,
            next: 0,
        }
    }

    /// A zero-capacity sink (an empty buffer) silently discards — consistent with "loss is
    /// acceptable outbound", and it means a stage never needs to check capacity before writing.
    pub fn push(&mut self, entry: TelemetryEntry) {
        if self.buffer.is_empty() {
            return;
        }
        self.buffer[self.next] = entry;
        self.next = (self.next + 1) % self.buffer.len();
        self.len = (self.len + 1).min(self.buffer.len());
    }

    /// Number of entries currently held, up to `capacity`.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no entries have been pushed yet.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The fixed capacity `buffer` was constructed with.
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Oldest-to-newest order.
    pub fn entries(&self) -> impl Iterator<Item = TelemetryEntry> + '_ {
        let start = if self.len < self.buffer.len() {
            0
        } else {
            self.next
        };
        (0..self.len).map(move |i| self.buffer[(start + i) % self.buffer.len()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_is_visible_via_entries() {
        let mut storage = [TelemetryEntry { id: 0, value: 0.0 }; 4];
        let mut sink = TelemetrySink::new(&mut storage);
        sink.push(TelemetryEntry { id: 1, value: 0.5 });
        sink.push(TelemetryEntry { id: 2, value: -1.0 });
        let got: Vec<_> = sink.entries().collect();
        assert_eq!(
            got,
            vec![
                TelemetryEntry { id: 1, value: 0.5 },
                TelemetryEntry { id: 2, value: -1.0 },
            ]
        );
    }

    #[test]
    fn overflow_overwrites_oldest() {
        let mut storage = [TelemetryEntry { id: 0, value: 0.0 }; 2];
        let mut sink = TelemetrySink::new(&mut storage);
        sink.push(TelemetryEntry { id: 1, value: 1.0 });
        sink.push(TelemetryEntry { id: 2, value: 2.0 });
        sink.push(TelemetryEntry { id: 3, value: 3.0 }); // overwrites id 1
        let got: Vec<_> = sink.entries().collect();
        assert_eq!(
            got,
            vec![
                TelemetryEntry { id: 2, value: 2.0 },
                TelemetryEntry { id: 3, value: 3.0 },
            ]
        );
        assert_eq!(sink.len(), 2);
        assert_eq!(sink.capacity(), 2);
    }

    #[test]
    fn zero_capacity_sink_never_panics() {
        let mut storage: [TelemetryEntry; 0] = [];
        let mut sink = TelemetrySink::new(&mut storage);
        sink.push(TelemetryEntry { id: 1, value: 1.0 });
        assert!(sink.is_empty());
    }
}
