//! CLAP hosting on `clack-host` (plan §4.5): library loading, plugin instances, parameters,
//! state, and the embeddable editor.

mod host;
mod processor;

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, Window as ClapWindow};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_host::prelude::*;
use merlin_model::Format;

use host::{ClapHost, FdPoller, MainThread, Shared};
use processor::{ClapProcessor, MAX_FRAMES, ParamChange, port_channels};

use crate::{
    EditorSize, ParamInfo, PluginEditor, PluginHandle, PluginInfo, PluginProcessor, Waker,
};

/// A loaded `.clap` file.
pub struct ClapLibrary {
    entry: PluginEntry,
    pub path: PathBuf,
}

impl ClapLibrary {
    pub fn load(path: &Path) -> Result<Self> {
        // SAFETY: loading a plugin binary runs its code; that is the point of a host.
        let entry = unsafe { PluginEntry::load(path) }
            .map_err(|e| anyhow!("loading {}: {e:?}", path.display()))?;
        Ok(Self {
            entry,
            path: path.to_owned(),
        })
    }

    /// Every plugin in the file.
    pub fn plugins(&self) -> Result<Vec<PluginInfo>> {
        let factory = self
            .entry
            .get_plugin_factory()
            .context("no plugin factory")?;
        Ok(factory
            .plugin_descriptors()
            .map(|d| {
                let s = |c: Option<&std::ffi::CStr>| {
                    c.map(|c| c.to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                PluginInfo {
                    format: Format::Clap,
                    id: s(d.id()),
                    name: s(d.name()),
                    vendor: s(d.vendor()),
                    version: s(d.version()),
                    features: d
                        .features()
                        .map(|f| f.to_string_lossy().into_owned())
                        .collect(),
                }
            })
            .collect())
    }

    /// Creates an instance (main thread). The plugin is not activated yet.
    pub fn create(&self, plugin_id: &str, waker: Arc<dyn Waker>) -> Result<ClapPlugin> {
        let info = self
            .plugins()?
            .into_iter()
            .find(|p| p.id == plugin_id)
            .with_context(|| format!("no plugin '{plugin_id}' in {}", self.path.display()))?;
        let id = CString::new(plugin_id)?;
        let host_info = HostInfo::new("Merlin", "Merlin", "", env!("CARGO_PKG_VERSION"))?;
        let poller_waker = waker.clone();
        let instance = PluginInstance::<ClapHost>::new(
            |_| Shared::new(waker),
            |shared| MainThread::new(shared),
            &self.entry,
            &id,
            &host_info,
        )?;
        Ok(ClapPlugin {
            fds: FdPoller::spawn(poller_waker),
            instance,
            _entry: self.entry.clone(),
            info,
            params_tx: None,
            editor: EditorState::default(),
        })
    }
}

#[derive(Default)]
struct EditorState {
    attached: bool,
    size: Option<GuiSize>,
    can_resize: bool,
}

// Field order matters on drop: the fd poller goes first, then the instance, then the library.
pub struct ClapPlugin {
    fds: FdPoller,
    instance: PluginInstance<ClapHost>,
    _entry: PluginEntry,
    info: PluginInfo,
    params_tx: Option<rtrb::Producer<ParamChange>>,
    editor: EditorState,
}

impl ClapPlugin {
    /// Did the plugin say its state changed since the last call?
    pub fn take_dirty(&mut self) -> bool {
        self.instance
            .access_handler(|h| h.state_dirty.replace(false))
    }

    fn gui(&self) -> Option<clack_extensions::gui::PluginGui> {
        self.instance.access_handler(|h| h.gui.get())
    }
}

impl PluginHandle for ClapPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn activate(&mut self, sample_rate: f64, max_block: u32) -> Result<Box<dyn PluginProcessor>> {
        if max_block as usize > MAX_FRAMES {
            return Err(anyhow!("block size {max_block} exceeds {MAX_FRAMES}"));
        }
        let in_channels = port_channels(&mut self.instance, true);
        let out_channels = port_channels(&mut self.instance, false);
        let config = PluginAudioConfiguration {
            sample_rate,
            min_frames_count: 1,
            max_frames_count: max_block,
        };
        let processor = self
            .instance
            .activate(|_, _| (), config)?
            .start_processing()
            .map_err(|e| anyhow!("start_processing: {e}"))?;
        let (tx, rx) = rtrb::RingBuffer::new(256);
        self.params_tx = Some(tx);
        Ok(Box::new(ClapProcessor::new(
            processor,
            in_channels,
            out_channels,
            rx,
        )))
    }

    fn deactivate(&mut self) {
        self.params_tx = None;
        if let Err(e) = self.instance.try_deactivate() {
            eprintln!("deactivate {}: {e}", self.info.name);
        }
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        let handle = self.instance.plugin_handle();
        let Some(ext) = handle.get_extension::<PluginParams>() else {
            return Vec::new();
        };
        let mut buf = ParamInfoBuffer::new();
        (0..ext.count(&handle))
            .filter_map(|i| {
                ext.get_info(&handle, i, &mut buf).map(|p| ParamInfo {
                    id: p.id.get(),
                    name: String::from_utf8_lossy(p.name).into_owned(),
                    module: String::from_utf8_lossy(p.module).into_owned(),
                    min: p.min_value,
                    max: p.max_value,
                    default: p.default_value,
                    automatable: p.flags.contains(ParamInfoFlags::IS_AUTOMATABLE),
                    stepped: p.flags.contains(ParamInfoFlags::IS_STEPPED),
                    hidden: p.flags.contains(ParamInfoFlags::IS_HIDDEN),
                    read_only: p.flags.contains(ParamInfoFlags::IS_READONLY),
                })
            })
            .collect()
    }

    fn param_value(&mut self, id: u32) -> Option<f64> {
        let handle = self.instance.plugin_handle();
        let ext = handle.get_extension::<PluginParams>()?;
        ext.get_value(&handle, ClapId::from_raw(id)?)
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(tx) = self.params_tx.as_mut() {
            let _ = tx.push(ParamChange { id, value });
        }
    }

    fn save_state(&mut self) -> Result<Vec<u8>> {
        let ext = self
            .instance
            .access_handler(|h| h.state_ext.get())
            .context("plugin has no state extension")?;
        let mut buf = Vec::new();
        ext.save(&self.instance.plugin_handle(), &mut buf)?;
        Ok(buf)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        let ext = self
            .instance
            .access_handler(|h| h.state_ext.get())
            .context("plugin has no state extension")?;
        ext.load(
            &self.instance.plugin_handle(),
            &mut std::io::Cursor::new(data),
        )?;
        Ok(())
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        self.gui()
            .is_some()
            .then_some(self as &mut dyn PluginEditor)
    }

    fn on_main_thread(&mut self) {
        self.instance.call_on_main_thread_callback();
    }

    fn service(&mut self, now: Instant) {
        let (timer_ext, due) = self
            .instance
            .access_handler(|h| (h.timer_ext.get(), h.due_timers(now)));
        if let Some(ext) = timer_ext {
            for id in due {
                ext.on_timer(&self.instance.plugin_handle(), id);
            }
        }
        if let Some(fds) = self
            .instance
            .access_handler(|h| h.fds_changed.replace(false).then(|| h.fds.borrow().clone()))
        {
            self.fds.set(&fds);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.instance.access_handler(|h| h.next_timer_deadline())
    }

    fn on_fd_ready(&mut self) {
        let (ext, fds) = self
            .instance
            .access_handler(|h| (h.fd_ext.get(), h.fds.borrow().clone()));
        if let Some(ext) = ext {
            for (fd, flags) in self.fds.ready(&fds) {
                ext.on_fd(&self.instance.plugin_handle(), fd, flags);
            }
        }
        self.fds.ack();
        self.service(Instant::now());
    }
}

fn to_size(s: GuiSize) -> EditorSize {
    EditorSize {
        width: s.width,
        height: s.height,
    }
}

fn from_size(s: EditorSize) -> GuiSize {
    GuiSize {
        width: s.width,
        height: s.height,
    }
}

impl PluginEditor for ClapPlugin {
    fn attach(&mut self, parent: u32, scale: f64) -> Result<EditorSize> {
        let gui = self.gui().context("plugin has no GUI extension")?;
        let handle = self.instance.plugin_handle();
        let config = GuiConfiguration {
            api_type: GuiApiType::X11,
            is_floating: false,
        };
        if !gui.is_api_supported(&handle, config) {
            return Err(anyhow!("plugin cannot embed an X11 editor"));
        }
        gui.create(&handle, config)?;
        let _ = gui.set_scale(&handle, scale);
        self.editor.can_resize = gui.can_resize(&handle);
        let size = gui.get_size(&handle).unwrap_or(GuiSize {
            width: 800,
            height: 600,
        });
        // SAFETY: the caller keeps the parent window alive until `detach`.
        unsafe { gui.set_parent(&handle, ClapWindow::from_x11_handle(parent as _))? };
        self.editor.attached = true;
        self.editor.size = Some(size);
        Ok(to_size(size))
    }

    fn is_attached(&self) -> bool {
        self.editor.attached
    }

    fn show(&mut self) {
        if let Some(gui) = self.gui() {
            let _ = gui.show(&self.instance.plugin_handle());
        }
    }

    fn hide(&mut self) {
        if let Some(gui) = self.gui() {
            let _ = gui.hide(&self.instance.plugin_handle());
        }
    }

    fn size(&self) -> EditorSize {
        self.editor.size.map(to_size).unwrap_or(EditorSize {
            width: 0,
            height: 0,
        })
    }

    fn can_resize(&self) -> bool {
        self.editor.can_resize
    }

    fn adjust_size(&mut self, want: EditorSize) -> EditorSize {
        let Some(gui) = self.gui() else { return want };
        gui.adjust_size(&self.instance.plugin_handle(), from_size(want))
            .map(to_size)
            .unwrap_or(want)
    }

    fn set_size(&mut self, size: EditorSize) -> bool {
        let Some(gui) = self.gui() else { return false };
        let ok = gui
            .set_size(&self.instance.plugin_handle(), from_size(size))
            .is_ok();
        if ok {
            self.editor.size = Some(from_size(size));
        }
        ok
    }

    fn note_size(&mut self, size: EditorSize) {
        self.editor.size = Some(from_size(size));
    }

    fn detach(&mut self) {
        if self.editor.attached
            && let Some(gui) = self.gui()
        {
            gui.destroy(&self.instance.plugin_handle());
        }
        self.editor = EditorState::default();
    }
}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        // Close the editor before the instance goes.
        self.detach();
    }
}
