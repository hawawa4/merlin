//! The transport (plan §5): a tempo clock with a count-in, a click, and MIDI clock out.
//!
//! The audio thread owns the [`Transport`]. The main thread sets the tempo, the click and the
//! start/stop requests through [`TransportShared`] atomics, and reads the position back from
//! them. Everything is sample-accurate inside a block: the clock is 24 ticks per beat and every
//! beat (every 24th tick) is where the click sounds and the bar counter moves.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

const TICKS_PER_BEAT: u32 = 24;
/// The click: two recorded samples (mono, 16-bit, 24 kHz; the same ones Lyrica uses), the louder
/// one on the first beat of a bar. Decoded and resampled once, when the transport is made.
const CLICK_WAV: &[u8] = include_bytes!("../assets/metronome_click.wav");
const ACCENT_WAV: &[u8] = include_bytes!("../assets/metronome_accent.wav");

/// A canonical 44-byte-header WAV as f32 samples at `out_rate` (linear interpolation).
fn decode_click(wav: &[u8], out_rate: f32) -> Vec<f32> {
    let src_rate = u32::from_le_bytes(wav[24..28].try_into().unwrap()) as f64;
    let pcm: Vec<f32> = wav[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b) as f32 / 32768.0)
        .collect();
    let ratio = src_rate / f64::from(out_rate);
    let len = (pcm.len() as f64 / ratio) as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = pcm[idx];
            let b = pcm.get(idx + 1).copied().unwrap_or(0.0);
            a + (b - a) * frac
        })
        .collect()
}

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
    /// The free-running metronome: clicks at the tempo even while the transport is stopped.
    metronome_on: AtomicBool,
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
            metronome_on: AtomicBool::new(false),
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

    pub fn set_metronome(&self, on: bool) {
        self.metronome_on.store(on, Ordering::Relaxed);
    }

    pub fn metronome(&self) -> bool {
        self.metronome_on.load(Ordering::Relaxed)
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
    /// The normal and the accent click, at the output rate.
    clicks: [Vec<f32>; 2],
    /// Frames of the current click played so far, which one, and its volume.
    click_pos: usize,
    click_accent: bool,
    click_active: bool,
    click_gain: f32,
    /// The free-running metronome's clock while the transport is stopped: frames until its next
    /// beat (`None` when it is not running) and the beat in the bar.
    metro_until: Option<f64>,
    metro_beat: u32,
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
            clicks: [
                decode_click(CLICK_WAV, sample_rate),
                decode_click(ACCENT_WAV, sample_rate),
            ],
            click_pos: 0,
            click_accent: false,
            click_active: false,
            click_gain: 0.0,
            metro_until: None,
            metro_beat: 0,
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
        self.click_accent = accent;
        // Both recordings are normalised to the same peak: the accent is louder by this.
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
        let metronome = shared.metronome_on.load(Ordering::Relaxed);
        // The metronome is heard whatever the transport's own click is set to.
        let click_on = shared.click_on.load(Ordering::Relaxed) || metronome;
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
        // Stopped: the metronome keeps its own beat. A run takes the click over with its clock.
        if self.until_tick.is_some() || !metronome {
            self.metro_until = None;
        } else {
            let tempo = f64::from(shared.tempo());
            let per_beat = f64::from(self.sample_rate) * 60.0 / tempo;
            let mut until = *self.metro_until.get_or_insert_with(|| {
                self.metro_beat = 0;
                0.0
            });
            while until < frames as f64 {
                if n_triggers < triggers.len() {
                    triggers[n_triggers] = (until.max(0.0) as u32, self.metro_beat == 0);
                    n_triggers += 1;
                }
                self.metro_beat = (self.metro_beat + 1) % bpb;
                until += per_beat;
            }
            self.metro_until = Some(until - frames as f64);
        }
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

        let mut next_trigger = 0;
        for t in 0..frames {
            while next_trigger < n_triggers && triggers[next_trigger].0 as usize == t {
                self.trigger_click(triggers[next_trigger].1, volume);
                next_trigger += 1;
            }
            if self.click_active {
                let sample = &self.clicks[usize::from(self.click_accent)];
                let Some(&raw) = sample.get(self.click_pos) else {
                    self.click_active = false;
                    continue;
                };
                // The recordings are normalised to nearly full scale; the volume is the level.
                let s = raw * self.click_gain;
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
        // A recorded click has zero crossings inside it: a new one is sound after a real silence.
        let mut silent_for = usize::MAX / 2;
        for b in 0..blocks {
            t.render(shared, &mut l, &mut r, &mut clock);
            events.extend(
                clock
                    .iter()
                    .map(|e| (b * frames + e.time as usize, e.bytes[0])),
            );
            for (i, &s) in l.iter().enumerate() {
                if s == 0.0 {
                    silent_for += 1;
                } else {
                    if silent_for >= 1000 {
                        clicks.push(b * frames + i);
                    }
                    silent_for = 0;
                }
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
        // (A recorded click starts a few frames in, from its own pre-roll.)
        assert!(clicks[0] <= 200);
        assert!((24_000..=24_200).contains(&clicks[1]), "{clicks:?}");
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

    #[test]
    fn the_metronome_clicks_at_the_tempo_while_stopped_and_accents_the_bar() {
        let shared = TransportShared::new();
        shared.set_tempo(120.0);
        shared.set_click(false, 0.5); // the transport's own click is off: the metronome is separate
        shared.set_metronome(true);
        let mut t = Transport::new(SR);
        // 120 bpm = a click every 24000 frames; 2.5 s covers beats at 0, 24000, 48000, 72000, 96000.
        let (l, mut r, mut clock) = (vec![0.0; 128], vec![0.0; 128], Vec::new());
        let mut l = l;
        let mut starts = Vec::new();
        let mut peaks = Vec::new();
        let mut silent_for = usize::MAX / 2;
        let mut peak = 0.0f32;
        for b in 0..(120_000 / 128) {
            t.render(&shared, &mut l, &mut r, &mut clock);
            for (i, &s) in l.iter().enumerate() {
                if s == 0.0 {
                    silent_for += 1;
                    continue;
                }
                if silent_for >= 1000 {
                    if !starts.is_empty() {
                        peaks.push(peak);
                    }
                    peak = 0.0;
                    starts.push(b * 128 + i);
                }
                silent_for = 0;
                peak = peak.max(s.abs());
            }
        }
        peaks.push(peak);
        assert!(
            clock.is_empty() || !clock.iter().any(|e| e.bytes[0] == MIDI_CLOCK),
            "no MIDI clock from a metronome"
        );
        assert_eq!(starts.len(), 5, "{starts:?}");
        for (n, s) in starts.iter().enumerate() {
            assert!((n * 24_000..=n * 24_000 + 200).contains(s), "{starts:?}");
        }
        assert!(!shared.running());
        // Beat one of the 4/4 bar is the accent: louder than the beats after it.
        assert!(peaks[0] > peaks[1], "{peaks:?}");
        assert!(peaks[4] > peaks[3], "{peaks:?}");

        // Switched off, it falls silent.
        shared.set_metronome(false);
        let (_, clicks) = run(&mut t, &shared, 400, 128);
        assert!(clicks.is_empty() || clicks[0] < 128 * 200, "{clicks:?}");
        let (_, clicks) = run(&mut t, &shared, 400, 128);
        assert!(clicks.is_empty(), "{clicks:?}");
    }

    #[test]
    fn a_run_takes_the_click_over_from_the_metronome_without_doubling_it() {
        let shared = TransportShared::new();
        shared.set_tempo(120.0);
        shared.set_metronome(true);
        let mut t = Transport::new(SR);
        run(&mut t, &shared, 100, 128);
        shared.start(0);
        // 2 s of run: beats at 0, 0.5, 1.0, 1.5 s; one click each, not two.
        let (_, clicks) = run(&mut t, &shared, 750, 128);
        assert_eq!(clicks.len(), 4, "{clicks:?}");
    }
}
