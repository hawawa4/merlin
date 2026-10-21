//! A lookahead brickwall limiter for the master output.
//!
//! The signal is delayed by `lookahead` samples while the gain that would be needed to keep
//! each sample under the ceiling is computed, minimum-filtered over the lookahead window and
//! smoothed with a moving average of the same length. The smoothed gain is never higher than
//! the needed gain for any sample that is still in the window, so the output cannot exceed the
//! ceiling, and the gain ramps down before a peak arrives and back up after it.

pub struct Limiter {
    ceiling: f32,
    lookahead: usize,
    delay: [Vec<f32>; 2],
    /// Needed gain per sample, over the last `lookahead` samples.
    needed: Vec<f32>,
    /// The minimum-filtered gain, over the last `lookahead` samples, and its running sum.
    mins: Vec<f32>,
    sum: f32,
    pos: usize,
    /// Smallest gain used in the last block, for meters.
    gain_floor: f32,
}

impl Limiter {
    /// `ceiling_db` is the highest output level, e.g. -0.3.
    pub fn new(sample_rate: f32, ceiling_db: f32) -> Self {
        let lookahead = ((0.0015 * sample_rate) as usize).max(2);
        Self {
            ceiling: 10f32.powf(ceiling_db / 20.0),
            lookahead,
            delay: [vec![0.0; lookahead], vec![0.0; lookahead]],
            needed: vec![1.0; lookahead],
            mins: vec![1.0; lookahead],
            sum: lookahead as f32,
            pos: 0,
            gain_floor: 1.0,
        }
    }

    /// Delay the limiter adds, in samples.
    pub fn latency(&self) -> usize {
        self.lookahead - 1
    }

    pub fn set_ceiling_db(&mut self, db: f32) {
        self.ceiling = 10f32.powf(db.min(0.0) / 20.0);
    }

    /// The most gain reduction applied in the last block, in dB (>= 0).
    pub fn reduction_db(&self) -> f32 {
        // abs(): -0.0 would compare as the largest value in an atomic f32-bits max.
        (-20.0 * self.gain_floor.max(1e-6).log10()).max(0.0).abs()
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = self.lookahead;
        let mut floor = 1.0f32;
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let (in_l, in_r) = (*x, *y);
            let peak = in_l.abs().max(in_r.abs());
            let need = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            self.needed[self.pos] = need;
            // Minimum over the last `n` needed gains (the ring holds exactly those).
            let min = self.needed.iter().copied().fold(1.0f32, f32::min);
            self.sum += min - self.mins[self.pos];
            self.mins[self.pos] = min;
            let gain = (self.sum / n as f32).min(1.0);
            floor = floor.min(gain);

            // Output the sample that entered `lookahead - 1` samples ago.
            let out_idx = (self.pos + 1) % n;
            let (dl, dr) = (self.delay[0][out_idx], self.delay[1][out_idx]);
            self.delay[0][self.pos] = in_l;
            self.delay[1][self.pos] = in_r;
            *x = dl * gain;
            *y = dr * gain;
            self.pos = (self.pos + 1) % n;
        }
        self.gain_floor = floor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn run(l: &mut [f32]) -> Limiter {
        let mut lim = Limiter::new(SR, -1.0);
        let mut r = l.to_vec();
        lim.process(l, &mut r);
        lim
    }

    #[test]
    fn nothing_exceeds_the_ceiling_whatever_comes_in() {
        let ceiling = 10f32.powf(-1.0 / 20.0);
        let mut seed = 99u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / 8388608.0 - 1.0
        };
        let mut l: Vec<f32> = (0..96000)
            .map(|i| match i / 8000 {
                0..=2 => noise() * 4.0,
                3 => 0.0,
                4 => (i as f32 * 0.07).sin() * 9.0,
                5 => {
                    if i % 2000 == 0 {
                        20.0
                    } else {
                        0.0
                    }
                }
                _ => noise() * 0.1,
            })
            .collect();
        run(&mut l);
        let worst = l.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!(worst <= ceiling + 1e-5, "peak {worst} vs ceiling {ceiling}");
    }

    #[test]
    fn quiet_signals_pass_unchanged_apart_from_the_delay() {
        let input: Vec<f32> = (0..9600).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let mut l = input.clone();
        let lim = run(&mut l);
        let d = lim.latency();
        assert!(l[..d].iter().all(|x| *x == 0.0));
        for i in d..input.len() {
            assert!((l[i] - input[i - d]).abs() < 1e-6, "sample {i}");
        }
        assert!(lim.reduction_db() < 0.001);
    }

    #[test]
    fn gain_comes_down_before_the_peak_and_recovers_after() {
        let mut l = vec![0.5; 24000];
        l[12000] = 4.0;
        let mut r = l.clone();
        let mut lim = Limiter::new(SR, 0.0);
        lim.process(&mut l, &mut r);
        let d = lim.latency();
        // The 4.0 sample ends up at 12000 + d, at the ceiling.
        assert!((l[12000 + d] - 1.0).abs() < 1e-4, "{}", l[12000 + d]);
        // Samples before it are already turned down (ramp), not clipped suddenly.
        assert!(l[12000 + d - 20] < 0.5 && l[12000 + d - 20] > 0.1);
        // And long after, the gain is back to unity.
        assert!((l[20000] - 0.5).abs() < 1e-5);
        assert!(lim.reduction_db() > 6.0);
    }

    #[test]
    fn stereo_linked_so_the_image_does_not_shift() {
        let mut l: Vec<f32> = (0..9600).map(|i| (i as f32 * 0.03).sin() * 3.0).collect();
        let mut r: Vec<f32> = l.iter().map(|x| x * 0.5).collect();
        let mut lim = Limiter::new(SR, 0.0);
        lim.process(&mut l, &mut r);
        let d = lim.latency();
        for i in d + 100..9000 {
            if l[i].abs() > 0.01 {
                assert!((r[i] / l[i] - 0.5).abs() < 1e-4, "ratio at {i}");
            }
        }
    }
}
