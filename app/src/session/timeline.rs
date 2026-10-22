//! A song's timeline on the session (plan §5): markers at bars and beats of the run. The audio
//! thread fires them sample-accurately (and sends their MIDI itself); the ones that change the
//! app (enter a part, show a cue, stop) come back here as ids.

use std::time::{Duration, Instant};

use merlin_engine::{Command, EngineMarker};
use merlin_model::{MAX_SONG_MARKERS, Marker, MarkerAction};

use super::Session;

/// How long a cue stays on the Perform screen.
const CUE_SHOWN: Duration = Duration::from_secs(8);

impl Session {
    /// Gives the audio thread the markers of song `n`. Not while the transport runs: the list
    /// would start over.
    pub(super) fn send_markers(&mut self, n: usize) {
        let _ = self.engine.send(Command::ClearMarkers);
        let Some(song) = self.rig.songs.get(n) else {
            return;
        };
        let bpb = song.beats_per_bar;
        let markers: Vec<EngineMarker> = song
            .markers
            .iter()
            .enumerate()
            .map(|(i, m)| EngineMarker {
                tick: m.tick(bpb),
                id: i as u16,
                midi: m.midi(),
            })
            .collect();
        for m in markers {
            let _ = self.engine.send(Command::AddMarker(m));
        }
    }

    pub fn add_marker(&mut self, song: usize, marker: Marker) {
        let Some(s) = self.rig.songs.get_mut(song) else {
            return;
        };
        if s.markers.len() >= MAX_SONG_MARKERS {
            return;
        }
        s.markers.push(marker);
        self.markers_changed(song);
    }

    pub fn remove_marker(&mut self, song: usize, k: usize) {
        if let Some(s) = self.rig.songs.get_mut(song)
            && k < s.markers.len()
        {
            s.markers.remove(k);
            self.markers_changed(song);
        }
    }

    pub fn edit_marker(&mut self, song: usize, k: usize, edit: impl FnOnce(&mut Marker)) {
        if let Some(m) = self
            .rig
            .songs
            .get_mut(song)
            .and_then(|s| s.markers.get_mut(k))
        {
            edit(m);
            m.beat = m.beat.max(1);
            m.bar = m.bar.max(1);
            self.markers_changed(song);
        }
    }

    pub(super) fn markers_changed(&mut self, song: usize) {
        if let Some(s) = self.rig.songs.get_mut(song) {
            let bpb = s.beats_per_bar;
            // A beat the bar does not have is moved to its last one.
            for m in &mut s.markers {
                m.beat = m.beat.min(bpb);
            }
            s.markers.sort_by_key(|m| (m.bar, m.beat));
        }
        self.dirty = true;
        if self.armed_song == Some(song) {
            if self.transport_running() || self.counting_in() {
                self.needs_rearm = true;
            } else {
                self.send_markers(song);
            }
        }
    }

    /// A marker of the armed song fired on the audio thread.
    pub(super) fn marker_fired(&mut self, id: u16) {
        let Some(song) = self.armed_song else { return };
        let Some(action) = self
            .rig
            .songs
            .get(song)
            .and_then(|s| s.markers.get(usize::from(id)))
            .map(|m| m.action.clone())
        else {
            return;
        };
        match action {
            MarkerAction::GoToPart { part } => self.go_to(song, usize::from(part)),
            MarkerAction::NextPart => self.next_part(),
            MarkerAction::Cue { text } => {
                self.say(format!("cue: {text}"));
                self.cue = Some((text, Instant::now()));
            }
            MarkerAction::Stop => {
                if self.transport_running() || self.counting_in() {
                    self.transport_toggle();
                }
            }
            // Sent by the audio thread itself.
            MarkerAction::ProgramChange { .. } | MarkerAction::ControlChange { .. } => {}
        }
    }

    /// The cue to show now, if a marker showed one recently.
    pub fn cue(&self) -> Option<&str> {
        self.cue
            .as_ref()
            .filter(|(_, at)| at.elapsed() < CUE_SHOWN)
            .map(|(t, _)| t.as_str())
    }
}
