//! The app's side of a sandboxed plugin: a [`PluginHandle`] and a [`PluginProcessor`] that look
//! like any other plugin but run in a `merlin-plugin-host` process.
//!
//! If that process crashes or stops answering, nothing here blocks or aborts: the processor goes
//! silent, every call fails at once, and the app is told with [`HostEvent::Crashed`].

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use merlin_hosting::{
    EditorSize, HostEvent, Io, MidiEvent, ParamInfo, PluginEditor, PluginHandle, PluginInfo,
    PluginProcessor, ProcessStatus, Waker, host_time,
};

use crate::proto::{Request, Response, ToApp, ToHost, read_message, write_message};
use crate::shm::{MAX_FRAMES, Shm, futex_wait, futex_wake};

/// `Header::done` after the host is gone: never equal to a request number.
const DEAD: u32 = u32::MAX;

/// The longest the audio thread waits for the host to finish one block before giving up on the
/// plugin. A healthy plugin takes a fraction of a block; this only bounds a hang.
const BLOCK_TIMEOUT: Duration = Duration::from_secs(1);

/// How often a plugin is asked whether its main thread still answers, and how long it has.
const PING_EVERY: Duration = Duration::from_secs(3);
const PING_TIMEOUT: Duration = Duration::from_secs(8);

const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const STATE_TIMEOUT: Duration = Duration::from_secs(12);
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(25);
const VALUES_TIMEOUT: Duration = Duration::from_secs(5);
/// Plugins that were just asked for their parameters are not asked again within this.
const VALUES_TTL: Duration = Duration::from_millis(25);

/// Why a plugin process is gone. An error of this type tells a crash or hang from an ordinary
/// failure (a plugin that refuses to load).
#[derive(Debug)]
pub struct PluginFailed(pub String);

impl std::fmt::Display for PluginFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the plugin {}", self.0)
    }
}

impl std::error::Error for PluginFailed {}

fn signal_name(sig: i32) -> String {
    match sig {
        libc::SIGSEGV => "SIGSEGV, a memory access error".into(),
        libc::SIGABRT => "SIGABRT, it aborted".into(),
        libc::SIGBUS => "SIGBUS, a memory access error".into(),
        libc::SIGILL => "SIGILL, an illegal instruction".into(),
        libc::SIGFPE => "SIGFPE, an arithmetic error".into(),
        libc::SIGKILL => "SIGKILL, it was killed".into(),
        libc::SIGTERM => "SIGTERM, it was told to stop".into(),
        n => format!("signal {n}"),
    }
}

fn describe_exit(status: ExitStatus) -> String {
    match status.signal() {
        Some(sig) => format!("crashed ({})", signal_name(sig)),
        None => format!(
            "exited unexpectedly (status {})",
            status.code().unwrap_or(-1)
        ),
    }
}

/// What the app threads, the audio thread and the helper threads share about one host process.
struct Shared {
    shm: Shm,
    pid: i32,
    waker: Arc<dyn Waker>,
    reason: Mutex<Option<String>>,
    /// The app is closing the plugin on purpose: the process ending is no failure.
    closing: AtomicBool,
    /// The process has exited and been reaped.
    exited: AtomicBool,
    exit: Mutex<Option<ExitStatus>>,
    /// [`HostEvent::Crashed`] went out (or must not, while the plugin is still loading).
    reported: AtomicBool,
}

impl Shared {
    fn failure(&self) -> Option<String> {
        self.reason.lock().unwrap().clone()
    }

    fn is_dead(&self) -> bool {
        self.shm.header().dead.load(Ordering::Acquire) != 0
    }

    /// Kills the host and everything it started (Wine hosts of Windows plugins).
    fn kill(&self) {
        if !self.exited.load(Ordering::SeqCst) {
            // SAFETY: signalling the process group we made for the host.
            unsafe { libc::kill(-self.pid, libc::SIGKILL) };
        }
    }

    /// Gives up on the plugin: records why (the first reason stays), frees the audio thread, kills the process.
    fn fail(&self, reason: String) {
        {
            let mut r = self.reason.lock().unwrap();
            if r.is_none() {
                *r = Some(reason);
            }
        }
        let h = self.shm.header();
        h.dead.store(1, Ordering::Release);
        h.done.store(DEAD, Ordering::Release);
        futex_wake(&h.done);
        self.kill();
        if !self.reported.swap(true, Ordering::SeqCst) {
            self.waker.wake(HostEvent::Crashed);
        }
    }

    fn wait_exited(&self, limit: Duration) -> bool {
        let until = Instant::now() + limit;
        while !self.exited.load(Ordering::SeqCst) {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }
}

/// A plugin running in its own process (main-thread half).
pub struct RemotePlugin {
    info: PluginInfo,
    shared: Arc<Shared>,
    writer: Mutex<UnixStream>,
    replies: mpsc::Receiver<(u64, Response, Vec<u8>)>,
    next_id: u64,
    next_ping: Instant,
    /// The ping that is out, and when it was sent.
    ping: Option<(u64, Instant)>,
    values: Option<(Instant, HashMap<u32, f64>)>,
    has_editor: bool,
    editor: EditorCache,
}

#[derive(Default)]
struct EditorCache {
    attached: bool,
    can_resize: bool,
    size: Option<EditorSize>,
}

impl RemotePlugin {
    /// Starts `host_exe` and has it load `id` from `path`. `load_timeout` bounds the whole load
    /// (a cold Wine start takes a while).
    pub fn spawn(
        host_exe: &Path,
        path: &Path,
        id: &str,
        waker: Arc<dyn Waker>,
        sample_rate: f64,
        max_block: u32,
        load_timeout: Duration,
    ) -> Result<Self> {
        let shm = Shm::create()?;
        let (mine, theirs) = UnixStream::pair()?;
        let (sock_fd, shm_fd) = (theirs.as_raw_fd(), shm.raw_fd());
        let mut cmd = Command::new(host_exe);
        cmd.arg("--sandbox")
            .arg(sock_fd.to_string())
            .arg(shm_fd.to_string())
            .stdin(Stdio::null())
            // Its own group: killing it takes the Wine processes it starts along.
            .process_group(0);
        // SAFETY: only fcntl, which is async-signal-safe, runs between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                for fd in [sock_fd, shm_fd] {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("cannot start {}: {e}", host_exe.display()))?;
        drop(theirs);
        let pid = child.id() as i32;

        let shared = Arc::new(Shared {
            shm,
            pid,
            waker: waker.clone(),
            reason: Mutex::new(None),
            closing: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            exit: Mutex::new(None),
            // Nobody is told about a crash while the plugin loads: the load itself fails.
            reported: AtomicBool::new(true),
        });

        // Reaps the process, then cleans up what it left in its group and wakes the reader.
        let for_waiter = shared.clone();
        let wake_reader = mine.try_clone()?;
        std::thread::Builder::new()
            .name("plugin-wait".into())
            .spawn(move || {
                let status = child.wait().ok();
                *for_waiter.exit.lock().unwrap() = status;
                for_waiter.exited.store(true, Ordering::SeqCst);
                // SAFETY: stragglers of the host's process group (a Wine host), if any.
                unsafe { libc::kill(-for_waiter.pid, libc::SIGKILL) };
                let _ = wake_reader.shutdown(std::net::Shutdown::Both);
            })?;

        let (tx, replies) = mpsc::channel();
        let mut read_half = mine.try_clone()?;
        let for_reader = shared.clone();
        std::thread::Builder::new()
            .name("plugin-read".into())
            .spawn(move || {
                loop {
                    match read_message::<ToApp>(&mut read_half) {
                        Ok(Some((ToApp::Reply { id, response }, blob))) => {
                            let _ = tx.send((id, response, blob));
                        }
                        Ok(Some((ToApp::Event(event), _))) => for_reader.waker.wake(event),
                        Ok(None) | Err(_) => break,
                    }
                }
                // The host closed the socket: it is gone or going. Give it a moment to be reaped.
                if !for_reader.wait_exited(Duration::from_secs(2)) {
                    for_reader.kill();
                    for_reader.wait_exited(Duration::from_secs(2));
                }
                if !for_reader.closing.load(Ordering::SeqCst) {
                    let status = *for_reader.exit.lock().unwrap();
                    for_reader
                        .fail(status.map_or_else(|| "stopped unexpectedly".into(), describe_exit));
                }
                // `tx` drops here, after the reason is set: a waiting call sees both.
            })?;

        mine.set_write_timeout(Some(Duration::from_secs(3)))?;
        let mut plugin = Self {
            info: PluginInfo {
                format: merlin_model::Format::Clap,
                id: id.to_owned(),
                name: String::new(),
                vendor: String::new(),
                version: String::new(),
                features: Vec::new(),
            },
            shared,
            writer: Mutex::new(mine),
            replies,
            next_id: 0,
            next_ping: Instant::now() + PING_EVERY,
            ping: None,
            values: None,
            has_editor: false,
            editor: EditorCache::default(),
        };
        let load = Request::Load {
            path: path.to_owned(),
            id: id.to_owned(),
            sample_rate,
            max_block,
        };
        match plugin.call(load, &[], load_timeout, "loading") {
            Ok((Response::Loaded { info, has_editor }, _)) => {
                plugin.info = info;
                plugin.has_editor = has_editor;
                plugin.shared.reported.store(false, Ordering::SeqCst);
                Ok(plugin)
            }
            Ok(_) => bail!("the plugin host answered wrongly"),
            Err(e) => Err(e),
        }
    }

    /// The host process.
    pub fn pid(&self) -> u32 {
        self.shared.pid as u32
    }

    fn send(&self, id: u64, request: Request, blob: &[u8]) -> Result<()> {
        let sent = write_message(
            &mut *self.writer.lock().unwrap(),
            &ToHost { id, request },
            blob,
        );
        if let Err(e) = sent {
            self.shared.fail(format!("stopped answering ({e})"));
            return Err(self.failed());
        }
        Ok(())
    }

    fn failed(&self) -> anyhow::Error {
        PluginFailed(
            self.shared
                .failure()
                .unwrap_or_else(|| "is not running".into()),
        )
        .into()
    }

    /// Sends a request and waits for its answer. A host that does not answer in time is killed.
    fn call(
        &mut self,
        request: Request,
        blob: &[u8],
        timeout: Duration,
        what: &'static str,
    ) -> Result<(Response, Vec<u8>)> {
        if self.shared.is_dead() {
            return Err(self.failed());
        }
        self.next_id += 1;
        let id = self.next_id;
        self.send(id, request, blob)?;
        let started = Instant::now();
        loop {
            match self
                .replies
                .recv_timeout(timeout.saturating_sub(started.elapsed()))
            {
                Ok((rid, response, blob)) if rid == id => {
                    self.next_ping = Instant::now() + PING_EVERY;
                    return match response {
                        Response::Err(e) => Err(anyhow!(e)),
                        r => Ok((r, blob)),
                    };
                }
                Ok((rid, ..)) => {
                    if self.ping.is_some_and(|(p, _)| p == rid) {
                        self.ping = None;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.shared.fail(format!("stopped responding while {what}"));
                    return Err(self.failed());
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.failed()),
            }
        }
    }

    /// A request that needs no answer.
    fn tell(&mut self, request: Request) {
        if self.shared.is_dead() {
            return;
        }
        self.next_id += 1;
        let _ = self.send(self.next_id, request, &[]);
    }

    fn call_ok(&mut self, request: Request, what: &'static str) -> Result<Response> {
        Ok(self.call(request, &[], CALL_TIMEOUT, what)?.0)
    }
}

impl Drop for RemotePlugin {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        if !self.shared.is_dead() {
            self.next_id += 1;
            let _ = self.send(self.next_id, Request::Shutdown, &[]);
        }
        // A plugin that hangs while it is destroyed does not hold the app up.
        if !self.shared.wait_exited(Duration::from_secs(2)) {
            self.shared.kill();
            self.shared.wait_exited(Duration::from_secs(1));
        }
    }
}

impl PluginHandle for RemotePlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn failure(&self) -> Option<String> {
        self.shared.failure()
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.shared.pid as u32)
    }

    fn activate(&mut self, sample_rate: f64, max_block: u32) -> Result<Box<dyn PluginProcessor>> {
        if max_block as usize > MAX_FRAMES {
            bail!("block size {max_block} exceeds {MAX_FRAMES}");
        }
        let request = Request::Activate {
            sample_rate,
            max_block,
        };
        self.call(request, &[], ACTIVATE_TIMEOUT, "starting up")?;
        Ok(Box::new(RemoteProcessor {
            shared: self.shared.clone(),
            told_scheduling: false,
        }))
    }

    fn deactivate(&mut self) {
        let _ = self.call_ok(Request::Deactivate, "shutting down");
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        match self.call_ok(Request::Params, "listing its parameters") {
            Ok(Response::Params(p)) => p,
            _ => Vec::new(),
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.tell(Request::SetParam { id, value });
        if let Some((_, values)) = &mut self.values {
            values.insert(id, value);
        }
    }

    fn param_value(&mut self, id: u32) -> Option<f64> {
        let fresh = self
            .values
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() < VALUES_TTL);
        if !fresh {
            // One round trip reads every parameter: asking one by one would take thousands.
            let Ok((Response::ParamValues(all), _)) = self.call(
                Request::ParamValues,
                &[],
                VALUES_TIMEOUT,
                "reading parameters",
            ) else {
                return None;
            };
            self.values = Some((Instant::now(), all.into_iter().collect()));
        }
        self.values.as_ref()?.1.get(&id).copied()
    }

    fn save_state(&mut self) -> Result<Vec<u8>> {
        match self.call(Request::SaveState, &[], STATE_TIMEOUT, "saving its state")? {
            (Response::State, blob) => Ok(blob),
            _ => bail!("the plugin host answered wrongly"),
        }
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        self.call(Request::LoadState, data, STATE_TIMEOUT, "loading its state")?;
        self.values = None;
        Ok(())
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        self.has_editor.then_some(self as &mut dyn PluginEditor)
    }

    // The plugin's own main thread lives in the host process: its callbacks, timers and fds are
    // served there.
    fn on_main_thread(&mut self) {}

    fn service(&mut self, now: Instant) {
        if self.shared.is_dead() {
            return;
        }
        // Pings never make the app wait: one goes out, and the answer is looked for next time.
        while let Ok((rid, ..)) = self.replies.try_recv() {
            if self.ping.is_some_and(|(p, _)| p == rid) {
                self.ping = None;
            }
        }
        match self.ping {
            Some((_, sent)) if now.saturating_duration_since(sent) > PING_TIMEOUT => {
                // The host's main thread is stuck (its audio thread may still run).
                self.shared
                    .fail("stopped responding while idle".to_string());
            }
            Some(_) => {}
            None if now >= self.next_ping => {
                self.next_ping = now + PING_EVERY;
                self.next_id += 1;
                let id = self.next_id;
                if self.send(id, Request::Ping, &[]).is_ok() {
                    self.ping = Some((id, now));
                }
            }
            None => {}
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        if self.shared.is_dead() {
            return None;
        }
        Some(match self.ping {
            Some((_, sent)) => sent + PING_TIMEOUT + Duration::from_millis(50),
            None => self.next_ping,
        })
    }

    fn on_fd_ready(&mut self) {}
}

impl PluginEditor for RemotePlugin {
    fn attach(&mut self, parent: u32, scale: f64) -> Result<EditorSize> {
        match self.call_ok(
            Request::EditorAttach { parent, scale },
            "opening its editor",
        )? {
            Response::Attached { size, can_resize } => {
                self.editor = EditorCache {
                    attached: true,
                    can_resize,
                    size: Some(size),
                };
                Ok(size)
            }
            _ => bail!("the plugin host answered wrongly"),
        }
    }

    fn is_attached(&self) -> bool {
        self.editor.attached
    }

    fn show(&mut self) {
        self.tell(Request::EditorShow);
    }

    fn hide(&mut self) {
        self.tell(Request::EditorHide);
    }

    fn size(&self) -> EditorSize {
        self.editor.size.unwrap_or(EditorSize {
            width: 1,
            height: 1,
        })
    }

    fn can_resize(&self) -> bool {
        self.editor.can_resize
    }

    fn adjust_size(&mut self, want: EditorSize) -> EditorSize {
        match self.call_ok(Request::EditorAdjust(want), "resizing its editor") {
            Ok(Response::Size(s)) => s,
            _ => want,
        }
    }

    fn set_size(&mut self, size: EditorSize) -> bool {
        let ok = matches!(
            self.call_ok(Request::EditorSet(size), "resizing its editor"),
            Ok(Response::Resized(true))
        );
        if ok {
            self.editor.size = Some(size);
        }
        ok
    }

    fn note_size(&mut self, size: EditorSize) {
        self.editor.size = Some(size);
        self.tell(Request::EditorNote(size));
    }

    fn set_zoom(&mut self, scale: f64) -> Option<EditorSize> {
        match self.call_ok(Request::EditorZoom(scale), "scaling its editor") {
            Ok(Response::Zoomed(Some(size))) => {
                self.editor.size = Some(size);
                Some(size)
            }
            _ => None,
        }
    }

    fn detach(&mut self) {
        let _ = self.call_ok(Request::EditorDetach, "closing its editor");
        self.editor.attached = false;
    }
}

/// The audio-thread half. Hands each block to the host's audio thread through shared memory.
pub struct RemoteProcessor {
    shared: Arc<Shared>,
    told_scheduling: bool,
}

impl RemoteProcessor {
    /// Tells the host how this thread is scheduled so its audio thread can match (once).
    fn share_scheduling(&mut self) {
        self.told_scheduling = true;
        // SAFETY: reading the calling thread's own scheduling.
        let (policy, priority) = unsafe {
            let mut p: libc::sched_param = std::mem::zeroed();
            let policy = libc::sched_getscheduler(0);
            libc::sched_getparam(0, &mut p);
            (policy & !libc::SCHED_RESET_ON_FORK, p.sched_priority)
        };
        let h = self.shared.shm.header();
        if policy == libc::SCHED_FIFO || policy == libc::SCHED_RR {
            h.sched_priority.store(priority as u32, Ordering::Relaxed);
            h.sched_policy.store(policy as u32, Ordering::Relaxed);
        }
    }
}

impl PluginProcessor for RemoteProcessor {
    fn process(&mut self, io: &mut Io, events: &[MidiEvent]) -> ProcessStatus {
        let frames = io.frames();
        if !self.told_scheduling {
            self.share_scheduling();
        }
        let shm = &self.shared.shm;
        let h = shm.header();
        if h.dead.load(Ordering::Acquire) != 0 || frames > MAX_FRAMES {
            // The engine zeroed the outputs: a dead plugin is silent.
            return ProcessStatus::Sleep;
        }
        // SAFETY: between blocks the app's audio thread owns the shared buffers.
        unsafe {
            match io.input {
                Some((l, r)) => {
                    shm.buffer(0, frames).copy_from_slice(l);
                    shm.buffer(1, frames).copy_from_slice(r);
                    h.has_input.store(1, Ordering::Relaxed);
                }
                None => h.has_input.store(0, Ordering::Relaxed),
            }
            h.events
                .store(shm.put_events(events) as u32, Ordering::Relaxed);
        }
        h.frames.store(frames as u32, Ordering::Relaxed);
        h.tempo_bits
            .store(host_time::tempo().to_bits(), Ordering::Relaxed);
        h.playing
            .store(host_time::playing() as u32, Ordering::Relaxed);
        h.beats_per_bar
            .store(host_time::beats_per_bar(), Ordering::Relaxed);
        h.beat_unit.store(host_time::beat_unit(), Ordering::Relaxed);

        let mut want = h.req.load(Ordering::Relaxed).wrapping_add(1);
        if want == DEAD {
            want = 0;
        }
        h.req.store(want, Ordering::Release);
        futex_wake(&h.req);

        let deadline = Instant::now() + BLOCK_TIMEOUT;
        let mut spins = 0;
        loop {
            let done = h.done.load(Ordering::Acquire);
            if done == want {
                break;
            }
            if done == DEAD || h.dead.load(Ordering::Acquire) != 0 {
                return ProcessStatus::Sleep;
            }
            spins += 1;
            if spins < 200 {
                std::hint::spin_loop();
                continue;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.shared
                    .fail("stopped responding while playing".to_string());
                return ProcessStatus::Sleep;
            }
            futex_wait(&h.done, done, Some(left));
        }
        // SAFETY: the host handed the block back (done == req).
        unsafe {
            io.out_l.copy_from_slice(shm.buffer(2, frames));
            io.out_r.copy_from_slice(shm.buffer(3, frames));
        }
        if h.sleep.load(Ordering::Relaxed) != 0 {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }
}
