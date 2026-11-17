//! Velocity curves: how hard a key is struck on a keyboard against the velocity the instruments
//! get. A curve belongs to a device (a heavy keyboard wants another curve than a light one), not
//! to a rig, so it lives in the app's settings by device name.
//!
//! The audio thread never sees a [`VelocityCurve`]: it gets a [`table`](VelocityCurve::table)
//! of 128 bytes per input and looks each note-on up in it.

use serde::{Deserialize, Serialize};

/// Number of MIDI velocities, and the length of a curve's table.
pub const STEPS: usize = 128;

/// The velocity [`VelocityCurve::Constant`] plays every note at.
pub const CONSTANT: u8 = 64;

/// Exponent of the soft and hard curves: `out = in^g` on 0..1.
const SOFT: f32 = 0.55;
const HARD: f32 = 1.8;

/// What velocity a device's notes reach the instruments with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VelocityCurve {
    /// Light playing already sounds loud: for stiff keys or a light touch.
    Soft,
    /// As played.
    #[default]
    Medium,
    /// It takes a firm touch to sound loud: for light keys or a heavy hand.
    Hard,
    /// Every note at [`CONSTANT`], however it is played (organs, drum pads).
    Constant,
    /// Drawn by the user: the velocity sent for each velocity played (0..=127). A list of
    /// another length is read as evenly spaced points from 1 to 127 with straight lines between.
    Drawn(Vec<u8>),
}

impl VelocityCurve {
    /// The presets, in the order they are offered, with their names.
    pub const PRESETS: [(VelocityCurve, &'static str); 4] = [
        (VelocityCurve::Soft, "Soft"),
        (VelocityCurve::Medium, "Medium"),
        (VelocityCurve::Hard, "Hard"),
        (VelocityCurve::Constant, "Constant"),
    ];

    pub fn is_medium(&self) -> bool {
        *self == VelocityCurve::Medium
    }

    /// The velocity sent for each velocity played. Entry 0 is 0 (a note-on with velocity 0 is a
    /// note-off and must stay one); every other entry is 1..=127, so no curve turns a note into
    /// a note-off.
    pub fn table(&self) -> [u8; STEPS] {
        let mut t = [0u8; STEPS];
        for (v, out) in t.iter_mut().enumerate().skip(1) {
            let x = v as f32 / 127.0;
            let y = match self {
                VelocityCurve::Soft => x.powf(SOFT) * 127.0,
                VelocityCurve::Medium => v as f32,
                VelocityCurve::Hard => x.powf(HARD) * 127.0,
                VelocityCurve::Constant => CONSTANT as f32,
                VelocityCurve::Drawn(points) => sample(points, v),
            };
            *out = y.round().clamp(1.0, 127.0) as u8;
        }
        t
    }

    /// This curve as a drawable one, to start drawing from.
    pub fn to_drawn(&self) -> Vec<u8> {
        match self {
            VelocityCurve::Drawn(p) if p.len() == STEPS => p.clone(),
            other => other.table().to_vec(),
        }
    }

    /// A drawn curve that is the same as a preset is that preset, so the preset lights up and the
    /// settings file says its name.
    pub fn simplified(self) -> Self {
        let VelocityCurve::Drawn(_) = self else {
            return self;
        };
        let table = self.table();
        Self::PRESETS
            .into_iter()
            .map(|(p, _)| p)
            .find(|p| p.table() == table)
            .unwrap_or(self)
    }
}

/// The drawn curve at velocity `v` (1..=127).
fn sample(points: &[u8], v: usize) -> f32 {
    match points.len() {
        0 => v as f32,
        STEPS => points[v] as f32,
        1 => points[0] as f32,
        n => {
            // Evenly spaced over 1..=127.
            let at = (v - 1) as f32 / 126.0 * (n - 1) as f32;
            let i = (at.floor() as usize).min(n - 2);
            let f = at - i as f32;
            points[i] as f32 * (1.0 - f) + points[i + 1] as f32 * f
        }
    }
}

/// Draws a straight line on a drawn curve (`STEPS` long) from one point to another, so a quick
/// stroke that skipped velocities between two frames leaves no gaps. Points are (played, sent);
/// played 0 is not drawable, it is the note-off.
pub fn draw_line(points: &mut [u8], from: (u8, u8), to: (u8, u8)) {
    if points.len() != STEPS {
        return;
    }
    let ((x0, y0), (x1, y1)) = if from.0 <= to.0 {
        (from, to)
    } else {
        (to, from)
    };
    let (x0, x1) = (x0.max(1), x1.clamp(1, 127));
    for x in x0..=x1 {
        let y = if x1 == x0 {
            y1 as f32
        } else {
            y0 as f32 + (y1 as f32 - y0 as f32) * (x - x0) as f32 / (x1 - x0) as f32
        };
        points[x as usize] = y.round().clamp(1.0, 127.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medium_plays_as_played_and_zero_stays_a_note_off() {
        let t = VelocityCurve::Medium.table();
        for (v, out) in t.iter().enumerate() {
            assert_eq!(*out as usize, v);
        }
        for (c, _) in VelocityCurve::PRESETS {
            let t = c.table();
            assert_eq!(t[0], 0, "{c:?}");
            assert!(t[1..].iter().all(|v| (1..=127).contains(v)), "{c:?}");
        }
    }

    #[test]
    fn soft_is_louder_and_hard_quieter_and_both_reach_the_ends() {
        let (soft, hard) = (VelocityCurve::Soft.table(), VelocityCurve::Hard.table());
        for v in 1..=127 {
            assert!(soft[v] >= v as u8, "soft at {v}: {}", soft[v]);
            assert!(hard[v] <= v as u8, "hard at {v}: {}", hard[v]);
        }
        // Clearly apart where most playing happens.
        assert!(
            soft[64] >= 80 && hard[64] <= 40,
            "{} {}",
            soft[64],
            hard[64]
        );
        for t in [soft, hard] {
            assert_eq!(t[127], 127);
            assert!(t.windows(2).all(|w| w[0] <= w[1]), "rises");
        }
    }

    #[test]
    fn constant_plays_every_note_at_64() {
        let t = VelocityCurve::Constant.table();
        assert!(t[1..].iter().all(|v| *v == CONSTANT));
    }

    #[test]
    fn a_drawn_curve_never_sends_a_note_off_and_short_lists_are_points() {
        let mut zeros = vec![0u8; STEPS];
        zeros[10] = 200;
        let t = VelocityCurve::Drawn(zeros).table();
        assert_eq!(t[0], 0);
        assert_eq!(t[1], 1);
        assert_eq!(t[10], 127);
        // Two points: a straight line from 127 down to 1.
        let t = VelocityCurve::Drawn(vec![127, 1]).table();
        assert_eq!((t[1], t[64], t[127]), (127, 64, 1));
        assert_eq!(
            VelocityCurve::Drawn(Vec::new()).table(),
            VelocityCurve::Medium.table()
        );
    }

    #[test]
    fn drawing_fills_the_gaps_between_two_frames() {
        let mut p = VelocityCurve::Medium.to_drawn();
        draw_line(&mut p, (100, 20), (60, 100));
        assert_eq!(p[60], 100);
        assert_eq!(p[80], 60);
        assert_eq!(p[100], 20);
        assert_eq!(p[59], 59);
        assert_eq!(p[101], 101);
        // Played 0 is the note-off: drawing there does nothing.
        draw_line(&mut p, (0, 90), (0, 90));
        assert_eq!(p[0], 0);
        assert_eq!(p[1], 90);
    }

    #[test]
    fn a_drawn_preset_is_the_preset() {
        assert_eq!(
            VelocityCurve::Drawn(VelocityCurve::Hard.to_drawn()).simplified(),
            VelocityCurve::Hard
        );
        let mut p = VelocityCurve::Hard.to_drawn();
        p[5] = 99;
        assert!(matches!(
            VelocityCurve::Drawn(p).simplified(),
            VelocityCurve::Drawn(_)
        ));
    }
}
