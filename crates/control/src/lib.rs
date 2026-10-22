//! Turns MIDI messages into what the app should do (plan §4.10, Phase 3).
//!
//! Pure logic, no audio or UI: feed it the messages that arrive and the state it needs to know
//! (which slots are loaded, what is selected, the current volumes), get back a list of outcomes.
//! The app turns those into the same actions the buttons on screen perform.

use merlin_model::{
    ControlAction, MACROS_PER_SLOT, MAX_SLOTS, Mapping, SourceKind, Takeover,
};

/// A message from a device, as far as mappings care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    Cc { number: u8, value: u8 },
    Note { key: u8, velocity: u8, on: bool },
    Program { number: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    /// The device's name as the MIDI settings show it.
    pub device: String,
    /// MIDI channel 0-15.
    pub channel: u8,
    pub message: Message,
}

/// What the mapper needs to know about the rig.
#[derive(Clone, Debug)]
pub struct Context {
    /// Bit i set: slot i has an instrument.
    pub loaded: u16,
    /// Bit i set: slot i is a keyboard target now.
    pub targets: u16,
    pub volumes: [f32; MAX_SLOTS],
    pub master: f32,
    /// The first slot that plays now, for the controls that follow the selection.
    pub primary: Option<usize>,
    /// Where each slot's macros sit.
    pub macros: [[f32; MACROS_PER_SLOT]; MAX_SLOTS],
    /// How many snapshots the rig has, and the one recalled last.
    pub snapshots: usize,
    pub snapshot: Option<usize>,
}

/// What to do. Slots are 0-based.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    Select(usize),
    ToggleLayer(usize),
    ToggleMute(usize),
    SetVolume(usize, f32),
    SetMaster(f32),
    SetMacro(usize, usize, f32),
    Recall(usize),
    Panic,
}

#[derive(Clone, Copy, Default)]
struct State {
    /// The last position the knob reported, for pick-up.
    last: Option<f32>,
    picked_up: bool,
}

/// Holds the mappings and the little state some of them need.
#[derive(Default)]
pub struct Mapper {
    mappings: Vec<Mapping>,
    state: Vec<State>,
}

impl Mapper {
    pub fn new(mappings: &[Mapping]) -> Self {
        let mut m = Self::default();
        m.set_mappings(mappings);
        m
    }

    /// Replaces the mappings; a mapping that stays keeps its pick-up state.
    pub fn set_mappings(&mut self, mappings: &[Mapping]) {
        let mut state = vec![State::default(); mappings.len()];
        for (i, m) in mappings.iter().enumerate() {
            if let Some(j) = self.mappings.iter().position(|old| old == m) {
                state[i] = self.state[j];
            }
        }
        self.mappings = mappings.to_vec();
        self.state = state;
    }

    /// What plays changed: the controls that follow the selection start over.
    pub fn selection_changed(&mut self) {
        for (m, s) in self.mappings.iter().zip(self.state.iter_mut()) {
            if matches!(
                m.action,
                ControlAction::SelectedMacro { .. } | ControlAction::SelectedVolume
            ) {
                *s = State::default();
            }
        }
    }

    /// The knobs have to be picked up again (the values they control were changed elsewhere).
    pub fn reset_pickup(&mut self) {
        self.state.iter_mut().for_each(|s| *s = State::default());
    }

    pub fn mappings(&self) -> &[Mapping] {
        &self.mappings
    }

    fn matches(m: &Mapping, inc: &Incoming) -> bool {
        m.source.device.as_deref().is_none_or(|d| d == inc.device)
            && m.source.channel.is_none_or(|c| c == inc.channel)
            && match (m.source.kind, inc.message) {
                (SourceKind::Cc { number }, Message::Cc { number: n, .. }) => number == n,
                (SourceKind::Note { key }, Message::Note { key: k, .. }) => key == k,
                (SourceKind::Program, Message::Program { .. }) => true,
                _ => false,
            }
    }

    /// Does any mapping take this message? (Then it must not also play an instrument.)
    pub fn is_mapped(&self, inc: &Incoming) -> bool {
        self.mappings.iter().any(|m| Self::matches(m, inc))
    }

    /// What the message does, given the rig's state.
    pub fn handle(&mut self, inc: &Incoming, ctx: &Context) -> Vec<Outcome> {
        let mut out = Vec::new();
        for (m, state) in self.mappings.iter().zip(self.state.iter_mut()) {
            if !Self::matches(m, inc) {
                continue;
            }
            if m.action.is_continuous() {
                if let Some(o) = follow(m, state, inc, ctx) {
                    out.push(o);
                }
            } else if pressed(&inc.message) {
                out.extend(fire(&m.action, inc, ctx));
            }
        }
        out
    }
}

/// A press: a CC at or above half, a key going down, any Program Change.
fn pressed(msg: &Message) -> bool {
    match *msg {
        Message::Cc { value, .. } => value >= 64,
        Message::Note { on, velocity, .. } => on && velocity > 0,
        Message::Program { .. } => true,
    }
}

/// The 0..=127 a message carries.
fn value_of(msg: &Message) -> u8 {
    match *msg {
        Message::Cc { value, .. } => value,
        Message::Note { velocity, .. } => velocity,
        Message::Program { number } => number,
    }
}

fn loaded(ctx: &Context, slot: usize) -> bool {
    slot < MAX_SLOTS && ctx.loaded & (1 << slot) != 0
}

/// The loaded slots in order.
fn loaded_slots(ctx: &Context) -> Vec<usize> {
    (0..MAX_SLOTS).filter(|&i| loaded(ctx, i)).collect()
}

fn fire(action: &ControlAction, inc: &Incoming, ctx: &Context) -> Option<Outcome> {
    match *action {
        ControlAction::SelectSlot { slot } if loaded(ctx, slot as usize) => {
            Some(Outcome::Select(slot as usize))
        }
        ControlAction::ToggleLayer { slot } if loaded(ctx, slot as usize) => {
            Some(Outcome::ToggleLayer(slot as usize))
        }
        ControlAction::ToggleMute { slot } if loaded(ctx, slot as usize) => {
            Some(Outcome::ToggleMute(slot as usize))
        }
        ControlAction::SelectByValue { base } => {
            let key = match inc.message {
                Message::Note { key, .. } => key,
                ref m => value_of(m),
            };
            let slot = base as usize + key as usize;
            loaded(ctx, slot).then_some(Outcome::Select(slot))
        }
        ControlAction::NextSlot | ControlAction::PrevSlot => {
            let slots = loaded_slots(ctx);
            if slots.is_empty() {
                return None;
            }
            let forward = matches!(action, ControlAction::NextSlot);
            // From the first slot that plays now; with none playing, from the edge.
            let current = (0..MAX_SLOTS).find(|&i| ctx.targets & (1 << i) != 0);
            let next = match current.and_then(|c| slots.iter().position(|&s| s == c)) {
                Some(at) if forward => slots[(at + 1) % slots.len()],
                Some(at) => slots[(at + slots.len() - 1) % slots.len()],
                None if forward => slots[0],
                None => slots[slots.len() - 1],
            };
            Some(Outcome::Select(next))
        }
        ControlAction::RecallSnapshot { snapshot } => {
            ((snapshot as usize) < ctx.snapshots).then_some(Outcome::Recall(snapshot as usize))
        }
        ControlAction::RecallSnapshotByValue { base } => {
            let key = match inc.message {
                Message::Note { key, .. } => key,
                ref m => value_of(m),
            };
            let n = base as usize + key as usize;
            (n < ctx.snapshots).then_some(Outcome::Recall(n))
        }
        ControlAction::NextSnapshot | ControlAction::PrevSnapshot => {
            let count = ctx.snapshots;
            if count == 0 {
                return None;
            }
            let forward = matches!(action, ControlAction::NextSnapshot);
            Some(Outcome::Recall(match ctx.snapshot {
                Some(c) if forward => (c + 1) % count,
                Some(c) => (c + count - 1) % count,
                None if forward => 0,
                None => count - 1,
            }))
        }
        ControlAction::Panic => Some(Outcome::Panic),
        _ => None,
    }
}

/// A knob or fader following its value, with jump, pick-up or scale, or an endless encoder
/// moving it by steps.
fn follow(m: &Mapping, state: &mut State, inc: &Incoming, ctx: &Context) -> Option<Outcome> {
    // What the control drives, and where that sits now.
    let (slot, which_macro, current) = match m.action {
        ControlAction::SlotVolume { slot } => {
            let s = slot as usize;
            if !loaded(ctx, s) {
                return None;
            }
            (Some(s), None, ctx.volumes[s])
        }
        ControlAction::SelectedVolume => {
            let s = ctx.primary.filter(|&s| loaded(ctx, s))?;
            (Some(s), None, ctx.volumes[s])
        }
        ControlAction::SlotMacro { slot, index } => {
            let (s, k) = (slot as usize, index as usize);
            if !loaded(ctx, s) || k >= MACROS_PER_SLOT {
                return None;
            }
            (Some(s), Some(k), ctx.macros[s][k])
        }
        ControlAction::SelectedMacro { index } => {
            let s = ctx.primary.filter(|&s| loaded(ctx, s))?;
            let k = index as usize;
            if k >= MACROS_PER_SLOT {
                return None;
            }
            (Some(s), Some(k), ctx.macros[s][k])
        }
        _ => (None, None, ctx.master),
    };
    let current = current.clamp(0.0, 1.0);
    let glue = &m.glue;
    let value = value_of(&inc.message);
    let new = if let Some(steps) = glue.relative.steps(value) {
        // An encoder: it says how far it turned, so there is nothing to pick up.
        let steps = if glue.invert { -steps } else { steps };
        (current + steps as f32 * glue.step).clamp(glue.min.min(glue.max), glue.min.max(glue.max))
    } else {
        let y = glue.shape(f32::from(value) / 127.0);
        let last = state.last.replace(y);
        match m.takeover {
            Takeover::Jump => y,
            Takeover::Pickup => {
                if !state.picked_up {
                    // Close enough, or the knob went across the value since the last message.
                    let near = (y - current).abs() <= 1.5 / 127.0;
                    let crossed = last.is_some_and(|l| (l - current) * (y - current) <= 0.0);
                    if !(near || crossed) {
                        return None;
                    }
                    state.picked_up = true;
                }
                y
            }
            Takeover::Scale => {
                // The first message only says where the knob is.
                let l = last?;
                if y > l {
                    if l >= 1.0 {
                        return None;
                    }
                    current + (1.0 - current) * (y - l) / (1.0 - l)
                } else {
                    if l <= 0.0 {
                        return None;
                    }
                    current * y / l
                }
            }
        }
    };
    Some(match (slot, which_macro) {
        (Some(s), Some(k)) => Outcome::SetMacro(s, k, new),
        (Some(s), None) => Outcome::SetVolume(s, new),
        _ => Outcome::SetMaster(new),
    })
}

#[cfg(test)]
mod tests {
    use merlin_model::{Curve, Glue, Relative, Source};

    use super::*;

    fn ctx(loaded: &[usize], targets: &[usize]) -> Context {
        let bits = |v: &[usize]| v.iter().fold(0u16, |m, &i| m | 1 << i);
        Context {
            loaded: bits(loaded),
            targets: bits(targets),
            volumes: [1.0; MAX_SLOTS],
            master: 1.0,
            primary: None,
            macros: [[0.0; MACROS_PER_SLOT]; MAX_SLOTS],
            snapshots: 0,
            snapshot: None,
        }
    }

    fn cc(device: &str, ch: u8, number: u8, value: u8) -> Incoming {
        Incoming {
            device: device.into(),
            channel: ch,
            message: Message::Cc { number, value },
        }
    }

    fn map(device: Option<&str>, kind: SourceKind, action: ControlAction) -> Mapping {
        Mapping {
            source: Source {
                device: device.map(str::to_owned),
                channel: None,
                kind,
            },
            action,
            takeover: Takeover::Jump,
            glue: Glue::default(),
        }
    }

    #[test]
    fn a_button_fires_when_pressed_not_when_released() {
        let mut m = Mapper::new(&[map(
            None,
            SourceKind::Cc { number: 20 },
            ControlAction::SelectSlot { slot: 2 },
        )]);
        let c = ctx(&[0, 1, 2], &[0]);
        assert_eq!(
            m.handle(&cc("Pad", 0, 20, 127), &c),
            vec![Outcome::Select(2)]
        );
        assert!(
            m.handle(&cc("Pad", 0, 20, 0), &c).is_empty(),
            "release does nothing"
        );
        assert!(
            m.handle(&cc("Pad", 0, 21, 127), &c).is_empty(),
            "another control"
        );
        // A slot with nothing in it is not selected.
        assert!(
            m.handle(&cc("Pad", 0, 20, 127), &ctx(&[0, 1], &[0]))
                .is_empty()
        );
    }

    #[test]
    fn device_and_channel_narrow_a_mapping() {
        let mut one = map(
            Some("Pad"),
            SourceKind::Cc { number: 20 },
            ControlAction::Panic,
        );
        one.source.channel = Some(3);
        let mut m = Mapper::new(&[one]);
        let c = ctx(&[0], &[0]);
        assert_eq!(m.handle(&cc("Pad", 3, 20, 100), &c), vec![Outcome::Panic]);
        assert!(m.handle(&cc("Other", 3, 20, 100), &c).is_empty());
        assert!(m.handle(&cc("Pad", 4, 20, 100), &c).is_empty());
        assert!(
            m.is_mapped(&cc("Pad", 3, 20, 0)),
            "mapped even on release: it must not reach an instrument"
        );
        assert!(!m.is_mapped(&cc("Other", 3, 20, 0)));
    }

    #[test]
    fn program_changes_pick_a_slot_by_number() {
        let mut m = Mapper::new(&[map(
            None,
            SourceKind::Program,
            ControlAction::SelectByValue { base: 1 },
        )]);
        let c = ctx(&[0, 1, 2, 3], &[0]);
        let pc = |n| Incoming {
            device: "Keys".into(),
            channel: 0,
            message: Message::Program { number: n },
        };
        assert_eq!(m.handle(&pc(0), &c), vec![Outcome::Select(1)]);
        assert_eq!(m.handle(&pc(2), &c), vec![Outcome::Select(3)]);
        assert!(m.handle(&pc(9), &c).is_empty(), "no such slot");
    }

    #[test]
    fn next_and_previous_walk_the_loaded_slots_and_wrap() {
        let mut m = Mapper::new(&[
            map(None, SourceKind::Cc { number: 1 }, ControlAction::NextSlot),
            map(None, SourceKind::Cc { number: 2 }, ControlAction::PrevSlot),
        ]);
        // Slots 0, 2 and 5 have instruments; slot 2 plays.
        let c = ctx(&[0, 2, 5], &[2]);
        assert_eq!(m.handle(&cc("F", 0, 1, 127), &c), vec![Outcome::Select(5)]);
        assert_eq!(m.handle(&cc("F", 0, 2, 127), &c), vec![Outcome::Select(0)]);
        // From the last one, next wraps to the first; from the first, previous to the last.
        assert_eq!(
            m.handle(&cc("F", 0, 1, 127), &ctx(&[0, 2, 5], &[5])),
            vec![Outcome::Select(0)]
        );
        assert_eq!(
            m.handle(&cc("F", 0, 2, 127), &ctx(&[0, 2, 5], &[0])),
            vec![Outcome::Select(5)]
        );
        // Nothing playing: next starts at the first, previous at the last.
        assert_eq!(
            m.handle(&cc("F", 0, 1, 127), &ctx(&[0, 2, 5], &[])),
            vec![Outcome::Select(0)]
        );
        assert_eq!(
            m.handle(&cc("F", 0, 2, 127), &ctx(&[0, 2, 5], &[])),
            vec![Outcome::Select(5)]
        );
        assert!(m.handle(&cc("F", 0, 1, 127), &ctx(&[], &[])).is_empty());
    }

    #[test]
    fn notes_trigger_on_press_only() {
        let mut m = Mapper::new(&[map(
            Some("Pads"),
            SourceKind::Note { key: 36 },
            ControlAction::ToggleLayer { slot: 1 },
        )]);
        let c = ctx(&[0, 1], &[0]);
        let note = |on, vel| Incoming {
            device: "Pads".into(),
            channel: 9,
            message: Message::Note {
                key: 36,
                velocity: vel,
                on,
            },
        };
        assert_eq!(m.handle(&note(true, 90), &c), vec![Outcome::ToggleLayer(1)]);
        assert!(m.handle(&note(false, 0), &c).is_empty());
        assert!(
            m.handle(&note(true, 0), &c).is_empty(),
            "note-on with velocity 0 is a release"
        );
    }

    #[test]
    fn a_fader_follows_and_pickup_waits_for_the_value() {
        let mut jump = map(
            None,
            SourceKind::Cc { number: 7 },
            ControlAction::SlotVolume { slot: 0 },
        );
        let mut pick = jump.clone();
        jump.takeover = Takeover::Jump;
        pick.takeover = Takeover::Pickup;
        let mut c = ctx(&[0], &[0]);
        c.volumes[0] = 0.5;

        let mut j = Mapper::new(&[jump]);
        assert_eq!(
            j.handle(&cc("F", 0, 7, 0), &c),
            vec![Outcome::SetVolume(0, 0.0)],
            "jumps at once"
        );
        assert_eq!(
            j.handle(&cc("F", 0, 7, 127), &c),
            vec![Outcome::SetVolume(0, 1.0)]
        );

        let mut p = Mapper::new(&[pick]);
        // The fader is far below the volume (0.5): nothing happens...
        assert!(p.handle(&cc("F", 0, 7, 10), &c).is_empty());
        assert!(p.handle(&cc("F", 0, 7, 40), &c).is_empty());
        // ...until it passes it (63.5 of 127), then it follows, up and down.
        let picked = p.handle(&cc("F", 0, 7, 70), &c);
        assert_eq!(picked.len(), 1);
        assert!(matches!(picked[0], Outcome::SetVolume(0, v) if (v - 70.0 / 127.0).abs() < 1e-6));
        assert_eq!(
            p.handle(&cc("F", 0, 7, 20), &c),
            vec![Outcome::SetVolume(0, 20.0 / 127.0)]
        );
        // After the values were changed elsewhere it has to be picked up again.
        p.reset_pickup();
        assert!(p.handle(&cc("F", 0, 7, 5), &c).is_empty());
    }

    #[test]
    fn the_master_volume_is_a_fader_too() {
        let mut m = Mapper::new(&[map(
            None,
            SourceKind::Cc { number: 9 },
            ControlAction::MasterVolume,
        )]);
        let c = ctx(&[], &[]);
        assert_eq!(
            m.handle(&cc("F", 0, 9, 127), &c),
            vec![Outcome::SetMaster(1.0)]
        );
    }

    #[test]
    fn replacing_the_mappings_keeps_the_state_of_those_that_stay() {
        let mut pick = map(
            None,
            SourceKind::Cc { number: 7 },
            ControlAction::SlotVolume { slot: 0 },
        );
        pick.takeover = Takeover::Pickup;
        let mut m = Mapper::new(std::slice::from_ref(&pick));
        let mut c = ctx(&[0], &[0]);
        c.volumes[0] = 0.5;
        m.handle(&cc("F", 0, 7, 64), &c); // picked up
        m.set_mappings(&[
            map(None, SourceKind::Cc { number: 1 }, ControlAction::Panic),
            pick,
        ]);
        assert_eq!(
            m.handle(&cc("F", 0, 7, 10), &c),
            vec![Outcome::SetVolume(0, 10.0 / 127.0)]
        );
    }

    #[test]
    fn snapshots_are_recalled_by_button_by_number_and_in_turn() {
        let mut m = Mapper::new(&[
            map(
                None,
                SourceKind::Cc { number: 1 },
                ControlAction::RecallSnapshot { snapshot: 2 },
            ),
            map(
                None,
                SourceKind::Cc { number: 2 },
                ControlAction::NextSnapshot,
            ),
            map(
                None,
                SourceKind::Cc { number: 3 },
                ControlAction::PrevSnapshot,
            ),
            map(
                None,
                SourceKind::Program,
                ControlAction::RecallSnapshotByValue { base: 0 },
            ),
        ]);
        let mut c = ctx(&[0], &[0]);
        assert!(
            m.handle(&cc("F", 0, 1, 127), &c).is_empty(),
            "none saved yet"
        );
        c.snapshots = 3;
        assert_eq!(m.handle(&cc("F", 0, 1, 127), &c), vec![Outcome::Recall(2)]);
        // Nothing recalled yet: next starts at the first, previous at the last.
        assert_eq!(m.handle(&cc("F", 0, 2, 127), &c), vec![Outcome::Recall(0)]);
        assert_eq!(m.handle(&cc("F", 0, 3, 127), &c), vec![Outcome::Recall(2)]);
        c.snapshot = Some(2);
        assert_eq!(m.handle(&cc("F", 0, 2, 127), &c), vec![Outcome::Recall(0)]);
        c.snapshot = Some(0);
        assert_eq!(m.handle(&cc("F", 0, 3, 127), &c), vec![Outcome::Recall(2)]);
        let pc = Incoming {
            device: "K".into(),
            channel: 0,
            message: Message::Program { number: 1 },
        };
        assert_eq!(m.handle(&pc, &c), vec![Outcome::Recall(1)]);
    }

    #[test]
    fn a_knob_drives_a_macro_with_pickup() {
        let mut pick = map(
            None,
            SourceKind::Cc { number: 40 },
            ControlAction::SlotMacro { slot: 1, index: 2 },
        );
        pick.takeover = Takeover::Pickup;
        let mut m = Mapper::new(&[pick]);
        let mut c = ctx(&[0, 1], &[0]);
        c.macros[1][2] = 0.5;
        // Far from the macro's position: ignored until the knob gets there.
        assert!(m.handle(&cc("K", 0, 40, 5), &c).is_empty());
        assert_eq!(
            m.handle(&cc("K", 0, 40, 64), &c),
            vec![Outcome::SetMacro(1, 2, 64.0 / 127.0)]
        );
        // A slot with nothing loaded has no macros to move.
        let mut m = Mapper::new(&[map(
            None,
            SourceKind::Cc { number: 40 },
            ControlAction::SlotMacro { slot: 5, index: 0 },
        )]);
        assert!(m.handle(&cc("K", 0, 40, 64), &c).is_empty());
    }

    fn knob(action: ControlAction, takeover: Takeover, glue: Glue) -> Mapper {
        let mut m = map(None, SourceKind::Cc { number: 50 }, action);
        m.takeover = takeover;
        m.glue = glue;
        Mapper::new(&[m])
    }

    #[test]
    fn range_curve_and_direction_shape_the_value() {
        let glue = Glue {
            min: 0.2,
            max: 0.6,
            invert: true,
            ..Glue::default()
        };
        let mut m = knob(ControlAction::MasterVolume, Takeover::Jump, glue);
        let c = ctx(&[0], &[0]);
        assert_eq!(
            m.handle(&cc("K", 0, 50, 0), &c),
            vec![Outcome::SetMaster(0.6)]
        );
        let Some(Outcome::SetMaster(v)) = m.handle(&cc("K", 0, 50, 127), &c).pop() else {
            panic!("no outcome");
        };
        assert!((v - 0.2).abs() < 1e-6);
        let mut m = knob(
            ControlAction::MasterVolume,
            Takeover::Jump,
            Glue {
                curve: Curve::Soft,
                ..Glue::default()
            },
        );
        let Some(Outcome::SetMaster(v)) = m.handle(&cc("K", 0, 50, 64), &c).pop() else {
            panic!("no outcome");
        };
        assert!(v < 0.3, "soft curve: {v}");
    }

    #[test]
    fn an_encoder_moves_the_value_by_steps() {
        let glue = Glue {
            relative: Relative::TwosComplement,
            step: 0.1,
            ..Glue::default()
        };
        let mut m = knob(ControlAction::MasterVolume, Takeover::Jump, glue);
        let mut c = ctx(&[0], &[0]);
        c.master = 0.5;
        assert_eq!(
            m.handle(&cc("E", 0, 50, 2), &c),
            vec![Outcome::SetMaster(0.7)]
        );
        let Some(Outcome::SetMaster(v)) = m.handle(&cc("E", 0, 50, 127), &c).pop() else {
            panic!("no outcome");
        };
        assert!((v - 0.4).abs() < 1e-6, "one step down: {v}");
        // It stops at the ends.
        c.master = 0.95;
        assert_eq!(
            m.handle(&cc("E", 0, 50, 10), &c),
            vec![Outcome::SetMaster(1.0)]
        );
    }

    #[test]
    fn scale_takeover_meets_at_the_ends_without_a_jump() {
        let mut m = knob(
            ControlAction::MasterVolume,
            Takeover::Scale,
            Glue::default(),
        );
        let mut c = ctx(&[0], &[0]);
        c.master = 0.8;
        // The knob is at 0.25 (somewhere far from 0.8): the first message only reports it.
        assert!(m.handle(&cc("K", 0, 50, 32), &c).is_empty());
        // Moving up closes the gap proportionally: the value goes up a bit, not to the knob.
        let Some(Outcome::SetMaster(v)) = m.handle(&cc("K", 0, 50, 64), &c).pop() else {
            panic!("no outcome");
        };
        assert!(v > 0.8 && v < 0.9, "scaled up: {v}");
        // Moving down reduces it proportionally.
        let Some(Outcome::SetMaster(v)) = m.handle(&cc("K", 0, 50, 32), &c).pop() else {
            panic!("no outcome");
        };
        assert!(v < 0.8 && v > 0.3, "scaled down: {v}");
        // All the way up reaches the top.
        let Some(Outcome::SetMaster(v)) = m.handle(&cc("K", 0, 50, 127), &c).pop() else {
            panic!("no outcome");
        };
        assert!(v > 0.99, "{v}");
    }

    #[test]
    fn controls_that_follow_the_selection_drive_the_slot_that_plays() {
        let mut m = Mapper::new(&[
            map(
                None,
                SourceKind::Cc { number: 1 },
                ControlAction::SelectedMacro { index: 3 },
            ),
            map(
                None,
                SourceKind::Cc { number: 2 },
                ControlAction::SelectedVolume,
            ),
        ]);
        let mut c = ctx(&[0, 2], &[2]);
        c.primary = Some(2);
        assert_eq!(
            m.handle(&cc("K", 0, 1, 127), &c),
            vec![Outcome::SetMacro(2, 3, 1.0)]
        );
        assert_eq!(
            m.handle(&cc("K", 0, 2, 127), &c),
            vec![Outcome::SetVolume(2, 1.0)]
        );
        // Nothing plays: nothing to drive.
        c.primary = None;
        assert!(m.handle(&cc("K", 0, 1, 127), &c).is_empty());
    }
}
