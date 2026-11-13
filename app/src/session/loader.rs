//! Creating plugins off the main thread, so the window keeps drawing (and the rest of the rig
//! keeps answering) while a heavy plugin starts: a cold Wine start, a big sample library, an
//! SF3 to decode. A worker spawns the plugin's host process (or decodes the SoundFont), restores
//! its saved state and activates it; the main thread only installs the finished plugin. Several
//! plugins load side by side, a few at a time.
//!
//! Plugins loaded into the app's own process (`MERLIN_NO_SANDBOX`) are not created here: most
//! formats require their main thread to be ours.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use merlin_hosting::{PluginHandle, PluginProcessor, Waker};
use merlin_sandbox::RemotePlugin;
use winit::event_loop::EventLoopProxy;

use super::UserEvent;

/// Plugins created at the same time. More would mostly fight over the disk (sample libraries)
/// and over Wine's server; fewer would make a rig of Windows plugins wait for each cold start
/// in turn.
const AT_ONCE: usize = 4;

/// What a worker needs to create one plugin.
pub(super) struct Job {
    /// The editor id the plugin is for (a slot, or an insert's id).
    pub id: usize,
    pub generation: u64,
    pub path: PathBuf,
    pub plugin_id: String,
    pub windows: bool,
    pub state: Option<Vec<u8>>,
    pub waker: Arc<dyn Waker>,
    pub sample_rate: f64,
    pub max_block: u32,
    pub host: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    /// Set when the load is no longer wanted: the worker stops at the next step.
    pub cancelled: Arc<AtomicBool>,
}

/// A created and activated plugin, ready to be installed.
pub(super) struct Ready {
    pub plugin: Box<dyn PluginHandle>,
    pub processor: Box<dyn PluginProcessor>,
}

/// How a load ended.
pub(super) struct Done {
    pub id: usize,
    pub generation: u64,
    pub result: Result<Ready>,
    pub elapsed: Duration,
}

/// What crosses from a worker to the main thread (the plugin must be `Send` for that).
struct Sent {
    id: usize,
    generation: u64,
    result: Result<(Box<dyn PluginHandle + Send>, Box<dyn PluginProcessor>)>,
    elapsed: Duration,
}

/// A counting gate: at most [`AT_ONCE`] workers create plugins at a time.
#[derive(Default)]
struct Gate {
    busy: Mutex<usize>,
    freed: Condvar,
}

impl Gate {
    fn enter(&self) -> GatePass<'_> {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy >= AT_ONCE {
            busy = self.freed.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
        *busy += 1;
        GatePass(self)
    }
}

struct GatePass<'a>(&'a Gate);

impl Drop for GatePass<'_> {
    fn drop(&mut self) {
        *self.0.busy.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        self.0.freed.notify_one();
    }
}

pub(super) struct Loader {
    tx: mpsc::Sender<Sent>,
    rx: mpsc::Receiver<Sent>,
    gate: Arc<Gate>,
    proxy: EventLoopProxy<UserEvent>,
}

impl Loader {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            gate: Arc::default(),
            proxy,
        }
    }

    /// Creates the plugin on a worker thread; its [`Done`] comes back through [`Loader::take`]
    /// after a [`UserEvent::Loaded`].
    pub fn start(&self, job: Job) -> Result<()> {
        let (tx, gate, proxy) = (self.tx.clone(), self.gate.clone(), self.proxy.clone());
        std::thread::Builder::new()
            .name("plugin-load".into())
            .spawn(move || {
                let (id, generation) = (job.id, job.generation);
                let (result, elapsed) = {
                    let _pass = gate.enter();
                    let t = Instant::now();
                    // A panic in our own code (a SoundFont parser) fails the load, not the app.
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| create(job)))
                        .unwrap_or_else(|_| Err(anyhow!("the loader failed")));
                    (r, t.elapsed())
                };
                let _ = tx.send(Sent {
                    id,
                    generation,
                    result,
                    elapsed,
                });
                let _ = proxy.send_event(UserEvent::Loaded);
            })
            .context("cannot start a loader thread")?;
        Ok(())
    }

    /// The loads that finished since the last call.
    pub fn take(&self) -> Vec<Done> {
        self.rx
            .try_iter()
            .map(|s| Done {
                id: s.id,
                generation: s.generation,
                result: s.result.map(|(plugin, processor)| Ready {
                    plugin: plugin as Box<dyn PluginHandle>,
                    processor,
                }),
                elapsed: s.elapsed,
            })
            .collect()
    }
}

/// Throws a plugin away without making the main thread wait for its process to end (that can
/// take seconds for a Wine host). Returns the thread that waits for it, if there is one.
pub(super) fn discard(
    processor: Option<Box<dyn PluginProcessor>>,
    mut plugin: Box<dyn PluginHandle>,
) -> Option<JoinHandle<()>> {
    drop(processor);
    plugin.deactivate();
    close(plugin)
}

/// Drops a deactivated plugin; a process of its own is waited for (and killed if it hangs) on
/// another thread, which is returned.
pub(super) fn close(mut plugin: Box<dyn PluginHandle>) -> Option<JoinHandle<()>> {
    let rest = plugin.begin_close();
    drop(plugin);
    std::thread::Builder::new()
        .name("plugin-close".into())
        .spawn(rest?)
        .ok()
}

/// Creates, restores and activates one plugin (worker thread).
#[allow(clippy::type_complexity)]
fn create(job: Job) -> Result<(Box<dyn PluginHandle + Send>, Box<dyn PluginProcessor>)> {
    use merlin_hosting::soundfont::{Font, SoundFontPlugin, files_of, is_source};
    let cancelled = job.cancelled.clone();
    let check = || match cancelled.load(Ordering::Relaxed) {
        true => Err(anyhow!("cancelled")),
        false => Ok(()),
    };
    check()?;
    let mut plugin: Box<dyn PluginHandle + Send> = if is_source(&job.path) {
        // A folder's fonts decode side by side; the plugin then finds them decoded. They stay
        // alive until the plugin holds its own copies.
        let files = files_of(&job.path);
        let _fonts: Vec<_> = std::thread::scope(|scope| {
            let jobs: Vec<_> = files
                .iter()
                .map(|f| scope.spawn(move || Font::load(f)))
                .collect();
            jobs.into_iter().filter_map(|j| j.join().ok()).collect()
        });
        Box::new(SoundFontPlugin::open(&job.path)?)
    } else {
        // Shorter than the watchdog's limits, so a stuck load ends here first.
        let limit = Duration::from_secs(if job.windows { 100 } else { 30 });
        Box::new(
            RemotePlugin::spawn(
                &job.host,
                &job.path,
                &job.plugin_id,
                job.waker,
                job.sample_rate,
                job.max_block,
                limit,
                &job.env,
            )
            .map_err(|e| match job.host.exists() {
                true => e,
                false => e.context(format!(
                    "{} is missing (it belongs next to the app; `cargo build -p merlin` makes it)",
                    job.host.display()
                )),
            })?,
        )
    };
    check()?;
    if let Some(state) = &job.state {
        plugin
            .load_state(state)
            .context("restoring the saved state")?;
    }
    check()?;
    let processor = plugin.activate(job.sample_rate, job.max_block)?;
    Ok((plugin, processor))
}
