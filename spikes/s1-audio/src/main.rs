//! S1: JACK (via PipeWire's libjack) MIDI in -> polyphonic sine -> stereo out.
//!
//! Answers: does the `jack` crate load PipeWire's libjack without `pw-jack`, do we get
//! realtime scheduling, what latency do we see, and can the process callback stay
//! allocation-free.
//!
//! Usage: `cargo run -p s1-audio --release -- [--demo] [--seconds N]`
//! `--demo` plays an internal arpeggio so the spike can be checked without a keyboard.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use assert_no_alloc::assert_no_alloc;
use jack::{
    AudioOut, Client, ClientOptions, Control, LatencyType, MidiIn, NotificationHandler, PortFlags,
    ProcessScope,
};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const VOICES: usize = 32;

#[derive(Clone, Copy, Default)]
struct Voice {
    note: u8,
    active: bool,
    released: bool,
    phase: f32,
    step: f32,
    env: f32,
    vel: f32,
}

struct Synth {
    voices: [Voice; VOICES],
    sr: f32,
    sustain: bool,
}

impl Synth {
    fn note_on(&mut self, note: u8, vel: u8) {
        let slot = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_else(|| {
                // Steal the quietest voice.
                (0..VOICES)
                    .min_by(|&a, &b| self.voices[a].env.total_cmp(&self.voices[b].env))
                    .unwrap()
            });
        let freq = 440.0 * 2f32.powf((note as f32 - 69.0) / 12.0);
        self.voices[slot] = Voice {
            note,
            active: true,
            released: false,
            phase: 0.0,
            step: freq / self.sr,
            env: 0.0,
            vel: vel as f32 / 127.0,
        };
    }

    fn note_off(&mut self, note: u8) {
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.active && v.note == note)
        {
            v.released = true;
        }
    }

    fn midi(&mut self, bytes: &[u8]) {
        match bytes {
            [s, n, v] if s & 0xF0 == 0x90 && *v > 0 => self.note_on(*n, *v),
            [s, n, _] if s & 0xF0 == 0x80 || s & 0xF0 == 0x90 => self.note_off(*n),
            [s, 64, v] if s & 0xF0 == 0xB0 => self.sustain = *v >= 64,
            [s, 120 | 123, _] if s & 0xF0 == 0xB0 => {
                for v in &mut self.voices {
                    v.released = true;
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        l.fill(0.0);
        r.fill(0.0);
        let attack = 1.0 / (0.005 * self.sr);
        let release = 1.0 - 1.0 / (0.15 * self.sr);
        for v in self.voices.iter_mut().filter(|v| v.active) {
            for (sl, sr) in l.iter_mut().zip(r.iter_mut()) {
                if v.released && !self.sustain {
                    v.env *= release;
                    if v.env < 1e-4 {
                        v.active = false;
                        break;
                    }
                } else if v.env < 1.0 {
                    v.env = (v.env + attack).min(1.0);
                }
                let s = (v.phase * std::f32::consts::TAU).sin() * v.env * v.vel * 0.15;
                v.phase = (v.phase + v.step).fract();
                *sl += s;
                *sr += s;
            }
        }
    }
}

#[derive(Default)]
struct Stats {
    xruns: AtomicU32,
    cycles: AtomicU64,
    max_cycle_ns: AtomicU64,
    rt_policy: AtomicU32,
    rt_checked: AtomicBool,
}

struct Notifications(Arc<Stats>);

impl NotificationHandler for Notifications {
    fn xrun(&mut self, _: &Client) -> Control {
        self.0.xruns.fetch_add(1, Ordering::Relaxed);
        Control::Continue
    }
}

/// Distros that ship jackd2's libjack as the system `libjack.so.0` (Debian, Ubuntu, PikaOS)
/// put PipeWire's copy in a private directory, normally reached through `pw-jack`. Load it
/// by full path first: glibc then hands the jack crate's later `dlopen("libjack.so.0")`
/// the already-loaded PipeWire copy, because it matches on soname.
fn preload_pipewire_jack() -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        "/usr/lib/x86_64-linux-gnu/pipewire-0.3/jack/libjack.so.0",
        "/usr/lib64/pipewire-0.3/jack/libjack.so.0",
        "/usr/lib/pipewire-0.3/jack/libjack.so.0",
    ];
    if std::env::var_os("MERLIN_SYSTEM_JACK").is_some() {
        return None;
    }
    CANDIDATES.iter().copied().find(|path| {
        let c = std::ffi::CString::new(*path).unwrap();
        // SAFETY: loading a shared library by absolute path; the handle is leaked on purpose.
        !unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) }.is_null()
    })
}

fn main() -> Result<()> {
    match preload_pipewire_jack() {
        Some(path) => println!("preloaded {path}"),
        None => println!("using the system libjack.so.0"),
    }
    let args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|a| a == "--demo");
    let seconds: u64 = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let frames: u32 = args
        .iter()
        .position(|a| a == "--frames")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);
    // PipeWire's libjack reads this when the client opens: it asks the graph for this quantum
    // (the graph runs at the smallest quantum any client requests, within the server's limits).
    // SAFETY: still single-threaded here.
    unsafe { std::env::set_var("PIPEWIRE_QUANTUM", format!("{frames}/48000")) };

    let t0 = Instant::now();
    let (client, status) = Client::new("merlin-s1", ClientOptions::NO_START_SERVER)
        .context("could not open a JACK client (is PipeWire's JACK layer installed?)")?;
    println!("client opened in {:?}, status {:?}", t0.elapsed(), status);
    println!(
        "sample rate {} Hz, buffer {} frames ({:.2} ms)",
        client.sample_rate(),
        client.buffer_size(),
        client.buffer_size() as f64 * 1000.0 / client.sample_rate() as f64
    );

    let midi_in = client.register_port("midi_in", MidiIn::default())?;
    let mut out_l = client.register_port("out_l", AudioOut::default())?;
    let mut out_r = client.register_port("out_r", AudioOut::default())?;
    let out_l_name = out_l.name()?;
    let out_r_name = out_r.name()?;
    let midi_in_name = midi_in.name()?;

    let stats = Arc::new(Stats::default());
    let st = stats.clone();
    let mut synth = Synth {
        voices: [Voice::default(); VOICES],
        sr: client.sample_rate() as f32,
        sustain: false,
    };
    // Demo arpeggio: C major 7 up and down, one note per 150 ms.
    let demo_notes = [60u8, 64, 67, 71, 72, 71, 67, 64];
    let demo_period = (client.sample_rate() as f32 * 0.15) as u64;
    let mut frame_clock: u64 = 0;
    let mut demo_idx = 0usize;

    let process = jack::contrib::ClosureProcessHandler::new(
        move |_: &Client, ps: &ProcessScope| -> Control {
            let start = Instant::now();
            if !st.rt_checked.swap(true, Ordering::Relaxed) {
                // SAFETY: plain syscall on the calling (JACK process) thread.
                let policy = unsafe { libc::sched_getscheduler(0) };
                st.rt_policy.store(policy as u32, Ordering::Relaxed);
            }
            assert_no_alloc(|| {
                for ev in midi_in.iter(ps) {
                    synth.midi(ev.bytes);
                }
                let n = ps.n_frames() as u64;
                if demo && (frame_clock + n) / demo_period != frame_clock / demo_period {
                    let prev = demo_notes[demo_idx % demo_notes.len()];
                    demo_idx += 1;
                    synth.note_off(prev);
                    synth.note_on(demo_notes[demo_idx % demo_notes.len()], 100);
                }
                frame_clock += n;
                synth.render(out_l.as_mut_slice(ps), out_r.as_mut_slice(ps));
            });
            let ns = start.elapsed().as_nanos() as u64;
            st.max_cycle_ns.fetch_max(ns, Ordering::Relaxed);
            st.cycles.fetch_add(1, Ordering::Relaxed);
            Control::Continue
        },
    );

    let active = client.activate_async(Notifications(stats.clone()), process)?;
    let c = active.as_client();

    // Auto-connect: first two physical playback ports, every physical MIDI source.
    let playback = c.ports(
        None,
        Some("32 bit float mono audio"),
        PortFlags::IS_PHYSICAL | PortFlags::IS_INPUT,
    );
    for (src, dst) in [&out_l_name, &out_r_name].into_iter().zip(&playback) {
        match c.connect_ports_by_name(src, dst) {
            Ok(()) => println!("connected {src} -> {dst}"),
            Err(e) => println!("could not connect {src} -> {dst}: {e}"),
        }
    }
    let midi_sources = c.ports(None, Some("8 bit raw midi"), PortFlags::IS_OUTPUT);
    for src in midi_sources
        .iter()
        .filter(|p| !p.starts_with("merlin"))
    {
        match c.connect_ports_by_name(src, &midi_in_name) {
            Ok(()) => println!("connected {src} -> midi_in"),
            Err(e) => println!("could not connect {src}: {e}"),
        }
    }
    if midi_sources.is_empty() {
        println!("no MIDI sources found");
    }

    std::thread::sleep(Duration::from_millis(500));
    let raw_policy = stats.rt_policy.load(Ordering::Relaxed) as i32;
    let policy = raw_policy & !libc::SCHED_RESET_ON_FORK;
    let policy_name = match policy {
        libc::SCHED_FIFO => "SCHED_FIFO",
        libc::SCHED_RR => "SCHED_RR",
        libc::SCHED_OTHER => "SCHED_OTHER (not realtime!)",
        _ => "other",
    };
    println!("process thread policy: {policy_name} (raw {raw_policy:#x})");
    println!("buffer after activation: {} frames", c.buffer_size());

    if let Some(p) = c.port_by_name(&out_l_name) {
        let (lo, hi) = p.get_latency_range(LatencyType::Playback);
        println!(
            "playback latency of out_l: {lo}..{hi} frames ({:.2} ms)",
            hi as f64 * 1000.0 / c.sample_rate() as f64
        );
    }
    if let Some(p) = c.port_by_name(&midi_in_name) {
        let (lo, hi) = p.get_latency_range(LatencyType::Capture);
        println!("capture latency of midi_in: {lo}..{hi} frames");
    }

    println!(
        "running for {seconds} s{}",
        if demo { " (demo arpeggio)" } else { "" }
    );
    for _ in 0..seconds {
        std::thread::sleep(Duration::from_secs(1));
        let cycles = stats.cycles.load(Ordering::Relaxed);
        let max_us = stats.max_cycle_ns.swap(0, Ordering::Relaxed) as f64 / 1000.0;
        println!(
            "cycles {cycles:>7}  xruns {:>3}  dsp load {:>5.1}%  max cycle {:>7.1} us",
            stats.xruns.load(Ordering::Relaxed),
            c.cpu_load(),
            max_us
        );
    }
    active.deactivate()?;
    Ok(())
}
