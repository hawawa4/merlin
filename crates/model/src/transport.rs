//! The rig's transport settings: tempo, time signature, count-in and click (plan §5).

use serde::{Deserialize, Serialize};

use crate::setlist::{MAX_TEMPO, MIN_TEMPO};

/// Most bars of count-in.
pub const MAX_COUNT_IN_BARS: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransportSettings {
    /// The tempo when no song or part sets one; also where a tap or a nudge leaves it.
    pub tempo: f32,
    pub beats_per_bar: u8,
    /// The note that gets the beat: 4 for a quarter (3/4), 8 for an eighth (6/8). The tempo is
    /// always in quarter notes per minute.
    pub beat_unit: u8,
    /// Bars of click before the transport starts.
    pub count_in_bars: u8,
    pub click: bool,
    /// 0..=1.
    pub click_volume: f32,
}

impl Default for TransportSettings {
    fn default() -> Self {
        Self {
            tempo: 120.0,
            beats_per_bar: 4,
            beat_unit: 4,
            count_in_bars: 1,
            click: true,
            click_volume: 0.5,
        }
    }
}

impl TransportSettings {
    pub fn is_valid(&self) -> bool {
        (MIN_TEMPO..=MAX_TEMPO).contains(&self.tempo)
            && (1..=16).contains(&self.beats_per_bar)
            && matches!(self.beat_unit, 4 | 8)
            && self.count_in_bars <= MAX_COUNT_IN_BARS
            && (0.0..=1.0).contains(&self.click_volume)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_sparse_files_load() {
        assert!(TransportSettings::default().is_valid());
        let t: TransportSettings = serde_json::from_str(r#"{"tempo":90}"#).unwrap();
        assert_eq!((t.tempo, t.beats_per_bar, t.count_in_bars), (90.0, 4, 1));
        assert_eq!(t.beat_unit, 4, "rigs from before 6/8 are in quarters");
        assert!(!TransportSettings { beat_unit: 3, ..t }.is_valid());
        assert!(!TransportSettings { tempo: 5.0, ..t }.is_valid());
    }
}
