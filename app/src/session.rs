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
        let midi_sources = backend.connect_midi_sources();
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
        };
        s.rebuild_dirs();
        // Start from what the cache knows, then let the background scan catch up.
        s.entries = merlin_scanner::cached_entries(
            &s.plugin_dirs,
            &Cache::load(&crate::paths::plugin_cache_file()),
        );
        s.refresh_found();
        s.start_scan(Vec::new());
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
        dirs.extend(self.settings.plugin_dirs.iter().cloned());
        dirs.extend(self.cli_dirs.iter().cloned());
        dirs.dedup();
        self.plugin_dirs = dirs;
    }

    pub fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&crate::paths::settings_file()) {
            self.say(format!("could not save the settings: {e:#}"));
        }
    }

    pub fn add_plugin_dir(&mut self, dir: PathBuf) {
        if !dir.as_os_str().is_empty() && !self.settings.plugin_dirs.contains(&dir) {
            self.settings.plugin_dirs.push(dir);
            self.save_settings();
            self.rebuild_dirs();
            self.start_scan(Vec::new());
        }
    }

    pub fn remove_plugin_dir(&mut self, index: usize) {
        if index < self.settings.plugin_dirs.len() {
            self.settings.plugin_dirs.remove(index);
            self.save_settings();
            self.rebuild_dirs();
            // Entries from folders that are no longer scanned go away.
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

    pub fn set_buffer_size(&mut self, frames: u32) -> Result<()> {
        let backend = self.backend.as_ref().context("audio is not running")?;
        let now = backend.set_buffer_size(frames)?;
        self.settings.frames = frames;
        self.save_settings();
        self.say(format!("buffer size {now} frames"));
        Ok(())
    }

    /// Looks for MIDI devices that appeared since the last time and connects them.
    pub fn refresh_midi(&mut self) {
        if let Some(b) = self.backend.as_ref() {
            self.midi_sources = b.connect_midi_sources();
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
        let plugin: Box<dyn PluginHandle> = if path.extension().is_some_and(|e| e == "vst3") {
            Box::new(Vst3Library::load(path)?.create(id, waker)?)
        } else {
            Box::new(ClapLibrary::load(path)?.create(id, waker)?)
        };
        Ok(Loaded { plugin })
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
        // Known problems with this plugin's editor are shown beside it.
        self.notes[i] = self.rig.slots[i]
            .instrument
            .as_ref()
            .and_then(|r| crate::quirks::lookup(&r.id, r.windows).warning)
            .map(str::to_owned);
        let result = self
            .editors
            .show(i, &mut lookup(&mut self.plugins, &mut self.inserts));
        match result {
            Ok(o) if o.created => self.say(format!("editor {} created in {:?}", i + 1, o.elapsed)),
            Ok(_) => {}
            Err(e) => self.say(format!("slot {}: no editor: {e:#}", i + 1)),
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
        for l in self.plugins.iter_mut().flatten() {
            l.plugin.service(now);
        }
        for l in self.inserts.iter_mut().flatten().flatten() {
            l.plugin.service(now);
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
            for k in 0..MAX_INSERTS {
                if let Some(l) = self.inserts[i][k].as_mut()
                    && let Ok(state) = l.plugin.save_state()
                    && let Some(Some(ins)) = self.rig.slots[i].inserts.get_mut(k)
                {
                    ins.state = Some(state);
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
        let loaded = self.create_plugin(
            insert_id(slot, index),
            &found.path,
            &found.info.id,
            found.windows,
        )?;
        self.install_insert(slot, index, loaded)?;
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
        self.autosave(Duration::ZERO, true);
        Ok(())
    }

    fn install_insert(&mut self, slot: usize, index: usize, mut loaded: Loaded) -> Result<()> {
        let processor = loaded
            .plugin
            .activate(self.sample_rate as f64, MAX_BLOCK as u32)?;
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
