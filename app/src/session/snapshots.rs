//! Snapshots on the session (plan §4.10, Phase 3): capture the performance state of the rig
//! (which slots play, volumes, zones, effects on or off, SoundFont presets) and bring it back
//! at once. They are stored in the rig, so one file holds the whole setup.

use merlin_engine::Command;
use merlin_model::{MAX_SNAPSHOTS, PresetChoice, SlotSnapshot, Snapshot};

use super::{Session, UI_SLOTS};

impl Session {
    pub fn snapshots(&self) -> &[Snapshot] {
        &self.rig.snapshots
    }

    /// The snapshot recalled last (until the rig is replaced or that snapshot is removed).
    pub fn current_snapshot(&self) -> Option<usize> {
        self.last_snapshot
    }

    /// What the rig is doing now, as a snapshot.
    fn capture_snapshot(&self, name: String) -> Snapshot {
        let slots = (0..UI_SLOTS)
            .map(|i| {
                let s = &self.rig.slots[i];
                SlotSnapshot {
                    volume: s.volume,
                    pan: s.pan,
                    mute: s.mute,
                    filter: s.filter.clone(),
                    fx: s.fx,
                    insert_bypass: s
                        .inserts
                        .iter()
                        .map(|ins| ins.as_ref().is_some_and(|ins| ins.bypass))
                        .collect(),
                    preset: self
                        .preset(i)
                        .map(|(bank, program)| PresetChoice { bank, program }),
                }
            })
            .collect();
        Snapshot {
            name,
            selected: self.targets.clone(),
            master_volume: self.rig.master_volume,
            slots,
        }
    }

    /// Saves the current state as a new snapshot and returns its position.
    pub fn add_snapshot(&mut self) -> Option<usize> {
        if self.rig.snapshots.len() >= MAX_SNAPSHOTS {
            self.say(format!("at most {MAX_SNAPSHOTS} snapshots"));
            return None;
        }
        let name = format!("Snapshot {}", self.rig.snapshots.len() + 1);
        let snap = self.capture_snapshot(name);
        self.rig.snapshots.push(snap);
        let n = self.rig.snapshots.len() - 1;
        self.last_snapshot = Some(n);
        self.dirty = true;
        self.say(format!("saved snapshot {}", n + 1));
        Some(n)
    }

    /// Replaces a snapshot's contents with the current state, keeping its name.
    pub fn update_snapshot(&mut self, n: usize) {
        let Some(old) = self.rig.snapshots.get(n) else {
            return;
        };
        let snap = self.capture_snapshot(old.name.clone());
        self.rig.snapshots[n] = snap;
        self.last_snapshot = Some(n);
        self.dirty = true;
        self.say(format!("snapshot {} updated", n + 1));
    }

    pub fn rename_snapshot(&mut self, n: usize, name: String) {
        if let Some(s) = self.rig.snapshots.get_mut(n) {
            s.name = name;
            self.dirty = true;
        }
    }

    pub fn remove_snapshot(&mut self, n: usize) {
        if n >= self.rig.snapshots.len() {
            return;
        }
        self.rig.snapshots.remove(n);
        self.last_snapshot = match self.last_snapshot {
            Some(c) if c == n => None,
            Some(c) if c > n => Some(c - 1),
            other => other,
        };
        self.dirty = true;
    }

    /// Brings a snapshot back. Nothing is loaded or unloaded: slots that have no instrument
    /// get their numbers and nothing else.
    pub fn recall_snapshot(&mut self, n: usize) {
        let Some(snap) = self.rig.snapshots.get(n).cloned() else {
            return;
        };
        self.set_master_volume(snap.master_volume);
        for (i, s) in snap.slots.iter().enumerate().take(UI_SLOTS) {
            let slot = &mut self.rig.slots[i];
            slot.volume = s.volume;
            slot.pan = s.pan;
            slot.mute = s.mute;
            slot.filter = s.filter.clone();
            slot.fx = s.fx;
            slot.fx.clamp();
            let mut flips = Vec::new();
            for (k, &bypass) in s.insert_bypass.iter().enumerate() {
                if let Some(Some(ins)) = slot.inserts.get_mut(k)
                    && ins.bypass != bypass
                {
                    ins.bypass = bypass;
                    flips.push((k, bypass));
                }
            }
            self.push_slot_settings(i);
            for (index, bypass) in flips {
                let _ = self.engine.send(Command::SetInsertBypass {
                    slot: i,
                    index,
                    bypass,
                });
            }
            if let Some(p) = s.preset
                && self.preset(i).is_some_and(|now| now != (p.bank, p.program))
            {
                self.set_preset(i, p.bank, p.program);
            }
        }
        let wanted: Vec<usize> = snap
            .selected
            .iter()
            .copied()
            .filter(|&i| i < UI_SLOTS && self.plugins[i].is_some())
            .collect();
        let mut same = wanted.clone();
        let mut now = self.targets.clone();
        same.sort_unstable();
        now.sort_unstable();
        if !wanted.is_empty() && same != now {
            for (k, &i) in wanted.iter().enumerate() {
                self.select(i, k > 0);
            }
        }
        // The knobs now disagree with the values they control.
        self.mapper.reset_pickup();
        self.last_snapshot = Some(n);
        self.dirty = true;
        self.say(format!("snapshot {}: {}", n + 1, snap.name));
    }
}
