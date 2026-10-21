//! The built-in effects inside the engine.

use std::sync::atomic::Ordering;

use merlin_audio_io::{Item, OfflineRenderer, Rendered};
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

fn send(r: &mut OfflineRenderer, c: Command) {
    r.handle().send(c).ok().expect("command queue");
}

fn synth_rig() -> OfflineRenderer {
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
    r.render(BLOCK, vec![]);
    r
}

fn note(on: bool, at: usize) -> (usize, Item) {
    let ev = if on {
        MidiEvent::note_on(0, 0, 60, 110)
    } else {
        MidiEvent::note_off(0, 0, 60)
    };
    (at, Item::Midi(ev))
}

fn reverb_fx(room: f32) -> FxSettings {
    let mut fx = FxSettings::default();
    fx.reverb.enabled = true;
    fx.reverb.mix = 0.8;
    fx.reverb.room = room;
    fx
}

fn rms_of(r: &Rendered, range: std::ops::Range<usize>) -> f32 {
    let xs = &r.left[range];
    (xs.iter().map(|x| x * x).sum::<f32>() / xs.len() as f32).sqrt()
}

#[test]
fn disabled_effects_change_nothing() {
    let script = || vec![note(true, 10), note(false, 9000)];
    let mut a = synth_rig();
    let plain = a.render(24000, script());

    let mut b = synth_rig();
    // Settings that are all switched off, with non-default values everywhere.
    let mut fx = FxSettings::default();
    fx.eq.low_db = 12.0;
    fx.comp.ratio = 10.0;
    fx.delay.mix = 1.0;
    fx.reverb.mix = 1.0;
    send(
        &mut b,
        Command::SetFx {
            slot: 0,
            settings: fx,
        },
    );
    let with = b.render(24000, script());
    assert_eq!(plain.left, with.left, "off is bit-exact");
}

#[test]
fn a_reverb_tail_rings_on_after_the_instrument_sleeps_and_then_the_slot_goes_idle() {
    let mut r = synth_rig();
    send(
        &mut r,
        Command::SetFx {
            slot: 0,
            settings: reverb_fx(0.6),
        },
    );
    // Play on slot 0, release, then move the keyboard to slot 1 so slot 0 is idle.
    r.render(9600, vec![note(true, 0), note(false, 4800)]);
    send(
        &mut r,
        Command::Select {
            slot: 1,
            mode: SelectMode::Exclusive,
        },
    );
    let tail = r.render(48000 * 6, vec![]);
    assert!(
        rms_of(&tail, 24000..28800) > 1e-4,
        "the tail is still audible 0.5 s after the instrument stopped: {}",
        rms_of(&tail, 24000..28800)
    );
    assert!(
        rms_of(&tail, 24000..28800) > rms_of(&tail, 250000..254000),
        "and it decays"
    );
    // Eventually the tail is gone and the slot is skipped entirely.
    r.render(48000 * 10, vec![]);
    assert_eq!(
        r.handle().shared.sleeping.load(Ordering::Relaxed) & 1,
        1,
        "idle slot with a dead tail sleeps"
    );
}

#[test]
fn the_compressor_turns_loud_signals_down_and_reports_it() {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    send(
        &mut r,
        Command::SetProcessor {
            slot: 0,
            processor: Box::new(Dc(0.8)),
        },
    );
    let mut fx = FxSettings::default();
    fx.comp.enabled = true;
    fx.comp.threshold_db = -20.0;
    fx.comp.ratio = 8.0;
    fx.comp.attack_ms = 1.0;
    fx.comp.knee_db = 0.0;
    send(
        &mut r,
        Command::SetFx {
            slot: 0,
            settings: fx,
        },
    );
    r.render(BLOCK, vec![]);
    let out = r.render(24000, vec![]);
    let level = out.left[23999];
    // 0.8 is -1.94 dBFS: 18 dB over -> 8:1 leaves 2.25 dB over -> -17.75 dBFS.
    assert!((20.0 * level.log10() + 17.75).abs() < 0.3, "{level}");
    let reduction = f32::from_bits(r.handle().shared.comp_reduction[0].load(Ordering::Relaxed));
    assert!(
        reduction > 10.0 && reduction < 20.0,
        "meter reads {reduction} dB"
    );
}

#[test]
fn the_master_limiter_never_lets_the_output_exceed_the_ceiling() {
    let ceiling = 10f32.powf(-0.3 / 20.0);
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
        Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        },
    );
    send(
        &mut r,
        Command::SetMix {
            slot: 0,
            volume: 2.0,
            pan: 0.0,
            mute: false,
        },
    );
    r.render(BLOCK, vec![]);
    let chord: Vec<(usize, Item)> = [48u8, 55, 60, 64, 67, 72, 76, 79, 84]
        .iter()
        .map(|&k| (0, Item::Midi(MidiEvent::note_on(0, 0, k, 127))))
        .collect();
    let out = r.render(48000, chord);
    // 9 voices at 0.25 each, doubled by the slot volume: far above full scale before limiting.
    assert!(
        out.left
            .iter()
            .chain(&out.right)
            .all(|x| x.abs() <= ceiling + 1e-4),
        "limited"
    );
    assert!(out.peak(4000..48000) > 0.9, "and still loud");
    let reduction = f32::from_bits(r.handle().shared.limiter_reduction.load(Ordering::Relaxed));
    assert!(
        reduction > 1.0,
        "the meter shows the limiter working: {reduction}"
    );

    // Turned off, the same signal exceeds it.
    send(&mut r, Command::SetLimiter(false));
    let raw = r.render(24000, vec![]);
    assert!(
        raw.peak(8000..24000) > ceiling + 0.05,
        "peak {}",
        raw.peak(8000..24000)
    );
}

#[test]
fn effects_do_not_depend_on_the_block_size() {
    let render = |block: usize| {
        let mut r = OfflineRenderer::new(SR, block);
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
        fx.eq.enabled = true;
        fx.eq.mid_db = 6.0;
        fx.comp.enabled = true;
        fx.delay.enabled = true;
        fx.reverb.enabled = true;
        send(
            &mut r,
            Command::SetFx {
                slot: 0,
                settings: fx,
            },
        );
        // The same warm-up for every block size (the denormal guard adds a tiny constant per
        // sample, so even silence leaves a different state if it is rendered for longer).
        r.render(960, vec![]);
        r.render(
            24000,
            vec![
                (0, Item::Midi(MidiEvent::note_on(0, 0, 60, 100))),
                (6000, Item::Midi(MidiEvent::note_off(0, 0, 60))),
            ],
        )
    };
    let (a, b) = (render(32), render(480));
    let first = a.left.iter().zip(&b.left).position(|(x, y)| x != y);
    assert_eq!(
        first,
        None,
        "first difference: {:?}",
        first.map(|i| (i, a.left[i], b.left[i]))
    );
}

#[test]
fn absurd_settings_cannot_break_the_output() {
    let mut r = synth_rig();
    let mut fx = FxSettings::default();
    fx.eq.enabled = true;
    fx.eq.low_db = f32::NAN;
    fx.eq.mid_q = -5.0;
    fx.comp.enabled = true;
    fx.comp.ratio = f32::INFINITY;
    fx.comp.attack_ms = -1.0;
    fx.delay.enabled = true;
    fx.delay.time_ms = 1e9;
    fx.delay.feedback = 7.0;
    fx.reverb.enabled = true;
    fx.reverb.room = 50.0;
    fx.reverb.mix = f32::NAN;
    send(
        &mut r,
        Command::SetFx {
            slot: 0,
            settings: fx,
        },
    );
    let out = r.render(48000, vec![note(true, 0)]);
    assert!(
        out.left
            .iter()
            .chain(&out.right)
            .all(|x| x.is_finite() && x.abs() < 2.0)
    );
}
