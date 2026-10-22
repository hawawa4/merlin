//! The audio-thread half of a VST2 plugin.

use std::sync::Arc;

use libloading::os::unix::Library;

use super::abi::*;
use super::host::Ctx;
use crate::{Io, MidiEvent as EngineMidi, MidiKind, PluginProcessor, ProcessStatus};

/// Largest block handled at once (buffers are sized for it).
pub const MAX_FRAMES: usize = 4096;
/// MIDI events passed to the plugin in one block.
const MAX_EVENTS: usize = 256;
/// Seconds of exact silence (and no MIDI) after which the slot may sleep.
const SLEEP_AFTER_SECS: f32 = 3.0;

pub struct Vst2Processor {
    effect: *mut AEffect,
    can_replacing: bool,
    in_bufs: Vec<Vec<f32>>,
    out_bufs: Vec<Vec<f32>>,
    in_ptrs: Vec<*const f32>,
    out_ptrs: Vec<*mut f32>,
    midi: Box<[MidiEvent; MAX_EVENTS]>,
    events: Box<Events<MAX_EVENTS>>,
    params: rtrb::Consumer<(i32, f32)>,
    silent_frames: usize,
    sleep_after: usize,
    // The library and context outlive every call into the plugin.
    _ctx: Arc<Ctx>,
    _lib: Arc<Library>,
}

// SAFETY: the processor is handed to the audio thread and used only there; the plugin is
// called from one thread at a time (the plugin itself decides what it tolerates).
unsafe impl Send for Vst2Processor {}

impl Vst2Processor {
    pub(super) fn new(
        effect: *mut AEffect,
        params: rtrb::Consumer<(i32, f32)>,
        sample_rate: f32,
        ctx: Arc<Ctx>,
        lib: Arc<Library>,
    ) -> Self {
        // SAFETY: a live effect.
        let (n_in, n_out, flags) = unsafe {
            (
                (*effect).num_inputs.max(0) as usize,
                (*effect).num_outputs.max(0) as usize,
                (*effect).flags,
            )
        };
        // At least two of each, so a plugin that reads or writes a stereo pair always can.
        let in_bufs: Vec<Vec<f32>> = (0..n_in.max(2)).map(|_| vec![0.0; MAX_FRAMES]).collect();
        let out_bufs: Vec<Vec<f32>> = (0..n_out.max(2)).map(|_| vec![0.0; MAX_FRAMES]).collect();
        let zero = MidiEvent {
            kind: EVENT_MIDI,
            byte_size: std::mem::size_of::<MidiEvent>() as i32,
            delta_frames: 0,
            flags: 0,
            note_length: 0,
            note_offset: 0,
            midi_data: [0; 4],
            detune: 0,
            note_off_velocity: 0,
            reserved1: 0,
            reserved2: 0,
        };
        Self {
            effect,
            can_replacing: flags & FLAG_CAN_REPLACING != 0,
            in_ptrs: in_bufs.iter().map(|b| b.as_ptr()).collect(),
            out_ptrs: out_bufs.iter().map(|b| b.as_ptr().cast_mut()).collect(),
            in_bufs,
            out_bufs,
            midi: Box::new([zero; MAX_EVENTS]),
            events: Box::new(Events {
                num_events: 0,
                reserved: 0,
                events: [std::ptr::null_mut(); MAX_EVENTS],
            }),
            params,
            silent_frames: 0,
            sleep_after: (SLEEP_AFTER_SECS * sample_rate) as usize,
            _ctx: ctx,
            _lib: lib,
        }
    }
}

impl PluginProcessor for Vst2Processor {
    fn process(&mut self, io: &mut Io, events: &[EngineMidi]) -> ProcessStatus {
        let n = io.frames().min(MAX_FRAMES);
        // SAFETY: the effect is live for as long as this processor exists (the plugin handle
        // is deactivated only after the engine has handed the processor back).
        unsafe {
            while let Ok((index, value)) = self.params.pop() {
                ((*self.effect).set_parameter)(self.effect, index, value);
            }

            let mut count = 0;
            for ev in events {
                if count == MAX_EVENTS || ev.kind() == MidiKind::Other {
                    continue;
                }
                let m = &mut self.midi[count];
                m.delta_frames = (ev.time as usize).min(n.saturating_sub(1)) as i32;
                m.midi_data = [ev.data[0], ev.data[1], ev.data[2], 0];
                self.events.events[count] = m as *mut MidiEvent;
                count += 1;
            }
            if count > 0 {
                self.events.num_events = count as i32;
                ((*self.effect).dispatcher)(
                    self.effect,
                    EFF_PROCESS_EVENTS,
                    0,
                    0,
                    (&mut *self.events as *mut Events<MAX_EVENTS>).cast(),
                    0.0,
                );
            }

            match io.input {
                Some((l, r)) => {
                    self.in_bufs[0][..n].copy_from_slice(&l[..n]);
                    self.in_bufs[1][..n].copy_from_slice(&r[..n]);
                }
                None => {
                    self.in_bufs[0][..n].fill(0.0);
                    self.in_bufs[1][..n].fill(0.0);
                }
            }
            for b in self.in_bufs.iter_mut().skip(2) {
                b[..n].fill(0.0);
            }
            for b in &mut self.out_bufs {
                b[..n].fill(0.0);
            }
            let process = if self.can_replacing {
                (*self.effect).process_replacing
            } else {
                (*self.effect).process
            };
            process(
                self.effect,
                self.in_ptrs.as_ptr(),
                self.out_ptrs.as_mut_ptr(),
                n as i32,
            );
        }
        io.out_l[..n].copy_from_slice(&self.out_bufs[0][..n]);
        let right = if unsafe { (*self.effect).num_outputs } >= 2 {
            &self.out_bufs[1]
        } else {
            &self.out_bufs[0]
        };
        io.out_r[..n].copy_from_slice(&right[..n]);

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
