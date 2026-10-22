//! The audio path must not allocate. `assert_no_alloc` aborts the test if it does (debug builds).

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use merlin_engine::{Command, Filter, SelectMode};
use merlin_hosting::MidiEvent;
use merlin_hosting::builtin::SineSynth;
use merlin_model::FxSettings;

struct Gain(f32);

impl merlin_hosting::PluginProcessor for Gain {
    fn process(
        &mut self,
        io: &mut merlin_hosting::Io,
        _: &[MidiEvent],
    ) -> merlin_hosting::ProcessStatus {
        let (l, r) = io.input.unwrap();
        for i in 0..io.frames() {
            io.out_l[i] = l[i] * self.0;
            io.out_r[i] = r[i] * self.0;
        }
        merlin_hosting::ProcessStatus::Continue
    }
}

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn processing_does_not_allocate() {
    let (mut engine, mut handle) = merlin_engine::new(48000.0, 128);
    for slot in 0..4 {
        handle
            .send(Command::SetProcessor {
                slot,
                processor: Box::new(SineSynth::new(48000.0)),
            })
            .ok();
        handle
            .send(Command::SetFilter {
                slot,
                filter: Filter {
                    transpose: slot as i8,
                    ..Filter::ALL
                },
            })
            .ok();
        handle
            .send(Command::Select {
                slot,
                mode: if slot == 0 {
                    SelectMode::Exclusive
                } else {
                    SelectMode::Layer
                },
            })
            .ok();
    }
    for slot in 0..4 {
        for index in 0..2 {
            handle
                .send(Command::SetInsert {
                    slot,
                    index,
                    processor: Box::new(Gain(0.9)),
                })
                .ok();
        }
    }
    let mut fx = FxSettings::default();
    fx.eq.enabled = true;
    fx.eq.mid_db = 5.0;
    fx.comp.enabled = true;
    fx.delay.enabled = true;
    fx.reverb.enabled = true;
    for slot in 0..4 {
        handle.send(Command::SetFx { slot, settings: fx }).ok();
    }
    let (mut l, mut r) = (vec![0.0; 128], vec![0.0; 128]);
    let notes: Vec<MidiEvent> = (0..8)
        .map(|i| MidiEvent::note_on(i * 10, 0, 50 + i as u8, 100))
        .chain([
            MidiEvent::cc(90, 0, 64, 127),
            MidiEvent::pitch_bend(100, 0, 9000),
        ])
        .collect();
    let offs: Vec<MidiEvent> = (0..8)
        .map(|i| MidiEvent::note_off(i * 10, 0, 50 + i as u8))
        .collect();

    // Commands that run inside the guarded region too: select, filter, mix, panic.
    for round in 0..50 {
        if round == 20 {
            handle
                .send(Command::Select {
                    slot: 2,
                    mode: SelectMode::Exclusive,
                })
                .ok();
            handle
                .send(Command::SetMix {
                    slot: 1,
                    volume: 0.5,
                    pan: 0.3,
                    mute: false,
                })
                .ok();
            handle
                .send(Command::SetFilter {
                    slot: 3,
                    filter: Filter {
                        key_lo: 52,
                        ..Filter::ALL
                    },
                })
                .ok();
        }
        if round == 40 {
            handle.send(Command::Panic).ok();
        }
        assert_no_alloc(|| {
            engine.process(if round % 2 == 0 { &notes } else { &offs }, &mut l, &mut r);
        });
    }
    // A block longer than the engine's maximum is split without allocating too.
    let (mut l, mut r) = (vec![0.0; 1000], vec![0.0; 1000]);
    assert_no_alloc(|| engine.process(&notes, &mut l, &mut r));
}
