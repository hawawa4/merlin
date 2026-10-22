//! The LV2 worker extension: a plugin hands work that must not run on the audio thread (loading
//! a file, building a table) to a thread of the host, and gets the result back at the start of
//! a later `run`.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use super::abi::Handle;

pub const WORKER_SCHEDULE: &str = "http://lv2plug.in/ns/ext/worker#schedule";
pub const WORKER_INTERFACE: &str = "http://lv2plug.in/ns/ext/worker#interface";

/// Largest message either way.
const MSG_BYTES: usize = 4096;
const QUEUE: usize = 32;

pub type RespondFn = unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> i32;

#[repr(C)]
pub struct WorkerInterface {
    pub work: unsafe extern "C" fn(Handle, RespondFn, *mut c_void, u32, *const c_void) -> i32,
    pub work_response: unsafe extern "C" fn(Handle, u32, *const c_void) -> i32,
    pub end_run: Option<unsafe extern "C" fn(Handle) -> i32>,
}

#[repr(C)]
pub struct WorkerSchedule {
    pub handle: *mut c_void,
    pub schedule_work: unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> i32,
}

struct Msg {
    len: u32,
    data: [u8; MSG_BYTES],
}

/// Shared by the plugin (which schedules), the worker thread, and the audio thread (which
/// delivers the responses).
pub struct Worker {
    jobs_tx: Mutex<rtrb::Producer<Msg>>,
    jobs_rx: Mutex<rtrb::Consumer<Msg>>,
    resp_tx: Mutex<rtrb::Producer<Msg>>,
    resp_rx: Mutex<rtrb::Consumer<Msg>>,
    stop: AtomicBool,
    handle: AtomicPtr<c_void>,
    iface: AtomicPtr<WorkerInterface>,
    schedule: WorkerSchedule,
}

// SAFETY: the raw pointers are the plugin's handle and interface, used under the extension's
// own threading rules.
unsafe impl Send for Worker {}
unsafe impl Sync for Worker {}

unsafe extern "C" fn schedule_work(handle: *mut c_void, size: u32, data: *const c_void) -> i32 {
    // SAFETY: `handle` is the `Worker` that made the schedule struct, alive as long as the plugin.
    let w = unsafe { &*(handle as *const Worker) };
    push(&w.jobs_tx, size, data)
}

unsafe extern "C" fn respond(handle: *mut c_void, size: u32, data: *const c_void) -> i32 {
    // SAFETY: `handle` is the `Worker`, passed to `work`.
    let w = unsafe { &*(handle as *const Worker) };
    push(&w.resp_tx, size, data)
}

fn push(queue: &Mutex<rtrb::Producer<Msg>>, size: u32, data: *const c_void) -> i32 {
    if size as usize > MSG_BYTES || (data.is_null() && size > 0) {
        return 1; // LV2_WORKER_ERR_UNKNOWN
    }
    let Ok(mut q) = queue.try_lock() else {
        return 2;
    };
    let mut msg = Msg {
        len: size,
        data: [0; MSG_BYTES],
    };
    if size > 0 {
        // SAFETY: `size` bytes at `data`.
        msg.data[..size as usize].copy_from_slice(unsafe {
            std::slice::from_raw_parts(data as *const u8, size as usize)
        });
    }
    match q.push(msg) {
        Ok(()) => 0,
        Err(_) => 2, // LV2_WORKER_ERR_NO_SPACE
    }
}

impl Worker {
    pub fn new() -> Arc<Self> {
        let (jt, jr) = rtrb::RingBuffer::new(QUEUE);
        let (rt, rr) = rtrb::RingBuffer::new(QUEUE);
        // The schedule struct needs the address of the worker, which exists once it is in its
        // `Arc`: build it with a placeholder and fix the handle up right after.
        let mut arc = Arc::new(Self {
            jobs_tx: Mutex::new(jt),
            jobs_rx: Mutex::new(jr),
            resp_tx: Mutex::new(rt),
            resp_rx: Mutex::new(rr),
            stop: AtomicBool::new(false),
            handle: AtomicPtr::new(std::ptr::null_mut()),
            iface: AtomicPtr::new(std::ptr::null_mut()),
            schedule: WorkerSchedule {
                handle: std::ptr::null_mut(),
                schedule_work,
            },
        });
        let addr = Arc::as_ptr(&arc) as *mut c_void;
        Arc::get_mut(&mut arc).unwrap().schedule.handle = addr;
        arc
    }

    /// The feature's data: what the plugin calls to schedule work.
    pub fn schedule_ptr(&self) -> *mut c_void {
        &self.schedule as *const WorkerSchedule as *mut c_void
    }

    /// Starts the thread, once the plugin exists and has said it has a worker interface.
    pub fn start(
        self: &Arc<Self>,
        handle: Handle,
        iface: *const WorkerInterface,
    ) -> JoinHandle<()> {
        self.handle.store(handle, Ordering::Release);
        self.iface
            .store(iface as *mut WorkerInterface, Ordering::Release);
        let w = self.clone();
        std::thread::Builder::new()
            .name("lv2-worker".into())
            .spawn(move || {
                while !w.stop.load(Ordering::Relaxed) {
                    let job = w.jobs_rx.lock().unwrap().pop().ok();
                    match job {
                        Some(m) => {
                            let h = w.handle.load(Ordering::Acquire);
                            let i = w.iface.load(Ordering::Acquire);
                            // SAFETY: the plugin's own worker interface and handle; the instance
                            // joins this thread before cleaning up.
                            unsafe {
                                ((*i).work)(
                                    h,
                                    respond,
                                    Arc::as_ptr(&w) as *mut c_void,
                                    m.len,
                                    m.data.as_ptr().cast(),
                                );
                            }
                        }
                        None => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            })
            .expect("cannot start the LV2 worker thread")
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Audio thread, before `run`: hands the plugin what the worker finished.
    pub fn deliver(&self, handle: Handle) {
        let i = self.iface.load(Ordering::Acquire);
        if i.is_null() {
            return;
        }
        let Ok(mut rx) = self.resp_rx.try_lock() else {
            return;
        };
        while let Ok(m) = rx.pop() {
            // SAFETY: the plugin's own interface, called on the thread that runs the plugin.
            unsafe { ((*i).work_response)(handle, m.len, m.data.as_ptr().cast()) };
        }
    }

    /// Audio thread, after `run`.
    pub fn end_run(&self, handle: Handle) {
        let i = self.iface.load(Ordering::Acquire);
        if i.is_null() {
            return;
        }
        // SAFETY: as in `deliver`.
        unsafe {
            if let Some(f) = (*i).end_run {
                f(handle);
            }
        }
    }
}
