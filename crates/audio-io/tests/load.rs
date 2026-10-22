//! Per-slot audio-thread load: what the rack shows next to each slot.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use merlin_audio_io::OfflineRenderer;
use merlin_engine::{Command, SelectMode};
use merlin_hosting::{Io, MidiEvent, PluginProcessor, ProcessStatus};

const SR: f32 = 48000.0;
const BLOCK: usize = 128;

/// An instrument that takes a fixed time per block, like a heavy plugin would.
struct Busy(Duration);

impl PluginProcessor for Busy {
    fn process(&mut self, _: &mut Io, _: &[MidiEvent]) -> ProcessStatus {
        let start = Instant::now();
        while start.elapsed() < self.0 {
            std::hint::spin_loop();
        }
        ProcessStatus::Continue
    }
}

fn load(r: &mut OfflineRenderer, slot: usize) -> f32 {
    f32::from_bits(r.handle().shared.load[slot].load(Ordering::Relaxed))
}

#[test]
fn a_slot_reports_the_share_of_the_block_it_uses() {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    // 128 frames at 48 kHz is 2.67 ms; 400 µs of work is 15% of it.
    let work = Duration::from_micros(400);
    r.handle()
        .send(Command::SetProcessor {
            slot: 0,
            processor: Box::new(Busy(work)),
        })
        .ok();
    r.handle()
        .send(Command::SetProcessor {
            slot: 1,
            processor: Box::new(Busy(Duration::ZERO)),
        })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 0,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.render(BLOCK * 300, vec![]);
    let heavy = load(&mut r, 0);
    let light = load(&mut r, 1);
    assert!(
        (0.12..0.45).contains(&heavy),
        "about 15% expected, got {heavy}"
    );
    assert!(
        light < heavy / 3.0,
        "an idle slot is far cheaper: {light} vs {heavy}"
    );
    // The worst block is at least the average, and reading it resets it.
    let worst = f32::from_bits(r.handle().shared.load_max[0].swap(0, Ordering::Relaxed));
    assert!(worst >= heavy * 0.9, "worst {worst} avg {heavy}");
    assert_eq!(r.handle().shared.load_max[0].load(Ordering::Relaxed), 0);
    // A slot without a processor reports nothing.
    assert_eq!(load(&mut r, 5), 0.0);
}

#[test]
fn a_slot_that_goes_to_sleep_stops_costing_anything() {
    let mut r = OfflineRenderer::new(SR, BLOCK);
    struct Sleepy;
    impl PluginProcessor for Sleepy {
        fn process(&mut self, _: &mut Io, ev: &[MidiEvent]) -> ProcessStatus {
            let start = Instant::now();
            while start.elapsed() < Duration::from_micros(300) {
                std::hint::spin_loop();
            }
            if ev.is_empty() {
                ProcessStatus::Sleep
            } else {
                ProcessStatus::Continue
            }
        }
    }
    r.handle()
        .send(Command::SetProcessor {
            slot: 0,
            processor: Box::new(Sleepy),
        })
        .ok();
    r.handle()
        .send(Command::SetProcessor {
            slot: 1,
            processor: Box::new(Busy(Duration::ZERO)),
        })
        .ok();
    r.handle()
        .send(Command::Select {
            slot: 1,
            mode: SelectMode::Exclusive,
        })
        .ok();
    r.render(BLOCK * 20, vec![]);
    assert_eq!(r.handle().shared.sleeping.load(Ordering::Relaxed) & 1, 1);
    r.render(BLOCK * 600, vec![]);
    assert!(
        load(&mut r, 0) < 0.01,
        "asleep means free: {}",
        load(&mut r, 0)
    );
}
