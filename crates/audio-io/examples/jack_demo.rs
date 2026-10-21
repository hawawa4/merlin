//! Runs the engine on JACK/PipeWire: two sine slots split at middle C, an arpeggio from the
//! virtual keyboard, and any hardware MIDI keyboard connected automatically.
//!
//! `cargo run -p merlin-audio-io --release --example jack_demo -- [--frames 128] [--seconds 10] [--quiet]`

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use merlin_audio_io::{JackClient, lock_memory};
use merlin_engine::{Command, Filter, SelectMode};
use merlin_hosting::MidiEvent;
use merlin_hosting::builtin::SineSynth;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() -> Result<()> {
    let frames: u32 = arg("--frames").and_then(|v| v.parse().ok()).unwrap_or(128);
    let seconds: u64 = arg("--seconds").and_then(|v| v.parse().ok()).unwrap_or(10);
    let quiet = std::env::args().any(|a| a == "--quiet");

    println!("memory lock: {:?}", lock_memory());
    let client = JackClient::open("merlin-demo", Some(frames))?;
    println!(
        "JACK: {} Hz, {} frames",
        client.sample_rate, client.buffer_size
    );
    let sr = client.sample_rate as f32;

    let (engine, mut handle) = merlin_engine::new(sr, 4096);
    for slot in 0..2 {
        handle
            .send(Command::SetProcessor {
                slot,
                processor: Box::new(SineSynth::new(sr)),
            })
            .ok();
    }
    // Slot 0 plays below middle C, slot 1 an octave up from there; both are targets.
    handle
        .send(Command::SetFilter {
            slot: 0,
            filter: Filter {
                key_hi: 59,
                ..Filter::ALL
            },
        })
        .ok();
    handle
        .send(Command::SetFilter {
            slot: 1,
            filter: Filter {
                key_lo: 60,
                transpose: 12,
                ..Filter::ALL
            },
        })
        .ok();
    handle
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    handle
        .send(Command::Select {
            slot: 1,
            mode: SelectMode::Layer,
        })
        .ok();
    if quiet {
        handle.send(Command::SetMasterVolume(0.0)).ok();
    }

    let mut backend = client.start(engine)?;
    println!("outputs -> {:?}", backend.connect_outputs());
    for s in backend.connect_midi_sources() {
        println!("MIDI input {} <- {}", s.input, s.device);
    }

    let start = Instant::now();
    let notes = [48u8, 55, 60, 64, 67, 72, 76, 79];
    let mut step = 0usize;
    let mut next_note = Instant::now();
    let mut next_report = Instant::now() + Duration::from_secs(1);
    let mut on: Option<u8> = None;
    while start.elapsed() < Duration::from_secs(seconds) {
        let now = Instant::now();
        if now >= next_note {
            if let Some(n) = on.take() {
                let _ = backend.virtual_midi.push(MidiEvent::note_off(0, 0, n));
            }
            let n = notes[step % notes.len()];
            let _ = backend.virtual_midi.push(MidiEvent::note_on(0, 0, n, 100));
            on = Some(n);
            step += 1;
            next_note = now + Duration::from_millis(250);
        }
        if now >= next_report {
            next_report += Duration::from_secs(1);
            let s = &backend.stats;
            let peaks: Vec<String> = (0..2)
                .map(|i| {
                    format!(
                        "{:.2}",
                        f32::from_bits(handle.shared.peaks[i].swap(0, Ordering::Relaxed))
                    )
                })
                .collect();
            println!(
                "buffer {} · DSP {:.1}% · xruns {} · callback last {} µs, max {} µs · peaks {} · held {:?}",
                backend.buffer_size(),
                backend.cpu_load(),
                s.xruns.load(Ordering::Relaxed),
                s.last_ns.load(Ordering::Relaxed) / 1000,
                s.max_ns.swap(0, Ordering::Relaxed) / 1000,
                peaks.join("/"),
                (0..128)
                    .filter(|&k| handle.shared.held[k].load(Ordering::Relaxed) != 0)
                    .collect::<Vec<_>>(),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if let Some(n) = on {
        let _ = backend.virtual_midi.push(MidiEvent::note_off(0, 0, n));
    }
    std::thread::sleep(Duration::from_millis(100));
    let xruns = backend.stats.xruns.load(Ordering::Relaxed);
    backend.stop()?;
    println!("done, {xruns} xruns");
    Ok(())
}
