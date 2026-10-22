//! What a controller knob does when the slot has no macro on it (plan §4.10): eight useful
//! sound-shaping controls that need no setup. A macro on the same knob number wins; these are
//! what is left over, so a rig with no macros still has a mixing desk under the fingers.

use crate::Slot;

/// Knob 1 to 8, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultKnob {
    Pan,
    Reverb,
    Delay,
    Bass,
    Mid,
    Treble,
    DelayTime,
    /// A SoundFont's filter; plugins have no such control.
    Brightness,
}

/// Most reverb and delay a knob turned fully up adds (the effects' own mix runs to 1.0, which is
/// all wet and never wanted).
const MIX_MAX: f32 = 0.6;
/// The EQ knobs run this many dB either way from the knob's middle.
const EQ_RANGE: f32 = 12.0;
const DELAY_MIN_MS: f32 = 50.0;
const DELAY_MAX_MS: f32 = 1000.0;
/// A SoundFont's brightness knob: octaves of filter cutoff either way.
pub const BRIGHTNESS_OCTAVES: f32 = 3.0;
/// Below this knob position the reverb or delay is switched off again.
const OFF_BELOW: f32 = 0.02;
/// An EQ band this close to flat (dB) counts as off.
const FLAT_DB: f32 = 0.5;

impl DefaultKnob {
    pub const ALL: [DefaultKnob; 8] = [
        Self::Pan,
        Self::Reverb,
        Self::Delay,
        Self::Bass,
        Self::Mid,
        Self::Treble,
        Self::DelayTime,
        Self::Brightness,
    ];

    pub fn for_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Pan => "Pan",
            Self::Reverb => "Reverb",
            Self::Delay => "Delay",
            Self::Bass => "Bass",
            Self::Mid => "Mid",
            Self::Treble => "Treble",
            Self::DelayTime => "Delay time",
            Self::Brightness => "Brightness",
        }
    }

    /// Where the knob sits for this slot, 0..=1. `None` for [`DefaultKnob::Brightness`], which
    /// the session reads from the sampler.
    pub fn read(self, slot: &Slot) -> Option<f32> {
        let fx = &slot.fx;
        Some(match self {
            Self::Pan => (slot.pan + 1.0) / 2.0,
            Self::Reverb => {
                if fx.reverb.enabled {
                    fx.reverb.mix / MIX_MAX
                } else {
                    0.0
                }
            }
            Self::Delay => {
                if fx.delay.enabled {
                    fx.delay.mix / MIX_MAX
                } else {
                    0.0
                }
            }
            Self::Bass => (fx.eq.low_db / EQ_RANGE + 1.0) / 2.0,
            Self::Mid => (fx.eq.mid_db / EQ_RANGE + 1.0) / 2.0,
            Self::Treble => (fx.eq.high_db / EQ_RANGE + 1.0) / 2.0,
            Self::DelayTime => (fx.delay.time_ms - DELAY_MIN_MS) / (DELAY_MAX_MS - DELAY_MIN_MS),
            Self::Brightness => return None,
        })
        .map(|v| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        })
    }

    /// Turns the knob to `v` (0..=1): changes the slot's pan or effects, switching an effect on
    /// when the knob leaves its bottom and off when it comes back. Returns false for
    /// [`DefaultKnob::Brightness`], which is not in the slot's own settings.
    pub fn write(self, slot: &mut Slot, v: f32) -> bool {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let fx = &mut slot.fx;
        let db = (v * 2.0 - 1.0) * EQ_RANGE;
        match self {
            Self::Pan => slot.pan = v * 2.0 - 1.0,
            Self::Reverb => {
                fx.reverb.mix = v * MIX_MAX;
                fx.reverb.enabled = v >= OFF_BELOW;
            }
            Self::Delay => {
                fx.delay.mix = v * MIX_MAX;
                fx.delay.enabled = v >= OFF_BELOW;
            }
            Self::Bass => fx.eq.low_db = db,
            Self::Mid => fx.eq.mid_db = db,
            Self::Treble => fx.eq.high_db = db,
            Self::DelayTime => fx.delay.time_ms = DELAY_MIN_MS + v * (DELAY_MAX_MS - DELAY_MIN_MS),
            Self::Brightness => return false,
        }
        if matches!(self, Self::Bass | Self::Mid | Self::Treble) {
            fx.eq.enabled = [fx.eq.low_db, fx.eq.mid_db, fx.eq.high_db]
                .iter()
                .any(|d| d.abs() >= FLAT_DB);
        }
        true
    }

    /// The knob's position in words for a small screen: `C`, `L40`, `+3dB`, `35%`.
    pub fn display(self, v: f32) -> String {
        let v = v.clamp(0.0, 1.0);
        match self {
            Self::Pan => {
                let p = ((v * 2.0 - 1.0) * 100.0).round() as i32;
                match p {
                    0 => "C".to_owned(),
                    p if p < 0 => format!("L{}", -p),
                    p => format!("R{p}"),
                }
            }
            Self::Bass | Self::Mid | Self::Treble => {
                format!("{:+.0}dB", (v * 2.0 - 1.0) * EQ_RANGE)
            }
            Self::DelayTime => format!("{:.0}ms", DELAY_MIN_MS + v * (DELAY_MAX_MS - DELAY_MIN_MS)),
            Self::Brightness => format!("{:+.1}oct", (v * 2.0 - 1.0) * BRIGHTNESS_OCTAVES),
            Self::Reverb | Self::Delay => format!("{:.0}%", v * 100.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_slot_has_every_knob_at_rest_and_turning_one_switches_the_effect_on() {
        let mut s = Slot::default();
        assert_eq!(DefaultKnob::Pan.read(&s), Some(0.5));
        for k in [DefaultKnob::Reverb, DefaultKnob::Delay] {
            assert_eq!(k.read(&s), Some(0.0));
        }
        for k in [DefaultKnob::Bass, DefaultKnob::Mid, DefaultKnob::Treble] {
            assert_eq!(k.read(&s), Some(0.5), "{k:?}");
        }
        assert!(DefaultKnob::Reverb.write(&mut s, 0.5));
        assert!(s.fx.reverb.enabled);
        assert!((s.fx.reverb.mix - 0.3).abs() < 1e-6);
        assert!((DefaultKnob::Reverb.read(&s).unwrap() - 0.5).abs() < 1e-6);
        // Back to the bottom: off again.
        DefaultKnob::Reverb.write(&mut s, 0.0);
        assert!(!s.fx.reverb.enabled);
    }

    #[test]
    fn the_eq_knobs_are_flat_in_the_middle_and_the_eq_follows_them() {
        let mut s = Slot::default();
        DefaultKnob::Bass.write(&mut s, 1.0);
        assert_eq!(s.fx.eq.low_db, 12.0);
        assert!(s.fx.eq.enabled);
        DefaultKnob::Treble.write(&mut s, 0.0);
        assert_eq!(s.fx.eq.high_db, -12.0);
        DefaultKnob::Bass.write(&mut s, 0.5);
        assert!(s.fx.eq.enabled, "the treble is still cut");
        DefaultKnob::Treble.write(&mut s, 0.5);
        assert!(!s.fx.eq.enabled, "all flat: the EQ is out of the way");
    }

    #[test]
    fn pan_and_the_screen_text() {
        let mut s = Slot::default();
        DefaultKnob::Pan.write(&mut s, 0.0);
        assert_eq!(s.pan, -1.0);
        assert_eq!(DefaultKnob::Pan.display(0.0), "L100");
        assert_eq!(DefaultKnob::Pan.display(0.5), "C");
        assert_eq!(DefaultKnob::Pan.display(0.75), "R50");
        assert_eq!(DefaultKnob::Bass.display(1.0), "+12dB");
        assert_eq!(DefaultKnob::Reverb.display(0.355), "36%");
        // Brightness lives in the sampler, not in the slot.
        assert_eq!(DefaultKnob::Brightness.read(&s), None);
        assert!(!DefaultKnob::Brightness.write(&mut s, 0.9));
        // Values the effects accept after the UI's own clamp.
        DefaultKnob::DelayTime.write(&mut s, 1.0);
        let mut fx = s.fx;
        fx.clamp();
        assert_eq!(fx.delay.time_ms, 1000.0);
        assert_eq!(DefaultKnob::for_index(7), Some(DefaultKnob::Brightness));
        assert_eq!(DefaultKnob::for_index(8), None);
    }
}
