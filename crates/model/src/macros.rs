//! Slot macros (plan §4.10): up to eight named knobs per slot, each driving any number of
//! parameters (of the instrument, its inserts, or a SoundFont's sound shaping) over a range.
//! A macro is what a controller knob or a snapshot moves, so a player can reach "filter and
//! reverb and brightness" with one control.

use serde::{Deserialize, Serialize};

/// Macros per slot.
pub const MACROS_PER_SLOT: usize = 8;
/// Most parameters one macro can drive.
pub const MAX_MACRO_TARGETS: usize = 16;

/// What a SoundFont slot lets a macro change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplerField {
    Level,
    Attack,
    Release,
    Brightness,
    Resonance,
    Tuning,
    /// How many notes can sound at once (a whole number).
    Voices,
}

impl SamplerField {
    pub const ALL: [SamplerField; 7] = [
        Self::Level,
        Self::Attack,
        Self::Release,
        Self::Brightness,
        Self::Resonance,
        Self::Tuning,
        Self::Voices,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Level => "Level",
            Self::Attack => "Attack",
            Self::Release => "Release",
            Self::Brightness => "Brightness",
            Self::Resonance => "Resonance",
            Self::Tuning => "Tuning",
            Self::Voices => "Voices",
        }
    }

    /// The value at the bottom and top of the field's range, in the units the sound panel uses.
    pub fn range(self) -> (f32, f32) {
        match self {
            Self::Level => (0.0, 2.0),
            Self::Attack => (-3.0, 8.0),
            Self::Release => (-3.0, 4.0),
            Self::Brightness => (-3.0, 3.0),
            Self::Resonance => (0.0, 24.0),
            Self::Tuning => (-100.0, 100.0),
            Self::Voices => (4.0, 128.0),
        }
    }
}

/// One thing a macro moves. `from` and `to` are fractions (0..=1) of the target's range that the
/// macro's 0 and 1 map to; `from > to` runs the target backwards.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MacroTarget {
    /// A plugin parameter: of the slot's instrument, or of one of its inserts.
    Param {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        insert: Option<u8>,
        id: u32,
        name: String,
        /// The parameter's own range, kept here so a macro can run without asking the plugin.
        min: f64,
        max: f64,
        #[serde(default)]
        stepped: bool,
        from: f32,
        to: f32,
    },
    /// A SoundFont slot's sound shaping.
    Sampler {
        field: SamplerField,
        from: f32,
        to: f32,
    },
}

fn mix(from: f32, to: f32, v: f32) -> f32 {
    let v = if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    };
    from + (to - from) * v
}

impl MacroTarget {
    pub fn range_fractions(&self) -> (f32, f32) {
        match *self {
            Self::Param { from, to, .. } | Self::Sampler { from, to, .. } => (from, to),
        }
    }

    pub fn set_range_fractions(&mut self, lo: f32, hi: f32) {
        match self {
            Self::Param { from, to, .. } | Self::Sampler { from, to, .. } => {
                (*from, *to) = (lo.clamp(0.0, 1.0), hi.clamp(0.0, 1.0));
            }
        }
    }

    /// A parameter's value when the macro is at `v` (0..=1).
    pub fn param_value(&self, v: f32) -> Option<f64> {
        match *self {
            Self::Param {
                min,
                max,
                stepped,
                from,
                to,
                ..
            } => {
                let value = min + (max - min) * f64::from(mix(from, to, v));
                Some(if stepped { value.round() } else { value })
            }
            Self::Sampler { .. } => None,
        }
    }

    /// A sampler field's value when the macro is at `v`.
    pub fn sampler_value(&self, v: f32) -> Option<(SamplerField, f32)> {
        match *self {
            Self::Sampler { field, from, to } => {
                let (lo, hi) = field.range();
                Some((field, lo + (hi - lo) * mix(from, to, v)))
            }
            Self::Param { .. } => None,
        }
    }

    fn is_valid(&self) -> bool {
        let (from, to) = self.range_fractions();
        let fractions = (0.0..=1.0).contains(&from) && (0.0..=1.0).contains(&to);
        fractions
            && match self {
                Self::Param {
                    insert, min, max, ..
                } => {
                    insert.is_none_or(|i| (i as usize) < crate::MAX_INSERTS)
                        && min.is_finite()
                        && max.is_finite()
                }
                Self::Sampler { .. } => true,
            }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Macro {
    /// Empty shows as "Macro N".
    pub name: String,
    /// Where the knob sits, 0..=1.
    pub value: f32,
    pub targets: Vec<MacroTarget>,
}

impl Default for Macro {
    fn default() -> Self {
        Self {
            name: String::new(),
            value: 0.0,
            targets: Vec::new(),
        }
    }
}

impl Macro {
    /// The name to show for macro number `index` (from 0).
    pub fn label(&self, index: usize) -> String {
        if self.name.trim().is_empty() {
            format!("Macro {}", index + 1)
        } else {
            self.name.clone()
        }
    }

    pub fn is_valid(&self) -> bool {
        self.value.is_finite()
            && (0.0..=1.0).contains(&self.value)
            && self.targets.len() <= MAX_MACRO_TARGETS
            && self.targets.iter().all(MacroTarget::is_valid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(from: f32, to: f32, stepped: bool) -> MacroTarget {
        MacroTarget::Param {
            insert: None,
            id: 7,
            name: "Cutoff".into(),
            min: 20.0,
            max: 120.0,
            stepped,
            from,
            to,
        }
    }

    #[test]
    fn a_macro_maps_over_a_range_and_can_run_backwards() {
        let t = param(0.25, 0.75, false);
        assert_eq!(t.param_value(0.0), Some(45.0));
        assert_eq!(t.param_value(1.0), Some(95.0));
        assert_eq!(t.param_value(0.5), Some(70.0));
        let back = param(1.0, 0.0, false);
        assert_eq!(back.param_value(0.0), Some(120.0));
        assert_eq!(back.param_value(1.0), Some(20.0));
        // Out-of-range and broken values stay inside.
        assert_eq!(t.param_value(7.0), Some(95.0));
        assert_eq!(t.param_value(f32::NAN), Some(45.0));
        assert_eq!(param(0.0, 1.0, true).param_value(0.333), Some(53.0));
    }

    #[test]
    fn sampler_fields_use_their_own_units() {
        let t = MacroTarget::Sampler {
            field: SamplerField::Tuning,
            from: 0.0,
            to: 1.0,
        };
        assert_eq!(t.sampler_value(0.5), Some((SamplerField::Tuning, 0.0)));
        assert_eq!(t.sampler_value(1.0), Some((SamplerField::Tuning, 100.0)));
    }

    #[test]
    fn macros_round_trip_and_sparse_files_load() {
        let m = Macro {
            name: "Brighter".into(),
            value: 0.4,
            targets: vec![
                param(0.0, 1.0, false),
                MacroTarget::Sampler {
                    field: SamplerField::Brightness,
                    from: 0.5,
                    to: 1.0,
                },
            ],
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"param\"") && json.contains("\"type\":\"sampler\""));
        assert_eq!(serde_json::from_str::<Macro>(&json).unwrap(), m);
        let sparse: Macro = serde_json::from_str("{}").unwrap();
        assert_eq!(sparse.label(2), "Macro 3");
        assert!(sparse.is_valid());
        assert!(
            !Macro {
                value: 2.0,
                ..Macro::default()
            }
            .is_valid()
        );
    }
}
