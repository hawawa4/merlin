//! VST2 hosting (plan Phase 6): native Linux `.so` plugins, and Windows `.dll` plugins through
//! yabridge's VST2 chainloader (which is itself a Linux `.so` with the same entry point). The
//! ABI is written out in [`abi`] from the shared binary layout; no Steinberg code is involved.
//!
//! Known gaps: no sub-programs, no MIDI output from plugins, no double-precision processing,
//! and editors cannot be resized by the host (VST2 only lets the plugin ask).

mod abi;
mod host;
mod processor;

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use merlin_model::Format;
use libloading::os::unix::Library;

use crate::{
    EditorSize, ParamInfo, PluginEditor, PluginHandle, PluginInfo, PluginProcessor, Waker,
};
use abi::*;
use host::{Ctx, SHELL_ID, callback};
use processor::{MAX_FRAMES, Vst2Processor};

/// Magic at the start of our saved state.
const STATE_MAGIC: &[u8; 4] = b"IV2S";
const STATE_CHUNK: u8 = 0;
const STATE_PARAMS: u8 = 1;
/// Plugins that need `effEditIdle` get it this often while their editor is open.
const EDITOR_IDLE: Duration = Duration::from_millis(20);

/// A loaded `.so` (or yabridge chainloader).
pub struct Vst2Library {
    lib: Arc<Library>,
    pub path: PathBuf,
}

struct NoWaker;

impl Waker for NoWaker {
    fn wake(&self, _: crate::HostEvent) {}
}

fn text(effect: *mut AEffect, opcode: i32, index: i32) -> String {
    let mut buf = [0u8; 256];
    // SAFETY: a live effect and a buffer far larger than any VST2 string.
    unsafe {
        ((*effect).dispatcher)(effect, opcode, index, 0, buf.as_mut_ptr().cast(), 0.0);
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_owned()
}

impl Vst2Library {
    pub fn load(path: &Path) -> Result<Self> {
        // SAFETY: loading a plugin runs its static constructors; that is the point.
        let lib = unsafe { Library::new(path) }
            .with_context(|| format!("cannot load {}", path.display()))?;
        Ok(Self {
            lib: Arc::new(lib),
            path: path.to_owned(),
        })
    }

    /// Runs the entry point (for shell plugins, as sub-plugin `shell_id`).
    fn instantiate(&self, shell_id: i32, ctx: &Arc<Ctx>) -> Result<*mut AEffect> {
        // SAFETY: the three names are the entry points plugins have used over the years; each has
        // the same signature.
        let entry: EntryProc = unsafe {
            *self
                .lib
                .get::<EntryProc>(b"VSTPluginMain\0")
                .or_else(|_| self.lib.get::<EntryProc>(b"main_plugin\0"))
                .or_else(|_| self.lib.get::<EntryProc>(b"main\0"))
                .map_err(|_| anyhow!("not a VST2 plugin (no VSTPluginMain)"))?
        };
        SHELL_ID.with(|s| s.set(shell_id));
        // SAFETY: the plugin calls back into `callback` with the arguments of the protocol.
        let effect = unsafe { entry(callback) };
        SHELL_ID.with(|s| s.set(0));
        if effect.is_null() {
            bail!("the plugin did not create an effect");
        }
        // SAFETY: a non-null effect from the plugin.
        unsafe {
            if (*effect).magic != EFFECT_MAGIC {
                bail!("not a VST2 effect (bad magic)");
            }
            (*effect).user = Arc::as_ptr(ctx) as *mut c_void;
            ((*effect).dispatcher)(effect, EFF_OPEN, 0, 0, std::ptr::null_mut(), 0.0);
        }
        Ok(effect)
    }

    fn describe(&self, effect: *mut AEffect, fallback_name: &str) -> PluginInfo {
        // SAFETY: a live effect.
        let (flags, id, version) =
            unsafe { ((*effect).flags, (*effect).unique_id, (*effect).version) };
        let name = {
            let n = text(effect, EFF_GET_EFFECT_NAME, 0);
            if n.is_empty() {
                let p = text(effect, EFF_GET_PRODUCT_STRING, 0);
                if p.is_empty() {
                    fallback_name.to_owned()
                } else {
                    p
                }
            } else {
                n
            }
        };
        let mut features = Vec::new();
        // SAFETY: a live effect.
        let category = unsafe {
            ((*effect).dispatcher)(
                effect,
                EFF_GET_PLUG_CATEGORY,
                0,
                0,
                std::ptr::null_mut(),
                0.0,
            )
        };
        if flags & FLAG_IS_SYNTH != 0 || category == PLUG_CATEG_SYNTH {
            features.push("instrument".to_owned());
            features.push("synthesizer".to_owned());
        } else {
            features.push("audio-effect".to_owned());
        }
        PluginInfo {
            format: Format::Vst2,
            id: format!("{:08X}", id as u32),
            name,
            vendor: text(effect, EFF_GET_VENDOR_STRING, 0),
            version: version.to_string(),
            features,
        }
    }

    fn close(&self, effect: *mut AEffect) {
        // SAFETY: closing an effect we opened; the plugin frees itself.
        unsafe {
            ((*effect).dispatcher)(effect, EFF_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
        }
    }

    /// The plugins in the file (several for a shell plugin). Starts each one to ask it.
    pub fn plugins(&self) -> Result<Vec<PluginInfo>> {
        let stem = self.path.file_stem().map_or_else(
            || "VST2 plugin".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        let ctx = Arc::new(Ctx::new(Arc::new(NoWaker)));
        let effect = self.instantiate(0, &ctx)?;
        let first = self.describe(effect, &stem);
        // SAFETY: a live effect.
        let category = unsafe {
            ((*effect).dispatcher)(
                effect,
                EFF_GET_PLUG_CATEGORY,
                0,
                0,
                std::ptr::null_mut(),
                0.0,
            )
        };
        let mut out = Vec::new();
        if category == PLUG_CATEG_SHELL {
            let mut ids = Vec::new();
            loop {
                let mut name = [0u8; 256];
                // SAFETY: a live effect and a buffer far larger than a plugin name.
                let id = unsafe {
                    ((*effect).dispatcher)(
                        effect,
                        EFF_SHELL_GET_NEXT_PLUGIN,
                        0,
                        0,
                        name.as_mut_ptr().cast(),
                        0.0,
                    )
                };
                if id == 0 || ids.len() >= 512 {
                    break;
                }
                let end = name.iter().position(|&b| b == 0).unwrap_or(0);
                ids.push((
                    id as i32,
                    String::from_utf8_lossy(&name[..end]).into_owned(),
                ));
            }
            self.close(effect);
            for (id, name) in ids {
                let ctx = Arc::new(Ctx::new(Arc::new(NoWaker)));
                if let Ok(sub) = self.instantiate(id, &ctx) {
                    let mut info = self.describe(sub, &name);
                    if info.name.is_empty() {
                        info.name = name;
                    }
                    out.push(info);
                    self.close(sub);
                }
            }
        } else {
            out.push(first);
            self.close(effect);
        }
        Ok(out)
    }

    /// Creates an instance (main thread). `id` is the eight hex digits of the unique id.
    pub fn create(&self, id: &str, waker: Arc<dyn Waker>) -> Result<Vst2Plugin> {
        let unique = u32::from_str_radix(id, 16).context("not a VST2 id")? as i32;
        let ctx = Arc::new(Ctx::new(waker));
        let effect = self.instantiate(unique, &ctx)?;
        // SAFETY: a live effect.
        let got = unsafe { (*effect).unique_id };
        if got != unique {
            self.close(effect);
            bail!("the file holds plugin {got:08X}, not {unique:08X}");
        }
        let stem = self
            .path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let info = self.describe(effect, &stem);
        Ok(Vst2Plugin {
            info,
            effect,
            ctx,
            params_tx: None,
            editor: EditorState::default(),
            last_idle: Instant::now(),
            active: false,
            lib: self.lib.clone(),
        })
    }
}

#[derive(Default)]
struct EditorState {
    open: bool,
    size: Option<EditorSize>,
}

pub struct Vst2Plugin {
    info: PluginInfo,
    effect: *mut AEffect,
    ctx: Arc<Ctx>,
    params_tx: Option<rtrb::Producer<(i32, f32)>>,
    editor: EditorState,
    last_idle: Instant,
    active: bool,
    // Last: the library must outlive the effect.
    lib: Arc<Library>,
}

impl Vst2Plugin {
    fn dispatch(&self, opcode: i32, index: i32, value: isize, ptr: *mut c_void, opt: f32) -> isize {
        // SAFETY: the effect is live until drop.
        unsafe { ((*self.effect).dispatcher)(self.effect, opcode, index, value, ptr, opt) }
    }

    fn flags(&self) -> i32 {
        // SAFETY: the effect is live until drop.
        unsafe { (*self.effect).flags }
    }

    fn num_params(&self) -> i32 {
        // SAFETY: the effect is live until drop.
        unsafe { (*self.effect).num_params }
    }

    fn get_param(&self, index: i32) -> f32 {
        // SAFETY: the effect is live until drop.
        unsafe { ((*self.effect).get_parameter)(self.effect, index) }
    }

    fn set_param_now(&self, index: i32, value: f32) {
        // SAFETY: the effect is live until drop.
        unsafe { ((*self.effect).set_parameter)(self.effect, index, value) }
    }

    fn rect(&self) -> Option<EditorSize> {
        let mut rect: *mut ERect = std::ptr::null_mut();
        self.dispatch(
            EFF_EDIT_GET_RECT,
            0,
            0,
            (&mut rect as *mut *mut ERect).cast(),
            0.0,
        );
        if rect.is_null() {
            return None;
        }
        // SAFETY: the plugin returned a pointer to its own ERect, valid until the next call.
        let r = unsafe { *rect };
        let (w, h) = (
            i32::from(r.right) - i32::from(r.left),
            i32::from(r.bottom) - i32::from(r.top),
        );
        (w > 0 && h > 0).then_some(EditorSize {
            width: w as u32,
            height: h as u32,
        })
    }
}

impl PluginHandle for Vst2Plugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn activate(&mut self, sample_rate: f64, max_block: u32) -> Result<Box<dyn PluginProcessor>> {
        let block = (max_block as usize).min(MAX_FRAMES);
        self.ctx.set_audio(sample_rate, block as i32);
        self.dispatch(
            EFF_SET_SAMPLE_RATE,
            0,
            0,
            std::ptr::null_mut(),
            sample_rate as f32,
        );
        self.dispatch(
            EFF_SET_BLOCK_SIZE,
            0,
            block as isize,
            std::ptr::null_mut(),
            0.0,
        );
        self.dispatch(EFF_MAINS_CHANGED, 0, 1, std::ptr::null_mut(), 0.0);
        self.dispatch(EFF_START_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
        self.active = true;
        let (tx, rx) = rtrb::RingBuffer::new(512);
        self.params_tx = Some(tx);
        Ok(Box::new(Vst2Processor::new(
            self.effect,
            rx,
            sample_rate as f32,
            self.ctx.clone(),
            self.lib.clone(),
        )))
    }

    fn deactivate(&mut self) {
        if std::mem::take(&mut self.active) {
            self.dispatch(EFF_STOP_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
            self.dispatch(EFF_MAINS_CHANGED, 0, 0, std::ptr::null_mut(), 0.0);
        }
        self.params_tx = None;
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        (0..self.num_params())
            .map(|i| ParamInfo {
                id: i as u32,
                name: text(self.effect, EFF_GET_PARAM_NAME, i),
                module: String::new(),
                min: 0.0,
                max: 1.0,
                default: f64::from(self.get_param(i)),
                automatable: self.dispatch(EFF_CAN_BE_AUTOMATED, i, 0, std::ptr::null_mut(), 0.0)
                    != 0,
                stepped: false,
                hidden: false,
                read_only: false,
            })
            .collect()
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let value = value.clamp(0.0, 1.0) as f32;
        match self.params_tx.as_mut() {
            // Applied by the audio thread at the start of its next block.
            Some(tx) => {
                let _ = tx.push((id as i32, value));
            }
            None => self.set_param_now(id as i32, value),
        }
    }

    fn save_state(&mut self) -> Result<Vec<u8>> {
        let mut out = STATE_MAGIC.to_vec();
        if self.flags() & FLAG_PROGRAM_CHUNKS != 0 {
            let mut data: *mut u8 = std::ptr::null_mut();
            // index 0 = the whole bank, which includes the current program
            let len = self.dispatch(EFF_GET_CHUNK, 0, 0, (&mut data as *mut *mut u8).cast(), 0.0);
            if len > 0 && !data.is_null() {
                out.push(STATE_CHUNK);
                // SAFETY: the plugin owns `len` bytes at `data` until the next call.
                out.extend_from_slice(unsafe { std::slice::from_raw_parts(data, len as usize) });
                return Ok(out);
            }
        }
        out.push(STATE_PARAMS);
        for i in 0..self.num_params() {
            out.extend_from_slice(&self.get_param(i).to_le_bytes());
        }
        Ok(out)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < 5 || &data[..4] != STATE_MAGIC {
            bail!("not a saved VST2 state");
        }
        match data[4] {
            STATE_CHUNK => {
                let chunk = &data[5..];
                let r = self.dispatch(
                    EFF_SET_CHUNK,
                    0,
                    chunk.len() as isize,
                    chunk.as_ptr().cast_mut().cast(),
                    0.0,
                );
                if r < 0 {
                    bail!("the plugin refused its saved state");
                }
            }
            STATE_PARAMS => {
                let (values, _) = data[5..].as_chunks::<4>();
                for (i, v) in values
                    .iter()
                    .enumerate()
                    .take(self.num_params().max(0) as usize)
                {
                    self.set_param_now(i as i32, f32::from_le_bytes(*v));
                }
            }
            other => bail!("unknown VST2 state kind {other}"),
        }
        Ok(())
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        (self.flags() & FLAG_HAS_EDITOR != 0).then_some(self as &mut dyn PluginEditor)
    }

    fn on_main_thread(&mut self) {}

    fn service(&mut self, now: Instant) {
        if self.editor.open && now.duration_since(self.last_idle) >= EDITOR_IDLE {
            self.last_idle = now;
            self.dispatch(EFF_EDIT_IDLE, 0, 0, std::ptr::null_mut(), 0.0);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.editor.open.then(|| self.last_idle + EDITOR_IDLE)
    }

    fn on_fd_ready(&mut self) {}
}

impl PluginEditor for Vst2Plugin {
    fn attach(&mut self, parent: u32, _scale: f64) -> Result<EditorSize> {
        // Some plugins want the size asked before the editor opens.
        let before = self.rect();
        let ok = self.dispatch(EFF_EDIT_OPEN, 0, 0, parent as usize as *mut c_void, 0.0);
        if ok == 0 && self.rect().is_none() {
            bail!("the plugin did not open its editor");
        }
        self.editor.open = true;
        let size = self.rect().or(before).unwrap_or(EditorSize {
            width: 640,
            height: 480,
        });
        self.editor.size = Some(size);
        self.last_idle = Instant::now();
        Ok(size)
    }

    fn is_attached(&self) -> bool {
        self.editor.open
    }

    fn show(&mut self) {}

    fn hide(&mut self) {}

    fn size(&self) -> EditorSize {
        self.editor.size.unwrap_or(EditorSize {
            width: 640,
            height: 480,
        })
    }

    // VST2 editors have a size of their own; only the plugin can change it.
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
        self.editor.size = Some(size);
    }

    fn detach(&mut self) {
        if std::mem::take(&mut self.editor.open) {
            self.dispatch(EFF_EDIT_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
        }
    }
}

impl Drop for Vst2Plugin {
    fn drop(&mut self) {
        self.detach();
        self.deactivate();
        self.dispatch(EFF_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
    }
}

// SAFETY: a plugin handle lives on the main thread; the raw effect pointer is not shared with
// anything that is not synchronised through the protocol.
unsafe impl Send for Vst2Plugin {}
