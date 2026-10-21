use std::sync::Arc;
use std::sync::atomic::Ordering;

use merlin_hosting::{Io, MidiEvent, PluginProcessor, ProcessStatus};
use merlin_model::{MAX_SLOTS, SelectMode};

use crate::Shared;
use crate::events::EventBuf;
use crate::router::{Filter, Router};

const COMMAND_QUEUE: usize = 256;
const RETIRED_QUEUE: usize = 64;
/// Master duck around a panic.
const FADE_OUT_SECS: f32 = 0.005;
const FADE_IN_SECS: f32 = 0.020;

/// Main thread -> audio thread.
pub enum Command {
    /// Install or replace a slot's instrument. The old one comes back as [`Retired`].
    SetProcessor {
        slot: usize,
        processor: Box<dyn PluginProcessor>,
    },
    ClearProcessor {
        slot: usize,
    },
    SetFilter {
        slot: usize,
        filter: Filter,
    },
    SetMix {
        slot: usize,
        volume: f32,
        pan: f32,
        mute: bool,
    },
    Select {
        slot: usize,
        mode: SelectMode,
    },
    SetInheritPedal(bool),
    SetMasterVolume(f32),
    Panic,
}

/// Audio thread -> main thread: a processor to be dropped (and deactivated) off the
/// audio thread.
pub struct Retired {
    pub slot: usize,
    pub processor: Box<dyn PluginProcessor>,
}

/// The main thread's end of the engine.
pub struct EngineHandle {
    commands: rtrb::Producer<Command>,
    retired: rtrb::Consumer<Retired>,
    pub shared: Arc<Shared>,
}

impl EngineHandle {
    /// Queues a command; gives it back if the queue is full.
    pub fn send(&mut self, command: Command) -> Result<(), Command> {
        self.commands
            .push(command)
            .map_err(|rtrb::PushError::Full(c)| c)
    }

    /// Processors the engine has let go of since the last call.
    pub fn take_retired(&mut self) -> Vec<Retired> {
        let mut v = Vec::new();
        while let Ok(r) = self.retired.pop() {
            v.push(r);
        }
        v
    }
}

struct SlotRt {
    processor: Option<Box<dyn PluginProcessor>>,
    volume: f32,
    pan: f32,
    mute: bool,
    /// Gains applied at the end of the previous block, to ramp from.
    last_l: f32,
    last_r: f32,
    sleeping: bool,
}

impl SlotRt {
    fn new() -> Self {
        Self {
            processor: None,
            volume: 1.0,
            pan: 0.0,
            mute: false,
            last_l: 0.0,
            last_r: 0.0,
            sleeping: false,
        }
    }

    /// Constant-power pan law, unity at the centre.
    fn target_gains(&self) -> (f32, f32) {
        if self.mute {
            return (0.0, 0.0);
        }
        let a = (self.pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        (
            self.volume * a.cos() * std::f32::consts::SQRT_2,
            self.volume * a.sin() * std::f32::consts::SQRT_2,
        )
    }
}

#[derive(Clone, Copy)]
enum Fade {
    Idle,
    Out(u32),
    In(u32),
}

pub struct Engine {
    max_block: usize,
    router: Router,
    slots: Vec<SlotRt>,
    events: Vec<EventBuf>,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
    mix_l: Vec<f32>,
    mix_r: Vec<f32>,
    master_volume: f32,
    fade: Fade,
    fade_out: u32,
    fade_in: u32,
    commands: rtrb::Consumer<Command>,
    retired: rtrb::Producer<Retired>,
    shared: Arc<Shared>,
}

/// Creates the engine (for the audio thread) and its handle (for the main thread).
pub fn new(sample_rate: f32, max_block: usize) -> (Engine, EngineHandle) {
    let shared = Arc::new(Shared::new());
    let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(COMMAND_QUEUE);
    let (ret_tx, ret_rx) = rtrb::RingBuffer::new(RETIRED_QUEUE);
    let engine = Engine {
        max_block,
        router: Router::new(shared.clone()),
        slots: (0..MAX_SLOTS).map(|_| SlotRt::new()).collect(),
        events: (0..MAX_SLOTS).map(|_| EventBuf::new()).collect(),
        scratch_l: vec![0.0; max_block],
        scratch_r: vec![0.0; max_block],
        mix_l: vec![0.0; max_block],
        mix_r: vec![0.0; max_block],
        master_volume: 1.0,
        fade: Fade::Idle,
        fade_out: (FADE_OUT_SECS * sample_rate) as u32,
        fade_in: (FADE_IN_SECS * sample_rate) as u32,
        commands: cmd_rx,
        retired: ret_tx,
        shared: shared.clone(),
    };
    let handle = EngineHandle {
        commands: cmd_tx,
        retired: ret_rx,
        shared,
    };
    (engine, handle)
}

impl Engine {
    pub fn router(&self) -> &Router {
        &self.router
    }

    fn retire(&mut self, slot: usize) {
        if let Some(processor) = self.slots[slot].processor.take() {
            // If the queue is full the processor is leaked rather than freed here: dropping a
            // plugin on the audio thread is worse.
            if let Err(rtrb::PushError::Full(r)) = self.retired.push(Retired { slot, processor }) {
                std::mem::forget(r);
            }
        }
        self.slots[slot].sleeping = false;
        self.router.drop_slot(slot);
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::SetProcessor { slot, processor } => {
                self.retire(slot);
                let s = &mut self.slots[slot];
                s.processor = Some(processor);
                s.sleeping = false;
                (s.last_l, s.last_r) = s.target_gains();
                self.router.set_loaded(slot, true);
            }
            Command::ClearProcessor { slot } => self.retire(slot),
            Command::SetFilter { slot, filter } => self.router.set_filter(slot, filter),
            Command::SetMix {
                slot,
                volume,
                pan,
                mute,
            } => {
                let s = &mut self.slots[slot];
                (s.volume, s.pan, s.mute) = (volume, pan, mute);
            }
            Command::Select { slot, mode } => self.router.select(slot, mode, &mut self.events),
            Command::SetInheritPedal(on) => self.router.inherit_pedal = on,
            Command::SetMasterVolume(v) => self.master_volume = v,
            Command::Panic => {
                self.router.panic(&mut self.events);
                self.fade = Fade::Out(0);
            }
        }
    }

    /// Renders `out_l.len()` frames. `midi` is sorted by time (frames from the start of this
    /// call). Allocation-free, lock-free.
    pub fn process(&mut self, midi: &[MidiEvent], out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(out_r.len());
        let mut start = 0;
        let mut next = 0;
        while start < n {
            let len = (n - start).min(self.max_block);
            let end = start + len;
            let first = next;
            while next < midi.len() && (midi[next].time as usize) < end {
                next += 1;
            }
            self.block(
                start,
                &midi[first..next],
                &mut out_l[start..end],
                &mut out_r[start..end],
            );
            start = end;
        }
    }

    fn block(&mut self, start: usize, midi: &[MidiEvent], out_l: &mut [f32], out_r: &mut [f32]) {
        let frames = out_l.len();
        while let Ok(command) = self.commands.pop() {
            self.apply(command);
        }
        for ev in midi {
            let mut local = *ev;
            local.time = ev.time.saturating_sub(start as u32);
            self.router.route(&local, &mut self.events);
        }

        self.mix_l[..frames].fill(0.0);
        self.mix_r[..frames].fill(0.0);
        let mut sleeping = 0u16;
        for i in 0..MAX_SLOTS {
            let slot = &mut self.slots[i];
            let Some(processor) = slot.processor.as_mut() else {
                continue;
            };
            let events = self.events[i].as_slice();
            // A slot that is a keyboard target always runs: no wake-up latency for what is being
            // played, and its editor can see regular process calls. Only idle slots sleep.
            let target = self.router.targets() & (1 << i) != 0;
            if slot.sleeping && events.is_empty() && !target {
                sleeping |= 1 << i;
                continue;
            }
            self.scratch_l[..frames].fill(0.0);
            self.scratch_r[..frames].fill(0.0);
            let status = processor.process(
                &mut Io {
                    input: None,
                    out_l: &mut self.scratch_l[..frames],
                    out_r: &mut self.scratch_r[..frames],
                },
                events,
            );
            slot.sleeping = status == ProcessStatus::Sleep;

            // Ramp the gain across the block so volume, pan and mute changes don't click.
            let (tl, tr) = slot.target_gains();
            let (mut gl, mut gr) = (slot.last_l, slot.last_r);
            let (dl, dr) = ((tl - gl) / frames as f32, (tr - gr) / frames as f32);
            let mut peak = 0f32;
            for t in 0..frames {
                gl += dl;
                gr += dr;
                let (l, r) = (self.scratch_l[t] * gl, self.scratch_r[t] * gr);
                peak = peak.max(l.abs()).max(r.abs());
                self.mix_l[t] += l;
                self.mix_r[t] += r;
            }
            (slot.last_l, slot.last_r) = (tl, tr);
            self.shared.peaks[i].fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
        self.shared.sleeping.store(sleeping, Ordering::Relaxed);

        // Master volume and the panic duck.
        let mut master_peak = 0f32;
        for t in 0..frames {
            let duck = match self.fade {
                Fade::Idle => 1.0,
                Fade::Out(pos) => {
                    let g = 1.0 - pos as f32 / self.fade_out.max(1) as f32;
                    self.fade = if pos + 1 >= self.fade_out {
                        Fade::In(0)
                    } else {
                        Fade::Out(pos + 1)
                    };
                    g
                }
                Fade::In(pos) => {
                    let g = pos as f32 / self.fade_in.max(1) as f32;
                    self.fade = if pos + 1 >= self.fade_in {
                        Fade::Idle
                    } else {
                        Fade::In(pos + 1)
                    };
                    g
                }
            };
            let g = self.master_volume * duck;
            out_l[t] = self.mix_l[t] * g;
            out_r[t] = self.mix_r[t] * g;
            master_peak = master_peak.max(out_l[t].abs()).max(out_r[t].abs());
        }
        self.shared
            .master_peak
            .fetch_max(master_peak.to_bits(), Ordering::Relaxed);

        let dropped: u32 = self.events.iter().map(|e| e.dropped).sum();
        self.shared.dropped_events.store(dropped, Ordering::Relaxed);
        self.shared.blocks.fetch_add(1, Ordering::Relaxed);
        for e in &mut self.events {
            e.clear();
        }
    }
}
