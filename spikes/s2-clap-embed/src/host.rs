//! CLAP host callbacks: main-thread wakeups, GUI requests, timers and posix fds.
//!
//! Everything a plugin asks of the main thread is turned into a winit user event, so the
//! main loop sleeps until there is real work (no polling).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::fd::RawFd;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
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
use winit::event_loop::EventLoopProxy;

#[derive(Debug, Clone, Copy)]
pub enum UserEvent {
    /// The plugin called `request_callback`.
    MainThreadCallback,
    /// The plugin wants its editor resized.
    GuiResize(GuiSize),
    /// The plugin's (floating) GUI was closed.
    GuiClosed,
    /// One of the plugin's registered fds became ready.
    FdReady,
}

pub struct S2Host;

impl HostHandlers for S2Host {
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
    // A Mutex because request_callback may come from any thread. Not realtime-safe if a
    // plugin calls it from process(); the real engine will use a lock-free flag instead.
    proxy: Mutex<EventLoopProxy<UserEvent>>,
}

impl Shared {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            proxy: Mutex::new(proxy),
        }
    }

    fn send(&self, event: UserEvent) {
        let _ = self.proxy.lock().unwrap().send_event(event);
    }
}

impl<'a> SharedHandler<'a> for Shared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {
        self.send(UserEvent::MainThreadCallback);
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
        self.send(UserEvent::GuiResize(new_size));
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.send(UserEvent::GuiClosed);
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
        // CLAP suggests hosts may clamp; 10 ms keeps UIs smooth without burning CPU.
        let period = Duration::from_millis(period_ms.max(10) as u64);
        self.timers.borrow_mut().insert(
            id,
            Timer {
                period,
                next: Instant::now() + period,
            },
        );
        println!("timer {} registered: {} ms", id.0, period.as_millis());
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
        println!("fd {fd} registered: {flags:?}");
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

/// Watches the plugin's fds on a helper thread and wakes the main loop when one is ready.
/// After waking it waits for an ack, so a still-readable fd cannot make it spin.
pub struct FdPoller {
    list: Arc<Mutex<Vec<libc::pollfd>>>,
    wake_w: RawFd,
    ack: mpsc::Sender<()>,
}

impl FdPoller {
    pub fn spawn(proxy: EventLoopProxy<UserEvent>) -> Self {
        let mut pipe = [0; 2];
        // SAFETY: plain pipe creation.
        unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        let (wake_r, wake_w) = (pipe[0], pipe[1]);
        let list: Arc<Mutex<Vec<libc::pollfd>>> = Arc::default();
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

    /// Called on the main thread after an FdReady event: returns the ready fds, then lets the
    /// poller continue.
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

    pub fn ack(&self) {
        let _ = self.ack.send(());
    }
}
