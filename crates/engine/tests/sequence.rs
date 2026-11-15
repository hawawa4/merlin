//! A MIDI file through the whole engine, offline: it plays the selected instrument at its frames,
//! side by side with what is played live, and a stop or a panic leaves nothing sounding.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use merlin_engine::{Command, Engine, EngineHandle, SelectMode, SeqEvent, Sequence, new};
use merlin_hosting::{Io, MidiEvent, PluginProcessor, ProcessStatus};

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;

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
    handle: EngineHandle,
    seen: Seen,
}

impl Rig {
    fn new() -> Self {
        let (mut engine, mut handle) = new(SR, BLOCK);
        let seen = Arc::new(Mutex::new(Vec::new()));
        for c in [
            Command::SetProcessor {
                slot: 0,
                processor: Box::new(Spy {
                    seen: seen.clone(),
                    blocks: 0,
                }),
            },
            Command::Select {
                slot: 0,
                mode: SelectMode::Exclusive,
            },
        ] {
            handle.send(c).ok().unwrap();
        }
        // Block 0 settles the commands.
        engine.process(&[], &mut [0.0; BLOCK], &mut [0.0; BLOCK]);
        Self {
            engine,
            handle,
            seen,
        }
    }

    fn block(&mut self, live: &[MidiEvent]) {
        self.engine
            .process(live, &mut [0.0; BLOCK], &mut [0.0; BLOCK]);
    }

    fn send(&mut self, c: Command) {
        self.handle.send(c).ok().unwrap();
    }

    fn take_seen(&self) -> Vec<(usize, [u8; 3])> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

fn ev(frame: u64, data: [u8; 3]) -> SeqEvent {
    SeqEvent { frame, data }
}

#[test]
fn the_file_plays_the_selected_instrument_alongside_live_playing() {
    let mut rig = Rig::new();
    let seq = Sequence::new(
        vec![
            ev(10, [0x90, 60, 100]),
            ev(300, [0x80, 60, 0]),
            ev(400, [0x90, 62, 90]),
        ],
        600,
    );
    rig.send(Command::SetSequence(Some(Box::new(seq))));
    rig.send(Command::PlaySequence);
    // Block 1 (frames 256..512): the file starts at 256. Live, a key at 100 into the block.
    rig.block(&[MidiEvent::new(100, 1, [0x90, 70, 80])]);
    assert!(rig.handle.shared.sequence_playing.load(Ordering::Relaxed));
    assert_eq!(rig.handle.shared.sequence_runs.load(Ordering::Relaxed), 1);
    rig.block(&[]);
    rig.block(&[]);
    assert!(!rig.handle.shared.sequence_playing.load(Ordering::Relaxed));
    assert_eq!(
        rig.handle.shared.sequence_position.load(Ordering::Relaxed),
        600
    );
    assert_eq!(
        rig.take_seen(),
        vec![
            (256 + 10, [0x90, 60, 100]),
            (256 + 100, [0x90, 70, 80]),
            (256 + 300, [0x80, 60, 0]),
            (256 + 400, [0x90, 62, 90]),
            // The file ends at 600 with note 62 still down: it is let go there.
            (256 + 600, [0x80, 62, 0]),
        ]
    );
}

#[test]
fn stop_and_panic_leave_nothing_sounding() {
    let mut rig = Rig::new();
    let seq = Sequence::new(
        vec![ev(0, [0x90, 60, 100]), ev(5_000, [0x90, 64, 100])],
        48_000,
    );
    rig.send(Command::SetSequence(Some(Box::new(seq.clone()))));
    rig.send(Command::PlaySequence);
    rig.block(&[]);
    rig.send(Command::StopSequence);
    rig.block(&[]);
    assert_eq!(
        rig.take_seen(),
        vec![(256, [0x90, 60, 100]), (512, [0x80, 60, 0])]
    );
    // A stopped file plays nothing more.
    for _ in 0..30 {
        rig.block(&[]);
    }
    assert!(rig.take_seen().is_empty());

    rig.send(Command::PlaySequence);
    rig.block(&[]);
    rig.send(Command::Panic);
    for _ in 0..30 {
        rig.block(&[]);
    }
    assert!(!rig.handle.shared.sequence_playing.load(Ordering::Relaxed));
    let seen = rig.take_seen();
    assert!(
        !seen.iter().any(|(_, d)| *d == [0x90, 64, 100]),
        "the file went on after a panic: {seen:?}"
    );

    // A new file replaces the old, which comes back to be freed here.
    rig.send(Command::SetSequence(Some(Box::new(seq))));
    rig.block(&[]);
    assert_eq!(rig.handle.take_retired_sequences().len(), 1);
}
