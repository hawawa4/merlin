//! An LV2 plugin's X11 editor, embedded into a window of ours.

use std::ffi::{CString, c_void};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use libloading::os::unix::Library;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

use super::abi::*;
use super::features::{
    ATOM_FLOAT, Audio, FeatureSet, INSTANCE_ACCESS, OPTIONS_INSTANCE, OPTIONS_INTERFACE, UI_IDLE,
    UI_IDLE_EXT, UI_PARENT, UI_RESIZE, UI_SCALE, ui_supported,
};
use super::instance::{Bank, Instance};
use super::processor::{UI_ATOM_BYTES, UiAtom};
use super::ttl::{PluginDesc, PortKind, UiDesc};
use super::urid::Urids;
use crate::{EditorSize, HostEvent, Waker};

const IDLE_EVERY: Duration = Duration::from_millis(16);

/// What the UI's callbacks reach: the control values, the way to the audio thread, and the size
/// the UI asked for. Its address is the UI's "controller".
struct Controller {
    bank: Arc<Bank>,
    to_plugin: Arc<Mutex<rtrb::Producer<UiAtom>>>,
    waker: Arc<dyn Waker>,
    /// Width and height packed (0 = never asked).
    size: AtomicU32,
    event_transfer: u32,
}

unsafe extern "C" fn write(
    controller: *mut c_void,
    port: u32,
    size: u32,
    protocol: u32,
    buffer: *const c_void,
) {
    if controller.is_null() || buffer.is_null() {
        return;
    }
    // SAFETY: the controller we gave the UI is alive as long as the UI.
    let c = unsafe { &*(controller as *const Controller) };
    if protocol == 0 && size == 4 {
        // SAFETY: a float, as the protocol says.
        let v = unsafe { *(buffer as *const f32) };
        c.bank.set(port as usize, v);
        c.waker.wake(HostEvent::StateDirty);
    } else if protocol == c.event_transfer && size as usize <= UI_ATOM_BYTES {
        let mut msg = UiAtom {
            port,
            len: size,
            data: [0; UI_ATOM_BYTES],
        };
        // SAFETY: `size` bytes at `buffer`.
        msg.data[..size as usize].copy_from_slice(unsafe {
            std::slice::from_raw_parts(buffer as *const u8, size as usize)
        });
        if let Ok(mut tx) = c.to_plugin.lock() {
            let _ = tx.push(msg);
        }
        c.waker.wake(HostEvent::StateDirty);
    }
}

unsafe extern "C" fn ui_resize(handle: *mut c_void, w: i32, h: i32) -> i32 {
    // SAFETY: the handle is the controller.
    let c = unsafe { &*(handle as *const Controller) };
    let (w, h) = (w.clamp(1, 16383) as u32, h.clamp(1, 16383) as u32);
    c.size.store(w << 16 | h, Ordering::Release);
    c.waker.wake(HostEvent::EditorResize {
        width: w,
        height: h,
    });
    0
}

pub struct Lv2Editor {
    lib: Arc<Library>,
    ui_desc: *const UiDescriptor,
    handle: Handle,
    idle: Option<*const UiIdle>,
    /// The UI's own `ui:resize`: how the host resizes it.
    resize: Option<*const UiResize>,
    /// The UI's `opts:interface`: how the host changes its scale after it was made.
    options: Option<*const OptionsInterface>,
    urids: Arc<Urids>,
    controller: Box<Controller>,
    // Kept alive for the UI's lifetime.
    _features: FeatureSet,
    _host_resize: Box<UiResize>,
    pub size: EditorSize,
    /// The window the UI is in, and the `ui:scaleFactor` it was made with.
    pub parent: u32,
    pub scale: f64,
    last_idle: Instant,
}

// SAFETY: the editor lives on the main thread with the plugin handle.
unsafe impl Send for Lv2Editor {}

fn x11_size(window: u32) -> Option<EditorSize> {
    let (conn, _) = RustConnection::connect(None).ok()?;
    let g = conn.get_geometry(window).ok()?.reply().ok()?;
    (g.width > 0 && g.height > 0).then_some(EditorSize {
        width: u32::from(g.width),
        height: u32::from(g.height),
    })
}

impl Lv2Editor {
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        plugin: &PluginDesc,
        ui: &UiDesc,
        inst: &Instance,
        plugin_lib: &Arc<Library>,
        bank: Arc<Bank>,
        to_plugin: Arc<Mutex<rtrb::Producer<UiAtom>>>,
        waker: Arc<dyn Waker>,
        parent: u32,
        scale: f64,
        sample_rate: f64,
    ) -> Result<Self> {
        for r in &ui.required {
            if !ui_supported(r) {
                bail!("the editor needs {r}, which this host does not provide");
            }
        }
        let lib = if ui.binary == plugin.binary {
            plugin_lib.clone()
        } else {
            // SAFETY: loading the UI library runs its static constructors.
            Arc::new(
                unsafe { Library::new(&ui.binary) }
                    .with_context(|| format!("cannot load {}", ui.binary.display()))?,
            )
        };
        // SAFETY: the entry point every LV2 UI library exports.
        let entry: UiDescriptorFn = unsafe {
            *lib.get::<UiDescriptorFn>(b"lv2ui_descriptor\0")
                .map_err(|_| anyhow!("no lv2ui_descriptor in {}", ui.binary.display()))?
        };
        let mut ui_desc = std::ptr::null();
        for i in 0..64 {
            // SAFETY: the library answers with its i-th UI, or null past the last.
            let d = unsafe { entry(i) };
            if d.is_null() {
                break;
            }
            // SAFETY: a descriptor the library owns.
            if unsafe { super::features::text((*d).uri) } == ui.uri {
                ui_desc = d;
                break;
            }
        }
        if ui_desc.is_null() {
            bail!("the library does not contain the editor {}", ui.uri);
        }

        let urids = inst.urids().clone();
        let own_urids = urids.clone();
        let controller = Box::new(Controller {
            bank: bank.clone(),
            to_plugin,
            waker,
            size: AtomicU32::new(0),
            event_transfer: urids.map("http://lv2plug.in/ns/ext/atom#eventTransfer"),
        });
        let resize = Box::new(UiResize {
            handle: &*controller as *const Controller as *mut c_void,
            ui_resize,
        });
        let audio = Audio {
            sample_rate,
            min_block: 1,
            max_block: 4096,
            scale: scale as f32,
        };
        let features = FeatureSet::new(
            urids,
            &audio,
            vec![
                (UI_PARENT, parent as usize as *mut c_void),
                (UI_IDLE, std::ptr::null_mut()),
                (UI_RESIZE, &*resize as *const UiResize as *mut c_void),
                (INSTANCE_ACCESS, inst.handle),
            ],
        );

        let plugin_uri = CString::new(plugin.uri.clone())?;
        let mut bundle = plugin.bundle.to_string_lossy().into_owned();
        if !bundle.ends_with('/') {
            bundle.push('/');
        }
        let bundle = CString::new(bundle)?;
        let mut widget: *mut c_void = std::ptr::null_mut();
        // SAFETY: the arguments are what LV2 UI defines; the features and the controller
        // outlive the UI.
        let handle = unsafe {
            ((*ui_desc).instantiate)(
                ui_desc,
                plugin_uri.as_ptr(),
                bundle.as_ptr(),
                write,
                &*controller as *const Controller as *mut c_void,
                &mut widget,
                features.as_ptr(),
            )
        };
        if handle.is_null() {
            bail!("the editor refused to start (instantiate returned null)");
        }
        let extension = |uri: &str| {
            let ext = CString::new(uri).unwrap();
            // SAFETY: extension_data is optional and returns static data.
            unsafe {
                (*ui_desc)
                    .extension_data
                    .map(|f| f(ext.as_ptr()))
                    .filter(|p| !p.is_null())
            }
        };
        let idle = extension(UI_IDLE_EXT).map(|p| p as *const UiIdle);
        let ui_resize_ext = extension(UI_RESIZE).map(|p| p as *const UiResize);
        let options = extension(OPTIONS_INTERFACE).map(|p| p as *const OptionsInterface);
        let packed = controller.size.load(Ordering::Acquire);
        let size = if packed != 0 {
            EditorSize {
                width: packed >> 16,
                height: packed & 0xFFFF,
            }
        } else {
            x11_size(widget as usize as u32).unwrap_or(EditorSize {
                width: 800,
                height: 600,
            })
        };
        let editor = Self {
            lib,
            ui_desc,
            handle,
            idle,
            resize: ui_resize_ext,
            options,
            urids: own_urids,
            controller,
            _features: features,
            _host_resize: resize,
            size,
            parent,
            scale,
            last_idle: Instant::now(),
        };
        // Show the current values.
        for p in &plugin.ports {
            if p.kind == PortKind::Control {
                editor.port_event(p.index as u32, bank.get(p.index));
            }
        }
        Ok(editor)
    }

    /// Tells the UI a control port has this value.
    pub fn port_event(&self, port: u32, value: f32) {
        // SAFETY: a live UI; a float buffer of 4 bytes, protocol 0.
        unsafe {
            ((*self.ui_desc).port_event)(
                self.handle,
                port,
                4,
                0,
                &value as *const f32 as *const c_void,
            );
        }
    }

    /// Whether the host can resize the UI (it offers `ui:resize` itself).
    pub fn can_resize(&self) -> bool {
        self.resize.is_some()
    }

    /// Asks the UI to take this size. `false` if it cannot be resized or refused.
    pub fn set_size(&mut self, size: EditorSize) -> bool {
        let Some(r) = self.resize else {
            return false;
        };
        let (w, h) = (size.width.min(16383) as i32, size.height.min(16383) as i32);
        // SAFETY: a live UI. The `handle` field of a UI's own LV2UI_Resize is unused: the host
        // passes the UI's handle.
        let ok = unsafe { ((*r).ui_resize)(self.handle, w, h) } == 0;
        if ok {
            self.size = size;
        }
        ok
    }

    /// Changes the UI's `ui:scaleFactor` (its content scale) and returns the size it has then.
    /// `None` if it has no options interface or refused the option.
    pub fn set_zoom(&mut self, scale: f64) -> Option<EditorSize> {
        let o = self.options?;
        let value = scale as f32;
        let options = [
            OptionsOption {
                context: self.urids.map(OPTIONS_INSTANCE),
                subject: 0,
                key: self.urids.map(UI_SCALE),
                size: 4,
                kind: self.urids.map(ATOM_FLOAT),
                value: &value as *const f32 as *const c_void,
            },
            OptionsOption {
                context: 0,
                subject: 0,
                key: 0,
                size: 0,
                kind: 0,
                value: std::ptr::null(),
            },
        ];
        // SAFETY: a live UI; a list ending with a zero key, alive for the call.
        let status = unsafe { ((*o).set)(self.handle, options.as_ptr()) };
        if status != 0 {
            return None;
        }
        self.scale = scale;
        // A UI answers a new scale by asking for a new size, through our `ui_resize`.
        let packed = self.controller.size.load(Ordering::Acquire);
        if packed != 0 {
            self.size = EditorSize {
                width: packed >> 16,
                height: packed & 0xFFFF,
            };
        }
        Some(self.size)
    }

    /// Runs the UI's idle callback when it is due.
    pub fn idle(&mut self, now: Instant) {
        if let Some(idle) = self.idle
            && now.duration_since(self.last_idle) >= IDLE_EVERY
        {
            self.last_idle = now;
            // SAFETY: a live UI. A non-zero answer means "close me": the host's dock decides that.
            unsafe { ((*idle).idle)(self.handle) };
        }
    }

    pub fn next_idle(&self) -> Option<Instant> {
        self.idle.map(|_| self.last_idle + IDLE_EVERY)
    }
}

impl Drop for Lv2Editor {
    fn drop(&mut self) {
        // SAFETY: the last use of the UI handle; the library is held until after this.
        unsafe { ((*self.ui_desc).cleanup)(self.handle) };
        let _ = (&self.lib, &self.controller);
    }
}
