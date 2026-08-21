//! Built-in drums as one-shot samples.
//!
//! The Sounds-tab Kick/Snare/Clap/Hats are synthesizers, but the musician's
//! workflow is a sample's: drop a wav, trim, fade, pitch. Rendering one hit of
//! each drum into a factory wav, then putting that file on a one-shot channel,
//! makes the floating editor and the inspector the same surface as an AUDIO-tab
//! file — and a replace keeps recorded pitch because the grid fires C4.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mydaw_plugin_instruments::SingleDrum;
use mydaw_plugin_sequencer::{AudioSource, AudioSourceError, AudioSourceRegistry, PatternSet};

use crate::preview::{render_source_pcm, PreviewError};
use crate::project::Project;
use crate::source::Source;
use crate::tracks::TrackHost;

/// MIDI key at which a one-shot plays the file at recorded pitch.
const SAMPLE_ROOT_KEY: u8 = mydaw_plugin_sequencer::DEFAULT_STEP_KEY;

/// Why a factory drum could not become a sample.
#[derive(Debug, thiserror::Error)]
pub enum DrumSampleError {
    #[error(transparent)]
    Preview(#[from] PreviewError),
    #[error(transparent)]
    Audio(#[from] AudioSourceError),
    #[error("{0}")]
    Io(String),
}

/// The MIDI key a channel's grid should use so the instrument plays untransposed.
pub fn natural_step_key(source: &Source) -> u8 {
    match source {
        Source::Drum(drum) => drum.base_key(),
        _ => SAMPLE_ROOT_KEY,
    }
}

/// If `source` is a Sounds-tab drum, turn it into the factory one-shot.
///
/// `drum-kit` stays a synthesizer: it is several drums behind one channel, not
/// a sample. Poly synth stays an instrument.
pub fn realize_source(
    tracks: &TrackHost,
    sources: &AudioSourceRegistry,
    source: Source,
) -> Result<Source, DrumSampleError> {
    match source {
        Source::Drum(drum) => {
            let audio = ensure_drum_sample(tracks, sources, drum)?;
            Ok(Source::OneShot {
                source_id: audio.id(),
            })
        }
        other => Ok(other),
    }
}

/// Replace every per-drum synthesizer on `project` with its factory sample.
///
/// When `patterns` is present, each converted channel's notes and step keys
/// move by the same interval as the instrument root (GM kick 36 → C4).
///
/// Returns whether anything changed.
pub fn realize_drum_tracks(
    project: &mut Project,
    mut patterns: Option<&mut PatternSet>,
    tracks: &TrackHost,
    sources: &AudioSourceRegistry,
) -> bool {
    let mut changed = false;
    for (index, track) in project.tracks.iter_mut().enumerate() {
        let Source::Drum(drum) = &track.source else {
            continue;
        };
        let drum = *drum;
        let from = drum.base_key();
        match ensure_drum_sample(tracks, sources, drum) {
            Ok(audio) => {
                track.source = Source::OneShot {
                    source_id: audio.id(),
                };
                if let Some(set) = patterns.as_deref_mut() {
                    set.shift_channel_keys(index as u8, from, SAMPLE_ROOT_KEY);
                }
                changed = true;
            }
            Err(err) => log::warn!(
                "could not turn '{}' into a sample; it stays a synthesizer: {err}",
                drum.display_name()
            ),
        }
    }
    changed
}

/// Decode (or reuse) the factory wav for `drum`.
pub fn ensure_drum_sample(
    tracks: &TrackHost,
    sources: &AudioSourceRegistry,
    drum: SingleDrum,
) -> Result<Arc<AudioSource>, DrumSampleError> {
    let path = factory_drum_path(drum);
    if let Some(existing) = sources.find_by_path(&path) {
        if !existing.is_missing() && existing.frames() > 0 {
            return Ok(existing);
        }
    }
    if path.is_file() {
        return Ok(sources.load(&path)?);
    }
    let (left, right) = render_source_pcm(tracks, &Source::Drum(drum))?;
    let frames = trim_tail(&left, &right);
    if frames == 0 {
        return Err(DrumSampleError::Io(format!(
            "{} rendered silence",
            drum.display_name()
        )));
    }
    let data = interleave(&left[..frames], &right[..frames]);
    write_stereo_wav(&path, 48_000, &data)?;
    Ok(sources.load(&path)?)
}

fn factory_drum_dir() -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    root.join("WAVE Studio").join("factory-drums")
}

fn factory_drum_path(drum: SingleDrum) -> PathBuf {
    factory_drum_dir().join(format!("{}.wav", drum.type_id()))
}

fn trim_tail(left: &[f32], right: &[f32]) -> usize {
    const THRESHOLD: f32 = 1e-4;
    let frames = left.len().min(right.len());
    if frames == 0 {
        return 0;
    }
    let mut last = 0;
    for index in 0..frames {
        if left[index].abs().max(right[index].abs()) > THRESHOLD {
            last = index;
        }
    }
    // 8 ms of air after the last audible sample, so a fade-out has somewhere
    // to land rather than cutting the last sample.
    let pad = 384;
    (last + 1 + pad).min(frames)
}

fn interleave(left: &[f32], right: &[f32]) -> Vec<f32> {
    let mut data = Vec::with_capacity(left.len().min(right.len()).saturating_mul(2));
    for (left_sample, right_sample) in left.iter().zip(right) {
        data.push(*left_sample);
        data.push(*right_sample);
    }
    data
}

fn write_stereo_wav(
    path: &Path,
    sample_rate: u32,
    interleaved: &[f32],
) -> Result<(), DrumSampleError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| DrumSampleError::Io(err.to_string()))?;
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer =
        hound::WavWriter::create(path, spec).map_err(|err| DrumSampleError::Io(err.to_string()))?;
    for sample in interleaved {
        writer
            .write_sample(*sample)
            .map_err(|err| DrumSampleError::Io(err.to_string()))?;
    }
    writer
        .finalize()
        .map_err(|err| DrumSampleError::Io(err.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{boot_kernel, AppState};

    #[test]
    fn a_starter_kick_is_a_oneshot_sample_on_c4() {
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        let track = &state.mixer_state().tracks[0];
        assert_eq!(track.name, "Kick");
        assert_eq!(track.engine, "sample");
        assert!(track.audio_source_id.is_some());
        let key = state.sequencer().active_pattern().channels[0]
            .steps
            .as_ref()
            .unwrap()
            .key;
        assert_eq!(key, SAMPLE_ROOT_KEY);
    }

    #[test]
    fn placing_a_sounds_drum_becomes_a_sample() {
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        let realized = realize_source(
            state.tracks(),
            state.audio_sources(),
            Source::Drum(SingleDrum::Snare),
        )
        .expect("snare renders");
        assert!(matches!(realized, Source::OneShot { .. }), "{realized:?}");

        let before = state.mixer_state().tracks.len();
        let _ = state.add_track(Source::Drum(SingleDrum::Clap), Some("Clap".to_owned()));
        let track = &state.mixer_state().tracks[before];
        assert_eq!(track.name, "Clap");
        assert_eq!(track.engine, "sample");
        assert!(track.audio_source_id.is_some());
        assert!(
            !matches!(
                state.tracks().project().tracks[before].source,
                Source::Drum(_)
            ),
            "Sounds-tab drums must not stay synthesizers after they are placed"
        );
    }

    #[test]
    fn converting_synth_drums_retargets_the_grid_to_c4() {
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        let mut project = crate::project::Project::default();
        let mut patterns = crate::state::starter_pattern_set(&project);
        assert_eq!(
            patterns.active().channels[0].steps.as_ref().unwrap().key,
            SingleDrum::Kick.base_key()
        );

        assert!(realize_drum_tracks(
            &mut project,
            Some(&mut patterns),
            state.tracks(),
            state.audio_sources(),
        ));
        assert!(matches!(project.tracks[0].source, Source::OneShot { .. }));
        assert!(matches!(project.tracks[5].source, Source::PolySynth));
        assert_eq!(
            patterns.active().channels[0].steps.as_ref().unwrap().key,
            SAMPLE_ROOT_KEY
        );
    }

    #[test]
    fn the_drum_kit_stays_a_synthesizer() {
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        let realized = realize_source(state.tracks(), state.audio_sources(), Source::DrumKit)
            .expect("kit is not converted");
        assert_eq!(realized, Source::DrumKit);
    }
}
