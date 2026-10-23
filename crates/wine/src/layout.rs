//! Where the Windows plugin support keeps its files, so that the app and `merlin-bridge`
//! agree without sharing a configuration: the XDG base directories, below an `merlin`
//! folder (plan §4.11), and the folders other plugin hosts scan for bridges.

use std::path::{Path, PathBuf};

/// A release named `runtime` on the project's repository holds the current archive and manifest.
/// This is a repo-specific URL: it has to change when the project moves.
pub const DEFAULT_RUNTIME_URL: &str =
    "https://github.com/hawawa4/linuxclap/releases/download/runtime/runtime-manifest.json";

/// The folder inside `~/.clap`, `~/.vst3` and `~/.vst` that holds the bridges made for other
/// plugin hosts. The app's own scan skips it: it makes its own bridges (see
/// `merlin_scanner::DAW_BRIDGES_DIR`, which must be the same name).
pub const DAW_BRIDGES_DIR: &str = "merlin-bridges";

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => home().join(fallback),
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// `~/.config/merlin`.
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("merlin")
}

/// Data that is ours to manage: `~/.local/share/merlin`.
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("merlin")
}

/// The downloaded Wine + yabridge pair.
pub fn runtime_dir() -> PathBuf {
    data_dir().join("runtime")
}

/// The Wine prefix our Windows plugins live in.
pub fn wine_prefix() -> PathBuf {
    data_dir().join("wineprefix")
}

/// The folders bridges for other plugin hosts go in, one per format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DawBridgeDirs {
    pub clap: PathBuf,
    pub vst3: PathBuf,
    pub vst2: PathBuf,
}

impl DawBridgeDirs {
    /// The default places: `~/.clap/merlin-bridges` and so on, which hosts scan.
    pub fn standard() -> Self {
        Self::below(&home())
    }

    /// The same below another home folder.
    pub fn below(home: &Path) -> Self {
        Self {
            clap: home.join(".clap").join(DAW_BRIDGES_DIR),
            vst3: home.join(".vst3").join(DAW_BRIDGES_DIR),
            vst2: home.join(".vst").join(DAW_BRIDGES_DIR),
        }
    }

    pub fn all(&self) -> [&Path; 3] {
        [&self.clap, &self.vst3, &self.vst2]
    }
}
