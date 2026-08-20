//! One-shot file preview: play a sample without putting it in the registry.
//!
//! Clicking a row in the Audio browser must audition the file. That must not
//! consume a [`MAX_SOURCES`](mydaw_plugin_sequencer::MAX_SOURCES) slot — a
//! user flipping through a pack is not loading a project — and it must play
//! with the transport stopped, the same way FL Studio's preview player does.
//!
//! ```text
//!   preview_audio_file(path)
//!        |  decode (capped at PREVIEW_MAX_SECONDS)
//!        v
//!   Arc<AudioSource>  -- rtrb + basedrop -->  FilePreviewNode
//!                                                 += into the master bus
//! ```
//!
//! The node is unconditional in every graph (like the clip player) and
//! transparent while idle. A new preview replaces the current one.

use std::path::Path;
use std::sync::Arc;

use mydaw_audio_file::{cubic, decode_stereo, taps, DecodeError, CHANNELS};
use mydaw_plugin_audio_io::AudioIoError;
use mydaw_plugin_sequencer::{
    pitch_ratio, playback_step, ramp_step, AudioSource, CLIP_RAMP_MS,
};
use mydaw_rt_core::basedrop::{Handle, Owned};
use mydaw_rt_core::{ProcessCtx, RtNode};
use parking_lot::Mutex;
use rtrb::{Consumer, Producer};

use crate::source::SourceError;

/// In-flight preview swaps. A click-click-click through the browser is the
/// burst this has to absorb.
const PREVIEW_QUEUE_CAPACITY: usize = 8;

/// Longest preview decode. Fifteen seconds is past any one-shot and most
/// loops; a ten-minute take still auditions from the start without eating
/// 200 MB of RAM.
const PREVIEW_MAX_SECONDS: f64 = 15.0;

/// What crosses the RT wall: a new take, or silence.
struct PreviewTake {
    source: Arc<AudioSource>,
    start: f64,
    end: f64,
    ratio: f64,
}

type PreviewMsg = Option<PreviewTake>;

/// Non-RT end, for one open stream.
struct PreviewSender {
    tx: Producer<Owned<PreviewMsg>>,
    gc: Handle,
}

/// The shell's handle on the preview player. `Send + Sync`.
pub struct FilePreviewBus {
    sender: Mutex<Option<PreviewSender>>,
}

impl Default for FilePreviewBus {
    fn default() -> Self {
        Self::new()
    }
}

impl FilePreviewBus {
    /// An empty, detached bus.
    pub fn new() -> Self {
        Self {
            sender: Mutex::new(None),
        }
    }

    /// Build the channel for a freshly opened stream and take the RT end.
    pub fn open(&self, gc: Handle) -> Box<dyn RtNode> {
        let (tx, rx) = mydaw_rt_core::rt_channel::<Owned<PreviewMsg>>(PREVIEW_QUEUE_CAPACITY);
        *self.sender.lock() = Some(PreviewSender { tx, gc });
        Box::new(FilePreviewNode {
            rx,
            current: None,
            position: 0.0,
            env: 0.0,
            releasing: false,
        })
    }

    /// Forget the current stream's channel. Called when audio stops.
    pub fn close(&self) {
        *self.sender.lock() = None;
    }

    /// Decode `path` and play it. Replaces whatever was previewing.
    ///
    /// # Errors
    /// A decode failure, or [`AudioIoError::NotRunning`] when no stream is open.
    pub fn play(&self, path: &Path) -> Result<(), PreviewFileError> {
        self.play_tuned(path, 0, 0, 0.0)
    }

    /// Decode `path` and play it at `pitch_cents`, cropped to the SMP region.
    ///
    /// `end_frames == 0` means the file's last frame. Same crop and resample
    /// the sample window writes onto the channel, so Preview there is the
    /// channel, not a second opinion about the file.
    ///
    /// # Errors
    /// A decode failure, or [`AudioIoError::NotRunning`] when no stream is open.
    pub fn play_tuned(
        &self,
        path: &Path,
        start_frames: u64,
        end_frames: u64,
        pitch_cents: f32,
    ) -> Result<(), PreviewFileError> {
        let source = decode_preview(path)?;
        let frames = source.frames() as u64;
        let start = start_frames.min(frames) as f64;
        let end = if end_frames == 0 || end_frames > frames {
            frames as f64
        } else {
            end_frames as f64
        };
        self.install(Some(PreviewTake {
            source: Arc::new(source),
            start,
            end: end.max(start),
            ratio: pitch_ratio(pitch_cents),
        }))?;
        Ok(())
    }

    /// Cut the current preview.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] when no stream is open.
    pub fn stop(&self) -> Result<(), PreviewFileError> {
        self.install(None)?;
        Ok(())
    }

    /// Whether a stream's channel is attached.
    pub fn is_open(&self) -> bool {
        self.sender.lock().is_some()
    }

    fn install(&self, msg: PreviewMsg) -> Result<(), SourceError> {
        let mut guard = self.sender.lock();
        let sender = guard.as_mut().ok_or(AudioIoError::NotRunning)?;
        let owned = Owned::new(&sender.gc, msg);
        sender.tx.push(owned).map_err(|err| match err {
            rtrb::PushError::Full(_) => SourceError::QueueFull,
        })
    }
}

impl std::fmt::Debug for FilePreviewBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePreviewBus")
            .field("open", &self.is_open())
            .finish()
    }
}

/// The RT end, as the graph builder wants it.
pub fn file_preview_node(bus: &Arc<FilePreviewBus>, gc: Handle) -> Box<dyn RtNode> {
    bus.open(gc)
}

/// Why a preview could not start.
#[derive(Debug, thiserror::Error)]
pub enum PreviewFileError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Source(#[from] SourceError),
}

/// Decode `path` into an unregistered source, capped at [`PREVIEW_MAX_SECONDS`].
fn decode_preview(path: &Path) -> Result<AudioSource, DecodeError> {
    let decoded = decode_stereo(path, |rate| Some((rate * PREVIEW_MAX_SECONDS) as usize))?;
    Ok(AudioSource::from_stereo(
        0,
        path,
        decoded.sample_rate,
        decoded.data,
    ))
}

/// Audio-thread player: one voice, independent of the transport.
struct FilePreviewNode {
    rx: Consumer<Owned<PreviewMsg>>,
    current: Option<Owned<PreviewMsg>>,
    position: f64,
    env: f32,
    releasing: bool,
}

impl FilePreviewNode {
    fn poll_swap(&mut self, ramp: f32) {
        let mut latest: Option<Owned<PreviewMsg>> = None;
        while let Ok(next) = self.rx.pop() {
            latest = Some(next);
        }
        let Some(latest) = latest else {
            return;
        };
        if latest.as_ref().is_some() {
            let start = latest.as_ref().as_ref().map(|take| take.start).unwrap_or(0.0);
            self.current = Some(latest);
            self.position = start;
            self.env = ramp.min(1.0);
            self.releasing = false;
        } else {
            self.releasing = true;
        }
    }

    fn render(&mut self, ctx: &mut ProcessCtx, ramp: f32) {
        // Borrow the take through the `Owned` for the whole render, never an
        // RT-side `Arc` clone: once `current` is queued to the collector, a
        // clone could become the last reference, and its drop would free the
        // decoded audio inside the callback.
        let Some(take) = self.current.as_ref().and_then(|owned| owned.as_ref()) else {
            return;
        };
        if take.source.frames() == 0 {
            return;
        }
        let frames = ctx.frames;
        if frames == 0 {
            return;
        }
        let padded = take.source.padded();
        let end = take.end;
        let step = playback_step(take.source.sample_rate(), ctx.sample_rate) * take.ratio;
        if step <= 0.0 {
            return;
        }
        let [left, right] = ctx.buffers.as_mut_slices();

        let mut finished = false;
        for frame in 0..frames {
            if self.position >= end || self.position < take.start {
                finished = true;
                break;
            }
            if !self.releasing && self.position + step >= end {
                self.releasing = true;
            }
            let index = self.position as usize;
            let fraction = (self.position - self.position.floor()) as f32;
            let gain = self.env;
            left[frame] += cubic(taps(padded, index, 0), fraction) * gain;
            if CHANNELS > 1 {
                right[frame] += cubic(taps(padded, index, 1), fraction) * gain;
            }
            if self.releasing {
                self.env -= ramp;
                if self.env <= 0.0 {
                    self.env = 0.0;
                    finished = true;
                    break;
                }
            } else if self.env < 1.0 {
                self.env = (self.env + ramp).min(1.0);
            }
            self.position += step;
        }
        if finished {
            // Dropping the `Owned` queues the take for the collector; the
            // deallocation never runs on this thread.
            self.current = None;
        }
    }
}

impl RtNode for FilePreviewNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        let ramp = ramp_step(CLIP_RAMP_MS, ctx.sample_rate);
        self.poll_swap(ramp);
        self.render(ctx, ramp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_rt_core::basedrop::Collector;
    use mydaw_rt_core::{AudioBuffers, PlayMode, ProcessCtx, RtNode, TransportSnapshot};

    const SR: f64 = 48_000.0;
    const BLOCK: usize = 256;

    fn take(source: Arc<AudioSource>, ratio: f64) -> PreviewTake {
        let end = source.frames() as f64;
        PreviewTake {
            source,
            start: 0.0,
            end,
            ratio,
        }
    }

    #[test]
    fn a_detached_bus_refuses_to_publish_rather_than_panicking() {
        let bus = FilePreviewBus::new();
        assert!(!bus.is_open());
        assert!(matches!(
            bus.stop(),
            Err(PreviewFileError::Source(SourceError::Audio(
                AudioIoError::NotRunning
            )))
        ));
    }

    #[test]
    fn an_open_bus_sums_a_preview_into_the_master_bus() {
        let collector = Collector::new();
        let bus = Arc::new(FilePreviewBus::new());
        let mut node = file_preview_node(&bus, collector.handle());
        assert!(bus.is_open());

        let source = Arc::new(AudioSource::from_stereo(
            0,
            Path::new("kick.wav"),
            SR,
            vec![0.25; 2_000 * CHANNELS],
        ));
        bus.install(Some(take(source, 1.0))).expect("publishes");

        let mut buffers = AudioBuffers::new(BLOCK);
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                sample_pos: 0,
                playing: false,
                bpm: 120.0,
                mode: PlayMode::Pattern,
            },
        );
        for channel in ctx.buffers.as_mut_slices() {
            channel.fill(0.5);
        }
        node.process(&mut ctx);

        let out = buffers.as_slices()[0];
        assert!(
            out.iter().any(|sample| *sample > 0.5),
            "preview must add to the bus while the transport is stopped"
        );
    }

    #[test]
    fn a_finished_preview_retires_through_the_collector_not_on_the_rt_thread() {
        // The ownership contract clips.rs states for arrangements, held here
        // too: the node keeps no `Arc` of its own, so the only references are
        // the caller's and the `Owned` queued for the collector -- the drop
        // that frees the decoded audio never runs in the callback.
        let mut collector = Collector::new();
        let bus = Arc::new(FilePreviewBus::new());
        let mut node = bus.open(collector.handle());

        let source = Arc::new(AudioSource::from_stereo(
            0,
            Path::new("hit.wav"),
            SR,
            vec![0.25; 300 * CHANNELS],
        ));
        bus.install(Some(take(Arc::clone(&source), 1.0)))
            .expect("publishes");

        let mut buffers = AudioBuffers::new(BLOCK);
        // 300 frames plus the 2 ms fade fit in two blocks; render a third so
        // the take is definitely finished and let go.
        for block in 0..3_u64 {
            let mut ctx = ProcessCtx::new(
                &mut buffers,
                SR,
                BLOCK,
                TransportSnapshot {
                    sample_pos: block * BLOCK as u64,
                    playing: false,
                    bpm: 120.0,
                    mode: PlayMode::Pattern,
                },
            );
            ctx.buffers.silence();
            node.process(&mut ctx);
        }

        assert_eq!(
            Arc::strong_count(&source),
            2,
            "only the test and the collector's queue may hold the take"
        );
        collector.collect();
        assert_eq!(
            Arc::strong_count(&source),
            1,
            "the collector must be the one that frees it"
        );
    }

    #[test]
    fn a_plus_octave_preview_consumes_the_file_in_half_the_blocks() {
        let collector = Collector::new();
        let bus = Arc::new(FilePreviewBus::new());
        let mut node = bus.open(collector.handle());
        let frames = 200;
        let source = Arc::new(AudioSource::from_stereo(
            0,
            Path::new("octave.wav"),
            SR,
            vec![0.25; frames * CHANNELS],
        ));
        bus.install(Some(take(source, 2.0))).expect("publishes");

        let mut buffers = AudioBuffers::new(BLOCK);
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                sample_pos: 0,
                playing: false,
                bpm: 120.0,
                mode: PlayMode::Pattern,
            },
        );
        ctx.buffers.silence();
        node.process(&mut ctx);
        // 200 frames at ratio 2 is 100 output frames plus the 2 ms fade,
        // which fits in one 256-frame block.
        assert!(
            node_is_idle(&mut node, &mut buffers),
            "octave-up preview must finish inside one block of 256"
        );
    }

    fn node_is_idle(node: &mut Box<dyn RtNode>, buffers: &mut AudioBuffers) -> bool {
        let mut ctx = ProcessCtx::new(
            buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                sample_pos: 0,
                playing: false,
                bpm: 120.0,
                mode: PlayMode::Pattern,
            },
        );
        ctx.buffers.silence();
        node.process(&mut ctx);
        buffers.as_slices()[0].iter().all(|sample| *sample == 0.0)
    }
}
