//! Instruments that ship inside the app. For now a small sine synth: it is the test
//! instrument for the engine and a fallback sound while no plugin is loaded.

use crate::{Io, MidiEvent, MidiKind, PluginProcessor, ProcessStatus};

const VOICES: usize = 16;
/// Release time; short so tests can see a note end quickly.
const RELEASE_SECS: f32 = 0.010;

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    releasing: bool,
    key: u8,
    ch: u8,
    phase: f32,
    inc: f32,
    amp: f32,
    env: f32,
}

/// 16-voice polyphonic sine synth. Allocation-free; sleeps when silent.
pub struct SineSynth {
    voices: [Voice; VOICES],
    sample_rate: f32,
    release_step: f32,
    sustain: bool,
    /// Notes whose key came up while the pedal was down.
    held_by_pedal: [bool; VOICES],
    bend: f32,
}

impl SineSynth {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            voices: [Voice::default(); VOICES],
            sample_rate,
            release_step: 1.0 / (RELEASE_SECS * sample_rate),
            sustain: false,
            held_by_pedal: [false; VOICES],
            bend: 0.0,
        }
    }

    fn note_on(&mut self, ch: u8, key: u8, vel: u8) {
        // Take a free voice, else steal the quietest releasing one, else the first.
        let i = self
            .voices
            .iter()
            .position(|v| !v.active)
            .or_else(|| self.voices.iter().position(|v| v.releasing))
            .unwrap_or(0);
        self.held_by_pedal[i] = false;
        self.voices[i] = Voice {
            active: true,
            releasing: false,
            key,
            ch,
            phase: 0.0,
            inc: self.increment(key),
            amp: vel as f32 / 127.0 * 0.25,
            env: 0.0,
        };
    }

    fn increment(&self, key: u8) -> f32 {
        let hz = 440.0 * 2f32.powf((key as f32 - 69.0 + self.bend) / 12.0);
        hz / self.sample_rate
    }

    fn note_off(&mut self, ch: u8, key: u8) {
        for i in 0..VOICES {
            let v = &mut self.voices[i];
            if v.active && !v.releasing && v.key == key && v.ch == ch {
                if self.sustain {
                    self.held_by_pedal[i] = true;
                } else {
                    v.releasing = true;
                }
            }
        }
    }

    fn release_all(&mut self) {
        for v in &mut self.voices {
            if v.active {
                v.releasing = true;
            }
        }
        self.held_by_pedal = [false; VOICES];
    }

    fn handle(&mut self, ev: &MidiEvent) {
        match ev.kind() {
            MidiKind::NoteOn { ch, key, vel } => self.note_on(ch, key, vel),
            MidiKind::NoteOff { ch, key, .. } => self.note_off(ch, key),
            MidiKind::Cc { cc: 64, value, .. } => {
                self.sustain = value >= 64;
                if !self.sustain {
                    for i in 0..VOICES {
                        if std::mem::take(&mut self.held_by_pedal[i]) {
                            self.voices[i].releasing = true;
                        }
                    }
                }
            }
            MidiKind::Cc { cc: 120 | 123, .. } => self.release_all(),
            MidiKind::PitchBend { value, .. } => {
                self.bend = (value as f32 - 8192.0) / 8192.0 * 2.0;
                for i in 0..VOICES {
                    if self.voices[i].active {
                        self.voices[i].inc = self.increment(self.voices[i].key);
                    }
                }
            }
            _ => {}
        }
    }

    /// Is any voice sounding?
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }
}

impl PluginProcessor for SineSynth {
    fn process(&mut self, io: &mut Io, events: &[MidiEvent]) -> ProcessStatus {
        let frames = io.frames();
        let mut next = 0;
        let attack_step = 1.0 / (0.002 * self.sample_rate);
        for t in 0..frames {
            while next < events.len() && events[next].time as usize <= t {
                self.handle(&events[next]);
                next += 1;
            }
            let mut sample = 0.0;
            for v in &mut self.voices {
                if !v.active {
                    continue;
                }
                if v.releasing {
                    v.env -= self.release_step;
                    if v.env <= 0.0 {
                        *v = Voice::default();
                        continue;
                    }
                } else if v.env < 1.0 {
                    v.env = (v.env + attack_step).min(1.0);
                }
                sample += (v.phase * std::f32::consts::TAU).sin() * v.amp * v.env;
                v.phase += v.inc;
                if v.phase >= 1.0 {
                    v.phase -= 1.0;
                }
            }
            io.out_l[t] = sample;
            io.out_r[t] = sample;
        }
        // Events stamped at or after the block end do not exist; anything left is a bug in
        // the caller, but handle it rather than lose a note-off.
        for ev in &events[next..] {
            self.handle(ev);
        }
        if self.active_voices() == 0 {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        synth: &mut SineSynth,
        frames: usize,
        events: &[MidiEvent],
    ) -> (Vec<f32>, ProcessStatus) {
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        let status = synth.process(
            &mut Io {
                input: None,
                out_l: &mut l,
                out_r: &mut r,
            },
            events,
        );
        (l, status)
    }

    #[test]
    fn plays_then_sleeps() {
        let mut s = SineSynth::new(48000.0);
        let (out, status) = run(&mut s, 480, &[MidiEvent::note_on(0, 0, 69, 127)]);
        assert_eq!(status, ProcessStatus::Continue);
        assert!(out.iter().any(|x| x.abs() > 0.1));
        let (_, status) = run(&mut s, 1024, &[MidiEvent::note_off(0, 0, 69)]);
        assert_eq!(
            status,
            ProcessStatus::Sleep,
            "release is 10 ms, block is 21 ms"
        );
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn sustain_pedal_holds_the_note() {
        let mut s = SineSynth::new(48000.0);
        run(
            &mut s,
            64,
            &[
                MidiEvent::cc(0, 0, 64, 127),
                MidiEvent::note_on(0, 0, 60, 100),
            ],
        );
        run(&mut s, 2048, &[MidiEvent::note_off(0, 0, 60)]);
        assert_eq!(s.active_voices(), 1);
        run(&mut s, 2048, &[MidiEvent::cc(0, 0, 64, 0)]);
        assert_eq!(s.active_voices(), 0);
    }
}
