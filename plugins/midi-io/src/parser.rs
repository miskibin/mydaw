//! MIDI 1.0 channel-voice parser.
//!
//! `midir` (and WinMM behind it) hands us a slice of raw bytes per callback.
//! Running status, SysEx, and real-time messages are the reasons that slice is
//! not already a structured event. This module is the whole decoder, so the
//! rest of the plugin never looks at a status nibble.
//!
//! Real-time bytes (`0xF8..=0xFF`) may appear in the middle of another message
//! and are skipped. SysEx is consumed until `0xF7` and discarded. Channel-mode
//! CCs (all sound off / all notes off) stay as [`MidiEvent::ControlChange`] so
//! the router, not the parser, decides what "panic" means.

/// One decoded MIDI 1.0 event. Velocity on [`NoteOn`](Self::NoteOn) is already
/// normalised to `0.0..=1.0`; a MIDI note-on with velocity 0 has been rewritten
/// as [`NoteOff`](Self::NoteOff).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidiEvent {
    NoteOn {
        channel: u8,
        key: u8,
        /// `1/127..=1.0`.
        velocity: f32,
    },
    NoteOff {
        channel: u8,
        key: u8,
    },
    ControlChange {
        channel: u8,
        number: u8,
        value: u8,
    },
    PitchBend {
        channel: u8,
        /// 14-bit value, 0..=16383, centre 8192.
        value: u16,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    ChannelPressure {
        channel: u8,
        value: u8,
    },
    PolyAftertouch {
        channel: u8,
        key: u8,
        value: u8,
    },
}

impl MidiEvent {
    /// Channel-mode CC 123.
    pub const ALL_NOTES_OFF: u8 = 123;
    /// Channel-mode CC 120.
    pub const ALL_SOUND_OFF: u8 = 120;

    /// Channel-mode panic CCs the router turns into note-offs.
    pub const fn is_panic_cc(self) -> bool {
        matches!(
            self,
            Self::ControlChange {
                number: Self::ALL_NOTES_OFF | Self::ALL_SOUND_OFF,
                ..
            }
        )
    }
}

/// Incremental MIDI 1.0 decoder. Feed any number of bytes; leftover data for a
/// truncated message stays here until the next call.
#[derive(Debug, Default)]
pub struct MidiParser {
    status: Option<u8>,
    data: [u8; 2],
    got: usize,
    needed: usize,
    in_sysex: bool,
}

impl MidiParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode `bytes`, returning every complete channel-voice event they
    /// contained (plus anything completed from a previous truncated feed).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<MidiEvent> {
        let mut out = Vec::new();
        for &byte in bytes {
            self.push_byte(byte, &mut out);
        }
        out
    }

    fn push_byte(&mut self, byte: u8, out: &mut Vec<MidiEvent>) {
        if byte >= 0xF8 {
            // Real-time: ignore, do not disturb running status or an in-flight
            // message. MIDI clock would otherwise drown the activity lamp.
            return;
        }

        if self.in_sysex {
            if byte == 0xF7 {
                self.in_sysex = false;
            } else if byte >= 0x80 {
                // A new status aborted SysEx without an F7. Fall through and
                // handle it as a status byte.
                self.in_sysex = false;
            } else {
                return;
            }
            if byte == 0xF7 {
                return;
            }
        }

        if byte >= 0x80 {
            if byte == 0xF0 {
                self.begin_sysex();
                return;
            }
            if byte == 0xF7 {
                return;
            }
            if (0xF1..=0xF6).contains(&byte) {
                self.begin_system_common(byte);
                return;
            }
            self.begin_channel_status(byte);
            return;
        }

        // Data byte.
        if self.in_sysex {
            return;
        }
        if self.status.is_none() || self.needed == 0 {
            return;
        }
        if self.got < self.data.len() {
            self.data[self.got] = byte;
            self.got += 1;
        }
        if self.got >= self.needed {
            if let Some(event) = self.take_message() {
                out.push(event);
            }
        }
    }

    fn begin_sysex(&mut self) {
        self.in_sysex = true;
        self.status = None;
        self.got = 0;
        self.needed = 0;
    }

    fn begin_system_common(&mut self, status: u8) {
        self.status = None;
        self.got = 0;
        self.needed = match status {
            0xF1 | 0xF3 => 1,
            0xF2 => 2,
            _ => 0,
        };
        // v1: system common is parsed only so it does not leak into running
        // status. The bytes are discarded.
        if self.needed == 0 {
            return;
        }
        // Park a dummy status so data bytes are consumed, then drop the
        // completed message in `take_message`.
        self.status = Some(status);
    }

    fn begin_channel_status(&mut self, status: u8) {
        let needed = data_bytes_for(status);
        self.status = Some(status);
        self.got = 0;
        self.needed = needed;
        if needed == 0 {
            // Tune request etc. already filtered; nothing to emit.
            self.status = None;
        }
    }

    fn take_message(&mut self) -> Option<MidiEvent> {
        let status = self.status?;
        let event = decode(status, &self.data[..self.needed]);
        self.got = 0;
        if status >= 0xF0 {
            // System common does not set running status.
            self.status = None;
            self.needed = 0;
            return None;
        }
        event
    }
}

fn data_bytes_for(status: u8) -> usize {
    match status & 0xF0 {
        0xC0 | 0xD0 => 1,
        0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => 2,
        _ => match status {
            0xF1 | 0xF3 => 1,
            0xF2 => 2,
            _ => 0,
        },
    }
}

fn decode(status: u8, data: &[u8]) -> Option<MidiEvent> {
    let kind = status & 0xF0;
    let channel = status & 0x0F;
    match kind {
        0x80 => Some(MidiEvent::NoteOff {
            channel,
            key: data[0],
        }),
        0x90 => {
            let key = data[0];
            let velocity = data[1];
            if velocity == 0 {
                Some(MidiEvent::NoteOff { channel, key })
            } else {
                Some(MidiEvent::NoteOn {
                    channel,
                    key,
                    velocity: f32::from(velocity) / 127.0,
                })
            }
        }
        0xA0 => Some(MidiEvent::PolyAftertouch {
            channel,
            key: data[0],
            value: data[1],
        }),
        0xB0 => Some(MidiEvent::ControlChange {
            channel,
            number: data[0],
            value: data[1],
        }),
        0xC0 => Some(MidiEvent::ProgramChange {
            channel,
            program: data[0],
        }),
        0xD0 => Some(MidiEvent::ChannelPressure {
            channel,
            value: data[0],
        }),
        0xE0 => {
            let lsb = u16::from(data[0]);
            let msb = u16::from(data[1]);
            Some(MidiEvent::PitchBend {
                channel,
                value: lsb | (msb << 7),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_on_decodes_channel_key_and_normalised_velocity() {
        let events = MidiParser::new().feed(&[0x93, 60, 127]);
        assert_eq!(
            events,
            vec![MidiEvent::NoteOn {
                channel: 3,
                key: 60,
                velocity: 1.0
            }]
        );
    }

    #[test]
    fn a_note_on_with_velocity_zero_is_a_note_off() {
        let events = MidiParser::new().feed(&[0x90, 64, 0]);
        assert_eq!(
            events,
            vec![MidiEvent::NoteOff {
                channel: 0,
                key: 64
            }]
        );
    }

    #[test]
    fn running_status_reuses_the_previous_channel_voice_status() {
        let events = MidiParser::new().feed(&[0x90, 60, 100, 64, 80, 67, 90]);
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[0],
            MidiEvent::NoteOn {
                key: 60,
                channel: 0,
                ..
            }
        ));
        assert!(matches!(
            events[1],
            MidiEvent::NoteOn {
                key: 64,
                channel: 0,
                ..
            }
        ));
        assert!(matches!(
            events[2],
            MidiEvent::NoteOn {
                key: 67,
                channel: 0,
                ..
            }
        ));
    }

    #[test]
    fn a_truncated_message_completes_on_the_next_feed() {
        let mut parser = MidiParser::new();
        assert!(parser.feed(&[0x90, 60]).is_empty());
        assert_eq!(
            parser.feed(&[100]),
            vec![MidiEvent::NoteOn {
                channel: 0,
                key: 60,
                velocity: 100.0 / 127.0
            }]
        );
    }

    #[test]
    fn sysex_is_consumed_and_does_not_emit() {
        let events = MidiParser::new().feed(&[0xF0, 0x00, 0x20, 0x32, 0xF7, 0x80, 12, 64]);
        assert_eq!(
            events,
            vec![MidiEvent::NoteOff {
                channel: 0,
                key: 12
            }]
        );
    }

    #[test]
    fn realtime_bytes_do_not_break_a_channel_message() {
        let events = MidiParser::new().feed(&[0x90, 0xF8, 60, 0xFE, 100]);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MidiEvent::NoteOn { key: 60, .. }));
    }

    #[test]
    fn all_notes_off_is_a_control_change() {
        let events = MidiParser::new().feed(&[0xB0, 123, 0]);
        assert_eq!(
            events,
            vec![MidiEvent::ControlChange {
                channel: 0,
                number: 123,
                value: 0
            }]
        );
        assert!(events[0].is_panic_cc());
    }

    #[test]
    fn all_sound_off_is_a_panic_cc() {
        let events = MidiParser::new().feed(&[0xB2, 120, 0]);
        assert!(events[0].is_panic_cc());
    }

    #[test]
    fn control_change_pitch_bend_and_program_change_decode() {
        let mut parser = MidiParser::new();
        assert_eq!(
            parser.feed(&[0xB1, 7, 64]),
            vec![MidiEvent::ControlChange {
                channel: 1,
                number: 7,
                value: 64
            }]
        );
        assert_eq!(
            parser.feed(&[0xE0, 0x00, 0x40]),
            vec![MidiEvent::PitchBend {
                channel: 0,
                value: 8192
            }]
        );
        assert_eq!(
            parser.feed(&[0xC4, 12]),
            vec![MidiEvent::ProgramChange {
                channel: 4,
                program: 12
            }]
        );
    }

    #[test]
    fn a_stray_data_byte_without_status_is_ignored() {
        assert!(MidiParser::new().feed(&[60, 100]).is_empty());
    }
}
