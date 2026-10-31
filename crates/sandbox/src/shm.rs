//! The audio path between the app and a plugin host process: one shared memory block per
//! plugin, and two counters to hand it back and forth.
//!
//! The app's audio thread fills the input, the MIDI events and the block header, bumps `req`
//! and wakes the host's audio thread. That thread processes the block in place and sets `done`
//! equal to `req`. Both sides wait on the counters with futexes (no sockets, no allocation).
//! Whoever owns the block (between `req` and `done`) is the only one touching the buffers.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};
use merlin_hosting::MidiEvent;

/// Longest block the shared buffers hold.
pub const MAX_FRAMES: usize = 8192;
/// Most MIDI events handed over per block; any more are dropped.
pub const MAX_EVENTS: usize = 2048;

#[repr(C)]
pub struct Header {
    /// The app bumps this to ask for a block.
    pub req: AtomicU32,
    /// The host sets this to `req` when the block is done.
    pub done: AtomicU32,
    /// The app sets this when the process is gone (or given up on): nobody is coming.
    pub dead: AtomicU32,
    /// The host's audio thread leaves when this is set.
    pub stop: AtomicU32,
    pub frames: AtomicU32,
    pub events: AtomicU32,
    pub has_input: AtomicU32,
    /// 1 when the plugin has no tail and is silent: the engine may skip it.
    pub sleep: AtomicU32,
    /// The app's audio thread scheduling, copied to the host's (policy 0 = leave it).
    pub sched_policy: AtomicU32,
    pub sched_priority: AtomicU32,
    /// What plugins may ask the host about time ([`merlin_hosting::host_time`]).
    pub tempo_bits: AtomicU32,
    pub playing: AtomicU32,
    pub beats_per_bar: AtomicU32,
    pub beat_unit: AtomicU32,
}

/// A MIDI event as it sits in the block (same fields as [`MidiEvent`], with a fixed layout).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawEvent {
    time: u32,
    input: u8,
    data: [u8; 3],
}

const HEADER_BYTES: usize = 128;
const EVENTS_BYTES: usize = MAX_EVENTS * std::mem::size_of::<RawEvent>();
const BUF_BYTES: usize = MAX_FRAMES * 4;
const SIZE: usize = HEADER_BYTES + EVENTS_BYTES + 4 * BUF_BYTES;

const _: () = assert!(std::mem::size_of::<Header>() <= HEADER_BYTES);
const _: () = assert!(std::mem::size_of::<RawEvent>() == 8);

pub struct Shm {
    base: NonNull<u8>,
    fd: OwnedFd,
}

// SAFETY: the memory is only touched under the req/done protocol above, and the counters are atomics.
unsafe impl Send for Shm {}
unsafe impl Sync for Shm {}

impl Shm {
    /// Creates a zeroed block (app side).
    pub fn create() -> Result<Self> {
        // SAFETY: plain syscalls; the fd is ours.
        let fd = unsafe {
            let raw = libc::memfd_create(c"merlin-audio".as_ptr(), libc::MFD_CLOEXEC);
            if raw < 0 {
                bail!("memfd_create: {}", std::io::Error::last_os_error());
            }
            OwnedFd::from_raw_fd(raw)
        };
        // SAFETY: sizing the memfd we just made.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), SIZE as libc::off_t) } != 0 {
            bail!("ftruncate: {}", std::io::Error::last_os_error());
        }
        Self::map(fd)
    }

    /// Maps a block made by the app (host side).
    ///
    /// # Safety
    /// `fd` must be an open descriptor the caller owns, for a block made by [`Shm::create`].
    pub unsafe fn from_fd(fd: i32) -> Result<Self> {
        // SAFETY: by the caller's promise.
        Self::map(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn map(fd: OwnedFd) -> Result<Self> {
        // SAFETY: mapping SIZE bytes of a memfd of that size.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if p == libc::MAP_FAILED {
            bail!("mmap: {}", std::io::Error::last_os_error());
        }
        Ok(Self {
            base: NonNull::new(p.cast()).expect("mmap result"),
            fd,
        })
    }

    pub fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    pub fn header(&self) -> &Header {
        // SAFETY: the block is at least HEADER_BYTES, page aligned, and all-zero is a valid Header.
        unsafe { &*self.base.as_ptr().cast::<Header>() }
    }

    fn events_ptr(&self) -> *mut RawEvent {
        // SAFETY: inside the block.
        unsafe { self.base.as_ptr().add(HEADER_BYTES).cast() }
    }

    fn buf(&self, n: usize) -> *mut f32 {
        // SAFETY: inside the block; the buffers follow the events.
        unsafe {
            self.base
                .as_ptr()
                .add(HEADER_BYTES + EVENTS_BYTES + n * BUF_BYTES)
                .cast()
        }
    }

    /// The `n`th buffer (0 = in left, 1 = in right, 2 = out left, 3 = out right).
    ///
    /// # Safety
    /// The caller must own the block (see the module docs) and not hold another reference to
    /// the same buffer.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn buffer(&self, n: usize, frames: usize) -> &mut [f32] {
        // SAFETY: by the caller's promise; frames is clamped to the buffer.
        unsafe { std::slice::from_raw_parts_mut(self.buf(n), frames.min(MAX_FRAMES)) }
    }

    /// Copies events into the block (app side). Returns how many fitted.
    ///
    /// # Safety
    /// The caller must own the block.
    pub unsafe fn put_events(&self, events: &[MidiEvent]) -> usize {
        let n = events.len().min(MAX_EVENTS);
        for (i, e) in events[..n].iter().enumerate() {
            // SAFETY: i < MAX_EVENTS.
            unsafe {
                self.events_ptr().add(i).write(RawEvent {
                    time: e.time,
                    input: e.input,
                    data: e.data,
                });
            }
        }
        n
    }

    /// Reads the events out of the block into `out` (host side; `out` has room for MAX_EVENTS).
    ///
    /// # Safety
    /// The caller must own the block.
    pub unsafe fn take_events(&self, count: usize, out: &mut Vec<MidiEvent>) {
        out.clear();
        for i in 0..count.min(MAX_EVENTS) {
            // SAFETY: i < MAX_EVENTS.
            let e = unsafe { self.events_ptr().add(i).read() };
            out.push(MidiEvent {
                time: e.time,
                input: e.input,
                data: e.data,
            });
        }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        // SAFETY: unmapping what `map` mapped.
        unsafe { libc::munmap(self.base.as_ptr().cast(), SIZE) };
    }
}

/// Sleeps while `*word == expected`, for at most `timeout` (forever for `None`). Spurious
/// returns are fine: callers re-check the word. Works across processes (a shared futex).
pub fn futex_wait(word: &AtomicU32, expected: u32, timeout: Option<Duration>) {
    let ts = timeout.map(|d| libc::timespec {
        tv_sec: d.as_secs() as libc::time_t,
        tv_nsec: d.subsec_nanos() as libc::c_long,
    });
    // SAFETY: FUTEX_WAIT on a live atomic; the timeout pointer is valid for the call.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAIT,
            expected,
            ts.as_ref()
                .map_or(std::ptr::null(), |t| t as *const libc::timespec),
            std::ptr::null::<u32>(),
            0,
        );
    }
}

pub fn futex_wake(word: &AtomicU32) {
    // SAFETY: FUTEX_WAKE on a live atomic.
    unsafe {
        libc::syscall(libc::SYS_futex, word.as_ptr(), libc::FUTEX_WAKE, i32::MAX);
    }
}

/// Waits until `word` is no longer `unchanged`, up to `timeout`, spinning briefly first (the
/// other side usually answers within microseconds). Returns the value it saw.
pub fn wait_change(word: &AtomicU32, unchanged: u32, timeout: Duration) -> u32 {
    for _ in 0..200 {
        let v = word.load(Ordering::Acquire);
        if v != unchanged {
            return v;
        }
        std::hint::spin_loop();
    }
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let v = word.load(Ordering::Acquire);
        if v != unchanged {
            return v;
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return v;
        }
        futex_wait(word, unchanged, Some(left));
    }
}
