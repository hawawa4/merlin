//! The loop station on the session: what the audio thread's looper is doing, shaped for the
//! Looper view, and the buttons that drive it. The loops themselves live on the audio thread
//! and are not part of the rig: they are for playing with, and gone when the app closes.

use std::sync::atomic::Ordering;

use merlin_engine::Command;
use merlin_engine::looper::{LoopCommand, LoopMode, NO_PART, PPQ};

use super::{Session, UI_SLOTS};

/// One slot's loop as the view draws it.
#[derive(Clone, Copy, Debug)]
pub struct LoopLane {
    pub mode: LoopMode,
    /// 0 is A, 1 is B.
    pub part: usize,
    /// The part waiting for the loop to come round.
    pub pending: Option<usize>,
    pub muted: bool,
    /// Per part, which bars hold something (bit n = bar n).
    pub filled: [u16; 2],
}

#[derive(Clone, Debug)]
pub struct LoopView {
    pub running: bool,
    /// Where the loop is, in beats from its start (a beat is a quarter or an eighth note).
    pub beat: f32,
    pub bars: u8,
    pub beats_per_bar: u8,
    pub lanes: [LoopLane; UI_SLOTS],
}

impl LoopView {
    /// The bar and beat (both from 1) the loop is at.
    pub fn position(&self) -> (u32, u32) {
        let beat = self.beat.max(0.0) as u32;
        let per = u32::from(self.beats_per_bar.max(1));
        (beat / per + 1, beat % per + 1)
    }

    /// The beat's index in the whole loop (0 ..= bars * beats_per_bar - 1).
    pub fn beat_index(&self) -> usize {
        self.beat.max(0.0) as usize
    }
}

impl Session {
    pub fn loop_view(&self) -> LoopView {
        let shared = &self.engine.shared.looper;
        let lanes = std::array::from_fn(|i| {
            let l = &shared.lanes[i];
            let pending = l.pending.load(Ordering::Relaxed);
            LoopLane {
                mode: LoopMode::from_u8(l.mode.load(Ordering::Relaxed)),
                part: usize::from(l.part.load(Ordering::Relaxed) & 1),
                pending: (pending != NO_PART).then_some(usize::from(pending & 1)),
                muted: l.muted.load(Ordering::Relaxed),
                filled: [
                    l.filled[0].load(Ordering::Relaxed),
                    l.filled[1].load(Ordering::Relaxed),
                ],
            }
        });
        LoopView {
            running: shared.running.load(Ordering::Relaxed),
            beat: shared.pos.load(Ordering::Relaxed) as f32
                / (PPQ * 4 / u32::from(self.beat_unit().max(1))) as f32,
            bars: shared.bars.load(Ordering::Relaxed),
            beats_per_bar: self.beats_per_bar(),
            lanes,
        }
    }

    /// The bar and beat the transport's click is at (it runs one ahead during the count-in).
    pub fn engine_position(&self) -> (u32, u32) {
        self.engine.shared.transport.position()
    }

    pub fn loop_cmd(&mut self, cmd: LoopCommand) {
        let _ = self.engine.send(Command::Loop(cmd));
    }

    /// A slot's REC button. Recording needs the slot to hear the keyboard, and the loop to run:
    /// both are done for the player.
    pub fn loop_rec(&mut self, slot: usize) {
        if slot >= UI_SLOTS || self.plugins[slot].is_none() {
            return;
        }
        let mode = self.loop_view().lanes[slot].mode;
        if matches!(mode, LoopMode::Idle | LoopMode::Play) {
            self.target_only(slot);
        }
        self.loop_cmd(LoopCommand::Rec(slot));
        let shared = &self.engine.shared.transport;
        if matches!(mode, LoopMode::Idle | LoopMode::Play)
            && !shared.running()
            && !shared.counting_in()
        {
            self.transport_toggle();
        }
    }

    /// Every slot switches to part A or B.
    pub fn loop_all(&mut self, part: usize) {
        for slot in 0..UI_SLOTS {
            if self.plugins[slot].is_some() {
                self.loop_cmd(LoopCommand::Part(slot, part));
            }
        }
    }

    pub fn loop_clear_all(&mut self) {
        self.loop_cmd(LoopCommand::ClearAll);
    }

    /// A different loop length: the loops would not fit, so they go, and the transport stops.
    pub fn loop_set_bars(&mut self, bars: u8) {
        self.engine.shared.transport.stop();
        self.loop_cmd(LoopCommand::SetBars(bars));
    }

    /// The time signature changed: the loops go, and the transport stops (nothing to do while
    /// nothing is recorded or recording).
    pub(super) fn loops_no_longer_fit(&mut self) {
        let busy = self
            .loop_view()
            .lanes
            .iter()
            .any(|l| l.mode != LoopMode::Idle || l.filled != [0, 0]);
        if busy {
            self.engine.shared.transport.stop();
            self.loop_cmd(LoopCommand::ClearAll);
        }
    }

    /// Makes the slot the only one the keyboard plays, without opening its editor.
    fn target_only(&mut self, i: usize) {
        if self.targets == [i] {
            return;
        }
        let _ = self.engine.send(Command::Select {
            slot: i,
            mode: merlin_engine::SelectMode::Exclusive,
        });
        self.targets = vec![i];
        self.rig.selected = self.targets.clone();
        self.mapper.selection_changed();
        self.dirty = true;
        self.update_surfaces();
    }
}
