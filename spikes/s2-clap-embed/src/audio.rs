//! JACK side: runs the plugin's audio processor in the JACK process callback.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_host::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent};
use clack_host::events::{EventFlags, Match};
use clack_host::prelude::*;
use jack::{
    AudioOut, Client, ClientOptions, Control, MidiIn, NotificationHandler, Port, PortFlags,
};

use crate::host::S2Host;

const MAX_FRAMES: usize = 4096;

/// Note events from the UI (on-screen keys), as raw MIDI.
#[derive(Clone, Copy)]
pub enum UiNote {
    Midi([u8; 3]),
    /// Parameter id and plain value, sent as a CLAP param-value event.
    Param(u32, f64),
}

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

pub struct Notifications(pub Arc<AtomicU32>);

impl NotificationHandler for Notifications {
    fn xrun(&mut self, _: &Client) -> Control {
        self.0.fetch_add(1, Ordering::Relaxed);
        Control::Continue
    }
}

pub struct Processor {
    pub processor: StartedPluginAudioProcessor<S2Host>,
    midi_in: Port<MidiIn>,
    out_l: Port<AudioOut>,
    out_r: Port<AudioOut>,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    in_channels: Vec<u32>,
    out_channels: Vec<u32>,
    in_bufs: Vec<Vec<f32>>,
    out_bufs: Vec<Vec<f32>>,
    events: EventBuffer,
    ui_notes: rtrb::Consumer<UiNote>,
    steady: u64,
}

fn push_midi(events: &mut EventBuffer, time: u32, bytes: &[u8]) {
    match *bytes {
        [s, key, vel] if s & 0xF0 == 0x90 && vel > 0 => events.push(
            &NoteOnEvent::new(
                time,
                Pckn::new(0u16, s & 0x0F, key as u16, Match::All),
                vel as f64 / 127.0,
            )
            .with_flags(EventFlags::IS_LIVE),
        ),
        [s, key, vel] if s & 0xF0 == 0x80 || s & 0xF0 == 0x90 => events.push(
            &NoteOffEvent::new(
                time,
                Pckn::new(0u16, s & 0x0F, key as u16, Match::All),
                vel as f64 / 127.0,
            )
            .with_flags(EventFlags::IS_LIVE),
        ),
        [a, b, c] => {
            events.push(&MidiEvent::new(time, 0, [a, b, c]).with_flags(EventFlags::IS_LIVE))
        }
        [a, b] => events.push(&MidiEvent::new(time, 0, [a, b, 0]).with_flags(EventFlags::IS_LIVE)),
        _ => {}
    }
}

impl jack::ProcessHandler for Processor {
    fn process(&mut self, _: &Client, ps: &jack::ProcessScope) -> Control {
        let n = (ps.n_frames() as usize).min(MAX_FRAMES);

        self.events.clear();
        for ev in self.midi_in.iter(ps) {
            push_midi(&mut self.events, ev.time, ev.bytes);
        }
        while let Ok(ev) = self.ui_notes.pop() {
            match ev {
                UiNote::Midi(bytes) => push_midi(&mut self.events, 0, &bytes),
                UiNote::Param(id, value) => self.events.push(&ParamValueEvent::new(
                    0,
                    ClapId::new(id),
                    Pckn::match_all(),
                    value,
                )),
            }
        }

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

        let l = self.out_l.as_mut_slice(ps);
        let r = self.out_r.as_mut_slice(ps);
        match (ok, self.out_bufs.first(), self.out_channels.first()) {
            (true, Some(main), Some(&ch)) => {
                l[..n].copy_from_slice(&main[..n]);
                let right = if ch >= 2 {
                    &main[MAX_FRAMES..MAX_FRAMES + n]
                } else {
                    &main[..n]
                };
                r[..n].copy_from_slice(right);
            }
            _ => {
                l.fill(0.0);
                r.fill(0.0);
            }
        }
        Control::Continue
    }
}

pub struct Audio {
    pub client: jack::AsyncClient<Notifications, Processor>,
    pub xruns: Arc<AtomicU32>,
    pub sample_rate: u32,
}

fn port_channels(instance: &mut PluginInstance<S2Host>, is_input: bool) -> Vec<u32> {
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

/// Opens the JACK client, activates the plugin and starts processing.
pub fn start(
    instance: &mut PluginInstance<S2Host>,
    ui_notes: rtrb::Consumer<UiNote>,
) -> Result<Audio> {
    preload_pipewire_jack();
    // SAFETY: called before any other thread reads the environment.
    unsafe { std::env::set_var("PIPEWIRE_QUANTUM", "128/48000") };
    let (client, _) =
        Client::new("merlin-s2", ClientOptions::NO_START_SERVER).context("JACK client")?;
    let sample_rate = client.sample_rate();

    let in_channels = port_channels(instance, true);
    let out_channels = port_channels(instance, false);
    println!("plugin audio ports: inputs {in_channels:?}, outputs {out_channels:?}");

    let config = PluginAudioConfiguration {
        sample_rate: sample_rate as f64,
        min_frames_count: 1,
        max_frames_count: MAX_FRAMES as u32,
    };
    let processor = instance
        .activate(|_, _| (), config)?
        .start_processing()
        .map_err(|e| anyhow::anyhow!("start_processing: {e}"))?;

    let total = |c: &[u32]| c.iter().sum::<u32>() as usize;
    let proc = Processor {
        processor,
        midi_in: client.register_port("midi_in", MidiIn::default())?,
        out_l: client.register_port("out_l", AudioOut::default())?,
        out_r: client.register_port("out_r", AudioOut::default())?,
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
        events: EventBuffer::with_capacity(1024),
        ui_notes,
        steady: 0,
    };
    let names = [proc.out_l.name()?, proc.out_r.name()?, proc.midi_in.name()?];

    let xruns = Arc::new(AtomicU32::new(0));
    let active = client.activate_async(Notifications(xruns.clone()), proc)?;
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
    })
}

impl Audio {
    /// Stops JACK and hands the processor back so the plugin can be deactivated cleanly.
    pub fn stop(self, instance: &mut PluginInstance<S2Host>) {
        if let Ok((_, _, proc)) = self.client.deactivate() {
            instance.deactivate(proc.processor.stop_processing());
        }
    }
}
