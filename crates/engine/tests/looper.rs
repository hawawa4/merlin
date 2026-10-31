//! The loop station through the whole engine, offline: what a slot is played while recording
//! comes back from the next loop on, at the same place, and nothing is recorded or played when
//! the transport is not running.

use std::sync::{Arc, Mutex};

use merlin_engine::looper::{LoopCommand, LoopMode};
use merlin_engine::{Command, Engine, SelectMode, new};
use merlin_hosting::{Io, MidiEvent, PluginProcessor, ProcessStatus};

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;
/// One bar of 4/4 at 120 bpm.
const LOOP: usize = 96_000;

/// Messages with the frame they arrived at.
type Seen = Arc<Mutex<Vec<(usize, [u8; 3])>>>;

/// Remembers every message it is given, with the frame it arrived at.
struct Spy {
    seen: Seen,
    blocks: usize,
}

impl PluginProcessor for Spy {
    fn process(&mut self, _io: &mut Io, events: &[MidiEvent]) -> ProcessStatus {
        let mut seen = self.seen.lock().unwrap();
        for e in events {
            seen.push((self.blocks * BLOCK + e.time as usize, e.data));
        }
        self.blocks += 1;
        ProcessStatus::Continue
    }
}

struct Rig {
    engine: Engine,
    handle: merlin_engine::EngineHandle,
    seen: Seen,
    /// Frames rendered so far.
    at: usize,
}

impl Rig {
    fn new() -> Self {
        let (mut engine, mut handle) = new(SR, BLOCK);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let send = |h: &mut merlin_engine::EngineHandle, c| h.send(c).ok().unwrap();
        send(
            &mut handle,
            Command::SetProcessor {
                slot: 0,
                processor: Box::new(Spy {
                    seen: seen.clone(),
                    blocks: 0,
                }),
            },
        );
        send(
            &mut handle,
            Command::Select {
                slot: 0,
                mode: SelectMode::Exclusive,
            },
        );
        send(&mut handle, Command::Loop(LoopCommand::SetBars(1)));
        handle.shared.transport.set_tempo(120.0);
        handle.shared.transport.set_beats_per_bar(4);
        handle.shared.transport.set_click(false, 0.0);
        // Settle the commands.
        engine.process(&[], &mut [0.0; BLOCK], &mut [0.0; BLOCK]);
        seen.lock().unwrap().clear();
        Self {
            engine,
            handle,
            seen,
            at: BLOCK,
        }
    }

    /// Renders `frames` frames, delivering `played` (absolute frame, message) as they come due.
    fn run(&mut self, frames: usize, played: &[(usize, [u8; 3])]) {
        for _ in 0..frames / BLOCK {
            let midi: Vec<MidiEvent> = played
                .iter()
                .filter(|(f, _)| (self.at..self.at + BLOCK).contains(f))
                .map(|(f, d)| MidiEvent::new((f - self.at) as u32, 0, *d))
                .collect();
            self.engine
                .process(&midi, &mut [0.0; BLOCK], &mut [0.0; BLOCK]);
            self.at += BLOCK;
        }
    }

    fn send(&mut self, c: LoopCommand) {
        self.handle.send(Command::Loop(c)).ok().unwrap();
    }

    fn mode(&self) -> LoopMode {
        LoopMode::from_u8(
            self.handle.shared.looper.lanes[0]
                .mode
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }
}

#[test]
fn what_is_played_while_recording_comes_back_every_loop() {
    let mut rig = Rig::new();
    rig.send(LoopCommand::Rec(0));
    rig.handle.shared.transport.start(0);
    // The run starts in the next block: the loop's frame 0 is `start`.
    let start = rig.at;
    let played = [
        (start + 24_000, [0x90, 60, 100]),
        (start + 26_400, [0x80, 60, 0]),
        (start + 60_000, [0x90, 64, 90]),
        (start + 61_000, [0x80, 64, 0]),
    ];
    rig.run(LOOP, &played);
    assert_eq!(rig.mode(), LoopMode::Play, "one pass, then it plays");
    rig.seen.lock().unwrap().clear();
    rig.run(LOOP, &[]);
    let heard = rig.seen.lock().unwrap().clone();
    assert_eq!(heard.len(), 4, "{heard:?}");
    for ((frame, data), (want, want_data)) in heard.iter().zip(&played) {
        let want = want + LOOP;
        assert!(
            frame.abs_diff(want) <= 30,
            "{data:?} at {frame}, wanted {want}"
        );
        assert_eq!(data, want_data);
    }
    // Stopping ends the loop; starting again plays it again from the top.
    rig.handle.shared.transport.stop();
    rig.run(BLOCK * 4, &[]);
    rig.seen.lock().unwrap().clear();
    rig.handle.shared.transport.start(0);
    rig.run(LOOP, &[]);
    assert_eq!(rig.seen.lock().unwrap().len(), 4);
}

#[test]
fn a_loop_note_held_when_the_transport_stops_is_released() {
    let mut rig = Rig::new();
    rig.send(LoopCommand::Rec(0));
    rig.handle.shared.transport.start(0);
    let start = rig.at;
    rig.run(
        LOOP,
        &[
            (start + 1_000, [0x90, 60, 100]),
            (start + 90_000, [0x80, 60, 0]),
        ],
    );
    rig.run(LOOP / 2, &[]);
    rig.seen.lock().unwrap().clear();
    rig.handle.shared.transport.stop();
    rig.run(BLOCK, &[]);
    let heard = rig.seen.lock().unwrap().clone();
    assert!(heard.iter().any(|(_, d)| *d == [0x80, 60, 0]), "{heard:?}");
}

#[test]
fn nothing_is_recorded_while_the_transport_is_stopped() {
    let mut rig = Rig::new();
    rig.send(LoopCommand::Rec(0));
    rig.run(
        LOOP,
        &[
            (rig.at + 5_000, [0x90, 60, 100]),
            (rig.at + 6_000, [0x80, 60, 0]),
        ],
    );
    assert_eq!(rig.mode(), LoopMode::Armed);
    let filled =
        rig.handle.shared.looper.lanes[0].filled[0].load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(filled, 0);
}
