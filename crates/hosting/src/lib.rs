//! The plugin abstraction the engine talks to (plan §4.5), plus built-in instruments.
//!
//! Format implementations (CLAP, VST3, ...) live in their own modules and are added with
//! the formats; this crate starts with the audio-thread half, which the engine needs.

pub mod builtin;
pub mod clap;
mod midi;
mod plugin;
pub mod soundfont;
pub mod vst3;

pub use midi::{MidiEvent, MidiKind};
pub use plugin::{
    EditorSize, HostEvent, ParamInfo, PluginEditor, PluginHandle, PluginInfo, PresetInfo,
    SamplerTone, Waker,
};

/// What a processor tells the engine after a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Keep calling me every block.
    Continue,
    /// I am silent and have no tail: skip me until new MIDI arrives.
    Sleep,
}

/// One block of audio for an instrument or effect. Everything has the same length.
pub struct Io<'a> {
    /// Audio in, for effects; `None` for instruments.
    pub input: Option<(&'a [f32], &'a [f32])>,
    pub out_l: &'a mut [f32],
    pub out_r: &'a mut [f32],
}

impl Io<'_> {
    pub fn frames(&self) -> usize {
        self.out_l.len()
    }
}

/// The audio-thread half of a plugin. Must be real-time safe: no allocation, locks or syscalls.
pub trait PluginProcessor: Send {
    /// Fill `io.out_*` (the engine zeroes them first). `events` are in time order, with
    /// `time` counted in frames from the start of this block.
    fn process(&mut self, io: &mut Io, events: &[MidiEvent]) -> ProcessStatus;
}
