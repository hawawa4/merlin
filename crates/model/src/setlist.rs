//! Songs and setlists (plan §5): the rig's songs, in playing order, each with parts. A part is
//! a snapshot plus what is special about it (a tempo, a note for the player). All songs share
//! the rig's slots, so moving between songs only changes what plays and how.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Most songs in a rig, and most parts in a song.
pub const MAX_SONGS: usize = 256;
pub const MAX_PARTS: usize = 64;

/// Most backing tracks a song can play at once.
pub const MAX_TRACKS: usize = 16;

/// Most timeline markers in a song.
pub const MAX_SONG_MARKERS: usize = 128;

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

/// A backing track: an audio file played in time with the song.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    pub name: String,
    pub path: PathBuf,
    /// Linear gain, 0..=2 (1 = as recorded).
    pub gain: f32,
    pub mute: bool,
    /// Plays on the cue output (where the click goes) instead of the main mix: a guide track
    /// for the player's headphones.
    pub cue: bool,
}

impl Default for Track {
    fn default() -> Self {
        Self {
            name: String::new(),
            path: PathBuf::new(),
            gain: 1.0,
            mute: false,
            cue: false,
        }
    }
}

impl Track {
    /// The name, or the file's name.
    pub fn label(&self) -> String {
        if self.name.trim().is_empty() {
            self.path
                .file_stem()
                .map_or_else(|| "track".to_owned(), |s| s.to_string_lossy().into_owned())
        } else {
            self.name.clone()
        }
    }
}

/// What a timeline marker does when the song's run reaches it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MarkerAction {
    /// Enter part `part` (counted from 0) of this song.
    GoToPart {
        part: u8,
    },
    NextPart,
    /// Show a message to the player in the Perform view.
    Cue {
        text: String,
    },
    /// Stop the transport (and the backing tracks).
    Stop,
    /// Send a Program Change on the MIDI output (channel 0-15).
    ProgramChange {
        channel: u8,
        program: u8,
    },
    /// Send a Control Change on the MIDI output.
    ControlChange {
        channel: u8,
        number: u8,
        value: u8,
    },
}

/// A point on the song's timeline, in bars and beats from the start of the run (bar 1 beat 1 is
/// the first beat).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub bar: u32,
    pub beat: u8,
    pub action: MarkerAction,
}

impl Marker {
    /// Clock ticks (24 to a beat) from the start of the run, for a song with `beats_per_bar`.
    pub fn tick(&self, beats_per_bar: u8) -> u64 {
        let beats = u64::from(self.bar.saturating_sub(1)) * u64::from(beats_per_bar)
            + u64::from(self.beat.saturating_sub(1));
        beats * 24
    }

    /// The bytes it sends on the MIDI output, if it sends any.
    pub fn midi(&self) -> Option<([u8; 3], u8)> {
        match self.action {
            MarkerAction::ProgramChange { channel, program } => {
                Some(([0xC0 | (channel & 15), program & 127, 0], 2))
            }
            MarkerAction::ControlChange {
                channel,
                number,
                value,
            } => Some(([0xB0 | (channel & 15), number & 127, value & 127], 3)),
            _ => None,
        }
    }

    fn is_valid(&self, beats_per_bar: u8) -> bool {
        (1..=9999).contains(&self.bar)
            && (1..=beats_per_bar).contains(&self.beat)
            && match self.action {
                MarkerAction::GoToPart { part } => usize::from(part) < MAX_PARTS,
                MarkerAction::ProgramChange { channel, program } => channel < 16 && program < 128,
                MarkerAction::ControlChange {
                    channel,
                    number,
                    value,
                } => channel < 16 && number < 128 && value < 128,
                _ => true,
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
    /// Backing tracks, played from the start of the run.
    pub tracks: Vec<Track>,
    /// Things that happen at points of the song: a part change, a cue, MIDI to other gear.
    pub markers: Vec<Marker>,
}

impl Default for Song {
    fn default() -> Self {
        Self {
            name: String::new(),
            tempo: 120.0,
            beats_per_bar: 4,
            parts: Vec::new(),
            note: String::new(),
            tracks: Vec::new(),
            markers: Vec::new(),
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
            && self.tracks.len() <= MAX_TRACKS
            && self.markers.len() <= MAX_SONG_MARKERS
            && self.markers.iter().all(|m| m.is_valid(self.beats_per_bar))
            && self.tracks.iter().all(|t| (0.0..=2.0).contains(&t.gain))
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
    fn tracks_have_names_and_limits() {
        let t = Track {
            path: "/music/stems/Drums.wav".into(),
            ..Track::default()
        };
        assert_eq!(t.label(), "Drums");
        let mut song = Song {
            tracks: vec![t; MAX_TRACKS + 1],
            ..Song::default()
        };
        assert!(!song.is_valid(0));
        song.tracks.truncate(1);
        song.tracks[0].gain = 3.0;
        assert!(!song.is_valid(0));
    }

    #[test]
    fn markers_know_their_tick_and_their_midi() {
        let m = Marker {
            bar: 3,
            beat: 2,
            action: MarkerAction::ProgramChange {
                channel: 1,
                program: 5,
            },
        };
        // Bar 3 beat 2 in 4/4 is the 10th beat from the start: 9 beats of 24 ticks.
        assert_eq!(m.tick(4), 9 * 24);
        assert_eq!(m.midi(), Some(([0xC1, 5, 0], 2)));
        assert_eq!(
            Marker {
                bar: 1,
                beat: 1,
                action: MarkerAction::Stop
            }
            .tick(3),
            0
        );
        let song = Song {
            beats_per_bar: 3,
            markers: vec![m],
            ..Song::default()
        };
        assert!(song.is_valid(0), "beat 2 exists in 3/4");
        let bad = Song {
            beats_per_bar: 2,
            markers: vec![Marker {
                bar: 1,
                beat: 3,
                action: MarkerAction::NextPart,
            }],
            ..Song::default()
        };
        assert!(!bad.is_valid(0), "there is no beat 3 in 2/4");
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
