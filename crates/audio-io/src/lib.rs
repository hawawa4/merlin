//! Audio backends. This crate starts with the dummy/offline one (plan §4.2): it drives the
//! engine from a script without any sound card, for tests and offline rendering. The JACK
//! backend is the next addition.

mod jack_backend;

pub use jack_backend::{
    HARDWARE_INPUTS, JackBackend, JackClient, MemLock, MidiSource, Stats, VIRTUAL_INPUT,
    lock_memory, preload_pipewire_jack,
};

use merlin_engine::{Command, Engine, EngineHandle};
use merlin_hosting::MidiEvent;

/// Something that happens at an absolute frame of the render.
pub enum Item {
    Midi(MidiEvent),
    Command(Command),
}

pub struct Rendered {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Rendered {
    /// Largest absolute sample in `frames` (both channels).
    pub fn peak(&self, frames: std::ops::Range<usize>) -> f32 {
        self.left[frames.clone()]
            .iter()
            .chain(&self.right[frames])
            .fold(0.0, |m, x| m.max(x.abs()))
    }
}

/// Runs an [`Engine`] in fixed-size blocks, like a backend callback would.
pub struct OfflineRenderer {
    engine: Engine,
    handle: EngineHandle,
    block: usize,
    /// Frames rendered so far.
    frame: u64,
}

impl OfflineRenderer {
    pub fn new(sample_rate: f32, block: usize) -> Self {
        let (engine, handle) = merlin_engine::new(sample_rate, block);
        Self {
            engine,
            handle,
            block,
            frame: 0,
        }
    }

    pub fn handle(&mut self) -> &mut EngineHandle {
        &mut self.handle
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Frames rendered so far; item times in `render` are relative to this.
    pub fn position(&self) -> u64 {
        self.frame
    }

    /// Renders `frames` frames. `items` are (frame offset from the start of this call, item),
    /// in any order. A command takes effect at the start of the block containing its frame;
    /// MIDI is sample-accurate within the block.
    pub fn render(&mut self, frames: usize, items: Vec<(usize, Item)>) -> Rendered {
        let mut items = items;
        items.sort_by_key(|(at, _)| *at);
        let mut items = items.into_iter().peekable();
        let (mut left, mut right) = (vec![0.0; frames], vec![0.0; frames]);
        let mut midi: Vec<MidiEvent> = Vec::new();
        let mut start = 0;
        while start < frames {
            let end = (start + self.block).min(frames);
            midi.clear();
            while let Some((at, _)) = items.peek() {
                if *at >= end {
                    break;
                }
                let (at, item) = items.next().unwrap();
                match item {
                    Item::Midi(mut ev) => {
                        ev.time = (at - start) as u32;
                        midi.push(ev);
                    }
                    Item::Command(c) => {
                        if self.handle.send(c).is_err() {
                            panic!("command queue full");
                        }
                    }
                }
            }
            self.engine
                .process(&midi, &mut left[start..end], &mut right[start..end]);
            start = end;
        }
        self.frame += frames as u64;
        Rendered { left, right }
    }
}
