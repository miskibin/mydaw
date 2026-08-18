//! Pre-allocated audio buffers handed to [`RtNode::process`](crate::RtNode).

/// Number of channels in the Phase 0 engine. Stereo only, on purpose: a fixed
/// channel count keeps the buffer layout allocation-free and the graph trivial.
/// Widening this is a Phase 2+ concern (see the mixer spec).
pub const CHANNELS: usize = 2;

/// Stereo scratch buffers, allocated once up front to `max_block` frames.
///
/// # RT safety
/// Every buffer is allocated in [`AudioBuffers::new`], which runs on a non-RT
/// thread. Nothing in this type allocates, reallocates or frees afterwards:
/// [`set_frames`](Self::set_frames) only shortens the *view*, never the storage.
pub struct AudioBuffers {
    channels: [Vec<f32>; CHANNELS],
    max_block: usize,
    /// Frames valid for the current block; always `<= max_block`.
    frames: usize,
}

impl AudioBuffers {
    /// Allocate buffers for up to `max_block` frames per channel. Non-RT.
    pub fn new(max_block: usize) -> Self {
        Self {
            channels: [vec![0.0; max_block], vec![0.0; max_block]],
            max_block,
            frames: max_block,
        }
    }

    /// Maximum block size the buffers were allocated for.
    pub fn max_block(&self) -> usize {
        self.max_block
    }

    /// Frames valid in the current block.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Set the frame count for the block about to be processed.
    ///
    /// RT-safe: clamps to `max_block` instead of growing the storage. A host
    /// asking for more than `max_block` is a bug, but silently truncating is
    /// vastly preferable to allocating on the audio thread.
    pub fn set_frames(&mut self, frames: usize) {
        self.frames = frames.min(self.max_block);
    }

    /// Mutable per-channel slices, truncated to the current frame count.
    pub fn as_mut_slices(&mut self) -> [&mut [f32]; CHANNELS] {
        let frames = self.frames;
        let (left, right) = self.channels.split_at_mut(1);
        [&mut left[0][..frames], &mut right[0][..frames]]
    }

    /// Immutable per-channel slices, truncated to the current frame count.
    pub fn as_slices(&self) -> [&[f32]; CHANNELS] {
        [
            &self.channels[0][..self.frames],
            &self.channels[1][..self.frames],
        ]
    }

    /// Zero the active region of every channel. RT-safe.
    pub fn silence(&mut self) {
        for ch in self.as_mut_slices() {
            ch.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_clamped_to_max_block() {
        let mut b = AudioBuffers::new(64);
        b.set_frames(999);
        assert_eq!(b.frames(), 64);
        b.set_frames(16);
        assert_eq!(b.frames(), 16);
        assert_eq!(b.as_mut_slices()[0].len(), 16);
    }

    #[test]
    fn silence_clears_active_region_only() {
        let mut b = AudioBuffers::new(8);
        for ch in b.as_mut_slices() {
            ch.fill(1.0);
        }
        b.set_frames(4);
        b.silence();
        b.set_frames(8);
        assert_eq!(b.as_slices()[0], &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
    }
}
