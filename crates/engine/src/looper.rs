//! The loop station: every slot can record what it is played and replay it, over and over.
//!
//! It records MIDI, not audio: what the router hands a slot is stored against a position in the
//! loop and handed to the slot again on the next pass. The loop is `bars` bars of the transport's
//! time signature long and runs while the transport does (position 0 is the first beat of the
//! run), so it is in step with the click and, being kept in ticks rather than frames, with tempo
//! changes. Each slot has two parts, A and B, of which one plays; a part switch, the start of a
//! recording and its end all wait for the loop to come round, so they land on beat 1.
//!
//! Everything here runs on the audio thread: nothing allocates after [`Looper::new`].

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};

use merlin_hosting::MidiEvent;
use merlin_model::MAX_SLOTS;

use crate::events::EventBuf;

/// Resolution of the loop: ticks per beat.
pub const PPQ: u32 = 960;
/// Longest loop, in bars.
pub const MAX_BARS: u8 = 8;
/// Most MIDI messages one part holds. Past that, further notes are not recorded.
const PART_EVENTS: usize = 4096;
/// How many passes of overdubbing one part can undo.
const MAX_PASSES: usize = 32;

/// What a slot's loop is doing. The numbers are what [`LoopShared`] publishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LoopMode {
    /// Nothing recorded in the part.
    Idle = 0,
    /// Waiting for the loop to come round, then records one pass.
    Armed = 1,
    /// Recording the first pass.
    Rec = 2,
    /// Playing what is recorded.
    Play = 3,
    /// Playing and recording more on top.
    Over = 4,
}

impl LoopMode {
    pub fn from_u8(n: u8) -> Self {
        match n {
            1 => Self::Armed,
            2 => Self::Rec,
            3 => Self::Play,
            4 => Self::Over,
            _ => Self::Idle,
        }
    }
}

/// Main thread -> audio thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopCommand {
    /// The slot's REC button: arm, finish, overdub, back to playing.
    Rec(usize),
    /// Switch to part 0 (A) or 1 (B) when the loop comes round.
    Part(usize, usize),
    Mute(usize, bool),
    /// Undo the last recorded pass of the playing part.
    Undo(usize),
    /// Empty the playing part.
    Clear(usize),
    ClearAll,
    /// Change the loop's length. The loops are emptied: they would no longer fit.
    SetBars(u8),
}

#[derive(Clone, Copy)]
struct LoopEv {
    tick: u32,
    pass: u16,
    data: [u8; 3],
}

struct Part {
    events: Vec<LoopEv>,
    /// Ids of the passes recorded, the last one on top; what undo takes off.
    passes: [u16; MAX_PASSES],
    n_passes: usize,
    /// Which bars hold a message (bit n = bar n), kept up to date by [`Part::refresh`].
    filled: u16,
    stale: bool,
    /// The bar length `filled` was worked out for.
    bar_ticks: u32,
}

impl Part {
    fn new() -> Self {
        Self {
            events: Vec::with_capacity(PART_EVENTS),
            passes: [0; MAX_PASSES],
            n_passes: 0,
            filled: 0,
            stale: true,
            bar_ticks: 0,
        }
    }

    fn clear(&mut self) {
        self.events.clear();
        self.n_passes = 0;
        self.stale = true;
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn push_pass(&mut self, id: u16) {
        if self.n_passes == MAX_PASSES {
            self.passes.copy_within(1.., 0);
            self.n_passes -= 1;
        }
        self.passes[self.n_passes] = id;
        self.n_passes += 1;
    }

    fn refresh(&mut self, bar_ticks: u32) {
        if self.stale || self.bar_ticks != bar_ticks {
            self.bar_ticks = bar_ticks;
            self.filled = self.events.iter().fold(0, |m, e| {
                m | 1u16.checked_shl(e.tick / bar_ticks.max(1)).unwrap_or(0)
            });
            self.stale = false;
        }
    }
}

struct Lane {
    parts: [Part; 2],
    part: usize,
    pending: Option<usize>,
    mode: LoopMode,
    muted: bool,
    /// The pass being recorded, and whether it holds anything yet.
    pass: u16,
    pass_has: bool,
    next_pass: u16,
    /// Notes that are down in what is being recorded (bit key of the channel's word).
    rec_held: [u128; 16],
    /// Notes the loop has started and not yet ended, and the channels whose pedal it holds down.
    sounding: [u128; 16],
    pedal: u16,
}

impl Lane {
    fn new() -> Self {
        Self {
            parts: [Part::new(), Part::new()],
            part: 0,
            pending: None,
            mode: LoopMode::Idle,
            muted: false,
            pass: 0,
            pass_has: false,
            next_pass: 0,
            rec_held: [0; 16],
            sounding: [0; 16],
            pedal: 0,
        }
    }

    fn part(&mut self) -> &mut Part {
        &mut self.parts[self.part]
    }

    fn rest(&self) -> LoopMode {
        if self.parts[self.part].is_empty() {
            LoopMode::Idle
        } else {
            LoopMode::Play
        }
    }

    fn new_pass(&mut self) {
        self.next_pass = self.next_pass.wrapping_add(1);
        self.pass = self.next_pass;
        self.pass_has = false;
    }

    /// Ends the pass being recorded: notes still down end at `at`, and the pass can be undone.
    fn finish_pass(&mut self, at: u32) {
        let pass = self.pass;
        for ch in 0..16u8 {
            let mut held = self.rec_held[ch as usize];
            while held != 0 {
                let key = held.trailing_zeros() as u8;
                held &= held - 1;
                self.push_event(at, [0x80 | ch, key, 0]);
            }
            self.rec_held[ch as usize] = 0;
        }
        if self.pass_has {
            self.part().push_pass(pass);
            self.pass_has = false;
        }
    }

    fn push_event(&mut self, tick: u32, data: [u8; 3]) {
        let pass = self.pass;
        let part = self.part();
        if part.events.len() < PART_EVENTS {
            part.events.push(LoopEv { tick, pass, data });
            part.stale = true;
            self.pass_has = true;
        }
    }

    /// Ends what the loop is sounding: its notes and pedals go off at frame `time`.
    fn silence(&mut self, eb: &mut EventBuf, time: u32) {
        for ch in 0..16u8 {
            let mut down = self.sounding[ch as usize];
            while down != 0 {
                let key = down.trailing_zeros() as u8;
                down &= down - 1;
                eb.insert(MidiEvent::new(time, 0, [0x80 | ch, key, 0]));
            }
            self.sounding[ch as usize] = 0;
            if self.pedal & (1 << ch) != 0 {
                eb.insert(MidiEvent::new(time, 0, [0xB0 | ch, 64, 0]));
            }
        }
        self.pedal = 0;
    }

    fn track_sounding(&mut self, data: [u8; 3]) {
        let ch = usize::from(data[0] & 15);
        let bit = 1u128 << (data[1] & 127);
        match data[0] & 0xF0 {
            0x90 if data[2] > 0 => self.sounding[ch] |= bit,
            0x80 | 0x90 => self.sounding[ch] &= !bit,
            0xB0 if data[1] == 64 => {
                if data[2] >= 64 {
                    self.pedal |= 1 << ch;
                } else {
                    self.pedal &= !(1 << ch);
                }
            }
            _ => {}
        }
    }

    fn track_held(&mut self, data: [u8; 3]) {
        let ch = usize::from(data[0] & 15);
        let bit = 1u128 << (data[1] & 127);
        match data[0] & 0xF0 {
            0x90 if data[2] > 0 => self.rec_held[ch] |= bit,
            0x80 | 0x90 => self.rec_held[ch] &= !bit,
            _ => {}
        }
    }

    /// One stretch of a block with no loop boundary in it: frames `off..off + len`, which are
    /// ticks `t0..t1` of the loop.
    fn segment(&mut self, eb: &mut EventBuf, off: usize, len: usize, t0: f64, t1: f64, rate: f64) {
        let last = (t1.ceil() as u32).saturating_sub(1);
        if matches!(self.mode, LoopMode::Rec | LoopMode::Over) {
            // What the slot is being played, before the loop adds its own to the list.
            let mut recorded = [MidiEvent::new(0, 0, [0; 3]); 64];
            let mut n = 0;
            for ev in eb.as_slice() {
                let t = ev.time as usize;
                if t >= off
                    && t < off + len
                    && (0x80..0xF0).contains(&ev.data[0])
                    && n < recorded.len()
                {
                    recorded[n] = *ev;
                    n += 1;
                }
            }
            for ev in &recorded[..n] {
                let tick = ((t0 + (ev.time as usize - off) as f64 * rate).round() as u32).min(last);
                self.track_held(ev.data);
                self.push_event(tick, ev.data);
            }
        }
        if matches!(self.mode, LoopMode::Play | LoopMode::Over) && !self.muted {
            let (pass, over) = (self.pass, self.mode == LoopMode::Over);
            let mut started = [LoopEv {
                tick: 0,
                pass: 0,
                data: [0; 3],
            }; 64];
            let mut n = 0;
            for ev in &self.parts[self.part].events {
                let tick = f64::from(ev.tick);
                if tick >= t0 && tick < t1 && !(over && ev.pass == pass) && n < started.len() {
                    started[n] = *ev;
                    n += 1;
                }
            }
            for ev in &started[..n] {
                let at = off + ((f64::from(ev.tick) - t0) / rate) as usize;
                eb.insert(MidiEvent::new(at.min(off + len - 1) as u32, 0, ev.data));
                self.track_sounding(ev.data);
            }
        }
    }
}

/// What the main thread reads: the loop's state, a few atomics per slot.
pub struct LoopShared {
    pub running: AtomicBool,
    /// Where the loop is, in ticks ([`PPQ`] to the beat).
    pub pos: AtomicU32,
    pub bars: AtomicU8,
    pub lanes: [LaneShared; MAX_SLOTS],
}

pub struct LaneShared {
    pub mode: AtomicU8,
    /// 0 for A, 1 for B.
    pub part: AtomicU8,
    /// The part waiting for the loop to come round, or `NO_PART`.
    pub pending: AtomicU8,
    pub muted: AtomicBool,
    /// Per part, which bars have something in them (bit n = bar n).
    pub filled: [AtomicU16; 2],
}

pub const NO_PART: u8 = 255;

impl LoopShared {
    pub(crate) fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            pos: AtomicU32::new(0),
            bars: AtomicU8::new(4),
            lanes: std::array::from_fn(|_| LaneShared {
                mode: AtomicU8::new(LoopMode::Idle as u8),
                part: AtomicU8::new(0),
                pending: AtomicU8::new(NO_PART),
                muted: AtomicBool::new(false),
                filled: [AtomicU16::new(0), AtomicU16::new(0)],
            }),
        }
    }
}

pub struct Looper {
    lanes: Vec<Lane>,
    bars: u8,
    running: bool,
    /// The loop has just started: its first block begins with a boundary.
    fresh: bool,
    /// Ticks into the loop.
    pos: f64,
    /// The loop's length in ticks, as of the latest block.
    len: f64,
}

impl Default for Looper {
    fn default() -> Self {
        Self::new()
    }
}

impl Looper {
    pub fn new() -> Self {
        Self {
            lanes: (0..MAX_SLOTS).map(|_| Lane::new()).collect(),
            bars: 4,
            running: false,
            fresh: false,
            pos: 0.0,
            len: f64::from(4 * 4 * PPQ),
        }
    }

    pub fn bars(&self) -> u8 {
        self.bars
    }

    pub fn mode(&self, slot: usize) -> LoopMode {
        self.lanes[slot].mode
    }

    /// The tick the loop is at, for ending a pass between two boundaries.
    fn now(&self) -> u32 {
        (self.pos as u32).min((self.len as u32).saturating_sub(1))
    }

    pub fn apply(&mut self, cmd: LoopCommand, events: &mut [EventBuf]) {
        let now = self.now();
        let running = self.running;
        match cmd {
            LoopCommand::Rec(i) => {
                let Some(lane) = self.lanes.get_mut(i) else {
                    return;
                };
                match lane.mode {
                    LoopMode::Idle => lane.mode = LoopMode::Armed,
                    LoopMode::Armed => lane.mode = LoopMode::Idle,
                    LoopMode::Rec | LoopMode::Over => {
                        lane.finish_pass(now);
                        lane.mode = lane.rest();
                    }
                    LoopMode::Play => {
                        lane.mode = LoopMode::Over;
                        lane.new_pass();
                    }
                }
            }
            LoopCommand::Part(i, p) => {
                let Some(lane) = self.lanes.get_mut(i).filter(|_| p < 2) else {
                    return;
                };
                if !matches!(lane.mode, LoopMode::Idle | LoopMode::Play) {
                    return;
                }
                if p == lane.part {
                    lane.pending = None;
                } else if running {
                    lane.pending = Some(p);
                } else {
                    lane.silence(&mut events[i], 0);
                    lane.part = p;
                    lane.pending = None;
                    lane.mode = lane.rest();
                }
            }
            LoopCommand::Mute(i, mute) => {
                if let Some(lane) = self.lanes.get_mut(i) {
                    lane.muted = mute;
                    if mute {
                        lane.silence(&mut events[i], 0);
                    }
                }
            }
            LoopCommand::Undo(i) => {
                let Some(lane) = self.lanes.get_mut(i) else {
                    return;
                };
                if lane.mode == LoopMode::Armed {
                    lane.mode = LoopMode::Idle;
                    return;
                }
                if matches!(lane.mode, LoopMode::Rec | LoopMode::Over) {
                    lane.finish_pass(now);
                }
                let part = lane.part();
                if part.n_passes > 0 {
                    part.n_passes -= 1;
                    let id = part.passes[part.n_passes];
                    part.events.retain(|e| e.pass != id);
                    part.stale = true;
                }
                lane.silence(&mut events[i], 0);
                lane.mode = lane.rest();
            }
            LoopCommand::Clear(i) => {
                if let Some(lane) = self.lanes.get_mut(i) {
                    lane.part().clear();
                    lane.rec_held = [0; 16];
                    lane.silence(&mut events[i], 0);
                    lane.pending = None;
                    lane.mode = LoopMode::Idle;
                }
            }
            LoopCommand::ClearAll => self.clear_all(events),
            LoopCommand::SetBars(n) => {
                self.bars = n.clamp(1, MAX_BARS);
                self.clear_all(events);
            }
        }
    }

    fn clear_all(&mut self, events: &mut [EventBuf]) {
        for (i, lane) in self.lanes.iter_mut().enumerate() {
            lane.silence(&mut events[i], 0);
            for p in &mut lane.parts {
                p.clear();
            }
            lane.rec_held = [0; 16];
            lane.pending = None;
            lane.part = 0;
            lane.mode = LoopMode::Idle;
        }
    }

    /// Everything has been silenced from outside (panic): forget what the loops were sounding.
    pub fn forget_sounding(&mut self) {
        for lane in &mut self.lanes {
            lane.sounding = [0; 16];
            lane.pedal = 0;
        }
    }

    /// The transport's run begins: the loop starts from its first beat.
    pub fn start(&mut self, events: &mut [EventBuf]) {
        if self.running {
            self.stop(events);
        }
        self.running = true;
        self.fresh = true;
        self.pos = 0.0;
    }

    /// The transport stopped: recordings end where they are, and the loop goes quiet.
    pub fn stop(&mut self, events: &mut [EventBuf]) {
        let now = self.now();
        for (i, lane) in self.lanes.iter_mut().enumerate() {
            match lane.mode {
                LoopMode::Armed => lane.mode = LoopMode::Idle,
                LoopMode::Rec | LoopMode::Over => {
                    lane.finish_pass(now);
                    lane.mode = lane.rest();
                }
                _ => {}
            }
            lane.silence(&mut events[i], 0);
            if let Some(p) = lane.pending.take() {
                lane.part = p;
                lane.mode = lane.rest();
            }
        }
        self.running = false;
        self.fresh = false;
        self.pos = 0.0;
    }

    /// The loop comes round to beat 1 at frame `time` of the block.
    fn boundary(&mut self, events: &mut [EventBuf], time: u32) {
        let at = (self.len as u32).saturating_sub(1);
        for (i, lane) in self.lanes.iter_mut().enumerate() {
            if matches!(lane.mode, LoopMode::Rec | LoopMode::Over) {
                lane.finish_pass(at);
            }
            if let Some(p) = lane.pending.take() {
                lane.silence(&mut events[i], time);
                lane.part = p;
                lane.mode = lane.rest();
            }
            match lane.mode {
                LoopMode::Armed => {
                    lane.mode = LoopMode::Rec;
                    lane.new_pass();
                }
                LoopMode::Rec => lane.mode = lane.rest(),
                LoopMode::Over => lane.new_pass(),
                _ => {}
            }
        }
    }

    /// Runs the loop over one block: records what the slots were played in it (`events` holds
    /// that already) and adds what the loop plays. The loop runs from frame `from` (where the
    /// transport's run began, else 0).
    pub fn process(
        &mut self,
        events: &mut [EventBuf],
        from: usize,
        frames: usize,
        tempo: f32,
        bar_ticks: u32,
        sample_rate: f32,
    ) {
        self.len = f64::from(u32::from(self.bars) * bar_ticks.max(1));
        if !self.running {
            return;
        }
        let rate = f64::from(tempo) / 60.0 / f64::from(sample_rate) * f64::from(PPQ);
        let mut off = from.min(frames);
        if std::mem::take(&mut self.fresh) {
            self.boundary(events, off.min(frames.saturating_sub(1)) as u32);
        }
        while off < frames {
            if self.pos >= self.len {
                self.pos -= self.len;
            }
            let to_end = (((self.len - self.pos) / rate).ceil() as usize).max(1);
            let seg = (frames - off).min(to_end);
            let (t0, t1) = (self.pos, (self.pos + seg as f64 * rate).min(self.len));
            for (i, lane) in self.lanes.iter_mut().enumerate() {
                lane.segment(&mut events[i], off, seg, t0, t1, rate);
            }
            self.pos += seg as f64 * rate;
            off += seg;
            if self.pos >= self.len - 1e-6 {
                self.pos -= self.len;
                self.boundary(events, off.min(frames - 1) as u32);
            }
        }
    }

    /// Puts the state where the main thread can see it.
    pub fn publish(&mut self, shared: &LoopShared, bar_ticks: u32) {
        let bar_ticks = bar_ticks.max(1);
        shared.running.store(self.running, Ordering::Relaxed);
        shared.pos.store(self.pos as u32, Ordering::Relaxed);
        shared.bars.store(self.bars, Ordering::Relaxed);
        for (lane, out) in self.lanes.iter_mut().zip(&shared.lanes) {
            out.mode.store(lane.mode as u8, Ordering::Relaxed);
            out.part.store(lane.part as u8, Ordering::Relaxed);
            out.pending
                .store(lane.pending.map_or(NO_PART, |p| p as u8), Ordering::Relaxed);
            out.muted.store(lane.muted, Ordering::Relaxed);
            for (part, filled) in lane.parts.iter_mut().zip(&out.filled) {
                part.refresh(bar_ticks);
                filled.store(part.filled, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;
    /// At 120 bpm a beat is 24000 frames, so 4/4 bars of 1 bar is 96000 frames.
    const TEMPO: f32 = 120.0;
    const BLOCK: usize = 480;
    /// A 4/4 bar in ticks.
    const BAR: u32 = 4 * PPQ;

    fn bufs() -> Vec<EventBuf> {
        (0..MAX_SLOTS).map(|_| EventBuf::new()).collect()
    }

    fn note_on(time: u32, key: u8) -> MidiEvent {
        MidiEvent::new(time, 0, [0x90, key, 100])
    }

    fn note_off(time: u32, key: u8) -> MidiEvent {
        MidiEvent::new(time, 0, [0x80, key, 0])
    }

    /// Runs `blocks` blocks of the loop; `live` gives the slot-0 events of block `n` (what the
    /// router would have delivered). Returns what the loop made slot 0 receive, with the absolute
    /// frame of every message.
    fn run(
        l: &mut Looper,
        ev: &mut [EventBuf],
        first_block: usize,
        blocks: usize,
        live: &dyn Fn(usize) -> Vec<MidiEvent>,
    ) -> Vec<(usize, [u8; 3])> {
        let mut out = Vec::new();
        for b in first_block..first_block + blocks {
            for e in ev.iter_mut() {
                e.clear();
            }
            let injected_from = live(b);
            for e in &injected_from {
                ev[0].push(*e);
            }
            let n_live = injected_from.len();
            l.process(ev, 0, BLOCK, TEMPO, BAR, SR);
            for e in ev[0].as_slice() {
                // Live events are the ones we pushed; tell them apart by pointer-free count.
                let _ = n_live;
                out.push((b * BLOCK + e.time as usize, e.data));
            }
        }
        out
    }

    fn started(frames_len: usize) -> (Looper, Vec<EventBuf>) {
        let mut l = Looper::new();
        l.apply(LoopCommand::SetBars(1), &mut bufs());
        let mut ev = bufs();
        l.start(&mut ev);
        let _ = frames_len;
        (l, ev)
    }

    #[test]
    fn a_pass_is_recorded_and_replayed_every_loop() {
        let (mut l, mut ev) = started(0);
        l.apply(LoopCommand::Rec(0), &mut ev);
        let loop_blocks = 96_000 / BLOCK;
        // Record: a note a quarter of the way in, lasting 2400 frames.
        let live = |b: usize| {
            if b == 50 {
                vec![note_on(10, 60)]
            } else if b == 55 {
                vec![note_off(10, 60)]
            } else {
                vec![]
            }
        };
        run(&mut l, &mut ev, 0, loop_blocks, &live);
        assert_eq!(l.mode(0), LoopMode::Play, "one pass, then it plays");
        // The next loop plays it back at the same place.
        let heard = run(&mut l, &mut ev, loop_blocks, loop_blocks, &|_| vec![]);
        let on = loop_blocks * BLOCK + 50 * BLOCK + 10;
        let off = loop_blocks * BLOCK + 55 * BLOCK + 10;
        let near = |got: usize, want: usize| got.abs_diff(want) <= 30;
        assert_eq!(heard.len(), 2, "{heard:?}");
        assert!(
            near(heard[0].0, on) && heard[0].1 == [0x90, 60, 100],
            "{heard:?}"
        );
        assert!(
            near(heard[1].0, off) && heard[1].1 == [0x80, 60, 0],
            "{heard:?}"
        );
        // And again the loop after.
        let again = run(&mut l, &mut ev, 2 * loop_blocks, loop_blocks, &|_| vec![]);
        assert_eq!(again.len(), 2);
    }

    #[test]
    fn rec_waits_for_the_loop_to_come_round() {
        let (mut l, mut ev) = started(0);
        // Run half a loop, press REC, play something: it is not recorded until beat 1.
        run(&mut l, &mut ev, 0, 100, &|_| vec![]);
        l.apply(LoopCommand::Rec(0), &mut ev);
        assert_eq!(l.mode(0), LoopMode::Armed);
        run(&mut l, &mut ev, 100, 100, &|b| {
            if b == 150 {
                vec![note_on(0, 61), note_off(5, 61)]
            } else {
                vec![]
            }
        });
        assert_eq!(l.mode(0), LoopMode::Rec, "recording began at the boundary");
        assert!(
            l.lanes[0].parts[0].is_empty(),
            "the early note was not kept"
        );
    }

    #[test]
    fn a_note_still_down_at_the_end_is_ended() {
        let (mut l, mut ev) = started(0);
        l.apply(LoopCommand::Rec(0), &mut ev);
        let loop_blocks = 96_000 / BLOCK;
        run(&mut l, &mut ev, 0, loop_blocks, &|b| {
            if b == 150 {
                vec![note_on(0, 62)]
            } else {
                vec![]
            }
        });
        let part = &l.lanes[0].parts[0];
        assert!(
            part.events.iter().any(|e| e.data == [0x80, 62, 0]),
            "closed at loop end"
        );
    }

    #[test]
    fn overdub_adds_and_undo_takes_the_last_pass_off() {
        let (mut l, mut ev) = started(0);
        l.apply(LoopCommand::Rec(0), &mut ev);
        let n = 96_000 / BLOCK;
        run(&mut l, &mut ev, 0, n, &|b| {
            if b == 10 {
                vec![note_on(0, 60), note_off(1, 60)]
            } else {
                vec![]
            }
        });
        l.apply(LoopCommand::Rec(0), &mut ev);
        assert_eq!(l.mode(0), LoopMode::Over);
        run(&mut l, &mut ev, n, n, &|b| {
            if b == n + 100 {
                vec![note_on(0, 64), note_off(1, 64)]
            } else {
                vec![]
            }
        });
        l.apply(LoopCommand::Rec(0), &mut ev);
        assert_eq!(l.mode(0), LoopMode::Play);
        assert_eq!(l.lanes[0].parts[0].events.len(), 4);
        l.apply(LoopCommand::Undo(0), &mut ev);
        assert_eq!(
            l.lanes[0].parts[0].events.len(),
            2,
            "the overdub is gone, the take stays"
        );
        l.apply(LoopCommand::Undo(0), &mut ev);
        assert!(l.lanes[0].parts[0].is_empty());
        assert_eq!(l.mode(0), LoopMode::Idle);
    }

    #[test]
    fn the_part_switches_at_the_end_of_the_loop() {
        let (mut l, mut ev) = started(0);
        l.apply(LoopCommand::Rec(0), &mut ev);
        let n = 96_000 / BLOCK;
        run(&mut l, &mut ev, 0, n, &|b| {
            if b == 10 {
                vec![note_on(0, 60), note_off(1, 60)]
            } else {
                vec![]
            }
        });
        l.apply(LoopCommand::Part(0, 1), &mut ev);
        assert_eq!(l.lanes[0].part, 0, "not yet");
        assert_eq!(l.lanes[0].pending, Some(1));
        // A plays out the loop it is in, then B (empty) takes over.
        let heard = run(&mut l, &mut ev, n, n, &|_| vec![]);
        assert_eq!(heard.len(), 2, "{heard:?}");
        assert_eq!(l.lanes[0].part, 1);
        assert_eq!(l.mode(0), LoopMode::Idle);
        let after = run(&mut l, &mut ev, 2 * n, n, &|_| vec![]);
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn stopping_ends_notes_the_loop_was_holding() {
        let (mut l, mut ev) = started(0);
        l.apply(LoopCommand::Rec(0), &mut ev);
        let n = 96_000 / BLOCK;
        run(&mut l, &mut ev, 0, n, &|b| {
            if b == 10 {
                vec![note_on(0, 60)]
            } else if b == 190 {
                vec![note_off(0, 60)]
            } else {
                vec![]
            }
        });
        // Play up to a point inside the note, then stop.
        run(&mut l, &mut ev, n, 50, &|_| vec![]);
        for e in ev.iter_mut() {
            e.clear();
        }
        l.stop(&mut ev);
        assert!(ev[0].as_slice().iter().any(|e| e.data == [0x80, 60, 0]));
    }

    #[test]
    fn it_records_nothing_while_the_loop_is_not_running() {
        let mut l = Looper::new();
        let mut ev = bufs();
        l.apply(LoopCommand::Rec(0), &mut ev);
        for _ in 0..10 {
            ev[0].push(note_on(0, 60));
            l.process(&mut ev, 0, BLOCK, TEMPO, BAR, SR);
            ev[0].clear();
        }
        assert!(l.lanes[0].parts[0].is_empty());
        assert_eq!(l.mode(0), LoopMode::Armed);
    }

    #[test]
    fn publishes_which_bars_have_notes() {
        let mut l = Looper::new();
        let mut ev = bufs();
        l.start(&mut ev);
        l.apply(LoopCommand::Rec(0), &mut ev);
        let n = 4 * 96_000 / BLOCK;
        // A note in the second bar (4/4 at 120 bpm: bars are 96000 frames long).
        run(&mut l, &mut ev, 0, n, &|b| {
            if b == 96_000 / BLOCK + 20 {
                vec![note_on(0, 60), note_off(1, 60)]
            } else {
                vec![]
            }
        });
        let shared = LoopShared::new();
        l.publish(&shared, BAR);
        assert_eq!(shared.lanes[0].filled[0].load(Ordering::Relaxed), 0b0010);
        assert_eq!(
            shared.lanes[0].mode.load(Ordering::Relaxed),
            LoopMode::Play as u8
        );
    }

    #[test]
    fn a_six_eight_bar_is_three_quarter_notes_long() {
        let mut l = Looper::new();
        let mut ev = bufs();
        l.start(&mut ev);
        l.apply(LoopCommand::Rec(0), &mut ev);
        // 6/8: six eighths, so a bar is 3 quarters = 72000 frames at 120 bpm.
        let bar = 6 * (PPQ / 2);
        let frames_per_bar = 72_000;
        let at = frames_per_bar / BLOCK + 20;
        for b in 0..4 * frames_per_bar / BLOCK {
            if b == at {
                ev[0].push(note_on(0, 60));
                ev[0].push(note_off(1, 60));
            }
            l.process(&mut ev, 0, BLOCK, TEMPO, bar, SR);
            ev[0].clear();
        }
        let shared = LoopShared::new();
        l.publish(&shared, bar);
        assert_eq!(shared.lanes[0].filled[0].load(Ordering::Relaxed), 0b0010);
        assert_eq!(
            shared.lanes[0].mode.load(Ordering::Relaxed),
            LoopMode::Play as u8,
            "the pass ended after four 6/8 bars"
        );
    }
}
