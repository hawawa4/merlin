//! `~/.config/merlin/settings.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::tags::Tag;

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
    /// Extra folders of plugins of any kind (CLAP, VST3, VST2, LV2, Linux or Windows); the
    /// standard ones are always searched.
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
    /// What the user tagged each plugin as (family and instrument), by the same key as favourites.
    /// A plugin with an entry here, even an empty one, is the user's to tag and the scanner's
    /// automatic tags are not used for it; plugins without one are tagged by the scanner.
    pub tags: BTreeMap<String, Vec<Tag>>,
    /// The two audio outputs of the interface the main mix goes to, as JACK port names; the
    /// system's default output when unset, or when that interface is not there.
    pub main_output: Option<[String; 2]>,
    /// The two audio outputs of the interface the click goes to; the main outputs when unset.
    pub click_output: Option<[String; 2]>,
    /// Older versions kept Windows VST2 folders apart; they are folded into `plugin_dirs` when
    /// the settings load and never written again.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub windows_vst2_dirs: Vec<PathBuf>,
    /// Plugin files the user told the scanner to leave alone (broken ones): never loaded,
    /// never listed, never reported as failed.
    pub ignored_files: Vec<PathBuf>,
    /// Plugins the user hid from the browser, by the same key as favourites. They are still
    /// scanned, so a rig that uses one keeps loading.
    pub hidden_plugins: Vec<String>,
    /// Minutes of MIDI the capture keeps, always (0: off).
    pub capture_minutes: u32,
    /// Where captures are saved; `~/Music/Merlin` when unset.
    pub capture_dir: Option<PathBuf>,
    /// The folder of backing tracks the Setlist view browses; none until the user picks one.
    pub backing_dir: Option<PathBuf>,
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
            tags: BTreeMap::new(),
            main_output: None,
            click_output: None,
            windows_vst2_dirs: Vec::new(),
            ignored_files: Vec::new(),
            hidden_plugins: Vec::new(),
            capture_minutes: 30,
            capture_dir: None,
            backing_dir: None,
        }
    }
}

impl Settings {
    /// Missing file: defaults. A file that does not parse is an error the caller reports,
    /// because silently ignoring it would hide the user's own configuration.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut s: Self = toml::from_str(&text)?;
                for d in std::mem::take(&mut s.windows_vst2_dirs) {
                    if !s.plugin_dirs.contains(&d) {
                        s.plugin_dirs.push(d);
                    }
                }
                Ok(s)
            }
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

        std::fs::write(
            d.join("old.toml"),
            "plugin_dirs = [\"/x\"]\nwindows_vst2_dirs = [\"/x\", \"/y\"]\n",
        )
        .unwrap();
        let old = Settings::load(&d.join("old.toml")).unwrap();
        assert_eq!(
            old.plugin_dirs,
            vec![PathBuf::from("/x"), PathBuf::from("/y")]
        );
        assert!(old.windows_vst2_dirs.is_empty());

        std::fs::write(d.join("bad.toml"), "frames = [").unwrap();
        assert!(Settings::load(&d.join("bad.toml")).is_err());

        let out = d.join("sub/s.toml");
        s.save(&out).unwrap();
        assert_eq!(Settings::load(&out).unwrap(), s);
    }

    #[test]
    fn tags_survive_the_settings_file_and_old_files_have_none() {
        let mut s = Settings::default();
        s.tags.insert(
            "clap:com.example.keys".into(),
            vec![Tag::new("Keys", Some("Piano")), Tag::new("Synth", None)],
        );
        // "No tags" is a choice the scanner must respect, so an empty list is kept in the file.
        s.tags.insert("clap:com.example.none".into(), Vec::new());
        // Settings written after the tags: the file must stay valid TOML.
        s.frames = 64;
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back, s, "{text}");
        assert!(
            toml::from_str::<Settings>("frames = 64\n")
                .unwrap()
                .tags
                .is_empty()
        );
    }

    #[test]
    fn the_chosen_outputs_survive_the_file_and_default_to_automatic() {
        let mut s = Settings::default();
        s.tags.insert("clap:a".into(), vec![Tag::new("Keys", None)]);
        s.main_output = Some([
            "Scarlett 2i2:playback_AUX0".into(),
            "Scarlett 2i2:playback_AUX1".into(),
        ]);
        s.click_output = Some(["Onboard:playback_FL".into(), "Onboard:playback_FR".into()]);
        // The values sit after the `tags` table in the struct: the file must stay valid TOML.
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back, s, "{text}");
        // A file from before this setting: the safe default, the first outputs found.
        assert_eq!(
            toml::from_str::<Settings>("frames = 64\n")
                .unwrap()
                .main_output,
            None
        );
    }
}
