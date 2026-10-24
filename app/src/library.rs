//! The backing-track library: a plain folder browser for the Setlist view. It lists the folders
//! and the audio files of one directory at a time, and nothing more (no tags, no database).

use std::path::{Path, PathBuf};

/// The audio formats a backing track can be (what the backing decoder reads).
pub const AUDIO: &[&str] = &["wav", "flac", "mp3", "ogg", "m4a"];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO.iter().any(|a| e.eq_ignore_ascii_case(a)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// The folders (first) and audio files of `dir`, each sorted by name ignoring case. Hidden
/// entries and other kinds of file are left out.
pub fn list(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            // `metadata` follows links, so a link to a folder or a file works.
            let meta = std::fs::metadata(&path).ok()?;
            let dir = meta.is_dir();
            (dir || is_audio(&path)).then_some(Entry {
                path,
                name,
                dir,
                size: meta.len(),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// `3.4 MB`.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// What the Setlist view remembers about the folder it is showing.
#[derive(Default)]
pub struct Browsing {
    /// The folder being shown (inside the library's root); the root when `None`.
    pub dir: Option<PathBuf>,
    /// Its entries, read when the folder changes or on Refresh.
    pub entries: Vec<Entry>,
    pub listed: Option<PathBuf>,
    pub filter: String,
}

impl Browsing {
    /// The folder to show for this root: where we are if that is still inside it, else the root.
    pub fn current(&mut self, root: &Path) -> PathBuf {
        let dir = self
            .dir
            .clone()
            .filter(|d| d.starts_with(root) && d.is_dir())
            .unwrap_or_else(|| root.to_owned());
        self.dir = Some(dir.clone());
        dir
    }

    /// Reads the folder if it is not the one listed (or when `force`).
    pub fn load(&mut self, dir: &Path, force: bool) {
        if force || self.listed.as_deref() != Some(dir) {
            self.entries = list(dir);
            self.listed = Some(dir.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("merlin-lib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn folders_come_first_then_audio_by_name_and_nothing_else_shows() {
        let d = temp("list");
        std::fs::create_dir(d.join("Zebra")).unwrap();
        std::fs::create_dir(d.join("apple")).unwrap();
        std::fs::create_dir(d.join(".hidden")).unwrap();
        for f in ["b.WAV", "A.flac", "notes.txt", "song.mp3", ".secret.wav"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let names: Vec<_> = list(&d).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["apple", "Zebra", "A.flac", "b.WAV", "song.mp3"]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn browsing_stays_inside_the_root() {
        let root = temp("root");
        std::fs::create_dir(root.join("sub")).unwrap();
        let mut b = Browsing::default();
        assert_eq!(b.current(&root), root);
        b.dir = Some(root.join("sub"));
        assert_eq!(b.current(&root), root.join("sub"));
        b.dir = Some(std::env::temp_dir());
        assert_eq!(b.current(&root), root, "outside the root: back to it");
        b.dir = Some(root.join("gone"));
        assert_eq!(
            b.current(&root),
            root,
            "a folder that vanished: back to the root"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(size_text(12), "12 B");
        assert_eq!(size_text(3_400_000), "3.4 MB");
    }
}
