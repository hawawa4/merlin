//! What a VST2 plugin can ask of its host (the `audioMaster` callback).

use std::cell::{Cell, UnsafeCell};
use std::ffi::{CStr, c_char, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use super::abi::*;
use crate::{HostEvent, Waker, host_time};

thread_local! {
    /// The sub-plugin a shell plugin (one `.so` holding several) is being created for.
    pub(super) static SHELL_ID: Cell<i32> = const { Cell::new(0) };
}

/// Everything the callback needs about one instance. Its address is kept in `AEffect::user`.
pub struct Ctx {
    waker: Arc<dyn Waker>,
    sample_rate: AtomicU64,
    block_size: AtomicI32,
    time: UnsafeCell<TimeInfo>,
}

// SAFETY: `time` is only written by the `getTime` callback, which plugins call from one thread
// at a time (the audio thread, or the main thread outside processing).
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

impl Ctx {
    pub fn new(waker: Arc<dyn Waker>) -> Self {
        Self {
            waker,
            sample_rate: AtomicU64::new(48_000f64.to_bits()),
            block_size: AtomicI32::new(1024),
            time: UnsafeCell::new(TimeInfo::default()),
        }
    }

    pub fn set_audio(&self, sample_rate: f64, block: i32) {
        self.sample_rate
            .store(sample_rate.to_bits(), Ordering::Relaxed);
        self.block_size.store(block, Ordering::Relaxed);
    }

    fn rate(&self) -> f64 {
        f64::from_bits(self.sample_rate.load(Ordering::Relaxed))
    }
}

/// Copies `text` into the plugin's buffer (at most 63 bytes and a terminator, the size every
/// host string has).
unsafe fn put_string(ptr: *mut c_void, text: &str) {
    if ptr.is_null() {
        return;
    }
    let bytes = text.as_bytes();
    let n = bytes.len().min(63);
    // SAFETY: the plugin gives a buffer of at least 64 bytes for these strings.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), n);
        *ptr.cast::<u8>().add(n) = 0;
    }
}

/// The host callback handed to every plugin.
///
/// # Safety
/// Called by plugins with the arguments the VST2 protocol defines.
pub unsafe extern "C" fn callback(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    ptr: *mut c_void,
    _opt: f32,
) -> isize {
    // SAFETY: `user` is either null (during start-up) or our `Ctx`, alive as long as the plugin.
    let ctx = unsafe {
        if effect.is_null() || (*effect).user.is_null() {
            None
        } else {
            Some(&*((*effect).user as *const Ctx))
        }
    };
    match opcode {
        MASTER_VERSION => 2400,
        // Deprecated, but old plugins ask it before they take MIDI: yes.
        MASTER_WANT_MIDI => 1,
        MASTER_CURRENT_ID => isize::try_from(SHELL_ID.with(Cell::get)).unwrap_or(0),
        MASTER_IDLE | MASTER_UPDATE_DISPLAY => {
            if let Some(c) = ctx {
                c.waker.wake(HostEvent::MainThreadCallback);
            }
            1
        }
        MASTER_AUTOMATE => {
            // The plugin's own editor changed a parameter: the saved state is out of date.
            if let Some(c) = ctx {
                c.waker.wake(HostEvent::StateDirty);
            }
            0
        }
        MASTER_GET_TIME => {
            let Some(c) = ctx else { return 0 };
            let tempo = f64::from(host_time::tempo());
            let beats = host_time::beats_per_bar() as i32;
            // SAFETY: see `Ctx`.
            let t = unsafe { &mut *c.time.get() };
            *t = TimeInfo {
                sample_rate: c.rate(),
                tempo,
                time_sig_numerator: beats,
                time_sig_denominator: host_time::beat_unit() as i32,
                flags: TIME_TEMPO_VALID
                    | TIME_SIG_VALID
                    | if host_time::playing() {
                        TIME_TRANSPORT_PLAYING
                    } else {
                        0
                    },
                ..TimeInfo::default()
            };
            t as *mut TimeInfo as isize
        }
        MASTER_SIZE_WINDOW => {
            if let Some(c) = ctx {
                c.waker.wake(HostEvent::EditorResize {
                    width: index.max(1) as u32,
                    height: (value as i32).max(1) as u32,
                });
            }
            1
        }
        MASTER_GET_SAMPLE_RATE => ctx.map_or(48_000, |c| c.rate() as isize),
        MASTER_GET_BLOCK_SIZE => {
            ctx.map_or(1024, |c| c.block_size.load(Ordering::Relaxed) as isize)
        }
        MASTER_GET_VENDOR_STRING | MASTER_GET_PRODUCT_STRING => {
            // SAFETY: a 64-byte buffer from the plugin.
            unsafe { put_string(ptr, "Merlin") };
            1
        }
        MASTER_GET_VENDOR_VERSION => 1000,
        MASTER_GET_LANGUAGE => 1, // English
        MASTER_CAN_DO => {
            if ptr.is_null() {
                return 0;
            }
            // SAFETY: a NUL-terminated string from the plugin.
            let what = unsafe { CStr::from_ptr(ptr as *const c_char) }.to_string_lossy();
            isize::from(matches!(
                what.as_ref(),
                "sendVstEvents"
                    | "sendVstMidiEvent"
                    | "sendVstTimeInfo"
                    | "sizeWindow"
                    | "startStopProcess"
                    | "shellCategory"
            ))
        }
        _ => 0,
    }
}
