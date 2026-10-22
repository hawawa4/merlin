//! Everything that is not drawing: the engine and its JACK backend, the loaded plugins, the
//! rig and its persistence, the editor dock. The UI asks it to do things and reads its state.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use merlin_audio_io::{InputRole, JackBackend, JackClient, MidiSource, friendly_name};
use merlin_embed::{EditorManager, PluginLookup};
use merlin_engine::{Command, EngineHandle, Filter};
use merlin_hosting::clap::ClapLibrary;
use merlin_hosting::vst3::Vst3Library;
use merlin_hosting::{EditorSize, HostEvent, MidiEvent, PluginHandle, Waker};
use merlin_model::{
    Format, FxSettings, Insert, MAX_INSERTS, MAX_SLOTS, PluginRef, Rig, SelectMode, Slot,
};
use merlin_scanner::{Cache, Entry, Found, Quarantine};
use std::sync::mpsc;

use crate::scan_worker::{self, ScanMsg};
use winit::event_loop::EventLoopProxy;

use crate::guard::{self, Guard, HostInfo};
use crate::settings::Settings;

/// Slots shown in the rack.
pub const UI_SLOTS: usize = 8;
const MAX_BLOCK: usize = 4096;

/// What plugins and background threads send to the main loop.
#[derive(Debug)]
pub enum UserEvent {
    Host(usize, HostEvent),
    /// The scan worker has news.
    Scan,
    /// A SoundFont finished decoding on a worker thread.
    Font,
    /// The Windows plugin installer or sync has news.
    Windows,
}

/// Progress of the Windows plugin job (installer, sync) running on a worker thread.
enum WindowsMsg {
    Status(String),
    Done(Result<String, String>),
}

/// A MIDI device as the settings list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiDeviceRow {
    /// The JACK port name.
    pub port: String,
    /// What a person calls it; what the settings remember it by.
    pub name: String,
    /// The engine input it is connected to, if it is.
    pub input: Option<u8>,
    pub role: crate::settings::DeviceRole,
    pub enabled: bool,
    /// Sent something in the last moment.
    pub active: bool,
}

/// What to do with each MIDI source: the device's saved role, a keyboard if nothing is saved, or
/// nothing if it was switched off.
fn midi_policy(
    prefs: &[crate::settings::MidiDevicePref],
) -> impl Fn(&str) -> Option<InputRole> + use<> {
    let prefs = prefs.to_vec();
    move |port| {
        let name = friendly_name(port);
        match prefs.iter().find(|p| p.name == name) {
            Some(p) if !p.enabled => None,
            Some(p) => Some(p.role.into()),
            None => Some(InputRole::Keyboard),
        }
    }
}

/// What a slot costs. The audio part is the share of the block's time budget the slot uses in the
/// engine; the Wine part is the CPU of its yabridge host process (the plugin's own threads and
/// its editor), as a percentage of one core.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct SlotCpu {
    /// Smoothed share of the block budget (0.05 = 5%).
    pub audio: f32,
    /// The worst recent block (decays over a few seconds).
    pub audio_worst: f32,
    /// `None` for a plugin that runs in our own process.
    pub wine: Option<f32>,
}

/// A decoded SoundFont (or why not) for the slot that asked for it.
struct FontMsg {
    slot: usize,
    found: Found,
    font: Result<Arc<merlin_hosting::soundfont::Font>>,
}

struct SlotWaker {
    proxy: Mutex<EventLoopProxy<UserEvent>>,
    slot: usize,
}

impl Waker for SlotWaker {
    fn wake(&self, event: HostEvent) {
        let _ = self
            .proxy
            .lock()
            .unwrap()
            .send_event(UserEvent::Host(self.slot, event));
    }
}

pub struct Loaded {
    pub plugin: Box<dyn PluginHandle>,
}

/// The editor id of an insert: instruments use their slot number, inserts the numbers after.
pub fn insert_id(slot: usize, index: usize) -> usize {
    UI_SLOTS + slot * MAX_INSERTS + index
}

/// Lets the editor manager borrow the plugins of a rack by editor id.
pub struct Lookup<'a> {
    plugins: &'a mut [Option<Loaded>],
    inserts: &'a mut [[Option<Loaded>; MAX_INSERTS]],
}

pub fn lookup<'a>(
    plugins: &'a mut [Option<Loaded>],
    inserts: &'a mut [[Option<Loaded>; MAX_INSERTS]],
) -> Lookup<'a> {
    Lookup { plugins, inserts }
}

impl PluginLookup for Lookup<'_> {
    fn plugin(&mut self, id: usize) -> Option<&mut dyn PluginHandle> {
        let loaded = if id < UI_SLOTS {
            self.plugins.get_mut(id)?.as_mut()
        } else {
            let k = id - UI_SLOTS;
            self.inserts
                .get_mut(k / MAX_INSERTS)?
                .get_mut(k % MAX_INSERTS)?
                .as_mut()
        };
        loaded.map(|l| &mut *l.plugin as &mut dyn PluginHandle)
    }
}

pub struct Session {
    pub rig: Rig,
    pub rig_path: Option<PathBuf>,
    /// The rig changed since it was last saved.
    pub dirty: bool,
    pub plugins: Vec<Option<Loaded>>,
    /// Insert effects, per slot.
    pub inserts: Vec<[Option<Loaded>; MAX_INSERTS]>,
    /// Why a slot's saved plugin could not be loaded; the slot keeps its saved plugin and
    /// state so nothing is lost.
    pub missing: Vec<Option<String>>,
    /// Slots that are keyboard targets, in selection order.
    pub targets: Vec<usize>,
    /// The slot whose editor is on show (or would be, if it has none or it is not allowed).
    pub viewing: Option<usize>,
    /// The editor id on show (the slot's instrument, or one of its inserts).
    pub view_id: Option<usize>,
    /// What to tell the user instead of, or beside, a slot's editor (see `quirks`).
    pub notes: Vec<Option<String>>,
    pub found: Vec<Found>,
    /// Effect plugins, for the inserts.
    pub effects: Vec<Found>,
    /// Every plugin file the scanner knows, including the ones that failed to load.
    pub entries: Vec<Entry>,
    /// Where plugins are looked for (standard folders, settings, command line).
    pub plugin_dirs: Vec<PathBuf>,
    cli_dirs: Vec<PathBuf>,
    pub settings: Settings,
    scan_rx: Option<mpsc::Receiver<ScanMsg>>,
    /// MIDI activity: the message count last seen per input, and until when its light stays on.
    midi_seen: [u32; merlin_engine::MAX_INPUTS],
    midi_lit: [Option<Instant>; merlin_engine::MAX_INPUTS],
    /// What each slot costs, refreshed by [`Session::update_loads`].
    pub slot_cpu: [SlotCpu; UI_SLOTS],
    /// Wine host CPU readings: pid -> (ticks, when), for the delta.
    host_cpu: std::collections::HashMap<u32, (u64, Instant)>,
    last_load_update: Instant,
    /// A scan was asked for while one was running: run it next, forgetting these files.
    scan_again: Option<Vec<PathBuf>>,
    windows_rx: Option<mpsc::Receiver<WindowsMsg>>,
    /// The running job is the runtime download: restart into it when it succeeds.
    restart_after_job: bool,
    /// Set by the runtime job when what it installed is not what this process runs with.
    runtime_changed: Arc<std::sync::atomic::AtomicBool>,
    /// Set when the app should restart itself (main does it on its next tick).
    pub restart_requested: bool,
    /// What the Windows job is doing, or how the last one ended (for the Settings tab).
    pub windows_status: String,
    font_tx: mpsc::Sender<FontMsg>,
    font_rx: mpsc::Receiver<FontMsg>,
    /// Per slot: the SoundFont being decoded for it (path and name).
    pub loading: Vec<Option<(PathBuf, String)>>,
    /// The file the scanner is probing right now.
    pub scan_current: Option<PathBuf>,
    pub quarantine: Quarantine,
    guard: Arc<Guard>,
    pub engine: EngineHandle,
    backend: Option<JackBackend>,
    pub midi_sources: Vec<MidiSource>,
    pub editors: EditorManager,
    pub log: Vec<String>,
    proxy: EventLoopProxy<UserEvent>,
    pub sample_rate: u32,
    last_autosave: Instant,
    /// A change of the rig's structure wants an autosave soon (not at once: a plugin that has
    /// only just loaded should be left alone for a moment before it is asked for its state).
    autosave_after: Option<Instant>,
}

impl Session {
    pub fn new(
        xid: u32,
        scale: f64,
        proxy: EventLoopProxy<UserEvent>,
        settings: &Settings,
        cli_dirs: Vec<PathBuf>,
        guard: Arc<Guard>,
    ) -> Result<Self> {
        let client = JackClient::open("merlin", Some(settings.frames))?;
        let sample_rate = client.sample_rate;
        let (engine, handle) = merlin_engine::new(sample_rate as f32, MAX_BLOCK);
        let backend = client.start(engine)?;
        let outputs = backend.connect_outputs();
        let midi_sources = backend.connect_midi_sources(&midi_policy(&settings.midi_devices));
        let (font_tx, font_rx) = mpsc::channel();
        let mut s = Self {
            rig: new_rig("Untitled"),
            rig_path: None,
            dirty: false,
            plugins: (0..UI_SLOTS).map(|_| None).collect(),
            inserts: (0..UI_SLOTS)
                .map(|_| std::array::from_fn(|_| None))
                .collect(),
            missing: vec![None; UI_SLOTS],
            targets: Vec::new(),
            viewing: None,
            view_id: None,
            notes: vec![None; UI_SLOTS],
            found: Vec::new(),
            effects: Vec::new(),
            entries: Vec::new(),
            plugin_dirs: Vec::new(),
            cli_dirs,
            settings: settings.clone(),
            scan_rx: None,
            midi_seen: Default::default(),
            midi_lit: Default::default(),
            slot_cpu: [SlotCpu::default(); UI_SLOTS],
            host_cpu: Default::default(),
            last_load_update: Instant::now(),
            scan_again: None,
            windows_rx: None,
            restart_after_job: false,
            runtime_changed: Arc::default(),
            restart_requested: false,
            windows_status: String::new(),
            font_tx,
            font_rx,
            loading: vec![None; UI_SLOTS],
            scan_current: None,
            quarantine: Quarantine::load(&crate::paths::quarantine_file()),
            guard,
            engine: handle,
            backend: Some(backend),
            midi_sources,
            editors: EditorManager::new(xid, scale, settings.editor_cache)?,
            log: Vec::new(),
            proxy,
            sample_rate,
            last_autosave: Instant::now(),
            autosave_after: None,
        };
        s.rebuild_dirs();
        // Start from what the cache knows, then let the background scan catch up.
        s.entries = merlin_scanner::cached_entries(
            &s.plugin_dirs,
            &Cache::load(&crate::paths::plugin_cache_file()),
        );
        s.refresh_found();
        s.start_scan(Vec::new());
        // Windows plugins installed in a known prefix are bridged without being asked.
        if s.windows_runtime().is_some() {
            let _ = s.sync_windows_plugins();
        }
        s.say(format!(
            "JACK at {sample_rate} Hz, outputs {outputs:?}, MIDI {:?}",
            s.midi_sources
                .iter()
                .map(|m| m.device.as_str())
                .collect::<Vec<_>>()
        ));
        Ok(s)
    }

    /// The folders to scan: the standard ones, the ones in the settings, and the command line's.
    fn rebuild_dirs(&mut self) {
        let mut dirs = merlin_scanner::default_plugin_dirs();
        dirs.extend(merlin_scanner::default_sound_dirs());
        dirs.extend(self.settings.plugin_dirs.iter().cloned());
        dirs.extend(self.settings.sound_dirs.iter().cloned());
        dirs.extend(self.cli_dirs.iter().cloned());
        // The bridges we make for the Windows plugins installed in our prefix.
        dirs.push(crate::paths::bridges_dir());
        dirs.push(crate::paths::bridges_vst3_dir());
        dirs.dedup();
        self.plugin_dirs = dirs;
    }

    /// The Wine, yabridge and prefix to use for Windows plugins, if Wine and yabridge are set up.
    pub fn windows_runtime(&self) -> Option<merlin_wine::Runtime> {
        // The settings win; otherwise what the environment already says (`just dev` sets
        // WINELOADER and puts yabridge on the PATH).
        let managed = merlin_wine::runtime::installed(&crate::paths::runtime_dir());
        let wine = self
            .settings
            .wine_loader
            .clone()
            .or_else(|| std::env::var_os("WINELOADER").map(PathBuf::from))
            .or_else(|| managed.as_ref().map(|m| m.wine.clone()))?;
        let yabridge_dir = self
            .settings
            .yabridge_dir
            .clone()
            .or_else(|| {
                let path = std::env::var_os("PATH")?;
                std::env::split_paths(&path)
                    .find(|d| d.join("libyabridge-chainloader-clap.so").is_file())
            })
            .or_else(|| managed.as_ref().map(|m| m.yabridge_dir.clone()))?;
        Some(merlin_wine::Runtime {
            wine,
            yabridge_dir,
            prefix: self
                .settings
                .wine_prefix
                .clone()
                .unwrap_or_else(crate::paths::wine_prefix),
        })
    }

    /// The Wine prefixes to look for Windows plugins in: ours first, then the one the
    /// environment names (`WINEPREFIX`) and `~/.wine`, when they exist.
    pub fn windows_prefixes(&self) -> Vec<PathBuf> {
        let mut v = vec![
            self.settings
                .wine_prefix
                .clone()
                .unwrap_or_else(crate::paths::wine_prefix),
        ];
        let others = [
            std::env::var_os("WINEPREFIX").map(PathBuf::from),
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".wine")),
        ];
        for p in others.into_iter().flatten() {
            if p.join("drive_c").is_dir() && !v.contains(&p) {
                v.push(p);
            }
        }
        v
    }

    pub fn windows_busy(&self) -> bool {
        self.windows_rx.is_some()
    }

    /// Runs `job` on a worker thread; its messages come back as [`UserEvent::Windows`].
    fn start_windows_job(
        &mut self,
        job: impl FnOnce(&mpsc::Sender<WindowsMsg>) -> Result<String> + Send + 'static,
    ) -> Result<()> {
        if self.windows_busy() {
            return Err(anyhow!("a Windows plugin job is already running"));
        }
        let (tx, rx) = mpsc::channel();
        let proxy = self.proxy.clone();
        std::thread::Builder::new()
            .name("windows-job".into())
            .spawn(move || {
                let done = job(&tx).map_err(|e| format!("{e:#}"));
                let _ = tx.send(WindowsMsg::Done(done));
                let _ = proxy.send_event(UserEvent::Windows);
            })
            .context("cannot start the Windows plugin job")?;
        self.windows_rx = Some(rx);
        Ok(())
    }

    /// The downloaded runtime, if there is one.
    pub fn installed_runtime(&self) -> Option<merlin_wine::runtime::Installed> {
        merlin_wine::runtime::installed(&crate::paths::runtime_dir())
    }

    /// Downloads the Wine + yabridge runtime on a worker thread, then restarts the app into it.
    pub fn install_runtime(&mut self) -> Result<()> {
        use merlin_wine::runtime::{self, Progress};
        let url = self
            .settings
            .runtime_url
            .clone()
            .unwrap_or_else(|| crate::settings::DEFAULT_RUNTIME_URL.to_owned());
        self.windows_status = "contacting the download server...".into();
        self.restart_after_job = true;
        let changed = self.runtime_changed.clone();
        let started = self.start_windows_job(move |tx| {
            let last = std::cell::Cell::new(String::new());
            let say = |text: String| {
                if last.replace(text.clone()) != text {
                    let _ = tx.send(WindowsMsg::Status(text));
                }
            };
            let mb = |b: u64| b / (1 << 20);
            let installed = runtime::install(&url, &crate::paths::runtime_dir(), &|p| match p {
                Progress::Manifest => say("contacting the download server...".into()),
                Progress::Downloading { done, total } => {
                    say(format!("downloading: {} of {} MB", mb(done), mb(total)))
                }
                Progress::Verifying => say("checking the download...".into()),
                Progress::Unpacking => say("unpacking...".into()),
            })?;
            // Wine is chosen at startup: a restart is needed unless this process already runs
            // with exactly this runtime.
            let in_use = std::env::var_os("WINELOADER").is_some_and(|w| w == installed.wine);
            changed.store(!in_use, Ordering::Relaxed);
            Ok(if in_use {
                format!(
                    "Windows plugin support {} is installed and in use",
                    installed.id
                )
            } else {
                format!(
                    "Windows plugin support {} installed; restarting",
                    installed.id
                )
            })
        });
        if started.is_err() {
            self.restart_after_job = false;
        }
        started
    }

    /// Makes the bridges for the Windows plugins in the prefix, then scans them.
    pub fn sync_windows_plugins(&mut self) -> Result<()> {
        let rt = self
            .windows_runtime()
            .context("set the Wine program and the yabridge folder first")?;
        self.windows_status = "looking for Windows plugins...".into();
        let prefixes = self.windows_prefixes();
        self.start_windows_job(move |_| sync_report(&rt, &prefixes))
    }

    /// Runs a plugin installer (`.exe` or `.msi`) in the Windows prefix, then syncs. The
    /// installer's own window opens as usual; this returns at once.
    pub fn install_windows_plugin(&mut self, file: PathBuf) -> Result<()> {
        let rt = self
            .windows_runtime()
            .context("set the Wine program and the yabridge folder first")?;
        let mut installer = rt.installer_command(&file)?;
        let name = file
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.windows_status = format!("preparing to install {name}...");
        let prefixes = self.windows_prefixes();
        self.start_windows_job(move |tx| {
            if !rt.prefix_ready() {
                let _ = tx.send(WindowsMsg::Status(
                    "creating the Windows environment (first time only, about a minute)...".into(),
                ));
                rt.ensure_prefix(Duration::from_secs(300))?;
            }
            let _ = tx.send(WindowsMsg::Status(format!(
                "running {name}: finish its installer window, then it is picked up here"
            )));
            let status = installer
                .spawn()
                .context("cannot start the installer")?
                .wait()?;
            if !status.success() {
                // Many installers return odd codes; what matters is whether plugins appeared.
                let _ = tx.send(WindowsMsg::Status(format!(
                    "{name} exited with {status}, looking for plugins anyway..."
                )));
            }
            sync_report(&rt, &prefixes)
        })
    }

    /// Takes in what the Windows job reported.
    pub fn poll_windows(&mut self) {
        let Some(rx) = self.windows_rx.as_ref() else {
            return;
        };
        let mut finished = None;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WindowsMsg::Status(t) => self.windows_status = t,
                WindowsMsg::Done(r) => finished = Some(r),
            }
        }
        let Some(result) = finished else { return };
        self.windows_rx = None;
        let restart = std::mem::take(&mut self.restart_after_job)
            && self.runtime_changed.load(Ordering::Relaxed);
        match result {
            Ok(text) if restart => {
                self.windows_status = text.clone();
                self.say(text);
                self.restart_requested = true;
            }
            Ok(text) => {
                self.windows_status = text.clone();
                self.say(format!("Windows plugins: {text}"));
                // New bridges are plugin files like any other.
                self.start_scan(Vec::new());
            }
            Err(e) => {
                self.windows_status = format!("failed: {e}");
                self.say(format!("Windows plugins failed: {e}"));
            }
        }
    }

    pub fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&crate::paths::settings_file()) {
            self.say(format!("could not save the settings: {e:#}"));
        }
    }

    pub fn add_plugin_dir(&mut self, dir: PathBuf) {
        if !dir.as_os_str().is_empty() && !self.settings.plugin_dirs.contains(&dir) {
            self.settings.plugin_dirs.push(dir);
            self.dirs_changed(true);
        }
    }

    pub fn remove_plugin_dir(&mut self, index: usize) {
        if index < self.settings.plugin_dirs.len() {
            self.settings.plugin_dirs.remove(index);
            self.dirs_changed(false);
        }
    }

    pub fn add_sound_dir(&mut self, dir: PathBuf) {
        if !dir.as_os_str().is_empty() && !self.settings.sound_dirs.contains(&dir) {
            self.settings.sound_dirs.push(dir);
            self.dirs_changed(true);
        }
    }

    pub fn remove_sound_dir(&mut self, index: usize) {
        if index < self.settings.sound_dirs.len() {
            self.settings.sound_dirs.remove(index);
            self.dirs_changed(false);
        }
    }

    /// The folder lists changed: save, and look again (new folder) or forget what was in the
    /// folders that are no longer scanned.
    fn dirs_changed(&mut self, added: bool) {
        self.save_settings();
        self.rebuild_dirs();
        if added {
            self.start_scan(Vec::new());
        } else {
            let dirs = self.plugin_dirs.clone();
            self.entries
                .retain(|e| dirs.iter().any(|d| e.path.starts_with(d)));
            self.refresh_found();
        }
    }

    /// Probes the plugin files again: only new and changed ones, or all of them.
    pub fn rescan(&mut self, everything: bool) {
        let forget = if everything {
            self.entries.iter().map(|e| e.path.clone()).collect()
        } else {
            Vec::new()
        };
        self.start_scan(forget);
    }

    /// Probes one file again (the "retry" for a file that failed to scan).
    pub fn retry_file(&mut self, path: PathBuf) {
        self.start_scan(vec![path]);
    }

    pub fn remove_from_quarantine(&mut self, path: &Path, id: &str) {
        if self.quarantine.remove(path, id) {
            let _ = self.quarantine.save(&crate::paths::quarantine_file());
        }
    }

    /// A stable key for a browser entry, for favourites.
    pub fn favorite_key(f: &Found) -> String {
        format!(
            "{}:{}{}",
            match f.info.format {
                Format::Vst3 => "vst3",
                _ => "clap",
            },
            f.info.id,
            if f.windows { ":win" } else { "" }
        )
    }

    pub fn is_favorite(&self, f: &Found) -> bool {
        self.settings.favorites.contains(&Self::favorite_key(f))
    }

    pub fn toggle_favorite(&mut self, f: &Found) {
        let key = Self::favorite_key(f);
        match self.settings.favorites.iter().position(|k| *k == key) {
            Some(p) => {
                self.settings.favorites.remove(p);
            }
            None => self.settings.favorites.push(key),
        }
        self.save_settings();
    }

    /// Loads browser entry `k` into the slot being viewed if that is empty, else the first
    /// empty slot.
    pub fn load_into_free_slot(&mut self, k: usize) -> Result<()> {
        let found = self.found.get(k).cloned().context("no such plugin")?;
        let free = self
            .viewing
            .filter(|&i| self.rig.slots[i].instrument.is_none())
            .or_else(|| (0..UI_SLOTS).find(|&i| self.rig.slots[i].instrument.is_none()))
            .context("all slots are in use: drop it on one to replace it")?;
        self.load(free, &found)
    }

    /// Loads a SoundFont file (dropped on the window) into the viewed slot if empty, else the
    /// first empty slot. It does not need to be in a scanned folder.
    pub fn load_soundfont_file(&mut self, path: &Path) -> Result<()> {
        if !merlin_hosting::soundfont::is_soundfont(path) {
            return Err(anyhow!("{} is not a SoundFont", path.display()));
        }
        let found = Found {
            path: path.to_owned(),
            windows: false,
            info: merlin_hosting::soundfont::info_for(path),
        };
        let free = self
            .viewing
            .filter(|&i| self.rig.slots[i].instrument.is_none() && self.loading[i].is_none())
            .or_else(|| {
                (0..UI_SLOTS)
                    .find(|&i| self.rig.slots[i].instrument.is_none() && self.loading[i].is_none())
            })
            .context("all slots are in use: remove one first")?;
        self.load(free, &found)
    }

    pub fn set_buffer_size(&mut self, frames: u32) -> Result<()> {
        let backend = self.backend.as_ref().context("audio is not running")?;
        let now = backend.set_buffer_size(frames)?;
        self.settings.frames = frames;
        self.save_settings();
        self.say(format!("buffer size {now} frames"));
        Ok(())
    }

    /// Looks for MIDI devices that appeared since the last time and connects them, as the
    /// per-device settings say.
    pub fn refresh_midi(&mut self) {
        let policy = midi_policy(&self.settings.midi_devices);
        if let Some(b) = self.backend.as_ref() {
            self.midi_sources = b.connect_midi_sources(&policy);
        }
    }

    /// Every MIDI device the system offers, connected or not, for the settings list.
    pub fn midi_devices(&self) -> Vec<MidiDeviceRow> {
        let Some(b) = self.backend.as_ref() else {
            return Vec::new();
        };
        let now = Instant::now();
        b.hardware_midi_sources()
            .into_iter()
            .map(|port| {
                let name = friendly_name(&port);
                let pref = self.midi_pref(&name);
                let connected = self.midi_sources.iter().find(|m| m.device == port);
                MidiDeviceRow {
                    active: connected
                        .and_then(|m| self.midi_lit.get(m.input as usize)?.as_ref())
                        .is_some_and(|t| *t > now),
                    input: connected.map(|m| m.input),
                    role: pref.role,
                    enabled: pref.enabled,
                    name,
                    port,
                }
            })
            .collect()
    }

    fn midi_pref(&self, name: &str) -> crate::settings::MidiDevicePref {
        self.settings
            .midi_devices
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .unwrap_or(crate::settings::MidiDevicePref {
                name: name.to_owned(),
                role: Default::default(),
                enabled: true,
            })
    }

    /// Remembers what a device is for (or that it is off) and applies it now.
    pub fn set_midi_device(
        &mut self,
        name: &str,
        role: crate::settings::DeviceRole,
        enabled: bool,
    ) {
        let pref = crate::settings::MidiDevicePref {
            name: name.to_owned(),
            role,
            enabled,
        };
        match self
            .settings
            .midi_devices
            .iter_mut()
            .find(|p| p.name == name)
        {
            Some(p) => *p = pref,
            None => self.settings.midi_devices.push(pref),
        }
        self.save_settings();
        self.refresh_midi();
        self.say(format!(
            "MIDI device {name}: {}",
            if !enabled {
                "off"
            } else if role == crate::settings::DeviceRole::Controller {
                "controller"
            } else {
                "keyboard"
            }
        ));
    }

    /// Is any MIDI input showing activity right now?
    pub fn midi_active(&self) -> bool {
        let now = Instant::now();
        self.midi_lit.iter().flatten().any(|t| *t > now)
    }

    /// Notes which inputs received MIDI since the last call; their lights stay on briefly.
    fn update_midi_activity(&mut self) {
        let Some(b) = self.backend.as_ref() else {
            return;
        };
        let now = Instant::now();
        for (i, seen) in self.midi_seen.iter_mut().enumerate() {
            let count = b.stats.midi_activity[i].load(Ordering::Relaxed);
            if count != *seen {
                *seen = count;
                self.midi_lit[i] = Some(now + Duration::from_millis(150));
            }
        }
    }

    /// Hotplug: when ports came or went, reconnects MIDI devices and says what changed.
    pub fn poll_midi_hotplug(&mut self) {
        if !self
            .backend
            .as_ref()
            .is_some_and(|b| b.take_ports_changed())
        {
            return;
        }
        let before: Vec<String> = self.midi_sources.iter().map(|m| m.device.clone()).collect();
        self.refresh_midi();
        let after: Vec<String> = self.midi_sources.iter().map(|m| m.device.clone()).collect();
        for d in after.iter().filter(|d| !before.contains(d)) {
            self.say(format!("MIDI device connected: {}", friendly_name(d)));
        }
        for d in before.iter().filter(|d| !after.contains(d)) {
            self.say(format!("MIDI device gone: {}", friendly_name(d)));
        }
    }

    fn refresh_found(&mut self) {
        self.found = merlin_scanner::with_feature(&self.entries, "instrument");
        self.effects = merlin_scanner::with_feature(&self.entries, "audio-effect");
    }

    pub fn scanning(&self) -> bool {
        self.scan_rx.is_some()
    }

    /// Probes new and changed plugin files in the background; `forget` files are probed again
    /// even if the cache has them ("retry").
    pub fn start_scan(&mut self, forget: Vec<PathBuf>) {
        if self.scan_rx.is_some() {
            // One scan at a time; ask for another when this one is done (new files may have
            // appeared since it listed the folders).
            self.scan_again.get_or_insert_with(Vec::new).extend(forget);
            return;
        }
        self.scan_rx = Some(scan_worker::spawn(
            self.plugin_dirs.clone(),
            crate::paths::plugin_cache_file(),
            forget,
            self.proxy.clone(),
        ));
    }

    /// Takes in what the scan worker has reported.
    pub fn poll_scan(&mut self) {
        let Some(rx) = self.scan_rx.as_ref() else {
            return;
        };
        let mut finished = false;
        let mut changed = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ScanMsg::Started(p) => self.scan_current = Some(p),
                ScanMsg::Entry(e) => {
                    match self.entries.iter_mut().find(|x| x.path == e.path) {
                        Some(slot) => *slot = e,
                        None => self.entries.push(e),
                    }
                    changed = true;
                }
                ScanMsg::Finished => finished = true,
            }
        }
        if changed {
            self.refresh_found();
        }
        if finished {
            self.scan_rx = None;
            self.scan_current = None;
            if let Some(forget) = self.scan_again.take() {
                self.start_scan(forget);
            }
            let failed = self.entries.iter().filter(|e| e.error.is_some()).count();
            self.say(format!(
                "scan finished: {} plugins{}",
                self.found.len(),
                if failed > 0 {
                    format!(", {failed} files failed")
                } else {
                    String::new()
                }
            ));
        }
    }

    /// The loaded plugin behind an editor id.
    fn plugin_by_id(&mut self, id: usize) -> Option<&mut Loaded> {
        if id < UI_SLOTS {
            self.plugins.get_mut(id)?.as_mut()
        } else {
            let k = id - UI_SLOTS;
            self.inserts
                .get_mut(k / MAX_INSERTS)?
                .get_mut(k % MAX_INSERTS)?
                .as_mut()
        }
    }

    pub fn say(&mut self, line: impl Into<String>) {
        let line = line.into();
        println!("{line}");
        self.log.push(line);
        if self.log.len() > 200 {
            self.log.remove(0);
        }
    }

    pub fn backend(&self) -> Option<&JackBackend> {
        self.backend.as_ref()
    }

    fn input_index(&self, name: &str) -> Option<u8> {
        self.midi_sources
            .iter()
            .find(|s| s.device == name)
            .map(|s| s.input)
    }

    // ---- slots ---------------------------------------------------------------------

    /// Sends a slot's filter and mix to the engine.
    fn push_slot_settings(&mut self, i: usize) {
        let s = &self.rig.slots[i];
        let filter = Filter::from_model(&s.filter, |n| self.input_index(n));
        let (volume, pan, mute) = (s.volume, s.pan, s.mute);
        let settings = s.fx;
        let _ = self.engine.send(Command::SetFilter { slot: i, filter });
        let _ = self.engine.send(Command::SetFx { slot: i, settings });
        let _ = self.engine.send(Command::SetMix {
            slot: i,
            volume,
            pan,
            mute,
        });
    }

    fn create_plugin(&mut self, i: usize, path: &Path, id: &str, windows: bool) -> Result<Loaded> {
        let name = self
            .found
            .iter()
            .chain(&self.effects)
            .find(|f| f.info.id == id)
            .map(|f| f.info.name.clone())
            .unwrap_or_else(|| id.to_owned());
        self.guard.register_plugin(HostInfo {
            slot: i,
            path: path.to_owned(),
            id: id.to_owned(),
            name,
        });
        // A bridged plugin starts a Wine host process: remember which one is ours, so the
        // watchdog can tell a crash from a normal unload.
        let before = if windows {
            guard::host_children()
        } else {
            Default::default()
        };
        // A cold Wine start is slow; a native plugin that takes this long is stuck.
        let limit = Duration::from_secs(if windows { 120 } else { 40 });
        let guard = self.guard.clone();
        let loaded = {
            let _call = guard.call(i, "loading", limit);
            self.create_plugin_inner(i, path, id)?
        };
        if windows {
            guard::install_socket_hook();
            let name = self
                .found
                .iter()
                .find(|f| f.info.id == id)
                .map(|f| f.info.name.clone())
                .unwrap_or_else(|| id.to_owned());
            for pid in guard::host_children().difference(&before) {
                self.guard.register(
                    *pid,
                    HostInfo {
                        slot: i,
                        path: path.to_owned(),
                        id: id.to_owned(),
                        name: name.clone(),
                    },
                );
            }
        }
        Ok(loaded)
    }

    fn create_plugin_inner(&mut self, i: usize, path: &Path, id: &str) -> Result<Loaded> {
        let waker = Arc::new(SlotWaker {
            proxy: Mutex::new(self.proxy.clone()),
            slot: i,
        });
        let plugin: Box<dyn PluginHandle> = if merlin_hosting::soundfont::is_soundfont(path)
        {
            Box::new(merlin_hosting::soundfont::SoundFontPlugin::open(
                path,
            )?)
        } else if path.extension().is_some_and(|e| e == "vst3") {
            Box::new(Vst3Library::load(path)?.create(id, waker)?)
        } else {
            Box::new(ClapLibrary::load(path)?.create(id, waker)?)
        };
        Ok(Loaded { plugin })
    }

    /// Installs a created plugin in a slot: activate and hand its processor to the engine.
    fn install(&mut self, i: usize, mut loaded: Loaded) -> Result<()> {
        let guard = self.guard.clone();
        let processor = {
            let _call = guard.call(i, "starting up", Duration::from_secs(30));
            loaded
                .plugin
                .activate(self.sample_rate as f64, MAX_BLOCK as u32)?
        };
        self.plugins[i] = Some(loaded);
        self.push_slot_settings(i);
        self.engine
            .send(Command::SetProcessor { slot: i, processor })
            .map_err(|_| anyhow!("engine command queue full"))?;
        Ok(())
    }

    /// Loads a plugin chosen in the browser into a slot (replacing what is there). A SoundFont
    /// that has to be decoded first (seconds for an SF3) is decoded on a worker thread, and the
    /// slot says "loading" until it is ready.
    pub fn load(&mut self, i: usize, found: &Found) -> Result<()> {
        use merlin_hosting::soundfont::{Font, is_soundfont};
        self.loading[i] = None;
        if is_soundfont(&found.path) && !Font::is_cached(&found.path) {
            self.loading[i] = Some((found.path.clone(), found.info.name.clone()));
            let (tx, owned, proxy) = (self.font_tx.clone(), found.clone(), self.proxy.clone());
            std::thread::Builder::new()
                .name("soundfont-load".into())
                .spawn(move || {
                    let font = Font::load(&owned.path);
                    let _ = tx.send(FontMsg {
                        slot: i,
                        found: owned,
                        font,
                    });
                    let _ = proxy.send_event(UserEvent::Font);
                })
                .context("cannot start the SoundFont loader")?;
            self.say(format!("slot {}: decoding {}...", i + 1, found.info.name));
            return Ok(());
        }
        self.load_now(i, found)
    }

    /// Finishes the SoundFont loads that were waiting for a worker.
    pub fn poll_fonts(&mut self) {
        while let Ok(msg) = self.font_rx.try_recv() {
            let current = self.loading[msg.slot]
                .as_ref()
                .is_some_and(|(p, _)| *p == msg.found.path);
            if !current {
                continue; // the slot was given something else meanwhile
            }
            self.loading[msg.slot] = None;
            match msg.font {
                // `font` stays alive until the plugin holds its own copy.
                Ok(font) => {
                    if let Err(e) = self.load_now(msg.slot, &msg.found) {
                        self.say(format!("error: {e:#}"));
                    }
                    drop(font);
                }
                Err(e) => self.say(format!(
                    "slot {}: {}: {e:#}",
                    msg.slot + 1,
                    msg.found.info.name
                )),
            }
        }
    }

    fn load_now(&mut self, i: usize, found: &Found) -> Result<()> {
        if let Some(why) = crate::quirks::lookup(&found.info.id, found.windows).unsupported {
            return Err(anyhow!("{} is not supported: {why}", found.info.name));
        }
        // Loading it by hand is the "retry": it comes off the quarantine list.
        if self.quarantine.remove(&found.path, &found.info.id) {
            let _ = self.quarantine.save(&crate::paths::quarantine_file());
        }
        self.unload(i);
        let t = Instant::now();
        let loading = crate::marker::Loading::begin(&found.path, &found.info.id, &found.info.name);
        let loaded = self.create_plugin(i, &found.path, &found.info.id, found.windows)?;
        self.install(i, loaded)?;
        drop(loading);
        let slot = &mut self.rig.slots[i];
        slot.instrument = Some(PluginRef {
            format: found.info.format,
            id: found.info.id.clone(),
            name: found.info.name.clone(),
            path: Some(found.path.clone()),
            windows: found.windows,
        });
        slot.state = None;
        if slot.name.is_empty() {
            slot.name = found.info.name.clone();
        }
        self.missing[i] = None;
        self.dirty = true;
        self.say(format!(
            "slot {} <- {}{} in {:?}",
            i + 1,
            found.info.name,
            if found.windows { " (Windows)" } else { "" },
            t.elapsed()
        ));
        if self.targets.is_empty() {
            self.select(i, false);
        }
        // Structural change: save soon, so a crash restarts from this rig.
        self.autosave_soon();
        Ok(())
    }

    /// Takes the plugin out of a slot: editor first, then the engine lets go of the processor,
    /// then the plugin is deactivated and destroyed. The rig slot is cleared.
    pub fn unload(&mut self, i: usize) {
        self.loading[i] = None;
        if self.remove_runtime(i) {
            let slot = &mut self.rig.slots[i];
            slot.instrument = None;
            slot.state = None;
            slot.name.clear();
            self.dirty = true;
        } else if self.rig.slots[i].instrument.is_some() {
            // A missing plugin's placeholder: clearing it is the user's explicit choice.
            self.rig.slots[i] = Slot::default();
            self.dirty = true;
        }
        self.missing[i] = None;
        self.targets.retain(|&t| t != i);
        self.rig.selected = self.targets.clone();
        self.notes[i] = None;
        if self.viewing == Some(i) {
            self.viewing = None;
            self.view_id = None;
        }
    }

    /// Removes the running plugin of a slot without touching the rig. Returns whether there was
    /// one.
    fn remove_runtime(&mut self, i: usize) -> bool {
        if self.plugins[i].is_none() {
            return false;
        }
        self.guard.unregister_slot(i);
        self.guard.begin_close();
        let _ = self
            .editors
            .forget(i, &mut lookup(&mut self.plugins, &mut self.inserts));
        let _ = self.engine.send(Command::ClearProcessor { slot: i });
        self.wait_for_retired();
        if let Some(mut l) = self.plugins[i].take() {
            l.plugin.deactivate();
        }
        self.guard.end_close();
        true
    }

    // ---- selection and mixing ------------------------------------------------------

    /// Exclusive selection, or toggling a layer; shows the slot's editor.
    pub fn select(&mut self, i: usize, layer: bool) {
        if self.plugins[i].is_none() {
            return;
        }
        let mode = if layer {
            SelectMode::Layer
        } else {
            SelectMode::Exclusive
        };
        let _ = self.engine.send(Command::Select { slot: i, mode });
        if layer {
            if let Some(p) = self.targets.iter().position(|&t| t == i) {
                self.targets.remove(p);
            } else {
                self.targets.push(i);
            }
        } else {
            self.targets = vec![i];
        }
        self.rig.selected = self.targets.clone();
        self.dirty = true;
        self.show_editor(i);
    }

    pub fn show_editor(&mut self, i: usize) {
        self.viewing = Some(i);
        self.view_id = Some(i);
        if self.is_sampler(i) {
            // A SoundFont has a preset panel, not an editor window: clear the dock.
            self.notes[i] = None;
            let _ = self
                .editors
                .hide(&mut lookup(&mut self.plugins, &mut self.inserts));
            return;
        }
        // Known problems with this plugin's editor are shown beside it.
        self.notes[i] = self.rig.slots[i]
            .instrument
            .as_ref()
            .and_then(|r| crate::quirks::lookup(&r.id, r.windows).warning)
            .map(str::to_owned);
        let guard = self.guard.clone();
        let result = {
            let _call = guard.call(i, "opening its editor", Duration::from_secs(40));
            self.editors
                .show(i, &mut lookup(&mut self.plugins, &mut self.inserts))
        };
        match result {
            Ok(o) if o.created => self.say(format!("editor {} created in {:?}", i + 1, o.elapsed)),
            Ok(_) => {}
            Err(e) => self.say(format!("slot {}: no editor: {e:#}", i + 1)),
        }
    }

    /// Does this slot hold a sampler with a preset list (a SoundFont) instead of an editor?
    pub fn is_sampler(&self, i: usize) -> bool {
        self.plugins[i]
            .as_ref()
            .is_some_and(|l| l.plugin.presets().is_some())
    }

    pub fn presets(&self, i: usize) -> Option<&[merlin_hosting::PresetInfo]> {
        self.plugins[i].as_ref()?.plugin.presets()
    }

    pub fn preset(&self, i: usize) -> Option<(u16, u8)> {
        self.plugins[i].as_ref()?.plugin.preset()
    }

    pub fn set_preset(&mut self, i: usize, bank: u16, program: u8) {
        if let Some(l) = self.plugins[i].as_mut() {
            l.plugin.set_preset(bank, program);
            self.dirty = true;
        }
    }

    pub fn sampler_tone(&self, i: usize) -> Option<merlin_hosting::SamplerTone> {
        self.plugins[i].as_ref()?.plugin.tone()
    }

    pub fn set_sampler_tone(&mut self, i: usize, tone: merlin_hosting::SamplerTone) {
        if let Some(l) = self.plugins[i].as_mut() {
            l.plugin.set_tone(tone);
            self.dirty = true;
        }
    }

    pub fn sampler_gain(&self, i: usize) -> Option<f32> {
        self.plugins[i].as_ref()?.plugin.gain()
    }

    pub fn set_sampler_gain(&mut self, i: usize, gain: f32) {
        if let Some(l) = self.plugins[i].as_mut() {
            l.plugin.set_gain(gain);
            self.dirty = true;
        }
    }

    /// Shows an insert's editor in the dock.
    pub fn show_insert_editor(&mut self, slot: usize, index: usize) {
        let id = insert_id(slot, index);
        if self.inserts[slot][index].is_none() {
            return;
        }
        self.viewing = Some(slot);
        self.view_id = Some(id);
        match self
            .editors
            .show(id, &mut lookup(&mut self.plugins, &mut self.inserts))
        {
            Ok(o) if o.created => self.say(format!(
                "insert {}.{} editor created in {:?}",
                slot + 1,
                index + 1,
                o.elapsed
            )),
            Ok(_) => {}
            Err(e) => self.say(format!(
                "insert {}.{}: no editor: {e:#}",
                slot + 1,
                index + 1
            )),
        }
    }

    pub fn toggle_mute(&mut self, i: usize) {
        let s = &mut self.rig.slots[i];
        s.mute = !s.mute;
        self.dirty = true;
        self.push_slot_settings(i);
    }

    pub fn set_zone(&mut self, i: usize, lo: u8, hi: u8) {
        let f = &mut self.rig.slots[i].filter;
        (f.key_lo, f.key_hi) = (lo.min(hi), lo.max(hi));
        self.dirty = true;
        self.push_slot_settings(i);
    }

    pub fn set_volume(&mut self, i: usize, volume: f32) {
        self.rig.slots[i].volume = volume;
        self.dirty = true;
        self.push_slot_settings(i);
    }

    /// A slot's built-in effects.
    pub fn set_fx(&mut self, i: usize, mut fx: FxSettings) {
        fx.clamp();
        self.rig.slots[i].fx = fx;
        self.dirty = true;
        self.push_slot_settings(i);
    }

    pub fn set_master_volume(&mut self, volume: f32) {
        self.rig.master_volume = volume.clamp(0.0, 1.5);
        self.dirty = true;
        // Decode the rig's SoundFonts side by side (an SF3 takes seconds); the slots below
        // then find them ready. Held until the rig is up.
        let _fonts = self.decode_soundfonts();
        let _ = self
            .engine
            .send(Command::SetMasterVolume(self.rig.master_volume));
    }

    pub fn set_transpose(&mut self, i: usize, semitones: i8) {
        self.rig.slots[i].filter.transpose = semitones;
        self.dirty = true;
        self.push_slot_settings(i);
    }

    pub fn panic(&mut self) {
        let _ = self.engine.send(Command::Panic);
        self.say("panic");
    }

    /// A note from the on-screen keyboard.
    pub fn note(&mut self, key: u8, on: bool) {
        if let Some(b) = self.backend.as_mut() {
            let ev = if on {
                MidiEvent::note_on(0, 0, key, 100)
            } else {
                MidiEvent::note_off(0, 0, key)
            };
            let _ = b.virtual_midi.push(ev);
        }
    }

    // ---- host events and housekeeping ----------------------------------------------

    pub fn handle_event(&mut self, event: UserEvent) {
        let (i, e) = match event {
            UserEvent::Host(i, e) => (i, e),
            UserEvent::Scan => {
                self.poll_scan();
                return;
            }
            UserEvent::Font => {
                self.poll_fonts();
                return;
            }
            UserEvent::Windows => {
                self.poll_windows();
                return;
            }
        };
        match e {
            HostEvent::MainThreadCallback => {
                if let Some(l) = self.plugin_by_id(i) {
                    l.plugin.on_main_thread();
                }
            }
            HostEvent::FdReady => {
                if let Some(l) = self.plugin_by_id(i) {
                    l.plugin.on_fd_ready();
                }
            }
            HostEvent::EditorResize { width, height } => {
                let _ = self.editors.plugin_resized(
                    i,
                    EditorSize { width, height },
                    &mut lookup(&mut self.plugins, &mut self.inserts),
                );
            }
            HostEvent::EditorClosed => {
                self.say(format!("editor {}: plugin closed its editor", i + 1))
            }
            HostEvent::StateDirty => self.dirty = true,
        }
        self.service();
    }

    /// Plugin timers, editor eviction, retired processors. Returns when to run again.
    pub fn service(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let guard = self.guard.clone();
        let limit = Duration::from_secs(15);
        for (i, l) in self.plugins.iter_mut().enumerate() {
            if let Some(l) = l {
                let _call = guard.call(i, "running its timers", limit);
                l.plugin.service(now);
            }
        }
        for (i, row) in self.inserts.iter_mut().enumerate() {
            for (k, l) in row.iter_mut().enumerate() {
                if let Some(l) = l {
                    let _call = guard.call(insert_id(i, k), "running its timers", limit);
                    l.plugin.service(now);
                }
            }
        }
        let evict = self
            .editors
            .tick(now, &mut lookup(&mut self.plugins, &mut self.inserts));
        // Processors the engine let go of without being asked (none today) are just dropped.
        drop(self.engine.take_retired());
        let timers = self
            .plugins
            .iter()
            .flatten()
            .chain(self.inserts.iter().flatten().flatten())
            .filter_map(|l| l.plugin.next_deadline())
            .min();
        [timers, evict].into_iter().flatten().min()
    }

    // ---- persistence -----------------------------------------------------------------

    /// Copies every plugin's current state into the rig (so it gets saved).
    fn capture_states(&mut self) {
        let guard = self.guard.clone();
        let limit = Duration::from_secs(15);
        for i in 0..UI_SLOTS {
            if let Some(l) = self.plugins[i].as_mut() {
                let saved = {
                    let _call = guard.call(i, "saving its state", limit);
                    l.plugin.save_state()
                };
                match saved {
                    Ok(state) => self.rig.slots[i].state = Some(state),
                    Err(e) => {
                        let line = format!("slot {}: cannot save state: {e:#}", i + 1);
                        self.log.push(line);
                    }
                }
            }
            for k in 0..MAX_INSERTS {
                if let Some(l) = self.inserts[i][k].as_mut() {
                    let saved = {
                        let _call = guard.call(insert_id(i, k), "saving its state", limit);
                        l.plugin.save_state()
                    };
                    if let Ok(state) = saved
                        && let Some(Some(ins)) = self.rig.slots[i].inserts.get_mut(k)
                    {
                        ins.state = Some(state);
                    }
                }
            }
        }
    }

    pub fn save_rig(&mut self, path: &Path) -> Result<()> {
        self.capture_states();
        self.rig.validate()?;
        write_rig(&self.rig, path)?;
        self.rig_path = Some(path.to_owned());
        self.dirty = false;
        self.say(format!("saved {}", path.display()));
        Ok(())
    }

    /// Writes the autosave if it is due (or `force`).
    pub fn autosave(&mut self, every: Duration, force: bool) {
        let due = self.autosave_after.is_some_and(|t| Instant::now() >= t);
        if due {
            self.autosave_after = None;
        }
        let force = force || due;
        if every.is_zero() && !force {
            return;
        }
        if !force && self.last_autosave.elapsed() < every {
            return;
        }
        self.last_autosave = Instant::now();
        if self.plugins.iter().all(Option::is_none)
            && self.rig.slots.iter().all(|s| s.instrument.is_none())
        {
            return;
        }
        self.capture_states();
        if let Err(e) = write_rig(&self.rig, &crate::paths::autosave_file()) {
            self.say(format!("autosave failed: {e:#}"));
        }
    }

    /// Asks for an autosave in a moment (several changes in a row share one).
    fn autosave_soon(&mut self) {
        self.autosave_after
            .get_or_insert_with(|| Instant::now() + Duration::from_secs(2));
    }

    pub fn new_rig(&mut self) {
        self.clear();
        self.rig = new_rig("Untitled");
        let _ = self.engine.send(Command::SetMasterVolume(1.0));
        self.rig_path = None;
        self.dirty = false;
    }

    fn clear(&mut self) {
        for i in 0..UI_SLOTS {
            for k in 0..MAX_INSERTS {
                self.remove_insert_runtime(i, k);
            }
            self.remove_runtime(i);
        }
        let _ = self
            .editors
            .hide(&mut lookup(&mut self.plugins, &mut self.inserts));
        self.missing = vec![None; UI_SLOTS];
        self.notes = vec![None; UI_SLOTS];
        self.targets.clear();
        self.viewing = None;
        self.view_id = None;
    }

    pub fn open_rig(&mut self, path: &Path) -> Result<()> {
        let rig = read_rig(path)?;
        self.apply_rig(rig);
        self.rig_path = Some(path.to_owned());
        self.dirty = false;
        self.say(format!("opened {}", path.display()));
        Ok(())
    }

    /// Replaces everything with this rig: loads each slot's plugin and restores its state. A
    /// plugin that cannot be found or loaded stays in the rig as a "missing" slot.
    pub fn apply_rig(&mut self, mut rig: Rig) {
        self.clear();
        rig.slots.resize_with(UI_SLOTS, Slot::default);
        for slot in &mut rig.slots {
            slot.fx.clamp();
            slot.inserts.resize(MAX_INSERTS, None);
        }
        self.rig = rig;
        let _ = self
            .engine
            .send(Command::SetMasterVolume(self.rig.master_volume));
        let _ = self
            .engine
            .send(Command::SetInheritPedal(self.rig.inherit_pedal));
        for i in 0..UI_SLOTS {
            let Some(r) = self.rig.slots[i].instrument.clone() else {
                continue;
            };
            let state = self.rig.slots[i].state.clone();
            match self.restore_slot(i, &r, state.as_deref()) {
                Ok(()) => self.missing[i] = None,
                Err(e) => {
                    let why = format!("{e:#}");
                    self.say(format!("slot {}: {} is missing: {why}", i + 1, r.name));
                    self.missing[i] = Some(why);
                }
            }
        }
        for i in 0..UI_SLOTS {
            for k in 0..MAX_INSERTS {
                let Some(ins) = self.rig.slots[i].inserts[k].clone() else {
                    continue;
                };
                if let Err(e) = self.restore_insert(i, k, &ins) {
                    self.say(format!(
                        "slot {} insert {}: {} is missing: {e:#}",
                        i + 1,
                        k + 1,
                        ins.plugin.name
                    ));
                }
            }
        }
        let selected = self.rig.selected.clone();
        for (n, &i) in selected.iter().enumerate() {
            if i < UI_SLOTS {
                self.select(i, n > 0);
            }
        }
        self.dirty = false;
    }

    /// Decodes every SoundFont the rig uses, one thread per file.
    fn decode_soundfonts(&self) -> Vec<Arc<merlin_hosting::soundfont::Font>> {
        use merlin_hosting::soundfont::{Font, is_soundfont};
        let mut paths: Vec<PathBuf> = self
            .rig
            .slots
            .iter()
            .filter_map(|s| s.instrument.as_ref())
            .filter_map(|r| self.locate(r).ok())
            .map(|(p, _)| p)
            .filter(|p| is_soundfont(p) && !Font::is_cached(p))
            .collect();
        paths.sort();
        paths.dedup();
        std::thread::scope(|scope| {
            let jobs: Vec<_> = paths
                .iter()
                .map(|p| scope.spawn(move || Font::load(p)))
                .collect();
            jobs.into_iter()
                .filter_map(|j| j.join().ok()?.ok())
                .collect()
        })
    }

    /// Finds the file for a saved plugin: by id in what was scanned (a path alone can go stale
    /// after a reinstall), else by the saved path. Refuses unsupported and quarantined plugins.
    fn locate(&self, r: &PluginRef) -> Result<(PathBuf, bool)> {
        let found = self
            .found
            .iter()
            .chain(&self.effects)
            .find(|f| f.info.id == r.id && f.windows == r.windows)
            .or_else(|| {
                self.found
                    .iter()
                    .chain(&self.effects)
                    .find(|f| f.info.id == r.id)
            });
        if let Some(why) = crate::quirks::lookup(&r.id, r.windows).unsupported {
            return Err(anyhow!("not supported: {why}"));
        }
        let path = match found {
            Some(f) => f.path.clone(),
            None => r
                .path
                .clone()
                .filter(|p| p.exists())
                .context("plugin not found")?,
        };
        if let Some(q) = self.quarantine.get(&path, &r.id) {
            return Err(anyhow!("quarantined: {}", q.reason));
        }
        Ok((path, found.map_or(r.windows, |f| f.windows)))
    }

    fn restore_slot(&mut self, i: usize, r: &PluginRef, state: Option<&[u8]>) -> Result<()> {
        let (path, windows) = self.locate(r)?;
        let _loading = crate::marker::Loading::begin(&path, &r.id, &r.name);
        let mut loaded = self.create_plugin(i, &path, &r.id, windows)?;
        if let Some(state) = state {
            loaded
                .plugin
                .load_state(state)
                .context("restoring the saved state")?;
        }
        self.install(i, loaded)
    }

    // ---- insert effects -------------------------------------------------------------

    /// Puts an effect plugin into position `index` of a slot's insert chain.
    pub fn load_insert(&mut self, slot: usize, index: usize, found: &Found) -> Result<()> {
        if let Some(why) = crate::quirks::lookup(&found.info.id, found.windows).unsupported {
            return Err(anyhow!("{} is not supported: {why}", found.info.name));
        }
        if self.quarantine.remove(&found.path, &found.info.id) {
            let _ = self.quarantine.save(&crate::paths::quarantine_file());
        }
        self.unload_insert(slot, index);
        let t = Instant::now();
        let loading = crate::marker::Loading::begin(&found.path, &found.info.id, &found.info.name);
        let loaded = self.create_plugin(
            insert_id(slot, index),
            &found.path,
            &found.info.id,
            found.windows,
        )?;
        self.install_insert(slot, index, loaded)?;
        drop(loading);
        self.rig.slots[slot].inserts.resize(MAX_INSERTS, None);
        self.rig.slots[slot].inserts[index] = Some(Insert {
            plugin: PluginRef {
                format: found.info.format,
                id: found.info.id.clone(),
                name: found.info.name.clone(),
                path: Some(found.path.clone()),
                windows: found.windows,
            },
            state: None,
            bypass: false,
        });
        self.dirty = true;
        self.say(format!(
            "slot {} insert {} <- {} in {:?}",
            slot + 1,
            index + 1,
            found.info.name,
            t.elapsed()
        ));
        self.autosave_soon();
        Ok(())
    }

    fn install_insert(&mut self, slot: usize, index: usize, mut loaded: Loaded) -> Result<()> {
        let guard = self.guard.clone();
        let processor = {
            let _call = guard.call(
                insert_id(slot, index),
                "starting up",
                Duration::from_secs(30),
            );
            loaded
                .plugin
                .activate(self.sample_rate as f64, MAX_BLOCK as u32)?
        };
        self.inserts[slot][index] = Some(loaded);
        self.engine
            .send(Command::SetInsert {
                slot,
                index,
                processor,
            })
            .map_err(|_| anyhow!("engine command queue full"))?;
        Ok(())
    }

    fn restore_insert(&mut self, slot: usize, index: usize, ins: &Insert) -> Result<()> {
        let (path, windows) = self.locate(&ins.plugin)?;
        let _loading = crate::marker::Loading::begin(&path, &ins.plugin.id, &ins.plugin.name);
        let mut loaded =
            self.create_plugin(insert_id(slot, index), &path, &ins.plugin.id, windows)?;
        if let Some(state) = &ins.state {
            loaded
                .plugin
                .load_state(state)
                .context("restoring the saved state")?;
        }
        self.install_insert(slot, index, loaded)?;
        if ins.bypass {
            let _ = self.engine.send(Command::SetInsertBypass {
                slot,
                index,
                bypass: true,
            });
        }
        Ok(())
    }

    pub fn unload_insert(&mut self, slot: usize, index: usize) {
        self.remove_insert_runtime(slot, index);
        if let Some(entry) = self.rig.slots[slot].inserts.get_mut(index)
            && entry.take().is_some()
        {
            self.dirty = true;
        }
        if self.view_id == Some(insert_id(slot, index)) {
            self.view_id = Some(slot);
        }
    }

    fn remove_insert_runtime(&mut self, slot: usize, index: usize) -> bool {
        if self.inserts[slot][index].is_none() {
            return false;
        }
        let id = insert_id(slot, index);
        self.guard.unregister_slot(id);
        self.guard.begin_close();
        let _ = self
            .editors
            .forget(id, &mut lookup(&mut self.plugins, &mut self.inserts));
        let _ = self.engine.send(Command::ClearInsert { slot, index });
        self.wait_for_retired();
        if let Some(mut l) = self.inserts[slot][index].take() {
            l.plugin.deactivate();
        }
        self.guard.end_close();
        true
    }

    pub fn toggle_insert_bypass(&mut self, slot: usize, index: usize) {
        if let Some(Some(ins)) = self.rig.slots[slot].inserts.get_mut(index) {
            ins.bypass = !ins.bypass;
            let bypass = ins.bypass;
            self.dirty = true;
            let _ = self.engine.send(Command::SetInsertBypass {
                slot,
                index,
                bypass,
            });
        }
    }

    /// Waits (briefly) for the audio thread to hand a removed processor back, so its plugin can
    /// be deactivated. The audio thread answers within a cycle.
    fn wait_for_retired(&mut self) {
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut retired = Vec::new();
        while retired.is_empty() && Instant::now() < deadline {
            retired = self.engine.take_retired();
            if retired.is_empty() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        drop(retired);
    }

    /// Tries a missing or quarantined slot again from its saved plugin and state.
    pub fn retry(&mut self, i: usize) -> Result<()> {
        let r = self.rig.slots[i]
            .instrument
            .clone()
            .context("nothing to retry in this slot")?;
        if let Some(path) = r.path.clone().or_else(|| {
            self.found
                .iter()
                .find(|f| f.info.id == r.id)
                .map(|f| f.path.clone())
        }) && self.quarantine.remove(&path, &r.id)
        {
            let _ = self.quarantine.save(&crate::paths::quarantine_file());
        }
        let state = self.rig.slots[i].state.clone();
        self.restore_slot(i, &r, state.as_deref())?;
        self.missing[i] = None;
        self.say(format!("slot {}: {} loaded again", i + 1, r.name));
        if self.targets.is_empty() {
            self.select(i, false);
        }
        Ok(())
    }

    /// Orderly shutdown: autosave, editors, engine, plugins.
    pub fn shutdown(&mut self) {
        self.guard.disarm();
        self.autosave(Duration::ZERO, true);
        self.editors
            .shutdown(&mut lookup(&mut self.plugins, &mut self.inserts));
        if let Some(b) = self.backend.take() {
            let engine = b.stop();
            drop(engine);
        }
        // The engine (and the processors inside) is gone; the plugins can be deactivated.
        drop(self.engine.take_retired());
        for l in self.plugins.iter_mut().flatten() {
            l.plugin.deactivate();
        }
        for l in self.inserts.iter_mut().flatten().flatten() {
            l.plugin.deactivate();
        }
        self.plugins.iter_mut().for_each(|p| *p = None);
        self.inserts
            .iter_mut()
            .for_each(|row| row.iter_mut().for_each(|p| *p = None));
    }

    /// Refreshes [`Session::slot_cpu`] (call a few times a second).
    pub fn update_loads(&mut self) {
        self.update_midi_activity();
        let now = Instant::now();
        let dt = now.duration_since(self.last_load_update);
        if dt < Duration::from_millis(250) {
            return;
        }
        self.last_load_update = now;
        let shared = &self.engine.shared;
        for i in 0..UI_SLOTS {
            let avg = f32::from_bits(shared.load[i].load(Ordering::Relaxed));
            let worst = f32::from_bits(shared.load_max[i].swap(0, Ordering::Relaxed));
            let c = &mut self.slot_cpu[i];
            c.audio = avg;
            c.audio_worst = worst.max(c.audio_worst * 0.9);
        }
        // Wine hosts: CPU time since the last reading, per slot (inserts count for their slot).
        let ticks_per_s = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f32;
        let mut wine: [Option<f32>; UI_SLOTS] = [None; UI_SLOTS];
        let mut seen = std::collections::HashSet::new();
        for (id, pid) in self.guard.host_pids() {
            let slot = if id < UI_SLOTS {
                id
            } else {
                (id - UI_SLOTS) / MAX_INSERTS
            };
            let Some(ticks) = guard::cpu_ticks(pid) else {
                continue;
            };
            seen.insert(pid);
            let pct = match self.host_cpu.insert(pid, (ticks, now)) {
                Some((before, t0)) if now > t0 => {
                    ticks.saturating_sub(before) as f32
                        / ticks_per_s
                        / now.duration_since(t0).as_secs_f32()
                        * 100.0
                }
                _ => 0.0,
            };
            if slot < UI_SLOTS {
                *wine[slot].get_or_insert(0.0) += pct;
            }
        }
        self.host_cpu.retain(|pid, _| seen.contains(pid));
        for (c, w) in self.slot_cpu.iter_mut().zip(wine) {
            c.wine = w;
        }
    }

    /// Peaks since the last call, per slot.
    pub fn take_peaks(&self) -> [f32; UI_SLOTS] {
        std::array::from_fn(|i| {
            f32::from_bits(self.engine.shared.peaks[i].swap(0, Ordering::Relaxed))
        })
    }
}

pub fn new_rig(name: &str) -> Rig {
    Rig {
        name: name.into(),
        slots: (0..UI_SLOTS).map(|_| Slot::default()).collect(),
        ..Rig::default()
    }
}

pub fn write_rig(rig: &Rig, path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(rig)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_rig(path: &Path) -> Result<Rig> {
    let bytes = std::fs::read(path)?;
    let (rig, migrated) =
        Rig::from_json(&bytes).with_context(|| format!("reading {}", path.display()))?;
    if migrated {
        // Keep the old file next to the upgraded one, in case the new version is a mistake.
        let backup = path.with_extension("irig.bak");
        if !backup.exists() {
            let _ = std::fs::write(&backup, &bytes);
        }
    }
    Ok(rig)
}

/// Makes the bridges (CLAP and VST3) for every prefix and says what happened, in a sentence.
/// A plugin that is in two prefixes is bridged from the first.
fn sync_report(rt: &merlin_wine::Runtime, prefixes: &[PathBuf]) -> Result<String> {
    let (mut ready, mut new, mut removed) = (0, 0, 0);
    let (mut skipped, mut problems) = (Vec::new(), Vec::new());
    let mut looked = Vec::new();
    for (n, prefix) in prefixes.iter().enumerate() {
        let rt = merlin_wine::Runtime {
            prefix: prefix.clone(),
            ..rt.clone()
        };
        let (clap, vst3) = (
            rt.sync_clap_bridges(&crate::paths::bridges_dir()),
            rt.sync_vst3_bridges(&crate::paths::bridges_vst3_dir()),
        );
        if n == 0 {
            // A broken yabridge shows on the first prefix; say so instead of "0 plugins".
            if let (Err(a), Err(b)) = (&clap, &vst3) {
                return Err(anyhow!("{a:#}; {b:#}"));
            }
        }
        let mut here = 0;
        for r in [clap, vst3] {
            match r {
                Ok(r) => {
                    here += r.created.len() + r.unchanged;
                    new += r.created.len();
                    removed += r.removed.len();
                    skipped.extend(r.skipped);
                }
                Err(e) => problems.push(format!("{e:#}")),
            }
        }
        ready += here;
        looked.push(format!(
            "{} ({})",
            prefix.display(),
            if !prefix.join("drive_c").is_dir() {
                "not created yet".to_owned()
            } else if here == 0 {
                "no plugins in its CLAP or VST3 folders".to_owned()
            } else {
                format!("{here} plugin{}", if here == 1 { "" } else { "s" })
            }
        ));
    }
    let mut text = format!(
        "{ready} Windows plugin{} ready ({new} new, {removed} removed)",
        if ready == 1 { "" } else { "s" }
    );
    if ready == 0 {
        text.push_str(&format!("; looked in {}", looked.join(", ")));
    }
    if !skipped.is_empty() {
        let list: Vec<String> = skipped
            .iter()
            .map(|(p, why)| {
                format!(
                    "{} ({why})",
                    p.file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                )
            })
            .collect();
        text.push_str(&format!("; skipped: {}", list.join(", ")));
    }
    if !problems.is_empty() {
        text.push_str(&format!("; {}", problems.join("; ")));
    }
    Ok(text)
}

const _: () = assert!(UI_SLOTS <= MAX_SLOTS);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{DeviceRole, MidiDevicePref};

    fn pref(name: &str, role: DeviceRole, enabled: bool) -> MidiDevicePref {
        MidiDevicePref {
            name: name.into(),
            role,
            enabled,
        }
    }

    #[test]
    fn midi_devices_follow_their_saved_choices_by_name() {
        let policy = midi_policy(&[
            pref("Launchpad", DeviceRole::Controller, true),
            pref("Old Keys", DeviceRole::Keyboard, false),
        ]);
        // The names a person reads, from the port names the system uses.
        assert_eq!(
            policy("Midi-Bridge:Launchpad (capture)"),
            Some(InputRole::Controller)
        );
        assert_eq!(policy("Old Keys:out"), None, "switched off: not connected");
        // Nothing saved: a keyboard, so a new device just works.
        assert_eq!(
            policy("Midi-Bridge:Brand New (capture)"),
            Some(InputRole::Keyboard)
        );
    }

    #[test]
    fn device_prefs_survive_the_settings_file() {
        let s = Settings {
            midi_devices: vec![pref("Launchpad", DeviceRole::Controller, false)],
            ..Settings::default()
        };
        let text = toml::to_string_pretty(&s).unwrap();
        assert!(text.contains("role = \"controller\""), "{text}");
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.midi_devices, s.midi_devices);
        // Older settings files without any device choices still load.
        let old: Settings = toml::from_str("frames = 64\n").unwrap();
        assert!(old.midi_devices.is_empty());
        // And a device entry that only names the device means "keyboard, on".
        let sparse: Settings = toml::from_str("[[midi_devices]]\nname = \"X\"\n").unwrap();
        assert_eq!(
            sparse.midi_devices,
            vec![pref("X", DeviceRole::Keyboard, true)]
        );
    }
}
