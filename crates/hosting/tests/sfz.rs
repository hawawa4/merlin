//! An SFZ folder end to end: it plays, a switch to another file of the folder reaches the audio
//! thread while notes play (and while nothing does), and the audio path allocates nothing.
//! (`assert_no_alloc` sees Rust's allocations only; sfizz is C++ and checked by its design.)

use std::path::Path;
use std::time::{Duration, Instant};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use merlin_hosting::sfz::SfzPlugin;
use merlin_hosting::{Io, MidiEvent, PluginHandle, PluginProcessor, ProcessStatus};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: u32 = 48_000;
const BLOCK: usize = 256;

/// A 16-bit mono WAV: a sine at `hz`.
fn write_sine(path: &Path, hz: f32) {
    let frames = RATE * 2;
    let mut v = Vec::new();
    v.extend(b"RIFF");
    v.extend((36 + frames * 2).to_le_bytes());
    v.extend(b"WAVEfmt ");
    v.extend(16u32.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(RATE.to_le_bytes());
    v.extend((RATE * 2).to_le_bytes());
    v.extend(2u16.to_le_bytes());
    v.extend(16u16.to_le_bytes());
    v.extend(b"data");
    v.extend((frames * 2).to_le_bytes());
    for i in 0..frames {
        let s = (i as f32 / RATE as f32 * hz * std::f32::consts::TAU).sin();
        v.extend(((s * 0.5 * i16::MAX as f32) as i16).to_le_bytes());
    }
    std::fs::write(path, v).unwrap();
}

/// Renders one block; returns the left channel and the status.
fn block(p: &mut dyn PluginProcessor, events: &[MidiEvent]) -> (Vec<f32>, ProcessStatus) {
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let status = assert_no_alloc(|| {
        let mut io = Io {
            input: None,
            out_l: &mut l,
            out_r: &mut r,
        };
        p.process(&mut io, events)
    });
    (l, status)
}

/// Upward zero crossings per second over some blocks: the pitch of what plays.
fn pitch(p: &mut dyn PluginProcessor, blocks: usize) -> f32 {
    let mut out = Vec::new();
    for _ in 0..blocks {
        out.extend(block(p, &[]).0);
    }
    let ups = out.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    ups as f32 * RATE as f32 / out.len() as f32
}

/// Plays blocks until the pitch is near `hz` (the worker loads in the background).
fn wait_for_pitch(p: &mut dyn PluginProcessor, hz: f32) -> f32 {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let got = pitch(p, 20);
        if (got - hz).abs() < hz * 0.05 || Instant::now() > until {
            return got;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_sfz_folder_plays_and_switches_sounds() {
    let dir = std::env::temp_dir().join(format!("merlin-sfz-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("samples")).unwrap();
    write_sine(&dir.join("samples/low.wav"), 440.0);
    write_sine(&dir.join("samples/high.wav"), 1760.0);
    let region = |s: &str| {
        format!(
            "<region> sample=samples/{s}.wav pitch_keycenter=69 lokey=0 hikey=127 ampeg_release=0.05\n"
        )
    };
    std::fs::write(dir.join("a low.sfz"), region("low")).unwrap();
    std::fs::write(dir.join("b high.sfz"), region("high")).unwrap();
    std::fs::write(dir.join("c broken.sfz"), region("missing")).unwrap();

    let mut plugin = SfzPlugin::open(&dir).unwrap();
    let names: Vec<_> = plugin
        .presets()
        .unwrap()
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(names, ["a low", "b high", "c broken"]);
    let mut proc = plugin.activate(RATE as f64, BLOCK as u32).unwrap();

    let (out, status) = block(proc.as_mut(), &[]);
    assert!(out.iter().all(|&x| x == 0.0));
    assert_eq!(status, ProcessStatus::Sleep, "silent and idle: asleep");

    // A held note, then the other file while it sounds.
    block(proc.as_mut(), &[MidiEvent::note_on(0, 0, 69, 100)]);
    let low = pitch(proc.as_mut(), 20);
    assert!(
        (low - 440.0).abs() < 25.0,
        "plays the first file ({low} Hz)"
    );
    plugin.set_preset(0, 1);
    assert_eq!(plugin.preset(), Some((0, 1)));
    // The held key does not sound on the new synth (it never saw it go down), and the old one
    // releases it: silence follows. Play the key again to hear the new sound.
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let (out, _) = block(proc.as_mut(), &[]);
        if out.iter().all(|&x| x.abs() < 1e-4) || Instant::now() > until {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    block(proc.as_mut(), &[MidiEvent::note_on(0, 0, 69, 100)]);
    let high = wait_for_pitch(proc.as_mut(), 1760.0);
    assert!(
        (high - 1760.0).abs() < 90.0,
        "plays the second file ({high} Hz)"
    );
    block(proc.as_mut(), &[MidiEvent::note_off(0, 0, 69)]);

    // A file that cannot play: the choice goes back to what plays.
    plugin.set_preset(0, 2);
    let until = Instant::now() + Duration::from_secs(10);
    while plugin.preset() != Some((0, 1)) && Instant::now() < until {
        block(proc.as_mut(), &[]);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(plugin.preset(), Some((0, 1)), "a broken file is not kept");

    // A switch while asleep still arrives: the processor stays awake until it has.
    let until = Instant::now() + Duration::from_secs(10);
    while block(proc.as_mut(), &[]).1 != ProcessStatus::Sleep && Instant::now() < until {}
    plugin.set_preset(0, 0);
    let (_, status) = block(proc.as_mut(), &[]);
    assert_eq!(
        status,
        ProcessStatus::Continue,
        "awake while a sound is on its way"
    );
    // Asleep again once it has arrived (nothing plays).
    let until = Instant::now() + Duration::from_secs(10);
    while block(proc.as_mut(), &[]).1 != ProcessStatus::Sleep && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1));
    }
    block(proc.as_mut(), &[MidiEvent::note_on(0, 0, 69, 100)]);
    let low = wait_for_pitch(proc.as_mut(), 440.0);
    assert!(
        (low - 440.0).abs() < 25.0,
        "back to the first file ({low} Hz)"
    );

    // Level.
    let loud = block(proc.as_mut(), &[])
        .0
        .iter()
        .fold(0.0f32, |m, x| m.max(x.abs()));
    plugin.set_gain(0.0);
    let quiet = block(proc.as_mut(), &[])
        .0
        .iter()
        .fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(loud > 0.05 && quiet == 0.0, "level {loud} -> {quiet}");

    drop(proc);
    plugin.deactivate();
    drop(plugin);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_file_without_playable_regions_fails_to_activate() {
    let dir = std::env::temp_dir().join(format!("merlin-sfz-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("empty.sfz");
    std::fs::write(&file, "<region> sample=nowhere.wav\n").unwrap();
    let mut plugin = SfzPlugin::open(&file).unwrap();
    let err = plugin.activate(48_000.0, 256).err().expect("must fail");
    assert!(format!("{err:#}").contains("nothing to play"), "{err:#}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_rhodes_from_just_setup_plays_each_of_its_sounds() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/sfz/jRhodes3c");
    if !dir.is_dir() {
        eprintln!("skipped: run `just setup` for the jRhodes3c SFZ instrument");
        return;
    }
    let mut plugin = SfzPlugin::open(&dir).unwrap();
    let names: Vec<_> = plugin
        .presets()
        .unwrap()
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(
        names,
        [
            "jRhodes-both-looped",
            "jRhodes-mono-looped",
            "jRhodes-stereo-looped"
        ]
    );
    for n in 0..names.len() as u8 {
        plugin.set_preset(0, n);
        let t = Instant::now();
        let mut proc = plugin.activate(RATE as f64, BLOCK as u32).unwrap();
        let loaded = t.elapsed();
        let mut peak = 0.0f32;
        block(proc.as_mut(), &[MidiEvent::note_on(0, 0, 60, 100)]);
        for _ in 0..40 {
            peak = block(proc.as_mut(), &[])
                .0
                .iter()
                .fold(peak, |m, x| m.max(x.abs()));
        }
        eprintln!(
            "{}: loaded in {loaded:?}, peak {peak:.2}",
            names[n as usize]
        );
        assert!(
            peak > 0.01 && peak < 4.0,
            "{} sounds ({peak})",
            names[n as usize]
        );
        drop(proc);
        plugin.deactivate();
    }
}
