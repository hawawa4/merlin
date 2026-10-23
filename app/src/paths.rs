//! Where the app keeps its files (plan §4.11), following the XDG base directories.

use std::path::{Path, PathBuf};

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

/// Settings: `~/.config/merlin`.
pub fn config_dir() -> PathBuf {
    merlin_wine::layout::config_dir()
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
    merlin_wine::layout::data_dir()
}

/// The downloaded Wine + yabridge pair ("Windows plugin support").
pub fn runtime_dir() -> PathBuf {
    merlin_wine::layout::runtime_dir()
}

/// The Wine prefix our Windows plugins live in.
pub fn wine_prefix() -> PathBuf {
    merlin_wine::layout::wine_prefix()
}

/// Bridges (yabridge chainloader copies) for the Windows CLAP plugins in that prefix.
pub fn bridges_dir() -> PathBuf {
    data_dir().join("bridges").join("clap")
}

/// The same for Windows VST3 plugins (merged bundles).
pub fn bridges_vst3_dir() -> PathBuf {
    data_dir().join("bridges").join("vst3")
}

/// The same for Windows VST2 plugins (a chainloader `.so` with the `.dll` linked beside it).
pub fn bridges_vst2_dir() -> PathBuf {
    data_dir().join("bridges").join("vst2")
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
    state_dir().join("autosave.mrig")
}

/// Where MIDI captures go unless the settings say otherwise: `~/Music/Merlin`.
pub fn default_capture_dir() -> PathBuf {
    xdg("XDG_MUSIC_DIR", "Music").join("Merlin")
}

/// The extension of a rig file.
pub const RIG_EXT: &str = "mrig";
/// Rig files were `.irig` before the project was called Merlin; they still open.
const LEGACY_RIG_EXT: &str = "irig";

/// Is this a rig file (by its extension, whatever the case)?
pub fn is_rig_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| e == RIG_EXT || e == LEGACY_RIG_EXT)
}

/// Renames the `.irig` files (and their `.irig.bak` backups) in the rigs folder and the
/// autosave folder to `.mrig`, so rigs saved before the rename show up in the list.
pub fn migrate_legacy_rigs() {
    migrate_legacy_rigs_in(&[rigs_dir(), state_dir()]);
}

/// Never overwrites a file that is already there.
fn migrate_legacy_rigs_in(dirs: &[PathBuf]) {
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for old in rd.flatten().map(|e| e.path()) {
            let Some(name) = old.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            let new_name = if let Some(stem) = name.strip_suffix(".irig") {
                format!("{stem}.{RIG_EXT}")
            } else if let Some(stem) = name.strip_suffix(".irig.bak") {
                format!("{stem}.{RIG_EXT}.bak")
            } else {
                continue;
            };
            let new = dir.join(new_name);
            if !new.exists() {
                let _ = std::fs::rename(&old, &new);
            }
        }
    }
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
    format!(
        "{}.{RIG_EXT}",
        if clean.is_empty() { "Untitled" } else { clean }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_skips_the_folder_the_bridge_tool_writes() {
        assert_eq!(
            merlin_scanner::DAW_BRIDGES_DIR,
            merlin_wine::layout::DAW_BRIDGES_DIR
        );
    }

    #[test]
    fn old_rig_files_are_renamed_and_never_overwrite_new_ones() {
        let d = std::env::temp_dir().join(format!("merlin-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (name, text) in [
            ("a.irig", "old a"),
            ("a.irig.bak", "old a backup"),
            ("b.irig", "old b"),
            ("b.mrig", "new b"),
            ("c.sf2", "not a rig"),
        ] {
            std::fs::write(d.join(name), text).unwrap();
        }
        migrate_legacy_rigs_in(&[d.clone(), d.join("missing")]);
        let read = |n: &str| std::fs::read_to_string(d.join(n)).ok();
        assert_eq!(read("a.mrig").as_deref(), Some("old a"));
        assert_eq!(read("a.mrig.bak").as_deref(), Some("old a backup"));
        assert!(!d.join("a.irig").exists());
        assert_eq!(read("b.mrig").as_deref(), Some("new b"), "a new file wins");
        assert_eq!(
            read("b.irig").as_deref(),
            Some("old b"),
            "and the old one stays"
        );
        assert_eq!(read("c.sf2").as_deref(), Some("not a rig"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn both_extensions_are_rig_files() {
        for ok in ["x.mrig", "x.irig", "X.MRIG", "/a/b/My rig.mrig"] {
            assert!(is_rig_file(Path::new(ok)), "{ok}");
        }
        for no in ["x.sf2", "x.mrig.bak", "mrig", "x.rig"] {
            assert!(!is_rig_file(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn rig_names_become_safe_file_names() {
        assert_eq!(rig_file_name("Sunday gig"), "Sunday gig.mrig");
        assert_eq!(rig_file_name("a/b\\c:d"), "a_b_c_d.mrig");
        assert_eq!(rig_file_name("  "), "Untitled.mrig");
        assert_eq!(rig_file_name("../../etc"), "_.._etc.mrig");
        assert!(!rig_file_name("../x").contains('/'));
    }
}
