//! Three-band EQ: low shelf, peaking mid, high shelf. Stereo, one filter set per channel.

use merlin_model::EqSettings;

use crate::biquad::{Biquad, Coeffs};

#[derive(Default)]
pub struct Eq {
    settings: EqSettings,
    sample_rate: f32,
    // [band][channel]
    bands: [[Biquad; 2]; 3],
    active: [bool; 3],
}

impl Eq {
    pub fn new(sample_rate: f32) -> Self {
        let mut eq = Self {
            sample_rate,
            ..Self::default()
        };
        eq.set(&EqSettings::default());
        eq
    }

    pub fn set(&mut self, s: &EqSettings) {
        self.settings = *s;
        let sr = self.sample_rate;
        let coeffs = [
            Coeffs::low_shelf(sr, s.low_hz, s.low_db),
            Coeffs::peaking(sr, s.mid_hz, s.mid_q, s.mid_db),
            Coeffs::high_shelf(sr, s.high_hz, s.high_db),
        ];
        let gains = [s.low_db, s.mid_db, s.high_db];
        for band in 0..3 {
            for ch in 0..2 {
                self.bands[band][ch].c = coeffs[band];
            }
            // A band at 0 dB is skipped; filter memory is dropped so it starts clean when used.
            let active = gains[band].abs() > 0.01;
            if !active {
                self.bands[band].iter_mut().for_each(Biquad::reset);
            }
            self.active[band] = active;
        }
    }

    pub fn is_active(&self) -> bool {
        self.settings.enabled && self.active.iter().any(|&a| a)
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.is_active() {
            return;
        }
        for band in 0..3 {
            if !self.active[band] {
                continue;
            }
            let [fl, fr] = &mut self.bands[band];
            for x in l.iter_mut() {
                *x = fl.process(*x);
            }
            for x in r.iter_mut() {
                *x = fr.process(*x);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{db_to_lin, rms};

    const SR: f32 = 48000.0;

    fn sine(hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / SR).sin() * 0.5)
            .collect()
    }

    /// Gain in dB of the EQ at `hz`, measured on a steady sine (after the filter settled).
    fn gain_db(eq: &mut Eq, hz: f32) -> f32 {
        let mut l = sine(hz, 48000);
        let mut r = l.clone();
        let input = rms(&l[24000..]);
        eq.process(&mut l, &mut r);
        20.0 * (rms(&l[24000..]) / input).log10()
    }

    fn with(f: impl FnOnce(&mut EqSettings)) -> Eq {
        let mut s = EqSettings {
            enabled: true,
            ..EqSettings::default()
        };
        f(&mut s);
        let mut eq = Eq::new(SR);
        eq.set(&s);
        eq
    }

    #[test]
    fn disabled_or_flat_is_bit_exact() {
        let input = sine(440.0, 1000);
        for settings in [
            EqSettings::default(),
            EqSettings {
                low_db: 6.0,
                ..EqSettings::default()
            },
        ] {
            let (mut l, mut r) = (input.clone(), input.clone());
            let mut eq = Eq::new(SR);
            eq.set(&settings); // not enabled
            eq.process(&mut l, &mut r);
            assert_eq!(l, input);
        }
        let (mut l, mut r) = (input.clone(), input.clone());
        with(|_| {}).process(&mut l, &mut r);
        assert_eq!(l, input, "enabled but flat");
    }

    #[test]
    fn bands_boost_and_cut_where_they_should() {
        let mut eq = with(|s| s.low_db = 12.0);
        assert!(
            (gain_db(&mut eq, 40.0) - 12.0).abs() < 1.0,
            "low shelf boosts the bass"
        );
        assert!(gain_db(&mut eq, 8000.0).abs() < 0.5, "and leaves the highs");

        let mut eq = with(|s| s.high_db = -12.0);
        assert!(
            (gain_db(&mut eq, 14000.0) + 12.0).abs() < 1.5,
            "high shelf cuts the top"
        );
        assert!(gain_db(&mut eq, 100.0).abs() < 0.5);

        let mut eq = with(|s| {
            s.mid_db = 9.0;
            s.mid_hz = 2000.0;
            s.mid_q = 2.0;
        });
        assert!(
            (gain_db(&mut eq, 2000.0) - 9.0).abs() < 0.5,
            "peak at its centre"
        );
        assert!(
            gain_db(&mut eq, 200.0).abs() < 1.0,
            "narrow: far away is untouched"
        );
    }

    #[test]
    fn coefficients_match_the_analytic_response() {
        let c = Coeffs::peaking(SR, 1000.0, 1.0, 6.0);
        assert!((c.magnitude(SR, 1000.0) - db_to_lin(6.0)).abs() < 1e-3);
        let c = Coeffs::low_shelf(SR, 200.0, -6.0);
        assert!((c.magnitude(SR, 20.0) - db_to_lin(-6.0)).abs() < 0.02);
        assert!((c.magnitude(SR, 20000.0) - 1.0).abs() < 0.02);
    }

    #[test]
    fn stays_stable_at_extremes() {
        let mut eq = with(|s| {
            s.low_db = 18.0;
            s.low_hz = 20.0;
            s.mid_db = 18.0;
            s.mid_q = 8.0;
            s.high_db = 18.0;
            s.high_hz = 16000.0;
        });
        let mut l: Vec<f32> = (0..96000)
            .map(|i| ((i * 7919) % 2000) as f32 / 1000.0 - 1.0)
            .collect();
        let mut r = l.clone();
        eq.process(&mut l, &mut r);
        assert!(l.iter().all(|x| x.is_finite() && x.abs() < 100.0));
    }
}
