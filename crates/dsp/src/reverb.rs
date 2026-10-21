//! A Freeverb-style reverb: eight parallel damped comb filters into four series all-pass
//! filters per channel, with the right channel's delays slightly longer for stereo width.

use merlin_model::ReverbSettings;

use crate::util::DENORMAL_GUARD;

const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;
/// Tunings above are for 44.1 kHz.
const REFERENCE_RATE: f32 = 44100.0;
/// Input is scaled down because eight combs add up.
const INPUT_GAIN: f32 = 0.015;
const WET_SCALE: f32 = 3.0;

struct Comb {
    buf: Vec<f32>,
    idx: usize,
    store: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
            store: 0.0,
        }
    }

    fn process(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buf[self.idx];
        self.store = out * (1.0 - damp) + self.store * damp + DENORMAL_GUARD;
        self.buf[self.idx] = input + self.store * feedback;
        self.idx += 1;
        if self.idx == self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    idx: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.buf[self.idx];
        let out = delayed - input;
        self.buf[self.idx] = input + delayed * 0.5 + DENORMAL_GUARD;
        self.idx += 1;
        if self.idx == self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

struct Channel {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Channel {
    fn new(scale: f32, extra: usize) -> Self {
        let len = |n: usize| ((n + extra) as f32 * scale) as usize;
        Self {
            combs: COMBS.iter().map(|&n| Comb::new(len(n))).collect(),
            allpasses: ALLPASSES.iter().map(|&n| Allpass::new(len(n))).collect(),
        }
    }

    fn process(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let mut out = 0.0;
        for c in &mut self.combs {
            out += c.process(input, feedback, damp);
        }
        for a in &mut self.allpasses {
            out = a.process(out);
        }
        out
    }
}

pub struct Reverb {
    s: ReverbSettings,
    left: Channel,
    right: Channel,
    feedback: f32,
    damp: f32,
    wet1: f32,
    wet2: f32,
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        let scale = sample_rate / REFERENCE_RATE;
        let mut r = Self {
            s: ReverbSettings::default(),
            left: Channel::new(scale, 0),
            right: Channel::new(scale, STEREO_SPREAD),
            feedback: 0.0,
            damp: 0.0,
            wet1: 1.0,
            wet2: 0.0,
        };
        r.set(&ReverbSettings::default());
        r
    }

    pub fn set(&mut self, s: &ReverbSettings) {
        self.s = *s;
        self.feedback = 0.7 + s.room * 0.28;
        self.damp = s.damping * 0.4;
        self.wet1 = s.width / 2.0 + 0.5;
        self.wet2 = (1.0 - s.width) / 2.0;
    }

    pub fn is_enabled(&self) -> bool {
        self.s.enabled
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.s.enabled {
            return;
        }
        let gain = self.s.mix * WET_SCALE;
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let input = (*x + *y) * INPUT_GAIN;
            let wl = self.left.process(input, self.feedback, self.damp);
            let wr = self.right.process(input, self.feedback, self.damp);
            *x += (wl * self.wet1 + wr * self.wet2) * gain;
            *y += (wr * self.wet1 + wl * self.wet2) * gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::rms;

    const SR: f32 = 48000.0;

    fn verb(f: impl FnOnce(&mut ReverbSettings)) -> Reverb {
        let mut s = ReverbSettings {
            enabled: true,
            mix: 1.0,
            ..ReverbSettings::default()
        };
        f(&mut s);
        let mut r = Reverb::new(SR);
        r.set(&s);
        r
    }

    fn impulse_response(r: &mut Reverb, seconds: f32) -> (Vec<f32>, Vec<f32>) {
        let n = (seconds * SR) as usize;
        let mut l = vec![0.0; n];
        l[0] = 1.0;
        let mut rr = l.clone();
        r.process(&mut l, &mut rr);
        l[0] -= 1.0; // remove the dry impulse
        rr[0] -= 1.0;
        (l, rr)
    }

    /// Seconds until the response is 60 dB below its loudest 100 ms.
    fn rt60(x: &[f32]) -> f32 {
        let win = 4800;
        let peak = x.chunks(win).map(rms).fold(0.0, f32::max);
        let last = x
            .chunks(win)
            .enumerate()
            .filter(|(_, c)| rms(c) > peak * 0.001)
            .map(|(i, _)| i)
            .next_back()
            .unwrap_or(0);
        (last + 1) as f32 * win as f32 / SR
    }

    #[test]
    fn it_rings_and_dies_away_without_blowing_up() {
        let mut r = verb(|_| {});
        let (l, rr) = impulse_response(&mut r, 8.0);
        assert!(rms(&l[2000..20000]) > 1e-3, "there is a tail");
        assert!(l.iter().chain(&rr).all(|x| x.is_finite() && x.abs() < 2.0));
        assert!(
            rms(&l[l.len() - 4800..]) < 1e-5,
            "and it has died away after 8 s"
        );
        // No DC offset builds up.
        let mean = l.iter().sum::<f32>() / l.len() as f32;
        assert!(mean.abs() < 1e-4);
    }

    #[test]
    fn bigger_rooms_ring_longer() {
        let small = rt60(&impulse_response(&mut verb(|s| s.room = 0.1), 10.0).0);
        let large = rt60(&impulse_response(&mut verb(|s| s.room = 0.95), 10.0).0);
        assert!(large > small * 2.0, "small {small} s, large {large} s");
    }

    #[test]
    fn damping_removes_highs_from_the_tail() {
        let hf = |damping: f32| {
            let (l, _) = impulse_response(&mut verb(|s| s.damping = damping), 3.0);
            let tail = &l[24000..];
            tail.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum::<f32>()
                / tail.iter().map(|x| x * x).sum::<f32>()
        };
        assert!(hf(1.0) < hf(0.0) * 0.5, "damped tail is darker");
    }

    #[test]
    fn width_controls_the_stereo_spread() {
        let (l, r) = impulse_response(&mut verb(|s| s.width = 0.0), 1.0);
        assert!(
            l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-7),
            "width 0 is mono"
        );
        let (l, r) = impulse_response(&mut verb(|s| s.width = 1.0), 1.0);
        assert!(
            l.iter().zip(&r).any(|(a, b)| (a - b).abs() > 1e-4),
            "width 1 is stereo"
        );
    }

    #[test]
    fn mix_scales_the_wet_signal_and_disabled_is_bit_exact() {
        let (a, _) = impulse_response(&mut verb(|s| s.mix = 0.5), 1.0);
        let (b, _) = impulse_response(&mut verb(|s| s.mix = 1.0), 1.0);
        assert!((rms(&b) / rms(&a) - 2.0).abs() < 0.01);

        let mut off = Reverb::new(SR);
        let mut l: Vec<f32> = (0..2000).map(|i| (i as f32 * 0.02).sin()).collect();
        let copy = l.clone();
        let mut r = l.clone();
        off.process(&mut l, &mut r);
        assert_eq!(l, copy);
    }

    #[test]
    fn long_silence_after_a_burst_does_not_leave_denormals_behind() {
        let mut r = verb(|s| s.room = 1.0);
        let mut l: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.3).sin()).collect();
        let mut rr = l.clone();
        r.process(&mut l, &mut rr);
        for _ in 0..60 {
            let mut l = vec![0.0; 48000];
            let mut rr = vec![0.0; 48000];
            r.process(&mut l, &mut rr);
            assert!(l.iter().all(|x| *x == 0.0 || x.abs() > 1e-30 || *x == 0.0));
        }
    }
}
