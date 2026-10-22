//! A pretend MIDI controller that sends a script of messages, for trying mappings without
//! hardware (and for the automated tests of the control layer).
//!
//!     cargo run -p merlin-audio-io --example fake_controller -- "Name" "SCRIPT" [seconds]
//!
//! SCRIPT is `time:status data1 data2` items separated by `;`, times in seconds after the
//! controller plugs in, bytes in hex or decimal: `2:B0 14 127; 3:B0 14 0; 4:C0 1; 5:90 36 100`.
//! It leaves after `seconds` (default: the last message plus two seconds).

use std::time::Duration;

use jack::{Client, ClientOptions, Control, MidiOut, ProcessScope, RawMidi};

fn byte(s: &str) -> u8 {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u8::from_str_radix(h, 16).expect("a byte"),
        // Two hex digits starting with A-F (`B0`, `C0`) are hex; plain digits are decimal.
        None if s.len() == 2 && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) => {
            u8::from_str_radix(s, 16).expect("a byte")
        }
        None => s.parse().expect("a byte"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let name = args.next().unwrap_or_else(|| "Fake Controller".into());
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
    let last = events.last().map_or(0.0, |e| e.0);
    let seconds: f64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(last + 2.0);

    merlin_audio_io::preload_pipewire_jack();
    let (client, _) = Client::new(&name, ClientOptions::NO_START_SERVER)
        .expect("cannot connect to JACK/PipeWire");
    let mut port = client
        .register_port("out", MidiOut::default())
        .expect("cannot register the MIDI port");
    let rate = client.sample_rate() as f64;
    let schedule: Vec<(u64, Vec<u8>)> = events
        .into_iter()
        .map(|(t, b)| ((t * rate) as u64, b))
        .collect();
    let (mut frame, mut next) = (0u64, 0usize);
    let process = move |_: &Client, ps: &ProcessScope| {
        let n = ps.n_frames() as u64;
        let mut writer = port.writer(ps);
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
    std::thread::sleep(Duration::from_secs_f64(seconds));
    drop(active);
    println!("{name} unplugged");
}
