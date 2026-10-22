//! Where the app keeps its files (plan §4.11), following the XDG base directories.

use std::path::PathBuf;

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

/// Settings: `~/.config/merlin`.
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("merlin")
}

/// Plugin cache: `~/.cache/merlin`.
pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("merlin")
}

/// Autosave and crash markers: `~/.local/state/merlin`.
pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("merlin")
}

/// Rigs: `~/.local/share/merlin/rigs`.
pub fn rigs_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
        .join("merlin")
        .join("rigs")
}

/// Data that is ours to manage: `~/.local/share/merlin`.
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("merlin")
}

/// The downloaded Wine + yabridge pair ("Windows plugin support").
pub fn runtime_dir() -> PathBuf {
    data_dir().join("runtime")
}

/// The Wine prefix our Windows plugins live in.
pub fn wine_prefix() -> PathBuf {
    data_dir().join("wineprefix")
}

/// Bridges (yabridge chainloader copies) for the Windows CLAP plugins in that prefix.
pub fn bridges_dir() -> PathBuf {
    data_dir().join("bridges").join("clap")
}

/// The same for Windows VST3 plugins (merged bundles).
pub fn bridges_vst3_dir() -> PathBuf {
    data_dir().join("bridges").join("vst3")
}

pub fn settings_file() -> PathBuf {
    config_dir().join("settings.toml")
}

pub fn plugin_cache_file() -> PathBuf {
    cache_dir().join("plugins.json")
}

pub fn quarantine_file() -> PathBuf {
    cache_dir().join("quarantine.json")
}

pub fn autosave_file() -> PathBuf {
    state_dir().join("autosave.irig")
}

/// A file-system safe name for a rig typed by the user.
pub fn rig_file_name(name: &str) -> String {
    let clean: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_matches('.').trim();
    format!("{}.irig", if clean.is_empty() { "Untitled" } else { clean })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rig_names_become_safe_file_names() {
        assert_eq!(rig_file_name("Sunday gig"), "Sunday gig.irig");
        assert_eq!(rig_file_name("a/b\\c:d"), "a_b_c_d.irig");
        assert_eq!(rig_file_name("  "), "Untitled.irig");
        assert_eq!(rig_file_name("../../etc"), "_.._etc.irig");
        assert!(!rig_file_name("../x").contains('/'));
    }
}
