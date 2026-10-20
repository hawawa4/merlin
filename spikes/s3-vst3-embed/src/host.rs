//! VST3 hosting: module loading, the host-side COM objects (context, component handler,
//! plug frame with run loop, memory streams) and the plugin instance.
//!
//! Everything a plugin asks of the main thread (resize, timers, fds) becomes a winit user
//! event or is serviced from the main loop's `about_to_wait`, as in S2.

use std::ffi::{c_char, c_void};
use std::os::fd::RawFd;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use libloading::os::unix::Library;
use vst3::Steinberg::Linux::{IEventHandler, IRunLoop, IRunLoopTrait, ITimerHandler};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};
use winit::event_loop::EventLoopProxy;

use crate::audio::AudioMsg;

// Enum values from the SDK headers (the bindings expose them in nested modules).
pub const K_AUDIO: i32 = 0;
pub const K_EVENT: i32 = 1;
pub const K_INPUT: i32 = 0;
pub const K_OUTPUT: i32 = 1;

#[derive(Debug, Clone, Copy)]
pub enum UserEvent {
    /// A plugin-registered fd became readable.
    FdReady,
    /// The plugin asked (`IPlugFrame::resizeView`) for a new editor size.
    ViewResize(u32, u32),
    /// `IComponentHandler::restartComponent` with these flags.
    Restart(i32),
}

pub fn cstr(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

pub fn utf16(chars: &[u16]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end])
}

// ---------------------------------------------------------------------------------------------
// Module

pub struct ClassInfo {
    pub cid: TUID,
    pub name: String,
    pub category: String,
    pub subcategories: String,
    pub vendor: String,
}

/// A loaded `.vst3` bundle: `ModuleEntry` has run, `ModuleExit` runs on drop.
pub struct Module {
    factory: Option<ComPtr<IPluginFactory>>,
    exit: Option<unsafe extern "C" fn() -> bool>,
    _lib: Library,
}

impl Module {
    pub fn load(bundle: &Path) -> Result<Self> {
        let stem = bundle
            .file_stem()
            .and_then(|s| s.to_str())
            .context("bundle path has no name")?;
        let binary = bundle
            .join("Contents/x86_64-linux")
            .join(format!("{stem}.so"));
        // SAFETY: loading a plugin binary runs its code; that is the point of a host.
        unsafe {
            let lib = Library::open(Some(&binary), libc::RTLD_NOW | libc::RTLD_LOCAL)
                .map_err(|e| anyhow!("dlopen {}: {e}", binary.display()))?;
            // The SDK passes the dlopen handle to ModuleEntry.
            let raw = lib.into_raw();
            let lib = Library::from_raw(raw);
            if let Ok(entry) =
                lib.get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"ModuleEntry\0")
                && !entry(raw)
            {
                bail!("ModuleEntry returned false");
            }
            let exit = lib
                .get::<unsafe extern "C" fn() -> bool>(b"ModuleExit\0")
                .ok()
                .map(|s| *s);
            let get_factory = lib
                .get::<unsafe extern "C" fn() -> *mut IPluginFactory>(b"GetPluginFactory\0")
                .map_err(|e| anyhow!("no GetPluginFactory: {e}"))?;
            let factory = ComPtr::from_raw(get_factory()).context("GetPluginFactory gave null")?;
            Ok(Self {
                factory: Some(factory),
                exit,
                _lib: lib,
            })
        }
    }

    pub fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory.as_ref().unwrap()
    }

    pub fn classes(&self) -> Vec<ClassInfo> {
        let f = self.factory();
        let f2 = f.cast::<IPluginFactory2>();
        // SAFETY: plain factory calls with zeroed out-structs.
        unsafe {
            (0..f.countClasses())
                .filter_map(|i| {
                    if let Some(f2) = &f2 {
                        let mut info: PClassInfo2 = std::mem::zeroed();
                        (f2.getClassInfo2(i, &mut info) == kResultOk).then(|| ClassInfo {
                            cid: info.cid,
                            name: cstr(&info.name),
                            category: cstr(&info.category),
                            subcategories: cstr(&info.subCategories),
                            vendor: cstr(&info.vendor),
                        })
                    } else {
                        let mut info: PClassInfo = std::mem::zeroed();
                        (f.getClassInfo(i, &mut info) == kResultOk).then(|| ClassInfo {
                            cid: info.cid,
                            name: cstr(&info.name),
                            category: cstr(&info.category),
                            subcategories: String::new(),
                            vendor: String::new(),
                        })
                    }
                })
                .collect()
        }
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // Every plugin object must be gone by now; release the factory, then ModuleExit.
        self.factory = None;
        if let Some(exit) = self.exit {
            // SAFETY: the SDK contract: called once, after all objects are released.
            unsafe { exit() };
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Host context: IHostApplication + IComponentHandler

pub struct HostCtx {
    proxy: Mutex<EventLoopProxy<UserEvent>>,
    /// Edits made in the plugin's editor, forwarded to the processor as parameter changes.
    edits: Mutex<rtrb::Producer<AudioMsg>>,
}

impl Class for HostCtx {
    type Interfaces = (IHostApplication, IComponentHandler);
}

impl HostCtx {
    pub fn new(proxy: EventLoopProxy<UserEvent>, edits: rtrb::Producer<AudioMsg>) -> Self {
        Self {
            proxy: Mutex::new(proxy),
            edits: Mutex::new(edits),
        }
    }

    pub fn send_param(&self, id: u32, value: f64) {
        let _ = self.edits.lock().unwrap().push(AudioMsg::Param(id, value));
    }

    pub fn send_midi(&self, bytes: [u8; 3]) {
        let _ = self.edits.lock().unwrap().push(AudioMsg::Midi(bytes));
    }
}

impl IHostApplicationTrait for HostCtx {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        // SAFETY: the plugin passes a valid String128.
        let name = unsafe { &mut *name };
        for (dst, src) in name.iter_mut().zip("Merlin S3\0".encode_utf16()) {
            *dst = src as _;
        }
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        _cid: *mut TUID,
        _iid: *mut TUID,
        _obj: *mut *mut c_void,
    ) -> tresult {
        // No IMessage / IAttributeList yet: plugins that message between component and
        // controller will need them (a known long-tail item).
        kResultFalse
    }
}

impl IComponentHandlerTrait for HostCtx {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.send_param(id, value);
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        let _ = self
            .proxy
            .lock()
            .unwrap()
            .send_event(UserEvent::Restart(flags));
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// Plug frame + run loop

struct TimerReg {
    handler: ComPtr<ITimerHandler>,
    period: Duration,
    next: Instant,
}

struct FdReg {
    handler: ComPtr<IEventHandler>,
    fd: RawFd,
}

/// The host side of the editor: `IPlugFrame` for resize requests and `IRunLoop` so the
/// plugin can register its timers and file descriptors on our main thread.
pub struct HostFrame {
    proxy: Mutex<EventLoopProxy<UserEvent>>,
    timers: Mutex<Vec<TimerReg>>,
    fds: Mutex<Vec<FdReg>>,
    fds_changed: AtomicBool,
}

impl Class for HostFrame {
    type Interfaces = (IPlugFrame, IRunLoop);
}

impl HostFrame {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            proxy: Mutex::new(proxy),
            timers: Mutex::default(),
            fds: Mutex::default(),
            fds_changed: AtomicBool::new(false),
        }
    }

    /// Handlers whose timer is due, rescheduled. Collected first so a handler may register
    /// or unregister timers from inside `onTimer`.
    pub fn due_timers(&self, now: Instant) -> Vec<ComPtr<ITimerHandler>> {
        let mut due = Vec::new();
        for t in self.timers.lock().unwrap().iter_mut() {
            if t.next <= now {
                due.push(t.handler.clone());
                t.next = now + t.period;
            }
        }
        due
    }

    pub fn next_timer_deadline(&self) -> Option<Instant> {
        self.timers.lock().unwrap().iter().map(|t| t.next).min()
    }

    pub fn counts(&self) -> (usize, usize) {
        (
            self.timers.lock().unwrap().len(),
            self.fds.lock().unwrap().len(),
        )
    }

    /// The watched fd set, if it changed since the last call.
    pub fn take_fds_changed(&self) -> Option<Vec<RawFd>> {
        self.fds_changed
            .swap(false, Ordering::AcqRel)
            .then(|| self.fds.lock().unwrap().iter().map(|r| r.fd).collect())
    }

    pub fn handlers_for(&self, ready: &[RawFd]) -> Vec<(ComPtr<IEventHandler>, RawFd)> {
        self.fds
            .lock()
            .unwrap()
            .iter()
            .filter(|r| ready.contains(&r.fd))
            .map(|r| (r.handler.clone(), r.fd))
            .collect()
    }

    pub fn clear(&self) {
        self.timers.lock().unwrap().clear();
        self.fds.lock().unwrap().clear();
        self.fds_changed.store(true, Ordering::Release);
    }
}

impl IPlugFrameTrait for HostFrame {
    unsafe fn resizeView(&self, _view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        // SAFETY: the plugin passes a valid rect.
        let r = unsafe { &*new_size };
        let _ = self.proxy.lock().unwrap().send_event(UserEvent::ViewResize(
            (r.right - r.left).max(1) as u32,
            (r.bottom - r.top).max(1) as u32,
        ));
        kResultOk
    }
}

/// An extra reference to a handler the plugin passed us (it keeps its own).
unsafe fn retain<I: Interface>(ptr: *mut I) -> Option<ComPtr<I>> {
    // SAFETY: the caller passes the plugin's live interface pointer.
    unsafe { ComRef::from_raw(ptr).map(|r| r.to_com_ptr()) }
}

impl IRunLoopTrait for HostFrame {
    unsafe fn registerEventHandler(&self, handler: *mut IEventHandler, fd: i32) -> tresult {
        // SAFETY: handler is the plugin's live object.
        let Some(handler) = (unsafe { retain(handler) }) else {
            return kInvalidArgument;
        };
        println!("run loop: fd {fd} registered");
        self.fds.lock().unwrap().push(FdReg { handler, fd });
        self.fds_changed.store(true, Ordering::Release);
        kResultOk
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        self.fds
            .lock()
            .unwrap()
            .retain(|r| r.handler.as_ptr() != handler);
        self.fds_changed.store(true, Ordering::Release);
        kResultOk
    }

    unsafe fn registerTimer(&self, handler: *mut ITimerHandler, ms: u64) -> tresult {
        // SAFETY: handler is the plugin's live object.
        let Some(handler) = (unsafe { retain(handler) }) else {
            return kInvalidArgument;
        };
        // Clamp like S2: 10 ms keeps UIs smooth without burning CPU.
        let period = Duration::from_millis(ms.max(10));
        println!("run loop: timer registered, {} ms", period.as_millis());
        self.timers.lock().unwrap().push(TimerReg {
            handler,
            period,
            next: Instant::now() + period,
        });
        kResultOk
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        self.timers
            .lock()
            .unwrap()
            .retain(|t| t.handler.as_ptr() != handler);
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// fd poller (same design as S2: a helper thread wakes the main loop; the main thread acks)

pub struct FdPoller {
    list: std::sync::Arc<Mutex<Vec<libc::pollfd>>>,
    wake_w: RawFd,
    ack: mpsc::Sender<()>,
}

impl FdPoller {
    pub fn spawn(proxy: EventLoopProxy<UserEvent>) -> Self {
        let mut pipe = [0; 2];
        // SAFETY: plain pipe creation.
        unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        let (wake_r, wake_w) = (pipe[0], pipe[1]);
        let list: std::sync::Arc<Mutex<Vec<libc::pollfd>>> = Default::default();
        let (ack, ack_rx) = mpsc::channel::<()>();
        let thread_list = list.clone();
        std::thread::Builder::new()
            .name("fd-poller".into())
            .spawn(move || {
                loop {
                    let mut fds = vec![libc::pollfd {
                        fd: wake_r,
                        events: libc::POLLIN,
                        revents: 0,
                    }];
                    fds.extend(thread_list.lock().unwrap().iter().copied());
                    // SAFETY: fds is a valid array of pollfd.
                    let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
                    if n < 0 {
                        continue;
                    }
                    if fds[0].revents != 0 {
                        let mut buf = [0u8; 64];
                        // SAFETY: draining our own non-blocking pipe.
                        while unsafe { libc::read(wake_r, buf.as_mut_ptr().cast(), 64) } > 0 {}
                        continue;
                    }
                    if proxy.send_event(UserEvent::FdReady).is_err() || ack_rx.recv().is_err() {
                        return;
                    }
                }
            })
            .unwrap();
        Self { list, wake_w, ack }
    }

    pub fn set(&self, fds: &[RawFd]) {
        *self.list.lock().unwrap() = fds
            .iter()
            .map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: one byte into our own pipe to interrupt poll().
        unsafe { libc::write(self.wake_w, [1u8].as_ptr().cast(), 1) };
    }

    /// Main thread, after `FdReady`: which of the watched fds are readable now.
    pub fn ready(&self, fds: &[RawFd]) -> Vec<RawFd> {
        let mut pfds: Vec<libc::pollfd> = fds
            .iter()
            .map(|&fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: valid pollfd array, zero timeout.
        unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, 0) };
        pfds.iter()
            .filter(|p| p.revents != 0)
            .map(|p| p.fd)
            .collect()
    }

    pub fn ack(&self) {
        let _ = self.ack.send(());
    }
}

// ---------------------------------------------------------------------------------------------
// IBStream in memory

#[derive(Default)]
pub struct MemStream {
    inner: Mutex<(Vec<u8>, usize)>,
}

impl Class for MemStream {
    type Interfaces = (IBStream,);
}

impl MemStream {
    pub fn with_data(data: &[u8]) -> ComWrapper<Self> {
        ComWrapper::new(Self {
            inner: Mutex::new((data.to_vec(), 0)),
        })
    }

    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.inner.lock().unwrap().0)
    }
}

impl IBStreamTrait for MemStream {
    unsafe fn read(&self, buffer: *mut c_void, num: i32, read: *mut i32) -> tresult {
        let mut g = self.inner.lock().unwrap();
        let (data, pos) = &mut *g;
        let n = (num.max(0) as usize).min(data.len().saturating_sub(*pos));
        // SAFETY: the caller provides `num` writable bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().add(*pos), buffer.cast::<u8>(), n) };
        *pos += n;
        if !read.is_null() {
            // SAFETY: non-null out pointer from the caller.
            unsafe { *read = n as i32 };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, num: i32, written: *mut i32) -> tresult {
        let mut g = self.inner.lock().unwrap();
        let (data, pos) = &mut *g;
        let n = num.max(0) as usize;
        if data.len() < *pos + n {
            data.resize(*pos + n, 0);
        }
        // SAFETY: the caller provides `num` readable bytes.
        let src = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), n) };
        data[*pos..*pos + n].copy_from_slice(src);
        *pos += n;
        if !written.is_null() {
            // SAFETY: non-null out pointer from the caller.
            unsafe { *written = n as i32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> tresult {
        let mut g = self.inner.lock().unwrap();
        let (data, cur) = &mut *g;
        let base = match mode {
            0 => 0,
            1 => *cur as i64,
            2 => data.len() as i64,
            _ => return kInvalidArgument,
        };
        let new = base + pos;
        if new < 0 {
            return kInvalidArgument;
        }
        *cur = new as usize;
        if !result.is_null() {
            // SAFETY: non-null out pointer from the caller.
            unsafe { *result = new };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut i64) -> tresult {
        if !pos.is_null() {
            // SAFETY: non-null out pointer from the caller.
            unsafe { *pos = self.inner.lock().unwrap().1 as i64 };
        }
        kResultOk
    }
}

/// Run `f` with a stream the plugin writes into; returns what it wrote.
pub fn save_with(f: impl FnOnce(*mut IBStream) -> tresult) -> Option<Vec<u8>> {
    let stream = ComWrapper::new(MemStream::default());
    let ptr = stream.to_com_ptr::<IBStream>()?;
    (f(ptr.as_ptr()) == kResultOk).then(|| stream.take())
}

// ---------------------------------------------------------------------------------------------
// The plugin instance

pub struct ParamDesc {
    pub id: u32,
    pub title: String,
}

pub struct Buses {
    /// Channel counts per audio bus.
    pub audio_in: Vec<i32>,
    pub audio_out: Vec<i32>,
    pub event_in: i32,
}

pub struct Plugin {
    pub name: String,
    pub component: ComPtr<IComponent>,
    pub processor: ComPtr<IAudioProcessor>,
    pub controller: ComPtr<IEditController>,
    separate_controller: bool,
    connected: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    _host_app: ComPtr<IHostApplication>,
    pub host: ComWrapper<HostCtx>,
    pub frame: ComWrapper<HostFrame>,
    pub buses: Buses,
    pub midi_map: Box<[Option<u32>; 130]>,
    active: bool,
}

unsafe fn create<I: Interface>(factory: &ComPtr<IPluginFactory>, cid: &TUID) -> Result<ComPtr<I>> {
    let mut obj: *mut c_void = null_mut();
    // SAFETY: both ids are 16-byte TUIDs; obj receives an owned interface pointer.
    let r = unsafe { factory.createInstance(cid.as_ptr(), I::IID.as_ptr().cast(), &mut obj) };
    if r != kResultOk {
        bail!("createInstance failed: {r}");
    }
    // SAFETY: createInstance gave us an owned reference of the requested interface.
    unsafe { ComPtr::from_raw(obj.cast::<I>()) }.context("createInstance returned null")
}

impl Plugin {
    pub fn new(
        module: &Module,
        class: &ClassInfo,
        proxy: EventLoopProxy<UserEvent>,
        edits: rtrb::Producer<AudioMsg>,
    ) -> Result<Self> {
        let host = ComWrapper::new(HostCtx::new(proxy.clone(), edits));
        let frame = ComWrapper::new(HostFrame::new(proxy));
        let host_app = host.to_com_ptr::<IHostApplication>().unwrap();
        let ctx = host_app.as_ptr().cast::<FUnknown>();
        let factory = module.factory();
        // SAFETY: COM calls on objects we just created, in the order the VST3 docs prescribe.
        unsafe {
            if let Some(f3) = factory.cast::<IPluginFactory3>() {
                f3.setHostContext(ctx);
            }
            let component: ComPtr<IComponent> = create(factory, &class.cid)?;
            let r = component.initialize(ctx);
            if r != kResultOk {
                bail!("component.initialize failed: {r}");
            }
            let processor = component
                .cast::<IAudioProcessor>()
                .context("component is not an IAudioProcessor")?;

            // Single-component plugins implement the controller on the same object.
            let (controller, separate_controller) = match component.cast::<IEditController>() {
                Some(c) => (c, false),
                None => {
                    let mut cid: TUID = [0; 16];
                    if component.getControllerClassId(&mut cid) != kResultOk {
                        bail!("no controller class");
                    }
                    let c: ComPtr<IEditController> = create(factory, &cid)?;
                    let r = c.initialize(ctx);
                    if r != kResultOk {
                        bail!("controller.initialize failed: {r}");
                    }
                    (c, true)
                }
            };
            let connected = match (
                component.cast::<IConnectionPoint>(),
                controller.cast::<IConnectionPoint>(),
            ) {
                (Some(a), Some(b)) if separate_controller => {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                    Some((a, b))
                }
                _ => None,
            };
            let handler = host.to_com_ptr::<IComponentHandler>().unwrap();
            controller.setComponentHandler(handler.as_ptr());

            let buses = Buses {
                audio_in: bus_channels(&component, K_AUDIO, K_INPUT),
                audio_out: bus_channels(&component, K_AUDIO, K_OUTPUT),
                event_in: component.getBusCount(K_EVENT, K_INPUT),
            };
            let mut midi_map = Box::new([None; 130]);
            if let Some(map) = controller.cast::<IMidiMapping>() {
                for (cc, slot) in midi_map.iter_mut().enumerate() {
                    let mut id = 0;
                    if map.getMidiControllerAssignment(0, 0, cc as i16, &mut id) == kResultOk {
                        *slot = Some(id);
                    }
                }
            }
            Ok(Self {
                name: class.name.clone(),
                component,
                processor,
                controller,
                separate_controller,
                connected,
                _host_app: host_app,
                host,
                frame,
                buses,
                midi_map,
                active: false,
            })
        }
    }

    /// Activate the first audio buses and the event input, set up processing.
    pub fn activate(&mut self, sample_rate: f64, max_frames: i32) -> Result<()> {
        // SAFETY: setup calls on the main thread while not processing.
        unsafe {
            for dir in [K_INPUT, K_OUTPUT] {
                if self.component.getBusCount(K_AUDIO, dir) > 0 {
                    self.component.activateBus(K_AUDIO, dir, 0, 1);
                }
            }
            if self.buses.event_in > 0 {
                self.component.activateBus(K_EVENT, K_INPUT, 0, 1);
            }
            if self.processor.canProcessSampleSize(0) != kResultOk {
                bail!("plugin cannot process 32-bit float");
            }
            let mut setup = ProcessSetup {
                processMode: 0,        // kRealtime
                symbolicSampleSize: 0, // kSample32
                maxSamplesPerBlock: max_frames,
                sampleRate: sample_rate,
            };
            let r = self.processor.setupProcessing(&mut setup);
            if r != kResultOk {
                bail!("setupProcessing failed: {r}");
            }
            let r = self.component.setActive(1);
            if r != kResultOk {
                bail!("setActive failed: {r}");
            }
            self.processor.setProcessing(1);
        }
        self.active = true;
        // The controller starts out knowing nothing of the processor's state; tell it.
        self.sync_controller();
        Ok(())
    }

    /// Hand the processor's current state to the controller (`setComponentState`).
    pub fn sync_controller(&self) {
        if let Ok((comp, _)) = self.save_state() {
            let s = MemStream::with_data(&comp);
            let p = s.to_com_ptr::<IBStream>().unwrap();
            // SAFETY: the stream lives through the call.
            unsafe { self.controller.setComponentState(p.as_ptr()) };
        }
    }

    pub fn deactivate(&mut self) {
        if std::mem::take(&mut self.active) {
            // SAFETY: the audio thread has stopped calling process().
            unsafe {
                self.processor.setProcessing(0);
                self.component.setActive(0);
            }
        }
    }

    pub fn latency(&self) -> u32 {
        // SAFETY: simple query.
        unsafe { self.processor.getLatencySamples() }
    }

    pub fn params(&self) -> Vec<ParamDesc> {
        // SAFETY: zeroed ParameterInfo is a valid out-struct.
        unsafe {
            (0..self.controller.getParameterCount())
                .filter_map(|i| {
                    let mut info: ParameterInfo = std::mem::zeroed();
                    (self.controller.getParameterInfo(i, &mut info) == kResultOk).then_some(info)
                })
                .filter(|p| {
                    // kCanAutomate = 1, kIsReadOnly = 2, kIsHidden = 16, kIsBypass = 65536
                    p.flags & 1 != 0 && p.flags & (2 | 16 | 65536) == 0
                })
                .map(|p| ParamDesc {
                    id: p.id,
                    title: utf16(&p.title),
                })
                .collect()
        }
    }

    /// Component state and controller state, as two blobs (controller state is optional).
    pub fn save_state(&self) -> Result<(Vec<u8>, Option<Vec<u8>>)> {
        // SAFETY: the plugin writes into streams we own for the duration of the call.
        let comp = save_with(|s| unsafe { self.component.getState(s) })
            .context("component.getState failed")?;
        let ctrl = save_with(|s| unsafe { self.controller.getState(s) });
        Ok((comp, ctrl))
    }

    pub fn load_state(&self, comp: &[u8], ctrl: Option<&[u8]>) -> Result<()> {
        // SAFETY: the plugin reads from streams we own for the duration of each call.
        unsafe {
            let s = MemStream::with_data(comp);
            let p = s.to_com_ptr::<IBStream>().unwrap();
            if self.component.setState(p.as_ptr()) != kResultOk {
                bail!("component.setState failed");
            }
            // The controller must be told about the processor's state, or its UI is stale.
            let s = MemStream::with_data(comp);
            let p = s.to_com_ptr::<IBStream>().unwrap();
            self.controller.setComponentState(p.as_ptr());
            if let Some(ctrl) = ctrl {
                let s = MemStream::with_data(ctrl);
                let p = s.to_com_ptr::<IBStream>().unwrap();
                self.controller.setState(p.as_ptr());
            }
        }
        Ok(())
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        self.deactivate();
        self.frame.clear();
        // SAFETY: teardown in the reverse order of Plugin::new.
        unsafe {
            self.controller.setComponentHandler(null_mut());
            if let Some((a, b)) = &self.connected {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if self.separate_controller {
                self.controller.terminate();
            }
            self.component.terminate();
        }
    }
}

unsafe fn bus_channels(component: &ComPtr<IComponent>, media: i32, dir: i32) -> Vec<i32> {
    // SAFETY: zeroed BusInfo is a valid out-struct.
    unsafe {
        (0..component.getBusCount(media, dir))
            .filter_map(|i| {
                let mut info: BusInfo = std::mem::zeroed();
                (component.getBusInfo(media, dir, i, &mut info) == kResultOk)
                    .then_some(info.channelCount)
            })
            .collect()
    }
}
