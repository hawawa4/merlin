//! Everything that is not drawing: the engine and its JACK backend, the loaded plugins, the
//! rig and its persistence, the editor dock. The UI asks it to do things and reads its state.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use merlin_audio_io::{JackBackend, JackClient, MidiSource};
use merlin_embed::{EditorManager, PluginLookup};
use merlin_engine::{Command, EngineHandle, Filter};
use merlin_hosting::clap::ClapLibrary;
use merlin_hosting::{EditorSize, HostEvent, MidiEvent, PluginHandle, Waker};
use merlin_model::{Format, MAX_SLOTS, PluginRef, Rig, SelectMode, Slot};
use merlin_scanner::{Found, Quarantine};
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

/// Lets the editor manager borrow the plugins of a rack.
pub struct Lookup<'a>(&'a mut [Option<Loaded>]);

pub fn lookup(plugins: &mut [Option<Loaded>]) -> Lookup<'_> {
    Lookup(plugins)
}

impl PluginLookup for Lookup<'_> {
    fn plugin(&mut self, slot: usize) -> Option<&mut dyn PluginHandle> {
        self.0
            .get_mut(slot)?
            .as_mut()
            .map(|l| &mut *l.plugin as &mut dyn PluginHandle)
    }
}

pub struct Session {
    pub rig: Rig,
    pub rig_path: Option<PathBuf>,
    /// The rig changed since it was last saved.
    pub dirty: bool,
    pub plugins: Vec<Option<Loaded>>,
    /// Why a slot's saved plugin could not be loaded; the slot keeps its saved plugin and
    /// state so nothing is lost.
    pub missing: Vec<Option<String>>,
    /// Slots that are keyboard targets, in selection order.
    pub targets: Vec<usize>,
    /// The slot whose editor is on show (or would be, if it has none or it is not allowed).
    pub viewing: Option<usize>,
    /// What to tell the user instead of, or beside, a slot's editor (see `quirks`).
    pub notes: Vec<Option<String>>,
    pub found: Vec<Found>,
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
}

impl Session {
    pub fn new(
        xid: u32,
        scale: f64,
        proxy: EventLoopProxy<UserEvent>,
        settings: &Settings,
        found: Vec<Found>,
        guard: Arc<Guard>,
    ) -> Result<Self> {
        let client = JackClient::open("merlin", Some(settings.frames))?;
        let sample_rate = client.sample_rate;
        let (engine, handle) = merlin_engine::new(sample_rate as f32, MAX_BLOCK);
        let backend = client.start(engine)?;
        let outputs = backend.connect_outputs();
        let midi_sources = backend.connect_midi_sources();
        let mut s = Self {
            rig: new_rig("Untitled"),
            rig_path: None,
            dirty: false,
            plugins: (0..UI_SLOTS).map(|_| None).collect(),
            missing: vec![None; UI_SLOTS],
            targets: Vec::new(),
            viewing: None,
            notes: vec![None; UI_SLOTS],
            found,
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
        };
        s.say(format!(
            "JACK at {sample_rate} Hz, outputs {outputs:?}, MIDI {:?}",
            s.midi_sources
                .iter()
                .map(|m| m.device.as_str())
                .collect::<Vec<_>>()
        ));
        Ok(s)
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
        let _ = self.engine.send(Command::SetFilter { slot: i, filter });
        let _ = self.engine.send(Command::SetMix {
            slot: i,
            volume,
            pan,
            mute,
        });
    }

    fn create_plugin(&mut self, i: usize, path: &Path, id: &str, windows: bool) -> Result<Loaded> {
        // A bridged plugin starts a Wine host process: remember which one is ours, so the
        // watchdog can tell a crash from a normal unload.
        let before = if windows {
            guard::host_children()
        } else {
            Default::default()
        };
        let loaded = self.create_plugin_inner(i, path, id)?;
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
        let plugin = ClapLibrary::load(path)?.create(id, waker)?;
        Ok(Loaded {
            plugin: Box::new(plugin),
        })
    }

    /// Installs a created plugin in a slot: activate and hand its processor to the engine.
    fn install(&mut self, i: usize, mut loaded: Loaded) -> Result<()> {
        let processor = loaded
            .plugin
            .activate(self.sample_rate as f64, MAX_BLOCK as u32)?;
        self.plugins[i] = Some(loaded);
        self.push_slot_settings(i);
        self.engine
            .send(Command::SetProcessor { slot: i, processor })
            .map_err(|_| anyhow!("engine command queue full"))?;
        Ok(())
    }

    /// Loads a plugin chosen in the browser into a slot (replacing what is there).
    pub fn load(&mut self, i: usize, found: &Found) -> Result<()> {
        if let Some(why) = crate::quirks::lookup(&found.info.id, found.windows).unsupported {
            return Err(anyhow!("{} is not supported: {why}", found.info.name));
        }
        // Loading it by hand is the "retry": it comes off the quarantine list.
        if self.quarantine.remove(&found.path, &found.info.id) {
            let _ = self.quarantine.save(&crate::paths::quarantine_file());
        }
        self.unload(i);
        let t = Instant::now();
        let loaded = self.create_plugin(i, &found.path, &found.info.id, found.windows)?;
        self.install(i, loaded)?;
        let slot = &mut self.rig.slots[i];
        slot.instrument = Some(PluginRef {
            format: Format::Clap,
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
        // Structural change: save now, so a crash right after restarts from this rig.
        self.autosave(Duration::ZERO, true);
        Ok(())
    }

    /// Takes the plugin out of a slot: editor first, then the engine lets go of the processor,
    /// then the plugin is deactivated and destroyed. The rig slot is cleared.
    pub fn unload(&mut self, i: usize) {
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
        let _ = self.editors.forget(i, &mut Lookup(&mut self.plugins));
        let _ = self.engine.send(Command::ClearProcessor { slot: i });
        // The processor comes back through the retired queue; wait for it (the audio thread
        // answers within a cycle) so the plugin can be deactivated.
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut retired = Vec::new();
        while retired.is_empty() && Instant::now() < deadline {
            retired = self.engine.take_retired();
            if retired.is_empty() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        drop(retired);
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
        // Known problems with this plugin's editor are shown beside it.
        self.notes[i] = self.rig.slots[i]
            .instrument
            .as_ref()
            .and_then(|r| crate::quirks::lookup(&r.id, r.windows).warning)
            .map(str::to_owned);
        let result = self.editors.show(i, &mut Lookup(&mut self.plugins));
        match result {
            Ok(o) if o.created => self.say(format!("editor {} created in {:?}", i + 1, o.elapsed)),
            Ok(_) => {}
            Err(e) => self.say(format!("slot {}: no editor: {e:#}", i + 1)),
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
        let UserEvent::Host(i, e) = event;
        match e {
            HostEvent::MainThreadCallback => {
                if let Some(l) = self.plugins.get_mut(i).and_then(Option::as_mut) {
                    l.plugin.on_main_thread();
                }
            }
            HostEvent::FdReady => {
                if let Some(l) = self.plugins.get_mut(i).and_then(Option::as_mut) {
                    l.plugin.on_fd_ready();
                }
            }
            HostEvent::EditorResize { width, height } => {
                let _ = self.editors.plugin_resized(
                    i,
                    EditorSize { width, height },
                    &mut Lookup(&mut self.plugins),
                );
            }
            HostEvent::EditorClosed => {
                self.say(format!("slot {}: plugin closed its editor", i + 1))
            }
            HostEvent::StateDirty => self.dirty = true,
        }
        self.service();
    }

    /// Plugin timers, editor eviction, retired processors. Returns when to run again.
    pub fn service(&mut self) -> Option<Instant> {
        let now = Instant::now();
        for l in self.plugins.iter_mut().flatten() {
            l.plugin.service(now);
        }
        let evict = self.editors.tick(now, &mut Lookup(&mut self.plugins));
        // Processors the engine let go of without being asked (none today) are just dropped.
        drop(self.engine.take_retired());
        let timers = self
            .plugins
            .iter()
            .flatten()
            .filter_map(|l| l.plugin.next_deadline())
            .min();
        [timers, evict].into_iter().flatten().min()
    }

    // ---- persistence -----------------------------------------------------------------

    /// Copies every plugin's current state into the rig (so it gets saved).
    fn capture_states(&mut self) {
        for i in 0..UI_SLOTS {
            if let Some(l) = self.plugins[i].as_mut() {
                match l.plugin.save_state() {
                    Ok(state) => self.rig.slots[i].state = Some(state),
                    Err(e) => {
                        let line = format!("slot {}: cannot save state: {e:#}", i + 1);
                        self.log.push(line);
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

    pub fn new_rig(&mut self) {
        self.clear();
        self.rig = new_rig("Untitled");
        self.rig_path = None;
        self.dirty = false;
    }

    fn clear(&mut self) {
        for i in 0..UI_SLOTS {
            self.remove_runtime(i);
        }
        let _ = self.editors.hide(&mut Lookup(&mut self.plugins));
        self.missing = vec![None; UI_SLOTS];
        self.notes = vec![None; UI_SLOTS];
        self.targets.clear();
        self.viewing = None;
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
        self.rig = rig;
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
        let selected = self.rig.selected.clone();
        for (n, &i) in selected.iter().enumerate() {
            if i < UI_SLOTS {
                self.select(i, n > 0);
            }
        }
        self.dirty = false;
    }

    fn restore_slot(&mut self, i: usize, r: &PluginRef, state: Option<&[u8]>) -> Result<()> {
        // Find the plugin again by id (a path alone can go stale after a reinstall).
        let found = self
            .found
            .iter()
            .find(|f| f.info.id == r.id && f.windows == r.windows)
            .or_else(|| self.found.iter().find(|f| f.info.id == r.id))
            .cloned();
        if let Some(why) = crate::quirks::lookup(&r.id, r.windows).unsupported {
            return Err(anyhow!("not supported: {why}"));
        }
        let path = match &found {
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
        let windows = found.as_ref().map_or(r.windows, |f| f.windows);
        let mut loaded = self.create_plugin(i, &path, &r.id, windows)?;
        if let Some(state) = state {
            loaded
                .plugin
                .load_state(state)
                .context("restoring the saved state")?;
        }
        self.install(i, loaded)
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
        self.editors.shutdown(&mut Lookup(&mut self.plugins));
        if let Some(b) = self.backend.take() {
            let engine = b.stop();
            drop(engine);
        }
        // The engine (and the processors inside) is gone; the plugins can be deactivated.
        drop(self.engine.take_retired());
        for l in self.plugins.iter_mut().flatten() {
            l.plugin.deactivate();
        }
        self.plugins.iter_mut().for_each(|p| *p = None);
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
    let rig: Rig = serde_json::from_slice(&std::fs::read(path)?)
        .with_context(|| format!("reading {}", path.display()))?;
    rig.validate()?;
    Ok(rig)
}

const _: () = assert!(UI_SLOTS <= MAX_SLOTS);
