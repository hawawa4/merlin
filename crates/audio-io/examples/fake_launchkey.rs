//! A pretend Launchkey MK3 DAW port: sends a script of messages (pads, knobs, buttons) and
//! prints every message the app sends back (DAW mode, LEDs, screen text), for trying the
//! controller layout without the hardware and for the end-to-end test.
//!
//!     cargo run -p merlin-audio-io --example fake_launchkey -- "Launchkey MK3 61 LKMK3 DAW In" "SCRIPT" [seconds]
//!
//! SCRIPT is as for `fake_controller`. Output lines: `recv <seconds> <hex bytes>`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use jack::{Client, ClientOptions, Control, MidiIn, MidiOut, ProcessScope, RawMidi};

fn byte(s: &str) -> u8 {
    match s.strip_prefix("0x") {
        Some(h) => u8::from_str_radix(h, 16).expect("a byte"),
        None if s.len() == 2 && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) => {
            u8::from_str_radix(s, 16).expect("a byte")
        }
        None => s.parse().expect("a byte"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let name = args
        .next()
        .unwrap_or_else(|| "Launchkey MK3 61 LKMK3 DAW In".into());
    let script = args.next().unwrap_or_default();
    let mut events: Vec<(f64, Vec<u8>)> = script
        .split(';')
        .filter_map(|item| {
            let (t, bytes) = item.trim().split_once(':')?;
            Some((
                t.trim().parse().ok()?,
                bytes.split_whitespace().map(byte).collect(),
            ))
        })
        .collect();
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let seconds: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30.0);

    merlin_audio_io::preload_pipewire_jack();
    let (client, _) = Client::new(&name, ClientOptions::NO_START_SERVER)
        .expect("cannot connect to JACK/PipeWire");
    let mut out = client.register_port("out", MidiOut::default()).unwrap();
    let input = client.register_port("in", MidiIn::default()).unwrap();
    let rate = client.sample_rate() as f64;
    let schedule: Vec<(u64, Vec<u8>)> = events
        .into_iter()
        .map(|(t, b)| ((t * rate) as u64, b))
        .collect();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let (mut frame, mut next) = (0u64, 0usize);
    let process = move |_: &Client, ps: &ProcessScope| {
        let n = ps.n_frames() as u64;
        for m in input.iter(ps) {
            let _ = tx.send(m.bytes.to_vec());
        }
        let mut writer = out.writer(ps);
        while let Some((at, bytes)) = schedule.get(next) {
            if *at >= frame + n {
                break;
            }
            let _ = writer.write(&RawMidi {
                time: at.saturating_sub(frame) as u32,
                bytes,
            });
            next += 1;
        }
        frame += n;
        Control::Continue
    };
    let active = client
        .activate_async((), jack::contrib::ClosureProcessHandler::new(process))
        .expect("cannot activate");
    println!("{name} is plugged in for {seconds:.0} s");
    let start = Instant::now();
    while start.elapsed().as_secs_f64() < seconds {
        if let Ok(m) = rx.recv_timeout(Duration::from_millis(20)) {
            let hex: Vec<String> = m.iter().map(|b| format!("{b:02X}")).collect();
            println!(
                "recv {:.2} {}",
                start.elapsed().as_secs_f64(),
                hex.join(" ")
            );
        }
    }
    drop(active);
    println!("{name} unplugged");
}
