//! A fixed-capacity event list: filled by the router on the audio thread, never grows.

use merlin_hosting::MidiEvent;

pub const EVENT_CAPACITY: usize = 2048;

pub struct EventBuf {
    events: Vec<MidiEvent>,
    /// Events dropped because the list was full (reported through `Shared`).
    pub dropped: u32,
}

impl EventBuf {
    pub fn new() -> Self {
        Self {
            events: Vec::with_capacity(EVENT_CAPACITY),
            dropped: 0,
        }
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    pub fn push(&mut self, ev: MidiEvent) {
        if self.events.len() < EVENT_CAPACITY {
            self.events.push(ev);
        } else {
            self.dropped += 1;
        }
    }

    /// Adds an event in its place among the others (they are in time order). Events with the
    /// same time keep the order they came in.
    pub fn insert(&mut self, ev: MidiEvent) {
        self.push(ev);
        let mut i = self.events.len() - 1;
        while i > 0 && self.events[i - 1].time > self.events[i].time {
            self.events.swap(i - 1, i);
            i -= 1;
        }
    }

    pub fn as_slice(&self) -> &[MidiEvent] {
        &self.events
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl Default for EventBuf {
    fn default() -> Self {
        Self::new()
    }
}
