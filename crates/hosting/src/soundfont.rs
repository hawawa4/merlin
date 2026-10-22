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
use oxisynth::{MidiEvent as OxiEvent, Synth, SynthDescriptor};

use crate::{
    Io, MidiEvent, MidiKind, ParamInfo, PluginEditor, PluginHandle, PluginInfo, PluginProcessor,
    PresetInfo, ProcessStatus,
};

/// Voices a slot can play at once.
const POLYPHONY: u16 = 96;
/// oxisynth is quiet at FluidSynth's default 0.2: this is "1.0" on our level control, matched by ear
/// to Surge XT's default patch (6 dB over the first guess of 0.5; the master limiter catches peaks).
const BASE_GAIN: f32 = 1.0;
const STATE_MAGIC: &[u8; 4] = b"ISF1";

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
                })
            })
            .collect();
        presets.sort_by_key(|p| (p.bank, p.program));
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

    /// The preset to start with: General MIDI piano if there is one, else the first.
    fn default_preset(&self) -> (u16, u8) {
        let p = self
            .presets
            .iter()
            .find(|p| (p.bank, p.program) == (0, 0))
            .unwrap_or(&self.presets[0]);
        (p.bank, p.program)
    }

    fn has(&self, bank: u16, program: u8) -> bool {
        self.presets
            .binary_search_by_key(&(bank, program), |p| (p.bank, p.program))
            .is_ok()
    }
}

/// What the main thread and the audio thread both see.
struct Shared {
    /// `bank << 8 | program`, written by the UI and by MIDI program changes.
    preset: AtomicU32,
    /// `f32` bits.
    gain: AtomicU32,
}

fn pack(bank: u16, program: u8) -> u32 {
    (bank as u32) << 8 | program as u32
}

pub struct SoundFontPlugin {
    info: PluginInfo,
    font: Arc<Font>,
    shared: Arc<Shared>,
}

impl SoundFontPlugin {
    pub fn open(path: &Path) -> Result<Self> {
        let font = Font::load(path)?;
        let (bank, program) = font.default_preset();
        Ok(Self {
            info: info_for(path),
            shared: Arc::new(Shared {
                preset: AtomicU32::new(pack(bank, program)),
                gain: AtomicU32::new(1.0f32.to_bits()),
            }),
            font,
        })
    }

    pub fn font(&self) -> &Arc<Font> {
        &self.font
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
            polyphony: POLYPHONY,
            // The slot has its own reverb and chorus-free chain; these would only add CPU.
            reverb_active: false,
            chorus_active: false,
            ..Default::default()
        })
        .map_err(|e| anyhow!("cannot start the SoundFont synth: {e:?}"))?;
        synth.add_font(self.font.font.clone(), true);
        let mut p = Processor {
            synth,
            font: self.font.clone(),
            shared: self.shared.clone(),
            applied: u32::MAX,
            applied_gain: f32::NAN,
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
        let mut v = STATE_MAGIC.to_vec();
        v.extend_from_slice(&((p >> 8) as u16).to_le_bytes());
        v.push(p as u8);
        v.extend_from_slice(&self.shared.gain.load(Ordering::Relaxed).to_le_bytes());
        Ok(v)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        if data.len() != 11 || &data[..4] != STATE_MAGIC {
            bail!("not a SoundFont slot state");
        }
        let bank = u16::from_le_bytes([data[4], data[5]]);
        let program = data[6];
        let gain = f32::from_le_bytes([data[7], data[8], data[9], data[10]]);
        // A saved preset the file no longer has leaves the default in place.
        if self.font.has(bank, program) {
            self.set_preset(bank, program);
        }
        if gain.is_finite() {
            self.set_gain(gain);
        }
        Ok(())
    }

    fn presets(&self) -> Option<&[PresetInfo]> {
        Some(&self.font.presets)
    }

    fn preset(&self) -> Option<(u16, u8)> {
        let p = self.shared.preset.load(Ordering::Relaxed);
        Some(((p >> 8) as u16, p as u8))
    }

    fn set_preset(&mut self, bank: u16, program: u8) {
        if self.font.has(bank, program) {
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
    font: Arc<Font>,
    shared: Arc<Shared>,
    /// The preset and gain the synth has now.
    applied: u32,
    applied_gain: f32,
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
        if want != self.applied {
            self.applied = want;
            let (bank, program) = ((want >> 8) as u16, want as u8);
            let old = self.channel;
            self.send(OxiEvent::AllNotesOff { channel: old });
            self.channel = if bank >= 128 { 9 } else { 0 };
            let channel = self.channel;
            self.send(OxiEvent::ControlChange {
                channel,
                ctrl: 0,
                value: bank.min(127) as u8,
            });
            self.send(OxiEvent::ControlChange {
                channel,
                ctrl: 32,
                value: 0,
            });
            self.send(OxiEvent::ProgramChange {
                channel,
                program_id: program,
            });
        }
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
                let bank = self
                    .pending_bank
                    .take()
                    .unwrap_or((self.applied >> 8) as u16);
                if self.font.has(bank, program) {
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
