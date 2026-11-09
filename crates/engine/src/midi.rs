//! Live MIDI: message parsing and the recorder that turns played notes into a pattern.
//!
//! Pure data and arithmetic, no device and no engine state, so it is unit-testable without
//! hardware. The device layer (`wave-midi-in`) parses bytes with [`MidiMsg::parse`] and hands
//! the result to `Engine::midi_event`; the engine feeds a [`MidiRecorder`] while recording.

use crate::model::{Id, Note};

/// Steps (1/16 notes) per beat: the pattern/piano-roll grid unit.
pub const STEPS_PER_BEAT: f64 = 4.0;
/// Pitch-bend range in semitones (the General MIDI default of ±2).
pub const BEND_RANGE: f32 = 2.0;
/// Shortest recorded note, in steps (a very quick tap still shows up and plays).
pub const MIN_NOTE_STEPS: f32 = 0.25;

/// A decoded channel-voice message. `ch` is the MIDI channel 0..15.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MidiMsg {
    NoteOn { ch: u8, key: u8, vel: u8 },
    NoteOff { ch: u8, key: u8 },
    /// CC 64 (damper pedal): down at value ≥ 64.
    Sustain { ch: u8, down: bool },
    /// Pitch bend in −8192..=8191 (0 = centre).
    PitchBend { ch: u8, value: i16 },
    /// CC 1 (mod wheel), 0..=127.
    ModWheel { ch: u8, value: u8 },
    /// CC 120 (all sound off) / CC 123 (all notes off).
    AllNotesOff { ch: u8 },
}

impl MidiMsg {
    /// Decodes one raw MIDI message. A note-on with velocity 0 is a note-off. Anything
    /// else (system messages, aftertouch, other CCs, truncated data) is `None`.
    pub fn parse(b: &[u8]) -> Option<Self> {
        let status = *b.first()?;
        if !(0x80..0xF0).contains(&status) {
            return None;
        }
        let ch = status & 0x0F;
        let d1 = *b.get(1)?;
        if d1 > 0x7F {
            return None;
        }
        let d2 = b.get(2).copied();
        if matches!(d2, Some(x) if x > 0x7F) {
            return None;
        }
        match status & 0xF0 {
            0x80 => Some(Self::NoteOff { ch, key: d1 }),
            0x90 => match d2? {
                0 => Some(Self::NoteOff { ch, key: d1 }),
                vel => Some(Self::NoteOn { ch, key: d1, vel }),
            },
            0xB0 => match (d1, d2?) {
                (64, v) => Some(Self::Sustain { ch, down: v >= 64 }),
                (1, v) => Some(Self::ModWheel { ch, value: v }),
                (120 | 123, _) => Some(Self::AllNotesOff { ch }),
                _ => None,
            },
            0xE0 => Some(Self::PitchBend { ch, value: (((d2? as i16) << 7) | d1 as i16) - 8192 }),
            _ => None,
        }
    }

    /// Velocity 1..127 as the linear gain the pattern format uses (0..1).
    pub fn vel_gain(vel: u8) -> f32 {
        (vel as f32 / 127.0).clamp(0.0, 1.0)
    }

    /// A pitch-bend value in semitones.
    pub fn bend_semitones(value: i16) -> f32 {
        value as f32 / 8192.0 * BEND_RANGE
    }
}

/// One finished played note, in beats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecNote {
    pub key: u8,
    pub vel: u8,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Copy, Debug)]
struct Open {
    key: u8,
    vel: u8,
    start: f64,
    /// Key released while the pedal was down: ends when the pedal comes up.
    sustained: bool,
}

/// Collects played notes with their beat positions. Absolute beats, so a loop that wraps
/// during recording just overdubs into the same range.
#[derive(Clone, Debug, Default)]
pub struct MidiRecorder {
    /// Events before this beat (count-in) are ignored.
    pub from: f64,
    open: Vec<Open>,
    done: Vec<RecNote>,
    pedal: bool,
}

impl MidiRecorder {
    pub fn new(from: f64) -> Self {
        Self { from, ..Default::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty() && self.done.is_empty()
    }

    pub fn note_on(&mut self, beat: f64, key: u8, vel: u8) {
        if beat < self.from {
            return;
        }
        // the same key struck again ends its previous note first
        self.close(beat, key);
        self.open.push(Open { key, vel, start: beat, sustained: false });
    }

    pub fn note_off(&mut self, beat: f64, key: u8) {
        if self.pedal {
            if let Some(o) = self.open.iter_mut().find(|o| o.key == key && !o.sustained) {
                o.sustained = true;
            }
        } else {
            self.close(beat, key);
        }
    }

    pub fn sustain(&mut self, beat: f64, down: bool) {
        self.pedal = down;
        if !down {
            let ending: Vec<u8> = self.open.iter().filter(|o| o.sustained).map(|o| o.key).collect();
            for k in ending {
                self.close(beat, k);
            }
        }
    }

    /// Closes the open note of `key`.
    fn close(&mut self, beat: f64, key: u8) {
        if let Some(i) = self.open.iter().position(|o| o.key == key) {
            let o = self.open.remove(i);
            self.done.push(RecNote { key: o.key, vel: o.vel, start: o.start, end: beat.max(o.start) });
        }
    }

    /// Ends the take at `beat`: every note still held stops there. Returns the notes in
    /// start order.
    pub fn finish(mut self, beat: f64) -> Vec<RecNote> {
        for o in std::mem::take(&mut self.open) {
            self.done.push(RecNote { key: o.key, vel: o.vel, start: o.start, end: beat.max(o.start) });
        }
        self.done.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
        self.done
    }
}

/// A recorded take laid out as a pattern clip.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedClip {
    /// Clip start on the timeline, beats (snapped down to a bar).
    pub start: f64,
    /// Pattern and clip length in beats (whole bars).
    pub len: f64,
    pub notes: Vec<Note>,
}

/// Places `notes` (absolute beats) into a pattern for channel `ch`: the clip starts on the
/// bar that holds the first note and covers whole bars to the last note's end. Note
/// positions are in steps from the clip start, velocity is linear gain.
pub fn layout(notes: &[RecNote], ch: Id, beats_per_bar: f64) -> Option<RecordedClip> {
    let first = notes.iter().map(|n| n.start).fold(f64::INFINITY, f64::min);
    let last = notes.iter().map(|n| n.end).fold(f64::NEG_INFINITY, f64::max);
    if !first.is_finite() || !last.is_finite() {
        return None;
    }
    let start = (first / beats_per_bar).floor() * beats_per_bar;
    let len = ((last - start) / beats_per_bar).ceil().max(1.0) * beats_per_bar;
    let notes = notes
        .iter()
        .map(|n| Note {
            ch,
            inst: None,
            step: ((n.start - start) * STEPS_PER_BEAT) as f32,
            len: (((n.end - n.start) * STEPS_PER_BEAT) as f32).max(MIN_NOTE_STEPS),
            p: Some(n.key),
            v: MidiMsg::vel_gain(n.vel),
        })
        .collect();
    Some(RecordedClip { start, len, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_note_on_off() {
        assert_eq!(MidiMsg::parse(&[0x90, 60, 100]), Some(MidiMsg::NoteOn { ch: 0, key: 60, vel: 100 }));
        assert_eq!(MidiMsg::parse(&[0x93, 61, 1]), Some(MidiMsg::NoteOn { ch: 3, key: 61, vel: 1 }));
        assert_eq!(MidiMsg::parse(&[0x80, 60, 64]), Some(MidiMsg::NoteOff { ch: 0, key: 60 }));
        assert_eq!(MidiMsg::parse(&[0x8F, 60, 0]), Some(MidiMsg::NoteOff { ch: 15, key: 60 }));
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        assert_eq!(MidiMsg::parse(&[0x90, 60, 0]), Some(MidiMsg::NoteOff { ch: 0, key: 60 }));
    }

    #[test]
    fn parses_sustain_with_threshold() {
        assert_eq!(MidiMsg::parse(&[0xB0, 64, 127]), Some(MidiMsg::Sustain { ch: 0, down: true }));
        assert_eq!(MidiMsg::parse(&[0xB0, 64, 64]), Some(MidiMsg::Sustain { ch: 0, down: true }));
        assert_eq!(MidiMsg::parse(&[0xB2, 64, 63]), Some(MidiMsg::Sustain { ch: 2, down: false }));
        assert_eq!(MidiMsg::parse(&[0xB0, 64, 0]), Some(MidiMsg::Sustain { ch: 0, down: false }));
    }

    #[test]
    fn parses_pitch_bend() {
        assert_eq!(MidiMsg::parse(&[0xE0, 0x00, 0x40]), Some(MidiMsg::PitchBend { ch: 0, value: 0 }));
        assert_eq!(MidiMsg::parse(&[0xE0, 0x00, 0x00]), Some(MidiMsg::PitchBend { ch: 0, value: -8192 }));
        assert_eq!(MidiMsg::parse(&[0xE1, 0x7F, 0x7F]), Some(MidiMsg::PitchBend { ch: 1, value: 8191 }));
        assert!((MidiMsg::bend_semitones(-8192) + 2.0).abs() < 1e-6);
        assert_eq!(MidiMsg::bend_semitones(0), 0.0);
    }

    #[test]
    fn parses_all_notes_off_and_ignores_the_rest() {
        assert_eq!(MidiMsg::parse(&[0xB0, 123, 0]), Some(MidiMsg::AllNotesOff { ch: 0 }));
        assert_eq!(MidiMsg::parse(&[0xB0, 7, 100]), None); // volume CC
        assert_eq!(MidiMsg::parse(&[0xC0, 5]), None); // program change
        assert_eq!(MidiMsg::parse(&[0xF8]), None); // clock
        assert_eq!(MidiMsg::parse(&[0xFE]), None);
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(MidiMsg::parse(&[]), None);
        assert_eq!(MidiMsg::parse(&[0x90]), None);
        assert_eq!(MidiMsg::parse(&[0x90, 60]), None);
        assert_eq!(MidiMsg::parse(&[0x90, 200, 10]), None);
        assert_eq!(MidiMsg::parse(&[0x90, 60, 200]), None);
        assert_eq!(MidiMsg::parse(&[0x40, 60, 100]), None); // data byte first
    }

    #[test]
    fn velocity_maps_to_unit_gain() {
        assert_eq!(MidiMsg::vel_gain(127), 1.0);
        assert!((MidiMsg::vel_gain(64) - 0.5039).abs() < 1e-3);
    }

    #[test]
    fn records_a_simple_note() {
        let mut r = MidiRecorder::new(0.0);
        r.note_on(1.0, 60, 100);
        r.note_off(1.5, 60);
        assert_eq!(r.finish(4.0), vec![RecNote { key: 60, vel: 100, start: 1.0, end: 1.5 }]);
    }

    #[test]
    fn held_notes_end_at_stop() {
        let mut r = MidiRecorder::new(0.0);
        r.note_on(2.0, 64, 90);
        let n = r.finish(3.25);
        assert_eq!(n, vec![RecNote { key: 64, vel: 90, start: 2.0, end: 3.25 }]);
    }

    #[test]
    fn count_in_events_are_dropped() {
        let mut r = MidiRecorder::new(0.0);
        r.note_on(-2.0, 60, 100);
        r.note_off(-1.0, 60);
        assert!(r.is_empty());
        assert!(r.finish(1.0).is_empty());
    }

    #[test]
    fn sustain_extends_notes_until_pedal_up() {
        let mut r = MidiRecorder::new(0.0);
        r.sustain(0.0, true);
        r.note_on(0.5, 60, 100);
        r.note_off(1.0, 60);
        r.note_on(1.5, 62, 100);
        r.note_off(1.75, 62);
        r.sustain(3.0, false);
        let n = r.finish(8.0);
        assert_eq!(n[0], RecNote { key: 60, vel: 100, start: 0.5, end: 3.0 });
        assert_eq!(n[1], RecNote { key: 62, vel: 100, start: 1.5, end: 3.0 });
    }

    #[test]
    fn restriking_a_key_ends_the_previous_note() {
        let mut r = MidiRecorder::new(0.0);
        r.note_on(0.0, 60, 100);
        r.note_on(1.0, 60, 80);
        r.note_off(1.5, 60);
        let n = r.finish(4.0);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].end, 1.0);
        assert_eq!(n[1], RecNote { key: 60, vel: 80, start: 1.0, end: 1.5 });
    }

    #[test]
    fn loop_wrap_does_not_produce_negative_length() {
        let mut r = MidiRecorder::new(0.0);
        r.note_on(7.9, 60, 100);
        r.note_off(0.1, 60); // loop wrapped back to the start
        let n = r.finish(8.0);
        assert!(n[0].end >= n[0].start);
    }

    #[test]
    fn layout_snaps_to_bars_and_converts_to_steps() {
        let notes = [RecNote { key: 60, vel: 127, start: 4.5, end: 5.0 }, RecNote { key: 64, vel: 64, start: 5.0, end: 6.25 }];
        let c = layout(&notes, 7, 4.0).unwrap();
        assert_eq!(c.start, 4.0);
        assert_eq!(c.len, 4.0); // up to 6.25 → 2.25 beats in → one bar
        assert_eq!(c.notes.len(), 2);
        assert_eq!(c.notes[0], Note { ch: 7, inst: None, step: 2.0, len: 2.0, p: Some(60), v: 1.0 });
        assert_eq!(c.notes[1].step, 4.0);
        assert_eq!(c.notes[1].len, 5.0);
        assert!((c.notes[1].v - 64.0 / 127.0).abs() < 1e-6);
    }

    #[test]
    fn layout_spans_extra_bars_and_enforces_min_length() {
        let notes = [RecNote { key: 60, vel: 100, start: 3.9, end: 3.9 }, RecNote { key: 62, vel: 100, start: 8.0, end: 9.5 }];
        let c = layout(&notes, 1, 4.0).unwrap();
        assert_eq!(c.start, 0.0);
        assert_eq!(c.len, 12.0);
        assert_eq!(c.notes[0].len, MIN_NOTE_STEPS);
        assert_eq!(c.notes[1].step, 32.0);
    }

    #[test]
    fn layout_of_nothing_is_none() {
        assert!(layout(&[], 1, 4.0).is_none());
    }
}
