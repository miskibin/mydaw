//! Transport clock shared between the audio thread and the rest of the app.
//!
//! # Why tempo lives here
//! A BPM is not a property of the sequencer: it is a property of *time*. The
//! playhead, the sequencer, a future metronome, a piano roll and every
//! tempo-synced effect all have to agree on it, and the one thing they already
//! share is this clock. Putting it anywhere else would mean a second shared
//! value with a second publication story, which is exactly how a DAW ends up
//! with a sequencer that is one block ahead of its metronome.
//!
//! It is stored as the bits of an `f64` in an `AtomicU64` because there is no
//! `AtomicF64`. Reading it is one relaxed load and a bit-cast; the audio thread
//! does that once per block, into the [`TransportSnapshot`], so every node in
//! the graph sees the same tempo for the whole block.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

/// What the transport is playing.
///
/// FL Studio's PAT/SONG switch, and the same two answers: the pattern you are
/// editing, or the arrangement you built out of them.
///
/// # Why this is here and not in the sequencer
/// For the same reason the tempo is (see the module docs): it is a property of
/// *the transport*, not of whatever happens to be reading it. The playhead, the
/// sequencer, a future metronome and a UI badge all have to agree on it, and
/// putting it in the crate that owns one of those readers would mean rt-core
/// depending on the sequencer or the mode travelling by a second route with a
/// second publication story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayMode {
    /// Loop the pattern set's active pattern, forever, from tick zero.
    #[default]
    Pattern,
    /// Play the playlist from the transport's position to the end of the last
    /// clip.
    Song,
}

impl PlayMode {
    /// Stable wire name: `pat` or `song`.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Pattern => "pat",
            Self::Song => "song",
        }
    }

    /// Parse a wire name.
    ///
    /// Accepts `pattern` as well as `pat`, because both spellings are obvious
    /// and a mode switch failing over a synonym would be an infuriating bug.
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "pat" | "pattern" => Some(Self::Pattern),
            "song" => Some(Self::Song),
            _ => None,
        }
    }

    /// Whether this is song mode.
    pub const fn is_song(self) -> bool {
        matches!(self, Self::Song)
    }

    /// As stored in the clock's atomic.
    const fn as_u8(self) -> u8 {
        match self {
            Self::Pattern => 0,
            Self::Song => 1,
        }
    }

    /// Back from it. Anything else is pattern mode, which is the safe answer:
    /// a corrupt byte must not make the transport play an arrangement.
    const fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::Song,
            _ => Self::Pattern,
        }
    }
}

/// Whether a fresh session loops the arrangement.
///
/// On, because a playlist that plays once and stops is a playlist you restart
/// by hand every eight bars while you are writing the thing. Every DAW loops
/// the arrangement while you work; FL calls it song loop and has it on.
pub const DEFAULT_LOOPING: bool = true;

/// Tempo a fresh session starts at.
///
/// 130 rather than 120: this is a DAW for making dance music on, and the first
/// four-on-the-floor anyone clicks in should already feel like one.
pub const DEFAULT_BPM: f64 = 130.0;

/// Slowest tempo the transport accepts.
pub const MIN_BPM: f64 = 20.0;

/// Fastest tempo the transport accepts.
///
/// 999 is the conventional DAW ceiling. It also keeps the step length safely
/// above one sample at every supported rate (44.1 kHz / 999 BPM / 4 is still
/// 662 samples), so nothing downstream has to defend against a zero-length
/// step.
pub const MAX_BPM: f64 = 999.0;

/// Clamp a tempo into `MIN_BPM..=MAX_BPM`, mapping NaN to [`DEFAULT_BPM`].
///
/// A NaN cannot be clamped -- `f64::clamp` panics on a NaN bound and returns
/// NaN for a NaN value -- and a NaN tempo would turn every step boundary into a
/// NaN. Refusing it here means no code downstream has to ask.
pub fn clamp_bpm(bpm: f64) -> f64 {
    if bpm.is_nan() {
        DEFAULT_BPM
    } else {
        bpm.clamp(MIN_BPM, MAX_BPM)
    }
}

/// Immutable transport state for the duration of one block.
///
/// Taken once per block ([`TransportClock::snapshot`]) so that every node in the
/// graph sees the *same* position and tempo, instead of racing on the atomics.
///
/// Not `Eq`: it carries an `f64`. Compare fields, or compare with a tolerance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportSnapshot {
    /// Sample position of the first frame of this block.
    pub sample_pos: u64,
    pub playing: bool,
    /// Tempo in beats per minute, already clamped to `MIN_BPM..=MAX_BPM`.
    pub bpm: f64,
    /// Whether the transport is playing a pattern or the arrangement.
    pub mode: PlayMode,
}

// The song loop is deliberately **not** in the snapshot. Every node in the
// graph reads a snapshot per block, and none of them has any business knowing
// whether the arrangement repeats -- only the scheduler does, and it is handed
// a `TransportWindow` the callback builds. Putting it here would have added a
// field to every offline render in the test suite to say nothing to any of them.

impl Default for TransportSnapshot {
    /// Stopped at zero, at [`DEFAULT_BPM`].
    ///
    /// Hand-written rather than derived: a derived `Default` would give a tempo
    /// of 0 BPM, which is not a tempo, and every offline render in the test
    /// suite builds its snapshot from this.
    fn default() -> Self {
        Self {
            sample_pos: 0,
            playing: false,
            bpm: DEFAULT_BPM,
            mode: PlayMode::Pattern,
        }
    }
}

/// Sample-accurate play position and tempo, shared via `Arc`.
///
/// # RT safety
/// Plain atomics, no locks: the audio thread reads a snapshot at the top of the
/// block and calls [`advance`](Self::advance) at the bottom. Non-RT threads (UI,
/// IPC) may read at any time; they get a possibly-one-block-stale value, which
/// is exactly what the UI wants (it interpolates the playhead locally anyway).
pub struct TransportClock {
    sample_pos: AtomicU64,
    playing: AtomicBool,
    /// Whether song mode repeats the arrangement. See [`DEFAULT_LOOPING`].
    looping: AtomicBool,
    /// `f64::to_bits` of the tempo. See the module docs.
    bpm_bits: AtomicU64,
    /// [`PlayMode`] as a byte. One relaxed load per block, like the tempo.
    mode: AtomicU8,
    sample_rate: f64,
}

impl TransportClock {
    /// A stopped clock at [`DEFAULT_BPM`].
    pub fn new(sample_rate: f64) -> Self {
        Self::with_bpm(sample_rate, DEFAULT_BPM)
    }

    /// A stopped clock at `bpm` (clamped).
    pub fn with_bpm(sample_rate: f64, bpm: f64) -> Self {
        Self {
            sample_pos: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            looping: AtomicBool::new(DEFAULT_LOOPING),
            bpm_bits: AtomicU64::new(clamp_bpm(bpm).to_bits()),
            mode: AtomicU8::new(PlayMode::Pattern.as_u8()),
            sample_rate,
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Read the current state. RT-safe.
    pub fn snapshot(&self) -> TransportSnapshot {
        TransportSnapshot {
            sample_pos: self.sample_pos.load(Ordering::Relaxed),
            playing: self.playing.load(Ordering::Relaxed),
            bpm: self.bpm(),
            mode: self.play_mode(),
        }
    }

    /// Advance the play head by `frames`, but only while playing.
    ///
    /// Called by the audio thread *after* a block has been processed, so that
    /// `snapshot().sample_pos` always names the first frame of the block that is
    /// currently being rendered.
    pub fn advance(&self, frames: usize) {
        if self.playing.load(Ordering::Relaxed) {
            self.sample_pos.fetch_add(frames as u64, Ordering::Relaxed);
        }
    }

    pub fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Jump to an absolute sample position.
    pub fn seek(&self, sample_pos: u64) {
        self.sample_pos.store(sample_pos, Ordering::Relaxed);
    }

    /// Current tempo in beats per minute. Always inside `MIN_BPM..=MAX_BPM`.
    pub fn bpm(&self) -> f64 {
        f64::from_bits(self.bpm_bits.load(Ordering::Relaxed))
    }

    /// Set the tempo, clamped to `MIN_BPM..=MAX_BPM`. Returns what was stored.
    ///
    /// Clamping rather than rejecting: this is reachable from a webview number
    /// input, and a tempo of 0 or NaN must not be able to reach the scheduler's
    /// arithmetic. The caller that cares (an IPC command) validates first and
    /// reports; this is the last line of defence.
    pub fn set_bpm(&self, bpm: f64) -> f64 {
        let clamped = clamp_bpm(bpm);
        self.bpm_bits.store(clamped.to_bits(), Ordering::Relaxed);
        clamped
    }

    /// Whether the transport is playing a pattern or the arrangement.
    pub fn play_mode(&self) -> PlayMode {
        PlayMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    /// Set the play mode. Takes effect on the audio thread's next block.
    pub fn set_play_mode(&self, mode: PlayMode) {
        self.mode.store(mode.as_u8(), Ordering::Relaxed);
    }

    /// Whether song mode repeats the arrangement rather than stopping at it.
    pub fn is_looping(&self) -> bool {
        self.looping.load(Ordering::Relaxed)
    }

    /// Turn the song loop on or off. Takes effect on the next block.
    pub fn set_looping(&self, looping: bool) {
        self.looping.store(looping, Ordering::Relaxed);
    }

    /// Current position in seconds, for display purposes.
    pub fn position_seconds(&self) -> f64 {
        self.sample_pos.load(Ordering::Relaxed) as f64 / self.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_advance_while_stopped() {
        let t = TransportClock::new(48_000.0);
        t.advance(512);
        assert_eq!(t.snapshot().sample_pos, 0);
    }

    #[test]
    fn advances_while_playing() {
        let t = TransportClock::new(48_000.0);
        t.set_playing(true);
        t.advance(512);
        t.advance(512);
        let s = t.snapshot();
        assert_eq!(s.sample_pos, 1024);
        assert!(s.playing);
    }

    #[test]
    fn seek_and_seconds() {
        let t = TransportClock::new(48_000.0);
        t.seek(48_000);
        assert_eq!(t.snapshot().sample_pos, 48_000);
        assert!((t.position_seconds() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_fresh_clock_runs_at_the_default_tempo_and_the_snapshot_carries_it() {
        let t = TransportClock::new(48_000.0);
        assert_eq!(t.bpm(), DEFAULT_BPM);
        assert_eq!(t.snapshot().bpm, DEFAULT_BPM);
        assert_eq!(TransportSnapshot::default().bpm, DEFAULT_BPM);
        assert_eq!(DEFAULT_BPM, 130.0, "the documented starting tempo");
    }

    #[test]
    fn the_tempo_round_trips_through_the_bits_and_reaches_the_snapshot() {
        let t = TransportClock::with_bpm(44_100.0, 174.3);
        assert_eq!(t.bpm(), 174.3, "exactly, not approximately: it is the bits");
        assert_eq!(t.snapshot().bpm, 174.3);
        assert_eq!(t.set_bpm(90.5), 90.5);
        assert_eq!(t.snapshot().bpm, 90.5);
    }

    #[test]
    fn a_tempo_the_ui_should_never_have_sent_is_clamped_rather_than_propagated() {
        let t = TransportClock::new(48_000.0);
        assert_eq!(t.set_bpm(0.0), MIN_BPM);
        assert_eq!(t.set_bpm(-40.0), MIN_BPM);
        assert_eq!(t.set_bpm(100_000.0), MAX_BPM);
        assert_eq!(t.set_bpm(f64::INFINITY), MAX_BPM);
        assert_eq!(t.set_bpm(f64::NEG_INFINITY), MIN_BPM);
        // A NaN cannot be clamped into a range; it becomes the default instead
        // of being allowed to poison every step boundary downstream.
        assert_eq!(t.set_bpm(f64::NAN), DEFAULT_BPM);
        assert!(t.bpm().is_finite());
    }

    #[test]
    fn the_play_mode_starts_on_pattern_and_reaches_the_snapshot() {
        let t = TransportClock::new(48_000.0);
        assert_eq!(t.play_mode(), PlayMode::Pattern);
        assert_eq!(t.snapshot().mode, PlayMode::Pattern);
        t.set_play_mode(PlayMode::Song);
        assert_eq!(t.play_mode(), PlayMode::Song);
        assert_eq!(t.snapshot().mode, PlayMode::Song);
        assert_eq!(TransportSnapshot::default().mode, PlayMode::Pattern);
    }

    #[test]
    fn the_play_mode_round_trips_through_its_wire_name() {
        assert_eq!(PlayMode::Pattern.id(), "pat");
        assert_eq!(PlayMode::Song.id(), "song");
        assert_eq!(PlayMode::parse("pat"), Some(PlayMode::Pattern));
        assert_eq!(PlayMode::parse("pattern"), Some(PlayMode::Pattern));
        assert_eq!(PlayMode::parse("song"), Some(PlayMode::Song));
        assert_eq!(PlayMode::parse("PAT"), None);
        assert_eq!(PlayMode::parse(""), None);
        assert_eq!(PlayMode::default(), PlayMode::Pattern);
        assert!(PlayMode::Song.is_song());
        assert!(!PlayMode::Pattern.is_song());
        // A byte the clock could never have stored must not play a song.
        assert_eq!(PlayMode::from_u8(200), PlayMode::Pattern);
        for mode in [PlayMode::Pattern, PlayMode::Song] {
            assert_eq!(PlayMode::from_u8(mode.as_u8()), mode);
        }
    }

    #[test]
    fn the_tempo_bounds_are_the_documented_ones() {
        assert_eq!((MIN_BPM, MAX_BPM), (20.0, 999.0));
        assert_eq!(clamp_bpm(MIN_BPM), MIN_BPM);
        assert_eq!(clamp_bpm(MAX_BPM), MAX_BPM);
        assert_eq!(clamp_bpm(130.0), 130.0);
    }
}
