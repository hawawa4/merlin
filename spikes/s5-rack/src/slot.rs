//! One rack slot: a loaded CLAP plugin, its editor state and its main-thread plumbing.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use clack_extensions::gui::GuiSize;
use clack_host::prelude::*;
use winit::event_loop::EventLoopProxy;

use crate::audio::Route;
use crate::host::{FdPoller, Host, MainThread, Shared, UserEvent};

/// A plugin file we can load.
pub struct Found {
    pub label: String,
    pub path: PathBuf,
    /// A Windows plugin behind a yabridge bridge.
    pub windows: bool,
}

/// Looks for `*.clap` files directly inside each directory. Bridges (yabridge chainloaders
/// next to a `.clap-win` link) count as Windows plugins.
pub fn discover(dirs: &[PathBuf]) -> Vec<Found> {
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "clap"))
            .collect();
        files.sort();
        for path in files {
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            let windows = path.with_extension("clap-win").exists();
            found.push(Found {
                label: if windows {
                    format!("{stem} (Windows)")
                } else {
                    stem
                },
                path,
                windows,
            });
        }
    }
    found
}

pub struct Editor {
    /// `gui.create` + `set_parent` done; the editor exists (shown or hidden).
    pub created: bool,
    pub size: GuiSize,
    pub can_resize: bool,
    pub scroll: (u32, u32),
    pub last_shown: Instant,
}

// Field order matters on drop: the fd poller goes first, then the instance, then the library.
pub struct Slot {
    pub fds: FdPoller,
    pub instance: PluginInstance<Host>,
    _entry: PluginEntry,
    pub name: String,
    pub windows: bool,
    pub load_time: std::time::Duration,
    pub editor: Editor,
    pub route: Route,
}

impl Slot {
    pub fn load(
        proxy: &EventLoopProxy<UserEvent>,
        index: usize,
        path: &Path,
        windows: bool,
    ) -> Result<Self> {
        let t = Instant::now();
        // SAFETY: loading a plugin binary runs its code; that is the point of a host.
        let entry = unsafe { PluginEntry::load(path) }
            .map_err(|e| anyhow!("loading {}: {e:?}", path.display()))?;
        let factory = entry.get_plugin_factory().context("no plugin factory")?;
        let desc = factory
            .plugin_descriptors()
            .next()
            .context("no plugins in bundle")?;
        let id = CString::from(desc.id().context("no id")?);
        let name = desc
            .name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let host_info = HostInfo::new("Merlin S5", "Merlin", "", "0.0.0")?;
        let proxy_for_shared = proxy.clone();
        let instance = PluginInstance::<Host>::new(
            |_| Shared::new(proxy_for_shared, index),
            |shared| MainThread::new(shared),
            &entry,
            &id,
            &host_info,
        )?;
        Ok(Self {
            fds: FdPoller::spawn(proxy.clone(), index),
            instance,
            _entry: entry,
            name,
            windows,
            load_time: t.elapsed(),
            editor: Editor {
                created: false,
                size: GuiSize {
                    width: 0,
                    height: 0,
                },
                can_resize: false,
                scroll: (0, 0),
                last_shown: Instant::now(),
            },
            route: Route::DEFAULT,
        })
    }

    pub fn gui(&self) -> Option<clack_extensions::gui::PluginGui> {
        self.instance.access_handler(|h| h.gui.get())
    }

    /// Main-thread housekeeping: due timers and fd-set changes.
    pub fn service(&mut self, now: Instant) {
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

    pub fn on_fd_ready(&mut self) {
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
