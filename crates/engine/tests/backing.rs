//! Backing tracks through the engine, offline: a stem starts exactly where the transport's run
//! starts, goes where its gain and output say, and stops with the transport.

use std::path::PathBuf;
use std::time::Duration;

use merlin_engine::{Command, Stem, new};

const SR: u32 = 48_000;

/// A 16-bit stereo WAV of a constant level (easy to recognise in the mix).
fn write_constant_wav(name: &str, level: f32, secs: f32) -> PathBuf {
    let frames = (SR as f32 * secs) as usize;
    let sample = (level * 32767.0) as i16;
    let mut data = Vec::new();
    for _ in 0..frames {
        data.extend_from_slice(&sample.to_le_bytes());
        data.extend_from_slice(&sample.to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&SR.to_le_bytes());
    out.extend_from_slice(&(SR * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    let dir = std::env::temp_dir().join(format!("irig-engine-backing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, out).unwrap();
    path
}

fn render(engine: &mut merlin_engine::Engine, blocks: usize, frames: usize) -> Vec<f32> {
    let mut all = Vec::new();
    for _ in 0..blocks {
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        engine.process(&[], &mut l, &mut r);
        all.extend_from_slice(&l);
    }
    all
}

fn wait_ready(control: &merlin_backing::StemControl) {
    for _ in 0..500 {
        if control.ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the stem never became ready");
}

#[test]
fn a_stem_starts_with_the_run_and_stops_with_it() {
    let path = write_constant_wav("level.wav", 0.25, 2.0);
    let (mut engine, mut handle) = new(SR as f32, 256);
    let (reader, control) = merlin_backing::open(&path, SR).unwrap();
    wait_ready(&control);
    handle
        .send(Command::AddStem(Stem::new(reader, 1.0, false, false)))
        .ok()
        .unwrap();
    // Click off, so what we hear is only the stem. 120 bpm, 1 beat of count-in = 0.5 s.
    handle.shared.transport.set_click(false, 0.0);
    handle.shared.transport.set_tempo(120.0);
    handle.shared.transport.start(1);
    // Master volume 1 and the limiter at -0.3 dB leave 0.25 alone.
    let audio = render(&mut engine, 375, 256); // 2 s
    let first = audio
        .iter()
        .position(|&x| x.abs() > 0.1)
        .expect("the stem never played");
    // The count-in is 24000 frames; the run starts on the beat after it. (The master limiter's
    // look-ahead delays everything the main mix carries by a millisecond or two.)
    assert!(
        (24_000..24_200).contains(&first),
        "the stem started at frame {first}"
    );
    assert!((audio[first + 1000] - 0.25).abs() < 0.01);
    assert!(
        handle
            .shared
            .stems_playing
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    // Stopping silences it at once.
    handle.shared.transport.stop();
    let after = render(&mut engine, 4, 256);
    assert!(
        after[512..].iter().all(|&x| x.abs() < 1e-6),
        "still sounding after stop"
    );
    // Clearing hands the stem back to be freed off the audio thread.
    handle.send(Command::ClearStems).ok().unwrap();
    render(&mut engine, 1, 256);
    assert_eq!(handle.take_retired_stems().len(), 1);
}

#[test]
fn gain_and_mute_apply_and_a_cue_stem_stays_out_of_the_main_mix() {
    let path = write_constant_wav("level2.wav", 0.4, 2.0);
    let (mut engine, mut handle) = new(SR as f32, 256);
    let (reader, control) = merlin_backing::open(&path, SR).unwrap();
    wait_ready(&control);
    handle
        .send(Command::AddStem(Stem::new(reader, 0.5, false, false)))
        .ok()
        .unwrap();
    handle.shared.transport.set_click(false, 0.0);
    handle.shared.transport.start(0);
    let audio = render(&mut engine, 40, 256);
    assert!(
        (audio[5000] - 0.2).abs() < 0.01,
        "gain 0.5 of 0.4: {}",
        audio[5000]
    );
    // Muted: silent (after the ramp).
    handle
        .send(Command::SetStemMix {
            index: 0,
            gain: 0.5,
            mute: true,
            cue: false,
        })
        .ok()
        .unwrap();
    let audio = render(&mut engine, 8, 256);
    assert!(audio[audio.len() - 100..].iter().all(|&x| x.abs() < 1e-4));
    // Cue: unmuted again but on the cue bus. With the click not in the main mix it is absent
    // from the main output and present in the click output.
    handle.shared.transport.set_click_to_main(false);
    handle
        .send(Command::SetStemMix {
            index: 0,
            gain: 0.5,
            mute: false,
            cue: true,
        })
        .ok()
        .unwrap();
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    let (mut cl, mut cr) = (vec![0.0; 256], vec![0.0; 256]);
    let mut clock = Vec::with_capacity(512);
    for _ in 0..8 {
        clock.clear();
        let mut click = merlin_engine::ClickOut {
            l: &mut cl,
            r: &mut cr,
            clock: &mut clock,
        };
        engine.process_with_click(&[], &mut l, &mut r, Some(&mut click));
    }
    assert!(
        l.iter().all(|&x| x.abs() < 1e-4),
        "cue stem leaked into the main mix"
    );
    assert!((cl[100] - 0.2).abs() < 0.02, "cue output: {}", cl[100]);
}
