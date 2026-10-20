//! JACK side: runs the VST3 audio processor in the JACK process callback.
//!
//! The audio-thread COM objects (event list, parameter changes) are preallocated and
//! single-threaded: only the JACK thread, and the plugin from inside `process()`, touch them.

use std::cell::{Cell, UnsafeCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use jack::{
    AudioOut, Client, ClientOptions, Control, MidiIn, NotificationHandler, Port, PortFlags,
};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper};

use crate::host::Plugin;

pub const MAX_FRAMES: usize = 4096;
const MAX_QUEUES: usize = 64;
const MAX_POINTS: usize = 32;
const MAX_EVENTS: usize = 1024;

/// Messages to the audio thread: raw MIDI from the keyboard or UI, and parameter changes
/// (from the UI, or edits the plugin's own editor reported through the component handler).
#[derive(Clone, Copy, Debug)]
pub enum AudioMsg {
    Midi([u8; 3]),
    /// Parameter id and normalised value.
    Param(u32, f64),
}

/// Same trick as S1: make the jack crate pick up PipeWire's libjack.
pub fn preload_pipewire_jack() {
    for path in [
        "/usr/lib/x86_64-linux-gnu/pipewire-0.3/jack/libjack.so.0",
        "/usr/lib64/pipewire-0.3/jack/libjack.so.0",
    ] {
        let c = std::ffi::CString::new(path).unwrap();
        // SAFETY: loading a library by absolute path; handle intentionally leaked.
        if !unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) }.is_null() {
            return;
        }
    }
}

pub struct Notifications(pub Arc<AtomicU32>);

impl NotificationHandler for Notifications {
    fn xrun(&mut self, _: &Client) -> Control {
        self.0.fetch_add(1, Ordering::Relaxed);
        Control::Continue
    }
}

// ---------------------------------------------------------------------------------------------
// IEventList

pub struct EventList {
    events: UnsafeCell<Vec<Event>>,
}

// SAFETY: see the module docs: single-threaded use, only during one process() call at a time.
unsafe impl Send for EventList {}
unsafe impl Sync for EventList {}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl EventList {
    fn new() -> Self {
        Self {
            events: UnsafeCell::new(Vec::with_capacity(MAX_EVENTS)),
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn events(&self) -> &mut Vec<Event> {
        // SAFETY: single-threaded access, see above.
        unsafe { &mut *self.events.get() }
    }

    fn push(&self, e: Event) {
        let v = self.events();
        if v.len() < MAX_EVENTS {
            v.push(e);
        }
    }
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> i32 {
        self.events().len() as i32
    }

    unsafe fn getEvent(&self, index: i32, e: *mut Event) -> tresult {
        match self.events().get(index as usize) {
            Some(src) => {
                // SAFETY: the plugin passes a valid Event to fill.
                unsafe { *e = *src };
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        // SAFETY: the plugin passes a valid Event.
        self.push(unsafe { *e });
        kResultOk
    }
}

fn blank_event() -> Event {
    // SAFETY: Event is plain data; all-zero is a valid value.
    unsafe { std::mem::zeroed() }
}

// ---------------------------------------------------------------------------------------------
// IParameterChanges / IParamValueQueue

pub struct ParamQueue {
    id: Cell<u32>,
    len: Cell<usize>,
    points: UnsafeCell<[(i32, f64); MAX_POINTS]>,
}

// SAFETY: see the module docs.
unsafe impl Send for ParamQueue {}
unsafe impl Sync for ParamQueue {}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> i32 {
        self.len.get() as i32
    }

    unsafe fn getPoint(&self, index: i32, offset: *mut i32, value: *mut f64) -> tresult {
        if index < 0 || index as usize >= self.len.get() {
            return kInvalidArgument;
        }
        // SAFETY: bounds checked; the plugin passes valid out pointers.
        unsafe {
            let (o, v) = (*self.points.get())[index as usize];
            *offset = o;
            *value = v;
        }
        kResultOk
    }

    unsafe fn addPoint(&self, offset: i32, value: f64, index: *mut i32) -> tresult {
        let n = self.len.get();
        if n >= MAX_POINTS {
            return kResultFalse;
        }
        // SAFETY: bounds checked; single-threaded.
        unsafe { (*self.points.get())[n] = (offset, value) };
        self.len.set(n + 1);
        if !index.is_null() {
            // SAFETY: non-null out pointer from the plugin.
            unsafe { *index = n as i32 };
        }
        kResultOk
    }
}

pub struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    count: Cell<usize>,
}

// SAFETY: see the module docs.
unsafe impl Send for ParamChanges {}
unsafe impl Sync for ParamChanges {}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParamChanges {
    fn new() -> Self {
        Self {
            queues: (0..MAX_QUEUES)
                .map(|_| {
                    ComWrapper::new(ParamQueue {
                        id: Cell::new(0),
                        len: Cell::new(0),
                        points: UnsafeCell::new([(0, 0.0); MAX_POINTS]),
                    })
                })
                .collect(),
            count: Cell::new(0),
        }
    }

    fn clear(&self) {
        self.count.set(0);
        for q in &self.queues {
            q.len.set(0);
        }
    }

    /// The queue for `id`, claiming a free one if there is none yet.
    fn queue_for(&self, id: u32) -> Option<&ComWrapper<ParamQueue>> {
        let n = self.count.get();
        if let Some(q) = self.queues[..n].iter().find(|q| q.id.get() == id) {
            return Some(q);
        }
        let q = self.queues.get(n)?;
        q.id.set(id);
        q.len.set(0);
        self.count.set(n + 1);
        Some(q)
    }

    /// Output side: the last value of every queue the plugin wrote this block.
    fn drain(&self, mut f: impl FnMut(u32, f64)) {
        for q in &self.queues[..self.count.get()] {
            let n = q.len.get();
            if n > 0 {
                // SAFETY: single-threaded, n <= MAX_POINTS.
                let (_, value) = unsafe { (*q.points.get())[n - 1] };
                f(q.id.get(), value);
            }
        }
    }

    /// Input side: a single value at the start of the block (last write wins).
    fn set(&self, id: u32, value: f64) {
        if let Some(q) = self.queue_for(id) {
            q.len.set(0);
            // SAFETY: valid out pointer for the (ignored) index.
            unsafe { q.addPoint(0, value, std::ptr::null_mut()) };
        }
    }
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        self.count.get() as i32
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        match self
            .queues
            .get(index as usize)
            .filter(|_| (index as usize) < self.count.get())
        {
            Some(q) => q.as_com_ref::<IParamValueQueue>().unwrap().as_ptr(),
            None => std::ptr::null_mut(),
        }
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut i32,
    ) -> *mut IParamValueQueue {
        // SAFETY: the plugin passes a valid id pointer.
        let id = unsafe { *id };
        match self.queue_for(id) {
            Some(q) => {
                if !index.is_null() {
                    let i = self
                        .queues
                        .iter()
                        .position(|o| o.id.get() == id)
                        .unwrap_or(0);
                    // SAFETY: non-null out pointer from the plugin.
                    unsafe { *index = i as i32 };
                }
                q.as_com_ref::<IParamValueQueue>().unwrap().as_ptr()
            }
            None => std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Processor

struct BusBuffers {
    channels: Vec<Vec<f32>>,
    ptrs: Vec<*mut f32>,
}

impl BusBuffers {
    fn new(channels: i32) -> Self {
        let mut channels: Vec<Vec<f32>> = (0..channels.max(0))
            .map(|_| vec![0.0; MAX_FRAMES])
            .collect();
        let ptrs = channels.iter_mut().map(|c| c.as_mut_ptr()).collect();
        Self { channels, ptrs }
    }

    fn header(&mut self) -> AudioBusBuffers {
        AudioBusBuffers {
            numChannels: self.channels.len() as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: self.ptrs.as_mut_ptr(),
            },
        }
    }
}

pub struct Processor {
    processor: ComPtr<IAudioProcessor>,
    midi_in: Port<MidiIn>,
    out_l: Port<AudioOut>,
    out_r: Port<AudioOut>,
    inputs: Vec<BusBuffers>,
    outputs: Vec<BusBuffers>,
    in_headers: Vec<AudioBusBuffers>,
    out_headers: Vec<AudioBusBuffers>,
    events_in: ComWrapper<EventList>,
    params_in: ComWrapper<ParamChanges>,
    params_out: ComWrapper<ParamChanges>,
    msgs: rtrb::Consumer<AudioMsg>,
    /// Parameter changes the plugin reports (state loads, internal automation) for the controller.
    reports: rtrb::Producer<(u32, f64)>,
    midi_map: Box<[Option<u32>; 130]>,
    sample_rate: f64,
    position: i64,
    peak: Arc<AtomicU32>,
}

// SAFETY: the raw buffer pointers point into Vecs the Processor owns, and the Processor is
// used from the JACK thread only after being moved there.
unsafe impl Send for Processor {}

impl Processor {
    fn push_midi(&self, time: u32, bytes: &[u8]) {
        let status = bytes.first().copied().unwrap_or(0);
        let channel = (status & 0x0F) as i16;
        let mut e = blank_event();
        e.sampleOffset = time as i32;
        e.flags = 1; // kIsLive
        match *bytes {
            [s, key, vel] if s & 0xF0 == 0x90 && vel > 0 => {
                e.r#type = 0; // kNoteOnEvent
                e.__field0.noteOn = NoteOnEvent {
                    channel,
                    pitch: key as i16,
                    tuning: 0.0,
                    velocity: vel as f32 / 127.0,
                    length: 0,
                    noteId: key as i32,
                };
                self.events_in.push(e);
            }
            [s, key, vel] if s & 0xF0 == 0x80 || s & 0xF0 == 0x90 => {
                e.r#type = 1; // kNoteOffEvent
                e.__field0.noteOff = NoteOffEvent {
                    channel,
                    pitch: key as i16,
                    velocity: vel as f32 / 127.0,
                    noteId: key as i32,
                    tuning: 0.0,
                };
                self.events_in.push(e);
            }
            // VST3 has no raw MIDI: map CCs, aftertouch and pitch bend through IMidiMapping.
            [s, a, b] if s & 0xF0 == 0xB0 => self.map_controller(a as usize, b as f64 / 127.0),
            [s, a] if s & 0xF0 == 0xD0 => self.map_controller(128, a as f64 / 127.0),
            [s, lsb, msb] if s & 0xF0 == 0xE0 => {
                let v = ((msb as u16) << 7) | lsb as u16;
                self.map_controller(129, v as f64 / 16383.0)
            }
            _ => {}
        }
    }

    fn map_controller(&self, number: usize, value: f64) {
        if let Some(Some(id)) = self.midi_map.get(number) {
            self.params_in.set(*id, value);
        }
    }
}

impl jack::ProcessHandler for Processor {
    fn process(&mut self, _: &Client, ps: &jack::ProcessScope) -> Control {
        let n = (ps.n_frames() as usize).min(MAX_FRAMES);

        self.events_in.events().clear();
        self.params_in.clear();
        self.params_out.clear();
        for ev in self.midi_in.iter(ps) {
            self.push_midi(ev.time, ev.bytes);
        }
        while let Ok(msg) = self.msgs.pop() {
            match msg {
                AudioMsg::Midi(bytes) => self.push_midi(0, &bytes),
                AudioMsg::Param(id, value) => self.params_in.set(id, value),
            }
        }

        for b in &mut self.inputs {
            for c in &mut b.channels {
                c[..n].fill(0.0);
            }
        }

        let mut ctx: ProcessContext = unsafe { std::mem::zeroed() };
        ctx.state = 2 | (1 << 10) | (1 << 13); // kPlaying | kTempoValid | kTimeSigValid
        ctx.sampleRate = self.sample_rate;
        ctx.projectTimeSamples = self.position;
        ctx.tempo = 120.0;
        ctx.timeSigNumerator = 4;
        ctx.timeSigDenominator = 4;

        let mut data = ProcessData {
            processMode: 0,
            symbolicSampleSize: 0,
            numSamples: n as i32,
            numInputs: self.in_headers.len() as i32,
            numOutputs: self.out_headers.len() as i32,
            inputs: if self.in_headers.is_empty() {
                std::ptr::null_mut()
            } else {
                self.in_headers.as_mut_ptr()
            },
            outputs: if self.out_headers.is_empty() {
                std::ptr::null_mut()
            } else {
                self.out_headers.as_mut_ptr()
            },
            inputParameterChanges: self
                .params_in
                .as_com_ref::<IParameterChanges>()
                .unwrap()
                .as_ptr(),
            outputParameterChanges: self
                .params_out
                .as_com_ref::<IParameterChanges>()
                .unwrap()
                .as_ptr(),
            inputEvents: self.events_in.as_com_ref::<IEventList>().unwrap().as_ptr(),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut ctx,
        };
        // SAFETY: every pointer in `data` is valid for the duration of the call.
        let ok = unsafe { self.processor.process(&mut data) } == kResultOk;
        self.position += n as i64;
        let reports = &mut self.reports;
        self.params_out.drain(|id, v| {
            let _ = reports.push((id, v));
        });

        let l = self.out_l.as_mut_slice(ps);
        let r = self.out_r.as_mut_slice(ps);
        match (ok, self.outputs.first()) {
            (true, Some(main)) if !main.channels.is_empty() => {
                l[..n].copy_from_slice(&main.channels[0][..n]);
                let right = main.channels.get(1).unwrap_or(&main.channels[0]);
                r[..n].copy_from_slice(&right[..n]);
                let peak = l[..n]
                    .iter()
                    .chain(&r[..n])
                    .fold(0.0f32, |m, s| m.max(s.abs()));
                // Max of f32 bit patterns is the max of non-negative floats.
                self.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
            }
            _ => {
                l.fill(0.0);
                r.fill(0.0);
            }
        }
        Control::Continue
    }
}

pub struct Audio {
    pub client: jack::AsyncClient<Notifications, Processor>,
    pub xruns: Arc<AtomicU32>,
    /// Output peak since the last `take_peak`, as f32 bits.
    pub peak: Arc<AtomicU32>,
    pub sample_rate: u32,
}

/// Opens the JACK client, activates the plugin and starts processing.
pub fn start(
    plugin: &mut Plugin,
    msgs: rtrb::Consumer<AudioMsg>,
    reports: rtrb::Producer<(u32, f64)>,
) -> Result<Audio> {
    preload_pipewire_jack();
    // SAFETY: called before any other thread reads the environment.
    unsafe { std::env::set_var("PIPEWIRE_QUANTUM", "128/48000") };
    let (client, _) =
        Client::new("merlin-s3", ClientOptions::NO_START_SERVER).context("JACK client")?;
    let sample_rate = client.sample_rate();
    println!(
        "plugin audio buses: inputs {:?}, outputs {:?}, event inputs {}",
        plugin.buses.audio_in, plugin.buses.audio_out, plugin.buses.event_in
    );

    plugin.activate(sample_rate as f64, MAX_FRAMES as i32)?;

    let mut inputs: Vec<BusBuffers> = plugin
        .buses
        .audio_in
        .iter()
        .map(|&c| BusBuffers::new(c))
        .collect();
    let mut outputs: Vec<BusBuffers> = plugin
        .buses
        .audio_out
        .iter()
        .map(|&c| BusBuffers::new(c))
        .collect();
    let peak = Arc::new(AtomicU32::new(0));
    let proc = Processor {
        processor: plugin.processor.clone(),
        midi_in: client.register_port("midi_in", MidiIn::default())?,
        out_l: client.register_port("out_l", AudioOut::default())?,
        out_r: client.register_port("out_r", AudioOut::default())?,
        in_headers: inputs.iter_mut().map(BusBuffers::header).collect(),
        out_headers: outputs.iter_mut().map(BusBuffers::header).collect(),
        inputs,
        outputs,
        events_in: ComWrapper::new(EventList::new()),
        params_in: ComWrapper::new(ParamChanges::new()),
        params_out: ComWrapper::new(ParamChanges::new()),
        msgs,
        reports,
        midi_map: plugin.midi_map.clone(),
        sample_rate: sample_rate as f64,
        position: 0,
        peak: peak.clone(),
    };
    let names = [proc.out_l.name()?, proc.out_r.name()?, proc.midi_in.name()?];

    let xruns = Arc::new(AtomicU32::new(0));
    let active = client.activate_async(Notifications(xruns.clone()), proc)?;
    let c = active.as_client();
    let playback = c.ports(
        None,
        Some("32 bit float mono audio"),
        PortFlags::IS_PHYSICAL | PortFlags::IS_INPUT,
    );
    for (src, dst) in names[..2].iter().zip(&playback) {
        let _ = c.connect_ports_by_name(src, dst);
    }
    for src in c.ports(None, Some("8 bit raw midi"), PortFlags::IS_OUTPUT) {
        if !src.starts_with("merlin") {
            let _ = c.connect_ports_by_name(&src, &names[2]);
        }
    }
    Ok(Audio {
        client: active,
        xruns,
        peak,
        sample_rate,
    })
}

impl Audio {
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    /// Stops JACK, then deactivates the plugin (no `process()` can be running after this).
    pub fn stop(self, plugin: &mut Plugin) {
        drop(self.client.deactivate());
        plugin.deactivate();
    }
}
