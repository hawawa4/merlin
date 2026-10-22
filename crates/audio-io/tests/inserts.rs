//! Insert effects (plugins between the built-in EQ/compressor and the delay/reverb).

use std::sync::atomic::Ordering;

use merlin_audio_io::{Item, OfflineRenderer};
use merlin_engine::{Command, SelectMode};
use merlin_hosting::builtin::SineSynth;
use merlin_hosting::{Io, MidiEvent, PluginProcessor, ProcessStatus};
use merlin_model::FxSettings;

const SR: f32 = 48000.0;
const BLOCK: usize = 128;

struct Dc(f32);

impl PluginProcessor for Dc {
    fn process(&mut self, io: &mut Io, _: &[MidiEvent]) -> ProcessStatus {
        io.out_l.fill(self.0);
        io.out_r.fill(self.0);
        ProcessStatus::Continue
    }
}

/// An effect that multiplies its input.
struct Gain(f32);

impl PluginProcessor for Gain {
    fn process(&mut self, io: &mut Io, _: &[MidiEvent]) -> ProcessStatus {
        let (l, r) = io.input.expect("an insert gets audio input");
        for i in 0..io.frames() {
            io.out_l[i] = l[i] * self.0;
            io.out_r[i] = r[i] * self.0;
        }
        ProcessStatus::Continue
    }
}

/// An effect with a tail: a slow one-pole low-pass, so output outlives the input by seconds.
struct Tail(f32);

impl PluginProcessor for Tail {
    fn process(&mut self, io: &mut Io, _: &[MidiEvent]) -> ProcessStatus {
        let (l, _) = io.input.unwrap();
        for (i, x) in l.iter().enumerate().take(io.frames()) {
            // Smooths the rectified input, so it has a level (a plain low-pass of a sine has none).
            self.0 = self.0 * 0.9998 + x.abs() * 0.0002;
            io.out_l[i] = self.0;
            io.out_r[i] = self.0;
        }
        ProcessStatus::Continue
    }
}

fn send(r: &mut OfflineRenderer, c: Command) {
    r.handle().send(c).ok().expect("command queue");
}

fn note(on: bool, at: usize) -> (usize, Item) {
    let ev = if on {
        MidiEvent::note_on(0, 0, 60, 110)
    } else {
        MidiEvent::note_off(0, 0, 60)
    };
    (at, Item::Midi(ev))
}

#[test]
fn an_insert_processes_the_slots_audio() {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    send(
        &mut r,
        Command::SetProcessor {
            slot: 0,
            processor: Box::new(Dc(0.5)),
        },
    );
    send(
        &mut r,
        Command::SetInsert {
            slot: 0,
            index: 0,
            processor: Box::new(Gain(0.5)),
        },
    );
    r.render(BLOCK * 4, vec![]);
    let out = r.render(BLOCK * 4, vec![]);
    assert!(
        (out.left[BLOCK * 4 - 1] - 0.25).abs() < 1e-4,
        "0.5 through a 0.5 gain: {}",
        out.left[BLOCK * 4 - 1]
    );
    // Two inserts chain in order.
    send(
        &mut r,
        Command::SetInsert {
            slot: 0,
            index: 1,
            processor: Box::new(Gain(0.5)),
        },
    );
    r.render(BLOCK * 4, vec![]);
    let out = r.render(BLOCK * 4, vec![]);
    assert!((out.left[BLOCK * 4 - 1] - 0.125).abs() < 1e-4);
}

#[test]
fn bypass_and_removal_work_and_processors_come_back() {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    send(
        &mut r,
        Command::SetProcessor {
            slot: 0,
            processor: Box::new(Dc(0.5)),
        },
    );
    send(
        &mut r,
        Command::SetInsert {
            slot: 0,
            index: 2,
            processor: Box::new(Gain(0.0)),
        },
    );
    r.render(BLOCK * 4, vec![]);
    assert!(
        r.render(BLOCK * 4, vec![]).peak(200..BLOCK * 4) < 1e-6,
        "a zero gain silences the slot"
    );

    send(
        &mut r,
        Command::SetInsertBypass {
            slot: 0,
            index: 2,
            bypass: true,
        },
    );
    r.render(BLOCK * 4, vec![]);
    assert!(
        (r.render(BLOCK * 4, vec![]).left[BLOCK * 4 - 1] - 0.5).abs() < 1e-4,
        "bypassed: untouched"
    );

    // Replacing hands the old one back, tagged with its position.
    send(
        &mut r,
        Command::SetInsert {
            slot: 0,
            index: 2,
            processor: Box::new(Gain(1.0)),
        },
    );
    r.render(BLOCK, vec![]);
    let retired = r.handle().take_retired();
    assert_eq!(retired.len(), 1);
    assert_eq!((retired[0].slot, retired[0].insert), (0, Some(2)));

    send(&mut r, Command::ClearInsert { slot: 0, index: 2 });
    r.render(BLOCK, vec![]);
    let retired = r.handle().take_retired();
    assert_eq!((retired[0].slot, retired[0].insert), (0, Some(2)));
}

#[test]
fn inserts_sit_before_the_delay_and_reverb() {
    let render = |bypass: bool| {
        let mut r = OfflineRenderer::new(SR, BLOCK);
        send(
            &mut r,
            Command::SetProcessor {
                slot: 0,
                processor: Box::new(SineSynth::new(SR)),
            },
        );
        send(
            &mut r,
            Command::Select {
                slot: 0,
                mode: SelectMode::Exclusive,
            },
        );
        let mut fx = FxSettings::default();
        fx.delay.enabled = true;
        fx.delay.mix = 1.0;
        fx.delay.time_ms = 100.0;
        fx.delay.feedback = 0.5;
        send(
            &mut r,
            Command::SetFx {
                slot: 0,
                settings: fx,
            },
        );
        send(
            &mut r,
            Command::SetInsert {
                slot: 0,
                index: 0,
                processor: Box::new(Gain(0.0)),
            },
        );
        send(
            &mut r,
            Command::SetInsertBypass {
                slot: 0,
                index: 0,
                bypass,
            },
        );
        r.render(BLOCK, vec![]);
        r.render(48000, vec![note(true, 0), note(false, 4000)])
    };
    // A muting insert in front of the delay leaves nothing for the delay to repeat: total silence
    // (if the insert were after the delay, the echoes would survive).
    assert!(
        render(false).peak(0..48000) < 1e-9,
        "the delay only ever sees the insert's output"
    );
    assert!(
        render(true).peak(0..48000) > 0.05,
        "and with the insert bypassed there is sound"
    );
}

#[test]
fn an_insert_tail_keeps_an_idle_slot_awake_until_it_is_gone() {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    send(
        &mut r,
        Command::SetProcessor {
            slot: 0,
            processor: Box::new(SineSynth::new(SR)),
        },
    );
    send(
        &mut r,
        Command::SetProcessor {
            slot: 1,
            processor: Box::new(Dc(0.0)),
        },
    );
    send(
        &mut r,
        Command::SetInsert {
            slot: 0,
            index: 0,
            processor: Box::new(Tail(0.0)),
        },
    );
    send(
        &mut r,
        Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        },
    );
    r.render(BLOCK, vec![]);
    r.render(9600, vec![note(true, 0), note(false, 4800)]);
    // Move the keyboard away: slot 0 is idle, its instrument asleep, but the insert still rings.
    send(
        &mut r,
        Command::Select {
            slot: 1,
            mode: SelectMode::Exclusive,
        },
    );
    let tail = r.render(48000, vec![]);
    let rms = |a: usize, b: usize| {
        (tail.left[a..b].iter().map(|x| x * x).sum::<f32>() / (b - a) as f32).sqrt()
    };
    assert!(
        rms(24000, 28000) > 1e-4,
        "the insert's tail is still audible: {}",
        rms(24000, 28000)
    );
    assert_eq!(
        r.handle().shared.sleeping.load(Ordering::Relaxed) & 1,
        0,
        "not asleep while ringing"
    );
    // Much later it has died away and the slot is skipped again.
    r.render(48000 * 40, vec![]);
    assert_eq!(r.handle().shared.sleeping.load(Ordering::Relaxed) & 1, 1);
}
