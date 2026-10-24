//! The MIDI capture on the session: the audio backend always keeps the last minutes of what was
//! played (see `merlin-recorder`); here is the button that saves them as a `.mid` file, and the
//! settings for how long they are kept and where the files go.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use merlin_audio_io::friendly_name;
use merlin_recorder::SmfOptions;

use super::Session;
use crate::paths;

/// How long the "saved" note beside the button stays.
const NOTICE_FOR: Duration = Duration::from_secs(10);
/// The longest window the settings offer. The buffer is capped by size too (see
/// `merlin_recorder::MAX_EVENTS`), so this is about what a person would want, not about memory.
pub const MAX_CAPTURE_MINUTES: u32 = 120;

impl Session {
    /// Minutes of playing kept (0: off).
    pub fn capture_minutes(&self) -> u32 {
        self.settings.capture_minutes
    }

    pub fn set_capture_minutes(&mut self, minutes: u32) {
        let minutes = minutes.min(MAX_CAPTURE_MINUTES);
        self.settings.capture_minutes = minutes;
        self.apply_capture_window();
        self.save_settings();
    }

    /// Tells the recorder the window the settings ask for.
    pub(super) fn apply_capture_window(&self) {
        if let Some(b) = self.backend() {
            b.recorder.set_window(Duration::from_secs(
                u64::from(self.settings.capture_minutes) * 60,
            ));
        }
    }

    /// Where the files go: the folder in the settings, else `~/Music/Merlin`.
    pub fn capture_dir(&self) -> PathBuf {
        self.settings
            .capture_dir
            .clone()
            .unwrap_or_else(paths::default_capture_dir)
    }

    /// The folder of backing tracks the Setlist view browses, if one was chosen.
    pub fn backing_dir(&self) -> Option<&Path> {
        self.settings.backing_dir.as_deref()
    }

    pub fn set_backing_dir(&mut self, dir: Option<PathBuf>) {
        self.settings.backing_dir = dir.filter(|d| !d.as_os_str().is_empty());
        self.save_settings();
    }

    pub fn set_capture_dir(&mut self, dir: Option<PathBuf>) {
        self.settings.capture_dir = dir.filter(|d| !d.as_os_str().is_empty());
        self.save_settings();
    }

    /// How long the app has been listening (up to the window), and how many messages it holds.
    pub fn capture_status(&self) -> (Duration, usize) {
        self.backend().map_or((Duration::ZERO, 0), |b| {
            (b.recorder.listened(), b.recorder.stored())
        })
    }

    /// What the button last did, while it is news.
    pub fn capture_notice(&self) -> Option<&str> {
        self.capture_notice
            .as_ref()
            .filter(|(at, _)| at.elapsed() < NOTICE_FOR)
            .map(|(_, text)| text.as_str())
    }

    /// Saves the last minutes of what was played, and says where.
    pub fn capture_midi(&mut self) {
        let text = match self.write_capture() {
            Ok(text) => text,
            Err(e) => format!("MIDI capture failed: {e:#}"),
        };
        self.say(text.clone());
        self.capture_notice = Some((Instant::now(), text));
    }

    fn write_capture(&mut self) -> Result<String> {
        let backend = self.backend().context("audio is not running")?;
        let window = backend.recorder.window();
        if window.is_zero() {
            return Ok("MIDI capture is off (Settings > MIDI)".into());
        }
        let capture = backend.recorder.capture();
        if capture.is_empty() {
            return Ok(format!(
                "nothing to save: no MIDI was played in the last {}",
                clock(backend.recorder.listened())
            ));
        }
        let options = SmfOptions {
            tempo: self.tempo(),
            beats_per_bar: self.rig.transport.beats_per_bar,
            title: format!("Merlin capture · {}", self.rig.name),
            input_names: self.capture_input_names(),
        };
        let bytes = capture.to_smf(&options);
        let dir = self.capture_dir();
        let path = save_unique(&dir, &format!("Merlin capture {}", stamp()), "mid", &bytes)?;
        Ok(format!(
            "saved {} of MIDI to {}",
            clock(capture.duration()),
            path.display()
        ))
    }

    /// Names for the tracks: the on-screen keyboard, and each device that is connected now.
    fn capture_input_names(&self) -> Vec<String> {
        let mut names = vec!["On-screen keyboard".to_owned()];
        for source in &self.midi_sources {
            let i = source.input as usize;
            if names.len() <= i {
                names.resize(i + 1, String::new());
            }
            names[i] = friendly_name(&source.device);
        }
        names
    }
}

/// `12:31` for 751 seconds; `1:02:03` past an hour.
pub fn clock(d: Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The local date and time, safe in a file name: `2026-10-23 14-05-33`.
fn stamp() -> String {
    // SAFETY: `time` takes a null pointer; `localtime_r` fills the struct we give it.
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    format!(
        "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Writes `bytes` to `dir/stem.ext`, or `stem (2).ext` and on if that is taken: a capture is
/// never written over another. The file appears whole or not at all.
fn save_unique(dir: &Path, stem: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = dir.join(format!(".{stem}.{ext}.partial"));
    std::fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    for n in 1.. {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let path = dir.join(name);
        // A hard link fails when the name exists, which `rename` would not: no race, no overwrite.
        match std::fs::hard_link(&tmp, &path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                // A file system without hard links: fall back to a check and a rename.
                if !path.exists() {
                    std::fs::rename(&tmp, &path)
                        .with_context(|| format!("cannot write {}", path.display()))?;
                    return Ok(path);
                }
            }
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_a_clock() {
        assert_eq!(clock(Duration::from_secs(5)), "0:05");
        assert_eq!(clock(Duration::from_secs(751)), "12:31");
        assert_eq!(clock(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn a_second_capture_never_overwrites_the_first() {
        let d = std::env::temp_dir().join(format!("merlin-capture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let a = save_unique(&d, "Merlin capture x", "mid", b"one").unwrap();
        let b = save_unique(&d, "Merlin capture x", "mid", b"two").unwrap();
        assert_eq!(a.file_name().unwrap(), "Merlin capture x.mid");
        assert_eq!(b.file_name().unwrap(), "Merlin capture x (2).mid");
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(std::fs::read(&b).unwrap(), b"two");
        // Only the captures are left in the folder.
        let left: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().collect();
        assert_eq!(left.len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_stamp_is_safe_in_a_file_name() {
        let s = stamp();
        assert_eq!(s.len(), 19, "{s}");
        assert!(!s.contains(['/', ':']));
    }
}
