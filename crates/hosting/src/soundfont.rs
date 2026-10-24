//! SoundFont (`.sf2` / `.sf3`) instruments, played by oxisynth (plan §4.9).
//!
//! A SoundFont is a file, not a plugin, but the rest of the app treats it as one: it has a
//! [`PluginInfo`], a [`PluginHandle`] on the main thread and a [`PluginProcessor`] on the audio
//! thread. There is no editor; the slot shows a preset picker instead.
//!
//! Decoding a file is slow (an SF3 takes seconds), so decoded fonts are cached by path and
//! shared: a second slot with the same file costs almost nothing.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use merlin_model::Format;
use oxisynth::{GeneratorType, MidiEvent as OxiEvent, SoundFontId, Synth, SynthDescriptor};

use crate::{
    Io, MidiEvent, MidiKind, ParamInfo, PluginEditor, PluginHandle, PluginInfo, PluginProcessor,
    PresetInfo, ProcessStatus, SamplerTone,
};

/// oxisynth is quiet at FluidSynth's default 0.2: this is "1.0" on our level control, matched by ear
/// to Surge XT's default patch (6 dB over the first guess of 0.5; the master limiter catches peaks).
const BASE_GAIN: f32 = 1.0;
/// State formats: v1 had the preset and the level, v2 adds the tone controls.
const STATE_V1: &[u8; 4] = b"ISF1";
const STATE_V2: &[u8; 4] = b"ISF2";
/// A folder of SoundFonts: v2 plus the name of the file the preset is in, so adding or removing a
/// file in the folder does not move a saved preset to another file.
const STATE_V3: &[u8; 4] = b"ISF3";
/// A folder holds at most this many SoundFonts (a file's number must fit the high byte of a bank).
pub const MAX_COLLECTION: usize = 255;

/// True for a file this module plays.
pub fn is_soundfont(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("sf2") || e.eq_ignore_ascii_case("sf3"))
}

/// What the browser shows for a SoundFont file. Cheap: nothing is decoded.
pub fn info_for(path: &Path) -> PluginInfo {
    let file = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let name = path
        .file_stem()
        .map_or_else(|| file.clone(), |n| n.to_string_lossy().into_owned());
    PluginInfo {
        format: Format::SoundFont,
        id: file,
        name,
        vendor: "SoundFont".into(),
        version: String::new(),
        features: vec!["instrument".into(), "soundfont".into()],
    }
}

/// The SoundFont files an instrument is made of: the file itself, or, for a folder, the
/// SoundFonts directly inside it, in name order.
pub fn files_of(path: &Path) -> Vec<PathBuf> {
    if !path.is_dir() {
        return vec![path.to_owned()];
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_soundfont(p))
        .collect();
    files.sort();
    files.truncate(MAX_COLLECTION);
    files
}

/// A folder that is an instrument of its own: all the SoundFonts in it play as one. (Plugin
/// bundles are folders too, but they have an extension of their own.)
pub fn is_collection(path: &Path) -> bool {
    path.is_dir()
        && !path.extension().is_some_and(|e| {
            ["vst3", "lv2", "clap", "vst", "component"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        })
}

/// A file or folder this module plays.
pub fn is_source(path: &Path) -> bool {
    is_soundfont(path) || is_collection(path)
}

/// What the browser shows for a folder of SoundFonts: one instrument named like the folder.
pub fn collection_info(dir: &Path) -> PluginInfo {
    let name = dir
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    PluginInfo {
        format: Format::SoundFont,
        id: format!("folder:{name}"),
        name,
        vendor: "SoundFont folder".into(),
        version: String::new(),
        features: vec!["instrument".into(), "soundfont".into()],
    }
}

/// The presets of all the fonts as one list. A single font keeps its own numbers; with several, a
/// preset's bank is `file number << 8 | the file's bank` (banks above 255 are left out) and its
/// name says which file it is in.
fn merged_presets(fonts: &[Arc<Font>]) -> Vec<PresetInfo> {
    if let [only] = fonts {
        return only.presets.clone();
    }
    fonts
        .iter()
        .enumerate()
        .flat_map(|(n, font)| {
            let stem = font
                .path
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            font.presets
                .iter()
                .filter(|p| p.bank < 256)
                .map(move |p| PresetInfo {
                    bank: (n as u16) << 8 | p.bank,
                    program: p.program,
                    name: p.name.clone(),
                    file: Some(stem.clone()),
                })
        })
        .collect()
}

/// A decoded SoundFont, shared between slots.
pub struct Font {
    font: oxisynth::SoundFont,
    pub presets: Vec<PresetInfo>,
    pub path: PathBuf,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Weak<Font>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<Font>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

impl Font {
    /// Is a decoded copy held by some slot right now? Then [`Font::load`] is instant.
    pub fn is_cached(path: &Path) -> bool {
        cache()
            .lock()
            .unwrap()
            .get(path)
            .is_some_and(|w| w.strong_count() > 0)
    }

    /// Decodes the file, or returns the copy another slot already holds. Slow; call it off the
    /// UI thread when you can.
    pub fn load(path: &Path) -> Result<Arc<Font>> {
        // One decode per file at a time: two slots loading the same file must end up sharing one
        // copy, while different files decode side by side.
        static LOADING: Mutex<Vec<(PathBuf, Arc<Mutex<()>>)>> = Mutex::new(Vec::new());
        let gate = {
            let mut gates = LOADING.lock().unwrap_or_else(|e| e.into_inner());
            match gates.iter().find(|(p, _)| p == path) {
                Some((_, g)) => g.clone(),
                None => {
                    let g = Arc::new(Mutex::new(()));
                    gates.push((path.to_owned(), g.clone()));
                    g
                }
            }
        };
        let _one_at_a_time = gate.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(font) = cache().lock().unwrap().get(path).and_then(Weak::upgrade) {
            return Ok(font);
        }
        let mut file =
            std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mut head = [0u8; 12];
        if file.read_exact(&mut head).is_err() || &head[..4] != b"RIFF" || &head[8..] != b"sfbk" {
            bail!("{} is not a SoundFont", path.display());
        }
        file.rewind()?;
        // The parser asserts on some damaged files instead of returning an error.
        let font = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            oxisynth::SoundFont::load(&mut file)
        }))
        .map_err(|_| anyhow!("{} is damaged", path.display()))?
        .map_err(|e| anyhow!("cannot read {} as a SoundFont: {e:?}", path.display()))?;
        let mut presets: Vec<PresetInfo> = font
            .presets()
            .iter()
            .filter_map(|p| {
                Some(PresetInfo {
                    bank: u16::try_from(p.banknum()).ok()?,
                    program: u8::try_from(p.num()).ok()?,
                    name: p.name().to_string(),
                    file: None,
                })
            })
            .collect();
        // Some fonts list a bank/program twice (MuseScore General has a placeholder named
        // "Bank:0,Preset0" beside the real piano); keep the real name.
        presets.sort_by_key(|p| (p.bank, p.program, p.name.starts_with("Bank:")));
        presets.dedup_by_key(|p| (p.bank, p.program));
        if presets.is_empty() {
            bail!("{} has no presets", path.display());
        }
        let font = Arc::new(Font {
            font,
            presets,
            path: path.to_owned(),
        });
        cache()
            .lock()
            .unwrap()
            .insert(path.to_owned(), Arc::downgrade(&font));
        Ok(font)
    }
}

/// What the main thread and the audio thread both see.
struct Shared {
    /// `bank << 8 | program`, written by the UI and by MIDI program changes.
    preset: AtomicU32,
    /// `f32` bits.
    gain: AtomicU32,
    /// The [`SamplerTone`] fields as `f32` bits (attack, release, cutoff, resonance, fine tune).
    tone: [AtomicU32; 5],
    polyphony: AtomicU32,
}

impl Shared {
    fn tone(&self) -> SamplerTone {
        let f = |i: usize| f32::from_bits(self.tone[i].load(Ordering::Relaxed));
        SamplerTone {
            attack: f(0),
            release: f(1),
            cutoff: f(2),
            resonance: f(3),
            fine_tune: f(4),
            polyphony: self.polyphony.load(Ordering::Relaxed) as u16,
        }
    }

    fn set_tone(&self, t: SamplerTone) {
        let t = t.clamped();
        for (slot, v) in
            self.tone
                .iter()
                .zip([t.attack, t.release, t.cutoff, t.resonance, t.fine_tune])
        {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
        self.polyphony.store(t.polyphony as u32, Ordering::Relaxed);
    }
}

fn pack(bank: u16, program: u8) -> u32 {
    (bank as u32) << 8 | program as u32
}

/// The fonts an instrument plays and their presets as one list (see [`merged_presets`]).
struct Fonts {
    fonts: Vec<Arc<Font>>,
    presets: Vec<PresetInfo>,
}

impl Fonts {
    fn has(&self, bank: u16, program: u8) -> bool {
        self.presets
            .binary_search_by_key(&(bank, program), |p| (p.bank, p.program))
            .is_ok()
    }

    /// The preset to start with: General MIDI piano if there is one, else the first.
    fn default_preset(&self) -> (u16, u8) {
        let p = self
            .presets
            .iter()
            .find(|p| (p.bank, p.program) == (0, 0))
            .unwrap_or(&self.presets[0]);
        (p.bank, p.program)
    }

    /// Which font and which of its own banks a preset's number means.
    fn split(&self, bank: u16) -> (usize, u16) {
        if self.fonts.len() == 1 {
            (0, bank)
        } else {
            (usize::from(bank >> 8), bank & 0xFF)
        }
    }
}

pub struct SoundFontPlugin {
    info: PluginInfo,
    fonts: Arc<Fonts>,
    /// Whether it is a folder of fonts (its state names the file of the preset).
    collection: bool,
    shared: Arc<Shared>,
}

impl SoundFontPlugin {
    /// A SoundFont file, or a folder of them played as one instrument.
    pub fn open(path: &Path) -> Result<Self> {
        let collection = is_collection(path);
        let files = files_of(path);
        if files.is_empty() {
            bail!("{} has no SoundFonts", path.display());
        }
        let mut fonts = Vec::new();
        let mut problems = Vec::new();
        for file in &files {
            match Font::load(file) {
                Ok(f) => fonts.push(f),
                // One damaged file must not take the other fonts of the folder with it.
                Err(e) if collection => problems.push(format!("{e:#}")),
                Err(e) => return Err(e),
            }
        }
        if fonts.is_empty() {
            bail!(
                "no SoundFont in {} could be read: {}",
                path.display(),
                problems.join("; ")
            );
        }
        let presets = merged_presets(&fonts);
        if presets.is_empty() {
            bail!("{} has no presets", path.display());
        }
        let fonts = Fonts { fonts, presets };
        let (bank, program) = fonts.default_preset();
        Ok(Self {
            info: if collection {
                collection_info(path)
            } else {
                info_for(path)
            },
            shared: Arc::new(Shared {
                preset: AtomicU32::new(pack(bank, program)),
                gain: AtomicU32::new(1.0f32.to_bits()),
                tone: Default::default(),
                polyphony: AtomicU32::new(SamplerTone::DEFAULT_POLYPHONY as u32),
            }),
            fonts: Arc::new(fonts),
            collection,
        })
    }

    /// The first decoded font (the only one, unless this is a folder).
    pub fn font(&self) -> &Arc<Font> {
        &self.fonts.fonts[0]
    }

    /// The file a preset number lives in, for a folder.
    fn file_of(&self, bank: u16) -> Option<&str> {
        self.fonts
            .presets
            .iter()
            .find(|p| p.bank == bank)
            .and_then(|p| p.file.as_deref())
    }
}

impl PluginHandle for SoundFontPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn activate(&mut self, sample_rate: f64, _max_block: u32) -> Result<Box<dyn PluginProcessor>> {
        let mut synth = Synth::new(SynthDescriptor {
            sample_rate: sample_rate as f32,
            gain: BASE_GAIN * f32::from_bits(self.shared.gain.load(Ordering::Relaxed)),
            // Room for the most voices the tone control allows; the limit is set from the tone.
            polyphony: SamplerTone::MAX_POLYPHONY,
            // The slot has its own reverb and chorus-free chain; these would only add CPU.
            reverb_active: false,
            chorus_active: false,
            ..Default::default()
        })
        .map_err(|e| anyhow!("cannot start the SoundFont synth: {e:?}"))?;
        let ids = self
            .fonts
            .fonts
            .iter()
            .map(|f| synth.add_font(f.font.clone(), true))
            .collect();
        let mut p = Processor {
            synth,
            ids,
            fonts: self.fonts.clone(),
            shared: self.shared.clone(),
            applied: u32::MAX,
            applied_gain: f32::NAN,
            applied_tone: None,
            channel: 0,
            pending_bank: None,
        };
        p.sync();
        Ok(Box::new(p))
    }

    fn deactivate(&mut self) {}

    fn params(&mut self) -> Vec<ParamInfo> {
        Vec::new()
    }
    fn set_param(&mut self, _id: u32, _value: f64) {}

    fn save_state(&mut self) -> Result<Vec<u8>> {
        let p = self.shared.preset.load(Ordering::Relaxed);
        let mut v = if self.collection { STATE_V3 } else { STATE_V2 }.to_vec();
        v.extend_from_slice(&((p >> 8) as u16).to_le_bytes());
        v.push(p as u8);
        v.extend_from_slice(&self.shared.gain.load(Ordering::Relaxed).to_le_bytes());
        for t in &self.shared.tone {
            v.extend_from_slice(&t.load(Ordering::Relaxed).to_le_bytes());
        }
        v.extend_from_slice(&(self.shared.polyphony.load(Ordering::Relaxed) as u16).to_le_bytes());
        if self.collection {
            let name = self.file_of((p >> 8) as u16).unwrap_or_default();
            v.extend_from_slice(&(name.len() as u16).to_le_bytes());
            v.extend_from_slice(name.as_bytes());
        }
        Ok(v)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        let v3 = data.len() >= 35 && &data[..4] == STATE_V3;
        let v2 = (data.len() == 33 && &data[..4] == STATE_V2) || v3;
        let v1 = data.len() == 11 && &data[..4] == STATE_V1;
        if !v1 && !v2 {
            bail!("not a SoundFont slot state");
        }
        let mut bank = u16::from_le_bytes([data[4], data[5]]);
        let program = data[6];
        if v3 {
            // Find the preset's file by name: the folder may have gained or lost files since.
            let len = usize::from(u16::from_le_bytes([data[33], data[34]]));
            if let Some(name) = data
                .get(35..35 + len)
                .and_then(|n| std::str::from_utf8(n).ok())
                && let Some(p) = self
                    .fonts
                    .presets
                    .iter()
                    .find(|p| p.file.as_deref() == Some(name) && p.bank & 0xFF == bank & 0xFF)
            {
                bank = p.bank & 0xFF00 | (bank & 0xFF);
            }
        }
        let f =
            |at: usize| f32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        // A saved preset the file no longer has leaves the default in place.
        if self.fonts.has(bank, program) {
            self.set_preset(bank, program);
        }
        if f(7).is_finite() {
            self.set_gain(f(7));
        }
        if v2 {
            self.set_tone(SamplerTone {
                attack: f(11),
                release: f(15),
                cutoff: f(19),
                resonance: f(23),
                fine_tune: f(27),
                polyphony: u16::from_le_bytes([data[31], data[32]]),
            });
        }
        Ok(())
    }

    fn presets(&self) -> Option<&[PresetInfo]> {
        Some(&self.fonts.presets)
    }

    fn preset(&self) -> Option<(u16, u8)> {
        let p = self.shared.preset.load(Ordering::Relaxed);
        Some(((p >> 8) as u16, p as u8))
    }

    fn set_preset(&mut self, bank: u16, program: u8) {
        if self.fonts.has(bank, program) {
            self.shared
                .preset
                .store(pack(bank, program), Ordering::Relaxed);
        }
    }

    fn gain(&self) -> Option<f32> {
        Some(f32::from_bits(self.shared.gain.load(Ordering::Relaxed)))
    }

    fn set_gain(&mut self, gain: f32) {
        self.shared
            .gain
            .store(gain.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    fn tone(&self) -> Option<SamplerTone> {
        Some(self.shared.tone())
    }

    fn set_tone(&mut self, tone: SamplerTone) {
        self.shared.set_tone(tone);
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        None
    }
    fn on_main_thread(&mut self) {}
    fn service(&mut self, _now: Instant) {}
    fn next_deadline(&self) -> Option<Instant> {
        None
    }
    fn on_fd_ready(&mut self) {}
}

struct Processor {
    synth: Synth,
    /// The synth's id for each font, in the order of [`Fonts::fonts`].
    ids: Vec<SoundFontId>,
    fonts: Arc<Fonts>,
    shared: Arc<Shared>,
    /// The preset and gain the synth has now.
    applied: u32,
    applied_gain: f32,
    applied_tone: Option<(SamplerTone, u8)>,
    /// Where notes go: channel 0, or 9 for a drum kit (bank 128), the one channel oxisynth plays
    /// drums on.
    channel: u8,
    /// A bank-select CC waiting for its program change.
    pending_bank: Option<u16>,
}

impl Processor {
    fn send(&mut self, e: OxiEvent) {
        let _ = self.synth.send_event(e);
    }

    /// Follows the main thread's preset and gain choices.
    fn sync(&mut self) {
        let gain = f32::from_bits(self.shared.gain.load(Ordering::Relaxed));
        if gain != self.applied_gain {
            self.applied_gain = gain;
            self.synth.set_gain(BASE_GAIN * gain);
        }
        let want = self.shared.preset.load(Ordering::Relaxed);
        let tone = self.shared.tone().clamped();
        if self.applied_tone != Some((tone, self.channel)) && want == self.applied {
            self.apply_tone(tone);
        }
        if want != self.applied {
            self.applied = want;
            let (bank, program) = ((want >> 8) as u16, want as u8);
            let old = self.channel;
            self.send(OxiEvent::AllNotesOff { channel: old });
            // Which font, and its own bank: the first font (or the only one) is the plain case.
            let (font, bank) = self.fonts.split(bank);
            self.channel = if bank >= 128 { 9 } else { 0 };
            if let Some(&id) = self.ids.get(font) {
                let _ = self
                    .synth
                    .program_select(self.channel, id, u32::from(bank), program);
            }
            // The tone belongs to the slot, not to a preset: put it on the new channel too.
            self.apply_tone(tone);
        }
    }

    /// Sets the generator offsets (what NRPN would) that shape every note on the channel.
    fn apply_tone(&mut self, t: SamplerTone) {
        let ch = self.channel as usize;
        let mut set = |g: GeneratorType, v: f32| {
            let _ = self.synth.set_gen(ch, g, v);
        };
        set(GeneratorType::VolEnvAttack, t.attack * 1200.0); // timecents
        set(GeneratorType::VolEnvRelease, t.release * 1200.0);
        set(GeneratorType::FilterFc, t.cutoff * 1200.0); // cents
        set(GeneratorType::FilterQ, t.resonance * 10.0); // centibels
        set(GeneratorType::FineTune, t.fine_tune); // cents
        let _ = self.synth.set_polyphony(t.polyphony);
        self.applied_tone = Some((t, self.channel));
    }

    fn event(&mut self, ev: &MidiEvent) {
        let channel = self.channel;
        match ev.kind() {
            MidiKind::NoteOn { key, vel, .. } => self.send(OxiEvent::NoteOn { channel, key, vel }),
            MidiKind::NoteOff { key, .. } => self.send(OxiEvent::NoteOff { channel, key }),
            MidiKind::PolyPressure { key, value, .. } => {
                self.send(OxiEvent::PolyphonicKeyPressure {
                    channel,
                    key,
                    value,
                });
            }
            MidiKind::ChannelPressure { value, .. } => {
                self.send(OxiEvent::ChannelPressure { channel, value });
            }
            MidiKind::PitchBend { value, .. } => self.send(OxiEvent::PitchBend { channel, value }),
            MidiKind::Cc { cc: 0, value, .. } => self.pending_bank = Some(value as u16),
            // Bank LSB would move us off the banks the file lists; ignore it.
            MidiKind::Cc { cc: 32, .. } => {}
            MidiKind::Cc { cc, value, .. } => self.send(OxiEvent::ControlChange {
                channel,
                ctrl: cc,
                value,
            }),
            MidiKind::ProgramChange { program, .. } => {
                // A program change from the keyboard picks a preset of this file, in the bank
                // last selected (the rack shows it too, through the shared value).
                let current = (self.applied >> 8) as u16;
                // A bank select names a bank of the font that is playing.
                let bank = match self.pending_bank.take() {
                    Some(b) if self.ids.len() > 1 => current & 0xFF00 | b & 0xFF,
                    Some(b) => b,
                    None => current,
                };
                if self.fonts.has(bank, program) {
                    self.shared
                        .preset
                        .store(pack(bank, program), Ordering::Relaxed);
                    self.sync();
                }
            }
            MidiKind::Other => {}
        }
    }
}

impl PluginProcessor for Processor {
    fn process(&mut self, io: &mut Io, events: &[MidiEvent]) -> ProcessStatus {
        self.sync();
        let frames = io.frames();
        let mut pos = 0;
        for ev in events {
            let at = (ev.time as usize).min(frames);
            if at > pos {
                self.synth
                    .write_f32(at - pos, io.out_l, pos, 1, io.out_r, pos, 1);
                pos = at;
            }
            self.event(ev);
        }
        if frames > pos {
            self.synth
                .write_f32(frames - pos, io.out_l, pos, 1, io.out_r, pos, 1);
        }
        if events.is_empty() && self.synth.active_voice_count() == 0 {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}
