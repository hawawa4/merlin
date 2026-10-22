//! Backing tracks on the session (plan §5): the tracks of the song that is entered are opened
//! and decoded ahead ("armed"), so the transport's run can start them at once, and are armed
//! again after each stop. The decoding happens on background threads.

use std::path::PathBuf;
use std::time::Duration;

use merlin_backing::StemControl;
use merlin_engine::{Command, Stem};
use merlin_model::{MAX_TRACKS, Track};

use super::Session;

/// How long a start waits for the backing tracks to be ready before going without them.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// What the UI shows for a track.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackState {
    /// Another song is armed (or nothing is).
    NotArmed,
    Loading,
    Ready,
    Failed(String),
}

/// One track of the armed song.
pub(super) struct Armed {
    control: Option<StemControl>,
    /// Which stem of the engine plays it.
    engine_index: Option<usize>,
    error: Option<String>,
}

/// What a track edit changes.
#[derive(Debug)]
pub enum TrackEdit {
    Name(String),
    Gain(f32),
    Mute(bool),
    Cue(bool),
}

impl Session {
    /// The song whose tracks are open.
    pub fn armed_song(&self) -> Option<usize> {
        self.armed_song
    }

    pub fn track_state(&self, song: usize, k: usize) -> TrackState {
        if self.armed_song != Some(song) {
            return TrackState::NotArmed;
        }
        match self.armed.get(k) {
            Some(a) => match (&a.error, &a.control) {
                (Some(e), _) => TrackState::Failed(e.clone()),
                (None, Some(c)) if c.ready() => TrackState::Ready,
                (None, Some(c)) if c.failed() => {
                    TrackState::Failed("the file stopped being readable".into())
                }
                _ => TrackState::Loading,
            },
            None => TrackState::NotArmed,
        }
    }

    /// Seconds since the run began, for the song clock.
    pub fn run_seconds(&self) -> f64 {
        self.engine
            .shared
            .transport
            .run_seconds(self.sample_rate as f32)
    }

    /// A track's length, from the file.
    pub fn track_seconds(&self, song: usize, k: usize) -> Option<f64> {
        if self.armed_song != Some(song) {
            return None;
        }
        self.armed.get(k)?.control.as_ref()?.info.seconds
    }

    fn all_ready(&self) -> bool {
        self.armed
            .iter()
            .all(|a| a.control.as_ref().is_none_or(|c| c.ready() || c.failed()))
    }

    /// Takes every track out of the engine and stops decoding.
    pub(super) fn disarm(&mut self) {
        let _ = self.engine.send(Command::ClearStems);
        let _ = self.engine.send(Command::ClearMarkers);
        self.armed.clear();
        self.armed_song = None;
        self.needs_rearm = false;
        self.pending_start = None;
    }

    /// Opens the tracks of song `n` and hands them to the engine. Not while the transport runs.
    pub fn arm_song(&mut self, n: usize) {
        if self.transport_running() || self.counting_in() {
            // Pick it up after the next stop.
            self.armed_song = Some(n);
            self.needs_rearm = true;
            return;
        }
        let _ = self.engine.send(Command::ClearStems);
        self.send_markers(n);
        self.armed.clear();
        self.armed_song = Some(n);
        self.needs_rearm = false;
        let tracks = self
            .rig
            .songs
            .get(n)
            .map(|s| s.tracks.clone())
            .unwrap_or_default();
        let mut next_index = 0;
        for track in tracks.iter().take(MAX_TRACKS) {
            match merlin_backing::open(&track.path, self.sample_rate) {
                Ok((reader, control)) => {
                    let stem = Stem::new(reader, track.gain, track.mute, track.cue);
                    match self.engine.send(Command::AddStem(stem)) {
                        Ok(()) => {
                            self.armed.push(Armed {
                                control: Some(control),
                                engine_index: Some(next_index),
                                error: None,
                            });
                            next_index += 1;
                        }
                        Err(_) => self.armed.push(Armed {
                            control: None,
                            engine_index: None,
                            error: Some("the audio engine is busy".into()),
                        }),
                    }
                }
                Err(e) => {
                    self.say(format!("track {}: {e:#}", track.label()));
                    self.armed.push(Armed {
                        control: None,
                        engine_index: None,
                        error: Some(format!("{e:#}")),
                    });
                }
            }
        }
    }

    /// Called often: frees what the engine has let go of, re-arms after a stop, and starts a
    /// transport that was waiting for its tracks.
    pub(super) fn service_backing(&mut self) {
        drop(self.engine.take_retired_stems());
        let idle = !self.transport_running()
            && !self.counting_in()
            && !self
                .engine
                .shared
                .stems_playing
                .load(std::sync::atomic::Ordering::Relaxed);
        if self.needs_rearm
            && idle
            && let Some(n) = self.armed_song
        {
            self.arm_song(n);
        }
        if let Some(since) = self.pending_start
            && (self.all_ready() || since.elapsed() > READY_TIMEOUT)
        {
            self.pending_start = None;
            self.start_transport_now();
        }
    }

    /// Whether a start has to wait for the tracks to be decoded.
    pub(super) fn must_wait_for_tracks(&self) -> bool {
        !self.all_ready()
    }

    // ---- editing tracks ----------------------------------------------------------------

    pub fn add_track(&mut self, song: usize, path: PathBuf) {
        let Some(s) = self.rig.songs.get_mut(song) else {
            return;
        };
        if s.tracks.len() >= MAX_TRACKS {
            return;
        }
        s.tracks.push(Track {
            path,
            ..Track::default()
        });
        self.dirty = true;
        self.tracks_changed(song);
    }

    pub fn remove_track(&mut self, song: usize, k: usize) {
        let Some(s) = self.rig.songs.get_mut(song) else {
            return;
        };
        if k < s.tracks.len() {
            s.tracks.remove(k);
            self.dirty = true;
            self.tracks_changed(song);
        }
    }

    pub fn edit_track(&mut self, song: usize, k: usize, edit: TrackEdit) {
        let Some(t) = self
            .rig
            .songs
            .get_mut(song)
            .and_then(|s| s.tracks.get_mut(k))
        else {
            return;
        };
        match edit {
            TrackEdit::Name(v) => t.name = v,
            TrackEdit::Gain(v) => t.gain = v.clamp(0.0, 2.0),
            TrackEdit::Mute(v) => t.mute = v,
            TrackEdit::Cue(v) => t.cue = v,
        }
        let t = t.clone();
        self.dirty = true;
        // Gain, mute and output apply live.
        if self.armed_song == Some(song)
            && let Some(index) = self.armed.get(k).and_then(|a| a.engine_index)
        {
            let _ = self.engine.send(Command::SetStemMix {
                index,
                gain: t.gain,
                mute: t.mute,
                cue: t.cue,
            });
        }
    }

    /// The set of tracks of a song changed: arm it again, if it is the one that is armed.
    fn tracks_changed(&mut self, song: usize) {
        if self.armed_song == Some(song) {
            self.arm_song(song);
        }
    }
}
