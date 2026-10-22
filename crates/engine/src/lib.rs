//! The real-time core: MIDI router, slot processing, mixing, command and event queues.
//! No UI or audio-backend dependencies; everything is testable offline.
//!
//! Threading (plan §4.1): the audio thread owns the [`Engine`] and calls [`Engine::process`].
//! The main thread talks to it through the [`EngineHandle`]: commands go in through a
//! lock-free queue, retired processors come back through another (so they are freed off the
//! audio thread), and meters and note activity are read from [`Shared`] atomics.

mod engine;
mod events;
pub mod router;
pub mod transport;

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64};

pub use engine::{ClickOut, Command, Engine, EngineHandle, MAX_STEMS, Retired, Stem, new};
pub use merlin_model::{MAX_SLOTS, SelectMode};
pub use router::{Filter, MAX_INPUTS, Router};
pub use transport::{ClockEvent, MIDI_CLOCK, MIDI_START, MIDI_STOP, Transport, TransportShared};

/// State the audio thread publishes for the UI.
pub struct Shared {
    /// For each key, the slots that own a held note on it (bit i = slot i).
    pub held: [AtomicU16; 128],
    /// Output peak per slot since the UI last read it (`swap(0)`), as `f32` bits.
    pub peaks: [AtomicU32; MAX_SLOTS],
    pub master_peak: AtomicU32,
    /// Compressor gain reduction per slot, dB as `f32` bits (the latest block).
    pub comp_reduction: [AtomicU32; MAX_SLOTS],
    /// Master limiter gain reduction, dB as `f32` bits (the largest since the UI last read it).
    pub limiter_reduction: AtomicU32,
    /// Bit i set: slot i is asleep (idle, silent, no tail, and not being processed).
    pub sleeping: AtomicU16,
    /// Audio-thread time per slot as a fraction of the block's time budget (1.0 = the slot alone
    /// uses the whole block), smoothed over a few dozen blocks, as `f32` bits.
    pub load: [AtomicU32; MAX_SLOTS],
    /// The largest single-block load per slot since the UI last read it (`swap(0)`).
    pub load_max: [AtomicU32; MAX_SLOTS],
    /// Events dropped because a slot's event list was full.
    pub dropped_events: AtomicU32,
    pub blocks: AtomicU64,
    /// Tempo, click and start/stop (see [`transport`]).
    pub transport: TransportShared,
    /// Backing tracks are playing; and how often one ran dry (the decoder fell behind).
    pub stems_playing: AtomicBool,
    pub backing_underruns: AtomicU32,
}

impl Shared {
    fn new() -> Self {
        Self {
            held: std::array::from_fn(|_| AtomicU16::new(0)),
            peaks: std::array::from_fn(|_| AtomicU32::new(0)),
            master_peak: AtomicU32::new(0),
            comp_reduction: std::array::from_fn(|_| AtomicU32::new(0)),
            limiter_reduction: AtomicU32::new(0),
            sleeping: AtomicU16::new(0),
            load: std::array::from_fn(|_| AtomicU32::new(0)),
            load_max: std::array::from_fn(|_| AtomicU32::new(0)),
            dropped_events: AtomicU32::new(0),
            blocks: AtomicU64::new(0),
            transport: TransportShared::new(),
            stems_playing: AtomicBool::new(false),
            backing_underruns: AtomicU32::new(0),
        }
    }
}
