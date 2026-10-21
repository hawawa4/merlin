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

use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64};

pub use engine::{Command, Engine, EngineHandle, Retired, new};
pub use merlin_model::{MAX_SLOTS, SelectMode};
pub use router::{Filter, MAX_INPUTS, Router};

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
    /// Events dropped because a slot's event list was full.
    pub dropped_events: AtomicU32,
    pub blocks: AtomicU64,
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
            dropped_events: AtomicU32::new(0),
            blocks: AtomicU64::new(0),
        }
    }
}
