//! The audio-thread half of a VST3 plugin.
//!
//! The audio-thread COM objects (event list, parameter changes) are preallocated and
//! single-threaded: only the audio thread, and the plugin from inside `process()`, touch them.
//! VST3 has no raw MIDI: notes are note events; CCs, channel pressure and pitch bend go through
//! the controller's `IMidiMapping` as parameter changes.

use std::cell::{Cell, UnsafeCell};
use std::sync::Arc;

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper};

use super::module::Module;
use crate::{Io, MidiEvent, MidiKind, PluginProcessor, ProcessStatus};

/// Largest block this processor accepts (buffers are sized for it).
pub const MAX_FRAMES: usize = 4096;
const MAX_QUEUES: usize = 64;
const MAX_POINTS: usize = 32;
const MAX_EVENTS: usize = 1024;
/// Silent output for this long lets the engine put the slot to sleep.
const SLEEP_AFTER_SECS: f64 = 1.0;

/// A parameter change heading for the processor: from the plugin's editor (through the component
/// handler) or from the app. The value is normalised.
#[derive(Clone, Copy, Debug)]
pub struct ParamChange {
    pub id: u32,
    pub value: f64,
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
// Buffers

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

// ---------------------------------------------------------------------------------------------
// Processor

pub struct Vst3Processor {
    processor: ComPtr<IAudioProcessor>,
    inputs: Vec<BusBuffers>,
    outputs: Vec<BusBuffers>,
    in_headers: Vec<AudioBusBuffers>,
    out_headers: Vec<AudioBusBuffers>,
    events_in: ComWrapper<EventList>,
    params_in: ComWrapper<ParamChanges>,
    params_out: ComWrapper<ParamChanges>,
    changes: rtrb::Consumer<ParamChange>,
    /// Parameter changes the plugin reports (state loads, internal automation), for the
    /// controller on the main thread.
    reports: rtrb::Producer<(u32, f64)>,
    midi_map: Box<[Option<u32>; 130]>,
    sample_rate: f64,
    position: i64,
    silent_frames: usize,
    // Keeps the library loaded for as long as the processor exists.
    _module: Arc<Module>,
}

// SAFETY: the raw buffer pointers point into Vecs the processor owns, and COM objects are used
// from one thread at a time (created on the main thread, then used on the audio thread).
unsafe impl Send for Vst3Processor {}

impl Vst3Processor {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        processor: ComPtr<IAudioProcessor>,
        audio_in: &[i32],
        audio_out: &[i32],
        changes: rtrb::Consumer<ParamChange>,
        reports: rtrb::Producer<(u32, f64)>,
        midi_map: Box<[Option<u32>; 130]>,
        sample_rate: f64,
        module: Arc<Module>,
    ) -> Self {
        let mut inputs: Vec<BusBuffers> = audio_in.iter().map(|&c| BusBuffers::new(c)).collect();
        let mut outputs: Vec<BusBuffers> = audio_out.iter().map(|&c| BusBuffers::new(c)).collect();
        Self {
            processor,
            in_headers: inputs.iter_mut().map(BusBuffers::header).collect(),
            out_headers: outputs.iter_mut().map(BusBuffers::header).collect(),
            inputs,
            outputs,
            events_in: ComWrapper::new(EventList::new()),
            params_in: ComWrapper::new(ParamChanges::new()),
            params_out: ComWrapper::new(ParamChanges::new()),
            changes,
            reports,
            midi_map,
            sample_rate,
            position: 0,
            silent_frames: 0,
            _module: module,
        }
    }

    fn push_midi(&self, ev: &MidiEvent) {
        let mut e = blank_event();
        e.sampleOffset = ev.time as i32;
        e.flags = 1; // kIsLive
        match ev.kind() {
            MidiKind::NoteOn { ch, key, vel } => {
                e.r#type = 0; // kNoteOnEvent
                e.__field0.noteOn = NoteOnEvent {
                    channel: ch as i16,
                    pitch: key as i16,
                    tuning: 0.0,
                    velocity: vel as f32 / 127.0,
                    length: 0,
                    noteId: ch as i32 * 128 + key as i32,
                };
                self.events_in.push(e);
            }
            MidiKind::NoteOff { ch, key, vel } => {
                e.r#type = 1; // kNoteOffEvent
                e.__field0.noteOff = NoteOffEvent {
                    channel: ch as i16,
                    pitch: key as i16,
                    velocity: vel as f32 / 127.0,
                    noteId: ch as i32 * 128 + key as i32,
                    tuning: 0.0,
                };
                self.events_in.push(e);
            }
            MidiKind::PolyPressure { ch, key, value } => {
                e.r#type = 3; // kPolyPressureEvent
                e.__field0.polyPressure = PolyPressureEvent {
                    channel: ch as i16,
                    pitch: key as i16,
                    pressure: value as f32 / 127.0,
                    noteId: ch as i32 * 128 + key as i32,
                };
                self.events_in.push(e);
            }
            MidiKind::Cc { cc, value, .. } => {
                self.map_controller(cc as usize, value as f64 / 127.0)
            }
            MidiKind::ChannelPressure { value, .. } => {
                self.map_controller(128, value as f64 / 127.0)
            }
            MidiKind::PitchBend { value, .. } => self.map_controller(129, value as f64 / 16383.0),
            MidiKind::ProgramChange { .. } | MidiKind::Other => {}
        }
    }

    fn map_controller(&self, number: usize, value: f64) {
        if let Some(Some(id)) = self.midi_map.get(number) {
            self.params_in.set(*id, value);
        }
    }
}

impl PluginProcessor for Vst3Processor {
    fn process(&mut self, io: &mut Io, events: &[MidiEvent]) -> ProcessStatus {
        let n = io.frames().min(MAX_FRAMES);

        self.events_in.events().clear();
        self.params_in.clear();
        self.params_out.clear();
        while let Ok(c) = self.changes.pop() {
            self.params_in.set(c.id, c.value);
        }
        for ev in events {
            self.push_midi(ev);
        }

        for b in &mut self.inputs {
            for c in &mut b.channels {
                c[..n].fill(0.0);
            }
        }
        if let (Some((l, r)), Some(bus)) = (io.input, self.inputs.first_mut()) {
            if let Some(c) = bus.channels.get_mut(0) {
                c[..n].copy_from_slice(&l[..n]);
            }
            if let Some(c) = bus.channels.get_mut(1) {
                c[..n].copy_from_slice(&r[..n]);
            }
        }
        for h in &mut self.out_headers {
            h.silenceFlags = 0;
        }

        // SAFETY: ProcessContext is plain data; zero is a valid initial value.
        let mut ctx: ProcessContext = unsafe { std::mem::zeroed() };
        ctx.state = 2 | (1 << 10) | (1 << 13); // kPlaying | kTempoValid | kTimeSigValid
        ctx.sampleRate = self.sample_rate;
        ctx.projectTimeSamples = self.position;
        ctx.tempo = f64::from(crate::host_time::tempo());
        ctx.timeSigNumerator = crate::host_time::beats_per_bar().min(i32::MAX as u32) as i32;
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

        let Some(main) = self
            .outputs
            .first()
            .filter(|b| ok && !b.channels.is_empty())
        else {
            return ProcessStatus::Continue;
        };
        io.out_l[..n].copy_from_slice(&main.channels[0][..n]);
        let right = main.channels.get(1).unwrap_or(&main.channels[0]);
        io.out_r[..n].copy_from_slice(&right[..n]);

        // Exactly-zero output with nothing going in for long enough: nothing left to ring out.
        let silent = io.out_l[..n]
            .iter()
            .chain(&io.out_r[..n])
            .all(|&s| s == 0.0);
        if silent && events.is_empty() && io.input.is_none() {
            self.silent_frames += n;
        } else {
            self.silent_frames = 0;
        }
        if self.silent_frames as f64 >= SLEEP_AFTER_SECS * self.sample_rate {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}
