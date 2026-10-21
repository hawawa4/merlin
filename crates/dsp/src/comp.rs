//! A feed-forward compressor with a soft knee. The detector looks at the louder of the two
//! channels, so the stereo image does not shift. Gain reduction is smoothed in dB.

use merlin_model::CompSettings;

use crate::util::{db_to_lin, lin_to_db, time_coef};

pub struct Compressor {
    s: CompSettings,
    sample_rate: f32,
    attack: f32,
    release: f32,
    makeup: f32,
    /// Current gain reduction in dB (>= 0).
    reduction_db: f32,
}

impl Compressor {
    pub fn new(sample_rate: f32) -> Self {
        let mut c = Self {
            s: CompSettings::default(),
            sample_rate,
            attack: 0.0,
            release: 0.0,
            makeup: 1.0,
            reduction_db: 0.0,
        };
        c.set(&CompSettings::default());
        c
    }

    pub fn set(&mut self, s: &CompSettings) {
        self.s = *s;
        self.attack = time_coef(s.attack_ms / 1000.0, self.sample_rate);
        self.release = time_coef(s.release_ms / 1000.0, self.sample_rate);
        self.makeup = db_to_lin(s.makeup_db);
    }

    /// Gain reduction at the end of the last block, in dB, for meters.
    pub fn reduction_db(&self) -> f32 {
        self.reduction_db
    }

    /// Static curve: how many dB the signal at `level_db` is turned down.
    pub fn reduction_for(&self, level_db: f32) -> f32 {
        let (t, ratio, knee) = (self.s.threshold_db, self.s.ratio, self.s.knee_db);
        let over = level_db - t;
        let slope = 1.0 - 1.0 / ratio;
        if knee > 0.0 && over > -knee / 2.0 && over < knee / 2.0 {
            // Quadratic interpolation through the knee.
            slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.s.enabled {
            self.reduction_db = 0.0;
            return;
        }
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let level = lin_to_db(x.abs().max(y.abs()));
            let target = self.reduction_for(level);
            // Attack when more reduction is needed, release when less.
            let coef = if target > self.reduction_db {
                self.attack
            } else {
                self.release
            };
            self.reduction_db = target + (self.reduction_db - target) * coef;
            let gain = db_to_lin(-self.reduction_db) * self.makeup;
            *x *= gain;
            *y *= gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::rms;

    const SR: f32 = 48000.0;

    fn comp(f: impl FnOnce(&mut CompSettings)) -> Compressor {
        let mut s = CompSettings {
            enabled: true,
            knee_db: 0.0,
            ..CompSettings::default()
        };
        f(&mut s);
        let mut c = Compressor::new(SR);
        c.set(&s);
        c
    }

    fn sine(db: f32, n: usize) -> Vec<f32> {
        let a = db_to_lin(db);
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 220.0 * i as f32 / SR).sin() * a)
            .collect()
    }

    /// Steady-state output level in dB (peak) for a sine of `db`.
    fn out_db(c: &mut Compressor, db: f32) -> f32 {
        let mut l = sine(db, 48000);
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        lin_to_db(l[36000..].iter().fold(0f32, |m, x| m.max(x.abs())))
    }

    #[test]
    fn the_static_curve_follows_threshold_and_ratio() {
        let mut c = comp(|s| {
            s.threshold_db = -20.0;
            s.ratio = 4.0;
        });
        assert!(
            (c.reduction_for(-30.0)).abs() < 1e-6,
            "below threshold: untouched"
        );
        assert!(
            (c.reduction_for(-8.0) - 9.0).abs() < 1e-4,
            "12 dB over at 4:1 leaves 3 dB: 9 dB reduction"
        );
        // Measured on a steady level (the detector follows the signal sample by sample, so a
        // sine is reduced a little less at its peaks): -2 dBFS, 18 dB over, 4:1 -> 4.5 dB over.
        let mut l = vec![db_to_lin(-2.0); 24000];
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        let measured = lin_to_db(l[23999]);
        assert!(
            (measured - (-20.0 + 4.5)).abs() < 0.05,
            "measured {measured}"
        );
        // A sine ends up between that and the uncompressed level, never louder than the input.
        let sine_out = out_db(&mut c, -2.0);
        assert!(
            sine_out < -2.0 && sine_out > -20.0 + 4.5 - 0.5,
            "{sine_out}"
        );
    }

    #[test]
    fn soft_knee_is_continuous_and_gentler() {
        let hard = comp(|s| s.threshold_db = -20.0);
        let soft = comp(|s| {
            s.threshold_db = -20.0;
            s.knee_db = 12.0;
        });
        assert!(
            soft.reduction_for(-20.0) > 0.0,
            "the knee starts working below the threshold"
        );
        assert!(soft.reduction_for(-20.0) < 1.0);
        assert!(soft.reduction_for(-3.0) < hard.reduction_for(-3.0) + 1e-3);
        // Continuity across the knee edges.
        let edge_in = soft.reduction_for(-26.01);
        let edge_out = soft.reduction_for(-25.99);
        assert!((edge_in - edge_out).abs() < 0.01);
    }

    #[test]
    fn attack_and_release_times_are_respected() {
        let mut c = comp(|s| {
            s.threshold_db = -30.0;
            s.ratio = 10.0;
            s.attack_ms = 20.0;
            s.release_ms = 200.0;
        });
        // A sudden loud burst: after one attack time, about 63 % of the final reduction.
        let mut l = vec![0.5; 4800];
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        let final_red = c.reduction_for(lin_to_db(0.5));
        let at_20ms = lin_to_db(1.0 / l[960 - 1] * 0.5).max(0.0);
        assert!(
            at_20ms > final_red * 0.5 && at_20ms < final_red * 0.75,
            "{at_20ms} vs {final_red}"
        );
        // Then silence: reduction releases slowly (still most of it after 20 ms).
        let mut l = vec![0.0; 960];
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        assert!(
            c.reduction_db() > final_red * 0.6,
            "release is slow: {}",
            c.reduction_db()
        );
    }

    #[test]
    fn makeup_gain_and_bypass() {
        let mut c = comp(|s| {
            s.threshold_db = 0.0;
            s.makeup_db = 6.0;
        });
        let mut l = sine(-30.0, 4800);
        let before = rms(&l);
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        assert!(
            (rms(&l) / before - db_to_lin(6.0)).abs() < 0.02,
            "makeup applies when nothing is compressed"
        );

        let mut off = Compressor::new(SR);
        let mut l = sine(-3.0, 4800);
        let copy = l.clone();
        let mut r = l.clone();
        off.process(&mut l, &mut r);
        assert_eq!(l, copy, "disabled is bit-exact");
    }

    #[test]
    fn the_stereo_image_does_not_shift() {
        let mut c = comp(|s| {
            s.threshold_db = -30.0;
            s.ratio = 8.0;
        });
        let mut l = sine(-6.0, 9600);
        let mut r: Vec<f32> = l.iter().map(|x| x * 0.25).collect();
        c.process(&mut l, &mut r);
        let ratio = rms(&l[4800..]) / rms(&r[4800..]);
        assert!(
            (ratio - 4.0).abs() < 0.05,
            "channel balance is kept: {ratio}"
        );
    }
}
