//! Crash marker for plugin loading (plan: crash marker → safe mode).
//!
//! Loading a plugin runs foreign code in our process. If that code takes the whole app down (a
//! segfault in a CLAP or VST3 plugin), the next start would restore the same autosave, load the
//! same plugin, and crash again. So while a plugin loads, a marker file names it; a normal
//! return removes the marker, and a marker found at startup means the app died mid-load: that
//! plugin goes on the quarantine list and the rest of the rig comes back without it.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use merlin_scanner::{Quarantine, Quarantined};

const FILE: &str = "loading.json";

/// Held while a plugin loads; dropping it (the load returned, well or badly) clears the marker.
pub struct Loading {
    file: PathBuf,
}

impl Loading {
    /// Writes the marker for a plugin about to load, in `dir`.
    pub fn begin_in(dir: &Path, path: &Path, id: &str, name: &str) -> Self {
        let file = dir.join(FILE);
        let entry = Quarantined {
            path: path.to_owned(),
            id: id.to_owned(),
            name: name.to_owned(),
            reason: "the app crashed while loading it".into(),
            when: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        };
        let _ = std::fs::create_dir_all(dir);
        if let Ok(json) = serde_json::to_vec(&entry) {
            let _ = std::fs::write(&file, json);
        }
        Self { file }
    }

    pub fn begin(path: &Path, id: &str, name: &str) -> Self {
        Self::begin_in(&crate::paths::state_dir(), path, id, name)
    }
}

impl Drop for Loading {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
    }
}

/// A marker left behind by a previous run means the app died while that plugin was loading.
/// Removes the marker and returns the culprit.
pub fn take_interrupted_load_in(dir: &Path) -> Option<Quarantined> {
    let file = dir.join(FILE);
    let found = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let _ = std::fs::remove_file(&file);
    found
}

/// Startup check: quarantines the plugin whose load killed the last run. Returns its name.
pub fn quarantine_interrupted_load() -> Option<String> {
    let culprit = take_interrupted_load_in(&crate::paths::state_dir())?;
    let file = crate::paths::quarantine_file();
    let mut q = Quarantine::load(&file);
    let name = culprit.name.clone();
    q.add(culprit);
    let _ = q.save(&file);
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("irig-marker-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_finished_load_leaves_nothing_behind() {
        let dir = temp("ok");
        let m = Loading::begin_in(&dir, Path::new("/p/a.clap"), "id.a", "A");
        assert!(dir.join(FILE).exists());
        drop(m);
        assert!(take_interrupted_load_in(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_marker_left_by_a_dead_process_names_the_culprit_once() {
        let dir = temp("crash");
        // The process died mid-load: the guard's Drop never ran.
        std::mem::forget(Loading::begin_in(&dir, Path::new("/p/b.vst3"), "id.b", "B"));
        let c = take_interrupted_load_in(&dir).expect("marker");
        assert_eq!((c.name.as_str(), c.id.as_str()), ("B", "id.b"));
        assert_eq!(c.path, Path::new("/p/b.vst3"));
        assert!(
            take_interrupted_load_in(&dir).is_none(),
            "the marker is consumed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_is_ignored() {
        let dir = temp("junk");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), b"{not json").unwrap();
        assert!(take_interrupted_load_in(&dir).is_none());
        assert!(!dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
