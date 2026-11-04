//! Proves the scheduler and the song slot allocate nothing on the audio thread,
//! in both play modes.
//!
//! The scheduler runs *before* the graph, on the same callback, under the same
//! rule (see [`RtNode`](mydaw_rt_core::RtNode)'s zero-alloc invariant). Reading
//! the code is not evidence -- and it is *less* evidence than it used to be,
//! because a `CompiledSong` is built out of `Vec`s. The claim is that they are
//! built off this thread and only indexed on it, which is exactly the sort of
//! claim a counting allocator settles and an argument does not.
//! `SongSlot::poll_swap` also drops a `basedrop::Owned` right here, the whole
//! point of which is that dropping it must *not* free anything.
//!
//! Same harness as `plugins/instruments/tests/rt_alloc.rs`: a counting global
//! allocator armed per-thread, so the test's own setup is not counted, and the
//! counter is proved to work before anything is asserted with it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use std::sync::Arc;

use mydaw_plugin_sequencer::{
    audio_clip_channel, song_channel, AudioSource, ChannelClip, CompiledAudioClip,
    CompiledAudioSong, CompiledPatternClip, CompiledSong, MelodicNote, OneShotNode, Pattern,
    PlayMode, SampleChannelHead, SamplePlaybackParams, Scheduler, Step, StepRow, TransportWindow,
    AUDIO_CLIP_QUEUE_CAPACITY, AUDIO_CLIP_VOICES, MAX_NOTES, MAX_PATTERN_CHANNELS, MAX_STEPS,
    TICKS_PER_BAR,
};
use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{
    AudioBuffers, NoteEvent, NoteEventBuffer, ProcessCtx, RtNode, TransportSnapshot, CHANNELS,
    DEFAULT_BPM,
};

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// `System`, plus a per-thread counter of allocation events.
struct CountingAllocator;

// SAFETY: every method forwards to `System`, which upholds the `GlobalAlloc`
// contract; the counter is a side effect that touches no allocator state.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Freeing on the audio thread is just as forbidden as allocating, and
        // for `basedrop` it is the whole property under test.
        record();
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc_zeroed(layout) }
    }
}

fn record() {
    // `try_with`: during thread teardown the TLS may already be destroyed, and
    // panicking inside the allocator would abort the process.
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn count_allocations(body: impl FnOnce()) -> u64 {
    ALLOCATIONS.with(|count| count.set(0));
    ARMED.with(|armed| armed.set(true));
    body();
    ARMED.with(|armed| armed.set(false));
    ALLOCATIONS.with(Cell::get)
}

/// The block sizes a real device asks for, plus the awkward ones it should not.
const BLOCK_SIZES: [usize; 8] = [1, 32, 64, 127, 256, 441, 512, 4_096];

/// Every channel hitting every step of the longest pattern, plus a full
/// thousand melodic notes on one of them: the densest pattern MYDAW can express,
/// and therefore the most events one block can be asked to produce.
fn dense_pattern() -> Pattern {
    let mut pattern = Pattern::silent(1, "Dense");
    pattern.length_steps = MAX_STEPS as u8;
    let pattern_ticks = pattern.ticks();
    for channel in 0..MAX_PATTERN_CHANNELS {
        let key = 36 + channel as u8;
        let mut clip = ChannelClip::stepped(channel as u8, key);
        let mut row = StepRow::empty(key);
        for step in 0..MAX_STEPS {
            row.steps[step] = Step::accent();
        }
        clip.steps = Some(row);
        if channel == 0 {
            // Spread across the four bars, several to a tick position, so the
            // scheduler's binary search has ties to get right and its pending
            // array is put under real pressure.
            clip.notes = (0..MAX_NOTES)
                .map(|index| {
                    let start = (index as u32 * 15) % pattern_ticks;
                    MelodicNote::new(
                        36 + (index % 60) as u8,
                        start,
                        (pattern_ticks - start).clamp(1, 480),
                    )
                })
                .collect();
        }
        pattern.channels.push(clip);
    }
    pattern
        .validate()
        .expect("the densest legal pattern is legal");
    pattern
}

/// That pattern, alone, with nothing arranged.
fn dense_song() -> CompiledSong {
    CompiledSong::new(vec![dense_pattern().compile()], 0, Vec::new())
}

/// That pattern arranged as thirty-two overlapping clips: the densest
/// arrangement song mode can be asked to walk in one block.
fn dense_arrangement() -> CompiledSong {
    let bar = TICKS_PER_BAR as u64;
    CompiledSong::new(
        vec![dense_pattern().compile()],
        0,
        (0..32)
            .map(|index| CompiledPatternClip {
                pattern: 0,
                // Overlapping on purpose: an arrangement where only one clip is
                // ever live would not exercise the loop this is bounding.
                start_ticks: index * bar,
                length_ticks: 8 * bar,
            })
            .collect(),
    )
}

#[test]
fn the_allocation_counter_actually_counts() {
    let allocating = count_allocations(|| {
        let buffer: Vec<f32> = Vec::with_capacity(1_024);
        std::hint::black_box(&buffer);
    });
    assert!(
        allocating >= 1,
        "the instrument is broken: an allocation was not counted"
    );

    let arithmetic = count_allocations(|| {
        let mut total = 0.0_f64;
        for i in 0..1_000 {
            total += i as f64;
        }
        std::hint::black_box(total);
    });
    assert_eq!(arithmetic, 0, "pure arithmetic must not be counted");
}

#[test]
fn scheduling_allocates_nothing_across_modes_tempos_rates_and_block_sizes() {
    let pattern_only = dense_song();
    let arrangement = dense_arrangement();
    // Built before arming: the event buffer is the host's, not the scheduler's.
    let mut buffer = NoteEventBuffer::new(8_192);
    let mut scheduler = Scheduler::new();

    let count = count_allocations(|| {
        for (mode, song) in [
            (PlayMode::Pattern, &pattern_only),
            (PlayMode::Song, &arrangement),
        ] {
            for sample_rate in [44_100.0_f64, 48_000.0] {
                for bpm in [20.0_f64, 130.0, 174.3, 999.0] {
                    for frames in BLOCK_SIZES {
                        scheduler.reset();
                        let mut start = 0_u64;
                        for _ in 0..24 {
                            buffer.clear();
                            scheduler.schedule(
                                Some(song),
                                &TransportWindow {
                                    start_sample: start,
                                    frames,
                                    sample_rate,
                                    bpm,
                                    playing: true,
                                    mode,
                                    looping: false,
                                },
                                &mut buffer,
                            );
                            start += frames as u64;
                        }
                    }
                }
            }
        }
    });

    assert_eq!(count, 0, "the scheduler allocated {count} times");
}

#[test]
fn switching_mode_every_block_allocates_nothing() {
    // The mode edge takes the flush path, and takes it while the pending array
    // is full of a dense pattern's owed note-offs.
    let pattern_only = dense_song();
    let arrangement = dense_arrangement();
    let mut buffer = NoteEventBuffer::new(8_192);
    let mut scheduler = Scheduler::new();

    let count = count_allocations(|| {
        for block in 0..64_u64 {
            let (mode, song) = if block % 2 == 0 {
                (PlayMode::Pattern, &pattern_only)
            } else {
                (PlayMode::Song, &arrangement)
            };
            buffer.clear();
            scheduler.schedule(
                Some(song),
                &TransportWindow {
                    start_sample: block * 512,
                    frames: 512,
                    sample_rate: 48_000.0,
                    bpm: DEFAULT_BPM,
                    playing: true,
                    mode,
                    looping: false,
                },
                &mut buffer,
            );
        }
    });

    assert_eq!(count, 0, "switching mode allocated {count} times");
}

#[test]
fn the_stop_flush_and_the_seek_flush_allocate_nothing() {
    let song = dense_song();
    let mut buffer = NoteEventBuffer::new(2_048);
    let mut scheduler = Scheduler::new();

    let count = count_allocations(|| {
        for round in 0..64_u64 {
            // Play a block, then stop (flush), then seek somewhere else and
            // play again (flush) -- the two paths that must release owed notes.
            for (start, playing) in [
                (round * 4_096, true),
                (round * 4_096 + 4_096, false),
                (round * 90_000 + 1_234, true),
            ] {
                buffer.clear();
                scheduler.schedule(
                    Some(&song),
                    &TransportWindow {
                        start_sample: start,
                        frames: 64,
                        sample_rate: 48_000.0,
                        bpm: DEFAULT_BPM,
                        playing,
                        mode: PlayMode::Pattern,
                        looping: false,
                    },
                    &mut buffer,
                );
            }
        }
        buffer.clear();
        scheduler.flush(&mut buffer);
        scheduler.reset();
    });

    assert_eq!(count, 0, "the flush paths allocated {count} times");
}

#[test]
fn a_full_event_buffer_does_not_make_the_scheduler_allocate() {
    // The overflow branch: 256 events into a 4-slot buffer. Dropping must be a
    // returned count, never a growth.
    let song = dense_song();
    let mut buffer = NoteEventBuffer::new(4);
    let capacity = buffer.capacity();
    let mut scheduler = Scheduler::new();

    let count = count_allocations(|| {
        for block in 0..32_u64 {
            buffer.clear();
            scheduler.schedule(
                Some(&song),
                &TransportWindow {
                    start_sample: block * 4_096,
                    frames: 4_096,
                    sample_rate: 48_000.0,
                    bpm: 999.0,
                    playing: true,
                    mode: PlayMode::Pattern,
                    looping: false,
                },
                &mut buffer,
            );
        }
    });

    assert_eq!(count, 0, "the overflow path allocated {count} times");
    assert_eq!(buffer.capacity(), capacity, "the buffer must never grow");
}

#[test]
fn swapping_the_song_on_the_audio_thread_neither_allocates_nor_frees() {
    // The `basedrop` claim, measured: the slot picks up a new song and drops the
    // old one, and neither may touch the allocator on this thread. This is the
    // test that makes `Vec` inside a `CompiledSong` legitimate rather than
    // merely convenient -- a `Vec` frees its buffer on drop, and dropping here
    // must not free.
    let mut collector = Collector::new();
    let (mut handle, mut slot) = song_channel(8, collector.handle());
    let dense = dense_song();
    let arrangement = dense_arrangement();

    // Installed off the "audio thread" -- that is where the allocation belongs.
    handle.install(dense.clone()).unwrap();
    handle.install(arrangement).unwrap();
    handle.install(dense).unwrap();

    let mut buffer = NoteEventBuffer::new(8_192);
    let mut scheduler = Scheduler::new();

    let count = count_allocations(|| {
        for block in 0..8_u64 {
            slot.poll_swap();
            buffer.clear();
            scheduler.schedule(
                slot.current(),
                &TransportWindow {
                    start_sample: block * 512,
                    frames: 512,
                    sample_rate: 48_000.0,
                    bpm: DEFAULT_BPM,
                    playing: true,
                    mode: PlayMode::Song,
                    looping: false,
                },
                &mut buffer,
            );
        }
    });

    assert_eq!(count, 0, "the song swap allocated or freed {count} times");
    assert!(slot.has_song());
    // And the superseded patterns really were handed to the collector rather
    // than freed: they are still allocated until it runs.
    assert_eq!(collector.alloc_count(), 3);
    collector.collect();
    assert_eq!(collector.alloc_count(), 1, "only the current one survives");
}

// ---------------------------------------------------------------- audio clips

/// A source of `frames` frames at 44.1 kHz -- deliberately *not* the device
/// rate, so the interpolator is actually running rather than short-circuiting
/// on a step of exactly 1.0.
fn clip_source(id: u64, frames: usize) -> Arc<AudioSource> {
    let data: Vec<f32> = (0..frames * CHANNELS)
        .map(|index| ((index % 97) as f32 / 97.0) - 0.5)
        .collect();
    Arc::new(AudioSource::from_stereo(
        id,
        std::path::Path::new("alloc.wav"),
        44_100.0,
        data,
    ))
}

/// More clips than there are voices, all overlapping, so every block is
/// starting, stealing and fading at once.
fn dense_audio_arrangement(source: &Arc<AudioSource>, count: u64) -> CompiledAudioSong {
    let bar = TICKS_PER_BAR as u64;
    CompiledAudioSong::new(
        (0..count)
            .map(|index| CompiledAudioClip {
                clip_id: index + 1,
                route: source.id(),
                source: Arc::clone(source),
                // Staggered by a sixteenth, so a clip starts in almost every
                // block and the pool is never quiet.
                start_ticks: index * 240,
                length_ticks: 4 * bar,
                offset_frames: index % 64,
                end_frames: 0,
                pitch_ratio: 1.0,
                gain: 0.5,
                // A real fade on every clip, so the allocation claim covers the
                // envelope path and not only the anti-click ramp.
                fade_in_ms: 25.0,
                fade_out_ms: 120.0,
            })
            .collect(),
    )
}

/// The clip player is an `RtNode` on the master bus, under the same rule as
/// every other node: no allocation, no free, in the callback.
///
/// This drives it through the whole state machine -- start, steal, seek, stop,
/// mode switch -- because the interesting allocations would be in the paths that
/// only run at an edge.
#[test]
fn rendering_audio_clips_allocates_nothing_across_starts_seeks_stops_and_steals() {
    let collector = Collector::new();
    let (mut handle, mut node) = audio_clip_channel(8, collector.handle());
    let source = clip_source(1, 44_100 * 4);
    handle
        .install(dense_audio_arrangement(
            &source,
            AUDIO_CLIP_VOICES as u64 + 16,
        ))
        .unwrap();

    let mut buffers = AudioBuffers::new(4_096);

    for frames in BLOCK_SIZES {
        let mut sample_pos = 0_u64;
        let count = count_allocations(|| {
            for block in 0..24_u64 {
                // Every fifth block is a seek and every eleventh is a stop, so
                // the edge paths -- release-all, derive-covering-clips -- are
                // exercised rather than only the steady state.
                if block % 5 == 4 {
                    sample_pos = sample_pos.wrapping_add(97_003);
                }
                let playing = block % 11 != 10;
                let mode = if block % 7 == 6 {
                    PlayMode::Pattern
                } else {
                    PlayMode::Song
                };
                let mut ctx = ProcessCtx::new(
                    &mut buffers,
                    48_000.0,
                    frames,
                    TransportSnapshot {
                        sample_pos,
                        playing,
                        bpm: DEFAULT_BPM,
                        mode,
                    },
                );
                ctx.buffers.silence();
                node.process(&mut ctx);
                sample_pos += frames as u64;
            }
        });
        assert_eq!(
            count, 0,
            "{frames}-frame blocks: the clip player allocated or freed {count} times"
        );
    }
}

/// Swapping the arrangement on the audio thread must neither allocate nor free
/// -- and the superseded one must go to the collector, which is the whole point
/// of `basedrop` here: the last `Arc<AudioSource>` in it can be a hundred
/// megabytes, and `free()` in a callback is a dropout.
#[test]
fn swapping_the_arrangement_on_the_audio_thread_neither_allocates_nor_frees() {
    let mut collector = Collector::new();
    let (mut handle, mut node) = audio_clip_channel(8, collector.handle());
    let source = clip_source(1, 44_100);

    // Three arrangements, all built off the "audio thread" -- that is where the
    // allocation belongs. The middle one drops every clip, so the swap has to
    // retire voices rather than rebind them.
    handle.install(dense_audio_arrangement(&source, 8)).unwrap();

    let mut buffers = AudioBuffers::new(512);
    let mut render = |node: &mut _, pos: u64| {
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            48_000.0,
            512,
            TransportSnapshot {
                sample_pos: pos,
                playing: true,
                bpm: DEFAULT_BPM,
                mode: PlayMode::Song,
            },
        );
        ctx.buffers.silence();
        RtNode::process(node, &mut ctx);
    };
    render(&mut node, 0);
    render(&mut node, 512);

    handle.install(CompiledAudioSong::empty()).unwrap();
    handle.install(dense_audio_arrangement(&source, 4)).unwrap();

    let count = count_allocations(|| {
        for block in 2..8_u64 {
            render(&mut node, block * 512);
        }
    });
    assert_eq!(
        count, 0,
        "the arrangement swap allocated or freed {count} times"
    );

    // The superseded arrangements are still allocated: they were handed to the
    // collector, not freed on the audio thread.
    assert!(collector.alloc_count() >= 2, "{}", collector.alloc_count());
    collector.collect();
    collector.collect();
    assert_eq!(
        collector.alloc_count(),
        1,
        "only the current arrangement survives"
    );
}

#[test]
fn reading_live_one_shot_parameters_allocates_nothing() {
    let source = Arc::new(AudioSource::from_stereo(
        1,
        std::path::Path::new("hit.wav"),
        48_000.0,
        vec![0.5; 48_000 * CHANNELS],
    ));
    let params = Arc::new(SamplePlaybackParams::new(0, 0, 0.0, (0.0, 0.0)));
    let mut node = OneShotNode::with_params(source, Arc::clone(&params));
    let mut buffers = AudioBuffers::new(256);

    let first = [NoteEvent::note_on(0, 60, 1.0)];
    let mut ctx = ProcessCtx::with_events(
        &mut buffers,
        48_000.0,
        256,
        TransportSnapshot::default(),
        &first,
    );
    assert_eq!(count_allocations(|| node.process(&mut ctx)), 0);

    params.set_pitch_cents(1_200.0);
    params.set_fades(250.0, 500.0);
    params.set_region(24, 24_000);
    params.set_root(54, 12.0);
    let second = [NoteEvent::note_on(0, 61, 0.8)];
    let mut ctx = ProcessCtx::with_events(
        &mut buffers,
        48_000.0,
        256,
        TransportSnapshot::default(),
        &second,
    );
    assert_eq!(
        count_allocations(|| node.process(&mut ctx)),
        0,
        "snapshotting edited one-shot parameters must remain RT-safe"
    );
}

#[test]
fn a_sample_channel_head_note_and_root_change_allocates_nothing() {
    let source = Arc::new(AudioSource::from_stereo(
        1,
        std::path::Path::new("hit.wav"),
        48_000.0,
        vec![0.5; 48_000 * CHANNELS],
    ));
    let params = Arc::new(SamplePlaybackParams::new(0, 0, 0.0, (0.0, 0.0)));
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
    let collector = Collector::new();
    let (mut handle, arrangement) =
        audio_clip_channel(AUDIO_CLIP_QUEUE_CAPACITY, collector.handle());
    handle.install(song).unwrap();
    let notes = OneShotNode::with_params(source, Arc::clone(&params));
    let mut node = SampleChannelHead::new(arrangement, notes);
    let mut buffers = AudioBuffers::new(256);
    let on = [NoteEvent::note_on(0, 54, 1.0)];
    let mut ctx = ProcessCtx::with_events(
        &mut buffers,
        48_000.0,
        256,
        TransportSnapshot {
            sample_pos: 0,
            playing: true,
            bpm: 120.0,
            mode: PlayMode::Song,
        },
        &on,
    );
    assert_eq!(count_allocations(|| node.process(&mut ctx)), 0);

    params.set_root(54, 12.0);
    let off = [NoteEvent::note_off(0, 54)];
    let mut ctx = ProcessCtx::with_events(
        &mut buffers,
        48_000.0,
        256,
        TransportSnapshot {
            sample_pos: 256,
            playing: true,
            bpm: 120.0,
            mode: PlayMode::Song,
        },
        &off,
    );
    assert_eq!(
        count_allocations(|| node.process(&mut ctx)),
        0,
        "Audio Clip notes after a root change must remain RT-safe"
    );
}
