use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use merlin_dsp::{FxChain, Limiter};
use merlin_hosting::{Io, MidiEvent, PluginProcessor, ProcessStatus};
use merlin_model::{FxSettings, MAX_INSERTS, MAX_SLOTS, SelectMode};

use crate::Shared;
use crate::events::EventBuf;
use crate::router::{Filter, Router};

const COMMAND_QUEUE: usize = 256;
const RETIRED_QUEUE: usize = 64;
/// Master duck around a panic.
const FADE_OUT_SECS: f32 = 0.005;
const FADE_IN_SECS: f32 = 0.020;
/// Master limiter ceiling.
const LIMITER_CEILING_DB: f32 = -0.3;
/// An idle slot's effects are skipped after this long without any output.
const FX_IDLE_SECS: f32 = 2.0;

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
    /// Install or replace an insert effect (position `index` in the slot's chain). The old one
    /// comes back as [`Retired`].
    SetInsert {
        slot: usize,
        index: usize,
        processor: Box<dyn PluginProcessor>,
    },
    ClearInsert {
        slot: usize,
        index: usize,
    },
    SetInsertBypass {
        slot: usize,
        index: usize,
        bypass: bool,
    },
    /// A slot's built-in effects.
    SetFx {
        slot: usize,
        settings: FxSettings,
    },
    SetInheritPedal(bool),
    SetMasterVolume(f32),
    /// The master limiter is on by default.
    SetLimiter(bool),
    Panic,
}

/// Audio thread -> main thread: a processor to be dropped (and deactivated) off the
/// audio thread.
pub struct Retired {
    pub slot: usize,
    /// `Some(k)` for an insert effect, `None` for the slot's instrument.
    pub insert: Option<usize>,
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

/// How slowly the per-slot load follows the latest block (per block; about 50 blocks).
const LOAD_SMOOTHING: f32 = 0.98;

struct SlotRt {
    processor: Option<Box<dyn PluginProcessor>>,
    volume: f32,
    pan: f32,
    mute: bool,
    /// Gains applied at the end of the previous block, to ramp from.
    last_l: f32,
    last_r: f32,
    sleeping: bool,
    fx: FxChain,
    inserts: [Option<Box<dyn PluginProcessor>>; MAX_INSERTS],
    insert_bypass: [bool; MAX_INSERTS],
    /// Frames of exactly silent output while the instrument sleeps with effects on.
    quiet_frames: usize,
    /// Smoothed share of the block budget this slot takes (what [`Shared::load`] shows).
    load_avg: f32,
}

impl SlotRt {
    fn new(sample_rate: f32) -> Self {
        Self {
            processor: None,
            volume: 1.0,
            pan: 0.0,
            mute: false,
            last_l: 0.0,
            last_r: 0.0,
            sleeping: false,
            fx: FxChain::new(sample_rate),
            inserts: std::array::from_fn(|_| None),
            insert_bypass: [false; MAX_INSERTS],
            quiet_frames: 0,
            load_avg: 0.0,
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
    tmp_l: Vec<f32>,
    tmp_r: Vec<f32>,
    mix_l: Vec<f32>,
    mix_r: Vec<f32>,
    master_volume: f32,
    fade: Fade,
    fade_out: u32,
    fade_in: u32,
    limiter: Limiter,
    limiter_on: bool,
    sample_rate: f32,
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
        slots: (0..MAX_SLOTS).map(|_| SlotRt::new(sample_rate)).collect(),
        events: (0..MAX_SLOTS).map(|_| EventBuf::new()).collect(),
        scratch_l: vec![0.0; max_block],
        scratch_r: vec![0.0; max_block],
        tmp_l: vec![0.0; max_block],
        tmp_r: vec![0.0; max_block],
        mix_l: vec![0.0; max_block],
        mix_r: vec![0.0; max_block],
        master_volume: 1.0,
        fade: Fade::Idle,
        fade_out: (FADE_OUT_SECS * sample_rate) as u32,
        fade_in: (FADE_IN_SECS * sample_rate) as u32,
        limiter: Limiter::new(sample_rate, LIMITER_CEILING_DB),
        limiter_on: true,
        sample_rate,
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
            if let Err(rtrb::PushError::Full(r)) = self.retired.push(Retired {
                slot,
                insert: None,
                processor,
            }) {
                std::mem::forget(r);
            }
        }
        self.slots[slot].sleeping = false;
        self.router.drop_slot(slot);
    }

    fn retire_insert(&mut self, slot: usize, index: usize) {
        if let Some(processor) = self.slots[slot].inserts[index].take() {
            // As for instruments: leak rather than free on the audio thread if the queue is full.
            if let Err(rtrb::PushError::Full(r)) = self.retired.push(Retired {
                slot,
                insert: Some(index),
                processor,
            }) {
                std::mem::forget(r);
            }
        }
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
            Command::SetInsert {
                slot,
                index,
                processor,
            } => {
                self.retire_insert(slot, index);
                self.slots[slot].inserts[index] = Some(processor);
                self.slots[slot].quiet_frames = 0;
            }
            Command::ClearInsert { slot, index } => self.retire_insert(slot, index),
            Command::SetInsertBypass {
                slot,
                index,
                bypass,
            } => self.slots[slot].insert_bypass[index] = bypass,
            Command::SetFx { slot, settings } => self.slots[slot].fx.set(&settings),
            Command::SetLimiter(on) => self.limiter_on = on,
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
        let fx_idle_frames = (FX_IDLE_SECS * self.sample_rate) as usize;
        // What one block is allowed to take, for the per-slot load.
        let budget_ns = frames as f32 / self.sample_rate * 1e9;
        for i in 0..MAX_SLOTS {
            let slot = &mut self.slots[i];
            let Some(processor) = slot.processor.as_mut() else {
                continue;
            };
            let started = Instant::now();
            let events = self.events[i].as_slice();
            // A slot that is a keyboard target always runs: no wake-up latency for what is being
            // played, and its editor can see regular process calls. Only idle slots sleep.
            let target = self.router.targets() & (1 << i) != 0;
            let skip_instrument = slot.sleeping && events.is_empty() && !target;
            // A sleeping instrument's effects keep running until their tails have died away.
            let has_inserts = slot
                .inserts
                .iter()
                .zip(&slot.insert_bypass)
                .any(|(p, &bypass)| p.is_some() && !bypass);
            let effects = slot.fx.is_active() || has_inserts;
            let fx_live = effects && slot.quiet_frames < fx_idle_frames;
            if skip_instrument && !fx_live {
                sleeping |= 1 << i;
                // A sleeping slot costs nothing: let its load fade out.
                slot.load_avg *= LOAD_SMOOTHING;
                self.shared.load[i].store(slot.load_avg.to_bits(), Ordering::Relaxed);
                continue;
            }
            self.scratch_l[..frames].fill(0.0);
            self.scratch_r[..frames].fill(0.0);
            if !skip_instrument {
                let status = processor.process(
                    &mut Io {
                        input: None,
                        out_l: &mut self.scratch_l[..frames],
                        out_r: &mut self.scratch_r[..frames],
                    },
                    events,
                );
                slot.sleeping = status == ProcessStatus::Sleep;
            }
            if effects {
                // EQ and compressor, then the insert plugins, then delay and reverb.
                slot.fx
                    .process_pre(&mut self.scratch_l[..frames], &mut self.scratch_r[..frames]);
                for k in 0..MAX_INSERTS {
                    let Some(insert) = slot.inserts[k].as_mut().filter(|_| !slot.insert_bypass[k])
                    else {
                        continue;
                    };
                    self.tmp_l[..frames].copy_from_slice(&self.scratch_l[..frames]);
                    self.tmp_r[..frames].copy_from_slice(&self.scratch_r[..frames]);
                    self.scratch_l[..frames].fill(0.0);
                    self.scratch_r[..frames].fill(0.0);
                    insert.process(
                        &mut Io {
                            input: Some((&self.tmp_l[..frames], &self.tmp_r[..frames])),
                            out_l: &mut self.scratch_l[..frames],
                            out_r: &mut self.scratch_r[..frames],
                        },
                        &[],
                    );
                }
                slot.fx
                    .process_post(&mut self.scratch_l[..frames], &mut self.scratch_r[..frames]);
                self.shared.comp_reduction[i].store(
                    slot.fx.compressor_reduction_db().to_bits(),
                    Ordering::Relaxed,
                );
                if skip_instrument {
                    let loud = self.scratch_l[..frames]
                        .iter()
                        .chain(&self.scratch_r[..frames])
                        .any(|x| x.abs() > 1e-7);
                    slot.quiet_frames = if loud { 0 } else { slot.quiet_frames + frames };
                } else {
                    slot.quiet_frames = 0;
                }
            }

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
            let used = started.elapsed().as_nanos() as f32 / budget_ns;
            slot.load_avg = slot.load_avg * LOAD_SMOOTHING + used * (1.0 - LOAD_SMOOTHING);
            self.shared.load[i].store(slot.load_avg.to_bits(), Ordering::Relaxed);
            self.shared.load_max[i].fetch_max(used.to_bits(), Ordering::Relaxed);
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
        }
        if self.limiter_on {
            self.limiter.process(out_l, out_r);
            self.shared
                .limiter_reduction
                .fetch_max(self.limiter.reduction_db().to_bits(), Ordering::Relaxed);
        }
        for t in 0..frames {
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
