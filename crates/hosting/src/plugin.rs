//! The main-thread half of the plugin abstraction (plan §4.5): lifecycle, parameters, state
//! and editor. The audio half is [`crate::PluginProcessor`].

use std::time::Instant;

use anyhow::Result;
use merlin_model::Format;

use crate::PluginProcessor;

/// What the host knows about a plugin without running it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginInfo {
    pub format: Format,
    /// CLAP plugin id (or VST3 class id).
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub features: Vec<String>,
}

impl PluginInfo {
    pub fn is_instrument(&self) -> bool {
        self.features.iter().any(|f| f == "instrument")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub automatable: bool,
    pub stepped: bool,
    pub hidden: bool,
    pub read_only: bool,
}

/// One selectable sound inside an instrument (a SoundFont's bank/program).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetInfo {
    pub bank: u16,
    pub program: u8,
    pub name: String,
}

/// How a sampler's sound is shaped on top of its preset, in musical units. All zeros (and the
/// default polyphony) leaves the preset as the file made it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplerTone {
    /// Volume envelope attack, in octaves of time (+1 = twice as slow, -1 = twice as fast). Piano
    /// attacks are about a millisecond, so audible slowing takes many octaves (up to +8 = x256).
    pub attack: f32,
    /// Volume envelope release, in octaves of time.
    pub release: f32,
    /// Filter cutoff, in octaves (+1 = brighter).
    pub cutoff: f32,
    /// Filter resonance added, in dB (0..=24).
    pub resonance: f32,
    /// Tuning, in cents (-100..=100).
    pub fine_tune: f32,
    /// Notes that can sound at once.
    pub polyphony: u16,
}

impl SamplerTone {
    pub const MAX_POLYPHONY: u16 = 128;
    pub const DEFAULT_POLYPHONY: u16 = 64;

    /// Brings every value into its range.
    pub fn clamped(self) -> Self {
        let f = |v: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { 0.0 };
        Self {
            attack: f(self.attack, -3.0, 8.0),
            release: f(self.release, -3.0, 4.0),
            cutoff: f(self.cutoff, -3.0, 3.0),
            resonance: f(self.resonance, 0.0, 24.0),
            fine_tune: f(self.fine_tune, -100.0, 100.0),
            polyphony: self.polyphony.clamp(4, Self::MAX_POLYPHONY),
        }
    }
}

impl Default for SamplerTone {
    fn default() -> Self {
        Self {
            attack: 0.0,
            release: 0.0,
            cutoff: 0.0,
            resonance: 0.0,
            fine_tune: 0.0,
            polyphony: Self::DEFAULT_POLYPHONY,
        }
    }
}

/// Something a plugin asked of the main thread. The app turns these into its own wakeups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// Call [`PluginHandle::on_main_thread`].
    MainThreadCallback,
    /// The plugin wants its editor at this size.
    EditorResize { width: u32, height: u32 },
    /// The plugin closed its editor by itself.
    EditorClosed,
    /// One of the plugin's fds is ready: call [`PluginHandle::on_fd_ready`].
    FdReady,
    /// The plugin's state changed (mark the rig dirty).
    StateDirty,
}

/// How a plugin reaches the main thread. Called from any thread, so keep it cheap and
/// non-blocking (the app implements it with an event-loop proxy).
pub trait Waker: Send + Sync {
    fn wake(&self, event: HostEvent);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorSize {
    pub width: u32,
    pub height: u32,
}

/// A plugin's GUI, embedded into an X11 window we own.
pub trait PluginEditor {
    /// Creates the editor inside the X11 window `parent` (an XID) and returns its size.
    fn attach(&mut self, parent: u32, scale: f64) -> Result<EditorSize>;
    fn is_attached(&self) -> bool;
    fn show(&mut self);
    fn hide(&mut self);
    fn size(&self) -> EditorSize;
    fn can_resize(&self) -> bool;
    /// The size the plugin would accept closest to `want`.
    fn adjust_size(&mut self, want: EditorSize) -> EditorSize;
    /// Resizes the editor; false if the plugin refused. Only call with sizes from
    /// [`adjust_size`](Self::adjust_size) (JUCE logs misbehaviour otherwise).
    fn set_size(&mut self, size: EditorSize) -> bool;
    /// The plugin asked for a new size and we accepted it without calling `set_size`.
    fn note_size(&mut self, size: EditorSize);
    /// Destroys the editor.
    fn detach(&mut self);
}

/// Main-thread side of a plugin instance.
pub trait PluginHandle {
    fn info(&self) -> &PluginInfo;

    /// Activates the plugin and returns the audio-thread half. Drop that (after the engine
    /// hands it back) and then call [`deactivate`](Self::deactivate).
    fn activate(&mut self, sample_rate: f64, max_block: u32) -> Result<Box<dyn PluginProcessor>>;
    fn deactivate(&mut self);

    fn params(&mut self) -> Vec<ParamInfo>;
    /// Sets a parameter from the main thread (reaches the plugin on the next block).
    fn set_param(&mut self, id: u32, value: f64);

    fn save_state(&mut self) -> Result<Vec<u8>>;
    fn load_state(&mut self, data: &[u8]) -> Result<()>;

    /// The sounds this instrument can switch between, for instruments that have a preset list
    /// instead of an editor (SoundFonts). `None` for everything else.
    fn presets(&self) -> Option<&[PresetInfo]> {
        None
    }
    /// The selected `(bank, program)`.
    fn preset(&self) -> Option<(u16, u8)> {
        None
    }
    /// Selects a sound; takes effect on the next audio block.
    fn set_preset(&mut self, _bank: u16, _program: u8) {}
    /// Output gain of a sampler, 0..=2 (1 = the instrument's own level).
    fn gain(&self) -> Option<f32> {
        None
    }
    fn set_gain(&mut self, _gain: f32) {}
    /// The sound-shaping controls of a sampler.
    fn tone(&self) -> Option<SamplerTone> {
        None
    }
    fn set_tone(&mut self, _tone: SamplerTone) {}

    /// The plugin's GUI, if it has an embeddable one.
    fn editor(&mut self) -> Option<&mut dyn PluginEditor>;

    /// Run what the plugin asked for with [`HostEvent::MainThreadCallback`].
    fn on_main_thread(&mut self);
    /// Run due plugin timers and pick up fd changes. Call after any wakeup.
    fn service(&mut self, now: Instant);
    /// When [`service`](Self::service) next has timer work.
    fn next_deadline(&self) -> Option<Instant>;
    /// Run what the plugin's ready fds need ([`HostEvent::FdReady`]).
    fn on_fd_ready(&mut self);
}
