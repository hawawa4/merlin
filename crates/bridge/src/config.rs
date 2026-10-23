//! What the tool remembers between runs: `~/.config/merlin/bridge.json`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Folders of Windows plugins (outside the managed prefix) that every `sync` bridges.
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    /// The Wine prefix, when it is not the one the app uses.
    #[serde(default)]
    pub prefix: Option<PathBuf>,
}

impl Config {
    pub fn load(file: &Path) -> Result<Self> {
        match std::fs::read_to_string(file) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("{} is not valid; fix or delete it", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
        }
    }

    pub fn save(&self, file: &Path) -> Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)? + "\n")?;
        std::fs::rename(&tmp, file).with_context(|| format!("cannot write {}", file.display()))
    }

    /// Remembers `folder` (as an absolute path). Returns whether it was new.
    pub fn add_folder(&mut self, folder: PathBuf) -> bool {
        if self.folders.contains(&folder) {
            return false;
        }
        self.folders.push(folder);
        true
    }
}

/// `path` made absolute without resolving links (the folder may be on a disk that is not
/// mounted right now, so it need not exist).
pub fn absolute(path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // Drop `.` and resolve `..` textually.
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    Ok(out)
}
