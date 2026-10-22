//! The tempo and play state plugins can ask the host for (VST2's `getTime`, and later LV2's
//! transport). The app copies them from the transport; plugins read them on any thread.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static TEMPO: AtomicU32 = AtomicU32::new(0x42f0_0000); // 120.0
static PLAYING: AtomicBool = AtomicBool::new(false);
static BEATS_PER_BAR: AtomicU32 = AtomicU32::new(4);

pub fn set_tempo(bpm: f32) {
    TEMPO.store(bpm.to_bits(), Ordering::Relaxed);
}

pub fn tempo() -> f32 {
    f32::from_bits(TEMPO.load(Ordering::Relaxed))
}

pub fn set_playing(playing: bool) {
    PLAYING.store(playing, Ordering::Relaxed);
}

pub fn playing() -> bool {
    PLAYING.load(Ordering::Relaxed)
}

pub fn set_beats_per_bar(beats: u32) {
    BEATS_PER_BAR.store(beats.max(1), Ordering::Relaxed);
}

pub fn beats_per_bar() -> u32 {
    BEATS_PER_BAR.load(Ordering::Relaxed)
}
