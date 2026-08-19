//! The shell's end of the audio-clip player.
//!
//! ```text
//!   tracks ─▶ Σ ─▶ AudioClipNode ─▶ InsertSlot ─▶ master fader ─▶ device
//!                       ▲
//!                       └── AudioClipBus::install(CompiledAudioSong)
//!                           (rtrb + basedrop, off the audio thread)
//! ```
//!
//! [`InsertHandle`](crate::insert::InsertHandle) with a different payload, and
//! for the same two reasons. The **node has to be in every graph** whether or not
//! the arrangement has audio in it, because gaining a first audio clip must not
//! be a graph rebuild; and the **handle has to outlive the stream**, because a
//! channel is created with the stream and a project loaded before the device
//! came up still has to publish itself when it does.
//!
//! It sits **first** among the master inserts, which is what puts a clip on the
//! master bus after every track strip and before the master chain -- §6 of the
//! playlist spec. The node adds to the bus rather than replacing it, so the
//! tracks' sum passes through untouched.
//!
//! # Why this is not on `TrackHost`'s critical path
//! `TrackHost` owns it (it is the thing that assembles the graph) but nothing
//! about a *track* is involved: a playlist lane is not a mixer strip. The
//! ownership is about who builds the spec, not about what a clip belongs to.

use std::sync::Arc;

use mydaw_plugin_audio_io::AudioIoError;
use mydaw_plugin_sequencer::{
    audio_clip_channel, AudioClipHandle, AudioClipInstallError, AudioClipStats, CompiledAudioSong,
    AUDIO_CLIP_QUEUE_CAPACITY,
};
use mydaw_rt_core::basedrop::Handle;
use mydaw_rt_core::RtNode;
use parking_lot::Mutex;

use crate::source::SourceError;

/// The shell's handle on the audio-clip player. `Send + Sync`.
///
/// Created once at boot and reused across stream restarts.
pub struct AudioClipBus {
    /// `None` until a stream is open. See [`open`](Self::open).
    sender: Mutex<Option<AudioClipHandle>>,
    /// The counters of the *current* stream's node, so the UI can ask about
    /// stolen voices without reaching across the RT wall.
    stats: Mutex<Option<Arc<AudioClipStats>>>,
    /// The last arrangement published, replayed into every node this bus opens.
    ///
    /// A graph rebuild (adding a track, changing a source) produces a **new**
    /// clip node with an empty queue, and the arrangement is not the graph's to
    /// lose. Rather than making every caller that can trigger a rebuild
    /// remember to republish -- which is the kind of obligation that is honoured
    /// in four places and forgotten in the fifth -- the bus replays it itself.
    /// The copy is one `Arc` bump per clip; the audio is not duplicated.
    last: Mutex<Option<CompiledAudioSong>>,
}

impl Default for AudioClipBus {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioClipBus {
    /// An empty, detached bus.
    pub fn new() -> Self {
        Self {
            sender: Mutex::new(None),
            stats: Mutex::new(None),
            last: Mutex::new(None),
        }
    }

    /// Build the channel for a freshly opened stream and take the RT end.
    ///
    /// The node starts with **no arrangement**, exactly as the insert slot
    /// starts empty: the caller republishes afterwards, which is the same path a
    /// project load takes.
    pub fn open(&self, gc: Handle) -> Box<dyn RtNode> {
        let (mut handle, node) = audio_clip_channel(AUDIO_CLIP_QUEUE_CAPACITY, gc);
        if let Some(song) = self.last.lock().clone() {
            // A queue this fresh cannot be full, so this cannot fail for a
            // reason worth reporting -- and failing to open a stream because an
            // arrangement did not fit would be the wrong trade anyway.
            if let Err(err) = handle.install(song) {
                log::warn!("could not replay the arrangement into a new graph: {err}");
            }
        }
        *self.stats.lock() = Some(Arc::clone(handle.stats()));
        *self.sender.lock() = Some(handle);
        Box::new(node)
    }

    /// Forget the current stream's channel. Called when audio stops.
    pub fn close(&self) {
        *self.sender.lock() = None;
        *self.stats.lock() = None;
    }

    /// Publish an arrangement to the audio thread.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] when no stream is open, or
    /// [`SourceError::QueueFull`] when the audio thread is behind (retryable).
    pub fn install(&self, song: CompiledAudioSong) -> Result<(), SourceError> {
        // Remembered *before* the publish and whether or not it lands: with no
        // device open there is nothing to publish to, and the arrangement still
        // has to be there when one comes up.
        *self.last.lock() = Some(song.clone());
        let mut guard = self.sender.lock();
        let handle = guard.as_mut().ok_or(AudioIoError::NotRunning)?;
        handle.install(song).map_err(|err| match err {
            AudioClipInstallError::QueueFull => SourceError::QueueFull,
            AudioClipInstallError::Disconnected => SourceError::Audio(AudioIoError::NotRunning),
        })
    }

    /// Whether a stream's channel is attached.
    pub fn is_open(&self) -> bool {
        self.sender.lock().is_some()
    }

    /// Voices the current stream has taken from a still-sounding clip.
    ///
    /// `0` with no stream open. Non-zero means an arrangement with more than
    /// `AUDIO_CLIP_VOICES` clips overlapping -- audible as the oldest one
    /// ending early, which is why it is counted rather than left to be noticed.
    pub fn voices_stolen(&self) -> u64 {
        self.stats
            .lock()
            .as_ref()
            .map_or(0, |stats| stats.voices_stolen())
    }

    /// Voices the current stream has started. Diagnostics and tests.
    pub fn voices_started(&self) -> u64 {
        self.stats
            .lock()
            .as_ref()
            .map_or(0, |stats| stats.voices_started())
    }
}

impl std::fmt::Debug for AudioClipBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioClipBus")
            .field("open", &self.is_open())
            .field("voices_stolen", &self.voices_stolen())
            .finish()
    }
}

/// The RT end, as the graph builder wants it. Mirrors
/// [`insert_slot`](crate::insert::insert_slot).
pub fn audio_clip_node(bus: &Arc<AudioClipBus>, gc: Handle) -> Box<dyn RtNode> {
    bus.open(gc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_plugin_sequencer::{AudioSource, CompiledAudioClip};
    use mydaw_rt_core::basedrop::Collector;
    use mydaw_rt_core::{AudioBuffers, PlayMode, ProcessCtx, TransportSnapshot, CHANNELS};

    const SR: f64 = 48_000.0;
    const BLOCK: usize = 256;

    fn source(frames: usize, value: f32) -> Arc<AudioSource> {
        Arc::new(AudioSource::from_stereo(
            1,
            std::path::Path::new("t.wav"),
            SR,
            vec![value; frames * CHANNELS],
        ))
    }

    fn one_clip_song(value: f32) -> CompiledAudioSong {
        CompiledAudioSong::new(vec![CompiledAudioClip {
            clip_id: 1,
            source: source(48_000, value),
            start_ticks: 0,
            length_ticks: 3_840,
            offset_frames: 0,
            gain: 1.0,
        }])
    }

    #[test]
    fn a_detached_bus_refuses_to_publish_rather_than_panicking() {
        let bus = AudioClipBus::new();
        assert!(!bus.is_open());
        assert!(matches!(
            bus.install(CompiledAudioSong::empty()),
            Err(SourceError::Audio(AudioIoError::NotRunning))
        ));
        assert_eq!(bus.voices_stolen(), 0);
    }

    #[test]
    fn an_open_bus_publishes_and_the_node_sums_it_into_the_bus() {
        let collector = Collector::new();
        let bus = Arc::new(AudioClipBus::new());
        let mut node = audio_clip_node(&bus, collector.handle());
        assert!(bus.is_open());
        bus.install(one_clip_song(0.25)).expect("publishes");

        let mut buffers = AudioBuffers::new(BLOCK);
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                sample_pos: 0,
                playing: true,
                bpm: 120.0,
                mode: PlayMode::Song,
            },
        );
        // The track sum is already on the bus when this node runs.
        for channel in ctx.buffers.as_mut_slices() {
            channel.fill(0.5);
        }
        node.process(&mut ctx);

        let out = buffers.as_slices()[0];
        assert!(
            out[200] > 0.7,
            "the clip must be added to the track sum, got {}",
            out[200]
        );
        assert_eq!(bus.voices_started(), 1);
    }

    #[test]
    fn closing_the_bus_detaches_it_and_a_reopen_gets_a_fresh_node() {
        let collector = Collector::new();
        let bus = Arc::new(AudioClipBus::new());
        let _first = audio_clip_node(&bus, collector.handle());
        bus.close();
        assert!(!bus.is_open());
        assert_eq!(bus.voices_stolen(), 0);

        let _second = audio_clip_node(&bus, collector.handle());
        assert!(bus.is_open());
        bus.install(CompiledAudioSong::empty()).expect("publishes");
    }

    #[test]
    fn a_graph_rebuild_does_not_lose_the_arrangement() {
        // Adding a track rebuilds the graph, which makes a *new* clip node with
        // an empty queue. The arrangement is not the graph's to lose.
        let collector = Collector::new();
        let bus = Arc::new(AudioClipBus::new());
        let _first = audio_clip_node(&bus, collector.handle());
        bus.install(one_clip_song(0.25)).expect("publishes");

        let mut rebuilt = audio_clip_node(&bus, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                sample_pos: 0,
                playing: true,
                bpm: 120.0,
                mode: PlayMode::Song,
            },
        );
        ctx.buffers.silence();
        rebuilt.process(&mut ctx);
        assert!(
            buffers.as_slices()[0][200].abs() > 0.2,
            "the clip must survive the rebuild"
        );
    }

    #[test]
    fn an_arrangement_published_before_the_device_came_up_is_replayed_into_it() {
        // A project loaded on a machine whose stream has not started yet.
        let collector = Collector::new();
        let bus = Arc::new(AudioClipBus::new());
        assert!(bus.install(one_clip_song(0.25)).is_err(), "no stream yet");

        let mut node = audio_clip_node(&bus, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                sample_pos: 0,
                playing: true,
                bpm: 120.0,
                mode: PlayMode::Song,
            },
        );
        ctx.buffers.silence();
        node.process(&mut ctx);
        assert!(buffers.as_slices()[0][200].abs() > 0.2);
    }
}
