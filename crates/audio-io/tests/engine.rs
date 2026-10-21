//! The engine end to end, rendered offline: routing, switching, sleeping, mixing, panic.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use merlin_audio_io::{Item, OfflineRenderer, Rendered};
use merlin_engine::{Command, Filter, SelectMode};
use merlin_hosting::builtin::SineSynth;
use merlin_hosting::{Io, MidiEvent, PluginProcessor, ProcessStatus};

const SR: f32 = 48000.0;
const BLOCK: usize = 128;

/// Writes a constant to both channels (a stand-in for a steady instrument).
struct Dc(f32);

impl PluginProcessor for Dc {
    fn process(&mut self, io: &mut Io, _: &[MidiEvent]) -> ProcessStatus {
        io.out_l.fill(self.0);
        io.out_r.fill(self.0);
        ProcessStatus::Continue
    }
}

/// A sine synth that counts how often the engine calls it.
struct Counted(SineSynth, Arc<AtomicUsize>);

impl PluginProcessor for Counted {
    fn process(&mut self, io: &mut Io, events: &[MidiEvent]) -> ProcessStatus {
        self.1.fetch_add(1, Ordering::Relaxed);
        self.0.process(io, events)
    }
}

fn rig(n: usize) -> OfflineRenderer {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    for slot in 0..n {
        let _ = r.handle().send(Command::SetProcessor {
            slot,
            processor: Box::new(SineSynth::new(SR)),
        });
    }
    r.render(BLOCK, vec![]);
    r
}

fn cmd(c: Command) -> Item {
    Item::Command(c)
}

fn midi(ev: MidiEvent) -> Item {
    Item::Midi(ev)
}

fn peak_of(r: &mut OfflineRenderer, slot: usize) -> f32 {
    f32::from_bits(r.handle().shared.peaks[slot].swap(0, Ordering::Relaxed))
}

#[test]
fn only_the_selected_slot_sounds() {
    let mut r = rig(2);
    r.handle()
        .send(Command::Select {
            slot: 1,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.render(BLOCK, vec![]);
    peak_of(&mut r, 0);
    peak_of(&mut r, 1);
    r.render(4800, vec![(0, midi(MidiEvent::note_on(0, 0, 69, 127)))]);
    assert_eq!(peak_of(&mut r, 0), 0.0);
    assert!(peak_of(&mut r, 1) > 0.1);
}

#[test]
fn switching_while_a_note_is_held_lets_it_ring_and_release_on_the_old_slot() {
    let mut r = rig(2);
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.render(BLOCK, vec![]);
    r.render(2400, vec![(0, midi(MidiEvent::note_on(0, 0, 69, 127)))]);
    // Switch to slot 1 and play there; then let go of the first key.
    let out = r.render(
        4800,
        vec![
            (
                0,
                cmd(Command::Select {
                    slot: 1,
                    mode: SelectMode::Exclusive,
                }),
            ),
            (BLOCK * 4, midi(MidiEvent::note_on(0, 0, 72, 127))),
            (BLOCK * 8, midi(MidiEvent::note_off(0, 0, 69))),
            (BLOCK * 8, midi(MidiEvent::note_off(0, 0, 72))),
        ],
    );
    assert!(
        out.peak(0..BLOCK * 4) > 0.1,
        "slot 0's note keeps ringing after the switch"
    );
    // Both notes are released: everything ends in silence and nothing is held.
    let tail = r.render(4800, vec![]);
    assert_eq!(tail.peak(2400..4800), 0.0, "no stuck note");
    assert!(
        r.handle()
            .shared
            .held
            .iter()
            .all(|h| h.load(Ordering::Relaxed) == 0)
    );
}

#[test]
fn keyboard_activity_is_published_per_slot() {
    let mut r = rig(2);
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 1,
            mode: SelectMode::Layer,
        })
        .ok();
    r.render(BLOCK, vec![]);
    r.render(BLOCK, vec![(0, midi(MidiEvent::note_on(0, 0, 60, 100)))]);
    assert_eq!(r.handle().shared.held[60].load(Ordering::Relaxed), 0b11);
    r.render(BLOCK, vec![(0, midi(MidiEvent::note_off(0, 0, 60)))]);
    assert_eq!(r.handle().shared.held[60].load(Ordering::Relaxed), 0);
}

#[test]
fn zones_and_transpose_through_the_engine() {
    let mut r = rig(2);
    r.handle()
        .send(Command::SetFilter {
            slot: 0,
            filter: Filter {
                key_hi: 59,
                ..Filter::ALL
            },
        })
        .ok();
    r.handle()
        .send(Command::SetFilter {
            slot: 1,
            filter: Filter {
                key_lo: 60,
                transpose: -12,
                ..Filter::ALL
            },
        })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 1,
            mode: SelectMode::Layer,
        })
        .ok();
    r.render(BLOCK, vec![]);
    peak_of(&mut r, 0);
    r.render(2400, vec![(0, midi(MidiEvent::note_on(0, 0, 72, 127)))]);
    assert_eq!(peak_of(&mut r, 0), 0.0, "72 is outside slot 0's zone");
    assert!(peak_of(&mut r, 1) > 0.1);
}

#[test]
fn block_size_does_not_change_the_sound() {
    let script = || {
        vec![
            (100, midi(MidiEvent::note_on(0, 0, 60, 100))),
            (997, midi(MidiEvent::note_on(0, 0, 67, 80))),
            (3001, midi(MidiEvent::note_off(0, 0, 60))),
            (5003, midi(MidiEvent::note_off(0, 0, 67))),
        ]
    };
    let render = |block: usize| -> Rendered {
        let mut r = OfflineRenderer::new(SR, block);
        r.handle()
            .send(Command::SetProcessor {
                slot: 0,
                processor: Box::new(SineSynth::new(SR)),
            })
            .ok();
        r.handle()
            .send(Command::Select {
                slot: 0,
                mode: SelectMode::Exclusive,
            })
            .ok();
        r.render(block, vec![]);
        r.render(9600, script())
    };
    let a = render(32);
    let b = render(480);
    let c = render(128);
    assert_eq!(a.left, b.left, "32 vs 480 frames");
    assert_eq!(a.left, c.left, "32 vs 128 frames");
    assert!(a.peak(0..9600) > 0.1);
}

#[test]
fn rendering_is_deterministic() {
    let run = || {
        let mut r = rig(1);
        r.handle()
            .send(Command::Select {
                slot: 0,
                mode: SelectMode::Exclusive,
            })
            .ok();
        r.render(
            2400,
            vec![
                (10, midi(MidiEvent::note_on(0, 0, 64, 90))),
                (1500, midi(MidiEvent::note_off(0, 0, 64))),
            ],
        )
    };
    let (a, b) = (run(), run());
    assert_eq!(a.left, b.left);
    assert_eq!(a.right, b.right);
}

#[test]
fn idle_slots_sleep_and_wake_on_midi() {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    let calls = Arc::new(AtomicUsize::new(0));
    r.handle()
        .send(Command::SetProcessor {
            slot: 0,
            processor: Box::new(Counted(SineSynth::new(SR), calls.clone())),
        })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.render(BLOCK, vec![]);
    // Silent and idle: asleep after the first call, and never called again.
    let before = calls.load(Ordering::Relaxed);
    r.render(BLOCK * 50, vec![]);
    assert_eq!(
        calls.load(Ordering::Relaxed),
        before,
        "a sleeping slot is not processed"
    );
    assert_eq!(r.handle().shared.sleeping.load(Ordering::Relaxed), 1);
    // A note wakes it; once the release has run out it sleeps again.
    r.render(
        BLOCK * 20,
        vec![
            (0, midi(MidiEvent::note_on(0, 0, 60, 100))),
            (BLOCK * 5, midi(MidiEvent::note_off(0, 0, 60))),
        ],
    );
    let awake = calls.load(Ordering::Relaxed);
    assert!(awake > before + 5);
    r.render(BLOCK * 50, vec![]);
    assert_eq!(
        calls.load(Ordering::Relaxed),
        awake,
        "asleep again once the tail is over"
    );
}

#[test]
fn volume_pan_and_mute() {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    r.handle()
        .send(Command::SetProcessor {
            slot: 0,
            processor: Box::new(Dc(0.5)),
        })
        .ok();
    r.render(BLOCK, vec![]);
    let centre = r.render(BLOCK, vec![]);
    assert!(
        (centre.left[BLOCK - 1] - 0.5).abs() < 1e-4,
        "unity at the centre"
    );
    assert!((centre.right[BLOCK - 1] - 0.5).abs() < 1e-4);

    r.handle()
        .send(Command::SetMix {
            slot: 0,
            volume: 1.0,
            pan: -1.0,
            mute: false,
        })
        .ok();
    r.render(BLOCK, vec![]); // ramp
    let left = r.render(BLOCK, vec![]);
    assert!((left.left[10] - 0.5 * std::f32::consts::SQRT_2).abs() < 1e-3);
    assert!(left.right[10].abs() < 1e-3);

    r.handle()
        .send(Command::SetMix {
            slot: 0,
            volume: 1.0,
            pan: 0.0,
            mute: true,
        })
        .ok();
    r.render(BLOCK, vec![]);
    let muted = r.render(BLOCK, vec![]);
    assert_eq!(muted.peak(0..BLOCK), 0.0);

    r.handle()
        .send(Command::SetMix {
            slot: 0,
            volume: 1.0,
            pan: 0.0,
            mute: false,
        })
        .ok();
    r.handle().send(Command::SetMasterVolume(0.5)).ok();
    r.render(BLOCK, vec![]);
    let half = r.render(BLOCK, vec![]);
    assert!((half.left[10] - 0.25).abs() < 1e-3, "master volume");
}

#[test]
fn gain_changes_are_ramped_not_stepped() {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    r.handle()
        .send(Command::SetProcessor {
            slot: 0,
            processor: Box::new(Dc(0.5)),
        })
        .ok();
    r.render(BLOCK, vec![]);
    r.handle()
        .send(Command::SetMix {
            slot: 0,
            volume: 0.0,
            pan: 0.0,
            mute: false,
        })
        .ok();
    let out = r.render(BLOCK, vec![]);
    let biggest_step = out
        .left
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(
        biggest_step < 0.01,
        "no click when a slot is turned down: {biggest_step}"
    );
}

#[test]
fn panic_silences_everything_and_the_rig_keeps_working() {
    let mut r = rig(2);
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 1,
            mode: SelectMode::Layer,
        })
        .ok();
    r.render(BLOCK, vec![]);
    r.render(
        2400,
        vec![
            (0, midi(MidiEvent::cc(0, 0, 64, 127))),
            (0, midi(MidiEvent::note_on(0, 0, 60, 100))),
            (0, midi(MidiEvent::note_on(0, 0, 64, 100))),
        ],
    );
    let out = r.render(4800, vec![(0, cmd(Command::Panic))]);
    assert!(
        out.peak(0..4800) > 0.0,
        "the duck fades the sound out, it does not cut it"
    );
    assert_eq!(
        out.peak(2400..4800),
        0.0,
        "silent after the panic, pedal included"
    );
    assert!(
        r.handle()
            .shared
            .held
            .iter()
            .all(|h| h.load(Ordering::Relaxed) == 0)
    );
    // The duck is over and a new note is heard at full level.
    let after = r.render(4800, vec![(0, midi(MidiEvent::note_on(0, 0, 69, 127)))]);
    assert!(after.peak(2400..4800) > 0.2);
}

#[test]
fn removed_processors_come_back_to_the_main_thread() {
    let mut r = rig(1);
    r.handle()
        .send(Command::SetProcessor {
            slot: 0,
            processor: Box::new(Dc(0.1)),
        })
        .ok();
    r.render(BLOCK, vec![]);
    let retired = r.handle().take_retired();
    assert_eq!(
        retired.len(),
        1,
        "the replaced synth is handed back, not dropped on the audio thread"
    );
    r.handle().send(Command::ClearProcessor { slot: 0 }).ok();
    r.render(BLOCK, vec![]);
    assert_eq!(r.handle().take_retired().len(), 1);
    let silent = r.render(BLOCK, vec![]);
    assert_eq!(silent.peak(0..BLOCK), 0.0);
}

#[test]
fn removing_an_instrument_mid_note_leaves_nothing_held() {
    let mut r = rig(1);
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.render(BLOCK, vec![]);
    r.render(BLOCK, vec![(0, midi(MidiEvent::note_on(0, 0, 60, 100)))]);
    r.handle().send(Command::ClearProcessor { slot: 0 }).ok();
    r.render(BLOCK, vec![(0, midi(MidiEvent::note_off(0, 0, 60)))]);
    assert_eq!(r.engine().router().held_notes(), 0);
    assert_eq!(r.handle().shared.held[60].load(Ordering::Relaxed), 0);
}
