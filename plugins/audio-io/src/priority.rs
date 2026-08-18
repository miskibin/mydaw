//! Real-time priority for the audio callback thread.
//!
//! `audio_thread_priority` hands back an opaque handle that must be kept for the
//! lifetime of the promotion and given back to demote. On Windows that handle
//! wraps a raw MMCSS `HANDLE`, which makes it `!Send` -- and the whole RT state
//! has to be `Send` for cpal to move the callback closure onto the audio thread.
//!
//! [`RealtimeToken`] is the (small, contained) `unsafe impl Send` that resolves
//! the conflict, plus the `Drop` that actually reverts the promotion.

use audio_thread_priority::{
    demote_current_thread_from_real_time, promote_current_thread_to_real_time, RtPriorityHandle,
};

/// Holds the real-time promotion for the thread that created it.
///
/// # Safety of the `Send` impl
/// The token is only ever *created* by [`RealtimeToken::promote`], which runs on
/// the audio thread itself, and only ever *used* (dropped) when the callback
/// closure is torn down -- which cpal does on that same audio thread. It crosses
/// a thread boundary exactly once: when the still-empty [`RealtimeToken::empty`]
/// value is moved into the closure at stream build time, before any promotion has
/// happened and while the payload is `None`.
///
/// In other words, the handle inside is never observed from a thread other than
/// the one it was made on. The `unsafe impl` encodes that invariant; the private
/// field is what keeps it true.
pub struct RealtimeToken {
    handle: Option<RtPriorityHandle>,
}

// SAFETY: see the type docs. The payload is `None` at the only point where the
// value crosses threads, and is created and dropped on the audio thread.
unsafe impl Send for RealtimeToken {}

impl RealtimeToken {
    /// An unpromoted token, safe to move to the audio thread.
    pub fn empty() -> Self {
        Self { handle: None }
    }

    /// Promote the *calling* thread. Returns `false` if the OS refused.
    ///
    /// A refusal is not fatal: audio keeps flowing at ordinary priority, it just
    /// becomes vulnerable to dropouts under load. The caller records that in
    /// [`crate::AudioIoStats`] rather than logging, because this runs on the audio
    /// thread.
    pub fn promote(&mut self, buffer_frames: u32, sample_rate_hz: u32) -> bool {
        match promote_current_thread_to_real_time(buffer_frames, sample_rate_hz) {
            Ok(handle) => {
                self.handle = Some(handle);
                true
            }
            Err(_) => false,
        }
    }

    /// Whether this thread is currently promoted.
    pub fn is_promoted(&self) -> bool {
        self.handle.is_some()
    }
}

impl Drop for RealtimeToken {
    fn drop(&mut self) {
        // Runs when the callback closure is dropped, which cpal does on the audio
        // thread during stream teardown -- the same thread that was promoted, as
        // the OS requires. Errors here are unactionable: the thread is going away.
        if let Some(handle) = self.handle.take() {
            let _ = demote_current_thread_from_real_time(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_token_is_inert() {
        let token = RealtimeToken::empty();
        assert!(!token.is_promoted());
        drop(token); // must not touch the OS
    }

    #[test]
    fn promotion_is_attempted_and_reverted_cleanly() {
        // Whether promotion succeeds depends on the machine (a CI container may
        // refuse), so this asserts the *contract*, not the outcome: promote() must
        // return a definite answer and drop must not panic either way.
        let mut token = RealtimeToken::empty();
        let promoted = token.promote(512, 48_000);
        assert_eq!(promoted, token.is_promoted());
    }
}
