//! A rolling MIDI capture: the app always listens, keeps the last few minutes of what was
//! played, and writes them out as a Standard MIDI File when asked.
//!
//! Not a DAW feature: there is no transport, no tracks to arm, nothing to start. The audio
//! thread holds a [`Tap`] and pushes every message that reaches the instruments into a
//! lock-free queue. A small thread moves them into a [`VecDeque`] and drops what is older than
//! the window, so the newest moment replaces the oldest one. [`Recorder::capture`] copies what
//! is there; [`Capture::to_smf`] turns it into a file.
//!
//! Time is counted in audio frames (the tap is told how long each cycle was), so it follows the
//! sound card's clock and not the wall clock.

mod smf;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

pub use smf::{SmfOptions, TICKS_PER_BEAT};

/// How many events the buffer keeps at most, whatever the window: 16 bytes each, so 16 MB. Half an
/// hour of dense playing with a wheel moving all the time is well under it; past it the oldest
/// events go first, like they do at the end of the window.
pub const MAX_EVENTS: usize = 1_000_000;
/// Events the audio thread can have waiting for the thread that stores them.
const QUEUE: usize = 32 * 1024;
/// How often the storing thread wakes up.
const PUMP_INTERVAL: Duration = Duration::from_millis(250);

/// One channel message and the frame it arrived at (counted from when the recorder started).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub frame: u64,
    /// Which MIDI input it came from (the engine's input index).
    pub input: u8,
    pub data: [u8; 3],
}

struct Inner {
    rx: rtrb::Consumer<Event>,
    events: VecDeque<Event>,
}

struct Shared {
    /// Frames the audio thread has gone through.
    frame: AtomicU64,
    window_frames: AtomicU64,
    /// Events the queue had no room for.
    dropped: AtomicU32,
    /// Events in the buffer, as of the last time the storing thread ran.
    stored: AtomicUsize,
    stop: AtomicBool,
    sample_rate: u32,
    inner: Mutex<Inner>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock cannot leave the buffer half-written in a way that
        // matters here; keep serving.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes in what the audio thread queued and lets the oldest events go.
    fn pump(&self, inner: &mut Inner) {
        while let Ok(ev) = inner.rx.pop() {
            inner.events.push_back(ev);
        }
        // Read after the queue was emptied: the clock never runs behind the events.
        let now = self.frame.load(Ordering::Acquire);
        let cutoff = now.saturating_sub(self.window_frames.load(Ordering::Relaxed));
        while inner.events.front().is_some_and(|e| e.frame < cutoff) {
            inner.events.pop_front();
        }
        while inner.events.len() > MAX_EVENTS {
            inner.events.pop_front();
        }
        self.stored.store(inner.events.len(), Ordering::Relaxed);
    }
}

/// The audio thread's end: no locks, no allocation.
pub struct Tap {
    tx: rtrb::Producer<Event>,
    shared: Arc<Shared>,
    /// The frame the current cycle began at.
    cycle_start: u64,
    enabled: bool,
}

impl Tap {
    /// Call at the start of every cycle.
    pub fn begin_cycle(&mut self) {
        self.cycle_start = self.shared.frame.load(Ordering::Relaxed);
        self.enabled = self.shared.window_frames.load(Ordering::Relaxed) > 0;
    }

    /// A message that arrived `offset` frames into the cycle; `data` is padded with zeros when the
    /// message is shorter than three bytes.
    pub fn record(&mut self, offset: u32, input: u8, data: [u8; 3]) {
        if !self.enabled {
            return;
        }
        let ev = Event {
            frame: self.cycle_start + u64::from(offset),
            input,
            data,
        };
        if self.tx.push(ev).is_err() {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Call at the end of every cycle with its length in frames.
    pub fn end_cycle(&mut self, frames: u32) {
        // Release: the events pushed in this cycle are visible to whoever sees the new time.
        self.shared
            .frame
            .store(self.cycle_start + u64::from(frames), Ordering::Release);
    }
}

/// The main thread's end.
pub struct Recorder {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// A copy of what the buffer held, ready to be written.
#[derive(Clone, Debug)]
pub struct Capture {
    pub events: Vec<Event>,
    /// The audio clock when the capture was taken, in the same frames as the events.
    pub now: u64,
    pub sample_rate: u32,
}

impl Recorder {
    /// A recorder that keeps `window` of playing (zero: it records nothing).
    pub fn new(sample_rate: u32, window: Duration) -> (Tap, Recorder) {
        let (tx, rx) = rtrb::RingBuffer::new(QUEUE);
        let shared = Arc::new(Shared {
            frame: AtomicU64::new(0),
            window_frames: AtomicU64::new(0),
            dropped: AtomicU32::new(0),
            stored: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            sample_rate,
            inner: Mutex::new(Inner {
                rx,
                events: VecDeque::new(),
            }),
        });
        let tap = Tap {
            tx,
            shared: shared.clone(),
            cycle_start: 0,
            enabled: false,
        };
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("merlin-recorder".into())
            .spawn(move || {
                while !worker.stop.load(Ordering::Relaxed) {
                    worker.pump(&mut worker.lock());
                    std::thread::park_timeout(PUMP_INTERVAL);
                }
            })
            .ok();
        let recorder = Recorder { shared, thread };
        recorder.set_window(window);
        (tap, recorder)
    }

    /// How much playing is kept. Shortening it lets the older part go at once; zero stops
    /// recording and forgets what was kept.
    pub fn set_window(&self, window: Duration) {
        let frames = (window.as_secs_f64() * f64::from(self.shared.sample_rate)) as u64;
        self.shared.window_frames.store(frames, Ordering::Relaxed);
        let mut inner = self.shared.lock();
        self.shared.pump(&mut inner);
    }

    pub fn window(&self) -> Duration {
        Duration::from_secs_f64(
            self.shared.window_frames.load(Ordering::Relaxed) as f64
                / f64::from(self.shared.sample_rate),
        )
    }

    /// How long the app has been listening, up to the window: what a capture can reach back to.
    pub fn listened(&self) -> Duration {
        let frames = self
            .shared
            .frame
            .load(Ordering::Relaxed)
            .min(self.shared.window_frames.load(Ordering::Relaxed));
        Duration::from_secs_f64(frames as f64 / f64::from(self.shared.sample_rate))
    }

    /// Messages in the buffer now (as of a fraction of a second ago).
    pub fn stored(&self) -> usize {
        self.shared.stored.load(Ordering::Relaxed)
    }

    /// Messages lost because the queue from the audio thread was full (a stalled storing thread).
    pub fn dropped(&self) -> u32 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Copies the buffer. The recording goes on: capturing again a minute later includes the
    /// same playing and the new.
    pub fn capture(&self) -> Capture {
        let mut inner = self.shared.lock();
        self.shared.pump(&mut inner);
        Capture {
            events: inner.events.iter().copied().collect(),
            now: self.shared.frame.load(Ordering::Acquire),
            sample_rate: self.shared.sample_rate,
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

impl Capture {
    /// Nothing worth a file: no notes, no controllers.
    pub fn is_empty(&self) -> bool {
        smf::tidy(&self.events, self.now).events.is_empty()
    }

    /// From the first message to the last (or to now, if a note is still held).
    pub fn duration(&self) -> Duration {
        let tidied = smf::tidy(&self.events, self.now);
        let Some(first) = tidied.events.first() else {
            return Duration::ZERO;
        };
        Duration::from_secs_f64(
            tidied.end.saturating_sub(first.frame) as f64 / f64::from(self.sample_rate),
        )
    }

    /// A Standard MIDI File (format 1): a track for the tempo and one per MIDI input that played.
    /// Playing that was cut off by the start of the window is tidied: a note-off whose note-on
    /// fell outside it is left out, and a note still held at the end is closed.
    pub fn to_smf(&self, options: &SmfOptions) -> Vec<u8> {
        smf::write(self, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    /// Feeds `cycles` cycles of 128 frames, with `play` called for each one with its first frame.
    fn run(tap: &mut Tap, cycles: u32, mut play: impl FnMut(&mut Tap, u32)) {
        for c in 0..cycles {
            tap.begin_cycle();
            play(tap, c);
            tap.end_cycle(128);
        }
    }

    fn on(key: u8) -> [u8; 3] {
        [0x90, key, 100]
    }

    #[test]
    fn what_the_audio_thread_records_can_be_captured() {
        let (mut tap, rec) = Recorder::new(SR, Duration::from_secs(60));
        run(&mut tap, 10, |t, c| {
            if c == 3 {
                t.record(5, 1, on(60));
            }
        });
        let cap = rec.capture();
        assert_eq!(
            cap.events,
            vec![Event {
                frame: 3 * 128 + 5,
                input: 1,
                data: on(60)
            }]
        );
        assert_eq!(cap.now, 10 * 128);
    }

    #[test]
    fn the_newest_second_replaces_the_oldest() {
        let (mut tap, rec) = Recorder::new(SR, Duration::from_secs(2));
        // 10 seconds, one note a second.
        let per_second = SR / 128;
        run(&mut tap, per_second * 10, |t, c| {
            if c % per_second == 0 {
                t.record(0, 1, on((c / per_second) as u8));
            }
        });
        let keys: Vec<u8> = rec.capture().events.iter().map(|e| e.data[1]).collect();
        // The window is 2 s: the notes of seconds 8 and 9 (and not 7, whose frame is just before
        // the cutoff).
        assert_eq!(keys, vec![8, 9]);
        assert!((rec.listened().as_secs_f64() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_window_of_zero_records_nothing_and_forgets() {
        let (mut tap, rec) = Recorder::new(SR, Duration::from_secs(60));
        run(&mut tap, 4, |t, _| t.record(0, 1, on(60)));
        assert_eq!(rec.capture().events.len(), 4);
        rec.set_window(Duration::ZERO);
        assert!(rec.capture().is_empty());
        run(&mut tap, 4, |t, _| t.record(0, 1, on(60)));
        assert!(rec.capture().is_empty());
        // And it can be switched on again.
        rec.set_window(Duration::from_secs(60));
        run(&mut tap, 4, |t, _| t.record(0, 1, on(60)));
        assert_eq!(rec.capture().events.len(), 4);
    }

    #[test]
    fn a_capture_does_not_empty_the_buffer() {
        let (mut tap, rec) = Recorder::new(SR, Duration::from_secs(60));
        run(&mut tap, 4, |t, _| t.record(0, 1, on(60)));
        assert_eq!(rec.capture().events.len(), 4);
        run(&mut tap, 4, |t, _| t.record(0, 1, on(62)));
        assert_eq!(rec.capture().events.len(), 8);
    }

    #[test]
    fn the_buffer_never_holds_more_than_the_cap() {
        let (mut tap, rec) = Recorder::new(SR, Duration::from_secs(3600));
        // Fill past the cap, emptying the queue now and then like the storing thread does.
        let cycles = (MAX_EVENTS / 1_000 + 5) as u32;
        run(&mut tap, cycles, |t, c| {
            for i in 0..1_000 {
                t.record(i / 8, 1, [0xE0, (i % 128) as u8, 64]);
            }
            if c % 20 == 0 {
                rec.capture();
            }
        });
        let cap = rec.capture();
        assert_eq!(rec.dropped(), 0);
        assert_eq!(cap.events.len(), MAX_EVENTS);
        // The newest are the ones kept, in order.
        assert!(cap.events.windows(2).all(|w| w[0].frame <= w[1].frame));
        assert_eq!(
            cap.events.last().unwrap().frame,
            u64::from(cycles - 1) * 128 + 124
        );
    }
}
