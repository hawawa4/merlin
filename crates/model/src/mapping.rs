//! MIDI mappings (plan §4.10): a *source* (a control on a device) drives an *action* (a thing
//! the app can do). The same actions are what the UI buttons do.

use serde::{Deserialize, Serialize};

use crate::{MACROS_PER_SLOT, MAX_SLOTS, MAX_SNAPSHOTS};

/// Which message on which device a mapping listens to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// The device's name as the MIDI settings show it; `None` listens to every device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// MIDI channel 0-15; `None` listens to all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    pub kind: SourceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceKind {
    /// A control change: knob, fader, pedal, button.
    Cc { number: u8 },
    /// A key or pad, by note number.
    Note { key: u8 },
    /// Any Program Change; the program number is the value.
    Program,
}

/// What a mapping does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlAction {
    /// Make the slot the only one that plays.
    SelectSlot {
        slot: u8,
    },
    /// Add the slot to what plays, or take it out.
    ToggleLayer {
        slot: u8,
    },
    /// Move to the next / previous loaded slot (a foot switch).
    NextSlot,
    PrevSlot,
    ToggleMute {
        slot: u8,
    },
    /// The value picks the slot: `base + value` (a Program Change of 0 selects slot `base`).
    SelectByValue {
        base: u8,
    },
    /// A fader or knob for one slot's volume.
    SlotVolume {
        slot: u8,
    },
    MasterVolume,
    /// A knob for one of a slot's macros.
    SlotMacro {
        slot: u8,
        index: u8,
    },
    /// A knob for a macro of whichever slot plays now (the first selected one).
    SelectedMacro {
        index: u8,
    },
    /// A fader for the volume of whichever slot plays now.
    SelectedVolume,
    /// Recall a snapshot.
    RecallSnapshot {
        snapshot: u8,
    },
    NextSnapshot,
    PrevSnapshot,
    /// The value picks the snapshot: `base + value`.
    RecallSnapshotByValue {
        base: u8,
    },
    /// All notes off.
    Panic,
}

impl ControlAction {
    /// Follows a knob or fader (as opposed to firing once per press).
    pub fn is_continuous(&self) -> bool {
        matches!(
            self,
            Self::SlotVolume { .. }
                | Self::MasterVolume
                | Self::SlotMacro { .. }
                | Self::SelectedMacro { .. }
                | Self::SelectedVolume
        )
    }

    fn slot(&self) -> Option<u8> {
        match *self {
            Self::SelectSlot { slot }
            | Self::ToggleLayer { slot }
            | Self::ToggleMute { slot }
            | Self::SlotVolume { slot }
            | Self::SlotMacro { slot, .. } => Some(slot),
            _ => None,
        }
    }

    /// A short phrase for lists, slots counted from 1.
    pub fn describe(&self) -> String {
        match *self {
            Self::SelectSlot { slot } => format!("Select slot {}", slot as usize + 1),
            Self::ToggleLayer { slot } => format!("Layer slot {} on/off", slot as usize + 1),
            Self::NextSlot => "Next slot".into(),
            Self::PrevSlot => "Previous slot".into(),
            Self::ToggleMute { slot } => format!("Mute slot {} on/off", slot as usize + 1),
            Self::SelectByValue { base } => {
                format!(
                    "Select slot by number (value 0 = slot {})",
                    base as usize + 1
                )
            }
            Self::SlotVolume { slot } => format!("Volume of slot {}", slot as usize + 1),
            Self::MasterVolume => "Master volume".into(),
            Self::SelectedMacro { index } => {
                format!("Macro {} of the slot that plays", index as usize + 1)
            }
            Self::SelectedVolume => "Volume of the slot that plays".into(),
            Self::SlotMacro { slot, index } => {
                format!("Macro {} of slot {}", index as usize + 1, slot as usize + 1)
            }
            Self::RecallSnapshot { snapshot } => {
                format!("Recall snapshot {}", snapshot as usize + 1)
            }
            Self::NextSnapshot => "Next snapshot".into(),
            Self::PrevSnapshot => "Previous snapshot".into(),
            Self::RecallSnapshotByValue { base } => format!(
                "Recall snapshot by number (value 0 = snapshot {})",
                base as usize + 1
            ),
            Self::Panic => "Panic (all notes off)".into(),
        }
    }
}

/// What a knob does when its position and the value it controls disagree (after a switch).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Takeover {
    /// The value jumps to the knob.
    #[default]
    Jump,
    /// The knob does nothing until it passes the current value, so it never makes a jump.
    Pickup,
    /// The value moves with the knob but is scaled so both reach the end of their range
    /// together: no jump, and no dead stretch.
    Scale,
}

/// How a knob's position turns into the value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    #[default]
    Linear,
    /// Fine control at the low end (position squared).
    Soft,
    /// Fine control at the high end (square root of the position).
    Hard,
}

/// An endless encoder sends how far it turned, not where it is. The encodings differ by maker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relative {
    /// An ordinary knob or fader.
    #[default]
    Off,
    /// 1..63 up, 127..65 down.
    TwosComplement,
    /// 1..63 up, 65..127 down.
    SignedBit,
    /// 64 is still, above is up, below is down.
    BinaryOffset,
}

impl Relative {
    /// How many steps (up positive) the value means; `None` for a plain knob.
    pub fn steps(self, value: u8) -> Option<i32> {
        let v = i32::from(value);
        match self {
            Self::Off => None,
            Self::TwosComplement => Some(if v < 64 { v } else { v - 128 }),
            Self::SignedBit => Some(if v < 64 { v } else { -(v - 64) }),
            Self::BinaryOffset => Some(v - 64),
        }
    }
}

/// What happens between the control and the value: range, direction, response curve, and
/// whether the control is an endless encoder.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Glue {
    /// Flip the direction.
    pub invert: bool,
    /// The value at the bottom and the top of the knob's travel (0..=1 of the target's range).
    pub min: f32,
    pub max: f32,
    pub curve: Curve,
    pub relative: Relative,
    /// For an encoder: how much one step moves the value (a fraction of the range).
    pub step: f32,
}

impl Default for Glue {
    fn default() -> Self {
        Self {
            invert: false,
            min: 0.0,
            max: 1.0,
            curve: Curve::Linear,
            relative: Relative::Off,
            step: 0.01,
        }
    }
}

impl Glue {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn is_valid(&self) -> bool {
        (0.0..=1.0).contains(&self.min)
            && (0.0..=1.0).contains(&self.max)
            && self.step.is_finite()
            && self.step > 0.0
            && self.step <= 1.0
    }

    /// A knob position (0..=1) as the value it asks for.
    pub fn shape(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let x = if self.invert { 1.0 - x } else { x };
        let x = match self.curve {
            Curve::Linear => x,
            Curve::Soft => x * x,
            Curve::Hard => x.sqrt(),
        };
        self.min + (self.max - self.min) * x
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    pub source: Source,
    pub action: ControlAction,
    #[serde(default)]
    pub takeover: Takeover,
    #[serde(default, skip_serializing_if = "Glue::is_default")]
    pub glue: Glue,
}

impl Source {
    /// "Launchpad · ch 1 · CC 20", slots and channels counted from 1 like people do.
    pub fn describe(&self) -> String {
        let what = match self.kind {
            SourceKind::Cc { number } => format!("CC {number}"),
            SourceKind::Note { key } => format!("note {key}"),
            SourceKind::Program => "Program Change".to_owned(),
        };
        let device = self.device.as_deref().unwrap_or("any device");
        match self.channel {
            Some(c) => format!("{device} · ch {} · {what}", c as usize + 1),
            None => format!("{device} · {what}"),
        }
    }
}

impl Mapping {
    /// Is it something the engine and the rig can use?
    pub fn is_valid(&self) -> bool {
        self.glue.is_valid()
            && self.action.slot().is_none_or(|s| (s as usize) < MAX_SLOTS)
            && self.source.channel.is_none_or(|c| c < 16)
            && match self.source.kind {
                SourceKind::Cc { number } => number < 128,
                SourceKind::Note { key } => key < 128,
                SourceKind::Program => true,
            }
            && match self.action {
                ControlAction::SelectByValue { base } => (base as usize) < MAX_SLOTS,
                ControlAction::SlotMacro { index, .. } | ControlAction::SelectedMacro { index } => {
                    (index as usize) < MACROS_PER_SLOT
                }
                ControlAction::RecallSnapshot { snapshot: n }
                | ControlAction::RecallSnapshotByValue { base: n } => (n as usize) < MAX_SNAPSHOTS,
                _ => true,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(n: u8) -> Source {
        Source {
            device: Some("Launchpad".into()),
            channel: Some(0),
            kind: SourceKind::Cc { number: n },
        }
    }

    #[test]
    fn mappings_round_trip_through_json_in_a_readable_shape() {
        let m = Mapping {
            source: cc(20),
            action: ControlAction::SelectSlot { slot: 2 },
            takeover: Takeover::Jump,
            glue: Glue::default(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(
            json.contains("\"type\":\"cc\"") && json.contains("\"type\":\"select_slot\""),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Mapping>(&json).unwrap(), m);
        // A mapping with only what matters: any device, any channel, default takeover.
        let sparse: Mapping = serde_json::from_str(
            r#"{"source":{"kind":{"type":"program"}},"action":{"type":"next_slot"}}"#,
        )
        .unwrap();
        assert_eq!(sparse.source.device, None);
        assert_eq!(sparse.takeover, Takeover::Jump);
    }

    #[test]
    fn descriptions_count_from_one() {
        assert_eq!(cc(20).describe(), "Launchpad · ch 1 · CC 20");
        assert_eq!(
            Source {
                device: None,
                channel: None,
                kind: SourceKind::Program
            }
            .describe(),
            "any device · Program Change"
        );
        assert_eq!(
            ControlAction::SelectSlot { slot: 0 }.describe(),
            "Select slot 1"
        );
        assert!(ControlAction::MasterVolume.is_continuous());
        assert!(!ControlAction::Panic.is_continuous());
    }

    #[test]
    fn invalid_mappings_are_caught() {
        let ok = Mapping {
            source: cc(1),
            action: ControlAction::Panic,
            takeover: Takeover::Jump,
            glue: Glue::default(),
        };
        assert!(ok.is_valid());
        let mut bad = ok.clone();
        bad.action = ControlAction::SelectSlot {
            slot: MAX_SLOTS as u8,
        };
        assert!(!bad.is_valid());
        let mut bad = ok.clone();
        bad.source.channel = Some(16);
        assert!(!bad.is_valid());
        let mut bad = ok.clone();
        bad.source.kind = SourceKind::Cc { number: 200 };
        assert!(!bad.is_valid());
        let mut bad = ok;
        bad.glue.step = 0.0;
        assert!(!bad.is_valid());
    }

    #[test]
    fn glue_shapes_a_knob_and_reads_encoders() {
        let g = Glue {
            min: 0.2,
            max: 0.8,
            ..Glue::default()
        };
        assert!((g.shape(0.0) - 0.2).abs() < 1e-6 && (g.shape(1.0) - 0.8).abs() < 1e-6);
        let inv = Glue {
            invert: true,
            ..Glue::default()
        };
        assert_eq!(inv.shape(0.0), 1.0);
        let soft = Glue {
            curve: Curve::Soft,
            ..Glue::default()
        };
        assert!(soft.shape(0.5) < 0.5);
        let hard = Glue {
            curve: Curve::Hard,
            ..Glue::default()
        };
        assert!(hard.shape(0.5) > 0.5);
        // The three ways an endless encoder says "one step down".
        assert_eq!(Relative::TwosComplement.steps(127), Some(-1));
        assert_eq!(Relative::SignedBit.steps(65), Some(-1));
        assert_eq!(Relative::BinaryOffset.steps(63), Some(-1));
        assert_eq!(Relative::TwosComplement.steps(3), Some(3));
        assert_eq!(Relative::Off.steps(3), None);
    }

    #[test]
    fn a_mapping_without_glue_keeps_its_file_short() {
        let m = Mapping {
            source: cc(1),
            action: ControlAction::MasterVolume,
            takeover: Takeover::Scale,
            glue: Glue::default(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("glue"), "{json}");
        assert_eq!(serde_json::from_str::<Mapping>(&json).unwrap(), m);
    }
}
