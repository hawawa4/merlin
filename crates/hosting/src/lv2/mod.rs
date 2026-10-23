//! LV2 hosting (plan Phase 6): plugins described by Turtle files, run through their C
//! interface, with X11 editors. Everything is done here (no lilv, no suil), so nothing needs to
//! be installed beside the app.
//!
//! Supported: audio, control and atom (MIDI) ports, parameters as control ports, state through
//! `state:interface`, X11 editors with idle and resize, and the usual features (urid, options,
//! buf-size, log, state paths). Not yet: the worker extension, patch parameters (plugins that
//! expose parameters only through `patch:writable`, like Surge XT, work and save their state but
//! list no parameters), CV ports (connected to silence), GTK and Qt editors.

mod abi;
mod editor;
mod features;
mod instance;
mod processor;
pub mod ttl;
mod urid;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use libloading::os::unix::Library;
use merlin_model::Format;

use crate::{
    EditorSize, ParamInfo, PluginEditor, PluginHandle, PluginInfo, PluginProcessor, Waker,
};
use editor::Lv2Editor;
use features::{Audio, FeatureSet, supported};
use instance::{Bank, Instance, Property, pack_state, unpack_state};
use processor::{Lv2Processor, MAX_FRAMES, UiAtom};
use ttl::{PluginDesc, PortKind, read_bundle};
use urid::Urids;

/// Where LV2 bundles usually live, with `LV2_PATH` first.
pub fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = std::env::var_os("LV2_PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".lv2"));
    }
    dirs.extend(
        ["/usr/lib/lv2", "/usr/lib64/lv2", "/usr/local/lib/lv2"]
            .into_iter()
            .map(PathBuf::from),
    );
    dirs
}

/// A `.lv2` bundle.
pub struct Lv2Library {
    pub path: PathBuf,
    plugins: Vec<PluginDesc>,
}

fn is_effect_like(d: &PluginDesc) -> bool {
    d.ports
        .iter()
        .any(|p| p.kind == PortKind::Audio && !p.input)
}

impl Lv2Library {
    pub fn open(bundle: &Path) -> Result<Self> {
        Ok(Self {
            path: bundle.to_owned(),
            plugins: read_bundle(bundle)?,
        })
    }

    /// The plugins that make audio (a plugin with no audio output is not an instrument or an
    /// effect to put in a slot).
    pub fn plugins(&self) -> Vec<PluginInfo> {
        self.plugins
            .iter()
            .filter(|d| is_effect_like(d))
            .map(|d| PluginInfo {
                format: Format::Lv2,
                id: d.uri.clone(),
                name: d.name.clone(),
                vendor: d.vendor.clone(),
                version: d.version.clone(),
                features: if d.instrument {
                    vec!["instrument".into(), "synthesizer".into()]
                } else {
                    vec!["audio-effect".into()]
                },
            })
            .collect()
    }

    /// Creates an instance (main thread). The plugin is not running until it is activated.
    pub fn create(
        &self,
        uri: &str,
        waker: Arc<dyn Waker>,
        sample_rate: f64,
        max_block: u32,
    ) -> Result<Lv2Plugin> {
        let desc = self
            .plugins
            .iter()
            .find(|d| d.uri == uri)
            .with_context(|| format!("no plugin {uri} in {}", self.path.display()))?
            .clone();
        for r in &desc.required {
            if !supported(r) {
                bail!("the plugin needs {r}, which this host does not provide yet");
            }
        }
        // SAFETY: loading a plugin runs its static constructors; that is the point.
        let lib = Arc::new(
            unsafe { Library::new(&desc.binary) }
                .with_context(|| format!("cannot load {}", desc.binary.display()))?,
        );
        let urids = Urids::new();
        let block = (max_block as usize).min(MAX_FRAMES) as i32;
        let audio = Audio {
            sample_rate,
            min_block: 1,
            max_block: block,
            scale: 1.0,
        };
        let worker = worker::Worker::new();
        let features = FeatureSet::new(
            urids,
            &audio,
            vec![(worker::WORKER_SCHEDULE, worker.schedule_ptr())],
        );
        let inst = Arc::new(Instance::new(
            lib,
            &desc.uri,
            sample_rate,
            &desc.bundle,
            features,
            Some(worker),
        )?);
        let count = desc.ports.iter().map(|p| p.index + 1).max().unwrap_or(0);
        let bank = Arc::new(Bank::new(count, |i| {
            desc.ports
                .iter()
                .find(|p| p.index == i)
                .map_or(0.0, |p| p.default as f32)
        }));
        let (tx, _rx) = rtrb::RingBuffer::new(64);
        let info = self
            .plugins()
            .into_iter()
            .find(|i| i.id == desc.uri)
            .context("the plugin has no audio output")?;
        Ok(Lv2Plugin {
            info,
            desc,
            editor: None,
            to_plugin: Arc::new(Mutex::new(tx)),
            bank,
            waker,
            sample_rate,
            active: false,
            inst,
        })
    }
}

pub struct Lv2Plugin {
    info: PluginInfo,
    desc: PluginDesc,
    // Before `inst`: the editor goes away first.
    editor: Option<Lv2Editor>,
    to_plugin: Arc<Mutex<rtrb::Producer<UiAtom>>>,
    bank: Arc<Bank>,
    waker: Arc<dyn Waker>,
    sample_rate: f64,
    active: bool,
    inst: Arc<Instance>,
}

impl Lv2Plugin {
    /// The current value of a control port (an output shows what the plugin last wrote).
    pub fn port_value(&self, port: usize) -> f32 {
        self.bank.get(port)
    }

    fn control_inputs(&self) -> impl Iterator<Item = &ttl::PortDesc> {
        self.desc
            .ports
            .iter()
            .filter(|p| p.kind == PortKind::Control && p.input)
    }
}

impl PluginHandle for Lv2Plugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn activate(&mut self, sample_rate: f64, _max_block: u32) -> Result<Box<dyn PluginProcessor>> {
        if (sample_rate - self.sample_rate).abs() > 1.0 {
            bail!(
                "the plugin was started at {} Hz but the audio runs at {sample_rate} Hz",
                self.sample_rate
            );
        }
        let (tx, rx) = rtrb::RingBuffer::new(64);
        *self.to_plugin.lock().unwrap() = tx;
        self.active = true;
        Ok(Box::new(Lv2Processor::new(
            self.inst.clone(),
            &self.desc,
            self.bank.clone(),
            rx,
            sample_rate as f32,
        )))
    }

    fn deactivate(&mut self) {
        self.active = false;
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        self.control_inputs()
            .filter(|p| !p.hidden)
            .map(|p| ParamInfo {
                id: p.index as u32,
                name: if p.name.is_empty() {
                    p.symbol.clone()
                } else {
                    p.name.clone()
                },
                module: String::new(),
                min: p.min,
                max: p.max,
                default: p.default,
                automatable: true,
                stepped: p.toggled || p.integer,
                hidden: false,
                read_only: false,
            })
            .collect()
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(p) = self.desc.ports.iter().find(|p| p.index == id as usize) else {
            return;
        };
        let v = value.clamp(p.min.min(p.max), p.min.max(p.max)) as f32;
        self.bank.set(id as usize, v);
        if let Some(e) = &self.editor {
            e.port_event(id, v);
        }
    }

    fn save_state(&mut self) -> Result<Vec<u8>> {
        let controls: Vec<(u32, f32)> = self
            .control_inputs()
            .map(|p| (p.index as u32, self.bank.get(p.index)))
            .collect();
        let props: Vec<Property> = self.inst.save_properties()?;
        Ok(pack_state(&controls, &props))
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        let (controls, props) = unpack_state(data)?;
        for (port, v) in controls {
            if self.control_inputs().any(|p| p.index == port as usize) {
                self.bank.set(port as usize, v);
            }
        }
        self.inst.restore_properties(&props)
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        self.desc
            .ui
            .is_some()
            .then_some(self as &mut dyn PluginEditor)
    }

    fn on_main_thread(&mut self) {}

    fn service(&mut self, now: Instant) {
        if let Some(e) = self.editor.as_mut() {
            e.idle(now);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.editor.as_ref().and_then(Lv2Editor::next_idle)
    }

    fn on_fd_ready(&mut self) {}
}

impl PluginEditor for Lv2Plugin {
    fn attach(&mut self, parent: u32, scale: f64) -> Result<EditorSize> {
        let ui = self
            .desc
            .ui
            .clone()
            .context("the plugin has no X11 editor")?;
        let e = Lv2Editor::open(
            &self.desc,
            &ui,
            &self.inst,
            &self.inst.lib,
            self.bank.clone(),
            self.to_plugin.clone(),
            self.waker.clone(),
            parent,
            scale,
            self.sample_rate,
        )?;
        let size = e.size;
        self.editor = Some(e);
        Ok(size)
    }

    fn is_attached(&self) -> bool {
        self.editor.is_some()
    }

    fn show(&mut self) {}

    fn hide(&mut self) {}

    fn size(&self) -> EditorSize {
        self.editor.as_ref().map_or(
            EditorSize {
                width: 640,
                height: 480,
            },
            |e| e.size,
        )
    }

    fn can_resize(&self) -> bool {
        false
    }

    fn adjust_size(&mut self, _want: EditorSize) -> EditorSize {
        self.size()
    }

    fn set_size(&mut self, _size: EditorSize) -> bool {
        false
    }

    fn note_size(&mut self, size: EditorSize) {
        if let Some(e) = self.editor.as_mut() {
            e.size = size;
        }
    }

    fn detach(&mut self) {
        self.editor = None;
    }
}

// SAFETY: a plugin handle lives on the main thread, as the other formats' do.
unsafe impl Send for Lv2Plugin {}
