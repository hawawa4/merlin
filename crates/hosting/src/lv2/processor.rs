//! The audio-thread half of an LV2 plugin: connects the ports, fills the MIDI atom sequence,
//! and runs the plugin.

use std::ffi::c_void;
use std::sync::Arc;

use super::abi::{Atom, AtomEvent, SequenceBody};
use super::instance::{Bank, Instance};
use super::ttl::{PluginDesc, PortDesc, PortKind};
use crate::{Io, MidiEvent, MidiKind, PluginProcessor, ProcessStatus};

/// Largest block handled at once.
pub const MAX_FRAMES: usize = 4096;
/// Bytes of an atom sequence buffer (MIDI in, and the dummy outputs).
const ATOM_BYTES: usize = 64 * 1024;
const SLEEP_AFTER_SECS: f32 = 3.0;
/// Largest message a UI can send to the plugin.
pub const UI_ATOM_BYTES: usize = 2048;

/// A message from the plugin's UI to its atom input (a patch message, a MIDI event).
pub struct UiAtom {
    #[allow(dead_code)]
    pub port: u32,
    pub len: u32,
    pub data: [u8; UI_ATOM_BYTES],
}

struct AtomBuf {
    port: usize,
    midi: bool,
    input: bool,
    /// 8-byte aligned storage.
    buf: Vec<u64>,
}

impl AtomBuf {
    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: a `u64` slice is valid as bytes.
        unsafe {
            std::slice::from_raw_parts_mut(self.buf.as_mut_ptr().cast::<u8>(), self.buf.len() * 8)
        }
    }
}

pub struct Lv2Processor {
    inst: Arc<Instance>,
    bank: Arc<Bank>,
    ports: Vec<PortDesc>,
    /// One float of storage per port, for control and latency-like ports.
    scalars: Box<[f32]>,
    /// Audio buffers; `audio_of[port]` says which one a port uses.
    audio: Vec<Vec<f32>>,
    audio_of: Vec<Option<usize>>,
    in_audio: Vec<usize>,
    out_audio: Vec<usize>,
    atoms: Vec<AtomBuf>,
    ui: rtrb::Consumer<UiAtom>,
    urid_sequence: u32,
    urid_chunk: u32,
    urid_midi: u32,
    silent_frames: usize,
    sleep_after: usize,
}

// SAFETY: the processor is used on the audio thread only; the plugin's threading rules allow
// `run` on a thread other than the one that created the instance.
unsafe impl Send for Lv2Processor {}

impl Lv2Processor {
    pub fn new(
        inst: Arc<Instance>,
        desc: &PluginDesc,
        bank: Arc<Bank>,
        ui: rtrb::Consumer<UiAtom>,
        sample_rate: f32,
    ) -> Self {
        let urids = inst.urids().clone();
        let count = desc.ports.iter().map(|p| p.index + 1).max().unwrap_or(0);
        let mut s = Self {
            inst,
            bank,
            ports: desc.ports.clone(),
            scalars: vec![0.0; count].into_boxed_slice(),
            audio: Vec::new(),
            audio_of: vec![None; count],
            in_audio: Vec::new(),
            out_audio: Vec::new(),
            atoms: Vec::new(),
            ui,
            urid_sequence: urids.map("http://lv2plug.in/ns/ext/atom#Sequence"),
            urid_chunk: urids.map("http://lv2plug.in/ns/ext/atom#Chunk"),
            urid_midi: urids.map("http://lv2plug.in/ns/ext/midi#MidiEvent"),
            silent_frames: 0,
            sleep_after: (SLEEP_AFTER_SECS * sample_rate) as usize,
        };
        for p in &desc.ports {
            match p.kind {
                PortKind::Audio | PortKind::Cv => {
                    s.audio_of[p.index] = Some(s.audio.len());
                    s.audio.push(vec![0.0; MAX_FRAMES]);
                    if p.kind == PortKind::Audio {
                        if p.input {
                            s.in_audio.push(p.index);
                        } else {
                            s.out_audio.push(p.index);
                        }
                    }
                }
                PortKind::Control => s.scalars[p.index] = p.default as f32,
                PortKind::Atom => s.atoms.push(AtomBuf {
                    port: p.index,
                    midi: p.midi,
                    input: p.input,
                    buf: vec![0; ATOM_BYTES / 8],
                }),
            }
        }
        s.connect();
        s
    }

    /// Points every port at its storage. The storage never moves afterwards.
    fn connect(&mut self) {
        // SAFETY: valid port indices and buffers that live as long as the processor.
        unsafe {
            let connect = (*self.inst.desc).connect_port;
            for p in &self.ports {
                let ptr: *mut c_void = match p.kind {
                    PortKind::Audio | PortKind::Cv => self.audio[self.audio_of[p.index].unwrap()]
                        .as_mut_ptr()
                        .cast(),
                    PortKind::Control => (&mut self.scalars[p.index] as *mut f32).cast(),
                    PortKind::Atom => self
                        .atoms
                        .iter_mut()
                        .find(|a| a.port == p.index)
                        .unwrap()
                        .buf
                        .as_mut_ptr()
                        .cast(),
                };
                connect(self.inst.handle, p.index as u32, ptr);
            }
            if let Some(activate) = (*self.inst.desc).activate {
                activate(self.inst.handle);
            }
        }
    }
}

impl Drop for Lv2Processor {
    fn drop(&mut self) {
        // SAFETY: no run() is in progress; the instance itself is cleaned up when its last
        // owner lets go.
        unsafe {
            if let Some(deactivate) = (*self.inst.desc).deactivate {
                deactivate(self.inst.handle);
            }
        }
    }
}

/// Appends an event (time in frames, an atom of `kind` with `body`) to a sequence buffer.
/// Returns false if it does not fit.
fn push_event(bytes: &mut [u8], frames: i64, kind: u32, body: &[u8]) -> bool {
    let header = std::mem::size_of::<Atom>() + std::mem::size_of::<SequenceBody>();
    let used = u32::from_ne_bytes(bytes[0..4].try_into().unwrap()) as usize; // seq atom size
    let start = std::mem::size_of::<Atom>() + used;
    let event = std::mem::size_of::<AtomEvent>() + body.len();
    let padded = (event + 7) & !7;
    if start + padded > bytes.len() || used < std::mem::size_of::<SequenceBody>() {
        let _ = header;
        return false;
    }
    bytes[start..start + 8].copy_from_slice(&frames.to_ne_bytes());
    bytes[start + 8..start + 12].copy_from_slice(&(body.len() as u32).to_ne_bytes());
    bytes[start + 12..start + 16].copy_from_slice(&kind.to_ne_bytes());
    bytes[start + 16..start + 16 + body.len()].copy_from_slice(body);
    bytes[start + 16 + body.len()..start + padded].fill(0);
    let new_size = (used + padded) as u32;
    bytes[0..4].copy_from_slice(&new_size.to_ne_bytes());
    true
}

impl PluginProcessor for Lv2Processor {
    fn process(&mut self, io: &mut Io, events: &[MidiEvent]) -> ProcessStatus {
        let n = io.frames().min(MAX_FRAMES);

        for p in &self.ports {
            if p.kind == PortKind::Control && p.input {
                self.scalars[p.index] = self.bank.get(p.index);
            }
        }

        let mut ui_pending = true;
        for a in &mut self.atoms {
            let (input, midi) = (a.input, a.midi);
            let bytes = a.bytes();
            if input {
                // An empty sequence: the sequence body (unit, pad) and no events.
                bytes[0..4]
                    .copy_from_slice(&(std::mem::size_of::<SequenceBody>() as u32).to_ne_bytes());
                bytes[4..8].copy_from_slice(&self.urid_sequence.to_ne_bytes());
                bytes[8..16].fill(0);
                if midi {
                    for ev in events {
                        if ev.kind() == MidiKind::Other {
                            continue;
                        }
                        push_event(
                            bytes,
                            i64::from(ev.time.min(n.saturating_sub(1) as u32)),
                            self.urid_midi,
                            &ev.data,
                        );
                    }
                }
                // What the plugin's UI sent goes into the first input that can take it.
                if ui_pending {
                    ui_pending = false;
                    while let Ok(m) = self.ui.pop() {
                        let body = &m.data[..m.len as usize];
                        if body.len() >= std::mem::size_of::<Atom>() {
                            let kind = u32::from_ne_bytes(body[4..8].try_into().unwrap());
                            push_event(bytes, 0, kind, &body[8..]);
                        }
                    }
                }
            } else {
                // An output sequence: a chunk with the room it has.
                let room = (bytes.len() - std::mem::size_of::<Atom>()) as u32;
                bytes[0..4].copy_from_slice(&room.to_ne_bytes());
                bytes[4..8].copy_from_slice(&self.urid_chunk.to_ne_bytes());
            }
        }

        // Audio in: the first two inputs get the signal (or silence), the rest silence.
        for (k, &port) in self.in_audio.iter().enumerate() {
            let buf = &mut self.audio[self.audio_of[port].unwrap()];
            match (io.input, k) {
                (Some((l, _)), 0) => buf[..n].copy_from_slice(&l[..n]),
                (Some((_, r)), 1) => buf[..n].copy_from_slice(&r[..n]),
                _ => buf[..n].fill(0.0),
            }
        }
        for &port in &self.out_audio {
            self.audio[self.audio_of[port].unwrap()][..n].fill(0.0);
        }

        if let Some(w) = &self.inst.worker {
            w.deliver(self.inst.handle);
        }
        // SAFETY: every port is connected, and the buffers hold at least `n` frames.
        unsafe { ((*self.inst.desc).run)(self.inst.handle, n as u32) };
        if let Some(w) = &self.inst.worker {
            w.end_run(self.inst.handle);
        }

        io.out_l[..n].fill(0.0);
        io.out_r[..n].fill(0.0);
        match self.out_audio.as_slice() {
            [] => {}
            [only] => {
                let b = &self.audio[self.audio_of[*only].unwrap()];
                io.out_l[..n].copy_from_slice(&b[..n]);
                io.out_r[..n].copy_from_slice(&b[..n]);
            }
            [l, r, ..] => {
                io.out_l[..n].copy_from_slice(&self.audio[self.audio_of[*l].unwrap()][..n]);
                io.out_r[..n].copy_from_slice(&self.audio[self.audio_of[*r].unwrap()][..n]);
            }
        }
        for p in &self.ports {
            if p.kind == PortKind::Control && !p.input {
                self.bank.set(p.index, self.scalars[p.index]);
            }
        }

        let quiet = events.is_empty()
            && io.input.is_none()
            && io.out_l[..n]
                .iter()
                .chain(&io.out_r[..n])
                .all(|x| x.abs() < 1e-7);
        self.silent_frames = if quiet { self.silent_frames + n } else { 0 };
        if self.silent_frames >= self.sleep_after {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}
