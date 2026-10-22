//! Songs and setlists (plan §5): the rig's songs, in playing order, each with parts. A part is
//! a snapshot plus what is special about it (a tempo, a note for the player). All songs share
//! the rig's slots, so moving between songs only changes what plays and how.

use serde::{Deserialize, Serialize};

/// Most songs in a rig, and most parts in a song.
pub const MAX_SONGS: usize = 256;
pub const MAX_PARTS: usize = 64;

pub const MIN_TEMPO: f32 = 20.0;
pub const MAX_TEMPO: f32 = 300.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Part {
    pub name: String,
    /// The snapshot recalled when the part starts (by position in the rig's snapshots).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<usize>,
    /// Beats per minute from here on; the song's tempo when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tempo: Option<f32>,
    /// A cue for the player, shown in the Perform view.
    pub note: String,
}

impl Part {
    pub fn label(&self, index: usize) -> String {
        if self.name.trim().is_empty() {
            format!("Part {}", index + 1)
        } else {
            self.name.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Song {
    pub name: String,
    pub tempo: f32,
    /// Beats in a bar (the top of the time signature; the bottom is a quarter note).
    pub beats_per_bar: u8,
    pub parts: Vec<Part>,
    pub note: String,
}

impl Default for Song {
    fn default() -> Self {
        Self {
            name: String::new(),
            tempo: 120.0,
            beats_per_bar: 4,
            parts: Vec::new(),
            note: String::new(),
        }
    }
}

impl Song {
    pub fn label(&self, index: usize) -> String {
        if self.name.trim().is_empty() {
            format!("Song {}", index + 1)
        } else {
            self.name.clone()
        }
    }

    /// The tempo that applies in part `part`.
    pub fn tempo_in(&self, part: usize) -> f32 {
        self.parts
            .get(part)
            .and_then(|p| p.tempo)
            .unwrap_or(self.tempo)
    }

    /// Is it something the app can play? `snapshots` is how many snapshots the rig has.
    pub fn is_valid(&self, snapshots: usize) -> bool {
        let tempo = |t: f32| (MIN_TEMPO..=MAX_TEMPO).contains(&t);
        tempo(self.tempo)
            && (1..=16).contains(&self.beats_per_bar)
            && self.parts.len() <= MAX_PARTS
            && self
                .parts
                .iter()
                .all(|p| p.snapshot.is_none_or(|s| s < snapshots) && p.tempo.is_none_or(tempo))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_can_override_the_tempo() {
        let song = Song {
            tempo: 100.0,
            parts: vec![
                Part::default(),
                Part {
                    tempo: Some(140.0),
                    ..Part::default()
                },
            ],
            ..Song::default()
        };
        assert_eq!(song.tempo_in(0), 100.0);
        assert_eq!(song.tempo_in(1), 140.0);
        assert_eq!(song.tempo_in(9), 100.0, "no such part: the song's tempo");
    }

    #[test]
    fn songs_are_checked_against_the_snapshots_that_exist() {
        let mut song = Song {
            parts: vec![Part {
                snapshot: Some(1),
                ..Part::default()
            }],
            ..Song::default()
        };
        assert!(song.is_valid(2));
        assert!(!song.is_valid(1), "snapshot 2 does not exist");
        song.parts[0].snapshot = None;
        song.tempo = 5.0;
        assert!(!song.is_valid(0));
    }

    #[test]
    fn sparse_songs_load() {
        let s: Song =
            serde_json::from_str(r#"{"name":"Intro","parts":[{"name":"A"},{}]}"#).unwrap();
        assert_eq!(s.tempo, 120.0);
        assert_eq!(s.parts[1].label(1), "Part 2");
        assert_eq!(Song::default().label(2), "Song 3");
    }
}
