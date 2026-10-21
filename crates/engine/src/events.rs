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
