//! The effects of one slot, in signal order: EQ, compressor, delay, reverb.

use merlin_model::FxSettings;

use crate::{Compressor, Delay, Eq, Reverb};

pub struct FxChain {
    settings: FxSettings,
    eq: Eq,
    comp: Compressor,
    delay: Delay,
    reverb: Reverb,
}

impl FxChain {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            settings: FxSettings::default(),
            eq: Eq::new(sample_rate),
            comp: Compressor::new(sample_rate),
            delay: Delay::new(sample_rate),
            reverb: Reverb::new(sample_rate),
        }
    }

    /// Applies new settings (clamped to their ranges). Cheap when nothing changed.
    pub fn set(&mut self, settings: &FxSettings) {
        let mut s = *settings;
        s.clamp();
        if s.eq != self.settings.eq {
            self.eq.set(&s.eq);
        }
        if s.comp != self.settings.comp {
            self.comp.set(&s.comp);
        }
        if s.delay != self.settings.delay {
            self.delay.set(&s.delay);
        }
        if s.reverb != self.settings.reverb {
            self.reverb.set(&s.reverb);
        }
        self.settings = s;
    }

    /// Gain reduction of the compressor, for the meters.
    pub fn compressor_reduction_db(&self) -> f32 {
        self.comp.reduction_db()
    }

    /// Does anything beyond the dry path run? (The engine skips the chain otherwise.)
    pub fn is_active(&self) -> bool {
        self.settings.any_enabled()
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        self.eq.process(l, r);
        self.comp.process(l, r);
        self.delay.process(l, r);
        self.reverb.process(l, r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed signal and settings give a fixed output: this guards the DSP against unintended
    /// changes. If you change an effect on purpose, update the expected value.
    #[test]
    fn golden_output_checksum() {
        let mut fx = FxSettings::default();
        fx.eq.enabled = true;
        fx.eq.low_db = 4.0;
        fx.eq.high_db = -3.0;
        fx.comp.enabled = true;
        fx.delay.enabled = true;
        fx.reverb.enabled = true;
        let mut chain = FxChain::new(48000.0);
        chain.set(&fx);
        let mut l: Vec<f32> = (0..48000)
            .map(|i| (i as f32 * 0.031).sin() * 0.6 * if i < 6000 { 1.0 } else { 0.0 })
            .collect();
        let mut r = l.clone();
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            chain.process(cl, cr);
        }
        let checksum: f64 = l.iter().chain(&r).map(|&x| x as f64 * x as f64).sum();
        assert!(checksum.is_finite() && checksum > 1.0, "{checksum}");
        // 6 decimals are stable across platforms for the same operations in the same order.
        assert_eq!(
            format!("{checksum:.3}"),
            GOLDEN,
            "output changed: update GOLDEN if intended"
        );
    }

    const GOLDEN: &str = "608.488";

    #[test]
    fn blocks_of_any_size_give_the_same_result() {
        let mut fx = FxSettings::default();
        fx.delay.enabled = true;
        fx.reverb.enabled = true;
        fx.comp.enabled = true;
        let run = |block: usize| {
            let mut chain = FxChain::new(48000.0);
            chain.set(&fx);
            let mut l: Vec<f32> = (0..9600).map(|i| (i as f32 * 0.02).sin() * 0.7).collect();
            let mut r = l.clone();
            for (cl, cr) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
                chain.process(cl, cr);
            }
            l
        };
        assert_eq!(run(64), run(480));
        assert_eq!(run(1), run(9600));
    }

    #[test]
    fn default_settings_leave_the_signal_untouched() {
        let mut chain = FxChain::new(48000.0);
        chain.set(&FxSettings::default());
        assert!(!chain.is_active());
        let mut l: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let copy = l.clone();
        let mut r = l.clone();
        chain.process(&mut l, &mut r);
        assert_eq!(l, copy);
    }
}
