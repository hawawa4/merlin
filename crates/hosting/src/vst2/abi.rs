#![allow(dead_code)]

//! The VST 2 plugin ABI, written from the binary layout that plugins and hosts have shared
//! since 1999 (the structures and opcodes below are what every host needs to talk to a `.so`
//! or, through yabridge, a Windows `.dll`). No Steinberg header is used.

use std::ffi::c_void;

/// `'VstP'`.
pub const EFFECT_MAGIC: i32 = 0x5673_7450;

pub type DispatcherProc =
    unsafe extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
pub type ProcessProc = unsafe extern "C" fn(*mut AEffect, *const *const f32, *mut *mut f32, i32);
pub type SetParameterProc = unsafe extern "C" fn(*mut AEffect, i32, f32);
pub type GetParameterProc = unsafe extern "C" fn(*mut AEffect, i32) -> f32;
pub type HostCallback =
    unsafe extern "C" fn(*mut AEffect, i32, i32, isize, *mut c_void, f32) -> isize;
pub type EntryProc = unsafe extern "C" fn(HostCallback) -> *mut AEffect;

#[repr(C)]
pub struct AEffect {
    pub magic: i32,
    pub dispatcher: DispatcherProc,
    pub process: ProcessProc,
    pub set_parameter: SetParameterProc,
    pub get_parameter: GetParameterProc,
    pub num_programs: i32,
    pub num_params: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    pub flags: i32,
    pub resvd1: isize,
    pub resvd2: isize,
    pub initial_delay: i32,
    pub real_qualities: i32,
    pub off_qualities: i32,
    pub io_ratio: f32,
    pub object: *mut c_void,
    /// Ours: points at the host's context for this instance.
    pub user: *mut c_void,
    pub unique_id: i32,
    pub version: i32,
    pub process_replacing: ProcessProc,
    pub process_double_replacing: *const c_void,
    pub future: [u8; 56],
}

// Flags.
pub const FLAG_HAS_EDITOR: i32 = 1 << 0;
pub const FLAG_CAN_REPLACING: i32 = 1 << 4;
pub const FLAG_PROGRAM_CHUNKS: i32 = 1 << 5;
pub const FLAG_IS_SYNTH: i32 = 1 << 8;

// Effect opcodes (host -> plugin).
pub const EFF_OPEN: i32 = 0;
pub const EFF_CLOSE: i32 = 1;
pub const EFF_SET_PROGRAM: i32 = 2;
pub const EFF_GET_PROGRAM: i32 = 3;
pub const EFF_GET_PARAM_DISPLAY: i32 = 7;
pub const EFF_GET_PARAM_NAME: i32 = 8;
pub const EFF_SET_SAMPLE_RATE: i32 = 10;
pub const EFF_SET_BLOCK_SIZE: i32 = 11;
pub const EFF_MAINS_CHANGED: i32 = 12;
pub const EFF_EDIT_GET_RECT: i32 = 13;
pub const EFF_EDIT_OPEN: i32 = 14;
pub const EFF_EDIT_CLOSE: i32 = 15;
pub const EFF_EDIT_IDLE: i32 = 19;
pub const EFF_GET_CHUNK: i32 = 23;
pub const EFF_SET_CHUNK: i32 = 24;
pub const EFF_PROCESS_EVENTS: i32 = 25;
pub const EFF_CAN_BE_AUTOMATED: i32 = 26;
pub const EFF_GET_PLUG_CATEGORY: i32 = 35;
pub const EFF_GET_EFFECT_NAME: i32 = 45;
pub const EFF_GET_VENDOR_STRING: i32 = 47;
pub const EFF_GET_PRODUCT_STRING: i32 = 48;
pub const EFF_GET_VENDOR_VERSION: i32 = 49;
pub const EFF_CAN_DO: i32 = 51;
pub const EFF_GET_VST_VERSION: i32 = 58;
pub const EFF_START_PROCESS: i32 = 71;
pub const EFF_STOP_PROCESS: i32 = 72;
pub const EFF_SHELL_GET_NEXT_PLUGIN: i32 = 70;

pub const PLUG_CATEG_SYNTH: isize = 2;
pub const PLUG_CATEG_SHELL: isize = 10;

// Host opcodes (plugin -> host).
pub const MASTER_AUTOMATE: i32 = 0;
pub const MASTER_VERSION: i32 = 1;
pub const MASTER_CURRENT_ID: i32 = 2;
pub const MASTER_IDLE: i32 = 3;
pub const MASTER_GET_TIME: i32 = 7;
pub const MASTER_PROCESS_EVENTS: i32 = 8;
pub const MASTER_IO_CHANGED: i32 = 13;
pub const MASTER_SIZE_WINDOW: i32 = 15;
pub const MASTER_GET_SAMPLE_RATE: i32 = 16;
pub const MASTER_GET_BLOCK_SIZE: i32 = 17;
pub const MASTER_GET_INPUT_LATENCY: i32 = 18;
pub const MASTER_GET_OUTPUT_LATENCY: i32 = 19;
pub const MASTER_GET_CURRENT_PROCESS_LEVEL: i32 = 23;
pub const MASTER_GET_AUTOMATION_STATE: i32 = 24;
pub const MASTER_GET_VENDOR_STRING: i32 = 32;
pub const MASTER_GET_PRODUCT_STRING: i32 = 33;
pub const MASTER_GET_VENDOR_VERSION: i32 = 34;
pub const MASTER_CAN_DO: i32 = 37;
pub const MASTER_GET_LANGUAGE: i32 = 38;
pub const MASTER_UPDATE_DISPLAY: i32 = 42;
pub const MASTER_BEGIN_EDIT: i32 = 43;
pub const MASTER_END_EDIT: i32 = 44;

/// `VstMidiEvent`: 32 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MidiEvent {
    pub kind: i32, // 1 = MIDI
    pub byte_size: i32,
    pub delta_frames: i32,
    pub flags: i32,
    pub note_length: i32,
    pub note_offset: i32,
    pub midi_data: [u8; 4],
    pub detune: i8,
    pub note_off_velocity: u8,
    pub reserved1: u8,
    pub reserved2: u8,
}

pub const EVENT_MIDI: i32 = 1;

/// `VstEvents` with room for `N` events.
#[repr(C)]
pub struct Events<const N: usize> {
    pub num_events: i32,
    pub reserved: isize,
    pub events: [*mut MidiEvent; N],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TimeInfo {
    pub sample_pos: f64,
    pub sample_rate: f64,
    pub nano_seconds: f64,
    pub ppq_pos: f64,
    pub tempo: f64,
    pub bar_start_pos: f64,
    pub cycle_start_pos: f64,
    pub cycle_end_pos: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    pub smpte_offset: i32,
    pub smpte_frame_rate: i32,
    pub samples_to_next_clock: i32,
    pub flags: i32,
}

pub const TIME_TRANSPORT_PLAYING: i32 = 1 << 1;
pub const TIME_PPQ_VALID: i32 = 1 << 9;
pub const TIME_TEMPO_VALID: i32 = 1 << 10;
pub const TIME_BARS_VALID: i32 = 1 << 11;
pub const TIME_SIG_VALID: i32 = 1 << 13;

/// `ERect`: the editor's size and position.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ERect {
    pub top: i16,
    pub left: i16,
    pub bottom: i16,
    pub right: i16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The layout every plugin was compiled against (64-bit Linux).
    #[test]
    fn the_structures_have_the_layout_plugins_expect() {
        assert_eq!(offset_of!(AEffect, dispatcher), 8);
        assert_eq!(offset_of!(AEffect, num_programs), 40);
        assert_eq!(offset_of!(AEffect, flags), 56);
        assert_eq!(offset_of!(AEffect, resvd1), 64);
        assert_eq!(offset_of!(AEffect, initial_delay), 80);
        assert_eq!(offset_of!(AEffect, object), 96);
        assert_eq!(offset_of!(AEffect, unique_id), 112);
        assert_eq!(offset_of!(AEffect, process_replacing), 120);
        assert_eq!(size_of::<AEffect>(), 192);
        assert_eq!(size_of::<MidiEvent>(), 32);
        assert_eq!(size_of::<TimeInfo>(), 8 * 8 + 6 * 4);
        assert_eq!(offset_of!(Events<2>, events), 16);
    }
}
