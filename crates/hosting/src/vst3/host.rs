//! The host-side COM objects a VST3 plugin talks to: application context and component handler,
//! plug frame with run loop, in-memory streams, and messages.

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vst3::Steinberg::Linux::{IEventHandler, IRunLoop, IRunLoopTrait, ITimerHandler};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

use crate::{HostEvent, Waker};

use super::processor::ParamChange;

pub fn is_iid<I: Interface>(tuid: *const TUID) -> bool {
    // SAFETY: the plugin passes a valid 16-byte id.
    !tuid.is_null() && unsafe { (*tuid).iter().map(|b| *b as u8).eq(I::IID.iter().copied()) }
}

// ---------------------------------------------------------------------------------------------
// Host context: IHostApplication + IComponentHandler

pub struct HostCtx {
    waker: Arc<dyn Waker>,
    /// Edits made in the plugin's editor (and parameters set by the app), forwarded to the
    /// processor on its next block. Installed when the plugin is activated.
    pub edits: Mutex<Option<rtrb::Producer<ParamChange>>>,
    /// `restartComponent` flags not handled yet.
    pub restart_flags: AtomicI32,
}

impl Class for HostCtx {
    type Interfaces = (IHostApplication, IComponentHandler);
}

impl HostCtx {
    pub fn new(waker: Arc<dyn Waker>) -> Self {
        Self {
            waker,
            edits: Mutex::new(None),
            restart_flags: AtomicI32::new(0),
        }
    }

    pub fn send_param(&self, id: u32, value: f64) {
        if let Some(tx) = self.edits.lock().unwrap().as_mut() {
            let _ = tx.push(ParamChange { id, value });
        }
    }
}

impl IHostApplicationTrait for HostCtx {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        // SAFETY: the plugin passes a valid String128.
        let name = unsafe { &mut *name };
        for (dst, src) in name.iter_mut().zip("Merlin\0".encode_utf16()) {
            *dst = src as _;
        }
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        // Plugins ask for messages (to talk between component and controller) and attribute
        // lists by class id equal to the interface id.
        if is_iid::<IMessage>(cid) && is_iid::<IMessage>(iid) {
            let m = ComWrapper::new(Message::default());
            if let Some(p) = m.to_com_ptr::<IMessage>() {
                // SAFETY: obj is the plugin's out pointer; ownership of the reference moves to it.
                unsafe { *obj = p.into_raw().cast() };
                return kResultOk;
            }
        }
        if is_iid::<IAttributeList>(cid) && is_iid::<IAttributeList>(iid) {
            let a = ComWrapper::new(AttributeList::default());
            if let Some(p) = a.to_com_ptr::<IAttributeList>() {
                // SAFETY: as above.
                unsafe { *obj = p.into_raw().cast() };
                return kResultOk;
            }
        }
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
        self.restart_flags.fetch_or(flags, Ordering::AcqRel);
        self.waker.wake(HostEvent::MainThreadCallback);
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// Messages (IMessage / IAttributeList)

#[derive(Clone, Debug)]
enum Value {
    Int(i64),
    Float(f64),
    String(Vec<u16>),
    Binary(Vec<u8>),
}

#[derive(Default)]
pub struct AttributeList {
    values: Mutex<HashMap<String, Value>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

fn attr_name(id: IAttrID) -> String {
    // SAFETY: attribute ids are NUL-terminated C strings.
    unsafe { std::ffi::CStr::from_ptr(id) }
        .to_string_lossy()
        .into_owned()
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: IAttrID, value: i64) -> tresult {
        self.values
            .lock()
            .unwrap()
            .insert(attr_name(id), Value::Int(value));
        kResultOk
    }

    unsafe fn getInt(&self, id: IAttrID, value: *mut i64) -> tresult {
        match self.values.lock().unwrap().get(&attr_name(id)) {
            Some(Value::Int(v)) => {
                // SAFETY: the caller passes a valid out pointer.
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: IAttrID, value: f64) -> tresult {
        self.values
            .lock()
            .unwrap()
            .insert(attr_name(id), Value::Float(value));
        kResultOk
    }

    unsafe fn getFloat(&self, id: IAttrID, value: *mut f64) -> tresult {
        match self.values.lock().unwrap().get(&attr_name(id)) {
            Some(Value::Float(v)) => {
                // SAFETY: the caller passes a valid out pointer.
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: IAttrID, string: *const TChar) -> tresult {
        let mut s = Vec::new();
        // SAFETY: the string is NUL-terminated UTF-16.
        unsafe {
            let mut p = string;
            while *p != 0 {
                s.push(*p);
                p = p.add(1);
            }
        }
        self.values
            .lock()
            .unwrap()
            .insert(attr_name(id), Value::String(s));
        kResultOk
    }

    unsafe fn getString(&self, id: IAttrID, string: *mut TChar, size_in_bytes: u32) -> tresult {
        match self.values.lock().unwrap().get(&attr_name(id)) {
            Some(Value::String(s)) => {
                let capacity = (size_in_bytes as usize / 2).saturating_sub(1);
                for (i, c) in s.iter().take(capacity).enumerate() {
                    // SAFETY: i < capacity, inside the caller's buffer.
                    unsafe { *string.add(i) = *c as _ };
                }
                // SAFETY: the NUL fits: capacity reserves one element.
                unsafe { *string.add(s.len().min(capacity)) = 0 };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setBinary(&self, id: IAttrID, data: *const c_void, size: u32) -> tresult {
        // SAFETY: the caller provides `size` readable bytes.
        let bytes =
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) }.to_vec();
        self.values
            .lock()
            .unwrap()
            .insert(attr_name(id), Value::Binary(bytes));
        kResultOk
    }

    unsafe fn getBinary(&self, id: IAttrID, data: *mut *const c_void, size: *mut u32) -> tresult {
        match self.values.lock().unwrap().get(&attr_name(id)) {
            Some(Value::Binary(b)) => {
                // The pointer must stay valid as long as the attribute exists: the Vec's heap
                // buffer is not moved by the map rehashing.
                // SAFETY: the caller passes valid out pointers.
                unsafe {
                    *data = b.as_ptr().cast();
                    *size = b.len() as u32;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

pub struct Message {
    id: Mutex<std::ffi::CString>,
    attributes: ComWrapper<AttributeList>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            id: Mutex::new(std::ffi::CString::default()),
            attributes: ComWrapper::new(AttributeList::default()),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        // The CString lives as long as the message; setMessageID replaces it, as the SDK's own
        // implementation does.
        self.id.lock().unwrap().as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        if !id.is_null() {
            // SAFETY: NUL-terminated string from the plugin.
            *self.id.lock().unwrap() = unsafe { std::ffi::CStr::from_ptr(id) }.to_owned();
        }
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
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

/// The host side of the editor: `IPlugFrame` for resize requests and `IRunLoop` so the plugin
/// can register its timers and file descriptors on our main thread.
pub struct HostFrame {
    waker: Arc<dyn Waker>,
    timers: Mutex<Vec<TimerReg>>,
    fds: Mutex<Vec<FdReg>>,
    fds_changed: AtomicBool,
    /// The editor size the host last set, `width << 32 | height`; resize requests that merely
    /// echo it are not forwarded.
    pub known_size: AtomicU64,
}

impl Class for HostFrame {
    type Interfaces = (IPlugFrame, IRunLoop);
}

pub fn pack_size(w: u32, h: u32) -> u64 {
    (w as u64) << 32 | h as u64
}

impl HostFrame {
    pub fn new(waker: Arc<dyn Waker>) -> Self {
        Self {
            waker,
            timers: Mutex::default(),
            fds: Mutex::default(),
            fds_changed: AtomicBool::new(false),
            known_size: AtomicU64::new(0),
        }
    }

    /// Handlers whose timer is due, rescheduled. Collected first so a handler may register or
    /// unregister timers from inside `onTimer`.
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

    /// The watched fd set, if it changed since the last call.
    pub fn take_fds_changed(&self) -> Option<Vec<RawFd>> {
        self.fds_changed
            .swap(false, Ordering::AcqRel)
            .then(|| self.fds.lock().unwrap().iter().map(|r| r.fd).collect())
    }

    pub fn watched_fds(&self) -> Vec<RawFd> {
        self.fds.lock().unwrap().iter().map(|r| r.fd).collect()
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
        let (w, h) = (
            (r.right - r.left).max(1) as u32,
            (r.bottom - r.top).max(1) as u32,
        );
        if self.known_size.load(Ordering::Acquire) != pack_size(w, h) {
            self.waker.wake(HostEvent::EditorResize {
                width: w,
                height: h,
            });
        }
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
        // 10 ms keeps UIs smooth without burning CPU.
        let period = Duration::from_millis(ms.max(10));
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
