//! The saved data model: rigs, slots and their MIDI filters. Plain serde types, no behaviour
//! beyond defaults and validation, and no dependency on audio or UI code.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Most slots a rig can have. The engine's note-ownership masks are `u16`.
pub const MAX_SLOTS: usize = 16;

/// Current `Rig` file format version.
pub const RIG_VERSION: u32 = 1;

mod fx;
mod macros;
mod mapping;
mod migrate;
mod setlist;
mod snapshot;
mod transport;

pub use fx::{CompSettings, DelaySettings, EqSettings, FxSettings, ReverbSettings};
pub use macros::{MACROS_PER_SLOT, MAX_MACRO_TARGETS, Macro, MacroTarget, SamplerField};
pub use mapping::{ControlAction, Curve, Glue, Mapping, Relative, Source, SourceKind, Takeover};
pub use migrate::{Migration, migrate};
pub use setlist::{
    MAX_PARTS, MAX_SONG_MARKERS, MAX_SONGS, MAX_TEMPO, MAX_TRACKS, MIN_TEMPO, Marker, MarkerAction,
    Part, Song, Track,
};
pub use snapshot::{MAX_SNAPSHOTS, PresetChoice, SlotSnapshot, Snapshot};
pub use transport::{MAX_COUNT_IN_BARS, TransportSettings};

mod base64_opt {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => s.serialize_str(&STANDARD.encode(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|t| STANDARD.decode(t).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Clap,
    Vst3,
    /// An instrument or effect that ships inside the app.
    Builtin,
    SoundFont,
}

/// Enough to find a plugin again after a rescan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRef {
    pub format: Format,
    /// CLAP plugin id, VST3 class id (hex), or the built-in's name.
    pub id: String,
    pub name: String,
    /// Where it was last found; the id is what identifies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Runs through Wine + yabridge.
    #[serde(default)]
    pub windows: bool,
}

/// Plugin effects per slot, between the built-in EQ/compressor and the delay/reverb.
pub const MAX_INSERTS: usize = 4;

/// One insert effect: the plugin, its saved state, and whether it is bypassed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Insert {
    pub plugin: PluginRef,
    #[serde(default, with = "base64_opt", skip_serializing_if = "Option::is_none")]
    pub state: Option<Vec<u8>>,
    #[serde(default)]
    pub bypass: bool,
}

/// What a slot listens to (plan §4.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiFilter {
    /// Input device name; `None` listens to every input.
    pub input: Option<String>,
    /// MIDI channel 0-15; `None` listens to all.
    pub channel: Option<u8>,
    pub key_lo: u8,
    pub key_hi: u8,
    pub vel_lo: u8,
    pub vel_hi: u8,
    /// Semitones added to incoming notes.
    pub transpose: i8,
}

impl Default for MidiFilter {
    fn default() -> Self {
        Self {
            input: None,
            channel: None,
            key_lo: 0,
            key_hi: 127,
            vel_lo: 1,
            vel_hi: 127,
            transpose: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Slot {
    pub name: String,
    /// Faceplate colour, RGB.
    pub colour: [u8; 3],
    pub instrument: Option<PluginRef>,
    /// The instrument's saved state, an opaque blob (base64 in the file).
    #[serde(default, with = "base64_opt", skip_serializing_if = "Option::is_none")]
    pub state: Option<Vec<u8>>,
    /// Linear gain, 1.0 = unity.
    pub volume: f32,
    /// -1 (left) to 1 (right).
    pub pan: f32,
    pub mute: bool,
    pub filter: MidiFilter,
    /// Receives MIDI when selected.
    pub armed: bool,
    /// The slot's built-in effects (plan §4.3): EQ, compressor, delay, reverb.
    pub fx: FxSettings,
    /// Up to [`MAX_INSERTS`] effect plugins; position in the list is the insert's position.
    /// A `None` entry is an empty insert position.
    pub inserts: Vec<Option<Insert>>,
    /// Up to [`MACROS_PER_SLOT`] macros; a shorter list means the rest are untouched.
    pub macros: Vec<Macro>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            name: String::new(),
            colour: [0x5a, 0xa9, 0xe0],
            instrument: None,
            state: None,
            volume: 1.0,
            pan: 0.0,
            mute: false,
            filter: MidiFilter::default(),
            armed: true,
            fx: FxSettings::default(),
            inserts: Vec::new(),
            macros: Vec::new(),
        }
    }
}

/// How selecting a slot treats the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectMode {
    /// The selected slot becomes the only keyboard target.
    #[default]
    Exclusive,
    /// Selecting toggles the slot in the set of targets.
    Layer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rig {
    pub version: u32,
    pub name: String,
    pub slots: Vec<Slot>,
    /// Slots that are keyboard targets when the rig loads.
    pub selected: Vec<usize>,
    /// Does a slot that gains focus inherit a pedal that is already down? (plan §4.4)
    pub inherit_pedal: bool,
    pub master_volume: f32,
    /// What MIDI controls do in this rig.
    pub mappings: Vec<Mapping>,
    /// Saved performance states (which slots play, volumes, effects on or off, presets).
    pub snapshots: Vec<Snapshot>,
    /// The setlist: songs in playing order.
    pub songs: Vec<Song>,
    pub transport: TransportSettings,
}

impl Default for Rig {
    fn default() -> Self {
        Self {
            version: RIG_VERSION,
            name: "Untitled".into(),
            slots: Vec::new(),
            selected: Vec::new(),
            inherit_pedal: false,
            master_volume: 1.0,
            mappings: Vec::new(),
            snapshots: Vec::new(),
            songs: Vec::new(),
            transport: TransportSettings::default(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RigError {
    TooManySlots(usize),
    BadSelection(usize),
    BadFilter(usize),
    TooManyInserts(usize),
    BadMacro(usize),
    BadMapping(usize),
    BadSnapshot(usize),
    BadSong(usize),
    BadTransport,
    NewerVersion(u32),
    /// No migration path from this format version.
    Unmigratable(u32),
    /// Not a rig file.
    Parse(String),
}

impl std::fmt::Display for RigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManySlots(n) => write!(f, "{n} slots (at most {MAX_SLOTS})"),
            Self::BadSelection(i) => write!(f, "selected slot {i} does not exist"),
            Self::BadFilter(i) => write!(f, "slot {} has an invalid MIDI filter", i + 1),
            Self::TooManyInserts(i) => {
                write!(f, "slot {} has more than {MAX_INSERTS} inserts", i + 1)
            }
            Self::BadMacro(i) => write!(f, "slot {} has an invalid macro", i + 1),
            Self::BadMapping(i) => write!(f, "mapping {} is not valid", i + 1),
            Self::BadTransport => write!(f, "invalid tempo or click settings"),
            Self::BadSong(i) => write!(f, "song {} is not valid", i + 1),
            Self::BadSnapshot(i) => write!(f, "snapshot {} is not valid", i + 1),
            Self::NewerVersion(v) => write!(f, "rig format {v} is newer than this app understands"),
            Self::Unmigratable(v) => write!(f, "cannot upgrade rig format {v}"),
            Self::Parse(e) => write!(f, "not a valid rig file: {e}"),
        }
    }
}

impl std::error::Error for RigError {}

impl Rig {
    /// Checks what the engine relies on. Call after loading a file.
    pub fn validate(&self) -> Result<(), RigError> {
        if self.version > RIG_VERSION {
            return Err(RigError::NewerVersion(self.version));
        }
        if self.slots.len() > MAX_SLOTS {
            return Err(RigError::TooManySlots(self.slots.len()));
        }
        if let Some(i) = self.mappings.iter().position(|m| !m.is_valid()) {
            return Err(RigError::BadMapping(i));
        }
        if self.snapshots.len() > MAX_SNAPSHOTS {
            return Err(RigError::BadSnapshot(MAX_SNAPSHOTS));
        }
        if let Some(i) = self.snapshots.iter().position(|s| !s.is_valid()) {
            return Err(RigError::BadSnapshot(i));
        }
        if !self.transport.is_valid() {
            return Err(RigError::BadTransport);
        }
        if self.songs.len() > MAX_SONGS {
            return Err(RigError::BadSong(MAX_SONGS));
        }
        let snapshots = self.snapshots.len();
        if let Some(i) = self.songs.iter().position(|s| !s.is_valid(snapshots)) {
            return Err(RigError::BadSong(i));
        }
        if let Some(&i) = self.selected.iter().find(|&&i| i >= self.slots.len()) {
            return Err(RigError::BadSelection(i));
        }
        for (i, s) in self.slots.iter().enumerate() {
            if s.inserts.len() > MAX_INSERTS {
                return Err(RigError::TooManyInserts(i));
            }
            if s.macros.len() > MACROS_PER_SLOT || !s.macros.iter().all(Macro::is_valid) {
                return Err(RigError::BadMacro(i));
            }
            let f = &s.filter;
            let ok = f.key_lo <= f.key_hi
                && f.key_hi <= 127
                && f.vel_lo <= f.vel_hi
                && f.vel_hi <= 127
                && f.channel.is_none_or(|c| c < 16);
            if !ok {
                return Err(RigError::BadFilter(i));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Rig {
        Rig {
            name: "Sunday gig".into(),
            slots: vec![
                Slot {
                    name: "Grand piano".into(),
                    instrument: Some(PluginRef {
                        format: Format::Clap,
                        id: "org.surge-synth-team.surge-xt".into(),
                        name: "Surge XT".into(),
                        path: Some("/usr/lib/clap/Surge XT.clap".into()),
                        windows: false,
                    }),
                    filter: MidiFilter {
                        key_lo: 36,
                        key_hi: 59,
                        transpose: -12,
                        ..MidiFilter::default()
                    },
                    ..Slot::default()
                },
                Slot::default(),
            ],
            selected: vec![0],
            ..Rig::default()
        }
    }

    #[test]
    fn json_round_trip() {
        let rig = sample();
        let json = serde_json::to_string_pretty(&rig).unwrap();
        let back: Rig = serde_json::from_str(&json).unwrap();
        assert_eq!(rig, back);
    }

    #[test]
    fn plugin_state_is_base64_in_the_file() {
        let mut rig = sample();
        rig.slots[0].state = Some(vec![0, 1, 2, 250, 255]);
        let json = serde_json::to_string(&rig).unwrap();
        assert!(json.contains("\"AAEC+v8=\""), "{json}");
        let back: Rig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.slots[0].state, Some(vec![0, 1, 2, 250, 255]));
        assert!(serde_json::from_str::<Rig>(r#"{"slots":[{"state":"not base64!"}]}"#).is_err());
    }

    #[test]
    fn missing_fields_take_defaults() {
        // Files written by older or newer versions must still load.
        let rig: Rig = serde_json::from_str(r#"{"name":"x","slots":[{"name":"a"}]}"#).unwrap();
        assert_eq!(rig.slots[0].volume, 1.0);
        assert_eq!(rig.slots[0].filter, MidiFilter::default());
        assert_eq!(rig.validate(), Ok(()));
    }

    #[test]
    fn validation_catches_bad_rigs() {
        let mut rig = sample();
        rig.selected = vec![5];
        assert_eq!(rig.validate(), Err(RigError::BadSelection(5)));

        let mut rig = sample();
        rig.slots[1].filter.key_lo = 100;
        rig.slots[1].filter.key_hi = 50;
        assert_eq!(rig.validate(), Err(RigError::BadFilter(1)));

        let mut rig = sample();
        rig.slots.resize(MAX_SLOTS + 1, Slot::default());
        assert_eq!(rig.validate(), Err(RigError::TooManySlots(MAX_SLOTS + 1)));

        let mut rig = sample();
        rig.version = RIG_VERSION + 1;
        assert_eq!(rig.validate(), Err(RigError::NewerVersion(RIG_VERSION + 1)));
    }
}
