//! `~/.config/merlin/settings.toml`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// What a MIDI device is for (the setting; `InputRole` is the audio side of it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceRole {
    /// Played: its notes go to the instruments.
    #[default]
    Keyboard,
    /// A control surface: never plays instruments.
    Controller,
}

impl From<DeviceRole> for merlin_audio_io::InputRole {
    fn from(r: DeviceRole) -> Self {
        match r {
            DeviceRole::Keyboard => Self::Keyboard,
            DeviceRole::Controller => Self::Controller,
        }
    }
}

/// What the user decided about a MIDI device, remembered by its name so it holds across plugging
/// it in and out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiDevicePref {
    pub name: String,
    #[serde(default)]
    pub role: DeviceRole,
    /// Off: the device is left unconnected.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

pub use merlin_wine::layout::DEFAULT_RUNTIME_URL;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Extra folders to look for `.clap` plugins (the standard ones are always searched).
    pub plugin_dirs: Vec<PathBuf>,
    /// Extra folders to look for SoundFonts (`.sf2`, `.sf3`); the standard ones are always searched.
    pub sound_dirs: Vec<PathBuf>,
    /// Per-device MIDI choices (role, on or off), by device name.
    pub midi_devices: Vec<MidiDevicePref>,
    /// Audio buffer size asked of PipeWire.
    pub frames: u32,
    /// Most plugin editors kept alive at once.
    pub editor_cache: usize,
    /// Wine and yabridge for Windows plugins; the bundled runtime fills these in later.
    pub wine_loader: Option<PathBuf>,
    pub yabridge_dir: Option<PathBuf>,
    /// Where Windows plugins are installed; the managed prefix when unset.
    pub wine_prefix: Option<PathBuf>,
    /// The manifest of the Wine + yabridge runtime to download; [`DEFAULT_RUNTIME_URL`] when unset.
    pub runtime_url: Option<String>,
    /// Seconds between autosaves; 0 turns autosave off.
    pub autosave_secs: u64,
    /// Browser favourites, as `Session::favorite_key` makes them.
    pub favorites: Vec<String>,
    /// The two audio outputs of the interface the click goes to; the main outputs when unset.
    pub click_output: Option<[String; 2]>,
    /// Folders of Windows VST2 plugins (`.dll`) outside the Windows prefix, to bridge.
    pub windows_vst2_dirs: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            plugin_dirs: Vec::new(),
            sound_dirs: Vec::new(),
            midi_devices: Vec::new(),
            frames: 128,
            editor_cache: 8,
            wine_loader: None,
            yabridge_dir: None,
            wine_prefix: None,
            runtime_url: None,
            autosave_secs: 30,
            favorites: Vec::new(),
            click_output: None,
            windows_vst2_dirs: Vec::new(),
        }
    }
}

impl Settings {
    /// Missing file: defaults. A file that does not parse is an error the caller reports,
    /// because silently ignoring it would hide the user's own configuration.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing_and_partial_files_fill_in() {
        let d = std::env::temp_dir().join(format!("instr-settings-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(
            Settings::load(&d.join("none.toml")).unwrap(),
            Settings::default()
        );

        std::fs::write(d.join("a.toml"), "frames = 64\nplugin_dirs = [\"/x\"]\n").unwrap();
        let s = Settings::load(&d.join("a.toml")).unwrap();
        assert_eq!(s.frames, 64);
        assert_eq!(s.plugin_dirs, vec![PathBuf::from("/x")]);
        assert_eq!(s.editor_cache, 8);

        std::fs::write(d.join("bad.toml"), "frames = [").unwrap();
        assert!(Settings::load(&d.join("bad.toml")).is_err());

        let out = d.join("sub/s.toml");
        s.save(&out).unwrap();
        assert_eq!(Settings::load(&out).unwrap(), s);
    }
}
