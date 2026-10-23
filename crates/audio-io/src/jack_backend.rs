//! The JACK backend (plan D3): runs the [`Engine`] in the JACK process callback.
//!
//! It works the same on PipeWire (preferred) and JACK2. Opening is two-phase because the
//! engine needs the sample rate: [`JackClient::open`] connects and reports the rate and
//! buffer size, [`JackClient::start`] takes the engine and goes live.
//!
//! MIDI: one JACK port per engine input (`midi_in_1`..), so slots can listen to one device,
//! plus a lock-free virtual input (engine input 0) for the on-screen keyboard.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use jack::{
    AudioOut, Client, ClientOptions, Control, MidiIn, MidiOut, NotificationHandler, Port,
    PortFlags, RawMidi,
};
use merlin_engine::{ClickOut, Engine, MAX_INPUTS, MidiOutEvent};
use merlin_hosting::MidiEvent;

/// The `input` of a [`ControlEvent`] that reports a timeline marker (its id is in `data`).
pub const MARKER_INPUT: u8 = 0xFE;

/// Engine input index of the virtual keyboard.
pub const VIRTUAL_INPUT: u8 = 0;
/// Hardware MIDI inputs the backend can serve at once.
pub const HARDWARE_INPUTS: usize = MAX_INPUTS - 1;

const MAX_MIDI_PER_CYCLE: usize = 512;
const VIRTUAL_QUEUE: usize = 256;
const CONTROL_QUEUE: usize = 512;
const DEVICE_QUEUE: usize = 256;
/// Messages to controllers written per cycle; the rest wait for the next one.
const MAX_DEVICE_PER_CYCLE: usize = 32;
/// Controllers the backend can talk to at once (one MIDI output port each).
pub const DEVICE_OUTPUTS: usize = 2;
/// Longest message to a controller: a text line is 16 characters plus a header.
pub const DEVICE_MESSAGE_MAX: usize = 48;

/// One MIDI message for a controller (LEDs, a screen, a mode switch), sent on its own output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceMessage {
    /// Which output (0..[`DEVICE_OUTPUTS`]).
    pub port: u8,
    len: u8,
    data: [u8; DEVICE_MESSAGE_MAX],
}

impl DeviceMessage {
    /// `None` when it is empty or too long.
    pub fn new(port: u8, bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > DEVICE_MESSAGE_MAX || port as usize >= DEVICE_OUTPUTS {
            return None;
        }
        let mut data = [0; DEVICE_MESSAGE_MAX];
        data[..bytes.len()].copy_from_slice(bytes);
        Some(Self {
            port,
            len: bytes.len() as u8,
            data,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

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

/// What a connected MIDI device is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InputRole {
    /// Played: its notes go to the instruments.
    #[default]
    Keyboard,
    /// A control surface (pads, knobs, faders): its messages never reach the instruments. They
    /// are counted, so the rack can show activity, and are kept for the control mapping to come.
    Controller,
}

impl InputRole {
    fn bits(self) -> u8 {
        match self {
            InputRole::Keyboard => 0,
            InputRole::Controller => 1,
        }
    }
}

/// The same name for a port we can send to: `Midi-Bridge:Launchkey MK3 49: LKMK3 DAW In
/// (playback)` -> `Launchkey MK3 49: LKMK3 DAW In`, so it can be matched with the source of the
/// same device.
pub fn sink_friendly_name(port: &str) -> String {
    if let Some(rest) = port.strip_prefix("Midi-Bridge:") {
        let rest = rest.trim();
        return match rest.rfind('(') {
            Some(i) if rest.ends_with(')') && rest[i..].contains("playback") => {
                rest[..i].trim_end().to_owned()
            }
            _ => rest.to_owned(),
        };
    }
    match port.rsplit_once(':') {
        Some((client, "in" | "input" | "midi_in" | "playback")) => client.to_owned(),
        _ => port.to_owned(),
    }
}

/// A device name a person can read, from the JACK port name PipeWire gives it:
/// `Midi-Bridge:Launchkey MK3 25: Launchkey MK3 25 MIDI (capture)` -> `Launchkey MK3 25: Launchkey MK3 25 MIDI`,
/// `BLE MIDI 1:out` -> `BLE MIDI 1`.
pub fn friendly_name(port: &str) -> String {
    if let Some(rest) = port.strip_prefix("Midi-Bridge:") {
        let rest = rest.trim();
        // Drop the direction PipeWire appends: " (capture)", "(capture_0)".
        let rest = match rest.rfind('(') {
            Some(i) if rest.ends_with(')') && rest[i..].contains("capture") => rest[..i].trim_end(),
            _ => rest,
        };
        return rest.to_owned();
    }
    match port.rsplit_once(':') {
        Some((client, tail))
            if matches!(tail, "out" | "output" | "midi_out" | "capture")
                || tail.starts_with("capture_")
                || tail.starts_with("midi_out") =>
        {
            client.to_owned()
        }
        _ => port.to_owned(),
    }
}

/// A control message the audio thread hands to the main thread: from a controller device, or
/// from a keyboard when something is mapped to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlEvent {
    /// Engine input (1 and up), which says which device sent it.
    pub input: u8,
    /// Status byte and two data bytes.
    pub data: [u8; 3],
}

/// What the main thread wants to hear about from keyboards. Messages that are mapped to
/// something never reach the instruments.
#[derive(Default)]
pub struct InputInterest {
    cc: [AtomicU64; 2],
    notes: [AtomicU64; 2],
    program: AtomicBool,
}

impl InputInterest {
    fn bit(words: &[AtomicU64; 2], n: u8) -> bool {
        words[(n >> 6) as usize & 1].load(Ordering::Relaxed) & (1 << (n & 63)) != 0
    }

    fn set(words: &[AtomicU64; 2], n: u8) {
        words[(n >> 6) as usize & 1].fetch_or(1 << (n & 63), Ordering::Relaxed);
    }

    pub fn clear(&self) {
        for w in self.cc.iter().chain(&self.notes) {
            w.store(0, Ordering::Relaxed);
        }
        self.program.store(false, Ordering::Relaxed);
    }

    pub fn add_cc(&self, n: u8) {
        Self::set(&self.cc, n);
    }

    pub fn add_note(&self, n: u8) {
        Self::set(&self.notes, n);
    }

    pub fn add_program(&self) {
        self.program.store(true, Ordering::Relaxed);
    }
}

/// [`InputInterest`] for every hardware input, plus "learning": while the user teaches a
/// mapping, every control message is reported.
#[derive(Default)]
pub struct Interest {
    pub inputs: [InputInterest; HARDWARE_INPUTS],
    pub learning: AtomicBool,
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
    /// Channel messages received per engine input (index = input), for the activity lights.
    pub midi_activity: [AtomicU32; merlin_engine::MAX_INPUTS],
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
        // The click (its own output pair) and the MIDI clock.
        let click_l = client.register_port("click_l", AudioOut::default())?;
        let click_r = client.register_port("click_r", AudioOut::default())?;
        let clock_out = client.register_port("midi_out", MidiOut::default())?;
        let click_names = [click_l.name()?, click_r.name()?];
        let clock_name = clock_out.name()?;
        let device_out = [
            client.register_port("device_out_1", MidiOut::default())?,
            client.register_port("device_out_2", MidiOut::default())?,
        ];
        let device_names = [device_out[0].name()?, device_out[1].name()?];
        let (device_tx, device_rx) = rtrb::RingBuffer::new(DEVICE_QUEUE);

        let (virtual_midi, virtual_rx) = rtrb::RingBuffer::new(VIRTUAL_QUEUE);
        let roles: Arc<[AtomicU8; HARDWARE_INPUTS]> = Arc::default();
        let interest: Arc<Interest> = Arc::default();
        let (control_tx, control_rx) = rtrb::RingBuffer::new(CONTROL_QUEUE);
        let stats = Arc::new(Stats::default());
        let callback = Callback {
            engine,
            midi_in,
            out_l,
            out_r,
            click_l,
            click_r,
            clock_out,
            clock: Vec::with_capacity(512),
            device_out,
            device_rx,
            virtual_rx,
            roles: roles.clone(),
            interest: interest.clone(),
            control_tx,
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
            roles,
            interest,
            control_rx: Some(control_rx),
            device_tx,
            device_names,
            out_names,
            click_names,
            clock_name,
            sample_rate: self.sample_rate,
        })
    }
}

struct Callback {
    engine: Engine,
    midi_in: Vec<Port<MidiIn>>,
    out_l: Port<AudioOut>,
    out_r: Port<AudioOut>,
    click_l: Port<AudioOut>,
    click_r: Port<AudioOut>,
    clock_out: Port<MidiOut>,
    clock: Vec<MidiOutEvent>,
    device_out: [Port<MidiOut>; DEVICE_OUTPUTS],
    device_rx: rtrb::Consumer<DeviceMessage>,
    virtual_rx: rtrb::Consumer<MidiEvent>,
    /// [`InputRole`] bits per hardware input, set from the main thread.
    roles: Arc<[AtomicU8; HARDWARE_INPUTS]>,
    interest: Arc<Interest>,
    control_tx: rtrb::Producer<ControlEvent>,
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
            let controller = self.roles[i].load(Ordering::Relaxed) == InputRole::Controller.bits();
            for raw in self.midi_in[i].iter(ps) {
                // Channel messages only (status 0x80..0xEF), up to three bytes.
                if !(1..=3).contains(&raw.bytes.len()) || !(0x80..0xF0).contains(&raw.bytes[0]) {
                    continue;
                }
                self.stats.midi_activity[i + 1].fetch_add(1, Ordering::Relaxed);
                let mut data = [0u8; 3];
                data[..raw.bytes.len()].copy_from_slice(raw.bytes);
                // Notes, control changes and program changes can be mapped to actions: those
                // are reported to the main thread, and a mapped one does not play anything.
                let wanted = &self.interest.inputs[i];
                let learning = self.interest.learning.load(Ordering::Relaxed);
                let (reported, consumed) = match data[0] & 0xF0 {
                    0x80 | 0x90 => {
                        let mapped = InputInterest::bit(&wanted.notes, data[1]);
                        (controller || mapped || learning, controller || mapped)
                    }
                    0xB0 => {
                        let mapped = InputInterest::bit(&wanted.cc, data[1]);
                        (controller || mapped || learning, controller || mapped)
                    }
                    0xC0 => {
                        let mapped = wanted.program.load(Ordering::Relaxed);
                        (controller || mapped || learning, controller || mapped)
                    }
                    // Pitch bend, aftertouch: a controller's are of no use to an instrument.
                    _ => (false, controller),
                };
                if reported {
                    let _ = self.control_tx.push(ControlEvent {
                        input: i as u8 + 1,
                        data,
                    });
                }
                if consumed {
                    continue;
                }
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
        let (cl, cr) = (self.click_l.as_mut_slice(ps), self.click_r.as_mut_slice(ps));
        cl[..n].fill(0.0);
        cr[..n].fill(0.0);
        self.clock.clear();
        let mut click = ClickOut {
            l: &mut cl[..n],
            r: &mut cr[..n],
            clock: &mut self.clock,
        };
        self.engine
            .process_with_click(&self.events, &mut l[..n], &mut r[..n], Some(&mut click));
        for &id in self.engine.fired_markers() {
            let _ = self.control_tx.push(ControlEvent {
                input: MARKER_INPUT,
                data: [(id >> 8) as u8, id as u8, 0],
            });
        }
        let mut writer = self.clock_out.writer(ps);
        for e in &self.clock {
            let _ = writer.write(&RawMidi {
                time: e.time,
                bytes: e.data(),
            });
        }

        // Messages for controllers: every port's buffer is taken each cycle (that clears it).
        let [pa, pb] = &mut self.device_out;
        let mut writers = [pa.writer(ps), pb.writer(ps)];
        for _ in 0..MAX_DEVICE_PER_CYCLE {
            let Ok(m) = self.device_rx.pop() else { break };
            let _ = writers[m.port as usize].write(&RawMidi {
                time: 0,
                bytes: m.bytes(),
            });
        }

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
    roles: Arc<[AtomicU8; HARDWARE_INPUTS]>,
    /// What keyboards should report; the main thread fills it from the mappings.
    pub interest: Arc<Interest>,
    control_rx: Option<rtrb::Consumer<ControlEvent>>,
    pub out_names: [String; 2],
    /// Our click output pair and the MIDI clock output.
    pub click_names: [String; 2],
    pub clock_name: String,
    /// Messages for controllers go here; they leave on the matching output of `device_names`.
    pub device_tx: rtrb::Producer<DeviceMessage>,
    pub device_names: [String; DEVICE_OUTPUTS],
    pub sample_rate: u32,
}

/// A hardware MIDI source and the engine input it was connected to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiSource {
    /// Engine input index (1 and up).
    pub input: u8,
    /// The source's JACK port name, which is also its device name for filters.
    pub device: String,
    pub role: InputRole,
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

    /// The queue of control messages. Taken once, by whoever forwards them to the main thread.
    pub fn take_control(&mut self) -> Option<rtrb::Consumer<ControlEvent>> {
        self.control_rx.take()
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

    /// The physical audio playback ports (the interface's outputs), for choosing where the
    /// click goes.
    pub fn playback_ports(&self) -> Vec<String> {
        self.client.as_client().ports(
            None,
            Some("32 bit float mono audio"),
            PortFlags::IS_PHYSICAL | PortFlags::IS_INPUT,
        )
    }

    /// Sends the click to these two playback ports (and nowhere else), or to none.
    pub fn connect_click(&self, to: Option<[&str; 2]>) {
        let c = self.client.as_client();
        for (src, dst) in self.click_names.iter().zip(to.unwrap_or(["", ""])) {
            if let Some(port) = c.port_by_name(src) {
                for existing in port.get_connections() {
                    let _ = c.disconnect_ports_by_name(src, &existing);
                }
            }
            if !dst.is_empty() {
                let _ = c.connect_ports_by_name(src, dst);
            }
        }
    }

    /// Hardware MIDI input ports (what we can send to) right now, not ours.
    pub fn hardware_midi_sinks(&self) -> Vec<String> {
        let c = self.client.as_client();
        let mut ports = c.ports(None, Some("8 bit raw midi"), PortFlags::IS_INPUT);
        ports.retain(|p| !self.device_names.contains(p) && !p.starts_with(c.name()));
        ports
    }

    /// Sends controller output `n` to this port, and nowhere else (`None`: to nothing).
    pub fn connect_device_out(&self, n: usize, to: Option<&str>) {
        let c = self.client.as_client();
        let Some(src) = self.device_names.get(n) else {
            return;
        };
        if let Some(port) = c.port_by_name(src) {
            for existing in port.get_connections() {
                if Some(existing.as_str()) != to {
                    let _ = c.disconnect_ports_by_name(src, &existing);
                }
            }
            if let Some(dst) = to
                && !port.is_connected_to(dst).unwrap_or(false)
            {
                let _ = c.connect_ports_by_name(src, dst);
            }
        }
    }

    /// Queues a message for a controller; false if the queue is full.
    pub fn send_to_device(&mut self, message: DeviceMessage) -> bool {
        self.device_tx.push(message).is_ok()
    }

    /// Hardware MIDI output ports right now (not ours).
    pub fn hardware_midi_sources(&self) -> Vec<String> {
        let c = self.client.as_client();
        let mut ports = c.ports(None, Some("8 bit raw midi"), PortFlags::IS_OUTPUT);
        ports.retain(|p| !self.midi_names.contains(p) && !p.starts_with(c.name()));
        ports
    }

    /// Connects each wanted hardware MIDI source to its own engine input (up to the limit),
    /// keeping existing assignments: a source that is already connected to one of our ports stays
    /// on it. `wanted` says what to do with a source (by JACK port name): `Some(role)` to use it,
    /// `None` to leave it unconnected (and disconnect it if it was). Returns the table of
    /// connected sources.
    pub fn connect_midi_sources(
        &self,
        wanted: &dyn Fn(&str) -> Option<InputRole>,
    ) -> Vec<MidiSource> {
        let c = self.client.as_client();
        let mut table: Vec<Option<(String, InputRole)>> = vec![None; HARDWARE_INPUTS];
        // Keep what is already connected, unless it is no longer wanted.
        for (i, name) in self.midi_names.iter().enumerate() {
            let Some(port) = c.port_by_name(name) else {
                continue;
            };
            for source in port.get_connections() {
                match wanted(&source) {
                    Some(role) if table[i].is_none() => table[i] = Some((source, role)),
                    _ => {
                        let _ = c.disconnect_ports_by_name(&source, name);
                    }
                }
            }
        }
        for source in self.hardware_midi_sources() {
            let Some(role) = wanted(&source) else {
                continue;
            };
            if table
                .iter()
                .any(|t| t.as_ref().is_some_and(|(s, _)| *s == source))
            {
                continue;
            }
            if let Some(i) = table.iter().position(Option::is_none)
                && c.connect_ports_by_name(&source, &self.midi_names[i])
                    .is_ok()
            {
                table[i] = Some((source, role));
            }
        }
        // The roles take effect on the audio thread; unused inputs go back to keyboard.
        for (i, entry) in table.iter().enumerate() {
            let role = entry.as_ref().map_or(InputRole::Keyboard, |(_, r)| *r);
            self.roles[i].store(role.bits(), Ordering::Relaxed);
        }
        table
            .into_iter()
            .enumerate()
            .filter_map(|(i, d)| {
                d.map(|(device, role)| MidiSource {
                    input: i as u8 + 1,
                    device,
                    role,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_names_become_device_names() {
        assert_eq!(
            friendly_name("Midi-Bridge:Midi Through: Port-0 (capture)"),
            "Midi Through: Port-0"
        );
        assert_eq!(
            friendly_name("Midi-Bridge:Launchkey MK3 25: Launchkey MK3 25 MIDI (capture_0)"),
            "Launchkey MK3 25: Launchkey MK3 25 MIDI"
        );
        assert_eq!(friendly_name("BLE MIDI 1:out"), "BLE MIDI 1");
        assert_eq!(friendly_name("Fake Keyboard:midi_out_1"), "Fake Keyboard");
        // A device's output side has the same readable name as its input side.
        assert_eq!(
            sink_friendly_name("Midi-Bridge:Launchkey MK3 49: LKMK3 DAW In (playback)"),
            friendly_name("Midi-Bridge:Launchkey MK3 49: LKMK3 DAW In (capture)")
        );
        assert_eq!(sink_friendly_name("Fake Launchkey:in"), "Fake Launchkey");
        assert!(DeviceMessage::new(0, &[0x9F, 0x0C, 0x7F]).is_some());
        assert!(DeviceMessage::new(0, &[]).is_none());
        assert!(DeviceMessage::new(0, &[0; DEVICE_MESSAGE_MAX + 1]).is_none());
        assert!(DeviceMessage::new(DEVICE_OUTPUTS as u8, &[1]).is_none());
        // Anything unrecognised is kept whole, so two different ports never share a name.
        assert_eq!(friendly_name("synth:keys"), "synth:keys");
        assert_eq!(friendly_name("plain"), "plain");
    }
}
