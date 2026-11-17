//! sfizz, the SFZ sampler, built from its release tarball (see `build.rs`), behind a small safe API.
//!
//! sfizz splits its calls between a control thread and the real-time thread, and some control
//! calls ("OFF" in `sfizz.h`) must not run while a real-time call does. Here every call takes
//! `&mut self`, so one owner at a time is all the type system allows: whoever owns a [`Synth`]
//! is the only thread in it. Load and configure a synth on one thread, then hand it to the
//! audio thread; build another for a different file rather than reloading one that plays.

use std::ffi::{CString, c_char, c_float, c_int, c_uint, c_void};
use std::path::Path;
use std::ptr::NonNull;

#[repr(C)]
struct RawSynth {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn sfizz_create_synth() -> *mut RawSynth;
    fn sfizz_free(synth: *mut RawSynth);
    fn sfizz_load_file(synth: *mut RawSynth, path: *const c_char) -> bool;
    fn sfizz_load_string(synth: *mut RawSynth, path: *const c_char, text: *const c_char) -> bool;
    fn sfizz_get_num_regions(synth: *mut RawSynth) -> c_int;
    fn sfizz_get_num_active_voices(synth: *mut RawSynth) -> c_int;
    fn sfizz_set_samples_per_block(synth: *mut RawSynth, samples_per_block: c_int);
    fn sfizz_set_sample_rate(synth: *mut RawSynth, sample_rate: c_float);
    fn sfizz_set_num_voices(synth: *mut RawSynth, num_voices: c_int);
    fn sfizz_set_preload_size(synth: *mut RawSynth, preload_size: c_uint);
    fn sfizz_send_note_on(synth: *mut RawSynth, delay: c_int, note: c_int, velocity: c_int);
    fn sfizz_send_note_off(synth: *mut RawSynth, delay: c_int, note: c_int, velocity: c_int);
    fn sfizz_send_cc(synth: *mut RawSynth, delay: c_int, cc: c_int, value: c_int);
    fn sfizz_send_pitch_wheel(synth: *mut RawSynth, delay: c_int, pitch: c_int);
    fn sfizz_send_channel_aftertouch(synth: *mut RawSynth, delay: c_int, aftertouch: c_int);
    fn sfizz_send_poly_aftertouch(
        synth: *mut RawSynth,
        delay: c_int,
        note: c_int,
        aftertouch: c_int,
    );
    fn sfizz_set_tuning_frequency(synth: *mut RawSynth, frequency: c_float);
    fn sfizz_all_sound_off(synth: *mut RawSynth);
    fn sfizz_render_block(
        synth: *mut RawSynth,
        channels: *mut *mut c_float,
        num_channels: c_int,
        num_frames: c_int,
    );
    fn sfizz_get_unknown_opcodes(synth: *mut RawSynth) -> *mut c_char;
    fn sfizz_free_memory(ptr: *mut c_void);
}

/// The most frames sfizz renders in one call (its `config::maxBlockSize`).
pub const MAX_BLOCK: usize = 8192;

/// One sfizz instance with the instrument it plays.
pub struct Synth {
    raw: NonNull<RawSynth>,
    /// What [`Synth::set_samples_per_block`] was given: renders longer than this are split.
    block: usize,
}

// A synth is not tied to the thread that made it; `&mut self` on every call keeps two threads
// from being inside it at once.
unsafe impl Send for Synth {}

impl Synth {
    pub fn new() -> Self {
        let raw = NonNull::new(unsafe { sfizz_create_synth() }).expect("sfizz_create_synth");
        let mut s = Self { raw, block: 1024 };
        s.set_samples_per_block(s.block);
        s
    }

    fn ptr(&self) -> *mut RawSynth {
        self.raw.as_ptr()
    }

    /// Loads an SFZ file and the start of every sample it uses (the rest streams from disk while
    /// playing). Slow for big instruments: control thread. False if the file could not be read
    /// or parsed; a file with no playable regions loads but is silent (see
    /// [`num_regions`](Self::num_regions)).
    pub fn load_file(&mut self, path: &Path) -> bool {
        let Ok(c) = CString::new(path.as_os_str().as_encoded_bytes()) else {
            return false;
        };
        unsafe { sfizz_load_file(self.ptr(), c.as_ptr()) }
    }

    /// Loads SFZ text; sample paths in it are relative to the folder of `virtual_path`.
    pub fn load_string(&mut self, virtual_path: &Path, text: &str) -> bool {
        let (Ok(p), Ok(t)) = (
            CString::new(virtual_path.as_os_str().as_encoded_bytes()),
            CString::new(text),
        ) else {
            return false;
        };
        unsafe { sfizz_load_string(self.ptr(), p.as_ptr(), t.as_ptr()) }
    }

    /// Regions the loaded instrument has (zero: nothing will sound).
    pub fn num_regions(&mut self) -> usize {
        unsafe { sfizz_get_num_regions(self.ptr()) }.max(0) as usize
    }

    /// Voices sounding now (including release tails).
    pub fn active_voices(&mut self) -> usize {
        unsafe { sfizz_get_num_active_voices(self.ptr()) }.max(0) as usize
    }

    /// Opcodes in the file that sfizz does not know (they are ignored).
    pub fn unknown_opcodes(&mut self) -> Vec<String> {
        unsafe {
            let p = sfizz_get_unknown_opcodes(self.ptr());
            if p.is_null() {
                return Vec::new();
            }
            let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
            sfizz_free_memory(p.cast());
            s.split(',')
                .filter(|o| !o.is_empty())
                .map(str::to_owned)
                .collect()
        }
    }

    /// Control thread.
    pub fn set_sample_rate(&mut self, rate: f32) {
        unsafe { sfizz_set_sample_rate(self.ptr(), rate) }
    }

    /// The longest block [`render`](Self::render) will be asked for. Control thread.
    pub fn set_samples_per_block(&mut self, frames: usize) {
        self.block = frames.clamp(1, MAX_BLOCK);
        unsafe { sfizz_set_samples_per_block(self.ptr(), self.block as c_int) }
    }

    /// Notes that can sound at once. Control thread.
    pub fn set_num_voices(&mut self, voices: usize) {
        unsafe { sfizz_set_num_voices(self.ptr(), voices.clamp(1, 512) as c_int) }
    }

    /// Frames of each sample kept in memory (the rest streams from disk). Control thread;
    /// call before loading a file.
    pub fn set_preload_size(&mut self, frames: u32) {
        unsafe { sfizz_set_preload_size(self.ptr(), frames) }
    }

    // ---- real-time calls: `delay` is the frame within the next rendered block. ----

    pub fn note_on(&mut self, delay: u32, key: u8, velocity: u8) {
        unsafe { sfizz_send_note_on(self.ptr(), delay as c_int, key.into(), velocity.into()) }
    }

    pub fn note_off(&mut self, delay: u32, key: u8, velocity: u8) {
        unsafe { sfizz_send_note_off(self.ptr(), delay as c_int, key.into(), velocity.into()) }
    }

    pub fn cc(&mut self, delay: u32, cc: u8, value: u8) {
        unsafe { sfizz_send_cc(self.ptr(), delay as c_int, cc.into(), value.into()) }
    }

    /// `value` is 14-bit MIDI, 8192 the centre.
    pub fn pitch_wheel(&mut self, delay: u32, value: u16) {
        let centred = i32::from(value.min(16383)) - 8192;
        unsafe { sfizz_send_pitch_wheel(self.ptr(), delay as c_int, centred) }
    }

    pub fn channel_aftertouch(&mut self, delay: u32, value: u8) {
        unsafe { sfizz_send_channel_aftertouch(self.ptr(), delay as c_int, value.into()) }
    }

    pub fn poly_aftertouch(&mut self, delay: u32, key: u8, value: u8) {
        unsafe { sfizz_send_poly_aftertouch(self.ptr(), delay as c_int, key.into(), value.into()) }
    }

    /// The pitch of A4 in Hz (440 normally). Real-time thread.
    pub fn set_tuning_frequency(&mut self, hz: f32) {
        unsafe { sfizz_set_tuning_frequency(self.ptr(), hz) }
    }

    /// Silences every voice at once.
    pub fn all_sound_off(&mut self) {
        unsafe { sfizz_all_sound_off(self.ptr()) }
    }

    /// Renders the next `left.len()` frames (overwriting both buffers). Events sent before this
    /// call land at their delay inside it.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let mut at = 0;
        while at < frames {
            let n = (frames - at).min(self.block);
            let mut ch = [left[at..].as_mut_ptr(), right[at..].as_mut_ptr()];
            unsafe { sfizz_render_block(self.ptr(), ch.as_mut_ptr(), 2, n as c_int) }
            at += n;
        }
    }
}

impl Default for Synth {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Synth {
    fn drop(&mut self) {
        unsafe { sfizz_free(self.ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit mono WAV of a sine.
    fn sine_wav(rate: u32, frames: u32, hz: f32) -> Vec<u8> {
        let mut v = Vec::new();
        let data = frames * 2;
        v.extend(b"RIFF");
        v.extend((36 + data).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes()); // PCM
        v.extend(1u16.to_le_bytes()); // mono
        v.extend(rate.to_le_bytes());
        v.extend((rate * 2).to_le_bytes());
        v.extend(2u16.to_le_bytes());
        v.extend(16u16.to_le_bytes());
        v.extend(b"data");
        v.extend(data.to_le_bytes());
        for i in 0..frames {
            let s = (i as f32 / rate as f32 * hz * std::f32::consts::TAU).sin();
            v.extend(((s * 0.5 * i16::MAX as f32) as i16).to_le_bytes());
        }
        v
    }

    #[test]
    fn plays_a_note_from_an_sfz_file() {
        let dir = std::env::temp_dir().join(format!("merlin-sfizz-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("samples")).unwrap();
        std::fs::write(dir.join("samples/a.wav"), sine_wav(48000, 48000, 440.0)).unwrap();
        let sfz = dir.join("test.sfz");
        std::fs::write(
            &sfz,
            "<region> sample=samples/a.wav pitch_keycenter=69 lokey=0 hikey=127 made_up_opcode=1\n",
        )
        .unwrap();

        let mut s = Synth::new();
        s.set_sample_rate(48000.0);
        s.set_samples_per_block(256);
        assert!(s.load_file(&sfz));
        assert_eq!(s.num_regions(), 1);
        assert_eq!(s.unknown_opcodes(), ["made_up_opcode"]);

        let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
        s.render(&mut l, &mut r);
        assert!(l.iter().all(|&x| x == 0.0), "silent before a note");
        s.note_on(0, 69, 100);
        let mut peak = 0.0f32;
        // Longer than the block size: the render is split.
        let (mut l, mut r) = (vec![0.0; 1000], vec![0.0; 1000]);
        for _ in 0..4 {
            s.render(&mut l, &mut r);
            peak = l.iter().chain(&r).fold(peak, |m, x| m.max(x.abs()));
        }
        assert!(peak > 0.05, "a note sounds (peak {peak})");
        assert_eq!(s.active_voices(), 1);

        assert!(!s.load_file(&dir.join("missing.sfz")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
