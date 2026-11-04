//! Track head for an audio-file channel: playlist playback plus gated notes.
//!
//! An Audio Clip strip has to keep arranging from the playlist *and* answer
//! live / piano-roll notes. Both players sum into the same buffers, so a
//! sounding clip and a keyboard hit overlap instead of replacing each other.
//! The note player is the same fixed-size [`OneShotNode`] sampler channels use.

use mydaw_rt_core::{ProcessCtx, RtNode};

use crate::clips::AudioClipNode;
use crate::oneshot::OneShotNode;

/// Arrangement player plus note-driven sample player, sharing one strip.
pub struct SampleChannelHead {
    arrangement: AudioClipNode,
    notes: OneShotNode,
}

impl SampleChannelHead {
    pub fn new(arrangement: AudioClipNode, notes: OneShotNode) -> Self {
        Self { arrangement, notes }
    }
}

impl RtNode for SampleChannelHead {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        self.arrangement.process(ctx);
        self.notes.process(ctx);
    }
}

impl std::fmt::Debug for SampleChannelHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SampleChannelHead")
            .field("arrangement", &self.arrangement)
            .field("notes", &self.notes)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mydaw_rt_core::{
        AudioBuffers, GcThread, NoteEvent, PlayMode, ProcessCtx, TransportSnapshot, CHANNELS,
    };

    use super::*;
    use crate::audio::AudioSource;
    use crate::clips::{
        audio_clip_channel, CompiledAudioClip, CompiledAudioSong, AUDIO_CLIP_QUEUE_CAPACITY,
    };
    use crate::oneshot::{OneShotNode, SamplePlaybackParams};
    use crate::pattern::TICKS_PER_BAR;

    fn flat_source(value: f32, frames: usize) -> Arc<AudioSource> {
        Arc::new(AudioSource::from_stereo(
            1,
            std::path::Path::new("hit.wav"),
            48_000.0,
            vec![value; frames * CHANNELS],
        ))
    }

    #[test]
    fn a_playlist_clip_and_a_live_note_sum_on_the_same_channel() {
        let source = flat_source(0.4, 48_000);
        let song = CompiledAudioSong::new(vec![CompiledAudioClip {
            clip_id: 1,
            route: source.id(),
            source: Arc::clone(&source),
            start_ticks: 0,
            length_ticks: TICKS_PER_BAR as u64,
            offset_frames: 0,
            end_frames: 0,
            pitch_ratio: 1.0,
            gain: 1.0,
            fade_in_ms: 0.0,
            fade_out_ms: 0.0,
        }]);
        let gc = GcThread::spawn();
        let (mut handle, arrangement) = audio_clip_channel(AUDIO_CLIP_QUEUE_CAPACITY, gc.handle());
        handle.install(song).expect("installs");
        let notes = OneShotNode::with_params(
            source,
            Arc::new(SamplePlaybackParams::new(0, 0, 0.0, (0.0, 0.0))),
        );
        let mut node = SampleChannelHead::new(arrangement, notes);
        let mut buffers = AudioBuffers::new(512);
        let events = [NoteEvent::note_on(0, 60, 1.0)];
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            48_000.0,
            512,
            TransportSnapshot {
                sample_pos: 0,
                playing: true,
                bpm: 120.0,
                mode: PlayMode::Song,
            },
            &events,
        );
        node.process(&mut ctx);
        let left = buffers.as_slices()[0][..512].to_vec();
        let peak = left.iter().fold(0.0_f32, |acc, sample| acc.max(*sample));
        assert!(
            peak > 0.65,
            "playlist 0.4 plus a note of 0.4 must sum, peak was {peak}"
        );
    }
}
