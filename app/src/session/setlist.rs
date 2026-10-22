//! The setlist on the session (plan §5): songs in order, each with parts, and moving through
//! them. Entering a part recalls its snapshot and sets the tempo. A part with no snapshot
//! leaves the sound alone.

use merlin_model::{MAX_PARTS, MAX_SONGS, MAX_TEMPO, MIN_TEMPO, Part, Song};

use super::Session;

impl Session {
    pub fn songs(&self) -> &[Song] {
        &self.rig.songs
    }

    /// The song and part that were entered last.
    pub fn position(&self) -> Option<(usize, usize)> {
        self.position
    }

    /// The tempo the transport runs at (what the last part, tap or nudge set).
    pub fn current_tempo(&self) -> f32 {
        self.tempo()
    }

    // ---- songs ------------------------------------------------------------------------

    pub fn add_song(&mut self) -> Option<usize> {
        if self.rig.songs.len() >= MAX_SONGS {
            return None;
        }
        self.rig.songs.push(Song::default());
        self.dirty = true;
        Some(self.rig.songs.len() - 1)
    }

    pub fn remove_song(&mut self, n: usize) {
        if n >= self.rig.songs.len() {
            return;
        }
        self.rig.songs.remove(n);
        self.position = match self.position {
            Some((s, _)) if s == n => None,
            Some((s, p)) if s > n => Some((s - 1, p)),
            other => other,
        };
        self.dirty = true;
    }

    /// Moves a song up or down the setlist.
    pub fn move_song(&mut self, n: usize, down: bool) {
        let to = if down { n + 1 } else { n.wrapping_sub(1) };
        if n >= self.rig.songs.len() || to >= self.rig.songs.len() {
            return;
        }
        self.rig.songs.swap(n, to);
        self.position = self.position.map(|(s, p)| {
            if s == n {
                (to, p)
            } else if s == to {
                (n, p)
            } else {
                (s, p)
            }
        });
        self.dirty = true;
    }

    pub fn edit_song(&mut self, n: usize, edit: impl FnOnce(&mut Song)) {
        if let Some(s) = self.rig.songs.get_mut(n) {
            edit(s);
            s.tempo = s.tempo.clamp(MIN_TEMPO, MAX_TEMPO);
            s.beats_per_bar = s.beats_per_bar.clamp(1, 16);
            self.dirty = true;
        }
    }

    // ---- parts ------------------------------------------------------------------------

    pub fn add_part(&mut self, song: usize, snapshot: Option<usize>) -> Option<usize> {
        let s = self.rig.songs.get_mut(song)?;
        if s.parts.len() >= MAX_PARTS {
            return None;
        }
        s.parts.push(Part {
            snapshot,
            ..Part::default()
        });
        self.dirty = true;
        Some(s.parts.len() - 1)
    }

    /// A new part with a snapshot of how the rig is set up right now.
    pub fn add_part_from_current(&mut self, song: usize) {
        if song >= self.rig.songs.len() || self.rig.songs[song].parts.len() >= MAX_PARTS {
            return;
        }
        let snap = self.add_snapshot();
        if let Some(k) = self.add_part(song, snap) {
            let n = snap.map(|s| s + 1).unwrap_or(0);
            if let Some(p) = self.rig.songs[song].parts.get_mut(k) {
                p.name = format!("Part {}", k + 1);
            }
            self.say(format!(
                "part {} of song {} uses snapshot {n}",
                k + 1,
                song + 1
            ));
        }
    }

    pub fn remove_part(&mut self, song: usize, k: usize) {
        let Some(s) = self.rig.songs.get_mut(song) else {
            return;
        };
        if k >= s.parts.len() {
            return;
        }
        s.parts.remove(k);
        self.position = match self.position {
            Some((si, p)) if si == song && p > k => Some((si, p - 1)),
            Some((si, p)) if si == song && p == k => {
                Some((si, p.min(s.parts.len().saturating_sub(1))))
            }
            other => other,
        };
        self.dirty = true;
    }

    pub fn move_part(&mut self, song: usize, k: usize, down: bool) {
        let to = if down { k + 1 } else { k.wrapping_sub(1) };
        let Some(s) = self.rig.songs.get_mut(song) else {
            return;
        };
        if k >= s.parts.len() || to >= s.parts.len() {
            return;
        }
        s.parts.swap(k, to);
        self.position = self.position.map(|(si, p)| {
            if si != song {
                (si, p)
            } else if p == k {
                (si, to)
            } else if p == to {
                (si, k)
            } else {
                (si, p)
            }
        });
        self.dirty = true;
    }

    pub fn edit_part(&mut self, song: usize, k: usize, edit: impl FnOnce(&mut Part)) {
        let snapshots = self.rig.snapshots.len();
        if let Some(p) = self
            .rig
            .songs
            .get_mut(song)
            .and_then(|s| s.parts.get_mut(k))
        {
            edit(p);
            p.snapshot = p.snapshot.filter(|&n| n < snapshots);
            p.tempo = p.tempo.map(|t| t.clamp(MIN_TEMPO, MAX_TEMPO));
            self.dirty = true;
        }
    }

    /// A snapshot was removed: parts that used it lose it, later ones move down.
    pub(super) fn snapshot_removed(&mut self, n: usize) {
        for song in &mut self.rig.songs {
            for p in &mut song.parts {
                p.snapshot = match p.snapshot {
                    Some(s) if s == n => None,
                    Some(s) if s > n => Some(s - 1),
                    other => other,
                };
            }
        }
    }

    // ---- moving through the setlist ---------------------------------------------------------

    /// Enters part `part` of `song`: recalls its snapshot (if it has one).
    pub fn go_to(&mut self, song: usize, part: usize) {
        let Some(s) = self.rig.songs.get(song) else {
            return;
        };
        let snapshot = s.parts.get(part).and_then(|p| p.snapshot);
        let label = format!(
            "{} · {}",
            s.label(song),
            s.parts
                .get(part)
                .map_or_else(|| "start".to_owned(), |p| p.label(part))
        );
        self.position = Some((song, part));
        let (tempo, beats) = (s.tempo_in(part), s.beats_per_bar);
        self.set_tempo_for_part(tempo, beats);
        if let Some(n) = snapshot {
            self.recall_snapshot(n);
        }
        self.say(label);
    }

    pub fn next_part(&mut self) {
        let songs = self.rig.songs.len();
        let Some((s, p)) = self.position else {
            if songs > 0 {
                self.go_to(0, 0);
            }
            return;
        };
        let parts = self.rig.songs.get(s).map_or(0, |x| x.parts.len());
        if p + 1 < parts {
            self.go_to(s, p + 1);
        } else if s + 1 < songs {
            self.go_to(s + 1, 0);
        }
    }

    pub fn prev_part(&mut self) {
        let Some((s, p)) = self.position else {
            return;
        };
        if p > 0 {
            self.go_to(s, p - 1);
        } else if s > 0 {
            let last = self.rig.songs[s - 1].parts.len().saturating_sub(1);
            self.go_to(s - 1, last);
        }
    }

    pub fn next_song(&mut self) {
        let next = self.position.map_or(0, |(s, _)| s + 1);
        if next < self.rig.songs.len() {
            self.go_to(next, 0);
        }
    }

    pub fn prev_song(&mut self) {
        if let Some((s, _)) = self.position
            && s > 0
        {
            self.go_to(s - 1, 0);
        }
    }
}
