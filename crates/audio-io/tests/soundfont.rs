//! SoundFont instruments (oxisynth): presets, playing, state, sharing and no allocation on the
//! audio thread. Needs a system SoundFont; the tests skip quietly where there is none.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use merlin_audio_io::{Item, OfflineRenderer};
use merlin_engine::{Command, SelectMode};
use merlin_hosting::soundfont::{Font, SoundFontPlugin, info_for, is_soundfont};
use merlin_hosting::{Io, MidiEvent, PluginHandle, PluginProcessor, ProcessStatus, SamplerTone};
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

/// Renders a C major chord on the piano with `tone` and returns the audio.
fn chord(path: &Path, tone: SamplerTone, blocks: usize) -> Vec<f32> {
    let mut plugin = SoundFontPlugin::open(path).unwrap();
    plugin.set_tone(tone);
    let mut p = plugin.activate(SR, BLOCK as u32).unwrap();
    let on = [60, 64, 67].map(|k| MidiEvent::note_on(0, 0, k, 100));
    let mut out = block(&mut *p, &on).0;
    for _ in 1..blocks {
        out.extend(block(&mut *p, &[]).0);
    }
    out
}

fn rms(v: &[f32]) -> f32 {
    (v.iter().map(|x| x * x).sum::<f32>() / v.len().max(1) as f32).sqrt()
}

/// Brightness: how much of the energy is in fast changes (the rms of the sample-to-sample
/// difference relative to the rms).
fn brightness(v: &[f32]) -> f32 {
    let d: Vec<f32> = v.windows(2).map(|w| w[1] - w[0]).collect();
    rms(&d) / rms(v).max(1e-9)
}

#[test]
fn the_tone_controls_change_the_sound() {
    let Some(path) = sf2() else { return };
    let plain = chord(&path, SamplerTone::default(), 200);
    assert!(rms(&plain) > 0.005);

    let dark = chord(
        &path,
        SamplerTone {
            cutoff: -2.0,
            ..Default::default()
        },
        200,
    );
    assert!(
        brightness(&dark) < brightness(&plain) * 0.8,
        "a lower cutoff is darker: {} vs {}",
        brightness(&dark),
        brightness(&plain)
    );

    // A slower attack: the first 20 ms are quieter.
    let first = |v: &[f32]| rms(&v[..960]);
    let slow = chord(
        &path,
        SamplerTone {
            attack: 6.0,
            ..Default::default()
        },
        200,
    );
    assert!(
        first(&slow) < first(&plain) * 0.7,
        "{} vs {}",
        first(&slow),
        first(&plain)
    );

    // A tuning offset moves the pitch: the waveform no longer lines up with the plain one.
    let sharp = chord(
        &path,
        SamplerTone {
            fine_tune: 100.0,
            ..Default::default()
        },
        100,
    );
    assert_ne!(&sharp[..2000], &plain[..2000]);

    // A longer release keeps the sound after note-off; a shorter one lets it go sooner.
    let tail = |release: f32| {
        let mut plugin = SoundFontPlugin::open(&path).unwrap();
        plugin.set_tone(SamplerTone {
            release,
            ..Default::default()
        });
        let mut p = plugin.activate(SR, BLOCK as u32).unwrap();
        block(&mut *p, &[MidiEvent::note_on(0, 0, 60, 100)]);
        for _ in 0..100 {
            block(&mut *p, &[]);
        }
        block(&mut *p, &[MidiEvent::note_off(0, 0, 60)]);
        let mut v = Vec::new();
        for _ in 0..40 {
            v.extend(block(&mut *p, &[]).0);
        }
        rms(&v[v.len() / 2..])
    };
    assert!(
        tail(-3.0) < tail(2.0),
        "release changes the tail: {} vs {}",
        tail(-3.0),
        tail(2.0)
    );
}

#[test]
fn tone_values_are_kept_in_range_and_saved_in_the_state() {
    let Some(path) = sf2() else { return };
    let wild = SamplerTone {
        attack: 99.0,
        release: f32::NAN,
        cutoff: -99.0,
        resonance: 500.0,
        fine_tune: 1e9,
        polyphony: 0,
    };
    let c = wild.clamped();
    assert_eq!(
        (c.attack, c.release, c.cutoff, c.resonance),
        (8.0, 0.0, -3.0, 24.0)
    );
    assert_eq!((c.fine_tune, c.polyphony), (100.0, 4));

    let mut a = SoundFontPlugin::open(&path).unwrap();
    let tone = SamplerTone {
        attack: 1.0,
        release: -1.0,
        cutoff: 0.5,
        resonance: 6.0,
        fine_tune: -20.0,
        polyphony: 32,
    };
    a.set_tone(tone);
    let state = a.save_state().unwrap();
    let mut b = SoundFontPlugin::open(&path).unwrap();
    b.load_state(&state).unwrap();
    assert_eq!(b.tone(), Some(tone));
    // States from before the tone controls still load, with the default tone.
    let mut old = b"ISF1".to_vec();
    old.extend_from_slice(&[0, 0, 24]);
    old.extend_from_slice(&1.0f32.to_le_bytes());
    let mut c = SoundFontPlugin::open(&path).unwrap();
    c.load_state(&old).unwrap();
    assert_eq!(c.preset(), Some((0, 24)));
    assert_eq!(c.tone(), Some(SamplerTone::default()));
}

#[test]
fn changing_the_tone_while_playing_does_not_allocate() {
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
    run(&[
        MidiEvent::note_on(0, 0, 60, 100),
        MidiEvent::note_on(0, 0, 64, 100),
    ]);
    for round in 0..60 {
        let x = (round % 7) as f32 - 3.0;
        plugin.set_tone(SamplerTone {
            attack: x,
            release: -x,
            cutoff: x / 2.0,
            resonance: (round % 24) as f32,
            fine_tune: x * 10.0,
            polyphony: 8 + (round % 100) as u16,
        });
        run(&[MidiEvent::note_on(5, 0, 40 + (round % 40) as u8, 90)]);
        run(&[]);
    }
}

#[test]
fn drum_kits_play_on_their_own_channel() {
    let Some(path) = sf2() else { return };
    let mut plugin = SoundFontPlugin::open(&path).unwrap();
    let Some(kit) = plugin
        .presets()
        .unwrap()
        .iter()
        .find(|p| p.bank == 128)
        .cloned()
    else {
        return; // this font has no drum kit
    };
    plugin.set_preset(kit.bank, kit.program);
    let mut p = plugin.activate(SR, BLOCK as u32).unwrap();
    let mut out = block(&mut *p, &[MidiEvent::note_on(0, 0, 38, 110)]).0;
    for _ in 0..30 {
        out.extend(block(&mut *p, &[]).0);
    }
    assert!(
        peak(&out) > 0.02,
        "a snare from the drum kit {:?}: {}",
        kit.name,
        peak(&out)
    );
    // Back to a melodic preset: the notes follow it to channel 0.
    plugin.set_preset(0, 0);
    let mut v = block(&mut *p, &[]).0;
    v.extend(block(&mut *p, &[MidiEvent::note_on(0, 0, 60, 100)]).0);
    for _ in 0..30 {
        v.extend(block(&mut *p, &[]).0);
    }
    assert!(peak(&v) > 0.02);
}

/// A folder with three SoundFonts (links to system files; the first two are the same font under
/// two names, the third a small GS variation font), if this machine has them.
fn three_fonts(tag: &str) -> Option<PathBuf> {
    let dir = Path::new("/usr/share/sounds/sf2");
    let (gm, gs) = (dir.join("TimGM6mb.sf2"), dir.join("FluidR3_GS.sf2"));
    if !gm.exists() || !gs.exists() {
        return None;
    }
    let folder = std::env::temp_dir().join(format!(
        "merlin-sf-{tag}-{}/Three Fonts",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::os::unix::fs::symlink(&gm, folder.join("B Tim.sf2")).unwrap();
    std::os::unix::fs::symlink(&gm, folder.join("C Tim again.sf2")).unwrap();
    std::os::unix::fs::symlink(gs, folder.join("D Gs.sf2")).unwrap();
    // Not a SoundFont: ignored.
    std::fs::write(folder.join("readme.txt"), b"x").unwrap();
    Some(folder)
}

#[test]
fn a_folder_of_soundfonts_is_one_instrument_named_like_the_folder() {
    let Some(folder) = three_fonts("one") else {
        return;
    };
    use merlin_hosting::soundfont::{collection_info, files_of, is_collection, is_source};
    assert!(is_collection(&folder) && is_source(&folder));
    assert!(!is_collection(Path::new(
        "/usr/share/sounds/sf2/TimGM6mb.sf2"
    )));
    assert!(!is_collection(Path::new("/nonexistent")));
    assert_eq!(files_of(&folder).len(), 3, "the readme is not a SoundFont");
    let info = collection_info(&folder);
    assert_eq!(
        (info.name.as_str(), info.id.as_str()),
        ("Three Fonts", "folder:Three Fonts")
    );

    let plugin = SoundFontPlugin::open(&folder).expect("open the folder");
    assert_eq!(plugin.info().name, "Three Fonts");
    let presets = plugin.presets().unwrap();
    let from = |file: &str| {
        presets
            .iter()
            .filter(|p| p.file.as_deref() == Some(file))
            .count()
    };
    assert!(from("B Tim") > 50 && from("C Tim again") > 50 && from("D Gs") > 20);
    // The same General MIDI piano is in two files, under different bank numbers.
    let pianos: Vec<_> = presets
        .iter()
        .filter(|p| p.bank & 0xFF == 0 && p.program == 0)
        .collect();
    assert_eq!(pianos.len(), 2);
    assert_ne!(pianos[0].bank, pianos[1].bank);
    assert!(
        presets
            .windows(2)
            .all(|w| (w[0].bank, w[0].program) <= (w[1].bank, w[1].program))
    );
    let _ = std::fs::remove_dir_all(folder.parent().unwrap());
}

#[test]
fn a_preset_of_a_later_font_plays_from_that_font() {
    let Some(folder) = three_fonts("play") else {
        return;
    };
    let mut plugin = SoundFontPlugin::open(&folder).unwrap();
    // A preset only the third file has: the synth must look in that file, not the first that
    // happens to be on top of its stack.
    let gs = plugin
        .presets()
        .unwrap()
        .iter()
        .find(|p| p.file.as_deref() == Some("D Gs") && p.program < 100)
        .or_else(|| {
            plugin
                .presets()
                .unwrap()
                .iter()
                .find(|p| p.file.as_deref() == Some("D Gs"))
        })
        .unwrap()
        .clone();
    let mut p = plugin.activate(SR, BLOCK as u32).unwrap();
    plugin.set_preset(gs.bank, gs.program);
    for _ in 0..50 {
        block(&mut *p, &[]);
    }
    let mut out = block(&mut *p, &[MidiEvent::note_on(0, 0, 60, 110)]).0;
    for _ in 0..40 {
        out.extend(block(&mut *p, &[]).0);
    }
    assert!(peak(&out) > 0.005, "{} is audible: {}", gs.name, peak(&out));
    assert_eq!(plugin.preset(), Some((gs.bank, gs.program)));

    // And the piano of the first file still sounds, after going there from another font.
    let piano = plugin
        .presets()
        .unwrap()
        .iter()
        .find(|p| p.file.as_deref() == Some("B Tim") && p.program == 0 && p.bank & 0xFF == 0)
        .unwrap()
        .clone();
    block(&mut *p, &[MidiEvent::note_off(0, 0, 60)]);
    plugin.set_preset(piano.bank, piano.program);
    for _ in 0..500 {
        block(&mut *p, &[]);
    }
    let mut out = block(&mut *p, &[MidiEvent::note_on(0, 0, 60, 100)]).0;
    for _ in 0..40 {
        out.extend(block(&mut *p, &[]).0);
    }
    assert!(peak(&out) > 0.02, "the piano is audible: {}", peak(&out));
    let _ = std::fs::remove_dir_all(folder.parent().unwrap());
}

#[test]
fn a_saved_preset_stays_in_its_file_when_the_folder_changes() {
    let Some(folder) = three_fonts("state") else {
        return;
    };
    let mut a = SoundFontPlugin::open(&folder).unwrap();
    let in_second = a
        .presets()
        .unwrap()
        .iter()
        .find(|p| p.file.as_deref() == Some("C Tim again") && p.program == 24)
        .unwrap()
        .clone();
    a.set_preset(in_second.bank, in_second.program);
    let state = a.save_state().unwrap();

    // A file that sorts first is added: every file's number moves up by one.
    std::os::unix::fs::symlink(
        "/usr/share/sounds/sf2/TimGM6mb.sf2",
        folder.join("A Extra.sf2"),
    )
    .unwrap();
    let mut b = SoundFontPlugin::open(&folder).unwrap();
    b.load_state(&state).unwrap();
    let (bank, program) = b.preset().unwrap();
    let now = b
        .presets()
        .unwrap()
        .iter()
        .find(|p| (p.bank, p.program) == (bank, program))
        .unwrap();
    assert_eq!(
        now.file.as_deref(),
        Some("C Tim again"),
        "still in the same file"
    );
    assert_eq!(program, 24);
    assert_ne!(bank, in_second.bank, "although its number changed");
    let _ = std::fs::remove_dir_all(folder.parent().unwrap());
}
