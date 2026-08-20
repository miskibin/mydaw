//! Offline waveform of a built-in (or sampled) source, for the channel window.
//!
//! A hosted plugin draws its own editor; asking this for Analog Lab would be a
//! picture of silence with that plugin's name on it. Built-ins and the sampler
//! have no window of their own, so this is what "click Kick and see the sound"
//! actually shows: the samples the node produces for one hit, folded into
//! min/max bins the webview already knows how to draw.

use mydaw_plugin_instruments::KICK_KEY;
use mydaw_plugin_sequencer::MAX_WAVEFORM_BINS;
use mydaw_rt_core::{AudioBuffers, NoteEvent, ProcessCtx, RtNode, TransportSnapshot};
use serde::Serialize;

use crate::source::{Source, SourceError};
use crate::state::AppState;

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
        let source = Source::parse(source_id).map_err(PreviewError::from_source)?;
        let mut node = self
            .tracks()
            .build_node(&source, PREVIEW_RATE, PREVIEW_BLOCK)
            .map_err(PreviewError::Build)?;
        let frames = ((PREVIEW_MS / 1_000.0) * PREVIEW_RATE).round() as usize;
        let events = preview_events(&source, frames);
        let (left, right) = render_offline(node.as_mut(), frames, PREVIEW_BLOCK, &events);
        let peaks = fold_peaks(&left, &right, target_bins);
        let peak = peaks.iter().fold(0.0_f32, |acc, pair| {
            acc.max(pair[0].abs()).max(pair[1].abs())
        });
        Ok(SourcePreviewDto {
            source_id: source.id(),
            engine: source.engine().to_owned(),
            frames: frames as u64,
            sample_rate: PREVIEW_RATE,
            duration_ms: PREVIEW_MS,
            bins: peaks.len(),
            peaks,
            peak,
        })
    }
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
/// offline render has no gate, so it just sings. Drums are one-shots (a
/// note-off must not choke them). The kit answers only on GM keys. Everything
/// else gets middle C, held then released, so a synth's envelope is visible.
fn preview_events(source: &Source, frames: usize) -> Vec<NoteEvent> {
    match source {
        Source::TestTone => Vec::new(),
        Source::DrumKit => vec![NoteEvent::note_on(0, KICK_KEY, PREVIEW_VELOCITY)],
        Source::Drum(drum) => {
            // The drum's own base key, so each instrument previews at the pitch
            // it was designed at rather than at whatever this list remembered.
            let key = drum.base_key();
            vec![NoteEvent::note_on(0, key, PREVIEW_VELOCITY)]
        }
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
    fn a_kick_preview_has_a_real_hit_not_a_flat_line() {
        with_state(|state| {
            let preview = state.preview_source("kick", 256).expect("kick renders");
            assert_eq!(preview.source_id, "kick");
            assert_eq!(preview.engine, "synth");
            assert!(
                preview.peak > 0.1,
                "a kick must move the needle, peak was {}",
                preview.peak
            );
            assert_eq!(preview.bins, preview.peaks.len());
            assert!(preview.bins >= 64, "enough bins to draw a shape");
            // The kick decays: the first quarter is louder than the last.
            let quarter = preview.bins / 4;
            let head = preview.peaks[..quarter]
                .iter()
                .fold(0.0_f32, |acc, pair| acc.max(pair[1].abs()));
            let tail = preview.peaks[preview.bins - quarter..]
                .iter()
                .fold(0.0_f32, |acc, pair| acc.max(pair[1].abs()));
            assert!(
                head > tail,
                "a kick decays; head {head} should outrank tail {tail}"
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
}
