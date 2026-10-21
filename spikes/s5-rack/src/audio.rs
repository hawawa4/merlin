//! JACK side: one process callback runs every rack slot, routes MIDI and mixes the outputs.
//!
//! Routing (plan §4.4 in miniature): a note-on goes to every slot that is active, not muted
//! and has the key inside its zone, and *remembers* those slots; the matching note-off goes
//! to the remembered slots no matter how the routing changed in between, so switching or
//! changing zones never leaves a note stuck. Other messages (CC, pitch bend) go to every
//! active slot.
//!
//! Slots are installed and removed through a command queue; a removed processor is handed
//! back to the main thread, so nothing is allocated or freed in the callback.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use anyhow::{Context, Result};
use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_host::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_host::events::{EventFlags, Match};
use clack_host::prelude::*;
use jack::{
    AudioOut, Client, ClientOptions, Control, MidiIn, NotificationHandler, Port, PortFlags,
};

use crate::host::Host;

pub const SLOTS: usize = 8;
const MAX_FRAMES: usize = 4096;
const MAX_MIDI: usize = 512;

/// Same trick as S1: make the jack crate pick up PipeWire's libjack.
pub fn preload_pipewire_jack() {
    for path in [
        "/usr/lib/x86_64-linux-gnu/pipewire-0.3/jack/libjack.so.0",
        "/usr/lib64/pipewire-0.3/jack/libjack.so.0",
    ] {
        let c = std::ffi::CString::new(path).unwrap();
        // SAFETY: loading a library by absolute path; handle intentionally leaked.
        if !unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) }.is_null() {
            return;
        }
    }
}

pub struct Notifications(Arc<AtomicU32>);

impl NotificationHandler for Notifications {
    fn xrun(&mut self, _: &Client) -> Control {
        self.0.fetch_add(1, Ordering::Relaxed);
        Control::Continue
    }
}

/// Who plays what: updated from the UI through `Cmd::Route`.
#[derive(Clone, Copy, Debug)]
pub struct Route {
    pub active: bool,
    pub mute: bool,
    pub lo: u8,
    pub hi: u8,
}

impl Route {
    pub const DEFAULT: Route = Route {
        active: false,
        mute: false,
        lo: 0,
        hi: 127,
    };

    fn takes(&self, key: u8) -> bool {
        self.active && !self.mute && (self.lo..=self.hi).contains(&key)
    }
}

pub enum Cmd {
    Install(usize, Box<SlotAudio>),
    Remove(usize),
    Route(usize, Route),
}

/// A plugin's processor plus its preallocated buffers.
pub struct SlotAudio {
    pub processor: StartedPluginAudioProcessor<Host>,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    in_channels: Vec<u32>,
    out_channels: Vec<u32>,
    in_bufs: Vec<Vec<f32>>,
    out_bufs: Vec<Vec<f32>>,
    events: EventBuffer,
    steady: u64,
}

fn port_channels(instance: &mut PluginInstance<Host>, is_input: bool) -> Vec<u32> {
    let handle = instance.plugin_handle();
    let Some(ext) = handle.get_extension::<PluginAudioPorts>() else {
        return if is_input { vec![] } else { vec![2] };
    };
    let mut buf = AudioPortInfoBuffer::new();
    (0..ext.count(&handle, is_input))
        .filter_map(|i| {
            ext.get(&handle, i, is_input, &mut buf)
                .map(|p| p.channel_count)
        })
        .collect()
}

impl SlotAudio {
    /// Activates the plugin and starts processing (main thread).
    pub fn new(instance: &mut PluginInstance<Host>, sample_rate: f64) -> Result<Box<Self>> {
        let in_channels = port_channels(instance, true);
        let out_channels = port_channels(instance, false);
        let config = PluginAudioConfiguration {
            sample_rate,
            min_frames_count: 1,
            max_frames_count: MAX_FRAMES as u32,
        };
        let processor = instance
            .activate(|_, _| (), config)?
            .start_processing()
            .map_err(|e| anyhow::anyhow!("start_processing: {e}"))?;
        let total = |c: &[u32]| c.iter().sum::<u32>() as usize;
        Ok(Box::new(Self {
            processor,
            in_ports: AudioPorts::with_capacity(total(&in_channels), in_channels.len()),
            out_ports: AudioPorts::with_capacity(total(&out_channels), out_channels.len()),
            in_bufs: in_channels
                .iter()
                .map(|&c| vec![0.0; c as usize * MAX_FRAMES])
                .collect(),
            out_bufs: out_channels
                .iter()
                .map(|&c| vec![0.0; c as usize * MAX_FRAMES])
                .collect(),
            in_channels,
            out_channels,
            events: EventBuffer::with_capacity(512),
            steady: 0,
        }))
    }

    /// Runs the plugin for `n` frames; its main output ends up in `out_bufs[0]`.
    fn process(&mut self, n: usize) -> bool {
        for b in &mut self.in_bufs {
            b.fill(0.0);
        }
        let ins =
            self.in_ports
                .with_input_buffers(self.in_bufs.iter_mut().zip(&self.in_channels).map(
                    |(buf, _)| AudioPortBuffer {
                        latency: 0,
                        channels:
                            AudioPortBufferType::f32_input_only(
                                buf.as_chunks_mut::<MAX_FRAMES>().0.iter_mut().map(|c| {
                                    InputChannel {
                                        buffer: &mut c[..n],
                                        is_constant: true,
                                    }
                                }),
                            ),
                    },
                ));
        let mut outs = self.out_ports.with_output_buffers(
            self.out_bufs
                .iter_mut()
                .zip(&self.out_channels)
                .map(|(buf, _)| AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        buf.as_chunks_mut::<MAX_FRAMES>()
                            .0
                            .iter_mut()
                            .map(|c| &mut c[..n]),
                    ),
                }),
        );
        let ok = self
            .processor
            .process(
                &ins,
                &mut outs,
                &self.events.as_input(),
                &mut OutputEvents::void(),
                Some(self.steady),
                None,
            )
            .is_ok();
        self.steady += n as u64;
        ok
    }
}

#[derive(Clone, Copy)]
struct MidiMsg {
    time: u32,
    len: u8,
    bytes: [u8; 3],
}

fn push_midi(events: &mut EventBuffer, m: &MidiMsg) {
    let b = m.bytes;
    match m.len {
        3 if b[0] & 0xF0 == 0x90 && b[2] > 0 => events.push(
            &NoteOnEvent::new(
                m.time,
                Pckn::new(0u16, b[0] & 0x0F, b[1] as u16, Match::All),
                b[2] as f64 / 127.0,
            )
            .with_flags(EventFlags::IS_LIVE),
        ),
        3 if b[0] & 0xF0 == 0x80 || b[0] & 0xF0 == 0x90 => events.push(
            &NoteOffEvent::new(
                m.time,
                Pckn::new(0u16, b[0] & 0x0F, b[1] as u16, Match::All),
                b[2] as f64 / 127.0,
            )
            .with_flags(EventFlags::IS_LIVE),
        ),
        2 | 3 => events.push(&MidiEvent::new(m.time, 0, b).with_flags(EventFlags::IS_LIVE)),
        _ => {}
    }
}

/// Live state the UI reads: which slots own each held key, and per-slot output peaks.
pub struct Shared {
    pub owners: [AtomicU8; 128],
    /// Peak since the UI last looked, as f32 bits (the UI resets it with `swap(0)`).
    pub peaks: [AtomicU32; SLOTS],
}

pub struct Engine {
    midi_in: Port<MidiIn>,
    out_l: Port<AudioOut>,
    out_r: Port<AudioOut>,
    slots: [Option<Box<SlotAudio>>; SLOTS],
    routes: [Route; SLOTS],
    cmds: rtrb::Consumer<Cmd>,
    ui_notes: rtrb::Consumer<[u8; 3]>,
    returned: rtrb::Producer<(usize, Box<SlotAudio>)>,
    shared: Arc<Shared>,
    midi: Vec<MidiMsg>,
    mix_l: Vec<f32>,
    mix_r: Vec<f32>,
}

impl Engine {
    fn route_midi(&mut self, m: MidiMsg) {
        let mut mask = 0u8;
        let status = m.bytes[0] & 0xF0;
        let key = (m.bytes[1] & 0x7F) as usize;
        if m.len == 3 && status == 0x90 && m.bytes[2] > 0 {
            for (i, r) in self.routes.iter().enumerate() {
                if r.takes(key as u8) && self.slots[i].is_some() {
                    mask |= 1 << i;
                }
            }
            self.shared.owners[key].fetch_or(mask, Ordering::Relaxed);
        } else if m.len == 3 && (status == 0x80 || status == 0x90) {
            mask = self.shared.owners[key].swap(0, Ordering::Relaxed);
        } else {
            for (i, r) in self.routes.iter().enumerate() {
                if r.active && !r.mute && self.slots[i].is_some() {
                    mask |= 1 << i;
                }
            }
        }
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let (true, Some(s)) = (mask & (1 << i) != 0, slot) {
                push_midi(&mut s.events, &m);
            }
        }
    }
}

impl jack::ProcessHandler for Engine {
    fn process(&mut self, _: &Client, ps: &jack::ProcessScope) -> Control {
        let n = (ps.n_frames() as usize).min(MAX_FRAMES);

        while let Ok(cmd) = self.cmds.pop() {
            match cmd {
                Cmd::Install(i, slot) => {
                    if let Some(old) = self.slots[i].replace(slot) {
                        let _ = self.returned.push((i, old));
                    }
                }
                Cmd::Remove(i) => {
                    if let Some(old) = self.slots[i].take() {
                        let _ = self.returned.push((i, old));
                    }
                }
                Cmd::Route(i, r) => self.routes[i] = r,
            }
        }

        self.midi.clear();
        for ev in self.midi_in.iter(ps) {
            let mut bytes = [0u8; 3];
            let len = ev.bytes.len().min(3);
            bytes[..len].copy_from_slice(&ev.bytes[..len]);
            if self.midi.len() < MAX_MIDI {
                self.midi.push(MidiMsg {
                    time: ev.time,
                    len: len as u8,
                    bytes,
                });
            }
        }
        while let Ok(bytes) = self.ui_notes.pop() {
            if self.midi.len() < MAX_MIDI {
                self.midi.push(MidiMsg {
                    time: 0,
                    len: 3,
                    bytes,
                });
            }
        }

        for s in self.slots.iter_mut().flatten() {
            s.events.clear();
        }
        for k in 0..self.midi.len() {
            let m = self.midi[k];
            self.route_midi(m);
        }

        self.mix_l[..n].fill(0.0);
        self.mix_r[..n].fill(0.0);
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let Some(s) = slot else { continue };
            let silent = !s.process(n);
            let (Some(main), Some(&ch)) = (s.out_bufs.first(), s.out_channels.first()) else {
                continue;
            };
            if silent {
                continue;
            }
            let left = &main[..n];
            let right = if ch >= 2 {
                &main[MAX_FRAMES..MAX_FRAMES + n]
            } else {
                left
            };
            let mut peak = 0f32;
            for k in 0..n {
                peak = peak.max(left[k].abs()).max(right[k].abs());
            }
            self.shared.peaks[i].fetch_max(peak.to_bits(), Ordering::Relaxed);
            if !self.routes[i].mute {
                for k in 0..n {
                    self.mix_l[k] += left[k];
                    self.mix_r[k] += right[k];
                }
            }
        }
        self.out_l.as_mut_slice(ps)[..n].copy_from_slice(&self.mix_l[..n]);
        self.out_r.as_mut_slice(ps)[..n].copy_from_slice(&self.mix_r[..n]);
        Control::Continue
    }
}

/// Main-thread handle to the running engine.
pub struct Audio {
    client: jack::AsyncClient<Notifications, Engine>,
    pub xruns: Arc<AtomicU32>,
    pub sample_rate: u32,
    pub shared: Arc<Shared>,
    cmds: rtrb::Producer<Cmd>,
    returned: rtrb::Consumer<(usize, Box<SlotAudio>)>,
    pub ui_notes: rtrb::Producer<[u8; 3]>,
}

pub fn start() -> Result<Audio> {
    preload_pipewire_jack();
    // SAFETY: called before any other thread reads the environment.
    unsafe { std::env::set_var("PIPEWIRE_QUANTUM", "128/48000") };
    let (client, _) =
        Client::new("merlin-s5", ClientOptions::NO_START_SERVER).context("JACK client")?;
    let sample_rate = client.sample_rate();

    let (cmds, cmds_rx) = rtrb::RingBuffer::new(64);
    let (ui_notes, ui_rx) = rtrb::RingBuffer::new(256);
    let (returned_tx, returned) = rtrb::RingBuffer::new(32);
    let shared = Arc::new(Shared {
        owners: std::array::from_fn(|_| AtomicU8::new(0)),
        peaks: std::array::from_fn(|_| AtomicU32::new(0)),
    });
    let engine = Engine {
        midi_in: client.register_port("midi_in", MidiIn::default())?,
        out_l: client.register_port("out_l", AudioOut::default())?,
        out_r: client.register_port("out_r", AudioOut::default())?,
        slots: std::array::from_fn(|_| None),
        routes: [Route::DEFAULT; SLOTS],
        cmds: cmds_rx,
        ui_notes: ui_rx,
        returned: returned_tx,
        shared: shared.clone(),
        midi: Vec::with_capacity(MAX_MIDI),
        mix_l: vec![0.0; MAX_FRAMES],
        mix_r: vec![0.0; MAX_FRAMES],
    };
    let names = [
        engine.out_l.name()?,
        engine.out_r.name()?,
        engine.midi_in.name()?,
    ];

    let xruns = Arc::new(AtomicU32::new(0));
    let active = client.activate_async(Notifications(xruns.clone()), engine)?;
    let c = active.as_client();
    let playback = c.ports(
        None,
        Some("32 bit float mono audio"),
        PortFlags::IS_PHYSICAL | PortFlags::IS_INPUT,
    );
    for (src, dst) in names[..2].iter().zip(&playback) {
        let _ = c.connect_ports_by_name(src, dst);
    }
    for src in c.ports(None, Some("8 bit raw midi"), PortFlags::IS_OUTPUT) {
        if !src.starts_with("merlin") {
            let _ = c.connect_ports_by_name(&src, &names[2]);
        }
    }
    Ok(Audio {
        client: active,
        xruns,
        sample_rate,
        shared,
        cmds,
        returned,
        ui_notes,
    })
}

impl Audio {
    pub fn buffer_size(&self) -> u32 {
        self.client.as_client().buffer_size()
    }

    pub fn cpu_load(&self) -> f32 {
        self.client.as_client().cpu_load()
    }

    pub fn send(&mut self, cmd: Cmd) {
        if self.cmds.push(cmd).is_err() {
            eprintln!("audio command queue full");
        }
    }

    /// Takes a slot out of the engine and waits (briefly) for its processor to come back.
    pub fn remove(&mut self, slot: usize) -> Option<Box<SlotAudio>> {
        self.send(Cmd::Remove(slot));
        for _ in 0..500 {
            while let Ok((i, s)) = self.returned.pop() {
                if i == slot {
                    return Some(s);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        None
    }

    /// Stops JACK; returns every processor still installed.
    pub fn stop(self) -> Vec<(usize, Box<SlotAudio>)> {
        let mut out = Vec::new();
        if let Ok((_, _, engine)) = self.client.deactivate() {
            for (i, s) in engine.slots.into_iter().enumerate() {
                if let Some(s) = s {
                    out.push((i, s));
                }
            }
        }
        out
    }
}
