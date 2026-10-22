//! The transport (plan §5): a tempo clock with a count-in, a click, and MIDI clock out.
//!
//! The audio thread owns the [`Transport`]. The main thread sets the tempo, the click and the
//! start/stop requests through [`TransportShared`] atomics, and reads the position back from
//! them. Everything is sample-accurate inside a block: the clock is 24 ticks per beat and every
//! beat (every 24th tick) is where the click sounds and the bar counter moves.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

const TICKS_PER_BEAT: u32 = 24;
/// The click: a short decaying sine, higher on the first beat of a bar.
const CLICK_SECS: f32 = 0.04;
const CLICK_FREQ: f32 = 1000.0;
const CLICK_ACCENT_FREQ: f32 = 1500.0;

const REQ_NONE: u8 = 0;
const REQ_START: u8 = 1;
const REQ_STOP: u8 = 2;

/// MIDI real-time bytes.
pub const MIDI_CLOCK: u8 = 0xF8;
pub const MIDI_START: u8 = 0xFA;
pub const MIDI_STOP: u8 = 0xFC;

/// What the main thread controls and reads.
pub struct TransportShared {
    tempo: AtomicU32,
    beats_per_bar: AtomicU8,
    count_in_beats: AtomicU8,
    click_on: AtomicBool,
    click_volume: AtomicU32,
    /// Mix the click into the main outputs (else it goes to its own output ports).
    click_to_main: AtomicBool,
    request: AtomicU8,
    running: AtomicBool,
    counting_in: AtomicBool,
    bar: AtomicU32,
    beat: AtomicU32,
    /// Counts every beat the click has marked since the app started, to see it is alive.
    beats_total: AtomicU32,
}

impl TransportShared {
    pub(crate) fn new() -> Self {
        Self {
            tempo: AtomicU32::new(120f32.to_bits()),
            beats_per_bar: AtomicU8::new(4),
            count_in_beats: AtomicU8::new(0),
            click_on: AtomicBool::new(true),
            click_volume: AtomicU32::new(0.5f32.to_bits()),
            click_to_main: AtomicBool::new(true),
            request: AtomicU8::new(REQ_NONE),
            running: AtomicBool::new(false),
            counting_in: AtomicBool::new(false),
            bar: AtomicU32::new(0),
            beat: AtomicU32::new(0),
            beats_total: AtomicU32::new(0),
        }
    }

    pub fn set_tempo(&self, bpm: f32) {
        self.tempo
            .store(bpm.clamp(20.0, 300.0).to_bits(), Ordering::Relaxed);
    }

    pub fn tempo(&self) -> f32 {
        f32::from_bits(self.tempo.load(Ordering::Relaxed))
    }

    pub fn set_beats_per_bar(&self, beats: u8) {
        self.beats_per_bar
            .store(beats.clamp(1, 16), Ordering::Relaxed);
    }

    pub fn set_click(&self, on: bool, volume: f32) {
        self.click_on.store(on, Ordering::Relaxed);
        self.click_volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_click_to_main(&self, on: bool) {
        self.click_to_main.store(on, Ordering::Relaxed);
    }

    pub fn click_to_main(&self) -> bool {
        self.click_to_main.load(Ordering::Relaxed)
    }

    /// Starts after `count_in_beats` beats of click (0 starts at once).
    pub fn start(&self, count_in_beats: u8) {
        self.count_in_beats.store(count_in_beats, Ordering::Relaxed);
        self.request.store(REQ_START, Ordering::Release);
    }

    pub fn stop(&self) {
        self.request.store(REQ_STOP, Ordering::Release);
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn counting_in(&self) -> bool {
        self.counting_in.load(Ordering::Relaxed)
    }

    /// The bar and beat (both from 1) of the last beat that sounded; (0, 0) when stopped.
    pub fn position(&self) -> (u32, u32) {
        (
            self.bar.load(Ordering::Relaxed),
            self.beat.load(Ordering::Relaxed),
        )
    }

    pub fn beats_total(&self) -> u32 {
        self.beats_total.load(Ordering::Relaxed)
    }
}

/// A MIDI real-time byte to send `time` frames into the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockEvent {
    pub time: u32,
    pub byte: u8,
}

pub struct Transport {
    sample_rate: f32,
    /// Frames until the next clock tick; `None` while stopped.
    until_tick: Option<f64>,
    tick: u32,
    count_in_left: u32,
    running: bool,
    counting: bool,
    beat_in_bar: u32,
    bar: u32,
    /// Frames of the current click played so far, its pitch and its volume.
    click_pos: usize,
    click_freq: f32,
    click_active: bool,
    click_gain: f32,
}

impl Transport {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            until_tick: None,
            tick: 0,
            count_in_left: 0,
            running: false,
            counting: false,
            beat_in_bar: 0,
            bar: 0,
            click_pos: 0,
            click_freq: CLICK_FREQ,
            click_active: false,
            click_gain: 0.0,
        }
    }

    fn trigger_click(&mut self, accent: bool, volume: f32) {
        self.click_active = true;
        self.click_pos = 0;
        self.click_freq = if accent {
            CLICK_ACCENT_FREQ
        } else {
            CLICK_FREQ
        };
        self.click_gain = volume * if accent { 1.0 } else { 0.7 };
    }

    /// Renders the click into `click_l`/`click_r` (overwritten) and the clock bytes into
    /// `clock` (cleared first). Allocation-free as long as `clock` has capacity.
    pub fn render(
        &mut self,
        shared: &TransportShared,
        click_l: &mut [f32],
        click_r: &mut [f32],
        clock: &mut Vec<ClockEvent>,
    ) {
        let frames = click_l.len().min(click_r.len());
        clock.clear();
        click_l[..frames].fill(0.0);
        click_r[..frames].fill(0.0);
        let bpb = u32::from(shared.beats_per_bar.load(Ordering::Relaxed)).max(1);
        let click_on = shared.click_on.load(Ordering::Relaxed);
        let volume = f32::from_bits(shared.click_volume.load(Ordering::Relaxed));

        match shared.request.swap(REQ_NONE, Ordering::Acquire) {
            REQ_START => {
                self.until_tick = Some(0.0);
                self.tick = 0;
                self.count_in_left = u32::from(shared.count_in_beats.load(Ordering::Relaxed));
                self.running = false;
                self.counting = self.count_in_left > 0;
                self.beat_in_bar = 0;
                self.bar = 0;
                self.click_active = false;
            }
            REQ_STOP => {
                if self.until_tick.is_some() {
                    clock.push(ClockEvent {
                        time: 0,
                        byte: MIDI_STOP,
                    });
                }
                self.until_tick = None;
                self.running = false;
                self.counting = false;
                self.click_active = false;
                self.bar = 0;
                self.beat_in_bar = 0;
            }
            _ => {}
        }

        // Where a click starts in this block (at most a few: a beat is longer than a block).
        let mut triggers = [(0u32, false); 4];
        let mut n_triggers = 0;
        if let Some(mut until) = self.until_tick {
            let tempo = f64::from(shared.tempo());
            let per_tick = f64::from(self.sample_rate) * 60.0 / tempo / f64::from(TICKS_PER_BEAT);
            while until < frames as f64 {
                let at = until.max(0.0) as u32;
                if self.tick.is_multiple_of(TICKS_PER_BEAT) {
                    let accent = self.on_beat(bpb, at, clock);
                    shared.beats_total.fetch_add(1, Ordering::Relaxed);
                    if click_on && n_triggers < triggers.len() {
                        triggers[n_triggers] = (at, accent);
                        n_triggers += 1;
                    }
                    // The position is that of the beat that sounded last.
                    shared.bar.store(self.bar, Ordering::Relaxed);
                    shared.beat.store(self.beat_in_bar + 1, Ordering::Relaxed);
                }
                clock.push(ClockEvent {
                    time: at,
                    byte: MIDI_CLOCK,
                });
                self.tick = self.tick.wrapping_add(1);
                until += per_tick;
            }
            self.until_tick = Some(until - frames as f64);
        }

        let len = (CLICK_SECS * self.sample_rate) as usize;
        let mut next_trigger = 0;
        for t in 0..frames {
            while next_trigger < n_triggers && triggers[next_trigger].0 as usize == t {
                self.trigger_click(triggers[next_trigger].1, volume);
                next_trigger += 1;
            }
            if self.click_active {
                if self.click_pos >= len {
                    self.click_active = false;
                    continue;
                }
                let env = 1.0 - self.click_pos as f32 / len as f32;
                let w = std::f32::consts::TAU * self.click_freq / self.sample_rate;
                let s = (w * self.click_pos as f32).sin() * env * env * self.click_gain;
                click_l[t] = s;
                click_r[t] = s;
                self.click_pos += 1;
            }
        }
        shared.running.store(self.running, Ordering::Relaxed);
        shared.counting_in.store(self.counting, Ordering::Relaxed);
        if self.until_tick.is_none() {
            shared.bar.store(0, Ordering::Relaxed);
            shared.beat.store(0, Ordering::Relaxed);
        }
    }

    /// A beat begins: moves the count-in or the bar counter on. Returns whether it is the first
    /// beat of a bar (the accent).
    fn on_beat(&mut self, bpb: u32, at: u32, clock: &mut Vec<ClockEvent>) -> bool {
        if self.count_in_left > 0 {
            let accent = self.beat_in_bar == 0;
            self.bar = 0;
            self.beat_in_bar = (self.beat_in_bar + 1) % bpb;
            self.count_in_left -= 1;
            self.counting = true;
            accent
        } else if !self.running {
            // The count-in is over: this beat is bar 1, beat 1.
            self.running = true;
            self.counting = false;
            self.bar = 1;
            self.beat_in_bar = 0;
            clock.push(ClockEvent {
                time: at,
                byte: MIDI_START,
            });
            true
        } else {
            self.beat_in_bar += 1;
            if self.beat_in_bar >= bpb {
                self.beat_in_bar = 0;
                self.bar += 1;
            }
            self.beat_in_bar == 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    /// Renders `blocks` blocks of `frames` frames; returns every clock event with its absolute
    /// frame, and the absolute frames where a click starts (a jump from silence to sound).
    fn run(
        t: &mut Transport,
        shared: &TransportShared,
        blocks: usize,
        frames: usize,
    ) -> (Vec<(usize, u8)>, Vec<usize>) {
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        let mut clock = Vec::with_capacity(256);
        let (mut events, mut clicks) = (Vec::new(), Vec::new());
        let mut was_silent = true;
        for b in 0..blocks {
            t.render(shared, &mut l, &mut r, &mut clock);
            events.extend(clock.iter().map(|e| (b * frames + e.time as usize, e.byte)));
            for (i, &s) in l.iter().enumerate() {
                let silent = s == 0.0;
                if was_silent && !silent {
                    clicks.push(b * frames + i);
                }
                // A click starts from a sine's zero: look for first non-zero after silence.
                was_silent = silent;
            }
        }
        (events, clicks)
    }

    #[test]
    fn stopped_means_silent_and_quiet() {
        let shared = TransportShared::new();
        let mut t = Transport::new(SR);
        let (events, clicks) = run(&mut t, &shared, 20, 256);
        assert!(events.is_empty() && clicks.is_empty());
        assert!(!shared.running());
    }

    #[test]
    fn the_clock_runs_24_ticks_a_beat_and_start_comes_first() {
        let shared = TransportShared::new();
        shared.set_tempo(120.0);
        let mut t = Transport::new(SR);
        shared.start(0);
        // One second at 120 bpm: 2 beats = 48 ticks (tick 0 at frame 0 ... tick 47).
        let (events, _) = run(&mut t, &shared, 375, 128);
        assert_eq!(events[0], (0, MIDI_START));
        let ticks: Vec<_> = events.iter().filter(|e| e.1 == MIDI_CLOCK).collect();
        assert_eq!(ticks.len(), 48);
        // 120 bpm at 48 kHz: a tick every 1000 frames.
        assert_eq!(ticks[1].0 - ticks[0].0, 1000);
        assert_eq!(ticks[24].0, 24_000);
        assert!(shared.running());
        assert_eq!(shared.position(), (1, 2), "second beat of bar 1");
    }

    #[test]
    fn a_count_in_clicks_before_the_run_starts() {
        let shared = TransportShared::new();
        shared.set_tempo(120.0);
        let mut t = Transport::new(SR);
        shared.start(4);
        // 4 count-in beats = 2 seconds; the run starts on the beat after them.
        let (events, clicks) = run(&mut t, &shared, 1000, 128);
        let start = events.iter().find(|e| e.1 == MIDI_START).unwrap().0;
        assert_eq!(start, 4 * 24_000, "start after four beats");
        assert!(clicks.len() >= 4, "the count-in clicked: {clicks:?}");
        // (A click's sine starts at zero, so the first sound is a frame in.)
        assert!(clicks[0] <= 1);
        assert!((24_000..=24_001).contains(&clicks[1]), "{clicks:?}");
    }

    #[test]
    fn bars_count_and_stop_says_stop() {
        let shared = TransportShared::new();
        shared.set_tempo(240.0);
        shared.set_beats_per_bar(3);
        let mut t = Transport::new(SR);
        shared.start(0);
        // 240 bpm: a beat is 12000 frames. After 4 beats: bar 2, beat 2 (3 beats a bar).
        run(&mut t, &shared, 1, 12_000 * 3 + 6_000);
        assert_eq!(shared.position(), (2, 1));
        shared.stop();
        let (events, _) = run(&mut t, &shared, 4, 128);
        assert_eq!(events, vec![(0, MIDI_STOP)]);
        assert!(!shared.running());
        assert_eq!(shared.position(), (0, 0));
    }

    #[test]
    fn the_click_can_be_muted_but_the_clock_runs() {
        let shared = TransportShared::new();
        shared.set_click(false, 0.5);
        let mut t = Transport::new(SR);
        shared.start(0);
        let (events, clicks) = run(&mut t, &shared, 100, 256);
        assert!(events.len() > 10 && clicks.is_empty());
    }

    #[test]
    fn a_tempo_change_takes_effect_on_the_next_tick() {
        let shared = TransportShared::new();
        shared.set_tempo(120.0);
        let mut t = Transport::new(SR);
        shared.start(0);
        run(&mut t, &shared, 4, 256);
        shared.set_tempo(240.0);
        let (events, _) = run(&mut t, &shared, 20, 256);
        let ticks: Vec<_> = events.iter().filter(|e| e.1 == MIDI_CLOCK).collect();
        let gaps: Vec<usize> = ticks.windows(2).map(|w| w[1].0 - w[0].0).collect();
        assert!(gaps.iter().all(|&g| (499..=501).contains(&g)), "{gaps:?}");
    }
}
