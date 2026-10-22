//! Snapshots (plan §4.10): the performance state of a rig, saved inside the rig file. Recalling
//! one changes what plays and how it sounds without loading or unloading anything, so it is
//! instant; the plugins themselves (and their own settings) stay as they are.

use serde::{Deserialize, Serialize};

use crate::{FxSettings, MAX_INSERTS, MAX_SLOTS, MidiFilter};

/// Most snapshots a rig can have: the range a Program Change can reach.
pub const MAX_SNAPSHOTS: usize = 128;

/// A SoundFont slot's preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetChoice {
    pub bank: u16,
    pub program: u8,
}

/// One slot's part of a snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlotSnapshot {
    pub volume: f32,
    pub pan: f32,
    pub mute: bool,
    /// Zone, channel, velocity range and transpose.
    pub filter: MidiFilter,
    pub fx: FxSettings,
    /// Per insert position: bypassed?
    pub insert_bypass: Vec<bool>,
    /// For a SoundFont slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetChoice>,
}

impl Default for SlotSnapshot {
    fn default() -> Self {
        Self {
            volume: 1.0,
            pan: 0.0,
            mute: false,
            filter: MidiFilter::default(),
            fx: FxSettings::default(),
            insert_bypass: Vec::new(),
            preset: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub name: String,
    /// Slots that play (keyboard targets).
    pub selected: Vec<usize>,
    pub master_volume: f32,
    /// By slot position; a snapshot taken with fewer slots leaves the later ones alone.
    pub slots: Vec<SlotSnapshot>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            name: String::new(),
            selected: Vec::new(),
            master_volume: 1.0,
            slots: Vec::new(),
        }
    }
}

impl Snapshot {
    /// Is it something the app can apply?
    pub fn is_valid(&self) -> bool {
        self.slots.len() <= MAX_SLOTS
            && self.selected.iter().all(|&i| i < MAX_SLOTS)
            && self.slots.iter().all(|s| {
                let f = &s.filter;
                s.insert_bypass.len() <= MAX_INSERTS
                    && f.key_lo <= f.key_hi
                    && f.key_hi <= 127
                    && f.vel_lo <= f.vel_hi
                    && f.vel_hi <= 127
                    && f.channel.is_none_or(|c| c < 16)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sparse_snapshot_loads_with_defaults() {
        let s: Snapshot =
            serde_json::from_str(r#"{"name":"Verse","slots":[{"mute":true}]}"#).unwrap();
        assert_eq!(s.master_volume, 1.0);
        assert!(s.slots[0].mute && s.slots[0].volume == 1.0);
        assert!(s.is_valid());
    }

    #[test]
    fn bad_snapshots_are_caught() {
        let s = Snapshot {
            selected: vec![MAX_SLOTS],
            ..Snapshot::default()
        };
        assert!(!s.is_valid());
        let s = Snapshot {
            slots: vec![SlotSnapshot::default(); MAX_SLOTS + 1],
            ..Snapshot::default()
        };
        assert!(!s.is_valid());
    }
}
