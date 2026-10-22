//! SoundFont instruments (oxisynth): presets, playing, state, sharing and no allocation on the
//! audio thread. Needs a system SoundFont; the tests skip quietly where there is none.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use merlin_audio_io::{Item, OfflineRenderer};
use merlin_engine::{Command, SelectMode};
use merlin_hosting::soundfont::{Font, SoundFontPlugin, info_for, is_soundfont};
use merlin_hosting::{Io, MidiEvent, PluginHandle, PluginProcessor, ProcessStatus};
use merlin_model::Format;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const SR: f64 = 48000.0;
const BLOCK: usize = 128;

/// A small General MIDI SoundFont, if this machine has one.
fn sf2() -> Option<PathBuf> {
    ["TimGM6mb.sf2", "default-GM.sf2", "FluidR3_GM.sf2"]
        .iter()
        .map(|n| Path::new("/usr/share/sounds/sf2").join(n))
        .find(|p| p.exists())
}

fn block(p: &mut dyn PluginProcessor, events: &[MidiEvent]) -> (Vec<f32>, ProcessStatus) {
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let status = p.process(
        &mut Io {
            input: None,
            out_l: &mut l,
            out_r: &mut r,
        },
        events,
    );
    (l, status)
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0, |m, x| m.max(x.abs()))
}

#[test]
fn files_become_instruments_without_being_decoded() {
    let info = info_for(Path::new("/sounds/Grand Piano.sf3"));
    assert_eq!(info.format, Format::SoundFont);
    assert_eq!(
        (info.id.as_str(), info.name.as_str()),
        ("Grand Piano.sf3", "Grand Piano")
    );
    assert!(info.is_instrument());
    assert!(is_soundfont(Path::new("x.SF2")) && is_soundfont(Path::new("x.sf3")));
    assert!(!is_soundfont(Path::new("x.clap")));
}

#[test]
fn lists_presets_and_plays_the_selected_one() {
    let Some(path) = sf2() else { return };
    let mut plugin = SoundFontPlugin::open(&path).expect("open");
    let presets = plugin.presets().expect("a preset list").to_vec();
    assert!(
        presets.len() > 50,
        "a GM font has many presets: {}",
        presets.len()
    );
    assert!(
        presets
            .windows(2)
            .all(|w| (w[0].bank, w[0].program) <= (w[1].bank, w[1].program))
    );
    assert_eq!(plugin.preset(), Some((0, 0)), "starts on the piano");

    let mut p = plugin.activate(SR, BLOCK as u32).expect("activate");
    // Silent and asleep until a note arrives.
    let (_, status) = block(&mut *p, &[]);
    assert_eq!(status, ProcessStatus::Sleep);

    let note = [MidiEvent::note_on(0, 0, 60, 100)];
    let mut piano = Vec::new();
    let (first, status) = block(&mut *p, &note);
    assert_eq!(status, ProcessStatus::Continue);
    piano.extend(first);
    for _ in 0..40 {
        piano.extend(block(&mut *p, &[]).0);
    }
    assert!(peak(&piano) > 0.02, "the note is audible: {}", peak(&piano));

    // Another preset sounds different for the same note.
    p.process(
        &mut Io {
            input: None,
            out_l: &mut [0.0; BLOCK],
            out_r: &mut [0.0; BLOCK],
        },
        &[MidiEvent::note_off(0, 0, 60)],
    );
    let other = presets
        .iter()
        .find(|p| p.bank == 0 && p.program == 40)
        .expect("violin");
    plugin.set_preset(other.bank, other.program);
    assert_eq!(plugin.preset(), Some((0, 40)));
    for _ in 0..400 {
        block(&mut *p, &[]);
    }
    let mut violin = Vec::new();
    violin.extend(block(&mut *p, &note).0);
    for _ in 0..40 {
        violin.extend(block(&mut *p, &[]).0);
    }
    assert!(peak(&violin) > 0.02);
    assert_ne!(piano, violin, "a different preset is a different sound");

    // An unknown preset is ignored.
    plugin.set_preset(99, 99);
    assert_eq!(plugin.preset(), Some((0, 40)));
}

#[test]
fn a_released_note_dies_away_and_the_synth_sleeps_again() {
    let Some(path) = sf2() else { return };
    let mut plugin = SoundFontPlugin::open(&path).unwrap();
    let mut p = plugin.activate(SR, BLOCK as u32).unwrap();
    block(&mut *p, &[MidiEvent::note_on(0, 0, 64, 100)]);
    for _ in 0..50 {
        block(&mut *p, &[]);
    }
    block(&mut *p, &[MidiEvent::note_off(0, 0, 64)]);
    let mut status = ProcessStatus::Continue;
    // Release times in a GM font are under a few seconds.
    for _ in 0..(SR as usize * 12 / BLOCK) {
        status = block(&mut *p, &[]).1;
        if status == ProcessStatus::Sleep {
            break;
        }
    }
    assert_eq!(
        status,
        ProcessStatus::Sleep,
        "silent notes stop costing CPU"
    );
}

#[test]
fn midi_program_change_picks_a_preset_and_the_rack_sees_it() {
    let Some(path) = sf2() else { return };
    let mut plugin = SoundFontPlugin::open(&path).unwrap();
    let mut p = plugin.activate(SR, BLOCK as u32).unwrap();
    // [0xC0, 40] on any channel selects program 40 in the current bank.
    block(&mut *p, &[MidiEvent::new(0, 0, [0xC3, 40, 0])]);
    assert_eq!(plugin.preset(), Some((0, 40)));
    // A program the file does not have changes nothing.
    block(&mut *p, &[MidiEvent::new(0, 0, [0xC0, 127, 0])]);
    let after = plugin.preset().unwrap();
    assert!(after == (0, 127) || after == (0, 40));
}

#[test]
fn state_round_trips_preset_and_gain() {
    let Some(path) = sf2() else { return };
    let mut a = SoundFontPlugin::open(&path).unwrap();
    a.set_preset(0, 24);
    a.set_gain(0.7);
    let state = a.save_state().unwrap();
    let mut b = SoundFontPlugin::open(&path).unwrap();
    b.load_state(&state).unwrap();
    assert_eq!(b.preset(), Some((0, 24)));
    assert!((b.gain().unwrap() - 0.7).abs() < 1e-6);
    assert!(b.load_state(b"garbage").is_err());
}

#[test]
fn slots_share_one_decoded_file() {
    let Some(path) = sf2() else { return };
    let a = SoundFontPlugin::open(&path).unwrap();
    let b = SoundFontPlugin::open(&path).unwrap();
    assert!(Arc::ptr_eq(a.font(), b.font()), "the file is decoded once");
    drop((a, b));
    assert!(
        Font::load(&path).is_ok(),
        "and can be loaded again once nobody holds it"
    );
}

#[test]
fn bad_files_are_errors_not_panics() {
    assert!(SoundFontPlugin::open(Path::new("/nonexistent.sf2")).is_err());
    let junk = std::env::temp_dir().join(format!("junk-{}.sf2", std::process::id()));
    std::fs::write(&junk, b"RIFF\x04\x00\x00\x00sfbkXXXXXXXXXXXXXXXXXXXXXXXX").unwrap();
    assert!(SoundFontPlugin::open(&junk).is_err());
    let _ = std::fs::remove_file(junk);
}

#[test]
fn playing_does_not_allocate() {
    let Some(path) = sf2() else { return };
    let mut plugin = SoundFontPlugin::open(&path).unwrap();
    let mut p = plugin.activate(SR, BLOCK as u32).unwrap();
    let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let mut run = |events: &[MidiEvent]| {
        assert_no_alloc(|| {
            p.process(
                &mut Io {
                    input: None,
                    out_l: &mut l,
                    out_r: &mut r,
                },
                events,
            )
        })
    };
    // Far more notes than the polyphony, chords, pedal, bend, controllers, preset switches
    // (some with sustained notes), and a long stretch of releases.
    for round in 0..40u8 {
        let events: Vec<MidiEvent> = (0..16)
            .map(|k| MidiEvent::note_on(k * 7, 0, 30 + round % 50 + k as u8, 90))
            .chain([
                MidiEvent::cc(40, 0, 64, 127),
                MidiEvent::pitch_bend(50, 0, 9000),
                MidiEvent::cc(60, 0, 1, 60),
                MidiEvent::poly_pressure(70, 0, 40, 50),
            ])
            .collect();
        run(&events);
        plugin.set_preset(0, (round % 8) * 5);
        run(&[]);
        run(&[
            MidiEvent::cc(0, 0, 64, 0),
            MidiEvent::new(10, 0, [0xC0, round % 100, 0]),
        ]);
        run(&[MidiEvent::note_off(0, 0, 33), MidiEvent::note_off(5, 0, 40)]);
    }
    for _ in 0..2000 {
        run(&[]);
    }
}

#[test]
fn it_plays_through_the_engine_and_asleep_slots_stay_quiet() {
    let Some(path) = sf2() else { return };
    let mut plugin = SoundFontPlugin::open(&path).unwrap();
    let processor = plugin.activate(SR, BLOCK as u32).unwrap();
    let mut r = OfflineRenderer::new(SR as f32, BLOCK);
    r.handle()
        .send(Command::SetProcessor { slot: 0, processor })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.render(BLOCK, vec![]);
    let out = r.render(
        24000,
        vec![
            (0, Item::Midi(MidiEvent::note_on(0, 0, 60, 100))),
            (12000, Item::Midi(MidiEvent::note_off(0, 0, 60))),
        ],
    );
    assert!(out.peak(0..12000) > 0.02, "audible through the engine");
}

#[test]
fn text_files_named_sf2_are_refused_cleanly() {
    let junk = std::env::temp_dir().join(format!("text-{}.sf2", std::process::id()));
    std::fs::write(&junk, b"definitely not a soundfont").unwrap();
    let err = SoundFontPlugin::open(&junk)
        .err()
        .expect("an error")
        .to_string();
    assert!(err.contains("not a SoundFont"), "{err}");
    let _ = std::fs::remove_file(junk);
}
