//! Settings of the built-in effects. Plain data with defaults; the DSP lives in `dsp`.
//!
//! Every effect starts disabled with neutral values, so a slot sounds exactly like its
//! instrument until the player turns something on. Ranges are enforced by [`FxSettings::clamp`]
//! (called on load and by the UI), not by the types, so files from other versions still load.

use serde::{Deserialize, Serialize};

fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { lo }
}

/// Three bands: low shelf, one peaking band, high shelf.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqSettings {
    pub enabled: bool,
    pub low_db: f32,
    pub low_hz: f32,
    pub mid_db: f32,
    pub mid_hz: f32,
    pub mid_q: f32,
    pub high_db: f32,
    pub high_hz: f32,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            low_db: 0.0,
            low_hz: 120.0,
            mid_db: 0.0,
            mid_hz: 1000.0,
            mid_q: 0.8,
            high_db: 0.0,
            high_hz: 6000.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompSettings {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,
}

impl Default for CompSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -18.0,
            ratio: 3.0,
            attack_ms: 10.0,
            release_ms: 120.0,
            knee_db: 6.0,
            makeup_db: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DelaySettings {
    pub enabled: bool,
    pub time_ms: f32,
    pub feedback: f32,
    /// How much of the echoes is added to the dry signal.
    pub mix: f32,
    pub ping_pong: bool,
    /// Echoes get darker: low-pass cutoff in the feedback path.
    pub damping_hz: f32,
}

impl Default for DelaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            time_ms: 350.0,
            feedback: 0.35,
            mix: 0.25,
            ping_pong: false,
            damping_hz: 5000.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReverbSettings {
    pub enabled: bool,
    /// 0..1, small room to large hall.
    pub room: f32,
    /// 0..1, how quickly highs die away.
    pub damping: f32,
    /// 0..1, stereo spread.
    pub width: f32,
    /// How much reverb is added to the dry signal.
    pub mix: f32,
}

impl Default for ReverbSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            room: 0.5,
            damping: 0.5,
            width: 1.0,
            mix: 0.2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FxSettings {
    pub eq: EqSettings,
    pub comp: CompSettings,
    pub delay: DelaySettings,
    pub reverb: ReverbSettings,
}

impl FxSettings {
    /// Forces every value into its allowed range (NaN becomes the lower bound).
    pub fn clamp(&mut self) {
        let e = &mut self.eq;
        e.low_db = clamp(e.low_db, -18.0, 18.0);
        e.low_hz = clamp(e.low_hz, 20.0, 800.0);
        e.mid_db = clamp(e.mid_db, -18.0, 18.0);
        e.mid_hz = clamp(e.mid_hz, 100.0, 10000.0);
        e.mid_q = clamp(e.mid_q, 0.2, 8.0);
        e.high_db = clamp(e.high_db, -18.0, 18.0);
        e.high_hz = clamp(e.high_hz, 1500.0, 16000.0);
        let c = &mut self.comp;
        c.threshold_db = clamp(c.threshold_db, -60.0, 0.0);
        c.ratio = clamp(c.ratio, 1.0, 20.0);
        c.attack_ms = clamp(c.attack_ms, 0.1, 200.0);
        c.release_ms = clamp(c.release_ms, 5.0, 2000.0);
        c.knee_db = clamp(c.knee_db, 0.0, 24.0);
        c.makeup_db = clamp(c.makeup_db, 0.0, 24.0);
        let d = &mut self.delay;
        d.time_ms = clamp(d.time_ms, 1.0, 1500.0);
        d.feedback = clamp(d.feedback, 0.0, 0.95);
        d.mix = clamp(d.mix, 0.0, 1.0);
        d.damping_hz = clamp(d.damping_hz, 500.0, 18000.0);
        let r = &mut self.reverb;
        r.room = clamp(r.room, 0.0, 1.0);
        r.damping = clamp(r.damping, 0.0, 1.0);
        r.width = clamp(r.width, 0.0, 1.0);
        r.mix = clamp(r.mix, 0.0, 1.0);
    }

    /// Is any effect turned on?
    pub fn any_enabled(&self) -> bool {
        self.eq.enabled || self.comp.enabled || self.delay.enabled || self.reverb.enabled
    }
}

/// One knob of a built-in effect: what it is called, how far it turns, and how a position on the
/// knob maps to a value. The screen draws its knobs from these and a MIDI mapping moves them by
/// `id`, so the two cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxKnob {
    /// Stable: saved in mappings.
    pub id: u32,
    /// The effect it belongs to.
    pub group: &'static str,
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    /// Turns evenly in octaves (frequencies, times) rather than in steps of value.
    pub log: bool,
    pub unit: &'static str,
    pub decimals: usize,
}

impl FxKnob {
    /// Where `value` sits on the knob, 0..=1.
    pub fn position(&self, value: f32) -> f32 {
        let v = clamp(value, self.min, self.max);
        let p = if self.log {
            (v / self.min).ln() / (self.max / self.min).ln()
        } else {
            (v - self.min) / (self.max - self.min)
        };
        clamp(p, 0.0, 1.0)
    }

    /// The value at position `p` (0..=1) of the knob.
    pub fn value(&self, p: f32) -> f32 {
        let p = clamp(p, 0.0, 1.0);
        if self.log {
            self.min * (self.max / self.min).powf(p)
        } else {
            self.min + (self.max - self.min) * p
        }
    }

    /// `-3.5 dB`, `440 Hz`.
    pub fn show(&self, value: f32) -> String {
        format!("{value:.*}{}", self.decimals, self.unit)
    }
}

const fn knob(
    id: u32,
    group: &'static str,
    name: &'static str,
    (min, max): (f32, f32),
    log: bool,
    unit: &'static str,
    decimals: usize,
) -> FxKnob {
    FxKnob {
        id,
        group,
        name,
        min,
        max,
        log,
        unit,
        decimals,
    }
}

pub const EQ_KNOBS: [FxKnob; 7] = [
    knob(1, "EQ", "low", (-18.0, 18.0), false, " dB", 1),
    knob(2, "EQ", "low freq", (20.0, 800.0), true, " Hz", 0),
    knob(3, "EQ", "mid", (-18.0, 18.0), false, " dB", 1),
    knob(4, "EQ", "mid freq", (100.0, 10000.0), true, " Hz", 0),
    knob(5, "EQ", "mid Q", (0.2, 8.0), true, "", 2),
    knob(6, "EQ", "high", (-18.0, 18.0), false, " dB", 1),
    knob(7, "EQ", "high freq", (1500.0, 16000.0), true, " Hz", 0),
];

pub const COMP_KNOBS: [FxKnob; 6] = [
    knob(11, "Compressor", "threshold", (-60.0, 0.0), false, " dB", 1),
    knob(12, "Compressor", "ratio", (1.0, 20.0), true, ":1", 1),
    knob(13, "Compressor", "attack", (0.1, 200.0), true, " ms", 1),
    knob(14, "Compressor", "release", (5.0, 2000.0), true, " ms", 0),
    knob(15, "Compressor", "knee", (0.0, 24.0), false, " dB", 1),
    knob(16, "Compressor", "makeup", (0.0, 24.0), false, " dB", 1),
];

pub const DELAY_KNOBS: [FxKnob; 4] = [
    knob(21, "Delay", "time", (1.0, 1500.0), true, " ms", 0),
    knob(22, "Delay", "feedback", (0.0, 0.95), false, "", 2),
    knob(23, "Delay", "mix", (0.0, 1.0), false, "", 2),
    knob(24, "Delay", "damping", (500.0, 18000.0), true, " Hz", 0),
];

pub const REVERB_KNOBS: [FxKnob; 4] = [
    knob(31, "Reverb", "room", (0.0, 1.0), false, "", 2),
    knob(32, "Reverb", "damping", (0.0, 1.0), false, "", 2),
    knob(33, "Reverb", "width", (0.0, 1.0), false, "", 2),
    knob(34, "Reverb", "mix", (0.0, 1.0), false, "", 2),
];

/// The knob with this id, from any effect.
pub fn fx_knob(id: u32) -> Option<&'static FxKnob> {
    EQ_KNOBS
        .iter()
        .chain(&COMP_KNOBS)
        .chain(&DELAY_KNOBS)
        .chain(&REVERB_KNOBS)
        .find(|k| k.id == id)
}

impl FxSettings {
    fn field(&mut self, id: u32) -> Option<&mut f32> {
        Some(match id {
            1 => &mut self.eq.low_db,
            2 => &mut self.eq.low_hz,
            3 => &mut self.eq.mid_db,
            4 => &mut self.eq.mid_hz,
            5 => &mut self.eq.mid_q,
            6 => &mut self.eq.high_db,
            7 => &mut self.eq.high_hz,
            11 => &mut self.comp.threshold_db,
            12 => &mut self.comp.ratio,
            13 => &mut self.comp.attack_ms,
            14 => &mut self.comp.release_ms,
            15 => &mut self.comp.knee_db,
            16 => &mut self.comp.makeup_db,
            21 => &mut self.delay.time_ms,
            22 => &mut self.delay.feedback,
            23 => &mut self.delay.mix,
            24 => &mut self.delay.damping_hz,
            31 => &mut self.reverb.room,
            32 => &mut self.reverb.damping,
            33 => &mut self.reverb.width,
            34 => &mut self.reverb.mix,
            _ => return None,
        })
    }

    /// The value of knob `id`.
    pub fn knob_value(&self, id: u32) -> Option<f32> {
        let mut copy = *self;
        copy.field(id).map(|f| *f)
    }

    /// Turns knob `id` to the position `p` (0..=1). False for an id that is no knob.
    pub fn set_knob_position(&mut self, id: u32, p: f32) -> bool {
        let Some(k) = fx_knob(id) else {
            return false;
        };
        let value = k.value(p);
        match self.field(id) {
            Some(f) => {
                *f = value;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_neutral_and_old_files_load() {
        let fx = FxSettings::default();
        assert!(!fx.any_enabled());
        let from_empty: FxSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(from_empty, fx);
        // A file that only knows some fields keeps the defaults of the rest.
        let partial: FxSettings =
            serde_json::from_str(r#"{"delay":{"enabled":true,"time_ms":500}}"#).unwrap();
        assert!(partial.delay.enabled && partial.delay.time_ms == 500.0);
        assert_eq!(partial.delay.feedback, DelaySettings::default().feedback);
        assert_eq!(partial.reverb, ReverbSettings::default());
    }

    #[test]
    fn every_knob_reads_and_writes_and_stays_in_its_range() {
        let all = EQ_KNOBS
            .iter()
            .chain(&COMP_KNOBS)
            .chain(&DELAY_KNOBS)
            .chain(&REVERB_KNOBS);
        let mut ids = Vec::new();
        for k in all {
            ids.push(k.id);
            let mut fx = FxSettings::default();
            for p in [0.0, 0.25, 0.5, 1.0] {
                assert!(fx.set_knob_position(k.id, p), "{}", k.name);
                let v = fx.knob_value(k.id).unwrap();
                assert!(v >= k.min - 1e-3 && v <= k.max + 1e-3, "{} {v}", k.name);
                assert!((k.position(v) - p).abs() < 1e-3, "{} at {p}", k.name);
            }
            // Where the settings start is a point on the knob.
            let d = FxSettings::default().knob_value(k.id).unwrap();
            assert!((k.min..=k.max).contains(&d), "{} default {d}", k.name);
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 21, "ids are unique");
        assert!(FxSettings::default().knob_value(99).is_none());
        assert!(!FxSettings::default().set_knob_position(99, 0.5));
    }

    #[test]
    fn frequency_knobs_turn_in_octaves() {
        let k = fx_knob(2).unwrap(); // 20..800 Hz
        // Halfway round is the geometric middle, not the arithmetic one.
        assert!((k.value(0.5) - 126.5).abs() < 0.5, "{}", k.value(0.5));
        assert_eq!(k.show(120.0), "120 Hz");
    }

    #[test]
    fn clamp_forces_values_into_range() {
        let mut fx = FxSettings::default();
        fx.eq.low_db = 99.0;
        fx.comp.ratio = f32::NAN;
        fx.delay.feedback = 5.0;
        fx.delay.time_ms = -3.0;
        fx.reverb.mix = f32::INFINITY;
        fx.clamp();
        assert_eq!(fx.eq.low_db, 18.0);
        assert_eq!(fx.comp.ratio, 1.0);
        assert_eq!(fx.delay.feedback, 0.95);
        assert_eq!(fx.delay.time_ms, 1.0);
        assert_eq!(fx.reverb.mix, 0.0);
    }
}
