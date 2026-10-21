//! CLAP host callbacks. Everything a plugin asks of the main thread becomes a [`HostEvent`] on
//! the app's [`Waker`], so the main loop sleeps until there is real work.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl, PluginGui};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::posix_fd::{FdFlags, HostPosixFd, HostPosixFdImpl, PluginPosixFd};
use clack_extensions::state::{HostState, HostStateImpl, PluginState};
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::prelude::*;

use crate::{HostEvent, Waker};

pub struct ClapHost;

impl HostHandlers for ClapHost {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostGui>()
            .register::<HostTimer>()
            .register::<HostPosixFd>()
            .register::<HostParams>()
            .register::<HostState>();
    }
}

pub struct Shared {
    waker: Arc<dyn Waker>,
}

impl Shared {
    pub fn new(waker: Arc<dyn Waker>) -> Self {
        Self { waker }
    }
}

impl<'a> SharedHandler<'a> for Shared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {
        self.waker.wake(HostEvent::MainThreadCallback);
    }
}

impl HostLogImpl for Shared {
    fn log(&self, severity: LogSeverity, message: &str) {
        if severity > LogSeverity::Debug {
            eprintln!("[plugin {severity}] {message}");
        }
    }
}

impl HostGuiImpl for Shared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        self.waker.wake(HostEvent::EditorResize {
            width: new_size.width,
            height: new_size.height,
        });
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.waker.wake(HostEvent::EditorClosed);
    }
}

impl HostParamsImplShared for Shared {
    fn request_flush(&self) {}
}

pub struct Timer {
    pub period: Duration,
    pub next: Instant,
}

pub struct MainThread<'a> {
    _shared: &'a Shared,
    pub gui: Cell<Option<PluginGui>>,
    pub timer_ext: Cell<Option<PluginTimer>>,
    pub fd_ext: Cell<Option<PluginPosixFd>>,
    pub state_ext: Cell<Option<PluginState>>,
    pub timers: RefCell<HashMap<TimerId, Timer>>,
    next_timer_id: Cell<u32>,
    pub fds: RefCell<HashMap<RawFd, FdFlags>>,
    pub fds_changed: Cell<bool>,
    pub state_dirty: Cell<bool>,
}

impl<'a> MainThread<'a> {
    pub fn new(shared: &'a Shared) -> Self {
        Self {
            _shared: shared,
            gui: Cell::new(None),
            timer_ext: Cell::new(None),
            fd_ext: Cell::new(None),
            state_ext: Cell::new(None),
            timers: RefCell::new(HashMap::new()),
            next_timer_id: Cell::new(0),
            fds: RefCell::new(HashMap::new()),
            fds_changed: Cell::new(false),
            state_dirty: Cell::new(false),
        }
    }

    /// Timer IDs that are due, rescheduling them. Collected first so that the plugin may
    /// register or unregister timers from inside `on_timer`.
    pub fn due_timers(&self, now: Instant) -> Vec<TimerId> {
        let mut due = Vec::new();
        for (id, t) in self.timers.borrow_mut().iter_mut() {
            if t.next <= now {
                due.push(*id);
                t.next = now + t.period;
            }
        }
        due
    }

    pub fn next_timer_deadline(&self) -> Option<Instant> {
        self.timers.borrow().values().map(|t| t.next).min()
    }
}

impl<'a> MainThreadHandler<'a> for MainThread<'a> {
    fn initialized(&self, instance: InitializedPluginHandle<'a>) {
        self.gui.set(instance.get_extension());
        self.timer_ext.set(instance.get_extension());
        self.fd_ext.set(instance.get_extension());
        self.state_ext.set(instance.get_extension());
    }
}

impl HostTimerImpl for MainThread<'_> {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        let id = TimerId(self.next_timer_id.get() + 1);
        self.next_timer_id.set(id.0);
        // CLAP lets hosts clamp; 10 ms keeps UIs smooth without burning CPU.
        let period = Duration::from_millis(period_ms.max(10) as u64);
        self.timers.borrow_mut().insert(
            id,
            Timer {
                period,
                next: Instant::now() + period,
            },
        );
        Ok(id)
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        self.timers
            .borrow_mut()
            .remove(&timer_id)
            .map(|_| ())
            .ok_or(HostError::Message("unknown timer"))
    }
}

impl HostPosixFdImpl for MainThread<'_> {
    fn register_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        self.fds.borrow_mut().insert(fd, flags);
        self.fds_changed.set(true);
        Ok(())
    }

    fn modify_fd(&self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        self.fds.borrow_mut().insert(fd, flags);
        self.fds_changed.set(true);
        Ok(())
    }

    fn unregister_fd(&self, fd: RawFd) -> Result<(), HostError> {
        self.fds.borrow_mut().remove(&fd);
        self.fds_changed.set(true);
        Ok(())
    }
}

impl HostParamsImplMainThread for MainThread<'_> {
    fn rescan(&self, _flags: ParamRescanFlags) {}
    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for MainThread<'_> {
    fn mark_dirty(&self) {
        self.state_dirty.set(true);
    }
}

fn poll_events(flags: FdFlags) -> i16 {
    let mut ev = 0;
    if flags.contains(FdFlags::READ) {
        ev |= libc::POLLIN;
    }
    if flags.contains(FdFlags::WRITE) {
        ev |= libc::POLLOUT;
    }
    ev
}

fn fd_flags(revents: i16) -> FdFlags {
    let mut f = FdFlags::empty();
    if revents & libc::POLLIN != 0 {
        f |= FdFlags::READ;
    }
    if revents & libc::POLLOUT != 0 {
        f |= FdFlags::WRITE;
    }
    if revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        f |= FdFlags::ERROR;
    }
    f
}

/// Watches a plugin's fds on a helper thread and wakes the app when one is ready. After
/// waking it waits for an ack, so a still-readable fd cannot make it spin. One per plugin.
pub struct FdPoller {
    list: Arc<Mutex<Vec<libc::pollfd>>>,
    wake_w: RawFd,
    ack: mpsc::Sender<()>,
    stop: Arc<AtomicBool>,
}

impl FdPoller {
    pub fn spawn(waker: Arc<dyn Waker>) -> Self {
        let mut pipe = [0; 2];
        // SAFETY: plain pipe creation.
        unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        let (wake_r, wake_w) = (pipe[0], pipe[1]);
        let list: Arc<Mutex<Vec<libc::pollfd>>> = Arc::default();
        let (ack, ack_rx) = mpsc::channel::<()>();
        let thread_list = list.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        std::thread::Builder::new()
            .name("plugin-fd-poller".into())
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
                        if thread_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        continue;
                    }
                    waker.wake(HostEvent::FdReady);
                    if ack_rx.recv().is_err() {
                        break;
                    }
                }
                // SAFETY: the read end is ours alone and nothing polls it again.
                unsafe { libc::close(wake_r) };
            })
            .expect("spawn fd poller");
        Self {
            list,
            wake_w,
            ack,
            stop,
        }
    }

    /// Replace the watched set (call after the plugin changed its fds).
    pub fn set(&self, fds: &HashMap<RawFd, FdFlags>) {
        *self.list.lock().unwrap() = fds
            .iter()
            .map(|(&fd, &flags)| libc::pollfd {
                fd,
                events: poll_events(flags),
                revents: 0,
            })
            .collect();
        // SAFETY: one byte into our own pipe to interrupt poll().
        unsafe { libc::write(self.wake_w, [1u8].as_ptr().cast(), 1) };
    }

    /// Called on the main thread after an FdReady event: returns the ready fds.
    pub fn ready(&self, fds: &HashMap<RawFd, FdFlags>) -> Vec<(RawFd, FdFlags)> {
        let mut pfds: Vec<libc::pollfd> = fds
            .iter()
            .map(|(&fd, &flags)| libc::pollfd {
                fd,
                events: poll_events(flags),
                revents: 0,
            })
            .collect();
        // SAFETY: valid pollfd array, zero timeout.
        unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, 0) };
        pfds.iter()
            .filter(|p| p.revents != 0)
            .map(|p| (p.fd, fd_flags(p.revents)))
            .collect()
    }

    /// Lets the poller continue after the ready fds were serviced.
    pub fn ack(&self) {
        let _ = self.ack.send(());
    }
}

impl Drop for FdPoller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // SAFETY: wake the poller so it sees `stop`, then close our write end.
        unsafe {
            libc::write(self.wake_w, [1u8].as_ptr().cast(), 1);
            libc::close(self.wake_w);
        }
    }
}
