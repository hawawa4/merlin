//! A pretend MIDI keyboard for trying the MIDI settings without hardware.
//!
//!     cargo run -p merlin-audio-io --example fake_keyboard -- ["Name"] [seconds]
//!
//! It shows up as a JACK MIDI source (the way a USB keyboard does through PipeWire), plays a
//! slow arpeggio on channel 1 and leaves after `seconds` (default 60), like unplugging it.

use std::time::Duration;

use jack::{Client, ClientOptions, Control, MidiOut, ProcessScope, RawMidi};

fn main() {
    let mut args = std::env::args().skip(1);
    let name = args.next().unwrap_or_else(|| "Fake Keyboard".into());
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);

    merlin_audio_io::preload_pipewire_jack();
    let (client, _) = Client::new(&name, ClientOptions::NO_START_SERVER)
        .expect("cannot connect to JACK/PipeWire");
    let mut port = client
        .register_port("out", MidiOut::default())
        .expect("cannot register the MIDI port");
    let rate = client.sample_rate() as u64;

    // One note every 300 ms, each held for 200 ms, walking up a C major scale and back.
    let scale = [60u8, 62, 64, 65, 67, 69, 71, 72, 71, 69, 67, 65, 64, 62];
    let mut frame: u64 = 0;
    let step = rate * 3 / 10;
    let hold = rate / 5;
    let process = move |_: &Client, ps: &ProcessScope| {
        let n = ps.n_frames() as u64;
        let mut writer = port.writer(ps);
        for i in 0..n {
            let t = frame + i;
            let key = scale[((t / step) as usize) % scale.len()];
            let at = (i) as u32;
            if t.is_multiple_of(step) {
                let _ = writer.write(&RawMidi {
                    time: at,
                    bytes: &[0x90, key, 100],
                });
            } else if t % step == hold {
                let _ = writer.write(&RawMidi {
                    time: at,
                    bytes: &[0x80, key, 0],
                });
            }
        }
        frame += n;
        Control::Continue
    };
    let active = client
        .activate_async((), jack::contrib::ClosureProcessHandler::new(process))
        .expect("cannot activate");
    println!("{name} is plugged in for {seconds} s");
    std::thread::sleep(Duration::from_secs(seconds));
    drop(active);
    println!("{name} unplugged");
}
