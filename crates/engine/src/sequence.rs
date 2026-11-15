//! The MIDI file player: one loaded file, played from its start into the router as if it were
//! the on-screen keyboard (engine input 0), so it plays the instruments that are selected, the
//! way the keyboard would. Not a sequencer: no tracks, no tempo of its own, no loop.
//!
//! The file is turned into frames on the main thread ([`Sequence`]); the audio thread only walks
//! the list. Whatever the file still holds when it stops (notes, the sustain pedal) is let go.

use merlin_hosting::MidiEvent;

/// The engine input the file plays as: the on-screen keyboard's.
pub const SEQUENCE_INPUT: u8 = 0;
/// Messages the file can hand the router in one block; any more wait for the next block.
pub(crate) const MAX_PER_BLOCK: usize = 1024;

/// One message of the file, at a frame from the file's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqEvent {
    pub frame: u64,
    pub data: [u8; 3],
}

/// A MIDI file ready for the audio thread.
#[derive(Clone, Debug, Default)]
pub struct Sequence {
    events: Vec<SeqEvent>,
    end: u64,
}

impl Sequence {
    /// `events` in any order (equal frames keep their order); `end` is where the file ends, at
    /// the last message at the earliest.
    pub fn new(mut events: Vec<SeqEvent>, end: u64) -> Self {
        events.sort_by_key(|e| e.frame);
        let end = end.max(events.last().map_or(0, |e| e.frame + 1));
        Self { events, end }
    }

    /// Its length in frames.
    pub fn end(&self) -> u64 {
        self.end
    }

    pub fn events(&self) -> &[SeqEvent] {
        &self.events
    }
}

/// The audio thread's player.
pub(crate) struct Player {
    pub(crate) sequence: Option<Box<Sequence>>,
    playing: bool,
    /// Frames played since the start.
    position: u64,
    next: usize,
    /// Notes the file holds down, per channel, and the channels whose sustain pedal it holds.
    notes: [u128; 16],
    pedal: u16,
}

impl Player {
    pub(crate) fn new() -> Self {
        Self {
            sequence: None,
            playing: false,
            position: 0,
            next: 0,
            notes: [0; 16],
            pedal: 0,
        }
    }

    pub(crate) fn playing(&self) -> bool {
        self.playing
    }

    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    /// Starts from the beginning (a file that plays starts over).
    pub(crate) fn play(&mut self, out: &mut Vec<MidiEvent>) {
        self.stop(out);
        if self.sequence.is_some() {
            self.playing = true;
            self.position = 0;
            self.next = 0;
        }
    }

    /// Stops and lets go of what the file holds.
    pub(crate) fn stop(&mut self, out: &mut Vec<MidiEvent>) {
        self.playing = false;
        self.release(0, out);
    }

    /// Takes the file out (to hand it back); stops first.
    pub(crate) fn replace(
        &mut self,
        sequence: Option<Box<Sequence>>,
        out: &mut Vec<MidiEvent>,
    ) -> Option<Box<Sequence>> {
        self.stop(out);
        self.position = 0;
        self.next = 0;
        std::mem::replace(&mut self.sequence, sequence)
    }

    /// Adds the file's messages due in the next `frames` frames to `out` (times within the
    /// block, in order), and stops at the file's end.
    pub(crate) fn render(&mut self, frames: usize, out: &mut Vec<MidiEvent>) {
        if !self.playing {
            return;
        }
        let Some(seq) = self.sequence.as_deref() else {
            self.playing = false;
            return;
        };
        let start = self.position;
        let end = start + frames as u64;
        while let Some(e) = seq.events.get(self.next) {
            if e.frame >= end || out.len() >= MAX_PER_BLOCK {
                break;
            }
            // A message held back by a full block plays at this block's start.
            let time = e.frame.saturating_sub(self.position) as u32;
            let ch = (e.data[0] & 15) as usize;
            let key = e.data[1] & 127;
            match e.data[0] & 0xF0 {
                0x90 if e.data[2] > 0 => self.notes[ch] |= 1 << key,
                0x80 | 0x90 => self.notes[ch] &= !(1 << key),
                0xB0 if key == 64 => {
                    if e.data[2] >= 64 {
                        self.pedal |= 1 << ch;
                    } else {
                        self.pedal &= !(1 << ch);
                    }
                }
                0xB0 if key == 120 || key == 123 => self.notes[ch] = 0,
                _ => {}
            }
            out.push(MidiEvent::new(time, SEQUENCE_INPUT, e.data));
            self.next += 1;
        }
        self.position = end.min(seq.end).max(start);
        if self.next >= seq.events.len() && end >= seq.end {
            self.position = seq.end.max(start);
            self.playing = false;
            let at = seq
                .end
                .saturating_sub(start)
                .min(frames.saturating_sub(1) as u64);
            self.release(at as u32, out);
        }
    }

    fn release(&mut self, time: u32, out: &mut Vec<MidiEvent>) {
        for ch in 0..16u8 {
            let held = std::mem::take(&mut self.notes[ch as usize]);
            for key in 0..128u8 {
                if held & (1 << key) != 0 && out.len() < out.capacity() {
                    out.push(MidiEvent::new(time, SEQUENCE_INPUT, [0x80 | ch, key, 0]));
                }
            }
            if self.pedal & (1 << ch) != 0 && out.len() < out.capacity() {
                out.push(MidiEvent::new(time, SEQUENCE_INPUT, [0xB0 | ch, 64, 0]));
            }
        }
        self.pedal = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq() -> Sequence {
        Sequence::new(
            vec![
                SeqEvent {
                    frame: 10,
                    data: [0x90, 60, 100],
                },
                SeqEvent {
                    frame: 300,
                    data: [0xB0, 64, 127],
                },
                SeqEvent {
                    frame: 200,
                    data: [0x91, 64, 100],
                },
                SeqEvent {
                    frame: 250,
                    data: [0x80, 60, 0],
                },
            ],
            1000,
        )
    }

    fn out() -> Vec<MidiEvent> {
        Vec::with_capacity(MAX_PER_BLOCK + 2 * 16 * 128)
    }

    #[test]
    fn events_come_in_their_block_at_their_frame_and_the_end_lets_go() {
        let mut p = Player::new();
        let mut o = out();
        p.replace(Some(Box::new(seq())), &mut o);
        p.play(&mut o);
        let mut got = Vec::new();
        let mut blocks = 0;
        while p.playing() {
            o.clear();
            p.render(128, &mut o);
            for e in &o {
                got.push((blocks * 128 + e.time as u64, e.data));
            }
            blocks += 1;
        }
        assert_eq!(
            got,
            vec![
                (10, [0x90, 60, 100]),
                (200, [0x91, 64, 100]),
                (250, [0x80, 60, 0]),
                (300, [0xB0, 64, 127]),
                // What is still held is let go where the file ends.
                (1000, [0xB0, 64, 0]),
                (1000, [0x81, 64, 0]),
            ]
        );
        assert_eq!(p.position(), 1000);
    }

    #[test]
    fn stopping_lets_go_of_what_is_held_and_play_starts_over() {
        let mut p = Player::new();
        let mut o = out();
        p.replace(Some(Box::new(seq())), &mut o);
        p.play(&mut o);
        o.clear();
        p.render(256, &mut o);
        // Note 60 on and off, note 64 on.
        assert_eq!(o.len(), 3);
        o.clear();
        p.stop(&mut o);
        let offs: Vec<[u8; 3]> = o.iter().map(|e| e.data).collect();
        assert_eq!(offs, vec![[0x81, 64, 0]]);
        o.clear();
        p.render(256, &mut o);
        assert!(o.is_empty(), "a stopped file plays nothing");
        p.play(&mut o);
        o.clear();
        p.render(128, &mut o);
        assert_eq!(o[0].data, [0x90, 60, 100]);
    }

    #[test]
    fn a_crowded_block_holds_messages_over_instead_of_losing_them() {
        let events = (0..MAX_PER_BLOCK as u64 + 10)
            .map(|i| SeqEvent {
                frame: 0,
                data: [0xB0, 1, (i % 128) as u8],
            })
            .collect();
        let mut p = Player::new();
        let mut o = out();
        p.replace(Some(Box::new(Sequence::new(events, 10))), &mut o);
        p.play(&mut o);
        o.clear();
        p.render(128, &mut o);
        assert_eq!(o.len(), MAX_PER_BLOCK);
        o.clear();
        p.render(128, &mut o);
        assert_eq!(o.len(), 10);
        assert!(o.iter().all(|e| e.time == 0));
        assert!(!p.playing());
    }
}
