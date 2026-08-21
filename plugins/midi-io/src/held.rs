//! Held-note bookkeeping so a dropped cable cannot leave a voice hanging.
//!
//! The parser emits channel-voice events. This module turns them into the
//! note-on / note-off actions the engine understands, including the panic
//! path (CC 120 / 123, or an explicit `panic`).

use std::collections::HashSet;

use crate::parser::MidiEvent;

/// A note action aimed at the selected engine track. Channel is applied by
/// the sink, not here — live MIDI is omni in v1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoteAction {
    On { key: u8, velocity: f32 },
    Off { key: u8 },
}

/// Keys currently held on one input port.
#[derive(Debug, Default)]
pub struct HeldNotes {
    keys: HashSet<u8>,
}

impl HeldNotes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = u8> + '_ {
        self.keys.iter().copied()
    }

    /// Translate `events` into engine actions and update the held set.
    pub fn apply(&mut self, events: &[MidiEvent]) -> Vec<NoteAction> {
        let mut actions = Vec::new();
        for event in events {
            match *event {
                MidiEvent::NoteOn { key, velocity, .. } => {
                    self.keys.insert(key);
                    actions.push(NoteAction::On { key, velocity });
                }
                MidiEvent::NoteOff { key, .. } => {
                    self.keys.remove(&key);
                    actions.push(NoteAction::Off { key });
                }
                event if event.is_panic_cc() => {
                    actions.extend(self.panic());
                }
                _ => {}
            }
        }
        actions
    }

    /// Release every held key. Used on unplug, disable, and shutdown.
    pub fn panic(&mut self) -> Vec<NoteAction> {
        let keys: Vec<u8> = self.keys.drain().collect();
        keys.into_iter()
            .map(|key| NoteAction::Off { key })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_on_then_off_tracks_the_held_set() {
        let mut held = HeldNotes::new();
        let on = held.apply(&[MidiEvent::NoteOn {
            channel: 0,
            key: 60,
            velocity: 0.8,
        }]);
        assert_eq!(
            on,
            vec![NoteAction::On {
                key: 60,
                velocity: 0.8
            }]
        );
        assert!(!held.is_empty());
        let off = held.apply(&[MidiEvent::NoteOff {
            channel: 0,
            key: 60,
        }]);
        assert_eq!(off, vec![NoteAction::Off { key: 60 }]);
        assert!(held.is_empty());
    }

    #[test]
    fn all_notes_off_releases_every_held_key() {
        let mut held = HeldNotes::new();
        held.apply(&[
            MidiEvent::NoteOn {
                channel: 0,
                key: 60,
                velocity: 1.0,
            },
            MidiEvent::NoteOn {
                channel: 0,
                key: 64,
                velocity: 1.0,
            },
        ]);
        let actions = held.apply(&[MidiEvent::ControlChange {
            channel: 0,
            number: MidiEvent::ALL_NOTES_OFF,
            value: 0,
        }]);
        assert_eq!(actions.len(), 2);
        assert!(held.is_empty());
        assert!(actions.iter().all(|a| matches!(a, NoteAction::Off { .. })));
    }

    #[test]
    fn panic_on_an_empty_set_is_a_no_op() {
        let mut held = HeldNotes::new();
        assert!(held.panic().is_empty());
    }

    #[test]
    fn a_control_change_that_is_not_panic_emits_nothing() {
        let mut held = HeldNotes::new();
        let actions = held.apply(&[MidiEvent::ControlChange {
            channel: 0,
            number: 7,
            value: 100,
        }]);
        assert!(actions.is_empty());
    }
}
