//! `cpal` gives this crate two independently-scheduled callbacks — an input one and an output
//! one, each running on its own OS-owned thread with no ordering guarantee relative to the other
//! (they may even be the *same* callback on a backend that supports full duplex on one stream, but
//! `cpal`'s cross-platform API always presents them as two `build_*_stream` calls, so this crate
//! treats them as independent throughout). [`namir_engine::AudioEngine::process`] needs one
//! contiguous input buffer per block, handed to it from whichever thread drives it — so something
//! has to carry captured samples from the input callback to wherever `process` actually runs.
//!
//! This module is that something: a wait-free SPSC ring ([`bridge`], built on the same `rtrb`
//! primitive `namir-engine`'s own command/return rings use — see that crate's `ring.rs` for the
//! full adoption argument, reused rather than re-litigated here) carrying raw captured `f32`
//! samples, plus the underrun accounting that *is* one of FR-IO-060's two xrun sources (the other
//! being `cpal`'s own `ErrorKind::Xrun`, handled in [`crate::audio_io`]).
//!
//! **Not RT-unsafe by construction, but written to be RT-cheap in practice:** `rtrb` in this
//! workspace's pinned version (0.3.4) has no bulk chunk-transfer API, so [`BridgeProducer`]/
//! [`BridgeConsumer`] push and pop one sample at a time. At the block sizes FR-IO-040 actually
//! exposes (tens to a few thousand frames), this is a bounded, allocation-free loop of atomic
//! operations — not the ring `namir-engine` uses for `Command`/`Resource` handover, which carries
//! far fewer, far larger elements per block and where per-item cost matters more.

use rtrb::RingBuffer;

/// The input callback's end: pushes captured samples in.
pub struct BridgeProducer {
    producer: rtrb::Producer<f32>,
}

/// The output callback's end: pulls samples out, padding and counting an underrun when the input
/// side hasn't produced enough yet.
pub struct BridgeConsumer {
    consumer: rtrb::Consumer<f32>,
}

/// Builds a fresh ring holding at least `capacity_frames` mono samples, split into its two ends.
/// Sized generously relative to one block (`crate::stream` sizes this at construction, not per
/// callback) so an ordinary scheduling jitter between the two callbacks does not itself cause an
/// underrun — only a genuine sustained stall does.
pub fn bridge(capacity_frames: usize) -> (BridgeProducer, BridgeConsumer) {
    let (producer, consumer) = RingBuffer::new(capacity_frames.max(1));
    (BridgeProducer { producer }, BridgeConsumer { consumer })
}

impl BridgeProducer {
    /// Pushes every sample in `captured`, in order. Returns how many did **not** fit because the
    /// ring was full — an overrun: the consumer side is draining slower than capture arrives,
    /// which under healthy operation (the consumer pulls once per output callback, at the same
    /// cadence audio flows) should not happen; a persistently nonzero return is the same kind of
    /// signal FR-IO-060 wants surfaced, and [`crate::stream`] feeds it into the same
    /// [`crate::xrun::XrunCounter`] an underrun does.
    pub fn push_captured(&mut self, captured: &[f32]) -> usize {
        let mut dropped = 0;
        for &sample in captured {
            if self.producer.push(sample).is_err() {
                dropped += 1;
            }
        }
        dropped
    }
}

impl BridgeConsumer {
    /// Fills every slot of `out` with the next captured sample, in order. Any slot for which no
    /// sample was yet available is set to `pad` instead (silence, in practice) — this is FR-IO-060's
    /// underrun: the input side has not produced enough since the last pull. Returns how many
    /// slots were padded (`0` means a clean pull, no dropout).
    pub fn pull_into(&mut self, out: &mut [f32], pad: f32) -> usize {
        let mut padded = 0;
        for slot in out.iter_mut() {
            match self.consumer.pop() {
                Ok(sample) => *slot = sample,
                Err(_) => {
                    *slot = pad;
                    padded += 1;
                }
            }
        }
        padded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordinary case: capture keeps up, every pulled sample is real, no underrun.
    #[test]
    fn a_full_pull_after_a_matching_push_reports_no_underrun() {
        let (mut producer, mut consumer) = bridge(64);
        let captured = [1.0f32, 2.0, 3.0, 4.0];
        assert_eq!(producer.push_captured(&captured), 0);

        let mut out = [0.0f32; 4];
        let padded = consumer.pull_into(&mut out, -1.0);
        assert_eq!(padded, 0);
        assert_eq!(out, captured);
    }

    /// FR-IO-060's core mechanism: pulling more than was ever pushed pads the shortfall and
    /// reports exactly how many frames were padded.
    #[test]
    fn pulling_more_than_was_pushed_pads_and_counts_the_shortfall() {
        let (mut producer, mut consumer) = bridge(64);
        producer.push_captured(&[1.0, 2.0]);

        let mut out = [0.0f32; 5];
        let padded = consumer.pull_into(&mut out, 0.0);
        assert_eq!(padded, 3);
        assert_eq!(out, [1.0, 2.0, 0.0, 0.0, 0.0]);
    }

    /// A ring that never received anything pads every requested frame.
    #[test]
    fn pulling_from_an_empty_ring_pads_everything() {
        let (_producer, mut consumer) = bridge(16);
        let mut out = [9.0f32; 3];
        let padded = consumer.pull_into(&mut out, 0.0);
        assert_eq!(padded, 3);
        assert_eq!(out, [0.0, 0.0, 0.0]);
    }

    /// Pushing more than the ring's capacity reports the overflow rather than silently losing it
    /// unaccounted-for.
    #[test]
    fn pushing_past_capacity_reports_the_dropped_count() {
        let (mut producer, _consumer) = bridge(4);
        let dropped = producer.push_captured(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(dropped, 2);
    }

    /// Across several push/pull cycles at matched rates, the ring stays healthy and every value
    /// arrives in order -- the steady-state condition an audio stream actually runs under.
    #[test]
    fn repeated_matched_push_pull_cycles_stay_in_order_with_no_underrun() {
        let (mut producer, mut consumer) = bridge(32);
        for block in 0..10u32 {
            let captured: Vec<f32> = (0..8).map(|i| (block * 8 + i) as f32).collect();
            producer.push_captured(&captured);
            let mut out = [0.0f32; 8];
            let padded = consumer.pull_into(&mut out, -1.0);
            assert_eq!(padded, 0);
            assert_eq!(out.to_vec(), captured);
        }
    }

    /// A partial pull followed by a later push-and-pull continues correctly -- the ring's cursor
    /// state after an underrun isn't corrupted by the padding.
    #[test]
    fn the_ring_recovers_cleanly_after_an_underrun() {
        let (mut producer, mut consumer) = bridge(32);
        producer.push_captured(&[1.0, 2.0]);
        let mut short = [0.0f32; 4];
        assert_eq!(consumer.pull_into(&mut short, 0.0), 2); // underrun: only 2 of 4 real

        producer.push_captured(&[3.0, 4.0, 5.0]);
        let mut next = [0.0f32; 3];
        assert_eq!(consumer.pull_into(&mut next, 0.0), 0);
        assert_eq!(next, [3.0, 4.0, 5.0]);
    }
}
