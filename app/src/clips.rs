//! The shell's end of the audio-clip player.
//!
//! ```text
//!   AudioClip track head ─▶ [inserts] ─▶ strip ─▶ Σ ─▶ master
//!            ▲
//!            └── AudioClipBus::open_for_source / install
//! ```
//!
//! Each audio-clip **channel** owns a node at the head of its mixer strip, so
//! volume, pan, mute and inserts are the strip's. The bus still holds one
//! compiled arrangement and fans it out: a node for file 3 only receives clips
//! of file 3.
//!
//! A graph rebuild produces new nodes with empty queues. The bus remembers the
//! last song and replays the right slice into every node it opens.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mydaw_plugin_audio_io::AudioIoError;
use mydaw_plugin_sequencer::{
    audio_clip_channel, AudioClipHandle, AudioClipInstallError, AudioClipNode, AudioClipStats,
    AudioSourceRegistry, CompiledAudioSong, OneShotNode, SampleChannelHead, SamplePlaybackParams,
    AUDIO_CLIP_QUEUE_CAPACITY,
};
use mydaw_rt_core::basedrop::Handle;
use mydaw_rt_core::RtNode;
use parking_lot::Mutex;

use crate::source::{SilentNode, SourceError};

struct Slot {
    /// `None` plays every clip (tests). `Some(id)` is one audio-clip channel.
    source_id: Option<u64>,
    handle: AudioClipHandle,
}

/// The shell's handle on the audio-clip player. `Send + Sync`.
pub struct AudioClipBus {
    attached: AtomicBool,
    slots: Mutex<Vec<Slot>>,
    stats: Mutex<Option<Arc<AudioClipStats>>>,
    last: Mutex<Option<CompiledAudioSong>>,
    sources: Mutex<Option<Arc<AudioSourceRegistry>>>,
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
            attached: AtomicBool::new(false),
            slots: Mutex::new(Vec::new()),
            stats: Mutex::new(None),
            last: Mutex::new(None),
            sources: Mutex::new(None),
        }
    }

    /// The registry one-shot nodes read files from. Called once at boot.
    pub fn bind_sources(&self, sources: Arc<AudioSourceRegistry>) {
        *self.sources.lock() = Some(sources);
    }

    /// Drop every slot before a graph rebuild fills them again.
    pub fn begin_rebuild(&self) {
        self.slots.lock().clear();
    }

    /// Unfiltered node: every clip. Used by tests.
    pub fn open(&self, gc: Handle) -> Box<dyn RtNode> {
        Box::new(self.open_filtered(None, gc))
    }

    /// Node that plays only clips of `source_id`, plus gated notes of that file.
    pub fn open_for_source(
        &self,
        source_id: u64,
        params: Arc<SamplePlaybackParams>,
        gc: Handle,
    ) -> Box<dyn RtNode> {
        let arrangement = self.open_filtered(Some(source_id), gc);
        match self.note_player(source_id, params) {
            Some(notes) => Box::new(SampleChannelHead::new(arrangement, notes)),
            None => Box::new(arrangement),
        }
    }

    /// The same node, **not** joined to the live stream's fan-out.
    ///
    /// An offline render (`crate::resample`) needs a clip channel loaded with
    /// the arrangement as it stands, but it must not become one of the slots
    /// [`install`](Self::install) publishes to: the render is over in a few
    /// hundred milliseconds and its nodes are dropped, and a slot list that
    /// collected them would be publishing the arrangement into dead queues.
    pub fn detached_for_source(
        &self,
        source_id: u64,
        params: Arc<SamplePlaybackParams>,
        gc: Handle,
    ) -> Box<dyn RtNode> {
        let (mut handle, node) = audio_clip_channel(AUDIO_CLIP_QUEUE_CAPACITY, gc);
        if let Some(song) = self.last.lock().clone() {
            if let Err(err) = handle.install(song.for_source(source_id)) {
                log::warn!("could not load the arrangement into an offline render: {err}");
            }
        }
        // The handle is dropped here on purpose: the node keeps whatever was
        // installed, and an offline render never publishes a second version.
        match self.note_player(source_id, params) {
            Some(notes) => Box::new(SampleChannelHead::new(node, notes)),
            None => Box::new(node),
        }
    }

    fn open_filtered(&self, source_id: Option<u64>, gc: Handle) -> AudioClipNode {
        let (mut handle, node) = audio_clip_channel(AUDIO_CLIP_QUEUE_CAPACITY, gc);
        if let Some(song) = self.last.lock().clone() {
            let slice = match source_id {
                Some(id) => song.for_source(id),
                None => song,
            };
            if let Err(err) = handle.install(slice) {
                log::warn!("could not replay the arrangement into a new graph: {err}");
            }
        }
        *self.stats.lock() = Some(Arc::clone(handle.stats()));
        self.slots.lock().push(Slot { source_id, handle });
        self.attached.store(true, Ordering::Release);
        node
    }

    fn note_player(
        &self,
        source_id: u64,
        params: Arc<SamplePlaybackParams>,
    ) -> Option<OneShotNode> {
        let sources = self.sources.lock();
        sources
            .as_ref()
            .and_then(|reg| reg.get(source_id))
            .map(|source| OneShotNode::with_params(source, params))
    }

    /// A note-driven player of `source_id`, or silence if the file is not loaded.
    pub fn oneshot_node(
        &self,
        source_id: u64,
        params: Arc<SamplePlaybackParams>,
    ) -> Box<dyn RtNode> {
        let sources = self.sources.lock();
        match sources.as_ref().and_then(|reg| reg.get(source_id)) {
            Some(source) => Box::new(OneShotNode::with_params(source, params)),
            None => {
                log::warn!("audio source {source_id} is not loaded; the sample channel is silent");
                Box::new(SilentNode)
            }
        }
    }

    /// Forget the current stream's channels. Called when audio stops.
    pub fn close(&self) {
        self.slots.lock().clear();
        *self.stats.lock() = None;
        self.attached.store(false, Ordering::Release);
    }

    /// Publish an arrangement to every open clip node.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] when no stream is open, or
    /// [`SourceError::QueueFull`] when the audio thread is behind (retryable).
    pub fn install(&self, song: CompiledAudioSong) -> Result<(), SourceError> {
        *self.last.lock() = Some(song.clone());
        if !self.attached.load(Ordering::Acquire) {
            return Err(SourceError::Audio(AudioIoError::NotRunning));
        }
        let mut slots = self.slots.lock();
        let mut full = false;
        slots.retain_mut(|slot| {
            let slice = match slot.source_id {
                Some(id) => song.for_source(id),
                None => song.clone(),
            };
            match slot.handle.install(slice) {
                Ok(()) => true,
                Err(AudioClipInstallError::Disconnected) => false,
                Err(AudioClipInstallError::QueueFull) => {
                    full = true;
                    true
                }
            }
        });
        if full {
            return Err(SourceError::QueueFull);
        }
        Ok(())
    }

    /// Whether a stream's channel is attached.
    pub fn is_open(&self) -> bool {
        self.attached.load(Ordering::Acquire) && !self.slots.lock().is_empty()
    }

    /// Voices the current stream has taken from a still-sounding clip.
    pub fn voices_stolen(&self) -> u64 {
        self.slots
            .lock()
            .iter()
            .map(|slot| slot.handle.stats().voices_stolen())
            .sum::<u64>()
            .max(
                self.stats
                    .lock()
                    .as_ref()
                    .map_or(0, |stats| stats.voices_stolen()),
            )
    }

    /// Voices the current stream has started. Diagnostics and tests.
    pub fn voices_started(&self) -> u64 {
        self.slots
            .lock()
            .iter()
            .map(|slot| slot.handle.stats().voices_started())
            .sum::<u64>()
            .max(
                self.stats
                    .lock()
                    .as_ref()
                    .map_or(0, |stats| stats.voices_started()),
            )
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
            end_frames: 0,
            pitch_ratio: 1.0,
            gain: 1.0,
            fade_in_ms: 0.0,
            fade_out_ms: 0.0,
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
