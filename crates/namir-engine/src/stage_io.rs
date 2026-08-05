//! D-6.2: "`StageIo` carries scratch buffers owned by the chain, not by the stage, sized at
//! preparation time for the maximum block size the host declared." The struct is built around
//! borrowed slices specifically so `Stage::process` has no way to reach an allocator through it:
//! there is no owned buffer inside `StageIo` for it to grow, and every channel accessor hands
//! back a sub-slice of what was already borrowed in. Who backs that borrow (the host's own
//! buffer directly, when the host's block is within the declared maximum, or chain/adapter-owned
//! fallback storage when D-6.2's "process in slices" case applies) is a decision for whatever
//! drives `Chain::process` — out of scope here, since that caller doesn't exist yet.

/// Per-block audio for one `Stage::process` call. `frames` may be less than each channel
/// buffer's length (D-6.2: "a smaller block simply uses a prefix") but never more.
pub struct StageIo<'a> {
    channels: &'a mut [&'a mut [f32]],
    frames: usize,
}

impl<'a> StageIo<'a> {
    /// Panics if `frames` exceeds any channel's length. That's a call-site programming error —
    /// D-6.2 makes the *caller* responsible for slicing to at most the declared maximum before
    /// ever constructing this — not something that happens from untrusted or host-supplied data,
    /// so a panic here, off any `Stage`'s own RT path, is acceptable per D-16.3.
    pub fn new(channels: &'a mut [&'a mut [f32]], frames: usize) -> Self {
        for channel in channels.iter() {
            assert!(
                frames <= channel.len(),
                "StageIo: frames ({frames}) exceeds channel buffer length ({})",
                channel.len()
            );
        }
        Self { channels, frames }
    }

    /// Number of valid frames in this block; may be less than a channel buffer's own length
    /// (see this struct's doc comment) but never more.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Number of channels.
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// The `frames`-length prefix of channel `index`. Panics on an out-of-range `index`, the
    /// same call-site-error contract as slice indexing.
    pub fn channel(&mut self, index: usize) -> &mut [f32] {
        &mut self.channels[index][..self.frames]
    }

    /// The `frames`-length prefix of every channel, in channel order.
    pub fn channels_mut(&mut self) -> impl Iterator<Item = &mut [f32]> {
        let frames = self.frames;
        self.channels.iter_mut().map(move |c| &mut c[..frames])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_is_a_prefix_of_a_longer_buffer() {
        let mut left = [1.0f32, 2.0, 3.0, 4.0];
        let mut right = [5.0f32, 6.0, 7.0, 8.0];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 2);
        assert_eq!(io.frames(), 2);
        assert_eq!(io.channel(0), &[1.0, 2.0]);
        assert_eq!(io.channel(1), &[5.0, 6.0]);
    }

    #[test]
    #[should_panic(expected = "exceeds channel buffer length")]
    fn frames_beyond_buffer_length_panics() {
        let mut left = [1.0f32, 2.0];
        let mut channels: [&mut [f32]; 1] = [&mut left];
        let _ = StageIo::new(&mut channels, 3);
    }

    #[test]
    fn channels_mut_yields_all_channels() {
        let mut left = [1.0f32, 2.0];
        let mut right = [3.0f32, 4.0];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut io = StageIo::new(&mut channels, 2);
        for ch in io.channels_mut() {
            for s in ch.iter_mut() {
                *s *= 2.0;
            }
        }
        assert_eq!(io.channel(0), &[2.0, 4.0]);
        assert_eq!(io.channel(1), &[6.0, 8.0]);
    }

    #[test]
    fn channel_count_matches_input() {
        let mut left = [0.0f32; 2];
        let mut right = [0.0f32; 2];
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let io = StageIo::new(&mut channels, 2);
        assert_eq!(io.channel_count(), 2);
    }
}
