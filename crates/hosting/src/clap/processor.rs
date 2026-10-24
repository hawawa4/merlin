//! The audio-thread half of a CLAP plugin: MIDI to CLAP events, audio buffers, status.

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_host::events::event_types::{
    MidiEvent as ClapMidi, NoteOffEvent, NoteOnEvent, ParamValueEvent, TransportEvent,
    TransportFlags,
};
use clack_host::events::{EventFlags, EventHeader, Match};
use clack_host::prelude::*;
use clack_host::utils::BeatTime;

use super::host::ClapHost;
use crate::{Io, MidiEvent, MidiKind, PluginProcessor, ProcessStatus, host_time};

/// Largest block a CLAP processor accepts (buffers are sized for it).
pub const MAX_FRAMES: usize = 4096;

/// A parameter change from the main thread.
#[derive(Clone, Copy, Debug)]
pub struct ParamChange {
    pub id: u32,
    pub value: f64,
}

pub struct ClapProcessor {
    processor: StartedPluginAudioProcessor<ClapHost>,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    in_channels: Vec<u32>,
    out_channels: Vec<u32>,
    in_bufs: Vec<Vec<f32>>,
    out_bufs: Vec<Vec<f32>>,
    events: EventBuffer,
    params: rtrb::Consumer<ParamChange>,
    steady: u64,
}

/// Channel counts of the plugin's audio ports. A plugin without the extension is assumed to
/// have one stereo output.
pub fn port_channels(instance: &mut PluginInstance<ClapHost>, is_input: bool) -> Vec<u32> {
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

impl ClapProcessor {
    pub(super) fn new(
        processor: StartedPluginAudioProcessor<ClapHost>,
        in_channels: Vec<u32>,
        out_channels: Vec<u32>,
        params: rtrb::Consumer<ParamChange>,
    ) -> Self {
        let total = |c: &[u32]| c.iter().sum::<u32>() as usize;
        Self {
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
            events: EventBuffer::with_capacity(2048),
            params,
            steady: 0,
        }
    }
}

fn push_midi(events: &mut EventBuffer, ev: &MidiEvent) {
    let time = ev.time;
    let flags = EventFlags::IS_LIVE;
    match ev.kind() {
        MidiKind::NoteOn { ch, key, vel } => events.push(
            &NoteOnEvent::new(
                time,
                Pckn::new(0u16, ch, key as u16, Match::All),
                vel as f64 / 127.0,
            )
            .with_flags(flags),
        ),
        MidiKind::NoteOff { ch, key, vel } => events.push(
            &NoteOffEvent::new(
                time,
                Pckn::new(0u16, ch, key as u16, Match::All),
                vel as f64 / 127.0,
            )
            .with_flags(flags),
        ),
        MidiKind::Other => {}
        _ => events.push(&ClapMidi::new(time, 0, ev.data).with_flags(flags)),
    }
}

impl PluginProcessor for ClapProcessor {
    fn process(&mut self, io: &mut Io, events: &[MidiEvent]) -> ProcessStatus {
        let n = io.frames().min(MAX_FRAMES);

        // Parameter changes first (time 0), so the list stays in time order.
        self.events.clear();
        while let Ok(p) = self.params.pop() {
            self.events.push(&ParamValueEvent::new(
                0,
                ClapId::new(p.id),
                Pckn::match_all(),
                p.value,
            ));
        }
        for ev in events {
            push_midi(&mut self.events, ev);
        }

        for b in &mut self.in_bufs {
            b.fill(0.0);
        }
        if let (Some((l, r)), Some(buf), Some(&ch)) =
            (io.input, self.in_bufs.first_mut(), self.in_channels.first())
        {
            buf[..n].copy_from_slice(&l[..n]);
            if ch >= 2 {
                buf[MAX_FRAMES..MAX_FRAMES + n].copy_from_slice(&r[..n]);
            }
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
                                        is_constant: false,
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
        // The host's tempo and time signature, for plugins that sync to them (delays, arpeggiators,
        // LFOs). A live rig has no timeline, so only these and "playing" are given.
        let zero = BeatTime::from_int(0);
        let transport = TransportEvent {
            header: EventHeader::new_core(0, EventFlags::empty()),
            flags: TransportFlags::HAS_TEMPO
                | TransportFlags::HAS_TIME_SIGNATURE
                | TransportFlags::IS_PLAYING,
            song_pos_beats: zero,
            song_pos_seconds: zero,
            tempo: f64::from(host_time::tempo()),
            tempo_inc: 0.0,
            loop_start_beats: zero,
            loop_end_beats: zero,
            loop_start_seconds: zero,
            loop_end_seconds: zero,
            bar_start: zero,
            bar_number: 0,
            time_signature_numerator: host_time::beats_per_bar().min(u32::from(u16::MAX)) as u16,
            time_signature_denominator: 4,
        };
        let result = self.processor.process(
            &ins,
            &mut outs,
            &self.events.as_input(),
            &mut OutputEvents::void(),
            Some(self.steady),
            Some(&transport),
        );
        self.steady += n as u64;

        let (Ok(status), Some(main), Some(&ch)) =
            (result, self.out_bufs.first(), self.out_channels.first())
        else {
            // A failed process() leaves silence (the engine zeroed the outputs).
            return ProcessStatus::Continue;
        };
        io.out_l[..n].copy_from_slice(&main[..n]);
        let right = if ch >= 2 {
            &main[MAX_FRAMES..MAX_FRAMES + n]
        } else {
            &main[..n]
        };
        io.out_r[..n].copy_from_slice(right);
        match status {
            clack_host::process::ProcessStatus::Sleep => ProcessStatus::Sleep,
            _ => ProcessStatus::Continue,
        }
    }
}
