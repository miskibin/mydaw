//! Offline waveform of a built-in (or sampled) source, for the channel window.
//!
//! A hosted plugin draws its own editor; asking this for Analog Lab would be a
//! picture of silence with that plugin's name on it. Built-ins and the sampler
//! have no window of their own, so this is what "click Kick and see the sound"
//! actually shows: the samples the node produces for one hit, folded into
//! min/max bins the webview already knows how to draw.

use std::path::Path;

use mydaw_audio_file::{cubic, taps, CHANNELS};
use mydaw_plugin_sequencer::{pitch_ratio, AudioClipTune, AudioSource, MAX_WAVEFORM_BINS};
use mydaw_rt_core::{AudioBuffers, NoteEvent, ProcessCtx, RtNode, TransportSnapshot};
use serde::Serialize;

use crate::source::{Source, SourceError};
use crate::state::AppState;

#[derive(Debug, thiserror::Error)]
pub enum SamplePreviewError {
    #[error("there is no track {0}")]
    NoSuchTrack(usize),
    #[error("track {0} is not an audio-file channel")]
    NotAudioFile(usize),
    #[error("audio source {0} is not loaded")]
    MissingSource(u64),
    #[error(transparent)]
    Preview(#[from] crate::file_preview::PreviewFileError),
}

/// Sample rate the preview is rendered at. Independent of the open device: the
/// picture is of the instrument, not of the session's interface.
const PREVIEW_RATE: f64 = 48_000.0;
/// Block size the render loop uses. An instrument that only works as one giant
/// `process` call has a bug this would hide if it were the whole capture.
const PREVIEW_BLOCK: usize = 256;
/// How long a hit is captured, in milliseconds. Past the open hat's 350 ms
/// decay and a synth's short note, short enough that the window appears at once.
const PREVIEW_MS: f64 = 500.0;
/// Velocity a preview hit is played at.
const PREVIEW_VELOCITY: f32 = 0.9;
/// Where a sustaining instrument's note-off lands, as a fraction of the capture.
const RELEASE_AT: f64 = 0.45;

/// Waveform click / Preview: the edited region at the same pitch as the root key.
///
/// Keyboard playback is `channel pitch + (note − root) − fine cents`. A click
/// is not a MIDI note, so it matches playing the stored root: channel pitch
/// minus the root's fine offset. Region and fades stay in the take.
fn audition_tune(
    pitch_cents: f32,
    root_fine_cents: f32,
    start_frames: u64,
    end_frames: u64,
    fade_in_ms: f32,
    fade_out_ms: f32,
) -> AudioClipTune {
    AudioClipTune {
        pitch_cents: pitch_cents - root_fine_cents,
        start_frames,
        end_frames,
        fade_in_ms,
        fade_out_ms,
        route: None,
    }
}

/// Render the file-domain edits of one sample channel into an audition take.
///
/// This runs only when the user presses Preview. The returned source advertises
/// the pitched rate, letting the existing preview node perform its normal cubic
/// resampling, while region and fades are baked into the copied samples.
pub(crate) fn render_sample_take(source: &AudioSource, tune: AudioClipTune) -> AudioSource {
    let source_frames = source.frames() as u64;
    let start = tune.start_frames.min(source_frames);
    let end = if tune.end_frames == 0 || tune.end_frames > source_frames {
        source_frames
    } else {
        tune.end_frames
    };
    if start >= end || source.sample_rate() <= 0.0 {
        return AudioSource::from_stereo(
            source.id(),
            source.path(),
            source.sample_rate(),
            Vec::new(),
        );
    }

    let rate = source.sample_rate() * pitch_ratio(tune.pitch_cents);
    let frames = (end - start) as usize;
    let fade_in_frames = (f64::from(tune.fade_in_ms.max(0.0)) / 1_000.0 * rate).max(0.0);
    let fade_out_frames = (f64::from(tune.fade_out_ms.max(0.0)) / 1_000.0 * rate).max(0.0);
    let padded = source.padded();
    let mut data = Vec::with_capacity(frames.saturating_mul(CHANNELS));
    for offset in 0..frames {
        let attack = if fade_in_frames > 0.0 {
            (offset as f64 / fade_in_frames).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let tail = if fade_out_frames > 0.0 {
            ((frames - offset) as f64 / fade_out_frames).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let envelope = attack.min(tail) as f32;
        let frame = start as usize + offset;
        for channel in 0..CHANNELS {
            data.push(cubic(taps(padded, frame, channel), 0.0) * envelope);
        }
    }
    AudioSource::from_stereo(source.id(), source.path(), rate, data)
}

/// A source's one-shot, as min/max pairs the sample window draws.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePreviewDto {
    /// The source id that was asked for, so the window can refuse a stale
    /// answer if the channel changed while the render ran.
    pub source_id: String,
    /// [`Source::engine`]: `synth`, `sampler`, `tone`. Never a hosted plugin.
    pub engine: String,
    pub frames: u64,
    pub sample_rate: f64,
    pub duration_ms: f64,
    pub bins: usize,
    pub peaks: Vec<[f32; 2]>,
    /// Largest absolute sample, so a silent render (missing sampler assets)
    /// can be labelled rather than drawn as a flat line that looks like a bug.
    pub peak: f32,
}

impl AppState {
    /// Audition one audio-file track with its current region, pitch and fades.
    ///
    /// Browser rows intentionally use the raw-file preview. This path belongs
    /// to the channel editor, where Preview must match the controls beside it.
    pub fn preview_track_sample(&self, track: usize) -> Result<(), SamplePreviewError> {
        let (source_id, tune) = {
            let project = self.tracks().project();
            let def = project
                .tracks
                .get(track)
                .ok_or(SamplePreviewError::NoSuchTrack(track))?;
            let source_id = match def.source {
                Source::AudioClip { source_id } | Source::OneShot { source_id } => source_id,
                _ => return Err(SamplePreviewError::NotAudioFile(track)),
            };
            (
                source_id,
                audition_tune(
                    def.pitch_cents,
                    def.root_fine_cents,
                    def.sample_start_frames,
                    def.sample_end_frames,
                    def.fade_in_ms,
                    def.fade_out_ms,
                ),
            )
        };
        let source = self
            .audio_source(source_id)
            .ok_or(SamplePreviewError::MissingSource(source_id))?;
        self.tracks()
            .file_preview()
            .play_source(render_sample_take(&source, tune))?;
        Ok(())
    }

    /// Render one hit of `source_id` into waveform bins.
    ///
    /// # Errors
    /// A hosted plugin, an unknown id, or a built-in the instruments plugin
    /// failed to contribute (a wiring bug).
    pub fn preview_source(
        &self,
        source_id: &str,
        target_bins: usize,
    ) -> Result<SourcePreviewDto, PreviewError> {
        let (source, left, right) = self.render_source_hit(source_id)?;
        let peaks = fold_peaks(&left, &right, target_bins);
        let peak = peaks.iter().fold(0.0_f32, |acc, pair| {
            acc.max(pair[0].abs()).max(pair[1].abs())
        });
        Ok(SourcePreviewDto {
            source_id: source.id(),
            engine: source.engine().to_owned(),
            frames: left.len().min(right.len()) as u64,
            sample_rate: PREVIEW_RATE,
            duration_ms: PREVIEW_MS,
            bins: peaks.len(),
            peaks,
            peak,
        })
    }

    /// One hit of `source_id` as a playable take, for the browser audition slot.
    ///
    /// Independent of the selected rack channel: clicking Snare while Kick is
    /// selected must still sound like a snare. Hosted plugins are refused.
    ///
    /// # Errors
    /// The same refusals as [`preview_source`].
    pub fn source_hit_audio(&self, source_id: &str) -> Result<AudioSource, PreviewError> {
        let (source, left, right) = self.render_source_hit(source_id)?;
        let id = source.id();
        Ok(AudioSource::from_stereo(
            0,
            Path::new(&id),
            PREVIEW_RATE,
            interleave_stereo(&left, &right),
        ))
    }

    fn render_source_hit(
        &self,
        source_id: &str,
    ) -> Result<(Source, Vec<f32>, Vec<f32>), PreviewError> {
        let source = Source::parse(source_id).map_err(PreviewError::from_source)?;
        let (left, right) = render_source_pcm(self.tracks(), &source)?;
        Ok((source, left, right))
    }
}

/// One hit of `source` as stereo PCM, for a preview or a factory sample.
pub(crate) fn render_source_pcm(
    tracks: &crate::tracks::TrackHost,
    source: &Source,
) -> Result<(Vec<f32>, Vec<f32>), PreviewError> {
    let mut node = tracks
        .build_node(source, PREVIEW_RATE, PREVIEW_BLOCK)
        .map_err(PreviewError::Build)?;
    let frames = ((PREVIEW_MS / 1_000.0) * PREVIEW_RATE).round() as usize;
    let events = preview_events(source, frames);
    Ok(render_offline(
        node.as_mut(),
        frames,
        PREVIEW_BLOCK,
        &events,
    ))
}

/// Why a preview could not be drawn.
#[derive(Debug)]
pub enum PreviewError {
    /// A CLAP or VST3 source: it has its own window, and this path cannot
    /// instantiate it off the main thread anyway.
    HostedPlugin {
        format: &'static str,
    },
    Unknown(String),
    Build(crate::source::SourceError),
}

impl PreviewError {
    fn from_source(err: SourceError) -> Self {
        match err {
            SourceError::ClapNeedsBundle { .. } => Self::HostedPlugin { format: "CLAP" },
            SourceError::Vst3NeedsModule { .. } => Self::HostedPlugin { format: "VST3" },
            SourceError::Unknown(id) => Self::Unknown(id),
            other => Self::Build(other),
        }
    }
}

impl std::fmt::Display for PreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostedPlugin { format } => write!(
                f,
                "a {format} plugin draws its own window — click the channel name to open it"
            ),
            Self::Unknown(id) => write!(f, "unknown source '{id}'"),
            Self::Build(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PreviewError {}

/// The note events the first block of a preview should see.
///
/// The test tone is free-running and gated on the transport in the graph; an
/// offline render has no gate, so it just sings. Everything else gets middle C,
/// held then released, so a synth's envelope is visible -- a sampled kit maps
/// that key to whatever its manifest put there, and a one-shot ignores the
/// note-off rather than being choked by it.
fn preview_events(source: &Source, frames: usize) -> Vec<NoteEvent> {
    match source {
        Source::TestTone => Vec::new(),
        Source::PolySynth | Source::Sampler(_) => {
            let off = ((frames as f64 * RELEASE_AT) as u32).min(frames.saturating_sub(1) as u32);
            vec![
                NoteEvent::note_on(0, 60, PREVIEW_VELOCITY),
                NoteEvent::note_off(off, 60),
            ]
        }
        Source::Clap(_) | Source::Vst3(_) | Source::AudioClip { .. } => Vec::new(),
        Source::OneShot { .. } => {
            vec![
                NoteEvent::note_on(0, 60, PREVIEW_VELOCITY),
                NoteEvent::note_off(
                    ((frames as f64 * RELEASE_AT) as u32).min(frames.saturating_sub(1) as u32),
                    60,
                ),
            ]
        }
    }
}

fn render_offline(
    node: &mut dyn RtNode,
    frames: usize,
    block: usize,
    events: &[NoteEvent],
) -> (Vec<f32>, Vec<f32>) {
    let block = block.max(1);
    let mut buffers = AudioBuffers::new(block);
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut done = 0;
    while done < frames {
        let n = block.min(frames - done);
        // Events are stamped in capture time; this block only sees the ones
        // that fall inside it, rebased to the block's own origin — which is
        // what `ProcessCtx` requires (`frame_offset < frames`).
        let block_events: Vec<NoteEvent> = events
            .iter()
            .filter_map(|event| {
                let at = event.frame_offset as usize;
                if at >= done && at < done + n {
                    Some(NoteEvent {
                        frame_offset: (at - done) as u32,
                        channel: event.channel,
                        kind: event.kind,
                    })
                } else {
                    None
                }
            })
            .collect();
        {
            let mut ctx = ProcessCtx::with_events(
                &mut buffers,
                PREVIEW_RATE,
                n,
                TransportSnapshot::default(),
                &block_events,
            );
            node.process(&mut ctx);
        }
        let [l, r] = buffers.as_slices();
        left.extend_from_slice(l);
        right.extend_from_slice(r);
        done += n;
    }
    (left, right)
}

fn interleave_stereo(left: &[f32], right: &[f32]) -> Vec<f32> {
    let mut data = Vec::with_capacity(left.len().min(right.len()).saturating_mul(2));
    for (left_sample, right_sample) in left.iter().zip(right) {
        data.push(*left_sample);
        data.push(*right_sample);
    }
    data
}

/// One `[min, max]` pair per bin, over both channels, so a transient on one
/// side is not hidden. Clamped to [`MAX_WAVEFORM_BINS`].
fn fold_peaks(left: &[f32], right: &[f32], target_bins: usize) -> Vec<[f32; 2]> {
    let frames = left.len().min(right.len());
    if frames == 0 {
        return Vec::new();
    }
    let bins = target_bins.clamp(1, MAX_WAVEFORM_BINS).min(frames);
    let mut peaks = Vec::with_capacity(bins);
    for index in 0..bins {
        let start = index * frames / bins;
        let end = ((index + 1) * frames / bins).max(start + 1);
        let mut min = 0.0_f32;
        let mut max = 0.0_f32;
        for frame in start..end {
            let sample = left[frame].min(right[frame]);
            let loud = left[frame].max(right[frame]);
            min = min.min(sample);
            max = max.max(loud);
        }
        peaks.push([min, max]);
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{boot_kernel, AppState};

    fn with_state<R>(run: impl FnOnce(&AppState) -> R) -> R {
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        run(&state)
    }

    #[test]
    fn a_synth_preview_has_a_real_note_not_a_flat_line() {
        with_state(|state| {
            let preview = state
                .preview_source("poly-synth", 256)
                .expect("the synth renders");
            assert_eq!(preview.source_id, "poly-synth");
            assert_eq!(preview.engine, "synth");
            assert!(
                preview.peak > 0.1,
                "a held note must move the needle, peak was {}",
                preview.peak
            );
            assert_eq!(preview.bins, preview.peaks.len());
            assert!(preview.bins >= 64, "enough bins to draw a shape");
            // The note is released three quarters of the way in, so the tail
            // has to be quieter than the sustain that preceded it.
            let quarter = preview.bins / 4;
            let head = preview.peaks[..quarter]
                .iter()
                .fold(0.0_f32, |acc, pair| acc.max(pair[1].abs()));
            let tail = preview.peaks[preview.bins - quarter..]
                .iter()
                .fold(0.0_f32, |acc, pair| acc.max(pair[1].abs()));
            assert!(
                head > tail,
                "a released note decays; head {head} should outrank tail {tail}"
            );
        });
    }

    #[test]
    fn a_hosted_plugin_is_refused_rather_than_drawn_as_silence() {
        with_state(|state| {
            let err = state.preview_source("clap:com.acme.thing", 64).unwrap_err();
            assert!(
                matches!(err, PreviewError::HostedPlugin { format: "CLAP" }),
                "{err:?}"
            );
            assert!(
                err.to_string().contains("own window"),
                "the UI quotes this: {err}"
            );
        });
    }

    #[test]
    fn an_unknown_id_names_what_was_asked_for() {
        with_state(|state| {
            let err = state.preview_source("wavetable", 64).unwrap_err();
            assert!(err.to_string().contains("wavetable"), "{err}");
        });
    }

    #[test]
    fn the_synth_previews_as_a_note_and_the_tone_as_a_tone() {
        with_state(|state| {
            let synth = state
                .source_hit_audio("poly-synth")
                .expect("the synth renders");
            let tone = state
                .source_hit_audio("test-tone")
                .expect("the tone renders");
            assert!(synth.frames() > 0 && tone.frames() > 0);
            let peak = |audio: &AudioSource| {
                audio
                    .padded()
                    .iter()
                    .fold(0.0_f32, |acc, sample| acc.max(sample.abs()))
            };
            assert!(peak(&synth) > 0.05, "the synth must make a sound");
            assert!(peak(&tone) > 0.05, "the tone must make a sound");
            assert_ne!(
                synth.padded(),
                tone.padded(),
                "two instruments must not render the same samples"
            );
        });
    }

    #[test]
    fn edited_file_preview_applies_region_pitch_and_overlapping_fades() {
        let source = AudioSource::from_stereo(
            7,
            Path::new("loop.wav"),
            48_000.0,
            vec![0.5; 4_800 * mydaw_audio_file::CHANNELS],
        );
        let take = render_sample_take(
            &source,
            mydaw_plugin_sequencer::AudioClipTune {
                pitch_cents: 1_200.0,
                start_frames: 480,
                end_frames: 4_320,
                fade_in_ms: 100.0,
                fade_out_ms: 100.0,
                route: None,
            },
        );

        assert_eq!(take.frames(), 3_840, "preview must use the edited region");
        assert!(
            (take.sample_rate() - 96_000.0).abs() < f64::EPSILON,
            "raising pitch one octave must double the playback rate"
        );
        let samples = take.padded();
        let peak = samples
            .iter()
            .fold(0.0_f32, |acc, sample| acc.max(sample.abs()));
        assert!(peak > 0.08, "overlapping fades must remain audible: {peak}");
        assert!(
            cubic(taps(samples, 0, 0), 0.0).abs() < 1e-6,
            "fade-in must start at silence"
        );
        assert!(
            cubic(taps(samples, take.frames() - 1, 0), 0.0).abs() < peak * 0.01,
            "fade-out must end near silence"
        );
    }

    #[test]
    fn waveform_audition_matches_playing_the_root_key() {
        let source = AudioSource::from_stereo(
            3,
            Path::new("tuned.wav"),
            48_000.0,
            vec![0.5; 1_000 * mydaw_audio_file::CHANNELS],
        );
        let take = render_sample_take(&source, audition_tune(100.0, 12.0, 0, 0, 0.0, 0.0));
        let expected = 48_000.0 * mydaw_plugin_sequencer::pitch_ratio(88.0);
        assert!(
            (take.sample_rate() - expected).abs() < 1e-6,
            "click audition is channel pitch minus root fine, got {}",
            take.sample_rate()
        );
    }
}
