//! Render to sample: one loop of the current pattern, offline, into a channel.
//!
//! ```text
//!   PatternSet ──compile──▶ CompiledSong ──▶ SongSlot ─┐
//!   TrackHost  ──build_offline_spec──▶ GraphSpec ──────┤
//!                                                      ▼
//!                            run_block × N   (the real callback, no device)
//!                                                      │
//!                        interleaved stereo ───────────┘
//!                                 │
//!                    32-bit float WAV ──▶ AudioSourceRegistry ──▶ new channel
//! ```
//!
//! # Why this is the same engine, not a copy of it
//!
//! Everything here is the machinery the audio device drives:
//! [`TrackHost::build_offline_spec`] builds the graph the stream builds,
//! `SequencerService::compile` compiles the song the stream plays, and
//! [`run_block`] *is* the cpal callback's body. The only differences are that
//! the samples go into a `Vec` instead of a sound card, and that the loop runs
//! as fast as the CPU allows. A render that used its own summing path would be
//! a second mixer to keep in step with the first, and the first sign of drift
//! would be a bounce that does not sound like the session.
//!
//! # What one render covers
//!
//! Exactly **one loop of the active pattern** — `length_steps` steps at the
//! session tempo — which is the length that loops seamlessly when the result is
//! dropped back on the grid. That is the trade FL Studio's resampler makes too:
//! a tail that rings past the loop point would make the sample longer than the
//! bar it came from, and every subsequent placement would drift.
//!
//! Two things are deliberately outside the render, and both are the same
//! limitation `build_offline_spec` already documents:
//!
//! - **hosted CLAP/VST3 plugins**, which only the main thread may instantiate;
//! - the **browser's file-preview** player, which is not part of the project.
//!
//! A channel whose instrument is a hosted plugin therefore renders silent, and
//! [`RenderOutcome::hosted_channels`] says how many were skipped so the shell
//! can tell the user rather than let them wonder.

use std::path::PathBuf;
use std::sync::Arc;

use mydaw_audio_file::CHANNELS;
use mydaw_plugin_audio_io::{run_block, AudioIoStats, RtState, RtStateParams};
use mydaw_plugin_sequencer::{
    song_channel, step_len_samples, AudioSourceError, SONG_QUEUE_CAPACITY,
};
use mydaw_rt_core::{
    graph_channel, note_channel, rt_channel, GcThread, GraphLimits, PlayMode, RtCommand,
    TransportClock,
};

use crate::sample_edit::{write_stereo_wav, SampleEditError};
use crate::source::Source;
use crate::state::AppState;

/// Sample rate a render uses when no device is open.
///
/// With a stream running the render matches the device instead, so a bounce and
/// what the user just heard are the same numbers.
pub const RENDER_FALLBACK_RATE: f64 = 48_000.0;

/// Block size the offline loop asks for.
///
/// The same order as a real buffer rather than one giant call: a node that only
/// behaves when it sees the whole render at once has a bug, and this is where it
/// would show.
const RENDER_BLOCK: usize = 256;

/// Largest block the offline graph is sized for. Twice [`RENDER_BLOCK`], which
/// is what the live stream allows itself too.
const RENDER_MAX_BLOCK: usize = 512;

/// Note events one offline block may carry.
const RENDER_NOTE_CAPACITY: usize = 256;

/// Blocks the render will run before giving up.
///
/// A guard rather than a limit anyone should meet: at 48 kHz and 256 frames a
/// block, ten thousand blocks is 53 seconds of audio, and the longest pattern
/// this engine has (64 steps at the minimum tempo) is a small fraction of that.
const MAX_RENDER_BLOCKS: usize = 10_000;

/// Why a pattern could not be rendered.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("the pattern is empty; there is nothing to render")]
    EmptyPattern,
    #[error("the rendered pattern is silent — check that a channel is unmuted and has steps")]
    Silent,
    #[error(transparent)]
    Audio(#[from] AudioSourceError),
    #[error(transparent)]
    Write(#[from] SampleEditError),
    #[error("could not add the rendered channel: {0}")]
    Channel(String),
}

/// What a render produced.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderOutcome {
    /// The rack channel the sample landed on.
    pub track: usize,
    /// Where the WAV was written.
    pub path: PathBuf,
    /// Frames written, in `sample_rate`.
    pub frames: usize,
    pub sample_rate: f64,
    /// Largest absolute sample in the render.
    pub peak: f32,
    /// Channels whose instrument is a hosted plugin, and so rendered silent.
    pub hosted_channels: usize,
}

impl AppState {
    /// Render one loop of the active pattern and add it as a sample channel.
    ///
    /// The landing is a recording's: [`add_sample_channel`](Self::add_sample_channel),
    /// so the result is a normal one-shot channel with a waveform, a region and
    /// the sample editor's controls — not a special "bounce" object.
    ///
    /// # Errors
    /// [`RenderError`] — a pattern with no channels, a render that came out
    /// silent, the WAV failing to write, or the rack being full.
    pub fn render_pattern_to_sample(&self) -> Result<RenderOutcome, RenderError> {
        let (data, sample_rate, hosted) = self.render_active_pattern()?;
        let peak = crate::sample_edit::peak(&data);
        if peak <= 0.0 {
            return Err(RenderError::Silent);
        }
        let frames = data.len() / CHANNELS;

        let dir = crate::drum_samples::sample_cache_dir().join("renders");
        std::fs::create_dir_all(&dir).map_err(|err| {
            RenderError::Write(SampleEditError::Io {
                path: dir.display().to_string(),
                message: err.to_string(),
            })
        })?;
        let name = self.sequencer().active_pattern().name;
        let path = render_path(&dir, &name);
        write_stereo_wav(&path, sample_rate, &data)?;

        // "No audio device" is not a failure to render: the channel is in the
        // project, the file is on disk, and there is simply nothing to play it
        // through yet. That is the same policy `publish_song` takes, and it is
        // what lets a render work on a machine with no sound card.
        let track = match self.add_sample_channel(&path) {
            Ok(track) => track,
            Err(err) if err.is_only_no_device() => self.tracks().track_count().saturating_sub(1),
            Err(err) => return Err(RenderError::Channel(err.to_string())),
        };
        Ok(RenderOutcome {
            track,
            path,
            frames,
            sample_rate,
            peak,
            hosted_channels: hosted,
        })
    }

    /// One loop of the active pattern as interleaved stereo, plus the rate it
    /// was rendered at and how many hosted-plugin channels were left out.
    ///
    /// Public so a test can assert the audio without also writing a file and
    /// growing the rack.
    ///
    /// # Errors
    /// [`RenderError::EmptyPattern`] for a pattern with no channels at all.
    pub fn render_active_pattern(&self) -> Result<(Vec<f32>, f64, usize), RenderError> {
        let pattern = self.sequencer().active_pattern();
        if pattern.channels.is_empty() {
            return Err(RenderError::EmptyPattern);
        }
        let bpm = self.bpm();
        let sample_rate = match self.audio().sample_rate() {
            Some(rate) if rate >= 1.0 => rate,
            _ => RENDER_FALLBACK_RATE,
        };
        let frames =
            (step_len_samples(sample_rate, bpm) * f64::from(pattern.length_steps)).round() as usize;
        if frames == 0 {
            return Err(RenderError::EmptyPattern);
        }

        let hosted = self
            .tracks()
            .project()
            .tracks
            .iter()
            .filter(|def| matches!(def.source, Source::Clap(_) | Source::Vst3(_)))
            .count();

        // A collector of this render's own, so nothing it drops waits on the
        // session's GC interval -- and so a render with no audio device open
        // still has somewhere for `basedrop` to put things.
        let gc = GcThread::spawn();
        let limits = GraphLimits {
            sample_rate,
            max_block: RENDER_MAX_BLOCK,
            max_events: RENDER_NOTE_CAPACITY,
        };
        let offline = self.tracks().build_offline_spec(&limits, gc.handle());

        let (mut graph, graph_slot) = graph_channel(2, gc.handle());
        let (mut commands, command_rx) = rt_channel::<RtCommand>(8);
        let (_notes_tx, notes) = note_channel(RENDER_NOTE_CAPACITY);
        let (mut song_tx, song_slot) = song_channel(SONG_QUEUE_CAPACITY, gc.handle());
        let stats = Arc::new(AudioIoStats::default());

        if graph.install(offline.spec).is_err() {
            return Err(RenderError::Silent);
        }
        let _ = song_tx.install(self.sequencer().compile());
        let _ = commands.push(RtCommand::SetBpm(bpm));
        // PAT, always: "render to sample" is about the pattern under the
        // cursor. Rendering the arrangement instead because the transport
        // happens to be in SONG would be a different command wearing this one's
        // label.
        let _ = commands.push(RtCommand::SetPlayMode(PlayMode::Pattern));
        let _ = commands.push(RtCommand::SetLooping(true));
        let _ = commands.push(RtCommand::SetPlaying(true));

        let mut state = RtState::new(RtStateParams {
            graph: graph_slot,
            commands: command_rx,
            notes,
            note_capacity: RENDER_NOTE_CAPACITY,
            song: song_slot,
            meters: None,
            debug_tap: None,
            spectrum: None,
            // A clock of this render's own, starting at sample zero, so the
            // bounce begins on the downbeat whatever the session's playhead is
            // doing.
            transport: Arc::new(TransportClock::new(sample_rate)),
            sample_rate,
            out_channels: CHANNELS,
            max_block: RENDER_MAX_BLOCK,
            stats: Arc::clone(&stats),
        });

        let mut block = vec![0.0_f32; RENDER_BLOCK * CHANNELS];
        let mut out: Vec<f32> = Vec::with_capacity(frames * CHANNELS);
        let mut blocks = 0;
        while out.len() < frames * CHANNELS && blocks < MAX_RENDER_BLOCKS {
            block.fill(0.0);
            run_block(&mut state, &mut block);
            out.extend_from_slice(&block);
            blocks += 1;
        }
        out.truncate(frames * CHANNELS);

        // The handles are what kept every track's source queue attached for the
        // length of the render; dropping them here, and not earlier, is the
        // whole reason `build_offline_spec` returns them.
        drop(offline.handles);
        drop(state);
        Ok((out, sample_rate, hosted))
    }
}

/// `<dir>/<pattern name>.wav`, never overwriting an earlier render.
fn render_path(dir: &std::path::Path, pattern: &str) -> PathBuf {
    let stem: String = pattern
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            other if other.is_control() => ' ',
            other => other,
        })
        .collect();
    let stem = stem.trim();
    let stem = if stem.is_empty() { "Pattern" } else { stem };
    let first = dir.join(format!("{stem}.wav"));
    if !first.exists() {
        return first;
    }
    for attempt in 2..10_000 {
        let candidate = dir.join(format!("{stem} {attempt}.wav"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem} {}.wav", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{boot_kernel, AppState};
    use mydaw_plugin_sequencer::StepRow;

    /// Four on the floor on the Kick channel of the starter rack.
    ///
    /// The engine's starter pattern is deliberately **empty** -- a new session
    /// opens on a blank grid -- so a render test has to program the beat it then
    /// claims to hear.
    fn program_a_beat(state: &AppState) {
        let mut pattern = state.sequencer().active_pattern();
        let key = pattern.channels[0]
            .steps
            .as_ref()
            .expect("the kick channel has a grid")
            .key;
        pattern.channels[0].steps = Some(StepRow::empty(key).with_steps(&[0, 4, 8, 12]));
        state.set_pattern(pattern).expect("the beat is programmed");
    }

    #[test]
    fn a_render_is_one_loop_of_the_pattern_long() {
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        program_a_beat(&state);
        let pattern = state.sequencer().active_pattern();
        let (data, rate, hosted) = state.render_active_pattern().expect("the pattern renders");
        assert_eq!(hosted, 0, "the starter rack has no hosted plugins");
        assert_eq!(rate, RENDER_FALLBACK_RATE, "no device is open in a test");
        let expected = (step_len_samples(rate, state.bpm()) * f64::from(pattern.length_steps))
            .round() as usize;
        assert_eq!(
            data.len(),
            expected * CHANNELS,
            "a render is exactly one loop of the active pattern"
        );
        assert!(data.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn a_programmed_beat_renders_audible_stereo() {
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        program_a_beat(&state);
        let (data, _, _) = state.render_active_pattern().expect("the pattern renders");
        let peak = crate::sample_edit::peak(&data);
        assert!(
            peak > 0.01,
            "four kicks were programmed; the render must not be silence (peak {peak})"
        );
        // The first hit is on the downbeat, so the head of the render must not
        // be a bar of nothing -- that would mean the transport never started.
        let head = crate::sample_edit::peak(&data[..(data.len() / 8).max(CHANNELS)]);
        assert!(head > 0.001, "the render must begin on the downbeat");
    }

    #[test]
    fn rendering_does_not_detach_the_live_source_handles() {
        // The bug `build_offline_spec` exists to avoid: an offline render that
        // reused the session's handles would leave every later source swap
        // pushing into a queue nobody reads.
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        program_a_beat(&state);
        let before = state.mixer_state();
        let _ = state.render_active_pattern().expect("the pattern renders");
        let after = state.mixer_state();
        assert_eq!(before.tracks.len(), after.tracks.len());
        // And the session is still editable afterwards.
        state
            .set_track_gain_db(0, -6.0)
            .expect("the mixer still takes edits after a render");
    }

    #[test]
    fn an_empty_grid_renders_silence_rather_than_a_channel_of_nothing() {
        // The other half of the claim, and the reason `render_pattern_to_sample`
        // refuses instead of adding a silent channel: a new session's pattern
        // has no steps at all, so there is nothing to resample yet.
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        let (data, _, _) = state.render_active_pattern().expect("the pattern renders");
        assert_eq!(crate::sample_edit::peak(&data), 0.0);
        assert!(matches!(
            state.render_pattern_to_sample(),
            Err(RenderError::Silent)
        ));
    }

    #[test]
    fn a_render_lands_as_a_playable_sample_channel() {
        let kernel = boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        program_a_beat(&state);
        let before = state.mixer_state().tracks.len();
        let outcome = state
            .render_pattern_to_sample()
            .expect("the pattern renders to a channel");
        let mixer = state.mixer_state();
        assert_eq!(mixer.tracks.len(), before + 1);
        let landed = &mixer.tracks[outcome.track];
        assert_eq!(landed.engine, "sample", "a render is a one-shot channel");
        assert!(
            landed.audio_source_id.is_some(),
            "the rendered file must be a loaded audio source"
        );
        assert!(outcome.frames > 0 && outcome.peak > 0.01);
        let _ = std::fs::remove_file(&outcome.path);
    }

    #[test]
    fn a_render_name_never_overwrites_an_earlier_one() {
        let dir = std::env::temp_dir().join(format!("mydaw-render-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let first = render_path(&dir, "Pattern 1");
        assert_eq!(first.file_name().unwrap(), "Pattern 1.wav");
        std::fs::write(&first, b"").expect("touch");
        assert_eq!(
            render_path(&dir, "Pattern 1").file_name().unwrap(),
            "Pattern 1 2.wav"
        );
        assert_eq!(
            render_path(&dir, "a/b").file_name().unwrap(),
            "a b.wav",
            "a pattern name is not a path"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

