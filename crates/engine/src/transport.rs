//! The transport (plan §5): a tempo clock with a count-in, a click, and MIDI clock out.
//!
//! The audio thread owns the [`Transport`]. The main thread sets the tempo, the click and the
//! start/stop requests through [`TransportShared`] atomics, and reads the position back from
//! them. Everything is sample-accurate inside a block: the clock is 24 ticks per beat and every
//! beat (every 24th tick) is where the click sounds and the bar counter moves.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

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
    /// Frames since the run started (not counting the count-in).
    run_frames: AtomicU64,
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
            run_frames: AtomicU64::new(0),
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

    /// Seconds since the run started (the count-in does not count).
    pub fn run_seconds(&self, sample_rate: f32) -> f64 {
        self.run_frames.load(Ordering::Relaxed) as f64 / f64::from(sample_rate)
    }

    pub fn beats_total(&self) -> u32 {
        self.beats_total.load(Ordering::Relaxed)
    }
}

/// A MIDI message to send `time` frames into the block: the clock and start/stop bytes, and
/// what timeline markers send (a program change, a controller).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiOutEvent {
    pub time: u32,
    pub bytes: [u8; 3],
    pub len: u8,
}

impl MidiOutEvent {
    pub fn byte(time: u32, byte: u8) -> Self {
        Self {
            time,
            bytes: [byte, 0, 0],
            len: 1,
        }
    }

    pub fn data(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// A marker on the song's timeline as the audio thread sees it: when it fires (in clock ticks
/// since the run started), what to tell the main thread, and what to send on MIDI, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineMarker {
    pub tick: u64,
    pub id: u16,
    pub midi: Option<([u8; 3], u8)>,
}

/// Most markers a song can have.
pub const MAX_MARKERS: usize = 128;

/// What happened in a block that the rest of the engine has to act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransportEvents {
    /// The run began at this frame of the block (backing tracks start here).
    pub run_start: Option<u32>,
    /// A stop was handled at the start of the block.
    pub stopped: bool,
}

pub struct Transport {
    sample_rate: f32,
    run_start: Option<u32>,
    run_frames: u64,
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
    /// Timeline markers, in tick order, and the next one to fire.
    markers: Vec<EngineMarker>,
    next_marker: usize,
    /// Ticks since the run started.
    run_tick: u64,
    /// Markers that fired in the last block.
    fired: Vec<u16>,
}

impl Transport {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            run_start: None,
            run_frames: 0,
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
            markers: Vec::with_capacity(MAX_MARKERS),
            next_marker: 0,
            run_tick: 0,
            fired: Vec::with_capacity(MAX_MARKERS),
        }
    }

    /// Adds a marker (kept in tick order). Ignored beyond [`MAX_MARKERS`].
    pub fn add_marker(&mut self, marker: EngineMarker) {
        if self.markers.len() < MAX_MARKERS {
            let at = self.markers.partition_point(|m| m.tick <= marker.tick);
            self.markers.insert(at, marker);
        }
    }

    pub fn clear_markers(&mut self) {
        self.markers.clear();
        self.next_marker = 0;
    }

    /// The ids of the markers that fired in the last [`render`](Self::render).
    pub fn fired(&self) -> &[u16] {
        &self.fired
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
        clock: &mut Vec<MidiOutEvent>,
    ) -> TransportEvents {
        let mut events = TransportEvents::default();
        self.run_start = None;
        self.fired.clear();
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
                self.run_frames = 0;
            }
            REQ_STOP => {
                events.stopped = self.until_tick.is_some();
                if self.until_tick.is_some() {
                    clock.push(MidiOutEvent::byte(0, MIDI_STOP));
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
                if self.running {
                    while self.next_marker < self.markers.len()
                        && self.markers[self.next_marker].tick <= self.run_tick
                    {
                        let m = self.markers[self.next_marker];
                        self.next_marker += 1;
                        if let Some((bytes, len)) = m.midi {
                            clock.push(MidiOutEvent {
                                time: at,
                                bytes,
                                len,
                            });
                        }
                        if self.fired.len() < self.fired.capacity() {
                            self.fired.push(m.id);
                        }
                    }
                    self.run_tick += 1;
                }
                clock.push(MidiOutEvent::byte(at, MIDI_CLOCK));
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
        events.run_start = self.run_start;
        if let Some(at) = self.run_start {
            self.run_frames = (frames as u64).saturating_sub(u64::from(at));
        } else if self.running {
            self.run_frames += frames as u64;
        }
        shared.run_frames.store(self.run_frames, Ordering::Relaxed);
        shared.running.store(self.running, Ordering::Relaxed);
        shared.counting_in.store(self.counting, Ordering::Relaxed);
        if self.until_tick.is_none() {
            shared.bar.store(0, Ordering::Relaxed);
            shared.beat.store(0, Ordering::Relaxed);
        }
        events
    }

    /// A beat begins: moves the count-in or the bar counter on. Returns whether it is the first
    /// beat of a bar (the accent).
    fn on_beat(&mut self, bpb: u32, at: u32, clock: &mut Vec<MidiOutEvent>) -> bool {
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
            self.run_tick = 0;
            self.next_marker = 0;
            self.run_start = Some(at);
            self.counting = false;
            self.bar = 1;
            self.beat_in_bar = 0;
            clock.push(MidiOutEvent::byte(at, MIDI_START));
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
            events.extend(
                clock
                    .iter()
                    .map(|e| (b * frames + e.time as usize, e.bytes[0])),
            );
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

    #[test]
    fn markers_fire_on_their_tick_and_send_their_midi_in_order_with_the_clock() {
        let shared = TransportShared::new();
        shared.set_tempo(120.0);
        let mut t = Transport::new(SR);
        // Bar 1 beat 1 (the run start), and 2 beats later with a program change.
        t.add_marker(EngineMarker {
            tick: 2 * 24,
            id: 7,
            midi: Some(([0xC1, 5, 0], 2)),
        });
        t.add_marker(EngineMarker {
            tick: 0,
            id: 3,
            midi: None,
        });
        shared.start(0);
        let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
        let mut clock = Vec::with_capacity(256);
        let mut fired = Vec::new();
        let mut pc_at = None;
        let mut last_time = 0usize;
        for b in 0..200 {
            t.render(&shared, &mut l, &mut r, &mut clock);
            // Events leave in time order, or JACK refuses them.
            for e in &clock {
                assert!(b * 512 + e.time as usize >= last_time);
                last_time = b * 512 + e.time as usize;
                if e.bytes[0] == 0xC1 {
                    pc_at = Some((last_time, e.data().to_vec()));
                }
            }
            fired.extend(t.fired().iter().map(|&id| (b * 512, id)));
        }
        assert_eq!(fired.iter().map(|f| f.1).collect::<Vec<_>>(), vec![3, 7]);
        // 120 bpm: two beats = 48000 frames; the program change is on that tick.
        let (at, bytes) = pc_at.expect("no program change was sent");
        assert!((47_999..=48_001).contains(&at), "{at}");
        assert_eq!(bytes, vec![0xC1, 5]);
        // Stopped and started again: the markers fire again from the top.
        shared.stop();
        t.render(&shared, &mut l, &mut r, &mut clock);
        shared.start(0);
        let mut again = Vec::new();
        for _ in 0..200 {
            t.render(&shared, &mut l, &mut r, &mut clock);
            again.extend_from_slice(t.fired());
        }
        assert_eq!(again, vec![3, 7]);
        t.clear_markers();
        shared.stop();
        t.render(&shared, &mut l, &mut r, &mut clock);
        shared.start(0);
        t.render(&shared, &mut l, &mut r, &mut clock);
        assert!(t.fired().is_empty(), "cleared markers stay quiet");
    }
}
