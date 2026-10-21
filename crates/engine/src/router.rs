//! MIDI routing and note ownership (plan §4.4): what makes switching seamless.
//!
//! Pure logic with preallocated state, so it is exhaustively testable and safe to run on the
//! audio thread.
//!
//! - A note-on goes to every target slot whose filter accepts it, and the router remembers
//!   *which slots got it and as which (transposed) note*, per (input, channel, key).
//! - The note-off, and poly pressure, always go to those slots, whatever was selected or
//!   re-zoned in between. So no note can get stuck by switching.
//! - Pedal (CC64): after a switch the old slot keeps getting pedal events until the pedal
//!   comes up. Whether a slot that gains focus inherits a pedal that is already down is a
//!   setting.
//! - Pitch bend, mod wheel and the other controllers follow the current targets, and are
//!   reset on a slot that loses focus.
//! - Panic: note-offs from the ownership table, then CC64=0, CC120 and CC123 on every channel
//!   of every loaded slot.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use merlin_hosting::{MidiEvent, MidiKind};
use merlin_model::{MAX_SLOTS, MidiFilter, SelectMode};

use crate::Shared;
use crate::events::EventBuf;

/// MIDI inputs the engine distinguishes (keyboard, controller, ...).
pub const MAX_INPUTS: usize = 4;

/// A slot's filter as the audio thread sees it (input names resolved to indices).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter {
    pub input: Option<u8>,
    pub channel: Option<u8>,
    pub key_lo: u8,
    pub key_hi: u8,
    pub vel_lo: u8,
    pub vel_hi: u8,
    pub transpose: i8,
}

impl Filter {
    pub const ALL: Filter = Filter {
        input: None,
        channel: None,
        key_lo: 0,
        key_hi: 127,
        vel_lo: 0,
        vel_hi: 127,
        transpose: 0,
    };

    /// Builds the audio-thread filter from the saved one. `input_index` maps a device name
    /// to the engine's input index; an unknown device name listens to nothing.
    pub fn from_model(f: &MidiFilter, input_index: impl Fn(&str) -> Option<u8>) -> Self {
        Self {
            input: f
                .input
                .as_deref()
                .map(|n| input_index(n).unwrap_or(u8::MAX)),
            channel: f.channel,
            key_lo: f.key_lo,
            key_hi: f.key_hi,
            vel_lo: f.vel_lo,
            vel_hi: f.vel_hi,
            transpose: f.transpose,
        }
    }

    fn accepts_source(&self, input: u8, ch: u8) -> bool {
        self.input.is_none_or(|i| i == input) && self.channel.is_none_or(|c| c == ch)
    }

    /// The transposed key if a note-on with this key and velocity passes.
    fn accepts_note(&self, input: u8, ch: u8, key: u8, vel: u8) -> Option<u8> {
        if !self.accepts_source(input, ch)
            || !(self.key_lo..=self.key_hi).contains(&key)
            || !(self.vel_lo..=self.vel_hi).contains(&vel)
        {
            return None;
        }
        let t = key as i16 + self.transpose as i16;
        (0..=127).contains(&t).then_some(t as u8)
    }
}

/// Who got a note-on, and as which key.
#[derive(Clone, Copy)]
struct Owner {
    slots: u16,
    keys: [u8; MAX_SLOTS],
}

const NO_OWNER: Owner = Owner {
    slots: 0,
    keys: [0; MAX_SLOTS],
};

fn bits(mask: u16) -> impl Iterator<Item = usize> {
    (0..MAX_SLOTS).filter(move |i| mask & (1 << i) != 0)
}

pub struct Router {
    filters: [Filter; MAX_SLOTS],
    /// Slots that have an instrument.
    loaded: u16,
    /// Slots that receive new notes.
    targets: u16,
    owners: Box<[Owner]>,
    /// Slots that were sent a pedal-down and not yet the matching pedal-up.
    pedal_owners: u16,
    pedal_down: bool,
    pub inherit_pedal: bool,
    shared: Arc<Shared>,
}

fn owner_index(input: u8, ch: u8, key: u8) -> usize {
    (input as usize % MAX_INPUTS) * 16 * 128 + (ch as usize & 15) * 128 + (key as usize & 127)
}

impl Router {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            filters: [Filter::ALL; MAX_SLOTS],
            loaded: 0,
            targets: 0,
            owners: vec![NO_OWNER; MAX_INPUTS * 16 * 128].into_boxed_slice(),
            pedal_owners: 0,
            pedal_down: false,
            inherit_pedal: false,
            shared,
        }
    }

    pub fn targets(&self) -> u16 {
        self.targets
    }

    pub fn loaded(&self) -> u16 {
        self.loaded
    }

    pub fn set_filter(&mut self, slot: usize, f: Filter) {
        self.filters[slot] = f;
    }

    pub fn set_loaded(&mut self, slot: usize, loaded: bool) {
        if loaded {
            self.loaded |= 1 << slot;
        } else {
            self.loaded &= !(1 << slot);
        }
    }

    /// Forgets a slot (its instrument was removed): it no longer owns notes or a pedal.
    pub fn drop_slot(&mut self, slot: usize) {
        let bit = 1u16 << slot;
        for idx in 0..self.owners.len() {
            if self.owners[idx].slots & bit != 0 {
                self.owners[idx].slots &= !bit;
                self.refresh_held((idx % 128) as u8);
            }
        }
        self.pedal_owners &= !bit;
        self.loaded &= !bit;
    }

    /// Is any note still owned by a slot? (for tests and panic checks)
    pub fn held_notes(&self) -> usize {
        self.owners.iter().filter(|o| o.slots != 0).count()
    }

    fn refresh_held(&self, key: u8) {
        let mut mask = 0u16;
        for input in 0..MAX_INPUTS as u8 {
            for ch in 0..16 {
                mask |= self.owners[owner_index(input, ch, key)].slots;
            }
        }
        self.shared.held[key as usize].store(mask, Ordering::Relaxed);
    }

    /// Sends the note-offs for whoever owns this note and forgets the note.
    fn release(&mut self, input: u8, ch: u8, key: u8, time: u32, out: &mut [EventBuf]) {
        let idx = owner_index(input, ch, key);
        let owner = std::mem::replace(&mut self.owners[idx], NO_OWNER);
        for s in bits(owner.slots) {
            out[s].push(MidiEvent::new(time, input, [0x80 | ch, owner.keys[s], 0]));
        }
        self.refresh_held(key);
    }

    /// Routes one incoming event into the per-slot lists.
    pub fn route(&mut self, ev: &MidiEvent, out: &mut [EventBuf]) {
        let input = ev.input;
        match ev.kind() {
            MidiKind::NoteOn { ch, key, vel } => {
                // A retrigger of a key that is still down releases the old note first.
                if self.owners[owner_index(input, ch, key)].slots != 0 {
                    self.release(input, ch, key, ev.time, out);
                }
                let mut owner = NO_OWNER;
                for s in bits(self.targets & self.loaded) {
                    if let Some(k) = self.filters[s].accepts_note(input, ch, key, vel) {
                        out[s].push(MidiEvent::new(ev.time, input, [0x90 | ch, k, vel]));
                        owner.slots |= 1 << s;
                        owner.keys[s] = k;
                    }
                }
                self.owners[owner_index(input, ch, key)] = owner;
                self.refresh_held(key);
            }
            MidiKind::NoteOff { ch, key, .. } => self.release(input, ch, key, ev.time, out),
            MidiKind::PolyPressure { ch, key, value } => {
                let owner = self.owners[owner_index(input, ch, key)];
                for s in bits(owner.slots) {
                    out[s].push(MidiEvent::poly_pressure(ev.time, ch, owner.keys[s], value));
                }
            }
            MidiKind::Cc { ch, cc: 64, value } => self.pedal(input, ch, value, ev, out),
            MidiKind::Cc {
                ch, cc: 120 | 123, ..
            } => {
                // "All notes off" from the keyboard: forward everywhere, forget this
                // channel's notes.
                for s in bits(self.loaded) {
                    out[s].push(*ev);
                }
                for key in 0..128u8 {
                    let idx = owner_index(input, ch, key);
                    if self.owners[idx].slots != 0 {
                        self.owners[idx] = NO_OWNER;
                        self.refresh_held(key);
                    }
                }
            }
            MidiKind::Cc { ch, .. }
            | MidiKind::PitchBend { ch, .. }
            | MidiKind::ChannelPressure { ch, .. } => {
                for s in bits(self.targets & self.loaded) {
                    if self.filters[s].accepts_source(input, ch) {
                        out[s].push(*ev);
                    }
                }
            }
            // Program change is reserved for switching (Phase 3).
            MidiKind::ProgramChange { .. } | MidiKind::Other => {}
        }
    }

    fn pedal(&mut self, input: u8, ch: u8, value: u8, ev: &MidiEvent, out: &mut [EventBuf]) {
        let mut targets = 0u16;
        for s in bits(self.targets & self.loaded) {
            if self.filters[s].accepts_source(input, ch) {
                targets |= 1 << s;
            }
        }
        if value >= 64 {
            self.pedal_down = true;
            self.pedal_owners |= targets;
            for s in bits(targets) {
                out[s].push(*ev);
            }
        } else {
            self.pedal_down = false;
            // Whoever was sent a pedal-down gets the pedal-up, however selection changed.
            let release = (self.pedal_owners | targets) & self.loaded;
            self.pedal_owners = 0;
            for s in bits(release) {
                out[s].push(*ev);
            }
        }
    }

    /// Changes the keyboard targets. Slots that lose focus get pitch bend and mod wheel reset;
    /// held notes and tails are untouched.
    pub fn select(&mut self, slot: usize, mode: SelectMode, out: &mut [EventBuf]) {
        let bit = 1u16 << slot;
        let new = match mode {
            SelectMode::Exclusive => bit,
            SelectMode::Layer => self.targets ^ bit,
        };
        let lost = self.targets & !new & self.loaded;
        let gained = new & !self.targets & self.loaded;
        for s in bits(lost) {
            let ch = self.filters[s].channel.unwrap_or(0);
            out[s].push(MidiEvent::pitch_bend(0, ch, 8192));
            out[s].push(MidiEvent::cc(0, ch, 1, 0));
        }
        if self.inherit_pedal && self.pedal_down {
            for s in bits(gained) {
                let ch = self.filters[s].channel.unwrap_or(0);
                out[s].push(MidiEvent::cc(0, ch, 64, 127));
                self.pedal_owners |= 1 << s;
            }
        }
        self.targets = new;
    }

    /// Releases every held note and silences every loaded slot's controllers.
    pub fn panic(&mut self, out: &mut [EventBuf]) {
        for idx in 0..self.owners.len() {
            let owner = std::mem::replace(&mut self.owners[idx], NO_OWNER);
            let ch = ((idx / 128) % 16) as u8;
            for s in bits(owner.slots) {
                out[s].push(MidiEvent::new(0, 0, [0x80 | ch, owner.keys[s], 0]));
            }
        }
        for key in 0..128 {
            self.shared.held[key].store(0, Ordering::Relaxed);
        }
        for s in bits(self.loaded) {
            for ch in 0..16 {
                out[s].push(MidiEvent::cc(0, ch, 64, 0));
                out[s].push(MidiEvent::cc(0, ch, 120, 0));
                out[s].push(MidiEvent::cc(0, ch, 123, 0));
            }
        }
        self.pedal_owners = 0;
        self.pedal_down = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn router(loaded: &[usize]) -> (Router, Vec<EventBuf>) {
        let mut r = Router::new(Arc::new(Shared::new()));
        for &s in loaded {
            r.set_loaded(s, true);
        }
        (r, (0..MAX_SLOTS).map(|_| EventBuf::new()).collect())
    }

    fn take(out: &mut [EventBuf], slot: usize) -> Vec<MidiEvent> {
        let v = out[slot].as_slice().to_vec();
        out[slot].clear();
        v
    }

    fn on(key: u8) -> MidiEvent {
        MidiEvent::note_on(0, 0, key, 100)
    }

    fn off(key: u8) -> MidiEvent {
        MidiEvent::note_off(0, 0, key)
    }

    #[test]
    fn exclusive_selection_sends_notes_only_to_the_target() {
        let (mut r, mut out) = router(&[0, 1]);
        r.select(1, SelectMode::Exclusive, &mut out);
        r.route(&on(60), &mut out);
        assert!(take(&mut out, 0).is_empty());
        assert_eq!(take(&mut out, 1), vec![on(60)]);
    }

    #[test]
    fn note_off_follows_the_note_after_a_switch() {
        let (mut r, mut out) = router(&[0, 1]);
        r.select(0, SelectMode::Exclusive, &mut out);
        r.route(&on(60), &mut out);
        r.select(1, SelectMode::Exclusive, &mut out);
        take(&mut out, 0);
        take(&mut out, 1);
        r.route(&off(60), &mut out);
        assert_eq!(
            take(&mut out, 0),
            vec![off(60)],
            "old slot gets the note-off"
        );
        assert!(take(&mut out, 1).is_empty(), "new slot never saw the note");
        assert_eq!(r.held_notes(), 0);
    }

    #[test]
    fn layering_and_transpose_each_slot_gets_its_own_key() {
        let (mut r, mut out) = router(&[0, 1]);
        r.set_filter(
            1,
            Filter {
                transpose: -12,
                ..Filter::ALL
            },
        );
        r.select(0, SelectMode::Exclusive, &mut out);
        r.select(1, SelectMode::Layer, &mut out);
        r.route(&on(60), &mut out);
        assert_eq!(take(&mut out, 0), vec![on(60)]);
        assert_eq!(take(&mut out, 1), vec![on(48)]);
        r.route(&off(60), &mut out);
        assert_eq!(take(&mut out, 0), vec![off(60)]);
        assert_eq!(
            take(&mut out, 1),
            vec![off(48)],
            "note-off carries the transposed key"
        );
    }

    #[test]
    fn zones_split_the_keyboard() {
        let (mut r, mut out) = router(&[0, 1]);
        r.set_filter(
            0,
            Filter {
                key_hi: 59,
                ..Filter::ALL
            },
        );
        r.set_filter(
            1,
            Filter {
                key_lo: 60,
                ..Filter::ALL
            },
        );
        r.select(0, SelectMode::Exclusive, &mut out);
        r.select(1, SelectMode::Layer, &mut out);
        r.route(&on(48), &mut out);
        r.route(&on(72), &mut out);
        assert_eq!(take(&mut out, 0), vec![on(48)]);
        assert_eq!(take(&mut out, 1), vec![on(72)]);
    }

    #[test]
    fn velocity_zone_and_transpose_range() {
        let (mut r, mut out) = router(&[0]);
        r.set_filter(
            0,
            Filter {
                vel_lo: 50,
                transpose: 12,
                ..Filter::ALL
            },
        );
        r.select(0, SelectMode::Exclusive, &mut out);
        r.route(&MidiEvent::note_on(0, 0, 60, 49), &mut out);
        assert!(take(&mut out, 0).is_empty(), "too soft");
        r.route(&MidiEvent::note_on(0, 0, 120, 100), &mut out);
        assert!(take(&mut out, 0).is_empty(), "transposed out of range");
        assert_eq!(r.held_notes(), 0, "nobody took the note, so nobody owns it");
        r.route(&off(120), &mut out);
        assert!(
            take(&mut out, 0).is_empty(),
            "and no stray note-off is sent"
        );
    }

    #[test]
    fn channel_and_input_filters() {
        let (mut r, mut out) = router(&[0, 1]);
        r.set_filter(
            0,
            Filter {
                channel: Some(2),
                ..Filter::ALL
            },
        );
        r.set_filter(
            1,
            Filter {
                input: Some(1),
                ..Filter::ALL
            },
        );
        r.select(0, SelectMode::Exclusive, &mut out);
        r.select(1, SelectMode::Layer, &mut out);
        r.route(&MidiEvent::note_on(0, 2, 60, 100), &mut out);
        assert_eq!(take(&mut out, 0).len(), 1);
        assert_eq!(take(&mut out, 1).len(), 0, "wrong input");
        let mut ev = MidiEvent::note_on(0, 5, 62, 100);
        ev.input = 1;
        r.route(&ev, &mut out);
        assert_eq!(take(&mut out, 0).len(), 0, "wrong channel");
        assert_eq!(take(&mut out, 1).len(), 1);
    }

    #[test]
    fn retrigger_releases_the_old_note_first() {
        let (mut r, mut out) = router(&[0]);
        r.select(0, SelectMode::Exclusive, &mut out);
        r.route(&on(60), &mut out);
        r.route(&on(60), &mut out);
        assert_eq!(take(&mut out, 0), vec![on(60), off(60), on(60)]);
    }

    #[test]
    fn pedal_up_reaches_the_slot_that_got_the_pedal_down() {
        let (mut r, mut out) = router(&[0, 1]);
        r.select(0, SelectMode::Exclusive, &mut out);
        r.route(&MidiEvent::cc(0, 0, 64, 127), &mut out);
        r.select(1, SelectMode::Exclusive, &mut out);
        take(&mut out, 0);
        take(&mut out, 1);
        r.route(&MidiEvent::cc(0, 0, 64, 0), &mut out);
        assert_eq!(take(&mut out, 0), vec![MidiEvent::cc(0, 0, 64, 0)]);
        assert_eq!(take(&mut out, 1), vec![MidiEvent::cc(0, 0, 64, 0)]);
    }

    #[test]
    fn pedal_inheritance_is_a_setting() {
        for inherit in [false, true] {
            let (mut r, mut out) = router(&[0, 1]);
            r.inherit_pedal = inherit;
            r.select(0, SelectMode::Exclusive, &mut out);
            r.route(&MidiEvent::cc(0, 0, 64, 127), &mut out);
            r.select(1, SelectMode::Exclusive, &mut out);
            let got = take(&mut out, 1);
            let has_pedal = got.contains(&MidiEvent::cc(0, 0, 64, 127));
            assert_eq!(has_pedal, inherit);
        }
    }

    #[test]
    fn controllers_follow_the_target_and_reset_on_the_one_that_loses_focus() {
        let (mut r, mut out) = router(&[0, 1]);
        r.select(0, SelectMode::Exclusive, &mut out);
        r.route(&MidiEvent::pitch_bend(0, 0, 12000), &mut out);
        r.route(&MidiEvent::cc(0, 0, 1, 90), &mut out);
        assert_eq!(take(&mut out, 0).len(), 2);
        assert!(take(&mut out, 1).is_empty());
        r.select(1, SelectMode::Exclusive, &mut out);
        let reset = take(&mut out, 0);
        assert_eq!(
            reset,
            vec![MidiEvent::pitch_bend(0, 0, 8192), MidiEvent::cc(0, 0, 1, 0)]
        );
    }

    #[test]
    fn panic_releases_everything_and_clears_state() {
        let (mut r, mut out) = router(&[0, 1]);
        r.select(0, SelectMode::Exclusive, &mut out);
        r.select(1, SelectMode::Layer, &mut out);
        r.route(&on(60), &mut out);
        r.route(&on(64), &mut out);
        out.iter_mut().for_each(EventBuf::clear);
        r.panic(&mut out);
        for s in [0, 1] {
            let ev = take(&mut out, s);
            assert!(ev.contains(&off(60)) && ev.contains(&off(64)));
            for ch in 0..16 {
                for cc in [64, 120, 123] {
                    assert!(
                        ev.contains(&MidiEvent::cc(0, ch, cc, 0)),
                        "slot {s} ch {ch} cc {cc}"
                    );
                }
            }
        }
        assert_eq!(r.held_notes(), 0);
        // A late note-off from the keyboard is harmless.
        r.route(&off(60), &mut out);
        assert!(take(&mut out, 0).is_empty());
    }

    #[test]
    fn removing_a_slot_drops_its_notes() {
        let (mut r, mut out) = router(&[0]);
        r.select(0, SelectMode::Exclusive, &mut out);
        r.route(&on(60), &mut out);
        r.drop_slot(0);
        assert_eq!(r.held_notes(), 0);
        assert_eq!(r.shared.held[60].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn keyboard_all_notes_off_clears_that_channel() {
        let (mut r, mut out) = router(&[0]);
        r.select(0, SelectMode::Exclusive, &mut out);
        r.route(&on(60), &mut out);
        r.route(&MidiEvent::cc(0, 0, 123, 0), &mut out);
        assert_eq!(r.held_notes(), 0);
    }

    #[derive(Clone, Debug)]
    enum Op {
        On(u8, u8),
        Off(u8, u8),
        Pedal(bool),
        Select(usize, bool),
        Filter(usize, u8, u8, i8),
        Unload(usize),
        Load(usize),
        Panic,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0u8..3, 36u8..96).prop_map(|(c, k)| Op::On(c, k)),
            4 => (0u8..3, 36u8..96).prop_map(|(c, k)| Op::Off(c, k)),
            1 => any::<bool>().prop_map(Op::Pedal),
            2 => (0usize..4, any::<bool>()).prop_map(|(s, l)| Op::Select(s, l)),
            1 => (0usize..4, 30u8..70, 60u8..110, -14i8..14).prop_map(|(s, a, b, t)| Op::Filter(s, a, b, t)),
            1 => (0usize..4).prop_map(Op::Unload),
            1 => (0usize..4).prop_map(Op::Load),
            1 => Just(Op::Panic),
        ]
    }

    proptest! {
        /// Every note-on a slot receives is matched by exactly one note-off for the same key,
        /// once the player has released their keys, whatever the selection, zones and loading
        /// did in between. (A slot that is unloaded mid-note is the one exception: it stops
        /// owning the note, which is the intent.)
        #[test]
        fn no_stuck_notes(ops in prop::collection::vec(op(), 0..200)) {
            let (mut r, mut out) = router(&[0, 1, 2, 3]);
            let mut counts = std::collections::HashMap::<(usize, u8), i32>::new();
            let mut down = std::collections::HashSet::<(u8, u8)>::new();
            type Counts = std::collections::HashMap<(usize, u8), i32>;
            let account = |out: &mut Vec<EventBuf>, counts: &mut Counts| {
                for (s, buf) in out.iter_mut().enumerate() {
                    for ev in buf.as_slice() {
                        match ev.kind() {
                            MidiKind::NoteOn { key, .. } => *counts.entry((s, key)).or_default() += 1,
                            MidiKind::NoteOff { key, .. } => *counts.entry((s, key)).or_default() -= 1,
                            _ => {}
                        }
                    }
                    buf.clear();
                }
            };
            for op in ops {
                match op {
                    Op::On(c, k) => { r.route(&MidiEvent::note_on(0, c, k, 90), &mut out); down.insert((c, k)); }
                    Op::Off(c, k) => { r.route(&MidiEvent::note_off(0, c, k), &mut out); down.remove(&(c, k)); }
                    Op::Pedal(d) => r.route(&MidiEvent::cc(0, 0, 64, if d { 127 } else { 0 }), &mut out),
                    Op::Select(s, layer) => r.select(s, if layer { SelectMode::Layer } else { SelectMode::Exclusive }, &mut out),
                    Op::Filter(s, lo, hi, t) => r.set_filter(s, Filter { key_lo: lo.min(hi), key_hi: hi.max(lo), transpose: t, ..Filter::ALL }),
                    Op::Unload(s) => {
                        // The slot disappears together with its notes: settle its counts.
                        account(&mut out, &mut counts);
                        r.drop_slot(s);
                        counts.retain(|(slot, _), _| *slot != s);
                    }
                    Op::Load(s) => r.set_loaded(s, true),
                    Op::Panic => { r.panic(&mut out); down.clear(); }
                }
                account(&mut out, &mut counts);
            }
            // The player lets go of everything.
            for (c, k) in down {
                r.route(&MidiEvent::note_off(0, c, k), &mut out);
            }
            account(&mut out, &mut counts);
            prop_assert_eq!(r.held_notes(), 0);
            for ((slot, key), n) in counts {
                prop_assert_eq!(n, 0, "slot {} key {} unbalanced", slot, key);
            }
        }
    }
}
