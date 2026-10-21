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
