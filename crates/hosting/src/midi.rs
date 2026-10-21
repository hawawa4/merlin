//! Raw channel MIDI as it travels through the engine: three bytes and a timestamp.

/// A channel-voice MIDI message at a frame offset inside the current block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiEvent {
    /// Frames from the start of the block.
    pub time: u32,
    /// Which MIDI input it came from (index into the engine's input table).
    pub input: u8,
    pub data: [u8; 3],
}

/// A decoded message. A note-on with velocity 0 is reported as a note-off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiKind {
    NoteOff {
        ch: u8,
        key: u8,
        vel: u8,
    },
    NoteOn {
        ch: u8,
        key: u8,
        vel: u8,
    },
    PolyPressure {
        ch: u8,
        key: u8,
        value: u8,
    },
    Cc {
        ch: u8,
        cc: u8,
        value: u8,
    },
    ProgramChange {
        ch: u8,
        program: u8,
    },
    ChannelPressure {
        ch: u8,
        value: u8,
    },
    /// 14-bit, 8192 is centre.
    PitchBend {
        ch: u8,
        value: u16,
    },
    /// System messages and anything malformed.
    Other,
}

impl MidiEvent {
    pub fn new(time: u32, input: u8, data: [u8; 3]) -> Self {
        Self { time, input, data }
    }

    pub fn note_on(time: u32, ch: u8, key: u8, vel: u8) -> Self {
        Self::new(time, 0, [0x90 | (ch & 15), key & 127, vel & 127])
    }

    pub fn note_off(time: u32, ch: u8, key: u8) -> Self {
        Self::new(time, 0, [0x80 | (ch & 15), key & 127, 0])
    }

    pub fn cc(time: u32, ch: u8, cc: u8, value: u8) -> Self {
        Self::new(time, 0, [0xB0 | (ch & 15), cc & 127, value & 127])
    }

    pub fn pitch_bend(time: u32, ch: u8, value: u16) -> Self {
        let v = value.min(16383);
        Self::new(time, 0, [0xE0 | (ch & 15), (v & 127) as u8, (v >> 7) as u8])
    }

    pub fn poly_pressure(time: u32, ch: u8, key: u8, value: u8) -> Self {
        Self::new(time, 0, [0xA0 | (ch & 15), key & 127, value & 127])
    }

    pub fn kind(&self) -> MidiKind {
        let [s, a, b] = self.data;
        let ch = s & 15;
        let (a, b) = (a & 127, b & 127);
        match s & 0xF0 {
            0x80 => MidiKind::NoteOff { ch, key: a, vel: b },
            0x90 if b == 0 => MidiKind::NoteOff { ch, key: a, vel: 0 },
            0x90 => MidiKind::NoteOn { ch, key: a, vel: b },
            0xA0 => MidiKind::PolyPressure {
                ch,
                key: a,
                value: b,
            },
            0xB0 => MidiKind::Cc {
                ch,
                cc: a,
                value: b,
            },
            0xC0 => MidiKind::ProgramChange { ch, program: a },
            0xD0 => MidiKind::ChannelPressure { ch, value: a },
            0xE0 => MidiKind::PitchBend {
                ch,
                value: (a as u16) | ((b as u16) << 7),
            },
            _ => MidiKind::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_basics() {
        assert_eq!(
            MidiEvent::note_on(0, 3, 60, 100).kind(),
            MidiKind::NoteOn {
                ch: 3,
                key: 60,
                vel: 100
            }
        );
        // Running-status style note-on with velocity 0 is a note-off.
        assert_eq!(
            MidiEvent::new(0, 0, [0x90, 60, 0]).kind(),
            MidiKind::NoteOff {
                ch: 0,
                key: 60,
                vel: 0
            }
        );
        assert_eq!(
            MidiEvent::pitch_bend(0, 1, 8192).kind(),
            MidiKind::PitchBend { ch: 1, value: 8192 }
        );
        assert_eq!(MidiEvent::new(0, 0, [0xF8, 0, 0]).kind(), MidiKind::Other);
    }
}
