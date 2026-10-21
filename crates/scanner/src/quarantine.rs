//! Plugins that crashed or hung us (plan §4.7): remembered across restarts so a rig that
//! contains one still opens. A quarantined plugin is not loaded automatically; the user can
//! retry it, which removes it from the list.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quarantined {
    pub path: PathBuf,
    pub id: String,
    pub name: String,
    pub reason: String,
    /// Seconds since the epoch.
    pub when: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quarantine {
    entries: Vec<Quarantined>,
}

impl Quarantine {
    /// A missing or damaged file is an empty list.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Adds an entry, replacing an older one for the same plugin.
    pub fn add(&mut self, entry: Quarantined) {
        self.entries
            .retain(|e| !(e.id == entry.id && e.path == entry.path));
        self.entries.push(entry);
    }

    pub fn get(&self, path: &Path, id: &str) -> Option<&Quarantined> {
        self.entries.iter().find(|e| e.id == id && e.path == path)
    }

    /// Takes a plugin off the list (the "retry" button). Returns whether it was on it.
    pub fn remove(&mut self, path: &Path, id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| !(e.id == id && e.path == path));
        self.entries.len() != before
    }

    pub fn entries(&self) -> &[Quarantined] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str, reason: &str) -> Quarantined {
        Quarantined {
            path: format!("/p/{id}.clap").into(),
            id: id.into(),
            name: id.to_uppercase(),
            reason: reason.into(),
            when: 1,
        }
    }

    #[test]
    fn add_replace_remove_and_persist() {
        let mut list = Quarantine::default();
        list.add(q("a", "first"));
        list.add(q("b", "x"));
        list.add(q("a", "second"));
        assert_eq!(list.entries().len(), 2);
        assert_eq!(
            list.get(Path::new("/p/a.clap"), "a").unwrap().reason,
            "second"
        );
        assert!(
            list.get(Path::new("/other/a.clap"), "a").is_none(),
            "same id elsewhere is another file"
        );

        let dir = std::env::temp_dir().join(format!("instr-quarantine-{}", std::process::id()));
        let file = dir.join("q.json");
        list.save(&file).unwrap();
        let mut back = Quarantine::load(&file);
        assert_eq!(back, list);
        assert!(back.remove(Path::new("/p/b.clap"), "b"));
        assert!(!back.remove(Path::new("/p/b.clap"), "b"));
        assert_eq!(back.entries().len(), 1);

        std::fs::write(&file, "garbage").unwrap();
        assert_eq!(Quarantine::load(&file), Quarantine::default());
    }
}
