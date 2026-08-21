//! A one-shot sample player: the rack-drop counterpart of an audio clip.
//!
//! An audio clip plays from the playlist timeline. This node plays the same
//! kind of file from **notes**: a step, a piano-roll hit, or the computer
//! keyboard, at C4 untransposed, with the channel's pitch cents folded in.
//! Region start/end is the SMP crop, shared with the sample window.
//!
//! Voices are the clip player's cubic interpolator and 2 ms ramps, minus the
//! song. Playback rate is channel pitch plus `(note - root key)` minus the
//! root's fine cents, so a detected F♯3 +12 ct plays concert F♯3 at MIDI 54.
//! RT-safe: array reads and multiplies, no allocation.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use mydaw_audio_file::{cubic, taps};
use mydaw_rt_core::{AtomicF32, NoteKind, ProcessCtx, RtNode, CHANNELS};

use crate::audio::AudioSource;
use crate::clips::{pitch_ratio, playback_step, ramp_step, CLIP_RAMP_MS};

/// Voices one sampler channel may hold at once.
pub const ONESHOT_VOICES: usize = 8;

/// MIDI key at which an uncalibrated file plays at recorded pitch (C4).
pub const ONESHOT_ROOT_KEY: u8 = 60;

/// Live defaults for one sample channel.
///
/// A note snapshots these atomics when it starts. Editing the channel therefore
/// changes the next hit without replacing the node or reshaping voices that are
/// already sounding. Region frames are packed as two `u32`s: loaded sources are
/// capped at ten minutes, well below that frame count even at supported rates.
#[derive(Debug)]
pub struct SamplePlaybackParams {
    region: AtomicU64,
    pitch_cents: AtomicF32,
    fades: AtomicU64,
    root_key: AtomicU32,
    root_fine_cents: AtomicF32,
}

#[derive(Clone, Copy)]
struct SamplePlaybackSnapshot {
    start_frames: u64,
    end_frames: u64,
    pitch_cents: f32,
    fades: (f32, f32),
    root_key: u8,
    root_fine_cents: f32,
}

impl SamplePlaybackParams {
    pub fn new(start_frames: u64, end_frames: u64, pitch_cents: f32, fades: (f32, f32)) -> Self {
        Self::with_root(
            start_frames,
            end_frames,
            pitch_cents,
            fades,
            ONESHOT_ROOT_KEY,
            0.0,
        )
    }

    pub fn with_root(
        start_frames: u64,
        end_frames: u64,
        pitch_cents: f32,
        fades: (f32, f32),
        root_key: u8,
        root_fine_cents: f32,
    ) -> Self {
        Self {
            region: AtomicU64::new(pack_region(start_frames, end_frames)),
            pitch_cents: AtomicF32::new(sane_pitch(pitch_cents)),
            fades: AtomicU64::new(pack_fades(fades.0, fades.1)),
            root_key: AtomicU32::new(u32::from(sane_root_key(root_key))),
            root_fine_cents: AtomicF32::new(sane_root_fine(root_fine_cents)),
        }
    }

    pub fn set_region(&self, start_frames: u64, end_frames: u64) {
        self.region
            .store(pack_region(start_frames, end_frames), Ordering::Relaxed);
    }

    pub fn set_pitch_cents(&self, pitch_cents: f32) {
        self.pitch_cents.set(sane_pitch(pitch_cents));
    }

    pub fn set_fades(&self, fade_in_ms: f32, fade_out_ms: f32) {
        self.fades
            .store(pack_fades(fade_in_ms, fade_out_ms), Ordering::Relaxed);
    }

    pub fn set_root(&self, root_key: u8, root_fine_cents: f32) {
        self.root_key
            .store(u32::from(sane_root_key(root_key)), Ordering::Relaxed);
        self.root_fine_cents.set(sane_root_fine(root_fine_cents));
    }

    fn snapshot(&self) -> SamplePlaybackSnapshot {
        let region = self.region.load(Ordering::Relaxed);
        let fades = self.fades.load(Ordering::Relaxed);
        SamplePlaybackSnapshot {
            start_frames: region >> 32,
            end_frames: region & u64::from(u32::MAX),
            pitch_cents: self.pitch_cents.get(),
            fades: (
                f32::from_bits((fades >> 32) as u32),
                f32::from_bits(fades as u32),
            ),
            root_key: sane_root_key(self.root_key.load(Ordering::Relaxed) as u8),
            root_fine_cents: sane_root_fine(self.root_fine_cents.get()),
        }
    }
}

fn pack_region(start_frames: u64, end_frames: u64) -> u64 {
    (start_frames.min(u64::from(u32::MAX)) << 32) | end_frames.min(u64::from(u32::MAX))
}

fn pack_fades(fade_in_ms: f32, fade_out_ms: f32) -> u64 {
    (u64::from(sane_fade(fade_in_ms).to_bits()) << 32) | u64::from(sane_fade(fade_out_ms).to_bits())
}

fn sane_pitch(cents: f32) -> f32 {
    if cents.is_finite() {
        cents.clamp(-1_200.0, 1_200.0)
    } else {
        0.0
    }
}

fn sane_fade(ms: f32) -> f32 {
    if ms.is_finite() {
        ms.max(0.0)
    } else {
        0.0
    }
}

fn sane_root_key(key: u8) -> u8 {
    key.min(127)
}

fn sane_root_fine(cents: f32) -> f32 {
    if cents.is_finite() {
        cents.clamp(-50.0, 50.0)
    } else {
        0.0
    }
}

/// A note waiting behind a steal's fade-out. See [`OneShotNode::start`].
#[derive(Clone, Copy)]
struct Pending {
    key: u8,
    start: f64,
    end: f64,
    step: f64,
    gain: f32,
    env_step: f32,
    fade_out_frames: f64,
}

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    key: u8,
    start: f64,
    end: f64,
    position: f64,
    step: f64,
    gain: f32,
    env: f32,
    env_step: f32,
    /// Source frames occupied by the channel's fade-out. The musical tail is a
    /// ceiling over the attack envelope, so overlapping fades meet instead of
    /// subtracting from each other and cancelling the voice.
    fade_out_frames: f64,
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
            start: 0.0,
            end: 0.0,
            position: 0.0,
            step: 1.0,
            gain: 0.0,
            env: 0.0,
            env_step: 0.0,
            fade_out_frames: 0.0,
            releasing: false,
            age: 0,
            pending: None,
        }
    }

    /// Hand the slot to the note waiting behind a steal's fade-out, or free it.
    /// Returns whether the voice is still sounding.
    fn finish(&mut self) -> bool {
        let Some(pending) = self.pending.take() else {
            self.active = false;
            return false;
        };
        self.key = pending.key;
        self.start = pending.start;
        self.end = pending.end;
        self.position = pending.start;
        self.step = pending.step;
        self.gain = pending.gain;
        self.env = pending.env_step.min(1.0);
        self.env_step = pending.env_step;
        self.fade_out_frames = pending.fade_out_frames;
        self.releasing = false;
        true
    }
}

/// Output frames a fade of `ms` covers. `0` when there is no fade.
#[inline]
fn fade_output_frames(ms: f32, sample_rate: f64) -> f64 {
    if !sample_rate.is_finite() || sample_rate <= 0.0 || !ms.is_finite() || ms <= 0.0 {
        return 0.0;
    }
    f64::from(ms) / 1_000.0 * sample_rate
}

/// Envelope step per output frame for a fade of `ms`. `1.0` when there is none,
/// which the caller floors with the anti-click ramp.
#[inline]
fn fade_rate(ms: f32, sample_rate: f64) -> f32 {
    let frames = fade_output_frames(ms, sample_rate);
    if frames < 1.0 {
        return 1.0;
    }
    (1.0 / frames) as f32
}

/// Note-driven player of one loaded audio file.
pub struct OneShotNode {
    source: Arc<AudioSource>,
    params: Arc<SamplePlaybackParams>,
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
        fades: (f32, f32),
    ) -> Self {
        Self::with_params(
            source,
            Arc::new(SamplePlaybackParams::new(
                start_frames,
                end_frames,
                pitch_cents,
                fades,
            )),
        )
    }

    pub fn with_params(source: Arc<AudioSource>, params: Arc<SamplePlaybackParams>) -> Self {
        Self {
            source,
            params,
            voices: [Voice::idle(); ONESHOT_VOICES],
            next_age: 0,
        }
    }

    fn playable_end(&self, requested: u64) -> u64 {
        let frames = self.source.frames() as u64;
        if requested == 0 || requested > frames {
            frames
        } else {
            requested
        }
    }

    fn start(&mut self, key: u8, velocity: f32, sample_rate: f64, ramp: f32) {
        let params = self.params.snapshot();
        let start = params.start_frames;
        let end = self.playable_end(params.end_frames);
        if start >= end || self.source.frames() == 0 {
            return;
        }
        let cents = params.pitch_cents + (f32::from(key) - f32::from(params.root_key)) * 100.0
            - params.root_fine_cents;
        let step = playback_step(self.source.sample_rate(), sample_rate) * pitch_ratio(cents);
        if step <= 0.0 {
            return;
        }
        let gain = velocity.clamp(0.0, 1.0);
        // A fade is a length of *time*, so it is that many output frames --
        // which is `step` times as many source frames, because a note played an
        // octave up walks the file twice as fast.
        let in_step = ramp.min(fade_rate(params.fades.0, sample_rate));
        let out_frames = fade_output_frames(params.fades.1, sample_rate) * step;
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
                start: start as f64,
                end: end as f64,
                step,
                gain,
                env_step: in_step,
                fade_out_frames: out_frames,
            });
            return;
        };
        self.voices[slot] = Voice {
            active: true,
            key,
            start: start as f64,
            end: end as f64,
            position: start as f64,
            step,
            gain,
            env: in_step.min(1.0),
            env_step: in_step,
            fade_out_frames: out_frames,
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
        let padded = self.source.padded();

        for voice in &mut self.voices {
            if !voice.active {
                continue;
            }
            for frame in 0..frames {
                if (voice.position >= voice.end || voice.position < 0.0) && !voice.finish() {
                    break;
                }
                let index = voice.position as usize;
                let fraction = (voice.position - voice.position.floor()) as f32;
                let tail = if voice.fade_out_frames > 0.0 {
                    ((voice.end - voice.position) / voice.fade_out_frames).clamp(0.0, 1.0) as f32
                } else {
                    1.0
                };
                let gain = voice.gain * voice.env.min(tail);
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
                        if !voice.finish() {
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
            .field("params", &self.params)
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
        let mut node = OneShotNode::new(source(4_000), 0, 0, 0.0, (0.0, 0.0));
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
        let mut node = OneShotNode::new(source, 50, 0, 0.0, (0.0, 0.0));
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
        let mut node = OneShotNode::new(source(4_000), 0, 0, 0.0, (0.0, 0.0));
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
        let mut node = OneShotNode::new(source(4_000), 0, 0, 0.0, (0.0, 0.0));
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
    fn overlapping_fades_do_not_cancel_a_short_pitched_sample() {
        // At +12 st this 100 ms source lasts 50 ms. Both requested fades are
        // much longer than that, so their curves overlap for the whole hit.
        // The attack and tail must meet in the middle instead of the fade-out
        // subtracting from the first tiny attack step and killing the voice.
        let mut node = OneShotNode::new(source(4_800), 0, 0, 1_200.0, (1_000.0, 1_000.0));
        let out = run(
            &mut node,
            2_500,
            &[NoteEvent::note_on(0, ONESHOT_ROOT_KEY, 1.0)],
        );
        let peak = out
            .iter()
            .fold(0.0_f32, |acc, sample| acc.max(sample.abs()));

        assert!(
            peak > 0.005,
            "overlapping fades must leave an audible hit, peak was {peak}"
        );
        assert!(
            out[0] < peak * 0.1,
            "the attack must still start near silence"
        );
        assert!(
            out[2_399].abs() < peak * 0.1,
            "the tail must still reach silence"
        );
    }

    #[test]
    fn live_parameter_edits_preserve_the_sounding_voice_and_apply_to_the_next() {
        let params = Arc::new(SamplePlaybackParams::new(0, 0, 0.0, (0.0, 0.0)));
        let mut node = OneShotNode::with_params(source(48_000), Arc::clone(&params));

        let attack = run(
            &mut node,
            256,
            &[NoteEvent::note_on(0, ONESHOT_ROOT_KEY, 1.0)],
        );
        assert!(attack[200] > 0.45, "the first voice must be sounding");

        params.set_pitch_cents(1_200.0);
        params.set_fades(1_000.0, 1_000.0);
        params.set_root(54, 12.0);
        let continuing = run(&mut node, 256, &[]);
        assert!(
            continuing[200] > 0.45,
            "editing defaults must not replace or reshape a voice already sounding"
        );

        let _ = run(&mut node, 256, &[NoteEvent::note_off(0, ONESHOT_ROOT_KEY)]);
        let next = run(
            &mut node,
            256,
            &[NoteEvent::note_on(0, ONESHOT_ROOT_KEY + 1, 1.0)],
        );
        assert!(
            next[200] < 0.01,
            "the next voice must use the newly requested long fade"
        );
    }

    #[test]
    fn a_steal_fades_the_oldest_voice_instead_of_cutting_it() {
        let ramp = ramp_step(CLIP_RAMP_MS, 48_000.0);
        let mut node = OneShotNode::new(source(48_000), 0, 0, 0.0, (0.0, 0.0));

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

    fn concert_hz(note: u8) -> f64 {
        440.0 * 2f64.powf((f64::from(note) - 69.0) / 12.0)
    }

    fn sine_source(hz: f64, frames: usize) -> Arc<AudioSource> {
        let rate = 48_000.0;
        let mut data = Vec::with_capacity(frames * CHANNELS);
        for frame in 0..frames {
            let value = ((frame as f64 / rate) * hz * std::f64::consts::TAU).sin() as f32 * 0.5;
            data.push(value);
            data.push(value);
        }
        Arc::new(AudioSource::from_stereo(
            1,
            std::path::Path::new("tone.wav"),
            rate,
            data,
        ))
    }

    fn estimate_hz(samples: &[f32], rate: f64) -> f64 {
        let mut crossings = 0_usize;
        for pair in samples.windows(2) {
            if pair[0] <= 0.0 && pair[1] > 0.0 {
                crossings += 1;
            }
        }
        crossings as f64 * rate / samples.len() as f64
    }

    #[test]
    fn a_detected_fs3_root_plays_concert_fs3_at_key_54() {
        let recorded = concert_hz(54) * 2f64.powf(12.0 / 1_200.0);
        let params = Arc::new(SamplePlaybackParams::with_root(
            0,
            0,
            0.0,
            (0.0, 0.0),
            54,
            12.0,
        ));
        let mut node = OneShotNode::with_params(sine_source(recorded, 48_000), params);
        let out = run(&mut node, 12_000, &[NoteEvent::note_on(0, 54, 1.0)]);
        let hz = estimate_hz(&out[400..], 48_000.0);
        let expected = concert_hz(54);
        assert!(
            (hz - expected).abs() < 2.0,
            "key 54 should play concert F#3 ({expected:.2} Hz), got {hz:.2}"
        );
    }

    #[test]
    fn channel_transpose_stacks_on_top_of_root_calibration() {
        let recorded = concert_hz(54) * 2f64.powf(12.0 / 1_200.0);
        let params = Arc::new(SamplePlaybackParams::with_root(
            0,
            0,
            100.0,
            (0.0, 0.0),
            54,
            12.0,
        ));
        let mut node = OneShotNode::with_params(sine_source(recorded, 48_000), params);
        let out = run(&mut node, 12_000, &[NoteEvent::note_on(0, 54, 1.0)]);
        let hz = estimate_hz(&out[400..], 48_000.0);
        let expected = concert_hz(55);
        assert!(
            (hz - expected).abs() < 2.5,
            "root-calibrated F#3 plus +100 ct should play G3 ({expected:.2} Hz), got {hz:.2}"
        );
    }
}
