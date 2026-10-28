//! The plugin host process (`merlin-plugin-host`): loads one plugin and serves the app.
//!
//! Its main thread is the plugin's main thread: it runs requests from the app, the plugin's
//! callbacks, timers and fds. A second thread plays audio blocks out of shared memory. If a
//! plugin takes this process down, the app notices and carries on without it.

use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use merlin_hosting::{
    HostEvent, Io, MidiEvent, PluginHandle, PluginProcessor, ProcessStatus, Waker, host_time,
    open_plugin,
};

use crate::proto::{Request, Response, ToApp, ToHost, read_message, write_message};
use crate::shm::{MAX_EVENTS, Shm, futex_wait, futex_wake};

/// What the main thread has to look at.
enum Task {
    Request(u64, Request, Vec<u8>),
    Host(HostEvent),
}

struct HostWaker(Mutex<Sender<Task>>);

impl Waker for HostWaker {
    fn wake(&self, event: HostEvent) {
        let _ = self.0.lock().unwrap().send(Task::Host(event));
    }
}

type Writer = Arc<Mutex<UnixStream>>;

fn reply(w: &Writer, id: u64, response: Response, blob: &[u8]) {
    let msg = ToApp::Reply { id, response };
    if write_message(&mut *w.lock().unwrap(), &msg, blob).is_err() {
        die(0);
    }
}

/// Ends the process and everything it started (Wine hosts), without running plugin destructors.
fn die(code: i32) -> ! {
    // SAFETY: if we lead our own process group (the app made it so), take the group down with
    // us; _exit never returns.
    unsafe {
        if libc::getpgrp() == libc::getpid() {
            // The whole group, ourselves included.
            libc::kill(0, libc::SIGKILL);
        }
        libc::_exit(code)
    }
}

struct AudioThread {
    handle: JoinHandle<()>,
}

/// Plays blocks until the header says stop.
fn audio_loop(shm: Arc<Shm>, mut processor: Box<dyn PluginProcessor>) {
    let h = shm.header();
    let mut events: Vec<MidiEvent> = Vec::with_capacity(MAX_EVENTS);
    // The last block that was finished: a request made before this thread got going is not lost.
    let mut last = h.done.load(Ordering::Acquire);
    let mut scheduled = false;
    loop {
        // Wake now and then to see whether we were told to stop.
        let req = loop {
            if h.stop.load(Ordering::Acquire) != 0 {
                return;
            }
            let req = h.req.load(Ordering::Acquire);
            if req != last {
                break req;
            }
            futex_wait(&h.req, last, Some(Duration::from_millis(100)));
        };
        last = req;
        if !scheduled {
            scheduled = true;
            let policy = h.sched_policy.load(Ordering::Relaxed);
            if policy != 0 {
                // SAFETY: scheduling the calling thread like the app's audio thread.
                let ok = unsafe {
                    let p = libc::sched_param {
                        sched_priority: h.sched_priority.load(Ordering::Relaxed) as i32,
                    };
                    libc::sched_setscheduler(0, policy as i32, &p) == 0
                };
                if !ok {
                    eprintln!(
                        "plugin host: cannot get realtime scheduling ({}); audio runs at normal priority",
                        std::io::Error::last_os_error()
                    );
                }
            }
        }
        host_time::set_tempo(f32::from_bits(h.tempo_bits.load(Ordering::Relaxed)));
        host_time::set_playing(h.playing.load(Ordering::Relaxed) != 0);
        host_time::set_beats_per_bar(h.beats_per_bar.load(Ordering::Relaxed));
        let frames = h.frames.load(Ordering::Relaxed) as usize;
        // SAFETY: the block belongs to this thread from the request until `done` is set.
        let status = unsafe {
            shm.take_events(h.events.load(Ordering::Relaxed) as usize, &mut events);
            let out_l = shm.buffer(2, frames);
            let out_r = shm.buffer(3, frames);
            out_l.fill(0.0);
            out_r.fill(0.0);
            let input = (h.has_input.load(Ordering::Relaxed) != 0)
                .then(|| (&*shm.buffer(0, frames), &*shm.buffer(1, frames)));
            processor.process(
                &mut Io {
                    input,
                    out_l,
                    out_r,
                },
                &events,
            )
        };
        h.sleep
            .store((status == ProcessStatus::Sleep) as u32, Ordering::Relaxed);
        h.done.store(req, Ordering::Release);
        futex_wake(&h.done);
    }
}

struct Host {
    shm: Arc<Shm>,
    writer: Writer,
    tasks: Sender<Task>,
    plugin: Option<Box<dyn PluginHandle>>,
    audio: Option<AudioThread>,
    /// Stops the audio thread without waiting on the app.
    stop: Arc<AtomicBool>,
}

impl Host {
    fn stop_audio(&mut self) {
        if let Some(a) = self.audio.take() {
            let h = self.shm.header();
            h.stop.store(1, Ordering::Release);
            futex_wake(&h.req);
            let _ = a.handle.join();
            h.stop.store(0, Ordering::Release);
        }
        self.stop.store(true, Ordering::SeqCst);
    }

    fn plugin(&mut self) -> Result<&mut Box<dyn PluginHandle>> {
        self.plugin.as_mut().context("no plugin loaded")
    }

    /// Runs one request. Returns the response and the blob that goes with it.
    fn handle(&mut self, request: Request, blob: Vec<u8>) -> Result<(Response, Vec<u8>)> {
        let ok = |r| Ok((r, Vec::new()));
        match request {
            Request::Load {
                path,
                id,
                sample_rate,
                max_block,
            } => {
                let waker = Arc::new(HostWaker(Mutex::new(self.tasks.clone())));
                let mut plugin = open_plugin(&path, &id, waker, sample_rate, max_block)?;
                let info = plugin.info().clone();
                let has_editor = plugin.editor().is_some();
                self.plugin = Some(plugin);
                ok(Response::Loaded { info, has_editor })
            }
            Request::Ping => ok(Response::Ok),
            Request::Activate {
                sample_rate,
                max_block,
            } => {
                let processor = self.plugin()?.activate(sample_rate, max_block)?;
                self.stop_audio();
                let shm = self.shm.clone();
                let handle = std::thread::Builder::new()
                    .name("plugin-audio".into())
                    .spawn(move || audio_loop(shm, processor))?;
                self.audio = Some(AudioThread { handle });
                ok(Response::Ok)
            }
            Request::Deactivate => {
                self.stop_audio();
                if let Some(p) = self.plugin.as_mut() {
                    p.deactivate();
                }
                ok(Response::Ok)
            }
            Request::Params => ok(Response::Params(self.plugin()?.params())),
            Request::ParamValues => {
                let plugin = self.plugin()?;
                let ids: Vec<u32> = plugin.params().iter().map(|p| p.id).collect();
                let values = ids
                    .into_iter()
                    .filter_map(|id| Some((id, plugin.param_value(id)?)))
                    .collect();
                ok(Response::ParamValues(values))
            }
            Request::SetParam { id, value } => {
                self.plugin()?.set_param(id, value);
                ok(Response::Ok)
            }
            Request::SaveState => Ok((Response::State, self.plugin()?.save_state()?)),
            Request::LoadState => {
                self.plugin()?.load_state(&blob)?;
                ok(Response::Ok)
            }
            Request::EditorAttach { parent, scale } => {
                let ed = self.editor()?;
                let size = ed.attach(parent, scale)?;
                ok(Response::Attached {
                    size,
                    can_resize: ed.can_resize(),
                })
            }
            Request::EditorShow => {
                self.editor()?.show();
                ok(Response::Ok)
            }
            Request::EditorHide => {
                self.editor()?.hide();
                ok(Response::Ok)
            }
            Request::EditorAdjust(want) => ok(Response::Size(self.editor()?.adjust_size(want))),
            Request::EditorSet(size) => ok(Response::Resized(self.editor()?.set_size(size))),
            Request::EditorNote(size) => {
                self.editor()?.note_size(size);
                ok(Response::Ok)
            }
            Request::EditorZoom(scale) => ok(Response::Zoomed(self.editor()?.set_zoom(scale))),
            Request::EditorDetach => {
                self.editor()?.detach();
                ok(Response::Ok)
            }
            Request::Shutdown => {
                self.stop_audio();
                drop(self.plugin.take());
                die(0)
            }
        }
    }

    fn editor(&mut self) -> Result<&mut dyn merlin_hosting::PluginEditor> {
        match self.plugin()?.editor() {
            Some(e) => Ok(e),
            None => bail!("the plugin has no editor"),
        }
    }
}

fn forward(w: &Writer, event: HostEvent) {
    if write_message(&mut *w.lock().unwrap(), &ToApp::Event(event), &[]).is_err() {
        die(0);
    }
}

/// Serves the app on `socket` and `shm_fd` (both inherited) until told to stop. Never returns.
pub fn serve(socket: i32, shm_fd: i32) -> ! {
    // A panic would unwind into plugin code or be swallowed by a plugin thread: end the process.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("plugin host: {info}");
        die(101)
    }));
    // SAFETY: the app passed us these descriptors for our own use.
    let (stream, shm) = unsafe { (UnixStream::from_raw_fd(socket), Shm::from_fd(shm_fd)) };
    let shm = match shm {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("plugin host: {e:#}");
            die(2)
        }
    };
    let writer: Writer = Arc::new(Mutex::new(stream.try_clone().expect("clone socket")));
    let (tx, rx) = mpsc::channel::<Task>();

    // The app closing the socket means the app is gone (or done with us): leave at once, even
    // if the plugin has the main thread stuck.
    let reader_tx = tx.clone();
    let mut read_half = stream;
    std::thread::Builder::new()
        .name("app-read".into())
        .spawn(move || {
            loop {
                match read_message::<ToHost>(&mut read_half) {
                    Ok(Some((m, blob))) => {
                        if reader_tx
                            .send(Task::Request(m.id, m.request, blob))
                            .is_err()
                        {
                            die(0);
                        }
                    }
                    Ok(None) | Err(_) => die(0),
                }
            }
        })
        .expect("spawn reader");

    main_loop(
        Host {
            shm,
            writer,
            tasks: tx,
            plugin: None,
            audio: None,
            stop: Arc::new(AtomicBool::new(false)),
        },
        rx,
    )
}

fn main_loop(mut host: Host, rx: Receiver<Task>) -> ! {
    loop {
        let deadline = host.plugin.as_ref().and_then(|p| p.next_deadline());
        let task = match deadline {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(t) => Some(t),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => die(0),
            },
            None => match rx.recv() {
                Ok(t) => Some(t),
                Err(_) => die(0),
            },
        };
        match task {
            Some(Task::Request(id, request, blob)) => {
                let wants_reply = request.wants_reply();
                let result = host.handle(request, blob);
                if wants_reply {
                    let writer = host.writer.clone();
                    match result {
                        Ok((response, blob)) => reply(&writer, id, response, &blob),
                        Err(e) => reply(&writer, id, Response::Err(format!("{e:#}")), &[]),
                    }
                }
            }
            Some(Task::Host(event)) => match event {
                HostEvent::MainThreadCallback => {
                    if let Some(p) = host.plugin.as_mut() {
                        p.on_main_thread();
                    }
                }
                HostEvent::FdReady => {
                    if let Some(p) = host.plugin.as_mut() {
                        p.on_fd_ready();
                    }
                }
                HostEvent::EditorResize { .. }
                | HostEvent::EditorClosed
                | HostEvent::StateDirty
                | HostEvent::Crashed => forward(&host.writer, event),
            },
            None => {}
        }
        if let Some(p) = host.plugin.as_mut() {
            p.service(Instant::now());
        }
    }
}

/// The `merlin-plugin-host` executable: `merlin-plugin-host --sandbox <socket fd> <shm fd>`.
pub fn main() -> ! {
    let args: Vec<String> = std::env::args().collect();
    match args.as_slice() {
        [_, flag, sock, shm] if flag == "--sandbox" => match (sock.parse(), shm.parse()) {
            (Ok(sock), Ok(shm)) => serve(sock, shm),
            _ => {
                eprintln!("usage: merlin-plugin-host --sandbox <socket fd> <shm fd>");
                std::process::exit(64)
            }
        },
        _ => {
            eprintln!(
                "merlin-plugin-host runs one plugin for the Merlin app; it is not meant to be started by hand"
            );
            std::process::exit(64)
        }
    }
}
