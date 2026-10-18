//! oxisynth: a pure-Rust port of FluidSynth (SF2 modulators, SF3 via lewton). This is
//! what ../musepractice uses with MuseScore_General.sf3.

use std::path::Path;

use anyhow::{Result, anyhow};
use oxisynth::{MidiEvent, SoundFont, Synth, SynthDescriptor};

use crate::{Engine, PresetInfo, SR};

pub struct Oxi {
    synth: Synth,
}

impl Oxi {
    pub fn load(path: &Path) -> Result<Self> {
        let mut file = std::fs::File::open(path)?;
        let font = SoundFont::load(&mut file).map_err(|e| anyhow!("{e:?}"))?;
        let mut synth = Synth::new(SynthDescriptor {
            sample_rate: SR as f32,
            // FluidSynth's default gain, so the comparison starts level.
            gain: 0.2,
            ..Default::default()
        })
        .map_err(|e| anyhow!("{e:?}"))?;
        synth.add_font(font, true);
        Ok(Self { synth })
    }

    fn send(&mut self, event: MidiEvent) {
        let _ = self.synth.send_event(event);
    }
}

impl Engine for Oxi {
    fn name(&self) -> &'static str {
        "oxisynth"
    }

    /// oxisynth has no public preset list: probe the usual GM banks via program changes on
    /// a spare channel (done off the audio thread, before playing).
    fn presets(&mut self) -> Vec<PresetInfo> {
        let mut out: Vec<PresetInfo> = Vec::new();
        // Melodic banks on spare channel 15; drums on channel 10 (index 9), which
        // FluidSynth-style synths always map to bank 128.
        let probes = (0..=127u8).map(|b| (15u8, b)).chain([(9u8, 0u8)]);
        for (channel, bank) in probes {
            self.send(MidiEvent::ControlChange {
                channel,
                ctrl: 0,
                value: bank,
            });
            for program_id in 0..=127u8 {
                self.send(MidiEvent::ProgramChange {
                    channel,
                    program_id,
                });
                if let Some(p) = self.synth.channel_preset(channel) {
                    let (b, n) = (p.banknum() as i32, p.num() as i32);
                    if n == program_id as i32 && !out.iter().any(|x| (x.bank, x.program) == (b, n))
                    {
                        out.push(PresetInfo {
                            bank: b,
                            program: n,
                            name: p.name().to_string(),
                        });
                    }
                }
            }
        }
        self.send(MidiEvent::SystemReset);
        out
    }

    fn program(&mut self, ch: i32, bank: i32, program: i32) {
        let channel = ch as u8;
        self.send(MidiEvent::ControlChange {
            channel,
            ctrl: 0,
            value: bank.min(127) as u8,
        });
        if bank >= 128 {
            // GM drums live in bank 128; fluid-style synths treat channel 10 as drums anyway.
            self.send(MidiEvent::ControlChange {
                channel,
                ctrl: 0,
                value: 0,
            });
        }
        self.send(MidiEvent::ProgramChange {
            channel,
            program_id: program as u8,
        });
    }

    fn note_on(&mut self, ch: i32, key: i32, vel: i32) {
        self.send(MidiEvent::NoteOn {
            channel: ch as u8,
            key: key as u8,
            vel: vel as u8,
        });
    }

    fn note_off(&mut self, ch: i32, key: i32) {
        self.send(MidiEvent::NoteOff {
            channel: ch as u8,
            key: key as u8,
        });
    }

    fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        self.synth.write_f32(n, l, 0, 1, r, 0, 1);
    }
}
