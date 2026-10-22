//! The JACK backend (plan D3): runs the [`Engine`] in the JACK process callback.
//!
//! It works the same on PipeWire (preferred) and JACK2. Opening is two-phase because the
//! engine needs the sample rate: [`JackClient::open`] connects and reports the rate and
//! buffer size, [`JackClient::start`] takes the engine and goes live.
//!
//! MIDI: one JACK port per engine input (`midi_in_1`..), so slots can listen to one device,
//! plus a lock-free virtual input (engine input 0) for the on-screen keyboard.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use merlin_engine::{Engine, MAX_INPUTS};
use merlin_hosting::MidiEvent;
use jack::{
    AudioOut, Client, ClientOptions, Control, MidiIn, NotificationHandler, Port, PortFlags,
};

/// Engine input index of the virtual keyboard.
pub const VIRTUAL_INPUT: u8 = 0;
/// Hardware MIDI inputs the backend can serve at once.
pub const HARDWARE_INPUTS: usize = MAX_INPUTS - 1;

const MAX_MIDI_PER_CYCLE: usize = 512;
const VIRTUAL_QUEUE: usize = 256;

/// Makes the `jack` crate load PipeWire's libjack instead of jackd2's (S1 finding): `dlopen`
/// PipeWire's copy by absolute path first, so the soname lookup finds it. A no-op where
/// PipeWire's libjack is the system one.
pub fn preload_pipewire_jack() {
    for path in [
        "/usr/lib/x86_64-linux-gnu/pipewire-0.3/jack/libjack.so.0",
        "/usr/lib64/pipewire-0.3/jack/libjack.so.0",
        "/usr/lib/pipewire-0.3/jack/libjack.so.0",
    ] {
        let c = std::ffi::CString::new(path).unwrap();
        // SAFETY: loading a library by absolute path; the handle is intentionally leaked.
        if !unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) }.is_null() {
            return;
        }
    }
}

/// What [`lock_memory`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemLock {
    /// All current and future memory is locked in RAM.
    Locked,
    /// Not attempted: `RLIMIT_MEMLOCK` is too low (the value, in bytes). Locking under a small
    /// limit is worse than not locking: with `MCL_FUTURE` every later mapping (a plugin's
    /// `dlopen`) counts against the limit and fails ("failed to map segment", seen in S-phase 1).
    LimitTooLow(u64),
    Failed,
}

/// Locks all memory so the audio path never page-faults, but only if `RLIMIT_MEMLOCK` is
/// unlimited (what the `audio`/`pipewire` groups' limits.d files grant).
pub fn lock_memory() -> MemLock {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain syscall writing into our struct.
    if unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut limit) } != 0 {
        return MemLock::Failed;
    }
    if limit.rlim_cur != libc::RLIM_INFINITY {
        return MemLock::LimitTooLow(limit.rlim_cur);
    }
    // SAFETY: mlockall has no memory-safety preconditions.
    if unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) } == 0 {
        MemLock::Locked
    } else {
        MemLock::Failed
    }
}

/// Counters the callback publishes.
#[derive(Default)]
pub struct Stats {
    pub xruns: AtomicU32,
    pub cycles: AtomicU64,
    /// Longest callback since last read, in nanoseconds (`swap(0)` to read and reset).
    pub max_ns: AtomicU64,
    /// Duration of the latest callback.
    pub last_ns: AtomicU64,
    /// MIDI events dropped because a cycle had too many.
    pub dropped_midi: AtomicU32,
    /// Set when a port appeared or went away (a device was plugged in or out).
    pub ports_changed: AtomicBool,
}

struct Notifications(Arc<Stats>);

impl NotificationHandler for Notifications {
    fn xrun(&mut self, _: &Client) -> Control {
        self.0.xruns.fetch_add(1, Ordering::Relaxed);
        Control::Continue
    }

    fn port_registration(&mut self, _: &Client, _: jack::PortId, _: bool) {
        self.0.ports_changed.store(true, Ordering::Relaxed);
    }
}

/// An open, not yet running client.
pub struct JackClient {
    client: Client,
    pub sample_rate: u32,
    pub buffer_size: u32,
}

impl JackClient {
    /// Connects to the JACK server (PipeWire's, ideally). `frames` asks PipeWire for that
    /// buffer size (the quantum); it is ignored by a plain JACK server.
    pub fn open(name: &str, frames: Option<u32>) -> Result<Self> {
        preload_pipewire_jack();
        if let Some(frames) = frames {
            // SAFETY: called during startup, before audio threads read the environment.
            unsafe { std::env::set_var("PIPEWIRE_QUANTUM", format!("{frames}/48000")) };
        }
        let (client, _) = Client::new(name, ClientOptions::NO_START_SERVER)
            .context("cannot connect to the JACK/PipeWire server")?;
        Ok(Self {
            sample_rate: client.sample_rate(),
            buffer_size: client.buffer_size(),
            client,
        })
    }

    /// Registers the ports, activates, and runs `engine` from now on.
    pub fn start(self, engine: Engine) -> Result<JackBackend> {
        let client = self.client;
        let midi_in = (0..HARDWARE_INPUTS)
            .map(|i| client.register_port(&format!("midi_in_{}", i + 1), MidiIn::default()))
            .collect::<Result<Vec<_>, _>>()?;
        let out_l = client.register_port("out_l", AudioOut::default())?;
        let out_r = client.register_port("out_r", AudioOut::default())?;
        let midi_names = midi_in
            .iter()
            .map(|p| p.name())
            .collect::<Result<Vec<_>, _>>()?;
        let out_names = [out_l.name()?, out_r.name()?];

        let (virtual_midi, virtual_rx) = rtrb::RingBuffer::new(VIRTUAL_QUEUE);
        let stats = Arc::new(Stats::default());
        let callback = Callback {
            engine,
            midi_in,
            out_l,
            out_r,
            virtual_rx,
            events: Vec::with_capacity(MAX_MIDI_PER_CYCLE),
            stats: stats.clone(),
            ftz_set: false,
        };
        let client = client.activate_async(Notifications(stats.clone()), callback)?;
        Ok(JackBackend {
            client,
            stats,
            virtual_midi,
            midi_names,
            out_names,
            sample_rate: self.sample_rate,
        })
    }
}

struct Callback {
    engine: Engine,
    midi_in: Vec<Port<MidiIn>>,
    out_l: Port<AudioOut>,
    out_r: Port<AudioOut>,
    virtual_rx: rtrb::Consumer<MidiEvent>,
    events: Vec<MidiEvent>,
    stats: Arc<Stats>,
    ftz_set: bool,
}

/// Flush denormals to zero (FTZ + DAZ) on this thread: plugins' decaying tails otherwise cost
/// 100x per sample.
fn set_flush_to_zero() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: reads and sets this thread's MXCSR; no memory is touched.
    unsafe {
        let mut csr: u32 = 0;
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
        csr |= 0x8040;
        std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack));
    }
}

/// Inserts keeping time order; stable for equal times (a note-off before a note-on at the
/// same frame must stay in that order).
fn push_sorted(events: &mut Vec<MidiEvent>, stats: &Stats, ev: MidiEvent) {
    if events.len() >= MAX_MIDI_PER_CYCLE {
        stats.dropped_midi.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let at = events
        .iter()
        .rposition(|e| e.time <= ev.time)
        .map_or(0, |i| i + 1);
    events.insert(at, ev);
}

impl jack::ProcessHandler for Callback {
    fn process(&mut self, _: &Client, ps: &jack::ProcessScope) -> Control {
        let t0 = Instant::now();
        if !self.ftz_set {
            set_flush_to_zero();
            self.ftz_set = true;
        }
        self.events.clear();
        for i in 0..self.midi_in.len() {
            for raw in self.midi_in[i].iter(ps) {
                // Channel messages only (status 0x80..0xEF), up to three bytes.
                if !(1..=3).contains(&raw.bytes.len()) || !(0x80..0xF0).contains(&raw.bytes[0]) {
                    continue;
                }
                let mut data = [0u8; 3];
                data[..raw.bytes.len()].copy_from_slice(raw.bytes);
                push_sorted(
                    &mut self.events,
                    &self.stats,
                    MidiEvent::new(raw.time, i as u8 + 1, data),
                );
            }
        }
        while let Ok(mut ev) = self.virtual_rx.pop() {
            ev.input = VIRTUAL_INPUT;
            ev.time = 0;
            push_sorted(&mut self.events, &self.stats, ev);
        }
        let n = ps.n_frames() as usize;
        let (l, r) = (self.out_l.as_mut_slice(ps), self.out_r.as_mut_slice(ps));
        self.engine.process(&self.events, &mut l[..n], &mut r[..n]);

        let ns = t0.elapsed().as_nanos() as u64;
        self.stats.last_ns.store(ns, Ordering::Relaxed);
        self.stats.max_ns.fetch_max(ns, Ordering::Relaxed);
        self.stats.cycles.fetch_add(1, Ordering::Relaxed);
        Control::Continue
    }
}

/// A running backend.
pub struct JackBackend {
    client: jack::AsyncClient<Notifications, Callback>,
    pub stats: Arc<Stats>,
    /// Events pushed here play as engine input 0 (the on-screen keyboard).
    pub virtual_midi: rtrb::Producer<MidiEvent>,
    /// JACK port names of our MIDI inputs, index = engine input - 1.
    pub midi_names: Vec<String>,
    pub out_names: [String; 2],
    pub sample_rate: u32,
}

/// A hardware MIDI source and the engine input it was connected to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiSource {
    /// Engine input index (1 and up).
    pub input: u8,
    /// The source's JACK port name, which is also its device name for filters.
    pub device: String,
}

impl JackBackend {
    pub fn buffer_size(&self) -> u32 {
        self.client.as_client().buffer_size()
    }

    /// Asks the server for a different buffer size. Returns the size now in effect.
    pub fn set_buffer_size(&self, frames: u32) -> Result<u32> {
        self.client
            .as_client()
            .set_buffer_size(frames)
            .map_err(|e| anyhow::anyhow!("the server refused the buffer size: {e}"))?;
        Ok(self.buffer_size())
    }

    /// DSP load reported by the server, percent.
    pub fn cpu_load(&self) -> f32 {
        self.client.as_client().cpu_load()
    }

    /// True once after ports appeared or disappeared since the last call: time to look for new
    /// MIDI devices.
    pub fn take_ports_changed(&self) -> bool {
        self.stats.ports_changed.swap(false, Ordering::Relaxed)
    }

    /// Connects our outputs to the first two physical playback ports.
    pub fn connect_outputs(&self) -> Vec<String> {
        let c = self.client.as_client();
        let playback = c.ports(
            None,
            Some("32 bit float mono audio"),
            PortFlags::IS_PHYSICAL | PortFlags::IS_INPUT,
        );
        self.out_names
            .iter()
            .zip(&playback)
            .filter(|(src, dst)| c.connect_ports_by_name(src, dst).is_ok())
            .map(|(_, dst)| dst.clone())
            .collect()
    }

    /// Hardware MIDI output ports right now (not ours).
    pub fn hardware_midi_sources(&self) -> Vec<String> {
        let c = self.client.as_client();
        let mut ports = c.ports(None, Some("8 bit raw midi"), PortFlags::IS_OUTPUT);
        ports.retain(|p| !self.midi_names.contains(p) && !p.starts_with(c.name()));
        ports
    }

    /// Connects each hardware MIDI source to its own engine input (up to the limit), keeping
    /// existing assignments: a source that is already connected to one of our ports stays on
    /// it. Returns the full source -> input table.
    pub fn connect_midi_sources(&self) -> Vec<MidiSource> {
        let c = self.client.as_client();
        let mut table: Vec<Option<String>> = vec![None; HARDWARE_INPUTS];
        // Keep what is already connected.
        for (i, name) in self.midi_names.iter().enumerate() {
            if let Some(port) = c.port_by_name(name)
                && let Some(first) = port.get_connections().first()
            {
                table[i] = Some(first.clone());
            }
        }
        for source in self.hardware_midi_sources() {
            if table.iter().any(|t| t.as_deref() == Some(&source)) {
                continue;
            }
            if let Some(i) = table.iter().position(Option::is_none)
                && c.connect_ports_by_name(&source, &self.midi_names[i])
                    .is_ok()
            {
                table[i] = Some(source);
            }
        }
        table
            .into_iter()
            .enumerate()
            .filter_map(|(i, d)| {
                d.map(|device| MidiSource {
                    input: i as u8 + 1,
                    device,
                })
            })
            .collect()
    }

    /// Stops the callback and gives the engine back.
    pub fn stop(self) -> Result<Engine> {
        let (_, _, callback) = self.client.deactivate()?;
        Ok(callback.engine)
    }
}
