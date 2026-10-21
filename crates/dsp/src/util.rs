//! Small helpers shared by the effects.

pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn lin_to_db(lin: f32) -> f32 {
    20.0 * lin.max(1e-9).log10()
}

/// One-pole smoothing coefficient for a time constant in seconds: `y += (x - y) * (1 - c)`.
pub fn time_coef(seconds: f32, sample_rate: f32) -> f32 {
    if seconds <= 0.0 {
        0.0
    } else {
        (-1.0 / (seconds * sample_rate)).exp()
    }
}

/// Added to filter state to keep recursive paths out of the denormal range, which can cost
/// 100x per sample. Far below audibility (-500 dB).
pub const DENORMAL_GUARD: f32 = 1e-20;

/// Root mean square of a block, for tests and meters.
pub fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}
