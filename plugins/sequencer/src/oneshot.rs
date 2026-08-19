//! A one-shot sample player: the rack-drop counterpart of an audio clip.
//!
//! An audio clip plays from the playlist timeline. This node plays the same
//! kind of file from **notes**: a step, a piano-roll hit, or the computer
//! keyboard, at C4 untransposed, with the channel's pitch cents folded in.
//! Region start/end is the SMP crop, shared with the sample window.
//!
//! Voices are the clip player's cubic interpolator and 2 ms ramps, minus the
//! song. RT-safe: array reads and multiplies, no allocation.

use std::sync::Arc;

use mydaw_audio_file::{cubic, taps};
use mydaw_rt_core::{NoteKind, ProcessCtx, RtNode, CHANNELS};

use crate::audio::AudioSource;
use crate::clips::{pitch_ratio, playback_step, ramp_step, CLIP_RAMP_MS};

/// Voices one sampler channel may hold at once.
pub const ONESHOT_VOICES: usize = 8;

/// MIDI key at which the file plays at recorded pitch (C4).
pub const ONESHOT_ROOT_KEY: u8 = 60;

/// A note waiting behind a steal's fade-out. See [`OneShotNode::start`].
#[derive(Clone, Copy)]
struct Pending {
    key: u8,
    step: f64,
    gain: f32,
    env_step: f32,
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    key: u8,
    position: f64,
    step: f64,
    gain: f32,
    env: f32,
    env_step: f32,
    releasing: bool,
    /// Note-ons seen when this voice started, so "oldest" is a number.
    age: u64,
    pending: Option<Pending>,
}

impl Voice {
    const fn idle() -> Self {
        Self {
            active: false,
            key: 0,
            position: 0.0,
            step: 1.0,
            gain: 0.0,
            env: 0.0,
            env_step: 0.0,
            releasing: false,
            age: 0,
            pending: None,
        }
    }

    /// Hand the slot to the note waiting behind a steal's fade-out, or free it.
    /// Returns whether the voice is still sounding.
    fn finish(&mut self, start: f64) -> bool {
        let Some(pending) = self.pending.take() else {
            self.active = false;
            return false;
        };
        self.key = pending.key;
        self.position = start;
        self.step = pending.step;
        self.gain = pending.gain;
        self.env = pending.env_step.min(1.0);
        self.env_step = pending.env_step;
        self.releasing = false;
        true
    }
}

/// Note-driven player of one loaded audio file.
pub struct OneShotNode {
    source: Arc<AudioSource>,
    start_frames: u64,
    end_frames: u64,
    pitch_cents: f32,
    voices: [Voice; ONESHOT_VOICES],
    /// Note-ons seen, so "oldest" is a number rather than a guess.
    next_age: u64,
}

impl OneShotNode {
    pub fn new(
        source: Arc<AudioSource>,
        start_frames: u64,
        end_frames: u64,
        pitch_cents: f32,
    ) -> Self {
        Self {
            source,
            start_frames,
            end_frames,
            pitch_cents,
            voices: [Voice::idle(); ONESHOT_VOICES],
            next_age: 0,
        }
    }

    fn playable_end(&self) -> u64 {
        let frames = self.source.frames() as u64;
        if self.end_frames == 0 || self.end_frames > frames {
            frames
        } else {
            self.end_frames
        }
    }

    fn start(&mut self, key: u8, velocity: f32, sample_rate: f64, ramp: f32) {
        let start = self.start_frames;
        let end = self.playable_end();
        if start >= end || self.source.frames() == 0 {
            return;
        }
        let semitones = f64::from(key) - f64::from(ONESHOT_ROOT_KEY);
        let step = playback_step(self.source.sample_rate(), sample_rate)
            * pitch_ratio(self.pitch_cents)
            * 2f64.powf(semitones / 12.0);
        if step <= 0.0 {
            return;
        }
        let gain = velocity.clamp(0.0, 1.0);
        let age = self.next_age;
        self.next_age = self.next_age.wrapping_add(1);
        let Some(slot) = self.voices.iter().position(|voice| !voice.active) else {
            // Full pool: a steal starts the fade rather than cutting (the clip
            // player's rule). The oldest voice fades out over the ramp and the
            // note starts from silence when it gets there -- at most 2 ms late,
            // which is not a click.
            let mut oldest = 0;
            for (index, voice) in self.voices.iter().enumerate() {
                if voice.age < self.voices[oldest].age {
                    oldest = index;
                }
            }
            let voice = &mut self.voices[oldest];
            voice.releasing = true;
            voice.env_step = voice.env_step.max(ramp);
            // A burst of steals must rotate through the pool rather than
            // overwrite one slot's pending note again and again.
            voice.age = age;
            voice.pending = Some(Pending {
                key,
                step,
                gain,
                env_step: ramp,
            });
            return;
        };
        self.voices[slot] = Voice {
            active: true,
            key,
            position: start as f64,
            step,
            gain,
            env: ramp.min(1.0),
            env_step: ramp,
            releasing: false,
            age,
            pending: None,
        };
    }

    fn release_key(&mut self, key: u8, ramp: f32) {
        for voice in &mut self.voices {
            if voice.active && voice.key == key && !voice.releasing {
                voice.releasing = true;
                voice.env_step = voice.env_step.max(ramp);
            }
        }
    }

    /// Sum every sounding voice into `left`/`right`.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len();
        if frames == 0 {
            return;
        }
        let start = self.start_frames as f64;
        let end = self.playable_end() as f64;
        let padded = self.source.padded();

        for voice in &mut self.voices {
            if !voice.active {
                continue;
            }
            for frame in 0..frames {
                if (voice.position >= end || voice.position < 0.0) && !voice.finish(start) {
                    break;
                }
                let index = voice.position as usize;
                let fraction = (voice.position - voice.position.floor()) as f32;
                let gain = voice.gain * voice.env;
                left[frame] += cubic(taps(padded, index, 0), fraction) * gain;
                if CHANNELS > 1 {
                    right[frame] += cubic(taps(padded, index, 1), fraction) * gain;
                }
                if voice.releasing {
                    voice.env -= voice.env_step;
                    if voice.env <= 0.0 {
                        voice.env = 0.0;
                        // A steal's pending note begins where the fade reached
                        // silence, so the handover is a fade, not a cut.
                        if !voice.finish(start) {
                            break;
                        }
                        continue;
                    }
                } else if voice.env < 1.0 {
                    voice.env = (voice.env + voice.env_step).min(1.0);
                }
                voice.position += voice.step;
            }
        }
    }
}

impl RtNode for OneShotNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        let ramp = ramp_step(CLIP_RAMP_MS, ctx.sample_rate);
        let frames = ctx.frames;
        if frames == 0 {
            return;
        }
        let sample_rate = ctx.sample_rate;
        let events = ctx.events;
        let [left, right] = ctx.buffers.as_mut_slices();

        // Split at every event offset, so a note at frame `n` starts at frame
        // `n` and not at the top of the block -- and a note-on and note-off in
        // one block still leave the frames between them sounding.
        let mut cursor = 0_usize;
        for event in events {
            let at = (event.frame_offset as usize).min(frames);
            if at > cursor {
                self.render(&mut left[cursor..at], &mut right[cursor..at]);
                cursor = at;
            }
            match event.kind {
                NoteKind::NoteOn { key, velocity } => {
                    self.start(key, velocity, sample_rate, ramp);
                }
                NoteKind::NoteOff { key } => self.release_key(key, ramp),
                _ => {}
            }
        }
        self.render(&mut left[cursor..frames], &mut right[cursor..frames]);
    }
}

impl std::fmt::Debug for OneShotNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OneShotNode")
            .field("source", &self.source.id())
            .field("start_frames", &self.start_frames)
            .field("end_frames", &self.end_frames)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_rt_core::{AudioBuffers, NoteEvent, PlayMode, TransportSnapshot};

    fn source(frames: usize) -> Arc<AudioSource> {
        Arc::new(AudioSource::from_stereo(
            1,
            std::path::Path::new("hit.wav"),
            48_000.0,
            vec![0.5; frames * CHANNELS],
        ))
    }

    /// One block of `frames` at 48 kHz, returning the left channel.
    fn run(node: &mut OneShotNode, frames: usize, events: &[NoteEvent]) -> Vec<f32> {
        let mut buffers = AudioBuffers::new(frames);
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            48_000.0,
            frames,
            TransportSnapshot {
                sample_pos: 0,
                playing: true,
                bpm: 120.0,
                mode: PlayMode::Pattern,
            },
            events,
        );
        node.process(&mut ctx);
        buffers.as_slices()[0][..frames].to_vec()
    }

    #[test]
    fn a_note_on_makes_the_file_audible() {
        let mut node = OneShotNode::new(source(4_000), 0, 0, 0.0);
        let mut buffers = AudioBuffers::new(64);
        let events = [NoteEvent::note_on(0, ONESHOT_ROOT_KEY, 1.0)];
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            48_000.0,
            64,
            TransportSnapshot {
                sample_pos: 0,
                playing: true,
                bpm: 120.0,
                mode: PlayMode::Pattern,
            },
            &events,
        );
        node.process(&mut ctx);
        let left = buffers.as_slices()[0];
        assert!(
            left.iter().any(|s| *s > 0.0),
            "a note-on must start the sample"
        );
    }

    #[test]
    fn the_region_start_skips_the_head_of_the_file() {
        let mut data = vec![0.0; 200 * CHANNELS];
        for i in 50..200 {
            data[i * CHANNELS] = 1.0;
            data[i * CHANNELS + 1] = 1.0;
        }
        let source = Arc::new(AudioSource::from_stereo(
            1,
            std::path::Path::new("region.wav"),
            48_000.0,
            data,
        ));
        let mut node = OneShotNode::new(source, 50, 0, 0.0);
        let mut buffers = AudioBuffers::new(128);
        let events = [NoteEvent::note_on(0, ONESHOT_ROOT_KEY, 1.0)];
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            48_000.0,
            128,
            TransportSnapshot {
                sample_pos: 0,
                playing: true,
                bpm: 120.0,
                mode: PlayMode::Pattern,
            },
            &events,
        );
        node.process(&mut ctx);
        // The 2 ms fade-in has to finish before the file's 1.0 is on the bus.
        assert!(
            buffers.as_slices()[0][80] > 0.4,
            "playback starts at the region, not the silent head of the file"
        );
    }

    #[test]
    fn a_note_starts_at_its_frame_offset_and_not_at_the_start_of_the_block() {
        let mut node = OneShotNode::new(source(4_000), 0, 0, 0.0);
        let out = run(
            &mut node,
            128,
            &[NoteEvent::note_on(32, ONESHOT_ROOT_KEY, 1.0)],
        );
        assert!(
            out[..32].iter().all(|s| *s == 0.0),
            "nothing may sound before the event's frame"
        );
        assert!(
            out[32..].iter().any(|s| *s > 0.0),
            "the note is missing after its frame"
        );
    }

    #[test]
    fn a_note_on_and_off_in_one_block_still_sounds_the_samples_onset() {
        let mut node = OneShotNode::new(source(4_000), 0, 0, 0.0);
        let out = run(
            &mut node,
            256,
            &[
                NoteEvent::note_on(0, ONESHOT_ROOT_KEY, 1.0),
                NoteEvent::note_off(64, ONESHOT_ROOT_KEY),
            ],
        );
        // 64 frames of 2 ms fade-in reach about two thirds of the 0.5 sample.
        assert!(
            out[..64].iter().any(|s| *s > 0.2),
            "the onset must be audible before the note-off's frame"
        );
        // And the note-off is a fade, not nothing: the block ends in silence.
        assert_eq!(out[255], 0.0, "the release must have finished");
    }

    #[test]
    fn a_steal_fades_the_oldest_voice_instead_of_cutting_it() {
        let ramp = ramp_step(CLIP_RAMP_MS, 48_000.0);
        let mut node = OneShotNode::new(source(48_000), 0, 0, 0.0);

        // Fill the pool and let every fade-in finish (2 ms is 96 frames).
        let fills: Vec<NoteEvent> = (0..ONESHOT_VOICES)
            .map(|_| NoteEvent::note_on(0, ONESHOT_ROOT_KEY, 1.0))
            .collect();
        run(&mut node, 256, &fills);
        let settled = run(&mut node, 256, &[]);

        // A ninth note steals. Over the boundary and the whole steal block, no
        // adjacent-sample step may exceed what one voice's ramp allows -- a cut
        // of a full voice would be a 0.5 jump in one frame.
        let mut out = vec![*settled.last().expect("rendered")];
        out.extend(run(
            &mut node,
            512,
            &[NoteEvent::note_on(0, ONESHOT_ROOT_KEY, 1.0)],
        ));
        let limit = 0.5 * ramp * 1.5;
        for (index, pair) in out.windows(2).enumerate() {
            let jump = (pair[1] - pair[0]).abs();
            assert!(
                jump <= limit,
                "frame {index}: {jump} > {limit} -- that is a click"
            );
        }
        // And the stolen slot ends up playing the new note, not silence.
        let after = run(&mut node, 256, &[]);
        assert!(
            after.iter().all(|s| *s > 0.0),
            "the pending note must have taken the faded slot"
        );
    }
}
