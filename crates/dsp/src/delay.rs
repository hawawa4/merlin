//! A stereo delay with feedback, damping in the feedback path and an optional ping-pong mode.
//! The time follows its setting smoothly (no pitch glitches when it is turned).

use merlin_model::DelaySettings;

use crate::biquad::{Biquad, Coeffs};
use crate::util::{DENORMAL_GUARD, time_coef};

/// Longest delay time the buffers can hold, in seconds.
pub const MAX_SECONDS: f32 = 1.6;

pub struct Delay {
    s: DelaySettings,
    sample_rate: f32,
    buf: [Vec<f32>; 2],
    pos: usize,
    /// Current (smoothed) delay in samples.
    time: f32,
    target: f32,
    smooth: f32,
    damp: [Biquad; 2],
}

impl Delay {
    pub fn new(sample_rate: f32) -> Self {
        let len = (MAX_SECONDS * sample_rate) as usize + 4;
        let mut d = Self {
            s: DelaySettings::default(),
            sample_rate,
            buf: [vec![0.0; len], vec![0.0; len]],
            pos: 0,
            time: 0.0,
            target: 0.0,
            smooth: time_coef(0.05, sample_rate),
            damp: [Biquad::default(); 2],
        };
        d.set(&DelaySettings::default());
        d.time = d.target;
        d
    }

    pub fn set(&mut self, s: &DelaySettings) {
        self.s = *s;
        self.target =
            (s.time_ms / 1000.0 * self.sample_rate).clamp(1.0, self.buf[0].len() as f32 - 3.0);
        let c = Coeffs::low_pass(self.sample_rate, s.damping_hz, 0.707);
        self.damp[0].c = c;
        self.damp[1].c = c;
    }

    pub fn is_enabled(&self) -> bool {
        self.s.enabled
    }

    fn read(&self, ch: usize, delay: f32) -> f32 {
        let len = self.buf[ch].len();
        let pos = (self.pos as f32 - delay).rem_euclid(len as f32);
        let i = pos as usize;
        let frac = pos - i as f32;
        let (a, b) = (self.buf[ch][i % len], self.buf[ch][(i + 1) % len]);
        a + (b - a) * frac
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.s.enabled {
            return;
        }
        let len = self.buf[0].len();
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            self.time = self.target + (self.time - self.target) * self.smooth;
            let (wl, wr) = (self.read(0, self.time), self.read(1, self.time));
            // Ping-pong: each side's echo feeds the other side.
            let (fl, fr) = if self.s.ping_pong { (wr, wl) } else { (wl, wr) };
            let (in_l, in_r) = if self.s.ping_pong {
                // Mono-ise the input into the left line so the echoes bounce.
                let m = (*x + *y) * 0.5;
                (m, 0.0)
            } else {
                (*x, *y)
            };
            let wet_l = self.damp[0].process(fl * self.s.feedback);
            let wet_r = self.damp[1].process(fr * self.s.feedback);
            self.buf[0][self.pos] = in_l + wet_l + DENORMAL_GUARD;
            self.buf[1][self.pos] = in_r + wet_r + DENORMAL_GUARD;
            self.pos = (self.pos + 1) % len;
            *x += wl * self.s.mix;
            *y += wr * self.s.mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn delay(f: impl FnOnce(&mut DelaySettings)) -> Delay {
        let mut s = DelaySettings {
            enabled: true,
            time_ms: 100.0,
            feedback: 0.0,
            mix: 1.0,
            damping_hz: 18000.0,
            ..DelaySettings::default()
        };
        f(&mut s);
        let mut d = Delay::new(SR);
        d.set(&s);
        d.time = d.target;
        d
    }

    fn impulse(n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; n];
        l[0] = 1.0;
        (l.clone(), l)
    }

    fn peak_positions(x: &[f32], threshold: f32) -> Vec<usize> {
        (1..x.len() - 1)
            .filter(|&i| x[i] > threshold && x[i] >= x[i - 1] && x[i] > x[i + 1])
            .collect()
    }

    #[test]
    fn an_impulse_comes_back_after_the_set_time() {
        let mut d = delay(|_| {});
        let (mut l, mut r) = impulse(12000);
        d.process(&mut l, &mut r);
        assert_eq!(l[0], 1.0, "the dry signal passes");
        let echoes = peak_positions(&l[1..], 0.5);
        assert_eq!(echoes.len(), 1);
        assert!(
            (echoes[0] as i64 + 1 - 4800).abs() <= 1,
            "echo at 100 ms = 4800 samples: {}",
            echoes[0] + 1
        );
    }

    /// Sum of the samples in a window around `centre`: the "strength" of an echo, independent
    /// of how the damping filter smears it.
    fn echo_strength(x: &[f32], centre: usize) -> f32 {
        x[centre.saturating_sub(40)..(centre + 400).min(x.len())]
            .iter()
            .sum()
    }

    #[test]
    fn feedback_repeats_decay_geometrically() {
        let mut d = delay(|s| s.feedback = 0.5);
        let (mut l, mut r) = impulse(30000);
        d.process(&mut l, &mut r);
        let strengths: Vec<f32> = (1..=5).map(|k| echo_strength(&l, k * 4800)).collect();
        for w in strengths.windows(2) {
            assert!(
                (w[1] / w[0] - 0.5).abs() < 0.02,
                "each repeat is half as strong: {strengths:?}"
            );
        }
    }

    #[test]
    fn damping_darkens_the_repeats() {
        // Noise in, then let the feedback circulate and look at the late tail: highs die first.
        let brightness = |damping: f32| {
            let mut d = delay(|s| {
                s.feedback = 0.9;
                s.damping_hz = damping;
            });
            let mut seed = 12345u32;
            let mut noise = || {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 8) as f32 / 8388608.0 - 1.0
            };
            let mut l: Vec<f32> = (0..48000).map(|_| noise()).collect();
            let mut r = l.clone();
            d.process(&mut l, &mut r);
            let mut tl = vec![0.0; 96000];
            let mut tr = vec![0.0; 96000];
            d.process(&mut tl, &mut tr);
            let late = &tl[48000..];
            late.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum::<f32>()
                / late.iter().map(|x| x * x).sum::<f32>()
        };
        assert!(
            brightness(1500.0) < brightness(16000.0) * 0.5,
            "darker repeats have less high-frequency content"
        );
    }

    #[test]
    fn ping_pong_alternates_sides() {
        let mut d = delay(|s| {
            s.ping_pong = true;
            s.feedback = 0.8;
            s.time_ms = 50.0;
            s.mix = 1.0;
        });
        let (mut l, mut r) = impulse(24000);
        d.process(&mut l, &mut r);
        let il = peak_positions(&l[1..], 0.05);
        let ir = peak_positions(&r[1..], 0.05);
        assert!(!il.is_empty() && !ir.is_empty(), "both sides get echoes");
        assert!(
            il[0] < ir[0],
            "the first echo is on the left, the next on the right: {il:?} {ir:?}"
        );
    }

    #[test]
    fn changing_the_time_moves_smoothly_and_stays_finite() {
        let mut d = delay(|s| s.feedback = 0.6);
        let mut l: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let mut r = l.clone();
        let (first_l, first_r) = (&mut l[..24000], &mut r[..24000]);
        d.process(first_l, first_r);
        let mut s = d.s;
        s.time_ms = 700.0;
        d.set(&s);
        d.process(&mut l, &mut r);
        assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 10.0));
        assert!(
            d.time > 4800.0 && d.time <= d.target + 1.0,
            "glides towards the target"
        );
    }

    #[test]
    fn disabled_is_bit_exact_and_memory_is_fixed() {
        let mut d = Delay::new(SR);
        let (mut l, mut r) = impulse(4000);
        let copy = l.clone();
        d.process(&mut l, &mut r);
        assert_eq!(l, copy);
        assert!(d.buf[0].len() as f32 >= MAX_SECONDS * SR);
    }
}
